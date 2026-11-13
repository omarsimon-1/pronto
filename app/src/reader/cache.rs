//! What the reader keeps between conversations so moving back and forth is instant: the
//! messages it prepared (the raw message read and parsed off the GTK thread, its body chosen,
//! its inline images decoded), by message and the stored raw message they came from, in a
//! bytes-capped LRU; and the heights its HTML bodies measured, by message, column width and
//! everything else that sets a height, so a view attaches at its final height and the page has
//! its final geometry before WebKit answers. Both are dropped for messages the store removed;
//! the prepared ones also when the raw message changed (a body downloaded or evicted), the
//! heights when the theme changed. GTK thread only, like the reader.

use super::prepare::{Body, Prepared};
use sealwax_domain::model::{BlobKey, MessageId};
use sealwax_domain::theme::BodyStyle;
use std::collections::{HashMap, VecDeque};

/// The most bytes of prepared messages kept (pages laid out ahead hold the conversations next
/// to the selection already; this keeps the ones seen a little earlier, close to ADR-011's
/// growth bar: about 20 MB of process growth over 200 opens at 8 MiB, measured).
pub const PREPARED_CAP: usize = 8 * 1024 * 1024;
/// A prepared message larger than this isn't kept.
pub const PREPARED_LARGEST: usize = 4 * 1024 * 1024;
/// The most heights kept.
pub const HEIGHTS_CAP: usize = 2048;

/// One message as the reader prepared it.
#[derive(Clone)]
pub struct Entry {
    /// The stored raw message it was prepared from: another one (downloaded since, or gone)
    /// means it is stale.
    pub blob: BlobKey,
    pub prepared: Prepared,
    bytes: usize,
}

/// What `prepared` holds, roughly, in bytes.
pub fn size(prepared: &Prepared) -> usize {
    let body = match &prepared.body {
        Body::Html { html, parts, .. } => html.len() + parts.bytes(),
        Body::Plain(text) => text.len(),
        Body::Empty | Body::NotDownloaded | Body::Unreadable(_) => 0,
    };
    body + prepared.attachments.len() * 256 + 1024
}

/// Whether [`PreparedCache::put`] keeps `prepared`: one larger than [`PREPARED_LARGEST`] isn't.
pub fn keeps(prepared: &Prepared) -> bool {
    size(prepared) <= PREPARED_LARGEST
}

/// Prepared messages, least recently used first.
#[derive(Default)]
pub struct PreparedCache {
    entries: VecDeque<(MessageId, Entry)>,
    bytes: usize,
}

impl PreparedCache {
    /// Message `id` as prepared from the raw message `blob`, counting as a use; `None` when it
    /// isn't kept, or was prepared from another raw message.
    pub fn get(&mut self, id: MessageId, blob: BlobKey) -> Option<Entry> {
        let at = self.entries.iter().position(|(known, _)| *known == id)?;
        let (_, entry) = self.entries.remove(at)?;
        if entry.blob != blob {
            self.bytes -= entry.bytes;
            return None;
        }
        let found = entry.clone();
        self.entries.push_back((id, entry));
        Some(found)
    }

    /// Whether message `id` is kept (prepared from any raw message), without counting as a use.
    pub fn contains(&self, id: MessageId) -> bool {
        self.entries.iter().any(|(known, _)| *known == id)
    }

    /// Keeps `prepared` for message `id`, dropping the least recently used to stay in
    /// [`PREPARED_CAP`]; one larger than [`PREPARED_LARGEST`] isn't kept.
    pub fn put(&mut self, id: MessageId, blob: BlobKey, prepared: Prepared) {
        self.forget(&[id]);
        if !keeps(&prepared) {
            return;
        }
        let bytes = size(&prepared);
        while self.bytes + bytes > PREPARED_CAP {
            let Some((_, dropped)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= dropped.bytes;
        }
        self.bytes += bytes;
        self.entries.push_back((
            id,
            Entry {
                blob,
                prepared,
                bytes,
            },
        ));
    }

    /// Drops what is kept of `messages`.
    pub fn forget(&mut self, messages: &[MessageId]) {
        if messages.is_empty() {
            return;
        }
        self.entries.retain(|(id, _)| !messages.contains(id));
        self.bytes = self.entries.iter().map(|(_, entry)| entry.bytes).sum();
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// What a body's height depends on besides its message: the column's width (the reader's
/// stage), how it is shown, its remote images, its quotes collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeightKey {
    pub message: MessageId,
    pub column: i32,
    pub style: BodyStyle,
    pub remote_allowed: bool,
    pub collapse_quotes: bool,
}

/// A measured height, and the width of the view it was measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measured {
    pub height: i32,
    pub width: i32,
}

/// Heights HTML bodies measured, least recently stored first.
#[derive(Default)]
pub struct Heights {
    heights: HashMap<HeightKey, Measured>,
    order: VecDeque<HeightKey>,
}

impl Heights {
    pub fn get(&self, key: &HeightKey) -> Option<Measured> {
        self.heights.get(key).copied()
    }

    pub fn put(&mut self, key: HeightKey, measured: Measured) {
        if self.heights.insert(key, measured).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > HEIGHTS_CAP {
            if let Some(old) = self.order.pop_front() {
                self.heights.remove(&old);
            }
        }
    }

    /// Drops the heights of `messages`.
    pub fn forget(&mut self, messages: &[MessageId]) {
        if messages.is_empty() {
            return;
        }
        self.heights
            .retain(|key, _| !messages.contains(&key.message));
        self.order.retain(|key| !messages.contains(&key.message));
    }

    /// Drops every height (the theme changed: fonts and sizes with it).
    pub fn clear(&mut self) {
        self.heights.clear();
        self.order.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.heights.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::engine::InlineParts;

    fn plain(bytes: usize) -> Prepared {
        Prepared {
            body: Body::Plain("x".repeat(bytes)),
            attachments: Vec::new(),
            signed: None,
        }
    }

    #[test]
    fn a_prepared_message_comes_back_until_its_raw_message_changes_or_it_is_gone() {
        let mut cache = PreparedCache::default();
        let (one, two) = (BlobKey([1; 32]), BlobKey([2; 32]));
        cache.put(MessageId(1), one, plain(10));
        cache.put(MessageId(2), one, plain(10));
        assert!(cache.get(MessageId(1), one).is_some());
        assert!(
            cache.get(MessageId(1), two).is_none(),
            "a body downloaded again is another raw message"
        );
        assert!(!cache.contains(MessageId(1)), "the stale one is dropped");
        cache.forget(&[MessageId(2)]);
        assert!(cache.get(MessageId(2), one).is_none());
        assert_eq!((cache.len(), cache.bytes()), (0, 0));
        let html = Prepared {
            body: Body::Html {
                html: "<p>hi</p>".into(),
                style: BodyStyle::Themed,
                remote_images: false,
                parts: InlineParts::default(),
            },
            attachments: Vec::new(),
            signed: None,
        };
        assert_eq!(size(&html), 9 + 1024);
    }

    #[test]
    fn a_kept_message_shares_its_html_rather_than_copying_it() {
        let mut cache = PreparedCache::default();
        let blob = BlobKey([3; 32]);
        let prepared = Prepared {
            body: Body::Html {
                html: "<p>hello</p>".repeat(1000).into(),
                style: BodyStyle::Themed,
                remote_images: false,
                parts: InlineParts::default(),
            },
            attachments: Vec::new(),
            signed: None,
        };
        let Body::Html { html: put, .. } = &prepared.body else {
            unreachable!()
        };
        let put = put.clone();
        cache.put(MessageId(9), blob, prepared);
        for _ in 0..2 {
            let entry = cache.get(MessageId(9), blob).expect("kept");
            let Body::Html { html, .. } = &entry.prepared.body else {
                panic!("not HTML");
            };
            assert!(std::sync::Arc::ptr_eq(html, &put), "a hit copied the HTML");
        }
    }

    #[test]
    fn the_least_recently_used_go_first_and_huge_messages_are_not_kept() {
        let mut cache = PreparedCache::default();
        let blob = BlobKey([7; 32]);
        let each = PREPARED_CAP / 4 - 2048;
        for id in 0..4 {
            cache.put(MessageId(id), blob, plain(each));
        }
        assert_eq!(cache.len(), 4);
        assert!(
            cache.get(MessageId(0), blob).is_some(),
            "used: now the newest"
        );
        cache.put(MessageId(4), blob, plain(each));
        assert!(
            !cache.contains(MessageId(1)),
            "the least recently used went"
        );
        assert!(cache.contains(MessageId(0)));
        assert!(cache.bytes() <= PREPARED_CAP);
        cache.put(MessageId(9), blob, plain(PREPARED_LARGEST + 1));
        assert!(!cache.contains(MessageId(9)));
        // What the reader asks before copying a body onto the GTK thread for the cache.
        assert!(!keeps(&plain(PREPARED_LARGEST + 1)));
        assert!(keeps(&plain(PREPARED_LARGEST - 2048)));
    }

    #[test]
    fn heights_are_kept_per_width_and_way_of_showing() {
        let mut heights = Heights::default();
        let key = HeightKey {
            message: MessageId(1),
            column: 800,
            style: BodyStyle::Themed,
            remote_allowed: false,
            collapse_quotes: true,
        };
        heights.put(
            key,
            Measured {
                height: 420,
                width: 760,
            },
        );
        assert_eq!(heights.get(&key).map(|m| m.height), Some(420));
        let wider = HeightKey {
            column: 1200,
            ..key
        };
        let images = HeightKey {
            remote_allowed: true,
            ..key
        };
        assert_eq!(heights.get(&wider), None);
        assert_eq!(heights.get(&images), None);
        heights.forget(&[MessageId(1)]);
        assert_eq!(heights.get(&key), None);
        for id in 0..(HEIGHTS_CAP as i64 + 10) {
            heights.put(
                HeightKey {
                    message: MessageId(id),
                    ..key
                },
                Measured {
                    height: 1,
                    width: 1,
                },
            );
        }
        assert_eq!(heights.len(), HEIGHTS_CAP);
        heights.clear();
        assert_eq!(heights.len(), 0);
    }
}
