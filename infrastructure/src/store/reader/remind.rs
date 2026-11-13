//! Remind Me's reads (docs/superhuman/remind.md): the reminders that still count, and the
//! Reminders folders the engine must create on a server, or sync once, before a move into one
//! runs.

use super::Reader;
use crate::store::StoreError;
use crate::store::remind;
use crate::store::remind_api::{FolderToPrepare, Reminder, ReminderId};
use rusqlite::params;
use sealwax_domain::model::{AccountId, FolderId};

impl Reader {
    /// Waiting and reminded reminders, the soonest due first, with their conversations' threads
    /// now: one snapshot.
    pub(super) fn reminders(&self) -> Result<Vec<Reminder>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let live = remind::live(&tx)?;
        Ok(live
            .into_iter()
            .map(|reminder| Reminder {
                id: ReminderId(reminder.id),
                account: reminder.account,
                threads: reminder.threads.into_iter().collect(),
                state: reminder.state,
                wake_at: reminder.wake_at,
                reminded_at: reminder.reminded_at,
                folder: reminder.folder,
                origin: reminder.origin,
            })
            .collect())
    }

    /// `account`'s Reminders folders (one the store made, or one a reminder waits in) that a
    /// move due at `now` (Unix ms) goes into, when the server doesn't have it yet or the store
    /// never synced it.
    pub(super) fn folders_to_prepare(
        &self,
        account: AccountId,
        now: i64,
    ) -> Result<Vec<FolderToPrepare>, StoreError> {
        let folders = self
            .conn
            .prepare_cached(
                "SELECT f.id, f.name_raw, \
                   EXISTS (SELECT 1 FROM fork_folder_creates c WHERE c.folder_id = f.id), \
                   f.uidvalidity IS NULL \
                 FROM folders f \
                 WHERE f.account_id = ?1 \
                   AND (f.id IN (SELECT folder_id FROM fork_folder_creates) \
                     OR f.id IN (SELECT folder_id FROM fork_reminders \
                       WHERE folder_id IS NOT NULL)) \
                   AND (f.uidvalidity IS NULL \
                     OR f.id IN (SELECT folder_id FROM fork_folder_creates)) \
                   AND EXISTS (SELECT 1 FROM ops o \
                     WHERE o.target = f.id AND o.done = 0 AND o.not_before <= ?2) \
                 ORDER BY f.id",
            )?
            .query_map(params![account.0, now], |row| {
                Ok(FolderToPrepare {
                    folder: FolderId(row.get(0)?),
                    name_raw: row.get(1)?,
                    create: row.get(2)?,
                    sync: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(folders)
    }
}
