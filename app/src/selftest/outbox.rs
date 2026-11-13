//! Self-test commands for the outbox in the main window (M6 phase B, ML-076) and the quick
//! reply's drafts: drafts queued in the app's store as the composer's backend queues them,
//! the engine's steps and events played by the fake engine (`engine-attach` first), the toasts'
//! buttons, and the stand-in backend's record of the quick reply's drafts. Draft names are the
//! script's own; message names are the conversation samples'. Every name, address and text is
//! fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `outbox-draft NAME ACCOUNT` | Saves a draft from ACCOUNT (`triage-account`'s, which has a server) in the store, closed |
//! | `outbox-queue NAME [MS]` | Queues it for the undo window (`queue_send`) and calls `outbox::queued`, as the backend's send does; with MS, for MS from now and `outbox::scheduled`, as Send Later does |
//! | `outbox-name NAME` | Names the outbox's newest entry NAME (a send the app queued itself), for the commands below |
//! | `outbox-copy NAME` | Files the draft's copy in its account's Drafts folder (its Message-ID, subject and sender), as the engine's upload and the next sync would |
//! | `outbox-quick NAME ACCOUNT MESSAGE TEXT` | A reply to MESSAGE from ACCOUNT, sent from the quick reply with TEXT: saved, marked as the quick reply's, queued |
//! | `outbox-engine start\|sent\|uncertain NAME [stopped\|no-reply\|unreadable]` | The engine's step on it: `send_started`; `send_done` and `Event::Sent`; `send_uncertain` and `Event::SendUncertain` with why (`stopped` when none is named) |
//! | `outbox-engine failed NAME ERROR` | `send_failed` (permanent) and `Event::SendFailed`: `recipients-refused` (two), `too-large`, `refused`, `needs-sign-in`, `other` |
//! | `outbox-expect KIND [KEY=VALUE]` | The app sent KIND to the engine (`outbox-queued`, `ops-queued`, `drafts-changed`) within a second; other commands are passed over |
//! | `toast-click LABEL` | Clicks the toast button labelled LABEL (`Undo`, `Open`) |
//! | `reply-draft-name NAME MESSAGE` | Names the store's newest closed reply draft to MESSAGE (the inline reply's, as `assert-reply-draft` reads it) NAME, for the commands here |
//! | `outbox-discard NAME` | Discards the draft in the store, as a card in another window would: the app isn't told |
//! | `quick-store-draft MESSAGE TEXT [KIND] [TO]` | A closed reply draft to MESSAGE whose body is TEXT, in the store, as an earlier session's quick reply left it: a `reply` (or `reply-all`) to Tomas Berg (or the addresses TO, comma-separated) |
//! | `assert-outbox NAME queued\|sending\|uncertain\|sent\|draft\|gone` | Where the draft stands in the store |
//! | `assert-outbox-entries N` | The outbox as the window last read it holds N entries |
//! | `assert-outbox-at NAME WHEN` | The store's outbox entry of NAME goes at WHEN, as the window says it (`tomorrow 08:00`, `Tue 08:00`; Send Later) |
//! | `stand-in-mark` | Notes the time, for `assert-stand-in saved-between` |
//! | `assert-stand-in sent\|discarded\|closed N` | The stand-in backend's count |
//! | `assert-stand-in saved-between MIN MAX` | A save came MIN to MAX ms after `stand-in-mark`; prints every save's time since |
//! | `assert-stand-in last-sent TEXT` | The stand-in's last sent draft starts with TEXT's lines |
//! | `assert-stand-in last-saved TEXT` | The stand-in's last saved draft starts with TEXT's lines |
//! | `assert-stand-in quick-draft MESSAGE TEXT\|none` | The stand-in's newest kept reply draft to MESSAGE: its body is TEXT (closed), or it was discarded |
//! | `assert-compose-draft NAME` | The latest `composer::open` request is `Draft(NAME)` |

use super::{Harness, StepResult, blocking, equal, number, setup, triage};
use crate::composer::{self, ComposeRequest};
use crate::outbox;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::compose::text::to_html;
use sealwax_domain::compose::{Draft, DraftSource, ReplyKind};
use sealwax_domain::model::{Address, DraftId, Flags, FolderRole};
use sealwax_engine::api::{Event, SendError, UncertainReason};
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{
    DraftSaved, Prepared, SaveDraft, SendFailed, SendFailure, SendQueued, SendState,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("outbox-draft", 2, 2),
    ("outbox-queue", 1, 2),
    ("outbox-name", 1, 1),
    ("outbox-copy", 1, 1),
    ("outbox-quick", 4, 4),
    ("outbox-engine", 2, 3),
    ("outbox-expect", 1, 2),
    ("toast-click", 1, 1),
    ("quick-store-draft", 2, 4),
    ("reply-draft-name", 2, 2),
    ("outbox-discard", 1, 1),
    ("assert-outbox", 2, 2),
    ("assert-outbox-entries", 1, 1),
    ("assert-outbox-at", 2, 2),
    ("assert-stand-in", 2, 3),
    ("stand-in-mark", 0, 0),
    ("assert-compose-draft", 1, 1),
];

thread_local! {
    /// The script's drafts by name, with their accounts.
    static DRAFTS: RefCell<HashMap<String, (DraftId, sealwax_domain::model::AccountId)>> =
        RefCell::new(HashMap::new());
    static MARK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

fn now_ms() -> i64 {
    glib::real_time() / 1000
}

pub(super) fn draft(name: &str) -> Result<(DraftId, sealwax_domain::model::AccountId), String> {
    DRAFTS
        .with(|drafts| drafts.borrow().get(name).copied())
        .ok_or_else(|| format!("no draft {name}; save it with outbox-draft"))
}

fn store(
    harness: &Harness,
) -> Result<std::sync::Arc<sealwax_infrastructure::store::Store>, String> {
    harness
        .state
        .mailbox()
        .shared_store()
        .ok_or_else(|| "the store is not open".to_owned())
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "outbox-draft" => save_draft(harness, arg(0), arg(1)).await,
        "outbox-queue" => {
            let later = args.get(1).map(|ms| number(ms)).transpose()?;
            queue(harness, arg(0), later.map(|ms| ms as i64)).await
        }
        "outbox-name" => {
            let entries = store(harness)?
                .outbox(None)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let newest = entries
                .iter()
                .max_by_key(|entry| entry.draft.0)
                .ok_or("the outbox is empty")?;
            println!("  # {} is draft {}", arg(0), newest.draft.0);
            DRAFTS.with(|drafts| {
                drafts
                    .borrow_mut()
                    .insert(arg(0).to_owned(), (newest.draft, newest.account))
            });
            Ok(())
        }
        "outbox-copy" => copy_draft(harness, arg(0)).await,
        "reply-draft-name" => {
            let message = super::conversation::id(arg(1))?;
            let stored = store(harness)?
                .reply_draft(message)
                .recv()
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("no reply draft to {}", arg(1)))?;
            println!("  # {} is draft {}", arg(0), stored.id.0);
            DRAFTS.with(|drafts| {
                drafts
                    .borrow_mut()
                    .insert(arg(0).to_owned(), (stored.id, stored.draft.account))
            });
            Ok(())
        }
        "outbox-discard" => {
            let (id, _) = draft(arg(0))?;
            let discarded = store(harness)?
                .discard_draft(id)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            match discarded.refused {
                None if discarded.found => Ok(()),
                None => Err(format!("{} wasn't in the store", arg(0))),
                Some(state) => Err(format!("{} wasn't discarded: {state:?}", arg(0))),
            }
        }
        "outbox-quick" => {
            save_draft(harness, arg(0), arg(1)).await?;
            let (id, _) = draft(arg(0))?;
            let message = super::conversation::id(arg(2))?;
            outbox::from_quick_reply(
                id,
                crate::reader::QuickDraft {
                    message,
                    kind: sealwax_domain::compose::ReplyKind::Reply,
                    text: arg(3).to_owned(),
                    recipients: None,
                    signature: None,
                },
            );
            queue(harness, arg(0), None).await
        }
        "outbox-engine" => engine(harness, args).await,
        "outbox-expect" => expect(harness, arg(0), args.get(1).map(String::as_str)).await,
        "toast-click" => {
            let button = toast_button(harness, arg(0))?;
            triage::click_button(harness, &button).await
        }
        "quick-store-draft" => {
            let kind = match args.get(2).map(String::as_str) {
                None | Some("reply") => ReplyKind::Reply,
                Some("reply-all") => ReplyKind::ReplyAll,
                Some(other) => return Err(format!("use reply or reply-all, not {other}")),
            };
            let to: Vec<Address> = match args.get(3) {
                Some(list) => list
                    .split(',')
                    .map(|email| Address::new("", email.trim()))
                    .collect(),
                None => vec![Address::new("Tomas Berg", "tomas@example.test")],
            };
            store_quick_draft(harness, arg(0), arg(1), kind, to).await
        }
        "assert-outbox" => {
            let (id, _) = draft(arg(0))?;
            let stored = store(harness)?
                .draft(id)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let state = match stored {
                None => "gone",
                Some(stored) => match stored.send {
                    None => "draft",
                    Some(SendState::Queued) => "queued",
                    Some(SendState::Sending) => "sending",
                    Some(SendState::Uncertain) => "uncertain",
                    Some(SendState::Sent) => "sent",
                },
            };
            equal(&format!("{}'s state", arg(0)), state, arg(1))
        }
        "assert-outbox-entries" => {
            equal("outbox entries", outbox::entries().len(), number(arg(0))?)
        }
        "assert-outbox-at" => {
            let (id, _) = draft(arg(0))?;
            let entries = store(harness)?
                .outbox(None)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let entry = entries
                .iter()
                .find(|entry| entry.draft == id)
                .ok_or_else(|| format!("{} isn't in the outbox", arg(0)))?;
            equal(
                &format!("when {} goes", arg(0)),
                crate::time_picker::describe(entry.not_before).as_str(),
                arg(1),
            )
        }
        "assert-stand-in" => stand_in(harness, args),
        "stand-in-mark" => {
            MARK.with(|mark| mark.set(Some(Instant::now())));
            Ok(())
        }
        "assert-compose-draft" => {
            let (id, _) = draft(arg(0))?;
            match composer::requests().last() {
                Some(ComposeRequest::Draft(opened)) => equal("draft opened", *opened, id),
                other => Err(format!("the latest request is {other:?}, not a draft")),
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// A fictional draft from the account named `account`, saved closed in the store.
async fn save_draft(harness: &Harness, name: &str, account: &str) -> StepResult {
    let account = harness
        .state
        .mailbox()
        .accounts()
        .into_iter()
        .find(|known| known.name == account)
        .ok_or_else(|| format!("no account {account}"))?;
    let draft = Draft {
        account: account.id,
        from: Address::new(account.name.clone(), account.address.clone()),
        to: vec![
            Address::new("Kari Nordmann", "kari@example.test"),
            Address::new("Ola Nordmann", "ola@example.test"),
        ],
        subject: format!("Notes {name}"),
        html: to_html("A fictional message from the self-test."),
        message_id: format!("{name}.{}@example.test", glib::uuid_string_random()),
        ..Draft::default()
    };
    let saved = store(harness)?
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: false,
            now: now_ms(),
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    let DraftSaved::Saved { id, .. } = saved else {
        return Err(format!("the draft wasn't saved: {saved:?}"));
    };
    println!("  # {name} is draft {}", id.0);
    DRAFTS.with(|drafts| {
        drafts
            .borrow_mut()
            .insert(name.to_owned(), (id, account.id))
    });
    Ok(())
}

/// The engine's upload of draft `name` to its account's Drafts folder, and the sync that brings
/// that copy into the store: a message with the draft's Message-ID there.
async fn copy_draft(harness: &Harness, name: &str) -> StepResult {
    let (id, account) = draft(name)?;
    let stored = store(harness)?
        .draft(id)
        .recv()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("no draft {name} in the store"))?;
    let mailbox = harness.state.mailbox();
    let folder = mailbox
        .folders()
        .into_iter()
        .find(|folder| folder.account == account && folder.role == FolderRole::Drafts)
        .ok_or("the draft's account has no Drafts folder")?
        .id;
    let draft = stored.draft;
    let date = glib::DateTime::now_local()
        .and_then(|now| now.format("%a, %d %b %Y %H:%M:%S %z"))
        .map_err(|error| error.to_string())?;
    let raw = format!(
        "Message-ID: <{}>\r\nFrom: {} <{}>\r\nTo: Kari Nordmann <kari@example.test>\r\n\
         Subject: {}\r\nDate: {date}\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nA fictional draft.\r\n",
        draft.message_id, draft.from.name, draft.from.email, draft.subject,
    )
    .into_bytes();
    let store = mailbox.shared_store().ok_or("the store is not open")?;
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let ingested = blocking(move || {
        ingest::ingest_raw(
            &store,
            &blobs,
            vec![RawMessage {
                folder,
                flags: Flags::SEEN.union(Flags::DRAFT),
                raw,
                ..RawMessage::default()
            }],
        )
        .map_err(|error| error.to_string())
    })
    .await??;
    let copy = ingested
        .ids
        .first()
        .copied()
        .flatten()
        .ok_or("the store already held the copy")?;
    println!("  # {name}'s copy is message {}", copy.0);
    super::mail::settled(harness).await
}

/// What the backend's send does: the store's queue for the undo window, then the window's toast.
async fn queue(harness: &Harness, name: &str, later: Option<i64>) -> StepResult {
    let (id, _) = draft(name)?;
    let window = outbox::send_window(&harness.state).as_millis() as i64;
    let not_before = now_ms() + later.unwrap_or(window);
    let queued = store(harness)?
        .queue_send(id, not_before)
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    let SendQueued::Queued { account, .. } = queued else {
        return Err(format!("not queued: {queued:?}"));
    };
    match later {
        Some(_) => outbox::scheduled(&harness.state, id, account, not_before),
        None => outbox::queued(&harness.state, id, account),
    }
    harness.wait_until_settled().await;
    Ok(())
}

fn send_error(name: &str) -> Result<(SendError, SendFailure), String> {
    Ok(match name {
        "recipients-refused" => (
            SendError::RecipientsRefused(vec![
                "kari@example.test".into(),
                "ola@example.test".into(),
            ]),
            SendFailure::RecipientsRefused,
        ),
        "too-large" => (
            SendError::TooLarge {
                limit: Some(26_214_400),
            },
            SendFailure::TooLarge,
        ),
        "refused" => (SendError::Refused, SendFailure::Refused),
        "needs-sign-in" => (SendError::NeedsSignIn, SendFailure::NeedsSignIn),
        "other" => (SendError::Other, SendFailure::Other),
        "interrupted" => (SendError::Interrupted, SendFailure::Interrupted),
        other => return Err(format!("no send error {other}")),
    })
}

/// The engine's part, played on the store and through the fake engine's events.
async fn engine(harness: &Harness, args: &[String]) -> StepResult {
    let (id, account) = draft(&args[1])?;
    let store = store(harness)?;
    let applied = |recorded: Result<sealwax_infrastructure::store::SendRecorded, _>| {
        let recorded = recorded
            .map_err(|error: sealwax_infrastructure::store::StoreError| error.to_string())?;
        if recorded.applied {
            Ok(())
        } else {
            Err(format!("the store didn't apply it: {recorded:?}"))
        }
    };
    match args[0].as_str() {
        "start" => {
            // A send started before (sent again after it was uncertain) sends its recorded bytes.
            let due = store
                .due_sends(account, now_ms())
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let recorded = due
                .sends
                .into_iter()
                .find(|send| send.draft == id)
                .and_then(|send| send.prepared);
            let prepared = match recorded {
                Some(prepared) => prepared,
                None => {
                    let blobs = harness
                        .state
                        .mailbox()
                        .blobs()
                        .ok_or("the blob store is not open")?;
                    let message_blob = blocking(move || {
                        blobs
                            .put(b"Subject: A fictional message\r\n\r\nFrom the self-test.\r\n")
                            .map_err(|error| error.to_string())
                    })
                    .await??;
                    Prepared {
                        message_blob,
                        sent_copy_blob: None,
                        size: 60,
                        envelope_from: "work@example.test".into(),
                        envelope_to: vec!["kari@example.test".into(), "ola@example.test".into()],
                    }
                }
            };
            let started = store
                .send_started(id, prepared, now_ms())
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            println!("  # started: {started:?}");
            Ok(())
        }
        "sent" => {
            applied(store.send_done(id, now_ms()).recv().await)?;
            setup::emit(harness, Event::Sent { account, draft: id }).await
        }
        "uncertain" => {
            let reason = match args.get(2).map(String::as_str) {
                None | Some("stopped") => UncertainReason::Stopped,
                Some("no-reply") => UncertainReason::NoReply,
                Some("unreadable") => UncertainReason::UnreadableReply,
                Some(other) => return Err(format!("no reason {other}")),
            };
            applied(store.send_uncertain(id).recv().await)?;
            setup::emit(
                harness,
                Event::SendUncertain {
                    account,
                    draft: id,
                    reason,
                },
            )
            .await
        }
        "failed" => {
            let name = args.get(2).ok_or("failed takes an error")?;
            let (error, failure) = send_error(name)?;
            applied(
                store
                    .send_failed(id, SendFailed::Permanent(failure))
                    .recv()
                    .await,
            )?;
            setup::emit(
                harness,
                Event::SendFailed {
                    account,
                    draft: id,
                    error,
                },
            )
            .await
        }
        other => Err(format!("use start, sent, failed or uncertain, not {other}")),
    }
}

/// Waits up to a second for the app to send `kind` to the engine; other commands are passed
/// over (and printed).
async fn expect(harness: &Harness, kind: &str, field: Option<&str>) -> StepResult {
    let started = Instant::now();
    loop {
        while let Some(command) = setup::next_command()? {
            let matched = command.split_whitespace().next() == Some(kind)
                && field.is_none_or(|field| command.split_whitespace().any(|part| part == field));
            if matched {
                println!("  # {command}");
                return Ok(());
            }
            println!("  # passed over: {command}");
        }
        if started.elapsed() > Duration::from_secs(1) {
            return Err(format!("the app sent no {kind}"));
        }
        glib::timeout_future(Duration::from_millis(20)).await;
        harness.wait_until_settled().await;
    }
}

fn toast_button(harness: &Harness, label: &str) -> Result<gtk::Button, String> {
    let toast = glib::Type::from_name("AdwToastWidget").ok_or("no toast is shown")?;
    let mut found = None;
    super::collect(harness.window()?.window().upcast_ref(), &mut |widget| {
        if found.is_none()
            && let Some(button) = widget.downcast_ref::<gtk::Button>()
            && button.is_mapped()
            && button.ancestor(toast).is_some()
            && button.label().as_deref() == Some(label)
        {
            found = Some(button.clone());
        }
    });
    found.ok_or_else(|| format!("no toast offers {label}"))
}

/// A closed reply draft to `message` with `text` as its body, in the store: what an earlier
/// session's quick reply left.
async fn store_quick_draft(
    harness: &Harness,
    message: &str,
    text: &str,
    kind: ReplyKind,
    to: Vec<Address>,
) -> StepResult {
    let id = super::conversation::id(message)?;
    let account = harness
        .state
        .mailbox()
        .accounts()
        .into_iter()
        .next()
        .ok_or("there is no account")?;
    let draft = Draft {
        account: account.id,
        from: Address::new(account.name.clone(), account.address.clone()),
        to,
        subject: "Re: Cabin trip in November?".into(),
        html: to_html(text),
        message_id: format!("quick.{}@example.test", glib::uuid_string_random()),
        source: Some(DraftSource { message: id, kind }),
        ..Draft::default()
    };
    let saved = store(harness)?
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: false,
            now: now_ms(),
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    println!("  # {saved:?}");
    Ok(())
}

fn stand_in(harness: &Harness, args: &[String]) -> StepResult {
    let stand_in =
        composer::ensure_stand_in(&harness.app).ok_or("the backend isn't the stand-in")?;
    let arg = |index: usize| args.get(index).map(String::as_str).unwrap_or_default();
    match arg(0) {
        "sent" => equal("sent", stand_in.sent().len(), number(arg(1))?),
        "discarded" => equal("discarded", stand_in.discarded().len(), number(arg(1))?),
        "closed" => equal("closed", stand_in.closed().len(), number(arg(1))?),
        "saved-between" => {
            let mark = MARK
                .with(std::cell::Cell::get)
                .ok_or("run stand-in-mark first")?;
            let (low, high) = (number(arg(1))? as u128, number(arg(2))? as u128);
            let since: Vec<u128> = stand_in
                .saves()
                .iter()
                .filter(|(_, at)| *at >= mark)
                .map(|(_, at)| at.duration_since(mark).as_millis())
                .collect();
            println!("  # saves after the mark, ms: {since:?}");
            if since.iter().any(|ms| (low..=high).contains(ms)) {
                Ok(())
            } else {
                Err(format!("no save {low}–{high} ms after the mark"))
            }
        }
        "last-saved" => {
            let (id, _) = stand_in.saves().pop().ok_or("nothing was saved")?;
            let saved = stand_in.draft(id).ok_or("the last saved draft is gone")?;
            if saved.html.starts_with(&to_html(arg(1))) {
                Ok(())
            } else {
                Err(format!(
                    "the last saved draft's body doesn't start with the text ({} bytes)",
                    saved.html.len()
                ))
            }
        }
        "last-sent" => {
            let (_, sent) = stand_in.sent().pop().ok_or("nothing was sent")?;
            let typed = to_html(arg(1));
            if sent.html.starts_with(&typed) {
                Ok(())
            } else {
                Err(format!(
                    "the sent draft's body doesn't start with the text ({} bytes)",
                    sent.html.len()
                ))
            }
        }
        "quick-draft" => {
            let message = super::conversation::id(arg(1))?;
            let wanted = arg(2);
            let kept = stand_in.saves().iter().rev().map(|(id, _)| *id).find(|id| {
                stand_in.draft(*id).is_some_and(|draft| {
                    draft.source.is_some_and(|source| source.message == message)
                }) || stand_in.discarded().contains(id)
            });
            let Some(id) = kept else {
                return equal("a kept draft", "none", wanted);
            };
            match stand_in.draft(id) {
                None => equal("the kept draft", "none", wanted),
                Some(draft) => {
                    equal(
                        "the kept draft's body",
                        draft.html.as_str(),
                        to_html(wanted).as_str(),
                    )?;
                    if stand_in.closed().contains(&id) {
                        Ok(())
                    } else {
                        Err("the kept draft wasn't closed".to_owned())
                    }
                }
            }
        }
        other => Err(format!(
            "use sent, discarded, closed, last-saved, last-sent or quick-draft, not {other}"
        )),
    }
}
