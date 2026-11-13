//! The reads behind body downloads (ML-044): where each message is on the server, where one
//! attachment is, which messages of Inbox conversations the prefetch may download, and what the
//! bodies take.

use super::Reader;
use crate::store::StoreError;
use crate::store::api::{BodyCandidate, BodyState, BodyTarget, BodyUsage, PartSource};
use rusqlite::OptionalExtension;
use sealwax_domain::model::{AccountId, BlobKey, FolderId, FolderRole, MessageId};

/// `conversation::rank` in SQL, of the folder role in the column `role` (Trash and Junk, which
/// the prefetch leaves out, rank with the other folders here).
fn rank_sql(role: &str) -> String {
    format!(
        "CASE {role} WHEN 'inbox' THEN 0 WHEN 'archive' THEN 1 WHEN 'all' THEN 2 \
         WHEN 'sent' THEN 3 WHEN 'drafts' THEN 5 ELSE 4 END"
    )
}

impl Reader {
    /// The stored messages of `ids`, in that order; ids the store doesn't hold are left out.
    pub(super) fn body_targets(&self, ids: &[MessageId]) -> Result<Vec<BodyTarget>, StoreError> {
        let mut select = self.conn.prepare_cached(
            "SELECT f.account_id, m.folder_id, m.uid, m.size, m.body_state, m.blob_key \
             FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.id = ?1",
        )?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let target = select
                .query_row([id.0], |row| {
                    Ok(BodyTarget {
                        message: *id,
                        account: AccountId(row.get(0)?),
                        folder: FolderId(row.get(1)?),
                        uid: row.get(2)?,
                        size: row.get::<_, i64>(3)?.max(0) as u64,
                        body_state: BodyState::from_code(row.get(4)?),
                        blob: row
                            .get::<_, Option<Vec<u8>>>(5)?
                            .and_then(|key| BlobKey::from_slice(&key)),
                    })
                })
                .optional()?;
            out.extend(target);
        }
        Ok(out)
    }

    /// Where the attachment at mail-parser index `part` of `message` is on the server.
    pub(super) fn part_source(
        &self,
        message: MessageId,
        part: u32,
    ) -> Result<Option<PartSource>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT m.folder_id, m.uid, p.section, p.mime_type FROM message_parts p \
                   JOIN messages m ON m.id = p.message \
                 WHERE p.message = ?1 AND p.part = ?2 AND p.role = 2 AND p.section IS NOT NULL",
            )?
            .query_row(rusqlite::params![message.0, part], |row| {
                Ok(PartSource {
                    message,
                    folder: FolderId(row.get(0)?),
                    uid: row.get(1)?,
                    section: row.get(2)?,
                    mime_type: row.get(3)?,
                })
            })
            .optional()?)
    }

    /// Messages without a body that arrived since `since`, in the Inboxes of server accounts
    /// (`accounts.toml` lists them) and, since the fork's Superhuman pass, in the rest of those
    /// Inboxes' conversations (a reply in Sent, a copy in the Archive; never the Trash or
    /// Junk): the Inbox's first, each part newest first; hidden ones are left out. A message
    /// the account holds in several folders (sent to oneself, a Gmail label) comes once, as the
    /// copy the conversation shows (`conversation::rank`: Inbox, Archive, All Mail, Sent, other
    /// folders, Drafts, then the oldest stored), and not at all when that copy has its body:
    /// Gmail counts every download against its daily IMAP limit.
    pub(super) fn body_candidates(
        &self,
        since: i64,
        limit: u32,
    ) -> Result<Vec<BodyCandidate>, StoreError> {
        // From the Inboxes' conversations (`messages_thread`), so only they are read; a
        // message's other copies by `messages_message_id`.
        let sql = format!(
            "WITH inbox(thread_id) AS ( \
               SELECT DISTINCT i.thread_id FROM folders fi \
                 JOIN accounts ai ON ai.id = fi.account_id \
                 JOIN messages i ON i.folder_id = fi.id \
               WHERE fi.role = 'inbox' AND ai.key IS NOT NULL AND ai.detached = 0 \
                 AND i.flags & 264 = 0) \
             SELECT m.id, f.account_id, m.folder_id, f.role, m.date, m.size \
             FROM inbox t JOIN messages m ON m.thread_id = t.thread_id \
               JOIN folders f ON f.id = m.folder_id \
               JOIN accounts a ON a.id = f.account_id \
             WHERE a.key IS NOT NULL AND a.detached = 0 \
               AND f.role NOT IN ('trash', 'junk') \
               AND m.blob_key IS NULL AND m.date >= ?1 AND m.flags & 264 = 0 \
               AND (f.role = 'inbox' OR coalesce(m.message_id, '') = '' OR NOT EXISTS ( \
                 SELECT 1 FROM messages c JOIN folders cf ON cf.id = c.folder_id \
                 WHERE c.message_id = m.message_id AND c.id <> m.id \
                   AND cf.account_id = f.account_id AND cf.role NOT IN ('trash', 'junk') \
                   AND c.flags & 264 = 0 \
                   AND ({copy} < {this} OR ({copy} = {this} AND c.id < m.id)))) \
             ORDER BY f.role = 'inbox' DESC, m.date DESC, m.id DESC LIMIT ?2",
            copy = rank_sql("cf.role"),
            this = rank_sql("f.role"),
        );
        let candidates = self
            .conn
            .prepare_cached(&sql)?
            .query_map(rusqlite::params![since, limit], |row| {
                Ok(BodyCandidate {
                    message: MessageId(row.get(0)?),
                    account: AccountId(row.get(1)?),
                    folder: FolderId(row.get(2)?),
                    role: FolderRole::parse(&row.get::<_, String>(3)?),
                    date: row.get(4)?,
                    size: row.get::<_, i64>(5)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(candidates)
    }

    pub(super) fn body_usage(&self) -> Result<BodyUsage, StoreError> {
        Ok(self.conn.query_row(
            "SELECT count(*), coalesce(sum(size), 0) FROM bodies",
            [],
            |row| {
                Ok(BodyUsage {
                    bodies: row.get::<_, i64>(0)?.max(0) as u64,
                    bytes: row.get::<_, i64>(1)?.max(0) as u64,
                })
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::rank_sql;
    use sealwax_domain::model::FolderRole;

    #[test]
    fn the_prefetch_ranks_copies_as_the_conversation_does() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for role in FolderRole::ALL {
            if matches!(role, FolderRole::Trash | FolderRole::Junk) {
                continue;
            }
            let ranked: u8 = conn
                .query_row(
                    &format!("SELECT {}", rank_sql("?1")),
                    [role.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(ranked, crate::store::conversation::rank(role), "{role:?}");
        }
    }
}
