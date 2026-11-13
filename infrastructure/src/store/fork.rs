//! The fork's own schema track and its conversation table (docs/superhuman/store.md). Upstream
//! numbers its migrations with `user_version`; the fork's tables are migrated after those, in
//! steps counted in `meta('fork_schema')`, so upstream's numbering stays upstream's and its
//! merges stay clean. Every step is idempotent, readers check only `user_version`, and a store
//! a newer fork build migrated is refused, as a newer upstream schema is.
//!
//! `fork_thread_folders` holds one row per conversation and folder with a shown message there
//! (`flags & 264 = 0`): the conversation views page through it. The writer keeps it exact
//! without a write path having to know: TEMP triggers on its connection note every thread whose
//! messages change (inserts, deletes, cascades, moves, merges, flags, a folder's role), and
//! before the transaction commits the writer recounts only those threads from `messages`
//! (`writer/conversations.rs`), comparing the rows before and after for the change event. Until
//! then the table still holds what the last commit published: the "before" of the diffs.
//! `meta('fork_through')` is the store version the table was last exact at; a store an older
//! build wrote to since (it bumps `store_version`, not this) is recounted at open.
//!
//! The Split Inbox (docs/superhuman/split.md) rides on the same recount: `fork_message_split`
//! keeps each message's facts and category from its ingest and publishes where the classifier
//! puts it, Important or Other, which depends on its thread (a conversation the user wrote in)
//! and the user's choices by sender; the recount of a dirty thread classifies its messages
//! again, and `fork_thread_folders` counts each row's Important members.
//!
//! Remind Me (docs/superhuman/remind.md) rides on it too: a conversation back from Reminders
//! sorts in its Inbox by when it came back while that is later than its latest message, so
//! the recount reads when the dirty threads' conversations came back (`DIRTY_BUMPS`) and the
//! sort key of their Inbox rows follows; the writer marks a conversation dirty whenever its
//! reminder starts or stops counting. Since phase C a conversation back from Reminders is
//! Important too (`split::classify_reminded`), and one waiting in a Reminders folder sorts
//! there by when it is due (`DIRTY_WAITS`, `remind::wait_key`), the soonest first, so the
//! Reminders view is that folder's view.

use super::StoreError;
use rusqlite::{Connection, OptionalExtension, params};
use sealwax_domain::model::{FolderId, MessageId, ThreadId};
use sealwax_domain::remind;
use sealwax_domain::split::{self, HeaderFacts, Split};
use std::collections::{BTreeSet, HashMap};

/// The fork's schema steps this build knows.
pub(crate) const VERSION: i64 = 4;

/// Each step, and whether it changes what the derived tables hold (they are recounted at the
/// open that applies it).
const MIGRATIONS: &[(&str, bool)] = &[(FORK1, true), (FORK2, true), (FORK3, false), (FORK4, false)];

/// Conversations per folder. Per folder: the shown members' count, unread, starred and with
/// attachments, the newest member and the oldest unread one, and whether the folder is a Trash,
/// Junk or Drafts folder (`bin`). Per conversation, repeated on each of its rows: the sort key
/// (`sort_date`, `sort_id`, the latest message: the newest shown one outside Trash, Junk and
/// drafts, else the newest shown one; in a `bin` folder the row's own newest member), its shown
/// messages counted once per Message-ID outside Trash, Junk and drafts (all of them when it has
/// none there), and its drafts. The list index and the
/// filters' partial indexes mirror the message views' (`views::filter_sql`). No foreign keys:
/// a merged or emptied thread keeps its rows until the writer recounts it in the same
/// transaction, so the diff still sees where it was. `fork_folder_counts` counts each folder's
/// rows, and those with unread, starred and attachments: a view's total without walking it.
const FORK1: &str = "
CREATE TABLE IF NOT EXISTS fork_thread_folders(
    thread_id INTEGER NOT NULL,
    folder_id INTEGER NOT NULL,
    messages INTEGER NOT NULL,
    unread INTEGER NOT NULL,
    flagged INTEGER NOT NULL,
    attachments INTEGER NOT NULL,
    last_date INTEGER NOT NULL,
    last_id INTEGER NOT NULL,
    unread_date INTEGER,
    unread_id INTEGER,
    sort_date INTEGER NOT NULL,
    sort_id INTEGER NOT NULL,
    shown INTEGER NOT NULL,
    drafts INTEGER NOT NULL,
    bin INTEGER NOT NULL,
    PRIMARY KEY(thread_id, folder_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS fork_thread_folders_list
    ON fork_thread_folders(folder_id, sort_date, sort_id);
CREATE INDEX IF NOT EXISTS fork_thread_folders_unread
    ON fork_thread_folders(folder_id, sort_date, sort_id) WHERE unread > 0;
CREATE INDEX IF NOT EXISTS fork_thread_folders_flagged
    ON fork_thread_folders(folder_id, sort_date, sort_id) WHERE flagged > 0;
CREATE INDEX IF NOT EXISTS fork_thread_folders_attachments
    ON fork_thread_folders(folder_id, sort_date, sort_id) WHERE attachments > 0;
CREATE TABLE IF NOT EXISTS fork_folder_counts(
    folder_id INTEGER PRIMARY KEY,
    conversations INTEGER NOT NULL,
    unread INTEGER NOT NULL,
    flagged INTEGER NOT NULL,
    attachments INTEGER NOT NULL
);
";

/// The Split Inbox (docs/superhuman/split.md). `fork_message_split` has one row per message:
/// what sorts it, kept from its ingest (the header's `facts`, `HeaderFacts` bits; the server's
/// `category`, NULL where it gave none, 1 Important, 0 Other; the `sender`'s address as
/// `split::sender_key` keeps it), and where the last commit published it (`thread_id`,
/// `folder_id`, `date`, and `split`: 1 Important, 2 Other, 0 while hidden), which the writer's
/// recount keeps as it keeps `fork_thread_folders`. The partial indexes are the message views'
/// walks, and the only ones until the fourth step adds the contact pane's (by sender): every
/// secondary index costs each ingest a write per message, and the recount reaches the rows by
/// message id. `fork_folder_split` counts each folder's Important and Other messages.
/// `fork_sender_split` holds the user's choices by sender ("Always Important": 1, "Always
/// Other": 0), the user's and not an account's, like `remote_senders`. The conversation table
/// and its counts are made again with the new columns: `important` counts a row's Important
/// members, and a folder's conversations with and without one. The open that applies the step
/// fills all of it (`rebuild`).
const FORK2: &str = "
CREATE TABLE IF NOT EXISTS fork_message_split(
    id INTEGER PRIMARY KEY,
    facts INTEGER NOT NULL,
    category INTEGER,
    sender TEXT NOT NULL,
    thread_id INTEGER NOT NULL,
    folder_id INTEGER NOT NULL,
    date INTEGER NOT NULL,
    split INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS fork_message_split_important
    ON fork_message_split(folder_id, date, id) WHERE split = 1;
CREATE INDEX IF NOT EXISTS fork_message_split_other
    ON fork_message_split(folder_id, date, id) WHERE split = 2;
CREATE TABLE IF NOT EXISTS fork_folder_split(
    folder_id INTEGER PRIMARY KEY,
    important INTEGER NOT NULL,
    other INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS fork_sender_split(
    address TEXT PRIMARY KEY,
    important INTEGER NOT NULL,
    added INTEGER NOT NULL
) WITHOUT ROWID;
DROP TABLE IF EXISTS fork_thread_folders;
CREATE TABLE fork_thread_folders(
    thread_id INTEGER NOT NULL,
    folder_id INTEGER NOT NULL,
    messages INTEGER NOT NULL,
    unread INTEGER NOT NULL,
    flagged INTEGER NOT NULL,
    attachments INTEGER NOT NULL,
    last_date INTEGER NOT NULL,
    last_id INTEGER NOT NULL,
    unread_date INTEGER,
    unread_id INTEGER,
    sort_date INTEGER NOT NULL,
    sort_id INTEGER NOT NULL,
    shown INTEGER NOT NULL,
    drafts INTEGER NOT NULL,
    bin INTEGER NOT NULL,
    important INTEGER NOT NULL,
    PRIMARY KEY(thread_id, folder_id)
) WITHOUT ROWID;
CREATE INDEX fork_thread_folders_list
    ON fork_thread_folders(folder_id, sort_date, sort_id);
CREATE INDEX fork_thread_folders_unread
    ON fork_thread_folders(folder_id, sort_date, sort_id) WHERE unread > 0;
CREATE INDEX fork_thread_folders_flagged
    ON fork_thread_folders(folder_id, sort_date, sort_id) WHERE flagged > 0;
CREATE INDEX fork_thread_folders_attachments
    ON fork_thread_folders(folder_id, sort_date, sort_id) WHERE attachments > 0;
CREATE INDEX fork_thread_folders_important
    ON fork_thread_folders(folder_id, sort_date, sort_id) WHERE important > 0;
CREATE INDEX fork_thread_folders_other
    ON fork_thread_folders(folder_id, sort_date, sort_id) WHERE important = 0;
DROP TABLE IF EXISTS fork_folder_counts;
CREATE TABLE fork_folder_counts(
    folder_id INTEGER PRIMARY KEY,
    conversations INTEGER NOT NULL,
    unread INTEGER NOT NULL,
    flagged INTEGER NOT NULL,
    attachments INTEGER NOT NULL,
    important INTEGER NOT NULL,
    other INTEGER NOT NULL
);
";

/// Remind Me (docs/superhuman/remind.md). `fork_reminders` has one row per reminder: the
/// account, the conversation's thread (`thread_id`, kept through merges, which finds it only
/// when its messages have no Message-ID; the others are found by `fork_reminder_keys`, their
/// messages' Message-IDs then, which outlive thread merges and a thread made anew when its
/// messages come back from a move the store couldn't place), the Reminders folder
/// (`folder_id`) and the folder its messages left (`origin`), when it is due (`wake_at`, Unix
/// seconds), its state (`remind::ReminderState`: 0 waiting, 1 reminded, 2 ended), when it came
/// back (`reminded_at`), the op batch that set it (Undo deletes it) and the one that ended it
/// with the state it ended from (Undo restores it). Thread, folder and batch ids are rowids,
/// which a later row may take once theirs is deleted, so a reminder lets go of each with its
/// row (`ON DELETE SET NULL`): a refused or deleted Reminders folder holds none of its
/// reminders, and an Undo of a later batch that took a pruned batch's id leaves them alone.
/// (`origin` only orders the account's Inboxes.) `fork_folder_creates` holds the Reminders
/// folders the store made for a server account before the server has them: the engine creates
/// them before the first move into one, and the folder list keeps them meanwhile. The step
/// changes no derived table, so the open that applies it recounts nothing.
const FORK3: &str = "
CREATE TABLE IF NOT EXISTS fork_reminders(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    thread_id INTEGER REFERENCES threads(id) ON DELETE SET NULL,
    folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    origin INTEGER NOT NULL,
    wake_at INTEGER NOT NULL,
    state INTEGER NOT NULL,
    reminded_at INTEGER,
    batch INTEGER REFERENCES op_batches(id) ON DELETE SET NULL,
    ended INTEGER REFERENCES op_batches(id) ON DELETE SET NULL,
    ended_from INTEGER,
    created INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS fork_reminders_waiting ON fork_reminders(wake_at) WHERE state = 0;
CREATE INDEX IF NOT EXISTS fork_reminders_thread ON fork_reminders(thread_id);
CREATE INDEX IF NOT EXISTS fork_reminders_folder ON fork_reminders(folder_id);
CREATE INDEX IF NOT EXISTS fork_reminders_batch ON fork_reminders(batch);
CREATE INDEX IF NOT EXISTS fork_reminders_ended ON fork_reminders(ended);
CREATE TABLE IF NOT EXISTS fork_reminder_keys(
    reminder INTEGER NOT NULL REFERENCES fork_reminders(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    PRIMARY KEY(reminder, message_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS fork_reminder_keys_message ON fork_reminder_keys(message_id);
CREATE TABLE IF NOT EXISTS fork_folder_creates(
    folder_id INTEGER PRIMARY KEY REFERENCES folders(id) ON DELETE CASCADE
);
";

/// The contact pane (docs/superhuman/contact.md): the shown messages' split rows by sender, as
/// `split::sender_key` keeps the address, then date, with the conversation, the folder and the
/// split in the index (SQLite reads a partial index's own condition from the row unless its
/// column is there). A person's latest conversations are then a walk of a few entries from
/// the end of their range, and their messages a count of that range, neither reading a table
/// row (its bench is `sealwax-fixtures bench-contact`). The writer's recount keeps it as it
/// keeps the split rows; the step changes no derived table, so the open that applies it builds
/// the index and recounts nothing. Builds made while the second step was written left an index
/// `fork_message_split_sender` on `sender` alone in their stores: the step drops it (each
/// ingest paid a write for it) and names its own index anew, which an older one of the same
/// name can't stand in for.
const FORK4: &str = "
DROP INDEX IF EXISTS fork_message_split_sender;
CREATE INDEX IF NOT EXISTS fork_message_split_by_sender
    ON fork_message_split(sender, date, thread_id, folder_id, split) WHERE split <> 0;
";

/// The reminded conversations' threads and when each came back (the latest, should two
/// reminders find one thread): through their Message-IDs, any stored message of the account
/// with one of them, hidden or not; a conversation without Message-IDs through its thread.
/// Two common table expressions, `reminded` and `bumps(t, at)`; the recounts read `bumps`.
macro_rules! bumps {
    () => {
        "reminded AS (
    SELECT m.thread_id AS t, r.reminded_at AS at
    FROM fork_reminders r JOIN fork_reminder_keys k ON k.reminder = r.id
        JOIN messages m ON m.message_id = k.message_id
        JOIN folders x ON x.id = m.folder_id AND x.account_id = r.account_id
    WHERE r.state = 1
    UNION ALL
    SELECT r.thread_id, r.reminded_at FROM fork_reminders r
    WHERE r.state = 1 AND NOT EXISTS (SELECT 1 FROM fork_reminder_keys k WHERE k.reminder = r.id)
),
bumps AS (SELECT t, max(at) AS at FROM reminded GROUP BY t)"
    };
}

/// The waiting conversations' threads, the Reminders folder each waits in and when it is due
/// (the soonest, should two reminders find one thread and folder), by `bumps!`'s rules: through
/// a message of the account, hidden or not, with one of its Message-IDs, or its thread without
/// any. A common table expression `waits(t, f, at)`; the recounts read it (phase C).
macro_rules! waits {
    () => {
        "waiting AS (
    SELECT m.thread_id AS t, r.folder_id AS f, r.wake_at AS at
    FROM fork_reminders r JOIN fork_reminder_keys k ON k.reminder = r.id
        JOIN messages m ON m.message_id = k.message_id
        JOIN folders x ON x.id = m.folder_id AND x.account_id = r.account_id
    WHERE r.state = 0 AND r.folder_id IS NOT NULL
    UNION ALL
    SELECT r.thread_id, r.folder_id, r.wake_at FROM fork_reminders r
    WHERE r.state = 0 AND r.folder_id IS NOT NULL
        AND NOT EXISTS (SELECT 1 FROM fork_reminder_keys k WHERE k.reminder = r.id)
),
waits AS (SELECT t, f, min(at) AS at FROM waiting GROUP BY t, f)"
    };
}

/// `remind::wait_key` in SQL, for a wake time `w.at`: `WAIT_BASE + SOMEDAY` less the time held
/// between 0 and `SOMEDAY` (a test keeps the literal and the domain's constants together).
macro_rules! wait_key_sql {
    () => {
        "1352913928575 - max(0, min(w.at, 253402300799))"
    };
}

/// When the dirty threads' conversations came back, those reminded (the latest, should two
/// reminders find one thread), by `bumps!`'s rules: through a message of the thread, hidden or
/// not, whose Message-ID is a reminder's key, or the thread of a reminder without keys. Its
/// cost follows the dirty threads' messages, not the reminders kept: the writer reads it in
/// every recount while any reminder is reminded.
const DIRTY_BUMPS: &str = "
SELECT m.thread_id, max(r.reminded_at)
FROM fork_dirty d CROSS JOIN messages m INDEXED BY messages_thread ON m.thread_id = d.thread_id
    JOIN fork_reminder_keys k ON k.message_id = m.message_id
    JOIN fork_reminders r ON r.id = k.reminder
    JOIN folders x ON x.id = m.folder_id AND x.account_id = r.account_id
WHERE r.state = 1
GROUP BY m.thread_id
UNION ALL
SELECT r.thread_id, r.reminded_at FROM fork_reminders r
WHERE r.state = 1 AND r.thread_id IN (SELECT thread_id FROM fork_dirty)
    AND NOT EXISTS (SELECT 1 FROM fork_reminder_keys k WHERE k.reminder = r.id)";

/// Where the dirty threads' waiting conversations wait and when each is due, by `waits!`'s
/// rules (phase C): the writer reads it in every recount while any reminder waits, at a cost
/// that follows the dirty threads' messages.
const DIRTY_WAITS: &str = "
SELECT m.thread_id, r.folder_id, min(r.wake_at)
FROM fork_dirty d CROSS JOIN messages m INDEXED BY messages_thread ON m.thread_id = d.thread_id
    JOIN fork_reminder_keys k ON k.message_id = m.message_id
    JOIN fork_reminders r ON r.id = k.reminder
    JOIN folders x ON x.id = m.folder_id AND x.account_id = r.account_id
WHERE r.state = 0 AND r.folder_id IS NOT NULL
GROUP BY m.thread_id, r.folder_id
UNION ALL
SELECT r.thread_id, r.folder_id, r.wake_at FROM fork_reminders r
WHERE r.state = 0 AND r.folder_id IS NOT NULL AND r.thread_id IN (SELECT thread_id FROM fork_dirty)
    AND NOT EXISTS (SELECT 1 FROM fork_reminder_keys k WHERE k.reminder = r.id)";

/// `fork_message_split.split` of a message a pending op hides, or one flagged `\Deleted`.
pub(crate) const HIDDEN: u8 = 0;
pub(crate) const IMPORTANT: u8 = 1;
pub(crate) const OTHER: u8 = 2;

/// The `split` code of a side.
pub(crate) fn code(split: Split) -> u8 {
    match split {
        Split::Important => IMPORTANT,
        Split::Other => OTHER,
    }
}

/// The columns of a `fork_thread_folders` row, in table order.
pub(crate) const COLUMNS: &str = "thread_id, folder_id, messages, unread, flagged, \
    attachments, last_date, last_id, unread_date, unread_id, sort_date, sort_id, shown, drafts, \
    bin, important";

/// The rows of `fork_thread_folders` recounted from `messages`, for the threads `$source`
/// reaches. Shown rows only. A row is "aside" in a Trash, Junk or Drafts folder or flagged
/// `\Draft`; a conversation's latest message is its newest row not aside, else its newest row
/// (`latest`), and its shown count counts the rows of that tier once per Message-ID. Folder
/// rows: the newest member (`newest`), the oldest unread one (`oldest` within the unread rows),
/// and the members `fork_message_split` publishes as Important. An Inbox row of a reminded
/// conversation sorts by when it came back, while that is later (`bumps`); the row of a waiting
/// conversation in its Reminders folder by when it is due (`waits`, `remind::wait_key`).
macro_rules! recount {
    ($source:literal) => {
        concat!(
            "WITH ",
            bumps!(),
            ",
",
            waits!(),
            ",
r AS (
    SELECT m.thread_id AS t, m.folder_id AS f, m.id AS id, m.date AS date, m.flags AS flags,
        m.has_attachments AS att, m.message_id AS mid,
        o.role = 'inbox' AS inbox,
        o.role IN ('trash', 'junk', 'drafts') AS bin,
        (o.role IN ('trash', 'junk', 'drafts') OR m.flags & 16 <> 0) AS aside,
        (o.role = 'drafts' OR m.flags & 16 <> 0) AS draft,
        s.split = 1 AS imp
    FROM ",
            $source,
            " JOIN folders o ON o.id = m.folder_id
    JOIN fork_message_split s ON s.id = m.id
    WHERE m.flags & 264 = 0
),
w AS (
    SELECT *,
        row_number() OVER (PARTITION BY t ORDER BY aside, date DESC, id DESC) AS latest,
        row_number() OVER (PARTITION BY t, f ORDER BY date DESC, id DESC) AS newest,
        row_number() OVER (PARTITION BY t, f, flags & 1 ORDER BY date, id) AS oldest,
        min(aside) OVER (PARTITION BY t) AS tier
    FROM r
),
c AS (
    SELECT t,
        max(CASE WHEN latest = 1 THEN date END) AS date,
        max(CASE WHEN latest = 1 THEN id END) AS id,
        count(DISTINCT CASE WHEN aside = tier THEN mid END)
            + sum(aside = tier AND mid IS NULL) AS shown,
        sum(draft) AS drafts
    FROM w GROUP BY t
),
g AS (
    SELECT t, f, max(bin) AS bin, max(inbox) AS inbox, count(*) AS messages,
        sum(flags & 1 = 0) AS unread,
        sum(flags & 4 <> 0) AS flagged, sum(att <> 0) AS attachments,
        max(CASE WHEN newest = 1 THEN date END) AS last_date,
        max(CASE WHEN newest = 1 THEN id END) AS last_id,
        max(CASE WHEN flags & 1 = 0 AND oldest = 1 THEN date END) AS unread_date,
        max(CASE WHEN flags & 1 = 0 AND oldest = 1 THEN id END) AS unread_id,
        sum(imp) AS important
    FROM w GROUP BY t, f
)
SELECT g.t, g.f, g.messages, g.unread, g.flagged, g.attachments, g.last_date, g.last_id,
    g.unread_date, g.unread_id,
    CASE WHEN g.bin THEN g.last_date WHEN w.at IS NOT NULL THEN ",
            wait_key_sql!(),
            "
        WHEN g.inbox AND b.at > c.date THEN b.at ELSE c.date END,
    CASE WHEN g.bin THEN g.last_id ELSE c.id END,
    c.shown, c.drafts, g.bin, g.important
FROM g JOIN c ON c.t = g.t LEFT JOIN bumps b ON b.t = g.t
    LEFT JOIN waits w ON w.t = g.t AND w.f = g.f"
        )
    };
}

/// Every thread's rows: the migration's and the check's recount. The writer recounts its
/// dirty threads in Rust (`recount_dirty`, the same rules), which costs a tenth of this on a
/// batch's few hundred rows, and `check` compares the two.
pub(crate) const RECOUNT_ALL: &str = recount!("messages m");

/// Where every message's split row puts it, recounted from `messages` and the kept facts,
/// categories and choices by `split::classify_reminded`'s rules: hidden (0); Important (1) in a
/// conversation back from Reminders (`bumps`, phase C); the user's choice for its sender;
/// Important in a thread with a shown message in a Sent folder that isn't a draft; the
/// server's category; Other (2) with any header fact, else Important. The check's
/// formulation; the writer's is `recount_dirty`, the rebuild's `split_rows`.
pub(crate) const SPLIT_ALL: &str = concat!(
    "
WITH sent AS (
    SELECT DISTINCT m.thread_id AS t FROM messages m JOIN folders o ON o.id = m.folder_id
    WHERE o.role = 'sent' AND m.flags & 280 = 0
),
",
    bumps!(),
    "
SELECT s.id AS id, m.thread_id AS thread_id, m.folder_id AS folder_id, m.date AS date,
    CASE WHEN m.flags & 264 <> 0 THEN 0
        WHEN b.t IS NOT NULL THEN 1
        WHEN c.important IS NOT NULL THEN 2 - c.important
        WHEN sent.t IS NOT NULL THEN 1
        WHEN s.category IS NOT NULL THEN 2 - s.category
        WHEN s.facts <> 0 THEN 2
        ELSE 1 END AS split
FROM fork_message_split s JOIN messages m ON m.id = s.id
LEFT JOIN fork_sender_split c ON c.address = s.sender
LEFT JOIN sent ON sent.t = m.thread_id
LEFT JOIN bumps b ON b.t = m.thread_id"
);

/// The writer's TEMP schema: the threads its transaction changed (`content` when more than
/// read and starred flags changed: a row came, went, moved, was hidden or shown again (256, 8)
/// or became a draft or not (16), or what a row shows of it changed: any of these can change a
/// conversation's participants without changing its counts), the folders whose members' read
/// and starred flags changed, the facts the transaction's new messages came with and the
/// messages it deleted (the recount writes and drops their split rows, so that until then the
/// split table holds what the last commit published), and the triggers that fill all but the
/// facts. A cascade from a folder or an account fires the delete trigger for each message.
/// `fork_reminders_touched` notes that a reminder that counts was set, changed or let go of
/// (phase C): the commit's change event says so (`ChangeEvent::reminders`), and the app reads
/// the reminders again. Its triggers insert only into an empty table rather than `OR IGNORE`:
/// a foreign key's action (a folder's delete setting `folder_id` NULL) fires them under the
/// outer statement's conflict policy, which overrides theirs. Only the writer writes messages,
/// so readers need none of this.
const TEMP: &str = "
CREATE TEMP TABLE IF NOT EXISTS fork_dirty(
    thread_id INTEGER PRIMARY KEY,
    content INTEGER NOT NULL
);
CREATE TEMP TABLE IF NOT EXISTS fork_flagged(
    thread_id INTEGER NOT NULL,
    folder_id INTEGER NOT NULL,
    PRIMARY KEY(thread_id, folder_id)
) WITHOUT ROWID;
CREATE TEMP TABLE IF NOT EXISTS fork_new_facts(
    id INTEGER PRIMARY KEY,
    facts INTEGER NOT NULL,
    category INTEGER,
    sender TEXT NOT NULL
);
CREATE TEMP TABLE IF NOT EXISTS fork_gone(id INTEGER PRIMARY KEY);
CREATE TEMP TRIGGER IF NOT EXISTS fork_message_added AFTER INSERT ON main.messages
BEGIN
    INSERT OR REPLACE INTO fork_dirty(thread_id, content) VALUES (new.thread_id, 1);
END;
CREATE TEMP TRIGGER IF NOT EXISTS fork_message_removed AFTER DELETE ON main.messages
BEGIN
    INSERT OR REPLACE INTO fork_dirty(thread_id, content) VALUES (old.thread_id, 1);
    INSERT OR IGNORE INTO fork_gone(id) VALUES (old.id);
END;
CREATE TEMP TRIGGER IF NOT EXISTS fork_message_flags AFTER UPDATE OF flags ON main.messages
WHEN old.flags <> new.flags AND old.flags & 280 = new.flags & 280
BEGIN
    INSERT OR IGNORE INTO fork_dirty(thread_id, content) VALUES (new.thread_id, 0);
    INSERT OR IGNORE INTO fork_flagged(thread_id, folder_id) VALUES (new.thread_id, new.folder_id);
END;
CREATE TEMP TRIGGER IF NOT EXISTS fork_message_shown AFTER UPDATE OF flags ON main.messages
WHEN old.flags & 280 <> new.flags & 280
BEGIN
    INSERT OR REPLACE INTO fork_dirty(thread_id, content) VALUES (new.thread_id, 1);
END;
CREATE TEMP TRIGGER IF NOT EXISTS fork_message_changed AFTER UPDATE OF thread_id, folder_id,
    date, has_attachments, snippet, subject, from_name, from_addr, message_id ON main.messages
WHEN old.thread_id IS NOT new.thread_id OR old.folder_id IS NOT new.folder_id
    OR old.date IS NOT new.date OR old.has_attachments IS NOT new.has_attachments
    OR old.snippet IS NOT new.snippet OR old.subject IS NOT new.subject
    OR old.from_name IS NOT new.from_name OR old.from_addr IS NOT new.from_addr
    OR old.message_id IS NOT new.message_id
BEGIN
    INSERT OR REPLACE INTO fork_dirty(thread_id, content) VALUES (old.thread_id, 1);
    INSERT OR REPLACE INTO fork_dirty(thread_id, content) VALUES (new.thread_id, 1);
END;
CREATE TEMP TRIGGER IF NOT EXISTS fork_folder_role AFTER UPDATE OF role ON main.folders
WHEN old.role IS NOT new.role
BEGIN
    INSERT OR REPLACE INTO fork_dirty(thread_id, content)
        SELECT DISTINCT thread_id, 1 FROM messages WHERE folder_id = new.id;
END;
CREATE TEMP TABLE IF NOT EXISTS fork_reminders_touched(one INTEGER PRIMARY KEY);
CREATE TEMP TRIGGER IF NOT EXISTS fork_reminder_set AFTER INSERT ON main.fork_reminders
BEGIN
    INSERT INTO fork_reminders_touched(one)
        SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM fork_reminders_touched);
END;
CREATE TEMP TRIGGER IF NOT EXISTS fork_reminder_changed AFTER UPDATE ON main.fork_reminders
WHEN old.state IS NOT new.state OR old.wake_at IS NOT new.wake_at
    OR old.reminded_at IS NOT new.reminded_at OR old.folder_id IS NOT new.folder_id
BEGIN
    INSERT INTO fork_reminders_touched(one)
        SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM fork_reminders_touched);
END;
CREATE TEMP TRIGGER IF NOT EXISTS fork_reminder_gone AFTER DELETE ON main.fork_reminders
WHEN old.state < 2
BEGIN
    INSERT INTO fork_reminders_touched(one)
        SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM fork_reminders_touched);
END;
";

fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<i64>> {
    conn.prepare_cached("SELECT value FROM meta WHERE key = ?1")?
        .query_row([key], |row| row.get(0))
        .optional()
}

fn set_meta(conn: &Connection, key: &str, value: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?
    .execute(params![key, value])?;
    Ok(())
}

/// Notes in `meta` that work under `key` is still to come (`true`), or that it is done.
pub(crate) fn set_pending(conn: &Connection, key: &str, pending: bool) -> rusqlite::Result<()> {
    if pending {
        set_meta(conn, key, 1)
    } else {
        conn.prepare_cached("DELETE FROM meta WHERE key = ?1")?
            .execute([key])?;
        Ok(())
    }
}

fn store_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.prepare_cached("SELECT version FROM store_version")?
        .query_row([], |row| row.get(0))
}

/// The fork's steps on the writer's connection, after upstream's migrations: applies the
/// missing ones, recounts the derived tables when a step that changes them or an upstream
/// migration just ran or an older build wrote since they were last exact, and installs the
/// TEMP schema that keeps them so. Such a step leaves the store marked inexact in its own
/// transaction, so the recount runs even if Pronto stops between the two.
pub(crate) fn open(conn: &mut Connection, upstream_migrated: bool) -> Result<(), StoreError> {
    let current = meta(conn, "fork_schema")?.unwrap_or(0);
    if current > VERSION {
        return Err(StoreError::NewerForkSchema {
            found: current,
            supported: VERSION,
        });
    }
    for (index, (sql, recount)) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = index as i64 + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        set_meta(&tx, "fork_schema", version)?;
        if *recount {
            tx.execute("DELETE FROM meta WHERE key = 'fork_through'", [])?;
        }
        tx.commit()?;
    }
    let tx = conn.transaction()?;
    let exact = meta(&tx, "fork_through")? == Some(store_version(&tx)?);
    // A choice for a sender was still classifying its mail again when Pronto stopped
    // (`writer/split.rs`): some of its conversations may be on the wrong side.
    let pending = meta(&tx, crate::store::writer::split::PENDING)?.is_some();
    if upstream_migrated || !exact || pending {
        let started = std::time::Instant::now();
        rebuild(&tx)?;
        tracing::info!(
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "conversation table recounted"
        );
        set_pending(&tx, crate::store::writer::split::PENDING, false)?;
    }
    tx.commit()?;
    conn.execute_batch(TEMP)?;
    Ok(())
}

/// Each folder's rows, and those with unread, starred, attachments and Important members, and
/// those without an Important member.
const FOLDER_COUNTS: &str = "SELECT folder_id, count(*), sum(unread > 0), sum(flagged > 0), \
    sum(attachments > 0), sum(important > 0), sum(important = 0) FROM fork_thread_folders";

/// Each folder's Important and Other messages.
const SPLIT_COUNTS: &str =
    "SELECT folder_id, sum(split = 1), sum(split = 2) FROM fork_message_split";

/// Recounts the derived tables and marks them exact at the current store version: every
/// message's split row (`split_rows`, then `SPLIT_ALL`), the conversation table and the
/// folders' counts.
pub(crate) fn rebuild(tx: &Connection) -> rusqlite::Result<()> {
    split_rows(tx)?;
    tx.execute("DELETE FROM fork_thread_folders", [])?;
    tx.execute(
        &format!("INSERT INTO fork_thread_folders({COLUMNS}) {RECOUNT_ALL}"),
        [],
    )?;
    tx.execute("DELETE FROM fork_folder_counts", [])?;
    tx.execute(
        &format!("INSERT INTO fork_folder_counts {FOLDER_COUNTS} GROUP BY folder_id"),
        [],
    )?;
    tx.execute("DELETE FROM fork_folder_split", [])?;
    tx.execute(
        &format!("INSERT INTO fork_folder_split {SPLIT_COUNTS} GROUP BY folder_id"),
        [],
    )?;
    set_meta(tx, "fork_through", store_version(tx)?)
}

/// Gives every message its split row, as the rebuild finds the store, in one pass by the rules
/// of `write_members` (`SPLIT_ALL` is the check's formulation). A row that still describes its
/// message (same sender, same date) keeps its facts and category; a message stored before the
/// fork read them (this step's backfill) or by an older build since, and a row id an older
/// build gave another message, get what the store kept of the header (`HeaderFacts::stored`:
/// its List-Id and sender) and no category. Rows of messages gone go.
fn split_rows(tx: &Connection) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM fork_message_split \
         WHERE NOT EXISTS (SELECT 1 FROM messages m WHERE m.id = fork_message_split.id)",
        [],
    )?;
    let sent: std::collections::HashSet<i64> = tx
        .prepare(
            "SELECT DISTINCT m.thread_id FROM messages m JOIN folders o ON o.id = m.folder_id \
             WHERE o.role = 'sent' AND m.flags & 280 = 0",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // Conversations back from Reminders are Important (phase C).
    let reminded: std::collections::HashSet<i64> = tx
        .prepare(concat!("WITH ", bumps!(), " SELECT t FROM bumps"))?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let choices = choices(tx)?;
    let mut select = tx.prepare(
        "SELECT m.id, m.thread_id, m.folder_id, m.date, m.flags, m.list_id IS NOT NULL, \
           m.from_addr, s.id IS NOT NULL, s.facts, s.category, s.sender, s.thread_id, \
           s.folder_id, s.date, s.split \
         FROM messages m LEFT JOIN fork_message_split s ON s.id = m.id",
    )?;
    let mut write = tx.prepare(
        "INSERT OR REPLACE INTO fork_message_split(id, facts, category, sender, thread_id, \
           folder_id, date, split) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    let mut rows = select.query([])?;
    while let Some(row) = rows.next()? {
        let (id, thread, folder, date, flags): (i64, i64, i64, i64, u32) = (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        );
        let listed: bool = row.get(5)?;
        let from: String = row.get(6)?;
        let address = sender(&from);
        let kept = if row.get::<_, bool>(7)?
            && row.get::<_, String>(10)? == address
            && row.get::<_, i64>(13)? == date
        {
            Some((
                HeaderFacts::from_bits(row.get(8)?),
                row.get::<_, Option<bool>>(9)?.map(Split::of),
                Published {
                    thread: row.get(11)?,
                    folder: row.get(12)?,
                    date: row.get(13)?,
                    split: row.get(14)?,
                },
            ))
        } else {
            None
        };
        let (facts, category) = kept.as_ref().map_or_else(
            || (HeaderFacts::stored(listed, &from), None),
            |(facts, category, _)| (*facts, *category),
        );
        let split = if flags & 264 != 0 {
            HIDDEN
        } else {
            code(split::classify_reminded(
                reminded.contains(&thread),
                facts,
                category,
                sent.contains(&thread),
                choices.get(&address).copied(),
            ))
        };
        let now = Published {
            thread,
            folder,
            date,
            split,
        };
        if kept.is_none_or(|(_, _, published)| published != now) {
            write.execute(params![
                id,
                facts.bits(),
                category.map(Split::is_important),
                address,
                thread,
                folder,
                date,
                split
            ])?;
        }
    }
    Ok(())
}

/// A sender's address as `fork_message_split.sender` and `fork_sender_split.address` keep it.
pub(crate) fn sender(address: &str) -> String {
    split::sender_key(address).unwrap_or_default()
}

/// Adds the dirty threads' rows to their folders' counts (`sign` 1), or takes them away (-1).
fn count_dirty(tx: &Connection, sign: i64) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT INTO fork_folder_counts(folder_id, conversations, unread, flagged, attachments, \
           important, other) \
         SELECT folder_id, ?1 * count(*), ?1 * sum(unread > 0), ?1 * sum(flagged > 0), \
           ?1 * sum(attachments > 0), ?1 * sum(important > 0), ?1 * sum(important = 0) \
         FROM fork_thread_folders \
         WHERE thread_id IN (SELECT thread_id FROM fork_dirty) GROUP BY folder_id \
         ON CONFLICT(folder_id) DO UPDATE SET \
           conversations = conversations + excluded.conversations, \
           unread = unread + excluded.unread, flagged = flagged + excluded.flagged, \
           attachments = attachments + excluded.attachments, \
           important = important + excluded.important, other = other + excluded.other",
    )?
    .execute([sign])?;
    Ok(())
}

/// Changes to the folders' Important and Other counts, as the recount moves split rows.
#[derive(Default)]
struct SplitDeltas(HashMap<i64, (i64, i64)>);

impl SplitDeltas {
    /// A row on `split` in `folder` came (`sign` 1) or went (-1).
    fn add(&mut self, folder: i64, split: u8, sign: i64) {
        let entry = self.0.entry(folder).or_default();
        match split {
            IMPORTANT => entry.0 += sign,
            OTHER => entry.1 += sign,
            _ => {}
        }
    }

    fn apply(self, tx: &Connection) -> rusqlite::Result<()> {
        let mut upsert = tx.prepare_cached(
            "INSERT INTO fork_folder_split(folder_id, important, other) VALUES (?1, ?2, ?3) \
             ON CONFLICT(folder_id) DO UPDATE SET \
               important = important + excluded.important, other = other + excluded.other",
        )?;
        for (folder, (important, other)) in self.0 {
            if important != 0 || other != 0 {
                upsert.execute(params![folder, important, other])?;
            }
        }
        Ok(())
    }
}

/// Marks the table exact at `version`: the writer calls it in each commit that bumps the store
/// version, after the commit's threads were recounted.
pub(crate) fn mark_through(tx: &Connection, version: u64) -> rusqlite::Result<()> {
    set_meta(tx, "fork_through", version as i64)
}

/// The threads the transaction changed, with whether anything but shown flags changed.
pub(crate) fn dirty(tx: &Connection) -> rusqlite::Result<Vec<(ThreadId, bool)>> {
    tx.prepare_cached("SELECT thread_id, content FROM fork_dirty ORDER BY thread_id")?
        .query_map([], |row| Ok((ThreadId(row.get(0)?), row.get(1)?)))?
        .collect()
}

/// The threads and folders where a member's shown flags changed.
pub(crate) fn flagged(tx: &Connection) -> rusqlite::Result<BTreeSet<(ThreadId, FolderId)>> {
    tx.prepare_cached("SELECT thread_id, folder_id FROM fork_flagged")?
        .query_map([], |row| Ok((ThreadId(row.get(0)?), FolderId(row.get(1)?))))?
        .collect()
}

/// Notes a new message's facts and category for the recount, which writes its split row:
/// `insert_message` calls it for every message it stores.
pub(crate) fn note_new(
    tx: &Connection,
    id: MessageId,
    facts: HeaderFacts,
    category: Option<Split>,
    from: &str,
) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT OR REPLACE INTO fork_new_facts(id, facts, category, sender) \
         VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![
        id.0,
        facts.bits(),
        category.map(Split::is_important),
        sender(from)
    ])?;
    Ok(())
}

/// Marks `threads` dirty without a content change: their messages are classified again by the
/// next recount (a category or a choice changed), and their rows' sort keys counted again (a
/// reminder started or stopped counting).
pub(crate) fn touch(tx: &Connection, threads: &BTreeSet<ThreadId>) -> rusqlite::Result<()> {
    let mut insert =
        tx.prepare_cached("INSERT OR IGNORE INTO fork_dirty(thread_id, content) VALUES (?1, 0)")?;
    for thread in threads {
        insert.execute([thread.0])?;
    }
    Ok(())
}

/// The user's choices by sender.
pub(crate) fn choices(conn: &Connection) -> rusqlite::Result<HashMap<String, Split>> {
    conn.prepare_cached("SELECT address, important FROM fork_sender_split")?
        .query_map([], |row| Ok((row.get(0)?, Split::of(row.get(1)?))))?
        .collect()
}

/// Recounts the dirty threads' rows (a thread without shown messages loses its rows) and
/// classifies their messages again. The dirty set stays until `clear`.
pub(crate) fn recount_dirty(tx: &Connection) -> rusqlite::Result<()> {
    count_dirty(tx, -1)?;
    tx.prepare_cached(
        "DELETE FROM fork_thread_folders WHERE thread_id IN (SELECT thread_id FROM fork_dirty)",
    )?
    .execute([])?;
    // The split rows of the messages the transaction deleted go first: a message it stored
    // may have taken one's id since.
    let mut deltas = SplitDeltas::default();
    {
        let mut gone = tx.prepare_cached(
            "SELECT s.folder_id, s.split FROM fork_gone g CROSS JOIN fork_message_split s \
               ON s.id = g.id",
        )?;
        let mut rows = gone.query([])?;
        while let Some(row) = rows.next()? {
            deltas.add(row.get(0)?, row.get(1)?, -1);
        }
    }
    tx.prepare_cached("DELETE FROM fork_message_split WHERE id IN (SELECT id FROM fork_gone)")?
        .execute([])?;
    let choices = choices(tx)?;
    let bumps = bumps(tx)?;
    let waits = waits(tx)?;
    // The dirty threads' rows, hidden ones too, a thread's together, with their split rows as
    // published (none for a new message, whose facts wait in `fork_new_facts`). CROSS JOIN
    // keeps the planner on the (unanalysed) dirty table.
    let mut select = tx.prepare_cached(
        "SELECT m.thread_id, m.folder_id, m.id, m.date, m.flags, m.has_attachments, \
           m.message_id, o.role, m.list_id IS NOT NULL, m.from_addr, \
           s.id IS NOT NULL, s.facts, s.category, s.sender, s.thread_id, s.folder_id, s.date, \
           s.split, n.id IS NOT NULL, n.facts, n.category, n.sender \
         FROM fork_dirty d CROSS JOIN messages m INDEXED BY messages_thread \
           ON m.thread_id = d.thread_id JOIN folders o ON o.id = m.folder_id \
           LEFT JOIN fork_message_split s ON s.id = m.id \
           LEFT JOIN fork_new_facts n ON n.id = m.id \
         ORDER BY m.thread_id",
    )?;
    let mut writes = Writes {
        thread: tx.prepare_cached(&format!(
            "INSERT INTO fork_thread_folders({COLUMNS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
        ))?,
        new_split: tx.prepare_cached(
            "INSERT OR REPLACE INTO fork_message_split(id, facts, category, sender, thread_id, \
               folder_id, date, split) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?,
        split: tx.prepare_cached(
            "UPDATE fork_message_split SET thread_id = ?2, folder_id = ?3, date = ?4, split = ?5 \
             WHERE id = ?1",
        )?,
    };
    let mut thread: Vec<Member> = Vec::new();
    let mut rows = select.query([])?;
    while let Some(row) = rows.next()? {
        let role: String = row.get(7)?;
        let shown = Shown {
            thread: row.get(0)?,
            folder: row.get(1)?,
            id: row.get(2)?,
            date: row.get(3)?,
            flags: row.get(4)?,
            attachments: row.get(5)?,
            message_id: row.get(6)?,
            bin: matches!(role.as_str(), "trash" | "junk" | "drafts"),
            drafts_folder: role == "drafts",
            inbox: role == "inbox",
            important: false,
        };
        let published = if row.get::<_, bool>(10)? {
            Some(Published {
                thread: row.get(14)?,
                folder: row.get(15)?,
                date: row.get(16)?,
                split: row.get(17)?,
            })
        } else {
            None
        };
        // A message this transaction stored is new, even where a message it deleted left a
        // split row under the same id.
        let new = row.get::<_, bool>(18)?;
        let kept = if new {
            Kept {
                facts: HeaderFacts::from_bits(row.get(19)?),
                category: row.get::<_, Option<bool>>(20)?.map(Split::of),
                sender: row.get(21)?,
            }
        } else if row.get::<_, bool>(10)? {
            Kept {
                facts: HeaderFacts::from_bits(row.get(11)?),
                category: row.get::<_, Option<bool>>(12)?.map(Split::of),
                sender: row.get(13)?,
            }
        } else {
            // A message no ingest noted: what the store kept of its header.
            let from: String = row.get(9)?;
            Kept {
                facts: HeaderFacts::stored(row.get(8)?, &from),
                category: None,
                sender: sender(&from),
            }
        };
        let member = Member {
            sent: role == "sent",
            shown,
            published,
            new,
            kept,
        };
        if thread
            .last()
            .is_some_and(|last: &Member| last.shown.thread != member.shown.thread)
        {
            write_members(
                &mut writes,
                &mut thread,
                &choices,
                &bumps,
                &waits,
                &mut deltas,
            )?;
            thread.clear();
        }
        thread.push(member);
    }
    if !thread.is_empty() {
        write_members(
            &mut writes,
            &mut thread,
            &choices,
            &bumps,
            &waits,
            &mut deltas,
        )?;
    }
    count_dirty(tx, 1)?;
    deltas.apply(tx)
}

/// A message of a dirty thread, as `recount_dirty` reads it; a hidden one counts in none of
/// the thread's rows.
struct Shown {
    thread: i64,
    folder: i64,
    id: i64,
    date: i64,
    flags: u32,
    attachments: bool,
    message_id: Option<String>,
    /// In a Trash, Junk or Drafts folder.
    bin: bool,
    drafts_folder: bool,
    /// In an Inbox: where a reminded conversation sorts by when it came back.
    inbox: bool,
    /// Classified Important.
    important: bool,
}

impl Shown {
    fn aside(&self) -> bool {
        self.bin || self.flags & 16 != 0
    }

    fn key(&self) -> (i64, i64) {
        (self.date, self.id)
    }

    fn hidden(&self) -> bool {
        self.flags & 264 != 0
    }
}

/// Where a message's split row placed it in the last commit.
#[derive(PartialEq, Eq)]
struct Published {
    thread: i64,
    folder: i64,
    date: i64,
    split: u8,
}

/// What sorts a message, kept from its ingest.
struct Kept {
    facts: HeaderFacts,
    category: Option<Split>,
    sender: String,
}

/// A message of a dirty thread, shown or hidden, with its split row.
struct Member {
    shown: Shown,
    /// In a Sent folder.
    sent: bool,
    /// `None` for a message the last commit didn't have.
    published: Option<Published>,
    /// Stored by this transaction: its split row is written anew.
    new: bool,
    kept: Kept,
}

struct Writes<'c> {
    thread: rusqlite::CachedStatement<'c>,
    new_split: rusqlite::CachedStatement<'c>,
    split: rusqlite::CachedStatement<'c>,
}

/// When the dirty threads' reminded conversations came back (`DIRTY_BUMPS`), read once per
/// recount; nothing to read while no reminder is reminded.
fn bumps(tx: &Connection) -> rusqlite::Result<HashMap<i64, i64>> {
    let any: bool = tx
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM fork_reminders WHERE state = 1)")?
        .query_row([], |row| row.get(0))?;
    let mut bumps = HashMap::new();
    if !any {
        return Ok(bumps);
    }
    let mut select = tx.prepare_cached(DIRTY_BUMPS)?;
    let mut rows = select.query([])?;
    while let Some(row) = rows.next()? {
        let (thread, at): (i64, i64) = (row.get(0)?, row.get(1)?);
        let bump = bumps.entry(thread).or_insert(at);
        *bump = (*bump).max(at);
    }
    Ok(bumps)
}

/// Where the dirty threads' waiting conversations wait and when each is due (`DIRTY_WAITS`),
/// by thread and folder, read once per recount; nothing to read while no reminder waits.
fn waits(tx: &Connection) -> rusqlite::Result<HashMap<(i64, i64), i64>> {
    let any: bool = tx
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM fork_reminders WHERE state = 0)")?
        .query_row([], |row| row.get(0))?;
    let mut waits = HashMap::new();
    if !any {
        return Ok(waits);
    }
    let mut select = tx.prepare_cached(DIRTY_WAITS)?;
    let mut rows = select.query([])?;
    while let Some(row) = rows.next()? {
        let (thread, folder, at): (i64, i64, i64) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let wait = waits.entry((thread, folder)).or_insert(at);
        *wait = (*wait).min(at);
    }
    Ok(waits)
}

/// Classifies one thread's messages (`split::classify_reminded`, as `SPLIT_ALL`), writes the
/// split rows that changed, and writes the thread's rows from its shown messages.
fn write_members(
    writes: &mut Writes<'_>,
    members: &mut [Member],
    choices: &HashMap<String, Split>,
    bumps: &HashMap<i64, i64>,
    waits: &HashMap<(i64, i64), i64>,
    deltas: &mut SplitDeltas,
) -> rusqlite::Result<()> {
    let sent_in = members
        .iter()
        .any(|member| member.sent && member.shown.flags & 280 == 0);
    let reminded = members
        .first()
        .is_some_and(|member| bumps.contains_key(&member.shown.thread));
    for member in members.iter_mut() {
        let split = if member.shown.hidden() {
            HIDDEN
        } else {
            code(split::classify_reminded(
                reminded,
                member.kept.facts,
                member.kept.category,
                sent_in,
                choices.get(&member.kept.sender).copied(),
            ))
        };
        member.shown.important = split == IMPORTANT;
        let shown = &member.shown;
        let now = Published {
            thread: shown.thread,
            folder: shown.folder,
            date: shown.date,
            split,
        };
        if let Some(published) = &member.published {
            deltas.add(published.folder, published.split, -1);
        }
        deltas.add(now.folder, now.split, 1);
        match &member.published {
            Some(published) if !member.new => {
                if *published != now {
                    writes.split.execute(params![
                        shown.id, now.thread, now.folder, now.date, now.split
                    ])?;
                }
            }
            _ => {
                writes.new_split.execute(params![
                    shown.id,
                    member.kept.facts.bits(),
                    member.kept.category.map(Split::is_important),
                    member.kept.sender,
                    now.thread,
                    now.folder,
                    now.date,
                    now.split,
                ])?;
            }
        }
    }
    let shown: Vec<&Shown> = members
        .iter()
        .map(|member| &member.shown)
        .filter(|shown| !shown.hidden())
        .collect();
    if shown.is_empty() {
        return Ok(());
    }
    let bump = bumps.get(&shown[0].thread).copied();
    let thread = shown[0].thread;
    write_thread(&mut writes.thread, &shown, bump, |folder| {
        waits.get(&(thread, folder)).copied()
    })
}

/// Writes one thread's rows, by the rules of `recount!`; `bump` is when the conversation came
/// back from Reminders, if it is reminded, and `wait` when it is due back, by folder, where it
/// waits in a Reminders folder.
fn write_thread(
    insert: &mut rusqlite::CachedStatement<'_>,
    rows: &[&Shown],
    bump: Option<i64>,
    wait: impl Fn(i64) -> Option<i64>,
) -> rusqlite::Result<()> {
    let tier: Vec<&Shown> = if rows.iter().any(|row| !row.aside()) {
        rows.iter().copied().filter(|row| !row.aside()).collect()
    } else {
        rows.to_vec()
    };
    let latest = tier.iter().map(|row| row.key()).max().unwrap_or_default();
    // An Inbox row sorts by when the conversation came back, while that is later; it still
    // names the latest message, whose date, sender and snippet the row shows.
    let reminded = (remind::sort_date(latest.0, bump), latest.1);
    let mut ids: Vec<&str> = tier
        .iter()
        .filter_map(|row| row.message_id.as_deref())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let shown = (ids.len() + tier.iter().filter(|row| row.message_id.is_none()).count()) as i64;
    let drafts = rows
        .iter()
        .filter(|row| row.drafts_folder || row.flags & 16 != 0)
        .count() as i64;
    let mut folders: Vec<i64> = rows.iter().map(|row| row.folder).collect();
    folders.sort_unstable();
    folders.dedup();
    for folder in folders {
        let members: Vec<&Shown> = rows
            .iter()
            .copied()
            .filter(|row| row.folder == folder)
            .collect();
        let count =
            |test: fn(&Shown) -> bool| members.iter().filter(|row| test(row)).count() as i64;
        let last = members
            .iter()
            .map(|row| row.key())
            .max()
            .unwrap_or_default();
        let first_unread = members
            .iter()
            .filter(|row| row.flags & 1 == 0)
            .map(|row| row.key())
            .min();
        let bin = members[0].bin;
        // Waiting in its Reminders folder, a conversation sorts there by when it is due.
        let sort = if bin {
            last
        } else if let Some(at) = wait(folder) {
            (remind::wait_key(at), latest.1)
        } else if members[0].inbox {
            reminded
        } else {
            latest
        };
        insert.execute(params![
            rows[0].thread,
            folder,
            members.len() as i64,
            count(|row| row.flags & 1 == 0),
            count(|row| row.flags & 4 != 0),
            count(|row| row.attachments),
            last.0,
            last.1,
            first_unread.map(|key| key.0),
            first_unread.map(|key| key.1),
            sort.0,
            sort.1,
            shown,
            drafts,
            bin,
            count(|row| row.important),
        ])?;
    }
    Ok(())
}

pub(crate) fn clear(tx: &Connection) -> rusqlite::Result<()> {
    tx.prepare_cached("DELETE FROM fork_dirty")?.execute([])?;
    tx.prepare_cached("DELETE FROM fork_flagged")?.execute([])?;
    tx.prepare_cached("DELETE FROM fork_new_facts")?
        .execute([])?;
    tx.prepare_cached("DELETE FROM fork_gone")?.execute([])?;
    Ok(())
}

/// Whether the transaction left a thread to recount: a commit then settles it with its diffs.
pub(crate) fn is_dirty(tx: &Connection) -> rusqlite::Result<bool> {
    tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM fork_dirty)")?
        .query_row([], |row| row.get(0))
}

/// Whether the transaction set, changed or let go of a reminder that counts (phase C).
pub(crate) fn reminders_touched(tx: &Connection) -> rusqlite::Result<bool> {
    tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM fork_reminders_touched)")?
        .query_row([], |row| row.get(0))
}

/// [`reminders_touched`], cleared for the next transaction: its commit's change event says so.
pub(crate) fn take_reminders_touched(tx: &Connection) -> rusqlite::Result<bool> {
    let touched = reminders_touched(tx)?;
    if touched {
        tx.prepare_cached("DELETE FROM fork_reminders_touched")?
            .execute([])?;
    }
    Ok(touched)
}

/// Recounts whatever is still dirty, without diffs; replies whether anything was. Every path
/// that changes messages settles with its view diffs before it commits (`view_changes`, the
/// reload paths), so this finds nothing: it keeps the table exact if a path ever doesn't.
pub(crate) fn settle_quietly(tx: &Connection) -> rusqlite::Result<bool> {
    let pending = is_dirty(tx)?;
    if pending {
        recount_dirty(tx)?;
    }
    clear(tx)?;
    Ok(pending)
}

/// What `check` finds wrong with the fork's tables: rows of the conversation table that a
/// recount from `messages` disagrees with, folders whose conversation counts disagree with the
/// table, split rows a recount disagrees with (or that are missing, or name another sender than
/// their message), and folders whose Important and Other counts disagree with them.
pub(crate) struct Disagreements {
    pub(crate) rows: i64,
    pub(crate) folders: i64,
    pub(crate) splits: i64,
    pub(crate) split_counts: i64,
}

pub(crate) fn disagreements(conn: &Connection) -> rusqlite::Result<Disagreements> {
    // Recounted once (about 1 s at 100,000 messages), compared both ways.
    let rows = conn.query_row(
        &format!(
            "WITH recounted AS MATERIALIZED (SELECT * FROM ({RECOUNT_ALL})), \
               stored AS (SELECT {COLUMNS} FROM fork_thread_folders) \
             SELECT (SELECT count(*) FROM (SELECT * FROM recounted EXCEPT SELECT * FROM stored)) \
               + (SELECT count(*) FROM (SELECT * FROM stored EXCEPT SELECT * FROM recounted))"
        ),
        [],
        |row| row.get(0),
    )?;
    let counted = "SELECT folder_id, conversations, unread, flagged, attachments, important, \
        other FROM fork_folder_counts WHERE conversations <> 0 OR unread <> 0 OR flagged <> 0 \
          OR attachments <> 0 OR important <> 0 OR other <> 0";
    let recounted = format!("{FOLDER_COUNTS} GROUP BY folder_id");
    let folders = conn.query_row(
        &format!(
            "SELECT (SELECT count(*) FROM ({counted} EXCEPT {recounted})) \
               + (SELECT count(*) FROM ({recounted} EXCEPT {counted}))"
        ),
        [],
        |row| row.get(0),
    )?;
    let mut splits: i64 = conn.query_row(
        &format!(
            "WITH recounted AS MATERIALIZED (SELECT * FROM ({SPLIT_ALL})), \
               stored AS (SELECT id, thread_id, folder_id, date, split FROM fork_message_split) \
             SELECT (SELECT count(*) FROM (SELECT * FROM recounted EXCEPT SELECT * FROM stored)) \
               + (SELECT count(*) FROM (SELECT * FROM stored EXCEPT SELECT * FROM recounted)) \
               + (SELECT count(*) FROM messages m \
                  WHERE NOT EXISTS (SELECT 1 FROM fork_message_split s WHERE s.id = m.id))"
        ),
        [],
        |row| row.get(0),
    )?;
    let mut select = conn.prepare(
        "SELECT s.sender, m.from_addr FROM fork_message_split s JOIN messages m ON m.id = s.id",
    )?;
    let mut rows_read = select.query([])?;
    while let Some(row) = rows_read.next()? {
        if row.get::<_, String>(0)? != sender(&row.get::<_, String>(1)?) {
            splits += 1;
        }
    }
    let counted = "SELECT folder_id, important, other FROM fork_folder_split \
        WHERE important <> 0 OR other <> 0";
    let recounted =
        format!("{SPLIT_COUNTS} GROUP BY folder_id HAVING sum(split = 1) + sum(split = 2) > 0");
    let split_counts = conn.query_row(
        &format!(
            "SELECT (SELECT count(*) FROM ({counted} EXCEPT {recounted})) \
               + (SELECT count(*) FROM ({recounted} EXCEPT {counted}))"
        ),
        [],
        |row| row.get(0),
    )?;
    Ok(Disagreements {
        rows,
        folders,
        splits,
        split_counts,
    })
}

#[cfg(test)]
mod tests {
    use sealwax_domain::remind::{SOMEDAY, WAIT_BASE, wait_key};

    #[test]
    fn the_recounts_wait_key_is_the_domains() {
        // `wait_key_sql!` writes `WAIT_BASE + SOMEDAY` and `SOMEDAY` out as literals.
        let sql = wait_key_sql!();
        assert!(
            sql.starts_with(&format!("{} - ", WAIT_BASE + SOMEDAY)),
            "{sql}"
        );
        assert!(sql.contains(&format!("min(w.at, {SOMEDAY})")), "{sql}");
        assert_eq!(wait_key(0), WAIT_BASE + SOMEDAY);
    }
}
