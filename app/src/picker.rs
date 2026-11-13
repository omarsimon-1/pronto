//! A palette-style picker over the window (the fork's phase B): a quiet heading, a field and a
//! list, in the command palette's look and classes, for choices that aren't commands: the time
//! picker (Send Later, and Remind Me's later; [`crate::time_picker`]) and the snippet picker
//! ([`crate::snippets`]). The field filters or reads what is typed, Up and Down move, Enter or a
//! click chooses, and Escape, a click beside it or the focus going elsewhere closes it; the
//! keyboard focus goes back where it was before a choice runs, so a snippet goes in at the caret.
//! The command palette and a picker take each other's place: the one open closes first, so the
//! focus goes back to the draft from either.
//!
//! Like the palette it is built once per window ([`install`]) and only hidden between uses: the
//! rows are a pool whose labels change in place. Its layer carries the palette's layer class, so
//! what lets the palette borrow the focus (the search field staying open, `palette::holds`) lets
//! a picker borrow it too.

use crate::accessibility;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The picker's width while the window has room for it, in px; narrower windows leave
/// [`EDGE`] at each side.
pub const WIDTH: i32 = 560;
/// Where its top edge sits: the palette's place, a fifth of the way down.
pub const TOP: f64 = crate::palette::TOP;
pub const EDGE: i32 = crate::palette::EDGE;
/// The list's height at most, in px (about eight rows).
pub const LIST_HEIGHT: i32 = 352;
/// How long it fades in and out: the palette's.
pub const FADE_IN: Duration = crate::palette::FADE_IN;
pub const FADE_OUT: Duration = crate::palette::FADE_OUT;
/// The rows made when the picker is built.
const POOL: usize = 8;
/// The palette's layer class (`palette::holds`).
const LAYER: &str = "sw-palette-layer";

/// One row of a picker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    /// Quiet, at the right: a preset's time, a snippet's first words.
    pub detail: String,
    /// A row that only says something (nothing matches, a hint): it can't be chosen.
    pub hint: bool,
}

impl Row {
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            hint: false,
        }
    }

    pub fn hint(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: String::new(),
            hint: true,
        }
    }
}

/// The rows for a query, best first.
pub type Rows = Box<dyn Fn(&str) -> Vec<Row>>;
/// A choice: the row's place among the last rows given, and the query.
pub type Choose = Box<dyn Fn(usize, &str)>;

/// Where a row's detail goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Details {
    /// At the row's end, right-aligned: a preset's time.
    #[default]
    End,
    /// In one column after the titles, left-aligned: a snippet's first words, which then all
    /// start at the same place (phase C's visual QA: right-aligned, each started elsewhere).
    Column,
}

/// The field's glyph while it finds something (Use Snippet…).
pub const FIND_ICON: &[&str] = &["system-search-symbolic"];
/// The field's glyph while it reads a time (Send Later, Remind Me): the composer's Send Later
/// clock (`appointment-soon-symbolic` carries an orange warning badge in Adwaita).
pub const TIME_ICON: &[&str] = &[
    "document-open-recent-symbolic",
    "preferences-system-time-symbolic",
    "alarm-symbolic",
];

/// What [`Picker::open`] shows this time.
pub struct Spec {
    /// The heading over the field and the dialog's accessible name ("Send later").
    pub title: String,
    pub placeholder: String,
    /// Its widget name, for checks: `time-picker`, `snippet-picker`.
    pub name: &'static str,
    /// Its width while the window has room ([`WIDTH`] for most).
    pub width: i32,
    /// The glyph at the field's start ([`FIND_ICON`], [`TIME_ICON`]).
    pub icon: &'static [&'static str],
    pub details: Details,
    pub rows: Rows,
    pub choose: Choose,
}

struct RowParts {
    row: gtk::ListBoxRow,
    title: gtk::Label,
    detail: gtk::Label,
}

struct Inner {
    window: glib::WeakRef<gtk::Window>,
    layer: gtk::Overlay,
    card: gtk::Box,
    heading: gtk::Label,
    entry: gtk::SearchEntry,
    list: gtk::ListBox,
    scrolled: gtk::ScrolledWindow,
    pool: RefCell<Vec<RowParts>>,
    rows: RefCell<Vec<Row>>,
    spec: RefCell<Option<Rc<Spec>>>,
    width: Cell<i32>,
    details: Cell<Details>,
    open: Cell<bool>,
    return_focus: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    quiet: Cell<bool>,
    fade: RefCell<Option<adw::TimedAnimation>>,
    /// When it last opened, and how long until its first frame was painted.
    opened_at: Cell<Option<Instant>>,
    shown_in: Cell<Option<Duration>>,
}

/// A window's picker; [`install`] makes it, [`of`] finds it.
#[derive(Clone)]
pub struct Picker {
    inner: Rc<Inner>,
}

thread_local! {
    /// Each window's picker.
    static PICKERS: RefCell<Vec<(glib::WeakRef<gtk::Window>, Picker)>> =
        const { RefCell::new(Vec::new()) };
}

/// The picker of `window`, built now if it has none.
pub fn of(window: &impl IsA<gtk::Window>) -> Picker {
    let window = window.as_ref();
    known(window).unwrap_or_else(|| install(window))
}

/// The picker of `window`, when it has one.
fn known(window: &gtk::Window) -> Option<Picker> {
    PICKERS.with(|pickers| {
        let mut pickers = pickers.borrow_mut();
        pickers.retain(|(held, _)| held.upgrade().is_some());
        pickers
            .iter()
            .find(|(held, _)| held.upgrade().as_ref() == Some(window))
            .map(|(_, picker)| picker.clone())
    })
}

/// Closes `window`'s picker when one is open, the focus going back where it was before it (the
/// command palette opening over it takes its place); a window without one gets none.
pub fn close_in(window: &impl IsA<gtk::Window>) {
    if let Some(picker) = known(window.as_ref()) {
        picker.close();
    }
}

/// Builds `window`'s picker, hidden, over its content (the content goes into an overlay whose
/// top layer the picker is, as the palette's does).
pub fn install(window: &impl IsA<gtk::Window>) -> Picker {
    let window = window.as_ref();
    let host = gtk::Overlay::new();
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

    let heading = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["sw-palette-heading", "sw-secondary", "sw-picker-heading"])
        .build();
    heading.set_accessible_role(gtk::AccessibleRole::Presentation);
    let entry = gtk::SearchEntry::builder()
        .search_delay(0)
        .hexpand(true)
        .css_classes(["sw-palette-search", "sw-picker-search"])
        .build();
    entry.set_widget_name("picker-search");
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Browse)
        .css_classes(["sw-palette-list", "sw-picker-list"])
        .build();
    list.set_widget_name("picker-list");
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
        .css_classes(["sw-palette-card", "sw-picker-card"])
        .build();
    card.update_property(&[gtk::accessible::Property::Modal(true)]);
    card.append(&heading);
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
        .css_classes([LAYER, "sw-picker-layer"])
        .build();
    layer.set_widget_name("picker-layer");
    layer.add_overlay(&card);
    host.add_overlay(&layer);

    let inner = Rc::new(Inner {
        window: window.downgrade(),
        layer,
        card,
        heading,
        entry,
        list,
        scrolled,
        pool: RefCell::new(Vec::new()),
        rows: RefCell::new(Vec::new()),
        spec: RefCell::new(None),
        width: Cell::new(WIDTH),
        details: Cell::new(Details::End),
        open: Cell::new(false),
        return_focus: RefCell::new(None),
        quiet: Cell::new(false),
        fade: RefCell::new(None),
        opened_at: Cell::new(None),
        shown_in: Cell::new(None),
    });
    connect(&inner, &backdrop);
    // A few rows made now, hidden: a first opening only fills them.
    inner.show_rows(&vec![Row::default(); POOL]);
    inner.show_rows(&[]);
    let picker = Picker { inner };
    PICKERS.with(|pickers| {
        pickers
            .borrow_mut()
            .push((window.downgrade(), picker.clone()))
    });
    picker
}

impl Picker {
    /// Opens over the window with the cursor in an empty field and `spec`'s rows; one open
    /// already, or the command palette, gives way to it.
    pub fn open(&self, spec: Spec) {
        let inner = &self.inner;
        if inner.open.get() {
            inner.close();
        }
        // The palette closes first and gives the focus back where it was (the draft it opened
        // over), which is where this picker gives it back: never into a layer that is closing.
        if let Some(window) = inner.window.upgrade() {
            crate::palette::close_in(&window);
        }
        inner.opened_at.set(Some(Instant::now()));
        inner.shown_in.set(None);
        inner.heading.set_label(&spec.title);
        inner.card.set_widget_name(spec.name);
        inner
            .card
            .update_property(&[gtk::accessible::Property::Label(&spec.title)]);
        inner.entry.set_placeholder_text(Some(&spec.placeholder));
        inner
            .entry
            .update_property(&[gtk::accessible::Property::Label(&spec.title)]);
        set_field_icon(&inner.entry, spec.icon);
        inner.details.set(spec.details);
        inner.width.set(spec.width);
        inner.spec.replace(Some(Rc::new(spec)));
        inner.quiet.set(true);
        inner.entry.set_text("");
        inner.quiet.set(false);
        inner.refresh();
        let window = inner.window.upgrade();
        let focus = window
            .as_ref()
            .and_then(gtk::prelude::GtkWindowExt::focus)
            .filter(|focus| !crate::palette::holds(focus));
        inner
            .return_focus
            .replace(focus.map(|focus| focus.downgrade()));
        inner.open.set(true);
        inner.layer.set_can_target(true);
        inner.layer.set_visible(true);
        inner.layer.queue_allocate();
        inner.fade(1.0, FADE_IN);
        inner.entry.grab_focus();
        if let Some(window) = window {
            inner.time_first_frame(&window);
        }
    }

    /// How long its last opening took, from [`Picker::open`] to the first painted frame.
    pub fn shown_in(&self) -> Option<Duration> {
        self.inner.shown_in.get()
    }

    /// Closes it without a choice; the focus goes back where it was.
    pub fn close(&self) {
        self.inner.close();
    }

    pub fn is_open(&self) -> bool {
        self.inner.open.get()
    }

    /// The heading of the open picker.
    pub fn title(&self) -> Option<String> {
        self.is_open()
            .then(|| self.inner.heading.label().to_string())
    }

    /// The widget name of the open picker (`time-picker`).
    pub fn name(&self) -> Option<String> {
        self.is_open()
            .then(|| self.inner.card.widget_name().to_string())
    }

    /// Types `query` into the field, as a user would.
    pub fn set_query(&self, query: &str) {
        self.inner.entry.set_text(query);
        self.inner.refresh();
    }

    pub fn query(&self) -> String {
        self.inner.entry.text().to_string()
    }

    /// The rows shown, best first.
    pub fn rows(&self) -> Vec<Row> {
        self.inner.rows.borrow().clone()
    }

    /// The row Enter would choose.
    pub fn selected(&self) -> Option<usize> {
        self.inner.selected()
    }

    /// Moves the selection as Down (positive) or Up (negative) does.
    pub fn move_selection(&self, step: i32) {
        self.inner.move_selection(step);
    }

    /// Selects row `index`, when it can be chosen.
    pub fn select(&self, index: usize) -> bool {
        let choosable = self
            .inner
            .rows
            .borrow()
            .get(index)
            .is_some_and(|row| !row.hint);
        let row = i32::try_from(index)
            .ok()
            .and_then(|index| self.inner.list.row_at_index(index));
        match (choosable, row) {
            (true, Some(row)) => {
                self.inner.list.select_row(Some(&row));
                true
            }
            _ => false,
        }
    }

    /// Chooses the selected row, as Enter does; `false` when there is none to choose.
    pub fn activate_selected(&self) -> bool {
        match self.inner.selected() {
            Some(index) => {
                self.inner.choose(index);
                true
            }
            None => false,
        }
    }

    /// The field: typing, Enter and Escape arrive there.
    pub fn search_entry(&self) -> gtk::SearchEntry {
        self.inner.entry.clone()
    }

    /// The icon at the start of the field (`document-open-recent-symbolic`), for checks.
    pub fn field_icon(&self) -> Option<String> {
        field_icon(&self.inner.entry)
    }

    /// Where the rows' details go now, for checks.
    pub fn details(&self) -> Details {
        self.inner.details.get()
    }

    /// Where the shown rows' details start, px from the list's left edge, for checks.
    pub fn detail_starts(&self) -> Vec<f32> {
        let pool = self.inner.pool.borrow();
        pool.iter()
            .filter(|parts| parts.row.is_visible() && parts.detail.is_visible())
            .filter_map(|parts| parts.detail.compute_bounds(&self.inner.list))
            .map(|bounds| bounds.x())
            .collect()
    }

    /// The dialog a screen reader knows.
    pub fn card(&self) -> gtk::Widget {
        self.inner.card.clone().upcast()
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
            && let Some(index) = inner.selected()
        {
            inner.choose(index);
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
        let index = usize::try_from(row.index()).ok();
        let choosable =
            index.is_some_and(|index| inner.rows.borrow().get(index).is_some_and(|row| !row.hint));
        if let (Some(index), true) = (index, choosable) {
            inner.choose(index);
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
    // The focus stays in the field, as a dialog keeps it.
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
    // The focus went elsewhere in the window: the picker closes, as on Escape.
    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(inner);
    focus.connect_leave(move |_| {
        let weak = weak.clone();
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
            (widget == inner.card.upcast_ref::<gtk::Widget>())
                .then(|| place(layer, &inner.card, inner.width.get()))
        });
}

/// Where the card goes: `width` wide (less in a narrow window), its top a fifth of the way
/// down, held inside the window's edges.
fn place(layer: &gtk::Overlay, card: &gtk::Box, width: i32) -> gdk::Rectangle {
    let (layer_width, height) = (layer.width(), layer.height());
    let w = width.min(layer_width - 2 * EDGE).max(1);
    let (_, natural, _, _) = card.measure(gtk::Orientation::Vertical, w);
    let h = natural.min(height - 2 * EDGE).max(1);
    let top = (f64::from(height) * TOP).round() as i32;
    let y = top.min(height - h - EDGE).max(EDGE.min(height - h).max(0));
    gdk::Rectangle::new(((layer_width - w) / 2).max(0), y, w, h)
}

impl Inner {
    fn refresh(&self) {
        let spec = self.spec.borrow().clone();
        let Some(spec) = spec else {
            return;
        };
        let rows = (spec.rows)(&self.entry.text());
        self.show_rows(&rows);
        self.rows.replace(rows);
        // The first row that can be chosen is the one Enter chooses.
        let first = self
            .rows
            .borrow()
            .iter()
            .position(|row| !row.hint)
            .and_then(|index| self.list.row_at_index(index as i32));
        match first {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
        self.scrolled.vadjustment().set_value(0.0);
        self.layer.queue_allocate();
    }

    fn show_rows(&self, rows: &[Row]) {
        let mut pool = self.pool.borrow_mut();
        while pool.len() < rows.len() {
            let parts = row();
            self.list.append(&parts.row);
            pool.push(parts);
        }
        let column = self.details.get() == Details::Column;
        for (position, parts) in pool.iter().enumerate() {
            let Some(row) = rows.get(position) else {
                parts.row.set_visible(false);
                continue;
            };
            lay_out(parts, column);
            parts.title.set_label(&row.title);
            parts.detail.set_label(&row.detail);
            parts.detail.set_visible(!row.detail.is_empty());
            parts.row.set_selectable(!row.hint);
            parts.row.set_activatable(!row.hint);
            if row.hint {
                parts.row.add_css_class("sw-picker-hint");
            } else {
                parts.row.remove_css_class("sw-picker-hint");
            }
            let spoken = if row.detail.is_empty() {
                row.title.clone()
            } else {
                format!("{}, {}", row.title, row.detail)
            };
            parts
                .row
                .update_property(&[gtk::accessible::Property::Label(&spoken)]);
            parts.row.set_visible(true);
        }
    }

    fn selected(&self) -> Option<usize> {
        let row = self.list.selected_row()?;
        let index = usize::try_from(row.index()).ok()?;
        self.rows
            .borrow()
            .get(index)
            .is_some_and(|row| !row.hint)
            .then_some(index)
    }

    fn move_selection(&self, step: i32) {
        let rows = self.rows.borrow().clone();
        let choosable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.hint)
            .map(|(index, _)| index)
            .collect();
        if choosable.is_empty() {
            return;
        }
        let current = self
            .selected()
            .and_then(|index| choosable.iter().position(|known| *known == index));
        let next = match current {
            Some(at) => (at as i32 + step).clamp(0, choosable.len() as i32 - 1) as usize,
            None => 0,
        };
        let index = choosable[next];
        let Some(row) = self.list.row_at_index(index as i32) else {
            return;
        };
        self.list.select_row(Some(&row));
        if let Some(bounds) = row.compute_bounds(&self.list) {
            let adjustment = self.scrolled.vadjustment();
            let (top, bottom) = (
                f64::from(bounds.y()),
                f64::from(bounds.y() + bounds.height()),
            );
            if top < adjustment.value() {
                adjustment.set_value(top);
            } else if bottom > adjustment.value() + adjustment.page_size() {
                adjustment.set_value(bottom - adjustment.page_size());
            }
        }
        // The focus stays in the field, so a screen reader hears the row this way.
        if let Some(row) = rows.get(index) {
            let spoken = if row.detail.is_empty() {
                row.title.clone()
            } else {
                format!("{}, {}", row.title, row.detail)
            };
            accessibility::announce(&self.entry, &spoken);
        }
    }

    /// Closes the picker (the focus goes back where it was), then runs the choice.
    fn choose(&self, index: usize) {
        let query = self.entry.text().to_string();
        let spec = self.spec.borrow().clone();
        self.close();
        if let Some(spec) = spec {
            (spec.choose)(index, &query);
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
        if !back.is_some_and(|widget| widget.grab_focus())
            && let Some(window) = self.window.upgrade()
        {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        }
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
                tracing::debug!(ms = took.as_secs_f64() * 1000.0, "picker shown");
            }
        });
        handler.replace(Some(id));
    }

    /// Fades the layer to `opacity` (0 hides it at the end), or at once where the desktop asks
    /// for less motion (the theme turns animations off).
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
}

/// Where `parts`' detail goes: in one column after titles of one width, or at the row's end.
fn lay_out(parts: &RowParts, column: bool) {
    if parts.detail.hexpands() == column {
        return;
    }
    parts.title.set_hexpand(!column);
    parts
        .title
        .set_width_chars(if column { TITLE_CHARS } else { -1 });
    parts
        .title
        .set_max_width_chars(if column { TITLE_CHARS } else { -1 });
    parts.detail.set_hexpand(column);
    parts.detail.set_xalign(if column { 0.0 } else { 1.0 });
    parts
        .detail
        .set_max_width_chars(if column { -1 } else { DETAIL_CHARS });
}

/// A title's width when the details are in a column, in characters.
const TITLE_CHARS: i32 = 18;
/// A detail's width at most at the row's end, in characters.
const DETAIL_CHARS: i32 = 36;

/// Puts `names`' icon at the start of `entry` in place of its magnifier (its first image; the
/// clear button's is the last).
fn set_field_icon(entry: &gtk::SearchEntry, names: &[&str]) {
    let mut child = entry.first_child();
    while let Some(widget) = child {
        if let Some(image) = widget.downcast_ref::<gtk::Image>() {
            image.set_from_gicon(&gtk::gio::ThemedIcon::from_names(names));
            return;
        }
        child = widget.next_sibling();
    }
}

/// The icon at the start of `entry`'s field, for checks.
pub fn field_icon(entry: &gtk::SearchEntry) -> Option<String> {
    let mut child = entry.first_child();
    while let Some(widget) = child {
        if let Some(image) = widget.downcast_ref::<gtk::Image>() {
            return image
                .gicon()
                .and_downcast::<gtk::gio::ThemedIcon>()
                .and_then(|icon| icon.names().first().map(|name| name.to_string()))
                .or_else(|| image.icon_name().map(|name| name.to_string()));
        }
        child = widget.next_sibling();
    }
    None
}

/// A row of the pool: the title, and its detail at the end (or in a column, [`lay_out`]).
fn row() -> RowParts {
    let title = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["sw-palette-title", "sw-picker-title"])
        .build();
    let detail = gtk::Label::builder()
        .xalign(1.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(DETAIL_CHARS)
        .css_classes(["sw-secondary", "sw-picker-detail"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["sw-palette-row-content"])
        .build();
    content.append(&title);
    content.append(&detail);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .css_classes(["sw-palette-row"])
        .build();
    row.set_focusable(false);
    RowParts { row, title, detail }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_say_whether_they_can_be_chosen() {
        let row = Row::new("Later today", "today 17:30");
        assert!(!row.hint);
        assert_eq!(row.detail, "today 17:30");
        let hint = Row::hint("Nothing matches");
        assert!(hint.hint && hint.detail.is_empty());
    }
}
