//! `fts_through` with batches whose two transactions interleave: the mark never passes a row
//! that still waits for its full-text entry, and the first start of this build looks at every
//! message once.

use super::*;
use crate::store::api::FolderSpec;
use crate::store::tests::{message, scratch};
use crate::store::views::Registry;
use crate::store::{Store, StoreOptions, schema};
use sealwax_domain::model::FolderRole;
use std::path::Path;
use std::sync::Mutex;

/// A store with one folder, closed again.
fn store_with_inbox(path: &Path) -> FolderId {
    let store = Store::open(path, StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Test".into(),
            address: "test@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let inbox = store
        .upsert_folder(FolderSpec::local(account, "INBOX", FolderRole::Inbox))
        .recv_blocking()
        .unwrap()
        .value;
    store.close().unwrap();
    inbox
}

/// The writer alone, its jobs run by hand.
fn writer(path: &Path) -> Writer {
    let shared = Arc::new(Shared {
        registry: Registry::new(),
        subscribers: Mutex::new(Vec::new()),
        blobs: None,
        index_progress: Mutex::new(None),
    });
    let (maintenance, _) = async_channel::bounded(8);
    Writer::new(
        schema::open_writer(path).unwrap(),
        shared,
        maintenance,
        1_000,
    )
    .unwrap()
}

fn through(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'fts_through'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn unindexed(conn: &Connection) -> Vec<i64> {
    conn.prepare(
        "SELECT id FROM messages m \
         WHERE NOT EXISTS (SELECT 1 FROM message_fts WHERE rowid = m.id) ORDER BY id",
    )
    .unwrap()
    .query_map([], |row| row.get(0))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn batch(folder: FolderId, prefix: &str) -> Vec<NewMessage> {
    (0..3)
        .map(|n| message(folder, &format!("{prefix}{n}"), n))
        .collect()
}

#[test]
fn the_mark_stays_below_a_batch_indexed_later() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let inbox = store_with_inbox(&path);
    let mut writer = writer(&path);
    let (_, _, seed) = writer.insert_batch(&batch(inbox, "seed")).unwrap();
    writer.index_docs(&seed).unwrap();
    let seeded = seed.iter().map(|doc| doc.id.0).max().unwrap();
    assert_eq!(through(&writer.conn), seeded);

    // A bulk batch commits its rows; before its entries, another batch commits and indexes.
    let (_, _, first) = writer.insert_batch(&batch(inbox, "first")).unwrap();
    assert_eq!(through(&writer.conn), seeded);
    let (_, _, second) = writer.insert_batch(&batch(inbox, "second")).unwrap();
    writer.index_docs(&second).unwrap();
    assert_eq!(through(&writer.conn), seeded, "below the waiting batch");
    let waiting: Vec<i64> = first.iter().map(|doc| doc.id.0).collect();
    assert_eq!(unindexed(&writer.conn), waiting);

    // The process stops here. The next start finds the rows past the mark.
    drop(writer);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    // The writer answers once its repair at the start is done.
    store.ingest(Vec::new()).recv_blocking().unwrap();
    let found = store.search("first1", None, 10).recv_blocking().unwrap();
    assert_eq!(found.value.len(), 1);
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    assert!(unindexed(&conn).is_empty());
    assert_eq!(through(&conn), *waiting.last().unwrap());
}

#[test]
fn the_mark_rises_once_every_batch_is_indexed() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let inbox = store_with_inbox(&path);
    let mut writer = writer(&path);
    let (_, _, first) = writer.insert_batch(&batch(inbox, "first")).unwrap();
    let (_, _, second) = writer.insert_batch(&batch(inbox, "second")).unwrap();
    writer.index_docs(&second).unwrap();
    assert_eq!(through(&writer.conn), first[0].id.0 - 1);
    writer.index_docs(&first).unwrap();
    assert!(writer.state.unindexed.is_empty());
    assert_eq!(through(&writer.conn), first[2].id.0);
    // The mark is lowered again with the rows of the next batch, in their transaction.
    let (_, _, third) = writer.insert_batch(&batch(inbox, "third")).unwrap();
    assert_eq!(through(&writer.conn), first[2].id.0);
    writer.index_docs(&third).unwrap();
    assert_eq!(through(&writer.conn), third[2].id.0);
    assert!(unindexed(&writer.conn).is_empty());
}

#[test]
fn rows_an_older_build_left_below_the_mark_are_indexed_once() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let inbox = store_with_inbox(&path);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    store.ingest(batch(inbox, "old")).recv_blocking().unwrap();
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let lost: i64 = conn
        .query_row("SELECT min(id) FROM messages", [], |row| row.get(0))
        .unwrap();
    conn.execute("DELETE FROM message_fts WHERE rowid = ?1", [lost])
        .unwrap();
    assert!(through(&conn) > lost);

    // This build has looked already: the mark is trusted.
    Store::open(&path, StoreOptions::default())
        .unwrap()
        .close()
        .unwrap();
    assert_eq!(unindexed(&conn), [lost]);

    // A store an older build wrote has no record of the look.
    conn.execute("DELETE FROM meta WHERE key = 'fts_rescanned'", [])
        .unwrap();
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    // The writer answers once its repair at the start is done.
    store.ingest(Vec::new()).recv_blocking().unwrap();
    let found = store.search("old0", None, 10).recv_blocking().unwrap();
    assert_eq!(found.value.len(), 1);
    store.close().unwrap();
    assert!(unindexed(&conn).is_empty());
}
