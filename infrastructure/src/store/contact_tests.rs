//! The contact pane's read (docs/superhuman/contact.md): a sender's messages counted once
//! outside Trash and Junk, their first and latest dates (undated mail dates nothing), and the
//! conversations they last wrote in, newest first, each opening at its best copy; the writer
//! keeps the sender index exact as mail comes, hides and goes; the fork step indexes an older
//! store, and replaces the sender index an older build left under its old name; both reads
//! stay on the index.

use super::tests::scratch;
use super::*;
use rusqlite::Connection;
use sealwax_domain::model::{Address, FolderRole, Headers};

const ME: &str = "me@example.test";
const INGRID: &str = "ingrid@example.test";

struct Fx {
    _dir: tempfile::TempDir,
    store: Store,
    account: AccountId,
    inbox: FolderId,
    archive: FolderId,
    sent: FolderId,
    trash: FolderId,
    junk: FolderId,
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
        archive: folder(&store, account, "Archive", FolderRole::Archive),
        sent: folder(&store, account, "Sent", FolderRole::Sent),
        trash: folder(&store, account, "Trash", FolderRole::Trash),
        junk: folder(&store, account, "Junk", FolderRole::Junk),
        _dir: dir,
        store,
        account,
    }
}

/// Message `key` from `from` at `minute`, in `folder`, replying to `parent`.
fn mail(folder: FolderId, key: &str, minute: i64, from: &str, parent: Option<&str>) -> NewMessage {
    let parent = parent.map(|parent| format!("{parent}@example.test"));
    NewMessage {
        folder,
        date: 1_790_000_000 + minute * 60,
        flags: Flags::SEEN,
        headers: Headers {
            from: vec![Address::new("", from)],
            subject: format!("Re: Subject {}", parent.as_deref().unwrap_or(key)),
            message_id: Some(format!("{key}@example.test")),
            in_reply_to: parent.iter().cloned().collect(),
            references: parent.into_iter().collect(),
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

fn contact(store: &Store, address: &str, limit: u32) -> ContactFacts {
    store.contact(address, limit).recv_blocking().unwrap().value
}

fn date(minute: i64) -> i64 {
    1_790_000_000 + minute * 60
}

/// The conversations' latest messages from the sender, as the pane lists them.
fn recent(facts: &ContactFacts) -> Vec<MessageId> {
    facts.recent.iter().map(|found| found.message).collect()
}

fn clean(store: &Store) {
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}

#[test]
fn a_senders_mail_counts_once_and_their_conversations_come_newest_first() {
    let f = fixture();
    // A label folder made after the Inbox: its copy of a message comes first in the walk.
    let label = folder(&f.store, f.account, "Hytta", FolderRole::Other);
    let ids = ingest(
        &f.store,
        vec![
            mail(f.inbox, "a1", 1, INGRID, None),
            mail(f.archive, "b1", 3, INGRID, None),
            mail(f.inbox, "a2", 5, INGRID, Some("a1")),
            mail(f.sent, "a3", 6, ME, Some("a2")),
            mail(f.inbox, "c1", 7, INGRID, None),
            mail(label, "c1", 7, INGRID, None),
            mail(f.trash, "t1", 9, INGRID, None),
            mail(f.junk, "j1", 10, INGRID, None),
            mail(f.inbox, "d1", 11, INGRID, None),
            mail(f.inbox, "k1", 12, "kari@example.test", None),
        ],
    );
    let [a1, b1, a2, _a3, c1, c1_label, _t1, _j1, d1, _k1] = ids[..] else {
        panic!("{ids:?}");
    };
    // A message the user deleted and the server hasn't expunged yet is hidden.
    f.store
        .set_flags(vec![d1], Flags::DELETED, Flags::NONE)
        .recv_blocking()
        .unwrap();

    let facts = contact(&f.store, INGRID, 6);
    // a1, b1, a2 and c1 (its label's copy once); the Trash, the Junk and the deleted one not.
    assert_eq!(facts.messages, 4);
    assert_eq!((facts.first, facts.last), (Some(date(1)), Some(date(7))));
    // c1 opens at its Inbox copy, which ranks above the label's.
    assert_eq!(recent(&facts), [c1, a2, b1]);
    assert_ne!(c1, c1_label);
    assert_eq!(facts.recent[0].folder, f.inbox);
    assert_eq!(facts.recent[1].folder, f.inbox);
    assert_eq!(facts.recent[2].folder, f.archive);
    assert_eq!(facts.recent[0].subject, "Subject c1");
    assert_eq!(facts.recent[1].subject, "Subject a1");
    assert_eq!(facts.recent[1].date, date(5));
    // The limit, and the address in any case.
    assert_eq!(recent(&contact(&f.store, INGRID, 2)), [c1, a2]);
    assert_eq!(contact(&f.store, " Ingrid@Example.TEST ", 6), facts);
    assert_eq!(
        contact(&f.store, INGRID, 0),
        ContactFacts {
            recent: Vec::new(),
            ..facts.clone()
        }
    );
    // The user's own, a stranger's and no address.
    assert_eq!(contact(&f.store, ME, 6).messages, 1);
    assert_eq!(
        contact(&f.store, "nobody@example.test", 6),
        ContactFacts::default()
    );
    assert_eq!(contact(&f.store, "  ", 6), ContactFacts::default());
    let _ = (a1, b1);
    clean(&f.store);
}

#[test]
fn the_index_follows_mail_that_comes_hides_and_goes() {
    let f = fixture();
    let ids = ingest(
        &f.store,
        vec![
            mail(f.inbox, "a1", 1, INGRID, None),
            mail(f.inbox, "b1", 2, INGRID, None),
        ],
    );
    assert_eq!(recent(&contact(&f.store, INGRID, 6)), [ids[1], ids[0]]);
    // A reply in the older conversation brings it to the top.
    let reply = ingest(&f.store, vec![mail(f.inbox, "a2", 3, INGRID, Some("a1"))]);
    let facts = contact(&f.store, INGRID, 6);
    assert_eq!(recent(&facts), [reply[0], ids[1]]);
    assert_eq!(facts.messages, 3);
    // Hidden by a pending delete, then shown again.
    f.store
        .set_flags(vec![reply[0]], Flags::DELETED, Flags::NONE)
        .recv_blocking()
        .unwrap();
    assert_eq!(recent(&contact(&f.store, INGRID, 6)), [ids[1], ids[0]]);
    f.store
        .set_flags(vec![reply[0]], Flags::NONE, Flags::DELETED)
        .recv_blocking()
        .unwrap();
    assert_eq!(recent(&contact(&f.store, INGRID, 6)), [reply[0], ids[1]]);
    // Expunged.
    f.store.remove(vec![ids[1]]).recv_blocking().unwrap();
    let facts = contact(&f.store, INGRID, 6);
    assert_eq!(recent(&facts), [reply[0]]);
    assert_eq!(facts.messages, 2);
    // A detached account's mail isn't counted: the app doesn't show it.
    let conn = Connection::open(f.store.path()).unwrap();
    conn.execute("UPDATE accounts SET detached = 1", [])
        .unwrap();
    drop(conn);
    assert_eq!(contact(&f.store, INGRID, 6), ContactFacts::default());
}

#[test]
fn the_fork_step_indexes_an_older_store_once() {
    let f = fixture();
    let ids = ingest(
        &f.store,
        vec![
            mail(f.inbox, "a1", 1, INGRID, None),
            mail(f.archive, "b1", 2, INGRID, None),
        ],
    );
    let path = f.store.path().to_owned();
    let Fx {
        _dir: dir, store, ..
    } = f;
    store.close().unwrap();

    // A store of the fork's third step: no sender index.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP INDEX fork_message_split_by_sender; \
         UPDATE meta SET value = 3 WHERE key = 'fork_schema';",
    )
    .unwrap();
    drop(conn);
    let store = open(&dir);
    assert_eq!(recent(&contact(&store, INGRID, 6)), [ids[1], ids[0]]);
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let schema: i64 = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'fork_schema'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema, super::fork::VERSION);
    assert_eq!(sender_indexes(&conn), [new_index()]);
    drop(conn);
    // Opened again, nothing more to do.
    let store = open(&dir);
    assert_eq!(contact(&store, INGRID, 6).messages, 2);
    clean(&store);
}

/// The fourth step's index: its name, and what it indexes.
fn new_index() -> (String, String) {
    (
        "fork_message_split_by_sender".to_owned(),
        "(sender, date, thread_id, folder_id, split) WHERE split <> 0".to_owned(),
    )
}

/// The indexes on the split rows with "sender" in their name, each with what follows `ON
/// fork_message_split` in its definition.
fn sender_indexes(conn: &Connection) -> Vec<(String, String)> {
    let mut statement = conn
        .prepare(
            "SELECT name, sql FROM sqlite_master WHERE type = 'index' \
             AND tbl_name = 'fork_message_split' AND name LIKE '%sender%' ORDER BY name",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            let sql: String = row.get(1)?;
            let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let on = sql
                .split_once(" ON fork_message_split")
                .map(|(_, indexed)| indexed.to_owned())
                .unwrap_or_default();
            Ok((row.get(0)?, on))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// `sql`'s query plan on `conn`, its steps one per line.
fn plan(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> String {
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let rows = statement
        .query_map(params, |row| row.get::<_, String>(3))
        .unwrap();
    rows.map(Result::unwrap).collect::<Vec<_>>().join("\n")
}

/// Both reads walk the fourth step's index alone, in its order.
fn assert_covering(conn: &Connection) {
    let count = plan(
        conn,
        super::reader::contact::COUNT,
        &[&INGRID, &"trash", &"junk"],
    );
    assert!(
        count.contains("COVERING INDEX fork_message_split_by_sender"),
        "{count}"
    );
    assert!(!count.contains("TEMP B-TREE"), "{count}");
    let newest = plan(conn, super::reader::contact::NEWEST, &[&INGRID]);
    assert!(
        newest.contains("COVERING INDEX fork_message_split_by_sender"),
        "{newest}"
    );
    assert!(!newest.contains("TEMP B-TREE"), "{newest}");
}

#[test]
fn the_fork_step_replaces_the_sender_index_an_older_build_left() {
    let f = fixture();
    let ids = ingest(
        &f.store,
        vec![
            mail(f.inbox, "a1", 1, INGRID, None),
            mail(f.archive, "b1", 2, INGRID, None),
        ],
    );
    let path = f.store.path().to_owned();
    let Fx {
        _dir: dir, store, ..
    } = f;
    store.close().unwrap();

    // A store of the fork's third step from a build made while the second was written: an
    // index on the sender alone, under the name the fourth step's index once had.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP INDEX IF EXISTS fork_message_split_by_sender; \
         DROP INDEX IF EXISTS fork_message_split_sender; \
         CREATE INDEX fork_message_split_sender ON fork_message_split(sender); \
         UPDATE meta SET value = 3 WHERE key = 'fork_schema';",
    )
    .unwrap();
    assert_eq!(
        sender_indexes(&conn),
        [(
            "fork_message_split_sender".to_owned(),
            "(sender)".to_owned()
        )]
    );
    drop(conn);
    let store = open(&dir);
    assert_eq!(recent(&contact(&store, INGRID, 6)), [ids[1], ids[0]]);
    store.close().unwrap();
    // The old index is gone, the new one in its place, and both reads cover on it.
    let conn = Connection::open(&path).unwrap();
    let schema: i64 = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'fork_schema'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema, super::fork::VERSION);
    assert_eq!(sender_indexes(&conn), [new_index()]);
    assert_covering(&conn);
    drop(conn);
    let store = open(&dir);
    clean(&store);
}

#[test]
fn undated_mail_counts_but_dates_nothing() {
    let f = fixture();
    // Neither an INTERNALDATE nor a Date header: the ingest stores it at 0.
    let undated = |folder: FolderId, key: &str, from: &str| NewMessage {
        date: 0,
        ..mail(folder, key, 0, from, None)
    };
    let ids = ingest(
        &f.store,
        vec![
            undated(f.inbox, "u1", INGRID),
            mail(f.inbox, "a1", 4, INGRID, None),
            mail(f.archive, "b1", 9, INGRID, None),
            undated(f.inbox, "k1", "kari@example.test"),
        ],
    );
    let facts = contact(&f.store, INGRID, 6);
    assert_eq!(facts.messages, 3);
    assert_eq!((facts.first, facts.last), (Some(date(4)), Some(date(9))));
    // Its conversation comes last: the walk reaches it after every dated one.
    assert_eq!(recent(&facts), [ids[2], ids[1], ids[0]]);
    assert_eq!(facts.recent[2].date, 0);
    let kari = contact(&f.store, "kari@example.test", 6);
    assert_eq!((kari.messages, kari.first, kari.last), (1, None, None));
    assert_eq!(recent(&kari), [ids[3]]);
    clean(&f.store);
}

#[test]
fn both_reads_stay_on_the_sender_index() {
    let f = fixture();
    ingest(&f.store, vec![mail(f.inbox, "a1", 1, INGRID, None)]);
    let conn = Connection::open(f.store.path()).unwrap();
    assert_covering(&conn);
}
