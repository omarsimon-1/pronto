//! Headless measurements of Remind Me's store (docs/superhuman/remind.md): a remind and a wake
//! of one conversation, each to its change event with three conversation views open; one new
//! message's writer transaction without reminders and with a few hundred conversations back
//! from Reminders (the recount reads when its dirty threads came back, `fork::DIRTY_BUMPS`);
//! and `check`. Phase C adds the same insert while a few hundred wait in Reminders (the recount
//! reads where the dirty threads wait, `fork::DIRTY_WAITS`), and the timer's wake with nothing
//! due while they wait (the "if no reply" check, which runs whenever new mail arrives). The
//! fixture account is a local one, so the moves are done in the transaction, as the store's
//! side of any account's are. Run it on a copy: it moves, inserts and removes messages.

use crate::bench::{self, Options, listen, wait_for};
use crate::import::{ACCOUNT_NAME, account_address};
use crate::stats::{ms, summary};
use anyhow::{Context, Result};
use rusqlite::Connection;
use sealwax_domain::model::{Flags, FolderRole, ThreadId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{OpenView, Remind, Store, ViewHandle, ViewScope, ViewSpec};
use std::time::{Duration, Instant};

/// Conversations reminded and woken for the recount's side of the measures.
const REMINDED: usize = 300;

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;
    let store = bench::open(options, &blobs)?;
    let folders = store.folders().recv_blocking()?.value;
    let accounts = store.accounts().recv_blocking()?.value;
    let account = accounts
        .iter()
        .find(|account| account.address == account_address(ACCOUNT_NAME))
        .context("no fixture account; run `import` first")?;
    let role = |role: FolderRole| {
        folders
            .iter()
            .find(|folder| folder.account == account.id && folder.role == role)
            .map(|folder| folder.id)
    };
    let inbox = role(FolderRole::Inbox).context("no fixture INBOX")?;
    let archive = role(FolderRole::Archive).unwrap_or(inbox);
    let events = listen(&store);
    let mut handles: Vec<ViewHandle> = Vec::new();
    for spec in [
        ViewSpec::folder(inbox).conversations(),
        ViewSpec::unified_inbox().conversations(),
        ViewSpec::folder(archive).conversations(),
    ] {
        let opened = store
            .open_view(OpenView {
                spec,
                first_page: 100,
                section_starts: Vec::new(),
            })
            .recv_blocking()?;
        handles.push(opened.view);
    }
    // The Inbox's conversations, newest first: the ones the measures send away.
    let threads: Vec<ThreadId> = store
        .page(ViewSpec::folder(inbox).conversations(), 0, 2_000)
        .recv_blocking()?
        .conversations
        .iter()
        .map(|row| row.thread)
        .collect();
    let run = store.accounts().recv_blocking()?.version.0;

    println!("## One new message, 3 conversation views open, no reminder");
    let (plain, plain_inserted) = inserts(&store, &blobs, &events, inbox, options, run, 0)?;
    println!("- writer transaction: {}", summary(&plain));

    println!("\n## A remind of one conversation → change event (3 views open)");
    let mut reminds = Vec::new();
    let mut moved = 0;
    for thread in threads.iter().take(options.warm) {
        let started = Instant::now();
        let reminded = store
            .remind(Remind {
                threads: vec![*thread],
                scope: ViewScope::Folder(inbox),
                wake_at: options.now,
                not_before: 0,
            })
            .recv_blocking()?;
        wait_for(&events, reminded.version)?;
        reminds.push(started.elapsed());
        moved += reminded.moved;
    }
    println!(
        "- {} conversations, {moved} messages: {}",
        reminds.len(),
        summary(&reminds)
    );

    println!("\n## A wake of one conversation → change event (back to the top of the Inbox)");
    // Each due reminder alone: one at a time, its time the oldest still waiting.
    let conn = Connection::open(&options.store)?;
    let due: Vec<(i64, i64)> = conn
        .prepare("SELECT id, wake_at FROM fork_reminders WHERE state = 0 ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (n, (id, _)) in due.iter().enumerate() {
        conn.execute(
            "UPDATE fork_reminders SET wake_at = ?2 WHERE id = ?1",
            [*id, options.now + n as i64],
        )?;
    }
    let mut wakes = Vec::new();
    for n in 0..due.len() {
        let started = Instant::now();
        let woken = store
            .wake_reminders(options.now + n as i64)
            .recv_blocking()?;
        wait_for(&events, woken.version)?;
        wakes.push(started.elapsed());
    }
    println!("- {} wakes: {}", wakes.len(), summary(&wakes));

    println!("\n## {REMINDED} conversations back from Reminders");
    let started = Instant::now();
    let reminded = store
        .remind(Remind {
            threads: threads
                .iter()
                .skip(options.warm)
                .take(REMINDED)
                .copied()
                .collect(),
            scope: ViewScope::Folder(inbox),
            wake_at: options.now,
            not_before: 0,
        })
        .recv_blocking()?;
    wait_for(&events, reminded.version)?;
    println!(
        "- one remind of {} conversations ({} messages): {:.1} ms",
        reminded.reminders.len(),
        reminded.moved,
        ms(started.elapsed())
    );
    let (waiting, waiting_inserted) = inserts(&store, &blobs, &events, inbox, options, run, 2)?;
    println!(
        "- while they wait, one new message, 3 views open, writer transaction: {}",
        summary(&waiting)
    );
    let mut idle = Vec::new();
    for _ in 0..options.warm {
        let started = Instant::now();
        let woken = store.wake_reminders(options.now - 1).recv_blocking()?;
        idle.push(started.elapsed());
        anyhow::ensure!(woken.woken.is_empty() && woken.ended.is_empty());
    }
    println!(
        "- while they wait, a wake with nothing due or answered: {}",
        summary(&idle)
    );
    let started = Instant::now();
    let woken = store.wake_reminders(options.now + 10_000).recv_blocking()?;
    wait_for(&events, woken.version)?;
    println!(
        "- one wake of {}: {:.1} ms",
        woken.woken.len(),
        ms(started.elapsed())
    );
    let (bumped, bumped_inserted) = inserts(&store, &blobs, &events, inbox, options, run, 1)?;
    println!(
        "- one new message, 3 views open, writer transaction: {}",
        summary(&bumped)
    );

    let removed = store
        .remove(
            plain_inserted
                .into_iter()
                .chain(waiting_inserted)
                .chain(bumped_inserted)
                .collect(),
        )
        .recv_blocking()?;
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

/// `options.inserts` new messages into `inbox`, one at a time: the writer transactions and the
/// stored ids.
fn inserts(
    store: &Store,
    blobs: &BlobStore,
    events: &std::sync::mpsc::Receiver<(
        std::sync::Arc<sealwax_infrastructure::store::ChangeEvent>,
        Instant,
    )>,
    inbox: sealwax_domain::model::FolderId,
    options: &Options,
    run: u64,
    round: usize,
) -> Result<(Vec<Duration>, Vec<sealwax_domain::model::MessageId>)> {
    let mut writer = Vec::new();
    let mut inserted = Vec::new();
    for n in 0..options.inserts {
        let raw = bench::new_message(
            run,
            round * 100_000 + n,
            n % 4,
            options.now + n as i64,
            None,
        );
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
        let ingested = ingest::submit(store, prepared).recv_blocking()?;
        wait_for(events, ingested.version)?;
        writer.push(ingested.header_time);
        inserted.extend(ingested.ids.iter().flatten());
    }
    Ok((writer, inserted))
}
