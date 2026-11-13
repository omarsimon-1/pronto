//! Remind Me's writes (the fork's Superhuman pass, docs/superhuman/remind.md): a remind, a
//! cancel and a wake are each one transaction that moves messages with the op queue's own
//! machinery (`ops::queue_batch`: the overlay, Undo, the server after the undo window, a local
//! account's folders at once) and keeps the reminders beside them, so a crash leaves both or
//! neither, and a retry finds what the server already did as any move's does. The Inbox rows'
//! sort key follows from the recount (`fork.rs`), which every change of a reminder's state
//! triggers by marking its conversation dirty: the change event carries the views' diffs.

use super::ops::{self, is_local, new_batch, queue_batch};
use super::overlay::{Row, chain, defer_blob_deletions, row};
use super::{Writer, folder_counts, sync, thread_counts};
use crate::store::api::{ChangeEvent, StoreVersion};
use crate::store::ops_api::{BatchId, Queued};
use crate::store::overlay::{self, BATCH_TRIAGE, Change};
use crate::store::remind::{self, Live};
use crate::store::remind_api::{Remind, Reminded, ReminderId, Woken};
use crate::store::{StoreError, fork, views};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sealwax_domain::model::{AccountId, FolderId, MessageId, ThreadId};
use sealwax_domain::remind::{self as rules, ReminderState, SOMEDAY};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

/// `kind` in `op_batches` of a wake's moves back to the Inbox, which no user's Undo takes back.
/// The fork's kinds start at 64, clear of upstream's.
pub(crate) const BATCH_WAKE: i64 = 64;

/// How long a due reminder waits for its messages to be stored again (seconds), and how soon
/// the timer looks again meanwhile.
const TRANSIT: i64 = 86_400;
const RETRY: i64 = 60;

pub(crate) enum RemindJob {
    Remind {
        request: Remind,
        reply: Sender<Reminded>,
    },
    Cancel {
        threads: Vec<ThreadId>,
        not_before: i64,
        reply: Sender<Queued>,
    },
    Wake {
        now: i64,
        reply: Sender<Woken>,
    },
    FolderCreated {
        folder: FolderId,
        reply: Sender<()>,
    },
    FolderRefused {
        folder: FolderId,
        reply: Sender<StoreVersion>,
    },
}

impl Writer {
    pub(crate) fn remind_job(&mut self, job: RemindJob) {
        match job {
            RemindJob::Remind { request, reply } => {
                let _ = reply.try_send(self.remind(&request));
            }
            RemindJob::Cancel {
                threads,
                not_before,
                reply,
            } => {
                let _ = reply.try_send(self.cancel_reminders(&threads, not_before));
            }
            RemindJob::Wake { now, reply } => {
                let _ = reply.try_send(self.wake_reminders(now));
            }
            RemindJob::FolderCreated { folder, reply } => {
                let _ = reply.try_send(self.folder_created(folder));
            }
            RemindJob::FolderRefused { folder, reply } => {
                let _ = reply.try_send(self.folder_refused(folder));
            }
        }
    }

    fn remind(&mut self, request: &Remind) -> Result<Reminded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = views::folders(&tx, request.scope)?;
        let asked: BTreeSet<ThreadId> = request.threads.iter().copied().collect();
        let live = remind::of_threads(&tx, &asked)?;
        let mut changes = Vec::new();
        let mut folder_of: HashMap<AccountId, FolderId> = HashMap::new();
        let mut made = Vec::new();
        let mut set = Vec::new();
        let mut reminders = Vec::new();
        let mut ending = Vec::new();
        let mut not_applied = 0u32;
        let mut seen = BTreeSet::new();
        for thread in &request.threads {
            if !seen.insert(*thread) {
                continue;
            }
            let Some(account) = thread_account(&tx, *thread)? else {
                not_applied += 1;
                continue;
            };
            let waiting = live
                .iter()
                .find(|r| r.state == ReminderState::Waiting && r.threads.contains(thread));
            // Where a waiting conversation waits, unless that folder went (the server refused
            // to make it, or it was deleted: its reminders let go of it, `fork.rs`); else the
            // account's Reminders folder, if it has one.
            let known = match (
                waiting.and_then(|waiting| waiting.folder),
                folder_of.get(&account),
            ) {
                (Some(folder), _) | (None, Some(&folder)) => Some(folder),
                (None, None) => remind::reminders_folder(&tx, account)?,
            };
            let members = members(&tx, *thread, &scope, known)?;
            if waiting.is_none() && members.is_empty() {
                not_applied += 1;
                continue;
            }
            let folder = match known {
                Some(folder) => Some(folder),
                None if members.is_empty() => None,
                None => {
                    let folder = make_folder(&tx, account)?;
                    made.push((folder, account));
                    Some(folder)
                }
            };
            if let Some(folder) = folder {
                folder_of.insert(account, folder);
            }
            if let Some(waiting) = waiting {
                // A new time; and what came to the scope meanwhile (a reply) goes too.
                tx.prepare_cached(
                    "UPDATE fork_reminders SET wake_at = ?2, folder_id = coalesce(?3, folder_id) \
                     WHERE id = ?1",
                )?
                .execute(params![
                    waiting.id,
                    request.wake_at,
                    folder.map(|f| f.0)
                ])?;
                add_keys(&tx, waiting.id, *thread)?;
                // Its row in Reminders sorts by the new time (phase C).
                fork::touch(&tx, &waiting.threads)?;
                reminders.push(ReminderId(waiting.id));
                if let Some(folder) = folder {
                    changes.extend(members.into_iter().map(|found| (found, to(folder))));
                }
                continue;
            }
            let Some(folder) = folder else {
                not_applied += 1;
                continue;
            };
            // Back from Reminders and sent again: its place at the top ends with this batch.
            ending.extend(
                live.iter()
                    .filter(|r| r.state == ReminderState::Reminded && r.threads.contains(thread)),
            );
            let id: i64 = tx
                .prepare_cached(
                    "INSERT INTO fork_reminders(account_id, thread_id, folder_id, origin, \
                       wake_at, state, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch()) \
                     RETURNING id",
                )?
                .query_row(
                    params![
                        account.0,
                        thread.0,
                        folder.0,
                        members[0].folder.0,
                        request.wake_at,
                        ReminderState::Waiting.code()
                    ],
                    |row| row.get(0),
                )?;
            add_keys(&tx, id, *thread)?;
            set.push(id);
            reminders.push(ReminderId(id));
            changes.extend(members.into_iter().map(|found| (found, to(folder))));
        }
        let moved = u32::try_from(changes.len()).unwrap_or(u32::MAX);
        let (batch, plans, accounts) = queue_batch(&tx, BATCH_TRIAGE, changes, request.not_before)?;
        {
            let mut stamp =
                tx.prepare_cached("UPDATE fork_reminders SET batch = ?2 WHERE id = ?1")?;
            for id in &set {
                stamp.execute(params![id, batch])?;
            }
        }
        for reminder in ending {
            remind::end(&tx, reminder, batch)?;
        }
        ops::prune(&tx)?;
        defer_blob_deletions(&tx, &plans)?;
        self.state.folders_made |= !made.is_empty();
        let version = self.state.commit_rows(tx, &plans, BTreeSet::new())?.version;
        for (folder, account) in made {
            self.state.accounts.insert(folder, account);
        }
        Ok(Reminded {
            version,
            batch: batch.map(BatchId),
            reminders,
            moved,
            not_applied,
            accounts: accounts.into_iter().collect(),
        })
    }

    fn cancel_reminders(
        &mut self,
        threads: &[ThreadId],
        not_before: i64,
    ) -> Result<Queued, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let wanted: BTreeSet<ThreadId> = threads.iter().copied().collect();
        let cancelled: Vec<Live> = remind::of_threads(&tx, &wanted)?;
        if cancelled.is_empty() {
            tx.commit()?;
            return Ok(Queued {
                version: StoreVersion(self.state.version),
                ..Queued::default()
            });
        }
        let mut changes = Vec::new();
        for reminder in cancelled
            .iter()
            .filter(|reminder| reminder.state == ReminderState::Waiting)
        {
            changes.extend(back(&tx, reminder)?);
        }
        let applied = u32::try_from(changes.len()).unwrap_or(u32::MAX);
        let (batch, plans, accounts) = queue_batch(&tx, BATCH_TRIAGE, changes, not_before)?;
        // Removing reminders that came back already moves nothing; the batch is Undo's all
        // the same (phase C: Remove Reminder is undoable), and `ops::prune` keeps it.
        let batch = match batch {
            Some(batch) => Some(batch),
            None => Some(new_batch(&tx, BATCH_TRIAGE)?),
        };
        for reminder in &cancelled {
            remind::end(&tx, reminder, batch)?;
        }
        ops::prune(&tx)?;
        defer_blob_deletions(&tx, &plans)?;
        let version = self.state.commit_rows(tx, &plans, BTreeSet::new())?.version;
        Ok(Queued {
            version,
            batch: batch.map(BatchId),
            applied,
            not_applied: Vec::new(),
            accounts: accounts.into_iter().collect(),
        })
    }

    fn wake_reminders(&mut self, now: i64) -> Result<Woken, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Superhuman's "if no reply" (phase C): someone answered a waiting conversation, so
        // the answer is in the Inbox and the reminder ends; what waited comes back with it.
        let replied = remind::replied(&tx)?;
        let due: Vec<Live> = remind::due(&tx, now)?
            .into_iter()
            .filter(|reminder| !replied.iter().any(|known| known.id == reminder.id))
            .collect();
        // Someday never comes due: the timer doesn't wait for it.
        let next = |tx: &Connection| -> rusqlite::Result<Option<i64>> {
            tx.prepare_cached(
                "SELECT min(wake_at) FROM fork_reminders INDEXED BY fork_reminders_waiting \
                 WHERE state = 0 AND wake_at > ?1 AND wake_at < ?2",
            )?
            .query_row(params![now, SOMEDAY], |row| row.get(0))
        };
        if due.is_empty() && replied.is_empty() {
            let next = next(&tx)?;
            tx.commit()?;
            return Ok(Woken {
                version: StoreVersion(self.state.version),
                next,
                ..Woken::default()
            });
        }
        let mut changes = Vec::new();
        let mut threads = BTreeSet::new();
        let mut woken = Vec::new();
        let mut ended = Vec::new();
        let mut deferred = false;
        for reminder in &replied {
            changes.extend(back(&tx, reminder)?);
            // Nothing to undo: no one asked for it, and the same transaction prunes it.
            remind::end(&tx, reminder, None)?;
            ended.push(ReminderId(reminder.id));
        }
        for reminder in &due {
            let moves = back(&tx, reminder)?;
            // Nothing in Reminders while a message of it isn't stored: on its way there (a move
            // the store couldn't place, until that folder's sync stores it). It waits for it,
            // a day at most. Nothing is on its way to a folder that is gone.
            if moves.is_empty()
                && reminder.wake_at > now - TRANSIT
                && remind::waits_in(&tx, reminder)?.is_some()
                && in_transit(&tx, reminder)?
            {
                deferred = true;
                continue;
            }
            // Nothing to bring back and nothing of it in an Inbox: its Reminders folder went
            // with its messages, or it left the Inbox while its folder was refused. It can
            // never come back, so it ends rather than linger (the final review of phase B).
            if moves.is_empty() && !remind::in_an_inbox(&tx, reminder)? {
                remind::end(&tx, reminder, None)?;
                ended.push(ReminderId(reminder.id));
                continue;
            }
            changes.extend(moves);
            tx.prepare_cached(
                "UPDATE fork_reminders SET state = ?2, reminded_at = ?3 WHERE id = ?1",
            )?
            .execute(params![reminder.id, ReminderState::Reminded.code(), now])?;
            threads.extend(reminder.threads.iter().copied());
            woken.push(ReminderId(reminder.id));
        }
        let (_, plans, accounts) = queue_batch(&tx, BATCH_WAKE, changes, 0)?;
        // Their Inbox rows sort by `now` from this commit (a server account's once its moves
        // are done and the rows are back in the Inbox).
        fork::touch(&tx, &threads)?;
        ops::prune(&tx)?;
        defer_blob_deletions(&tx, &plans)?;
        let next = match (next(&tx)?, deferred) {
            (next, false) => next,
            (next, true) => Some(next.map_or(now + RETRY, |next| next.min(now + RETRY))),
        };
        let version = self.state.commit_rows(tx, &plans, BTreeSet::new())?.version;
        Ok(Woken {
            version,
            woken,
            ended,
            accounts: accounts.into_iter().collect(),
            next,
        })
    }

    fn folder_created(&mut self, folder: FolderId) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM fork_folder_creates WHERE folder_id = ?1",
            [folder.0],
        )?;
        tx.commit()?;
        self.state.after_commit();
        Ok(())
    }

    fn folder_refused(&mut self, folder: FolderId) -> Result<StoreVersion, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let made: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM fork_folder_creates WHERE folder_id = ?1)",
            [folder.0],
            |row| row.get(0),
        )?;
        if !made {
            tx.commit()?;
            return Ok(StoreVersion(self.state.version));
        }
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let mut counted = BTreeSet::new();
        let dropped = sync::drop_folders(&tx, &open.open, &[folder], &mut counted)?;
        let event = ChangeEvent {
            removed: dropped.removed,
            changed: dropped.changed,
            folders: folder_counts(&tx, &counted)?,
            folders_changed: true,
            threads: thread_counts(&tx, &dropped.threads)?,
            views: dropped.views,
            ..ChangeEvent::default()
        };
        let version = self.state.commit(tx, event)?;
        drop(open);
        self.state.accounts.remove(&folder);
        Ok(version)
    }
}

/// A move into `folder`, flags as they are.
fn to(folder: FolderId) -> Change {
    Change::Move {
        to: folder,
        add: 0,
        remove: 0,
    }
}

fn thread_account(tx: &Connection, thread: ThreadId) -> rusqlite::Result<Option<AccountId>> {
    tx.prepare_cached("SELECT account_id FROM threads WHERE id = ?1")?
        .query_row([thread.0], |row| row.get(0).map(AccountId))
        .optional()
}

/// One of an account's folders, as a new Reminders folder is named among them.
struct Listed {
    name_raw: String,
    delimiter: Option<char>,
}

/// The account's folders' names and delimiters as `remind` reads them.
fn account_folders(tx: &Connection, account: AccountId) -> rusqlite::Result<Vec<Listed>> {
    tx.prepare_cached("SELECT name_raw, delimiter FROM folders WHERE account_id = ?1 ORDER BY id")?
        .query_map([account.0], |row| {
            Ok(Listed {
                name_raw: row.get(0)?,
                delimiter: row
                    .get::<_, Option<String>>(1)?
                    .and_then(|text| text.chars().next()),
            })
        })?
        .collect()
}

fn names(folders: &[Listed]) -> Vec<(&str, Option<char>)> {
    folders
        .iter()
        .map(|folder| (folder.name_raw.as_str(), folder.delimiter))
        .collect()
}

/// Makes the account's Reminders folder at its top, as folder management names a new folder
/// (`remind::new_folder`). A server account's is marked for the engine to create on the
/// server before the first move into it runs; until then the folder list keeps it
/// (`remind::keep_unlisted`).
fn make_folder(tx: &Connection, account: AccountId) -> rusqlite::Result<FolderId> {
    let folders = account_folders(tx, account)?;
    let (name, delimiter) = rules::new_folder(&names(&folders));
    let id: i64 = tx
        .prepare_cached(
            "INSERT INTO folders(account_id, name, name_raw, delimiter, role, attributes, \
               selectable) VALUES (?1, ?2, ?2, ?3, 'other', '', 1) RETURNING id",
        )?
        .query_row(
            params![account.0, name, delimiter.map(String::from)],
            |row| row.get(0),
        )?;
    if !is_local(tx, account)? {
        tx.prepare_cached("INSERT INTO fork_folder_creates(folder_id) VALUES (?1)")?
            .execute([id])?;
    }
    Ok(FolderId(id))
}

/// The conversation's shown messages in the scope's `folders` that a remind sends: not its
/// copies in Sent, Drafts, Trash or Junk, nor those already in the Reminders folder.
fn members(
    tx: &Connection,
    thread: ThreadId,
    folders: &[FolderId],
    reminders: Option<FolderId>,
) -> rusqlite::Result<Vec<Row>> {
    let ids: Vec<(MessageId, FolderId)> = tx
        .prepare_cached(
            "SELECT m.id, m.folder_id FROM messages m INDEXED BY messages_thread \
               JOIN folders f ON f.id = m.folder_id \
             WHERE m.thread_id = ?1 AND m.flags & 264 = 0 \
               AND f.role NOT IN ('sent', 'drafts', 'trash', 'junk') \
             ORDER BY m.date, m.id",
        )?
        .query_map([thread.0], |row| {
            Ok((MessageId(row.get(0)?), FolderId(row.get(1)?)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut members = Vec::new();
    for (id, folder) in ids {
        if Some(folder) != reminders
            && folders.contains(&folder)
            && let Some(found) = row(tx, id)?
        {
            members.push(found);
        }
    }
    Ok(members)
}

/// Whether a message of the reminder's conversation isn't stored now (one of its Message-IDs
/// with no message of the account).
fn in_transit(tx: &Connection, reminder: &Live) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM fork_reminder_keys k WHERE k.reminder = ?1 \
           AND NOT EXISTS (SELECT 1 FROM messages m JOIN folders x ON x.id = m.folder_id \
             WHERE m.message_id = k.message_id AND x.account_id = ?2))",
    )?
    .query_row(params![reminder.id, reminder.account.0], |row| row.get(0))
}

/// Gives a reminder its conversation's Message-IDs now (its key, `remind.rs`).
fn add_keys(tx: &Connection, reminder: i64, thread: ThreadId) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO fork_reminder_keys(reminder, message_id) \
         SELECT DISTINCT ?1, message_id FROM messages INDEXED BY messages_thread \
         WHERE thread_id = ?2 AND message_id IS NOT NULL",
    )?
    .execute(params![reminder, thread.0])?;
    Ok(())
}

/// The moves that bring a reminder's messages back from its Reminders folder (where the op
/// queue has them, or takes them) to the Inbox they came from, else the account's first
/// Inbox; none when the account has no Inbox, or no Reminders folder. Once its own folder is
/// gone it looks in the account's Reminders folder by name, which the server may have listed
/// again (`remind::waits_in`).
fn back(tx: &Connection, reminder: &Live) -> rusqlite::Result<Vec<(Row, Change)>> {
    let Some(folder) = remind::waits_in(tx, reminder)? else {
        return Ok(Vec::new());
    };
    let inbox: Option<i64> = tx
        .prepare_cached(
            "SELECT id FROM folders WHERE account_id = ?1 AND role = 'inbox' AND selectable \
             ORDER BY id <> ?2, id LIMIT 1",
        )?
        .query_row(params![reminder.account.0, reminder.origin.0], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(inbox) = inbox.map(FolderId) else {
        return Ok(Vec::new());
    };
    let mut moves = Vec::new();
    let mut select = tx.prepare_cached(
        "SELECT id FROM messages INDEXED BY messages_thread WHERE thread_id = ?1",
    )?;
    for thread in &reminder.threads {
        let ids: Vec<MessageId> = select
            .query_map([thread.0], |row| row.get(0).map(MessageId))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            let Some(found) = row(tx, id)? else {
                continue;
            };
            let folded = overlay::fold(found.folder, chain(tx, id)?.iter().map(|op| op.change));
            if !folded.expunge && folded.at == folder {
                moves.push((found, to(inbox)));
            }
        }
    }
    Ok(moves)
}
