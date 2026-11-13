//! Headless measurements of the contact pane's read (docs/superhuman/contact.md) against its
//! bar, 5 ms at 100,000 messages: the fork step that builds the sender index at open and its
//! size, the read (the sender's messages, first and latest, and six conversations) for the
//! busiest senders, a middling one and the least busy one, cold and warm, and what the index
//! costs a new message: the writer's transaction with it and without it, in interleaved
//! rounds. Run it on a copy: it drops and builds the index, and inserts and removes messages.

use crate::bench::{self, Options, evict};
use crate::import::{ACCOUNT_NAME, account_address};
use crate::stats::{ms, summary};
use anyhow::{Context, Result};
use rusqlite::Connection;
use sealwax_domain::model::{Flags, FolderRole, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::Store;
use std::time::{Duration, Instant};

/// The fork step's index, as `fork.rs` makes it.
const INDEX: &str = "CREATE INDEX IF NOT EXISTS fork_message_split_by_sender \
    ON fork_message_split(sender, date, thread_id, folder_id, split) WHERE split <> 0";

/// The conversations the pane asks for: five shown, and the open one left out.
const LIMIT: u32 = 6;

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;
    // Whatever steps an older fixture lacks first, so the rounds below measure this one alone.
    bench::open(options, &blobs)?.close()?;

    println!("## The fork step at open: the sender index");
    let mut migration = Vec::new();
    let mut plain = Vec::new();
    for _ in 0..options.cold.max(1) {
        let conn = Connection::open(&options.store)?;
        conn.execute_batch(
            "DROP INDEX IF EXISTS fork_message_split_by_sender; \
             UPDATE meta SET value = 3 WHERE key = 'fork_schema';",
        )?;
        drop(conn);
        evict(&options.store)?;
        let started = Instant::now();
        let store = bench::open(options, &blobs)?;
        migration.push(started.elapsed());
        store.close()?;
        evict(&options.store)?;
        let started = Instant::now();
        let store = bench::open(options, &blobs)?;
        plain.push(started.elapsed());
        store.close()?;
    }
    println!(
        "- store open applying the step (cache evicted): {}",
        summary(&migration)
    );
    println!("- store open after it (cache evicted): {}", summary(&plain));
    let conn = Connection::open(&options.store)?;
    let size = |name: &str| -> Option<f64> {
        conn.query_row(
            "SELECT sum(pgsize) FROM dbstat WHERE name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .map(|bytes| bytes as f64 / 1_048_576.0)
    };
    if let (Some(index), Some(rows)) = (
        size("fork_message_split_by_sender"),
        size("fork_message_split"),
    ) {
        println!("- the index: {index:.1} MiB (the split rows themselves: {rows:.1} MiB)");
    }

    // Senders by their shown messages: the busiest (the fixture's own account), a newsletter's
    // size, a middling person's and the least busy.
    let senders: Vec<(String, i64)> = conn
        .prepare(
            "SELECT sender, count(*) AS n FROM fork_message_split WHERE split <> 0 \
             GROUP BY sender ORDER BY n DESC",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(conn);
    anyhow::ensure!(!senders.is_empty(), "no mail; run `import` first");
    let pick = |at: usize| senders[at.min(senders.len() - 1)].clone();
    let picked = [
        ("the busiest sender", pick(0)),
        ("the second busiest", pick(1)),
        ("a middling sender", pick(senders.len() / 2)),
        ("the least busy sender", pick(senders.len() - 1)),
    ];

    let mut store = bench::open(options, &blobs)?;
    println!("\n## The read: messages, first and latest, {LIMIT} conversations (one snapshot)");
    for (name, (sender, shown)) in &picked {
        let mut cold = Vec::new();
        for _ in 0..options.cold {
            store.close()?;
            evict(&options.store)?;
            store = bench::open(options, &blobs)?;
            cold.push(read(&store, sender)?.0);
        }
        let mut warm = Vec::new();
        let mut found = None;
        for _ in 0..options.warm {
            let (elapsed, facts) = read(&store, sender)?;
            warm.push(elapsed);
            found = Some(facts);
        }
        let (messages, recent) = found.unwrap_or_default();
        println!(
            "- {name} ({shown} rows, {messages} messages, {recent} conversations): \
             cold {} | warm {}",
            summary(&cold),
            summary(&warm)
        );
    }

    println!("\n## What the index costs a new message (writer transaction, interleaved rounds)");
    let account = store
        .accounts()
        .recv_blocking()?
        .value
        .into_iter()
        .find(|account| account.address == account_address(ACCOUNT_NAME))
        .context("no fixture account; run `import` first")?;
    let inbox = store
        .folders()
        .recv_blocking()?
        .value
        .into_iter()
        .find(|folder| folder.account == account.id && folder.role == FolderRole::Inbox)
        .context("no fixture INBOX")?
        .id;
    let from = picked[1].1.0.clone();
    let rounds = 4;
    let per_round = (options.inserts / (2 * rounds)).max(10);
    let mut with = Vec::new();
    let mut without = Vec::new();
    let run = store.accounts().recv_blocking()?.version.0;
    let mut serial = 0;
    for round in 0..2 * rounds {
        let indexed = round % 2 == 0;
        store.close()?;
        let conn = Connection::open(&options.store)?;
        if indexed {
            conn.execute_batch(INDEX)?;
        } else {
            conn.execute_batch("DROP INDEX IF EXISTS fork_message_split_by_sender")?;
        }
        drop(conn);
        store = bench::open(options, &blobs)?;
        let mut inserted: Vec<MessageId> = Vec::new();
        for _ in 0..per_round {
            serial += 1;
            let raw = format!(
                "From: Bench <{from}>\r\nTo: alex.berg@example.test\r\nSubject: Måling {serial}\r\n\
                 Date: {}\r\nMessage-ID: <contact-{run}-{serial}@example.test>\r\n\
                 MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHei.\r\n",
                crate::mime::date(options.now + serial, 0)
            );
            let prepared = ingest::prepare(
                &blobs,
                vec![RawMessage {
                    folder: inbox,
                    flags: Flags::NONE,
                    raw: raw.into_bytes(),
                    ..RawMessage::default()
                }],
                1,
            )?;
            let ingested = ingest::submit(&store, prepared).recv_blocking()?;
            if indexed {
                with.push(ingested.header_time);
            } else {
                without.push(ingested.header_time);
            }
            inserted.extend(ingested.ids.iter().flatten());
        }
        let removed = store.remove(inserted).recv_blocking()?;
        for key in &removed.orphaned_blobs {
            blobs.remove(key)?;
        }
    }
    println!("- with the sender index: {}", summary(&with));
    println!("- without it: {}", summary(&without));
    store.close()?;
    let conn = Connection::open(&options.store)?;
    conn.execute_batch(INDEX)?;
    drop(conn);

    let store = bench::open(options, &blobs)?;
    let started = Instant::now();
    let problems = store.check().recv_blocking()?;
    println!(
        "\n- consistency check ({:.0} ms): {}",
        ms(started.elapsed()),
        if problems.is_empty() {
            "ok".to_owned()
        } else {
            problems.join("; ")
        }
    );
    store.close()?;
    Ok(())
}

/// One read of `sender`'s facts: how long, and how many messages and conversations it found.
fn read(store: &Store, sender: &str) -> Result<(Duration, (u32, usize))> {
    let started = Instant::now();
    let facts = store.contact(sender, LIMIT).recv_blocking()?.value;
    Ok((started.elapsed(), (facts.messages, facts.recent.len())))
}
