//! The message list and the reader in the window's mail area (the Superhuman pass, 2026-10-28):
//! side by side, the list at its dragged width (Split), or one at a time, the other out of the
//! way (Focused at every width, and any window too narrow for two). Switching is a change of
//! which child is allocated and drawn, so it lands in the next frame: AdwNavigationSplitView
//! slid its pages and can't be told not to.
//!
//! While the list is in front the reader can stay warm ([`MailPanes::set_layout`]'s `warm`):
//! mapped and laid out at the area's size, as [`crate::reader`]'s stage lays out a page unseen,
//! and drawn into nothing (an empty clip) until it has settled ([`MailPanes::set_reader_quiet`]:
//! then animated mail in it stops costing battery; the reader draws it again when a view in it
//! changes), out of reach of the pointer and the keyboard and hidden from screen readers.
//! The conversation the list's cursor is on is then ready when it is opened, and showing it is
//! one frame. A reader that isn't warm is unmapped, which lets it give its views back.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::{Cell, OnceCell, RefCell};

    /// Whether the warm reader has nothing left to paint ([`super::MailPanes::set_reader_quiet`]).
    pub type Quiet = Box<dyn Fn() -> bool>;

    #[derive(Default)]
    pub struct MailPanes {
        pub list: OnceCell<gtk::Widget>,
        pub reader: OnceCell<gtk::Widget>,
        /// One pane at a time; else the list beside the reader.
        pub one_pane: Cell<bool>,
        /// With one pane, the reader is the one shown.
        pub reader_front: Cell<bool>,
        /// With one pane and the list shown, the reader stays mapped behind it.
        pub warm: Cell<bool>,
        /// The list's width beside the reader, in px.
        pub list_width: Cell<i32>,
        pub quiet: RefCell<Option<Quiet>>,
        /// The warm reader's size when it was last drawn while quiet.
        pub drawn_quiet: Cell<Option<(i32, i32)>>,
        /// The warm reader drawn behind the list, and left undrawn because it was quiet.
        pub draws: Cell<u64>,
        pub skips: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailPanes {
        const NAME: &'static str = "ProntoMailPanes";
        type Type = super::MailPanes;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MailPanes {
        fn dispose(&self) {
            self.quiet.take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl MailPanes {
        fn list(&self) -> &gtk::Widget {
            self.list.get().expect("the list is set at construction")
        }

        fn reader(&self) -> &gtk::Widget {
            self.reader
                .get()
                .expect("the reader is set at construction")
        }

        fn front(&self) -> &gtk::Widget {
            if self.reader_front.get() {
                self.reader()
            } else {
                self.list()
            }
        }

        /// The list's width beside the reader: as dragged, but never under its own minimum.
        fn list_width_for(&self, height: i32) -> i32 {
            let (minimum, ..) = self.list().measure(gtk::Orientation::Horizontal, height);
            self.list_width.get().max(minimum)
        }

        /// The reader is laid out behind the list (warm and not in front).
        pub fn reader_behind(&self) -> bool {
            self.one_pane.get() && !self.reader_front.get() && self.warm.get()
        }
    }

    impl WidgetImpl for MailPanes {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            // With one pane only the one shown counts: a warm reader behind the list never
            // makes the window wider or taller than the list needs.
            if self.one_pane.get() {
                let (minimum, natural, ..) = self.front().measure(orientation, for_size);
                return (minimum, natural, -1, -1);
            }
            // Beside the list, a reader squeezed under its minimum is cut at the area's edge
            // (`size_allocate`), so the list's width is all the two need. libadwaita lays the old
            // width class out at the new width for a frame as the window narrows (1440 to
            // 800 px): with the reader's minimum counted too, the split view beside them gave
            // the hidden sidebar a negative width, and GTK warned for every widget in it.
            match orientation {
                gtk::Orientation::Horizontal => {
                    let list = self.list_width_for(for_size);
                    let (_, natural, ..) = self.reader().measure(orientation, for_size);
                    (list, list + natural, -1, -1)
                }
                _ => {
                    // Measured narrower than both need, the reader is measured at its minimum,
                    // as it is laid out (`size_allocate`).
                    let list = self.list_width_for(-1);
                    let (minimum, ..) = self.reader().measure(gtk::Orientation::Horizontal, -1);
                    let reader = if for_size < 0 {
                        -1
                    } else {
                        (for_size - list).max(minimum)
                    };
                    let (list_min, list_nat, ..) = self.list().measure(orientation, list);
                    let (reader_min, reader_nat, ..) = self.reader().measure(orientation, reader);
                    (list_min.max(reader_min), list_nat.max(reader_nat), -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let (list, reader) = (self.list(), self.reader());
            if !self.one_pane.get() {
                let list_width = self.list_width_for(height).min(width);
                list.allocate(list_width, height, -1, None);
                // A reader squeezed under its minimum keeps it and is cut at the area's edge.
                let (minimum, ..) = reader.measure(gtk::Orientation::Horizontal, height);
                let reader_width = (width - list_width).max(minimum);
                let at = gtk::gsk::Transform::new()
                    .translate(&gtk::graphene::Point::new(list_width as f32, 0.0));
                reader.allocate(reader_width, height, -1, Some(at));
                return;
            }
            self.front().allocate(width, height, -1, None);
            if self.reader_behind() {
                // Never drawn: it may keep its minimum size where the area is smaller.
                let (min_width, ..) = reader.measure(gtk::Orientation::Horizontal, -1);
                let width = width.max(min_width);
                let (min_height, ..) = reader.measure(gtk::Orientation::Vertical, width);
                reader.allocate(width, height.max(min_height), -1, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            if self.one_pane.get() {
                obj.snapshot_child(self.front(), snapshot);
                // A warm reader is drawn into nothing, as the reader's stage draws the pages it
                // lays out ahead: WebKit hands a view its next frame only once the last one was
                // drawn, so a reader never drawn behind the list stalls its views after their
                // first frame, and neither the conversation nor its neighbours get ready. Once
                // it is quiet (all settled at their width) it is drawn once more, then left:
                // its views stop until it shows, its size changes, or a view in it changes.
                if self.reader_behind() {
                    let reader = self.reader();
                    let size = (reader.width(), reader.height());
                    let quiet = self.quiet.borrow().as_ref().is_some_and(|quiet| quiet());
                    if quiet && self.drawn_quiet.get() == Some(size) {
                        self.skips.set(self.skips.get() + 1);
                    } else {
                        snapshot.push_clip(&gtk::graphene::Rect::zero());
                        obj.snapshot_child(reader, snapshot);
                        snapshot.pop();
                        self.draws.set(self.draws.get() + 1);
                        self.drawn_quiet.set(quiet.then_some(size));
                    }
                } else {
                    self.drawn_quiet.set(None);
                }
            } else {
                obj.snapshot_child(self.list(), snapshot);
                obj.snapshot_child(self.reader(), snapshot);
            }
        }
    }
}

glib::wrapper! {
    /// The list and the reader: side by side, or one shown and the reader possibly warm behind.
    pub struct MailPanes(ObjectSubclass<imp::MailPanes>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MailPanes {
    /// `list` beside `reader`, the list `list_width` px wide.
    pub fn new(
        list: &impl IsA<gtk::Widget>,
        reader: &impl IsA<gtk::Widget>,
        list_width: i32,
    ) -> Self {
        let panes: Self = glib::Object::new();
        panes.set_overflow(gtk::Overflow::Hidden);
        let imp = panes.imp();
        imp.list_width.set(list_width);
        // The reader first: the list draws over it and takes the pointer first wherever they
        // overlap, and Tab goes left to right whatever the order.
        reader.set_parent(&panes);
        list.set_parent(&panes);
        let _ = imp.reader.set(reader.as_ref().clone());
        let _ = imp.list.set(list.as_ref().clone());
        panes.apply();
        panes
    }

    /// One pane or two; with one, whether the reader is the one shown, and whether a reader
    /// behind the list stays warm.
    pub fn set_layout(&self, one_pane: bool, reader_front: bool, warm: bool) {
        let imp = self.imp();
        let changed = [
            imp.one_pane.replace(one_pane) != one_pane,
            imp.reader_front.replace(reader_front) != reader_front,
            imp.warm.replace(warm) != warm,
        ];
        if changed.contains(&true) {
            self.apply();
        }
    }

    pub fn is_one_pane(&self) -> bool {
        self.imp().one_pane.get()
    }

    /// The list shows: beside the reader, or alone.
    pub fn shows_list(&self) -> bool {
        let imp = self.imp();
        !imp.one_pane.get() || !imp.reader_front.get()
    }

    /// The reader shows: beside the list, or alone. A warm reader behind the list doesn't.
    pub fn shows_reader(&self) -> bool {
        let imp = self.imp();
        !imp.one_pane.get() || imp.reader_front.get()
    }

    /// The reader is mapped and laid out behind the list, unseen.
    pub fn reader_is_warm(&self) -> bool {
        self.imp().reader_behind()
    }

    /// Tells whether the warm reader has nothing left to paint: behind the list it is then
    /// drawn once more and no longer, while it stays quiet at its size.
    pub fn set_reader_quiet(&self, quiet: impl Fn() -> bool + 'static) {
        self.imp().quiet.replace(Some(Box::new(quiet)));
    }

    /// The warm reader drawn behind the list so far, and left undrawn because it was quiet:
    /// for checks.
    pub fn reader_draws(&self) -> (u64, u64) {
        let imp = self.imp();
        (imp.draws.get(), imp.skips.get())
    }

    pub fn set_list_width(&self, width: i32) {
        if self.imp().list_width.replace(width) != width {
            self.queue_resize();
        }
    }

    /// Maps what shows and the warm reader, unmaps the rest; only what shows is reachable.
    fn apply(&self) {
        let imp = self.imp();
        let (Some(list), Some(reader)) = (imp.list.get(), imp.reader.get()) else {
            return;
        };
        let (list_shown, reader_shown) = (self.shows_list(), self.shows_reader());
        list.set_child_visible(list_shown);
        reader.set_child_visible(reader_shown || imp.reader_behind());
        reader.set_can_target(reader_shown);
        reader.set_can_focus(reader_shown);
        reader.update_state(&[gtk::accessible::State::Hidden(!reader_shown)]);
        list.update_state(&[gtk::accessible::State::Hidden(!list_shown)]);
        // No opacity 0 for a warm reader: GTK draws nothing under a transparent widget, not
        // even into the empty clip `snapshot` gives it (`Window::is_behind_list` tells checks).
        self.queue_resize();
    }
}
