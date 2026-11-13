//! The message store (ADR-005, ADR-003): SQLite in WAL mode behind threads that own their
//! connections. One writer commits and emits change events, two read-only readers serve the
//! newest request first, and a maintenance thread checkpoints the WAL off the writer. Callers
//! exchange plain `Send` values over async-channel, so the GTK thread awaits replies on its main
//! context and never runs SQL.

mod api;
mod check;
mod codec;
mod compose;
mod compose_api;
mod contact_api;
mod contacts;
mod conversation;
mod conversations;
mod folders_api;
mod fork;
pub mod fts;
mod maintenance;
mod ops_api;
mod overlay;
mod pop3_api;
mod reader;
mod remind;
mod remind_api;
mod schema;
mod sync_api;
mod views;
mod writer;

#[cfg(test)]
mod resync_tests;
#[cfg(test)]
mod sync_tests;
#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod account_tests;
#[cfg(test)]
mod thread_tests;
#[cfg(test)]
mod unified_tests;

#[cfg(test)]
mod body_tests;

#[cfg(test)]
mod count_tests;

#[cfg(test)]
mod conversation_tests;
#[cfg(test)]
mod conversation_view_tests;

#[cfg(test)]
mod ops_prop_tests;
#[cfg(test)]
mod ops_tests;
#[cfg(test)]
mod rules_tests;

#[cfg(test)]
mod compose_prop_tests;
#[cfg(test)]
mod compose_tests;

#[cfg(test)]
mod folders_tests;

#[cfg(test)]
mod remote_tests;

#[cfg(test)]
mod search_tests;

#[cfg(test)]
mod split_tests;

#[cfg(test)]
mod remind_tests;

#[cfg(test)]
mod contact_tests;

#[cfg(test)]
mod pop3_tests;

pub use api::*;
pub use compose_api::*;
pub use contact_api::*;
pub use conversation::{Conversation, ConversationMessage};
pub use folders_api::*;
pub use ops_api::*;
pub use pop3_api::*;
pub use remind_api::*;
pub use schema::VERSION as SCHEMA_VERSION;
pub use sync_api::*;
pub use views::ViewHandle;

use crate::blobs::BlobStore;
use crate::xdg::ensure_private_dir;
use reader::{ReadJob, Reader};
use sealwax_domain::account::AccountKey;
use sealwax_domain::model::{
    Account, AccountId, BlobKey, Flags, Folder, FolderId, MessageDetail, MessageId, MessageSummary,
    ThreadId,
};
use sealwax_domain::split::Split;
use std::fs::{self, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use views::Registry;
use writer::{WriteJob, Writer};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the database has schema version {found}; this build supports up to {supported}")]
    NewerSchema { found: i64, supported: i64 },
    /// The fork's own schema track (`fork.rs`) is newer than this build's.
    #[error("the database has fork schema {found}; this build supports up to {supported}")]
    NewerForkSchema { found: i64, supported: i64 },
    #[error("SQLite {0} lacks FTS5 with contentless delete; Pronto needs SQLite 3.43 or newer")]
    MissingFts5(String),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("file: {0}")]
    Io(#[from] io::Error),
    #[error("unknown folder {0}")]
    UnknownFolder(FolderId),
    #[error("folder {0} is not in the sync state this step needs")]
    SyncState(FolderId),
    #[error("unknown account {0}")]
    UnknownAccount(AccountId),
    /// A POP3 step for an account that isn't one, or an IMAP step for one that is.
    #[error("account {0} is not a POP3 account, or is one where IMAP was asked")]
    NotPop3(AccountId),
    #[error("the store is closed")]
    Closed,
    /// Another process (or another `Store` of this one) holds the store's lock file.
    #[error("the mail store {0} is open in another Pronto; quit it, then start Pronto again")]
    Locked(PathBuf),
}

/// How a store runs.
#[derive(Clone, Debug)]
pub struct StoreOptions {
    /// Read-only reader threads.
    pub readers: usize,
    /// WAL frames after which the maintenance thread checkpoints.
    pub checkpoint_frames: u32,
    /// Where raw messages live, for `raw_message`.
    pub blobs: Option<BlobStore>,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            readers: 2,
            checkpoint_frames: 1_000,
            blobs: None,
        }
    }
}

/// The reply to one request. Await it on any executor, or block on it off the GTK thread.
/// Dropping it before the reply arrives cancels a read that hasn't started.
#[must_use = "a request does nothing visible unless its reply is read"]
pub struct Reply<T> {
    receiver: async_channel::Receiver<Result<T, StoreError>>,
}

impl<T> Reply<T> {
    fn pair() -> (async_channel::Sender<Result<T, StoreError>>, Reply<T>) {
        let (sender, receiver) = async_channel::bounded(1);
        (sender, Reply { receiver })
    }

    pub async fn recv(self) -> Result<T, StoreError> {
        self.receiver
            .recv()
            .await
            .unwrap_or(Err(StoreError::Closed))
    }

    pub fn recv_blocking(self) -> Result<T, StoreError> {
        self.receiver
            .recv_blocking()
            .unwrap_or(Err(StoreError::Closed))
    }
}

/// The writer's reply to a removal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub version: StoreVersion,
    pub removed: Vec<MessageId>,
    /// Raw messages no remaining row refers to; the caller may delete them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

/// The reply to [`Store::rebuild_index`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexRebuilt {
    /// Messages indexed again.
    pub messages: u64,
    pub elapsed: std::time::Duration,
    /// Messages that kept their entry: the store holds no body to index them from, and their
    /// entry holds words of the preview it doesn't keep.
    pub kept: u64,
    /// Entries deleted because their message was gone.
    pub removed: u64,
}

/// How far a rebuild of the search index has come: messages walked of those to walk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexProgress {
    pub done: u64,
    pub total: u64,
}

/// The writer's reply to an account's removal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemovedAccount {
    pub version: StoreVersion,
    /// Whether the account existed; removing one that doesn't is no error.
    pub found: bool,
    pub folders: u32,
    pub messages: u32,
    /// Raw messages no other account refers to; the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

pub(crate) struct Shared {
    registry: Registry,
    subscribers: Mutex<Vec<async_channel::Sender<Arc<ChangeEvent>>>>,
    blobs: Option<BlobStore>,
    /// The search index's rebuild under way, which the writer updates after each step.
    index_progress: Mutex<Option<IndexProgress>>,
}

impl Shared {
    fn publish(&self, event: ChangeEvent) {
        let event = Arc::new(event);
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        subscribers.retain(|subscriber| subscriber.try_send(event.clone()).is_ok());
    }
}

/// An open store. Requests go to its threads; dropping it lets them finish and exit, `close`
/// also waits for them.
pub struct Store {
    reads: async_channel::Sender<ReadJob>,
    writes: async_channel::Sender<WriteJob>,
    shared: Arc<Shared>,
    readers: Vec<JoinHandle<()>>,
    writers: Vec<JoinHandle<()>>,
    path: PathBuf,
    /// Held until the store is dropped: after `close` that is once its threads have stopped.
    _lock: StoreLock,
}

/// The advisory lock on a store, `<store>.lock` beside it, for as long as the value lives. Two
/// builds share the XDG directories (debug and release), and each would run its own writer,
/// outbox and blob sweep over the same mail; SQLite's own locks only keep the pages whole.
#[derive(Debug)]
pub(crate) struct StoreLock(#[allow(dead_code)] fs::File);

impl StoreLock {
    /// Takes the lock of the store at `path` without waiting; [`StoreError::Locked`] when
    /// another open file holds it. The kernel releases it when the file closes, also after a
    /// crash, so a leftover lock file never blocks anything.
    pub(crate) fn acquire(path: &Path) -> Result<StoreLock, StoreError> {
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&name)?;
        match file.try_lock() {
            Ok(()) => Ok(StoreLock(file)),
            Err(fs::TryLockError::WouldBlock) => Err(StoreError::Locked(path.to_owned())),
            Err(fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

impl Store {
    /// Opens or creates the store at `path` (private directory 0700, files 0600), migrates it,
    /// and starts its threads. Refuses a database from a newer build and an SQLite without FTS5.
    pub fn open(path: &Path, options: StoreOptions) -> Result<Store, StoreError> {
        schema::check_sqlite()?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            ensure_private_dir(parent)?;
        }
        let lock = StoreLock::acquire(path)?;
        private_files(path)?;
        let writer = schema::open_writer(path)?;
        let readers = (0..options.readers.max(1))
            .map(|_| schema::open_reader(path))
            .collect::<Result<Vec<_>, _>>()?;
        let maintenance = schema::open_maintenance(path)?;
        private_files(path)?;

        let shared = Arc::new(Shared {
            registry: Registry::new(),
            subscribers: Mutex::new(Vec::new()),
            blobs: options.blobs,
            index_progress: Mutex::new(None),
        });
        let mut reader_threads = Vec::new();
        let (reads, read_queue) = async_channel::unbounded();
        for (index, conn) in readers.into_iter().enumerate() {
            let reader = Reader::new(conn, shared.clone());
            let queue = read_queue.clone();
            reader_threads.push(
                std::thread::Builder::new()
                    .name(format!("sealwax-store-reader-{index}"))
                    .spawn(move || reader.run(queue))?,
            );
        }
        let mut writers = Vec::new();
        let (checkpoints, checkpoint_queue) = async_channel::bounded(8);
        writers.push(
            std::thread::Builder::new()
                .name("sealwax-store-maintenance".into())
                .spawn(move || maintenance::run(maintenance, checkpoint_queue))?,
        );
        let writer = Writer::new(
            writer,
            shared.clone(),
            checkpoints,
            options.checkpoint_frames,
        )?;
        let (writes, write_queue) = async_channel::unbounded();
        writers.push(
            std::thread::Builder::new()
                .name("sealwax-store-writer".into())
                .spawn(move || writer.run(write_queue))?,
        );
        tracing::debug!(readers = options.readers, "store opened");
        Ok(Store {
            reads,
            writes,
            shared,
            readers: reader_threads,
            writers,
            path: path.to_owned(),
            _lock: lock,
        })
    }

    /// `open` on a thread of its own, for callers that must not run SQL themselves (the GTK
    /// thread): await the reply.
    pub fn open_in_background(path: PathBuf, options: StoreOptions) -> Reply<Store> {
        let (sender, reply) = Reply::pair();
        let _ = std::thread::Builder::new()
            .name("sealwax-store-open".into())
            .spawn(move || {
                let _ = sender.try_send(Store::open(&path, options));
            });
        reply
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Change events, one per commit that changes what lists, the sidebar or the reader show,
    /// in commit order.
    pub fn subscribe(&self) -> async_channel::Receiver<Arc<ChangeEvent>> {
        let (sender, receiver) = async_channel::unbounded();
        self.shared
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        receiver
    }

    fn read<T>(
        &self,
        job: impl FnOnce(async_channel::Sender<Result<T, StoreError>>) -> ReadJob,
    ) -> Reply<T> {
        let (sender, reply) = Reply::pair();
        let _ = self.reads.try_send(job(sender));
        reply
    }

    fn write<T>(
        &self,
        job: impl FnOnce(async_channel::Sender<Result<T, StoreError>>) -> WriteJob,
    ) -> Reply<T> {
        let (sender, reply) = Reply::pair();
        let _ = self.writes.try_send(job(sender));
        reply
    }

    /// Registers a view for change events and reads its count, section counts and first page
    /// from one snapshot.
    pub fn open_view(&self, request: OpenView) -> Reply<ViewOpened> {
        self.read(|reply| ReadJob::OpenView { request, reply })
    }

    pub fn page(&self, spec: ViewSpec, offset: u32, limit: u32) -> Reply<Page> {
        self.read(|reply| ReadJob::Page {
            spec,
            offset,
            limit,
            reply,
        })
    }

    pub fn accounts(&self) -> Reply<Versioned<Vec<Account>>> {
        self.read(|reply| ReadJob::Accounts { reply })
    }

    /// Every folder with its total and unread counts.
    pub fn folders(&self) -> Reply<Versioned<Vec<Folder>>> {
        self.read(|reply| ReadJob::Folders { reply })
    }

    pub fn message(&self, id: MessageId) -> Reply<Versioned<Option<MessageDetail>>> {
        self.read(|reply| ReadJob::Detail { id, reply })
    }

    /// The raw message from the blob store, when the store was opened with one and holds it.
    pub fn raw_message(&self, id: MessageId) -> Reply<Option<Vec<u8>>> {
        self.read(|reply| ReadJob::Raw { id, reply })
    }

    /// The newest `limit` messages that what the user typed finds (the query language of
    /// [`sealwax_domain::search::parse_typed`]: a last word with nothing after it is the start
    /// of a word), in one folder or in every folder but Trash and Junk of the attached accounts.
    pub fn search(
        &self,
        text: &str,
        folder: Option<FolderId>,
        limit: u32,
    ) -> Reply<Versioned<Vec<MessageSummary>>> {
        let text = text.to_owned();
        self.read(|reply| ReadJob::Search {
            text,
            folder,
            limit,
            reply,
        })
    }

    /// The newest `limit` conversations what the user typed finds, as `search` finds
    /// messages, newest match first: each conversation's row as a conversation view of the
    /// searched folders shows it, with its newest match (docs/superhuman/store.md).
    pub fn search_conversations(
        &self,
        text: &str,
        folder: Option<FolderId>,
        limit: u32,
    ) -> Reply<Versioned<Vec<ConversationHit>>> {
        let text = text.to_owned();
        self.read(|reply| ReadJob::SearchConversations {
            text,
            folder,
            limit,
            reply,
        })
    }

    /// The shown members of `threads` in the folders of `spec`'s scope that pass its filter,
    /// each conversation's oldest first: what triage on conversation rows acts on, one target
    /// per member, all queued as one batch (docs/superhuman/store.md).
    pub fn conversation_members(
        &self,
        threads: Vec<ThreadId>,
        spec: ViewSpec,
    ) -> Reply<Versioned<Vec<ConversationMember>>> {
        self.read(|reply| ReadJob::ConversationMembers {
            threads,
            spec,
            reply,
        })
    }

    /// Indexes every stored message again in the background (ML-081, Rebuild Search Index):
    /// in place, a few messages a transaction on the writer's full-text lane, after every other
    /// write, so a search finds each message by its old entry or its new one throughout. A
    /// message whose body the store doesn't hold keeps its entry. Replies when done; a request
    /// while one runs gets the running one's reply. A rebuild cut short by closing the store
    /// goes on when it opens again.
    pub fn rebuild_index(&self) -> Reply<IndexRebuilt> {
        self.write(|reply| WriteJob::RebuildIndex { reply })
    }

    /// The rebuild of the search index under way, if any. Reads no database: safe to poll
    /// from the GTK thread.
    pub fn index_progress(&self) -> Option<IndexProgress> {
        *self
            .shared
            .index_progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Recounts every maintained aggregate and lists disagreements (a diagnostic).
    pub fn check(&self) -> Reply<Vec<String>> {
        self.read(|reply| ReadJob::Check { reply })
    }

    /// A local account with no key: fixtures and self-tests, which no settings file lists.
    pub fn add_account(&self, account: NewAccount) -> Reply<Versioned<AccountId>> {
        self.write(|reply| WriteJob::AddAccount { account, reply })
    }

    /// The account `accounts.toml` lists under `key`: added, or its name and address updated
    /// and the account attached again.
    pub fn ensure_account(
        &self,
        key: AccountKey,
        account: NewAccount,
    ) -> Reply<Versioned<AccountId>> {
        self.write(|reply| WriteJob::EnsureAccount {
            key,
            account,
            reply,
        })
    }

    /// Brings the keyed accounts in line with `accounts.toml`: every listed account as in
    /// `ensure_account`, every other keyed account detached (hidden from `accounts()` and the
    /// unified inbox, its mail kept). Replies with the ids in the order given.
    pub fn reconcile_accounts(
        &self,
        accounts: Vec<(AccountKey, NewAccount)>,
    ) -> Reply<Versioned<Vec<AccountId>>> {
        self.write(|reply| WriteJob::ReconcileAccounts { accounts, reply })
    }

    /// The account with `key`, detached or not.
    pub fn account_by_key(&self, key: AccountKey) -> Reply<Versioned<Option<Account>>> {
        self.read(|reply| ReadJob::AccountByKey { key, reply })
    }

    /// Deletes an account and everything it owns in one transaction (ML-038); the reply lists
    /// the raw messages to delete from the blob store.
    pub fn remove_account(&self, account: AccountId) -> Reply<RemovedAccount> {
        self.write(|reply| WriteJob::RemoveAccount { account, reply })
    }

    pub fn upsert_folder(&self, folder: FolderSpec) -> Reply<Versioned<FolderId>> {
        self.write(|reply| WriteJob::UpsertFolder { folder, reply })
    }

    /// Upserts a whole folder list in one transaction; the ids come back in input order.
    pub fn upsert_folders(&self, folders: Vec<FolderSpec>) -> Reply<Versioned<Vec<FolderId>>> {
        self.write(|reply| WriteJob::UpsertFolders { folders, reply })
    }

    /// A folder's wire name and sync state, for the sync engine.
    pub fn folder_sync(&self, folder: FolderId) -> Reply<Option<FolderSync>> {
        self.read(|reply| ReadJob::FolderSync { folder, reply })
    }

    /// Records a step of a folder's first sync. `Start` needs a new folder and `Floor` and
    /// `Finish` one in its first sync; otherwise the reply is [`StoreError::SyncState`].
    pub fn first_sync_step(&self, folder: FolderId, step: FirstSyncStep) -> Reply<StoreVersion> {
        self.write(|reply| WriteJob::FirstSync {
            folder,
            step,
            reply,
        })
    }

    /// Removes every message of a folder whose UIDVALIDITY changed and makes it new again,
    /// under the new UIDVALIDITY. Pending ops of its messages find them again by Message-ID as
    /// the folder syncs again; those still unmatched when its first sync finishes are refused.
    pub fn reset_folder(&self, folder: FolderId, uidvalidity: u32) -> Reply<Removed> {
        self.write(|reply| WriteJob::ResetFolder {
            folder,
            uidvalidity,
            reply,
        })
    }

    /// Stores messages in transactions of at most 200, each followed by its own full-text
    /// transaction; replies once all are indexed. Use `ingest::ingest_raw` for raw messages.
    pub fn ingest(&self, messages: Vec<NewMessage>) -> Reply<Ingested> {
        self.write(|reply| WriteJob::Ingest { messages, reply })
    }

    /// Adds and removes flags as the server reports them; what is shown keeps any pending op.
    /// The reply carries the version of the commit, or the current one when nothing shown
    /// changed.
    pub fn set_flags(&self, ids: Vec<MessageId>, add: Flags, remove: Flags) -> Reply<StoreVersion> {
        self.write(|reply| WriteJob::SetFlags {
            ids,
            add,
            remove,
            reply,
        })
    }

    /// Removes messages (an EXPUNGE or VANISHED). A pending move or expunge of one counts as
    /// done (S1).
    pub fn remove(&self, ids: Vec<MessageId>) -> Reply<Removed> {
        self.write(|reply| WriteJob::Remove { ids, reply })
    }

    /// Where each of `ids` is on the server and what of it the store holds, for downloading
    /// bodies (ML-044); ids the store doesn't hold are left out.
    pub fn body_targets(&self, ids: Vec<MessageId>) -> Reply<Vec<BodyTarget>> {
        self.read(|reply| ReadJob::BodyTargets { ids, reply })
    }

    /// Puts a downloaded body on its header-only row. Store the blob first: it must be durable
    /// before the commit that refers to it. Deletes nothing from the blob store itself; the
    /// reply lists what no row refers to any more.
    pub fn upgrade_body(&self, upgrade: BodyUpgrade) -> Reply<Upgraded> {
        self.write(|reply| WriteJob::UpgradeBody {
            upgrade: Box::new(upgrade),
            reply,
        })
    }

    /// Marks the body of `message` used at `at` (Unix seconds), when the reader shows it.
    pub fn touch_body(&self, message: MessageId, at: i64) -> Reply<()> {
        self.write(|reply| WriteJob::TouchBody { message, at, reply })
    }

    /// One batch of the disk cap: bodies used longest ago go back to headers and preview until
    /// the bodies take at most `limit` bytes, sparing those of `keep`. The caller deletes
    /// `Evicted::blobs` from the blob store, and calls again while `Evicted::more`.
    pub fn evict_bodies(&self, limit: u64, keep: Vec<MessageId>) -> Reply<Evicted> {
        self.write(|reply| WriteJob::EvictBodies { limit, keep, reply })
    }

    /// Deletes those of `keys` from `blobs` that no message, draft or outbox entry refers to:
    /// on the writer, in the transaction that looks, so no commit can add a reference between
    /// the look and the deletion. The lists other replies carry (`orphaned_blobs`,
    /// `Evicted::blobs`) are as old as their commit; identical content that arrived since
    /// shares the file. With `older_than`, a file stored (or stored again) more recently
    /// stays as well: the sweep's guard for a blob whose row isn't committed yet.
    pub fn discard_blobs(
        &self,
        blobs: BlobStore,
        keys: Vec<BlobKey>,
        older_than: Option<std::time::Duration>,
    ) -> Reply<BlobsDiscarded> {
        self.write(|reply| WriteJob::DiscardBlobs {
            blobs,
            keys,
            older_than,
            reply,
        })
    }

    /// Replies once every commit so far is on disk. Commits are `synchronous=NORMAL`: after a
    /// power cut the store is whole, but may lack its last transactions. A caller about to
    /// destroy the only other copy of what it stored (POP3's DELE and QUIT) waits for this.
    pub fn flush(&self) -> Reply<()> {
        self.write(|reply| WriteJob::Flush { reply })
    }

    /// What the downloaded bodies take.
    pub fn body_usage(&self) -> Reply<BodyUsage> {
        self.read(|reply| ReadJob::BodyUsage { reply })
    }

    /// Up to `limit` messages without a body that arrived since `since` in the Inboxes of
    /// server accounts and the rest of their conversations, the Inbox's first, newest first:
    /// the prefetch's candidates.
    pub fn body_candidates(&self, since: i64, limit: u32) -> Reply<Vec<BodyCandidate>> {
        self.read(|reply| ReadJob::BodyCandidates {
            since,
            limit,
            reply,
        })
    }

    /// Where the attachment at mail-parser index `part` of `message` is on the server, when the
    /// store recorded its section.
    pub fn part_source(&self, message: MessageId, part: u32) -> Reply<Option<PartSource>> {
        self.read(|reply| ReadJob::PartSource {
            message,
            part,
            reply,
        })
    }

    /// The senders whose remote images load without asking (ML-058), lower case, in order.
    pub fn remote_senders(&self) -> Reply<Vec<String>> {
        self.read(|reply| ReadJob::RemoteSenders { reply })
    }

    /// Images from `address` load without asking from now on; `at` (Unix seconds) is when the
    /// user added it. The address is kept trimmed and in lower case; an empty one is ignored.
    pub fn allow_remote_sender(&self, address: String, at: i64) -> Reply<()> {
        self.write(|reply| WriteJob::AllowRemoteSender { address, at, reply })
    }

    /// Images from `address` are hidden again until the user allows them.
    pub fn forget_remote_sender(&self, address: String) -> Reply<()> {
        self.write(|reply| WriteJob::ForgetRemoteSender { address, reply })
    }

    /// The user's choice for a sender in the Split Inbox (docs/superhuman/split.md): `Some`
    /// puts every message from `address` (exact address, any account, stored or to come) on
    /// that side, whatever else says; `None` forgets the choice. The sender's conversations are
    /// classified again, each transaction's change event carrying the views' diffs; the reply
    /// comes with the last ([`Store::set_sender_split_first`]).
    pub fn set_sender_split(&self, address: String, split: Option<Split>) -> Reply<StoreVersion> {
        self.set_sender_split_first(address, split, Vec::new())
    }

    /// [`Store::set_sender_split`], classifying `first` again in the first transaction (the
    /// conversations the user acted on: they move at once). A sender with much mail is
    /// classified again in transactions of a few hundred messages, the conversations the open
    /// views show first, with other writes between them; the reply comes with the last.
    pub fn set_sender_split_first(
        &self,
        address: String,
        split: Option<Split>,
        first: Vec<ThreadId>,
    ) -> Reply<StoreVersion> {
        self.write(|reply| WriteJob::SetSenderSplit {
            address,
            split,
            first,
            reply,
        })
    }

    /// Every sender the user chose a side for, by address (lower case), in order.
    pub fn sender_splits(&self) -> Reply<Vec<(String, Split)>> {
        self.read(|reply| ReadJob::SenderSplits { reply })
    }

    /// The server's own categories of stored messages (Gmail's Primary tab is Important): they
    /// win over the header rules. The conversations that changed are classified again.
    pub fn set_categories(&self, categories: Vec<(MessageId, Split)>) -> Reply<StoreVersion> {
        self.write(|reply| WriteJob::SetCategories { categories, reply })
    }

    /// The shown messages of `folder` the server gave no category for yet, newest first, at
    /// most `limit`: what the engine asks Gmail about.
    pub fn uncategorized(&self, folder: FolderId, limit: u32) -> Reply<Vec<Uncategorized>> {
        self.read(|reply| ReadJob::Uncategorized {
            folder,
            limit,
            reply,
        })
    }

    /// How many rows of `spec` (its scope and grouping) each side of the Split Inbox holds.
    pub fn split_counts(&self, spec: ViewSpec) -> Reply<Versioned<SplitCounts>> {
        self.read(|reply| ReadJob::SplitCounts { spec, reply })
    }

    /// Stops accepting requests, lets queued requests finish and waits for the threads. The
    /// readers stop first, so the writer and the maintenance thread close last and leave an
    /// empty WAL behind.
    pub fn close(mut self) -> Result<(), StoreError> {
        self.reads.close();
        for thread in self.readers.drain(..) {
            thread.join().map_err(|_| StoreError::Closed)?;
        }
        self.writes.close();
        for thread in self.writers.drain(..) {
            thread.join().map_err(|_| StoreError::Closed)?;
        }
        Ok(())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.reads.close();
        self.writes.close();
    }
}

/// How the list of senders whose images load keeps an address: trimmed, in lower case; `None`
/// for an empty one.
pub(crate) fn remote_sender_key(address: &str) -> Option<String> {
    let address = address.trim();
    (!address.is_empty()).then(|| address.to_lowercase())
}

/// Creates the database file 0600 before SQLite does (its WAL and shared-memory files copy the
/// database's mode), and narrows existing files to 0600.
fn private_files(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        match fs::metadata(&file) {
            Ok(metadata) if metadata.permissions().mode() & 0o777 != 0o600 => {
                fs::set_permissions(&file, Permissions::from_mode(0o600))?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
