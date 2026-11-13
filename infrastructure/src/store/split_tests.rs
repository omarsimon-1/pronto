//! The Split Inbox in the store (docs/superhuman/split.md): the header facts, the server's
//! categories, a conversation the user wrote in and the user's choices by sender sort messages
//! and conversations into Important and Other views and counts; a choice or a category
//! classifies a sender's or a thread's mail again in one transaction with its diffs; the fork
//! step backfills an older store from what it kept; and the split rows stay exact when a row id
//! comes back in the same commit, or after an older build wrote. Change events of every kind
//! are checked against fresh pages by the replay (`conversation_view_tests.rs`).

use super::ops_tests::ready;
use super::tests::{scratch, whole};
use super::*;
use rusqlite::Connection;
use sealwax_domain::model::{Address, FolderRole, Headers};
use sealwax_domain::split::{HeaderFacts, Split};

const ME: &str = "me@example.test";

struct Fx {
    _dir: tempfile::TempDir,
    store: Store,
    account: AccountId,
    inbox: FolderId,
    sent: FolderId,
}

fn open(dir: &tempfile::TempDir) -> Store {
    Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap()
}

fn folder(store: &Store, account: AccountId, name: &str, role: FolderRole) -> FolderId {
    store
        .upsert_folder(FolderSpec::local(account, name, role))
        .recv_blocking()
        .unwrap()
        .value
}

fn fixture() -> Fx {
    let dir = scratch();
    let store = open(&dir);
    let account = store
        .add_account(NewAccount {
            name: "Me".into(),
            address: ME.into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    Fx {
        inbox: folder(&store, account, "INBOX", FolderRole::Inbox),
        sent: folder(&store, account, "Sent", FolderRole::Sent),
        _dir: dir,
        store,
        account,
    }
}

/// Message `key` from `from`, replying to `parent`, with `facts`.
fn mail(
    folder: FolderId,
    key: &str,
    minute: i64,
    from: &str,
    parent: Option<&str>,
    facts: HeaderFacts,
) -> NewMessage {
    let parent = parent.map(|parent| format!("{parent}@example.test"));
    NewMessage {
        folder,
        date: 1_790_000_000 + minute * 60,
        flags: Flags::SEEN,
        headers: Headers {
            from: vec![Address::new("", from)],
            subject: format!("Subject {key}"),
            message_id: Some(format!("{key}@example.test")),
            in_reply_to: parent.iter().cloned().collect(),
            references: parent.into_iter().collect(),
            facts,
            ..Headers::default()
        },
        snippet: format!("Snippet {key}"),
        size: 100,
        ..NewMessage::default()
    }
}

fn ingest(store: &Store, messages: Vec<NewMessage>) -> Vec<MessageId> {
    store
        .ingest(messages)
        .recv_blocking()
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

/// The ids a message view lists.
fn messages(store: &Store, spec: ViewSpec) -> Vec<MessageId> {
    whole(store, spec).rows.iter().map(|row| row.id).collect()
}

/// The conversations' latest messages a conversation view lists.
fn conversations(store: &Store, spec: ViewSpec) -> Vec<MessageId> {
    whole(store, spec.conversations())
        .conversations
        .iter()
        .map(|row| row.latest)
        .collect()
}

fn counts(store: &Store, spec: ViewSpec) -> (u32, u32) {
    let counts = store.split_counts(spec).recv_blocking().unwrap().value;
    (counts.important, counts.other)
}

fn clean(store: &Store) {
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}

#[test]
fn facts_categories_replies_and_choices_sort_messages_and_conversations() {
    let f = fixture();
    let inbox = ViewSpec::folder(f.inbox);
    let important = inbox.with_filter(ViewFilter::Important);
    let other = inbox.with_filter(ViewFilter::Other);
    let mut gmail = mail(
        f.inbox,
        "promo",
        4,
        "kari@example.test",
        None,
        HeaderFacts::NONE,
    );
    gmail.category = Some(Split::Other);
    let mut primary = mail(
        f.inbox,
        "digest",
        5,
        "digest@example.test",
        None,
        HeaderFacts::LIST_UNSUBSCRIBE | HeaderFacts::ROBOT,
    );
    primary.category = Some(Split::Important);
    let ids = ingest(
        &f.store,
        vec![
            mail(
                f.inbox,
                "ingrid",
                1,
                "ingrid@example.test",
                None,
                HeaderFacts::NONE,
            ),
            mail(
                f.inbox,
                "news",
                2,
                "news@shop.example.test",
                None,
                HeaderFacts::LIST_UNSUBSCRIBE | HeaderFacts::BULK,
            ),
            mail(
                f.inbox,
                "ingrid2",
                3,
                "ingrid@example.test",
                Some("ingrid"),
                HeaderFacts::NONE,
            ),
            gmail,
            primary,
        ],
    );
    let [ingrid, news, ingrid2, promo, digest] = ids[..] else {
        unreachable!()
    };
    // The header rules, and the server's category over them both ways.
    assert_eq!(messages(&f.store, important), [digest, ingrid2, ingrid]);
    assert_eq!(messages(&f.store, other), [promo, news]);
    assert_eq!(conversations(&f.store, important), [digest, ingrid2]);
    assert_eq!(conversations(&f.store, other), [promo, news]);
    assert_eq!(counts(&f.store, inbox), (3, 2));
    assert_eq!(counts(&f.store, inbox.conversations()), (2, 2));
    assert_eq!(
        counts(&f.store, ViewSpec::unified_inbox().conversations()),
        (2, 2)
    );

    // The user answers the newsletter: its conversation is Important now, whatever else says.
    ingest(
        &f.store,
        vec![mail(
            f.sent,
            "reply",
            6,
            ME,
            Some("news"),
            HeaderFacts::NONE,
        )],
    );
    assert_eq!(messages(&f.store, other), [promo]);
    assert_eq!(conversations(&f.store, other), [promo]);
    assert_eq!(counts(&f.store, inbox.conversations()), (3, 1));

    // Always Other for Ingrid: one transaction, with the diffs of every open view.
    let events = f.store.subscribe();
    let views: Vec<ViewHandle> = [important, other, important.conversations()]
        .into_iter()
        .map(|spec| {
            f.store
                .open_view(OpenView {
                    spec,
                    first_page: 100,
                    section_starts: Vec::new(),
                })
                .recv_blocking()
                .unwrap()
                .view
        })
        .collect();
    let version = f
        .store
        .set_sender_split(" Ingrid@Example.TEST ".into(), Some(Split::Other))
        .recv_blocking()
        .unwrap();
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.version, version);
    let mut diffs: Vec<ViewId> = event.views.iter().map(|change| change.view).collect();
    diffs.sort_unstable();
    assert_eq!(diffs, views.iter().map(ViewHandle::id).collect::<Vec<_>>());
    assert!(events.try_recv().is_err(), "one commit");
    assert_eq!(messages(&f.store, important), [digest, news]);
    assert_eq!(messages(&f.store, other), [promo, ingrid2, ingrid]);
    assert_eq!(
        f.store.sender_splits().recv_blocking().unwrap(),
        [("ingrid@example.test".to_owned(), Split::Other)]
    );
    // The same choice again changes nothing; forgetting it puts her mail back.
    assert_eq!(
        f.store
            .set_sender_split("ingrid@example.test".into(), Some(Split::Other))
            .recv_blocking()
            .unwrap(),
        version
    );
    f.store
        .set_sender_split("ingrid@example.test".into(), None)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        messages(&f.store, important),
        [digest, ingrid2, news, ingrid]
    );
    // A later category from the server moves a message too.
    f.store
        .set_categories(vec![(promo, Split::Important), (ingrid, Split::Other)])
        .recv_blocking()
        .unwrap();
    assert_eq!(messages(&f.store, other), [ingrid]);
    assert_eq!(conversations(&f.store, other), Vec::<MessageId>::new());
    assert!(f.store.sender_splits().recv_blocking().unwrap().is_empty());
    let _ = f.account;
    clean(&f.store);
}

#[test]
fn a_message_stored_where_another_went_in_the_same_commit_keeps_its_own_facts() {
    let f = fixture();
    ready(&f.store, f.inbox, 7);
    let mut old = mail(
        f.inbox,
        "old",
        1,
        "news@shop.example.test",
        None,
        HeaderFacts::LIST_ID,
    );
    old.uid = Some(1);
    let first = ingest(&f.store, vec![old])[0];
    // The server expunged it and has a new message: the writer removes, then inserts, and
    // SQLite gives the new row the id of the newest row just deleted.
    let mut new = mail(
        f.inbox,
        "new",
        2,
        "kari@example.test",
        None,
        HeaderFacts::NONE,
    );
    new.uid = Some(2);
    let synced = f
        .store
        .sync_changes(
            f.inbox,
            FolderChanges {
                new: vec![new],
                removed: vec![1],
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(synced.new_messages, [first], "the id came back");
    assert_eq!(
        messages(
            &f.store,
            ViewSpec::folder(f.inbox).with_filter(ViewFilter::Important)
        ),
        [first]
    );
    clean(&f.store);
}

#[test]
fn the_fork_step_backfills_older_stores_from_what_they_kept() {
    let f = fixture();
    let ids = ingest(
        &f.store,
        vec![
            mail(
                f.inbox,
                "a",
                1,
                "kari@example.test",
                None,
                HeaderFacts::NONE,
            ),
            mail(
                f.inbox,
                "b",
                2,
                "kari@example.test",
                None,
                HeaderFacts::LIST_UNSUBSCRIBE,
            ),
            {
                let mut listed = mail(
                    f.inbox,
                    "c",
                    3,
                    "kari@example.test",
                    None,
                    HeaderFacts::LIST_ID,
                );
                listed.headers.list_id = Some("turlaget.example.test".into());
                listed
            },
            mail(
                f.inbox,
                "d",
                4,
                "noreply@bank.example.test",
                None,
                HeaderFacts::ROBOT,
            ),
        ],
    );
    let important = ViewSpec::folder(f.inbox).with_filter(ViewFilter::Important);
    let other = ViewSpec::folder(f.inbox).with_filter(ViewFilter::Other);
    assert_eq!(messages(&f.store, important), [ids[0]]);
    let path = f.store.path().to_owned();
    let Fx {
        _dir: dir, store, ..
    } = f;
    store.close().unwrap();

    // A store of the fork's first step (phase A): no split rows, choices or counts.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE fork_message_split; DROP TABLE fork_sender_split; \
         DROP TABLE fork_folder_split; UPDATE meta SET value = 1 WHERE key = 'fork_schema';",
    )
    .unwrap();
    drop(conn);
    let store = open(&dir);
    // Only the List-Id and the sender were kept: the List-Unsubscribe-only message is
    // Important now; the listed one and the robot's are Other.
    assert_eq!(messages(&store, important), [ids[1], ids[0]]);
    assert_eq!(messages(&store, other), [ids[3], ids[2]]);
    clean(&store);
    store.close().unwrap();

    // An older build deleted the newest message and stored another under its id.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE copy AS SELECT * FROM messages WHERE id = {old}; \
         DELETE FROM messages WHERE id = {old}; \
         UPDATE copy SET from_addr = 'ingrid@example.test', list_id = NULL, uid = uid + 100; \
         INSERT INTO messages SELECT * FROM copy; \
         UPDATE store_version SET version = version + 1;",
        old = ids[3].0
    ))
    .unwrap();
    drop(conn);
    let store = open(&dir);
    assert_eq!(messages(&store, important), [ids[3], ids[1], ids[0]]);
    clean(&store);
}

#[test]
fn all_inboxes_hold_a_conversation_in_other_only_without_an_important_member() {
    let f = fixture();
    let second = folder(&f.store, f.account, "Inbox 2", FolderRole::Inbox);
    ingest(
        &f.store,
        vec![
            mail(
                f.inbox,
                "a",
                1,
                "kari@example.test",
                None,
                HeaderFacts::NONE,
            ),
            mail(
                second,
                "b",
                2,
                "news@example.test",
                Some("a"),
                HeaderFacts::BULK,
            ),
            mail(second, "c", 3, "news@example.test", None, HeaderFacts::BULK),
        ],
    );
    let all = ViewSpec::unified_inbox().conversations();
    // One account's two inboxes fold: the conversation of a and b counts once, Important.
    assert_eq!(whole(&f.store, all).conversations.len(), 2);
    assert_eq!(counts(&f.store, all), (1, 1));
    let other = whole(&f.store, all.with_filter(ViewFilter::Other)).conversations;
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].subject, "Subject c");
    // Each inbox alone: b's row is Other in its own folder.
    assert_eq!(
        counts(&f.store, ViewSpec::folder(second).conversations()),
        (0, 2)
    );
    assert_eq!(counts(&f.store, ViewSpec::unified_inbox()), (1, 2));
    clean(&f.store);
}

#[test]
fn uncategorized_lists_a_folders_mail_without_a_category_newest_first() {
    let f = fixture();
    let mut known = mail(
        f.inbox,
        "a",
        3,
        "kari@example.test",
        None,
        HeaderFacts::NONE,
    );
    known.category = Some(Split::Other);
    known.uid = Some(3);
    let mut first = mail(
        f.inbox,
        "b",
        1,
        "kari@example.test",
        None,
        HeaderFacts::NONE,
    );
    first.uid = Some(1);
    let mut second = mail(
        f.inbox,
        "c",
        2,
        "kari@example.test",
        None,
        HeaderFacts::NONE,
    );
    second.uid = Some(2);
    ready(&f.store, f.inbox, 7);
    let ids = ingest(&f.store, vec![known, first, second]);
    let found = f.store.uncategorized(f.inbox, 10).recv_blocking().unwrap();
    assert_eq!(
        found,
        [
            Uncategorized {
                message: ids[2],
                uid: 2
            },
            Uncategorized {
                message: ids[1],
                uid: 1
            }
        ]
    );
    assert_eq!(
        f.store
            .uncategorized(f.inbox, 1)
            .recv_blocking()
            .unwrap()
            .len(),
        1
    );
}

/// Phase C: a side of the Split Inbox combines with the other filters (`ViewSpec::split`), in
/// both groupings, and its open views follow a flag change; the tabs' counts are the sides'
/// totals whatever the filter.
#[test]
fn a_side_combines_with_the_other_filters() {
    let f = fixture();
    let mut kari = mail(
        f.inbox,
        "kari",
        1,
        "kari@example.test",
        None,
        HeaderFacts::NONE,
    );
    kari.flags = Flags::NONE;
    let mut ingrid = mail(
        f.inbox,
        "ingrid",
        2,
        "ingrid@example.test",
        None,
        HeaderFacts::NONE,
    );
    ingrid.flags = Flags::SEEN | Flags::FLAGGED;
    let mut news = mail(
        f.inbox,
        "news",
        3,
        "news@example.test",
        None,
        HeaderFacts::LIST_ID,
    );
    news.flags = Flags::NONE;
    let mut receipt = mail(
        f.inbox,
        "receipt",
        4,
        "receipts@shop.example.test",
        None,
        HeaderFacts::ROBOT,
    );
    receipt.has_attachments = true;
    let [kari, ingrid, news, receipt] = ingest(&f.store, vec![kari, ingrid, news, receipt])[..]
    else {
        panic!("four messages");
    };
    let side = |filter: ViewFilter, split: Split| {
        ViewSpec::folder(f.inbox)
            .with_filter(filter)
            .with_split(Some(split))
    };
    for (spec, wanted) in [
        (side(ViewFilter::Unread, Split::Important), vec![kari]),
        (side(ViewFilter::Unread, Split::Other), vec![news]),
        (side(ViewFilter::Starred, Split::Important), vec![ingrid]),
        (side(ViewFilter::Starred, Split::Other), vec![]),
        (side(ViewFilter::Attachments, Split::Other), vec![receipt]),
        (side(ViewFilter::All, Split::Important), vec![ingrid, kari]),
    ] {
        assert_eq!(messages(&f.store, spec), wanted, "{spec:?}");
        assert_eq!(conversations(&f.store, spec), wanted, "{spec:?}");
    }
    // The split filter is the side alone, and wins over `split`.
    assert_eq!(
        messages(
            &f.store,
            ViewSpec::folder(f.inbox)
                .with_filter(ViewFilter::Other)
                .with_split(Some(Split::Important))
        ),
        [receipt, news]
    );
    // The tabs count each side whatever the filter.
    let unread = ViewSpec::folder(f.inbox).with_filter(ViewFilter::Unread);
    assert_eq!(counts(&f.store, unread.conversations()), (2, 2));
    assert_eq!(
        counts(&f.store, unread.with_split(Some(Split::Other))),
        (2, 2)
    );

    // Kari read: she leaves the Important side's unread views by their diffs.
    let events = f.store.subscribe();
    let specs = [
        side(ViewFilter::Unread, Split::Important),
        side(ViewFilter::Unread, Split::Important).conversations(),
    ];
    let views: Vec<ViewHandle> = specs
        .iter()
        .map(|spec| {
            f.store
                .open_view(OpenView {
                    spec: *spec,
                    first_page: 100,
                    section_starts: Vec::new(),
                })
                .recv_blocking()
                .unwrap()
                .view
        })
        .collect();
    let version = f
        .store
        .set_flags(vec![kari], Flags::SEEN, Flags::NONE)
        .recv_blocking()
        .unwrap();
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.version, version);
    for view in &views {
        let diff = event
            .views
            .iter()
            .find(|change| change.view == view.id())
            .map(|change| change.diff.clone());
        match diff {
            Some(ViewDiff::Splice { removed, total, .. })
            | Some(ViewDiff::ConversationSplice { removed, total, .. }) => {
                assert_eq!((removed, total), (vec![0], 0), "{:?}", view.spec());
            }
            other => panic!("{:?}: {other:?}", view.spec()),
        }
    }
    clean(&f.store);
}

/// A sender with a lot of mail: its choice classifies the conversations the user acted on and
/// those the open views show in its first transaction, the rest in transactions of their own,
/// and a write queued right after it goes before them (phase C: a triage after a choice for a
/// sender of 21,000 messages waits for one transaction at most).
#[test]
fn a_choice_for_a_busy_sender_goes_in_parts_with_other_writes_between() {
    let f = fixture();
    let archive = folder(&f.store, f.account, "Archive", FolderRole::Archive);
    let busy = "news@example.test";
    let total = super::writer::split::CHOICE_CHUNK as usize * 2 + 200;
    let mut mail_of: Vec<NewMessage> = (0..total)
        .map(|n| {
            mail(
                archive,
                &format!("old{n}"),
                n as i64,
                busy,
                None,
                HeaderFacts::NONE,
            )
        })
        .collect();
    // Two of its conversations in the Inbox, one the user acts on in the Archive, and Kari's.
    mail_of.push(mail(
        f.inbox,
        "shown1",
        5_000,
        busy,
        None,
        HeaderFacts::NONE,
    ));
    mail_of.push(mail(
        f.inbox,
        "shown2",
        5_001,
        busy,
        None,
        HeaderFacts::NONE,
    ));
    mail_of.push(mail(archive, "acted", 1, busy, None, HeaderFacts::NONE));
    mail_of.push(mail(
        f.inbox,
        "kari",
        5_002,
        "kari@example.test",
        None,
        HeaderFacts::NONE,
    ));
    let ids = ingest(&f.store, mail_of);
    let [.., shown1, shown2, acted, kari] = ids[..] else {
        panic!("the samples");
    };
    let thread = |id: MessageId| {
        f.store
            .message(id)
            .recv_blocking()
            .unwrap()
            .value
            .unwrap()
            .summary
            .thread
    };
    let important = ViewSpec::folder(f.inbox)
        .with_split(Some(Split::Important))
        .conversations();
    let opened = f
        .store
        .open_view(OpenView {
            spec: important,
            first_page: 100,
            section_starts: Vec::new(),
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(opened.total, 3);
    let events = f.store.subscribe();
    // The test holds the database's write lock while it queues both, so the writer, waiting in
    // the choice's first transaction, can't run a part before Kari's flag is queued, however
    // loaded the machine is: the flag then goes right after the first part (writes the user
    // waits for come before the parts).
    let lock = Connection::open(f.store.path()).unwrap();
    lock.busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let chosen =
        f.store
            .set_sender_split_first(busy.into(), Some(Split::Other), vec![thread(acted)]);
    let flagged = f.store.set_flags(vec![kari], Flags::NONE, Flags::SEEN);
    lock.execute_batch("COMMIT").unwrap();
    drop(lock);
    let read = flagged.recv_blocking().unwrap();
    let done = chosen.recv_blocking().unwrap();
    assert!(read < done, "Kari's flag went between the choice's parts");
    // The first part moved the conversations shown and the one acted on.
    let first = events.recv_blocking().unwrap();
    let change = first
        .views
        .iter()
        .find(|change| change.view == opened.view.id())
        .expect("the Inbox's Important side changed in the first part");
    assert!(
        matches!(&change.diff, ViewDiff::ConversationSplice { removed, total, .. }
            if removed.len() == 2 && *total == 1),
        "{:?}",
        change.diff
    );
    let parts = 1 + std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| event.version > read)
        .count();
    assert!(parts >= 3, "{parts} parts");
    assert_eq!(conversations(&f.store, important), [kari]);
    let other = ViewSpec::folder(archive)
        .with_split(Some(Split::Other))
        .conversations();
    assert_eq!(whole(&f.store, other).conversations.len(), total + 1);
    let _ = (shown1, shown2);
    let path = f.store.path().to_owned();
    let pending: Option<i64> = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT value FROM meta WHERE key = 'fork_split_pending'",
            [],
            |row| row.get(0),
        )
        .ok();
    assert_eq!(pending, None, "done");
    clean(&f.store);
}

/// A choice cut short by a crash between its parts leaves `fork_split_pending`: the next open
/// recounts the store, so its conversations end on the chosen side.
#[test]
fn a_choice_cut_short_is_finished_when_the_store_opens_again() {
    let f = fixture();
    let ids = ingest(
        &f.store,
        vec![
            mail(
                f.inbox,
                "a",
                1,
                "news@example.test",
                None,
                HeaderFacts::NONE,
            ),
            mail(
                f.inbox,
                "b",
                2,
                "kari@example.test",
                None,
                HeaderFacts::NONE,
            ),
        ],
    );
    let path = f.store.path().to_owned();
    let Fx {
        _dir, store, inbox, ..
    } = f;
    store.close().unwrap();
    // As a crash would leave it: the choice stored, its mail not classified again yet.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "INSERT INTO fork_sender_split(address, important, added) \
           VALUES ('news@example.test', 0, 0); \
         INSERT INTO meta(key, value) VALUES ('fork_split_pending', 1);",
    )
    .unwrap();
    drop(conn);
    let store = open(&_dir);
    let inbox = ViewSpec::folder(inbox);
    assert_eq!(
        messages(&store, inbox.with_split(Some(Split::Other))),
        [ids[0]]
    );
    assert_eq!(
        messages(&store, inbox.with_split(Some(Split::Important))),
        [ids[1]]
    );
    clean(&store);
}
