//! The Split Inbox's reads (docs/superhuman/split.md): the user's choices by sender, a view's
//! counts on each side, and the messages of a folder whose server category the engine still
//! has to ask for.

use super::{Reader, version};
use crate::store::api::{SplitCounts, Uncategorized, Versioned, ViewFilter, ViewSpec};
use crate::store::{StoreError, views};
use rusqlite::params;
use sealwax_domain::model::{FolderId, MessageId};
use sealwax_domain::split::Split;

impl Reader {
    /// Every sender the user chose a side for, by address (lower case), in order.
    pub(super) fn sender_splits(&self) -> Result<Vec<(String, Split)>, StoreError> {
        let choices = self
            .conn
            .prepare_cached("SELECT address, important FROM fork_sender_split ORDER BY address")?
            .query_map([], |row| Ok((row.get(0)?, Split::of(row.get(1)?))))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(choices)
    }

    /// The totals of `spec`'s scope on each side, in its grouping and whatever its filter, from
    /// one snapshot (the Split Inbox's tabs count every conversation, not the unread ones).
    pub(super) fn split_counts(
        &self,
        spec: ViewSpec,
    ) -> Result<Versioned<SplitCounts>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let side = |split| spec.with_filter(ViewFilter::All).with_split(Some(split));
        let value = SplitCounts {
            important: views::total(&tx, side(Split::Important))?,
            other: views::total(&tx, side(Split::Other))?,
        };
        Ok(Versioned { version, value })
    }

    /// The shown messages of `folder` without a server category, newest first, at most
    /// `limit`.
    pub(super) fn uncategorized(
        &self,
        folder: FolderId,
        limit: u32,
    ) -> Result<Vec<Uncategorized>, StoreError> {
        let found = self
            .conn
            .prepare_cached(
                "SELECT m.id, m.uid FROM messages m INDEXED BY messages_folder_date \
                   JOIN fork_message_split s ON s.id = m.id \
                 WHERE m.folder_id = ?1 AND m.flags & 264 = 0 AND s.category IS NULL \
                 ORDER BY m.date DESC, m.id DESC LIMIT ?2",
            )?
            .query_map(params![folder.0, limit], |row| {
                Ok(Uncategorized {
                    message: MessageId(row.get(0)?),
                    uid: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(found)
    }
}
