//! The contact pane's read (the fork's Superhuman pass, docs/superhuman/contact.md): what the
//! store holds from one person, by their address: how many messages they sent and since when,
//! and the conversations they last wrote in. Local only: nothing here asks a server.

use super::reader::ReadJob;
use super::{Reply, Store, Versioned};
use sealwax_domain::model::{FolderId, MessageId, ThreadId};

/// A conversation a person wrote in, as the contact pane lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactConversation {
    pub thread: ThreadId,
    /// Its subject without Re:/Fwd: and their kin (`threads.subject`, as the list's rows).
    pub subject: String,
    /// Their latest message in it, which it opens at: the copy the reader would show (an
    /// Inbox's before an Archive's, by `conversation::rank`), its folder and its date.
    pub message: MessageId,
    pub folder: FolderId,
    pub date: i64,
}

/// What the store holds from one sender, across the attached accounts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContactFacts {
    /// Their shown messages outside Trash and Junk, each once: copies of one message in
    /// several folders (Gmail's labels) count once. A copy is a row of the same conversation
    /// and the same date, so two messages of theirs in one conversation within one second
    /// count as one.
    pub messages: u32,
    /// The first and the latest of those messages (Unix seconds); undated mail (stored at 0,
    /// with neither an INTERNALDATE nor a Date header) dates neither.
    pub first: Option<i64>,
    pub last: Option<i64>,
    /// The conversations they last wrote in, by their latest message there, newest first: at
    /// most the limit asked for.
    pub recent: Vec<ContactConversation>,
}

impl Store {
    /// What the store holds from `address` (its case doesn't matter), as the contact pane shows
    /// it, from one snapshot: their messages and when the first and latest came, and the
    /// `limit` conversations they last wrote in (ask for one more than shown when the open
    /// conversation is left out). Two index walks (`fork.rs`' fourth step), whatever the
    /// store's size.
    pub fn contact(&self, address: &str, limit: u32) -> Reply<Versioned<ContactFacts>> {
        let address = address.to_owned();
        self.read(|reply| ReadJob::Contact {
            address,
            limit,
            reply,
        })
    }
}
