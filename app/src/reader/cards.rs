//! An expanded message's card: the message read off the GTK thread, its body chosen and
//! prepared (`prepare`; kept in [`super::cache`], so a message shown again is only checked
//! against the store), then the card built with its header, banner, body and attachment cards
//! and swapped in for the message's row. A click on a row expands it, a click on a card's header
//! collapses it (the only expanded message stays), "N more messages" reveals a step, and
//! Expand All and Collapse All act on the whole conversation. Each HTML body gets a view from
//! [`super::live`], at the height it measured before when it did; its events arrive in
//! [`Inner::body_event`].

use super::cache::{self, HeightKey, Measured};
use super::conversation::{Card, Entry, HtmlBody, Item, Page, redate_row, reveal};
use super::page::{self, Header};
use super::prepare::{self, Body, Prepared};
use super::remote::Notice;
use super::{
    Inner, READING_MEASURE, Reader, ShownBody, ViewEvent, download_failure, now, plain, protection,
    rgba, stylesheet, widgets,
};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use sealwax_domain::model::{FolderRole, MessageDetail, MessageId};
use sealwax_domain::theme::BodyStyle;
use sealwax_engine::api::BodyError;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// Expand All reveals the folds of a conversation up to this size; a longer one keeps them
/// folded (a hundred cards at once would hold the column up).
const EXPAND_FOLDED_UP_TO: usize = 100;

/// What a card is built from: the message, the user's addresses, its body prepared, and what
/// it shows instead while the body isn't stored.
struct Read {
    detail: MessageDetail,
    mine: Vec<String>,
    prepared: Prepared,
    download: Option<ShownBody>,
}

impl Inner {
    /// What a body's height measured now is kept by: the message, the column's width and how it
    /// is shown; `None` while the reader has no width.
    pub(super) fn height_key(
        &self,
        id: MessageId,
        style: BodyStyle,
        remote_allowed: bool,
        collapse_quotes: bool,
    ) -> Option<HeightKey> {
        let column = self.stage.width();
        (column > 1).then_some(HeightKey {
            message: id,
            column,
            style,
            remote_allowed,
            collapse_quotes,
        })
    }

    /// Whether load `token` of message `id` is still wanted on page `key`.
    fn wanted_load(&self, key: u64, id: MessageId, token: u64) -> bool {
        self.with_page(key, |page| {
            page.item(id)
                .is_some_and(|item| item.expanded && item.load == token)
        })
        .unwrap_or(false)
    }

    /// Loads message `id` of page `key` again (its body arrived, or Retry).
    pub(super) fn reload_card(self: &Rc<Self>, key: u64, id: MessageId) {
        let token = self.next_token();
        let wanted = self
            .with_page(key, |page| {
                page.item_mut(id).is_some_and(|item| {
                    item.expanded = true;
                    item.load = token;
                    true
                })
            })
            .unwrap_or(false);
        if wanted {
            let inner = self.clone();
            glib::spawn_future_local(async move { inner.load_card(key, id, token).await });
        }
    }

    /// Reads message `id` and builds its card on page `key`, in place of its row or of the card
    /// it had. A newer load of the message, or a collapse, drops this one.
    pub(super) async fn load_card(self: Rc<Self>, key: u64, id: MessageId, token: u64) {
        let Some(read) = self.read_card(key, id, token).await else {
            return;
        };
        let Some(Read {
            detail,
            mine,
            prepared,
            download,
        }) = read
        else {
            self.install_missing(key, id, token);
            return;
        };
        if !self.wanted_load(key, id, token) {
            return;
        }
        let draft = self
            .with_page(key, |page| {
                page.item(id)
                    .is_some_and(|item| item.message.role == FolderRole::Drafts)
            })
            .unwrap_or(false);
        let header = Header {
            detail: &detail,
            mine: &mine,
            now: now(),
            draft,
        };
        let sender = detail
            .headers
            .from
            .first()
            .map(|from| from.email.to_lowercase())
            .unwrap_or_default();
        let remote_images = matches!(
            &prepared.body,
            Body::Html {
                remote_images: true,
                ..
            }
        );
        let reader = Reader {
            inner: self.clone(),
        };
        let notice = self.remote.notice(id, &sender, remote_images);
        let widgets = page::card(&reader, &header, &prepared.attachments, notice);
        let replies = self.reply_buttons(&widgets.actions, id, draft);
        let weak = Rc::downgrade(&self);
        widgets.header.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade()
                && inner.is_shown(key)
            {
                inner.collapse(id);
            }
        });
        if let Some(banner) = &widgets.banner {
            let weak = Rc::downgrade(&self);
            banner.show_images.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.allow_remote(id);
                }
            });
        }
        let mut card = Card {
            widgets,
            body: ShownBody::Empty,
            html: None,
            attachments: prepared.attachments.len(),
            sender,
            remote_images,
            replies,
            headers: detail.headers.clone(),
        };
        match download {
            Some(state) => self.download_body(&mut card, &detail, state, key, id),
            None => self.fill_body(&mut card, id, prepared).await,
        }
        if !self.wanted_load(key, id, token) {
            return;
        }
        self.install(key, id, card);
        // Show Images follows the message the user is on, whose card may be this one.
        self.remote.changed();
    }

    /// What message `id`'s card is built from: from what the reader prepared before when the
    /// store still holds the raw message it came from, else read and prepared off the GTK
    /// thread (and kept). `None` when the load isn't wanted any more; `Some(None)` when the
    /// message is gone.
    async fn read_card(
        self: &Rc<Self>,
        key: u64,
        id: MessageId,
        token: u64,
    ) -> Option<Option<Read>> {
        let known = self.prepared.borrow().contains(id) && !self.mine.borrow().is_empty();
        if known && let Some(reply) = self.store.detail(id) {
            let detail = reply
                .recv()
                .await
                .ok()
                .and_then(|versioned| versioned.value);
            if !self.wanted_load(key, id, token) {
                return None;
            }
            let Some(detail) = detail else {
                return Some(None);
            };
            let entry = detail
                .blob
                .and_then(|blob| self.prepared.borrow_mut().get(id, blob));
            if let Some(entry) = entry {
                self.answers.borrow_mut().remove(&id);
                return Some(Some(Read {
                    detail,
                    mine: self.mine.borrow().clone(),
                    prepared: entry.prepared,
                    download: None,
                }));
            }
        }
        let Some(reads) = self.store.read(id) else {
            return Some(None);
        };
        let detail = reads.detail.recv().await;
        let accounts = reads.accounts.recv().await;
        let raw = reads.raw.recv().await.ok().flatten();
        if !self.wanted_load(key, id, token) {
            return None;
        }
        let detail = match detail {
            Ok(versioned) => versioned.value,
            Err(error) => {
                tracing::warn!(message = id.0, %error, "reader: the message couldn't be read");
                None
            }
        };
        let Some(detail) = detail else {
            return Some(None);
        };
        let mine: Vec<String> = accounts
            .map(|accounts| {
                accounts
                    .value
                    .into_iter()
                    .map(|account| account.address)
                    .collect()
            })
            .unwrap_or_default();
        if !mine.is_empty() {
            self.mine
                .replace(mine.iter().map(|address| address.to_lowercase()).collect());
        }
        let answer = self.answers.borrow_mut().remove(&id);
        let download = match &raw {
            None => self.download_state(answer),
            Some(_) => None,
        };
        let stored = raw.is_some();
        let for_worker = detail.clone();
        let mut prepared =
            gio::spawn_blocking(move || prepare::prepare(&for_worker, raw.as_deref()))
                .await
                .unwrap_or_else(|_| {
                    tracing::error!(message = id.0, "reader: preparing the message failed");
                    Prepared {
                        body: Body::Empty,
                        attachments: Vec::new(),
                        signed: None,
                    }
                });
        // The HTML the cache keeps, shared with the card, is copied once onto this thread's
        // heap: long-lived, it would otherwise sit in the worker thread's arena among its
        // short-lived allocations and keep that arena from shrinking. (Not measurable apart
        // from the noise in headless `reader_memory` runs on a shared machine.) A body the
        // cache won't keep (over `PREPARED_LARGEST`, 4 MiB) isn't copied: it was, about 1 ms
        // on this thread for 5 MiB, and then dropped by the cache.
        if stored && let Some(blob) = detail.blob {
            if cache::keeps(&prepared) {
                if let Body::Html { html, .. } = &mut prepared.body {
                    *html = Arc::from(&**html);
                }
                self.prepared.borrow_mut().put(id, blob, prepared.clone());
            } else {
                self.prepared.borrow_mut().forget(&[id]);
            }
        }
        Some(Some(Read {
            detail,
            mine,
            prepared,
            download,
        }))
    }

    /// What a message whose body isn't stored shows (ML-044): `None` without a fetcher (M1's
    /// "not downloaded yet"), else the engine's failure, or "Downloading…" (the reader asks for
    /// it once the card is on its page).
    fn download_state(&self, answer: Option<Result<(), BodyError>>) -> Option<ShownBody> {
        self.fetcher.borrow().as_ref()?;
        Some(match answer {
            Some(Err(error)) => ShownBody::DownloadFailed(error),
            // Answered, and still no body: asking again would only loop.
            Some(Ok(())) => ShownBody::DownloadFailed(BodyError::Other),
            None => ShownBody::Downloading,
        })
    }

    /// The snippet under a quiet "Downloading…", or under why the download failed with Retry.
    fn download_body(
        self: &Rc<Self>,
        card: &mut Card,
        detail: &MessageDetail,
        state: ShownBody,
        key: u64,
        id: MessageId,
    ) {
        if let ShownBody::DownloadFailed(error) = &state {
            let row = gtk::Box::builder().spacing(12).build();
            let notice = page::notice(download_failure(*error));
            notice.set_hexpand(true);
            row.append(&notice);
            let retry = gtk::Button::builder()
                .label("Retry")
                .valign(gtk::Align::Center)
                .css_classes(["sw-control"])
                .build();
            retry.set_widget_name("reader-retry");
            let weak = Rc::downgrade(self);
            retry.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.answers.borrow_mut().remove(&id);
                    inner.reload_card(key, id);
                }
            });
            row.append(&retry);
            card.widgets.body.append(&row);
        } else {
            card.widgets.body.append(&page::notice("Downloading…"));
        }
        let snippet = &detail.summary.snippet;
        if !snippet.is_empty() {
            let snippet = plain::plain_text(snippet);
            card.widgets
                .body
                .append(&widgets::ReadingBox::new(&snippet, READING_MEASURE));
            card.widgets.focus = Some(snippet.upcast());
        }
        card.body = state;
    }

    /// The card's body, where the reader builds every body: plain text in a text view at a
    /// reading measure, HTML in a frame whose view the live views give it (as tall as it was
    /// last measured at this width, when it was), or a notice (an encrypted message's among
    /// them), under the quiet line of a signed message. The quoted history collapses behind
    /// "•••" (DESIGN.md: always; a message whose text is all quote keeps it).
    async fn fill_body(self: &Rc<Self>, card: &mut Card, id: MessageId, prepared: Prepared) {
        let collapse_quotes = true;
        if let Some(signature) = prepared.signed {
            card.widgets
                .body
                .append(&protection::signed_note(signature));
        }
        match prepared.body {
            Body::Html {
                html,
                style,
                remote_images,
                parts,
            } => {
                let crashes = self.crashed.borrow().get(&id).copied().unwrap_or(0);
                if crashes >= 2 || self.filter.borrow().is_none() {
                    self.text_fallback(card, &html).await;
                    return;
                }
                let frame = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .overflow(gtk::Overflow::Hidden)
                    .css_classes([match style {
                        BodyStyle::Paper => "sw-paper",
                        BodyStyle::Themed => "sw-themed-body",
                    }])
                    .build();
                frame.set_widget_name("reader-frame");
                let holder = widgets::BodyBox::empty();
                frame.append(&holder);
                card.widgets.body.append(&frame);
                let remote_allowed = self.remote.allows(id, &card.sender);
                let measured = self
                    .height_key(id, style, remote_allowed, collapse_quotes)
                    .and_then(|key| self.heights.borrow().get(&key));
                holder.set_placeholder(measured.map_or(1, |measured| measured.height));
                card.body = ShownBody::Html {
                    style,
                    remote_images,
                    remote_allowed,
                    inline_parts: parts.len(),
                };
                card.html = Some(HtmlBody {
                    frame,
                    holder,
                    view: None,
                    style,
                    html,
                    parts: Rc::new(parts),
                    remote_images,
                    remote_allowed,
                    collapse_quotes,
                    quotes_open: false,
                    measured: measured.map(|measured| (measured.height, measured.width)),
                    loaded: false,
                    painted: false,
                    changed: Instant::now(),
                    released: false,
                    token: 0,
                });
            }
            Body::Plain(text) => {
                let text = plain::message_text(&text, &self.links, &self.theme, collapse_quotes);
                card.widgets
                    .body
                    .append(&widgets::ReadingBox::new(&text, READING_MEASURE));
                card.widgets.focus = Some(text.upcast());
                card.body = ShownBody::Plain;
            }
            Body::Unreadable(unreadable) => {
                card.widgets.body.append(&protection::notice(unreadable));
                card.body = ShownBody::Unreadable(unreadable);
            }
            Body::Empty => {
                card.widgets
                    .body
                    .append(&page::notice("This message has no text."));
                card.body = ShownBody::Empty;
            }
            Body::NotDownloaded => {
                card.widgets.body.append(&page::notice(
                    "The rest of this message hasn't been downloaded yet.",
                ));
                card.body = ShownBody::NotDownloaded;
            }
        }
    }

    /// The HTML as text, when no WebView may show it.
    async fn text_fallback(&self, card: &mut Card, html: &str) {
        let html = html.to_owned();
        let text = gio::spawn_blocking(move || sealwax_domain::message::html_to_text(&html))
            .await
            .unwrap_or_default();
        card.widgets.body.append(&page::notice(
            "This message is shown as text: its formatted view isn't available.",
        ));
        let text = plain::plain_text(&text);
        card.widgets
            .body
            .append(&widgets::ReadingBox::new(&text, READING_MEASURE));
        card.widgets.focus = Some(text.upcast());
        card.body = ShownBody::TextFallback;
    }

    /// A message the store no longer holds keeps its row, with a notice under it.
    fn install_missing(self: &Rc<Self>, key: u64, id: MessageId, token: u64) {
        if !self.wanted_load(key, id, token) {
            return;
        }
        tracing::debug!(
            message = id.0,
            "reader: a message of the conversation is gone"
        );
        let notice = page::notice("This message is no longer here.");
        self.with_page(key, |page| {
            if let Some(item) = page.item_mut(id) {
                item.expanded = false;
                if item.row.parent().is_none() {
                    item.slot.append(&item.row);
                }
                item.slot.append(&notice);
            }
        });
    }

    /// Puts `card` on page `key` in place of the message's row or earlier card; a focus held
    /// there moves into the new card.
    fn install(self: &Rc<Self>, key: u64, id: MessageId, card: Card) {
        let shown = self.is_shown(key);
        if shown {
            self.hold_scroll();
        }
        let target: gtk::Widget = card.widgets.header.clone().upcast();
        let old = self.with_page(key, |page| {
            let item = page.item_mut(id)?;
            let mut old_widgets = Vec::new();
            let mut child = item.slot.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                old_widgets.push(current);
            }
            item.slot.append(&card.widgets.root);
            for old in old_widgets {
                widgets::hand_over_focus(&old, Some(&target));
                item.slot.remove(&old);
            }
            item.card
                .replace(card)
                .and_then(|mut old| old.html.as_mut()?.view.take())
        });
        if let Some(Some(view)) = old {
            self.engine.release(view);
        }
        self.focus_loaded_body(key, id);
        if shown {
            self.request_bodies(key);
            self.report_opened(key);
        }
        self.schedule();
        if shown && self.with_page(key, |page| page.selected == id) == Some(true) {
            self.remote.changed();
        }
    }

    /// Expands message `id` of the shown conversation: its card replaces its row once read.
    pub(super) fn expand(self: &Rc<Self>, id: MessageId) {
        let token = self.next_token();
        let key = self.shown.borrow_mut().as_mut().and_then(|page| {
            let item = page.item_mut(id).filter(|item| !item.expanded)?;
            item.expanded = true;
            item.load = token;
            Some(page.key)
        });
        if let Some(key) = key {
            let inner = self.clone();
            glib::spawn_future_local(async move { inner.load_card(key, id, token).await });
        }
    }

    /// Collapses message `id` of the shown conversation to its row, unless it is the only
    /// message expanded.
    pub(super) fn collapse(self: &Rc<Self>, id: MessageId) {
        let only = self
            .shown
            .borrow()
            .as_ref()
            .is_none_or(|page| page.expanded().len() <= 1);
        if only {
            return;
        }
        self.hold_scroll();
        let view = self.shown.borrow_mut().as_mut().and_then(|page| {
            let item = page.item_mut(id).filter(|item| item.expanded)?;
            collapse_item(item)
        });
        if let Some(view) = view {
            self.engine.release(view);
        }
        self.schedule();
    }

    /// Expand All: every message expands, folded ones too (the folds are revealed first), but in
    /// a conversation over [`EXPAND_FOLDED_UP_TO`] messages folded ones stay folded.
    pub(super) fn expand_all(self: &Rc<Self>) {
        let Some((size, selected)) = self
            .shown
            .borrow()
            .as_ref()
            .map(|page| (page.message_count(), page.selected))
        else {
            return;
        };
        self.hold_scroll_at(selected);
        if size <= EXPAND_FOLDED_UP_TO {
            while let Some(first) = self.first_fold() {
                self.unfold_whole(first, true);
            }
        }
        let ids: Vec<MessageId> = self
            .shown
            .borrow()
            .as_ref()
            .map(|page| {
                page.items()
                    .filter(|item| !item.expanded)
                    .map(Item::id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.expand(id);
        }
    }

    /// Next (`forward`) or Previous Message in Conversation: the message after or before the one
    /// the user last moved to expands, and the column scrolls to it; before any move, from the
    /// message being read, whose body holds the keyboard focus (after a send the reader puts it
    /// in the answered one's: `p` skipped the message above it, from the selected one), else
    /// the selected one. A row holding the focus isn't read (a click on a fold's row leaves it
    /// on a row it revealed). Folded messages are passed over. A keyboard focus in the
    /// conversation goes along (as in a Focused conversation, where opening it focuses the
    /// selected message): Reply answers the message the user moved to, the inline reply says
    /// so, and a screen reader reads it.
    pub(super) fn step(self: &Rc<Self>, forward: bool) {
        let focus = self.stage.root().and_then(|root| root.focus());
        let target = self.shown.borrow_mut().as_mut().and_then(|page| {
            let ids: Vec<MessageId> = page.items().map(Item::id).collect();
            let focused = focus.as_ref().and_then(|focus| reading(page, focus));
            let current = page
                .current
                .filter(|id| ids.contains(id))
                .or(focused)
                .unwrap_or(page.selected);
            let at = ids.iter().position(|id| *id == current)?;
            // Next goes down the screen: to an older message when the newest is on top.
            let next = if forward != page.newest_first {
                ids.get(at + 1).copied()
            } else {
                at.checked_sub(1).map(|at| ids[at])
            }?;
            page.current = Some(next);
            Some((page.key, next))
        });
        if let Some((key, id)) = target {
            self.expand(id);
            self.scroll_to_message(id);
            self.focus_reached(id);
            self.follow_choice(key);
            let listeners = self.on_moved.borrow().clone();
            for listener in listeners {
                listener(id);
            }
        }
    }

    /// The user moved to message `id` of page `key` (a press in it, Tab into it from another
    /// of its messages): Reply answers it and `n`/`p` go on from it ([`Inner::follow_choice`]),
    /// the selected message as any other (a press in Ingrid's first unread message, where the
    /// trip opened, answers her). Where the reader puts the focus itself is no move
    /// ([`Inner::give_focus`]): the conversation opens focused in its first unread message
    /// while Reply answers the latest (phase C's visual QA: it answered the first unread one
    /// while the line under the conversation named the latest's sender), and the focus it gave
    /// the answered message after a send kept the reply box there when a newer reply came in
    /// (phase C's review).
    pub(super) fn moved_to(self: &Rc<Self>, key: u64, id: MessageId) {
        let moved = self
            .shown
            .borrow_mut()
            .as_mut()
            .filter(|page| page.key == key && page.item(id).is_some())
            .is_some_and(|page| page.current.replace(id) != Some(id));
        if moved {
            self.follow_choice(key);
            let listeners = self.on_moved.borrow().clone();
            for listener in listeners {
                listener(id);
            }
        }
    }

    /// The reader puts the keyboard focus on `target`, in message `id` of page `key`: no move of
    /// the user's, [`Inner::focus_went`] finds it where the reader put it. `false` when it
    /// couldn't take it.
    pub(super) fn give_focus(&self, key: u64, id: MessageId, target: &gtk::Widget) -> bool {
        // Noted once moved: the focus is read in an idle, after this.
        let given = target.grab_focus();
        if given {
            self.focus_in.set(Some((key, id)));
        }
        given
    }

    /// [`Inner::moved_to`] once the event that moved has been handled, before the next one: a
    /// press's handlers may still expand the message.
    pub(super) fn move_after_event(self: &Rc<Self>, key: u64, id: MessageId) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_full(glib::Priority::HIGH, move || {
            if let Some(inner) = weak.upgrade()
                && inner.is_shown(key)
            {
                inner.moved_to(key, id);
            }
            glib::ControlFlow::Break
        });
    }

    /// The keyboard focus went to `focus` ([`Inner::follow_focus`]). From one message of the
    /// shown page to another, the user moved there (Tab, Shift+Tab, WebKit's own Tab out of a
    /// body), through a fold's row ("3 earlier messages") between them too; from elsewhere
    /// (the list, the inline reply, a popover, a message the reader took off the page) it is
    /// no move ([`Inner::moved_to`]), nor where the reader put it ([`Inner::give_focus`]).
    pub(super) fn focus_went(self: &Rc<Self>, focus: Option<&gtk::Widget>) {
        let moved = {
            let shown = self.shown.borrow();
            let Some(page) = shown.as_ref() else {
                self.focus_in.set(None);
                return;
            };
            let on_fold = focus.is_some_and(|focus| {
                page.entries.iter().any(|entry| {
                    matches!(entry, Entry::Fold(fold) if fold.row.upcast_ref::<gtk::Widget>() == focus)
                })
            });
            if on_fold {
                return;
            }
            let now = focus
                .filter(|focus| focus.is_ancestor(&page.root))
                .and_then(|focus| holding(page, focus))
                .map(|id| (page.key, id));
            let before = self
                .focus_in
                .replace(now)
                .filter(|(key, id)| *key == page.key && page.item(*id).is_some());
            match (before, now) {
                (Some((_, left)), Some((key, id))) if left != id => Some((key, id)),
                _ => None,
            }
        };
        if let Some((key, id)) = moved {
            self.moved_to(key, id);
        }
    }

    /// The keyboard focus, when it is on a message of the shown conversation, moves to message
    /// `id`: its card's body or header, else its row. A header holds it only while the body
    /// loads: the body takes it then ([`Inner::focus_loaded_body`]), as when it was loaded
    /// already (before, the header kept it, its focus ring drawn).
    fn focus_reached(&self, id: MessageId) {
        let focus = self.stage.root().and_then(|root| root.focus());
        let target = self.shown.borrow().as_ref().and_then(|page| {
            let focus = focus?;
            page.items()
                .any(|item| focus.is_ancestor(&item.slot))
                .then(|| page.item(id))
                .flatten()
                .and_then(|item| match &item.card {
                    Some(card) => {
                        let loading = card.html.as_ref().is_some_and(|html| html.view.is_none());
                        card.focus_target()
                            .map(|target| (target, page.key, loading))
                    }
                    // Its card is being read: the row hands the focus to its header
                    // (`install`), and the body takes it from there.
                    None => Some((item.row.clone().upcast(), page.key, item.expanded)),
                })
        });
        self.focus_body.set(None);
        if let Some((target, key, loading)) = target {
            self.give_focus(key, id, &target);
            if loading {
                self.focus_body.set(Some((key, id)));
            }
        }
    }

    /// Message `id`'s card on page `key` is in, or its body loaded: when `n` or `p` left the
    /// focus on its row or header meanwhile ([`Inner::focus_reached`]) and it is there still,
    /// the body takes it once there is one.
    pub(super) fn focus_loaded_body(&self, key: u64, id: MessageId) {
        if self.focus_body.get() != Some((key, id)) {
            return;
        }
        let focus = self.stage.root().and_then(|root| root.focus());
        // `None` while the body loads; then the body, if the focus waits for it still.
        let found = self
            .with_page(key, |page| {
                let card = page.item(id)?.card.as_ref()?;
                if card.html.as_ref().is_some_and(|html| html.view.is_none()) {
                    return None;
                }
                let header: &gtk::Widget = card.widgets.header.upcast_ref();
                let waits = focus.as_ref().is_some_and(|focus| {
                    focus == header
                        || focus.is_ancestor(header)
                        || card.widgets.focus.as_ref() == Some(focus)
                });
                Some(waits.then(|| card.focus_target()).flatten())
            })
            .flatten();
        let Some(body) = found else {
            return;
        };
        self.focus_body.set(None);
        if let Some(body) = body {
            body.grab_focus();
        }
    }

    /// Collapse All: every message but the selected one becomes a row.
    pub(super) fn collapse_all(self: &Rc<Self>) {
        let Some(selected) = self.shown.borrow().as_ref().map(|page| page.selected) else {
            return;
        };
        self.expand(selected);
        self.hold_scroll();
        let views: Vec<_> = self
            .shown
            .borrow_mut()
            .as_mut()
            .map(|page| {
                page.items_mut()
                    .filter(|item| item.expanded && item.id() != selected)
                    .filter_map(collapse_item)
                    .collect()
            })
            .unwrap_or_default();
        for view in views {
            self.engine.release(view);
        }
        self.schedule();
    }

    /// Reveals one step of the fold that starts with message `first`: its newest messages, as
    /// rows under it; the fold goes once it is empty.
    pub(super) fn unfold(self: &Rc<Self>, first: MessageId) {
        self.unfold_whole(first, false);
    }

    /// The first message of the shown conversation's first fold.
    fn first_fold(&self) -> Option<MessageId> {
        self.shown.borrow().as_ref().and_then(|page| {
            page.entries.iter().find_map(|entry| match entry {
                Entry::Fold(fold) => fold.messages.first().map(|message| message.summary.id),
                Entry::Message(_) => None,
            })
        })
    }

    /// Reveals a step of the fold that starts with message `first`, or all of it (`whole`).
    fn unfold_whole(self: &Rc<Self>, first: MessageId, whole: bool) {
        let Some(key) = self.shown.borrow().as_ref().map(|page| page.key) else {
            return;
        };
        let taken = self.with_page(key, |page| {
            let index = page.entries.iter().position(|entry| {
                matches!(entry, Entry::Fold(fold)
                    if fold.messages.first().is_some_and(|message| message.summary.id == first))
            })?;
            let Entry::Fold(fold) = &mut page.entries[index] else {
                return None;
            };
            let all = 0..fold.messages.len();
            let range = if whole { all } else { reveal(all) };
            let revealed: Vec<_> = fold.messages.drain(range).collect();
            Some((index, revealed))
        });
        let Some(Some((index, revealed))) = taken else {
            return;
        };
        let items: Vec<Item> = revealed
            .iter()
            .map(|message| self.item(key, message, false))
            .collect();
        self.with_page(key, |page| {
            let Entry::Fold(fold) = &page.entries[index] else {
                return;
            };
            let mut after: gtk::Widget = fold.row.clone().upcast();
            let empty = fold.messages.is_empty();
            for item in &items {
                page.place(&item.slot, Some(&after));
                after = item.slot.clone().upcast();
            }
            let rest = page.entries.split_off(index + 1);
            page.entries
                .extend(items.into_iter().map(|item| Entry::Message(Box::new(item))));
            page.entries.extend(rest);
            if empty {
                if let Entry::Fold(fold) = page.entries.remove(index) {
                    // A focus on the fold's row goes to the first row it revealed (its slot,
                    // a box, can't take it: the focus fell out of the reader to the list).
                    let (next, first) = match page.entries.get(index) {
                        Some(Entry::Message(item)) => (
                            Some(
                                item.card
                                    .as_ref()
                                    .and_then(Card::focus_target)
                                    .unwrap_or_else(|| item.row.clone().upcast()),
                            ),
                            Some(item.id()),
                        ),
                        Some(Entry::Fold(next)) => (Some(next.row.clone().upcast()), None),
                        None => (None, None),
                    };
                    // The reader hands it over: no move of the user's ([`Inner::give_focus`]),
                    // though the fold's row stood between two messages ([`Inner::focus_went`]).
                    if widgets::hand_over_focus(&fold.row, next.as_ref())
                        && let Some(first) = first
                    {
                        self.focus_in.set(Some((key, first)));
                    }
                    page.list.remove(&fold.row);
                }
            } else if let Entry::Fold(fold) = &page.entries[index] {
                super::show::update_fold(fold);
            }
        });
        self.size_changed();
        self.schedule();
    }

    /// Reveals a step of the shown conversation's first fold.
    pub(super) fn unfold_first(self: &Rc<Self>) {
        if let Some(first) = self.first_fold() {
            self.unfold(first);
        }
    }

    /// Show images, for message `id`: allowed until Pronto quits, in every page it is on.
    pub(super) fn allow_remote(self: &Rc<Self>, id: MessageId) {
        self.remote.allow_message(id);
        self.refresh_remote();
        tracing::debug!(
            message = id.0,
            "reader: remote images allowed for a message"
        );
    }

    /// Every expanded card follows the remote-content policy (ML-058) after Show images, a
    /// sender allowed or forgotten, or the list read: a card whose images are now allowed for
    /// it alone loses its banner, and its view, if it holds one, loads again without the
    /// content filter; a card whose sender joined or left the list is built again, with the
    /// sender's line or the banner. Collapsed messages take the policy when expanded.
    pub(super) fn refresh_remote(self: &Rc<Self>) {
        let theme = self.theme.theme();
        let mut rebuild = Vec::new();
        let mut refresh = |page: &mut Page| {
            let key = page.key;
            for item in page.items_mut() {
                let id = item.id();
                let Some(card) = item.card.as_mut() else {
                    continue;
                };
                let wanted = self.remote.notice(id, &card.sender, card.remote_images);
                if wanted == card.widgets.notice {
                    continue;
                }
                if !matches!(
                    (&card.widgets.notice, &wanted),
                    (Notice::Hidden(_), Notice::None)
                ) {
                    rebuild.push((key, id));
                    continue;
                }
                card.widgets.notice = wanted;
                if let Some(banner) = &card.widgets.banner
                    && banner.root.parent().is_some()
                {
                    widgets::hand_over_focus(&banner.root, card.focus_target().as_ref());
                    card.widgets.root.remove(&banner.root);
                }
                let Some(html) = card.html.as_mut().filter(|html| !html.remote_allowed) else {
                    continue;
                };
                html.remote_allowed = true;
                card.body = ShownBody::Html {
                    style: html.style,
                    remote_images: html.remote_images,
                    remote_allowed: true,
                    inline_parts: html.parts.len(),
                };
                if let Some(view) = &html.view {
                    html.loaded = false;
                    html.painted = false;
                    html.changed = Instant::now();
                    html.quotes_open = false;
                    self.engine.set_parts(view.page_id(), html.parts.clone());
                    view.load(
                        &html.html,
                        &stylesheet(&theme, html.style),
                        &rgba(theme.webview_background(html.style)),
                        None,
                    );
                }
            }
        };
        for pages in self.pages() {
            if let Some(page) = pages.borrow_mut().as_mut() {
                refresh(page);
            }
        }
        for page in self.warm.borrow_mut().iter_mut() {
            refresh(page);
        }
        // A page laid out unseen and left undrawn loads again: drawn until it settles.
        self.redraw_unseen();
        for (key, id) in rebuild {
            self.reload_card(key, id);
        }
    }

    /// An event of the view that holds message `id`'s body on page `key`, from its load
    /// `token`: heights keep the body's place (and the column's scroll), loads and frames tell
    /// the layout wait, links open after vetting, a crash renders the body again once, then as
    /// text.
    pub(super) fn body_event(
        self: &Rc<Self>,
        key: u64,
        id: MessageId,
        token: u64,
        event: &ViewEvent,
    ) {
        let ours = |html: &HtmlBody| html.token == token && html.view.is_some();
        match event {
            ViewEvent::Height { height, width } => {
                let changed = self
                    .with_page(key, |page| {
                        let html = page
                            .item_mut(id)?
                            .card
                            .as_mut()?
                            .html
                            .as_mut()
                            .filter(|html| ours(html))?;
                        let before = html.measured.map(|(height, _)| height);
                        let moved = html.measured != Some((*height, *width));
                        if moved {
                            html.changed = Instant::now();
                        }
                        html.measured = Some((*height, *width));
                        html.holder.set_placeholder(*height);
                        html.released = false;
                        let at_width = (html.holder.width() - width).abs() <= 1;
                        // Measured with the quotes opened, it isn't the height a fresh open
                        // shows: the next one would attach at it, and what is below would jump.
                        let kept = (at_width && *width > 1 && !html.quotes_open)
                            .then(|| {
                                self.height_key(
                                    id,
                                    html.style,
                                    html.remote_allowed,
                                    html.collapse_quotes,
                                )
                            })
                            .flatten();
                        Some((before != Some(*height), moved, kept))
                    })
                    .flatten();
                if let Some((changed, moved, kept)) = changed {
                    // Laid out unseen and left undrawn, its page is drawn again until it
                    // settles at the new size (the frames WebKit makes for it are taken).
                    if moved {
                        self.redraw_unseen();
                    }
                    if let Some(kept) = kept {
                        self.heights.borrow_mut().put(
                            kept,
                            Measured {
                                height: *height,
                                width: *width,
                            },
                        );
                    }
                    if changed && self.is_shown(key) {
                        self.hold_scroll();
                    }
                }
                self.schedule();
            }
            ViewEvent::Loaded | ViewEvent::Frame => {
                let loaded = matches!(event, ViewEvent::Loaded);
                let ours = self
                    .with_page(key, |page| {
                        let html = page
                            .item_mut(id)
                            .and_then(|item| item.card.as_mut())
                            .and_then(|card| card.html.as_mut())
                            .filter(|html| ours(html))?;
                        if loaded {
                            html.loaded = true;
                            html.changed = Instant::now();
                        } else {
                            html.painted = true;
                        }
                        Some(())
                    })
                    .flatten();
                // Its images are in (the event comes whether or not the page is drawn): laid
                // out unseen, it is drawn again until the layout they bring has settled.
                if loaded && ours.is_some() {
                    self.redraw_unseen();
                }
            }
            ViewEvent::Link(link) => self.links.open(link),
            ViewEvent::Hover(hover) => {
                let view = self
                    .with_page(key, |page| {
                        page.item(id)?
                            .card
                            .as_ref()?
                            .html
                            .as_ref()
                            .filter(|html| ours(html))?
                            .view
                            .as_ref()
                            .map(|view| view.web_view().clone().upcast::<gtk::Widget>())
                    })
                    .flatten();
                if view.is_some() || hover.is_none() {
                    self.links.hover(view.as_ref(), hover.as_ref());
                }
            }
            ViewEvent::Margins(fitted) => {
                self.with_page(key, |page| {
                    if let Some(html) = page
                        .item(id)
                        .and_then(|item| item.card.as_ref())
                        .and_then(|card| card.html.as_ref())
                        .filter(|html| ours(html))
                    {
                        if *fitted {
                            html.frame.add_css_class("sw-paper-fitted");
                        } else {
                            html.frame.remove_css_class("sw-paper-fitted");
                        }
                    }
                });
            }
            ViewEvent::Quotes { expanded, .. } => {
                self.with_page(key, |page| {
                    if let Some(html) = page
                        .item_mut(id)
                        .and_then(|item| item.card.as_mut())
                        .and_then(|card| card.html.as_mut())
                        .filter(|html| ours(html))
                    {
                        html.quotes_open = *expanded;
                    }
                });
            }
            ViewEvent::Refused(refused) => {
                tracing::debug!(?refused, "reader: refused what a message asked for");
            }
            ViewEvent::Crashed | ViewEvent::Failed => self.body_failed(key, id, token, event),
            ViewEvent::Finished | ViewEvent::Resource(_) => {}
        }
        self.wake();
        self.view_event(id, event);
    }

    /// The view of message `id` crashed or its load failed: it goes back to the pool and the
    /// body loads again in another; after a second crash, or a failed load, the card is built
    /// again with the body as text.
    fn body_failed(self: &Rc<Self>, key: u64, id: MessageId, token: u64, event: &ViewEvent) {
        let view = self.with_page(key, |page| {
            let html = page
                .item_mut(id)?
                .card
                .as_mut()?
                .html
                .as_mut()
                .filter(|html| html.token == token)?;
            html.view.take()
        });
        let Some(Some(view)) = view else {
            return;
        };
        self.engine.release(view);
        let crashes = {
            let mut crashed = self.crashed.borrow_mut();
            let count = crashed.entry(id).or_default();
            *count += if matches!(event, ViewEvent::Crashed) {
                1
            } else {
                2
            };
            *count
        };
        tracing::warn!(message = id.0, crashes, "reader: a body's view failed");
        if crashes >= 2 {
            self.reload_card(key, id);
        } else {
            self.schedule();
        }
    }
}

/// Puts `item`'s row back in place of its card, its date said as of now (the row was made
/// when the page was built: a page laid out at 23:50 and read after midnight said "23:40" on a
/// collapsed row whose card said "Oct 28, 23:40"); a focus in the card goes to the row. Returns
/// the view the card's body held.
pub(super) fn collapse_item(item: &mut Item) -> Option<super::view::MessageView> {
    item.expanded = false;
    let mut card = item.card.take()?;
    redate_row(&item.row, &item.message, now());
    item.slot.append(&item.row);
    widgets::hand_over_focus(&card.widgets.root, Some(item.row.upcast_ref()));
    item.slot.remove(&card.widgets.root);
    card.html.as_mut().and_then(|html| html.view.take())
}

/// The message of `page` that holds `focus` (its row, card or body).
fn holding(page: &Page, focus: &gtk::Widget) -> Option<MessageId> {
    page.items()
        .find(|item| focus.is_ancestor(&item.slot))
        .map(Item::id)
}

/// The message of `page` whose open card holds `focus` (its body, the HTML view or the plain
/// text, and its header, actions and attachments): the one being read. A collapsed row holding
/// it isn't read (a collapsed message's card is out of the page). Since phase C's review the
/// whole card, not only its body: with the focus on the answered message's attachments after a
/// send, `p` went from the selected message.
fn reading(page: &Page, focus: &gtk::Widget) -> Option<MessageId> {
    let inside = |widget: &gtk::Widget| focus == widget || focus.is_ancestor(widget);
    page.items()
        .find(|item| {
            item.card.as_ref().is_some_and(|card| {
                let view = card.html.as_ref().and_then(|html| html.view.as_ref());
                view.is_some_and(|view| inside(view.web_view().upcast_ref()))
                    || inside(card.widgets.root.upcast_ref())
            })
        })
        .map(Item::id)
}
