//! Headless measurements of the Split Inbox's store (docs/superhuman/split.md) against S3's
//! bars: the fork step that backfills a store of the fork's first step (every message's split
//! row from what the store kept, then the recount), the Important and Other views' opens and
//! pages in both groupings, one new message with its diffs in three open split views, and a
//! choice for a frequent sender. The fixture's header facts are made up first (two in five
//! messages bulk), so both sides are large. Run it on a copy: it rewrites the split rows, and
//! inserts and removes messages.

use crate::bench::{self, Options, evict, listen, section_starts, wait_for};
use crate::import::{ACCOUNT_NAME, account_address};
use crate::stats::{ms, summary};
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use sealwax_domain::model::{Flags, FolderRole};
use sealwax_domain::split::Split;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{OpenView, Store, ViewDiff, ViewFilter, ViewHandle, ViewSpec};
use std::time::{Duration, Instant};

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;

    println!("## The fork step at open: split rows from what the store kept, then the recount");
    let mut migration = Vec::new();
    let mut plain = Vec::new();
    for _ in 0..options.cold.max(1) {
        let conn = Connection::open(&options.store)?;
        conn.execute_batch(
            "DROP TABLE IF EXISTS fork_message_split; DROP TABLE IF EXISTS fork_sender_split; \
             DROP TABLE IF EXISTS fork_folder_split; \
             UPDATE meta SET value = 1 WHERE key = 'fork_schema';",
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

    // Two in five messages bulk, so both sides hold plenty; the store recounts at open (an
    // older build wrote, as far as it can tell).
    let conn = Connection::open(&options.store)?;
    conn.execute_batch(
        "UPDATE fork_message_split SET facts = 2 WHERE id % 5 < 2; \
         UPDATE store_version SET version = version + 1;",
    )?;
    let started = Instant::now();
    drop(conn);
    let mut store = bench::open(options, &blobs)?;
    println!(
        "- recount at open after an older build wrote: {:.0} ms",
        ms(started.elapsed())
    );
    let conn = Connection::open(&options.store)?;
    let (rows, important, other): (i64, i64, i64) = conn.query_row(
        "SELECT count(*), sum(split = 1), sum(split = 2) FROM fork_message_split",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    println!("- {rows} split rows: {important} Important, {other} Other");
    let size: Option<i64> = conn
        .query_row(
            "SELECT sum(pgsize) FROM dbstat WHERE name LIKE 'fork_message_split%'",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(size) = size {
        println!(
            "- split rows and their indexes: {:.1} MiB",
            size as f64 / 1_048_576.0
        );
    }
    drop(conn);

    let folders = store.folders().recv_blocking()?.value;
    let accounts = store.accounts().recv_blocking()?.value;
    let account = accounts
        .iter()
        .find(|account| account.address == account_address(ACCOUNT_NAME))
        .context("no fixture account; run `import` first")?;
    let inbox = folders
        .iter()
        .find(|folder| folder.account == account.id && folder.role == FolderRole::Inbox)
        .context("no fixture INBOX")?
        .id;
    let important = ViewSpec::folder(inbox).with_filter(ViewFilter::Important);
    let other = ViewSpec::folder(inbox).with_filter(ViewFilter::Other);
    let unified = ViewSpec::unified_inbox();
    let views = [
        ("INBOX Important, messages", important),
        ("INBOX Other, messages", other),
        ("INBOX Important, conversations", important.conversations()),
        ("INBOX Other, conversations", other.conversations()),
        (
            "Unified Important, conversations",
            unified.with_filter(ViewFilter::Important).conversations(),
        ),
        (
            "Unified Other, conversations",
            unified.with_filter(ViewFilter::Other).conversations(),
        ),
    ];

    println!("\n## View open (count + 4 section counts + first 100 rows, one snapshot)");
    for (name, spec) in views {
        let mut cold = Vec::new();
        let mut total = 0;
        for _ in 0..options.cold {
            store.close()?;
            evict(&options.store)?;
            store = bench::open(options, &blobs)?;
            let (elapsed, rows) = open_view(&store, spec, options.now)?;
            cold.push(elapsed);
            total = rows;
        }
        let mut warm = Vec::new();
        for _ in 0..options.warm {
            let (elapsed, rows) = open_view(&store, spec, options.now)?;
            warm.push(elapsed);
            total = rows;
        }
        println!(
            "- {name} ({total} rows): cold {} | warm {}",
            summary(&cold),
            summary(&warm)
        );
    }

    for (name, spec) in views {
        let total = store
            .open_view(OpenView {
                spec,
                first_page: 0,
                section_starts: Vec::new(),
            })
            .recv_blocking()?
            .total;
        println!("\n## Pages of 100, {name} ({total}), warm");
        for fraction in [0u32, 25, 50, 75, 100] {
            let offset = (u64::from(total.saturating_sub(100)) * u64::from(fraction) / 100) as u32;
            let mut samples = Vec::new();
            for _ in 0..options.warm {
                let started = Instant::now();
                let page = store.page(spec, offset, 100).recv_blocking()?;
                samples.push(started.elapsed());
                if page.len() != 100.min(total as usize) {
                    bail!("page at {offset} had {} rows", page.len());
                }
            }
            println!("- offset {offset}: {}", summary(&samples));
        }
    }

    println!("\n## One new message with its diffs in 3 open split views");
    let events = listen(&store);
    let mut handles: Vec<ViewHandle> = Vec::new();
    for spec in [important, other.conversations(), views[4].1] {
        let opened = store
            .open_view(OpenView {
                spec,
                first_page: 100,
                section_starts: Vec::new(),
            })
            .recv_blocking()?;
        handles.push(opened.view);
    }
    let run = store.accounts().recv_blocking()?.version.0;
    let mut writer = Vec::new();
    let mut to_event = Vec::new();
    let mut splices = 0;
    let mut inserted = Vec::new();
    for n in 0..options.inserts {
        let raw = bench::new_message(run, n, n % 4, options.now + n as i64, None);
        let prepared = ingest::prepare(
            &blobs,
            vec![RawMessage {
                folder: inbox,
                flags: Flags::NONE,
                raw,
                ..RawMessage::default()
            }],
            1,
        )?;
        let submitted = Instant::now();
        let ingested = ingest::submit(&store, prepared).recv_blocking()?;
        let (event, at) = wait_for(&events, ingested.version)?;
        writer.push(ingested.header_time);
        to_event.push(at - submitted);
        inserted.extend(ingested.ids.iter().flatten());
        splices += event
            .views
            .iter()
            .filter(|change| !matches!(change.diff, ViewDiff::Reload { .. }))
            .count();
    }
    println!(
        "- writer transaction (insert, classify, recount, diffs, commit): {}",
        summary(&writer)
    );
    println!("- submit → change event received: {}", summary(&to_event));
    println!("- {splices} view splices in {} events", options.inserts);

    println!("\n## A choice for the most frequent sender (its mail classified again)");
    let conn = Connection::open(&options.store)?;
    let (sender, messages): (String, i64) = conn.query_row(
        "SELECT sender, count(*) AS n FROM fork_message_split GROUP BY sender \
         ORDER BY n DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    drop(conn);
    let mut samples: Vec<Duration> = Vec::new();
    for choice in [Some(Split::Other), None, Some(Split::Important), None] {
        let started = Instant::now();
        let version = store
            .set_sender_split(sender.clone(), choice)
            .recv_blocking()?;
        wait_for(&events, version)?;
        samples.push(started.elapsed());
    }
    println!(
        "- {messages} messages of one sender: choice → last change event: {}",
        summary(&samples)
    );

    // Phase C: the choice goes in parts (`writer/split.rs`), the conversation acted on in the
    // first; a triage queued right behind it waits for one part at most. The Inbox's two sides
    // open, as the app holds them (the side shown and the other, opened ahead). Twice: the
    // busiest sender (the fixture's own address, all of it in Sent) and the busiest in the
    // Inbox (a newsletter, whose conversations the open sides show and move).
    let sides: Vec<ViewHandle> = [important.conversations(), other.conversations()]
        .into_iter()
        .map(|spec| {
            store
                .open_view(OpenView {
                    spec,
                    first_page: 100,
                    section_starts: Vec::new(),
                })
                .recv_blocking()
                .map(|opened| opened.view)
        })
        .collect::<Result<_, _>>()?;
    let conn = Connection::open(&options.store)?;
    let inbox_sender: (String, i64) = conn.query_row(
        "SELECT sender, count(*) AS n FROM fork_message_split WHERE folder_id = ?1 \
         GROUP BY sender ORDER BY n DESC LIMIT 1",
        [inbox.0],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    drop(conn);
    for (chosen_sender, label) in [
        (
            sender.clone(),
            format!("{messages} messages, none in the Inbox"),
        ),
        (
            inbox_sender.0.clone(),
            format!(
                "the busiest in the Inbox, {} messages there",
                inbox_sender.1
            ),
        ),
    ] {
        println!("\n## A choice in parts, a triage right behind it (phase C): {label}");
        let conn = Connection::open(&options.store)?;
        // The conversation the user acts on: the sender's newest, in the Inbox where it has one.
        let acted: i64 = conn.query_row(
            "SELECT thread_id FROM fork_message_split WHERE sender = ?1 \
             ORDER BY folder_id = ?2 DESC, date DESC LIMIT 1",
            rusqlite::params![chosen_sender, inbox.0],
            |row| row.get(0),
        )?;
        // Inbox mail of other senders, read or unread in turn by the triage.
        let others: Vec<i64> = conn
            .prepare(
                "SELECT id FROM fork_message_split WHERE sender <> ?1 AND folder_id = ?2 \
                 ORDER BY date DESC LIMIT 8",
            )?
            .query_map(rusqlite::params![chosen_sender, inbox.0], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(conn);
        let mut first_part = Vec::new();
        let mut triage = Vec::new();
        let mut totals = Vec::new();
        let mut parts = Vec::new();
        for (round, choice) in [Some(Split::Other), None, Some(Split::Other), None]
            .into_iter()
            .enumerate()
        {
            let before = store.accounts().recv_blocking()?.version;
            let started = Instant::now();
            let chosen = store.set_sender_split_first(
                chosen_sender.clone(),
                choice,
                vec![sealwax_domain::model::ThreadId(acted)],
            );
            // Every change event's arrival from here on, by version.
            let mut seen: Vec<(sealwax_infrastructure::store::StoreVersion, Instant)> = Vec::new();
            let next = |seen: &mut Vec<_>| -> Result<()> {
                let (event, at) = events
                    .recv_timeout(Duration::from_secs(10))
                    .context("no change event")?;
                seen.push((event.version, at));
                Ok(())
            };
            // The first part's event: the conversation acted on moved.
            while seen.iter().all(|(version, _)| *version <= before) {
                next(&mut seen)?;
            }
            let first_at = seen.last().map(|(_, at)| *at).context("no first part")?;
            first_part.push(first_at - started);
            // A triage right behind it: Mark as Read or Unread on another sender's Inbox mail.
            let id = sealwax_domain::model::MessageId(others[round % others.len()]);
            let submitted = Instant::now();
            let queued = store
                .queue_triage(sealwax_infrastructure::store::QueueTriage {
                    action: sealwax_domain::triage::Triage::Read(round % 2 == 0),
                    messages: vec![id],
                    not_before: 0,
                })
                .recv_blocking()?;
            let done = chosen.recv_blocking()?;
            let last = done.max(queued.version);
            while seen.iter().all(|(version, _)| *version < last) {
                next(&mut seen)?;
            }
            let at = |wanted| {
                seen.iter()
                    .find(|(version, _)| *version == wanted)
                    .map(|(_, at)| *at)
                    .context("an event went missing")
            };
            triage.push(at(queued.version)? - submitted);
            totals.push(at(done)? - started);
            // Every commit since the choice but the triage's is one of its parts.
            parts.push(done.0 - before.0 - u64::from(queued.version < done));
        }
        println!(
            "- choice → its first part's change event (the conversation acted on moved): {}",
            summary(&first_part)
        );
        println!(
            "- a triage queued right behind it → its change event: {}",
            summary(&triage)
        );
        println!(
            "- choice → its last part's change event: {} (parts: {parts:?})",
            summary(&totals)
        );
    }
    drop(sides);

    let removed = store.remove(inserted).recv_blocking()?;
    for key in &removed.orphaned_blobs {
        blobs.remove(key)?;
    }
    drop(handles);
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

fn open_view(store: &Store, spec: ViewSpec, now: i64) -> Result<(Duration, u32)> {
    let started = Instant::now();
    let mut opened = store
        .open_view(OpenView {
            spec,
            first_page: 100,
            section_starts: section_starts(now),
        })
        .recv_blocking()?;
    let rows = opened.take_rows();
    if rows.len() != 100.min(opened.total as usize) {
        bail!("the first page had {} rows", rows.len());
    }
    Ok((started.elapsed(), opened.total))
}
