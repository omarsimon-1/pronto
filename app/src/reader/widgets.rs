//! Containers the reader needs. [`BodyBox`] holds a WebView at exactly its requested height
//! (S2: a WebView's natural height is WebKit's contents size, which never drops below its
//! viewport), or, once its view has gone back to the pool, keeps that height empty (ADR-011:
//! released bodies keep their measured height, so nothing jumps). [`Stage`] shows one page and
//! lays others out unseen at the same width: the next conversation while it gets its heights,
//! and the conversations next to the selection, ready to swap in (no jump, no blank frame),
//! drawn into nothing only until they have settled (then their animations stop: nothing draws
//! them; [`redraw_up`] draws one again when its view changes).
//! [`ReadingBox`] keeps a plain-text body to a reading measure and has it measured again when
//! its width changes. [`hand_over_focus`] keeps GTK from moving the focus out of a page that
//! has left the window.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

mod body_box {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    pub struct BodyBox {
        /// The height kept while there is no child.
        pub placeholder: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BodyBox {
        const NAME: &'static str = "ProntoReaderBodyBox";
        type Type = super::BodyBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for BodyBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for BodyBox {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let height = self.obj().first_child().map_or_else(
                || self.placeholder.get().max(0),
                |child| child.size_request().1.max(0),
            );
            match orientation {
                gtk::Orientation::Vertical => (height, height, -1, -1),
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }
    }
}

glib::wrapper! {
    /// One child at exactly its height request and the full width; without one, the height
    /// given to [`BodyBox::set_placeholder`].
    pub struct BodyBox(ObjectSubclass<body_box::BodyBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BodyBox {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let body = Self::empty();
        body.set_child(Some(child));
        body
    }

    /// A box with no child yet, as tall as its placeholder.
    pub fn empty() -> Self {
        glib::Object::new()
    }

    /// The height kept while the box has no child.
    pub fn set_placeholder(&self, height: i32) {
        if self.imp().placeholder.replace(height) != height && self.first_child().is_none() {
            self.queue_resize();
        }
    }

    pub fn child(&self) -> Option<gtk::Widget> {
        self.first_child()
    }

    pub fn set_child(&self, child: Option<&impl IsA<gtk::Widget>>) {
        while let Some(old) = self.first_child() {
            old.unparent();
        }
        if let Some(child) = child {
            if child.parent().is_some() {
                child.unparent();
            }
            child.set_parent(self);
        }
        self.queue_resize();
    }
}

mod reading_box {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    pub struct ReadingBox {
        /// The measure in characters (`0`s of the child's font); 0 for none.
        pub chars: Cell<u32>,
        /// The width the child was last given.
        pub width: Cell<i32>,
        /// A measure again waits for the main loop.
        pub queued: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ReadingBox {
        const NAME: &'static str = "ProntoReaderReadingBox";
        type Type = super::ReadingBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ReadingBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl ReadingBox {
        /// The child's width at `width`: the measure, when it is narrower.
        pub fn fit(&self, width: i32) -> i32 {
            let chars = self.chars.get();
            let Some(child) = self.obj().first_child().filter(|_| chars > 0) else {
                return width;
            };
            let layout = child.create_pango_layout(Some("0000000000"));
            let ten = layout.pixel_size().0.max(1);
            width.min(ten * chars as i32 / 10)
        }
    }

    impl WidgetImpl for ReadingBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.obj().first_child() else {
                return (0, 0, -1, -1);
            };
            match orientation {
                gtk::Orientation::Horizontal => {
                    let (minimum, natural, _, _) = child.measure(orientation, for_size);
                    (
                        minimum,
                        natural.min(self.fit(i32::MAX / 2)).max(minimum),
                        -1,
                        -1,
                    )
                }
                _ => {
                    let width = if for_size < 0 {
                        for_size
                    } else {
                        self.fit(for_size)
                    };
                    let (minimum, natural, _, _) = child.measure(orientation, width);
                    (minimum, natural, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            let fitted = self.fit(width);
            child.allocate(fitted, height, baseline, None);
            // A GtkTextView answers a height for the width it was last given, not the one asked
            // for, so a narrower column left its last lines cut until something resized it: once
            // the view has its new width (and has wrapped its text again), it is measured again.
            if self.width.replace(fitted) != fitted && !self.queued.replace(true) {
                let weak = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(reading) = weak.upgrade() {
                        reading.imp().queued.set(false);
                        if let Some(child) = reading.first_child() {
                            child.queue_resize();
                        }
                    }
                });
            }
        }
    }
}

glib::wrapper! {
    /// One plain-text body, no wider than a reading measure (the rest of the column stays
    /// empty beside it), measured again whenever its width changes.
    pub struct ReadingBox(ObjectSubclass<reading_box::ReadingBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ReadingBox {
    /// `child` at most `chars` characters wide (`0`s of its own font); 0 for the full width.
    pub fn new(child: &impl IsA<gtk::Widget>, chars: u32) -> Self {
        let reading: Self = glib::Object::new();
        reading.imp().chars.set(chars);
        reading.set_hexpand(true);
        child.as_ref().set_parent(&reading);
        reading
    }
}

mod stage {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Whether a staged page has settled ([`super::Stage::set_quiet`]).
    pub type Quiet = Box<dyn Fn(&gtk::Widget) -> bool>;

    #[derive(Default)]
    pub struct Stage {
        pub shown: RefCell<Option<gtk::Widget>>,
        /// Pages laid out unseen, in no particular order.
        pub staged: RefCell<Vec<gtk::Widget>>,
        pub quiet: RefCell<Option<Quiet>>,
        /// Staged pages drawn once since they went quiet, and the width they had then.
        pub drawn_quiet: RefCell<Vec<(gtk::Widget, i32)>>,
        /// Staged pages drawn, and left undrawn because they were quiet: for checks.
        pub draws: Cell<u64>,
        pub skips: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Stage {
        const NAME: &'static str = "ProntoReaderStage";
        type Type = super::Stage;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Stage {
        fn dispose(&self) {
            self.quiet.take();
            self.drawn_quiet.take();
            let staged = self.staged.take();
            for child in self.shown.take().into_iter().chain(staged) {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Stage {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.shown
                .borrow()
                .as_ref()
                .map_or(gtk::SizeRequestMode::ConstantSize, |child| {
                    child.request_mode()
                })
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match self.shown.borrow().as_ref() {
                Some(child) => child.measure(orientation, for_size),
                None => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.shown.borrow().as_ref() {
                child.allocate(width, height, baseline, None);
            }
            for child in self.staged.borrow().iter() {
                let (_, natural, _, _) = child.measure(gtk::Orientation::Vertical, width);
                child.allocate(width, natural.max(1), -1, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.shown.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
            // Pages laid out unseen are drawn into nothing: WebKit hands a view its next frame
            // only once the last one was drawn, so the views of a page never drawn stop after
            // their first frame (blank in the frame that swaps the page in, and deaf to a
            // width change). Once a page has settled (its views loaded, painted, measured at
            // its width and unchanged for a while: `live::SETTLE`), it is drawn once more (the
            // frame WebKit made meanwhile is taken) and then left alone: its views stop, and
            // an animated GIF or CSS animation in it no longer keeps the web process and the
            // frame clock busy while the user reads another conversation (battery). Shown,
            // given another width, or changed (its images in, a new height: the reader then
            // draws it through `redraw_up`), it is drawn again and WebKit goes on.
            let staged = self.staged.borrow();
            let quiet = self.quiet.borrow();
            let mut drawn_quiet = self.drawn_quiet.borrow_mut();
            let mut clipped = false;
            for child in staged.iter() {
                let width = child.width();
                let settled = quiet.as_ref().is_some_and(|quiet| quiet(child));
                let at = drawn_quiet.iter().position(|(page, _)| page == child);
                if settled && at.is_some_and(|at| drawn_quiet[at].1 == width) {
                    self.skips.set(self.skips.get() + 1);
                    continue;
                }
                if !clipped {
                    snapshot.push_clip(&gtk::graphene::Rect::zero());
                    clipped = true;
                }
                self.obj().snapshot_child(child, snapshot);
                self.draws.set(self.draws.get() + 1);
                match (settled, at) {
                    (true, Some(at)) => drawn_quiet[at].1 = width,
                    (true, None) => drawn_quiet.push((child.clone(), width)),
                    (false, Some(at)) => {
                        drawn_quiet.swap_remove(at);
                    }
                    (false, None) => {}
                }
            }
            if clipped {
                snapshot.pop();
            }
        }
    }
}

/// The CSS class of a page laid out unseen; widget walks (the self-tests') pass over it.
pub const STAGED: &str = "sw-reader-staged";

glib::wrapper! {
    /// The page on screen, and pages laid out at the same width without being drawn or
    /// reachable by pointer, keyboard or screen reader.
    pub struct Stage(ObjectSubclass<stage::Stage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Stage {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// Puts `page` out of sight and reach: no pointer, no focus, nothing for a screen reader.
fn hide(page: &gtk::Widget) {
    page.set_can_target(false);
    page.set_can_focus(false);
    page.add_css_class(STAGED);
    page.update_state(&[gtk::accessible::State::Hidden(true)]);
}

/// Gives `page` back what [`hide`] (or the reader's own hiding of a page the selection left)
/// took.
fn reveal(page: &gtk::Widget) {
    page.set_can_target(true);
    page.set_can_focus(true);
    page.set_opacity(1.0);
    page.remove_css_class(STAGED);
    page.update_state(&[gtk::accessible::State::Hidden(false)]);
}

impl Stage {
    pub fn shown(&self) -> Option<gtk::Widget> {
        self.imp().shown.borrow().clone()
    }

    /// Tells whether a staged page has settled: such a page is drawn once more, then no longer
    /// while it stays settled at that width.
    pub fn set_quiet(&self, quiet: impl Fn(&gtk::Widget) -> bool + 'static) {
        self.imp().quiet.replace(Some(Box::new(quiet)));
    }

    /// Staged pages drawn so far, and left undrawn because they were quiet: for checks.
    pub fn staged_draws(&self) -> (u64, u64) {
        let imp = self.imp();
        (imp.draws.get(), imp.skips.get())
    }

    /// A staged page leaves, or comes on screen: it is drawn afresh if it is staged again.
    fn forget_drawn(&self, page: &gtk::Widget) {
        self.imp()
            .drawn_quiet
            .borrow_mut()
            .retain(|(drawn, _)| drawn != page);
    }

    /// Whether `page` is laid out here unseen.
    pub fn is_staged(&self, page: &impl IsA<gtk::Widget>) -> bool {
        let page = page.as_ref();
        self.imp()
            .staged
            .borrow()
            .iter()
            .any(|staged| staged == page)
    }

    /// Lays `page` out unseen, beside any other page waiting.
    pub fn stage(&self, page: &impl IsA<gtk::Widget>) {
        let page = page.as_ref();
        if self.is_staged(page) {
            return;
        }
        hide(page);
        if page.parent().as_ref() != Some(self.upcast_ref()) {
            if page.parent().is_some() {
                page.unparent();
            }
            page.set_parent(self);
        }
        self.imp().staged.borrow_mut().push(page.clone());
        self.queue_resize();
    }

    /// Drops the waiting `page`.
    pub fn unstage(&self, page: &impl IsA<gtk::Widget>) {
        let page = page.as_ref();
        let removed = {
            let mut staged = self.imp().staged.borrow_mut();
            let before = staged.len();
            staged.retain(|staged| staged != page);
            staged.len() != before
        };
        if removed {
            self.forget_drawn(page);
            page.unparent();
            self.queue_resize();
        }
    }

    /// Shows `page` (a waiting one, or a new one) in place of the shown page; the page it
    /// replaces stays, waiting unseen (`keep`), or leaves. Returns the replaced page. If the
    /// focus was in the replaced page, `focus` takes it.
    pub fn show(
        &self,
        page: Option<&impl IsA<gtk::Widget>>,
        focus: Option<&gtk::Widget>,
        keep: bool,
    ) -> Option<gtk::Widget> {
        let page = page.map(|page| page.as_ref().clone());
        if let Some(page) = &page {
            let staged = {
                let mut staged = self.imp().staged.borrow_mut();
                let before = staged.len();
                staged.retain(|staged| staged != page);
                staged.len() != before
            };
            if !staged {
                if page.parent().is_some() {
                    page.unparent();
                }
                page.set_parent(self);
            }
            self.forget_drawn(page);
            reveal(page);
        }
        let old = self.imp().shown.replace(page);
        if let Some(old) = &old {
            hand_over_focus(old, focus);
            if keep {
                hide(old);
                self.imp().staged.borrow_mut().push(old.clone());
            } else {
                old.unparent();
            }
        }
        self.queue_resize();
        old
    }
}

/// Queues a draw of `widget` and of every widget above it. GTK passes a draw up only as far as
/// a widget that already waits for one, and a widget its parent leaves undrawn (a settled page
/// on the [`Stage`], a quiet reader behind a Focused list) goes on waiting, so a draw queued
/// below it would stop there.
pub fn redraw_up(widget: &impl IsA<gtk::Widget>) {
    let mut next = Some(widget.as_ref().clone());
    while let Some(widget) = next {
        widget.queue_draw();
        next = widget.parent();
    }
}

/// Before `old` leaves the window: if the focus is in it, `target` takes it, or nothing does.
/// Left to GTK 4.22, the focus moves later, from a widget no longer in the window, through the
/// whole window's focus chain (a Gtk-CRITICAL on the way), to the first selectable label.
/// `true` when `target` took it.
pub fn hand_over_focus(old: &impl IsA<gtk::Widget>, target: Option<&gtk::Widget>) -> bool {
    let old = old.as_ref();
    let Some(root) = old.root() else {
        return false;
    };
    let inside = root
        .focus()
        .is_some_and(|focus| &focus == old || focus.is_ancestor(old));
    if !inside {
        return false;
    }
    let taken = target.is_some_and(|target| target.grab_focus());
    if !taken {
        root.set_focus(None::<&gtk::Widget>);
    }
    taken
}
