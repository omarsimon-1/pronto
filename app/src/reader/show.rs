//! Showing a conversation. Another message of the conversation on screen is shown in place;
//! a conversation laid out ahead (next to the selection, or the one it just left) comes on
//! screen at once; any other is read, its page built (rows and card shells), its expanded
//! messages loaded, and laid out unseen until the bodies its first screen needs have their
//! heights, then put on screen in place of the shown one, scrolled to the selected message. The
//! conversations next to the selection are read and laid out the same way, unseen, one at a
//! time once the shown one is painted ([`Inner::warm_next`]). A store change that touches the
//! shown conversation reads it again and changes the page in place, keeping its bodies and its
//! scroll; one that touches a conversation laid out ahead drops it.

use super::conversation::{
    self, Entry, Fold, Item, Page, collapsed_row, fill_row, fold_row, folds, heading, heading_text,
    initially_expanded, initially_shown,
};
use super::{
    COALESCE, FIRST_READY_TIMEOUT, Inner, LAYOUT_BACKSTOP, LEFT_SHOWN, MAX_WARM_PAGES, Opened,
    READY_TIMEOUT, Shown, ShownBody, page, widgets,
};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use sealwax_domain::model::MessageId;
use sealwax_engine::api::BodyError;
use sealwax_infrastructure::store::{ChangeEvent, Conversation, ConversationMessage};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The thread's column, in ems of message text: about 70 characters of it, and room for a
/// newsletter's 600-700 px design (DESIGN.md "Reader", the fork's Superhuman pass).
const COLUMN_EMS: u32 = 48;

impl Inner {
    /// A `show`; `moved_on`: the selection moved on from the shown message (`show_next`).
    pub(super) fn show(self: &Rc<Self>, id: MessageId, moved_on: bool) {
        let now = Instant::now();
        let held = self
            .last_show
            .replace(Some(now))
            .is_some_and(|last| now.duration_since(last) < super::REPEAT);
        self.target.set(Some(id));
        self.answers.borrow_mut().remove(&id);
        let generation = self.next_generation();
        if self.retarget(id, generation) || self.promote_warm(id, generation, moved_on) {
            return;
        }
        if moved_on {
            self.arm_left(id);
        }
        self.spawn_load(id, generation, held);
    }

    /// Reads `id`'s conversation afresh, as a new `show` (the conversation order or Reply's
    /// setting changed: the page is built again).
    pub(super) fn read(self: &Rc<Self>, id: MessageId, held: bool) {
        self.target.set(Some(id));
        let generation = self.next_generation();
        self.spawn_load(id, generation, held);
    }

    fn spawn_load(self: &Rc<Self>, id: MessageId, generation: u64, held: bool) {
        let inner = self.clone();
        glib::spawn_future_local(async move {
            // A held key: read only the conversation it rests on.
            if held {
                loop {
                    let since = inner
                        .last_show
                        .get()
                        .map_or(COALESCE, |last| last.elapsed());
                    if since >= COALESCE {
                        break;
                    }
                    glib::timeout_future(COALESCE - since).await;
                    if inner.stale(generation) {
                        return;
                    }
                }
            }
            inner.load(id, generation).await;
        });
    }

    /// The shown conversation, which the selection moved on from, stays at most
    /// [`LEFT_SHOWN`] while the next one is read.
    fn arm_left(self: &Rc<Self>, id: MessageId) {
        let left = self
            .shown
            .borrow()
            .as_ref()
            .filter(|page| page.selected != id)
            .map(|page| page.key);
        if let Some(key) = left
            && self.left.get().is_none_or(|(earlier, _)| earlier != key)
        {
            self.left.set(Some((key, Instant::now() + LEFT_SHOWN)));
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(LEFT_SHOWN, move || {
                if let Some(inner) = weak.upgrade() {
                    inner.left_due(key);
                }
            });
        }
    }

    pub(super) async fn load(self: Rc<Self>, id: MessageId, generation: u64) {
        let started = Instant::now();
        let conversation = self.read_conversation(id).await;
        if self.stale(generation) {
            return;
        }
        let Some(conversation) = conversation else {
            self.show_missing(id, generation, started);
            return;
        };
        let fetched = started.elapsed();
        if !self.await_filter(generation).await {
            return;
        }
        self.discard_pending();
        let page = self.build_page(generation, &conversation);
        tracing::debug!(
            message = id.0,
            messages = conversation.messages.len(),
            expanded = page.expanded().len(),
            "reader: showing a conversation"
        );
        self.pending.replace(Some(page));
        self.load_cards(generation).await;
        if self.stale(generation) {
            return;
        }
        self.attach_quick_reply(generation);
        self.request_bodies(generation);
        self.with_page(generation, |page| page.built = true);
        let prepared = started.elapsed();
        self.finish(generation, generation, started, fetched, prepared)
            .await;
    }

    async fn read_conversation(&self, id: MessageId) -> Option<Conversation> {
        match self.store.conversation(id) {
            Some(reply) => match reply.recv().await {
                Ok(versioned) => versioned.value,
                Err(error) => {
                    tracing::warn!(message = id.0, %error, "reader: the conversation couldn't be read");
                    None
                }
            },
            None => None,
        }
    }

    /// Waits for the content filter the first time; `false` when the load went stale meanwhile.
    async fn await_filter(&self, generation: u64) -> bool {
        if self.filter.borrow().is_none() {
            let filter = self.engine.filter().await;
            if self.stale(generation) {
                return false;
            }
            self.filter.replace(filter);
        }
        true
    }

    /// Loads page `key`'s expanded messages, together, and returns once each is in.
    async fn load_cards(self: &Rc<Self>, key: u64) {
        let expanded: Vec<MessageId> = self
            .with_page(key, |page| page.expanded())
            .unwrap_or_default();
        let loads: Vec<_> = expanded
            .into_iter()
            .map(|message| {
                let token = self.next_token();
                self.with_page(key, |page| {
                    if let Some(item) = page.item_mut(message) {
                        item.load = token;
                    }
                });
                let inner = self.clone();
                glib::spawn_future_local(async move {
                    inner.load_card(key, message, token).await;
                })
            })
            .collect();
        for load in loads {
            let _ = load.await;
        }
    }

    /// Lays the page being shown out unseen and puts it on screen.
    async fn finish(
        self: &Rc<Self>,
        generation: u64,
        key: u64,
        started: Instant,
        fetched: Duration,
        prepared: Duration,
    ) {
        let staged = self.stage_pending(key);
        let mut early = false;
        if staged {
            early = !self.lay_out(generation, key).await;
            if self.stale(generation) {
                return;
            }
        }
        self.present(key, started, fetched, prepared, false);
        self.settle(early);
    }

    /// The page of `conversation`: the heading, then a row or an expanded card's place for each
    /// message, runs of read messages folded, in the thread's column.
    fn build_page(self: &Rc<Self>, key: u64, conversation: &Conversation) -> Page {
        let selected = conversation
            .messages
            .iter()
            .find(|message| message.summary.id == conversation.selected)
            .or(conversation.messages.last());
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sw-reader-page"])
            .build();
        content.set_widget_name("reader-page");
        // The conversation's subject, as its list row says it (phase C's visual QA: the heading
        // took the first unread message's, "Orbit Coffee, issue 47" under a row saying
        // "Harbor & Pine, issue 40"); the selected message's when the conversation has none,
        // or when the list's rows are messages, as the row that was opened says it.
        let subject = if self.message_rows.get() || conversation.subject.trim().is_empty() {
            selected.map_or("", |message| message.summary.subject.as_str())
        } else {
            conversation.subject.as_str()
        };
        let title = heading(subject);
        content.append(&title);
        let reminder = conversation::reminder_line();
        conversation::show_reminder(
            &reminder,
            crate::remind::line_text(conversation.thread).as_deref(),
        );
        content.append(&reminder);
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sw-reader-conversation"])
            .build();
        list.set_widget_name("reader-conversation");
        list.update_property(&[gtk::accessible::Property::Label(&format!(
            "Conversation, {}",
            conversation::size_text(conversation.messages.len())
        ))]);
        content.append(&list);
        let expanded = initially_expanded(&conversation.messages, conversation.selected);
        let shown = initially_shown(&expanded);
        let folded = folds(&shown);
        let mut entries = Vec::new();
        let mut index = 0;
        while index < conversation.messages.len() {
            if let Some(fold) = folded.iter().find(|fold| fold.start == index) {
                let messages = conversation.messages[fold.clone()].to_vec();
                entries.push(Entry::Fold(self.fold(key, messages)));
                index = fold.end;
                continue;
            }
            let item = self.item(key, &conversation.messages[index], expanded[index]);
            entries.push(Entry::Message(Box::new(item)));
            index += 1;
        }
        let newest_first = self.newest_first.get();
        for entry in &entries {
            if newest_first {
                list.prepend(&entry_widget(entry));
            } else {
                list.append(&entry_widget(entry));
            }
        }
        Page {
            key,
            selected: conversation.selected,
            thread: Some(conversation.thread),
            root: self.column(&content),
            content,
            heading: Some(title),
            reminder: Some(reminder),
            list,
            entries,
            members: conversation.members().collect(),
            missing: false,
            current: None,
            subject: conversation.subject.trim().to_owned(),
            quick: None,
            newest_first,
            built: false,
        }
    }

    /// The thread's column around `content`: [`COLUMN_EMS`] of message text wide at most,
    /// centred in the reader. In px, as the theme's text sizes are: the desktop's text scale
    /// (`gtk-xft-dpi`; the owner's is 0.9091) leaves the message text as it is, and a clamp in
    /// libadwaita's default unit (sp) would narrow the column and part it from the toolbar's
    /// (`Window::follow_reader_column`).
    fn column(&self, content: &gtk::Box) -> adw::Clamp {
        let width = column_width(self.theme.theme().scale.reading);
        let column = adw::Clamp::builder()
            .maximum_size(width)
            .tightening_threshold(width)
            .unit(adw::LengthUnit::Px)
            .child(content)
            .css_classes(["sw-reader-column"])
            .build();
        column.set_widget_name("reader-thread");
        column
    }

    /// One message's place in the page: its row, which a click expands.
    pub(super) fn item(
        self: &Rc<Self>,
        key: u64,
        message: &ConversationMessage,
        expanded: bool,
    ) -> Item {
        let row = collapsed_row(message, super::now());
        let id = message.summary.id;
        let weak = Rc::downgrade(self);
        row.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade()
                && inner.is_shown(key)
            {
                inner.expand(id);
            }
        });
        let slot = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sw-reader-item"])
            .build();
        slot.set_widget_name("reader-item");
        if !expanded {
            slot.append(&row);
        }
        // A press in the message (its row, header or body, any button): the user is on it
        // (`Inner::moved_to`). Only the user's own moves count, a press, Tab
        // (`Inner::follow_focus`) or `n`/`p`, never where the focus goes by itself: the reader
        // leaving the inline reply after a send, a window active again, a popover closing.
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(inner) = weak.upgrade() {
                inner.move_after_event(key, id);
            }
        });
        slot.add_controller(click);
        Item {
            message: message.clone(),
            slot,
            row,
            expanded,
            card: None,
            load: 0,
            opened: false,
        }
    }

    /// Folded messages' row, which reveals them a step at a time.
    pub(super) fn fold(self: &Rc<Self>, key: u64, messages: Vec<ConversationMessage>) -> Fold {
        let row = fold_row(messages.len());
        let first = messages.first().map(|message| message.summary.id);
        let weak = Rc::downgrade(self);
        row.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade()
                && inner.is_shown(key)
                && let Some(first) = first
            {
                inner.unfold(first);
            }
        });
        Fold { row, messages }
    }

    /// Asks once for the bodies the store doesn't hold of the page's expanded messages.
    pub(super) fn request_bodies(self: &Rc<Self>, key: u64) {
        let wanted: Vec<MessageId> = self
            .with_page(key, |page| {
                page.items()
                    .filter(|item| {
                        item.expanded
                            && item
                                .card
                                .as_ref()
                                .is_some_and(|card| card.body == ShownBody::Downloading)
                    })
                    .map(|item| item.id())
                    .collect()
            })
            .unwrap_or_default();
        let wanted: Vec<MessageId> = wanted
            .into_iter()
            .filter(|id| !self.fetching.borrow().contains(id))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let Some(fetch) = self.fetcher.borrow().clone() else {
            return;
        };
        if fetch(&wanted) {
            self.fetching.borrow_mut().extend(wanted);
        } else {
            for id in wanted {
                self.answers.borrow_mut().insert(id, Err(BodyError::Other));
                self.reload_card(key, id);
            }
        }
    }

    /// Puts the page being read into the stage, to lay it out unseen; `false` when it has no
    /// HTML body to wait for, or the reader isn't on screen to lay anything out.
    fn stage_pending(&self, key: u64) -> bool {
        let pending = self.pending.borrow();
        let Some(page) = pending.as_ref().filter(|page| page.key == key) else {
            return false;
        };
        if !page.has_html() || !self.stage.is_mapped() {
            return false;
        }
        self.stage.stage(&page.root);
        true
    }

    /// Waits until the bodies the page's first screen needs have their heights at the column's
    /// width and their images in (not those from the server, which fill in once it is shown):
    /// every expanded HTML body up to the selected message, and
    /// those below it that start within a screen of its top. With no previous conversation,
    /// wait at most [`FIRST_READY_TIMEOUT`], then show progressively. Otherwise keep the
    /// existing [`READY_TIMEOUT`], and no longer than a conversation the selection moved on
    /// from may stay ([`super::LEFT_SHOWN`]).
    /// It wakes on its views' events and on GTK's layout of the page (no polling: a 50 ms poll
    /// held a laid-out page back up to 50 ms). `false` when it is shown before its first
    /// screen is laid out.
    async fn lay_out(self: &Rc<Self>, generation: u64, key: u64) -> bool {
        let first = self.shown.borrow().is_none();
        let timeout = if first {
            FIRST_READY_TIMEOUT
        } else {
            READY_TIMEOUT
        };
        let deadline = Instant::now() + timeout;
        let woken = self.waiter();
        while let Some(waiting) = self.first_screen_waiting(key) {
            if !waiting || !self.stage.is_mapped() {
                return true;
            }
            let due = self.left_due_at();
            let now = Instant::now();
            if due.is_some_and(|due| due <= now) {
                tracing::debug!(
                    "reader: shown before it was laid out, in place of the message the selection left"
                );
                return false;
            }
            let left = deadline.saturating_duration_since(now);
            if left.is_zero() {
                if first {
                    tracing::debug!(
                        wait_ms = timeout.as_millis(),
                        "reader: first conversation shown progressively while its bodies lay out"
                    );
                } else {
                    tracing::warn!(
                        wait_ms = timeout.as_millis(),
                        "reader: the conversation took over {} s to lay out; shown as it is",
                        timeout.as_secs()
                    );
                }
                return false;
            }
            self.schedule_now();
            let wait = due.map_or(left, |due| left.min(due.saturating_duration_since(now)));
            let _ = glib::future_with_timeout(wait.min(LAYOUT_BACKSTOP), woken.recv()).await;
            if self.stale(generation) {
                return true;
            }
        }
        true
    }

    /// Puts the page being laid out on screen. The page it replaces stays laid out unseen when
    /// it is whole (the selection may come back) and another conversation, else gives its views
    /// back: the same conversation built again (`show_again` after the order or Reply's setting
    /// changed, a folded message shown) is stale, and kept it could be swapped back in.
    fn present(
        self: &Rc<Self>,
        key: u64,
        started: Instant,
        fetched: Duration,
        prepared: Duration,
        ahead: bool,
    ) {
        let page = {
            let mut pending = self.pending.borrow_mut();
            match pending.as_ref() {
                Some(page) if page.key == key => pending.take(),
                _ => None,
            }
        };
        let Some(page) = page else {
            return;
        };
        let focus = page.selected_card().and_then(|card| card.focus_target());
        let keep = self.shown.borrow().as_ref().is_some_and(|old| {
            old.built && old.complete() && (old.thread.is_none() || old.thread != page.thread)
        });
        self.stage.show(Some(&page.root), focus.as_ref(), keep);
        let selected = page.selected;
        let shown = Shown {
            id: selected,
            body: if page.missing {
                ShownBody::Missing
            } else {
                page.selected_card()
                    .map_or(ShownBody::Empty, |card| card.body.clone())
            },
            fetched,
            prepared,
            ready: started.elapsed(),
            messages: page.message_count(),
            expanded: page.expanded().len(),
            ahead,
        };
        let count = page.message_count();
        if let Some(old) = self.shown.replace(Some(page)) {
            if keep {
                self.keep_warm(old);
            } else {
                self.release_page(old);
            }
        }
        self.left.set(None);
        self.hidden.set(false);
        self.settle(false);
        self.scroll_to_selected();
        tracing::debug!(
            message = selected.0,
            body = ?shown.body,
            messages = shown.messages,
            read_ms = shown.fetched.as_secs_f64() * 1000.0,
            prepared_ms = shown.prepared.as_secs_f64() * 1000.0,
            ms = shown.ready.as_secs_f64() * 1000.0,
            "reader: shown"
        );
        if !self.first_preview_logged.replace(true) {
            let now = Instant::now();
            let opened = self.window_opened.get();
            let clicked = self.last_show.get().unwrap_or(started);
            tracing::info!(
                message = selected.0,
                open_to_click_ms = clicked.saturating_duration_since(opened).as_secs_f64() * 1000.0,
                click_to_shown_ms = now.saturating_duration_since(clicked).as_secs_f64() * 1000.0,
                open_to_shown_ms = now.saturating_duration_since(opened).as_secs_f64() * 1000.0,
                read_ms = shown.fetched.as_secs_f64() * 1000.0,
                prepared_ms = shown.prepared.as_secs_f64() * 1000.0,
                "reader: first preview after window open"
            );
        }
        self.last_shown.replace(Some(shown.clone()));
        let listeners = self.on_shown.borrow().clone();
        for listener in listeners {
            listener(&shown);
        }
        self.report_opened(key);
        self.size_changed();
        self.quick_changed();
        if count > 1 {
            self.stage.announce(
                &format!(
                    "Conversation, {}, {} expanded",
                    conversation::size_text(count),
                    shown.expanded
                ),
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        }
        self.schedule();
        self.after_paint(Inner::warm_next);
    }

    /// Another message of the shown conversation, shown in place: the messages a fresh `show`
    /// of it would expand are expanded (their cards loaded, bodies already shown kept), the
    /// others collapsed, the heading takes its subject and the column scrolls to it. Opened
    /// there, the conversation starts afresh: the user's earlier move is forgotten, and an
    /// empty reply box answers the message it would for a fresh `show` (while the list's rows
    /// are messages, this one). `false` when `id` isn't on the shown page (folded, or another
    /// conversation).
    fn retarget(self: &Rc<Self>, id: MessageId, generation: u64) -> bool {
        let key = {
            let shown = self.shown.borrow();
            let Some(page) = shown.as_ref() else {
                return false;
            };
            if page.missing || !page.built || self.hidden.get() || page.item(id).is_none() {
                return false;
            }
            page.key
        };
        self.discard_pending();
        self.left.set(None);
        let started = Instant::now();
        let mut views = Vec::new();
        let mut loads = Vec::new();
        let moved = {
            let mut shown = self.shown.borrow_mut();
            let Some(page) = shown.as_mut() else {
                return false;
            };
            let moved = page.selected != id;
            if moved {
                let messages: Vec<ConversationMessage> =
                    page.messages().into_iter().cloned().collect();
                let wanted: HashSet<MessageId> = messages
                    .iter()
                    .zip(initially_expanded(&messages, id))
                    .filter(|(_, expanded)| *expanded)
                    .map(|(message, _)| message.summary.id)
                    .collect();
                page.selected = id;
                page.current = None;
                // The conversation's subject stays; one without says the message's, as the
                // message rows do.
                relabel(page, self.message_rows.get());
                for item in page.items_mut() {
                    let expand = wanted.contains(&item.id());
                    if item.expanded && !expand {
                        views.extend(super::cards::collapse_item(item));
                    } else if !item.expanded && expand {
                        let token = self.next_token();
                        item.expanded = true;
                        item.load = token;
                        loads.push((item.id(), token));
                    }
                }
            }
            moved
        };
        for view in views {
            self.engine.release(view);
        }
        // The same message again (the list told its selection again): nothing moves.
        if moved {
            self.retarget_quick_reply(key);
            self.scroll_to_selected();
            self.remote.changed();
            self.schedule();
        }
        let inner = self.clone();
        glib::spawn_future_local(async move {
            let loads: Vec<_> = loads
                .into_iter()
                .map(|(message, token)| {
                    let inner = inner.clone();
                    glib::spawn_future_local(async move {
                        inner.load_card(key, message, token).await;
                    })
                })
                .collect();
            for load in loads {
                let _ = load.await;
            }
            if !inner.stale(generation) && inner.is_shown(key) {
                inner.shown_in_place(key, started);
            }
        });
        true
    }

    /// A message shown in place ([`Inner::retarget`]) is on screen with its card.
    fn shown_in_place(self: &Rc<Self>, key: u64, started: Instant) {
        let shown = self
            .shown
            .borrow()
            .as_ref()
            .filter(|page| page.key == key)
            .map(|page| Shown {
                id: page.selected,
                body: page
                    .selected_card()
                    .map_or(ShownBody::Empty, |card| card.body.clone()),
                fetched: Duration::ZERO,
                prepared: Duration::ZERO,
                ready: started.elapsed(),
                messages: page.message_count(),
                expanded: page.expanded().len(),
                ahead: false,
            });
        let Some(shown) = shown else {
            return;
        };
        // The focus in the conversation (not in the quick reply) goes to the message shown, as
        // it goes into the next conversation's when one replaces another: the reader's doing,
        // no move of the user's ([`Inner::give_focus`]). Its header holds it while its body
        // loads, then the body takes it, as after `n` or `p` (Enter on the header folds the
        // card; in the body it replies).
        let focus = self.stage.root().and_then(|root| root.focus());
        let target = self.shown.borrow().as_ref().and_then(|page| {
            let focus = focus.as_ref()?;
            let in_page = focus.is_ancestor(&page.root);
            let in_quick = page
                .quick
                .as_ref()
                .is_some_and(|quick| focus.is_ancestor(&quick.root));
            if !in_page || in_quick {
                return None;
            }
            let card = page.selected_card()?;
            let loading = card.html.as_ref().is_some_and(|html| html.view.is_none());
            Some((card.focus_target()?, page.selected, loading))
        });
        if let Some((target, selected, loading)) = target {
            self.give_focus(key, selected, &target);
            if loading {
                self.focus_body.set(Some((key, selected)));
            }
        }
        tracing::debug!(
            message = shown.id.0,
            ms = shown.ready.as_secs_f64() * 1000.0,
            "reader: shown in place"
        );
        self.last_shown.replace(Some(shown.clone()));
        let listeners = self.on_shown.borrow().clone();
        for listener in listeners {
            listener(&shown);
        }
        self.report_opened(key);
        self.size_changed();
        self.quick_changed();
        self.after_paint(Inner::warm_next);
    }

    /// The page laid out ahead for `id`, shown at once when its first screen is laid out, else
    /// finished as the target; `false` when there is none.
    fn promote_warm(self: &Rc<Self>, id: MessageId, generation: u64, moved_on: bool) -> bool {
        let index = self
            .warm
            .borrow()
            .iter()
            .position(|page| page.selected == id);
        let Some(index) = index else {
            return false;
        };
        let page = self.warm.borrow_mut().remove(index);
        // Built a while ago: its "25 min ago" and today's times say so as of now.
        page.redate(super::now());
        let key = page.key;
        let ready = self.ready(&page);
        self.discard_pending();
        self.pending.replace(Some(page));
        let started = Instant::now();
        if ready {
            self.request_bodies(key);
            self.present(key, started, Duration::ZERO, Duration::ZERO, true);
            return true;
        }
        if moved_on {
            self.arm_left(id);
        }
        let inner = self.clone();
        glib::spawn_future_local(async move { inner.adopt(key, generation, started).await });
        true
    }

    /// A page laid out ahead that wasn't ready when it was asked for: once its cards are in,
    /// it is laid out and shown as a load's would be.
    async fn adopt(self: Rc<Self>, key: u64, generation: u64, started: Instant) {
        let woken = self.waiter();
        while !self.with_page(key, |page| page.built).unwrap_or(true) {
            if self.stale(generation) {
                return;
            }
            let _ = glib::future_with_timeout(LAYOUT_BACKSTOP, woken.recv()).await;
        }
        let pending = self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|page| page.key == key);
        if self.stale(generation) || !pending {
            return;
        }
        self.attach_quick_reply(key);
        self.request_bodies(key);
        let prepared = started.elapsed();
        self.finish(generation, key, started, Duration::ZERO, prepared)
            .await;
    }

    /// Whether `page`, laid out ahead, can come on screen at once: its cards are in, it shows
    /// every expanded body, and its first screen is laid out.
    pub(super) fn ready(&self, page: &Page) -> bool {
        page.built && page.complete() && !self.waiting(page)
    }

    /// The page the selection left, kept laid out unseen. Shown again, it starts afresh
    /// ([`Inner::start_afresh`]).
    fn keep_warm(self: &Rc<Self>, page: Page) {
        let key = page.key;
        self.warm.borrow_mut().insert(0, page);
        self.trim_warm();
        self.start_afresh(key);
    }

    /// Page `key`, left, starts afresh when it is shown again: the message the user moved to
    /// there is forgotten (`n`/`p` go from the message being read again, else the selected
    /// one), and an empty reply box answers the latest message again, or the selected one
    /// while the list's rows are messages (it kept the one the user had left it on). Left for
    /// another conversation ([`Inner::keep_warm`]), or for the list that a Focused or narrow
    /// window shows in its place ([`super::Reader::left_screen`]: opened again, the
    /// conversation answered whom the user had moved to before).
    pub(super) fn start_afresh(self: &Rc<Self>, key: u64) {
        let quick = self.with_page(key, |page| {
            page.current = None;
            page.quick.is_some()
        });
        if quick == Some(true) {
            self.retarget_quick_reply(key);
        }
    }

    /// At most [`MAX_WARM_PAGES`] pages laid out ahead: those for no neighbour go first, the
    /// least recently wanted first.
    fn trim_warm(&self) {
        loop {
            let victim = {
                let warm = self.warm.borrow();
                if warm.len() <= MAX_WARM_PAGES {
                    return;
                }
                let neighbours = self.neighbours.borrow();
                warm.iter()
                    .rposition(|page| !neighbours.contains(&page.selected))
                    .unwrap_or(warm.len() - 1)
            };
            let page = self.warm.borrow_mut().remove(victim);
            self.drop_page(page);
        }
    }

    /// [`super::Reader::prefetch`].
    pub(super) fn prefetch(self: &Rc<Self>, ids: &[MessageId]) {
        *self.neighbours.borrow_mut() = ids.to_vec();
        self.drop_warm(|page| !ids.contains(&page.selected));
        self.after_paint(Inner::warm_next);
    }

    /// Whether `id` shows at once: in place on the shown page, or from a page laid out (or
    /// being laid out) for it.
    fn has_page(&self, id: MessageId) -> bool {
        self.shown
            .borrow()
            .as_ref()
            .is_some_and(|page| page.item(id).is_some())
            || self
                .pending
                .borrow()
                .as_ref()
                .is_some_and(|page| page.selected == id)
            || self.warm.borrow().iter().any(|page| page.selected == id)
    }

    /// Reads and lays out unseen the conversation of the first neighbour without a page, one
    /// at a time, while no conversation is being shown.
    pub(super) fn warm_next(self: &Rc<Self>) {
        // The conversation asked for comes first: not while it is read or laid out.
        let shown = self.shown.borrow().as_ref().map(|page| page.selected);
        let showing = self
            .target
            .get()
            .is_some_and(|target| shown != Some(target));
        if showing
            || self.warming.get().is_some()
            || self.pending.borrow().is_some()
            || self.hidden.get()
            || !self.stage.is_mapped()
        {
            return;
        }
        let next = self
            .neighbours
            .borrow()
            .iter()
            .copied()
            .find(|id| !self.has_page(*id));
        let Some(id) = next else {
            return;
        };
        let key = self.next_serial();
        self.warming.set(Some(key));
        let inner = self.clone();
        glib::spawn_future_local(async move { inner.warm_up(id, key).await });
    }

    async fn warm_up(self: Rc<Self>, id: MessageId, key: u64) {
        let conversation = self.read_conversation(id).await;
        let wanted = self.warming.get() == Some(key)
            && self.neighbours.borrow().contains(&id)
            && !self.has_page(id)
            && self.filter.borrow().is_some();
        let Some(conversation) = conversation.filter(|_| wanted) else {
            self.warmed(key);
            return;
        };
        let page = self.build_page(key, &conversation);
        self.warm.borrow_mut().insert(0, page);
        self.trim_warm();
        self.load_cards(key).await;
        let warm = self.warm.borrow().iter().any(|page| page.key == key);
        let adopted = self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|page| page.key == key);
        if warm || adopted {
            self.attach_quick_reply(key);
            self.with_page(key, |page| page.built = true);
        }
        if warm {
            // A page that would say "Downloading…" is read again when it is shown.
            let root = self
                .with_page(key, |page| page.complete().then(|| page.root.clone()))
                .flatten();
            match root {
                Some(root) if self.stage.is_mapped() => self.stage.stage(&root),
                Some(_) => {}
                None => self.drop_warm(|page| page.key == key),
            }
            tracing::debug!(message = id.0, "reader: a conversation laid out ahead");
        }
        self.wake();
        self.warmed(key);
    }

    /// The warm-up of page `key` is over: the next neighbour's may start.
    fn warmed(self: &Rc<Self>, key: u64) {
        if self.warming.get() == Some(key) {
            self.warming.set(None);
        }
        self.schedule();
        self.warm_next();
    }

    /// Runs `then` once the next frame is painted (or soon, when the reader has no frame
    /// clock): work that can wait leaves the frame that shows a conversation alone.
    fn after_paint(self: &Rc<Self>, then: fn(&Rc<Inner>)) {
        let weak = Rc::downgrade(self);
        let Some(clock) = self.stage.frame_clock() else {
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    then(&inner);
                }
            });
            return;
        };
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let held = handler.clone();
        let id = clock.connect_after_paint(move |clock| {
            if let Some(id) = held.take() {
                clock.disconnect(id);
            }
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    then(&inner);
                }
            });
        });
        handler.replace(Some(id));
        clock.request_phase(gdk::FrameClockPhase::AFTER_PAINT);
    }

    /// When the shown conversation, which the selection moved on from, must be off screen.
    fn left_due_at(&self) -> Option<Instant> {
        self.left
            .get()
            .filter(|(key, _)| self.is_shown(*key))
            .map(|(_, due)| due)
    }

    /// The time a conversation the selection moved on from may stay is up: a next one being
    /// laid out shows as it is; one not read yet hides the left one until it is.
    pub(super) fn left_due(self: &Rc<Self>, key: u64) {
        if self.left.get().is_none_or(|(left, _)| left != key) || !self.is_shown(key) {
            return;
        }
        let staged = self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|page| self.stage.is_staged(&page.root));
        if staged {
            self.wake();
            return;
        }
        if let Some(page) = self.shown.borrow().as_ref() {
            // Kept in place, unseen: a focus in it moves on to the next page as usual.
            let root = page.root.upcast_ref::<gtk::Widget>();
            root.set_opacity(0.0);
            root.set_can_target(false);
            root.update_state(&[gtk::accessible::State::Hidden(true)]);
        }
        self.hidden.set(true);
        tracing::debug!("reader: the message the selection left hidden; the next isn't read yet");
    }

    /// The bodies of page `key` that came on screen and weren't reported yet.
    pub(super) fn report_opened(&self, key: u64) {
        if !self.is_shown(key) {
            return;
        }
        let opened = self.with_page(key, |page| {
            let messages = page
                .items_mut()
                .filter(|item| {
                    !item.opened
                        && item.expanded
                        && item.card.as_ref().is_some_and(|card| card.body.is_shown())
                })
                .map(|item| {
                    item.opened = true;
                    item.message.summary.clone()
                })
                .collect();
            Opened {
                selected: page.selected,
                messages,
            }
        });
        if let Some(opened) = opened {
            for message in &opened.messages {
                self.store.opened(message.id, super::now());
            }
            self.opened(opened);
        }
    }

    fn show_missing(self: &Rc<Self>, id: MessageId, generation: u64, started: Instant) {
        self.discard_pending();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&page::notice("This message is no longer here."));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        self.pending.replace(Some(Page {
            key: generation,
            selected: id,
            thread: None,
            root: self.column(&content),
            content,
            heading: None,
            reminder: None,
            list,
            entries: Vec::new(),
            members: HashSet::from([id]),
            missing: true,
            current: None,
            subject: String::new(),
            quick: None,
            newest_first: false,
            built: true,
        }));
        let elapsed = started.elapsed();
        self.present(generation, started, elapsed, elapsed, false);
    }

    /// A store commit: cards waiting for their body load again; a commit that touched the
    /// shown conversation reads it again; one that touched the conversation being laid out
    /// starts its `show` again; one that touched a conversation laid out ahead drops it (it is
    /// laid out again while it is still next to the selection).
    pub(super) fn store_changed(self: &Rc<Self>, event: &ChangeEvent) {
        self.prepared.borrow_mut().forget(&event.removed);
        self.heights.borrow_mut().forget(&event.removed);
        let touches = |page: &Page| {
            event
                .inserted
                .iter()
                .chain(&event.removed)
                .chain(&event.changed)
                .any(|id| page.members.contains(id))
                || page.thread.is_some_and(|thread| {
                    event.threads.iter().any(|count| count.thread == thread)
                        || event
                            .merged_threads
                            .iter()
                            .any(|merge| merge.from == thread || merge.into == thread)
                })
        };
        let dropped = self.warm.borrow().iter().any(touches);
        if dropped {
            self.drop_warm(touches);
            self.after_paint(Inner::warm_next);
        }
        let pending = self
            .pending
            .borrow()
            .as_ref()
            .filter(|page| touches(page))
            .map(|page| page.selected);
        if let Some(selected) = pending
            && self.target.get() == Some(selected)
        {
            let generation = self.next_generation();
            let inner = self.clone();
            glib::spawn_future_local(async move { inner.load(selected, generation).await });
            return;
        }
        let shown = self.shown.borrow().as_ref().map(|page| {
            let waiting: Vec<(MessageId, Option<BodyError>)> = page
                .items()
                .filter(|item| event.changed.contains(&item.id()))
                .filter_map(|item| match item.card.as_ref().map(|card| &card.body) {
                    Some(ShownBody::Downloading) => Some((item.id(), None)),
                    Some(ShownBody::DownloadFailed(error)) => Some((item.id(), Some(*error))),
                    _ => None,
                })
                .collect();
            (page.key, touches(page), waiting)
        });
        let Some((key, touched, waiting)) = shown else {
            return;
        };
        for (id, failure) in waiting {
            if let Some(error) = failure {
                self.answers.borrow_mut().insert(id, Err(error));
            }
            self.reload_card(key, id);
        }
        if touched && !self.refresh_queued.replace(true) {
            let inner = self.clone();
            glib::spawn_future_local(async move { inner.refresh(key).await });
        }
    }

    /// The engine answered a download: the card waiting for it, on the shown page or the one
    /// being shown, loads again with the answer.
    pub(super) fn body_answered(self: &Rc<Self>, id: MessageId, result: Result<(), BodyError>) {
        let waiting = |page: &Page| {
            page.item(id)
                .filter(|item| {
                    item.card
                        .as_ref()
                        .is_some_and(|card| card.body == ShownBody::Downloading)
                })
                .map(|_| page.key)
        };
        let key = self
            .shown
            .borrow()
            .as_ref()
            .and_then(waiting)
            .or_else(|| self.pending.borrow().as_ref().and_then(waiting));
        if let Some(key) = key {
            self.answers.borrow_mut().insert(id, result);
            self.reload_card(key, id);
        }
    }

    /// Reads the shown conversation again and changes its page in place: messages gone are
    /// taken out, new ones put in their place as rows (unread until the user opens them),
    /// changed flags shown. The bodies already shown stay, and so does what the column shows.
    async fn refresh(self: Rc<Self>, key: u64) {
        let selected = self
            .shown
            .borrow()
            .as_ref()
            .filter(|page| page.key == key)
            .map(|page| page.selected);
        let reply = selected.and_then(|selected| self.store.conversation(selected));
        let conversation = match reply {
            Some(reply) => reply
                .recv()
                .await
                .ok()
                .and_then(|versioned| versioned.value),
            None => None,
        };
        self.refresh_queued.set(false);
        let Some(conversation) = conversation else {
            return;
        };
        if !self.is_shown(key) {
            return;
        }
        self.hold_scroll();
        let mut added = Vec::new();
        let mut removed = Vec::new();
        {
            let mut shown = self.shown.borrow_mut();
            let Some(page) = shown.as_mut() else {
                return;
            };
            let wanted: HashSet<MessageId> = conversation
                .messages
                .iter()
                .map(|message| message.summary.id)
                .collect();
            let mut kept: HashSet<MessageId> = HashSet::new();
            let mut index = 0;
            while index < page.entries.len() {
                let gone = match &mut page.entries[index] {
                    Entry::Message(item) => !wanted.contains(&item.id()),
                    Entry::Fold(fold) => {
                        fold.messages
                            .retain(|message| wanted.contains(&message.summary.id));
                        fold.messages.is_empty()
                    }
                };
                if gone {
                    match page.entries.remove(index) {
                        Entry::Message(item) => removed.push(*item),
                        Entry::Fold(fold) => page.list.remove(&fold.row),
                    }
                    continue;
                }
                match &page.entries[index] {
                    Entry::Message(item) => {
                        kept.insert(item.id());
                    }
                    Entry::Fold(fold) => {
                        kept.extend(fold.messages.iter().map(|message| message.summary.id));
                    }
                }
                index += 1;
            }
            for message in &conversation.messages {
                let id = message.summary.id;
                if !kept.contains(&id) {
                    added.push(message.clone());
                    continue;
                }
                for entry in &mut page.entries {
                    match entry {
                        Entry::Message(item) if item.id() == id => {
                            update_item(item, message);
                        }
                        Entry::Fold(fold) => {
                            for folded in &mut fold.messages {
                                if folded.summary.id == id {
                                    *folded = message.clone();
                                }
                            }
                        }
                        Entry::Message(_) => {}
                    }
                }
            }
            for entry in &page.entries {
                if let Entry::Fold(fold) = entry {
                    update_fold(fold);
                }
            }
            page.members = conversation.members().collect();
            page.thread = Some(conversation.thread);
            // Merged with another conversation, it may have another subject.
            let subject = conversation.subject.trim();
            if page.subject != subject {
                page.subject = subject.to_owned();
                relabel(page, self.message_rows.get());
            }
        }
        for item in removed {
            self.remove_item(key, item);
        }
        for message in added {
            let item = self.item(key, &message, false);
            self.with_page(key, |page| {
                let order =
                    |message: &ConversationMessage| (message.summary.date, message.summary.id);
                let at = page
                    .entries
                    .iter()
                    .position(|entry| entry_order(entry) > order(&message))
                    .unwrap_or(page.entries.len());
                let before = at.checked_sub(1).map(|at| entry_widget(&page.entries[at]));
                page.place(&item.slot, before.as_ref());
                page.entries.insert(at, Entry::Message(Box::new(item)));
            });
        }
        tracing::debug!("reader: the shown conversation changed");
        self.size_changed();
        self.retarget_quick_reply(key);
        self.schedule();
    }

    /// Takes `item` off page `key`: a focus in it goes to the selected message, its view back
    /// to the pool.
    fn remove_item(&self, key: u64, mut item: Item) {
        let target = self
            .shown
            .borrow()
            .as_ref()
            .filter(|page| page.key == key)
            .and_then(|page| page.selected_card())
            .and_then(|card| card.focus_target());
        widgets::hand_over_focus(&item.slot, target.as_ref());
        if let Some(parent) = item.slot.parent().and_downcast::<gtk::Box>() {
            parent.remove(&item.slot);
        }
        if let Some(view) = item
            .card
            .as_mut()
            .and_then(|card| card.html.as_mut())
            .and_then(|html| html.view.take())
        {
            self.engine.release(view);
        }
    }
}

/// The thread column's width for message text of `reading` px (the theme's reading size): the
/// window keeps the reader's toolbar to it.
pub fn column_width(reading: u32) -> i32 {
    i32::try_from(COLUMN_EMS * reading.max(1)).unwrap_or(i32::MAX)
}

/// The widget an entry puts in the page's list.
pub(super) fn entry_widget(entry: &Entry) -> gtk::Widget {
    match entry {
        Entry::Message(item) => item.slot.clone().upcast(),
        Entry::Fold(fold) => fold.row.clone().upcast(),
    }
}

/// Where an entry sorts: its (first) message's date and id.
fn entry_order(entry: &Entry) -> (i64, MessageId) {
    match entry {
        Entry::Message(item) => (item.message.summary.date, item.id()),
        Entry::Fold(fold) => fold
            .messages
            .first()
            .map_or((i64::MAX, MessageId(i64::MAX)), |message| {
                (message.summary.date, message.summary.id)
            }),
    }
}

/// Puts `page`'s subject in its heading: the conversation's, as its list row says it, else
/// (none, or `message_rows`: the list's rows are messages) the selected message's.
pub(super) fn relabel(page: &Page, message_rows: bool) {
    let Some(heading) = &page.heading else {
        return;
    };
    let subject = if message_rows || page.subject.is_empty() {
        page.item(page.selected)
            .map_or("", |item| item.message.summary.subject.as_str())
    } else {
        page.subject.as_str()
    };
    let text = heading_text(subject);
    if heading.label() != text {
        heading.set_label(text);
    }
}

/// A message read again: its row shows its new flags; its card keeps its body.
fn update_item(item: &mut Item, message: &ConversationMessage) {
    let unread_before = !item.message.summary.flags.is_seen();
    let unread_now = !message.summary.flags.is_seen();
    item.message = message.clone();
    if unread_before != unread_now {
        fill_row(&item.row, message, super::now());
    }
}

/// A fold's count after a re-read.
pub(super) fn update_fold(fold: &Fold) {
    conversation::set_fold_count(&fold.row, fold.messages.len());
}
