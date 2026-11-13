//! Which message bodies Pronto downloads (ML-044). A message up to [`WHOLE_LIMIT`] is fetched
//! whole; a larger one as its text parts, its attachments left on the server until asked for.
//! After a sync, the Inbox and the rest of its conversations are downloaded in the background
//! ([`prefetch`]) within part of the disk cap, so they open without waiting. Pure rules: the
//! engine fetches, the store keeps and evicts.

use crate::config::Storage;
use crate::model::{FolderRole, MessageId};

const MIB: u64 = 1024 * 1024;

/// Messages up to this size (RFC822.SIZE) are downloaded whole.
pub const WHOLE_LIMIT: u64 = 10 * MIB;

/// Background downloads may fill at most this share of the disk cap, in percent, so the bodies
/// the user opened aren't pushed out by ones nobody has read.
pub const PREFETCH_SHARE: u64 = 50;

/// What the background prefetch downloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefetch {
    /// `[storage]`'s `prefetch_days`: 0 turns prefetching off. Since the fork's Superhuman pass
    /// any other value prefetches the Inbox and its conversations whatever their age: what is
    /// in the Inbox is what the user will open next, old or not.
    pub days: u32,
    /// Larger messages wait until they are opened.
    pub max_size: u64,
}

impl Prefetch {
    pub fn new(storage: &Storage) -> Self {
        Self {
            days: storage.prefetch_days,
            max_size: WHOLE_LIMIT,
        }
    }

    /// Whether the prefetch downloads anything.
    pub fn on(&self) -> bool {
        self.days > 0
    }
}

/// A message the prefetch may download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub message: MessageId,
    pub role: FolderRole,
    /// When it arrived (Unix seconds).
    pub date: i64,
    /// RFC822.SIZE.
    pub size: u64,
    /// The store holds its body already.
    pub downloaded: bool,
}

/// What the downloaded bodies take and may take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUse {
    pub used: u64,
    /// The disk cap; `None` for none.
    pub limit: Option<u64>,
}

/// The messages to download in the background: the Inbox's, newest first, then the rest of the
/// Inbox conversations the candidates hold (a reply in Sent, a copy in the Archive), newest
/// first, whatever their age, never from the Trash or Junk; each within its size, while the
/// downloaded bodies stay within [`PREFETCH_SHARE`] of the cap.
pub fn prefetch(policy: &Prefetch, cache: CacheUse, candidates: &[Candidate]) -> Vec<MessageId> {
    if !policy.on() {
        return Vec::new();
    }
    let mut wanted: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| {
            !candidate.downloaded
                && !matches!(candidate.role, FolderRole::Trash | FolderRole::Junk)
                && candidate.size <= policy.max_size
        })
        .collect();
    wanted.sort_by(|a, b| {
        (b.role == FolderRole::Inbox)
            .cmp(&(a.role == FolderRole::Inbox))
            .then(b.date.cmp(&a.date))
            .then(b.message.cmp(&a.message))
    });
    let budget = cache
        .limit
        .map(|limit| (limit / 100 * PREFETCH_SHARE).saturating_sub(cache.used));
    let mut taken = 0u64;
    let mut out = Vec::new();
    for candidate in wanted {
        if budget.is_some_and(|budget| taken + candidate.size > budget) {
            break;
        }
        taken += candidate.size;
        out.push(candidate.message);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000;
    const DAY: i64 = 86_400;

    fn candidate(id: i64, days_ago: i64, size: u64) -> Candidate {
        Candidate {
            message: MessageId(id),
            role: FolderRole::Inbox,
            date: NOW - days_ago * DAY,
            size,
            downloaded: false,
        }
    }

    fn policy(days: u32) -> Prefetch {
        Prefetch::new(&Storage {
            prefetch_days: days,
            ..Storage::default()
        })
    }

    #[test]
    fn the_inbox_comes_first_newest_first_whatever_its_age() {
        let candidates = [
            candidate(1, 20, 1000),
            candidate(2, 1, 1000),
            candidate(3, 400, 1000),
            Candidate {
                role: FolderRole::Sent,
                ..candidate(4, 0, 1000)
            },
            Candidate {
                downloaded: true,
                ..candidate(5, 0, 1000)
            },
            candidate(6, 0, WHOLE_LIMIT + 1),
            candidate(7, 0, WHOLE_LIMIT),
            Candidate {
                role: FolderRole::Archive,
                ..candidate(8, 30, 1000)
            },
            Candidate {
                role: FolderRole::Trash,
                ..candidate(9, 0, 1000)
            },
            Candidate {
                role: FolderRole::Junk,
                ..candidate(10, 0, 1000)
            },
        ];
        let picked = prefetch(&policy(14), CacheUse::default(), &candidates);
        assert_eq!(
            picked,
            [
                MessageId(7),
                MessageId(2),
                MessageId(1),
                MessageId(3),
                MessageId(4),
                MessageId(8)
            ],
            "the Inbox newest first, then its conversations' other messages newest first"
        );
    }

    #[test]
    fn zero_days_turns_it_off() {
        let candidates = [candidate(1, 0, 10)];
        assert!(prefetch(&policy(0), CacheUse::default(), &candidates).is_empty());
        assert!(!policy(0).on());
        assert!(policy(1).on());
    }

    #[test]
    fn prefetching_fills_at_most_half_the_cap() {
        let candidates: Vec<Candidate> = (1..=10).map(|n| candidate(n, n, 10 * MIB)).collect();
        let cache = CacheUse {
            used: 0,
            limit: Some(100 * MIB),
        };
        let picked = prefetch(&policy(30), cache, &candidates);
        assert_eq!(picked.len(), 5, "{picked:?}");
        assert_eq!(picked[0], MessageId(1));
        let fuller = CacheUse {
            used: 45 * MIB,
            ..cache
        };
        assert!(prefetch(&policy(30), fuller, &candidates).is_empty());
        let unlimited = CacheUse {
            used: 1 << 40,
            limit: None,
        };
        assert_eq!(prefetch(&policy(30), unlimited, &candidates).len(), 10);
    }
}
