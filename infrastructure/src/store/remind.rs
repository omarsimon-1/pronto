//! Remind Me's tables (the fork's Superhuman pass, docs/superhuman/remind.md; `fork.rs` step 3):
//! which conversation a reminder holds now, and what the op queue's own writes do to reminders.
//! The writer's Remind Me operations are in `writer/remind.rs`.
//!
//! A reminder finds its conversation by its messages' Message-IDs (`fork_reminder_keys`): the
//! threads that hold a message of the account with one of them, hidden or not. Thread ids don't
//! last: a merge folds a conversation into another, and a move the store can't place (no
//! COPYUID, or a folder never synced) deletes the rows, and with them an emptied thread, until
//! the target's sync stores them again under a new one.

use super::api::FolderSpec;
use super::fork;
use rusqlite::{Connection, params};
use sealwax_domain::model::{AccountId, FolderId, FolderRole, ThreadId};
use sealwax_domain::remind::{self, ReminderState};
use sealwax_domain::triage::Plan;
use std::collections::BTreeSet;

/// The threads a reminder's conversation is in now: those of the account's stored messages with
/// one of its Message-IDs, or its thread when its messages have none (none once that is gone).
pub(crate) fn threads_of(conn: &Connection, reminder: i64) -> rusqlite::Result<BTreeSet<ThreadId>> {
    conn.prepare_cached(
        "SELECT m.thread_id FROM fork_reminders r \
           JOIN fork_reminder_keys k ON k.reminder = r.id \
           JOIN messages m ON m.message_id = k.message_id \
           JOIN folders x ON x.id = m.folder_id AND x.account_id = r.account_id \
         WHERE r.id = ?1 \
         UNION SELECT r.thread_id FROM fork_reminders r WHERE r.id = ?1 \
           AND r.thread_id IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM fork_reminder_keys k WHERE k.reminder = r.id)",
    )?
    .query_map([reminder], |row| row.get(0).map(ThreadId))?
    .collect()
}

/// A reminder that still counts: waiting, or reminded.
#[derive(Clone, Debug)]
pub(crate) struct Live {
    pub(crate) id: i64,
    pub(crate) account: AccountId,
    pub(crate) state: ReminderState,
    /// The Reminders folder its messages wait in; `None` once that folder is gone (the server
    /// refused to make it, or it was deleted): nothing of it waits anywhere then.
    pub(crate) folder: Option<FolderId>,
    /// The folder they left.
    pub(crate) origin: FolderId,
    pub(crate) wake_at: i64,
    pub(crate) reminded_at: Option<i64>,
    /// Its conversation's threads now.
    pub(crate) threads: BTreeSet<ThreadId>,
}

/// Whether any reminder still counts: the triage path asks before it looks for them.
pub(crate) fn any_live(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM fork_reminders WHERE state < 2)")?
        .query_row([], |row| row.get(0))
}

const LIVE: &str = "SELECT id, account_id, state, folder_id, origin, wake_at, reminded_at \
    FROM fork_reminders";

fn read_live(row: &rusqlite::Row<'_>) -> rusqlite::Result<Live> {
    Ok(Live {
        id: row.get(0)?,
        account: AccountId(row.get(1)?),
        state: ReminderState::from_code(row.get(2)?),
        folder: row.get::<_, Option<i64>>(3)?.map(FolderId),
        origin: FolderId(row.get(4)?),
        wake_at: row.get(5)?,
        reminded_at: row.get(6)?,
        threads: BTreeSet::new(),
    })
}

/// The reminders of `sql` (`LIVE` with a condition), each with its conversation's threads.
fn resolved(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<Live>> {
    let mut found: Vec<Live> = conn
        .prepare_cached(sql)?
        .query_map(params, read_live)?
        .collect::<rusqlite::Result<_>>()?;
    for reminder in &mut found {
        reminder.threads = threads_of(conn, reminder.id)?;
    }
    Ok(found)
}

/// Every reminder that still counts, the soonest due first, with its conversation's threads.
pub(crate) fn live(conn: &Connection) -> rusqlite::Result<Vec<Live>> {
    resolved(
        conn,
        &format!("{LIVE} WHERE state < 2 ORDER BY wake_at, id"),
        [],
    )
}

/// The waiting reminders whose conversation someone else answered while it waited
/// (Superhuman's "if no reply", phase C): a shown message of the conversation in an Inbox of
/// its account whose Message-ID isn't one of the reminder's keys (so it came after the reminder
/// was set) and that the user didn't write (its sender isn't the account's address, and no
/// copy of it is in a Sent folder). The soonest due first. The joins are pinned (`CROSS JOIN`):
/// from the waiting reminders through their keys to their conversations' messages, so the cost
/// follows those, never the store; left to itself the planner, without statistics for the
/// fork's tables, walked every message of the account's folders (55-100 ms a wake at 100,000).
/// A reminder without keys is found by none: its conversation's messages can't be told apart.
pub(crate) fn replied(conn: &Connection) -> rusqlite::Result<Vec<Live>> {
    resolved(
        conn,
        &format!(
            "{LIVE} WHERE state = 0 AND id IN (\
             WITH threads AS MATERIALIZED (\
               SELECT DISTINCT r.id AS reminder, r.account_id AS account, known.thread_id AS t \
               FROM fork_reminders r INDEXED BY fork_reminders_waiting \
                 CROSS JOIN fork_reminder_keys k ON k.reminder = r.id \
                 CROSS JOIN messages known INDEXED BY messages_message_id \
                   ON known.message_id = k.message_id \
                 CROSS JOIN folders kf ON kf.id = known.folder_id \
               WHERE r.state = 0 AND kf.account_id = r.account_id) \
             SELECT w.reminder FROM threads w \
               CROSS JOIN messages m INDEXED BY messages_thread ON m.thread_id = w.t \
               CROSS JOIN folders f ON f.id = m.folder_id \
               CROSS JOIN accounts a ON a.id = w.account \
             WHERE f.account_id = w.account AND f.role = 'inbox' AND m.flags & 264 = 0 \
               AND m.message_id IS NOT NULL \
               AND lower(m.from_addr) <> lower(a.address) \
               AND NOT EXISTS (SELECT 1 FROM fork_reminder_keys o \
                 WHERE o.reminder = w.reminder AND o.message_id = m.message_id) \
               AND NOT EXISTS (SELECT 1 FROM messages s INDEXED BY messages_message_id \
                 JOIN folders sf ON sf.id = s.folder_id \
                 WHERE s.message_id = m.message_id AND sf.account_id = w.account \
                   AND sf.role = 'sent')) \
             ORDER BY wake_at, id"
        ),
        [],
    )
}

/// Whether the reminder's conversation has a shown message in an Inbox of its account.
pub(crate) fn in_an_inbox(conn: &Connection, reminder: &Live) -> rusqlite::Result<bool> {
    let mut select = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM messages m INDEXED BY messages_thread \
           JOIN folders f ON f.id = m.folder_id \
         WHERE m.thread_id = ?1 AND f.account_id = ?2 AND f.role = 'inbox' \
           AND m.flags & 264 = 0)",
    )?;
    for thread in &reminder.threads {
        if select.query_row(params![thread.0, reminder.account.0], |row| row.get(0))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The account's Reminders folder (`remind::is_folder`), if it has one: where a reminder whose
/// own folder went looks for its conversation, should the server have listed the folder again
/// (phase C, the final review of phase B: hidden from IMAP and shown again, renamed and back).
pub(crate) fn reminders_folder(
    conn: &Connection,
    account: AccountId,
) -> rusqlite::Result<Option<FolderId>> {
    let folders: Vec<(FolderId, String, Option<char>, bool)> = conn
        .prepare_cached(
            "SELECT id, name_raw, delimiter, selectable FROM folders WHERE account_id = ?1 \
             ORDER BY id",
        )?
        .query_map([account.0], |row| {
            Ok((
                FolderId(row.get(0)?),
                row.get(1)?,
                row.get::<_, Option<String>>(2)?
                    .and_then(|text| text.chars().next()),
                row.get(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let listed: Vec<(&str, Option<char>)> = folders
        .iter()
        .map(|(_, name, delimiter, _)| (name.as_str(), *delimiter))
        .collect();
    Ok(folders
        .iter()
        .find(|(_, name, _, selectable)| *selectable && remind::is_folder(name, &listed))
        .map(|(id, ..)| *id))
}

/// The Reminders folder the reminder's conversation waits in: its own, else, once that went,
/// the account's by name ([`reminders_folder`]).
pub(crate) fn waits_in(conn: &Connection, reminder: &Live) -> rusqlite::Result<Option<FolderId>> {
    match reminder.folder {
        Some(folder) => Ok(Some(folder)),
        None => reminders_folder(conn, reminder.account),
    }
}

/// The waiting reminders due at `now` (Unix seconds), the soonest first.
pub(crate) fn due(conn: &Connection, now: i64) -> rusqlite::Result<Vec<Live>> {
    resolved(
        conn,
        &format!(
            "{LIVE} INDEXED BY fork_reminders_waiting WHERE state = 0 AND wake_at <= ?1 \
             ORDER BY wake_at, id"
        ),
        [now],
    )
}

/// The reminders that still count whose conversation is in one of `threads`: found through
/// those threads' messages (their Message-IDs among the keys) and, for a reminder without
/// keys, its thread. Its cost follows the threads' messages, not the reminders kept.
pub(crate) fn of_threads(
    conn: &Connection,
    threads: &BTreeSet<ThreadId>,
) -> rusqlite::Result<Vec<Live>> {
    let mut ids = BTreeSet::new();
    {
        let mut select = conn.prepare_cached(
            "SELECT r.id FROM messages m INDEXED BY messages_thread \
               JOIN fork_reminder_keys k ON k.message_id = m.message_id \
               JOIN fork_reminders r ON r.id = k.reminder \
               JOIN folders x ON x.id = m.folder_id AND x.account_id = r.account_id \
             WHERE m.thread_id = ?1 AND r.state < 2 \
             UNION SELECT r.id FROM fork_reminders r INDEXED BY fork_reminders_thread \
             WHERE r.thread_id = ?1 AND r.state < 2 \
               AND NOT EXISTS (SELECT 1 FROM fork_reminder_keys k WHERE k.reminder = r.id)",
        )?;
        for thread in threads {
            let found = select
                .query_map([thread.0], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.extend(found);
        }
    }
    let mut out = Vec::new();
    for id in ids {
        out.extend(resolved(conn, &format!("{LIVE} WHERE id = ?1"), [id])?);
    }
    Ok(out)
}

/// Ends a reminder (cancelled, or acted on): ended by `batch`, whose Undo restores the state
/// it ended from. Its conversation is marked dirty: a reminded one's Inbox rows sort by their
/// mail again. One that has ended stays as it ended (one action may reach a reminder through
/// two of its conversation's threads).
pub(crate) fn end(tx: &Connection, reminder: &Live, batch: Option<i64>) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE fork_reminders SET state = 2, ended = ?2, ended_from = state \
         WHERE id = ?1 AND state < 2",
    )?
    .execute(params![reminder.id, batch])?;
    fork::touch(tx, &reminder.threads)
}

/// Triage queued as `batch`, with the conversations whose messages it takes out of a folder
/// (thread, folder, its role, the plan): the reminders it ends (`remind::ends`) end.
pub(crate) fn triaged(
    tx: &Connection,
    batch: i64,
    leaving: &[(ThreadId, FolderId, FolderRole, Plan)],
) -> rusqlite::Result<()> {
    if leaving.is_empty() || !any_live(tx)? {
        return Ok(());
    }
    let threads: BTreeSet<ThreadId> = leaving.iter().map(|(thread, ..)| *thread).collect();
    for reminder in of_threads(tx, &threads)? {
        // Its own folder, or the account's by name once that went (relisted by the server).
        let waits_in = match reminder.state {
            ReminderState::Waiting => waits_in(tx, &reminder)?,
            _ => reminder.folder,
        };
        let ends = leaving.iter().any(|(thread, folder, role, plan)| {
            reminder.threads.contains(thread)
                && remind::ends(reminder.state, *plan, *role, Some(*folder) == waits_in)
        });
        if ends {
            end(tx, &reminder, Some(batch))?;
        }
    }
    Ok(())
}

/// Undo of `batch`: the reminders it set go, and those it ended count again as they were.
/// Their conversations are marked dirty.
pub(crate) fn undone(tx: &Connection, batch: i64) -> rusqlite::Result<()> {
    let ids: Vec<i64> = tx
        .prepare_cached("SELECT id FROM fork_reminders WHERE batch = ?1 OR ended = ?1")?
        .query_map([batch], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if ids.is_empty() {
        return Ok(());
    }
    let mut threads = BTreeSet::new();
    for id in ids {
        threads.extend(threads_of(tx, id)?);
    }
    tx.prepare_cached("DELETE FROM fork_reminders WHERE batch = ?1")?
        .execute([batch])?;
    tx.prepare_cached(
        "UPDATE fork_reminders SET state = ended_from, ended = NULL, ended_from = NULL \
         WHERE ended = ?1",
    )?
    .execute([batch])?;
    fork::touch(tx, &threads)
}

/// Drops ended reminders that Undo can no longer restore (their batch is gone, which lets go
/// of it, `fork.rs`, or they had none) and those that can't find their conversation again (no
/// Message-ID, and their thread is gone).
pub(crate) fn prune(tx: &Connection) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "DELETE FROM fork_reminders WHERE (state = 2 AND ended IS NULL) \
           OR (thread_id IS NULL \
             AND NOT EXISTS (SELECT 1 FROM fork_reminder_keys k WHERE k.reminder = fork_reminders.id))",
    )?
    .execute([])?;
    Ok(())
}

/// Drops the Reminders folders made for a server account that the server doesn't have yet,
/// when no move into one waits any more and it holds nothing (its reminders were undone before
/// a move ran); replies with the folders dropped.
pub(crate) fn drop_unused_folders(tx: &Connection) -> rusqlite::Result<Vec<FolderId>> {
    tx.prepare_cached(
        "DELETE FROM folders WHERE id IN (SELECT folder_id FROM fork_folder_creates) \
           AND NOT EXISTS (SELECT 1 FROM ops WHERE target = folders.id AND done = 0) \
           AND NOT EXISTS (SELECT 1 FROM messages WHERE folder_id = folders.id) \
         RETURNING id",
    )?
    .query_map([], |row| row.get(0).map(FolderId))?
    .collect()
}

/// `listed`, an account's folders as its server lists them, as the folder list should take
/// them: a Reminders folder the store made that the server doesn't list yet stays (as if
/// listed) while a move into it waits, and one the server lists is the server's from now on.
/// `None` when the store made none.
pub(crate) fn keep_unlisted(
    tx: &Connection,
    account: AccountId,
    listed: &[FolderSpec],
) -> rusqlite::Result<Option<Vec<FolderSpec>>> {
    let made: Vec<(i64, String, String, Option<String>, bool)> = tx
        .prepare_cached(
            "SELECT f.id, f.name, f.name_raw, f.delimiter, \
               EXISTS (SELECT 1 FROM ops WHERE target = f.id AND done = 0) \
             FROM fork_folder_creates c JOIN folders f ON f.id = c.folder_id \
             WHERE f.account_id = ?1",
        )?
        .query_map([account.0], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if made.is_empty() {
        return Ok(None);
    }
    let mut kept = listed.to_vec();
    for (id, name, name_raw, delimiter, waited_for) in made {
        if listed.iter().any(|folder| folder.name_raw == name_raw) {
            tx.prepare_cached("DELETE FROM fork_folder_creates WHERE folder_id = ?1")?
                .execute([id])?;
        } else if waited_for {
            kept.push(FolderSpec {
                account,
                name,
                name_raw,
                delimiter: delimiter.and_then(|text| text.chars().next()),
                role: FolderRole::Other,
                attributes: Vec::new(),
                selectable: true,
                server_counts: None,
            });
        }
    }
    Ok(Some(kept))
}
