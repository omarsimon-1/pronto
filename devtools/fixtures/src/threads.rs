//! Headless measurements of the fork's conversation views (docs/superhuman/store.md) against
//! S3's bars: the conversation table's recount when a store first opens with the fork track
//! (its migration), a conversation view's open and pages, and one new message with its diffs
//! in three open conversation views, next to the same messages with no view open (what keeping
//! the table costs every write). Run it on a copy: it recounts the table, and inserts and
//! removes messages.

use crate::bench::{self, Options, evict, listen, section_starts, wait_for};
use crate::import::{ACCOUNT_NAME, account_address};
use crate::stats::{ms, summary};
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use sealwax_domain::model::{Flags, FolderId, FolderRole, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{
    ChangeEvent, OpenView, Store, ViewDiff, ViewFilter, ViewHandle, ViewSpec,
};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;

    println!("## Conversation table: recount at open (the fork migration)");
    let mut recount = Vec::new();
    let mut plain = Vec::new();
    for _ in 0..options.cold.max(1) {
        let conn = Connection::open(&options.store)?;
        conn.execute_batch(
            "DROP TABLE IF EXISTS fork_thread_folders; DELETE FROM meta WHERE key LIKE 'fork_%';",
        )?;
        drop(conn);
        evict(&options.store)?;
        let started = Instant::now();
        let store = bench::open(options, &blobs)?;
        recount.push(started.elapsed());
        store.close()?;
        evict(&options.store)?;
        let started = Instant::now();
        let store = bench::open(options, &blobs)?;
        plain.push(started.elapsed());
        store.close()?;
    }
    let conn = Connection::open(&options.store)?;
    let (rows, conversations): (i64, i64) = conn.query_row(
        "SELECT count(*), count(DISTINCT thread_id) FROM fork_thread_folders",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    drop(conn);
    println!("- {rows} rows for {conversations} conversations");
    println!(
        "- store open with the table dropped (create, recount, index; cache evicted): {}",
        summary(&recount)
    );
    println!(
        "- store open after it (nothing to do; cache evicted): {}",
        summary(&plain)
    );

    let mut store = bench::open(options, &blobs)?;
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
    let views = [
        ("INBOX", ViewSpec::folder(inbox).conversations()),
        (
            "INBOX unread",
            ViewSpec::folder(inbox)
                .with_filter(ViewFilter::Unread)
                .conversations(),
        ),
        (
            "INBOX starred",
            ViewSpec::folder(inbox)
                .with_filter(ViewFilter::Starred)
                .conversations(),
        ),
        ("Unified inbox", ViewSpec::unified_inbox().conversations()),
    ];

    println!(
        "\n## Conversation view open (count + 4 section counts + first 100 rows, one snapshot)"
    );
    for (name, spec) in views {
        let mut cold = Vec::new();
        let mut rows = 0;
        for _ in 0..options.cold {
            store.close()?;
            evict(&options.store)?;
            store = bench::open(options, &blobs)?;
            let (elapsed, total) = open_view(&store, spec, options.now)?;
            cold.push(elapsed);
            rows = total;
        }
        let mut warm = Vec::new();
        for _ in 0..options.warm {
            let (elapsed, total) = open_view(&store, spec, options.now)?;
            warm.push(elapsed);
            rows = total;
        }
        println!(
            "- {name} ({rows} conversations): cold {} | warm {}",
            summary(&cold),
            summary(&warm)
        );
    }

    for (name, spec) in [views[0], views[3]] {
        let total = store
            .open_view(OpenView {
                spec,
                first_page: 0,
                section_starts: Vec::new(),
            })
            .recv_blocking()?
            .total;
        println!("\n## Pages of 100 conversations, {name} ({total}), warm");
        for fraction in [0u32, 25, 50, 75, 100] {
            let offset = (u64::from(total.saturating_sub(100)) * u64::from(fraction) / 100) as u32;
            let mut samples = Vec::new();
            for _ in 0..options.warm {
                let started = Instant::now();
                let page = store.page(spec, offset, 100).recv_blocking()?;
                samples.push(started.elapsed());
                if page.conversations.len() != 100.min(total as usize) {
                    bail!("page at {offset} had {} rows", page.conversations.len());
                }
            }
            println!("- offset {offset}: {}", summary(&samples));
        }
    }

    let events = listen(&store);
    let sample = store
        .page(
            ViewSpec::folder(inbox),
            folders
                .iter()
                .find(|folder| folder.id == inbox)
                .map_or(0, |folder| folder.total / 3),
            100,
        )
        .recv_blocking()?
        .rows;
    let mut parents = Vec::new();
    for row in sample.iter().take(50) {
        if let Some(detail) = store.message(row.id).recv_blocking()?.value
            && let Some(id) = detail.headers.message_id
        {
            parents.push(id);
        }
    }
    let run = store.accounts().recv_blocking()?.version.0;
    println!("\n## One new message, no view open (the table kept, no diff)");
    let quiet = inserts(&store, &blobs, &events, inbox, &parents, options, run, 0)?;
    report(&quiet, options);

    println!("\n## One new message with its diffs in 3 open conversation views");
    let mut handles: Vec<ViewHandle> = Vec::new();
    for spec in [views[0].1, views[1].1, views[3].1] {
        let opened = store
            .open_view(OpenView {
                spec,
                first_page: 100,
                section_starts: Vec::new(),
            })
            .recv_blocking()?;
        handles.push(opened.view);
    }
    let open = inserts(&store, &blobs, &events, inbox, &parents, options, run, 1)?;
    report(&open, options);

    let mut flag_events = Vec::new();
    let mut last = store.accounts().recv_blocking()?.version;
    for n in 0..options.inserts.min(200) {
        let row = &sample[(n / 2) % sample.len()];
        let flip = n % 2 == 0;
        let (add, remove) = if flip != row.flags.is_seen() {
            (Flags::SEEN, Flags::NONE)
        } else {
            (Flags::NONE, Flags::SEEN)
        };
        let started = Instant::now();
        let version = store.set_flags(vec![row.id], add, remove).recv_blocking()?;
        if version == last {
            continue;
        }
        last = version;
        let (_, at) = wait_for(&events, version)?;
        flag_events.push(at - started);
    }
    println!(
        "- mark read or unread → change event received: {}",
        summary(&flag_events)
    );

    let inserted: Vec<MessageId> = quiet.inserted.into_iter().chain(open.inserted).collect();
    let removed = store.remove(inserted).recv_blocking()?;
    for key in &removed.orphaned_blobs {
        blobs.remove(key)?;
    }
    drop(handles);
    println!(
        "- cleanup: {} inserted messages removed, {} blobs deleted",
        removed.removed.len(),
        removed.orphaned_blobs.len()
    );
    let started = Instant::now();
    let problems = store.check().recv_blocking()?;
    println!(
        "- consistency check ({:.0} ms): {}",
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
    let opened = store
        .open_view(OpenView {
            spec,
            first_page: 100,
            section_starts: section_starts(now),
        })
        .recv_blocking()?;
    if opened.conversations.len() != 100.min(opened.total as usize) {
        bail!("the first page had {} rows", opened.conversations.len());
    }
    Ok((started.elapsed(), opened.total))
}

struct Inserts {
    writer: Vec<Duration>,
    to_event: Vec<Duration>,
    splices: usize,
    reloads: usize,
    inserted: Vec<MessageId>,
}

/// `options.inserts` messages into the INBOX one at a time, the four kinds of `bench` in turn
/// (new at the top, a reply, old and deep, HTML at the top).
#[allow(clippy::too_many_arguments)]
fn inserts(
    store: &Store,
    blobs: &BlobStore,
    events: &mpsc::Receiver<(Arc<ChangeEvent>, Instant)>,
    inbox: FolderId,
    parents: &[String],
    options: &Options,
    run: u64,
    round: usize,
) -> Result<Inserts> {
    let mut out = Inserts {
        writer: Vec::new(),
        to_event: Vec::new(),
        splices: 0,
        reloads: 0,
        inserted: Vec::new(),
    };
    let mut state = 0x7ead_u64 + round as u64;
    for n in 0..options.inserts {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let kind = n % 4;
        let date = match kind {
            2 => options.now - 86_400 * (30 + (state >> 33) as i64 % 1_500),
            _ => options.now + (round * options.inserts + n) as i64,
        };
        let parent =
            (kind == 1 && !parents.is_empty()).then(|| parents[n % parents.len()].as_str());
        let serial = round * options.inserts + n;
        let raw = bench::new_message(run, serial, kind, date, parent);
        let prepared = ingest::prepare(
            blobs,
            vec![RawMessage {
                folder: inbox,
                flags: Flags::NONE,
                raw,
                ..RawMessage::default()
            }],
            1,
        )?;
        let submitted = Instant::now();
        let ingested = ingest::submit(store, prepared).recv_blocking()?;
        if ingested.ids.iter().all(Option::is_none) {
            bail!("message {serial} was already in the store");
        }
        let (event, at) = wait_for(events, ingested.version)?;
        out.writer.push(ingested.header_time);
        out.to_event.push(at - submitted);
        out.inserted.extend(ingested.ids.iter().flatten());
        for change in &event.views {
            match change.diff {
                ViewDiff::Reload { .. } => out.reloads += 1,
                _ => out.splices += 1,
            }
        }
    }
    Ok(out)
}

fn report(inserts: &Inserts, options: &Options) {
    println!(
        "- writer transaction (insert, thread, counts, conversation recount and diffs, commit): {}",
        summary(&inserts.writer)
    );
    let kinds = ["new, top", "reply", "old, deep", "HTML, top"];
    for (kind, name) in kinds.iter().enumerate() {
        let of_kind: Vec<Duration> = inserts
            .writer
            .iter()
            .copied()
            .skip(kind)
            .step_by(4)
            .collect();
        println!("  {name}: {}", summary(&of_kind));
    }
    println!(
        "- submit → change event received: {}",
        summary(&inserts.to_event)
    );
    println!(
        "- {} view splices, {} reloads in {} events",
        inserts.splices, inserts.reloads, options.inserts
    );
}
