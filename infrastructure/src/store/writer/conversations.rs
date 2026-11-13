//! The conversation views' change events (docs/superhuman/store.md). A transaction's writes
//! leave the threads they touched in the TEMP dirty set (`fork.rs`); before it commits, the
//! writer reads those threads' rows in `fork_thread_folders` (still what the last commit
//! published), recounts them from `messages`, and reads them again. In each open conversation
//! view, a conversation that leaves is removed, one that enters is inserted, one whose sort key
//! changed moves (removed, then inserted) unless it keeps its place among the rows that stay,
//! and one that stays is changed when its rows there, its content or its members' flags there
//! changed. Past `MAX_SPLICE` rows a view reloads.
//!
//! Positions after the commit are ranks in the recounted index. A position before it is the
//! same rank of the old key, less the dirty conversations ranked above it now and plus those
//! ranked above it before: the others are the same rows in both.
//!
//! The Split Inbox's message views are settled here too, from the dirty threads' split rows
//! (`writer/split.rs`): a recount can move messages the transaction never touched.

use super::{MAX_SPLICE, Touched, split};
use crate::store::api::{Positioned, ViewChange, ViewDiff, ViewGrouping, ViewId, ViewSpec};
use crate::store::conversations::{self, ForkRow, Shape, Summaries};
use crate::store::fork;
use crate::store::views::{self, Kept};
use rusqlite::Connection;
use sealwax_domain::model::{FolderId, MessageId, ThreadId};
use std::collections::{BTreeMap, BTreeSet};

type Key = (i64, MessageId);

/// Recounts the transaction's dirty threads and returns the diffs of the open conversation
/// views and the Split Inbox's message views but those in `skip` (which the caller reloads,
/// at their totals after this). `touched` are the messages whose rows the transaction changed,
/// with their flags before and after. Call once per transaction, after its last write to
/// `messages`.
pub(crate) fn settle(
    tx: &Connection,
    open: &BTreeMap<ViewId, ViewSpec>,
    skip: &BTreeSet<ViewId>,
    touched: &Touched,
) -> rusqlite::Result<Vec<ViewChange>> {
    let dirty = fork::dirty(tx)?;
    if dirty.is_empty() {
        fork::clear(tx)?;
        return Ok(Vec::new());
    }
    let views = open
        .iter()
        .filter(|(view, spec)| spec.grouping == ViewGrouping::Conversations && !skip.contains(view))
        .map(|(view, spec)| Ok((*view, Shape::of(tx, *spec)?)))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let splits = open
        .iter()
        .filter(|(view, spec)| {
            spec.grouping == ViewGrouping::Messages && spec.side().is_some() && !skip.contains(view)
        })
        .map(|(view, spec)| Ok((*view, views::folders(tx, spec.scope)?, Kept::of(*spec))))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if views.is_empty() && splits.is_empty() {
        fork::recount_dirty(tx)?;
        fork::clear(tx)?;
        return Ok(Vec::new());
    }
    let before = rows(tx, &dirty, !views.is_empty())?;
    let published = if splits.is_empty() {
        split::Published::new()
    } else {
        split::published(tx)?
    };
    fork::recount_dirty(tx)?;
    let mut changes = Vec::new();
    if !views.is_empty() {
        let flagged = fork::flagged(tx)?;
        let after = rows(tx, &dirty, true)?;
        let dirty = Dirty {
            content: dirty.iter().copied().collect(),
            flagged,
            before,
            after,
        };
        let mut summaries = Summaries::new(tx);
        for (view, shape) in &views {
            if let Some(diff) = splice(tx, shape, &dirty, &mut summaries)? {
                changes.push(ViewChange { view: *view, diff });
            }
        }
    }
    if !splits.is_empty() {
        let now = split::published(tx)?;
        for (view, folders, kept) in &splits {
            if let Some(diff) = split::diff(tx, folders, *kept, &published, &now, touched)? {
                changes.push(ViewChange { view: *view, diff });
            }
        }
    }
    fork::clear(tx)?;
    Ok(changes)
}

/// The dirty threads' rows, as the table holds them now (none unless `wanted`).
fn rows(
    tx: &Connection,
    dirty: &[(ThreadId, bool)],
    wanted: bool,
) -> rusqlite::Result<BTreeMap<ThreadId, Vec<ForkRow>>> {
    if !wanted {
        return Ok(BTreeMap::new());
    }
    dirty
        .iter()
        .map(|(thread, _)| Ok((*thread, conversations::rows_of(tx, *thread)?)))
        .collect()
}

/// What the transaction did to its threads.
struct Dirty {
    /// Whether anything but shown flags changed, by thread.
    content: BTreeMap<ThreadId, bool>,
    /// Threads and folders where a member's shown flags changed.
    flagged: BTreeSet<(ThreadId, FolderId)>,
    before: BTreeMap<ThreadId, Vec<ForkRow>>,
    after: BTreeMap<ThreadId, Vec<ForkRow>>,
}

/// The sort key of a conversation with these rows in a view, when it is in the view.
fn held(shape: &Shape, rows: &[ForkRow]) -> Option<Key> {
    let rows = shape.rows_in(rows);
    if shape.holds(&rows) {
        conversations::key(&rows).map(|(key, _)| key)
    } else {
        None
    }
}

/// One view's diff, or `None` when the commit didn't touch it.
fn splice(
    tx: &Connection,
    shape: &Shape,
    dirty: &Dirty,
    summaries: &mut Summaries<'_>,
) -> rusqlite::Result<Option<ViewDiff>> {
    // Each dirty conversation's key in the view before and after, when it is there.
    let mut keys: Vec<(ThreadId, Option<Key>, Option<Key>)> = Vec::new();
    for (thread, rows) in &dirty.after {
        let was = dirty.before.get(thread).and_then(|rows| held(shape, rows));
        keys.push((*thread, was, held(shape, rows)));
    }
    let mut leaving = Vec::new();
    let mut entering = Vec::new();
    let mut moving = Vec::new();
    let mut staying = Vec::new();
    for (thread, was, now) in &keys {
        match (was, now) {
            (Some(was), None) => leaving.push(*was),
            (None, Some(now)) => entering.push((*thread, *now)),
            (Some(was), Some(now)) if was != now => moving.push((*thread, *was, *now)),
            (Some(_), Some(now)) => {
                // Which members are Important decides only the split views it is in.
                let rows = |map: &BTreeMap<ThreadId, Vec<ForkRow>>| {
                    map.get(thread).map(|rows| {
                        shape
                            .rows_in(rows)
                            .iter()
                            .map(ForkRow::shown)
                            .collect::<Vec<_>>()
                    })
                };
                if dirty.content.get(thread).copied().unwrap_or(true)
                    || rows(&dirty.before) != rows(&dirty.after)
                    || shape
                        .folders
                        .iter()
                        .any(|folder| dirty.flagged.contains(&(*thread, *folder)))
                {
                    staying.push((*thread, *now));
                }
            }
            (None, None) => {}
        }
    }
    if leaving.is_empty() && entering.is_empty() && moving.is_empty() && staying.is_empty() {
        return Ok(None);
    }
    let total = conversations::count(tx, shape)?;
    if leaving.len() + entering.len() + 2 * moving.len() + staying.len() > MAX_SPLICE {
        return Ok(Some(ViewDiff::Reload { total }));
    }
    let after = |key: Key| conversations::rank(tx, shape, key);
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
    for key in leaving {
        removed.push(before(key)?);
    }
    let mut inserted = Vec::new();
    for (thread, key) in entering {
        inserted.push((after(key)?, thread));
    }
    let mut moved = Vec::new();
    for (thread, was, now) in moving {
        moved.push((thread, before(was)?, after(now)?));
    }
    let mut changed = Vec::new();
    // A lone move that keeps its place among the rows that stay (as many of them ahead of it
    // as before) needs no removal: say, a reply to the conversation at the top.
    if let [(thread, from, to)] = moved.as_slice() {
        let ahead_before = from - removed.iter().filter(|at| *at < from).count() as u32;
        let ahead_after = to - inserted.iter().filter(|(at, _)| at < to).count() as u32;
        if ahead_before == ahead_after {
            changed.push((*to, *thread));
            moved.clear();
        }
    }
    for (thread, from, to) in moved {
        removed.push(from);
        inserted.push((to, thread));
    }
    for (thread, key) in staying {
        changed.push((after(key)?, thread));
    }
    removed.sort_unstable_by(|a, b| b.cmp(a));
    inserted.sort_unstable();
    changed.sort_unstable();
    let mut rows = |list: Vec<(u32, ThreadId)>| -> rusqlite::Result<Vec<Positioned<_>>> {
        let threads: Vec<(ThreadId, Vec<ForkRow>)> = list
            .iter()
            .map(|(_, thread)| {
                let rows = dirty.after.get(thread).map(|rows| shape.rows_in(rows));
                (*thread, rows.unwrap_or_default())
            })
            .collect();
        Ok(list
            .iter()
            .zip(summaries.many(&threads, &shape.folders)?)
            .map(|((position, _), row)| Positioned {
                position: *position,
                row,
            })
            .collect())
    };
    Ok(Some(ViewDiff::ConversationSplice {
        removed,
        inserted: rows(inserted)?,
        changed: rows(changed)?,
        total,
    }))
}
