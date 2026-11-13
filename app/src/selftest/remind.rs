//! Self-test commands for Remind Me's backend (the fork's Superhuman pass,
//! docs/superhuman/remind.md): a conversation sent to Reminders until a time, the wake run at a
//! time the script names (its clock), cancelling and Undo, through the store as the app's
//! picker will (phase C); `h` itself is unchanged. Times are ISO 8601 (`2026-10-08T09:00:00`,
//! local time unless it names a zone), or `now`, `+30m`, `+2h`, `+1d` from the list's clock
//! (`list-clock`).
//!
//! | Command | What it does |
//! | --- | --- |
//! | `remind SUBJECT WHEN` | Sends the conversation with SUBJECT in the list's view to Reminders until WHEN (its messages in the view's folders), as one Undo batch; tells the engine (`OpsQueued`, `RemindersChanged`) |
//! | `remind-cancel SUBJECT` | Cancels the conversation's reminder: a waiting one comes back to its Inbox now |
//! | `remind-undo` | Undoes the last `remind` or `remind-cancel` |
//! | `wake-reminders [WHEN]` | Brings back the reminders due at WHEN (the list's clock's now without it), as the engine's timer does |
//! | `assert-reminder SUBJECT waiting\|reminded\|none [WHEN]` | The conversation's reminder: its state, and when it is due (waiting) or came back (reminded) |
//! | `assert-conversations FOLDER [SUBJECT…]` | Every conversation row of FOLDER (`all-inboxes`, `[ACCOUNT:]ROLE` or `[ACCOUNT:]NAME`) by subject, in order |
//! | `assert-conversation-date FOLDER SUBJECT WHEN` | The date a conversation's row in FOLDER sorts and shows by |
//! | `remind-reply SUBJECT FROM` | An answer from FROM to the conversation with SUBJECT arrives in its account's Inbox now (the list's clock) |
//! | `assert-reminder-at SUBJECT LABEL` | When the conversation's reminder comes back, as the picker and the reader say it (`tomorrow 08:00`, `someday`) |
//! | `assert-row-reminder POS BOOL` | Whether the list's row at POS shows Remind Me's clock (phase C) |
//! | `assert-reminders-shown marks\|wakes\|none` | What the list's rows show of Remind Me: the clock on returned conversations (an Inbox with one back, which gives the clock its slot), the wake times (the Reminders view's conversation rows), or nothing |
//! | `assert-reader-reminder TEXT\|none` | The reader's quiet line under the subject |
//! | `assert-quick-order POS NAMES` | The row's quick actions, left to right (`archive,remind,delete,star`), shown by hovering it |
//! | `assert-quick-off POS BOOL` | Whether the row's clock quick action is dimmed (Remind Me is off for what the list shows: Sent, Drafts, Trash, Junk), shown by hovering it |
//! | `assert-empty-icon NAME` | The first icon name of the list's empty state (`alarm-symbolic` in the Reminders view) |
//! | `assert-quick-tooltip POS NAME TEXT` | A row's quick action's tooltip (`remind`: "Remind Me… (h)", and why it is off where it is), shown by hovering it |
//! | `assert-empty-text TITLE DESCRIPTION` | The list's empty state's words |
//! | `remind-server-account NAME` | A server account (keyed, as the engine's are: its moves wait for the server) with the six role folders, each synced once and empty; mail added to it (`mail-add NAME:ROLE …`, `remind-reply`) gets UIDs (the third review) |
//! | `remind-server-run NAME` | A stand-in server with UIDPLUS carries out the account's ops due now, as the engine's op runner would: creates and syncs a Reminders folder a move goes into, moves with new UIDs (COPYUID), stores flags |
//! | `remind-new-session SINCE` | The app's reminders begin anew as when Pronto starts, its session begun at SINCE: the read it makes is the first, and what came back before SINCE is from an earlier session (waits for that read and its notifications) |
//!
//! WHEN may also be `someday` (phase C).

use super::{Harness, StepResult, equal};
use crate::list::model::Content;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::account::AccountKey;
use sealwax_domain::message::base_subject;
use sealwax_domain::model::{AccountId, FolderId, FolderRole, ThreadId};
use sealwax_domain::remind::ReminderState;
use sealwax_engine::api::Command;
use sealwax_infrastructure::store::{
    BatchId, FirstSyncStep, FolderSpec, NewAccount, OpAction, OpsDone, Remind, Store, ViewSpec,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("remind", 2, 2),
    ("remind-cancel", 1, 1),
    ("remind-undo", 0, 0),
    ("wake-reminders", 0, 1),
    ("assert-reminder", 2, 3),
    ("assert-conversations", 1, usize::MAX),
    ("assert-conversation-date", 3, 3),
    ("remind-reply", 2, 2),
    ("assert-reminder-at", 2, 2),
    ("assert-row-reminder", 2, 2),
    ("assert-reminders-shown", 1, 1),
    ("assert-reader-reminder", 1, 1),
    ("assert-quick-order", 2, 2),
    ("assert-quick-off", 2, 2),
    ("assert-empty-icon", 1, 1),
    ("assert-empty-text", 2, 2),
    ("assert-quick-tooltip", 3, 3),
    ("remind-server-account", 1, 1),
    ("remind-server-run", 1, 1),
    ("remind-new-session", 1, 1),
];

/// The UIDVALIDITY of the stand-in server's folders.
const UIDVALIDITY: u32 = 7;

thread_local! {
    /// What `remind-undo` takes back.
    static LAST: Cell<Option<BatchId>> = const { Cell::new(None) };
    /// The next UID of each folder the stand-in server has (`remind-server-account`).
    static SERVER: RefCell<HashMap<FolderId, u32>> = RefCell::default();
    /// The conversations found by subject so far: a server account's messages are hidden while
    /// a move of theirs waits for the server, and no folder's rows show them then.
    static FOUND: RefCell<HashMap<String, ThreadId>> = RefCell::default();
}

/// The UID a message stored in `folder` now gets: the stand-in server's next one there, or
/// `None` for a local folder (`mail-add`, `remind-reply`).
pub(super) fn server_uid(folder: FolderId) -> Option<u32> {
    SERVER.with(|server| {
        server.borrow_mut().get_mut(&folder).map(|next| {
            *next += 1;
            *next - 1
        })
    })
}

/// Gives `folder` the stand-in server's first sync of an empty folder: `Ready`, UIDs from 1.
async fn first_sync(store: &Store, folder: FolderId) -> StepResult {
    for step in [
        FirstSyncStep::Start {
            uidvalidity: UIDVALIDITY,
            top: 1,
            modseq: 0,
        },
        FirstSyncStep::Finish { below: 0 },
    ] {
        store
            .first_sync_step(folder, step)
            .recv()
            .await
            .map_err(|error| error.to_string())?;
    }
    SERVER.with(|server| {
        server.borrow_mut().entry(folder).or_insert(1);
    });
    Ok(())
}

/// `remind-server-account NAME`: a keyed account, as the engine adds one, so its triage and
/// reminders queue ops for the server (`remind-server-run`) where a local account's apply at
/// once; its role folders synced once and empty.
async fn server_account(harness: &Harness, name: &str) -> StepResult {
    let store = store(harness)?;
    let local: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    let account = store
        .ensure_account(
            AccountKey(local.clone()),
            NewAccount {
                name: name.to_owned(),
                address: format!("{local}@example.test"),
            },
        )
        .recv()
        .await
        .map_err(|error| error.to_string())?
        .value;
    super::mail::account_read(harness, account).await?;
    for (role, folder) in [
        (FolderRole::Inbox, "INBOX"),
        (FolderRole::Drafts, "Drafts"),
        (FolderRole::Sent, "Sent"),
        (FolderRole::Archive, "Archive"),
        (FolderRole::Junk, "Junk"),
        (FolderRole::Trash, "Trash"),
    ] {
        let folder = store
            .upsert_folder(FolderSpec::local(account, folder, role))
            .recv()
            .await
            .map_err(|error| error.to_string())?
            .value;
        first_sync(&store, folder).await?;
    }
    let mailbox = harness.state.mailbox();
    let started = Instant::now();
    while !mailbox.folders().iter().any(|f| f.account == account) {
        if started.elapsed() > Duration::from_secs(10) {
            return Err("the sidebar never got the new account's folders".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    super::mail::settled(harness).await
}

/// `remind-server-run NAME`: what the engine's op runner does with the account's ops due now,
/// against a server with UIDPLUS that does what it is asked: the Reminders folders a move goes
/// into first (`prepare_folders`: CREATE, then a first pass), then each group, a move's
/// messages given the target's next UIDs (COPYUID).
async fn server_run(harness: &Harness, name: &str) -> StepResult {
    let account: AccountId = harness
        .state
        .mailbox()
        .accounts()
        .iter()
        .find(|account| account.name == name)
        .map(|account| account.id)
        .ok_or_else(|| format!("no account {name}"))?;
    let store = store(harness)?;
    let now = glib::real_time() / 1000;
    let wanted = store
        .folders_to_prepare(account, now)
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    for folder in wanted {
        if folder.create {
            store
                .folder_created(folder.folder)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
        }
        if folder.sync {
            first_sync(&store, folder.folder).await?;
        }
    }
    let due = store
        .due_ops(account, now)
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    let groups = due.groups.len();
    for group in due.groups {
        let new_uids = match &group.action {
            OpAction::Move { to, .. } | OpAction::Copy { to, .. } => group
                .messages
                .iter()
                .map(|message| {
                    server_uid(*to)
                        .map(|uid| (message.uid, uid))
                        .ok_or_else(|| format!("the stand-in server has no folder {}", to.0))
                })
                .collect::<Result<Vec<_>, String>>()?,
            _ => Vec::new(),
        };
        store
            .start_ops(group.ops.clone(), None)
            .recv()
            .await
            .map_err(|error| error.to_string())?;
        store
            .ops_done(OpsDone {
                ops: group.ops,
                new_uids,
                target_uidvalidity: Some(UIDVALIDITY),
                flagged_only: false,
            })
            .recv()
            .await
            .map_err(|error| error.to_string())?;
    }
    println!("  # the server ran {groups} op groups");
    super::mail::settled(harness).await
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

/// A time as the commands write it, in Unix seconds.
fn when(harness: &Harness, text: &str) -> Result<i64, String> {
    let now = super::mail::list(harness)?.now_unix();
    if text == "now" {
        return Ok(now);
    }
    if text == "someday" {
        return Ok(sealwax_domain::remind::SOMEDAY);
    }
    if let Some(relative) = text.strip_prefix('+') {
        let (amount, unit) = relative.split_at(relative.len().saturating_sub(1));
        let amount: i64 = amount.parse().map_err(|_| format!("not a time: {text}"))?;
        let seconds = match unit {
            "m" => 60,
            "h" => 3_600,
            "d" => 86_400,
            _ => return Err(format!("not a time: {text}")),
        };
        return Ok(now + amount * seconds);
    }
    glib::DateTime::from_iso8601(text, Some(&glib::TimeZone::local()))
        .map(|time| time.to_unix())
        .map_err(|_| format!("not an ISO 8601 time: {text}"))
}

fn store(harness: &Harness) -> Result<Arc<Store>, String> {
    harness
        .state
        .mailbox()
        .shared_store()
        .ok_or_else(|| "the store is not open".to_owned())
}

/// The view a script names: `all-inboxes`, or a folder.
fn view(harness: &Harness, name: &str) -> Result<ViewSpec, String> {
    if name == "all-inboxes" {
        return Ok(ViewSpec::unified_inbox());
    }
    Ok(ViewSpec::folder(
        super::mail::find_folder(harness, name)?.id,
    ))
}

/// The thread of the row with `subject` (or that base subject) among `spec`'s rows.
async fn thread_in(
    store: &Store,
    spec: ViewSpec,
    subject: &str,
) -> Result<Option<ThreadId>, String> {
    let page = store
        .page(spec, 0, u32::MAX)
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    let message = page
        .rows
        .iter()
        .find(|row| row.subject == subject || base_subject(&row.subject) == subject)
        .map(|row| row.thread);
    let conversation = page
        .conversations
        .iter()
        .find(|row| row.subject == subject)
        .map(|row| row.thread);
    Ok(message.or(conversation))
}

/// The thread of the conversation with `subject`, wherever its messages are; while they are
/// hidden (a server account's move waits), the one found before.
async fn thread(harness: &Harness, subject: &str) -> Result<ThreadId, String> {
    let store = store(harness)?;
    for folder in harness.state.mailbox().folders() {
        if let Some(thread) = thread_in(&store, ViewSpec::folder(folder.id), subject).await? {
            FOUND.with(|found| found.borrow_mut().insert(subject.to_owned(), thread));
            return Ok(thread);
        }
    }
    FOUND
        .with(|found| found.borrow().get(subject).copied())
        .ok_or_else(|| format!("no conversation {subject}"))
}

/// Tells the engine, when one is attached: the moves are queued ops of `accounts`.
fn tell_engine(harness: &Harness, accounts: &[sealwax_domain::model::AccountId]) {
    let engine = harness.state.engine();
    for account in accounts {
        let _ = engine.send(Command::OpsQueued { account: *account });
    }
    let _ = engine.send(Command::RemindersChanged);
}

fn state_name(state: Option<ReminderState>) -> &'static str {
    match state {
        Some(ReminderState::Waiting) => "waiting",
        Some(ReminderState::Reminded) => "reminded",
        Some(ReminderState::Ended) | None => "none",
    }
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |n: usize| args.get(n).map(String::as_str).unwrap_or_default();
    match command {
        "remind" => {
            let list = super::mail::list(harness)?;
            let Content::View(spec) = list.model().content() else {
                return Err("the list shows no folder view".to_owned());
            };
            let store = store(harness)?;
            let thread = thread_in(&store, spec, arg(0))
                .await?
                .ok_or_else(|| format!("no conversation {} in the list's view", arg(0)))?;
            FOUND.with(|found| found.borrow_mut().insert(arg(0).to_owned(), thread));
            let reply = store
                .remind(Remind {
                    threads: vec![thread],
                    scope: spec.scope,
                    wake_at: when(harness, arg(1))?,
                    not_before: 0,
                })
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            if reply.reminders.is_empty() {
                return Err(format!("{} wasn't sent to Reminders", arg(0)));
            }
            LAST.with(|last| last.set(reply.batch));
            tell_engine(harness, &reply.accounts);
            super::mail::settled(harness).await
        }
        "remind-cancel" => {
            let thread = thread(harness, arg(0)).await?;
            let reply = store(harness)?
                .cancel_reminders(vec![thread], 0)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            LAST.with(|last| last.set(reply.batch));
            tell_engine(harness, &reply.accounts);
            super::mail::settled(harness).await
        }
        "remind-undo" => {
            let batch = LAST
                .with(Cell::take)
                .ok_or_else(|| "nothing to undo".to_owned())?;
            let reply = store(harness)?
                .undo(batch)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            tell_engine(harness, &reply.accounts);
            super::mail::settled(harness).await
        }
        "wake-reminders" => {
            let at = when(harness, if args.is_empty() { "now" } else { arg(0) })?;
            let woken = store(harness)?
                .wake_reminders(at)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            println!("  # {} reminders came back", woken.woken.len());
            super::mail::settled(harness).await
        }
        "assert-reminder" => {
            let thread = thread(harness, arg(0)).await?;
            let reminders = store(harness)?
                .reminders()
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let found = reminders
                .iter()
                .find(|reminder| reminder.threads.contains(&thread));
            equal(
                &format!("{}'s reminder", arg(0)),
                state_name(found.map(|reminder| reminder.state)),
                arg(1),
            )?;
            match (found, args.get(2)) {
                (Some(reminder), Some(time)) => {
                    let at = match reminder.state {
                        ReminderState::Reminded => reminder.reminded_at.unwrap_or_default(),
                        _ => reminder.wake_at,
                    };
                    equal("its time", at, when(harness, time)?)
                }
                _ => Ok(()),
            }
        }
        "assert-conversations" => {
            let spec = view(harness, arg(0))?.conversations();
            let page = store(harness)?
                .page(spec, 0, u32::MAX)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let actual: Vec<&str> = page
                .conversations
                .iter()
                .map(|row| row.subject.as_str())
                .collect();
            let wanted: Vec<&str> = args[1..].iter().map(String::as_str).collect();
            equal("conversations", actual.join(" | "), wanted.join(" | "))
        }
        "assert-conversation-date" => {
            let spec = view(harness, arg(0))?.conversations();
            let page = store(harness)?
                .page(spec, 0, u32::MAX)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let row = page
                .conversations
                .iter()
                .find(|row| row.subject == arg(1))
                .ok_or_else(|| format!("no conversation {} in {}", arg(1), arg(0)))?;
            equal("its date", row.date, when(harness, arg(2))?)
        }
        "remind-reply" => {
            let (thread, message) = message_of(harness, arg(0)).await?;
            let store = store(harness)?;
            let detail = store
                .message(message)
                .recv()
                .await
                .map_err(|error| error.to_string())?
                .value
                .ok_or("its message is gone")?;
            let serial = detail
                .headers
                .message_id
                .as_deref()
                .and_then(|id| id.strip_prefix("selftest-"))
                .and_then(|id| id.split('@').next())
                .and_then(|n| n.parse::<u32>().ok())
                .ok_or("not a self-test message")?;
            let folders = harness.state.mailbox().folders();
            let account = folders
                .iter()
                .find(|folder| folder.id == detail.summary.folder)
                .map(|folder| folder.account)
                .ok_or("its folder is gone")?;
            let inbox = folders
                .into_iter()
                .find(|folder| {
                    folder.account == account
                        && folder.role == sealwax_domain::model::FolderRole::Inbox
                })
                .ok_or("its account has no Inbox")?;
            let fiction = super::mail::Fiction {
                subject: format!("Re: {}", arg(0)),
                from: arg(1).to_owned(),
                text: "Svar fra selvtesten.".to_owned(),
                attachment: false,
                reply_to: Some(serial),
                html: false,
                list_id: None,
                to: None,
                cc: None,
            };
            let now = super::mail::list(harness)?.now_unix();
            super::mail::ingest_fiction(
                harness,
                inbox.id,
                &fiction,
                now,
                sealwax_domain::model::Flags::NONE,
            )
            .await?;
            println!("  # an answer to {} in thread {}", arg(0), thread.0);
            super::mail::settled(harness).await
        }
        "assert-reminder-at" => {
            let thread = thread(harness, arg(0)).await?;
            let reminders = store(harness)?
                .reminders()
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let reminder = reminders
                .iter()
                .find(|reminder| reminder.threads.contains(&thread))
                .ok_or_else(|| format!("{} has no reminder", arg(0)))?;
            equal(
                "when it comes back",
                crate::time_picker::describe_wake(reminder.wake_at).as_str(),
                arg(1),
            )
        }
        "assert-row-reminder" => {
            let list = super::mail::list(harness)?;
            let position: u32 = arg(0)
                .parse()
                .map_err(|_| format!("expected a row, got {}", arg(0)))?;
            let row = list
                .row_widget(position)
                .ok_or_else(|| format!("row {position} isn't bound"))?;
            equal(
                &format!("row {position}'s clock"),
                row.shows_reminder_mark(),
                super::script::boolean(arg(1))?,
            )
        }
        "assert-reminders-shown" => {
            let shown = super::mail::list(harness)?.reminders();
            let what = match (shown.marks, shown.wakes) {
                (true, _) => "marks",
                (_, true) => "wakes",
                _ => "none",
            };
            equal("the list's reminders", what, arg(0))
        }
        "assert-empty-icon" => {
            let icon = super::mail::list(harness)?.empty_icon();
            equal(
                "the empty state's icon",
                icon.as_deref().unwrap_or("none"),
                arg(0),
            )
        }
        "assert-quick-tooltip" => {
            let position: u32 = arg(0)
                .parse()
                .map_err(|_| format!("expected a row, got {}", arg(0)))?;
            let quick = crate::list::Quick::ALL
                .into_iter()
                .find(|quick| quick.name() == arg(1))
                .ok_or_else(|| format!("no quick action {}", arg(1)))?;
            let button = super::mail::list(harness)?
                .row_widget(position)
                .ok_or_else(|| format!("row {position} isn't bound"))?
                .quick_button(quick)
                .ok_or_else(|| format!("row {position} shows no quick actions"))?;
            equal(
                "its tooltip",
                button.tooltip_text().unwrap_or_default().as_str(),
                arg(2),
            )
        }
        "assert-empty-text" => {
            let (title, description) = super::mail::list(harness)?.empty_text();
            equal("the empty state's title", title.as_str(), arg(0))?;
            equal("its description", description.as_str(), arg(1))
        }
        "remind-server-account" => server_account(harness, arg(0)).await,
        "remind-server-run" => server_run(harness, arg(0)).await,
        "remind-new-session" => {
            crate::remind::new_session(&harness.state, when(harness, arg(0))?);
            let started = Instant::now();
            while crate::remind::reading() {
                if started.elapsed() > Duration::from_secs(10) {
                    return Err("the reminders' first read never ended".to_owned());
                }
                glib::timeout_future(Duration::from_millis(10)).await;
            }
            super::mail::settled(harness).await
        }
        "assert-reader-reminder" => {
            let reader = harness.state.reader().ok_or("no reader")?;
            let text = reader.reminder_text().unwrap_or_else(|| "none".to_owned());
            equal("the reader's reminder line", text.as_str(), arg(0))
        }
        "assert-quick-order" => {
            let list = super::mail::list(harness)?;
            let position: u32 = arg(0)
                .parse()
                .map_err(|_| format!("expected a row, got {}", arg(0)))?;
            let row = list
                .row_widget(position)
                .ok_or_else(|| format!("row {position} isn't bound"))?;
            let window = harness.window()?;
            let mut placed: Vec<(f32, &str)> = crate::list::Quick::ALL
                .iter()
                .filter_map(|quick| {
                    let button = row.quick_button(*quick)?;
                    let bounds = button.compute_bounds(window.window())?;
                    Some((bounds.x(), quick.name()))
                })
                .collect();
            placed.sort_by(|a, b| a.0.total_cmp(&b.0));
            let names: Vec<&str> = placed.iter().map(|(_, name)| *name).collect();
            equal("the quick actions", names.join(",").as_str(), arg(1))
        }
        "assert-quick-off" => {
            let position: u32 = arg(0)
                .parse()
                .map_err(|_| format!("expected a row, got {}", arg(0)))?;
            let row = super::mail::list(harness)?
                .row_widget(position)
                .ok_or_else(|| format!("row {position} isn't bound"))?;
            equal(
                "the clock quick action dimmed",
                row.quick_off(),
                super::script::boolean(arg(1))?,
            )
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// The thread and a message of the conversation with `subject`, wherever its messages are.
async fn message_of(
    harness: &Harness,
    subject: &str,
) -> Result<(ThreadId, sealwax_domain::model::MessageId), String> {
    let store = store(harness)?;
    for folder in harness.state.mailbox().folders() {
        let page = store
            .page(ViewSpec::folder(folder.id), 0, u32::MAX)
            .recv()
            .await
            .map_err(|error| error.to_string())?;
        if let Some(row) = page
            .rows
            .iter()
            .find(|row| row.subject == subject || base_subject(&row.subject) == subject)
        {
            return Ok((row.thread, row.id));
        }
    }
    Err(format!("no conversation {subject}"))
}
