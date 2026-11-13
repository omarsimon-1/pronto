//! The Split Inbox's tabs in the list's header row (the fork's phase C, docs/superhuman/split.md
//! "UI"): Important and Other in the view name's place, plain words at its size and regular
//! weight, the current one in the text's colour and the other in the secondary, each with its
//! total after it, smaller and quieter, and nothing when it is zero (Superhuman's: no
//! underline, pill, icon, box or colour; zero is the goal). A click picks one; a screen reader
//! hears a tab list ("Other, 4 conversations", selected).
//!
//! The widget lays its tabs out itself, with the filter's pill after them (a filter narrows the
//! side shown), so the header never clips them: where the row can't hold the totals too
//! (Split's 380 px list with the Unread pill: phase B's visual QA), the gap between the tabs
//! narrows first, then the totals go (each tab's accessible name still says it), and the pill
//! stays right after the tabs. A tab spans its total, so a click on the number picks it.
//!
//! Whose Inbox it is goes in the tab list's accessible name ("Inbox, Personal", "All
//! Inboxes"), and, with several accounts, after the tabs as the view's name's subtitle did
//! ("· Personal"): the tabs take the name's place, and with the sidebar hidden nothing else
//! says it. It is the first to go where the row is short.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use sealwax_domain::split::Split;
use sealwax_infrastructure::store::SplitCounts;
use std::cell::RefCell;
use std::rc::Rc;

/// Between a tab's name and its total (Superhuman: 4 to 6 px).
const COUNT_GAP: i32 = 6;
/// Between the tabs (Superhuman: 24 to 32 px), and the least it narrows to before the totals
/// go.
const TAB_GAP: i32 = 24;
const TIGHT_TAB_GAP: i32 = 14;
/// Before the place and the filter's pill: the header's spacing after the view's name
/// (`.sw-pane-heading`).
const HEADING_GAP: i32 = 10;

/// The sides in their order.
pub const SIDES: [Split; 2] = [Split::Important, Split::Other];

/// A side's name.
pub fn side_name(side: Split) -> &'static str {
    match side {
        Split::Important => "Important",
        Split::Other => "Other",
    }
}

type Picked = Rc<dyn Fn(Split)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SplitTabs {
        /// Each side's tab (its name) and its total, in [`SIDES`]' order.
        pub tabs: RefCell<Vec<(gtk::Button, gtk::Label)>>,
        /// Whose Inbox, after the tabs while it shows ("· Personal").
        pub place: RefCell<Option<gtk::Label>>,
        /// The filter's pill, after the tabs while it shows.
        pub pill: RefCell<Option<gtk::Widget>>,
        pub picked: RefCell<Vec<Picked>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SplitTabs {
        const NAME: &'static str = "ProntoSplitTabs";
        type Type = super::SplitTabs;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("splittabs");
            klass.set_accessible_role(gtk::AccessibleRole::TabList);
        }
    }

    impl ObjectImpl for SplitTabs {
        fn dispose(&self) {
            for (tab, count) in self.tabs.take() {
                tab.unparent();
                count.unparent();
            }
            if let Some(place) = self.place.take() {
                place.unparent();
            }
            if let Some(pill) = self.pill.take() {
                pill.unparent();
            }
        }
    }

    impl SplitTabs {
        /// Each tab's name's and total's natural widths; 0 for a total that doesn't show.
        fn widths(&self) -> Vec<(i32, i32)> {
            self.tabs
                .borrow()
                .iter()
                .map(|(tab, count)| {
                    let (_, name, ..) = tab.measure(gtk::Orientation::Horizontal, -1);
                    let total = if count.is_visible() {
                        count.measure(gtk::Orientation::Horizontal, -1).1
                    } else {
                        0
                    };
                    (name, total)
                })
                .collect()
        }

        /// The pill's natural width and the gap before it, while it shows.
        fn pill_width(&self) -> i32 {
            self.pill
                .borrow()
                .as_ref()
                .filter(|pill| pill.is_visible())
                .map_or(0, |pill| {
                    HEADING_GAP + pill.measure(gtk::Orientation::Horizontal, -1).1
                })
        }

        /// The place's natural width and the gap before it, while it shows.
        fn place_width(&self) -> i32 {
            self.place
                .borrow()
                .as_ref()
                .filter(|place| place.is_visible())
                .map_or(0, |place| {
                    HEADING_GAP + place.measure(gtk::Orientation::Horizontal, -1).1
                })
        }

        /// The tallest child's parts above and below the shared baseline.
        fn ascent_descent(&self) -> (i32, i32) {
            let mut ascent = 0;
            let mut descent = 0;
            let pill = self.pill.borrow().clone();
            let place = self
                .place
                .borrow()
                .clone()
                .map(|place| place.upcast::<gtk::Widget>());
            for (tab, count) in self.tabs.borrow().iter() {
                for child in [tab.upcast_ref::<gtk::Widget>(), count.upcast_ref()]
                    .into_iter()
                    .chain(pill.as_ref())
                    .chain(place.as_ref())
                {
                    if !child.is_visible() {
                        continue;
                    }
                    let (_, height, _, baseline) = child.measure(gtk::Orientation::Vertical, -1);
                    let baseline = if baseline < 0 { height / 2 } else { baseline };
                    ascent = ascent.max(baseline);
                    descent = descent.max(height - baseline);
                }
            }
            (ascent, descent)
        }
    }

    impl WidgetImpl for SplitTabs {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Vertical {
                let (ascent, descent) = self.ascent_descent();
                return (ascent + descent, ascent + descent, ascent, ascent);
            }
            let widths = self.widths();
            let gaps = widths.len().saturating_sub(1) as i32;
            let names: i32 = widths.iter().map(|(name, _)| name).sum();
            let totals: i32 = widths
                .iter()
                .filter(|(_, total)| *total > 0)
                .map(|(_, total)| COUNT_GAP + total)
                .sum();
            let pill = self.pill_width();
            (
                names + gaps * TIGHT_TAB_GAP + pill,
                names + totals + gaps * TAB_GAP + self.place_width() + pill,
                -1,
                -1,
            )
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let widths = self.widths();
            let gaps = widths.len().saturating_sub(1) as i32;
            let names: i32 = widths.iter().map(|(name, _)| name).sum();
            let totals: i32 = widths
                .iter()
                .filter(|(_, total)| *total > 0)
                .map(|(_, total)| COUNT_GAP + total)
                .sum();
            let room = width - self.pill_width();
            // The place goes first, then the gap narrows, then the totals go.
            let place_width = self.place_width();
            let with_place =
                place_width > 0 && room >= names + totals + gaps * TAB_GAP + place_width;
            let room = if with_place { room - place_width } else { room };
            let with_totals = room >= names + totals + gaps * TAB_GAP;
            let gap = if with_totals || gaps == 0 {
                TAB_GAP
            } else {
                ((room - names) / gaps).clamp(TIGHT_TAB_GAP, TAB_GAP)
            };
            let (ascent, descent) = self.ascent_descent();
            // The row's baseline: the parent's, else the tallest child's, centred.
            let baseline = if baseline >= 0 {
                baseline
            } else {
                (height - ascent - descent) / 2 + ascent
            };
            let rtl = self.obj().direction() == gtk::TextDirection::Rtl;
            let place = |child: &gtk::Widget, x: i32, w: i32| {
                let (_, h, _, child_baseline) = child.measure(gtk::Orientation::Vertical, w);
                let y = if child_baseline >= 0 {
                    baseline - child_baseline
                } else {
                    (height - h) / 2
                };
                let x = if rtl { width - x - w } else { x };
                child.size_allocate(&gtk::Allocation::new(x, y, w, h), -1);
            };
            let mut x = 0;
            for (index, ((tab, count), (name, total))) in
                self.tabs.borrow().iter().zip(widths).enumerate()
            {
                if index > 0 {
                    x += gap;
                }
                let shows = with_totals && total > 0;
                let span = if shows {
                    name + COUNT_GAP + total
                } else {
                    name
                };
                place(tab.upcast_ref(), x, span);
                count.set_child_visible(shows);
                if shows {
                    place(count.upcast_ref(), x + name + COUNT_GAP, total);
                }
                x += span;
            }
            if let Some(label) = self.place.borrow().as_ref() {
                label.set_child_visible(with_place);
                if with_place {
                    place(
                        label.upcast_ref(),
                        x + HEADING_GAP,
                        place_width - HEADING_GAP,
                    );
                    x += place_width;
                }
            }
            if let Some(pill) = self.pill.borrow().as_ref().filter(|pill| pill.is_visible()) {
                let (_, natural, ..) = pill.measure(gtk::Orientation::Horizontal, -1);
                place(pill, x + HEADING_GAP, natural);
            }
        }
    }
}

glib::wrapper! {
    pub struct SplitTabs(ObjectSubclass<imp::SplitTabs>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for SplitTabs {
    fn default() -> Self {
        Self::new()
    }
}

impl SplitTabs {
    pub fn new() -> Self {
        let this: Self = glib::Object::builder()
            .property("valign", gtk::Align::BaselineCenter)
            .property("visible", false)
            .build();
        this.add_css_class("sw-split-tabs");
        this.set_widget_name("list-split-tabs");
        this.update_property(&[gtk::accessible::Property::Label("Split Inbox")]);
        let mut tabs = Vec::new();
        for side in SIDES {
            let name = gtk::Label::builder()
                .label(side_name(side))
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::BaselineCenter)
                .build();
            let tab = gtk::Button::builder()
                .child(&name)
                .accessible_role(gtk::AccessibleRole::Tab)
                .valign(gtk::Align::BaselineCenter)
                .focus_on_click(false)
                .build();
            tab.add_css_class("sw-split-tab");
            tab.set_widget_name(&format!("list-split-{}", side_name(side).to_lowercase()));
            let weak = this.downgrade();
            tab.connect_clicked(move |_| {
                if let Some(tabs) = weak.upgrade() {
                    let picked = tabs.imp().picked.borrow().clone();
                    for handler in picked {
                        handler(side);
                    }
                }
            });
            // Its words are the tab's accessible name's; a click on it is the tab's.
            let count = gtk::Label::builder()
                .can_target(false)
                .visible(false)
                .valign(gtk::Align::BaselineCenter)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build();
            count.add_css_class("sw-split-count");
            count.set_widget_name(&format!(
                "list-split-{}-count",
                side_name(side).to_lowercase()
            ));
            tab.set_parent(&this);
            count.set_parent(&this);
            tabs.push((tab, count));
        }
        this.imp().tabs.replace(tabs);
        // The view name's subtitle's look; its words are the tab list's name's.
        let place = gtk::Label::builder()
            .visible(false)
            .valign(gtk::Align::BaselineCenter)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        place.add_css_class("sw-pane-subtitle");
        place.set_widget_name("list-split-place");
        place.set_parent(&this);
        this.imp().place.replace(Some(place));
        this
    }

    /// Whose Inbox the tabs are: `name` for a screen reader ("Inbox, Personal"), and `shown`
    /// after the tabs ("Personal", with several accounts), or nothing.
    pub fn set_place(&self, name: &str, shown: Option<&str>) {
        self.update_property(&[gtk::accessible::Property::Label(name)]);
        if let Some(label) = self.imp().place.borrow().as_ref() {
            let text = shown.map(|shown| format!("· {shown}")).unwrap_or_default();
            if label.label() != text {
                label.set_label(&text);
                label.set_visible(!text.is_empty());
                self.queue_resize();
            }
        }
    }

    /// Shows `current` as the side shown, each side with its total from `counts` (rows of the
    /// list: `unit` is "conversation" or "message"); totals unknown yet show nothing.
    pub fn set(&self, current: Split, counts: Option<SplitCounts>, unit: (&str, &str)) {
        for ((tab, count), side) in self.imp().tabs.borrow().iter().zip(SIDES) {
            let total = counts.map(|counts| match side {
                Split::Important => counts.important,
                Split::Other => counts.other,
            });
            let text = match total {
                Some(total) if total > 0 => total.to_string(),
                _ => String::new(),
            };
            if count.label() != text {
                count.set_label(&text);
            }
            count.set_visible(!text.is_empty());
            let chosen = side == current;
            if chosen {
                tab.add_css_class("sw-current");
            } else {
                tab.remove_css_class("sw-current");
            }
            let spoken = match total {
                Some(0) => format!("{}, no {}", side_name(side), unit.1),
                Some(1) => format!("{}, 1 {}", side_name(side), unit.0),
                Some(total) => format!("{}, {total} {}", side_name(side), unit.1),
                None => side_name(side).to_owned(),
            };
            tab.update_property(&[gtk::accessible::Property::Label(&spoken)]);
            tab.update_state(&[gtk::accessible::State::Selected(Some(chosen))]);
        }
        self.queue_resize();
    }

    /// `pill` (the filter's) goes after the tabs while it shows.
    pub fn set_pill(&self, pill: &impl IsA<gtk::Widget>) {
        pill.set_parent(self);
        self.imp().pill.replace(Some(pill.clone().upcast()));
    }

    /// Runs `handler` with the side a click picked.
    pub fn connect_picked(&self, handler: impl Fn(Split) + 'static) {
        self.imp().picked.borrow_mut().push(Rc::new(handler));
    }

    /// Whether the totals show (they go where the header can't hold them), for the self-test.
    pub fn shows_totals(&self) -> bool {
        self.imp()
            .tabs
            .borrow()
            .iter()
            .any(|(_, count)| count.is_visible() && count.is_child_visible())
    }
}
