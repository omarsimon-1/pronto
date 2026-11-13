//! What the service and phase B's account actor share about bodies (ML-044): the messages the
//! background prefetch downloads after a sync, and the disk cap's enforcement. The fetching
//! itself is `imap::bodies`; the rules are `sealwax_domain::bodies`.

use crate::imap::bodies::Sink;
use sealwax_domain::bodies::{CacheUse, Candidate, Prefetch, prefetch};
use sealwax_domain::config::Storage;
use sealwax_domain::model::{BlobKey, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{BlobsDiscarded, Store, StoreError};
use std::time::Duration;

/// Candidates read per prefetch; the policy picks from them.
const PREFETCH_CANDIDATES: u32 = 500;
/// Blobs the writer is asked about per transaction, so other writes pass between.
const DISCARD_BATCH: usize = 256;
/// The sweep leaves a file stored (or stored again) within this long: whatever stored it may
/// not have committed its row yet.
pub const SWEEP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Deletes the raw messages of `keys` that nothing refers to. The list is as old as the commit
/// that reported it, and identical content that arrived since shares the file, so the store's
/// writer looks again and deletes in that transaction (`Store::discard_blobs`). Returns what
/// was done; a failure is logged, and the files stay for the sweep.
pub async fn discard_blobs(store: &Store, blobs: &BlobStore, keys: Vec<BlobKey>) -> BlobsDiscarded {
    discard(store, blobs, keys, None).await
}

async fn discard(
    store: &Store,
    blobs: &BlobStore,
    keys: Vec<BlobKey>,
    older_than: Option<Duration>,
) -> BlobsDiscarded {
    let mut total = BlobsDiscarded::default();
    for chunk in keys.chunks(DISCARD_BATCH) {
        match store
            .discard_blobs(blobs.clone(), chunk.to_vec(), older_than)
            .recv()
            .await
        {
            Ok(discarded) => {
                total.removed += discarded.removed;
                total.referenced += discarded.referenced;
                total.failed += discarded.failed;
            }
            Err(error) => {
                tracing::warn!(%error, "raw messages no row refers to couldn't be deleted");
                total.failed += u32::try_from(chunk.len()).unwrap_or(u32::MAX);
            }
        }
    }
    total
}

/// Deletes the blob files no row refers to that a crash or a failed deletion left behind
/// (between a blob's write and its commit, or between a commit and its deletions): one fan-out
/// directory at a time, only files older than [`SWEEP_AGE`], each deleted by the writer in the
/// transaction that finds it unreferenced. Returns how many files went.
pub async fn sweep_blobs(store: &Store, blobs: &BlobStore) -> u32 {
    let mut removed = 0;
    for shard in 0..=u8::MAX {
        let walked = blobs.clone();
        let listed = tokio::task::spawn_blocking(move || walked.older_than(shard, SWEEP_AGE)).await;
        let keys = match listed {
            Ok(Ok(keys)) => keys,
            Ok(Err(error)) => {
                tracing::warn!(%error, "the blob sweep couldn't read a directory");
                continue;
            }
            Err(_) => return removed,
        };
        if keys.is_empty() {
            continue;
        }
        let swept = discard(store, blobs, keys, Some(SWEEP_AGE)).await;
        removed += swept.removed;
        // A closed store answers nothing more.
        if swept.failed > 0 && swept.removed == 0 && swept.referenced == 0 {
            break;
        }
    }
    if removed > 0 {
        tracing::info!(files = removed, "raw messages no row refers to were swept");
    }
    removed
}

/// The messages the background prefetch downloads under `storage`: the Inbox messages of server
/// accounts newest first, then the rest of their conversations newest first, whatever their age
/// (the fork's Superhuman pass), each up to the whole-message limit, while the bodies stay
/// within half the cap. Empty when `prefetch_days` is 0.
pub async fn prefetch_ids(store: &Store, storage: &Storage) -> Result<Vec<MessageId>, StoreError> {
    let policy = Prefetch::new(storage);
    if !policy.on() {
        return Ok(Vec::new());
    }
    let candidates = store
        .body_candidates(i64::MIN, PREFETCH_CANDIDATES)
        .recv()
        .await?;
    let usage = store.body_usage().recv().await?;
    let candidates: Vec<Candidate> = candidates
        .iter()
        .map(|candidate| Candidate {
            message: candidate.message,
            role: candidate.role,
            date: candidate.date,
            size: candidate.size,
            downloaded: false,
        })
        .collect();
    let cache = CacheUse {
        used: usage.bytes,
        limit: storage.cache_size,
    };
    Ok(prefetch(&policy, cache, &candidates))
}

/// Sends the bodies used longest ago back to their headers and preview until the bodies take
/// at most `limit` bytes, sparing those of `keep`, and deletes their blobs after each commit.
/// Returns what the bodies take afterwards.
pub async fn enforce_cap(
    sink: Sink<'_>,
    limit: u64,
    keep: &[MessageId],
) -> Result<u64, StoreError> {
    loop {
        let _writes = sink.writes.lock().await;
        let evicted = sink.store.evict_bodies(limit, keep.to_vec()).recv().await?;
        if !evicted.blobs.is_empty() {
            tracing::info!(
                bodies = evicted.blobs.len(),
                messages = evicted.messages.len(),
                "bodies evicted under the disk cap"
            );
            discard_blobs(sink.store, sink.blobs, evicted.blobs.clone()).await;
        }
        if !evicted.more {
            return Ok(evicted.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::AccountKey;
    use sealwax_domain::model::BodyKind;
    use sealwax_domain::model::FolderRole;
    use sealwax_domain::model::Headers;
    use sealwax_infrastructure::blobs::BlobStore;
    use sealwax_infrastructure::store::{
        BodyState, BodyUpgrade, FolderSpec, NewAccount, NewMessage, StoreOptions,
    };
    use std::path::Path;

    const NOW: i64 = 1_790_000_000;
    const MIB: u64 = 1024 * 1024;

    fn scratch() -> (tempfile::TempDir, Store, BlobStore) {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("engine-bodies-")
            .tempdir_in(base)
            .unwrap();
        let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
        let store = Store::open(
            &dir.path().join("store.sqlite"),
            StoreOptions {
                blobs: Some(blobs.clone()),
                ..StoreOptions::default()
            },
        )
        .unwrap();
        (dir, store, blobs)
    }

    /// A server account's Inbox with header-only messages of `sizes`, one a day back from now.
    async fn inbox(store: &Store, sizes: &[u64]) -> Vec<MessageId> {
        let account = store
            .ensure_account(
                AccountKey("work".into()),
                NewAccount {
                    name: "Work".into(),
                    address: "work@example.test".into(),
                },
            )
            .recv()
            .await
            .unwrap()
            .value;
        let folder = store
            .upsert_folder(FolderSpec::local(account, "INBOX", FolderRole::Inbox))
            .recv()
            .await
            .unwrap()
            .value;
        let messages = sizes
            .iter()
            .enumerate()
            .map(|(n, size)| NewMessage {
                folder,
                uid: Some(n as u32 + 1),
                date: NOW - n as i64 * 86_400 - 60,
                headers: Headers {
                    message_id: Some(format!("inbox.{n}@example.test")),
                    ..Headers::default()
                },
                size: *size,
                body_state: BodyState::Partial,
                ..NewMessage::default()
            })
            .collect();
        store
            .ingest(messages)
            .recv()
            .await
            .unwrap()
            .ids
            .into_iter()
            .map(Option::unwrap)
            .collect()
    }

    #[tokio::test]
    async fn the_prefetch_takes_the_whole_inbox_within_half_the_cap() {
        let (_dir, store, _blobs) = scratch();
        let ids = inbox(&store, &[MIB, 20 * MIB, MIB, MIB, 300 * MIB, MIB]).await;
        let storage = Storage {
            cache_size: Some(1000 * MIB),
            prefetch_days: 4,
        };
        let picked = prefetch_ids(&store, &storage).await.unwrap();
        assert_eq!(
            picked,
            [ids[0], ids[2], ids[3], ids[5]],
            "newest first, the five-day-old one too: {picked:?}"
        );
        let tight = Storage {
            cache_size: Some(5 * MIB),
            ..storage
        };
        assert_eq!(
            prefetch_ids(&store, &tight).await.unwrap(),
            [ids[0], ids[2]]
        );
        let off = Storage {
            prefetch_days: 0,
            ..storage
        };
        assert!(prefetch_ids(&store, &off).await.unwrap().is_empty());
        store.close().unwrap();
    }

    /// The fork's Superhuman pass: the rest of an Inbox conversation is prefetched after the
    /// Inbox (the owner's reply in Sent, which the reader shows expanded as the latest message),
    /// whatever its age; other folders' mail, and the Trash's, isn't.
    #[tokio::test]
    async fn the_prefetch_takes_the_rest_of_inbox_conversations_next() {
        let (_dir, store, _blobs) = scratch();
        let ids = inbox(&store, &[1000, 1000]).await;
        let account = store
            .ensure_account(
                AccountKey("work".into()),
                NewAccount {
                    name: "Work".into(),
                    address: "work@example.test".into(),
                },
            )
            .recv()
            .await
            .unwrap()
            .value;
        let folder =
            |name: &'static str, role| store.upsert_folder(FolderSpec::local(account, name, role));
        let sent = folder("Sent", FolderRole::Sent).recv().await.unwrap().value;
        let trash = folder("Trash", FolderRole::Trash)
            .recv()
            .await
            .unwrap()
            .value;
        let answered = store
            .message(ids[1])
            .recv()
            .await
            .unwrap()
            .value
            .unwrap()
            .headers
            .message_id;
        let reply = |folder, uid, days: i64, in_reply_to: Option<String>| NewMessage {
            folder,
            uid: Some(uid),
            date: NOW - days * 86_400,
            size: 1000,
            body_state: BodyState::Partial,
            headers: Headers {
                message_id: Some(format!("own.{uid}.{days}@example.test")),
                in_reply_to: in_reply_to.into_iter().collect(),
                ..Headers::default()
            },
            ..NewMessage::default()
        };
        let stored: Vec<MessageId> = store
            .ingest(vec![
                reply(sent, 1, 400, answered.clone()),
                reply(sent, 2, 0, None),
                reply(trash, 1, 0, answered),
            ])
            .recv()
            .await
            .unwrap()
            .ids
            .into_iter()
            .map(Option::unwrap)
            .collect();
        let storage = Storage {
            cache_size: None,
            prefetch_days: 14,
        };
        assert_eq!(
            prefetch_ids(&store, &storage).await.unwrap(),
            [ids[0], ids[1], stored[0]],
            "the Inbox, then the old reply in its conversation; not the other Sent mail, \
             nor the Trash's copy"
        );
        store.close().unwrap();
    }

    /// A message the account holds twice (sent to oneself: the Inbox and Sent; a Gmail label
    /// opened once) downloads once: the Inbox's copy, else the first, and none while a copy has
    /// its body. Gmail counts every download against its daily limit.
    #[tokio::test]
    async fn the_prefetch_takes_one_copy_of_a_message() {
        let (_dir, store, blobs) = scratch();
        let ids = inbox(&store, &[1000, 1000]).await;
        let account = store
            .ensure_account(
                AccountKey("work".into()),
                NewAccount {
                    name: "Work".into(),
                    address: "work@example.test".into(),
                },
            )
            .recv()
            .await
            .unwrap()
            .value;
        let folder = async |name: &str, role| {
            store
                .upsert_folder(FolderSpec::local(account, name, role))
                .recv()
                .await
                .unwrap()
                .value
        };
        let sent = folder("Sent", FolderRole::Sent).await;
        let label = folder("Label", FolderRole::Other).await;
        let archive = folder("Archive", FolderRole::Archive).await;
        let copy = |folder, uid, message_id: &str, in_reply_to: &str| NewMessage {
            folder,
            uid: Some(uid),
            date: NOW - 3_600,
            size: 1000,
            body_state: BodyState::Partial,
            headers: Headers {
                message_id: Some(message_id.to_owned()),
                in_reply_to: vec![in_reply_to.to_owned()],
                ..Headers::default()
            },
            ..NewMessage::default()
        };
        let stored: Vec<MessageId> = store
            .ingest(vec![
                // Sent to oneself: the Inbox's copy is the one.
                copy(sent, 1, "inbox.0@example.test", ""),
                // A reply in Sent and in a label: the first.
                copy(sent, 2, "own.2@example.test", "inbox.1@example.test"),
                copy(label, 1, "own.2@example.test", "inbox.1@example.test"),
                // Another reply, read in the label already: the conversation shows the Sent
                // copy, which has no body yet.
                copy(sent, 3, "own.3@example.test", "inbox.1@example.test"),
                copy(label, 2, "own.3@example.test", "inbox.1@example.test"),
                // A reply in Sent, then archived: the conversation shows the Archive's copy,
                // though Sent's is older.
                copy(sent, 4, "own.4@example.test", "inbox.1@example.test"),
                copy(archive, 1, "own.4@example.test", "inbox.1@example.test"),
            ])
            .recv()
            .await
            .unwrap()
            .ids
            .into_iter()
            .map(Option::unwrap)
            .collect();
        let raw = b"Subject: own.3\r\n\r\nRead already.\r\n";
        let upgraded = store
            .upgrade_body(BodyUpgrade {
                message: stored[4],
                uid: 2,
                blob: blobs.put(raw).unwrap(),
                blob_size: raw.len() as u64,
                body_state: BodyState::Full,
                body_kind: BodyKind::Plain,
                has_attachments: false,
                snippet: String::new(),
                text_parts: vec![0],
                html_parts: Vec::new(),
                attachments: Vec::new(),
                sections: Vec::new(),
                search_text: String::new(),
                used: NOW,
            })
            .recv()
            .await
            .unwrap();
        assert!(upgraded.found);
        let storage = Storage {
            cache_size: None,
            prefetch_days: 14,
        };
        // The copies the reader shows, as `conversation::rank` picks them.
        assert_eq!(
            prefetch_ids(&store, &storage).await.unwrap(),
            [ids[0], ids[1], stored[6], stored[3], stored[1]]
        );
        store.close().unwrap();
    }

    #[tokio::test]
    async fn the_cap_deletes_the_evicted_blobs() {
        let (_dir, store, blobs) = scratch();
        let ids = inbox(&store, &[100, 100, 100]).await;
        let mut keys = Vec::new();
        for (n, id) in ids.iter().enumerate() {
            let raw = format!("Subject: {n}\r\n\r\n{}\r\n", "x".repeat(1000)).into_bytes();
            let key = blobs.put(&raw).unwrap();
            keys.push(key);
            let upgraded = store
                .upgrade_body(BodyUpgrade {
                    message: *id,
                    uid: n as u32 + 1,
                    blob: key,
                    blob_size: raw.len() as u64,
                    body_state: BodyState::Full,
                    body_kind: BodyKind::Plain,
                    has_attachments: false,
                    snippet: String::new(),
                    text_parts: vec![0],
                    html_parts: Vec::new(),
                    attachments: Vec::new(),
                    sections: Vec::new(),
                    search_text: String::new(),
                    used: NOW + n as i64 * 100,
                })
                .recv()
                .await
                .unwrap();
            assert!(upgraded.found);
        }
        let writes = tokio::sync::Mutex::new(());
        let sink = Sink {
            store: &store,
            blobs: &blobs,
            writes: &writes,
        };
        let left = enforce_cap(sink, 2_100, &[ids[0]]).await.unwrap();
        assert!(left <= 2_100, "{left}");
        assert!(blobs.contains(&keys[0]), "kept, though used longest ago");
        assert!(!blobs.contains(&keys[1]), "the oldest of the others went");
        assert!(blobs.contains(&keys[2]));
        assert_eq!(store.body_usage().recv().await.unwrap().bodies, 2);
        store.close().unwrap();
    }

    fn body(id: MessageId, uid: u32, key: BlobKey, size: usize) -> BodyUpgrade {
        BodyUpgrade {
            message: id,
            uid,
            blob: key,
            blob_size: size as u64,
            body_state: BodyState::Full,
            body_kind: BodyKind::Plain,
            has_attachments: false,
            snippet: String::new(),
            text_parts: vec![0],
            html_parts: Vec::new(),
            attachments: Vec::new(),
            sections: Vec::new(),
            search_text: String::new(),
            used: NOW,
        }
    }

    fn set_age(blobs: &BlobStore, key: &BlobKey, age: Duration) {
        std::fs::File::options()
            .write(true)
            .open(blobs.path(key))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - age)
            .unwrap();
    }

    #[tokio::test]
    async fn the_sweep_takes_only_old_files_nothing_refers_to() {
        let (_dir, store, blobs) = scratch();
        let ids = inbox(&store, &[100]).await;
        let held = blobs.put(b"Subject: held\r\n\r\nbody\r\n").unwrap();
        let upgraded = store.upgrade_body(body(ids[0], 1, held, 26)).recv().await;
        assert!(upgraded.unwrap().found);
        // Left by a crash between a blob's write and its commit, long ago and just now.
        let forgotten = blobs.put(b"forgotten").unwrap();
        let arriving = blobs.put(b"arriving").unwrap();
        // Forgotten too, but the same content has just been stored again for a new row.
        let again = blobs.put(b"stored again").unwrap();
        for key in [&held, &forgotten, &again] {
            set_age(&blobs, key, 2 * SWEEP_AGE);
        }
        assert_eq!(blobs.put(b"stored again").unwrap(), again);
        assert_eq!(sweep_blobs(&store, &blobs).await, 1);
        assert!(!blobs.contains(&forgotten));
        for key in [&held, &arriving, &again] {
            assert!(blobs.contains(key));
        }
        assert_eq!(sweep_blobs(&store, &blobs).await, 0);
        store.close().unwrap();
    }

    #[tokio::test]
    async fn a_stale_list_never_deletes_a_blob_that_is_referenced_again() {
        let (_dir, store, blobs) = scratch();
        let ids = inbox(&store, &[100, 100]).await;
        let raw = b"Subject: same bytes\r\n\r\nbody\r\n";
        let key = blobs.put(raw).unwrap();
        let first = store.upgrade_body(body(ids[0], 1, key, raw.len()));
        assert!(first.recv().await.unwrap().found);
        let removed = store.remove(vec![ids[0]]).recv().await.unwrap();
        assert_eq!(removed.orphaned_blobs, [key]);
        // The same bytes arrive for another message before the list is acted on.
        assert_eq!(blobs.put(raw).unwrap(), key);
        let second = store.upgrade_body(body(ids[1], 2, key, raw.len()));
        assert!(second.recv().await.unwrap().found);
        let discarded = discard_blobs(&store, &blobs, removed.orphaned_blobs).await;
        assert_eq!((discarded.removed, discarded.referenced), (0, 1));
        assert_eq!(blobs.get(&key).unwrap(), raw);
        let removed = store.remove(vec![ids[1]]).recv().await.unwrap();
        let discarded = discard_blobs(&store, &blobs, removed.orphaned_blobs).await;
        assert_eq!((discarded.removed, discarded.failed), (1, 0));
        assert!(!blobs.contains(&key));
        store.close().unwrap();
    }
}
