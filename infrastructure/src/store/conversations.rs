//! Conversation views (the fork's threading, docs/superhuman/store.md): one row per
//! conversation with a shown message in the view's folders, newest first by the conversation's
//! latest message, read from `fork_thread_folders` (`fork.rs`) the way the message views read
//! their partial indexes. A row (`ConversationSummary`) carries what the list shows of a
//! conversation: its latest message's sender, date and snippet, its participants, and what its
//! members in the view's folders are; triage expands it to those members (`members`).

use super::api::{ConversationHit, ConversationMember, ViewFilter, ViewSpec};
use super::views::{self, Kept};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sealwax_domain::message::base_subject;
use sealwax_domain::model::{
    AccountId, ConversationSummary, Flags, FolderId, FolderRole, MessageId, Participant, ThreadId,
};
use sealwax_domain::remind;
use sealwax_domain::search::Query;
use sealwax_domain::split::Split;
use std::collections::HashMap;
use std::fmt::Write as _;

/// Participants a row lists: the first sender and the three who wrote last.
const MAX_PARTICIPANTS: usize = 4;
/// Messages a conversation search reads at most before it groups them.
const SEARCH_READS: u32 = 20_000;

/// One `fork_thread_folders` row: a conversation's members in one folder, with the
/// conversation's own figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ForkRow {
    pub(crate) thread: ThreadId,
    pub(crate) folder: FolderId,
    pub(crate) messages: u32,
    pub(crate) unread: u32,
    pub(crate) flagged: u32,
    pub(crate) attachments: u32,
    /// The newest member, `(date, id)`.
    pub(crate) last: (i64, MessageId),
    /// The oldest unread member.
    pub(crate) first_unread: Option<(i64, MessageId)>,
    /// The row's sort key: the conversation's latest message (`fork.rs`).
    pub(crate) sort: (i64, MessageId),
    pub(crate) shown: u32,
    pub(crate) drafts: u32,
    /// The folder is a Trash, Junk or Drafts folder: `sort` is the row's own newest member.
    pub(crate) bin: bool,
    /// Members the Split Inbox puts in Important (docs/superhuman/split.md).
    pub(crate) important: u32,
}

impl ForkRow {
    /// The row as a list row shows it: everything but which members are Important, which
    /// only decides the split views a conversation is in.
    pub(crate) fn shown(&self) -> ForkRow {
        ForkRow {
            important: 0,
            ..*self
        }
    }
}

fn fork_row(row: &Row<'_>) -> rusqlite::Result<ForkRow> {
    let unread_date: Option<i64> = row.get(8)?;
    let unread_id: Option<i64> = row.get(9)?;
    Ok(ForkRow {
        thread: ThreadId(row.get(0)?),
        folder: FolderId(row.get(1)?),
        messages: row.get(2)?,
        unread: row.get(3)?,
        flagged: row.get(4)?,
        attachments: row.get(5)?,
        last: (row.get(6)?, MessageId(row.get(7)?)),
        first_unread: unread_date.zip(unread_id.map(MessageId)),
        sort: (row.get(10)?, MessageId(row.get(11)?)),
        shown: row.get(12)?,
        drafts: row.get(13)?,
        bin: row.get(14)?,
        important: row.get(15)?,
    })
}

/// Every row of a conversation, by folder.
pub(crate) fn rows_of(conn: &Connection, thread: ThreadId) -> rusqlite::Result<Vec<ForkRow>> {
    conn.prepare_cached(&format!(
        "SELECT {} FROM fork_thread_folders WHERE thread_id = ?1 ORDER BY folder_id",
        super::fork::COLUMNS
    ))?
    .query_map([thread.0], fork_row)?
    .collect()
}

/// The partial index a filter reads and the condition that selects it, as `views::filter_sql`
/// for messages: a conversation passes when one of its members in the folder does. A side of
/// the Split Inbox under another filter walks the filter's index (a row is one conversation in
/// one folder, so both conditions on it are the conversation's there).
fn filter_sql(kept: Kept) -> (&'static str, String) {
    let (index, condition) = match (kept.filter, kept.side) {
        (ViewFilter::All, Some(Split::Important)) => {
            ("fork_thread_folders_important", " AND important > 0")
        }
        (ViewFilter::All, Some(Split::Other)) => {
            ("fork_thread_folders_other", " AND important = 0")
        }
        (ViewFilter::All | ViewFilter::Important | ViewFilter::Other, _) => {
            ("fork_thread_folders_list", "")
        }
        (ViewFilter::Unread, _) => ("fork_thread_folders_unread", " AND unread > 0"),
        (ViewFilter::Starred, _) => ("fork_thread_folders_flagged", " AND flagged > 0"),
        (ViewFilter::Attachments, _) => ("fork_thread_folders_attachments", " AND attachments > 0"),
    };
    let side = match (kept.filter, kept.side) {
        (ViewFilter::All, _) | (_, None) => "",
        (_, Some(Split::Important)) => " AND important > 0",
        (_, Some(Split::Other)) => " AND important = 0",
    };
    (index, format!("{condition}{side}"))
}

/// The column a filter counts, for a folded view's walk.
fn filter_column(filter: ViewFilter) -> Option<&'static str> {
    match filter {
        ViewFilter::Unread => Some("unread"),
        ViewFilter::Starred => Some("flagged"),
        ViewFilter::Attachments => Some("attachments"),
        ViewFilter::All | ViewFilter::Important | ViewFilter::Other => None,
    }
}

/// A folded view's walk: the condition on each row, and the one on each conversation's rows
/// together. Other keeps a conversation none of whose rows in the view's folders has an
/// Important member, which no single row tells; under another filter, the filter's member and
/// the side's may be in different rows, so both are the conversation's rows' together.
fn fold_sql(kept: Kept) -> (String, String) {
    match (filter_column(kept.filter), kept.side) {
        (None, Some(Split::Other)) => (String::new(), " HAVING max(important) = 0".to_owned()),
        (Some(column), Some(side)) => (
            String::new(),
            format!(
                " HAVING max({column}) > 0 AND max(important) {}",
                match side {
                    Split::Important => "> 0",
                    Split::Other => "= 0",
                }
            ),
        ),
        _ => (filter_sql(kept).1, String::new()),
    }
}

fn row_passes(filter: ViewFilter, row: &ForkRow) -> bool {
    match filter {
        ViewFilter::All => row.messages > 0,
        ViewFilter::Unread => row.unread > 0,
        ViewFilter::Starred => row.flagged > 0,
        ViewFilter::Attachments => row.attachments > 0,
        ViewFilter::Important => row.important > 0,
        ViewFilter::Other => row.important == 0,
    }
}

/// A conversation view's folders, filter and side. `fold`: two of its folders belong to one
/// account, so a conversation can be in both, and the walks count it once (an account with two
/// Inbox folders; the merged walk is used otherwise).
#[derive(Clone, Debug)]
pub(crate) struct Shape {
    pub(crate) folders: Vec<FolderId>,
    pub(crate) kept: Kept,
    fold: bool,
}

impl Shape {
    pub(crate) fn of(conn: &Connection, spec: ViewSpec) -> rusqlite::Result<Shape> {
        Self::of_folders(conn, views::folders(conn, spec.scope)?, Kept::of(spec))
    }

    pub(crate) fn of_folders(
        conn: &Connection,
        folders: Vec<FolderId>,
        kept: Kept,
    ) -> rusqlite::Result<Shape> {
        let mut fold = false;
        if folders.len() > 1 {
            let mut owner = conn.prepare_cached("SELECT account_id FROM folders WHERE id = ?1")?;
            let mut accounts = Vec::with_capacity(folders.len());
            for folder in &folders {
                let account: Option<i64> =
                    owner.query_row([folder.0], |row| row.get(0)).optional()?;
                accounts.extend(account);
            }
            accounts.sort_unstable();
            fold = accounts.windows(2).any(|pair| pair[0] == pair[1]);
        }
        Ok(Shape {
            folders,
            kept,
            fold,
        })
    }

    /// The rows of `rows` (one conversation's) in the view's folders.
    pub(crate) fn rows_in(&self, rows: &[ForkRow]) -> Vec<ForkRow> {
        rows.iter()
            .filter(|row| self.folders.contains(&row.folder))
            .copied()
            .collect()
    }

    /// Whether a conversation with these rows in the view's folders is in the view: one of
    /// them passes the filter, and it is on the view's side: Important with an Important member
    /// there, Other with none.
    pub(crate) fn holds(&self, rows: &[ForkRow]) -> bool {
        let filtered = rows.iter().any(|row| row_passes(self.kept.filter, row));
        filtered
            && match self.kept.side {
                None => true,
                Some(Split::Important) => rows
                    .iter()
                    .any(|row| row_passes(ViewFilter::Important, row)),
                Some(Split::Other) => rows.iter().all(|row| row_passes(ViewFilter::Other, row)),
            }
    }
}

/// A conversation's place among `rows` (its rows in a view's folders): its sort key, and the
/// folder of the row that carries it. The rows of ordinary folders all carry the conversation's
/// latest message; a Trash, Junk or Drafts folder's row its own newest member, which counts
/// only when the view holds the conversation in no other folder (a view of that folder alone).
pub(crate) fn key(rows: &[ForkRow]) -> Option<((i64, MessageId), FolderId)> {
    let ordinary = rows
        .iter()
        .filter(|row| !row.bin)
        .map(|row| (row.sort, row.folder))
        .max();
    ordinary.or_else(|| rows.iter().map(|row| (row.sort, row.folder)).max())
}

fn list(folders: &[FolderId]) -> String {
    let mut out = String::new();
    for (n, folder) in folders.iter().enumerate() {
        if n > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{}", folder.0);
    }
    out
}

/// A folder's conversations the view keeps, from the maintained `fork_folder_counts`; a side
/// under another filter has no count of its own and walks the filter's index.
fn count_in(conn: &Connection, folder: FolderId, kept: Kept) -> rusqlite::Result<i64> {
    let column = match (kept.filter, kept.side) {
        (ViewFilter::All, Some(Split::Important)) => "important",
        (ViewFilter::All, Some(Split::Other)) => "other",
        (ViewFilter::All | ViewFilter::Important | ViewFilter::Other, _) => "conversations",
        (filter, None) => filter_column(filter).unwrap_or("conversations"),
        (_, Some(_)) => {
            let (index, condition) = filter_sql(kept);
            return conn
                .prepare_cached(&format!(
                    "SELECT count(*) FROM fork_thread_folders INDEXED BY {index} \
                     WHERE folder_id = ?1{condition}"
                ))?
                .query_row([folder.0], |row| row.get(0));
        }
    };
    conn.prepare_cached(&format!(
        "SELECT coalesce((SELECT {column} FROM fork_folder_counts WHERE folder_id = ?1), 0)"
    ))?
    .query_row([folder.0], |row| row.get(0))
}

/// Conversations in the view.
pub(crate) fn count(conn: &Connection, shape: &Shape) -> rusqlite::Result<u32> {
    if shape.fold {
        let (condition, having) = fold_sql(shape.kept);
        let total: i64 = conn
            .prepare_cached(&format!(
                "SELECT count(*) FROM (SELECT thread_id FROM fork_thread_folders \
                 WHERE folder_id IN ({}){condition} GROUP BY thread_id{having})",
                list(&shape.folders)
            ))?
            .query_row([], |row| row.get(0))?;
        return Ok(total as u32);
    }
    let mut total = 0;
    for folder in &shape.folders {
        total += count_in(conn, *folder, shape.kept)?;
    }
    Ok(total as u32)
}

/// Conversations whose latest message is dated at or after `start`.
pub(crate) fn count_since(conn: &Connection, shape: &Shape, start: i64) -> rusqlite::Result<u32> {
    let (index, condition) = filter_sql(shape.kept);
    if shape.fold {
        let (condition, having) = fold_sql(shape.kept);
        let total: i64 = conn
            .prepare_cached(&format!(
                "SELECT count(*) FROM (SELECT thread_id FROM fork_thread_folders \
                 WHERE folder_id IN ({}){condition} AND sort_date >= ?1 \
                 GROUP BY thread_id{having})",
                list(&shape.folders)
            ))?
            .query_row([start], |row| row.get(0))?;
        return Ok(total as u32);
    }
    let mut statement = conn.prepare_cached(&format!(
        "SELECT count(*) FROM fork_thread_folders INDEXED BY {index} \
         WHERE folder_id = ?1{condition} AND sort_date >= ?2"
    ))?;
    let mut total = 0i64;
    for folder in &shape.folders {
        total += statement.query_row(params![folder.0, start], |row| row.get::<_, i64>(0))?;
    }
    Ok(total as u32)
}

/// The number of conversations in the view whose sort key is greater than `key`, which needn't
/// be a row's: a conversation's position when it is. As `views::position`, each folder is
/// counted from the end the key is nearer to.
pub(crate) fn rank(
    conn: &Connection,
    shape: &Shape,
    key: (i64, MessageId),
) -> rusqlite::Result<u32> {
    let (date, id) = (key.0, key.1.0);
    let (index, condition) = filter_sql(shape.kept);
    if shape.fold {
        let (condition, having) = fold_sql(shape.kept);
        let above: i64 = conn
            .prepare_cached(&format!(
                "SELECT count(*) FROM (SELECT thread_id FROM fork_thread_folders \
                 WHERE folder_id IN ({}){condition} \
                   AND (sort_date > ?1 OR (sort_date = ?1 AND sort_id > ?2)) \
                 GROUP BY thread_id{having})",
                list(&shape.folders)
            ))?
            .query_row(params![date, id], |row| row.get(0))?;
        return Ok(above as u32);
    }
    // Rows on one side of the key (`>` or `<`), or at it (`=`).
    let count = |comparison: &str, folder: FolderId| -> rusqlite::Result<i64> {
        let same = format!(
            "SELECT count(*) FROM fork_thread_folders INDEXED BY {index} \
             WHERE folder_id = ?1{condition} AND sort_date = ?2 AND sort_id {comparison} ?3"
        );
        let same = conn
            .prepare_cached(&same)?
            .query_row(params![folder.0, date, id], |row| row.get::<_, i64>(0))?;
        if comparison == "=" {
            return Ok(same);
        }
        let away = format!(
            "SELECT count(*) FROM fork_thread_folders INDEXED BY {index} \
             WHERE folder_id = ?1{condition} AND sort_date {comparison} ?2"
        );
        Ok(same
            + conn
                .prepare_cached(&away)?
                .query_row(params![folder.0, date], |row| row.get::<_, i64>(0))?)
    };
    let edge = |order: &str, folder: FolderId| -> rusqlite::Result<Option<i64>> {
        conn.prepare_cached(&format!(
            "SELECT sort_date FROM fork_thread_folders INDEXED BY {index} \
             WHERE folder_id = ?1{condition} AND sort_date > 0 ORDER BY sort_date {order} LIMIT 1"
        ))?
        .query_row([folder.0], |row| row.get(0))
        .optional()
    };
    let mut above = 0i64;
    for folder in &shape.folders {
        let deep = match (edge("ASC", *folder)?, edge("DESC", *folder)?) {
            (Some(oldest), Some(newest)) => {
                newest > oldest && (newest - date) * 2 > newest - oldest
            }
            _ => false,
        };
        above += if deep {
            count_in(conn, *folder, shape.kept)? - count("<", *folder)? - count("=", *folder)?
        } else {
            count(">", *folder)?
        };
    }
    Ok(above as u32)
}

/// Conversations `offset..offset + limit` of a view with `total`, as `views::page` walks
/// messages: from the oldest end past the middle, several folders merged by their indexes.
pub(crate) fn page(
    conn: &Connection,
    shape: &Shape,
    total: u32,
    offset: u32,
    limit: u32,
) -> rusqlite::Result<Vec<ConversationSummary>> {
    if offset >= total || limit == 0 || shape.folders.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.min(total - offset);
    let oldest_first = offset > total / 2;
    let (order, skip) = if oldest_first {
        ("ASC", total - offset - limit)
    } else {
        ("DESC", offset)
    };
    let (index, condition) = filter_sql(shape.kept);
    let sql = if let [folder] = shape.folders.as_slice() {
        format!(
            "SELECT thread_id FROM fork_thread_folders INDEXED BY {index} \
             WHERE folder_id = {}{condition} \
             ORDER BY sort_date {order}, sort_id {order} LIMIT ?1 OFFSET ?2",
            folder.0
        )
    } else if shape.fold {
        let (condition, having) = fold_sql(shape.kept);
        format!(
            "SELECT thread_id FROM fork_thread_folders WHERE folder_id IN ({}){condition} \
             GROUP BY thread_id{having} ORDER BY max(sort_date) {order}, max(sort_id) {order} \
             LIMIT ?1 OFFSET ?2",
            list(&shape.folders)
        )
    } else {
        let mut walks = String::new();
        for (n, folder) in shape.folders.iter().enumerate() {
            if n > 0 {
                walks.push_str(" UNION ALL ");
            }
            let _ = write!(
                walks,
                "SELECT thread_id, sort_date, sort_id FROM fork_thread_folders INDEXED BY {index} \
                 WHERE folder_id = {}{condition}",
                folder.0
            );
        }
        format!(
            "SELECT thread_id FROM ({walks} ORDER BY sort_date {order}, sort_id {order} \
             LIMIT ?1 OFFSET ?2)"
        )
    };
    let mut threads = conn
        .prepare_cached(&sql)?
        .query_map(params![limit, skip], |row| row.get(0).map(ThreadId))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if oldest_first {
        threads.reverse();
    }
    let mut rows = rows_of_many(conn, &threads)?;
    let page: Vec<(ThreadId, Vec<ForkRow>)> = threads
        .into_iter()
        .map(|thread| {
            let in_view = shape.rows_in(&rows.remove(&thread).unwrap_or_default());
            (thread, in_view)
        })
        .collect();
    Summaries::new(conn).many(&page, &shape.folders)
}

/// The rows of several conversations, by conversation.
fn rows_of_many(
    conn: &Connection,
    threads: &[ThreadId],
) -> rusqlite::Result<HashMap<ThreadId, Vec<ForkRow>>> {
    let mut out: HashMap<ThreadId, Vec<ForkRow>> = HashMap::new();
    for chunk in threads.chunks(IN_LIST) {
        let mut select = conn.prepare(&format!(
            "SELECT {} FROM fork_thread_folders WHERE thread_id IN ({}) \
             ORDER BY thread_id, folder_id",
            super::fork::COLUMNS,
            ids(chunk.iter().map(|thread| thread.0))
        ))?;
        for row in select.query_map([], fork_row)? {
            let row = row?;
            out.entry(row.thread).or_default().push(row);
        }
    }
    Ok(out)
}

/// Ids a batched read puts in one `IN` list.
const IN_LIST: usize = 500;

fn ids(ids: impl Iterator<Item = i64>) -> String {
    let mut out = String::new();
    for (n, id) in ids.enumerate() {
        if n > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{id}");
    }
    out
}

/// One shown copy of a conversation's message, as the participants read it.
/// A copy as read, before its folder's role is known: its sender's name and address, its
/// folder, flags and date.
type CopyRow = (String, String, FolderId, u32, i64);

struct Copy {
    name: String,
    email: String,
    folder: FolderId,
    flags: u32,
    role: FolderRole,
    date: i64,
}

/// Builds rows: the conversations' other columns read in a few batched queries per call, the
/// folders' roles and the accounts' own addresses kept across calls.
pub(crate) struct Summaries<'c> {
    conn: &'c Connection,
    own: HashMap<AccountId, Vec<String>>,
    roles: Option<HashMap<FolderId, FolderRole>>,
    /// A waiting conversation's row in its Reminders folder is dated by its sort key, the
    /// wait key (`remind::wait_key`), which the Reminders view reads back as the wake time.
    /// Off for search hits (phase C, the final review): a search's row always shows its
    /// latest message's date, as everywhere but that view.
    wait_keys: bool,
}

impl<'c> Summaries<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self {
            conn,
            own: HashMap::new(),
            roles: None,
            wait_keys: true,
        }
    }

    /// Rows dated by their latest message even where they wait in Reminders: a search's.
    pub(crate) fn dated(conn: &'c Connection) -> Self {
        Self {
            wait_keys: false,
            ..Self::new(conn)
        }
    }

    fn own(&mut self, account: AccountId) -> rusqlite::Result<&[String]> {
        if !self.own.contains_key(&account) {
            let addresses = self
                .conn
                .prepare_cached("SELECT address FROM accounts WHERE id = ?1")?
                .query_map([account.0], |row| row.get::<_, String>(0))?
                .map(|address| address.map(|address| address.trim().to_lowercase()))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            self.own.insert(account, addresses);
        }
        Ok(self
            .own
            .get(&account)
            .map(Vec::as_slice)
            .unwrap_or_default())
    }

    fn role(&mut self, folder: FolderId) -> rusqlite::Result<FolderRole> {
        if self.roles.is_none() {
            let roles = self
                .conn
                .prepare_cached("SELECT id, role FROM folders")?
                .query_map([], |row| {
                    Ok((
                        FolderId(row.get(0)?),
                        FolderRole::parse(&row.get::<_, String>(1)?),
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            self.roles = Some(roles);
        }
        Ok(self
            .roles
            .as_ref()
            .and_then(|roles| roles.get(&folder).copied())
            .unwrap_or(FolderRole::Other))
    }

    /// The rows of conversations in a view of `folders`, each from its rows there, in order.
    pub(crate) fn many(
        &mut self,
        threads: &[(ThreadId, Vec<ForkRow>)],
        folders: &[FolderId],
    ) -> rusqlite::Result<Vec<ConversationSummary>> {
        let conn = self.conn;
        let mut out = Vec::with_capacity(threads.len());
        for chunk in threads.chunks(IN_LIST) {
            let keys: Vec<((i64, MessageId), FolderId)> = chunk
                .iter()
                .map(|(_, rows)| key(rows).unwrap_or_default())
                .collect();
            let mut heads: HashMap<ThreadId, (AccountId, String)> = HashMap::new();
            let mut select = conn.prepare(&format!(
                "SELECT id, account_id, subject FROM threads WHERE id IN ({})",
                ids(chunk.iter().map(|(thread, _)| thread.0))
            ))?;
            for row in select.query_map([], |row| {
                Ok((ThreadId(row.get(0)?), (AccountId(row.get(1)?), row.get(2)?)))
            })? {
                let (thread, head) = row?;
                heads.insert(thread, head);
            }
            let mut latest: HashMap<MessageId, (String, String, String, String, i64)> =
                HashMap::new();
            let mut select = conn.prepare(&format!(
                "SELECT id, subject, from_name, from_addr, snippet, date FROM messages \
                 WHERE id IN ({})",
                ids(keys.iter().map(|((_, id), _)| id.0))
            ))?;
            for row in select.query_map([], |row| {
                Ok((
                    MessageId(row.get(0)?),
                    (
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ),
                ))
            })? {
                let (id, message) = row?;
                latest.insert(id, message);
            }
            let mut copies: HashMap<ThreadId, Vec<CopyRow>> = HashMap::new();
            let mut select = conn.prepare(&format!(
                "SELECT thread_id, from_name, from_addr, folder_id, flags, date \
                 FROM messages INDEXED BY messages_thread \
                 WHERE thread_id IN ({}) AND flags & 264 = 0 ORDER BY thread_id, date, id",
                ids(chunk.iter().map(|(thread, _)| thread.0))
            ))?;
            for row in select.query_map([], |row| {
                Ok((
                    ThreadId(row.get(0)?),
                    (
                        row.get(1)?,
                        row.get(2)?,
                        FolderId(row.get(3)?),
                        row.get(4)?,
                        row.get(5)?,
                    ),
                ))
            })? {
                let (thread, copy) = row?;
                copies.entry(thread).or_default().push(copy);
            }
            for ((thread, rows), ((key_date, latest_id), _)) in chunk.iter().zip(keys) {
                let (account, subject) = heads.remove(thread).unwrap_or_default();
                let (latest_subject, sender_name, sender_email, snippet, message_date) =
                    latest.get(&latest_id).cloned().unwrap_or_default();
                // A wait key's id is the latest message's, as every row's sort key is
                // (`fork.rs`), so that message's date is the conversation's.
                let latest_date = if !self.wait_keys && remind::wake_of_key(key_date).is_some() {
                    message_date
                } else {
                    key_date
                };
                let mut list = Vec::new();
                for (name, email, folder, flags, date) in copies.remove(thread).unwrap_or_default()
                {
                    let role = self.role(folder)?;
                    list.push(Copy {
                        name,
                        email,
                        folder,
                        flags,
                        role,
                        date,
                    });
                }
                let own = self.own(account)?;
                let (participants, participant_count) = participants(&list, own, folders);
                let last = rows
                    .iter()
                    .map(|row| (row.last, row.folder))
                    .max()
                    .unwrap_or_default();
                let anchor = rows
                    .iter()
                    .filter_map(|row| row.first_unread.map(|first| (first, row.folder)))
                    .min()
                    .unwrap_or(last);
                let sum = |field: fn(&ForkRow) -> u32| rows.iter().map(field).sum::<u32>();
                out.push(ConversationSummary {
                    thread: *thread,
                    account,
                    anchor: anchor.0.1,
                    folder: anchor.1,
                    last_member: last.0.1,
                    latest: latest_id,
                    date: latest_date,
                    sender_name,
                    sender_email,
                    snippet,
                    subject: if subject.is_empty() {
                        base_subject(&latest_subject).to_owned()
                    } else {
                        subject
                    },
                    participants,
                    participant_count,
                    messages: rows.first().map_or(0, |row| row.shown),
                    members: sum(|row| row.messages),
                    unread: sum(|row| row.unread),
                    flagged: sum(|row| row.flagged),
                    attachments: sum(|row| row.attachments),
                    has_draft: rows.first().is_some_and(|row| row.drafts > 0),
                });
            }
        }
        Ok(out)
    }
}

/// Whether a copy sits aside from the conversation's messages: in Trash, Junk or Drafts, or a
/// draft (`fork.rs` counts the same).
fn aside(copy: &Copy) -> bool {
    matches!(
        copy.role,
        FolderRole::Trash | FolderRole::Junk | FolderRole::Drafts
    ) || copy.flags & Flags::DRAFT.bits() != 0
}

/// Distinct senders of a conversation's shown copies (oldest first) in the order they first
/// wrote, with their newest name and when they last wrote, capped at [`MAX_PARTICIPANTS`] (the
/// first and the three who wrote last, so the row can name the latest writer, phase C), and how
/// many there are. Only copies outside Trash, Junk and drafts count while the conversation has
/// any. The user is any of `own` (lower case), and whoever sent a copy filed in Sent or Drafts;
/// a sender is unread with an unread copy in `folders`.
fn participants(copies: &[Copy], own: &[String], folders: &[FolderId]) -> (Vec<Participant>, u32) {
    let any_kept = copies.iter().any(|copy| !aside(copy));
    let key = |copy: &Copy| {
        if copy.email.is_empty() {
            copy.name.to_lowercase()
        } else {
            copy.email.to_lowercase()
        }
    };
    // Each person with the place of their newest copy, which orders the latest writers.
    let mut people: Vec<(String, Participant, usize)> = Vec::new();
    for (index, copy) in copies
        .iter()
        .enumerate()
        .filter(|(_, copy)| !any_kept || !aside(copy))
    {
        let key = key(copy);
        let at = match people.iter().position(|(known, _, _)| *known == key) {
            Some(at) => at,
            None => {
                people.push((
                    key.clone(),
                    Participant {
                        name: String::new(),
                        email: copy.email.clone(),
                        is_me: own.contains(&key),
                        unread: false,
                        last: copy.date,
                    },
                    index,
                ));
                people.len() - 1
            }
        };
        let (_, person, last) = &mut people[at];
        if !copy.name.is_empty() {
            person.name.clone_from(&copy.name);
        }
        person.last = copy.date;
        *last = index;
    }
    for copy in copies {
        let key = key(copy);
        let Some((_, person, _)) = people.iter_mut().find(|(known, _, _)| *known == key) else {
            continue;
        };
        if matches!(copy.role, FolderRole::Sent | FolderRole::Drafts) {
            person.is_me = true;
        }
        if copy.flags & Flags::SEEN.bits() == 0 && folders.contains(&copy.folder) {
            person.unread = true;
        }
    }
    let count = people.len() as u32;
    if people.len() > MAX_PARTICIPANTS {
        // The first, and the three who wrote last, still in the order they first wrote.
        let mut latest: Vec<usize> = (1..people.len()).collect();
        latest.sort_unstable_by_key(|at| std::cmp::Reverse(people[*at].2));
        latest.truncate(MAX_PARTICIPANTS - 1);
        let mut at = 0;
        people.retain(|_| {
            let kept = at == 0 || latest.contains(&at);
            at += 1;
            kept
        });
    }
    let people = people.into_iter().map(|(_, person, _)| person).collect();
    (people, count)
}

/// The members of `threads` in `folders` that pass `filter`, each conversation's oldest first,
/// in the order of `threads`.
pub(crate) fn members(
    conn: &Connection,
    threads: &[ThreadId],
    folders: &[FolderId],
    filter: ViewFilter,
) -> rusqlite::Result<Vec<ConversationMember>> {
    let mut select = conn.prepare_cached(
        "SELECT m.id, m.folder_id, m.flags, m.date, m.has_attachments, coalesce(s.split, 0) \
         FROM messages m INDEXED BY messages_thread \
           LEFT JOIN fork_message_split s ON s.id = m.id \
         WHERE m.thread_id = ?1 AND m.flags & 264 = 0 ORDER BY m.date, m.id",
    )?;
    let mut out = Vec::new();
    for thread in threads {
        let rows = select.query_map([thread.0], |row| {
            Ok((
                ConversationMember {
                    thread: *thread,
                    id: MessageId(row.get(0)?),
                    folder: FolderId(row.get(1)?),
                    flags: Flags::from_bits(row.get::<_, u32>(2)?),
                    date: row.get(3)?,
                    has_attachments: row.get(4)?,
                },
                row.get::<_, u8>(5)?,
            ))
        })?;
        for member in rows {
            let (member, split) = member?;
            if folders.contains(&member.folder)
                && views::passes(filter, member.flags.bits(), member.has_attachments, split)
            {
                out.push(member);
            }
        }
    }
    Ok(out)
}

/// The newest `limit` conversations a query finds, as `views::search` finds messages (in
/// `folder`, or in every searched folder), each with its newest match, newest match first.
/// Messages are read four times the limit at a time until enough conversations are found. Each
/// row is dated by its latest message ([`Summaries::dated`]), also one waiting in Reminders.
pub(crate) fn search(
    conn: &Connection,
    query: &Query,
    folder: Option<FolderId>,
    limit: u32,
) -> rusqlite::Result<Vec<ConversationHit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut reads = limit.saturating_mul(4).clamp(50, SEARCH_READS);
    let found = loop {
        let found = views::search(conn, query, folder, reads)?;
        let mut threads: Vec<ThreadId> = found.iter().map(|row| row.thread).collect();
        threads.sort_unstable();
        threads.dedup();
        if threads.len() >= limit as usize || found.len() < reads as usize || reads >= SEARCH_READS
        {
            break found;
        }
        reads = reads.saturating_mul(4).min(SEARCH_READS);
    };
    let shape = Shape::of_folders(
        conn,
        views::searched(conn, folder)?,
        Kept::from(ViewFilter::All),
    )?;
    let mut first: Vec<(ThreadId, MessageId, i64)> = Vec::new();
    for row in found {
        if first.len() >= limit as usize {
            break;
        }
        if !first.iter().any(|(thread, ..)| *thread == row.thread) {
            first.push((row.thread, row.id, row.date));
        }
    }
    let threads: Vec<ThreadId> = first.iter().map(|(thread, ..)| *thread).collect();
    let mut rows = rows_of_many(conn, &threads)?;
    let mut found = Vec::new();
    let mut matches = Vec::new();
    for (thread, matched, date) in first {
        let in_view = shape.rows_in(&rows.remove(&thread).unwrap_or_default());
        if !in_view.is_empty() {
            found.push((thread, in_view));
            matches.push((matched, date));
        }
    }
    let hits = Summaries::dated(conn)
        .many(&found, &shape.folders)?
        .into_iter()
        .zip(matches)
        .map(|(conversation, (matched, matched_date))| ConversationHit {
            matched,
            matched_date,
            conversation,
        })
        .collect();
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(name: &str, email: &str, folder: i64, flags: u32, role: FolderRole) -> Copy {
        Copy {
            name: name.into(),
            email: email.into(),
            folder: FolderId(folder),
            flags,
            role,
            date: 0,
        }
    }

    #[test]
    fn participants_keep_first_writers_order_and_mark_the_user() {
        let inbox = 1;
        let sent = 2;
        let copies = [
            copy(
                "Ingrid Berg",
                "ingrid@example.test",
                inbox,
                1,
                FolderRole::Inbox,
            ),
            copy("", "tomas@example.test", inbox, 0, FolderRole::Inbox),
            copy("Me Myself", "me@example.test", sent, 1, FolderRole::Sent),
            copy(
                "Ingrid B.",
                "INGRID@example.test",
                inbox,
                1,
                FolderRole::Inbox,
            ),
            copy("Tomas Lie", "tomas@example.test", 9, 0, FolderRole::Trash),
            copy("Draft", "alias@example.test", 7, 16, FolderRole::Drafts),
        ];
        let (people, count) =
            participants(&copies, &["me@example.test".into()], &[FolderId(inbox)]);
        assert_eq!(count, 3);
        let names: Vec<(&str, bool, bool)> = people
            .iter()
            .map(|person| (person.name.as_str(), person.is_me, person.unread))
            .collect();
        assert_eq!(
            names,
            [
                ("Ingrid B.", false, false),
                ("", false, true),
                ("Me Myself", true, false)
            ],
            "a name from Trash or a draft doesn't count while others do"
        );
        // Only drafts: they count, and their sender is the user.
        let (people, count) = participants(&copies[5..], &[], &[FolderId(7)]);
        assert_eq!((count, people[0].is_me), (1, true));
    }

    #[test]
    fn participants_beyond_four_keep_the_first_and_the_latest() {
        let copies: Vec<Copy> = (0..7)
            .map(|n| {
                copy(
                    &format!("P{n}"),
                    &format!("p{n}@example.test"),
                    1,
                    1,
                    FolderRole::Inbox,
                )
            })
            .collect();
        let (people, count) = participants(&copies, &[], &[FolderId(1)]);
        let names: Vec<&str> = people.iter().map(|person| person.name.as_str()).collect();
        assert_eq!((names, count), (vec!["P0", "P4", "P5", "P6"], 7));
    }

    /// Phase C: the three who wrote last, not the three who joined last, so an early writer who
    /// wrote the newest message is named (the visual QA: trip's newest is Tomas's, the row read
    /// "Ingrid … me, Sana"); each with when they last wrote.
    #[test]
    fn participants_beyond_four_keep_the_latest_writers() {
        let mut copies: Vec<Copy> = (0..7)
            .map(|n| Copy {
                date: 100 + n,
                ..copy(
                    &format!("P{n}"),
                    &format!("p{n}@example.test"),
                    1,
                    1,
                    FolderRole::Inbox,
                )
            })
            .collect();
        copies.push(Copy {
            date: 200,
            ..copy("P1", "p1@example.test", 1, 0, FolderRole::Inbox)
        });
        let (people, count) = participants(&copies, &[], &[FolderId(1)]);
        let named: Vec<(&str, i64, bool)> = people
            .iter()
            .map(|person| (person.name.as_str(), person.last, person.unread))
            .collect();
        assert_eq!(count, 7);
        assert_eq!(
            named,
            [
                ("P0", 100, false),
                ("P1", 200, true),
                ("P5", 105, false),
                ("P6", 106, false)
            ],
            "the first, then the three who wrote last, in the order they first wrote"
        );
    }
}
