//! Remind Me in the store (docs/superhuman/remind.md): a remind sends a conversation's messages
//! to the account's Reminders folder (made when absent) as one Undo batch; the wake brings due
//! ones back to the Inbox, where they sort by when they came back until they leave the Inbox
//! again; cancel and Undo; a server account's Reminders folder kept until the server has it; a
//! reminder that follows its conversation through a merge and a thread made anew. Every list
//! model is fed only by change events and checked against a fresh page after each step, and
//! `check` recounts the sort keys independently. The replay (`conversation_view_tests.rs`)
//! mixes reminders into random writes.

use super::conversation_view_tests::Conversations;
use super::ops_tests::{NOW, WINDOW, arrive, folder_list, message, server};
use super::tests::{scratch, whole};
use super::*;
use sealwax_domain::model::{Address, FolderRole, Headers, ThreadId};
use sealwax_domain::remind::ReminderState;
use sealwax_domain::triage::Triage;
use std::sync::Arc;

const ME: &str = "me@example.test";
const T0: i64 = 1_790_000_000;

/// `T0` and `minute` minutes.
fn at(minute: i64) -> i64 {
    T0 + minute * 60
}

struct Fx {
    _dir: tempfile::TempDir,
    store: Store,
    account: AccountId,
    inbox: FolderId,
    archive: FolderId,
    sent: FolderId,
}

fn folder(store: &Store, account: AccountId, name: &str, role: FolderRole) -> FolderId {
    store
        .upsert_folder(FolderSpec::local(account, name, role))
        .recv_blocking()
        .unwrap()
        .value
}

/// A local account: its moves are done at once.
fn fixture() -> Fx {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Me".into(),
            address: ME.into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let fx = Fx {
        inbox: folder(&store, account, "INBOX", FolderRole::Inbox),
        archive: folder(&store, account, "Archive", FolderRole::Archive),
        sent: folder(&store, account, "Sent", FolderRole::Sent),
        _dir: dir,
        store,
        account,
    };
    folder(&fx.store, account, "Trash", FolderRole::Trash);
    fx
}

/// Message `key` (its Message-ID `key@example.test`, its subject `key`) at `minute`, replying
/// to `parents`; from the user in Sent.
fn mail(folder: FolderId, sent: bool, key: &str, minute: i64, parents: &[&str]) -> NewMessage {
    let parents: Vec<String> = parents
        .iter()
        .map(|p| format!("{p}@example.test"))
        .collect();
    let from = if sent { ME } else { "kari@example.test" };
    NewMessage {
        folder,
        date: at(minute),
        flags: Flags::SEEN,
        headers: Headers {
            from: vec![Address::new("", from)],
            subject: key.to_owned(),
            date: Some(at(minute)),
            message_id: Some(format!("{key}@example.test")),
            in_reply_to: parents.last().cloned().into_iter().collect(),
            references: parents,
            ..Headers::default()
        },
        snippet: format!("Snippet {key}"),
        search_text: key.to_owned(),
        size: 100,
        ..NewMessage::default()
    }
}

fn ingest(store: &Store, messages: Vec<NewMessage>) -> (StoreVersion, Vec<MessageId>) {
    let reply = store.ingest(messages).recv_blocking().unwrap();
    (
        reply.version,
        reply.ids.into_iter().map(Option::unwrap).collect(),
    )
}

/// The thread of message `id`, found in `folder`'s message rows.
fn thread(store: &Store, folder: FolderId, id: MessageId) -> ThreadId {
    whole(store, ViewSpec::folder(folder))
        .rows
        .iter()
        .find(|row| row.id == id)
        .expect("the message is listed there")
        .thread
}

/// The conversation rows of a view, by their subjects (their first message's), in order.
fn subjects(store: &Store, spec: ViewSpec) -> Vec<String> {
    whole(store, spec.conversations())
        .conversations
        .into_iter()
        .map(|row| row.subject)
        .collect()
}

fn reminders_folder(store: &Store, account: AccountId) -> Option<FolderId> {
    store
        .folders()
        .recv_blocking()
        .unwrap()
        .value
        .into_iter()
        .find(|folder| folder.account == account && folder.name.ends_with("Reminders"))
        .map(|folder| folder.id)
}

fn clean(store: &Store) {
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}

fn remind(store: &Store, threads: Vec<ThreadId>, scope: ViewScope, wake_at: i64) -> Reminded {
    store
        .remind(Remind {
            threads,
            scope,
            wake_at,
            not_before: 0,
        })
        .recv_blocking()
        .unwrap()
}

fn triage(store: &Store, action: Triage, messages: Vec<MessageId>) -> Queued {
    store
        .queue_triage(QueueTriage {
            action,
            messages,
            not_before: 0,
        })
        .recv_blocking()
        .unwrap()
}

/// Conversation views fed only by change events.
struct Watch {
    events: async_channel::Receiver<Arc<ChangeEvent>>,
    models: Vec<Conversations>,
    /// Whether an event since the last `settle` said the folder list changed.
    folders_changed: bool,
    /// Whether an event since the last `settle` said the reminders changed (phase C).
    reminders_changed: bool,
}

impl Watch {
    fn new(store: &Store, specs: &[ViewSpec]) -> Watch {
        let events = store.subscribe();
        let models = specs
            .iter()
            .map(|spec| Conversations::open(store, spec.conversations()))
            .collect();
        Watch {
            events,
            models,
            folders_changed: false,
            reminders_changed: false,
        }
    }

    /// Applies the events up to `version`, then checks each model against a fresh page and
    /// the store's own recount.
    fn settle(&mut self, store: &Store, version: StoreVersion) {
        self.folders_changed = false;
        self.reminders_changed = false;
        while self.models[0].version < version {
            let event = self.events.recv_blocking().unwrap();
            self.folders_changed |= event.folders_changed;
            self.reminders_changed |= event.reminders;
            for model in &mut self.models {
                model.apply(store, &event);
            }
        }
        for model in &self.models {
            let spec = model.view.spec();
            assert_eq!(whole(store, spec).conversations, model.rows, "{spec:?}");
        }
        clean(store);
    }
}

#[test]
fn a_remind_sends_a_conversation_away_and_undo_brings_it_back() {
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "trip", 1, &[]),
            mail(f.inbox, false, "trip-2", 2, &["trip"]),
            mail(f.sent, true, "trip-3", 3, &["trip", "trip-2"]),
            mail(f.inbox, false, "lunch", 4, &[]),
        ],
    );
    let trip = thread(&f.store, f.inbox, ids[0]);
    let mut watch = Watch::new(
        &f.store,
        &[
            ViewSpec::folder(f.inbox),
            ViewSpec::folder(f.sent),
            ViewSpec::unified_inbox(),
        ],
    );
    assert_eq!(reminders_folder(&f.store, f.account), None);
    let reminded = remind(&f.store, vec![trip], ViewScope::Folder(f.inbox), at(600));
    assert_eq!((reminded.moved, reminded.not_applied), (2, 0));
    assert!(
        reminded.accounts.is_empty(),
        "a local account's moves are done"
    );
    watch.settle(&f.store, reminded.version);
    assert!(watch.folders_changed, "the sidebar reads the new folder");
    // The Inbox keeps the rest; Sent keeps the user's reply; Reminders holds the two.
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["lunch"]);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.sent)), ["trip"]);
    let folder = reminders_folder(&f.store, f.account).expect("a Reminders folder");
    assert_eq!(whole(&f.store, ViewSpec::folder(folder)).rows.len(), 2);
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(listed.len(), 1);
    let reminder = &listed[0];
    assert_eq!(reminded.reminders, [reminder.id]);
    assert_eq!(
        (reminder.state, reminder.wake_at, reminder.reminded_at),
        (ReminderState::Waiting, at(600), None)
    );
    assert_eq!(
        (reminder.account, reminder.folder, reminder.origin),
        (f.account, Some(folder), f.inbox)
    );
    assert_eq!(reminder.threads, [trip]);

    // Undo: back where they were, and no reminder.
    let undone = f
        .store
        .undo(reminded.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.reverted, 2);
    watch.settle(&f.store, undone.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        ["lunch", "trip"]
    );
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
    let woken = f.store.wake_reminders(at(700)).recv_blocking().unwrap();
    assert!(woken.woken.is_empty() && woken.next.is_none());

    // Nothing in the scope to send: left alone, and no reminder.
    let (_, more) = ingest(&f.store, vec![mail(f.archive, false, "filed", 5, &[])]);
    let filed = thread(&f.store, f.archive, more[0]);
    let none = remind(&f.store, vec![filed], ViewScope::Folder(f.inbox), at(600));
    assert_eq!((none.moved, none.not_applied, none.batch), (0, 1, None));
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
}

#[test]
fn the_wake_brings_it_back_to_the_top_of_the_inbox_until_it_leaves_again() {
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "old", 1, &[]),
            mail(f.inbox, false, "mid", 50, &[]),
            mail(f.inbox, false, "new", 100, &[]),
        ],
    );
    let old = thread(&f.store, f.inbox, ids[0]);
    let mut watch = Watch::new(
        &f.store,
        &[
            ViewSpec::folder(f.inbox),
            ViewSpec::unified_inbox(),
            ViewSpec::folder(f.inbox).with_filter(ViewFilter::Unread),
            ViewSpec::folder(f.archive),
        ],
    );
    let reminded = remind(&f.store, vec![old], ViewScope::UnifiedInbox, at(200));
    watch.settle(&f.store, reminded.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        ["new", "mid"]
    );

    // Not yet: nothing changes, and the timer learns when.
    let early = f.store.wake_reminders(at(199)).recv_blocking().unwrap();
    assert!(early.woken.is_empty());
    assert_eq!(
        (early.next, early.version),
        (Some(at(200)), reminded.version)
    );

    // Due: back in the Inbox, at the top, dated when it came back.
    let woken = f.store.wake_reminders(at(201)).recv_blocking().unwrap();
    assert_eq!(woken.woken, reminded.reminders);
    assert_eq!(woken.next, None);
    watch.settle(&f.store, woken.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::unified_inbox()),
        ["old", "new", "mid"]
    );
    let row = &whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations[0];
    assert_eq!((row.thread, row.date, row.latest), (old, at(201), ids[0]));
    assert_eq!(row.snippet, "Snippet old");
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(
        (listed[0].state, listed[0].reminded_at),
        (ReminderState::Reminded, Some(at(201)))
    );
    // Message rows sort by their own dates.
    let rows = whole(&f.store, ViewSpec::folder(f.inbox)).rows;
    assert_eq!(rows.last().map(|row| row.id), Some(ids[0]));

    // Later mail goes above it, older mail below.
    let (version, _) = ingest(&f.store, vec![mail(f.inbox, false, "later", 300, &[])]);
    watch.settle(&f.store, version);
    let (version, _) = ingest(&f.store, vec![mail(f.inbox, false, "earlier", 150, &[])]);
    watch.settle(&f.store, version);
    let order = ["later", "old", "earlier", "new", "mid"];
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), order);

    // Read, unread and starred, it keeps its place.
    for action in [Triage::Read(false), Triage::Star(true), Triage::Read(true)] {
        let queued = triage(&f.store, action, vec![ids[0]]);
        watch.settle(&f.store, queued.version);
        assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), order);
    }

    // Done: it leaves the Inbox and its place there ends; Undo restores both.
    let archived = triage(&f.store, Triage::Archive, vec![ids[0]]);
    watch.settle(&f.store, archived.version);
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
    let undone = f
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    watch.settle(&f.store, undone.version);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), order);
    assert_eq!(
        f.store.reminders().recv_blocking().unwrap()[0].state,
        ReminderState::Reminded
    );

    // Done again, and moved back by hand: it sorts by its own mail.
    let archived = triage(&f.store, Triage::Archive, vec![ids[0]]);
    watch.settle(&f.store, archived.version);
    let back = triage(&f.store, Triage::MoveTo(f.inbox), vec![ids[0]]);
    watch.settle(&f.store, back.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        ["later", "earlier", "new", "mid", "old"]
    );
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
}

#[test]
fn cancel_brings_a_waiting_conversation_back_and_undo_sends_it_again() {
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "plan", 1, &[]),
            mail(f.inbox, false, "other", 2, &[]),
        ],
    );
    let plan = thread(&f.store, f.inbox, ids[0]);
    let mut watch = Watch::new(&f.store, &[ViewSpec::folder(f.inbox)]);
    let reminded = remind(&f.store, vec![plan], ViewScope::Folder(f.inbox), at(100));
    watch.settle(&f.store, reminded.version);

    let cancelled = f
        .store
        .cancel_reminders(vec![plan], 0)
        .recv_blocking()
        .unwrap();
    assert_eq!(cancelled.applied, 1);
    watch.settle(&f.store, cancelled.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        ["other", "plan"]
    );
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());

    let undone = f
        .store
        .undo(cancelled.batch.unwrap())
        .recv_blocking()
        .unwrap();
    watch.settle(&f.store, undone.version);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["other"]);
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(
        (listed[0].state, listed[0].wake_at),
        (ReminderState::Waiting, at(100))
    );

    // Back and cancelled (Remove Reminder): it sorts by its own mail again. Nothing moved,
    // but Undo restores it all the same (phase C; it had no batch to undo before).
    let woken = f.store.wake_reminders(at(150)).recv_blocking().unwrap();
    watch.settle(&f.store, woken.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        ["plan", "other"]
    );
    let cancelled = f
        .store
        .cancel_reminders(vec![plan], 0)
        .recv_blocking()
        .unwrap();
    assert_eq!(cancelled.applied, 0);
    watch.settle(&f.store, cancelled.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        ["other", "plan"]
    );
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
    let undone = f
        .store
        .undo(cancelled.batch.expect("a batch for Undo"))
        .recv_blocking()
        .unwrap();
    watch.settle(&f.store, undone.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        ["plan", "other"]
    );
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(
        (listed[0].state, listed[0].reminded_at),
        (ReminderState::Reminded, Some(at(150)))
    );
}

#[test]
fn reminding_again_gives_a_new_time_or_ends_the_last_return() {
    let f = fixture();
    let (_, ids) = ingest(&f.store, vec![mail(f.inbox, false, "visit", 1, &[])]);
    let visit = thread(&f.store, f.inbox, ids[0]);
    let mut watch = Watch::new(&f.store, &[ViewSpec::folder(f.inbox)]);
    let first = remind(&f.store, vec![visit], ViewScope::Folder(f.inbox), at(100));
    watch.settle(&f.store, first.version);

    // A reply comes to the Inbox meanwhile and shows there.
    let (version, _) = ingest(
        &f.store,
        vec![mail(f.inbox, false, "visit-2", 20, &["visit"])],
    );
    watch.settle(&f.store, version);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["visit"]);
    // Reminded again from the Inbox: the same reminder, a new time, and the reply goes too.
    let again = remind(&f.store, vec![visit], ViewScope::Folder(f.inbox), at(300));
    assert_eq!(
        (again.reminders.clone(), again.moved),
        (first.reminders.clone(), 1)
    );
    watch.settle(&f.store, again.version);
    assert!(subjects(&f.store, ViewSpec::folder(f.inbox)).is_empty());
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!((listed.len(), listed[0].wake_at), (1, at(300)));
    // Not due at the first time any more.
    let early = f.store.wake_reminders(at(150)).recv_blocking().unwrap();
    assert!(early.woken.is_empty());
    let woken = f.store.wake_reminders(at(300)).recv_blocking().unwrap();
    watch.settle(&f.store, woken.version);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["visit"]);
    let row = &whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations[0];
    assert_eq!((row.date, row.members), (at(300), 2));
    // The latest mail still speaks for it: its sender, snippet and the message it names.
    assert_eq!(row.snippet, "Snippet visit-2");

    // Back, and sent away again: the return ends with the new batch; Undo restores it.
    let third = remind(&f.store, vec![visit], ViewScope::Folder(f.inbox), at(900));
    watch.settle(&f.store, third.version);
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, ReminderState::Waiting);
    let undone = f.store.undo(third.batch.unwrap()).recv_blocking().unwrap();
    watch.settle(&f.store, undone.version);
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(
        (listed.len(), listed[0].state, listed[0].reminded_at),
        (1, ReminderState::Reminded, Some(at(300)))
    );
    let row = &whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations[0];
    assert_eq!(row.date, at(300));

    // A reply later than its return sorts it by the reply.
    let (version, _) = ingest(
        &f.store,
        vec![mail(f.inbox, false, "visit-3", 500, &["visit"])],
    );
    watch.settle(&f.store, version);
    let row = &whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations[0];
    assert_eq!(
        (row.date, row.snippet.as_str()),
        (at(500), "Snippet visit-3")
    );
}

#[test]
fn acting_on_it_in_the_reminders_folder_ends_the_wait() {
    let f = fixture();
    let (_, ids) = ingest(&f.store, vec![mail(f.inbox, false, "bill", 1, &[])]);
    let bill = thread(&f.store, f.inbox, ids[0]);
    let mut watch = Watch::new(&f.store, &[ViewSpec::folder(f.inbox)]);
    let reminded = remind(&f.store, vec![bill], ViewScope::Folder(f.inbox), at(100));
    watch.settle(&f.store, reminded.version);
    let folder = reminders_folder(&f.store, f.account).unwrap();
    let waiting: Vec<MessageId> = whole(&f.store, ViewSpec::folder(folder))
        .rows
        .iter()
        .map(|row| row.id)
        .collect();
    let archived = triage(&f.store, Triage::Archive, waiting);
    watch.settle(&f.store, archived.version);
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
    let woken = f.store.wake_reminders(at(200)).recv_blocking().unwrap();
    assert!(woken.woken.is_empty());
    // Undo: waiting again, and due.
    let undone = f
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    watch.settle(&f.store, undone.version);
    let woken = f.store.wake_reminders(at(200)).recv_blocking().unwrap();
    assert_eq!(woken.woken, reminded.reminders);
    watch.settle(&f.store, woken.version);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["bill"]);
}

#[test]
fn a_reminder_follows_its_conversation_through_a_merge_and_a_new_thread() {
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "plan", 1, &[]),
            mail(f.archive, false, "notes", 2, &[]),
            mail(f.inbox, false, "solo", 3, &[]),
            mail(f.inbox, false, "rest", 4, &[]),
        ],
    );
    let plan = thread(&f.store, f.inbox, ids[0]);
    let solo = thread(&f.store, f.inbox, ids[2]);
    let mut watch = Watch::new(
        &f.store,
        &[ViewSpec::folder(f.inbox), ViewSpec::folder(f.archive)],
    );
    let reminded = remind(
        &f.store,
        vec![plan, solo],
        ViewScope::Folder(f.inbox),
        at(100),
    );
    assert_eq!(reminded.reminders.len(), 2);
    watch.settle(&f.store, reminded.version);

    // A message that answers both conversations merges them.
    let (version, _) = ingest(
        &f.store,
        vec![mail(f.archive, false, "both", 5, &["plan", "notes"])],
    );
    watch.settle(&f.store, version);
    let notes = thread(&f.store, f.archive, ids[1]);
    let listed = f.store.reminders().recv_blocking().unwrap();
    let merged = listed
        .iter()
        .find(|reminder| reminder.id == reminded.reminders[0])
        .unwrap();
    assert_eq!(merged.threads, [notes]);

    // The other's only message leaves the store and comes back to Reminders (a move the
    // store couldn't place, then that folder's sync): a new thread, the same reminder.
    let folder = reminders_folder(&f.store, f.account).unwrap();
    let away = whole(&f.store, ViewSpec::folder(folder))
        .rows
        .iter()
        .find(|row| row.thread == solo)
        .unwrap()
        .id;
    let removed = f.store.remove(vec![away]).recv_blocking().unwrap();
    watch.settle(&f.store, removed.version);
    let (version, again) = ingest(&f.store, vec![mail(folder, false, "solo", 3, &[])]);
    watch.settle(&f.store, version);
    let anew = thread(&f.store, folder, again[0]);
    assert_ne!(anew, solo);
    let listed = f.store.reminders().recv_blocking().unwrap();
    let found = listed
        .iter()
        .find(|reminder| reminder.id == reminded.reminders[1])
        .unwrap();
    assert_eq!(found.threads, [anew]);

    // Both come back to the top of the Inbox at once.
    let woken = f.store.wake_reminders(at(100)).recv_blocking().unwrap();
    assert_eq!(woken.woken.len(), 2);
    watch.settle(&f.store, woken.version);
    let rows = whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations;
    let mut top: Vec<(ThreadId, i64)> = rows.iter().map(|row| (row.thread, row.date)).collect();
    assert_eq!(top.pop(), Some((thread(&f.store, f.inbox, ids[3]), at(4))));
    top.sort_unstable();
    assert_eq!(top, [(notes, at(100)), (anew, at(100))]);
}

#[test]
fn a_server_accounts_reminders_folder_waits_for_the_server_and_the_list_keeps_it() {
    let fx = server();
    let ids = arrive(
        &fx.store,
        fx.inbox,
        vec![
            message(fx.inbox, 1, Flags::SEEN),
            message(fx.inbox, 2, Flags::SEEN),
        ],
    );
    let first = thread(&fx.store, fx.inbox, ids[0]);
    let mut watch = Watch::new(&fx.store, &[ViewSpec::folder(fx.inbox)]);
    let reminded = fx
        .store
        .remind(Remind {
            threads: vec![first],
            scope: ViewScope::Folder(fx.inbox),
            wake_at: NOW / 1000 + 3_600,
            not_before: NOW + WINDOW,
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(
        (reminded.moved, reminded.accounts.clone()),
        (1, vec![fx.account])
    );
    watch.settle(&fx.store, reminded.version);
    assert!(watch.folders_changed);
    // Hidden at once, as any pending move.
    assert_eq!(watch.models[0].rows.len(), 1);
    let folder = reminders_folder(&fx.store, fx.account).unwrap();

    // The engine makes it on the server and syncs it before the first move into it runs, not
    // before.
    let wanted = |now| {
        fx.store
            .folders_to_prepare(fx.account, now)
            .recv_blocking()
            .unwrap()
    };
    assert!(wanted(NOW).is_empty());
    assert_eq!(
        wanted(NOW + WINDOW),
        [FolderToPrepare {
            folder,
            name_raw: "Reminders".into(),
            create: true,
            sync: true,
        }]
    );
    // The account pass's list, without it: kept while the move waits.
    let replaced = fx
        .store
        .replace_folders(fx.account, folder_list(fx.account))
        .recv_blocking()
        .unwrap();
    assert_eq!(replaced.removed_folders, 0);
    assert_eq!(reminders_folder(&fx.store, fx.account), Some(folder));
    let due = fx
        .store
        .due_ops(fx.account, NOW + WINDOW)
        .recv_blocking()
        .unwrap();
    assert!(matches!(&due.groups[..], [group] if matches!(&group.action,
            OpAction::Move { to, to_name, .. } if *to == folder && to_name == "Reminders")));
    // Created: listed from then on, as any folder; synced once before the move runs.
    fx.store.folder_created(folder).recv_blocking().unwrap();
    assert_eq!(
        wanted(NOW + WINDOW),
        [FolderToPrepare {
            folder,
            name_raw: "Reminders".into(),
            create: false,
            sync: true,
        }]
    );
    super::ops_tests::ready(&fx.store, folder, 9);
    assert!(wanted(NOW + WINDOW).is_empty());
    let mut listed = folder_list(fx.account);
    listed.push(FolderSpec::local(
        fx.account,
        "Reminders",
        FolderRole::Other,
    ));
    let replaced = fx
        .store
        .replace_folders(fx.account, listed)
        .recv_blocking()
        .unwrap();
    assert_eq!(replaced.removed_folders, 0);
    assert_eq!(reminders_folder(&fx.store, fx.account), Some(folder));
    clean(&fx.store);
}

#[test]
fn a_reminders_folder_the_server_refuses_goes_and_its_moves_with_it() {
    let fx = server();
    let ids = arrive(&fx.store, fx.inbox, vec![message(fx.inbox, 1, Flags::SEEN)]);
    let first = thread(&fx.store, fx.inbox, ids[0]);
    let mut watch = Watch::new(&fx.store, &[ViewSpec::folder(fx.inbox)]);
    let reminded = fx
        .store
        .remind(Remind {
            threads: vec![first],
            scope: ViewScope::Folder(fx.inbox),
            wake_at: NOW / 1000 + 60,
            not_before: NOW,
        })
        .recv_blocking()
        .unwrap();
    watch.settle(&fx.store, reminded.version);
    assert!(watch.models[0].rows.is_empty());
    let folder = reminders_folder(&fx.store, fx.account).unwrap();
    let version = fx.store.folder_refused(folder).recv_blocking().unwrap();
    watch.settle(&fx.store, version);
    assert!(watch.folders_changed);
    assert_eq!(reminders_folder(&fx.store, fx.account), None);
    assert_eq!(watch.models[0].rows.len(), 1, "shown again where it is");
    let due = fx.store.due_ops(fx.account, NOW).recv_blocking().unwrap();
    assert!(due.groups.is_empty());
    assert_eq!(
        due.refused,
        [OpsRefusal {
            account: fx.account,
            change: RefusedChange::Move,
            messages: 1
        }]
    );
    // The reminder stays: the conversation comes back to the top at its time.
    let listed = fx.store.reminders().recv_blocking().unwrap();
    assert_eq!(listed[0].state, ReminderState::Waiting);

    // Undone before its move ran, a folder the server doesn't have yet goes too.
    let again = fx
        .store
        .remind(Remind {
            threads: vec![first],
            scope: ViewScope::Folder(fx.inbox),
            wake_at: NOW / 1000 + 60,
            not_before: NOW + WINDOW,
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(
        again.reminders,
        [listed[0].id],
        "the waiting one, a new time"
    );
    watch.settle(&fx.store, again.version);
    assert!(reminders_folder(&fx.store, fx.account).is_some());
    let undone = fx.store.undo(again.batch.unwrap()).recv_blocking().unwrap();
    watch.settle(&fx.store, undone.version);
    assert!(watch.folders_changed);
    assert_eq!(reminders_folder(&fx.store, fx.account), None);
    assert_eq!(watch.models[0].rows.len(), 1);
}

/// Folder ids are rowids: the next folder the server lists may take a refused Reminders
/// folder's. Its waiting reminders keep no claim on it: reminded again, a conversation goes to a
/// new Reminders folder, and at its wake nothing leaves the other folder.
#[test]
fn a_refused_reminders_folders_id_taken_by_another_folder_holds_no_reminder() {
    let fx = server();
    let ids = arrive(
        &fx.store,
        fx.inbox,
        vec![
            message(fx.inbox, 1, Flags::SEEN),
            message(fx.inbox, 2, Flags::SEEN),
        ],
    );
    let (first, second) = (
        thread(&fx.store, fx.inbox, ids[0]),
        thread(&fx.store, fx.inbox, ids[1]),
    );
    let (soon, later) = (NOW / 1000 + 3_600, NOW / 1000 + 7_200);
    let remind = |threads, wake_at, not_before| {
        fx.store
            .remind(Remind {
                threads,
                scope: ViewScope::Folder(fx.inbox),
                wake_at,
                not_before,
            })
            .recv_blocking()
            .unwrap()
    };
    remind(vec![first, second], soon, NOW);
    let refused = reminders_folder(&fx.store, fx.account).unwrap();
    fx.store.folder_refused(refused).recv_blocking().unwrap();
    let due = fx.store.due_ops(fx.account, NOW).recv_blocking().unwrap();
    assert!(due.groups.is_empty());
    let folders = |store: &Store| -> Vec<(Vec<ThreadId>, Option<FolderId>)> {
        let mut listed: Vec<_> = store
            .reminders()
            .recv_blocking()
            .unwrap()
            .into_iter()
            .map(|reminder| (reminder.threads, reminder.folder))
            .collect();
        listed.sort();
        listed
    };
    assert_eq!(
        folders(&fx.store),
        [(vec![first], None), (vec![second], None)],
        "both wait in no folder"
    );

    // The account pass lists a folder of the user's, which takes the free id; a reply of the
    // second conversation is filed there.
    let mut listed = folder_list(fx.account);
    listed.push(FolderSpec::local(fx.account, "Receipts", FolderRole::Other));
    fx.store
        .replace_folders(fx.account, listed)
        .recv_blocking()
        .unwrap();
    let receipts = fx
        .store
        .folders()
        .recv_blocking()
        .unwrap()
        .value
        .into_iter()
        .find(|folder| folder.name == "Receipts")
        .unwrap()
        .id;
    assert_eq!(receipts, refused, "the case at hand: the id is taken again");
    super::ops_tests::ready(&fx.store, receipts, 9);
    let mut filed = message(receipts, 7, Flags::SEEN);
    filed.headers.in_reply_to = vec!["m2@example.test".into()];
    filed.headers.references = filed.headers.in_reply_to.clone();
    let filed = arrive(&fx.store, receipts, vec![filed])[0];
    assert_eq!(thread(&fx.store, receipts, filed), second);

    // Reminded again: a new Reminders folder, not the user's.
    let again = remind(vec![first], later, NOW + WINDOW);
    assert_eq!(again.moved, 1);
    let folder = reminders_folder(&fx.store, fx.account).expect("a new Reminders folder");
    assert_ne!(folder, receipts);
    assert_eq!(
        folders(&fx.store),
        [(vec![first], Some(folder)), (vec![second], None)]
    );
    let due = fx
        .store
        .due_ops(fx.account, NOW + WINDOW)
        .recv_blocking()
        .unwrap();
    assert!(
        matches!(&due.groups[..], [group] if matches!(&group.action,
            OpAction::Move { to, to_name, .. } if *to == folder && to_name == "Reminders")),
        "{:?}",
        due.groups
    );

    // The other one's wake: nothing leaves Receipts, and it is back at the top of the Inbox.
    let woken = fx.store.wake_reminders(soon).recv_blocking().unwrap();
    assert_eq!(woken.woken.len(), 1);
    assert!(woken.accounts.is_empty(), "nothing to move");
    assert_eq!(whole(&fx.store, ViewSpec::folder(receipts)).rows.len(), 1);
    let rows = whole(&fx.store, ViewSpec::folder(fx.inbox).conversations()).conversations;
    assert_eq!(
        rows.iter()
            .map(|row| (row.thread, row.date))
            .collect::<Vec<_>>(),
        [(second, soon)]
    );
    clean(&fx.store);
}

/// Op batch ids are rowids too: a remind batch emptied by a refusal goes with the next prune,
/// and the next batch takes its id. Undo of that unrelated batch leaves the reminder alone.
#[test]
fn undo_of_a_batch_that_took_an_emptied_remind_batchs_id_keeps_the_reminder() {
    let fx = server();
    let ids = arrive(
        &fx.store,
        fx.inbox,
        vec![
            message(fx.inbox, 1, Flags::SEEN),
            message(fx.inbox, 2, Flags::SEEN),
        ],
    );
    let first = thread(&fx.store, fx.inbox, ids[0]);
    let wake_at = NOW / 1000 + 3_600;
    let reminded = fx
        .store
        .remind(Remind {
            threads: vec![first],
            scope: ViewScope::Folder(fx.inbox),
            wake_at,
            not_before: NOW,
        })
        .recv_blocking()
        .unwrap();
    let folder = reminders_folder(&fx.store, fx.account).unwrap();
    fx.store.folder_refused(folder).recv_blocking().unwrap();
    fx.store.due_ops(fx.account, NOW).recv_blocking().unwrap();
    let woken = fx.store.wake_reminders(wake_at).recv_blocking().unwrap();
    assert_eq!(woken.woken, reminded.reminders);

    // An unrelated star, then its Undo.
    let starred = fx
        .store
        .queue_triage(QueueTriage {
            action: Triage::Star(true),
            messages: vec![ids[1]],
            not_before: NOW + WINDOW,
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(
        starred.batch, reminded.batch,
        "the case at hand: the id is taken again"
    );
    fx.store
        .undo(starred.batch.unwrap())
        .recv_blocking()
        .unwrap();
    let listed = fx.store.reminders().recv_blocking().unwrap();
    assert_eq!(
        listed.iter().map(|r| r.state).collect::<Vec<_>>(),
        [ReminderState::Reminded]
    );
    let rows = whole(&fx.store, ViewSpec::folder(fx.inbox).conversations()).conversations;
    assert_eq!((rows[0].thread, rows[0].date), (first, wake_at));
    clean(&fx.store);
}

/// A reminder whose messages have no Message-ID finds its conversation by its thread alone;
/// once that thread is gone, a new conversation that takes its id isn't reminded.
#[test]
fn a_new_conversation_on_a_gone_threads_id_is_not_reminded() {
    let f = fixture();
    let keyless = |key: &str, minute| {
        let mut mail = mail(f.inbox, false, key, minute, &[]);
        mail.headers.message_id = None;
        mail
    };
    let (_, ids) = ingest(&f.store, vec![keyless("note", 1)]);
    let note = thread(&f.store, f.inbox, ids[0]);
    remind(&f.store, vec![note], ViewScope::Folder(f.inbox), at(100));
    let woken = f.store.wake_reminders(at(100)).recv_blocking().unwrap();
    assert_eq!(woken.woken.len(), 1);
    let back = whole(&f.store, ViewSpec::folder(f.inbox)).rows[0].id;
    let mut watch = Watch::new(&f.store, &[ViewSpec::folder(f.inbox)]);
    let removed = f.store.remove(vec![back]).recv_blocking().unwrap();
    watch.settle(&f.store, removed.version);

    let (version, ids) = ingest(&f.store, vec![keyless("other", 2)]);
    watch.settle(&f.store, version);
    assert_eq!(
        thread(&f.store, f.inbox, ids[0]),
        note,
        "the case at hand: the id is taken again"
    );
    let rows = whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations;
    assert_eq!(
        rows.iter()
            .map(|row| (row.subject.as_str(), row.date))
            .collect::<Vec<_>>(),
        [("other", at(2))]
    );
}

#[test]
fn a_due_reminder_waits_for_its_messages_on_their_way_to_reminders() {
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "parcel", 1, &[]),
            mail(f.sent, true, "parcel-2", 2, &["parcel"]),
        ],
    );
    let parcel = thread(&f.store, f.inbox, ids[0]);
    let mut watch = Watch::new(&f.store, &[ViewSpec::folder(f.inbox)]);
    let reminded = remind(&f.store, vec![parcel], ViewScope::Folder(f.inbox), at(100));
    watch.settle(&f.store, reminded.version);
    // The message leaves the store on its way (a move the store couldn't place); the user's
    // reply in Sent keeps the conversation.
    let folder = reminders_folder(&f.store, f.account).unwrap();
    let away = whole(&f.store, ViewSpec::folder(folder)).rows[0].id;
    let removed = f.store.remove(vec![away]).recv_blocking().unwrap();
    watch.settle(&f.store, removed.version);

    // Due meanwhile: it waits for it, and the timer looks again in a minute.
    let early = f.store.wake_reminders(at(100)).recv_blocking().unwrap();
    assert!(early.woken.is_empty());
    assert_eq!(early.next, Some(at(100) + 60));
    // Stored again by the Reminders folder's sync: it comes back.
    let (version, _) = ingest(&f.store, vec![mail(folder, false, "parcel", 1, &[])]);
    watch.settle(&f.store, version);
    let woken = f.store.wake_reminders(at(101)).recv_blocking().unwrap();
    assert_eq!(woken.woken, reminded.reminders);
    watch.settle(&f.store, woken.version);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["parcel"]);
    let row = &whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations[0];
    assert_eq!(row.date, at(101));

    // Gone for good: after a day, with nothing to move and nothing in the Inbox, it can never
    // come back, so it ends rather than linger as reminded (phase C, the final review of
    // phase B; it was reminded with nothing to bump before).
    let reminded = remind(&f.store, vec![parcel], ViewScope::Folder(f.inbox), at(200));
    watch.settle(&f.store, reminded.version);
    let away = whole(&f.store, ViewSpec::folder(folder)).rows[0].id;
    let removed = f.store.remove(vec![away]).recv_blocking().unwrap();
    watch.settle(&f.store, removed.version);
    let woken = f
        .store
        .wake_reminders(at(200) + 86_399)
        .recv_blocking()
        .unwrap();
    assert!(woken.woken.is_empty() && woken.next.is_some());
    let woken = f
        .store
        .wake_reminders(at(200) + 86_400)
        .recv_blocking()
        .unwrap();
    assert!(woken.woken.is_empty());
    assert_eq!(woken.ended, reminded.reminders);
    assert_eq!(woken.next, None);
    watch.settle(&f.store, woken.version);
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
    clean(&f.store);
}

/// A reminder whose messages have no Message-ID follows its thread through a merge: the
/// message its mail replied to links it to another conversation, which takes it in.
#[test]
fn a_reminder_without_message_ids_follows_its_thread_through_a_merge() {
    let f = fixture();
    let mut lone = mail(f.inbox, false, "lone", 1, &["link"]);
    lone.headers.message_id = None;
    let (_, ids) = ingest(
        &f.store,
        vec![
            lone,
            mail(f.inbox, false, "big", 2, &[]),
            mail(f.inbox, false, "big-2", 3, &["big"]),
        ],
    );
    let lone = thread(&f.store, f.inbox, ids[0]);
    let mut watch = Watch::new(&f.store, &[ViewSpec::folder(f.inbox)]);
    let reminded = remind(&f.store, vec![lone], ViewScope::Folder(f.inbox), at(100));
    watch.settle(&f.store, reminded.version);
    let woken = f.store.wake_reminders(at(100)).recv_blocking().unwrap();
    watch.settle(&f.store, woken.version);

    let (version, more) = ingest(&f.store, vec![mail(f.inbox, false, "link", 4, &["big"])]);
    watch.settle(&f.store, version);
    let merged = thread(&f.store, f.inbox, more[0]);
    assert_ne!(merged, lone, "merged into the larger conversation");
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(listed[0].threads, [merged]);
    let rows = whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations;
    assert_eq!(
        rows.iter()
            .map(|row| (row.thread, row.date))
            .collect::<Vec<_>>(),
        [(merged, at(100))]
    );
}

/// One action may reach a reminder through two of its conversation's threads: ended twice in
/// one batch, it keeps the state it ended from, which that batch's Undo restores.
#[test]
fn ending_a_reminder_twice_in_one_batch_keeps_what_undo_restores() {
    let f = fixture();
    let (_, ids) = ingest(&f.store, vec![mail(f.inbox, false, "twice", 1, &[])]);
    let twice = thread(&f.store, f.inbox, ids[0]);
    remind(&f.store, vec![twice], ViewScope::Folder(f.inbox), at(100));
    f.store.wake_reminders(at(100)).recv_blocking().unwrap();

    // On a writer connection of its own, in a transaction rolled back: the store stays as
    // it was.
    let mut conn = super::schema::open_writer(&f._dir.path().join("store.sqlite")).unwrap();
    let tx = conn.transaction().unwrap();
    let live = super::remind::live(&tx).unwrap();
    assert_eq!(live[0].state, ReminderState::Reminded);
    let batch: i64 = tx
        .query_row(
            "INSERT INTO op_batches(kind) VALUES (1) RETURNING id",
            [],
            |row| row.get(0),
        )
        .unwrap();
    super::remind::end(&tx, &live[0], Some(batch)).unwrap();
    super::remind::end(&tx, &live[0], Some(batch)).unwrap();
    assert!(super::remind::live(&tx).unwrap().is_empty());
    super::remind::undone(&tx, batch).unwrap();
    assert_eq!(
        super::remind::live(&tx)
            .unwrap()
            .iter()
            .map(|reminder| reminder.state)
            .collect::<Vec<_>>(),
        [ReminderState::Reminded]
    );
}

// Phase C: Someday, "if no reply", the Reminders view's order, Important on return, the
// change event's word, and the final review of phase B (a relisted folder, reminders that can
// never come back, an undoable Remove Reminder: the cancel test above).

/// Someday never comes due: no wake brings it back, and the timer isn't told to wait for it.
#[test]
fn someday_waits_until_it_is_removed() {
    use sealwax_domain::remind::SOMEDAY;
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "shelf", 1, &[]),
            mail(f.inbox, false, "soon", 2, &[]),
        ],
    );
    let (shelf, soon) = (
        thread(&f.store, f.inbox, ids[0]),
        thread(&f.store, f.inbox, ids[1]),
    );
    let mut watch = Watch::new(&f.store, &[ViewSpec::folder(f.inbox)]);
    let reminded = remind(&f.store, vec![shelf], ViewScope::Folder(f.inbox), SOMEDAY);
    watch.settle(&f.store, reminded.version);
    assert!(
        watch.reminders_changed,
        "the event says the reminders changed"
    );
    let woken = f
        .store
        .wake_reminders(at(1_000_000))
        .recv_blocking()
        .unwrap();
    assert!(woken.woken.is_empty() && woken.ended.is_empty());
    assert_eq!(woken.next, None, "nothing to wait for");
    // Another reminder's time is the next; Someday's never is.
    let other = remind(&f.store, vec![soon], ViewScope::Folder(f.inbox), at(50));
    watch.settle(&f.store, other.version);
    let woken = f.store.wake_reminders(at(10)).recv_blocking().unwrap();
    assert_eq!(woken.next, Some(at(50)));
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        Vec::<String>::new()
    );
    // Removed, it comes back now, by its own date.
    let cancelled = f
        .store
        .cancel_reminders(vec![shelf], 0)
        .recv_blocking()
        .unwrap();
    watch.settle(&f.store, cancelled.version);
    assert!(watch.reminders_changed);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["shelf"]);
}

/// The Reminders folder lists its waiting conversations by when they are due, the soonest
/// first and Someday last; a row's date is its time's key, which gives the time back. A new
/// time moves the row; a wake takes it out.
#[test]
fn reminders_list_the_soonest_due_first() {
    use sealwax_domain::remind::{SOMEDAY, wake_of_key};
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "dentist", 1, &[]),
            mail(f.inbox, false, "taxes", 2, &[]),
            mail(f.inbox, false, "novel", 3, &[]),
            mail(f.inbox, false, "trip", 4, &[]),
            mail(f.inbox, false, "trip-2", 5, &["trip"]),
        ],
    );
    let threads: Vec<ThreadId> = ids[..4]
        .iter()
        .map(|id| thread(&f.store, f.inbox, *id))
        .collect();
    let (dentist, taxes, novel, trip) = (threads[0], threads[1], threads[2], threads[3]);
    let first = remind(&f.store, vec![dentist], ViewScope::Folder(f.inbox), at(300));
    let folder = reminders_folder(&f.store, f.account).unwrap();
    let mut watch = Watch::new(
        &f.store,
        &[ViewSpec::folder(f.inbox), ViewSpec::folder(folder)],
    );
    watch.settle(&f.store, first.version);
    for (thread, wake_at) in [(taxes, at(100)), (novel, SOMEDAY), (trip, at(200))] {
        let reminded = remind(&f.store, vec![thread], ViewScope::Folder(f.inbox), wake_at);
        watch.settle(&f.store, reminded.version);
    }
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(folder)),
        ["taxes", "trip", "dentist", "novel"]
    );
    let rows = whole(&f.store, ViewSpec::folder(folder).conversations()).conversations;
    assert_eq!(
        rows.iter()
            .map(|row| wake_of_key(row.date))
            .collect::<Vec<_>>(),
        [Some(at(100)), Some(at(200)), Some(at(300)), Some(SOMEDAY)]
    );
    // The two-message trip is one row, its latest message shown.
    assert_eq!(rows[1].messages, 2);
    // A new time moves it; the moves don't change (nothing new to send).
    let again = remind(&f.store, vec![dentist], ViewScope::Folder(f.inbox), at(50));
    assert_eq!(again.moved, 0);
    watch.settle(&f.store, again.version);
    assert!(watch.reminders_changed);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(folder)),
        ["dentist", "taxes", "trip", "novel"]
    );
    // Due ones leave it for the Inbox, sorted there by their return.
    let woken = f.store.wake_reminders(at(150)).recv_blocking().unwrap();
    assert_eq!(woken.woken.len(), 2);
    watch.settle(&f.store, woken.version);
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(folder)),
        ["trip", "novel"]
    );
    // Both came back at 150: the later mail first.
    assert_eq!(
        subjects(&f.store, ViewSpec::folder(f.inbox)),
        ["taxes", "dentist"]
    );
}

/// A search finds a conversation waiting in Reminders dated by its latest message, as every
/// row but the Reminders view's is: that view's row sorts and is dated by the wait key, which
/// the app reads back as the wake time, and a search through that folder showed it as a date
/// (blank: the final review of phase C), searching everywhere or in Reminders alone.
#[test]
fn a_search_dates_a_waiting_conversation_by_its_latest_message() {
    use sealwax_domain::remind::wake_of_key;
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "dentist", 1, &[]),
            mail(f.inbox, false, "dentist-2", 2, &["dentist"]),
            mail(f.inbox, false, "dentist-bill", 3, &[]),
        ],
    );
    let dentist = thread(&f.store, f.inbox, ids[0]);
    remind(&f.store, vec![dentist], ViewScope::Folder(f.inbox), at(500));
    let folder = reminders_folder(&f.store, f.account).unwrap();
    let found = |within: Option<FolderId>| -> Vec<(String, i64)> {
        f.store
            .search_conversations("dentist", within, 10)
            .recv_blocking()
            .unwrap()
            .value
            .into_iter()
            .map(|hit| (hit.conversation.subject, hit.conversation.date))
            .collect()
    };
    let waiting = ("dentist".to_owned(), at(2));
    let bill = ("dentist-bill".to_owned(), at(3));
    assert_eq!(found(None), [bill.clone(), waiting.clone()]);
    assert_eq!(found(Some(folder)), [waiting]);
    // The Reminders view's own row still says when it comes back.
    let rows = whole(&f.store, ViewSpec::folder(folder).conversations()).conversations;
    assert_eq!(wake_of_key(rows[0].date), Some(at(500)));
    // Answered before the wake ran: in the Inbox and in Reminders at once, dated by the answer.
    ingest(
        &f.store,
        vec![mail(f.inbox, false, "dentist-3", 4, &["dentist-2"])],
    );
    assert_eq!(found(None), [("dentist".to_owned(), at(4)), bill]);
    assert_eq!(found(Some(folder)), [("dentist".to_owned(), at(4))]);
    clean(&f.store);
}

/// Superhuman's "if no reply": someone else answers a waiting conversation, so the answer lands
/// in the Inbox, the reminder ends, and what waited comes back with it, whenever the wake runs
/// (the engine runs it when mail arrives). The user's own answer, in Sent or to themselves in
/// the Inbox, and a copy of a message it holds, leave it waiting.
#[test]
fn a_reply_from_someone_else_ends_the_reminder() {
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "quote", 1, &[]),
            mail(f.inbox, false, "quote-2", 2, &["quote"]),
            mail(f.inbox, false, "other", 3, &[]),
        ],
    );
    let quote = thread(&f.store, f.inbox, ids[0]);
    let mut watch = Watch::new(&f.store, &[ViewSpec::folder(f.inbox)]);
    let reminded = remind(&f.store, vec![quote], ViewScope::Folder(f.inbox), at(500));
    watch.settle(&f.store, reminded.version);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["other"]);

    // The user's own answers: one in Sent, one Cc'd to themselves into the Inbox.
    let mut cc = mail(f.inbox, true, "quote-4", 5, &["quote-2"]);
    cc.flags = Flags::SEEN;
    let (version, _) = ingest(
        &f.store,
        vec![mail(f.sent, true, "quote-3", 4, &["quote-2"]), cc],
    );
    watch.settle(&f.store, version);
    let woken = f.store.wake_reminders(at(10)).recv_blocking().unwrap();
    assert!(woken.ended.is_empty() && woken.woken.is_empty());
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(listed[0].state, ReminderState::Waiting);

    // Kari answers.
    let (version, _) = ingest(
        &f.store,
        vec![mail(f.inbox, false, "quote-5", 6, &["quote-4"])],
    );
    watch.settle(&f.store, version);
    let woken = f.store.wake_reminders(at(11)).recv_blocking().unwrap();
    assert_eq!(woken.ended, reminded.reminders);
    assert!(woken.woken.is_empty());
    watch.settle(&f.store, woken.version);
    assert!(watch.reminders_changed);
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
    // Every message of it is back in the Inbox, by its own date (no return to sort by).
    let rows = whole(&f.store, ViewSpec::folder(f.inbox).conversations()).conversations;
    assert_eq!(
        rows.iter()
            .map(|row| (row.subject.as_str(), row.members, row.date))
            .collect::<Vec<_>>(),
        [("quote", 4, at(6)), ("other", 1, at(3))]
    );
    let folder = reminders_folder(&f.store, f.account).unwrap();
    assert!(whole(&f.store, ViewSpec::folder(folder)).rows.is_empty());
}

/// A conversation back from Reminders is Important, whatever its mail and the user's choice
/// for its sender say, until it is acted on (the Split Inbox, phase C).
#[test]
fn a_conversation_back_from_reminders_is_important() {
    let f = fixture();
    let mut news = mail(f.inbox, false, "digest", 1, &[]);
    news.headers.facts = sealwax_domain::split::HeaderFacts::LIST_ID;
    let (_, ids) = ingest(&f.store, vec![news, mail(f.inbox, false, "lunch", 2, &[])]);
    let digest = thread(&f.store, f.inbox, ids[0]);
    let important = ViewSpec::folder(f.inbox).with_filter(ViewFilter::Important);
    let other = ViewSpec::folder(f.inbox).with_filter(ViewFilter::Other);
    let mut watch = Watch::new(&f.store, &[important, other, ViewSpec::folder(f.inbox)]);
    let version = f
        .store
        .set_sender_split("kari@example.test".into(), Some(Split::Other))
        .recv_blocking()
        .unwrap();
    watch.settle(&f.store, version);
    assert_eq!(subjects(&f.store, other), ["lunch", "digest"]);
    let reminded = remind(&f.store, vec![digest], ViewScope::Folder(f.inbox), at(10));
    watch.settle(&f.store, reminded.version);
    let woken = f.store.wake_reminders(at(10)).recv_blocking().unwrap();
    watch.settle(&f.store, woken.version);
    assert_eq!(subjects(&f.store, important), ["digest"]);
    assert_eq!(subjects(&f.store, other), ["lunch"]);
    let counts = f
        .store
        .split_counts(important)
        .recv_blocking()
        .unwrap()
        .value;
    assert_eq!((counts.important, counts.other), (1, 1));
    // Read, it stays; Done, it goes, and the rules apply to it again.
    let queued = triage(&f.store, Triage::Read(false), vec![ids[0]]);
    watch.settle(&f.store, queued.version);
    assert_eq!(subjects(&f.store, important), ["digest"]);
    let queued = triage(&f.store, Triage::Archive, vec![ids[0]]);
    watch.settle(&f.store, queued.version);
    assert_eq!(subjects(&f.store, important), Vec::<String>::new());
    let archived = ViewSpec::folder(f.archive).with_filter(ViewFilter::Other);
    assert_eq!(subjects(&f.store, archived), ["digest"]);
}

/// The account's folders as a listing gives them, `Reminders` among them or not.
fn listing(f: &Fx, reminders: bool) -> Vec<FolderSpec> {
    let mut listed = vec![
        FolderSpec::local(f.account, "INBOX", FolderRole::Inbox),
        FolderSpec::local(f.account, "Archive", FolderRole::Archive),
        FolderSpec::local(f.account, "Sent", FolderRole::Sent),
        FolderSpec::local(f.account, "Trash", FolderRole::Trash),
    ];
    if reminders {
        listed.push(FolderSpec::local(f.account, "Reminders", FolderRole::Other));
    }
    listed
}

/// The final review of phase B: a Reminders folder whose row goes (the label hidden from IMAP,
/// a listing that missed it) and comes back as a new row, its sync storing the conversation
/// again, still brings it back: the reminder looks in the account's Reminders folder by name.
#[test]
fn a_relisted_reminders_folder_still_brings_its_conversation_back() {
    let f = fixture();
    let (_, ids) = ingest(
        &f.store,
        vec![
            mail(f.inbox, false, "trip", 1, &[]),
            mail(f.inbox, false, "x", 2, &[]),
        ],
    );
    let trip = thread(&f.store, f.inbox, ids[0]);
    let reminded = remind(&f.store, vec![trip], ViewScope::Folder(f.inbox), at(100));
    assert_eq!(reminded.moved, 1);
    // Not listed: the row goes with what it held, and the reminder lets go of it.
    f.store
        .replace_folders(f.account, listing(&f, false))
        .recv_blocking()
        .unwrap();
    assert_eq!(reminders_folder(&f.store, f.account), None);
    let listed = f.store.reminders().recv_blocking().unwrap();
    assert_eq!(listed[0].folder, None);
    // Listed again, a new row; its sync stores the message again.
    f.store
        .replace_folders(f.account, listing(&f, true))
        .recv_blocking()
        .unwrap();
    let relisted = reminders_folder(&f.store, f.account).unwrap();
    ingest(&f.store, vec![mail(relisted, false, "trip", 1, &[])]);
    let woken = f.store.wake_reminders(at(100)).recv_blocking().unwrap();
    assert_eq!(woken.woken, reminded.reminders);
    assert_eq!(subjects(&f.store, ViewSpec::folder(f.inbox)), ["trip", "x"]);
    assert!(whole(&f.store, ViewSpec::folder(relisted)).rows.is_empty());
    clean(&f.store);
}

/// A waiting conversation archived from a relisted Reminders folder ends its reminder, as from
/// its own folder.
#[test]
fn acting_on_it_in_a_relisted_reminders_folder_ends_the_wait() {
    let f = fixture();
    let (_, ids) = ingest(&f.store, vec![mail(f.inbox, false, "trip", 1, &[])]);
    let trip = thread(&f.store, f.inbox, ids[0]);
    remind(&f.store, vec![trip], ViewScope::Folder(f.inbox), at(100));
    f.store
        .replace_folders(f.account, listing(&f, false))
        .recv_blocking()
        .unwrap();
    f.store
        .replace_folders(f.account, listing(&f, true))
        .recv_blocking()
        .unwrap();
    let relisted = reminders_folder(&f.store, f.account).unwrap();
    let (_, stored) = ingest(&f.store, vec![mail(relisted, false, "trip", 1, &[])]);
    triage(&f.store, Triage::Archive, stored);
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
    clean(&f.store);
}

/// The final review of phase B: a Reminders folder deleted with its messages leaves nothing
/// to bring back; at its time the reminder ends, without waiting a day for messages that can't
/// be on their way to a folder that is gone.
#[test]
fn a_reminder_whose_folder_went_with_its_messages_ends_at_its_time() {
    let f = fixture();
    let (_, ids) = ingest(&f.store, vec![mail(f.inbox, false, "trip", 1, &[])]);
    let trip = thread(&f.store, f.inbox, ids[0]);
    let reminded = remind(&f.store, vec![trip], ViewScope::Folder(f.inbox), at(100));
    f.store
        .replace_folders(f.account, listing(&f, false))
        .recv_blocking()
        .unwrap();
    let woken = f.store.wake_reminders(at(100)).recv_blocking().unwrap();
    assert!(woken.woken.is_empty());
    assert_eq!(woken.ended, reminded.reminders);
    assert_eq!(woken.next, None);
    assert!(f.store.reminders().recv_blocking().unwrap().is_empty());
    clean(&f.store);
}
