//! The composer's dock (the user's UI tweak, 2026-10-08): drafts open as cards over the panes,
//! at the window's bottom right, as Gmail's composer does, instead of windows of their own. The
//! first card sits flush with the window's right and bottom edges (the fork's phase B: a 16 px
//! strip beside it showed the list's dates and scrollbar through, as a glitch), and each newer
//! one to its left; a card can be open,
//! minimized to its header, or expanded to the middle of the panes over a dimmed backdrop (the
//! other cards hide meanwhile; a click on the backdrop shrinks it back). Cards that no longer fit
//! minimize, the oldest first. While one pane fills the window an open card fills the panes
//! above the minimized ones. The dock only places cards: what a state shows is the card's own
//! business, told through the handler [`Dock::add`] takes.

use super::NARROW_WIDTH;
use super::panes::{classed, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// An open card, in px, while the window has room for it.
pub const CARD_SIZE: (i32, i32) = (640, 600);
/// A minimized card's width.
pub const MINIMIZED_WIDTH: i32 = 300;
/// An expanded card at most, and the room it keeps from the panes' edges.
pub const EXPANDED_SIZE: (i32, i32) = (1000, 820);
pub const EXPANDED_MARGIN: i32 = 32;
/// The room right of the first card (none: it is flush with the window's edge), and between
/// cards.
pub const EDGE: i32 = 0;
pub const GAP: i32 = 12;
/// The class of a card flush with the window's right edge, and of one that fills the panes'
/// width while one pane fills the window: their outer borders and corners go.
pub const EDGE_CLASS: &str = "sw-dock-edge";
pub const FULL_CLASS: &str = "sw-dock-full";
/// The room an open card leaves above it, so the panes' bars still show.
pub const TOP_ROOM: i32 = 48;
/// The room a toast keeps at each side (libadwaita's margin), for [`Dock::keep_toasts_clear`].
pub const TOAST_SIDE: i32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardState {
    Open,
    Minimized,
    Expanded,
}

impl CardState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Minimized => "minimized",
            Self::Expanded => "expanded",
        }
    }
}

type StateHandler = Rc<dyn Fn(CardState)>;
type OpenHandler = Rc<dyn Fn(bool)>;

struct Card {
    widget: gtk::Widget,
    state: Cell<CardState>,
    on_state: StateHandler,
}

/// The cards over the panes of one window.
#[derive(Clone)]
pub struct Dock {
    inner: Rc<Inner>,
}

/// A dock that doesn't keep its window's widgets alive.
#[derive(Clone, Default)]
pub struct WeakDock {
    inner: Weak<Inner>,
}

impl WeakDock {
    pub fn upgrade(&self) -> Option<Dock> {
        self.inner.upgrade().map(|inner| Dock { inner })
    }
}

struct Inner {
    overlay: gtk::Overlay,
    /// Under the cards, over the panes, while a card is expanded.
    backdrop: gtk::Box,
    /// Oldest first: the rightmost card first.
    cards: RefCell<Vec<Rc<Card>>>,
    /// Minimizing the cards a smaller window crowds is due (`place` can't change states).
    tidy_due: Cell<bool>,
    /// The cards' edge classes are due, for the width `place` saw (it can't restyle).
    classes_due: Cell<bool>,
    width: Cell<i32>,
    /// The window's toasts, kept clear of the cards ([`Dock::keep_toasts_clear`]), the
    /// overlay's children as a model (it says when a toast's widget comes), and the dock's width
    /// when the toasts were last placed.
    toasts: glib::WeakRef<adw::ToastOverlay>,
    toast_children: RefCell<Option<gio::ListModel>>,
    toasts_placed: Cell<Option<i32>>,
    toasts_due: Cell<bool>,
    /// Told whether a card is open over the panes ([`Dock::connect_open`]), and what they were
    /// told last.
    open_handlers: RefCell<Vec<OpenHandler>>,
    told_open: Cell<Option<bool>>,
    /// What else toasts keep clear of, where the toast overlay shows it: the open inline reply
    /// ([`Dock::keep_toasts_off`]).
    toasts_avoid: RefCell<Option<Avoid>>,
}

/// Where a widget toasts keep clear of is, in the toast overlay's coordinates.
type Avoid = Box<dyn Fn(&gtk::Widget) -> Option<gtk::graphene::Rect>>;

impl Dock {
    /// A dock over `panes`; [`Dock::widget`] takes their place in the window.
    pub fn new(panes: &impl IsA<gtk::Widget>) -> Self {
        let overlay = named(
            gtk::Overlay::builder()
                .child(panes)
                .vexpand(true)
                .hexpand(true)
                .build(),
            "composer-dock",
        );
        let backdrop = named(
            classed(
                gtk::Box::builder()
                    .visible(false)
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build(),
                &["sw-composer-backdrop"],
            ),
            "composer-backdrop",
        );
        overlay.add_overlay(&backdrop);
        let inner = Rc::new(Inner {
            overlay,
            backdrop,
            cards: RefCell::new(Vec::new()),
            tidy_due: Cell::new(false),
            classes_due: Cell::new(false),
            width: Cell::new(0),
            toasts: glib::WeakRef::new(),
            toast_children: RefCell::new(None),
            toasts_placed: Cell::new(None),
            toasts_due: Cell::new(false),
            open_handlers: RefCell::new(Vec::new()),
            told_open: Cell::new(None),
            toasts_avoid: RefCell::new(None),
        });
        let weak = Rc::downgrade(&inner);
        inner
            .overlay
            .connect_get_child_position(move |overlay, widget| {
                let inner = weak.upgrade()?;
                inner.place(overlay, widget)
            });
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&inner);
        click.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(inner) = weak.upgrade()
                && let Some(card) = inner.expanded()
            {
                inner.set_state(&card.widget, CardState::Open);
            }
        });
        inner.backdrop.add_controller(click);
        Self { inner }
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.inner.overlay
    }

    pub fn downgrade(&self) -> WeakDock {
        WeakDock {
            inner: Rc::downgrade(&self.inner),
        }
    }

    /// Docks `widget` open, left of the cards there are; `on_state` hears every state it takes,
    /// this first one included.
    pub fn add(&self, widget: &impl IsA<gtk::Widget>, on_state: impl Fn(CardState) + 'static) {
        let inner = &self.inner;
        let widget = widget.as_ref().clone();
        // The dock gives each card its rectangle (`place`); Fill keeps GTK from aligning it
        // inside.
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        let on_state: StateHandler = Rc::new(on_state);
        if let Some(expanded) = inner.expanded() {
            inner.set_state(&expanded.widget, CardState::Open);
        }
        inner.cards.borrow_mut().push(Rc::new(Card {
            widget: widget.clone(),
            state: Cell::new(CardState::Open),
            on_state: on_state.clone(),
        }));
        inner.overlay.add_overlay(&widget);
        on_state(CardState::Open);
        inner.make_room(&widget);
        inner.relayout();
    }

    /// Takes `widget` out of the dock.
    pub fn remove(&self, widget: &impl IsA<gtk::Widget>) {
        let inner = &self.inner;
        let widget = widget.as_ref();
        let removed = {
            let mut cards = inner.cards.borrow_mut();
            let before = cards.len();
            cards.retain(|card| &card.widget != widget);
            cards.len() != before
        };
        if removed {
            inner.overlay.remove_overlay(widget);
            inner.relayout();
        }
    }

    pub fn state(&self, widget: &impl IsA<gtk::Widget>) -> Option<CardState> {
        self.inner
            .card(widget.as_ref())
            .map(|card| card.state.get())
    }

    /// Puts `widget` in `state`: expanding one card puts an expanded other back open, and
    /// opening one minimizes the oldest others that no longer fit.
    pub fn set_state(&self, widget: &impl IsA<gtk::Widget>, state: CardState) {
        self.inner.set_state(widget.as_ref(), state);
    }

    /// The docked widgets, oldest first.
    pub fn cards(&self) -> Vec<gtk::Widget> {
        self.inner
            .cards
            .borrow()
            .iter()
            .map(|card| card.widget.clone())
            .collect()
    }

    /// Whether the backdrop of an expanded card shows.
    pub fn backdrop_shown(&self) -> bool {
        self.inner.backdrop.is_visible()
    }

    /// Runs `handler` with whether a card is open over the panes (open or expanded; a
    /// minimized one is its header alone), whenever that changes: the contact pane in the
    /// reader's margin hides under one (phase C).
    pub fn connect_open(&self, handler: impl Fn(bool) + 'static) {
        self.inner.open_handlers.borrow_mut().push(Rc::new(handler));
    }

    /// Keeps `toasts`' toasts clear of the cards (the fork's phase B): libadwaita centres a
    /// toast at the window's bottom, where at 1440 px it covered a card's Send. While cards are
    /// docked at the bottom right and the room left of them holds a toast, the toast is centred
    /// in that room instead (a margin at its right as wide as the cards); else on the window, as
    /// before. A toast's widget is made when it shows, so the overlay's children are watched
    /// (a few children: the cost `observe_children` warns of is for long lists).
    pub fn keep_toasts_clear(&self, toasts: &adw::ToastOverlay) {
        let inner = &self.inner;
        inner.toasts.set(Some(toasts));
        let children = toasts.observe_children();
        let weak = Rc::downgrade(inner);
        children.connect_items_changed(move |_, _, _, added| {
            if added > 0
                && let Some(inner) = weak.upgrade()
            {
                inner.place_toasts();
            }
        });
        inner.toast_children.replace(Some(children));
    }

    /// Keeps the toasts off what `area` says (where the toast overlay shows it, `None` when
    /// nothing): the inline reply while it is open (phase C's visual QA: "Sending undone" landed
    /// between its Send and Pop Out). A toast that would cover it goes to the room left of it
    /// when it fits there with its sides (the reader's margin at 1440 px, where Superhuman's
    /// toasts sit), else just above it. [`Dock::place_toasts_again`] when it changes.
    pub fn keep_toasts_off(
        &self,
        area: impl Fn(&gtk::Widget) -> Option<gtk::graphene::Rect> + 'static,
    ) {
        self.inner.toasts_avoid.replace(Some(Box::new(area)));
    }

    /// Places the toasts again, before the next frame: what they keep off moved or changed.
    pub fn place_toasts_again(&self) {
        let inner = &self.inner;
        if inner.toasts_due.replace(true) {
            return;
        }
        let weak = Rc::downgrade(inner);
        glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
            if let Some(inner) = weak.upgrade() {
                inner.place_toasts();
            }
            glib::ControlFlow::Break
        });
    }
}

/// Where a toast `own` px wide and `tall` px high goes, as the margins at its right and its
/// bottom, in an overlay `width` × `height` that centres it at the bottom with [`TOAST_SIDE`]
/// around it, so it keeps off `area` (x, y, width, height): unmoved when it doesn't cover it, in
/// the room left of it when it fits there with its sides, else just above it.
pub fn toast_room(
    own: i32,
    tall: i32,
    width: i32,
    height: i32,
    area: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (x, y, area_width, area_height) = area;
    let (left, top) = ((width - own) / 2, height - TOAST_SIDE - tall);
    let covers =
        x < left + own && x + area_width > left && y < height - TOAST_SIDE && y + area_height > top;
    if !covers {
        (0, 0)
    } else if x >= own + 2 * TOAST_SIDE {
        (width - x, 0)
    } else {
        (0, (height - y).max(0))
    }
}

impl Inner {
    fn card(&self, widget: &gtk::Widget) -> Option<Rc<Card>> {
        self.cards
            .borrow()
            .iter()
            .find(|card| &card.widget == widget)
            .cloned()
    }

    fn expanded(&self) -> Option<Rc<Card>> {
        self.cards
            .borrow()
            .iter()
            .find(|card| card.state.get() == CardState::Expanded)
            .cloned()
    }

    fn set_state(&self, widget: &gtk::Widget, state: CardState) {
        let Some(card) = self.card(widget) else {
            return;
        };
        if state == CardState::Expanded
            && let Some(other) = self.expanded().filter(|other| &other.widget != widget)
        {
            self.set_state(&other.widget, CardState::Open);
        }
        if card.state.replace(state) == state {
            return;
        }
        (card.on_state)(state);
        if state != CardState::Minimized {
            self.make_room(widget);
        }
        self.relayout();
    }

    /// Minimizes the oldest open cards until `keep` fits beside the rest; while one pane fills
    /// the window, every open card but `keep`.
    fn make_room(&self, keep: &gtk::Widget) {
        let width = self.overlay.width();
        if width <= 0 {
            return;
        }
        loop {
            let cards = self.cards.borrow().clone();
            if !crowded(&cards, width) {
                return;
            }
            let Some(oldest) = cards
                .iter()
                .find(|card| card.state.get() != CardState::Minimized && &card.widget != keep)
            else {
                return;
            };
            oldest.state.set(CardState::Minimized);
            (oldest.on_state)(CardState::Minimized);
        }
    }

    /// While a card is expanded the backdrop shows and the other cards hide under it.
    fn relayout(&self) {
        let expanded = self.expanded().map(|card| card.widget.clone());
        self.backdrop.set_visible(expanded.is_some());
        for card in self.cards.borrow().iter() {
            card.widget
                .set_child_visible(expanded.as_ref().is_none_or(|shown| shown == &card.widget));
        }
        self.restyle();
        self.overlay.queue_allocate();
        self.place_toasts();
        let open = self
            .cards
            .borrow()
            .iter()
            .any(|card| card.state.get() != CardState::Minimized);
        if self.told_open.replace(Some(open)) != Some(open) {
            let handlers = self.open_handlers.borrow().clone();
            for handler in handlers {
                handler(open);
            }
        }
    }

    /// The width the cards take at the bottom right, from the window's right edge: none while
    /// one pane fills the window (they fill it) or a card is expanded (over a backdrop).
    fn footprint(&self, width: i32) -> i32 {
        let cards = self.cards.borrow();
        if cards.is_empty() || width <= 0 || one_pane(width) || self.expanded().is_some() {
            return 0;
        }
        EDGE + cards
            .iter()
            .map(|card| slot_width(card.state.get()))
            .sum::<i32>()
            + GAP * (cards.len() as i32 - 1)
    }

    /// Centres each toast in the room left of the cards when it fits there with its sides,
    /// else on the window ([`Dock::keep_toasts_clear`]).
    fn place_toasts(&self) {
        self.toasts_due.set(false);
        let Some(toasts) = self.toasts.upgrade() else {
            return;
        };
        let width = self.overlay.width();
        let footprint = self.footprint(width);
        self.toasts_placed.set(Some(width));
        let mut child = toasts.first_child();
        while let Some(toast) = child {
            child = toast.next_sibling();
            if toast.css_name() != "toast" {
                continue;
            }
            // The cards sit at the right in either direction (`place`): the margin goes on the
            // right, which is the end only left to right.
            let rtl = toast.direction() == gtk::TextDirection::Rtl;
            let right = if rtl {
                toast.margin_start()
            } else {
                toast.margin_end()
            };
            let (_, natural) = measure(&toast, gtk::Orientation::Horizontal, -1);
            let own = natural - right;
            let mut margin = if footprint > 0 && width - footprint >= own + 2 * TOAST_SIDE {
                footprint
            } else {
                0
            };
            let mut lift = 0;
            if margin == 0
                && let Some(area) = self
                    .toasts_avoid
                    .borrow()
                    .as_ref()
                    .and_then(|area| area(toasts.upcast_ref()))
            {
                // Its own height: what it measures holds the lift it has.
                let (_, tall) = measure(&toast, gtk::Orientation::Vertical, -1);
                let tall = tall - toast.margin_bottom();
                (margin, lift) = toast_room(
                    own,
                    tall,
                    toasts.width(),
                    toasts.height(),
                    (
                        area.x() as i32,
                        area.y() as i32,
                        area.width() as i32,
                        area.height() as i32,
                    ),
                );
            }
            if right != margin {
                if rtl {
                    toast.set_margin_start(margin);
                } else {
                    toast.set_margin_end(margin);
                }
            }
            if toast.margin_bottom() != lift {
                toast.set_margin_bottom(lift);
            }
        }
    }

    /// The first card (unless expanded) is flush with the window's right edge; while one pane
    /// fills the window every card fills the panes' width.
    fn restyle(&self) {
        self.classes_due.set(false);
        let full = self.width.get() > 0 && one_pane(self.width.get());
        for (index, card) in self.cards.borrow().iter().enumerate() {
            let edge = !full && index == 0 && card.state.get() != CardState::Expanded;
            for (class, on) in [(EDGE_CLASS, edge), (FULL_CLASS, full)] {
                if on {
                    card.widget.add_css_class(class);
                } else {
                    card.widget.remove_css_class(class);
                }
            }
        }
    }

    /// Where `widget` goes in an overlay `overlay`'s size.
    fn place(
        self: &Rc<Self>,
        overlay: &gtk::Overlay,
        widget: &gtk::Widget,
    ) -> Option<gdk::Rectangle> {
        let (width, height) = (overlay.width(), overlay.height());
        if widget == self.backdrop.upcast_ref::<gtk::Widget>() {
            return Some(gdk::Rectangle::new(0, 0, width, height));
        }
        // A window of another width may give the toasts room beside the cards, or take it.
        if self
            .toasts_placed
            .get()
            .is_some_and(|placed| placed != width)
            && !self.toasts_due.replace(true)
        {
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.place_toasts();
                }
            });
        }
        let cards = self.cards.borrow();
        let index = cards.iter().position(|card| &card.widget == widget)?;
        if one_pane(self.width.replace(width)) != one_pane(width) && !self.classes_due.replace(true)
        {
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.restyle();
                }
            });
        }
        if crowded(&cards, width) && !self.tidy_due.replace(true) {
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.tidy();
                }
            });
        }
        if one_pane(width) {
            return Some(stacked(&cards, index, width, height));
        }
        let state = cards[index].state.get();
        let (min_width, _) = measure(widget, gtk::Orientation::Horizontal, -1);
        if state == CardState::Expanded {
            let w = EXPANDED_SIZE
                .0
                .min(width - 2 * EXPANDED_MARGIN)
                .max(min_width);
            let (min_height, _) = measure(widget, gtk::Orientation::Vertical, w);
            let h = EXPANDED_SIZE
                .1
                .min(height - 2 * EXPANDED_MARGIN)
                .max(min_height);
            return Some(gdk::Rectangle::new(
                ((width - w) / 2).max(0),
                ((height - h) / 2).max(0),
                w,
                h,
            ));
        }
        let mut right = width - EDGE;
        for card in &cards[..index] {
            right -= slot_width(card.state.get()) + GAP;
        }
        let w = slot_width(state).max(min_width);
        let (min_height, natural_height) = measure(widget, gtk::Orientation::Vertical, w);
        let h = match state {
            CardState::Minimized => natural_height.max(min_height),
            _ => CARD_SIZE.1.min(height - TOP_ROOM).max(min_height),
        };
        Some(gdk::Rectangle::new((right - w).max(0), height - h, w, h))
    }

    /// A smaller window crowds the cards: the newest one shown stays, the oldest others
    /// minimize.
    fn tidy(&self) {
        self.tidy_due.set(false);
        let newest = self
            .cards
            .borrow()
            .iter()
            .rev()
            .find(|card| card.state.get() != CardState::Minimized)
            .map(|card| card.widget.clone());
        if let Some(newest) = newest {
            self.make_room(&newest);
            self.relayout();
        }
    }
}

/// While one pane fills the window, cards take the panes' whole width: minimized ones stacked
/// at the bottom, the oldest lowest, and an open one filling the room above them.
fn stacked(cards: &[Rc<Card>], index: usize, width: i32, height: i32) -> gdk::Rectangle {
    let header = |card: &Card| {
        let (min, natural) = measure(&card.widget, gtk::Orientation::Vertical, width);
        natural.max(min)
    };
    let minimized = |cards: &[Rc<Card>]| -> i32 {
        cards
            .iter()
            .filter(|card| card.state.get() == CardState::Minimized)
            .map(|card| header(card))
            .sum()
    };
    let card = &cards[index];
    if card.state.get() == CardState::Minimized {
        let below = minimized(&cards[..index]);
        let h = header(card);
        return gdk::Rectangle::new(0, height - below - h, width, h);
    }
    let below = minimized(cards);
    let (min_height, _) = measure(&card.widget, gtk::Orientation::Vertical, width);
    let h = (height - below).max(min_height);
    gdk::Rectangle::new(0, height - below - h, width, h)
}

/// One pane fills the window (`NARROW_WIDTH`): an open card fills the panes.
fn one_pane(width: i32) -> bool {
    f64::from(width) < NARROW_WIDTH
}

/// Whether the cards that aren't minimized don't fit side by side in a dock `width` wide; while
/// one pane fills the window, whether more than one is.
fn crowded(cards: &[Rc<Card>], width: i32) -> bool {
    let shown = cards
        .iter()
        .filter(|card| card.state.get() != CardState::Minimized)
        .count();
    if shown < 2 {
        return false;
    }
    if one_pane(width) {
        return true;
    }
    let needed: i32 = EDGE
        + cards
            .iter()
            .map(|card| slot_width(card.state.get()) + GAP)
            .sum::<i32>();
    needed > width
}

/// The width a card in `state` takes beside the others.
fn slot_width(state: CardState) -> i32 {
    match state {
        CardState::Minimized => MINIMIZED_WIDTH,
        CardState::Open | CardState::Expanded => CARD_SIZE.0,
    }
}

fn measure(widget: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> (i32, i32) {
    let (min, natural, _, _) = widget.measure(orientation, for_size);
    (min, natural)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toast_keeps_off_the_open_reply() {
        // The window's toast at 1440 × 900: 300 × 46 px, centred at the bottom.
        let (own, tall, width, height) = (300, 46, 1440, 900);
        // The reply mid-column, above the toast: it stays.
        assert_eq!(
            toast_room(own, tall, width, height, (360, 300, 720, 190)),
            (0, 0)
        );
        // The reply at the column's foot: the toast goes to the reader's left margin.
        assert_eq!(
            toast_room(own, tall, width, height, (360, 700, 720, 188)),
            (1080, 0)
        );
        // A 710 px window has no margin: just above the reply.
        assert_eq!(
            toast_room(own, tall, 710, height, (24, 700, 662, 188)),
            (0, 200)
        );
        // A reply beside the toast's place doesn't move it.
        assert_eq!(
            toast_room(own, tall, width, height, (1100, 700, 300, 188)),
            (0, 0)
        );
    }

    #[test]
    fn an_open_card_fits_beside_the_list() {
        assert!(one_pane(NARROW_WIDTH as i32 - 1));
        assert!(!one_pane(NARROW_WIDTH as i32));
        assert!(CARD_SIZE.0 + 2 * EDGE <= NARROW_WIDTH as i32);
        // Flush with the window's edge: nothing of the list shows beside it.
        assert_eq!(EDGE, 0);
        assert!(MINIMIZED_WIDTH < CARD_SIZE.0);
        assert!(EXPANDED_SIZE.0 > CARD_SIZE.0 && EXPANDED_SIZE.1 > CARD_SIZE.1);
    }
}
