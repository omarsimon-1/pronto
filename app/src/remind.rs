//! Remind Me's UI (the fork's phase C, docs/superhuman/remind.md "UI"): `h` on the selected
//! conversations (in the list, or the one open) opens the time picker titled "Remind Me"; a
//! choice sends them to the account's Reminders folder until then (the store's `remind`, one Undo
//! batch, the moves after the undo window), so they leave the list at once and the list moves on,
//! and a toast says when ("Reminder set for tomorrow 08:00 · Undo"). On a conversation waiting
//! already, the picker changes its time and offers Remove Reminder, which Shift+E (Superhuman's
//! Mark Not Done) and the palette run too: a waiting one comes back to its Inbox now, a returned
//! one stops sorting by its return; Undo restores either.
//!
//! The reminders that count are read from the store whenever a change event says they changed
//! ([`install`]): the list's rows take Remind Me's marks from them (a clock in an Inbox on a
//! conversation back from Reminders; the wake times in the Reminders view's date column), the
//! reader its quiet line, the commands their availability, and a conversation that came back
//! notifies ("Reminder: …", `notify::reminders`) once it has a row in an Inbox again (a server
//! account's moves back run after the wake: [`Known::awaited`]). The Reminders view is the
//! shown account's Reminders folder (`g h`, the palette, the sidebar's row under the Inbox),
//! whose waiting conversations the store sorts by when they are due.

use crate::application::AppState;
use crate::list::MessageList;
use crate::list::model::Content;
use crate::list::row::RowReminders;
use crate::time_picker::{self, Reminding};
use crate::triage::Notice;
use gtk4::glib;
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::{AccountId, Folder, FolderId, FolderRole, ThreadId};
use sealwax_domain::remind::{self, ReminderState};
use sealwax_domain::when::Preset;
use sealwax_engine::api::Command;
use sealwax_infrastructure::store::{
    ChangeEvent, ListRow, Remind, Reminder, ReminderId, RowKey, ViewGrouping, ViewScope, ViewSpec,
};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The commands this module runs.
pub const COMMANDS: [ActionId; 3] = [
    ActionId::RemindMe,
    ActionId::RemoveReminder,
    ActionId::GoToReminders,
];

/// What Go to Reminders says while no account has a Reminders folder.
pub const NONE_YET: &str = "No reminders yet. Press h on a conversation to set one.";

/// How long a reminder that came back waits to notify for its conversation's rows in an Inbox
/// ([`Known::awaited`]): a server account's moves back can wait for the connection (an
/// offline spell, the engine's start), and one whose moves never land (refused, or its mail
/// deleted elsewhere meanwhile) stops being looked for.
const BACK_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// A reminder back from Reminders whose notification is still to come ([`Known::awaited`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Awaited {
    /// When it was seen back.
    since: Instant,
    /// Back before this session began, seen at its first read ([`back_before`]): it notifies
    /// only if its conversation isn't in an Inbox at the first look, as then its moves back
    /// never landed and it never notified (an earlier session ended between its wake and the
    /// server's moves: offline at its time, then quit). One in an Inbox then did, or was seen.
    unsure: bool,
}

#[derive(Default)]
struct Known {
    /// Read at least once: a reminder seen waiting before and reminded now notifies.
    read: bool,
    /// When this session began (Unix seconds, [`install`]): the first read notifies what came
    /// back since, which the engine's start can wake before that read ([`came_back`]).
    since: i64,
    /// A read is under way (with the notifications it makes); `again` asks for one more after
    /// it.
    reading: bool,
    again: bool,
    /// The reminders that count (waiting and reminded), the soonest due first.
    reminders: Vec<Reminder>,
    /// The reminders that came back and haven't notified yet, with when they were seen back.
    /// The notification names the conversation's newest message in an Inbox, and on a server
    /// account the wake only queues the moves back: until the server has made them, the
    /// messages wait hidden in Reminders and no Inbox has a row of it (the third review: on the
    /// owner's Gmail it never notified, at runtime or at startup). So it waits until the
    /// conversation is back in an Inbox: a change event that touches an Inbox of one of these
    /// accounts reads again ([`install`]), until it is back, its reminder ends, or [`BACK_WAIT`]
    /// passes. A local account's moves are made with the wake, so its reminders notify at once.
    awaited: HashMap<ReminderId, Awaited>,
}

thread_local! {
    static KNOWN: RefCell<Known> = RefCell::default();
}

/// Reads the reminders once the store is ready, and again whenever a change event says they
/// changed, or touches an Inbox where a conversation back from Reminders is awaited
/// ([`Known::awaited`]). Once at startup, before the engine starts.
pub fn install(state: &Rc<AppState>) {
    KNOWN.with(|known| known.borrow_mut().since = now_ms().div_euclid(1000));
    let weak = Rc::downgrade(state);
    let ready = state.mailbox().wait_ready();
    glib::spawn_future_local(async move {
        ready.await;
        if let Some(state) = weak.upgrade() {
            refresh(&state);
        }
    });
    let weak = Rc::downgrade(state);
    state.mailbox().connect_event(move |event| {
        if let Some(state) = weak.upgrade()
            && (event.reminders || lands_awaited(&state, event))
        {
            refresh(&state);
        }
    });
}

/// Whether `event` touched an Inbox of an account whose returned conversation is still to
/// notify ([`Known::awaited`]): its rows may have landed there (the server's moves back, or the
/// Inbox's sync where the server gives no new UIDs).
fn lands_awaited(state: &AppState, event: &ChangeEvent) -> bool {
    if event.folders.is_empty() {
        return false;
    }
    let accounts: Vec<AccountId> = KNOWN.with(|known| {
        let known = known.borrow();
        if known.awaited.is_empty() {
            return Vec::new();
        }
        known
            .reminders
            .iter()
            .filter(|reminder| known.awaited.contains_key(&reminder.id))
            .map(|reminder| reminder.account)
            .collect()
    });
    !accounts.is_empty()
        && event.folders.iter().any(|counts| {
            state.mailbox().folder(counts.folder).is_some_and(|folder| {
                folder.role == FolderRole::Inbox && accounts.contains(&folder.account)
            })
        })
}

/// Reads the reminders from the store and takes them in, then notifies what is back; one read
/// at a time (so a conversation never notifies twice), another after it when asked meanwhile.
pub fn refresh(state: &Rc<AppState>) {
    let busy = KNOWN.with(|known| {
        let mut known = known.borrow_mut();
        if known.reading {
            known.again = true;
        }
        std::mem::replace(&mut known.reading, true)
    });
    if busy {
        return;
    }
    let Some(store) = state.mailbox().shared_store() else {
        KNOWN.with(|known| known.borrow_mut().reading = false);
        return;
    };
    let reply = store.reminders();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let read = reply.recv().await;
        let notifying = match read {
            Ok(reminders) => weak.upgrade().map(|state| {
                let back = apply(&state, reminders);
                notify_back(&state, back)
            }),
            Err(error) => {
                tracing::warn!("the reminders couldn't be read: {error}");
                None
            }
        };
        if let Some(notifying) = notifying {
            notifying.await;
        }
        let again = KNOWN.with(|known| {
            let mut known = known.borrow_mut();
            known.reading = false;
            std::mem::take(&mut known.again)
        });
        if again && let Some(state) = weak.upgrade() {
            refresh(&state);
        }
    });
}

/// Takes in what the store holds now: the list's marks, the reader's line, the commands, and
/// the notifications of conversations no longer back, which go. Replies with the reminders
/// whose notification is still to come: those back since the last read, and those back earlier
/// whose conversation had no row in an Inbox yet ([`Known::awaited`]), each with whether it is
/// [`Awaited::unsure`].
fn apply(state: &Rc<AppState>, reminders: Vec<Reminder>) -> Vec<(Reminder, bool)> {
    let awaited = KNOWN.with(|known| {
        let mut known = known.borrow_mut();
        let before: HashMap<_, _> = known
            .reminders
            .iter()
            .map(|reminder| (reminder.id, reminder.state))
            .collect();
        let read = std::mem::replace(&mut known.read, true);
        let since = known.since;
        let now = Instant::now();
        for back in came_back(read.then_some(&before), since, &reminders) {
            known.awaited.entry(back.id).or_insert(Awaited {
                since: now,
                unsure: false,
            });
        }
        if !read {
            for id in back_before(since, &reminders) {
                known.awaited.entry(id).or_insert(Awaited {
                    since: now,
                    unsure: true,
                });
            }
        }
        let awaited = still_awaited(&mut known.awaited, &reminders, now);
        known.reminders = reminders;
        awaited
    });
    update_list(state);
    if let Some(reader) = state.reader() {
        reader.reminders_changed();
    }
    state.refresh_commands();
    let back: HashSet<ThreadId> = KNOWN.with(|known| {
        known
            .borrow()
            .reminders
            .iter()
            .filter(|reminder| reminder.state == ReminderState::Reminded)
            .flat_map(|reminder| reminder.threads.iter().copied())
            .collect()
    });
    crate::notify::reminders_now(&back);
    awaited
}

/// Of `awaited`, those still back in `now_read` and seen back less than [`BACK_WAIT`] before
/// `now`, as `now_read` has them (its threads now), each with whether it is unsure; the others
/// leave `awaited`: their reminders ended (Done, Remove Reminder) or went, or waited too long.
fn still_awaited(
    awaited: &mut HashMap<ReminderId, Awaited>,
    now_read: &[Reminder],
    now: Instant,
) -> Vec<(Reminder, bool)> {
    awaited.retain(|id, back| {
        now.saturating_duration_since(back.since) < BACK_WAIT
            && now_read
                .iter()
                .any(|reminder| reminder.id == *id && reminder.state == ReminderState::Reminded)
    });
    now_read
        .iter()
        .filter_map(|reminder| {
            let back = awaited.get(&reminder.id)?;
            Some((reminder.clone(), back.unsure))
        })
        .collect()
}

/// At the first read, the reminders back since before this session began (`since`, Unix
/// seconds; [`came_back`] has those back since): [`Awaited::unsure`] ones.
fn back_before(since: i64, now: &[Reminder]) -> Vec<ReminderId> {
    now.iter()
        .filter(|reminder| {
            reminder.state == ReminderState::Reminded
                && reminder.reminded_at.is_none_or(|at| at < since)
        })
        .map(|reminder| reminder.id)
        .collect()
}

/// The reminders of `now` that came back since the last read (`before`: each one's state
/// then), which notify: those seen waiting then and reminded now. At the first read (`None`),
/// those reminded since this session began (`since`, Unix seconds): the engine wakes the
/// overdue ones at its start, maybe before that read, so a reminder due while Pronto wasn't
/// running notifies at the next start, as new mail that came meanwhile does (the second review).
fn came_back(
    before: Option<&HashMap<ReminderId, ReminderState>>,
    since: i64,
    now: &[Reminder],
) -> Vec<Reminder> {
    now.iter()
        .filter(|reminder| reminder.state == ReminderState::Reminded)
        .filter(|reminder| match before {
            Some(before) => before.get(&reminder.id) == Some(&ReminderState::Waiting),
            None => reminder.reminded_at.is_some_and(|at| at >= since),
        })
        .cloned()
        .collect()
}

/// The reader shows the list's selection, on screen: the reminder notifications of those
/// conversations go, as a new message's once it is read (the final review of phase C).
pub fn seen(state: &AppState) {
    if !crate::notify::reminders_shown() {
        return;
    }
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return;
    };
    if window.reader_on_screen()
        && let Ok(rows) = list.selected_rows()
    {
        let threads: Vec<ThreadId> = rows.iter().map(thread_of).collect();
        crate::notify::reminders_seen(&threads);
    }
}

/// The reminders that count, as last read (the self-test).
pub fn reminders() -> Vec<Reminder> {
    KNOWN.with(|known| known.borrow().reminders.clone())
}

/// Begins the reminders' session anew, as at startup: it began at `since` (Unix seconds), and
/// the read this starts is its first (the self-test's `remind-new-session`: a script can't
/// start Pronto again).
pub fn new_session(state: &Rc<AppState>, since: i64) {
    KNOWN.with(|known| {
        let mut known = known.borrow_mut();
        known.read = false;
        known.since = since;
        known.reminders.clear();
        known.awaited.clear();
    });
    refresh(state);
}

/// Whether a read of the reminders, with the notifications it makes, is under way (the
/// self-test).
pub fn reading() -> bool {
    KNOWN.with(|known| known.borrow().reading)
}

/// The reminder of `thread`'s conversation, if one counts.
pub fn of_thread(thread: ThreadId) -> Option<Reminder> {
    KNOWN.with(|known| {
        known
            .borrow()
            .reminders
            .iter()
            .find(|reminder| reminder.threads.contains(&thread))
            .cloned()
    })
}

/// When `thread`'s conversation comes back, while it waits in Reminders.
fn waiting(thread: ThreadId) -> Option<i64> {
    of_thread(thread)
        .filter(|reminder| reminder.state == ReminderState::Waiting)
        .map(|reminder| reminder.wake_at)
}

/// The reader's quiet line for `thread`'s conversation while it waits: "Reminder set for
/// tomorrow 08:00", "Reminder set for someday".
pub fn line_text(thread: ThreadId) -> Option<String> {
    waiting(thread).map(|wake| format!("Reminder set for {}", time_picker::describe_wake(wake)))
}

/// The account's Reminders folder among `folders` (`remind::is_folder`), if it has one.
pub fn reminders_folder(account: AccountId, folders: &[Folder]) -> Option<FolderId> {
    let listed: Vec<(&str, Option<char>)> = folders
        .iter()
        .filter(|folder| folder.account == account)
        .map(|folder| (folder.name.as_str(), folder.delimiter))
        .collect();
    folders
        .iter()
        .filter(|folder| folder.account == account && folder.selectable)
        .find(|folder| remind::is_folder(&folder.name, &listed))
        .map(|folder| folder.id)
}

/// Whether `folder` is its account's Reminders folder.
pub fn is_reminders_folder(folder: FolderId, folders: &[Folder]) -> bool {
    folders
        .iter()
        .find(|known| known.id == folder)
        .is_some_and(|known| reminders_folder(known.account, folders) == Some(folder))
}

/// What the rows of a list showing `scope` (`None`: nothing, or search results) in `grouping`
/// show of Remind Me: in an Inbox or All Inboxes the clock on the conversations of its
/// accounts back from Reminders (the clock's slot only while one is: the final review), in a
/// Reminders folder's conversation rows when each comes back, and how many wait there (on
/// their way while it lists none). Message rows there keep their dates, which their order
/// follows. In Sent, Drafts, Trash and Junk the rows' clock quick action is dimmed and says why
/// ([`unavailable`]).
pub fn row_reminders(
    scope: Option<ViewScope>,
    grouping: ViewGrouping,
    folders: &[Folder],
) -> RowReminders {
    // An Inbox view, with the account it shows (`None`: All Inboxes, every account).
    let inbox: Option<Option<AccountId>> = match scope {
        Some(ViewScope::UnifiedInbox) => Some(None),
        Some(ViewScope::Folder(folder)) => folders
            .iter()
            .find(|known| known.id == folder && known.role == FolderRole::Inbox)
            .map(|known| Some(known.account)),
        None => None,
    };
    let wakes = grouping == ViewGrouping::Conversations
        && matches!(scope, Some(ViewScope::Folder(folder)) if is_reminders_folder(folder, folders));
    let arriving = match scope {
        Some(ViewScope::Folder(folder)) if wakes => KNOWN.with(|known| {
            known
                .borrow()
                .reminders
                .iter()
                .filter(|reminder| {
                    reminder.state == ReminderState::Waiting && reminder.folder == Some(folder)
                })
                .count()
        }),
        _ => 0,
    };
    let returned: HashSet<ThreadId> = match inbox {
        Some(shown) => KNOWN.with(|known| {
            known
                .borrow()
                .reminders
                .iter()
                .filter(|reminder| {
                    reminder.state == ReminderState::Reminded
                        && shown.is_none_or(|account| reminder.account == account)
                })
                .flat_map(|reminder| reminder.threads.iter().copied())
                .collect()
        }),
        None => HashSet::new(),
    };
    RowReminders {
        marks: !returned.is_empty(),
        wakes,
        returned,
        off: scope.and_then(|scope| unavailable_in(scope, folders)),
        arriving,
    }
}

/// Gives the list's rows what they show of Remind Me now.
pub fn update_list(state: &AppState) {
    if let Some(list) = state.list() {
        let scope = list.query().is_empty().then(|| list.scope()).flatten();
        list.set_reminders(row_reminders(
            scope,
            list.grouping(),
            &state.mailbox().folders(),
        ));
    }
}

/// A row's conversation.
fn thread_of(row: &ListRow) -> ThreadId {
    match row {
        ListRow::Message(summary) => summary.thread,
        ListRow::Conversation(summary) => summary.thread,
    }
}

/// The selected rows' conversations, each once.
fn threads(rows: &[ListRow]) -> Vec<ThreadId> {
    let mut seen = BTreeSet::new();
    rows.iter()
        .map(thread_of)
        .filter(|thread| seen.insert(*thread))
        .collect()
}

/// Connects the commands on the main window's action set.
pub fn install_commands(state: &Rc<AppState>, actions: &crate::actions::ActionSet) {
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::RemindMe, move || {
        if let Some(state) = weak.upgrade() {
            with_selected(&state, ask);
        }
    });
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::RemoveReminder, move || {
        if let Some(state) = weak.upgrade() {
            with_selected(&state, remove);
        }
    });
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::GoToReminders, move || {
        if let Some(state) = weak.upgrade() {
            go_to(&state);
        }
    });
    if let Some(list) = state.list()
        && let Some(keys) = list.keys()
    {
        explain_keys(state, &keys);
    }
}

/// Has `keys` (the list's, the reader's) say why Remind Me is off where nothing can be
/// reminded: `h` there shows the reason as a row's clock does, where a key whose command is
/// off does nothing (the second review: a keyboard-first user heard nothing).
pub fn explain_keys(state: &Rc<AppState>, keys: &crate::actions::ScopedShortcuts) {
    let weak = Rc::downgrade(state);
    keys.connect_refused(move |id| {
        id == ActionId::RemindMe && weak.upgrade().is_some_and(|state| explain(&state))
    });
}

/// Says why nothing the list shows can be reminded, in a toast ("Mail in Sent can't be
/// reminded"), when that is so.
fn explain(state: &Rc<AppState>) -> bool {
    let reason = state
        .list()
        .and_then(|list| unavailable(&list, &state.mailbox().folders()));
    if let Some(reason) = reason {
        state.show_notice(&Notice::plain(sentence(reason)), true);
    }
    reason.is_some()
}

/// Runs `then` on the selected rows; selected rows far down a range load first.
fn with_selected(state: &Rc<AppState>, then: fn(&Rc<AppState>, Vec<ListRow>)) {
    let Some(list) = state.list() else {
        return;
    };
    match list.selected_rows() {
        Ok(rows) if rows.is_empty() => {}
        Ok(rows) => then(state, rows),
        Err(_) => {
            let weak = Rc::downgrade(state);
            glib::spawn_future_local(async move {
                let rows = list.wait_rows().await;
                if let (Some(state), Some(rows)) = (weak.upgrade(), rows) {
                    then(&state, rows);
                }
            });
        }
    }
}

/// `h`: Remind Me's picker for `rows`; one waiting already can be given a new time or removed.
/// Where nothing can be reminded (a row's clock in Sent, say) a toast says why instead. A
/// search of every folder sends only its conversations' Inbox mail away, so there the rows
/// with none (and no reminder waiting) are left out first, and when that is all of them the
/// toast says so before any picker (the third review: the picker, then the toast).
pub fn ask(state: &Rc<AppState>, rows: Vec<ListRow>) {
    if explain(state) {
        return;
    }
    let everywhere = state.list().is_some_and(|list| searches_everywhere(&list));
    let store = state.mailbox().shared_store();
    let (true, Some(store)) = (everywhere, store) else {
        pick(state, rows);
        return;
    };
    let reply = store.conversation_members(threads(&rows), ViewSpec::unified_inbox());
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let members = reply.recv().await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let in_inbox: HashSet<ThreadId> = match members {
            Ok(members) => members.value.iter().map(|member| member.thread).collect(),
            Err(error) => {
                tracing::warn!("a search's conversations couldn't be read: {error}");
                return;
            }
        };
        let rows: Vec<ListRow> = rows
            .into_iter()
            .filter(|row| {
                let thread = thread_of(row);
                in_inbox.contains(&thread) || waiting(thread).is_some()
            })
            .collect();
        if rows.is_empty() {
            let text = nothing_to_remind(&ViewScope::UnifiedInbox).to_owned();
            state.show_notice(&Notice::plain(text), true);
        } else {
            pick(&state, rows);
        }
    });
}

/// Opens Remind Me's picker for `rows`.
fn pick(state: &Rc<AppState>, rows: Vec<ListRow>) {
    let Some(window) = state.window() else {
        return;
    };
    let removing = removing(&threads(&rows));
    let weak = Rc::downgrade(state);
    time_picker::pick_reminder(window.window(), &Preset::ALL, removing, move |choice| {
        if let Some(state) = weak.upgrade() {
            match choice {
                Reminding::Remove => remove(&state, rows),
                choice => remind_rows(&state, rows, &choice),
            }
        }
    });
}

/// What the picker's Remove Reminder says of the waiting conversations among `threads`: when
/// they come back ("set for Tue 08:00"), or how many when they come back at different times
/// ("3 reminders"); nothing when none waits.
fn removing(threads: &[ThreadId]) -> Option<String> {
    let mut waiting: Vec<Reminder> = threads
        .iter()
        .filter_map(|thread| of_thread(*thread))
        .filter(|reminder| reminder.state == ReminderState::Waiting)
        .collect();
    waiting.sort_by_key(|reminder| reminder.id);
    waiting.dedup_by_key(|reminder| reminder.id);
    let first = waiting.first()?;
    if waiting
        .iter()
        .all(|reminder| reminder.wake_at == first.wake_at)
    {
        Some(format!(
            "set for {}",
            time_picker::describe_wake(first.wake_at)
        ))
    } else {
        Some(format!("{} reminders", waiting.len()))
    }
}

/// Whether the list shows a search of every folder (from All Inboxes), whose remind sends only
/// Inbox mail away.
fn searches_everywhere(list: &MessageList) -> bool {
    matches!(list.model().content(), Content::Search { folder: None, .. })
}

/// Unix milliseconds now.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// The undo window: the triage controller's.
fn window(state: &AppState) -> Duration {
    state
        .triage()
        .map_or(crate::triage::UNDO_WINDOW, |triage| triage.window())
}

/// Runs `work` on the main loop, counted as the triage controller's actions are while they wait
/// for the store (the self-test's `settled` waits for them too).
fn spawn(state: &AppState, work: impl std::future::Future<Output = ()> + 'static) {
    match state.triage() {
        Some(triage) => triage.spawn(work),
        None => {
            glib::spawn_future_local(work);
        }
    }
}

/// A change Undo can't take back (a new time for a waiting conversation) is the last action now
/// (ADR-015), as its toast takes the last one's place: the action before it can't be undone any
/// more, by `z`, Ctrl+Z or the palette, as a send's or a list change's Undo takes over
/// (`outbox::announce`, `rules::remember`; the second review: `z` after a new time brought back
/// an archived conversation). What changes nothing ("Nothing here to remind", a removal of
/// reminders that had ended already) leaves the last action as it was.
fn forget_last(state: &AppState) {
    if let Some(triage) = state.triage() {
        triage.forget_last();
    }
    crate::outbox::forget_last();
    crate::rules::forget_last();
    state.refresh_commands();
}

/// Tells the engine: the moves of `accounts` are queued, and the reminders changed.
fn tell_engine(state: &AppState, accounts: &[AccountId]) {
    let engine = state.engine();
    for account in accounts {
        let _ = engine.send(Command::OpsQueued { account: *account });
    }
    let _ = engine.send(Command::RemindersChanged);
}

/// The folders whose messages a remind from the list sends away: the view's (an Inbox, All
/// Inboxes, the Archive, the Reminders folder itself for a new time), a search's folder, or for a
/// search of every folder the Inboxes.
fn scope(state: &AppState) -> ViewScope {
    state
        .list()
        .map_or(ViewScope::UnifiedInbox, |list| list_scope(&list))
}

fn list_scope(list: &MessageList) -> ViewScope {
    match list.model().content() {
        Content::View(spec) => spec.scope,
        Content::Search {
            folder: Some(folder),
            ..
        } => ViewScope::Folder(folder),
        _ => ViewScope::UnifiedInbox,
    }
}

/// Why Remind Me has nothing to send away from what the list shows, when it hasn't: a
/// conversation's copies in Sent, Drafts, Trash and Junk never go to Reminders (the store's
/// `remind`), so there the picker would only end in "Nothing here to remind" (the final
/// review of phase C). There Remind Me is off, and its buttons say why.
pub fn unavailable(list: &MessageList, folders: &[Folder]) -> Option<&'static str> {
    unavailable_in(list_scope(list), folders)
}

fn unavailable_in(scope: ViewScope, folders: &[Folder]) -> Option<&'static str> {
    let ViewScope::Folder(folder) = scope else {
        return None;
    };
    match folders.iter().find(|known| known.id == folder)?.role {
        FolderRole::Sent => Some("mail in Sent can't be reminded"),
        FolderRole::Drafts => Some("drafts can't be reminded"),
        FolderRole::Trash => Some("mail in Trash can't be reminded"),
        FolderRole::Junk => Some("mail in Junk can't be reminded"),
        _ => None,
    }
}

/// A reason as a toast says it: "Mail in Sent can't be reminded".
fn sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Sends `rows`' conversations to Reminders until `choice`: they leave the list at once and the
/// list moves on to the row after them; the toast says when, and its Undo (or `z`) takes the
/// moves and the reminders back. A conversation waiting already gets the new time; that is the
/// last action, with nothing for Undo to take back (the store keeps no earlier time).
pub fn remind_rows(state: &Rc<AppState>, rows: Vec<ListRow>, choice: &Reminding) {
    let (Some(list), Some(store), Some(wake_at)) = (
        state.list(),
        state.mailbox().shared_store(),
        choice.wake_at(),
    ) else {
        return;
    };
    let threads = threads(&rows);
    let scope = scope(state);
    // The rows' mail in a folder's list goes to Reminders (a waiting one's that came there
    // meanwhile too), so they leave it and the cursor moves on past them. Nothing leaves the
    // Reminders view, where a remind moves nothing, nor a search of every folder, which finds
    // the mail in Reminders as well (the third review: the cursor moved on from rows that
    // stayed).
    let in_reminders = matches!(scope, ViewScope::Folder(folder)
        if is_reminders_folder(folder, &state.mailbox().folders()));
    if !in_reminders && !searches_everywhere(&list) {
        let keys: Vec<RowKey> = rows.iter().map(ListRow::key).collect();
        list.advance_after(&keys);
    }
    let reply = store.remind(Remind {
        threads,
        scope,
        wake_at,
        not_before: now_ms() + window(state).as_millis() as i64,
    });
    let label = choice.label();
    let weak = Rc::downgrade(state);
    spawn(state, async move {
        let reminded = reply.recv().await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let reminded = match reminded {
            Ok(reminded) => reminded,
            Err(error) => {
                tracing::warn!("a reminder wasn't saved: {error}");
                state.show_notice(
                    &Notice::plain("The reminder couldn't be saved in the mail store".into()),
                    true,
                );
                return;
            }
        };
        tell_engine(&state, &reminded.accounts);
        // Nothing reminded changes nothing, and the last action stays as it was (triage's
        // "Nothing to change").
        let notice = if reminded.reminders.is_empty() {
            Notice::plain(nothing_to_remind(&scope).to_owned())
        } else if let Some(batch) = reminded.batch {
            if let Some(triage) = state.triage() {
                triage.adopt(batch);
            }
            Notice {
                text: set_text(&label, reminded.reminders.len()),
                undo: Some(batch),
                shown: window(&state),
            }
        } else {
            // Only new times: nothing moved, and the store keeps no earlier time to go back to.
            forget_last(&state);
            Notice::plain(moved_text(&label, reminded.reminders.len()))
        };
        state.show_notice(&notice, true);
    });
}

/// "Reminder set for tomorrow 08:00", "3 reminders set for someday".
pub fn set_text(label: &str, count: usize) -> String {
    match count {
        1 => format!("Reminder set for {label}"),
        count => format!("{count} reminders set for {label}"),
    }
}

/// A waiting conversation given a new time: "Reminder moved to Thu 08:00".
pub fn moved_text(label: &str, count: usize) -> String {
    match count {
        1 => format!("Reminder moved to {label}"),
        count => format!("{count} reminders moved to {label}"),
    }
}

/// Why nothing was reminded: a search of every folder sends only Inbox mail away.
fn nothing_to_remind(scope: &ViewScope) -> &'static str {
    match scope {
        ViewScope::UnifiedInbox => "Only a conversation in an Inbox can be reminded from here",
        ViewScope::Folder(_) => "Nothing here to remind",
    }
}

/// Remove Reminder (Shift+E, the palette, the picker) on `rows`: a waiting conversation comes
/// back to its Inbox now (after the undo window on the server), a returned one stops sorting by
/// its return; the toast's Undo restores them.
pub fn remove(state: &Rc<AppState>, rows: Vec<ListRow>) {
    let (Some(list), Some(store)) = (state.list(), state.mailbox().shared_store()) else {
        return;
    };
    let threads: Vec<ThreadId> = threads(&rows)
        .into_iter()
        .filter(|thread| of_thread(*thread).is_some())
        .collect();
    if threads.is_empty() {
        return;
    }
    // In the Reminders view those with a reminder leave the list.
    if matches!(list.scope(), Some(ViewScope::Folder(folder))
        if is_reminders_folder(folder, &state.mailbox().folders()))
    {
        let keys: Vec<RowKey> = rows
            .iter()
            .filter(|row| threads.contains(&thread_of(row)))
            .map(ListRow::key)
            .collect();
        list.advance_after(&keys);
    }
    let count = threads.len();
    let reply = store.cancel_reminders(threads, now_ms() + window(state).as_millis() as i64);
    let weak = Rc::downgrade(state);
    spawn(state, async move {
        let queued = reply.recv().await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let queued = match queued {
            Ok(queued) => queued,
            Err(error) => {
                tracing::warn!("a reminder wasn't removed: {error}");
                state.show_notice(
                    &Notice::plain("The reminder couldn't be removed".into()),
                    true,
                );
                return;
            }
        };
        tell_engine(&state, &queued.accounts);
        let Some(batch) = queued.batch else {
            // They had ended meanwhile (an answer, another window): nothing changes, and the
            // last action stays as it was, as after "Nothing here to remind" (the third review:
            // it said "Reminder removed" and the action before it couldn't be undone any more).
            state.show_notice(&Notice::plain(ended_text(count)), true);
            return;
        };
        if let Some(triage) = state.triage() {
            triage.adopt(batch);
        }
        state.show_notice(
            &Notice {
                text: removed_text(count),
                undo: Some(batch),
                shown: window(&state),
            },
            true,
        );
    });
}

/// "Reminder removed", "2 reminders removed".
pub fn removed_text(count: usize) -> String {
    match count {
        1 => "Reminder removed".to_owned(),
        count => format!("{count} reminders removed"),
    }
}

/// What Remove Reminder says when the store found nothing to end: "The reminder had ended
/// already", "The reminders had ended already".
pub fn ended_text(count: usize) -> String {
    match count {
        1 => "The reminder had ended already".to_owned(),
        _ => "The reminders had ended already".to_owned(),
    }
}

/// Go to Reminders (`g h`): the shown account's Reminders folder (in All Inboxes the selected
/// conversation's account's), else the first account's that has one.
pub fn go_to(state: &Rc<AppState>) {
    let Some(list) = state.list() else {
        return;
    };
    let folders = state.mailbox().folders();
    let selected = list
        .selected_targets()
        .ok()
        .and_then(|targets| targets.first().map(|target| target.folder));
    let shown = crate::compose_actions::compose_account(list.scope(), selected, &folders);
    let mut accounts: Vec<AccountId> = state
        .mailbox()
        .accounts()
        .iter()
        .map(|account| account.id)
        .collect();
    if let Some(shown) = shown {
        accounts.retain(|account| *account != shown);
        accounts.insert(0, shown);
    }
    match accounts
        .into_iter()
        .find_map(|account| reminders_folder(account, &folders))
    {
        Some(folder) => crate::go_to::show_folder(state, folder),
        None => state.show_notice(&Notice::plain(NONE_YET.to_owned()), true),
    }
}

/// Remind Me applies to a selection where something can be reminded ([`unavailable`]), Remove
/// Reminder while a selected conversation has a reminder (the loaded rows': the palette, the
/// toolbar, `h` and Shift+E follow them).
pub fn refresh_commands(
    actions: &crate::actions::ActionSet,
    list: &MessageList,
    folders: &[Folder],
) {
    let positions = list.selected_positions();
    // The selected rows' conversations by reference, against one set of the reminders'
    // (nothing at all while none counts: the usual case, and Select All's rows).
    let reminded: HashSet<ThreadId> = KNOWN.with(|known| {
        known
            .borrow()
            .reminders
            .iter()
            .flat_map(|reminder| reminder.threads.iter().copied())
            .collect()
    });
    let model = list.model();
    let any = !reminded.is_empty()
        && positions.iter().any(|position| {
            model
                .thread_at(*position)
                .is_some_and(|thread| reminded.contains(&thread))
        });
    actions.set_applies(ActionId::RemoveReminder, any);
    actions.set_applies(
        ActionId::RemindMe,
        !positions.is_empty() && unavailable(list, folders).is_none(),
    );
}

/// Notifies the conversations of `reminders` (back from Reminders, still to notify; with
/// whether each is [`Awaited::unsure`]) that have a row in an Inbox of their account now,
/// together, each by its newest message there, read from the store, under the new-mail settings
/// (`notify::reminders`: past three on screen, one that counts them; quiet for the Inbox the
/// focused window shows). They leave [`Known::awaited`], an unsure one without a notification;
/// the others stay for a later read, their moves back not landed yet (an unsure one now sure).
fn notify_back(
    state: &Rc<AppState>,
    reminders: Vec<(Reminder, bool)>,
) -> impl Future<Output = ()> + use<> {
    let store = state
        .mailbox()
        .shared_store()
        .filter(|_| !reminders.is_empty());
    let folders = state.mailbox().folders();
    let reads: Vec<(ReminderId, bool, Vec<_>)> = store.as_ref().map_or_else(Vec::new, |store| {
        reminders
            .iter()
            .map(|(reminder, unsure)| {
                let replies = folders
                    .iter()
                    .filter(|folder| {
                        folder.account == reminder.account && folder.role == FolderRole::Inbox
                    })
                    .map(|folder| {
                        store.conversation_members(
                            reminder.threads.clone(),
                            ViewSpec::folder(folder.id),
                        )
                    })
                    .collect();
                (reminder.id, *unsure, replies)
            })
            .collect()
    });
    let weak = Rc::downgrade(state);
    async move {
        let Some(store) = store else {
            return;
        };
        let mut messages = Vec::new();
        let mut back = Vec::new();
        let mut not_yet = Vec::new();
        for (reminder, unsure, replies) in reads {
            let mut newest = None;
            for reply in replies {
                if let Ok(members) = reply.recv().await {
                    for member in members.value {
                        if newest.is_none_or(|(date, id)| (member.date, member.id) > (date, id)) {
                            newest = Some((member.date, member.id));
                        }
                    }
                }
            }
            let Some((_, message)) = newest else {
                not_yet.push(reminder);
                continue;
            };
            if unsure {
                back.push(reminder);
            } else if let Ok(found) = store.message(message).recv().await
                && let Some(detail) = found.value
            {
                back.push(reminder);
                messages.push(detail.summary);
            }
        }
        KNOWN.with(|known| {
            let mut known = known.borrow_mut();
            for reminder in &back {
                known.awaited.remove(reminder);
            }
            for reminder in &not_yet {
                if let Some(awaited) = known.awaited.get_mut(reminder) {
                    awaited.unsure = false;
                }
            }
        });
        if !messages.is_empty()
            && let Some(state) = weak.upgrade()
        {
            crate::notify::reminders(&state, messages);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, account: i64, name: &str, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: name.to_owned(),
            delimiter: Some('/'),
            role,
            selectable: true,
            inferiors: true,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn each_account_has_its_reminders_folder() {
        let folders = [
            folder(1, 1, "INBOX", FolderRole::Inbox),
            folder(2, 1, "Reminders", FolderRole::Other),
            folder(3, 2, "INBOX", FolderRole::Inbox),
            folder(4, 2, "Work/Reminders", FolderRole::Other),
        ];
        assert_eq!(reminders_folder(AccountId(1), &folders), Some(FolderId(2)));
        assert_eq!(reminders_folder(AccountId(2), &folders), None);
        assert!(is_reminders_folder(FolderId(2), &folders));
        assert!(!is_reminders_folder(FolderId(4), &folders));
        // A Reminders folder's conversation rows show wake times; its message rows their dates.
        let shown = |scope, grouping| row_reminders(Some(scope), grouping, &folders);
        let conversations = ViewGrouping::Conversations;
        let reminders = shown(ViewScope::Folder(FolderId(2)), conversations);
        assert!(!reminders.marks && reminders.wakes);
        assert!(!shown(ViewScope::Folder(FolderId(2)), ViewGrouping::Messages).wakes);
        assert_eq!(
            row_reminders(None, conversations, &folders),
            RowReminders::default()
        );
        // An Inbox gives the clock's slot room only while one of its conversations is back.
        let inbox = shown(ViewScope::Folder(FolderId(1)), conversations);
        assert!(!inbox.marks && !inbox.wakes);
        let back = |account: i64, thread: i64| Reminder {
            id: ReminderId(thread),
            account: AccountId(account),
            threads: vec![ThreadId(thread)],
            state: ReminderState::Reminded,
            wake_at: 0,
            reminded_at: Some(0),
            folder: None,
            origin: FolderId(1),
        };
        KNOWN.with(|known| known.borrow_mut().reminders = vec![back(2, 7)]);
        // Another account's: not this Inbox's, but All Inboxes'.
        assert!(!shown(ViewScope::Folder(FolderId(1)), conversations).marks);
        let all = shown(ViewScope::UnifiedInbox, conversations);
        assert!(all.marks && !all.wakes && all.returned.contains(&ThreadId(7)));
        KNOWN.with(|known| known.borrow_mut().reminders.push(back(1, 9)));
        let inbox = shown(ViewScope::Folder(FolderId(1)), conversations);
        assert!(inbox.marks);
        assert_eq!(inbox.returned, HashSet::from([ThreadId(9)]));
        assert!(!shown(ViewScope::Folder(FolderId(2)), conversations).marks);
        // Waiting in its Reminders folder: on its way there while the view lists none of it
        // (the empty state's words); a returned one, or one whose folder went, isn't.
        assert_eq!(
            shown(ViewScope::Folder(FolderId(2)), conversations).arriving,
            0
        );
        let waiting = |thread: i64, folder| Reminder {
            state: ReminderState::Waiting,
            reminded_at: None,
            folder,
            ..back(1, thread)
        };
        KNOWN.with(|known| {
            let mut known = known.borrow_mut();
            known.reminders.push(waiting(10, Some(FolderId(2))));
            known.reminders.push(waiting(11, Some(FolderId(2))));
            known.reminders.push(waiting(12, None));
        });
        assert_eq!(
            shown(ViewScope::Folder(FolderId(2)), conversations).arriving,
            2
        );
        assert_eq!(
            shown(ViewScope::Folder(FolderId(1)), conversations).arriving,
            0
        );
        assert_eq!(
            shown(ViewScope::Folder(FolderId(2)), ViewGrouping::Messages).arriving,
            0
        );
        KNOWN.with(|known| known.borrow_mut().reminders.clear());
    }

    #[test]
    fn nothing_in_sent_drafts_trash_or_junk_can_be_reminded() {
        let folders = [
            folder(1, 1, "INBOX", FolderRole::Inbox),
            folder(2, 1, "Sent", FolderRole::Sent),
            folder(3, 1, "Drafts", FolderRole::Drafts),
            folder(4, 1, "Trash", FolderRole::Trash),
            folder(5, 1, "Junk", FolderRole::Junk),
            folder(6, 1, "Archive", FolderRole::Archive),
            folder(7, 1, "Reminders", FolderRole::Other),
        ];
        let reason = |id| unavailable_in(ViewScope::Folder(FolderId(id)), &folders);
        assert_eq!(reason(2), Some("mail in Sent can't be reminded"));
        assert_eq!(reason(3), Some("drafts can't be reminded"));
        assert!(reason(4).is_some() && reason(5).is_some());
        // An Inbox, the Archive, Reminders itself (a new time), All Inboxes.
        assert_eq!([reason(1), reason(6), reason(7)], [None, None, None]);
        assert_eq!(unavailable_in(ViewScope::UnifiedInbox, &folders), None);
        // There the rows' clock is dimmed; elsewhere, and in search results, it isn't.
        let off = |scope| {
            row_reminders(scope, ViewGrouping::Conversations, &folders)
                .off
                .is_some()
        };
        assert!(off(Some(ViewScope::Folder(FolderId(2)))));
        assert!(off(Some(ViewScope::Folder(FolderId(5)))));
        assert!(!off(Some(ViewScope::Folder(FolderId(1)))));
        assert!(!off(Some(ViewScope::Folder(FolderId(7)))));
        assert!(!off(Some(ViewScope::UnifiedInbox)));
        assert!(!off(None));
        // The clock's tooltip names the reason, as the toolbar's does.
        assert_eq!(
            row_reminders(
                Some(ViewScope::Folder(FolderId(3))),
                ViewGrouping::Conversations,
                &folders
            )
            .off,
            Some("drafts can't be reminded")
        );
        assert_eq!(
            sentence("mail in Sent can't be reminded"),
            "Mail in Sent can't be reminded"
        );
    }

    #[test]
    fn what_came_back_notifies() {
        let reminder = |id: i64, state, reminded_at| Reminder {
            id: ReminderId(id),
            account: AccountId(1),
            threads: vec![ThreadId(id)],
            state,
            wake_at: 0,
            reminded_at,
            folder: None,
            origin: FolderId(1),
        };
        let ids = |back: Vec<Reminder>| -> Vec<i64> { back.iter().map(|r| r.id.0).collect() };
        let now = [
            reminder(1, ReminderState::Reminded, Some(1_000)),
            reminder(2, ReminderState::Reminded, Some(2_000)),
            reminder(3, ReminderState::Waiting, None),
            reminder(4, ReminderState::Reminded, Some(2_500)),
        ];
        // Read before: what waited then and is back now; not what was back already, nor what
        // wasn't seen waiting.
        let before = HashMap::from([
            (ReminderId(1), ReminderState::Waiting),
            (ReminderId(2), ReminderState::Reminded),
            (ReminderId(3), ReminderState::Waiting),
        ]);
        assert_eq!(ids(came_back(Some(&before), 1_500, &now)), [1]);
        // The first read: what came back since the session began (the engine's start woke it),
        // not what came back in an earlier session; that is unsure (notified only if its
        // conversation isn't in an Inbox yet).
        assert_eq!(ids(came_back(None, 2_000, &now)), [2, 4]);
        assert!(came_back(None, 3_000, &now).is_empty());
        assert_eq!(back_before(2_000, &now), [ReminderId(1)]);
        assert_eq!(
            back_before(3_000, &now),
            [ReminderId(1), ReminderId(2), ReminderId(4)]
        );
    }

    #[test]
    fn a_reminder_back_waits_to_notify_until_it_ends_or_too_long() {
        let reminder = |id: i64, state, thread: i64| Reminder {
            id: ReminderId(id),
            account: AccountId(1),
            threads: vec![ThreadId(thread)],
            state,
            wake_at: 0,
            reminded_at: Some(0),
            folder: None,
            origin: FolderId(1),
        };
        let start = Instant::now();
        let later = |seconds| start + Duration::from_secs(seconds);
        let at = |unsure| Awaited {
            since: start,
            unsure,
        };
        // Back on a server account: its conversation not yet in an Inbox, it stays awaited, as
        // the store has it now (its thread may be new, the moves landing without new UIDs).
        let mut awaited = HashMap::from([
            (ReminderId(1), at(false)),
            (ReminderId(2), at(true)),
            (ReminderId(3), at(false)),
        ]);
        let now_read = [
            reminder(1, ReminderState::Reminded, 11),
            reminder(2, ReminderState::Reminded, 22),
            reminder(4, ReminderState::Waiting, 44),
        ];
        let kept = still_awaited(&mut awaited, &now_read, later(60));
        // 3 ended (Done, Remove Reminder, an answer) or went: no notification for it.
        assert_eq!(
            kept.iter()
                .map(|(r, unsure)| (r.id.0, *unsure))
                .collect::<Vec<_>>(),
            [(1, false), (2, true)],
            "{kept:?}"
        );
        assert_eq!(kept[1].0.threads, [ThreadId(22)]);
        assert_eq!(awaited.len(), 2);
        // Waiting again (reminded again before it landed): not back, so not awaited.
        let now_read = [
            reminder(1, ReminderState::Waiting, 11),
            reminder(2, ReminderState::Reminded, 22),
        ];
        assert_eq!(still_awaited(&mut awaited, &now_read, later(120)).len(), 1);
        assert!(!awaited.contains_key(&ReminderId(1)));
        // A day on, it stops being looked for.
        assert!(still_awaited(&mut awaited, &now_read, later(86_400)).is_empty());
        assert!(awaited.is_empty());
    }

    #[test]
    fn remove_reminder_says_when_or_how_many() {
        let waiting = |id: i64, wake_at| Reminder {
            id: ReminderId(id),
            account: AccountId(1),
            threads: vec![ThreadId(id), ThreadId(id + 100)],
            state: ReminderState::Waiting,
            wake_at,
            reminded_at: None,
            folder: Some(FolderId(2)),
            origin: FolderId(1),
        };
        let someday = remind::SOMEDAY;
        KNOWN.with(|known| {
            known.borrow_mut().reminders = vec![
                waiting(1, someday),
                waiting(2, someday),
                waiting(3, 1_900_000_000),
                Reminder {
                    state: ReminderState::Reminded,
                    reminded_at: Some(0),
                    ..waiting(4, 0)
                },
            ];
        });
        // One reminder (two threads of it), or two set for the same time: when.
        let threads = |ids: &[i64]| ids.iter().map(|id| ThreadId(*id)).collect::<Vec<_>>();
        let set_for = Some("set for someday".to_owned());
        assert_eq!(removing(&threads(&[1, 101])), set_for);
        assert_eq!(removing(&threads(&[1, 2])), set_for);
        // Different times: how many, each reminder once.
        assert_eq!(
            removing(&threads(&[1, 101, 3])),
            Some("2 reminders".to_owned())
        );
        // A returned conversation, or none with a reminder: no Remove Reminder in the picker.
        assert_eq!(removing(&threads(&[4])), None);
        assert_eq!(removing(&threads(&[9])), None);
        KNOWN.with(|known| known.borrow_mut().reminders.clear());
    }

    #[test]
    fn toasts_say_when() {
        assert_eq!(
            set_text("tomorrow 08:00", 1),
            "Reminder set for tomorrow 08:00"
        );
        assert_eq!(set_text("someday", 3), "3 reminders set for someday");
        assert_eq!(moved_text("Thu 08:00", 1), "Reminder moved to Thu 08:00");
        assert_eq!(removed_text(1), "Reminder removed");
        assert_eq!(removed_text(2), "2 reminders removed");
        assert_eq!(ended_text(1), "The reminder had ended already");
        assert_eq!(ended_text(3), "The reminders had ended already");
    }
}
