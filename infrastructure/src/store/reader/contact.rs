//! The contact pane's read (docs/superhuman/contact.md): a sender's messages and the
//! conversations they last wrote in, from the split rows' sender index (`fork.rs`' fourth step).
//! Both walks read the index alone: the count its sender's range (copies of a message, the same
//! conversation at the same date, side by side in it, count once), the conversations a few
//! entries from its end.

use super::{Reader, version};
use crate::store::StoreError;
use crate::store::contact_api::{ContactConversation, ContactFacts};
use crate::store::conversation::rank;
use crate::store::views::UNSEARCHED;
use rusqlite::{Connection, OptionalExtension, params};
use sealwax_domain::model::{FolderId, FolderRole, MessageId, ThreadId};
use sealwax_domain::split;
use std::collections::HashMap;

use crate::store::Versioned;

/// The folders a sender's mail counts in, with their roles: every folder of an attached account
/// but the Trash and the Junk, as a search of every folder (`views::searched`).
fn counted_folders(conn: &Connection) -> rusqlite::Result<HashMap<i64, FolderRole>> {
    conn.prepare_cached(
        "SELECT f.id, f.role FROM folders f JOIN accounts a ON a.id = f.account_id \
         WHERE a.detached = 0 AND f.role NOT IN (?1, ?2)",
    )?
    .query_map([UNSEARCHED[0].as_str(), UNSEARCHED[1].as_str()], |row| {
        Ok((row.get(0)?, FolderRole::parse(&row.get::<_, String>(1)?)))
    })?
    .collect()
}

/// A sender's messages in the counted folders, each once, and the first's and latest's dates.
/// A message with neither an INTERNALDATE nor a Date header is stored at 0 (the epoch): it
/// counts, but dates nothing.
pub(crate) const COUNT: &str = "
SELECT count(*), min(CASE WHEN date > 0 THEN date END), max(CASE WHEN date > 0 THEN date END)
FROM (
    SELECT DISTINCT date, thread_id
    FROM fork_message_split INDEXED BY fork_message_split_by_sender
    WHERE sender = ?1 AND split <> 0
        AND folder_id IN (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id
            WHERE a.detached = 0 AND f.role NOT IN (?2, ?3))
)";

/// A sender's shown messages, newest first, from the end of their range in the index.
pub(crate) const NEWEST: &str = "
SELECT id, thread_id, folder_id, date
FROM fork_message_split INDEXED BY fork_message_split_by_sender
WHERE sender = ?1 AND split <> 0
ORDER BY date DESC, thread_id DESC";

impl Reader {
    /// What the store holds from `address`, from one snapshot ([`crate::store::Store::contact`]).
    pub(super) fn contact(
        &self,
        address: &str,
        limit: u32,
    ) -> Result<Versioned<ContactFacts>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let value = match split::sender_key(address) {
            Some(sender) => facts(&tx, &sender, limit)?,
            None => ContactFacts::default(),
        };
        Ok(Versioned { version, value })
    }
}

fn facts(conn: &Connection, sender: &str, limit: u32) -> rusqlite::Result<ContactFacts> {
    let (messages, first, last): (u32, Option<i64>, Option<i64>) =
        conn.prepare_cached(COUNT)?.query_row(
            params![sender, UNSEARCHED[0].as_str(), UNSEARCHED[1].as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let recent = if limit == 0 || messages == 0 {
        Vec::new()
    } else {
        recent(conn, sender, limit as usize)?
    };
    Ok(ContactFacts {
        messages,
        first,
        last,
        recent,
    })
}

/// How many of a sender's messages the walk for their conversations reads at most: a sender
/// whose last few thousand messages are all in fewer conversations than asked for (a ticket
/// system's one thread) shows those.
const WALK: usize = 4_096;

/// The `limit` conversations `sender` last wrote in, newest first: each one's latest message
/// of theirs, the copy in the best-ranked folder (its copies come side by side in the walk).
fn recent(
    conn: &Connection,
    sender: &str,
    limit: usize,
) -> rusqlite::Result<Vec<ContactConversation>> {
    let folders = counted_folders(conn)?;
    let mut found: Vec<ContactConversation> = Vec::new();
    let mut statement = conn.prepare_cached(NEWEST)?;
    let mut rows = statement.query([sender])?;
    let mut walked = 0;
    while let Some(row) = rows.next()? {
        walked += 1;
        if walked > WALK {
            break;
        }
        let (id, thread, folder, date): (i64, i64, i64, i64) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        let Some(role) = folders.get(&folder).copied() else {
            continue;
        };
        if let Some(known) = found.iter_mut().find(|known| known.thread.0 == thread) {
            // Another copy of the message found first is a better one if its folder ranks
            // higher; an older message of theirs in the conversation changes nothing.
            if known.date == date && rank(role) < rank(folders[&known.folder.0]) {
                known.message = MessageId(id);
                known.folder = FolderId(folder);
            }
            continue;
        }
        // The copies of the last one found came before any other conversation's message.
        if found.len() == limit {
            break;
        }
        found.push(ContactConversation {
            thread: ThreadId(thread),
            subject: String::new(),
            message: MessageId(id),
            folder: FolderId(folder),
            date,
        });
    }
    let mut subject = conn.prepare_cached("SELECT subject FROM threads WHERE id = ?1")?;
    for conversation in &mut found {
        conversation.subject = subject
            .query_row([conversation.thread.0], |row| row.get(0))
            .optional()?
            .unwrap_or_default();
    }
    Ok(found)
}
