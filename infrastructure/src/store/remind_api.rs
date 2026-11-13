//! Remind Me's requests (the fork's Superhuman pass, docs/superhuman/remind.md): a conversation
//! sent to the account's Reminders folder until a time, as one undoable action; the wake that
//! brings due ones back to the Inbox, where each sorts by when it came back until it is acted
//! on; cancelling; and what the engine needs to make a Reminders folder on the server. Every
//! write is one transaction with one exact change event. Times of reminders are Unix seconds
//! (UTC), as message dates; undo windows are Unix milliseconds, as the op queue's.

use super::reader::ReadJob;
use super::writer::WriteJob;
use super::writer::remind::RemindJob;
use super::{BatchId, Queued, Reply, Store, StoreVersion, ViewScope};
use sealwax_domain::model::{AccountId, FolderId, ThreadId};
use sealwax_domain::remind::ReminderState;

/// One reminder: a conversation sent away until a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReminderId(pub i64);

/// Send conversations to Reminders until `wake_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remind {
    /// The conversations, by a row's `thread`.
    pub threads: Vec<ThreadId>,
    /// Their messages in this scope's folders go (a view's: an Inbox, All Inboxes); their
    /// copies in Sent, Drafts, Trash and Junk stay. Any conversation of these with a reminder
    /// waiting gets the new time, and what came to the scope meanwhile goes too.
    pub scope: ViewScope,
    /// When they come back, Unix seconds.
    pub wake_at: i64,
    /// When the undo window ends (Unix ms): the moves don't reach the server before it.
    pub not_before: i64,
}

/// The reply to [`Store::remind`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reminded {
    pub version: StoreVersion,
    /// What Undo takes back: the moves, and the reminders this set; `None` when nothing moved
    /// (a waiting reminder only got a new time, which Undo doesn't take back).
    pub batch: Option<BatchId>,
    /// The reminders set or given a new time, in the order of the threads asked for.
    pub reminders: Vec<ReminderId>,
    /// Messages sent to Reminders.
    pub moved: u32,
    /// Conversations left alone: gone, or without a message in the scope to send.
    pub not_applied: u32,
    /// Server accounts with new ops: the app sends `Command::OpsQueued` for each, and
    /// `Command::RemindersChanged` once.
    pub accounts: Vec<AccountId>,
}

/// A reminder as the store keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    pub id: ReminderId,
    pub account: AccountId,
    /// Its conversation's threads now (one, unless its messages are in two conversations).
    pub threads: Vec<ThreadId>,
    /// `Waiting` or `Reminded`; ended ones aren't listed.
    pub state: ReminderState,
    /// When it is due, Unix seconds.
    pub wake_at: i64,
    /// When it came back, Unix seconds: its Inbox rows sort by it while it is later than their
    /// latest message.
    pub reminded_at: Option<i64>,
    /// The Reminders folder its messages wait in; `None` once that folder is gone (the server
    /// refused to make it, or it was deleted): its conversation stays where it is and comes back
    /// to the top of the Inbox at its time, and reminding it again makes a new one.
    pub folder: Option<FolderId>,
    /// The folder they came from.
    pub origin: FolderId,
}

/// The reply to [`Store::wake_reminders`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Woken {
    pub version: StoreVersion,
    /// The reminders that came back.
    pub woken: Vec<ReminderId>,
    /// The reminders that ended instead (phase C): someone answered the conversation while it
    /// waited, so it came back with the answer (Superhuman's "if no reply"), or it could never
    /// come back (its Reminders folder went with its messages).
    pub ended: Vec<ReminderId>,
    /// Server accounts with moves back to the Inbox queued: the engine wakes their op runners.
    pub accounts: Vec<AccountId>,
    /// When the next waiting reminder is due (Unix seconds), for the engine's timer.
    pub next: Option<i64>,
}

/// A server account's Reminders folder that a move due now goes into, which the engine
/// prepares first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderToPrepare {
    pub folder: FolderId,
    /// The name on the wire.
    pub name_raw: String,
    /// The store made it and the server doesn't have it yet: CREATE it.
    pub create: bool,
    /// The store never synced it: a pass first, so the moves land in rows of their own
    /// (COPYUID) rather than leave the store until that folder's sync.
    pub sync: bool,
}

impl Store {
    /// Sends `request.threads` to their accounts' Reminders folders until `request.wake_at`,
    /// in one transaction and one Undo batch: each conversation's messages in the scope's
    /// folders are moved as triage moves them (a server account's after the undo window), and
    /// its reminder is kept. An account without a Reminders folder gets one ("Reminders" at its
    /// top, as folder management names a new folder); a server account's is created on the
    /// server before the first move into it. A conversation back from Reminders that is sent
    /// again ends its place at the top of the Inbox; Undo restores it.
    pub fn remind(&self, request: Remind) -> Reply<Reminded> {
        self.write(|reply| WriteJob::Remind(RemindJob::Remind { request, reply }))
    }

    /// Cancels the reminders of `threads`' conversations (Remove Reminder), as one Undo batch:
    /// a waiting one's messages go back to the Inbox they came from (after the undo window,
    /// `not_before`, Unix ms), and a reminded one stops sorting by when it came back. Undo
    /// restores them, also when nothing moved (the batch is then empty; phase C).
    pub fn cancel_reminders(&self, threads: Vec<ThreadId>, not_before: i64) -> Reply<Queued> {
        self.write(|reply| {
            WriteJob::Remind(RemindJob::Cancel {
                threads,
                not_before,
                reply,
            })
        })
    }

    /// Brings back every waiting reminder due at `now` (Unix seconds): its conversation's
    /// messages in the Reminders folder are moved to the Inbox they came from (an account's
    /// Inbox when that is gone) at once, it is reminded at `now`, and its Inbox rows sort by
    /// `now` until it is acted on. Replies when the next one is due (never Someday's).
    ///
    /// Phase C: a waiting reminder whose conversation someone else answered meanwhile ends
    /// whenever it is, and what waited comes back to the Inbox beside the answer (Superhuman's
    /// "if no reply"); the engine runs this when new mail arrives. A due one with nothing to
    /// bring back and nothing in an Inbox ends rather than being reminded.
    pub fn wake_reminders(&self, now: i64) -> Reply<Woken> {
        self.write(|reply| WriteJob::Remind(RemindJob::Wake { now, reply }))
    }

    /// The reminders that still count (waiting or reminded), the soonest due first.
    pub fn reminders(&self) -> Reply<Vec<Reminder>> {
        self.read(|reply| ReadJob::Reminders { reply })
    }

    /// `account`'s Reminders folders that a move due at `now` (Unix ms) goes into and that the
    /// engine must create on the server, or sync once, before it runs.
    pub fn folders_to_prepare(&self, account: AccountId, now: i64) -> Reply<Vec<FolderToPrepare>> {
        self.read(|reply| ReadJob::FoldersToPrepare {
            account,
            now,
            reply,
        })
    }

    /// The server created `folder` (or had it): the folder list keeps it from now on as any
    /// listed folder.
    pub fn folder_created(&self, folder: FolderId) -> Reply<()> {
        self.write(|reply| WriteJob::Remind(RemindJob::FolderCreated { folder, reply }))
    }

    /// The server refused to create `folder`: it goes, and the moves into it are refused as
    /// for a folder gone from the server (the messages show again where they are, and the op
    /// runner reports it). Its reminders stay: their conversations come back to the top of the
    /// Inbox at their time.
    pub fn folder_refused(&self, folder: FolderId) -> Reply<StoreVersion> {
        self.write(|reply| WriteJob::Remind(RemindJob::FolderRefused { folder, reply }))
    }
}
