//! Live views (ADR-011's memory decision): at most [`super::MAX_LIVE_VIEWS`] expanded HTML
//! bodies hold a WebView, at most [`PAGE_VIEWS`] of one page. Those nearest the visible part of
//! the column keep theirs; bodies far off screen give their view back and keep a placeholder of
//! their measured height, so nothing jumps, and get one again before they scroll into sight.
//! Plain-text bodies never hold a view. The order: the bodies the column shows, then the first
//! screen of the conversation being shown, then those of the conversations laid out ahead (the
//! next one first), then the shown conversation's bodies near the visible part. The choice is
//! pure ([`wanted`], [`next_to_measure`]); the reader applies it whenever the column scrolls,
//! resizes, a body's height changes, or GTK lays a page out ([`Inner::schedule`]).
//!
//! The column's scroll is the reader's too: a new conversation opens scrolled to the selected
//! message, and a change above what the column shows (a body measured, expanded or collapsed, a
//! message arriving) keeps what it shows in place. Both are applied in the frame clock's layout
//! phase, after GTK's own layout, so no frame shows the content moved.

use super::conversation::{Entry, HtmlBody, Page};
use super::show::entry_widget;
use super::view::MessageView;
use super::widgets;
use super::{Inner, MAX_LIVE_VIEWS, PAGE_VIEWS, rgba, stylesheet};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib, graphene};
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::BodyStyle;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long after a change above the visible part the column keeps what it shows in place
/// (text views and bodies settle over a few frames).
const HOLD: Duration = Duration::from_millis(600);
/// Room left above the selected message when the column scrolls to it.
const SCROLL_MARGIN: f64 = 12.0;
/// The least room the heading's box keeps above it when the column scrolls to show a widget
/// ([`reveal_delta`]): its letters start a few px lower, so they stay clear of the toolbar's
/// hairline.
const HEADING_ROOM: f64 = 4.0;
/// How long a widget brought into view is kept there while it grows (a text view takes a few
/// frames to reach its height).
const REVEAL: Duration = Duration::from_millis(400);
/// Scroll Message Down and Up move the column half its height (issue #10).
pub(super) const HALF_PAGE: f64 = 0.5;
/// Scroll Message Page Down and Up move it a page, less a tenth: the last lines read stay in
/// sight, as a browser's Space keeps them.
pub(super) const PAGE: f64 = 0.9;

impl Inner {
    /// Where the inline reply on screen is while it is open, where `relative_to` shows it, as it
    /// will be once open and scrolled into view: its natural height (it grows over a few
    /// frames), from where it is or from the column's foot ([`reveal_delta`] scrolls it there);
    /// `None` while no reply is open on screen. Toasts keep off it (`Dock::keep_toasts_off`).
    pub(super) fn quick_area(&self, relative_to: &gtk::Widget) -> Option<gtk::graphene::Rect> {
        let quick = self
            .shown
            .borrow()
            .as_ref()
            .filter(|_| !self.hidden.get() && self.stage.is_mapped())
            .and_then(|page| page.quick.clone())
            .filter(|quick| quick.is_open())?;
        let view = self.scroller()?.compute_bounds(relative_to)?;
        let bounds = quick.root.compute_bounds(relative_to)?;
        let (_, natural, _, _) = quick
            .root
            .measure(gtk::Orientation::Vertical, quick.root.width());
        let height = bounds.height().max(natural as f32);
        let foot = view.y() + view.height() - SCROLL_MARGIN as f32;
        let top = if bounds.y() + height > foot {
            (foot - height).max(view.y())
        } else {
            bounds.y()
        };
        Some(gtk::graphene::Rect::new(
            bounds.x(),
            top,
            bounds.width(),
            height,
        ))
    }
}

/// Where a column at `value` (an adjustment's, with its `lower`, `upper` and `page` size) is
/// after scrolling `pages` of its height down, or up when negative: never past either end.
fn scrolled(value: f64, lower: f64, upper: f64, page: f64, pages: f64) -> f64 {
    let end = (upper - page).max(lower);
    (value + page * pages).clamp(lower, end)
}

/// How far the column scrolls to show a widget drawn at `top..bottom` in a view `page` px tall,
/// which can scroll `room` px further: the least that shows all of it with [`SCROLL_MARGIN`]
/// below (its top, when it is taller than the view). The conversation's heading (its box at
/// `heading`) is never left cut by the view's top, nor nearer it than [`HEADING_ROOM`] (phase C's
/// visual QA: the inline reply, opening, scrolled 28 px and left the subject's 21 px text against
/// the toolbar's hairline, where it read as cut): the heading goes out of view whole, where the
/// column has the room (the open reply keeps room below it for that, style.css); else the widget
/// keeps less room below it, as long as all of it shows (phase C's review: kept first, that left
/// the heading 4 px under the toolbar and the reply 4 px above the window's foot).
fn reveal_delta(top: f64, bottom: f64, page: f64, heading: Option<(f64, f64)>, room: f64) -> f64 {
    let delta = if bottom - top > page - SCROLL_MARGIN || top < 0.0 {
        top - SCROLL_MARGIN
    } else if bottom > page {
        bottom - page + SCROLL_MARGIN
    } else {
        0.0
    };
    let Some((heading_top, heading_bottom)) = heading else {
        return delta;
    };
    if delta <= 0.0 || heading_bottom <= delta || heading_top - delta >= HEADING_ROOM {
        return delta;
    }
    // What the heading allows, and the least that shows the whole widget.
    let allowed = heading_top - HEADING_ROOM;
    let least = (bottom - page).max(0.0);
    if heading_bottom <= room && top - heading_bottom >= 0.0 {
        heading_bottom
    } else if least <= allowed && top - least >= 0.0 {
        allowed.max(least)
    } else {
        delta
    }
}

/// How far beyond the visible part of the column a body keeps or gets its view, in column
/// heights: one screen above and one below, so a body is rendered again before it scrolls into
/// sight at reading speed.
pub const MARGIN_SCREENS: f64 = 1.0;

/// How long a view laid out unseen goes on being drawn after its load last changed (attached,
/// its `load` event, a new height or width): the frames that follow a change (the view resized
/// to its new height, a viewport-sized body converging, an image decoded) are taken, so the
/// page swaps in whole. A view still changing is never left; one that settled and changes
/// again (a transition that plays late, its images in) is drawn again until it settles.
pub const SETTLE: Duration = Duration::from_secs(1);

/// One expanded HTML body of a page, in page coordinates (px from the page's top).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    pub id: MessageId,
    pub top: f64,
    pub bottom: f64,
    /// It holds a view now.
    pub live: bool,
    /// Its height is known at the column's current width.
    pub measured: bool,
    /// It keeps its view whatever its place: the focus is in it.
    pub pinned: bool,
}

impl Body {
    /// How far the body is from `window` (`top`, `bottom`); 0 when they overlap.
    fn distance(&self, window: (f64, f64)) -> f64 {
        if self.bottom <= window.0 {
            window.0 - self.bottom
        } else if self.top >= window.1 {
            self.top - window.1
        } else {
            0.0
        }
    }
}

/// The bodies that should hold a view: pinned ones, then the nearest to `window` within
/// `margin` of it (visible ones first), at most `cap` in all. At the same distance a body that
/// holds a view keeps it, so scrolling doesn't trade views back and forth.
pub fn wanted(bodies: &[Body], window: (f64, f64), margin: f64, cap: usize) -> Vec<MessageId> {
    let mut near: Vec<&Body> = bodies
        .iter()
        .filter(|body| body.pinned || body.distance(window) <= margin)
        .collect();
    near.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(a.distance(window).total_cmp(&b.distance(window)))
            .then(b.live.cmp(&a.live))
            .then(a.top.total_cmp(&b.top))
    });
    near.into_iter().take(cap).map(|body| body.id).collect()
}

/// The body to measure next with a spare view: the unmeasured one nearest to `window`, so
/// heights above and below become exact (and placeholders right) before the reader scrolls
/// there.
pub fn next_to_measure(bodies: &[Body], window: (f64, f64)) -> Option<MessageId> {
    bodies
        .iter()
        .filter(|body| !body.measured && !body.live)
        .min_by(|a, b| {
            a.distance(window)
                .total_cmp(&b.distance(window))
                .then(a.top.total_cmp(&b.top))
        })
        .map(|body| body.id)
}

/// The reader's live-view and scroll state.
#[derive(Default)]
pub struct Scheduling {
    /// A scheduling pass waits for the main loop.
    queued: Cell<bool>,
    /// What the column keeps in place while things above it change.
    anchor: RefCell<Option<Anchor>>,
    /// Scroll to the selected message at the next layout (a conversation was just shown), or to
    /// `scroll_target` (Next or Previous Message in Conversation).
    scroll_to: Cell<bool>,
    scroll_target: Cell<Option<MessageId>>,
    /// The shown page came on screen before its first screen was laid out: its selected
    /// message keeps its place while the bodies get their heights.
    settling: Cell<bool>,
    /// A widget to bring into view while it takes its new size over the next frames (the quick
    /// reply opening), and until when.
    reveal: RefCell<Option<(gtk::Widget, Instant)>>,
    clock: RefCell<Option<(gdk::FrameClock, glib::SignalHandlerId)>>,
    column: RefCell<Option<(gtk::Adjustment, Vec<glib::SignalHandlerId>)>>,
}

/// An entry of the shown page and its top (page coordinates) when the change began.
struct Anchor {
    widget: gtk::Widget,
    top: f64,
    until: Instant,
}

/// What the first screen of a page being laid out needs: the bodies that must be measured and
/// loaded before it shows (selected first), and those already done that it shows.
struct FirstScreen {
    needed: Vec<MessageId>,
    done: Vec<MessageId>,
}

/// `widget`'s top in `root`'s coordinates.
fn top_in(widget: &impl IsA<gtk::Widget>, root: &impl IsA<gtk::Widget>) -> Option<f64> {
    widget
        .as_ref()
        .compute_point(root.as_ref(), &graphene::Point::zero())
        .map(|point| f64::from(point.y()))
}

/// Whether the focus is in `html`'s view.
fn focused(html: &HtmlBody, focus: Option<&gtk::Widget>) -> bool {
    let (Some(view), Some(focus)) = (&html.view, focus) else {
        return false;
    };
    let web = view.web_view().upcast_ref::<gtk::Widget>();
    focus == web || focus.is_ancestor(web)
}

/// The expanded HTML bodies of `page`, with their place in it.
fn bodies(page: &Page, focus: Option<&gtk::Widget>) -> Vec<Body> {
    page.items()
        .filter_map(|item| {
            let html = item.card.as_ref()?.html.as_ref()?;
            let top = top_in(&html.frame, &page.root)?;
            Some(Body {
                id: item.id(),
                top,
                bottom: top + f64::from(html.frame.height()),
                live: html.view.is_some(),
                measured: html.measured_at(html.holder.width()),
                pinned: focused(html, focus),
            })
        })
        .collect()
}

/// The part of `page` the column shows once it is on screen, scrolled to the selected message:
/// from the top while the selected message starts in the first third of a screen, else from
/// just above it ([`opening_scroll`]).
fn first_window(page: &Page, height: f64) -> Option<(f64, f64)> {
    let selected = top_in(&page.item(page.selected)?.slot, &page.root)?;
    let top = opening_scroll(selected, heading_bottom(page), height, f64::INFINITY);
    Some((top, top + height))
}

/// Where the subject heading ends on `page` (0 without one).
fn heading_bottom(page: &Page) -> f64 {
    page.heading
        .as_ref()
        .and_then(|heading| {
            top_in(heading, &page.root).map(|top| top + f64::from(heading.height()))
        })
        .unwrap_or(0.0)
}

/// How far down a conversation opens, its selected message starting at `selected` (page
/// coordinates), the heading ending at `heading`, in a column `height` tall that can scroll at
/// most `end`: at the top while the message starts in the first third of a screen, else just
/// above the message; never with the heading cut in half (the top again then: the heading
/// stays whole whenever a scroll would leave part of it).
pub fn opening_scroll(selected: f64, heading: f64, height: f64, end: f64) -> f64 {
    if selected <= height / 3.0 {
        return 0.0;
    }
    let top = (selected - SCROLL_MARGIN).clamp(0.0, end.max(0.0));
    if top < heading { 0.0 } else { top }
}

/// Every expanded HTML body up to the selected message, and those below it that start within
/// the first screen, is needed; done once measured at its width and loaded, or only measured
/// when its images must come from the server (they fill in as they arrive).
fn first_screen(page: &Page, height: f64) -> Option<FirstScreen> {
    let window = first_window(page, height)?;
    let selected = page
        .entries
        .iter()
        .position(|entry| matches!(entry, Entry::Message(item) if item.id() == page.selected))?;
    let mut needed: Vec<(f64, MessageId)> = Vec::new();
    let mut done = Vec::new();
    for (index, entry) in page.entries.iter().enumerate() {
        let Entry::Message(item) = entry else {
            continue;
        };
        let Some(html) = item.card.as_ref().and_then(|card| card.html.as_ref()) else {
            continue;
        };
        let Some(top) = top_in(&html.frame, &page.root) else {
            continue;
        };
        let bottom = top + f64::from(html.frame.height());
        if index > selected && top >= window.1 {
            continue;
        }
        if html.measured_at(html.holder.width()) && (html.loaded || html.parts.waits_for_server()) {
            if bottom > window.0 && top < window.1 {
                done.push(item.id());
            }
        } else {
            let distance = if item.id() == page.selected {
                -1.0
            } else if bottom <= window.0 {
                window.0 - bottom
            } else {
                (top - window.1).max(0.0)
            };
            needed.push((distance, item.id()));
        }
    }
    needed.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(FirstScreen {
        needed: needed.into_iter().map(|(_, id)| id).collect(),
        done,
    })
}

impl Inner {
    fn scroller(&self) -> Option<gtk::ScrolledWindow> {
        self.stage
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
    }

    /// The column's height, while the reader is on screen.
    fn column_height(&self) -> Option<f64> {
        let height = f64::from(self.scroller()?.height());
        (self.stage.is_mapped() && height > 1.0).then_some(height)
    }

    /// The part of the shown `page` the column shows, in page coordinates: from the column's
    /// scroll value and the page's place in the scrolled content, which allocations only catch
    /// up with at the next layout.
    fn window(&self, page: &Page) -> Option<(f64, f64)> {
        let scroller = self.scroller()?;
        let content = scroller.child().and_downcast::<gtk::Viewport>()?.child()?;
        let adjustment = scroller.vadjustment();
        let top = adjustment.value() - top_in(&page.root, &content)?;
        let height = match adjustment.page_size() {
            size if size > 1.0 => size,
            _ => f64::from(scroller.height()),
        };
        Some((top, top + height))
    }

    /// Follows the column's scroll and size, and the window's layout, while the reader is on
    /// screen.
    pub(super) fn watch_column(self: &Rc<Self>) {
        self.unwatch_column();
        let Some(scroller) = self.scroller() else {
            return;
        };
        let adjustment = scroller.vadjustment();
        let weak = Rc::downgrade(self);
        let moved = adjustment.connect_value_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.schedule();
                inner.reply_moved();
            }
        });
        let weak = Rc::downgrade(self);
        let resized = adjustment.connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.schedule();
            }
        });
        self.scheduling
            .column
            .replace(Some((adjustment, vec![moved, resized])));
        if let Some(clock) = self.stage.frame_clock() {
            let weak = Rc::downgrade(self);
            let handler = clock.connect_layout(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.on_layout();
                }
            });
            self.scheduling.clock.replace(Some((clock, handler)));
        }
    }

    /// The column scrolled: an open inline reply moved with it ([`Reader::connect_reply_moved`]).
    /// The reader may scroll it while it holds its pages; then the next scroll tells.
    fn reply_moved(&self) {
        let open = self.shown.try_borrow().is_ok_and(|shown| {
            shown
                .as_ref()
                .and_then(|page| page.quick.as_ref())
                .is_some_and(|quick| quick.is_open())
        });
        if !open {
            return;
        }
        let listeners = self.on_reply_moved.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    pub(super) fn unwatch_column(&self) {
        if let Some((adjustment, handlers)) = self.scheduling.column.take() {
            for handler in handlers {
                adjustment.disconnect(handler);
            }
        }
        if let Some((clock, handler)) = self.scheduling.clock.take() {
            clock.disconnect(handler);
        }
    }

    /// Chooses which bodies hold views once the main loop is free.
    pub(super) fn schedule(self: &Rc<Self>) {
        if self.scheduling.queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(inner) = weak.upgrade() {
                inner.scheduling.queued.set(false);
                inner.schedule_now();
            }
        });
    }

    /// Gives views to the bodies that need them and takes them from the others: first the
    /// bodies the shown conversation shows (and a focused one), then the first screen of the
    /// conversation being shown, then the first screens of those laid out ahead in the order
    /// they are wanted, then the shown conversation's bodies near the visible part, then one more
    /// to measure a body not measured yet. One page holds at most [`PAGE_VIEWS`].
    pub(super) fn schedule_now(self: &Rc<Self>) {
        let Some(height) = self.column_height() else {
            return;
        };
        let margin = height * MARGIN_SCREENS;
        let focus = self.stage.root().and_then(|root| root.focus());
        let mut keep: Vec<(u64, MessageId)> = Vec::new();
        let shown = self.shown.borrow().as_ref().and_then(|page| {
            let window = self.window(page)?;
            Some((page.key, bodies(page, focus.as_ref()), window))
        });
        // What the column shows keeps (or gets) its views whatever else waits.
        if let Some((key, bodies, window)) = &shown {
            keep.extend(
                wanted(bodies, *window, 0.0, PAGE_VIEWS)
                    .into_iter()
                    .map(|id| (*key, id)),
            );
        }
        let showing = keep.len();
        let unseen = |page: &Page| {
            (self.stage.is_staged(&page.root) && page.root.width() > 1)
                .then(|| first_screen(page, height))
                .flatten()
                .map(|screen| (page.key, screen))
        };
        let pending = self.pending.borrow().as_ref().and_then(unseen);
        let warm: Vec<(u64, FirstScreen)> = self
            .warm
            .borrow()
            .iter()
            .filter(|page| page.built)
            .filter_map(unseen)
            .collect();
        for (index, (key, screen)) in pending.iter().chain(&warm).enumerate() {
            let room = MAX_LIVE_VIEWS.saturating_sub(keep.len());
            // The conversation being shown gets a view even when every one is on screen.
            let room = if index == 0 && pending.is_some() {
                room.max(1)
            } else {
                room
            };
            keep.extend(
                screen
                    .needed
                    .iter()
                    .chain(&screen.done)
                    .take(room.min(PAGE_VIEWS))
                    .map(|id| (*key, *id)),
            );
        }
        if let Some((key, bodies, window)) = &shown {
            let budget = MAX_LIVE_VIEWS
                .saturating_sub(keep.len())
                .min(PAGE_VIEWS.saturating_sub(showing));
            let kept = |id: &MessageId| keep.contains(&(*key, *id));
            let mut chosen: Vec<MessageId> = wanted(bodies, *window, margin, PAGE_VIEWS)
                .into_iter()
                .filter(|id| !kept(id))
                .take(budget)
                .collect();
            for body in bodies {
                if chosen.len() < budget
                    && body.live
                    && !body.measured
                    && !chosen.contains(&body.id)
                    && !kept(&body.id)
                {
                    chosen.push(body.id);
                }
            }
            if chosen.len() < budget
                && pending.is_none()
                && let Some(next) = next_to_measure(bodies, *window).filter(|id| !kept(id))
            {
                chosen.push(next);
            }
            keep.extend(chosen.into_iter().map(|id| (*key, id)));
        }
        tracing::trace!(
            window = ?shown.as_ref().map(|(_, _, window)| *window),
            bodies = shown.as_ref().map_or(0, |(_, bodies, _)| bodies.len()),
            keep = ?keep.iter().map(|(_, id)| id.0).collect::<Vec<_>>(),
            "reader: live views chosen"
        );
        self.apply_views(&keep, focus.as_ref());
    }

    /// Takes views from the bodies not in `keep` (not from one with the focus) and gives views
    /// to those in it, in its order, as long as the pool has one.
    fn apply_views(self: &Rc<Self>, keep: &[(u64, MessageId)], focus: Option<&gtk::Widget>) {
        let mut released = Vec::new();
        let mut release = |page: &mut Page| {
            let key = page.key;
            for item in page.items_mut() {
                let id = item.id();
                let Some(html) = item.card.as_mut().and_then(|card| card.html.as_mut()) else {
                    continue;
                };
                if html.view.is_none() || keep.contains(&(key, id)) || focused(html, focus) {
                    continue;
                }
                if let Some(view) = html.view.take() {
                    html.holder
                        .set_placeholder(html.measured.map_or(view.height(), |(height, _)| height));
                    released.push(view);
                }
            }
        };
        for pages in self.pages() {
            if let Some(page) = pages.borrow_mut().as_mut() {
                release(page);
            }
        }
        for page in self.warm.borrow_mut().iter_mut() {
            release(page);
        }
        for view in released {
            self.engine.release(view);
        }
        let fonts = self.theme.theme().webview_fonts();
        for (key, id) in keep {
            let lacks = self
                .with_page(*key, |page| {
                    page.item(*id)
                        .and_then(|item| item.card.as_ref())
                        .and_then(|card| card.html.as_ref())
                        .is_some_and(|html| html.view.is_none())
                })
                .unwrap_or(false);
            if !lacks {
                continue;
            }
            let Some(view) = self.engine.acquire(&fonts) else {
                break;
            };
            self.attach(*key, *id, view);
        }
    }

    /// Loads message `id`'s body into `view`, at the height it was measured at, with the content
    /// filter unless its remote images are allowed.
    fn attach(self: &Rc<Self>, key: u64, id: MessageId, view: MessageView) {
        let theme = self.theme.theme();
        let filter = self.filter.borrow().clone();
        let token = self.next_token();
        let attached = self
            .with_page(key, |page| {
                let card = page.item_mut(id)?.card.as_mut()?;
                let allowed = self.remote.allows(id, &card.sender);
                let html = card.html.as_mut()?;
                let filter = match (allowed || html.remote_allowed, &filter) {
                    (true, _) => None,
                    (false, Some(filter)) => Some(filter),
                    (false, None) => return None,
                };
                html.token = token;
                html.loaded = false;
                html.painted = false;
                html.changed = Instant::now();
                html.quotes_open = false;
                let height = html.measured.map_or(1, |(height, _)| height);
                view.web_view().set_size_request(-1, height);
                html.holder.set_child(Some(view.web_view()));
                let weak = Rc::downgrade(self);
                view.set_handler(move |event| {
                    if let Some(inner) = weak.upgrade() {
                        inner.body_event(key, id, token, event);
                    }
                });
                self.engine.set_parts(view.page_id(), html.parts.clone());
                view.set_collapse_quotes(html.collapse_quotes);
                view.set_fit_margins(html.style == BodyStyle::Paper);
                view.load(
                    &html.html,
                    &stylesheet(&theme, html.style),
                    &rgba(theme.webview_background(html.style)),
                    filter,
                );
                html.view = Some(view.clone());
                Some(())
            })
            .flatten();
        if attached.is_none() {
            self.engine.release(view);
            return;
        }
        self.focus_loaded_body(key, id);
    }

    /// Whether `page`, laid out unseen, still waits for a body its first screen needs (always,
    /// before the stage allocated it).
    pub(super) fn waiting(&self, page: &Page) -> bool {
        if !page.has_html() {
            return false;
        }
        if !self.stage.is_staged(&page.root) || page.root.width() <= 1 || self.layout_held.get() {
            return true;
        }
        let Some(height) = self.column_height() else {
            return true;
        };
        first_screen(page, height).is_none_or(|screen| {
            // Swapped in, its bodies on screen show at once: drawn by WebKit already.
            !screen.needed.is_empty()
                || page.items().any(|item| {
                    screen.done.contains(&item.id())
                        && item
                            .card
                            .as_ref()
                            .and_then(|card| card.html.as_ref())
                            .is_some_and(|html| html.view.is_none() || !html.painted)
                })
        })
    }

    /// Whether every view on `page` has settled at `now`: its document loaded (images, `cid:`
    /// parts and fonts in), a frame painted, measured at its width, and nothing of that changed
    /// for [`SETTLE`]. Drawing it again would only show what it shows (or its animations).
    pub(super) fn settled(page: &Page, now: Instant) -> bool {
        page.built
            && page.items().all(|item| {
                item.card
                    .as_ref()
                    .and_then(|card| card.html.as_ref())
                    .filter(|html| html.view.is_some())
                    .is_none_or(|html| {
                        html.loaded
                            && html.painted
                            && html.measured_at(html.holder.width())
                            && now.saturating_duration_since(html.changed) >= SETTLE
                    })
            })
    }

    /// Whether the staged page `root` may go undrawn ([`widgets::Stage::set_quiet`]): one laid
    /// out ahead that is ready to swap in and has settled. The page being laid out to be shown
    /// never is.
    pub(super) fn quiet(&self, root: &gtk::Widget) -> bool {
        let Ok(warm) = self.warm.try_borrow() else {
            return false;
        };
        let now = Instant::now();
        warm.iter()
            .find(|page| page.root.upcast_ref::<gtk::Widget>() == root)
            .is_some_and(|page| Self::settled(page, now) && self.ready(page))
    }

    /// Whether every page laid out ahead has settled, and there is one: for checks.
    pub(super) fn ahead_settled(&self) -> bool {
        let now = Instant::now();
        let warm = self.warm.borrow();
        !warm.is_empty()
            && warm
                .iter()
                .all(|page| Self::settled(page, now) && self.ready(page))
    }

    /// Whether nothing in the reader waits for a frame: no conversation is being read or laid
    /// out, the shown one has settled, and so has every one laid out ahead. A reader warm
    /// behind the list is then left undrawn ([`crate::window::Window`]'s panes).
    pub(super) fn is_quiet(&self) -> bool {
        let (Ok(shown), Ok(pending), Ok(warm)) = (
            self.shown.try_borrow(),
            self.pending.try_borrow(),
            self.warm.try_borrow(),
        ) else {
            return false;
        };
        let now = Instant::now();
        pending.is_none()
            && self.warming.get().is_none()
            && shown.as_ref().is_none_or(|page| Self::settled(page, now))
            && warm
                .iter()
                .all(|page| Self::settled(page, now) && self.ready(page))
    }

    /// A view's load changed (its `load` event, a new size): a page left undrawn, laid out
    /// ahead or in a reader warm behind the list, is drawn again until it settles. GTK passes
    /// a draw up only as far as a widget that already waits for one, and a page its parent
    /// left undrawn goes on waiting, so the draw goes to the stage and every widget above it.
    pub(super) fn redraw_unseen(&self) {
        widgets::redraw_up(&self.stage);
    }

    /// Whether a page is being laid out unseen: the one being shown, or one laid out ahead
    /// whose first screen isn't in yet.
    fn laying_out(&self) -> bool {
        self.pending
            .borrow()
            .as_ref()
            .is_some_and(|page| self.stage.is_staged(&page.root))
            || self
                .warm
                .borrow()
                .iter()
                .any(|page| page.built && self.stage.is_staged(&page.root) && self.waiting(page))
    }

    /// Whether the page `key` being laid out still waits for a body its first screen needs;
    /// `None` once it isn't being laid out.
    pub(super) fn first_screen_waiting(&self, key: u64) -> Option<bool> {
        let pending = self.pending.borrow();
        let page = pending.as_ref().filter(|page| page.key == key)?;
        if page.root.width() <= 1 || self.layout_held.get() {
            return Some(true);
        }
        let Some(height) = self.column_height() else {
            return Some(false);
        };
        Some(first_screen(page, height).is_some_and(|screen| !screen.needed.is_empty()))
    }

    /// The page just shown came on screen before its first screen was laid out (`early`): its
    /// selected message keeps its place until it is.
    pub(super) fn settle(&self, early: bool) {
        self.scheduling.settling.set(early);
    }

    /// Keeps what the column shows in place while what is above it changes: the topmost entry
    /// it shows keeps its place on screen for [`HOLD`]; the selected message instead while the
    /// page is [settling](Self::settle).
    pub(super) fn hold_scroll(&self) {
        let now = Instant::now();
        let mut anchor = self.scheduling.anchor.borrow_mut();
        if let Some(held) = anchor.as_mut().filter(|held| held.until > now) {
            held.until = now + HOLD;
            return;
        }
        let shown = self.shown.borrow();
        let Some(page) = shown.as_ref() else {
            return;
        };
        if self.scheduling.settling.get() {
            let laid_out = self
                .column_height()
                .and_then(|height| first_screen(page, height))
                .is_some_and(|screen| screen.needed.is_empty());
            if laid_out {
                self.scheduling.settling.set(false);
            } else if let Some(item) = page.item(page.selected) {
                *anchor = top_in(&item.slot, &page.root).map(|top| Anchor {
                    widget: item.slot.clone().upcast(),
                    top,
                    until: now + HOLD,
                });
                return;
            }
        }
        let Some((top, _)) = self.window(page) else {
            return;
        };
        *anchor = page.entries.iter().find_map(|entry| {
            let widget = entry_widget(entry);
            let start = top_in(&widget, &page.root)?;
            (start + f64::from(widget.height()) > top).then_some(Anchor {
                widget,
                top: start,
                until: now + HOLD,
            })
        });
    }

    /// [`Inner::hold_scroll`], holding message `id`'s row or card in place when the column shows
    /// it (Expand All keeps the message being read where it is, whatever opens above it).
    pub(super) fn hold_scroll_at(&self, id: MessageId) {
        let held = {
            let shown = self.shown.borrow();
            shown.as_ref().and_then(|page| {
                let slot = &page.item(id)?.slot;
                let start = top_in(slot, &page.root)?;
                let (top, bottom) = self.window(page)?;
                (start + f64::from(slot.height()) > top && start < bottom).then(|| Anchor {
                    widget: slot.clone().upcast(),
                    top: start,
                    until: Instant::now() + HOLD,
                })
            })
        };
        match held {
            Some(anchor) => {
                self.scheduling.anchor.replace(Some(anchor));
            }
            None => self.hold_scroll(),
        }
    }

    /// Scrolls the column `pages` of its height down, or up when negative, within the page:
    /// [`HALF_PAGE`] for Scroll Message Down and Up (issue #10), [`PAGE`] for Scroll Message
    /// Page Down and Up.
    pub(super) fn scroll_pages(&self, pages: f64) {
        let Some(scroller) = self.scroller() else {
            return;
        };
        let adjustment = scroller.vadjustment();
        self.scheduling.anchor.replace(None);
        adjustment.set_value(scrolled(
            adjustment.value(),
            adjustment.lower(),
            adjustment.upper(),
            adjustment.page_size(),
            pages,
        ));
    }

    /// The conversation just shown opens at its selected message.
    pub(super) fn scroll_to_selected(&self) {
        self.scheduling.anchor.replace(None);
        self.scheduling.scroll_target.set(None);
        self.scheduling.scroll_to.set(true);
        self.stage.queue_resize();
    }

    /// Scrolls the column to message `id`'s row or card at the next layout, its top a margin
    /// below the column's.
    pub(super) fn scroll_to_message(&self, id: MessageId) {
        self.scheduling.anchor.replace(None);
        self.scheduling.scroll_target.set(Some(id));
        self.scheduling.scroll_to.set(true);
        self.stage.queue_resize();
    }

    /// Whether a widget is being brought into view ([`Inner::reveal_in_column`]), for checks.
    pub(super) fn revealing(&self) -> bool {
        self.scheduling
            .reveal
            .borrow()
            .as_ref()
            .is_some_and(|(_, until)| *until > Instant::now())
    }

    /// Lets a widget brought into view go: the user left it (the inline reply closed), so the
    /// column scrolls freely again before [`REVEAL`] is over.
    pub(super) fn end_reveal(&self, widget: &impl IsA<gtk::Widget>) {
        let mut reveal = self.scheduling.reveal.borrow_mut();
        if reveal
            .as_ref()
            .is_some_and(|(revealed, _)| revealed == widget.as_ref())
        {
            reveal.take();
        }
    }

    /// Scrolls the column just enough to show `widget` whole (its top when it is taller than
    /// the column), at every layout for [`REVEAL`] while it settles at its new size.
    pub(super) fn reveal_in_column(&self, widget: &impl IsA<gtk::Widget>) {
        self.scheduling
            .reveal
            .replace(Some((widget.as_ref().clone(), Instant::now() + REVEAL)));
        self.scheduling.anchor.replace(None);
        self.stage.queue_resize();
    }

    /// After GTK's layout in a frame: a page being laid out unseen may be ready now (its
    /// waits wake, its views are chosen again); then scrolls to the selected message, or moves
    /// the scroll by as much as the held entry moved; the frame clock lays out again before
    /// painting.
    fn on_layout(self: &Rc<Self>) {
        if self.laying_out() {
            self.wake();
            self.schedule();
        }
        let Some(scroller) = self.scroller() else {
            return;
        };
        let adjustment = scroller.vadjustment();
        let reveal = self
            .scheduling
            .reveal
            .borrow()
            .clone()
            .filter(|(_, until)| *until > Instant::now());
        // The widget's drawn box (border and padding included) where the column shows it.
        if let Some((widget, _)) = reveal
            && let Some(viewport) = scroller.child()
            && let Some(bounds) = widget.compute_bounds(&viewport)
        {
            let top = f64::from(bounds.y());
            let bottom = top + f64::from(bounds.height());
            let heading = self
                .shown
                .borrow()
                .as_ref()
                .and_then(|page| page.heading.clone())
                .and_then(|heading| heading.compute_bounds(&viewport))
                .map(|bounds| {
                    let top = f64::from(bounds.y());
                    (top, top + f64::from(bounds.height()))
                });
            let room = adjustment.upper() - adjustment.page_size() - adjustment.value();
            let delta = reveal_delta(top, bottom, adjustment.page_size(), heading, room);
            if delta.abs() >= 0.5 {
                adjustment.set_value(adjustment.value() + delta);
            }
            return;
        }
        self.scheduling.reveal.replace(None);
        if self.scheduling.scroll_to.get() {
            if let Some(value) = self.selected_scroll(&scroller) {
                self.scheduling.scroll_to.set(false);
                adjustment.set_value(value);
            }
            return;
        }
        let mut anchor = self.scheduling.anchor.borrow_mut();
        let Some(held) = anchor.as_mut() else {
            return;
        };
        let shown = self.shown.borrow();
        let top = shown
            .as_ref()
            .filter(|page| held.until > Instant::now() && held.widget.is_ancestor(&page.root))
            .and_then(|page| top_in(&held.widget, &page.root));
        let Some(top) = top else {
            *anchor = None;
            return;
        };
        let delta = top - held.top;
        held.top = top;
        if delta.abs() >= 0.5 {
            adjustment.set_value(adjustment.value() + delta);
        }
    }

    /// Where message `id`'s row or card starts in the column, px from the top of what it shows.
    pub(super) fn message_top(&self, id: MessageId) -> Option<f64> {
        let shown = self.shown.borrow();
        top_in(&shown.as_ref()?.item(id)?.slot, &self.scroller()?)
    }

    /// The shown conversation's expanded HTML bodies, where the column shows them.
    pub(super) fn body_reports(&self) -> Vec<super::BodyReport> {
        let shown = self.shown.borrow();
        let (Some(page), Some(scroller)) = (shown.as_ref(), self.scroller()) else {
            return Vec::new();
        };
        page.items()
            .filter_map(|item| {
                let html = item.card.as_ref()?.html.as_ref()?;
                let top = top_in(&html.frame, &scroller)?;
                Some(super::BodyReport {
                    id: item.id(),
                    top,
                    bottom: top + f64::from(html.frame.height()),
                    live: html.view.is_some(),
                    loaded: html.loaded,
                    painted: html.painted,
                    measured: html.measured.map(|(height, _)| height),
                    height: html.holder.height(),
                })
            })
            .collect()
    }

    /// Where the column scrolls to show the selected message, once the page is laid out.
    fn selected_scroll(&self, scroller: &gtk::ScrolledWindow) -> Option<f64> {
        let shown = self.shown.borrow();
        let page = shown.as_ref()?;
        if page.root.width() <= 1 {
            return None;
        }
        let root = top_in(&page.root, scroller)? + scroller.vadjustment().value();
        if let Some(target) = self.scheduling.scroll_target.get() {
            let top = top_in(&page.item(target)?.slot, &page.root)?;
            self.scheduling.scroll_target.set(None);
            return Some(root + top - SCROLL_MARGIN);
        }
        let item = page.item(page.selected)?;
        let selected = top_in(&item.slot, &page.root)?;
        let adjustment = scroller.vadjustment();
        let height = f64::from(scroller.height());
        let end = adjustment.upper() - adjustment.page_size() - root;
        // At the top: the column's own top, its margin above the page included.
        Some(
            match opening_scroll(selected, heading_bottom(page), height, end) {
                top if top <= 0.0 => 0.0,
                top => root + top,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_scrolls_by_pages_within_its_ends() {
        // 800 px of a 3,000 px column on screen.
        assert_eq!(scrolled(0.0, 0.0, 3000.0, 800.0, HALF_PAGE), 400.0);
        assert_eq!(scrolled(400.0, 0.0, 3000.0, 800.0, -HALF_PAGE), 0.0);
        assert_eq!(scrolled(0.0, 0.0, 3000.0, 800.0, PAGE), 720.0);
        assert_eq!(scrolled(1000.0, 0.0, 3000.0, 800.0, -PAGE), 280.0);
        // Never past the foot or the top.
        assert_eq!(scrolled(2000.0, 0.0, 3000.0, 800.0, PAGE), 2200.0);
        assert_eq!(scrolled(100.0, 0.0, 3000.0, 800.0, -PAGE), 0.0);
        // A column shorter than the view stays where it is.
        assert_eq!(scrolled(0.0, 0.0, 500.0, 800.0, PAGE), 0.0);
    }

    #[test]
    fn a_widget_brought_into_view_never_leaves_the_heading_cut() {
        // The trip at 1440 × 900, measured: the heading's box at 24..68 in the column.
        let heading = Some((24.0, 68.0));
        // In view: nothing moves.
        assert_eq!(reveal_delta(300.0, 500.0, 851.0, heading, 400.0), 0.0);
        // The inline reply opening at the column's foot needs 28 px with its margin, which
        // left the heading's letters against the toolbar: the heading goes out of view whole,
        // the reply 52 px above the view's foot...
        assert_eq!(reveal_delta(680.0, 867.0, 851.0, heading, 112.0), 68.0);
        assert_eq!(reveal_delta(680.0, 900.0, 851.0, heading, 400.0), 68.0);
        // ... where the column can scroll that far; else the heading keeps 4 px above its box
        // and the reply all of it, with less room below...
        assert_eq!(reveal_delta(680.0, 867.0, 851.0, heading, 50.0), 20.0);
        // ... or, when that can't show all of it, what the widget needs.
        assert_eq!(reveal_delta(680.0, 900.0, 851.0, heading, 60.0), 61.0);
        // Far enough down, the heading keeps its room: only what the widget needs.
        assert_eq!(
            reveal_delta(682.0, 866.0, 850.0, Some((137.0, 176.0)), 400.0),
            28.0
        );
        // The heading out of view already: what the widget needs.
        assert_eq!(
            reveal_delta(682.0, 866.0, 850.0, Some((-200.0, -150.0)), 400.0),
            28.0
        );
        // A widget taller than the view: its top, whatever the heading.
        assert_eq!(
            reveal_delta(50.0, 900.0, 850.0, Some((10.0, 60.0)), 400.0),
            38.0
        );
        // Above the view: up to its top, whatever the heading.
        assert_eq!(
            reveal_delta(-40.0, 60.0, 850.0, Some((-90.0, -50.0)), 400.0),
            -52.0
        );
        // Without a heading (a message gone).
        assert_eq!(reveal_delta(682.0, 866.0, 850.0, None, 400.0), 28.0);
    }

    fn body(id: i64, top: f64, height: f64) -> Body {
        Body {
            id: MessageId(id),
            top,
            bottom: top + height,
            live: false,
            measured: true,
            pinned: false,
        }
    }

    fn ids(list: &[MessageId]) -> Vec<i64> {
        list.iter().map(|id| id.0).collect()
    }

    /// Eight 1,500 px newsletters one under the other, read through a 900 px window.
    fn newsletters() -> Vec<Body> {
        (0..8)
            .map(|n| body(n, n as f64 * 1_520.0, 1_500.0))
            .collect()
    }

    #[test]
    fn the_visible_and_nearest_bodies_hold_views_up_to_the_cap() {
        let bodies = newsletters();
        assert_eq!(ids(&wanted(&bodies, (0.0, 900.0), 900.0, 4)), [0, 1]);
        assert_eq!(
            ids(&wanted(&bodies, (3_000.0, 3_900.0), 900.0, 4)),
            [1, 2, 3]
        );
        assert_eq!(ids(&wanted(&bodies, (3_000.0, 3_900.0), 900.0, 2)), [1, 2]);
        // Short bodies: more fit near the window than the cap allows.
        let short: Vec<Body> = (0..10).map(|n| body(n, n as f64 * 200.0, 180.0)).collect();
        let chosen = wanted(&short, (800.0, 1_700.0), 900.0, 4);
        assert_eq!(chosen.len(), 4);
        assert!(
            ids(&chosen).iter().all(|id| (4..=8).contains(id)),
            "{chosen:?}"
        );
    }

    #[test]
    fn a_focused_body_keeps_its_view_and_live_ones_win_ties() {
        let mut bodies = newsletters();
        bodies[7].pinned = true;
        bodies[7].live = true;
        assert_eq!(ids(&wanted(&bodies, (0.0, 900.0), 900.0, 2)), [7, 0]);
        let mut pair = vec![body(1, 0.0, 100.0), body(2, 900.0, 100.0)];
        pair[1].live = true;
        assert_eq!(ids(&wanted(&pair, (450.0, 550.0), 900.0, 1)), [2]);
        pair[1].live = false;
        assert_eq!(ids(&wanted(&pair, (450.0, 550.0), 900.0, 1)), [1]);
    }

    #[test]
    fn a_conversation_opens_with_its_heading_whole_or_out_of_sight() {
        // The selected message in the first third of the column: from the top.
        assert_eq!(opening_scroll(250.0, 60.0, 900.0, 4_000.0), 0.0);
        // Further down: just above it, the heading scrolled away.
        assert_eq!(opening_scroll(700.0, 60.0, 900.0, 4_000.0), 688.0);
        // A conversation a little taller than the column, scrolled to its end, would cut the
        // heading in half under the toolbar: it opens at the top instead.
        assert_eq!(opening_scroll(700.0, 60.0, 900.0, 40.0), 0.0);
        // The end lies past the heading: the end, the heading out of sight.
        assert_eq!(opening_scroll(700.0, 60.0, 900.0, 300.0), 300.0);
        // Nothing to scroll.
        assert_eq!(opening_scroll(700.0, 60.0, 900.0, -5.0), 0.0);
    }

    #[test]
    fn the_nearest_unmeasured_body_is_measured_next() {
        let mut bodies = newsletters();
        for body in &mut bodies {
            body.measured = false;
        }
        bodies[1].live = true;
        assert_eq!(
            next_to_measure(&bodies, (3_000.0, 3_900.0)),
            Some(MessageId(2))
        );
        bodies[2].measured = true;
        assert_eq!(
            next_to_measure(&bodies, (3_000.0, 3_900.0)),
            Some(MessageId(3))
        );
        for body in &mut bodies {
            body.measured = true;
        }
        assert_eq!(next_to_measure(&bodies, (0.0, 900.0)), None);
    }
}
