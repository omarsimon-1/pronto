//! The conversation's page in the reader (the fork's Superhuman pass, phase C): the thread's
//! column in its scroller and, in the column's right margin where it has room, a side pane
//! (the contact pane, docs/superhuman/contact.md).
//!
//! The column never moves. The pane shows only when the margin right of the column's own
//! gutter holds [`SIDE_MIN`] px; the scroller then ends at that gutter, so its overlay
//! scrollbar sits between the column and the pane instead of beyond the pane, while its content
//! ([`Inset`]) is moved right by what the scroller lost and the column stays centred in the
//! whole page, where the toolbar above it is. Scrolling over the margin left of the column
//! still scrolls the conversation; the pane scrolls on its own, and a wheel or a touchpad over
//! it when it has nothing to scroll (or beside it) scrolls the conversation. The room is
//! measured on the page's own allocation, so a sidebar beside the list, or Split's list,
//! leaves the pane out when the reader is that much narrower. A composer card open over the
//! reader hides it too ([`ColumnMargin::set_blocked`]): the card would cover its lower half.
//! Hidden, the pane is unmapped, which tells it to wait (`contact::ContactPane::show`).

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The narrowest side pane, its padding included: at the default reading size (a 720 px
/// column) a Focused window shows it from 1360 px.
pub const SIDE_MIN: i32 = 296;
/// The widest: a wider margin leaves the rest of it empty, the pane beside the column.
pub const SIDE_MAX: i32 = 360;

mod inset {
    use super::*;

    #[derive(Default)]
    pub struct Inset {
        pub left: Rc<Cell<i32>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Inset {
        const NAME: &'static str = "ProntoReaderInset";
        type Type = super::Inset;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Inset {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Inset {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.obj()
                .first_child()
                .map_or(gtk::SizeRequestMode::ConstantSize, |child| {
                    child.request_mode()
                })
        }

        // The inset never counts in a width asked for: it is only ever given where the page
        // is wide enough for the column beside it.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.obj().first_child() else {
                return (0, 0, -1, -1);
            };
            let for_size = match orientation {
                gtk::Orientation::Vertical if for_size >= 0 => {
                    let (minimum, ..) = child.measure(gtk::Orientation::Horizontal, -1);
                    (for_size - self.left.get()).max(minimum)
                }
                _ => for_size,
            };
            child.measure(orientation, for_size)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            let (minimum, ..) = child.measure(gtk::Orientation::Horizontal, -1);
            let inset = self.left.get().clamp(0, (width - minimum).max(0));
            // Right to left, the pane is at the left and the scroller at the right: the content
            // gives up its right part instead.
            let x = if self.obj().direction() == gtk::TextDirection::Rtl {
                0
            } else {
                inset
            };
            let at =
                gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32, 0.0));
            child.allocate(width - inset, height, baseline, Some(at));
        }
    }
}

glib::wrapper! {
    /// One child, narrowed by a shared amount at its start (its left, or its right right to
    /// left) that its measured width leaves out.
    pub struct Inset(ObjectSubclass<inset::Inset>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Inset {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let inset: Self = glib::Object::new();
        child.as_ref().set_parent(&inset);
        inset
    }

    /// The amount, which [`ColumnMargin`] sets before it allocates the scroller around it.
    pub fn left(&self) -> Rc<Cell<i32>> {
        self.imp().left.clone()
    }
}

type LeftHandler = Rc<dyn Fn()>;

mod column_margin {
    use super::*;
    use std::cell::OnceCell;

    pub struct ColumnMargin {
        pub column: OnceCell<gtk::Widget>,
        pub side: RefCell<Option<gtk::Widget>>,
        pub inset: OnceCell<Rc<Cell<i32>>>,
        /// The thread's scroller, which a wheel over the margin scrolls, and whether the
        /// pointer is over the column (its scroller scrolls it there).
        pub scroller: OnceCell<gtk::ScrolledWindow>,
        pub over_column: OnceCell<gtk::EventControllerMotion>,
        /// The thread column's width, and the gutter its clamp keeps on either side (its CSS
        /// margin), in px.
        pub column_width: Cell<i32>,
        pub gutter: Cell<i32>,
        /// The user wants the pane (Contact Details); a composer card over the reader hides it.
        pub wanted: Cell<bool>,
        pub blocked: Cell<bool>,
        /// The pane shows; the width it was decided at.
        pub shown: Cell<bool>,
        pub width: Cell<i32>,
        pub due: Cell<bool>,
        /// Told when the pane hides with the keyboard focus in it.
        pub left: RefCell<Vec<LeftHandler>>,
    }

    impl Default for ColumnMargin {
        fn default() -> Self {
            Self {
                column: OnceCell::new(),
                side: RefCell::new(None),
                inset: OnceCell::new(),
                scroller: OnceCell::new(),
                over_column: OnceCell::new(),
                column_width: Cell::new(720),
                gutter: Cell::new(24),
                wanted: Cell::new(true),
                blocked: Cell::new(false),
                shown: Cell::new(false),
                width: Cell::new(0),
                due: Cell::new(false),
                left: RefCell::new(Vec::new()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ColumnMargin {
        const NAME: &'static str = "ProntoColumnMargin";
        type Type = super::ColumnMargin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ColumnMargin {
        fn dispose(&self) {
            self.left.take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl ColumnMargin {
        fn column(&self) -> &gtk::Widget {
            self.column
                .get()
                .expect("the column is set at construction")
        }

        /// Where the scroller ends at `width`, and the room right of it.
        pub fn split(&self, width: i32) -> (i32, i32) {
            let gutter = self.gutter.get();
            let column = self.column_width.get().min(width - 2 * gutter).max(0);
            let end = (width + column) / 2 + gutter;
            (end, width - end)
        }

        /// Whether the pane shows at `width`.
        pub fn fits(&self, width: i32) -> bool {
            self.side.borrow().is_some()
                && self.wanted.get()
                && !self.blocked.get()
                && self.split(width).1 >= SIDE_MIN
        }
    }

    impl WidgetImpl for ColumnMargin {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.column().request_mode()
        }

        // The column alone asks for room: the pane takes what is left over, or doesn't show.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (minimum, natural, ..) = self.column().measure(orientation, for_size);
            (minimum, natural, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let shown = self.shown.get();
            if self.width.replace(width) != width && self.fits(width) != shown {
                // Shown or hidden from the next frame: a child's visibility doesn't change in
                // the middle of the allocation that measured it.
                self.obj().queue_update();
            }
            let side = self.side.borrow().clone();
            let column = self.column();
            match side.filter(|_| shown) {
                Some(side) => {
                    let (end, room) = self.split(width);
                    let side_width = room.min(SIDE_MAX);
                    if let Some(inset) = self.inset.get() {
                        inset.set(width - end);
                    }
                    // Right to left, mirrored: the pane in the column's left margin.
                    let (column_x, side_x) = if self.obj().direction() == gtk::TextDirection::Rtl {
                        (width - end, room - side_width)
                    } else {
                        (0, end)
                    };
                    let at = |x: i32| {
                        gtk::gsk::Transform::new()
                            .translate(&gtk::graphene::Point::new(x as f32, 0.0))
                    };
                    column.allocate(end, height, -1, Some(at(column_x)));
                    side.allocate(side_width, height, -1, Some(at(side_x)));
                }
                None => {
                    if let Some(inset) = self.inset.get() {
                        inset.set(0);
                    }
                    column.allocate(width, height, -1, None);
                }
            }
        }
    }
}

glib::wrapper! {
    /// The thread's scroller, and a pane in the column's right margin when it has room.
    pub struct ColumnMargin(ObjectSubclass<column_margin::ColumnMargin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ColumnMargin {
    /// `column` is the reader's scroller as the page holds it (its `root`), `scroller` the
    /// scrolled window in it, `inset` the [`Inset`] around its content.
    pub fn new(
        column: &impl IsA<gtk::Widget>,
        scroller: &gtk::ScrolledWindow,
        inset: &Inset,
    ) -> Self {
        let margin: Self = glib::Object::new();
        margin.set_hexpand(true);
        margin.set_vexpand(true);
        column.as_ref().set_parent(&margin);
        let imp = margin.imp();
        let _ = imp.column.set(column.as_ref().clone());
        let _ = imp.scroller.set(scroller.clone());
        let _ = imp.inset.set(inset.left());
        let over_column = gtk::EventControllerMotion::new();
        column.as_ref().add_controller(over_column.clone());
        let _ = imp.over_column.set(over_column);
        // What the pane leaves (it scrolls itself while it has more to show) comes here.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = margin.downgrade();
        scroll.connect_scroll(move |scroll, _, dy| {
            weak.upgrade().map_or(glib::Propagation::Proceed, |margin| {
                margin.scroll_column(scroll, dy)
            })
        });
        margin.add_controller(scroll);
        margin
    }

    /// A wheel or a touchpad over the margin beside the column (the pane with nothing left to
    /// scroll, or the room past it) scrolls the conversation, as the whole page did before the
    /// pane was there: a wheel's notch as far as the scroller's own (the page's height to the
    /// power 2/3, GTK's step), a touchpad's pixels as they come, without its fling.
    fn scroll_column(&self, scroll: &gtk::EventControllerScroll, dy: f64) -> glib::Propagation {
        let imp = self.imp();
        let over_column = imp
            .over_column
            .get()
            .is_some_and(|motion| motion.contains_pointer());
        let Some(scroller) = imp.scroller.get() else {
            return glib::Propagation::Proceed;
        };
        if dy == 0.0 || !imp.shown.get() || over_column {
            return glib::Propagation::Proceed;
        }
        let adjustment = scroller.vadjustment();
        let step = match scroll.unit() {
            gdk::ScrollUnit::Wheel => adjustment.page_size().powf(2.0 / 3.0),
            _ => 1.0,
        };
        let end = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        adjustment.set_value((adjustment.value() + dy * step).clamp(adjustment.lower(), end));
        glib::Propagation::Stop
    }

    /// The pane in the margin: after the column in the Tab order, hidden until it fits.
    pub fn set_side(&self, side: &impl IsA<gtk::Widget>) {
        let side = side.as_ref();
        if let Some(old) = self.imp().side.replace(Some(side.clone())) {
            old.unparent();
        }
        side.set_child_visible(false);
        side.insert_after(self, self.imp().column.get());
        self.queue_update();
    }

    /// The thread column's width and its gutter (`reader::column_width`; the reader clamp's
    /// CSS margin), as the theme says them.
    pub fn set_column(&self, width: i32, gutter: i32) {
        let imp = self.imp();
        let wider = imp.column_width.replace(width) != width;
        let gutter_changed = imp.gutter.replace(gutter) != gutter;
        if wider || gutter_changed {
            self.queue_update();
        }
    }

    /// Contact Details: whether the pane shows where it fits.
    pub fn set_wanted(&self, wanted: bool) {
        if self.imp().wanted.replace(wanted) != wanted {
            self.queue_update();
        }
    }

    /// A composer card is open over the reader: the pane hides meanwhile.
    pub fn set_blocked(&self, blocked: bool) {
        if self.imp().blocked.replace(blocked) != blocked {
            self.queue_update();
        }
    }

    /// The pane is shown (mapped where the page is).
    pub fn side_shown(&self) -> bool {
        self.imp().shown.get()
    }

    /// The pane in the margin, shown or not.
    pub fn side(&self) -> Option<gtk::Widget> {
        self.imp().side.borrow().clone()
    }

    /// Whether the margin has the pane's room at the width it was last allocated, the pane
    /// wanted or not; `None` before its first allocation.
    pub fn has_room(&self) -> Option<bool> {
        let imp = self.imp();
        let width = imp.width.get();
        (width > 0).then(|| imp.split(width).1 >= SIDE_MIN)
    }

    /// Runs `handler` when the pane hides with the keyboard focus in it (a narrower window,
    /// Contact Details turned off): it puts the focus in the conversation, where GTK would
    /// leave it on the window's first control.
    pub fn connect_side_left(&self, handler: impl Fn() + 'static) {
        self.imp().left.borrow_mut().push(Rc::new(handler));
    }

    /// Shows or hides the pane for the width the page has now, outside its allocation.
    fn queue_update(&self) {
        let imp = self.imp();
        if imp.due.replace(true) {
            return;
        }
        let weak = self.downgrade();
        glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
            if let Some(margin) = weak.upgrade() {
                margin.update();
            }
            glib::ControlFlow::Break
        });
    }

    fn update(&self) {
        let imp = self.imp();
        imp.due.set(false);
        let shown = imp.fits(imp.width.get());
        let side = imp.side.borrow().clone();
        let mut left = false;
        if let Some(side) = side {
            left = !shown
                && side
                    .root()
                    .and_then(|root| root.focus())
                    .is_some_and(|focus| focus == side || focus.is_ancestor(&side));
            side.set_child_visible(shown);
        }
        imp.shown.set(shown);
        self.queue_allocate();
        if left {
            let handlers = imp.left.borrow().clone();
            for handler in handlers {
                handler();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_needs_the_margin_beside_the_gutter() {
        // A Focused window at the default reading size: the 720 px column centred, its 24 px
        // gutter, then the pane.
        let room = |width: i32| {
            let column = 720.min(width - 48);
            width - ((width + column) / 2 + 24)
        };
        assert_eq!(room(1440), 336);
        assert!(room(1360) >= SIDE_MIN);
        assert!(room(1358) < SIDE_MIN);
        // The sidebar beside the list (1440 - 220), and Split's reader beside a 380 px list.
        assert!(room(1220) < SIDE_MIN);
        assert!(room(1060) < SIDE_MIN);
        assert!(room(1740) >= SIDE_MIN);
        const { assert!(SIDE_MIN < SIDE_MAX) };
    }
}
