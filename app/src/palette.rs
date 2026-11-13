//! The command palette (Ctrl+K, F1, Ctrl+Shift+P), Superhuman's hub: an overlay over the whole
//! window, anchored a fifth of the way down, listing the commands the window can run now and,
//! once something is typed, its folders ("Go to Projects"). Each row shows the command and the
//! keycaps of its first key; a command found by another of its words shows it ("Archive
//! (done)"). The empty query suggests what suits where it opened (Archive, Reply, Star… on a
//! selection), then the commands run last (`palette.toml`), then the rest by category. Typing
//! filters, Up and Down move, Enter runs the selected row and Escape, a click beside it or its
//! key again closes it; the keyboard focus goes back where it was. Opened from a composer card,
//! it lists and runs that card's commands as well.
//!
//! It is built once per window ([`install`]) and only hidden between uses, so opening is
//! instant: the rows are a pool whose labels change in place as the query does, never rebuilt
//! per key. It is not an `AdwDialog`: libadwaita centres those and can't anchor one to the top,
//! and the palette must draw over everything as Superhuman's does. So it does what a dialog
//! does itself: the dialog role, the focus kept in its field (Tab stays there), the focus given
//! back on close, the window below out of reach of the pointer while it shows.

use crate::accessibility;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::palette::{self, Entry, Recent, Section};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The palette's width while the window has room for it, in px (Superhuman's is about half
/// the window; 640–720 px reads well at 1440 px).
pub const WIDTH: i32 = 680;
/// Where its top edge sits: this share of the window's height from the top.
pub const TOP: f64 = 0.2;
/// The room it keeps from the window's edges.
pub const EDGE: i32 = 16;
/// The list's height at most, in px: about nine rows, the last one fading out.
pub const LIST_HEIGHT: i32 = 396;
/// How long it fades in and out (Superhuman: overlays fade, in 150 ms or less).
pub const FADE_IN: Duration = Duration::from_millis(100);
pub const FADE_OUT: Duration = Duration::from_millis(80);
/// The keycaps a row shows at most: a sequence's two keys, or three modifiers and a key.
const CAPS: usize = 4;
/// The CSS class of the palette's layer over the window (the backdrop and the card).
const LAYER: &str = "sw-palette-layer";

/// `widget` is in a palette over its window: the focus there is the palette's for a moment, and
/// it goes back where it was when the palette closes.
pub fn holds(widget: &gtk::Widget) -> bool {
    std::iter::successors(Some(widget.clone()), gtk::Widget::parent)
        .any(|part| part.has_css_class(LAYER))
}

thread_local! {
    /// Each window's palette, for [`close_in`].
    static PALETTES: RefCell<Vec<(glib::WeakRef<gtk::Window>, WeakPalette)>> =
        const { RefCell::new(Vec::new()) };
}

/// Closes `window`'s palette when it is open, the focus going back where it was before it: a
/// picker (`crate::picker`) opening over it takes its place (the fork's phase B).
pub fn close_in(window: &impl IsA<gtk::Window>) {
    let window = window.as_ref();
    let palette = PALETTES.with(|palettes| {
        let mut palettes = palettes.borrow_mut();
        palettes.retain(|(held, palette)| held.upgrade().is_some() && palette.upgrade().is_some());
        palettes
            .iter()
            .find(|(held, _)| held.upgrade().as_ref() == Some(window))
            .and_then(|(_, palette)| palette.upgrade())
    });
    if let Some(palette) = palette {
        palette.close();
    }
}

/// A place the palette can go to: a folder of the sidebar.
#[derive(Clone)]
pub struct Place {
    /// What `palette.toml` remembers it by: the sidebar's key (`folder-12`).
    pub key: String,
    /// "Go to Projects", "Go to Inbox — Work".
    pub title: String,
    /// The go-to command that leads to this very folder (Go to Inbox, with one account): the
    /// palette lists that one instead, with its keys.
    pub command: Option<ActionId>,
    pub run: Rc<dyn Fn()>,
}

/// What the palette offers this time it opens; [`Palette::open`] takes it.
#[derive(Default)]
pub struct Context {
    pub keymap: Keymap,
    /// The composer card it opened from: its commands are listed and run there.
    pub composer: Option<gio::ActionGroup>,
    /// Commands' titles where they differ from the registry's: Not Junk in a Junk folder,
    /// Unstar, Mark as Unread, Go to Work.
    pub titles: Vec<(ActionId, String)>,
    /// Whether messages are selected, for the suggestions.
    pub selection: bool,
    /// The folders, found once something is typed.
    pub places: Vec<Place>,
    /// When its command ran, for how long the palette took to show (now when not given).
    pub requested: Option<Instant>,
}

/// What a row runs.
#[derive(Clone)]
enum Run {
    Command(ActionId),
    Place(Rc<dyn Fn()>),
}

/// One row the palette can show.
#[derive(Clone)]
struct Item {
    /// The command's registry name, or the place's key: what `palette.toml` remembers.
    key: String,
    title: String,
    aliases: &'static [&'static str],
    /// The heading it goes under on the empty query: its category's, or Folders.
    group: &'static str,
    /// The keycaps of its first key, a list per key of a sequence.
    caps: Vec<Vec<String>>,
    /// Every key, as a screen reader says it: `e, y`.
    keys: String,
    typed_only: bool,
    run: Run,
}

impl Item {
    fn command(&self) -> Option<ActionId> {
        match self.run {
            Run::Command(id) => Some(id),
            Run::Place(_) => None,
        }
    }

    /// The title with the word it was found by: `Archive (done)`.
    fn shown_title(&self, alias: Option<usize>) -> String {
        match alias.and_then(|index| self.aliases.get(index)) {
            Some(alias) => format!("{} ({alias})", self.title),
            None => self.title.clone(),
        }
    }

    /// What a screen reader says for the row: the command and its keys.
    fn spoken(&self, alias: Option<usize>) -> String {
        let title = self.shown_title(alias);
        if self.keys.is_empty() {
            title
        } else {
            format!("{title}, {}", self.keys)
        }
    }
}

/// A row on screen: what it shows now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shown {
    item: usize,
    alias: Option<usize>,
    section: Section,
}

/// One row of the pool, and its parts.
struct RowParts {
    row: gtk::ListBoxRow,
    title: gtk::Label,
    alias: gtk::Label,
    /// One box of keycaps per key of a sequence.
    keys: Vec<(gtk::Box, Vec<gtk::Label>)>,
}

type RecentHandler = Rc<dyn Fn(&Recent)>;

struct Inner {
    window: glib::WeakRef<gtk::Window>,
    /// The whole window's layer: the backdrop and the card over it.
    layer: gtk::Overlay,
    card: gtk::Box,
    entry: gtk::SearchEntry,
    list: gtk::ListBox,
    scrolled: gtk::ScrolledWindow,
    rows: RefCell<Vec<RowParts>>,
    items: RefCell<Vec<Item>>,
    shown: RefCell<Vec<Shown>>,
    /// The commands suggested this time, by their places in `items`.
    suggested: RefCell<Vec<usize>>,
    composer: RefCell<Option<gio::ActionGroup>>,
    recent: RefCell<Recent>,
    recent_changed: RefCell<Option<RecentHandler>>,
    open: Cell<bool>,
    /// Where the focus was when it opened, and where it goes when that is gone.
    return_focus: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    fallback_focus: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    /// A query set by the palette itself, which doesn't refresh twice.
    quiet: Cell<bool>,
    fade: RefCell<Option<adw::TimedAnimation>>,
    /// When it last opened, how long until its first frame was painted, how much of that
    /// was its own work (finding the commands, filling the rows), and how long the last query
    /// took to show its rows.
    opened_at: Cell<Option<Instant>>,
    shown_in: Cell<Option<Duration>>,
    worked: Cell<Option<Duration>>,
    queried: Cell<Option<Duration>>,
}

/// The palette of one window; [`install`] makes it.
#[derive(Clone)]
pub struct Palette {
    inner: Rc<Inner>,
}

/// A palette that doesn't keep its window's widgets alive.
#[derive(Clone, Default)]
pub struct WeakPalette {
    inner: std::rc::Weak<Inner>,
}

impl WeakPalette {
    pub fn upgrade(&self) -> Option<Palette> {
        self.inner.upgrade().map(|inner| Palette { inner })
    }
}

/// Builds the palette over `window`'s content, hidden: the content goes into an overlay whose
/// top layer the palette is, so it draws over the panes, the composer's cards and the banner.
pub fn install(window: &impl IsA<gtk::Window>) -> Palette {
    let window = window.as_ref();
    let host = gtk::Overlay::new();
    // libadwaita's windows keep their child for themselves: theirs is `content`.
    if let Some(adw_window) = window.downcast_ref::<adw::ApplicationWindow>() {
        let content = adw_window.content();
        adw_window.set_content(Some(&host));
        host.set_child(content.as_ref());
    } else if let Some(adw_window) = window.downcast_ref::<adw::Window>() {
        let content = adw_window.content();
        adw_window.set_content(Some(&host));
        host.set_child(content.as_ref());
    } else {
        let content = window.child();
        window.set_child(Some(&host));
        host.set_child(content.as_ref());
    }

    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Type a command")
        .search_delay(0)
        .hexpand(true)
        .css_classes(["sw-palette-search"])
        .build();
    entry.set_widget_name("palette-search");
    entry.update_property(&[gtk::accessible::Property::Label("Command palette")]);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Browse)
        .css_classes(["sw-palette-list"])
        .build();
    list.set_widget_name("palette-list");
    list.update_property(&[gtk::accessible::Property::Label("Commands")]);
    let empty = gtk::Label::builder()
        .label("No matching command")
        .css_classes(["sw-palette-empty", "sw-secondary"])
        .build();
    list.set_placeholder(Some(&empty));
    // The focus stays in the entry; the list is no Tab stop of its own.
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .focusable(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(LIST_HEIGHT)
        .css_classes(["sw-palette-scroll"])
        .build();
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .accessible_role(gtk::AccessibleRole::Dialog)
        .css_classes(["sw-palette-card"])
        .build();
    card.set_widget_name("palette");
    card.update_property(&[
        gtk::accessible::Property::Label("Command Palette"),
        gtk::accessible::Property::Modal(true),
    ]);
    card.append(&entry);
    card.append(
        &gtk::Separator::builder()
            .css_classes(["sw-palette-separator"])
            .build(),
    );
    card.append(&scrolled);
    let backdrop = gtk::Box::builder()
        .hexpand(true)
        .vexpand(true)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .css_classes(["sw-palette-backdrop"])
        .build();
    let layer = gtk::Overlay::builder()
        .child(&backdrop)
        .visible(false)
        .css_classes([LAYER])
        .build();
    layer.set_widget_name("palette-layer");
    layer.add_overlay(&card);
    host.add_overlay(&layer);

    let inner = Rc::new(Inner {
        window: window.downgrade(),
        layer,
        card,
        entry,
        list,
        scrolled,
        rows: RefCell::new(Vec::new()),
        items: RefCell::new(Vec::new()),
        shown: RefCell::new(Vec::new()),
        suggested: RefCell::new(Vec::new()),
        composer: RefCell::new(None),
        recent: RefCell::new(Recent::default()),
        recent_changed: RefCell::new(None),
        open: Cell::new(false),
        return_focus: RefCell::new(None),
        fallback_focus: RefCell::new(None),
        quiet: Cell::new(false),
        fade: RefCell::new(None),
        opened_at: Cell::new(None),
        shown_in: Cell::new(None),
        worked: Cell::new(None),
        queried: Cell::new(None),
    });
    connect(&inner, &backdrop);
    let palette = Palette { inner };
    PALETTES.with(|palettes| {
        palettes
            .borrow_mut()
            .push((window.downgrade(), palette.downgrade()))
    });
    palette
}

impl Palette {
    pub fn downgrade(&self) -> WeakPalette {
        WeakPalette {
            inner: Rc::downgrade(&self.inner),
        }
    }

    /// Opens over the window with the cursor in the field and `context`'s commands; open
    /// already, it stays as it is.
    pub fn open(&self, context: Context) {
        let inner = &self.inner;
        if inner.open.get() {
            return;
        }
        let started = Instant::now();
        inner
            .opened_at
            .set(Some(context.requested.unwrap_or(started)));
        inner.shown_in.set(None);
        inner.fill(context);
        inner.quiet.set(true);
        inner.entry.set_text("");
        inner.quiet.set(false);
        inner.refresh();
        let window = inner.window.upgrade();
        // A picker over the window gives way to it (the fork's phase B): it closes first and
        // gives the focus back where it was, the draft it was for, which is where the palette
        // gives it back. Nothing in a layer over the window is where the focus goes back: that
        // layer is closing.
        if let Some(window) = window.as_ref() {
            crate::picker::close_in(window);
        }
        let focus = window
            .as_ref()
            .and_then(gtk::prelude::GtkWindowExt::focus)
            .filter(|focus| !holds(focus));
        inner
            .return_focus
            .replace(focus.map(|focus| focus.downgrade()));
        inner.open.set(true);
        inner.layer.set_can_target(true);
        inner.layer.set_visible(true);
        inner.fade(1.0, FADE_IN);
        inner.entry.grab_focus();
        inner.worked.set(Some(started.elapsed()));
        if let Some(window) = window {
            inner.time_first_frame(&window);
        }
    }

    /// Closes it, and the focus goes back where it was.
    pub fn close(&self) {
        self.inner.close();
    }

    pub fn is_open(&self) -> bool {
        self.inner.open.get()
    }

    /// The widget a screen reader knows as the palette's dialog.
    pub fn card(&self) -> gtk::Widget {
        self.inner.card.clone().upcast()
    }

    /// Where the focus goes on close when the widget that had it is gone: the message list.
    pub fn set_fallback_focus(&self, widget: &impl IsA<gtk::Widget>) {
        self.inner
            .fallback_focus
            .replace(Some(widget.upcast_ref::<gtk::Widget>().downgrade()));
    }

    /// The commands run last, as `palette.toml` holds them.
    pub fn set_recent(&self, recent: Recent) {
        self.inner.recent.replace(recent);
    }

    pub fn recent(&self) -> Recent {
        self.inner.recent.borrow().clone()
    }

    /// Runs `handler` with the recent commands after the palette ran one (to save them).
    pub fn connect_recent_changed(&self, handler: impl Fn(&Recent) + 'static) {
        self.inner.recent_changed.replace(Some(Rc::new(handler)));
    }

    /// Types `query` into the field, as a user would.
    pub fn set_query(&self, query: &str) {
        self.inner.entry.set_text(query);
        self.inner.refresh();
    }

    /// The commands shown, best match first (places left out).
    pub fn shown(&self) -> Vec<ActionId> {
        let items = self.inner.items.borrow();
        self.inner
            .shown
            .borrow()
            .iter()
            .filter_map(|shown| items.get(shown.item)?.command())
            .collect()
    }

    /// The rows' titles as shown, best match first: `Archive (done)`, `Go to Projects`.
    pub fn shown_titles(&self) -> Vec<String> {
        let items = self.inner.items.borrow();
        self.inner
            .shown
            .borrow()
            .iter()
            .filter_map(|shown| Some(items.get(shown.item)?.shown_title(shown.alias)))
            .collect()
    }

    /// The headings over the rows of the empty query, in order.
    pub fn headings(&self) -> Vec<String> {
        let shown = self.inner.shown.borrow();
        (0..shown.len())
            .filter_map(|index| self.inner.heading(index))
            .collect()
    }

    /// The field: typing, Enter (`activate`) and Escape (`stop-search`) arrive there.
    pub fn search_entry(&self) -> Option<gtk::SearchEntry> {
        Some(self.inner.entry.clone())
    }

    /// The command Enter would run.
    pub fn selected(&self) -> Option<ActionId> {
        let index = self.inner.selected_index()?;
        self.inner.items.borrow().get(index)?.command()
    }

    /// The title of the row Enter would run, as shown.
    pub fn selected_title(&self) -> Option<String> {
        let row = self.inner.list.selected_row()?;
        let position = usize::try_from(row.index()).ok()?;
        let shown = *self.inner.shown.borrow().get(position)?;
        Some(
            self.inner
                .items
                .borrow()
                .get(shown.item)?
                .shown_title(shown.alias),
        )
    }

    /// Where the selected row ends in what the list shows (px from its top), how tall that is,
    /// and whether rows lie below it, for checks that the row is clear of the bottom fade.
    pub fn selected_extent(&self) -> Option<(f64, f64, bool)> {
        let row = self.inner.list.selected_row()?;
        let bounds = row.compute_bounds(&self.inner.list)?;
        let adjustment = self.inner.scrolled.vadjustment();
        let bottom = f64::from(bounds.y() + bounds.height()) - adjustment.value();
        let below = adjustment.value() + adjustment.page_size() < adjustment.upper() - 0.5;
        Some((bottom, adjustment.page_size(), below))
    }

    /// Runs the selected row, as Enter does, and returns its command.
    pub fn activate_selected(&self) -> Option<ActionId> {
        let index = self.inner.selected_index()?;
        let id = self.inner.items.borrow().get(index)?.command();
        self.inner.run(index);
        id
    }

    /// Moves the selection as Down (positive) or Up (negative) does.
    pub fn move_selection(&self, step: i32) {
        self.inner.move_selection(step);
    }

    /// How long its last opening took, from the command to the first painted frame.
    pub fn shown_in(&self) -> Option<Duration> {
        self.inner.shown_in.get()
    }

    /// How much of the last opening was the palette's own work, before GTK drew it.
    pub fn worked(&self) -> Option<Duration> {
        self.inner.worked.get()
    }

    /// How long the last query took to rank the rows and fill them.
    pub fn queried(&self) -> Option<Duration> {
        self.inner.queried.get()
    }
}

fn connect(inner: &Rc<Inner>, backdrop: &gtk::Box) {
    let entry = &inner.entry;
    let weak = Rc::downgrade(inner);
    entry.connect_search_changed(move |_| {
        if let Some(inner) = weak.upgrade()
            && !inner.quiet.get()
        {
            inner.refresh();
        }
    });
    let weak = Rc::downgrade(inner);
    entry.connect_activate(move |_| {
        if let Some(inner) = weak.upgrade()
            && let Some(index) = inner.selected_index()
        {
            inner.run(index);
        }
    });
    let weak = Rc::downgrade(inner);
    entry.connect_stop_search(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.close();
        }
    });
    let weak = Rc::downgrade(inner);
    inner.list.connect_row_activated(move |_, row| {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let index = usize::try_from(row.index())
            .ok()
            .and_then(|position| inner.shown.borrow().get(position).map(|shown| shown.item));
        if let Some(index) = index {
            inner.run(index);
        }
    });
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(inner);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(inner) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let step = match key {
            gdk::Key::Down => 1,
            gdk::Key::Up => -1,
            gdk::Key::Page_Down => 8,
            gdk::Key::Page_Up => -8,
            _ => return glib::Propagation::Proceed,
        };
        inner.move_selection(step);
        glib::Propagation::Stop
    });
    entry.add_controller(keys);
    // The focus stays in the field, as a dialog keeps it: Tab and Shift+Tab go nowhere.
    let trap = gtk::EventControllerKey::new();
    trap.set_propagation_phase(gtk::PropagationPhase::Capture);
    trap.connect_key_pressed(|_, key, _, _| {
        if matches!(
            key,
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab | gdk::Key::KP_Tab
        ) {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    inner.card.add_controller(trap);
    // The focus went elsewhere in the window (a click on the banner, a command's key that
    // opened something): the palette closes, as it would on Escape.
    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(inner);
    focus.connect_leave(move |_| {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(&inner);
        glib::idle_add_local_once(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let window = inner.window.upgrade();
            let elsewhere = window.as_ref().is_some_and(|window| {
                window.is_active()
                    && gtk::prelude::GtkWindowExt::focus(window)
                        .is_some_and(|focus| !focus.is_ancestor(&inner.layer))
            });
            if inner.open.get() && elsewhere {
                inner.close();
            }
        });
    });
    inner.card.add_controller(focus);
    let click = gtk::GestureClick::new();
    let weak = Rc::downgrade(inner);
    click.connect_released(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(inner) = weak.upgrade() {
            inner.close();
        }
    });
    backdrop.add_controller(click);
    let weak = Rc::downgrade(inner);
    inner
        .layer
        .connect_get_child_position(move |layer, widget| {
            let inner = weak.upgrade()?;
            (widget == inner.card.upcast_ref::<gtk::Widget>()).then(|| place(layer, &inner.card))
        });
    let weak = Rc::downgrade(inner);
    inner.list.set_header_func(move |row, _| {
        let heading = weak.upgrade().and_then(|inner| {
            usize::try_from(row.index())
                .ok()
                .and_then(|index| inner.heading(index))
        });
        match heading {
            Some(text) => {
                let label = gtk::Label::builder()
                    .label(text.as_str())
                    .xalign(0.0)
                    .css_classes(["sw-palette-heading", "sw-secondary"])
                    .build();
                label.update_property(&[gtk::accessible::Property::Label(&text)]);
                row.set_header(Some(&label));
            }
            None => row.set_header(None::<&gtk::Widget>),
        }
    });
}

/// Where the card goes in the window: [`WIDTH`] wide (less in a narrow window), its top a
/// fifth of the way down, held inside the window's edges.
fn place(layer: &gtk::Overlay, card: &gtk::Box) -> gdk::Rectangle {
    let (width, height) = (layer.width(), layer.height());
    let w = WIDTH.min(width - 2 * EDGE).max(1);
    let (_, natural, _, _) = card.measure(gtk::Orientation::Vertical, w);
    let h = natural.min(height - 2 * EDGE).max(1);
    let top = (f64::from(height) * TOP).round() as i32;
    let y = top.min(height - h - EDGE).max(EDGE.min(height - h).max(0));
    gdk::Rectangle::new(((width - w) / 2).max(0), y, w, h)
}

impl Inner {
    /// The rows this opening offers: the window's commands that are enabled now (and the
    /// composer's), then the places.
    fn fill(&self, context: Context) {
        let Context {
            keymap,
            composer,
            titles,
            selection,
            places,
            requested: _,
        } = context;
        let window = self.window.upgrade();
        let enabled = |id: ActionId| match id.scope() {
            Scope::App => window
                .as_ref()
                .and_then(|window| window.application())
                .is_some_and(|app| app.is_action_enabled(id.name())),
            Scope::Composer => composer
                .as_ref()
                .is_some_and(|group| group.is_action_enabled(id.name())),
            _ => window
                .as_ref()
                .and_then(|window| window.downcast_ref::<gtk::ApplicationWindow>())
                .is_some_and(|window| window.is_action_enabled(id.name())),
        };
        let mut items: Vec<Item> = Vec::with_capacity(ActionId::ALL.len() + places.len());
        for id in ActionId::ALL
            .into_iter()
            .filter(|id| id.spec().palette && enabled(*id))
        {
            let title = titles
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .map_or_else(|| id.title().to_owned(), |(_, title)| title.clone());
            // The toggle says what it would do, Mark as Read or Mark as Unread: the command of
            // that name would do the same, so the two are one row, the toggle's, with `u`.
            if matches!(id, ActionId::MarkRead | ActionId::MarkUnread)
                && items
                    .iter()
                    .any(|item| item.command() == Some(ActionId::ToggleRead) && item.title == title)
            {
                continue;
            }
            let shown = keymap.shown(id);
            items.push(Item {
                key: id.name().to_owned(),
                title,
                aliases: id.spec().aliases,
                group: id.category().label(),
                caps: shown.first().map(|first| first.caps()).unwrap_or_default(),
                keys: keymap.display(id),
                typed_only: false,
                run: Run::Command(id),
            });
        }
        // The fork's later additions sit at the registry's end whatever their category: the
        // palette lists the commands by category all the same (a stable sort keeps the
        // registry's order within one).
        items.sort_by_key(|item| item.command().map(ActionId::category));
        for place in places {
            if place
                .command
                .is_some_and(|id| items.iter().any(|item| item.command() == Some(id)))
            {
                continue;
            }
            items.push(Item {
                key: place.key,
                title: place.title,
                aliases: &[],
                group: "Folders",
                caps: Vec::new(),
                keys: String::new(),
                typed_only: true,
                run: Run::Place(place.run),
            });
        }
        let suggested: Vec<usize> = palette::suggestions(composer.is_some(), selection)
            .iter()
            .filter_map(|id| items.iter().position(|item| item.command() == Some(*id)))
            .collect();
        // The rows on screen point into the items: none until the next refresh.
        self.shown.borrow_mut().clear();
        self.suggested.replace(suggested);
        self.items.replace(items);
        self.composer.replace(composer);
    }

    fn refresh(&self) {
        let started = Instant::now();
        let query = self.entry.text();
        let items = self.items.borrow();
        let recent = self.recent.borrow();
        let suggested = self.suggested.borrow();
        let entries: Vec<Entry> = items
            .iter()
            .enumerate()
            .map(|(index, item)| Entry {
                title: &item.title,
                aliases: item.aliases,
                boost: recent.boost(&item.key)
                    + if suggested.contains(&index) {
                        palette::SUGGESTED_BOOST
                    } else {
                        0
                    },
                typed_only: item.typed_only,
            })
            .collect();
        let shown: Vec<Shown> = if query.trim().is_empty() {
            let recent: Vec<usize> = recent
                .keys()
                .iter()
                .filter_map(|key| items.iter().position(|item| item.key == *key))
                .collect();
            palette::browse(&entries, &suggested, &recent)
                .into_iter()
                .map(|(item, section)| Shown {
                    item,
                    alias: None,
                    section,
                })
                .collect()
        } else {
            palette::find(&query, &entries)
                .into_iter()
                .map(|found| Shown {
                    item: found.index,
                    alias: found.alias,
                    section: Section::Other,
                })
                .collect()
        };
        drop((entries, recent, suggested));
        // Known before the rows change: the list asks for their headings as they do.
        self.shown.replace(shown.clone());
        self.show_rows(&items, &shown);
        drop(items);
        self.list.invalidate_headers();
        // Its own visibility: the palette itself is still hidden while it opens.
        if let Some(first) = self.list.row_at_index(0)
            && first.get_visible()
        {
            self.list.select_row(Some(&first));
        } else {
            self.list.unselect_all();
        }
        self.scrolled.vadjustment().set_value(0.0);
        self.queried.set(Some(started.elapsed()));
    }

    /// The pool's rows show `shown` in place; the rest hide. New rows are made only when more
    /// are shown than ever before.
    fn show_rows(&self, items: &[Item], shown: &[Shown]) {
        let mut rows = self.rows.borrow_mut();
        while rows.len() < shown.len() {
            let parts = row();
            self.list.append(&parts.row);
            rows.push(parts);
        }
        for (position, parts) in rows.iter().enumerate() {
            let Some(shown) = shown.get(position) else {
                parts.row.set_visible(false);
                continue;
            };
            let item = &items[shown.item];
            parts.title.set_label(&item.title);
            match shown.alias.and_then(|index| item.aliases.get(index)) {
                Some(alias) => {
                    parts.alias.set_label(&format!("({alias})"));
                    parts.alias.set_visible(true);
                }
                None => parts.alias.set_visible(false),
            }
            for (step, (keys, caps)) in parts.keys.iter().enumerate() {
                let wanted = item.caps.get(step);
                keys.set_visible(wanted.is_some());
                for (index, cap) in caps.iter().enumerate() {
                    match wanted.and_then(|wanted| wanted.get(index)) {
                        Some(text) => {
                            cap.set_label(text);
                            cap.set_visible(true);
                        }
                        None => cap.set_visible(false),
                    }
                }
            }
            parts
                .row
                .update_property(&[gtk::accessible::Property::Label(&item.spoken(shown.alias))]);
            parts.row.set_visible(true);
        }
    }

    /// The heading over the row in `position`: on the empty query, where a section or a
    /// category starts.
    fn heading(&self, position: usize) -> Option<String> {
        let shown = self.shown.borrow();
        let items = self.items.borrow();
        let group = |shown: &Shown| match shown.section {
            Section::Suggested => "Suggested",
            Section::Recent => "Recent",
            Section::Other => items.get(shown.item).map_or("", |item| item.group),
        };
        if !self.entry.text().trim().is_empty() {
            return None;
        }
        let here = shown.get(position)?;
        let before = position.checked_sub(1).and_then(|before| shown.get(before));
        (before.map(group) != Some(group(here))).then(|| group(here).to_owned())
    }

    fn selected_index(&self) -> Option<usize> {
        let row = self.list.selected_row()?;
        let position = usize::try_from(row.index()).ok()?;
        self.shown.borrow().get(position).map(|shown| shown.item)
    }

    fn move_selection(&self, step: i32) {
        let count = i32::try_from(self.shown.borrow().len()).unwrap_or(i32::MAX);
        if count == 0 {
            return;
        }
        let current = self.list.selected_row().map_or(-1, |row| row.index());
        let next = (current + step).clamp(0, count - 1);
        let Some(row) = self.list.row_at_index(next) else {
            return;
        };
        self.list.select_row(Some(&row));
        self.scroll_to(&row);
        // The focus stays in the entry, so a screen reader hears the row this way.
        let spoken = usize::try_from(next).ok().and_then(|position| {
            let shown = *self.shown.borrow().get(position)?;
            Some(self.items.borrow().get(shown.item)?.spoken(shown.alias))
        });
        if let Some(spoken) = spoken {
            accessibility::announce(&self.entry, &spoken);
        }
    }

    /// Scrolls just enough to show `row`, and the heading over it when it starts a group,
    /// leaving the keyboard focus in the entry. While rows follow it, the next one shows too:
    /// the last row in sight fades into the card's edge (style.css), and a row is taller than
    /// the fade, so the selected one is never under it; the last row shows the list's end, so
    /// nothing is left below to fade.
    fn scroll_to(&self, row: &gtk::ListBoxRow) {
        let Some(bounds) = row.compute_bounds(&self.list) else {
            return;
        };
        // The heading is the list's child above the row, outside the row's bounds.
        let top = row
            .header()
            .filter(|header| header.is_visible())
            .and_then(|header| header.compute_bounds(&self.list))
            .map_or(bounds.y(), |header| header.y().min(bounds.y()));
        let adjustment = self.scrolled.vadjustment();
        // The pool's rows past the last one shown are hidden.
        let bottom = self
            .list
            .row_at_index(row.index() + 1)
            .filter(|next| next.is_visible())
            .and_then(|next| next.compute_bounds(&self.list))
            .map_or(adjustment.upper(), |next| {
                f64::from(next.y() + next.height())
            });
        let (top, bottom) = (f64::from(top), bottom);
        if top < adjustment.value() {
            adjustment.set_value(top);
        } else if bottom > adjustment.value() + adjustment.page_size() {
            adjustment.set_value(bottom - adjustment.page_size());
        }
    }

    /// Closes the palette, then runs the row's command on the window (or the composer card
    /// it opened from), so a command that opens a dialog opens it over the window and one that
    /// moves the focus moves it from where it was.
    fn run(&self, index: usize) {
        let Some(item) = self.items.borrow().get(index).cloned() else {
            return;
        };
        self.recent.borrow_mut().used(&item.key);
        let handler = self.recent_changed.borrow().clone();
        if let Some(handler) = handler {
            handler(&self.recent.borrow());
        }
        let composer = self.composer.borrow().clone();
        self.close();
        match item.run {
            Run::Command(id) if id.scope() == Scope::Composer => {
                if let Some(composer) = composer {
                    composer.activate_action(id.name(), None);
                }
            }
            Run::Command(id) => {
                let Some(window) = self.window.upgrade() else {
                    return;
                };
                if let Err(error) = WidgetExt::activate_action(&window, &id.detailed_name(), None) {
                    tracing::warn!(action = id.name(), %error, "the palette could not run a command");
                }
            }
            Run::Place(run) => run(),
        }
    }

    fn close(&self) {
        if !self.open.replace(false) {
            return;
        }
        self.layer.set_can_target(false);
        self.fade(0.0, FADE_OUT);
        let back = self
            .return_focus
            .take()
            .and_then(|focus| focus.upgrade())
            .filter(|focus| focus.is_mapped() && focus.is_sensitive());
        let back = back.or_else(|| {
            self.fallback_focus
                .borrow()
                .as_ref()
                .and_then(glib::WeakRef::upgrade)
        });
        if !back.is_some_and(|widget| widget.grab_focus())
            && let Some(window) = self.window.upgrade()
        {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        }
        self.composer.replace(None);
    }

    /// Fades the layer to `opacity` (1 shows it, 0 hides it at the end) over `duration`, or at
    /// once where the desktop asks for less motion.
    fn fade(&self, opacity: f64, duration: Duration) {
        if let Some(previous) = self.fade.take() {
            previous.skip();
        }
        let layer = self.layer.clone();
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak]
            layer,
            move |value| layer.set_opacity(value)
        ));
        let animation = adw::TimedAnimation::builder()
            .widget(&self.layer)
            .value_from(self.layer.opacity())
            .value_to(opacity)
            .duration(duration.as_millis() as u32)
            .easing(adw::Easing::EaseOutCubic)
            .target(&target)
            .build();
        if opacity == 0.0 {
            let layer = self.layer.downgrade();
            animation.connect_done(move |animation| {
                if let Some(layer) = layer.upgrade()
                    && animation.value() == 0.0
                    && !layer.can_target()
                {
                    layer.set_visible(false);
                }
            });
        }
        animation.play();
        self.fade.replace(Some(animation));
    }

    /// Notes how long this opening took to reach the screen, once its first frame is painted.
    fn time_first_frame(self: &Rc<Self>, window: &gtk::Window) {
        let Some(clock) = window.frame_clock() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let held = handler.clone();
        let id = clock.connect_after_paint(move |clock| {
            if let Some(id) = held.take() {
                clock.disconnect(id);
            }
            if let Some(inner) = weak.upgrade()
                && let Some(opened) = inner.opened_at.get()
            {
                let took = opened.elapsed();
                inner.shown_in.set(Some(took));
                tracing::debug!(ms = took.as_secs_f64() * 1000.0, "palette shown");
            }
        });
        handler.replace(Some(id));
    }
}

/// A row of the pool: the title, the word it was found by, and the keycaps at the end.
fn row() -> RowParts {
    let title = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["sw-palette-title"])
        .build();
    let alias = gtk::Label::builder()
        .xalign(0.0)
        .visible(false)
        .css_classes(["sw-palette-alias", "sw-secondary"])
        .build();
    let caps_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .css_classes(["sw-palette-keys"])
        .build();
    let mut keys = Vec::new();
    for _ in 0..2 {
        let step = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(["sw-palette-step"])
            .build();
        let caps: Vec<gtk::Label> = (0..CAPS)
            .map(|_| {
                let cap = gtk::Label::builder().css_classes(["sw-keycap"]).build();
                step.append(&cap);
                cap
            })
            .collect();
        caps_box.append(&step);
        keys.push((step, caps));
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["sw-palette-row-content"])
        .build();
    content.append(&title);
    content.append(&alias);
    content.append(&caps_box);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .css_classes(["sw-palette-row"])
        .build();
    // The focus stays in the search entry, where Up and Down move the chosen row and a screen
    // reader hears it: Tab doesn't walk every row.
    row.set_focusable(false);
    RowParts {
        row,
        title,
        alias,
        keys,
    }
}
