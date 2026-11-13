//! The Split Inbox's writes (docs/superhuman/split.md): the user's choice for a sender and the
//! server's categories, each classifying the conversations concerned again.
//!
//! A choice for a sender with many messages (tens of thousands) takes most of a second to
//! classify again, which would hold every other write up as long (phase C: a triage right
//! after it must stay under 100 ms). So a choice is applied in transactions of at most
//! [`CHOICE_CHUNK`] of the sender's messages: the first one at once, with the conversations the
//! user acted on and those the open views show first, the others between the writer's other
//! jobs ([`Writer::reclassify_step`]). Until the last one commits, `meta('fork_split_pending')`
//! says so: a store opened after a crash between two of them is recounted whole.
//!
//! And its message views' diffs: their rows are the messages `fork_message_split` publishes
//! as Important, or as Other, in their folders, kept by the view's filter on top (the Important
//! side's unread messages). A thread's
//! recount can move messages the transaction never touched from one side to the other (a reply
//! the user sent makes the whole conversation Important), so these views are diffed as the
//! conversation views are (`writer/conversations.rs`): the dirty threads' split rows as the
//! last commit published them against the recount. Positions after the commit are ranks among
//! the recounted rows; a position before it is the same rank, less the dirty threads' messages
//! above it now and plus those above it before. A message the transaction touched that stays in
//! the view is changed, as in the other message views.

use super::{MAX_SPLICE, State, Touched, Writer, conversations};
use crate::store::api::{ChangeEvent, Positioned, StoreVersion, ViewDiff, ViewFilter};
use crate::store::views::Kept;
use crate::store::{StoreError, fork, views};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sealwax_domain::model::{FolderId, MessageId, ThreadId};
use sealwax_domain::split::{self, Split};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// The most of a sender's messages one transaction of a choice classifies again (measured at
/// 100,000 messages: about 42 µs a message, so a transaction holds the writer for about 20 ms).
/// The conversations the user acted on go in the first whatever their size.
pub(crate) const CHOICE_CHUNK: u32 = 500;

/// `meta` while a choice's transactions are still to come.
pub(crate) const PENDING: &str = "fork_split_pending";

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

/// A choice's conversations still to classify again, each with its messages from the sender,
/// and who waits for the last of them.
pub(crate) struct Reclassify {
    address: String,
    threads: VecDeque<(ThreadId, u32)>,
    replies: Vec<Sender<StoreVersion>>,
}

impl Writer {
    /// The user's choice for a sender: `Some` puts every message from `address` (any account,
    /// stored or still to come) on that side, `None` forgets the choice. The sender's
    /// conversations are classified again with the views' diffs: `first` (the conversations
    /// the user acted on) and those the open views show in the first transaction, the rest in
    /// transactions of their own after it. `reply` hears the last one's version.
    pub(super) fn set_sender_split(
        &mut self,
        address: &str,
        choice: Option<Split>,
        first: &[ThreadId],
        reply: Sender<StoreVersion>,
    ) {
        match self.choose(address, choice, first) {
            Ok(Some(mut work)) => {
                work.replies.push(reply);
                self.reclassify.push_back(work);
            }
            Ok(None) => {
                let _ = reply.try_send(Ok(StoreVersion(self.state.version)));
            }
            Err(error) => {
                let _ = reply.try_send(Err(error));
            }
        }
    }

    /// Applies a choice and its first transaction; the conversations left, if any.
    fn choose(
        &mut self,
        address: &str,
        choice: Option<Split>,
        first: &[ThreadId],
    ) -> Result<Option<Reclassify>, StoreError> {
        let Some(address) = split::sender_key(address) else {
            return Ok(None);
        };
        let shown = self.open_folders();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = match choice {
            Some(choice) => tx
                .prepare_cached(
                    "INSERT INTO fork_sender_split(address, important, added) \
                     VALUES (?1, ?2, unixepoch()) ON CONFLICT(address) DO UPDATE SET \
                       important = excluded.important, added = excluded.added \
                     WHERE important <> excluded.important",
                )?
                .execute(params![address, choice.is_important()])?,
            None => tx
                .prepare_cached("DELETE FROM fork_sender_split WHERE address = ?1")?
                .execute([&address])?,
        };
        if changed == 0 {
            return Ok(None);
        }
        // A choice for this sender still under way is superseded: its conversations are all
        // in this one's, and its waiters hear when this one is done.
        let mut replies = Vec::new();
        self.reclassify.retain_mut(|work| {
            let same = work.address == address;
            if same {
                replies.append(&mut work.replies);
            }
            !same
        });
        let (mut threads, firsts) = sender_threads(&tx, &address, first, &shown)?;
        let chunk = take_chunk(&mut threads, firsts);
        fork::set_pending(
            &tx,
            PENDING,
            !threads.is_empty() || !self.reclassify.is_empty(),
        )?;
        let version = classify_again(&mut self.state, tx, &chunk)?;
        if threads.is_empty() {
            for reply in replies {
                let _ = reply.try_send(Ok(version));
            }
            return Ok(None);
        }
        Ok(Some(Reclassify {
            address,
            threads,
            replies,
        }))
    }

    /// The next transaction of the oldest choice under way (the writer runs it when no other
    /// job waits): its next conversations classified again, with the views' diffs.
    pub(super) fn reclassify_step(&mut self) {
        let Some(mut work) = self.reclassify.pop_front() else {
            return;
        };
        let chunk = take_chunk(&mut work.threads, 0);
        let done = work.threads.is_empty();
        let result = (|| {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            fork::set_pending(&tx, PENDING, !done || !self.reclassify.is_empty())?;
            classify_again(&mut self.state, tx, &chunk)
        })();
        match result {
            Ok(version) if done => {
                for reply in work.replies {
                    let _ = reply.try_send(Ok(version));
                }
            }
            Ok(_) => self.reclassify.push_front(work),
            Err(error) => {
                // The choice is stored; the store is recounted whole at its next open.
                tracing::warn!(%error, "a sender's mail wasn't classified again");
                for reply in work.replies {
                    let _ = reply.try_send(Err(StoreError::Closed));
                }
            }
        }
    }

    /// The folders the open views list: their conversations go first after a choice.
    fn open_folders(&self) -> HashSet<FolderId> {
        let shared = Arc::clone(&self.state.shared);
        let specs: Vec<_> = shared.registry.lock().open.values().copied().collect();
        specs
            .into_iter()
            .filter_map(|spec| views::folders(&self.conn, spec.scope).ok())
            .flatten()
            .collect()
    }

    /// The server's categories of stored messages (Gmail's Primary tab, found after their
    /// headers were stored): each conversation that gained or changed one is classified again.
    pub(super) fn set_categories(
        &mut self,
        categories: &[(MessageId, Split)],
    ) -> Result<StoreVersion, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut threads = BTreeSet::new();
        {
            let mut update = tx.prepare_cached(
                "UPDATE fork_message_split SET category = ?2 \
                 WHERE id = ?1 AND category IS NOT ?2 RETURNING thread_id",
            )?;
            for (id, category) in categories {
                let thread: Option<i64> = update
                    .query_row(params![id.0, category.is_important()], |row| row.get(0))
                    .optional()?;
                threads.extend(thread.map(ThreadId));
            }
        }
        if threads.is_empty() {
            return Ok(StoreVersion(self.state.version));
        }
        classify_again(&mut self.state, tx, &threads)
    }
}

/// The conversations with mail from `address`, each with its messages from it, in the order a
/// choice classifies them again: those of `first`, then those with rows in `shown` (folders an
/// open view lists), then the others, newest thread first within each; and how many of `first`
/// are among them.
fn sender_threads(
    tx: &Connection,
    address: &str,
    first: &[ThreadId],
    shown: &HashSet<FolderId>,
) -> rusqlite::Result<(VecDeque<(ThreadId, u32)>, usize)> {
    // A scan of the split rows, which saves every ingest an index write per message.
    let counted: Vec<(ThreadId, u32)> = tx
        .prepare_cached(
            "SELECT thread_id, count(*) FROM fork_message_split WHERE sender = ?1 \
             GROUP BY thread_id ORDER BY thread_id DESC",
        )?
        .query_map([address], |row| Ok((ThreadId(row.get(0)?), row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut folders_of = tx.prepare_cached(
        "SELECT folder_id FROM fork_thread_folders WHERE thread_id = ?1 ORDER BY folder_id",
    )?;
    let mut ordered: Vec<(u8, (ThreadId, u32))> = Vec::with_capacity(counted.len());
    for (thread, count) in counted {
        let rank = if first.contains(&thread) {
            0
        } else if shown.is_empty() {
            1
        } else {
            let mut folders = folders_of.query_map([thread.0], |row| row.get(0).map(FolderId))?;
            let seen = folders.any(|folder| folder.is_ok_and(|folder| shown.contains(&folder)));
            if seen { 1 } else { 2 }
        };
        ordered.push((rank, (thread, count)));
    }
    // Stable: newest thread first within each rank.
    ordered.sort_by_key(|(rank, _)| *rank);
    let firsts = ordered.iter().filter(|(rank, _)| *rank == 0).count();
    Ok((
        ordered.into_iter().map(|(_, thread)| thread).collect(),
        firsts,
    ))
}

/// The next transaction's conversations: the first `at_least` whatever their size, then more
/// while their messages fit [`CHOICE_CHUNK`] (one at least).
fn take_chunk(threads: &mut VecDeque<(ThreadId, u32)>, at_least: usize) -> BTreeSet<ThreadId> {
    let mut chunk = BTreeSet::new();
    let mut messages = 0u32;
    while let Some(&(thread, count)) = threads.front() {
        let forced = chunk.len() < at_least;
        if !forced && !chunk.is_empty() && messages.saturating_add(count) > CHOICE_CHUNK {
            break;
        }
        threads.pop_front();
        chunk.insert(thread);
        messages = messages.saturating_add(count);
    }
    chunk
}

/// Commits `tx` with `threads` recounted and their diffs: what changed is how their messages
/// are classified.
fn classify_again(
    state: &mut State,
    tx: rusqlite::Transaction<'_>,
    threads: &BTreeSet<ThreadId>,
) -> Result<StoreVersion, StoreError> {
    fork::touch(&tx, threads)?;
    let shared = Arc::clone(&state.shared);
    let open = shared.registry.lock();
    let views = conversations::settle(&tx, &open.open, &BTreeSet::new(), &Touched::new())?;
    let version = state.commit(
        tx,
        ChangeEvent {
            views,
            ..ChangeEvent::default()
        },
    )?;
    drop(open);
    Ok(version)
}

/// Where a split row placed its message: folder, date, split.
type Place = (FolderId, i64, u8);

/// The dirty threads' split rows by message, as the table holds them now.
pub(crate) type Published = BTreeMap<MessageId, Place>;

type Key = (i64, MessageId);

pub(crate) fn published(tx: &Connection) -> rusqlite::Result<Published> {
    // The dirty threads' messages, and those the transaction deleted (by id: the split rows
    // have no index by thread). CROSS JOIN keeps the planner on the (unanalysed) TEMP tables.
    let mut select = tx.prepare_cached(
        "SELECT s.id, s.folder_id, s.date, s.split \
         FROM fork_dirty d CROSS JOIN messages m INDEXED BY messages_thread \
           ON m.thread_id = d.thread_id JOIN fork_message_split s ON s.id = m.id \
         UNION ALL SELECT s.id, s.folder_id, s.date, s.split \
         FROM fork_gone g CROSS JOIN fork_message_split s ON s.id = g.id",
    )?;
    let rows = select.query_map([], |row| {
        Ok((
            MessageId(row.get(0)?),
            (FolderId(row.get(1)?), row.get(2)?, row.get(3)?),
        ))
    })?;
    rows.collect()
}

/// The diff of a message view of `folders` keeping a side (and maybe a filter on top), or
/// `None` when the commit didn't touch it. `touched` are the messages whose rows the
/// transaction changed, with their flags before and after; under a filter, a message of a
/// dirty thread the transaction didn't touch is as it was (its flags didn't change: they would
/// have touched it).
pub(crate) fn diff(
    tx: &Connection,
    folders: &[FolderId],
    kept: Kept,
    before: &Published,
    after: &Published,
    touched: &Touched,
) -> rusqlite::Result<Option<ViewDiff>> {
    let Some(side) = kept.side.map(fork::code) else {
        return Ok(None);
    };
    // The flags of the messages the filter looks at, as they are now, read once each.
    let mut flags_now: HashMap<MessageId, Option<(u32, bool)>> = HashMap::new();
    let mut read_flags =
        tx.prepare_cached("SELECT flags, has_attachments FROM messages WHERE id = ?1")?;
    let mut flags = |id: MessageId, then: bool| -> rusqlite::Result<Option<(u32, bool)>> {
        if let Some((was, is)) = touched.get(&id) {
            return Ok(if then { *was } else { *is });
        }
        if let Some(found) = flags_now.get(&id) {
            return Ok(*found);
        }
        let found = read_flags
            .query_row([id.0], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        flags_now.insert(id, found);
        Ok(found)
    };
    let mut held =
        |place: Option<&Place>, id: MessageId, then: bool| -> rusqlite::Result<Option<Key>> {
            let Some((folder, date, split)) = place else {
                return Ok(None);
            };
            if *split != side || !folders.contains(folder) {
                return Ok(None);
            }
            if kept.filter != ViewFilter::All {
                let passes = flags(id, then)?.is_some_and(|(flags, has_attachments)| {
                    views::passes(kept.filter, flags, has_attachments, side)
                });
                if !passes {
                    return Ok(None);
                }
            }
            Ok(Some((*date, id)))
        };
    // Each dirty thread's message's key in the view before and after, when it is there.
    let ids: BTreeSet<MessageId> = before.keys().chain(after.keys()).copied().collect();
    let mut keys: Vec<(MessageId, Option<Key>, Option<Key>)> = Vec::with_capacity(ids.len());
    for id in &ids {
        let was = held(before.get(id), *id, true)?;
        let now = held(after.get(id), *id, false)?;
        keys.push((*id, was, now));
    }
    let mut leaving = Vec::new();
    let mut entering = Vec::new();
    let mut moving = Vec::new();
    let mut staying = Vec::new();
    for (id, was, now) in &keys {
        match (was, now) {
            (Some(was), None) => leaving.push(*was),
            (None, Some(now)) => entering.push(*now),
            (Some(was), Some(now)) if was != now => moving.push((*was, *now)),
            (Some(_), Some(now)) if touched.contains_key(id) => staying.push(*now),
            _ => {}
        }
    }
    // A message changed outside the dirty threads (a body that changed nothing a thread
    // counts) keeps its place.
    let mut place =
        tx.prepare_cached("SELECT folder_id, date, split FROM fork_message_split WHERE id = ?1")?;
    for id in touched.keys().filter(|id| !ids.contains(id)) {
        let found = place
            .query_row([id.0], |row| {
                Ok((FolderId(row.get(0)?), row.get(1)?, row.get(2)?))
            })
            .optional()?;
        staying.extend(held(found.as_ref(), *id, false)?);
    }
    if leaving.is_empty() && entering.is_empty() && moving.is_empty() && staying.is_empty() {
        return Ok(None);
    }
    let total = views::count(tx, folders, kept)?;
    if leaving.len() + entering.len() + 2 * moving.len() + staying.len() > MAX_SPLICE {
        return Ok(Some(ViewDiff::Reload { total }));
    }
    let after = |key: Key| views::rank(tx, folders, kept, key);
    let before = |key: Key| -> rusqlite::Result<u32> {
        let above_now = keys
            .iter()
            .filter(|(_, _, now)| now.is_some_and(|now| now > key))
            .count() as u32;
        let above_then = keys
            .iter()
            .filter(|(_, was, _)| was.is_some_and(|was| was > key))
            .count() as u32;
        Ok(after(key)? - above_now + above_then)
    };
    let mut removed = Vec::new();
    let mut inserted = Vec::new();
    for key in leaving {
        removed.push(before(key)?);
    }
    for key in entering {
        inserted.push((after(key)?, key.1));
    }
    for (was, now) in moving {
        removed.push(before(was)?);
        inserted.push((after(now)?, now.1));
    }
    let mut changed = Vec::new();
    for key in staying {
        changed.push((after(key)?, key.1));
    }
    removed.sort_unstable_by(|a, b| b.cmp(a));
    inserted.sort_unstable();
    changed.sort_unstable();
    let rows = |list: Vec<(u32, MessageId)>| -> rusqlite::Result<Vec<Positioned>> {
        list.into_iter()
            .map(|(position, id)| {
                Ok(Positioned {
                    position,
                    row: views::summary(tx, id)?,
                })
            })
            .collect()
    };
    Ok(Some(ViewDiff::Splice {
        removed,
        inserted: rows(inserted)?,
        changed: rows(changed)?,
        total,
    }))
}
