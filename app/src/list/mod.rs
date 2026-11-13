//! The message list (ML-026, DESIGN.md "Message list"): a `gtk::ListView` of the store's view of
//! a folder or the unified inbox, in date sections, or of search results, filtered (All,
//! Unread, Starred, Attachments: keys and the palette set the filter, the window's bar shows it).
//! Since the Superhuman pass (2026-10-28) the search field shows only while a search is typed
//! or runs (Search: `/`, Ctrl+F, the bar's button), the filter chips are gone, and an empty
//! Inbox or All Inboxes shows Inbox Zero.
//!
//! - The model ([`model`]) pages lazily and applies the store's change events in version order.
//! - New mail keeps the list at its top while the first row is in view (S3).
//! - Rows show their placeholder look while the list moves faster than ~30,000 px/s (S3).
//! - After every reset the list shows [`FIRST_ROWS`] rows, then grows by [`GROW_STEP`] a frame
//!   (S6's first-frame trim).
//! - Sections regroup when the day changes.
//! - The List scope's keys move the selection (`j`, `k`; GtkListView's Down and Up) and open it
//!   (Enter, `o`). Several messages can be selected for batch actions (ML-054): Ctrl+click
//!   toggles a row, Shift+click and Shift with the arrows or `j`/`k` extend, and Select (`x`,
//!   Space) enters a select mode whose rows show checkboxes, where `j`/`k` and the arrows move a
//!   cursor and Escape leaves.
//! - Search (ML-082): results follow the field as it is typed ([`SEARCH_DELAY`]); the
//!   field suggests operators, values, days and people ([`suggestions`]). In a folder a search
//!   covers that folder, with a scope row to search every folder instead; in the unified inbox
//!   it covers every folder.
//! - Rows are messages or, since the Superhuman pass, conversations ([`MessageList::set_grouping`],
//!   docs/superhuman/conversations.md). The selection, the cursor and what an action leaves
//!   behind are kept by row key, so a conversation a reply moves stays selected and the reader
//!   showing it isn't asked again.
//! - The Inbox views show one side of the Split Inbox at a time (phase C, [`split`]).

pub mod item;
pub mod model;
pub mod row;
pub mod sections;
mod split;
pub mod suggestions;

pub use model::{
    Change, Content, FIRST_ROWS, GROW_STEP, MOVE_SPAN, MessageModel, ModelStats, SEARCH_PAGE,
};

use crate::actions::{self, ScopedShortcuts};
use crate::mailbox::{Latch, Mailbox};
use crate::triage::Target;
use crate::window::empty;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use item::MessageItem;
pub use row::Quick;
use row::{MessageRow, RowContext, RowReminders, SectionHeader};
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_domain::search::Date;
use sealwax_infrastructure::store::{
    ChangeEvent, ListRow, RowKey, ViewFilter, ViewGrouping, ViewScope, ViewSpec,
};
use sections::{Thresholds, until_next_day};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};
use suggestions::Suggestions;

/// Faster than this (px/s), rows skip their text (S3: a scrollbar drag through 100k rows then
/// holds 144 Hz).
pub const SKELETON_SPEED: f64 = 30_000.0;
/// Frames slower than the skeleton speed before rows show their text again.
const SLOW_FRAMES: u32 = 2;
/// Frames without movement before the speed watch stops.
const STILL_FRAMES: u32 = 8;
/// Within this many pixels of the top, new mail keeps the list at its top (S3).
const PIN_SLACK: f64 = 48.0;
/// The day is checked at least this often, so sections regroup soon after a suspend over
/// midnight too.
const DAY_CHECK: Duration = Duration::from_secs(600);
/// How long the list waits for the messages of an action to leave it.
const LEAVING_WAIT: Duration = Duration::from_secs(10);
/// How long an action waits for selected rows that aren't loaded yet.
const TARGETS_WAIT: Duration = Duration::from_secs(3);
/// The most selected rows a change keeps selected when they move, or carries to a filter or a
/// grouping.
const KEPT_SELECTED: usize = 64;
/// How long after the last key in the search field the search runs (GtkSearchEntry's
/// `search-delay`; GTK's default and M1's was 150 ms). None: at 100,000 messages a key's
/// results were painted within 63 ms p95 from a folder and 88 ms from All Inboxes (M7,
/// docs/m7/search-ui.md).
pub const SEARCH_DELAY: Duration = Duration::ZERO;
/// The search field's placeholder, the mock's.
pub const SEARCH_PLACEHOLDER: &str = "Search — from: has:attachment is:unread";
/// The search field's width in the header row, in characters: at least, and at most (about
/// 520 px at the default text size). At least 4, so Split's 380 px header holds the field,
/// the scope and a filter's pill without the pane growing (phase C).
pub const SEARCH_CHARS: (i32, i32) = (4, 52);
/// The search scope that widens it to every folder ([`MessageList::set_everywhere`]); "All"
/// while a filter's pill shows beside it.
const SCOPE_ALL: &str = "All folders";

pub const FILTERS: [(ViewFilter, &str); 4] = [
    (ViewFilter::All, "All"),
    (ViewFilter::Unread, "Unread"),
    (ViewFilter::Starred, "Starred"),
    (ViewFilter::Attachments, "Attachments"),
];

pub fn filter_name(filter: ViewFilter) -> &'static str {
    // A side of the Split Inbox as a filter (the backend's) is named as its tab is, never "All".
    if let Some(side) = filter.split() {
        return crate::window::split_tabs::side_name(side);
    }
    FILTERS
        .iter()
        .find(|(candidate, _)| *candidate == filter)
        .map_or("All", |(_, name)| name)
}

/// The list's title while it shows search results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHeading {
    pub results: u32,
    /// More results can be fetched.
    pub more: bool,
    pub searching: bool,
    /// The list shows a folder and the search covers every folder (the scope row's All
    /// folders).
    pub everywhere: bool,
}

impl SearchHeading {
    pub fn subtitle(&self) -> String {
        let counted = match (self.searching && self.results == 0, self.results, self.more) {
            (true, _, _) if self.everywhere => return "Searching all folders…".to_owned(),
            (true, _, _) => return "Searching…".to_owned(),
            (_, 0, _) => "No results".to_owned(),
            (_, 1, false) => "1 result".to_owned(),
            (_, count, false) => format!("{count} results"),
            (_, count, true) => format!("{count}+ results"),
        };
        if self.everywhere {
            format!("{counted} in all folders")
        } else {
            counted
        }
    }
}

/// A bound row as the self-test and the perf probe see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundRow {
    pub position: Option<u32>,
    /// The message the reader shows for the row (a conversation's anchor).
    pub id: Option<MessageId>,
    pub key: Option<RowKey>,
    pub placeholder: bool,
}

type IdHandler = Rc<dyn Fn(MessageId)>;
type Handler = Rc<dyn Fn()>;
type SearchHandler = Rc<dyn Fn(Option<&SearchHeading>)>;
type CountHandler = Rc<dyn Fn(u32)>;
type QuickHandler = Rc<dyn Fn(ActionId, ListRow)>;
type MenuHandler = Rc<dyn Fn(gdk::Rectangle)>;
type FilterHandler = Rc<dyn Fn(ViewFilter)>;
type FlagHandler = Rc<dyn Fn(bool)>;

#[derive(Default)]
struct Handlers {
    selected: RefCell<Vec<IdHandler>>,
    opened: RefCell<Vec<IdHandler>>,
    cleared: RefCell<Vec<Handler>>,
    batch: RefCell<Vec<CountHandler>>,
    search: RefCell<Vec<SearchHandler>>,
    older: RefCell<Vec<FolderHandler>>,
    quick: RefCell<Vec<QuickHandler>>,
    menu: RefCell<Vec<MenuHandler>>,
    filter: RefCell<Vec<FilterHandler>>,
    empty: RefCell<Vec<FlagHandler>>,
    zero: RefCell<Vec<FlagHandler>>,
    searching: RefCell<Vec<FlagHandler>>,
    reset: RefCell<Vec<Handler>>,
}

type FolderHandler = Rc<dyn Fn(FolderId)>;

/// What the list last reported about its selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selected {
    Nothing,
    /// One row, outside the select mode: the reader shows this message, the row's own or its
    /// conversation's anchor (the first unread, else the latest; the match, in search results).
    /// The row is reported again only when another row is selected: a conversation that gains
    /// a message keeps the reader on what it shows.
    One(MessageId),
    /// The select mode, with this many rows selected.
    Batch(u32),
}

/// Rows an action is taking out of the list. Once one is gone and nothing is selected, the row
/// that took the first one's place is selected: the next message or conversation, as Gmail
/// does.
struct Leaving {
    keys: HashSet<RowKey>,
    /// The newest member in the view ([`newest_member`]) of each of those rows that was loaded.
    newest: HashMap<RowKey, MessageId>,
    position: u32,
    since: Instant,
}

/// What a row held of the view when an action took it out, which an Undo puts back as it was:
/// a message row's message, a conversation's newest member in the view's folders. A reply to a
/// conversation archived brings the row back with another newest member.
fn newest_member(row: &ListRow) -> MessageId {
    match row {
        ListRow::Message(message) => message.id,
        ListRow::Conversation(conversation) => conversation.last_member,
    }
}

/// Older mail of a folder whose first sync kept only its newest messages (ML-043).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Older {
    /// Asked for; "Loading older messages…" shows.
    Asked,
    /// Nothing came and more may be there (offline): asked again once the folder is shown again.
    Paused,
    /// The server has nothing older.
    Complete,
}

#[derive(Clone)]
pub struct MessageList {
    inner: Rc<Inner>,
}

struct Inner {
    mailbox: Rc<Mailbox>,
    root: gtk::Box,
    search: gtk::SearchEntry,
    suggestions: Suggestions,
    /// The Search command's key at the field's end while it is empty (the mock's `/`).
    search_key: gtk::Label,
    /// The search field while a search is typed or runs, and the scope after it: the window
    /// puts it in the list's header row, in the view name's place (phase B, the visual QA).
    head: gtk::Box,
    search_row: gtk::Widget,
    /// Search was asked for (`/`): the field shows until it is left empty.
    search_open: Cell<bool>,
    /// Under the field while a search runs from a folder: that folder, or every folder.
    scope_row: gtk::Box,
    scope_folder: gtk::ToggleButton,
    scope_all: gtk::ToggleButton,
    /// The search covers every folder although the list shows one.
    everywhere: Cell<bool>,
    /// What the search field last said of the results, as its accessible description.
    search_described: RefCell<Option<String>>,
    /// Enter in the search field before its results came: the first is selected once they do.
    select_first: Cell<bool>,
    /// `list`, `empty` or `zero` (Inbox Zero).
    stack: gtk::Stack,
    empty: empty::EmptyState,
    zero: empty::InboxZero,
    /// What the list last told about being empty, showing Inbox Zero and searching.
    told_empty: Cell<Option<bool>>,
    told_zero: Cell<Option<bool>>,
    told_searching: Cell<Option<bool>>,
    scroller: gtk::ScrolledWindow,
    view: gtk::ListView,
    /// The date sections' headers, shown in Split only ([`MessageList::set_focused`]).
    headers: gtk::SignalListItemFactory,
    model: MessageModel,
    selection: gtk::MultiSelection,
    context: Rc<RowContext>,
    scope: Cell<Option<ViewScope>>,
    filter: Cell<ViewFilter>,
    /// One row per message or per conversation (Group by Conversation).
    grouping: Cell<ViewGrouping>,
    query: RefCell<String>,
    /// Typing into the search field while the list changes it itself.
    quiet: Cell<bool>,
    /// A fixed "now" (Unix seconds) for the self-test; the real clock otherwise.
    clock: Cell<Option<i64>>,
    content: RefCell<Content>,
    growth: RefCell<Option<gtk::TickCallbackId>>,
    growth_frames: RefCell<Vec<u32>>,
    speed: RefCell<Option<gtk::TickCallbackId>>,
    /// What the list last reported as selected, and that row's key.
    reported: Cell<Selected>,
    reported_key: Cell<Option<RowKey>>,
    /// That row's own message as it was reported (a conversation's anchor): one that moves
    /// later is new mail in it ([`MessageList::report_new_unread`]).
    reported_anchor: Cell<Option<MessageId>>,
    /// The message a row opens at instead of its own (a notification's new message), while
    /// that row is being selected.
    open_at: Cell<Option<(RowKey, MessageId)>>,
    /// Rows to select again once they load, after a reload, until `wanted_until`.
    wanted: RefCell<Vec<RowKey>>,
    wanted_until: Cell<Option<Instant>>,
    /// The selected rows and the select mode just before a reset.
    restoring: RefCell<(Vec<RowKey>, bool)>,
    /// Rows to select once the next content's rows are in: the selection a filter or the
    /// grouping took over (by key, in the new grouping).
    carried: RefCell<Vec<RowKey>>,
    /// The selected rows when the selection last settled.
    settled: RefCell<Vec<RowKey>>,
    /// Rows show checkboxes, and `j`/`k` and the arrows move the cursor, not the selection.
    select_mode: Cell<bool>,
    /// A row is always under the cursor (selected) once the list has rows: Focused mode, where
    /// selecting only lays the conversation out behind the list (the Superhuman pass).
    keeps_cursor: Cell<bool>,
    /// Focused mode: a click opens the row, and no date headers show.
    focused: Cell<bool>,
    /// Where the one selected row last settled, for the row that takes its place.
    settled_at: Cell<Option<u32>>,
    /// The row the select mode's cursor is on, when the list hasn't got the keyboard focus.
    cursor: Cell<Option<RowKey>>,
    /// A click without Ctrl or Shift selects one row and leaves the select mode.
    plain_click: Cell<bool>,
    /// The row a plain click pressed, which its release opens in Focused mode.
    pressed: Cell<Option<u32>>,
    /// The primary button is down on the list, between its press and its release: the
    /// keyboard focus the press gave its row stays there until the release selects that row
    /// (phase B's review: following the selection then scrolled the list under a person's
    /// click, and the release landed on another row).
    pointer_down: Cell<bool>,
    /// A Ctrl+arrow (or Page Up, Page Down, Home, End) is being handled: GtkListView moves its
    /// own focus row apart from the selection on purpose, and the focus stays there.
    cursor_keys: Cell<bool>,
    /// The clicks that opened a row in Focused mode, whose double click's second press would
    /// land on the reader now in the list's place ([`MessageList::swallow_double_clicks`]).
    double_clicks: RefCell<DoubleClicks>,
    leaving: RefCell<Option<Leaving>>,
    /// The last rows an action took out, when they left the list empty, with their newest
    /// members: back as they left (an Undo from Inbox Zero), they are selected again.
    returning: RefCell<HashMap<RowKey, MessageId>>,
    /// Next and Previous Message's one-key keys, which extend the selection with Shift.
    extend_keys: RefCell<Vec<(gdk::Key, i64)>>,
    /// Where the list was just before its last reset, and whether it had the keyboard focus.
    place: Cell<f64>,
    refocus: Cell<bool>,
    settle_queued: Cell<bool>,
    waiting: RefCell<Option<(MessageItem, glib::SignalHandlerId)>>,
    sweep_queued: Cell<bool>,
    handlers: Handlers,
    keys: RefCell<Option<ScopedShortcuts>>,
    first_rows: RefCell<Option<Rc<Latch>>>,
    day_check: RefCell<Option<glib::SourceId>>,
    /// The quiet row under the list while older mail loads.
    older_row: gtk::Label,
    older: RefCell<HashMap<FolderId, Older>>,
    /// The Split Inbox: the side shown, each side's place, the tabs' totals ([`split`]).
    splits: split::Splits,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(source) = self.day_check.take() {
            source.remove();
        }
    }
}

impl MessageList {
    pub fn new(mailbox: Rc<Mailbox>) -> Self {
        // In the header row: as wide as about 52 characters (the visual QA's 480–560 px), and
        // never narrower than 8, so a narrow pane's header holds it without growing.
        let search = gtk::SearchEntry::builder()
            .placeholder_text(SEARCH_PLACEHOLDER)
            .search_delay(SEARCH_DELAY.as_millis() as u32)
            .width_chars(SEARCH_CHARS.0)
            .max_width_chars(SEARCH_CHARS.1)
            .build();
        search.add_css_class("sw-search");
        search.set_widget_name("list-search");
        search.update_property(&[gtk::accessible::Property::Label("Search mail")]);
        let search_key = gtk::Label::builder()
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .can_target(false)
            .visible(false)
            .build();
        search_key.add_css_class("sw-search-key");
        search_key.set_widget_name("list-search-key");
        let field = gtk::Overlay::builder()
            .child(&search)
            .visible(false)
            .build();
        field.add_overlay(&search_key);
        field.set_widget_name("list-search-row");
        let suggestions = Suggestions::new(&search, mailbox.clone());
        // Spacing from style.css alone: GTK adds a box's border-spacing to its own spacing.
        let head = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        head.add_css_class("sw-list-head");
        head.set_widget_name("list-search-bar");
        head.append(&field);
        let (scope_row, scope_folder, scope_all) = scope_row();
        head.append(&scope_row);

        let model = MessageModel::new(mailbox.clone());
        let selection = gtk::MultiSelection::new(Some(model.clone()));
        let context = RowContext::new();
        let factory = gtk::SignalListItemFactory::new();
        let row_context = context.clone();
        factory.connect_setup(move |_, object| {
            if let Some(list_item) = object.downcast_ref::<gtk::ListItem>() {
                list_item.set_child(Some(&MessageRow::new(row_context.clone())));
            }
        });
        factory.connect_bind(|_, object| {
            let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if let (Some(row), Some(item)) = (
                list_item.child().and_downcast::<MessageRow>(),
                list_item.item().and_downcast::<MessageItem>(),
            ) {
                row.bind(list_item, &item);
            }
        });
        let unbind = |_: &gtk::SignalListItemFactory, object: &glib::Object| {
            if let Some(row) = object
                .downcast_ref::<gtk::ListItem>()
                .and_then(|list_item| list_item.child().and_downcast::<MessageRow>())
            {
                row.unbind();
            }
        };
        factory.connect_unbind(unbind);
        factory.connect_teardown(unbind);
        let headers = gtk::SignalListItemFactory::new();
        headers.connect_setup(|_, object| {
            if let Some(header) = object.downcast_ref::<gtk::ListHeader>() {
                header.set_child(Some(&SectionHeader::new()));
            }
        });
        let header_context = context.clone();
        headers.connect_bind(move |_, object| {
            let Some(header) = object.downcast_ref::<gtk::ListHeader>() else {
                return;
            };
            if let (Some(widget), Some(item)) = (
                header.child().and_downcast::<SectionHeader>(),
                header.item().and_downcast::<MessageItem>(),
            ) {
                widget.bind(&item, &header_context);
            }
        });
        let header_context = context.clone();
        let unbind_header = move |_: &gtk::SignalListItemFactory, object: &glib::Object| {
            if let Some(widget) = object
                .downcast_ref::<gtk::ListHeader>()
                .and_then(|header| header.child().and_downcast::<SectionHeader>())
            {
                widget.unbind(&header_context);
            }
        };
        headers.connect_unbind(unbind_header.clone());
        // Taking the factory away (Focused) tears the headers down without unbinding them.
        headers.connect_teardown(unbind_header);
        let view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .header_factory(&headers)
            // One Tab stop, as the folder list: Up and Down move between messages, and Tab goes
            // on to the reader instead of through every row on screen.
            .tab_behavior(gtk::ListTabBehavior::Item)
            .build();
        view.add_css_class("sw-message-list");
        view.set_widget_name("message-list");
        view.update_property(&[gtk::accessible::Property::Label("Messages")]);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let empty = empty::folder("No messages", "Messages in this folder appear here.");
        empty.widget().set_widget_name("list-empty");
        empty.set_name("list-empty-state");
        let zero = empty::inbox_zero();
        zero.widget().set_widget_name("inbox-zero");
        let stack = gtk::Stack::builder().vexpand(true).build();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(empty.widget(), Some("empty"));
        stack.add_named(zero.widget(), Some("zero"));
        stack.set_visible_child_name("list");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("sw-message-list");
        root.add_css_class("sw-message-list-pane");
        root.set_widget_name("message-list-pane");
        root.append(&stack);
        let older_row = gtk::Label::builder()
            .label("Loading older messages…")
            .visible(false)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        older_row.add_css_class("dim-label");
        older_row.add_css_class("caption");
        older_row.add_css_class("sw-list-older");
        older_row.set_widget_name("list-older");
        root.append(&older_row);
        // The sender column's widest width, measured from CSS for every row (RowContext).
        let ruler = gtk::Box::builder()
            .can_target(false)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        ruler.add_css_class("sw-row-ruler");
        root.append(&ruler);
        context.set_ruler(&ruler);

        let list = Self {
            inner: Rc::new(Inner {
                mailbox,
                root,
                search,
                suggestions,
                search_key,
                head,
                search_row: field.upcast(),
                search_open: Cell::new(false),
                scope_row,
                scope_folder,
                scope_all,
                everywhere: Cell::new(false),
                search_described: RefCell::new(None),
                select_first: Cell::new(false),
                stack,
                empty,
                zero,
                told_empty: Cell::new(None),
                told_zero: Cell::new(None),
                told_searching: Cell::new(None),
                scroller,
                view,
                headers,
                model,
                selection,
                context,
                scope: Cell::new(None),
                filter: Cell::new(ViewFilter::All),
                grouping: Cell::new(ViewGrouping::Messages),
                query: RefCell::new(String::new()),
                quiet: Cell::new(false),
                clock: Cell::new(None),
                content: RefCell::new(Content::Nothing),
                growth: RefCell::new(None),
                growth_frames: RefCell::new(Vec::new()),
                speed: RefCell::new(None),
                reported: Cell::new(Selected::Nothing),
                reported_key: Cell::new(None),
                open_at: Cell::new(None),
                reported_anchor: Cell::new(None),
                wanted: RefCell::new(Vec::new()),
                wanted_until: Cell::new(None),
                restoring: RefCell::new((Vec::new(), false)),
                carried: RefCell::new(Vec::new()),
                settled: RefCell::new(Vec::new()),
                select_mode: Cell::new(false),
                keeps_cursor: Cell::new(false),
                focused: Cell::new(false),
                settled_at: Cell::new(None),
                cursor: Cell::new(None),
                plain_click: Cell::new(false),
                pressed: Cell::new(None),
                pointer_down: Cell::new(false),
                cursor_keys: Cell::new(false),
                double_clicks: RefCell::new(DoubleClicks::default()),
                leaving: RefCell::new(None),
                returning: RefCell::new(HashMap::new()),
                extend_keys: RefCell::new(Vec::new()),
                place: Cell::new(0.0),
                refocus: Cell::new(false),
                settle_queued: Cell::new(false),
                waiting: RefCell::new(None),
                sweep_queued: Cell::new(false),
                handlers: Handlers::default(),
                keys: RefCell::new(None),
                first_rows: RefCell::new(None),
                day_check: RefCell::new(None),
                older_row,
                older: RefCell::default(),
                splits: split::Splits::default(),
            }),
        };
        list.connect();
        list.schedule_day_check();
        list
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.inner)
    }

    fn with(weak: &Weak<Inner>, f: impl FnOnce(MessageList)) {
        if let Some(inner) = weak.upgrade() {
            f(MessageList { inner });
        }
    }

    fn connect(&self) {
        let inner = &self.inner;
        let weak = self.downgrade();
        inner.model.set_listener(move |change| {
            Self::with(&weak, |list| list.model_changed(change));
        });
        let weak = self.downgrade();
        inner.selection.connect_selection_changed(move |_, _, _| {
            Self::with(&weak, |list| list.queue_settle());
        });
        // GtkMultiSelection drops removed rows from the selection without a signal of its own.
        let weak = self.downgrade();
        inner
            .selection
            .connect_items_changed(move |_, _, removed, _| {
                if removed > 0 {
                    Self::with(&weak, |list| list.queue_settle());
                }
            });
        let weak = self.downgrade();
        inner.view.connect_activate(move |_, position| {
            Self::with(&weak, |list| {
                list.inner.select_mode.set(false);
                list.select_position(position);
                list.settle();
                list.open_selected();
            });
        });
        self.connect_selection_input();
        let weak = self.downgrade();
        inner.context.set_quick(move |action, row| {
            Self::with(&weak, |list| {
                let handlers = list.inner.handlers.quick.borrow().clone();
                for handler in handlers {
                    handler(action, row.clone());
                }
            });
        });
        let weak = self.downgrade();
        inner.suggestions.set_context(move || {
            weak.upgrade().map(|inner| {
                let list = MessageList { inner };
                (list.today(), list.search_accounts())
            })
        });
        let weak = self.downgrade();
        inner.search.connect_changed(move |entry| {
            let empty = entry.text().is_empty();
            Self::with(&weak, |list| list.show_search_key(empty));
        });
        let weak = self.downgrade();
        inner.scope_folder.connect_toggled(move |chip| {
            if chip.is_active() {
                Self::with(&weak, |list| list.set_everywhere(false));
            }
        });
        let weak = self.downgrade();
        inner.scope_all.connect_toggled(move |chip| {
            if chip.is_active() {
                Self::with(&weak, |list| list.set_everywhere(true));
            }
        });
        let weak = self.downgrade();
        inner.search.connect_search_changed(move |entry| {
            let text = entry.text().trim().to_owned();
            Self::with(&weak, |list| {
                if !list.inner.quiet.get() {
                    list.set_query(text);
                }
            });
        });
        let weak = self.downgrade();
        inner.search.connect_stop_search(move |_| {
            Self::with(&weak, |list| {
                list.clear_search();
                list.inner.search_open.set(false);
                list.update_head();
                list.focus_selected();
            });
        });
        // Left empty, the field goes (a click into the list after `/`). The command palette
        // over the window only borrows the focus: it gives it back here when it closes.
        let leave = gtk::EventControllerFocus::new();
        let weak = self.downgrade();
        leave.connect_leave(move |_| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                Self::with(&weak, |list| {
                    let focused = list
                        .inner
                        .search
                        .root()
                        .and_then(|root| root.focus())
                        .is_some_and(|focus| {
                            focus == *list.inner.search.upcast_ref::<gtk::Widget>()
                                || focus.is_ancestor(&list.inner.search)
                                || crate::palette::holds(&focus)
                        });
                    if !focused && list.inner.search.text().is_empty() {
                        list.inner.search_open.set(false);
                        list.update_head();
                    }
                });
            });
        });
        inner.search.add_controller(leave);
        let weak = self.downgrade();
        inner.search.connect_activate(move |entry| {
            let text = entry.text().trim().to_owned();
            Self::with(&weak, |list| list.search_activated(text));
        });
        let weak = self.downgrade();
        inner
            .scroller
            .vadjustment()
            .connect_value_changed(move |_| Self::with(&weak, |list| list.scrolled()));
        let weak = self.downgrade();
        inner
            .scroller
            .vadjustment()
            .connect_changed(move |_| Self::with(&weak, |list| list.check_end()));
        let weak = self.downgrade();
        inner
            .scroller
            .hadjustment()
            .connect_page_size_notify(move |adjustment| {
                let width = adjustment.page_size();
                Self::with(&weak, |list| list.width_changed(width));
            });
        let weak = self.downgrade();
        inner.mailbox.connect_event(move |event| {
            Self::with(&weak, |list| list.on_event(event));
        });
    }

    /// The list pane's content: the search field (while searching) and the list.
    pub fn widget(&self) -> &gtk::Box {
        &self.inner.root
    }

    pub fn list_view(&self) -> &gtk::ListView {
        &self.inner.view
    }

    /// When `window`'s focus goes nowhere while the list is shown, as when a toast that held it
    /// hides after the last row left, it comes back to the list once the frame is done, so the
    /// list's keys (`z` among them) keep working. A focus that comes into the list on a row
    /// other than the selected one moves to it ([`MessageList::focus_follows_selection`]).
    pub fn keep_focus(&self, window: &impl IsA<gtk::Window>) {
        let weak = self.downgrade();
        window.connect_focus_widget_notify(move |window| {
            if let Some(focus) = gtk::prelude::GtkWindowExt::focus(window) {
                Self::with(&weak, |list| list.focus_moved(&focus));
                return;
            }
            let weak = weak.clone();
            let window = window.downgrade();
            glib::idle_add_local_once(move || {
                let Some(window) = window.upgrade() else {
                    return;
                };
                if gtk::prelude::GtkWindowExt::focus(&window).is_some() {
                    return;
                }
                Self::with(&weak, |list| {
                    if list.inner.root.is_mapped() {
                        list.take_focus();
                    }
                });
            });
        });
    }

    pub fn search_entry(&self) -> &gtk::SearchEntry {
        &self.inner.search
    }

    pub fn scroller(&self) -> &gtk::ScrolledWindow {
        &self.inner.scroller
    }

    pub fn model(&self) -> &MessageModel {
        &self.inner.model
    }

    pub fn selection(&self) -> &gtk::MultiSelection {
        &self.inner.selection
    }

    /// The empty state shown, as its title and its line: a folder's ("No messages", …), or
    /// Inbox Zero and the day.
    pub fn empty_state(&self) -> Option<(String, String)> {
        match self.inner.stack.visible_child_name().as_deref() {
            Some("empty") => Some((
                self.inner.empty.title().to_string(),
                self.inner
                    .empty
                    .description()
                    .map(|text| text.to_string())
                    .unwrap_or_default(),
            )),
            Some("zero") => Some((
                self.inner.zero.title().to_string(),
                self.inner.zero.date().to_string(),
            )),
            _ => None,
        }
    }

    /// The first name of the empty state's icon (a folder's, the Reminders view's clock), for
    /// checks.
    pub fn empty_icon(&self) -> Option<String> {
        self.inner.empty.icon_name()
    }

    /// The empty state's title and description (for the self-test).
    pub fn empty_text(&self) -> (String, String) {
        let empty = &self.inner.empty;
        (
            empty.title().to_string(),
            empty.description().map(String::from).unwrap_or_default(),
        )
    }

    /// Whether the search field shows.
    pub fn shows_search(&self) -> bool {
        self.inner.search_row.get_visible()
    }

    /// Opens `latch` once the list has the first rows of what it shows (the first window waits
    /// for it, S6).
    pub fn set_first_rows(&self, latch: Rc<Latch>) {
        self.inner.first_rows.replace(Some(latch));
    }

    /// Shows a folder or the unified inbox (`None`: nothing). A search in progress ends. An
    /// Inbox shows the side of the Split Inbox it showed last, as it was left.
    pub fn show(&self, scope: Option<ViewScope>) {
        self.leave_split();
        self.take_wanted_split(scope);
        let moved = self.inner.scope.get() != scope;
        self.inner.splits.shown.set(self.split_for(scope));
        self.inner.splits.arriving.set(self.arrival_for(scope));
        self.inner.scope.set(scope);
        if moved {
            // Another Inbox's totals: the last one's don't show until they are counted.
            self.forget_split_counts();
        }
        // Another folder's rows: a selection carried to a filter doesn't follow.
        self.inner.carried.borrow_mut().clear();
        if let Some(ViewScope::Folder(folder)) = scope {
            let mut older = self.inner.older.borrow_mut();
            if older.get(&folder) == Some(&Older::Paused) {
                older.remove(&folder);
            }
        }
        if !self.inner.query.borrow().is_empty() {
            self.inner.query.borrow_mut().clear();
            self.inner.quiet.set(true);
            self.inner.search.set_text("");
            self.inner.quiet.set(false);
            self.emit_search();
        }
        self.inner.search_open.set(false);
        self.reload();
        if moved {
            // The header's tabs (or the title) at once, the totals counted with the rows.
            self.count_splits();
            self.tell_split();
        }
    }

    pub fn scope(&self) -> Option<ViewScope> {
        self.inner.scope.get()
    }

    pub fn filter(&self) -> ViewFilter {
        self.inner.filter.get()
    }

    /// One row per message or per conversation.
    pub fn grouping(&self) -> ViewGrouping {
        self.inner.grouping.get()
    }

    /// Whether the rows are conversations (the content shown, or asked for).
    pub fn shows_conversations(&self) -> bool {
        self.inner.grouping.get() == ViewGrouping::Conversations
    }

    /// Lists one row per conversation or per message (Group by Conversation), showing what it
    /// shows again; a selected row stays selected as its conversation or as the message the
    /// reader shows for it.
    pub fn set_grouping(&self, grouping: ViewGrouping) {
        if self.inner.grouping.get() == grouping {
            return;
        }
        self.carry(grouping);
        self.inner.grouping.set(grouping);
        // The sides' totals count rows of the grouping: counted again with them.
        self.forget_split_counts();
        if self.inner.scope.get().is_some() {
            self.reload();
        }
    }

    /// Notes the selected rows to select again in the next content, by key in `grouping`: a
    /// message row stands for its conversation, a conversation row for the message the reader
    /// shows for it.
    fn carry(&self, grouping: ViewGrouping) {
        let model = &self.inner.model;
        let keys: Vec<RowKey> = self
            .selected_positions_if_few()
            .into_iter()
            .filter_map(|position| model.list_row_at(position))
            .map(|row| match (row, grouping) {
                (ListRow::Message(row), ViewGrouping::Conversations) => {
                    RowKey::Conversation(row.thread)
                }
                (ListRow::Conversation(row), ViewGrouping::Messages) => RowKey::Message(
                    self.reported_message(RowKey::Conversation(row.thread))
                        .unwrap_or(row.anchor),
                ),
                (row, _) => row.key(),
            })
            .collect();
        self.inner.carried.replace(keys);
    }

    /// Shows only the messages `filter` lets through (All for every one); the window's bar
    /// shows it ([`MessageList::connect_filter_changed`]). The selected rows stay selected
    /// where the filter keeps them.
    pub fn set_filter(&self, filter: ViewFilter) {
        if self.inner.filter.replace(filter) != filter {
            // The filter's pill shows after the search's scope: a compact scope ("Inbox · All ·
            // Unread ×") leaves the field its room in Split's 380 px header, where "workshop"
            // showed cut (phase C's review). It still says "All folders" to a screen reader.
            let scope = &self.inner.scope_all;
            scope.set_label(if filter == ViewFilter::All {
                SCOPE_ALL
            } else {
                "All"
            });
            crate::accessibility::name_button(scope, SCOPE_ALL);
            self.carry(self.inner.grouping.get());
            self.reload();
            let handlers = self.inner.handlers.filter.borrow().clone();
            for handler in handlers {
                handler(filter);
            }
        }
    }

    /// Runs `handler` with every filter the list starts showing.
    pub fn connect_filter_changed(&self, handler: impl Fn(ViewFilter) + 'static) {
        self.inner
            .handlers
            .filter
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` with whether the list shows an empty state, whenever that changes.
    pub fn connect_empty_changed(&self, handler: impl Fn(bool) + 'static) {
        self.inner
            .handlers
            .empty
            .borrow_mut()
            .push(Rc::new(handler));
    }

    pub fn query(&self) -> String {
        self.inner.query.borrow().clone()
    }

    /// Searches for `text` in the folder shown (every folder from the unified inbox); empty
    /// text shows the folder again.
    pub fn set_query(&self, text: String) {
        if *self.inner.query.borrow() == text {
            return;
        }
        if self.inner.search.text().trim() != text {
            self.inner.quiet.set(true);
            self.inner.search.set_text(&text);
            self.inner.quiet.set(false);
        }
        self.inner.query.replace(text);
        self.reload();
    }

    /// Ends the search, as Escape in the search field does; text typed within the entry's
    /// search delay goes too, so it doesn't start a search afterwards.
    pub fn clear_search(&self) {
        if !self.inner.search.text().is_empty() {
            self.inner.quiet.set(true);
            self.inner.search.set_text("");
            self.inner.quiet.set(false);
        }
        self.set_query(String::new());
    }

    /// Shows the search field and puts the keyboard focus in it (Search: `/`, Ctrl+F, the
    /// bar's button).
    pub fn focus_search(&self) {
        self.inner.search_open.set(true);
        self.update_head();
        self.inner.search.grab_focus();
    }

    /// The field shows while a search is asked for or runs, the scope after it while one runs
    /// from a folder; with neither, the header shows the view's name instead (the window,
    /// [`MessageList::connect_searching`]).
    fn update_head(&self) {
        let inner = &self.inner;
        let searching = inner.search_open.get()
            || !inner.query.borrow().is_empty()
            || !inner.search.text().is_empty();
        if inner.search_row.get_visible() != searching {
            inner.search_row.set_visible(searching);
        }
        let head = searching || inner.scope_row.get_visible();
        if inner.head.get_visible() != head {
            inner.head.set_visible(head);
        }
        if inner.told_searching.replace(Some(head)) != Some(head) {
            let handlers = inner.handlers.searching.borrow().clone();
            for handler in handlers {
                handler(head);
            }
        }
    }

    /// The search field and its scope, for the list's header row (`Window::set_list_search`).
    pub fn search_bar(&self) -> &gtk::Box {
        &self.inner.head
    }

    /// Runs `handler` with whether the search shows (the field, a search running), whenever
    /// that changes: the header shows it in the view name's place, and a Split window keeps
    /// its panes while it shows, whatever the results (phase B: typing jumped the list between
    /// 380 px and the whole window as results came and went).
    pub fn connect_searching(&self, handler: impl Fn(bool) + 'static) {
        self.inner
            .handlers
            .searching
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Puts the keyboard focus on the row under the cursor (the selected one outside the select
    /// mode), scrolled into view, else on the list: back from the reader, the list as it was
    /// left (the Superhuman pass).
    pub fn focus_selected(&self) {
        let view = &self.inner.view;
        if view.is_mapped()
            && let Some(position) = self.cursor_position()
        {
            if position >= self.inner.model.n_items() {
                self.inner.model.complete_growth();
            }
            view.scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
        }
        self.take_focus();
    }

    /// The window's keyboard focus moved to `focus`. In the list it lands where GtkListView's
    /// own focus row was left (Tab, the palette closing, the reader's Back, a reset under it),
    /// which needn't be the selected row: it moves to the selected row once the frame is done,
    /// and again once a list a pane just showed is laid out. Not the focus a press gives its
    /// row, which its release selects, nor the one Ctrl with the arrows moves on its own.
    fn focus_moved(&self, focus: &gtk::Widget) {
        let inner = &self.inner;
        let view = &inner.view;
        let inside = focus == view.upcast_ref::<gtk::Widget>() || focus.is_ancestor(view);
        if !inside || inner.pointer_down.get() || inner.cursor_keys.get() {
            return;
        }
        let weak = self.downgrade();
        glib::idle_add_local_once(move || {
            Self::with(&weak, |list| list.focus_follows_selection());
        });
        self.after_paint(|list| list.focus_follows_selection());
    }

    /// Moves the keyboard focus from another row of the list to the selected one, outside the
    /// select mode, so no ring stands apart from the selection. Never while the primary button
    /// is down on the list (its release selects the row it pressed, whose row has the focus),
    /// and never by scrolling: only a selected row wholly in view takes it (phase B's review:
    /// scrolling to it between a person's press and release lost the click). With nothing
    /// selected (Split before a pick), the focus goes to the first row in view, not where
    /// GtkListView's own focus row drifted as mail came in above it (phase C; the review: row 2
    /// showed the bar after two newer conversations).
    fn focus_follows_selection(&self) {
        let inner = &self.inner;
        if inner.select_mode.get() || inner.pointer_down.get() || !self.has_focus_within() {
            return;
        }
        let wanted = match self.selected_in_view() {
            Some(selected) => Some(selected),
            None if inner.selection.selection().size() == 0 => self.first_row_in_view(),
            None => None,
        };
        if let Some(wanted) = wanted
            && self.focused_row().is_some_and(|focused| focused != wanted)
        {
            // In view, GtkListView doesn't move.
            inner
                .view
                .scroll_to(wanted, gtk::ListScrollFlags::FOCUS, None);
        }
    }

    /// The keyboard focus for the list: its empty state while one shows (Inbox Zero, "No
    /// messages"), where the list's keys still work, else the list view, on the selected row
    /// when it is in view (GtkListView's own focus row may be another: where a grab left it
    /// before `j` and `k` moved the selection from the reader).
    fn take_focus(&self) {
        let taken = match self.inner.stack.visible_child_name().as_deref() {
            Some("zero") => self.inner.zero.focus(),
            Some("empty") => self.inner.empty.focus(),
            _ => false,
        };
        if taken {
            return;
        }
        let view = &self.inner.view;
        if !self.inner.select_mode.get()
            && let Some(selected) = self.selected_in_view()
        {
            // GtkListView's focus row, without moving the list: the grab below lands there.
            view.scroll_to(selected, gtk::ListScrollFlags::FOCUS, None);
        }
        view.grab_focus();
    }

    /// The one selected row, while it is wholly in view: the keyboard focus goes to it without
    /// the list moving.
    fn selected_in_view(&self) -> Option<u32> {
        let position = self.selected_position()?;
        self.row_widget(position)
            .is_some_and(|row| self.in_view(&row))
            .then_some(position)
    }

    /// Whether `row` is on screen whole, between the list's top and bottom edges.
    fn in_view(&self, row: &MessageRow) -> bool {
        let scroller = self.scroller();
        let height = scroller.height() as f32;
        row.is_mapped()
            && row
                .compute_bounds(scroller)
                .is_some_and(|bounds| bounds.y() >= 0.0 && bounds.y() + bounds.height() <= height)
    }

    /// Runs `work` once the next frame is painted: the list laid out as it shows now.
    fn after_paint(&self, work: impl FnOnce(MessageList) + 'static) {
        let Some(clock) = self.inner.view.frame_clock() else {
            return;
        };
        let weak = self.downgrade();
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let once = handler.clone();
        let work = RefCell::new(Some(work));
        let id = clock.connect_after_paint(move |clock| {
            if let Some(id) = once.take() {
                clock.disconnect(id);
            }
            if let Some(work) = work.take() {
                Self::with(&weak, work);
            }
        });
        handler.replace(Some(id));
        self.inner.view.queue_draw();
    }

    /// The keyboard focus is on the list's rows or its empty state, or on a row a reset just
    /// took out of the window.
    fn holds_focus(&self) -> bool {
        let Some(focus) = self.inner.view.root().and_then(|root| root.focus()) else {
            return false;
        };
        focus.root().is_none()
            || focus == *self.inner.stack.upcast_ref::<gtk::Widget>()
            || focus.is_ancestor(&self.inner.stack)
    }

    /// The search field's suggestions.
    pub fn suggestions(&self) -> &Suggestions {
        &self.inner.suggestions
    }

    /// How long after the last key the search runs ([`SEARCH_DELAY`]; the self-test measures
    /// others).
    pub fn set_search_delay(&self, delay: Duration) {
        self.inner
            .search
            .set_search_delay(u32::try_from(delay.as_millis()).unwrap_or(u32::MAX));
    }

    pub fn search_delay(&self) -> Duration {
        Duration::from_millis(u64::from(self.inner.search.search_delay()))
    }

    /// Searches every folder (`true`) or only the folder shown, as the scope row's chips do;
    /// the search's end goes back to the folder.
    pub fn set_everywhere(&self, everywhere: bool) {
        let inner = &self.inner;
        let chip = if everywhere {
            &inner.scope_all
        } else {
            &inner.scope_folder
        };
        if !chip.is_active() {
            chip.set_active(true);
            return;
        }
        if inner.everywhere.replace(everywhere) != everywhere && !inner.query.borrow().is_empty() {
            self.reload();
        }
    }

    /// Whether the search covers every folder although the list shows one.
    pub fn is_everywhere(&self) -> bool {
        self.inner.everywhere.get() && matches!(self.inner.scope.get(), Some(ViewScope::Folder(_)))
    }

    /// The scope row: shown while a search runs from a folder, naming that folder.
    pub fn scope_row(&self) -> &gtk::Box {
        &self.inner.scope_row
    }

    /// Runs the search shown again, as after the search index was rebuilt.
    pub fn search_again(&self) {
        self.inner.model.search_again();
    }

    /// Searches every folder for `text` as if it was typed into the field and Enter pressed:
    /// the field shows it, the first result is selected once it comes, and the list takes the
    /// keyboard focus (the contact pane's "Search all mail from …", phase C).
    pub fn search_for(&self, text: &str) {
        self.inner.search_open.set(true);
        self.set_everywhere(true);
        self.search_activated(text.to_owned());
    }

    /// Enter in the search field: the search runs now, the first result is selected (now, or
    /// once the results for what was typed come) and the list takes the keyboard focus.
    fn search_activated(&self, text: String) {
        self.set_query(text);
        let model = &self.inner.model;
        if matches!(model.content(), Content::Search { .. }) && model.is_pending() {
            self.inner.select_first.set(true);
        } else if self.selected_positions().is_empty() && model.total() > 0 {
            self.select_position(0);
        }
        self.inner.view.grab_focus();
    }

    fn today(&self) -> Date {
        let now = self.now();
        Date {
            year: now.year(),
            month: now.month() as u8,
            day: now.day_of_month() as u8,
        }
    }

    /// The accounts whose people the search field suggests: the folder's while the search
    /// covers one folder, else every account.
    fn search_accounts(&self) -> Vec<AccountId> {
        let mailbox = &self.inner.mailbox;
        match self.inner.scope.get() {
            Some(ViewScope::Folder(folder)) if !self.inner.everywhere.get() => mailbox
                .folder(folder)
                .map(|folder| vec![folder.account])
                .unwrap_or_default(),
            _ => mailbox
                .accounts()
                .into_iter()
                .map(|account| account.id)
                .collect(),
        }
    }

    fn show_search_key(&self, empty: bool) {
        let key = &self.inner.search_key;
        key.set_visible(empty && !key.label().is_empty());
    }

    /// The scope row follows the search: shown from a folder while there is one, back to the
    /// folder once it ends.
    fn update_scope(&self) {
        let inner = &self.inner;
        let searching = !inner.query.borrow().is_empty();
        if !searching && inner.everywhere.replace(false) {
            inner.scope_folder.set_active(true);
        }
        let folder = match inner.scope.get() {
            Some(ViewScope::Folder(folder)) if searching => inner.mailbox.folder(folder),
            _ => None,
        };
        if let Some(folder) = &folder {
            let title = crate::triage::folder_title(folder);
            if inner.scope_folder.label().as_deref() != Some(title.as_str()) {
                inner.scope_folder.set_label(&title);
                if let Some(label) = inner.scope_folder.child().and_downcast::<gtk::Label>() {
                    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    label.set_max_width_chars(18);
                }
            }
        }
        inner.scope_row.set_visible(folder.is_some());
        self.update_head();
    }

    fn reload(&self) {
        let inner = &self.inner;
        let thresholds = self.thresholds_now();
        let query = inner.query.borrow().clone();
        self.update_scope();
        self.update_reminders();
        match inner.scope.get() {
            None => inner.model.clear(),
            Some(scope) if query.is_empty() => {
                let spec = ViewSpec {
                    scope,
                    filter: inner.filter.get(),
                    grouping: inner.grouping.get(),
                    split: inner.splits.shown.get(),
                };
                if let Some((opened, events)) = self.take_warm(spec, thresholds) {
                    inner.model.adopt_with(spec, thresholds, opened, events);
                } else {
                    match inner.mailbox.take_first_view(spec, thresholds) {
                        Some(opened) => inner.model.adopt(spec, thresholds, opened),
                        None => inner.model.open(spec, thresholds),
                    }
                }
            }
            Some(scope) => {
                let folder = match scope {
                    ViewScope::Folder(folder) if !inner.everywhere.get() => Some(folder),
                    _ => None,
                };
                inner.model.search(
                    query,
                    folder,
                    inner.filter.get(),
                    inner.grouping.get(),
                    thresholds,
                );
            }
        }
        self.emit_search();
    }

    fn model_changed(&self, change: Change) {
        match change {
            Change::Resetting => {
                self.inner
                    .place
                    .set(self.inner.scroller.vadjustment().value());
                // The model holds the new rows already; the selection is what settled last.
                let selected = self.inner.settled.borrow().clone();
                self.inner
                    .restoring
                    .replace((selected, self.inner.select_mode.get()));
                // A reload of the same view destroys the row that has the keyboard focus, and
                // GTK would then scroll its next focus row, the first, into view: the focus
                // leaves the list until the rows are back at their place.
                let reload = *self.inner.content.borrow() == self.inner.model.content();
                let refocus = reload && self.has_focus_within();
                self.inner.refocus.set(refocus);
                if refocus && let Some(root) = self.inner.view.root() {
                    root.set_focus(None::<&gtk::Widget>);
                }
                return;
            }
            Change::Reset => {
                self.start_growth();
                let content = self.inner.model.content();
                let same = *self.inner.content.borrow() == content;
                self.inner.content.replace(content);
                if same {
                    // The same view read again (a reload): it keeps its place, its selection
                    // and the keyboard focus. GTK lost the row that had the focus with the reset
                    // and would scroll its next focus row, the first, into view.
                    self.inner.model.complete_growth();
                    self.scroll_after_layout(self.inner.place.get());
                    if self.inner.refocus.get() {
                        self.focus_after_layout();
                    }
                    self.inner.carried.borrow_mut().clear();
                    self.restore_selection();
                } else {
                    self.inner.wanted.borrow_mut().clear();
                    self.inner.leaving.take();
                    self.inner.returning.borrow_mut().clear();
                    self.inner.cursor.set(None);
                    self.set_select_mode(false);
                    if !self.arrive_split() && !self.select_carried() {
                        self.scroll_after_layout(0.0);
                        // Focused: the cursor on the first row, as Superhuman shows it.
                        if self.inner.keeps_cursor.get() && self.inner.model.total() > 0 {
                            self.select_position(0);
                        }
                    }
                }
                // The Split Inbox: the other side opened ahead, the tabs counted.
                self.warm_other();
                self.count_splits();
                self.tell_split();
                if let Some(latch) = self.inner.first_rows.borrow().as_ref() {
                    latch.open();
                }
                if self.inner.select_first.take()
                    && matches!(self.inner.model.content(), Content::Search { .. })
                    && self.inner.model.total() > 0
                {
                    self.select_position(0);
                }
                let handlers = self.inner.handlers.reset.borrow().clone();
                for handler in handlers {
                    handler();
                }
            }
            Change::Rows => {
                self.select_wanted();
                self.check_leaving();
                self.select_returning();
                self.keep_cursor();
            }
            Change::Failed => {
                self.inner.select_first.set(false);
                if let Some(latch) = self.inner.first_rows.borrow().as_ref() {
                    latch.open();
                }
            }
        }
        self.update_empty();
        self.emit_search();
        self.queue_sweep();
    }

    fn update_empty(&self) {
        let inner = &self.inner;
        let model = &inner.model;
        if model.is_pending() {
            return;
        }
        let empty = model.total() == 0;
        let zero = empty && self.is_inbox_view(&model.content());
        if zero {
            inner.zero.set_date(&self.now());
            // An empty side of the Split Inbox says what the other one holds.
            inner.zero.set_other(self.zero_other_line().as_deref());
        } else if empty {
            // The Reminders view's (phase C) has its clock, and one line that fits. Under a
            // filter, the filter's own state says why nothing shows (the second review: "No
            // reminders" beside the Unread pill).
            let reminders = self.inner.context.reminders().wakes
                && matches!(
                    model.content(),
                    Content::View(spec) if spec.filter == ViewFilter::All
                );
            inner.empty.set_icons(if reminders {
                &row::REMIND_ICONS
            } else {
                &empty::FOLDER_ICONS
            });
            // Reminders waiting there that it doesn't list yet are on their way: a server
            // account's moves wait for the undo window and the server (the third review: "No
            // reminders … press h" right after h).
            let arriving = self.inner.context.reminders().arriving;
            let on_the_way =
                format!("Your {arriving} reminders show here once the mail server has moved them.");
            let found_nothing;
            let (title, description) = match model.content() {
                Content::Nothing => ("No folder", "Pick a folder to see its messages."),
                Content::Search {
                    text,
                    folder,
                    filter,
                    grouping,
                } => {
                    let folder = folder
                        .and_then(|folder| inner.mailbox.folder(folder))
                        .map(|folder| crate::triage::folder_title(&folder));
                    found_nothing = no_results(&text, folder.as_deref(), filter, grouping);
                    ("No results", found_nothing.as_str())
                }
                Content::View(_) if reminders && arriving == 1 => (
                    "On the way",
                    "Your reminder shows here once the mail server has moved it.",
                ),
                Content::View(_) if reminders && arriving > 1 => {
                    ("On the way", on_the_way.as_str())
                }
                Content::View(_) if reminders => (
                    "No reminders",
                    "Press h on a conversation to set a reminder.",
                ),
                Content::View(spec) => match spec.filter {
                    ViewFilter::All => ("No messages", "Messages in this folder appear here."),
                    ViewFilter::Unread => ("No unread messages", "Everything here is read."),
                    ViewFilter::Starred => ("No starred messages", "Starred messages appear here."),
                    ViewFilter::Attachments => {
                        ("No attachments", "Messages with attachments appear here.")
                    }
                    ViewFilter::Important => {
                        ("No important messages", "Mail from people appears here.")
                    }
                    ViewFilter::Other => ("No other messages", "Everything else appears here."),
                },
            };
            inner.empty.set_title(title);
            inner.empty.set_description(Some(description));
        }
        // The rows the focus was on may go with the reset: the focus stays with the list, or GTK
        // would move it on to the window's first control and the list's keys would stop.
        let focused = self.holds_focus();
        inner.stack.set_visible_child_name(match (zero, empty) {
            (true, _) => "zero",
            (_, true) => "empty",
            _ => "list",
        });
        if empty && focused {
            self.take_focus();
        }
        if inner.told_empty.replace(Some(empty)) != Some(empty) {
            let handlers = inner.handlers.empty.borrow().clone();
            for handler in handlers {
                handler(empty);
            }
        }
        if inner.told_zero.replace(Some(zero)) != Some(zero) {
            let handlers = inner.handlers.zero.borrow().clone();
            for handler in handlers {
                handler(zero);
            }
        }
    }

    /// Runs `handler` with whether the list shows Inbox Zero, whenever that changes (the window
    /// tints its whole pane, the header row too).
    pub fn connect_inbox_zero(&self, handler: impl Fn(bool) + 'static) {
        self.inner.handlers.zero.borrow_mut().push(Rc::new(handler));
    }

    /// An Inbox or All Inboxes with every message (no filter, no search), or one side of it:
    /// its empty state is Inbox Zero.
    fn is_inbox_view(&self, content: &Content) -> bool {
        let Content::View(spec) = content else {
            return false;
        };
        spec.filter == ViewFilter::All
            && match spec.scope {
                ViewScope::UnifiedInbox => true,
                ViewScope::Folder(folder) => {
                    self.inner.mailbox.folder(folder).is_some_and(|folder| {
                        folder.role == sealwax_domain::model::FolderRole::Inbox
                    })
                }
            }
    }

    /// Tells the search listeners again what the list shows (the window resets the list's title
    /// when it rebuilds its sidebar).
    pub fn refresh_heading(&self) {
        self.emit_search();
    }

    fn emit_search(&self) {
        let heading =
            matches!(self.inner.model.content(), Content::Search { .. }).then(|| SearchHeading {
                results: self.inner.model.total(),
                more: self.inner.model.has_more_results(),
                searching: self.inner.model.is_searching(),
                everywhere: self.is_everywhere(),
            });
        // The header shows the field in the heading's place: the field says the count.
        let described = heading.as_ref().map(SearchHeading::subtitle);
        if *self.inner.search_described.borrow() != described {
            match &described {
                Some(text) => self
                    .inner
                    .search
                    .update_property(&[gtk::accessible::Property::Description(text)]),
                None => self
                    .inner
                    .search
                    .reset_property(gtk::AccessibleProperty::Description),
            }
            self.inner.search_described.replace(described);
        }
        let handlers = self.inner.handlers.search.borrow().clone();
        for handler in handlers {
            handler(heading.as_ref());
        }
    }

    /// After a reset GTK sees [`FIRST_ROWS`] rows; each later frame shows [`GROW_STEP`] more until
    /// every row is there (S6). The rows bound after each frame are kept for the self-test.
    fn start_growth(&self) {
        if let Some(id) = self.inner.growth.take() {
            id.remove();
        }
        self.inner.growth_frames.borrow_mut().clear();
        let weak = self.downgrade();
        let ticks = Cell::new(0u32);
        let done = Cell::new(false);
        let id = self.inner.view.add_tick_callback(move |_, _| {
            let Some(inner) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let tick = ticks.get();
            ticks.set(tick + 1);
            if tick > 0 {
                let bound = inner.context.bound.borrow().len() as u32;
                inner.growth_frames.borrow_mut().push(bound);
            }
            if tick == 0 {
                return glib::ControlFlow::Continue;
            }
            if done.get() {
                inner.growth.take();
                MessageList { inner }.check_end();
                return glib::ControlFlow::Break;
            }
            if !inner.model.grow() {
                done.set(true);
            }
            glib::ControlFlow::Continue
        });
        self.inner.growth.replace(Some(id));
    }

    /// Rows bound after each frame since the last reset, while the list grew: the first is what
    /// the first frame laid out.
    pub fn growth_frames(&self) -> Vec<u32> {
        self.inner.growth_frames.borrow().clone()
    }

    pub fn is_growing(&self) -> bool {
        self.inner.growth.borrow().is_some()
    }

    fn on_event(&self, event: &Arc<ChangeEvent>) {
        let at_top = self.inner.scroller.vadjustment().value() < PIN_SLACK;
        // What is selected and focused now, by key: a conversation that moves keeps both.
        let selected = self.selected_keys_if_few();
        let focused = self
            .has_focus_within()
            .then(|| self.focused_row())
            .flatten()
            .and_then(|position| self.inner.model.key_at(position));
        let inserted = self.inner.model.apply_event(event);
        if inserted && at_top {
            self.scroll_after_layout(0.0);
        }
        self.keep_selected(&selected, focused, event);
        self.split_event(event);
    }

    /// Selects again the rows of `keys` that a change kept in the list but took out of the
    /// selection, and puts the keyboard focus back on `focused`: a conversation that moved too
    /// far to be told GTK at once, or one merged into another, which it is now. The selection
    /// is then the row reported, so the reader isn't asked again.
    fn keep_selected(&self, keys: &[RowKey], focused: Option<RowKey>, event: &ChangeEvent) {
        if keys.is_empty() {
            return;
        }
        let merged: HashMap<_, _> = event
            .merged_threads
            .iter()
            .map(|merge| (merge.from, merge.into))
            .collect();
        let follow = |key: RowKey| match key {
            RowKey::Conversation(thread) => {
                RowKey::Conversation(merged.get(&thread).copied().unwrap_or(thread))
            }
            key => key,
        };
        let inner = &self.inner;
        if let Some(reported) = inner.reported_key.get()
            && follow(reported) != reported
            && matches!(inner.reported.get(), Selected::One(_))
        {
            // The conversation shown is the one it merged into: the reader already follows it.
            inner.reported_key.set(Some(follow(reported)));
        }
        if let Some(cursor) = inner.cursor.get() {
            inner.cursor.set(Some(follow(cursor)));
        }
        let model = &inner.model;
        let several = keys.len() > 1;
        for key in keys.iter().copied().map(follow) {
            let Some(position) = model.position_of_key(key) else {
                continue;
            };
            if !inner.selection.is_selected(position) {
                self.select_item(position, several, false);
            }
        }
        if let Some(focused) = focused.map(follow)
            && let Some(position) = model.position_of_key(focused)
            && self.focused_row() != Some(position)
        {
            inner
                .view
                .scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
        }
    }

    /// Scrolls to `value` once GTK has laid out the rows that just changed. GTK keeps the row it
    /// showed first anchored, so new mail would land above the view; after a reset it anchors the
    /// first row's top, leaving its section header above the view (S3), and loses the place a
    /// reload should keep. Moving the adjustment in the layout phase, after GTK's own layout,
    /// lays the list out again within the same frame.
    fn scroll_after_layout(&self, value: f64) {
        let view = &self.inner.view;
        match view.frame_clock() {
            Some(clock) => self.scroll_on(&clock, value),
            None => {
                let weak = self.downgrade();
                let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
                let once = handler.clone();
                let id = view.connect_map(move |view| {
                    if let Some(id) = once.take() {
                        view.disconnect(id);
                    }
                    if let Some(clock) = view.frame_clock() {
                        Self::with(&weak, |list| list.scroll_on(&clock, value));
                    }
                });
                handler.replace(Some(id));
            }
        }
    }

    fn scroll_on(&self, clock: &gtk::gdk::FrameClock, value: f64) {
        let weak = self.downgrade();
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let once = handler.clone();
        let id = clock.connect_layout(move |clock| {
            if let Some(id) = once.take() {
                clock.disconnect(id);
            }
            Self::with(&weak, |list| {
                list.inner.scroller.vadjustment().set_value(value)
            });
        });
        handler.replace(Some(id));
        self.inner.view.queue_resize();
    }

    /// Gives the keyboard focus to the selected row, or else the first row in view, once the
    /// list is laid out at its place: the scroll lands in one frame's layout phase and GTK lays
    /// the rows out there in the next frame at the latest.
    fn focus_after_layout(&self) {
        let Some(clock) = self.inner.view.frame_clock() else {
            return;
        };
        let weak = self.downgrade();
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let once = handler.clone();
        let painted = Cell::new(0u32);
        let id = clock.connect_after_paint(move |clock| {
            painted.set(painted.get() + 1);
            if painted.get() < 2 {
                return;
            }
            if let Some(id) = once.take() {
                clock.disconnect(id);
            }
            Self::with(&weak, |list| list.focus_row_in_view());
        });
        handler.replace(Some(id));
        self.inner.view.queue_draw();
    }

    fn focus_row_in_view(&self) {
        let position = self.selected_in_view().or_else(|| self.first_row_in_view());
        if let Some(position) = position {
            let view = &self.inner.view;
            view.scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
            view.grab_focus();
        }
    }

    /// The first row wholly in view.
    fn first_row_in_view(&self) -> Option<u32> {
        self.inner
            .context
            .bound
            .borrow()
            .iter()
            .filter(|row| self.in_view(row))
            .filter_map(MessageRow::position)
            .min()
    }

    /// After rows moved under a page on its way, asks again for the rows still shown as
    /// placeholders.
    fn queue_sweep(&self) {
        if self.inner.sweep_queued.replace(true) {
            return;
        }
        let weak = self.downgrade();
        glib::idle_add_local_once(move || {
            Self::with(&weak, |list| {
                list.inner.sweep_queued.set(false);
                let positions: Vec<u32> = list
                    .inner
                    .context
                    .bound
                    .borrow()
                    .iter()
                    .filter(|row| row.item().is_some_and(|item| !item.is_loaded()))
                    .filter_map(MessageRow::position)
                    .collect();
                for position in positions {
                    list.inner.model.request_position(position);
                }
            });
        });
    }

    fn scrolled(&self) {
        self.fetch_more_results();
        self.check_end();
        if self.inner.speed.borrow().is_some() {
            return;
        }
        let weak = self.downgrade();
        let last: Cell<Option<(f64, i64)>> = Cell::new(None);
        let slow = Cell::new(0u32);
        let still = Cell::new(0u32);
        let id = self.inner.view.add_tick_callback(move |_, clock| {
            let Some(inner) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let list = MessageList { inner };
            let value = list.inner.scroller.vadjustment().value();
            let now = clock.frame_time();
            let (speed, moved) = match last.replace(Some((value, now))) {
                Some((before, then)) if now > then => (
                    (value - before).abs() / ((now - then) as f64 / 1e6),
                    value != before,
                ),
                _ => (0.0, true),
            };
            if speed > SKELETON_SPEED {
                slow.set(0);
                list.inner.context.skeleton.set(true);
            } else {
                slow.set(slow.get() + 1);
                if slow.get() >= SLOW_FRAMES {
                    list.show_text();
                }
            }
            still.set(if moved { 0 } else { still.get() + 1 });
            if still.get() >= STILL_FRAMES {
                list.show_text();
                list.inner.speed.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        self.inner.speed.replace(Some(id));
    }

    /// Search results come [`SEARCH_PAGE`] at a time; the next ones are fetched once the last
    /// screenful is in view. GtkListView realizes rows far beyond the view, so the rows it asks
    /// for can't tell.
    fn fetch_more_results(&self) {
        if !matches!(self.inner.model.content(), Content::Search { .. }) {
            return;
        }
        let adjustment = self.inner.scroller.vadjustment();
        let page = adjustment.page_size();
        if page > 0.0 && adjustment.value() + 2.0 * page >= adjustment.upper() {
            self.inner.model.search_more();
        }
    }

    /// Within a screenful of the end of a folder (never All Inboxes or search results), once all
    /// its rows are in: asks for older mail, unless it was asked for, the server has none, or an
    /// answer brought nothing since the folder was shown (ML-043).
    fn check_end(&self) {
        let inner = &self.inner;
        let Content::View(ViewSpec {
            scope: ViewScope::Folder(folder),
            ..
        }) = inner.model.content()
        else {
            return;
        };
        if self.is_growing()
            || inner.model.is_pending()
            || inner.older.borrow().contains_key(&folder)
        {
            return;
        }
        let adjustment = inner.scroller.vadjustment();
        let page = adjustment.page_size();
        if page <= 0.0 || adjustment.value() + 2.0 * page < adjustment.upper() {
            return;
        }
        inner.older.borrow_mut().insert(folder, Older::Asked);
        // Shown once the layout under way is done: this runs while the scroller is laid out
        // (its adjustment's `changed`), and the pane's box, laying out its children by the
        // count it took before, gave a row that showed meanwhile no height ("attempt to
        // allocate GtkLabel … height -12", phase C).
        let weak = self.downgrade();
        glib::idle_add_local_once(move || {
            Self::with(&weak, |list| {
                let asking = list
                    .inner
                    .older
                    .borrow()
                    .values()
                    .any(|state| *state == Older::Asked);
                list.inner.older_row.set_visible(asking);
            });
        });
        let handlers = inner.handlers.older.borrow().clone();
        for handler in handlers {
            handler(folder);
        }
    }

    /// Runs `handler` when the list wants `folder`'s older mail (`Command::LoadOlder`).
    pub fn connect_older_wanted(&self, handler: impl Fn(FolderId) + 'static) {
        self.inner
            .handlers
            .older
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// The engine's answer (`Event::OlderLoaded`): the row goes; the stored mail arrives as
    /// change events. Nothing more is asked once `complete`; after an answer with nothing, not
    /// until the folder is shown again.
    pub fn older_loaded(&self, folder: FolderId, fetched: u32, complete: bool) {
        let mut older = self.inner.older.borrow_mut();
        if complete {
            older.insert(folder, Older::Complete);
        } else if fetched > 0 {
            older.remove(&folder);
        } else {
            older.insert(folder, Older::Paused);
        }
        let asking = older.values().any(|state| *state == Older::Asked);
        drop(older);
        self.inner.older_row.set_visible(asking);
    }

    /// Rows show their text again after a fast scroll.
    fn show_text(&self) {
        if !self.inner.context.skeleton.replace(false) {
            return;
        }
        let rows: Vec<MessageRow> = self
            .inner
            .context
            .bound
            .borrow()
            .iter()
            .filter(|row| row.shows_skeleton())
            .cloned()
            .collect();
        for row in rows {
            row.refresh();
        }
    }

    /// Whether rows skip their text now because the list moves fast.
    pub fn is_skeleton(&self) -> bool {
        self.inner.context.skeleton.get()
    }

    fn width_changed(&self, width: f64) {
        let scale = ((width / 400.0) * 2.0).floor().max(2.0) as f32 / 2.0;
        if self.inner.context.scale.replace(scale) == scale {
            return;
        }
        let rows = self.inner.context.bound.borrow().clone();
        for row in rows {
            row.refresh();
        }
    }

    fn queue_settle(&self) {
        if self.inner.settle_queued.replace(true) {
            return;
        }
        let weak = self.downgrade();
        // Before GTK's redraw (HIGH_IDLE runs ahead of it): the reader swaps in a conversation it
        // laid out ahead within the same frame as the list's new selection, not a frame later.
        glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
            Self::with(&weak, |list| list.settle());
            glib::ControlFlow::Break
        });
    }

    /// Reports the selection once it has settled: a reset that keeps the same message selected
    /// reports nothing, and a placeholder is reported once its row is in. Two messages or more
    /// put the list in the select mode, and nothing selected, or a plain click, leaves it.
    fn settle(&self) {
        let inner = &self.inner;
        inner.settle_queued.set(false);
        if let Some((item, handler)) = inner.waiting.take() {
            item.disconnect(handler);
        }
        let positions = self.selected_positions();
        if !positions.is_empty() || inner.wanted_until.get() < Some(Instant::now()) {
            inner.wanted.borrow_mut().clear();
        }
        if !inner.wanted.borrow().is_empty() {
            // A reload is bringing the selection back as its rows load.
            return;
        }
        inner.settled.replace(self.selected_keys());
        let plain_click = inner.plain_click.replace(false);
        if positions.is_empty() || (plain_click && positions.len() == 1) {
            self.set_select_mode(false);
        } else if positions.len() > 1 {
            self.set_select_mode(true);
        }
        inner.settled_at.set(match positions.as_slice() {
            [position] => Some(*position),
            [] => inner.settled_at.get(),
            _ => None,
        });
        let mut own = None;
        let (selected, key) = match positions.as_slice() {
            [] => (Selected::Nothing, None),
            [position] if !inner.select_mode.get() => {
                let Some(item) = inner.model.item(*position).and_downcast::<MessageItem>() else {
                    return;
                };
                match (item.key(), item.message()) {
                    (Some(key), Some(message)) => {
                        inner.cursor.set(Some(key));
                        own = Some(message);
                        let message = match inner.open_at.get() {
                            Some((wanted, at)) if wanted == key => at,
                            _ => message,
                        };
                        (Selected::One(message), Some(key))
                    }
                    _ => {
                        let weak = self.downgrade();
                        let handler = item.connect_changed(move |_| {
                            Self::with(&weak, |list| list.queue_settle());
                        });
                        inner.waiting.replace(Some((item, handler)));
                        return;
                    }
                }
            }
            positions => (Selected::Batch(positions.len() as u32), None),
        };
        if inner
            .open_at
            .get()
            .is_some_and(|(wanted, _)| Some(wanted) != key)
        {
            inner.open_at.set(None);
        }
        self.report(selected, key, own);
    }

    /// Tells the handlers what is selected now: one row only when it is another row than the
    /// one reported (a conversation that moved or gained a message is the same row). `own`:
    /// that row's own message, a conversation's anchor.
    fn report(&self, selected: Selected, key: Option<RowKey>, own: Option<MessageId>) {
        let inner = &self.inner;
        let same = match (inner.reported.get(), selected) {
            (Selected::One(_), Selected::One(_)) => inner.reported_key.get() == key,
            (before, now) => before == now,
        };
        if same {
            return;
        }
        inner.reported.set(selected);
        inner.reported_key.set(key);
        inner.reported_anchor.set(own);
        let handlers = &self.inner.handlers;
        match selected {
            Selected::Nothing => {
                let cleared = handlers.cleared.borrow().clone();
                for handler in cleared {
                    handler();
                }
            }
            Selected::One(id) => {
                let selected = handlers.selected.borrow().clone();
                for handler in selected {
                    handler(id);
                }
            }
            Selected::Batch(count) => {
                let batch = handlers.batch.borrow().clone();
                for handler in batch {
                    handler(count);
                }
            }
        }
    }

    /// What the list last reported: nothing, one message, or the select mode's count.
    pub fn reported(&self) -> Selected {
        self.inner.reported.get()
    }

    /// The message reported for the row `key`, while it is the row reported.
    fn reported_message(&self, key: RowKey) -> Option<MessageId> {
        match self.inner.reported.get() {
            Selected::One(message) if self.inner.reported_key.get() == Some(key) => Some(message),
            _ => None,
        }
    }

    /// Whether the rows show checkboxes and `j`/`k` move a cursor.
    pub fn is_select_mode(&self) -> bool {
        self.inner.select_mode.get()
    }

    /// Focused: rows came into a list with nothing selected (an Undo from Inbox Zero, new mail
    /// there), and no reload or action is bringing a selection: the cursor goes to the first.
    fn keep_cursor(&self) {
        let inner = &self.inner;
        let total = inner.model.total();
        if inner.keeps_cursor.get()
            && !inner.select_mode.get()
            && total > 0
            && !inner.model.is_pending()
            && inner.wanted.borrow().is_empty()
            && inner.leaving.borrow().is_none()
            && inner.selection.selection().size() == 0
        {
            // The row that took the last one's place (one gone by another device's action).
            let position = inner.settled_at.get().unwrap_or(0).min(total - 1);
            self.select_position(position);
        }
    }

    /// Focused mode (the window's, the Superhuman pass): a row is always under the cursor, a
    /// plain click opens its row, and the list shows no date headers, its dates being on the
    /// rows (BRIEF.md "No date section headers in Focused mode"); Split keeps them, and a click
    /// that selects and shows.
    pub fn set_focused(&self, focused: bool) {
        self.inner.focused.set(focused);
        self.update_headers();
        self.set_keeps_cursor(focused);
    }

    /// The date headers: in Split, except in the Reminders view, whose rows are in the order
    /// they come back rather than of their dates (phase C).
    fn update_headers(&self) {
        let view = &self.inner.view;
        let shown = !self.inner.focused.get() && !self.inner.context.reminders().wakes;
        let headers: Option<gtk::ListItemFactory> =
            shown.then(|| self.inner.headers.clone().upcast());
        if view.header_factory() != headers {
            // A list at its top stays at its top: GTK keeps the first row where it was, and the
            // date header that comes above it would sit out of view (phase C, the visual QA:
            // Split's "TODAY" 26 px above the list right after leaving Focused).
            let at_top = self.inner.scroller.vadjustment().value() < 1.0;
            view.set_header_factory(headers.as_ref());
            if at_top {
                self.scroll_after_layout(0.0);
            }
        }
    }

    /// What the rows show of Remind Me (phase C, `remind::row_reminders`): the clock on
    /// conversations back from Reminders in an Inbox, the wake times in the Reminders view.
    pub fn set_reminders(&self, reminders: RowReminders) {
        let wakes = reminders.wakes;
        let marks = reminders.marks;
        let arriving = reminders.arriving;
        let before = self.inner.context.reminders();
        if self.inner.context.set_reminders(reminders) {
            if before.wakes != wakes {
                self.update_headers();
            }
            if before.wakes != wakes || before.marks != marks {
                self.inner.view.queue_resize();
            }
            // The empty Reminders view says what is on its way there.
            if before.arriving != arriving && self.inner.model.total() == 0 {
                self.update_empty();
            }
        }
    }

    /// What the rows show of Remind Me now.
    pub fn reminders(&self) -> RowReminders {
        self.inner.context.reminders()
    }

    /// Remind Me's marks for what the list shows now: none for search results.
    fn update_reminders(&self) {
        let scope = self
            .inner
            .query
            .borrow()
            .is_empty()
            .then(|| self.inner.scope.get())
            .flatten();
        self.set_reminders(crate::remind::row_reminders(
            scope,
            self.inner.grouping.get(),
            &self.inner.mailbox.folders(),
        ));
    }

    /// Keeps a row under the cursor (selected) whenever the list fills with new content
    /// (Focused mode: selecting there lays the conversation out unseen, unread); with no row
    /// selected now, the first one is at once.
    pub fn set_keeps_cursor(&self, keeps: bool) {
        if self.inner.keeps_cursor.replace(keeps) == keeps || !keeps {
            return;
        }
        let model = &self.inner.model;
        if !model.is_pending() && model.total() > 0 && self.selected_positions().is_empty() {
            self.select_position(0);
        }
    }

    fn set_select_mode(&self, on: bool) {
        if self.inner.select_mode.replace(on) == on {
            return;
        }
        self.inner.context.select_mode.set(on);
        if on {
            self.inner.view.add_css_class("sw-select-mode");
        } else {
            self.inner.view.remove_css_class("sw-select-mode");
        }
        let rows = self.inner.context.bound.borrow().clone();
        for row in rows {
            row.refresh();
        }
    }

    /// After the same content was read again (a reload), selects the rows that were selected,
    /// now or once they load, in the select mode if it was on.
    fn restore_selection(&self) {
        let (keys, select_mode) = self.inner.restoring.take();
        if select_mode {
            self.set_select_mode(true);
        }
        if self.selected_positions().is_empty() && !keys.is_empty() {
            self.inner.wanted.replace(keys);
            self.inner
                .wanted_until
                .set(Some(Instant::now() + TARGETS_WAIT));
            let weak = self.downgrade();
            glib::timeout_add_local_once(TARGETS_WAIT, move || {
                Self::with(&weak, |list| list.queue_settle());
            });
        }
        self.select_wanted();
    }

    fn select_wanted(&self) {
        if self.inner.wanted.borrow().is_empty() {
            return;
        }
        let wanted = self.inner.wanted.take();
        let mut waiting = Vec::new();
        for key in wanted {
            match self.inner.model.position_of_key(key) {
                Some(position) => {
                    if position >= self.inner.model.n_items() {
                        self.inner.model.complete_growth();
                    }
                    self.inner.selection.select_item(position, false);
                }
                None => waiting.push(key),
            }
        }
        self.inner.wanted.replace(waiting);
    }

    /// After a filter or the grouping changed what the list shows, the rows that were
    /// selected are selected again where the new content's first rows hold them (a filter
    /// that keeps the selected conversation keeps the reader on it, with no new report), in
    /// view and with the keyboard focus when the list has it; the others are gone. True when
    /// one was.
    fn select_carried(&self) -> bool {
        let keys = self.inner.carried.take();
        let model = &self.inner.model;
        let mut positions: Vec<u32> = keys
            .iter()
            .filter_map(|key| model.position_of_key(*key))
            .collect();
        // Two message rows of one conversation are its one row now: selected alone, not one
        // row ticked in the select mode (phase B's review).
        positions.sort_unstable();
        positions.dedup();
        if positions.is_empty() {
            return false;
        }
        self.select_all_of(&positions);
        true
    }

    /// Selects the rows at `positions` (the first on top): one alone, several in the select
    /// mode, the first under the cursor, in view and with the keyboard focus when the list has
    /// it.
    fn select_all_of(&self, positions: &[u32]) {
        let Some(first) = positions.first().copied() else {
            return;
        };
        let model = &self.inner.model;
        self.set_select_mode(positions.len() > 1);
        if let [position] = positions {
            self.select_position(*position);
            return;
        }
        if positions
            .iter()
            .any(|position| *position >= model.n_items())
        {
            model.complete_growth();
        }
        // One change for them all (an Undo of every row of a large folder).
        let chosen = gtk::Bitset::new_empty();
        for position in positions {
            chosen.add(*position);
        }
        self.inner.selection.set_selection(&chosen, &chosen);
        self.inner.cursor.set(model.key_at(first));
        let flags = if self.has_focus_within() {
            gtk::ListScrollFlags::FOCUS
        } else {
            gtk::ListScrollFlags::NONE
        };
        self.inner.view.scroll_to(first, flags, None);
    }

    /// The selected positions when there are at most [`KEPT_SELECTED`], else none: each is
    /// looked for among the rows again later, so a selection of every row (Select All at
    /// 100,000) isn't carried through a change.
    fn selected_positions_if_few(&self) -> Vec<u32> {
        if self.inner.selection.selection().size() > KEPT_SELECTED as u64 {
            return Vec::new();
        }
        self.selected_positions()
    }

    fn selected_keys_if_few(&self) -> Vec<RowKey> {
        self.selected_positions_if_few()
            .into_iter()
            .filter_map(|position| self.inner.model.key_at(position))
            .collect()
    }

    /// The selected positions, first to last.
    pub fn selected_positions(&self) -> Vec<u32> {
        let set = self.inner.selection.selection();
        match gtk::BitsetIter::init_first(&set) {
            Some((rest, first)) => std::iter::once(first).chain(rest).collect(),
            None => Vec::new(),
        }
    }

    /// The keys of the selected rows that are loaded.
    pub fn selected_keys(&self) -> Vec<RowKey> {
        self.selected_positions()
            .into_iter()
            .filter_map(|position| self.inner.model.key_at(position))
            .collect()
    }

    /// The message the reader shows for the row selected, when it is the only one: the
    /// message, or the message its conversation was opened at.
    pub fn selected(&self) -> Option<MessageId> {
        let position = self.selected_position()?;
        let key = self.inner.model.key_at(position)?;
        self.inner
            .open_at
            .get()
            .filter(|(wanted, _)| *wanted == key)
            .map(|(_, at)| at)
            .or_else(|| self.reported_message(key))
            .or_else(|| self.inner.model.message_at(position))
    }

    /// The row selected, when it is the only one.
    pub fn selected_row(&self) -> Option<ListRow> {
        self.selected_position()
            .and_then(|position| self.inner.model.list_row_at(position))
    }

    /// The selected position, when it is the only one.
    pub fn selected_position(&self) -> Option<u32> {
        match self.selected_positions().as_slice() {
            [position] => Some(*position),
            _ => None,
        }
    }

    /// The selected rows, or the positions whose rows aren't loaded yet.
    pub fn selected_rows(&self) -> Result<Vec<ListRow>, Vec<u32>> {
        let mut rows = Vec::new();
        let mut missing = Vec::new();
        for position in self.selected_positions() {
            match self.inner.model.list_row_at(position) {
                Some(row) => rows.push(row),
                None => missing.push(position),
            }
        }
        if missing.is_empty() {
            Ok(rows)
        } else {
            Err(missing)
        }
    }

    /// The selected rows, once the rows of those far down a range are loaded too; `None` when
    /// they don't come within [`TARGETS_WAIT`].
    pub async fn wait_rows(&self) -> Option<Vec<ListRow>> {
        let started = Instant::now();
        loop {
            match self.selected_rows() {
                Ok(rows) => return Some(rows),
                Err(_) if started.elapsed() > TARGETS_WAIT => return None,
                Err(missing) => {
                    for position in missing {
                        self.inner.model.request_position(position);
                    }
                    glib::timeout_future(Duration::from_millis(16)).await;
                }
            }
        }
    }

    /// The selected rows as availability sees them (one target a row: a conversation stands
    /// as its anchor with its members' state, [`Target::of_row`]), or the positions whose
    /// rows aren't loaded yet.
    pub fn selected_targets(&self) -> Result<Vec<Target>, Vec<u32>> {
        self.selected_rows()
            .map(|rows| rows.iter().map(Target::of_row).collect())
    }

    /// The row at `position`, as availability sees it.
    pub fn target_at(&self, position: u32) -> Option<Target> {
        self.inner
            .model
            .list_row_at(position)
            .map(|row| Target::of_row(&row))
    }

    /// The message the reader would show for the row at `position` (the reader lays out ahead
    /// the rows next to the selection, `Reader::prefetch`).
    pub fn message_at(&self, position: u32) -> Option<MessageId> {
        self.inner.model.message_at(position)
    }

    /// Where the row the reader shows `id` for is in the list, 1-based, and how many rows the
    /// list holds: the reader's "n of N" (conversations, with conversation rows). `None` when
    /// its row isn't loaded.
    pub fn position(&self, id: MessageId) -> Option<(u32, u32)> {
        let total = self.inner.model.total();
        if let Some(selected) = self.selected_position()
            && self.selected() == Some(id)
        {
            return Some((selected + 1, total));
        }
        self.inner
            .model
            .position_of(id)
            .map(|position| (position + 1, total))
    }

    /// Selects the row at `position` alone and opens it at `message` instead of its own
    /// message (a notification's new message in its conversation), as Enter does.
    pub fn open_at(&self, position: u32, message: MessageId) {
        if let Some(key) = self.inner.model.key_at(position) {
            self.inner.open_at.set(Some((key, message)));
            // The row the reader shows already, at another message (the cursor's row in
            // Focused, a reply's notification): reported again, at this one.
            if self
                .reported_message(key)
                .is_some_and(|shown| shown != message)
            {
                self.inner.reported.set(Selected::Nothing);
                self.inner.reported_key.set(None);
            }
        }
        self.inner.select_mode.set(false);
        self.select_position(position);
        self.settle();
        self.open_selected();
    }

    /// The row of `message` in this list: the message's own, or its conversation's (`thread`).
    pub fn key_for(&self, message: MessageId, thread: sealwax_domain::model::ThreadId) -> RowKey {
        match self.inner.model.grouping() {
            ViewGrouping::Conversations => RowKey::Conversation(thread),
            ViewGrouping::Messages => RowKey::Message(message),
        }
    }

    /// Selects the next message (Next Message: `j`, and GtkListView's own Down); in the select
    /// mode, moves the cursor.
    pub fn select_next(&self) {
        self.step(1);
    }

    /// Selects the previous message (Previous Message: `k`, and GtkListView's own Up); in the
    /// select mode, moves the cursor.
    pub fn select_previous(&self) {
        self.step(-1);
    }

    fn step(&self, delta: i64) {
        let total = self.inner.model.total();
        if total == 0 {
            return;
        }
        self.inner.wanted.borrow_mut().clear();
        if self.inner.select_mode.get() {
            let target = self
                .cursor_position()
                .map_or(0, |current| offset(current, delta, total));
            self.move_cursor(target);
            return;
        }
        let target = match self.selected_positions().as_slice() {
            // Nothing selected: the row the keyboard's focus is on, which shows the bar (phase
            // C; Split before a pick), else the first.
            [] => self
                .has_focus_within()
                .then(|| self.focused_row())
                .flatten()
                .unwrap_or(0)
                .min(total - 1),
            [first, ..] if delta < 0 => offset(*first, delta, total),
            positions => offset(positions[positions.len() - 1], delta, total),
        };
        self.select_position(target);
    }

    /// Extends the selection from where it started to the next or previous row (Shift with the
    /// arrows or with `j`/`k`), as Shift+click does.
    pub fn extend(&self, delta: i64) {
        let total = self.inner.model.total();
        if total == 0 {
            return;
        }
        self.inner.wanted.borrow_mut().clear();
        let Some(from) = self.cursor_position() else {
            self.select_position(0);
            return;
        };
        if self.selected_positions().is_empty() {
            self.select_position(from);
            return;
        }
        let target = offset(from, delta, total);
        if target >= self.inner.model.n_items() {
            self.inner.model.complete_growth();
        }
        self.select_item(target, false, true);
        self.move_cursor(target);
    }

    /// Select (`x`, Space): enters the select mode with the message under the cursor selected,
    /// or in the select mode selects or unselects it.
    pub fn toggle_selection(&self) {
        let total = self.inner.model.total();
        if total == 0 {
            return;
        }
        self.inner.wanted.borrow_mut().clear();
        let position = self.cursor_position().unwrap_or(0).min(total - 1);
        if position >= self.inner.model.n_items() {
            self.inner.model.complete_growth();
        }
        if let Some(key) = self.inner.model.key_at(position) {
            self.inner.cursor.set(Some(key));
        }
        if self.inner.select_mode.get() {
            self.select_item(position, true, false);
        } else {
            self.set_select_mode(true);
            if !self.inner.selection.is_selected(position) {
                self.select_item(position, true, false);
            }
            self.queue_settle();
        }
    }

    /// Leaves the select mode (Escape) with the message under the cursor selected alone.
    pub fn leave_select_mode(&self) {
        if !self.inner.select_mode.get() {
            return;
        }
        let position = self.cursor_position();
        self.set_select_mode(false);
        match position {
            Some(position) => self.select_position(position),
            None => {
                self.inner.selection.unselect_all();
            }
        }
        self.queue_settle();
    }

    /// The rows of `keys` are about to leave the list (an action on the selection). Once one
    /// has and nothing is selected any more, the row in the first one's place is selected.
    pub fn advance_after(&self, keys: &[RowKey]) {
        let keys: HashSet<RowKey> = keys.iter().copied().collect();
        let placed = self.inner.model.rows_of_keys(&keys, newest_member);
        let Some((position, _, _)) = placed.first().copied() else {
            return;
        };
        self.inner.returning.borrow_mut().clear();
        self.inner.leaving.replace(Some(Leaving {
            keys,
            newest: placed
                .into_iter()
                .map(|(_, key, newest)| (key, newest))
                .collect(),
            position,
            since: Instant::now(),
        }));
    }

    fn check_leaving(&self) {
        let ready = {
            let leaving = self.inner.leaving.borrow();
            let Some(leaving) = leaving.as_ref() else {
                return;
            };
            if leaving.since.elapsed() > LEAVING_WAIT {
                None
            } else if !self.selected_positions().is_empty()
                || self.inner.model.held(&leaving.keys) == leaving.keys.len()
            {
                return;
            } else {
                Some(leaving.position)
            }
        };
        let Some(leaving) = self.inner.leaving.take() else {
            return;
        };
        let total = self.inner.model.total();
        match ready {
            Some(position) if total > 0 => {
                self.set_select_mode(false);
                self.select_position(position.min(total - 1));
            }
            // The last rows left (Inbox Zero): an Undo brings them back selected.
            Some(_) => {
                self.inner.returning.replace(leaving.newest);
            }
            None => {}
        }
    }

    /// Rows came into a list with nothing selected: the last rows an action took out of it,
    /// when they left it empty, are selected again once they are back as they left (an Undo
    /// from Inbox Zero, in either mode): one alone (in Split, shown again), several in the
    /// select mode (nothing shown, nothing read), and the keyboard focus goes with them when
    /// the list had it. A conversation back with another newest member is new mail (a reply to
    /// one archived), which selects nothing in Split (phase B's review: it was shown and read).
    fn select_returning(&self) {
        let inner = &self.inner;
        if inner.returning.borrow().is_empty() || inner.model.is_pending() {
            return;
        }
        if inner.selection.selection().size() > 0 {
            // Something else took the cursor meanwhile (new mail in Focused): it stays.
            inner.returning.borrow_mut().clear();
            return;
        }
        let back: Vec<u32> = {
            let returning = inner.returning.borrow();
            let keys: HashSet<RowKey> = returning.keys().copied().collect();
            inner
                .model
                .rows_of_keys(&keys, newest_member)
                .into_iter()
                .filter(|(_, key, newest)| returning.get(key) == Some(newest))
                .map(|(position, _, _)| position)
                .collect()
        };
        if back.is_empty() {
            return;
        }
        inner.returning.borrow_mut().clear();
        let focus = self.holds_focus();
        self.select_all_of(&back);
        if focus {
            self.focus_selected();
        }
    }

    /// Selects the message at `position` alone and scrolls it into view; the keyboard focus
    /// moves with it only when it was in the list.
    pub fn select_position(&self, position: u32) {
        let inner = &self.inner;
        if position >= inner.model.total() {
            return;
        }
        if position >= inner.model.n_items() {
            inner.model.complete_growth();
        }
        inner.wanted.borrow_mut().clear();
        let focus = self.has_focus_within();
        self.select_item(position, false, false);
        if let Some(key) = inner.model.key_at(position) {
            inner.cursor.set(Some(key));
        }
        let flags = if focus {
            gtk::ListScrollFlags::FOCUS
        } else {
            gtk::ListScrollFlags::NONE
        };
        inner.view.scroll_to(position, flags, None);
    }

    /// GtkListView's own selection, as a click makes it: alone, toggled (`modify`) or from the
    /// anchor (`extend`); all but extending move the anchor.
    fn select_item(&self, position: u32, modify: bool, extend: bool) {
        if self
            .inner
            .view
            .activate_action(
                "list.select-item",
                Some(&(position, modify, extend).to_variant()),
            )
            .is_err()
        {
            tracing::warn!("the message list has no list.select-item");
        }
    }

    /// Moves the keyboard cursor without changing the selection (the select mode's `j`, `k`).
    fn move_cursor(&self, position: u32) {
        let inner = &self.inner;
        if position >= inner.model.n_items() {
            inner.model.complete_growth();
        }
        if let Some(key) = inner.model.key_at(position) {
            inner.cursor.set(Some(key));
        }
        let flags = if self.has_focus_within() {
            gtk::ListScrollFlags::FOCUS
        } else {
            gtk::ListScrollFlags::NONE
        };
        inner.view.scroll_to(position, flags, None);
    }

    /// The row the keyboard focus is on, else the cursor's, else the first selected.
    fn cursor_position(&self) -> Option<u32> {
        if self.has_focus_within()
            && let Some(position) = self.focused_row()
        {
            return Some(position);
        }
        self.inner
            .cursor
            .get()
            .and_then(|key| self.inner.model.position_of_key(key))
            .or_else(|| self.selected_positions().first().copied())
    }

    fn focused_row(&self) -> Option<u32> {
        let focus = self.inner.view.root()?.focus()?;
        self.inner
            .context
            .bound
            .borrow()
            .iter()
            .find(|row| row.is_ancestor(&focus) || focus == **row || focus.is_ancestor(*row))
            .and_then(MessageRow::position)
    }

    fn has_focus_within(&self) -> bool {
        let view = &self.inner.view;
        view.root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| {
                &focus == view.upcast_ref::<gtk::Widget>() || focus.is_ancestor(view)
            })
    }

    /// Clicks and keys of the selection: a plain click leaves the select mode; Shift with the
    /// arrows or with Next and Previous Message's keys extends; in the select mode the arrows
    /// move the cursor and Escape leaves; GtkListView's own Ctrl+A is no command here.
    fn connect_selection_input(&self) {
        let view = &self.inner.view;
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        // From the press to its release (or the gesture's end without one: a drag away, a
        // cancel), the focus the press gave its row isn't moved to the selection.
        let weak = self.downgrade();
        click.connect_begin(move |_, _| {
            Self::with(&weak, |list| list.inner.pointer_down.set(true));
        });
        let weak = self.downgrade();
        click.connect_end(move |_, _| {
            Self::with(&weak, |list| list.inner.pointer_down.set(false));
        });
        let weak = self.downgrade();
        click.connect_pressed(move |gesture, n_press, x, y| {
            let state = gesture.current_event_state();
            let modify = state.contains(gdk::ModifierType::CONTROL_MASK);
            let extend = state.contains(gdk::ModifierType::SHIFT_MASK);
            Self::with(&weak, |list| {
                let picked = list.inner.view.pick(x, y, gtk::PickFlags::DEFAULT);
                let control = picked.as_ref().is_some_and(|widget| {
                    widget.ancestor(gtk::CheckButton::static_type()).is_some()
                        || widget.ancestor(gtk::Button::static_type()).is_some()
                });
                let plain = !modify && !extend && !control;
                list.inner.plain_click.set(plain);
                // Ctrl and Shift clicks take effect here, on the press: GtkListView's own
                // Ctrl+click on a selected row selected it alone on the press and unselected it
                // on the release (seen headless, GTK 4.22).
                let position = picked
                    .and_then(|widget| widget.ancestor(MessageRow::static_type()))
                    .and_downcast::<MessageRow>()
                    .and_then(|row| row.position());
                list.inner
                    .pressed
                    .set(position.filter(|_| plain && n_press == 1));
                if (modify || extend)
                    && !control
                    && let Some(position) = position
                {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    list.inner.view.grab_focus();
                    list.select_item(position, modify, extend);
                    list.move_cursor(position);
                }
            });
        });
        // Focused: a plain click opens the row, as Superhuman's does (phase B); Ctrl, Shift and
        // the checkbox still select. GtkListView selects on the release, after this handler:
        // the row opens once that is done.
        let weak = self.downgrade();
        click.connect_released(move |gesture, n_press, x, y| {
            Self::with(&weak, |list| {
                let pressed = list.inner.pressed.take();
                if !list.inner.focused.get()
                    || n_press != 1
                    || !gesture.handles_sequence(gesture.current_sequence().as_ref())
                {
                    return;
                }
                let released = list
                    .inner
                    .view
                    .pick(x, y, gtk::PickFlags::DEFAULT)
                    .and_then(|widget| widget.ancestor(MessageRow::static_type()))
                    .and_downcast::<MessageRow>()
                    .and_then(|row| row.position());
                let Some(position) = pressed.filter(|pressed| Some(*pressed) == released) else {
                    return;
                };
                let weak = list.downgrade();
                glib::idle_add_local_once(move || {
                    Self::with(&weak, |list| list.click_open(position));
                });
            });
        });
        view.add_controller(click);
        // A right click opens the row's menu, on the row clicked.
        let menu = gtk::GestureClick::builder().button(3).build();
        menu.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        menu.connect_pressed(move |gesture, _, x, y| {
            Self::with(&weak, |list| {
                let position = list
                    .inner
                    .view
                    .pick(x, y, gtk::PickFlags::DEFAULT)
                    .and_then(|widget| widget.ancestor(MessageRow::static_type()))
                    .and_downcast::<MessageRow>()
                    .and_then(|row| row.position());
                if let Some(position) = position {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    list.open_context_menu(position, Some((x, y)));
                }
            });
        });
        view.add_controller(menu);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            MessageList { inner }.selection_key(key, state)
        });
        view.add_controller(keys);
    }

    fn selection_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        if state.contains(gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        // Menu and Shift+F10 open the menu of the row under the keyboard's cursor, as on the
        // sidebar's rows.
        if (key == gdk::Key::Menu || (key == gdk::Key::F10 && shift))
            && !state.contains(gdk::ModifierType::CONTROL_MASK)
        {
            return match self.cursor_position() {
                Some(position) if self.open_context_menu(position, None) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            };
        }
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            if key.to_lower() != gdk::Key::a {
                // GtkListView's Ctrl with the arrows moves its focus row without selecting
                // (Ctrl+Space then chooses it): the focus isn't sent back to the selection.
                if is_cursor_key(key) {
                    self.inner.cursor_keys.set(true);
                    let weak = self.downgrade();
                    glib::idle_add_local_once(move || {
                        Self::with(&weak, |list| list.inner.cursor_keys.set(false));
                    });
                }
                return glib::Propagation::Proceed;
            }
            // GtkListView's own Ctrl+A and Ctrl+Shift+A stay swallowed: they would select rows
            // that have no message yet, or clear the select mode behind its back. The very key
            // of a List command goes on to it (Superhuman pass: Select All on Ctrl+A, which
            // loads the rows first).
            let mut pressed = String::from("<Control>");
            if shift {
                pressed.push_str("<Shift>");
            }
            if state.contains(gdk::ModifierType::SUPER_MASK) {
                pressed.push_str("<Super>");
            }
            pressed.push('a');
            let ours = self
                .inner
                .keys
                .borrow()
                .as_ref()
                .is_some_and(|keys| keys.handles(&pressed));
            return if ours {
                glib::Propagation::Proceed
            } else {
                glib::Propagation::Stop
            };
        }
        let delta = match key {
            gdk::Key::Down => Some(1),
            gdk::Key::Up => Some(-1),
            _ if shift => self
                .inner
                .extend_keys
                .borrow()
                .iter()
                .find(|(bound, _)| *bound == key.to_lower())
                .map(|(_, delta)| *delta),
            _ => None,
        };
        match delta {
            Some(delta) if shift => self.extend(delta),
            Some(delta) if self.inner.select_mode.get() => self.step(delta),
            _ if key == gdk::Key::Escape && !shift && self.inner.select_mode.get() => {
                self.leave_select_mode();
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    /// A plain click on the row at `position` in Focused mode, once GtkListView selected it:
    /// it opens, as Enter does (nothing when the click selected something else meanwhile).
    fn click_open(&self, position: u32) {
        if self.selected_position() != Some(position) {
            return;
        }
        self.inner.double_clicks.borrow_mut().opened();
        self.settle();
        self.open_selected();
    }

    /// A double click on a row in Focused mode opens it on its first click ([`click_open`]),
    /// and its second press would land on the reader that now fills the list's place: on a
    /// link, an attachment's Open, a collapsed message (phase B's review). A press within GTK's
    /// double-click time and distance of the click that opened a row is swallowed, with its
    /// release, ahead of every widget of `window`.
    ///
    /// [`click_open`]: MessageList::click_open
    pub fn swallow_double_clicks(&self, window: &impl IsA<gtk::Window>) {
        let legacy = gtk::EventControllerLegacy::new();
        legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        legacy.connect_event(move |_, event| {
            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            // A key between the two (Escape back to the list) makes the next press a click of
            // its own.
            if event.event_type() == gdk::EventType::KeyPress {
                inner.double_clicks.borrow_mut().key();
            }
            let primary = event
                .downcast_ref::<gdk::ButtonEvent>()
                .is_some_and(|button| button.button() == gdk::BUTTON_PRIMARY);
            let Some((x, y)) = event.position().filter(|_| primary) else {
                return glib::Propagation::Proceed;
            };
            let time = event.time();
            let swallowed = match event.event_type() {
                gdk::EventType::ButtonPress => {
                    let (most_ms, most_px) =
                        gtk::Settings::default().map_or((400, 5), |settings| {
                            (
                                settings.gtk_double_click_time(),
                                settings.gtk_double_click_distance(),
                            )
                        });
                    inner
                        .double_clicks
                        .borrow_mut()
                        .press((x, y, time), most_ms, most_px)
                }
                gdk::EventType::ButtonRelease => {
                    inner.double_clicks.borrow_mut().release((x, y, time))
                }
                _ => false,
            };
            if swallowed {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.upcast_ref::<gtk::Window>().add_controller(legacy);
    }

    /// Opens the selected message (Open Message: Enter, `o`, or a double click; a click in
    /// Focused mode).
    pub fn open_selected(&self) {
        self.report_new_unread();
        if let Some(id) = self.selected() {
            let handlers = self.inner.handlers.opened.borrow().clone();
            for handler in handlers {
                handler(id);
            }
        }
    }

    /// The selected conversation, about to open, gained unread mail since it was reported (a
    /// reply came in under the cursor): its anchor moved to that first unread message, and it
    /// opens there, as a conversation selected afresh does. A conversation that gains a
    /// message stays reported as it was, so the reader follows it in place without reloading
    /// ([`MessageList::report`]); kept behind a Focused list, it opened at the old anchor, the
    /// new reply a collapsed row (phase C's review). Reported again here, the reader shows it
    /// at the new anchor in place ([`MessageList::open_at`]'s way), whatever message an
    /// earlier notification's click opened it at. An anchor that moved for another reason
    /// (marked read on open: the newest is the anchor then) isn't followed.
    fn report_new_unread(&self) {
        let inner = &self.inner;
        let Some(position) = self.selected_position() else {
            return;
        };
        let (Some(key), Some(ListRow::Conversation(row))) = (
            inner.model.key_at(position),
            inner.model.list_row_at(position),
        ) else {
            return;
        };
        let moved = self.reported_message(key).is_some()
            && inner
                .reported_anchor
                .get()
                .is_some_and(|anchor| anchor != row.anchor);
        if row.unread == 0 || !moved {
            return;
        }
        inner.open_at.set(None);
        inner.reported.set(Selected::Nothing);
        inner.reported_key.set(None);
        self.settle();
    }

    /// Runs `handler` with every message the user selects: clicks, keys, next and previous.
    pub fn connect_message_selected(&self, handler: impl Fn(MessageId) + 'static) {
        self.inner
            .handlers
            .selected
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when the user opens the selected message (Enter, `o`, double click).
    pub fn connect_message_opened(&self, handler: impl Fn(MessageId) + 'static) {
        self.inner
            .handlers
            .opened
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when no message is selected any more (another folder, the message gone).
    pub fn connect_selection_cleared(&self, handler: impl Fn() + 'static) {
        self.inner
            .handlers
            .cleared
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` once the rows of a view read anew are in (a reset), the selection settled
    /// on them: a reload that keeps the same row selected (a filter, the Split Inbox turned on)
    /// reports no new selection.
    pub fn connect_reset(&self, handler: impl Fn() + 'static) {
        self.inner
            .handlers
            .reset
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` with the count of selected messages while the list is in the select mode.
    pub fn connect_batch_selected(&self, handler: impl Fn(u32) + 'static) {
        self.inner
            .handlers
            .batch
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when a row's quick action is clicked: the command and that row, selected
    /// or not.
    pub fn connect_quick_action(&self, handler: impl Fn(ActionId, ListRow) + 'static) {
        self.inner
            .handlers
            .quick
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when a row's menu is asked for (a right click, Menu or Shift+F10), once
    /// the row is selected: with where the menu points, in the message list's coordinates.
    pub fn connect_context_menu(&self, handler: impl Fn(gdk::Rectangle) + 'static) {
        self.inner.handlers.menu.borrow_mut().push(Rc::new(handler));
    }

    /// Opens the menu of the row at `position`, at the point `at` (a right click) or under the
    /// row: a row not selected is selected alone first, as a click selects it; one in the
    /// selection keeps it, and the menu acts on every selected message. False when the row
    /// isn't on screen or nothing listens.
    pub fn open_context_menu(&self, position: u32, at: Option<(f64, f64)>) -> bool {
        let inner = &self.inner;
        if inner.handlers.menu.borrow().is_empty() || position >= inner.model.total() {
            return false;
        }
        if !inner.selection.is_selected(position) {
            inner.view.grab_focus();
            inner.plain_click.set(true);
            self.select_position(position);
            self.settle();
        }
        let view = inner.view.upcast_ref::<gtk::Widget>();
        let rect = match at {
            Some((x, y)) => gdk::Rectangle::new(x as i32, y as i32, 1, 1),
            None => {
                let Some(bounds) = self
                    .row_widget(position)
                    .and_then(|row| row.compute_bounds(view))
                else {
                    return false;
                };
                gdk::Rectangle::new(
                    bounds.x() as i32 + 24,
                    bounds.y() as i32,
                    1,
                    bounds.height() as i32,
                )
            }
        };
        let handlers = inner.handlers.menu.borrow().clone();
        for handler in handlers {
            handler(rect);
        }
        true
    }

    /// Shows the quick actions' keys in their tooltips (`keys.toml`).
    pub fn set_quick_tooltips(&self, tooltips: Vec<(Quick, String)>) {
        self.inner.context.set_quick_tooltips(tooltips);
    }

    /// Runs `handler` when search results come or go (`None`: the folder shows again).
    pub fn connect_search_changed(&self, handler: impl Fn(Option<&SearchHeading>) + 'static) {
        self.inner
            .handlers
            .search
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// The List scope's keys on the pane (after its search field) and on the list view itself
    /// (before GtkListView's own Enter and Space), as actions.md describes.
    pub fn install_keys(&self, keymap: &Keymap) {
        let keys = actions::install_scoped(&self.inner.root, Scope::List, keymap);
        keys.attach(&self.inner.view, gtk::PropagationPhase::Capture);
        self.inner.keys.replace(Some(keys));
        self.inner.extend_keys.replace(extend_keys(keymap));
        self.set_search_key(keymap);
    }

    /// `keys.toml` changed.
    pub fn update_keys(&self, keymap: &Keymap) {
        if let Some(keys) = self.inner.keys.borrow().as_ref() {
            keys.update(keymap);
        }
        self.inner.extend_keys.replace(extend_keys(keymap));
        self.set_search_key(keymap);
    }

    /// The Search command's first key at the end of the empty search field.
    fn set_search_key(&self, keymap: &Keymap) {
        let key = keymap
            .shown(ActionId::Search)
            .first()
            .map(sealwax_domain::keymap::Binding::display)
            .unwrap_or_default();
        self.inner.search_key.set_label(&key);
        self.show_search_key(self.inner.search.text().is_empty());
        self.inner.zero.set_keymap(keymap);
        self.set_split_keys(keymap);
    }

    pub fn keys(&self) -> Option<std::cell::Ref<'_, ScopedShortcuts>> {
        std::cell::Ref::filter_map(self.inner.keys.borrow(), Option::as_ref).ok()
    }

    fn now(&self) -> glib::DateTime {
        self.inner
            .clock
            .get()
            .and_then(|unix| glib::DateTime::from_unix_local(unix).ok())
            .or_else(|| glib::DateTime::now_local().ok())
            .unwrap_or_else(|| glib::DateTime::from_unix_utc(0).expect("the epoch is a valid date"))
    }

    /// When scheduled sends go (Unix ms), by their Drafts-folder copies (`outbox`): those rows
    /// say "Sending Tue 08:00" in their date's place.
    pub fn set_ai_drafts(
        &self,
        drafts: std::collections::HashSet<sealwax_domain::model::ThreadId>,
    ) {
        self.inner.context.set_ai_drafts(drafts);
    }

    pub fn set_scheduled(&self, scheduled: std::collections::HashMap<MessageId, i64>) {
        self.inner.context.set_scheduled(scheduled);
    }

    /// "Now" as the list sees it (Unix seconds).
    pub fn now_unix(&self) -> i64 {
        self.now().to_unix()
    }

    /// The sections for today.
    pub fn thresholds_now(&self) -> Thresholds {
        let now = self.now();
        self.inner.context.year.set(now.year());
        Thresholds::at(&now).unwrap_or_else(|_| Thresholds::new(0, 0, 0, 0))
    }

    /// Fixes "now" (Unix seconds; `None`: the real clock) and regroups, as midnight does (the
    /// self-test).
    pub fn set_clock(&self, unix: Option<i64>) {
        self.inner.clock.set(unix);
        self.inner.context.clock.set(unix);
        self.regroup_if_needed();
    }

    fn regroup_if_needed(&self) {
        // Inbox Zero's day too, for a list left empty past midnight.
        self.inner.zero.set_date(&self.now());
        // The Reminders view's wake times say their day from today (phase C).
        if self.inner.context.reminders().wakes {
            let rows = self.inner.context.bound.borrow().clone();
            for row in rows {
                row.refresh();
            }
        }
        let wanted = self.thresholds_now();
        if wanted != self.inner.model.thresholds() {
            tracing::debug!("the day changed; regrouping the list");
            self.inner.model.regroup(wanted);
        }
    }

    fn schedule_day_check(&self) {
        let delay = until_next_day(&self.now()).min(DAY_CHECK) + Duration::from_secs(1);
        let weak = self.downgrade();
        let source = glib::timeout_add_local_once(delay, move || {
            Self::with(&weak, |list| {
                list.inner.day_check.take();
                list.regroup_if_needed();
                list.schedule_day_check();
            });
        });
        if let Some(previous) = self.inner.day_check.replace(Some(source)) {
            previous.remove();
        }
    }

    /// The rows GtkListView has bound now.
    pub fn bound_rows(&self) -> Vec<BoundRow> {
        self.inner
            .context
            .bound
            .borrow()
            .iter()
            .map(|row| BoundRow {
                position: row.position(),
                id: row.message(),
                key: row.key(),
                placeholder: row.is_placeholder(),
            })
            .collect()
    }

    /// The bound row at `position`.
    pub fn row_widget(&self, position: u32) -> Option<MessageRow> {
        self.inner
            .context
            .bound
            .borrow()
            .iter()
            .find(|row| row.position() == Some(position))
            .cloned()
    }

    /// The section headers in view now, top to bottom.
    pub fn headers(&self) -> Vec<String> {
        let scroller = &self.inner.scroller;
        let height = scroller.height() as f32;
        let mut headers: Vec<(f32, String)> = self
            .inner
            .context
            .headers
            .borrow()
            .iter()
            .filter(|header| header.is_mapped())
            .filter_map(|header| {
                let bounds = header.compute_bounds(scroller)?;
                let visible = bounds.y() >= 0.0 && bounds.y() + bounds.height() <= height;
                visible.then(|| (bounds.y(), header.text()))
            })
            .collect();
        headers.sort_by(|a, b| a.0.total_cmp(&b.0));
        headers.into_iter().map(|(_, text)| text).collect()
    }

    /// Rows bound so far.
    pub fn binds(&self) -> u64 {
        self.inner.context.binds.get()
    }

    /// Row widgets created so far.
    pub fn rows_created(&self) -> u64 {
        self.inner.context.rows_created.get()
    }

    pub fn folder_of(scope: ViewScope) -> Option<FolderId> {
        match scope {
            ViewScope::Folder(folder) => Some(folder),
            ViewScope::UnifiedInbox => None,
        }
    }
}

/// The scope: the folder shown and All folders, as two quiet toggles after the field in the
/// header row, which a screen reader hears as "Search in" (the visual caption went with the
/// second row of chips, phase B).
fn scope_row() -> (gtk::Box, gtk::ToggleButton, gtk::ToggleButton) {
    let folder = gtk::ToggleButton::builder().active(true).build();
    folder.add_css_class("sw-chip");
    folder.set_widget_name("search-scope-folder");
    let all = gtk::ToggleButton::builder().label(SCOPE_ALL).build();
    all.set_group(Some(&folder));
    all.add_css_class("sw-chip");
    all.set_widget_name("search-scope-all");
    let row = gtk::Box::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    row.add_css_class("sw-search-scope");
    row.set_widget_name("list-search-scope");
    row.update_property(&[gtk::accessible::Property::Label("Search in")]);
    row.append(&folder);
    row.append(&all);
    (row, folder, all)
}

/// Why a search shows nothing, in words: the filter that narrowed it, where it looked and what
/// it looked for ("No unread conversations in Inbox match “contract”."; phase C, the visual QA:
/// a filter narrowed the search with no word of it).
fn no_results(
    text: &str,
    folder: Option<&str>,
    filter: ViewFilter,
    grouping: ViewGrouping,
) -> String {
    let rows = match grouping {
        ViewGrouping::Conversations => "conversations",
        ViewGrouping::Messages => "messages",
    };
    let what = match filter {
        ViewFilter::Unread => format!("unread {rows}"),
        ViewFilter::Starred => format!("starred {rows}"),
        ViewFilter::Attachments => format!("{rows} with attachments"),
        ViewFilter::All | ViewFilter::Important | ViewFilter::Other => rows.to_owned(),
    };
    let place = folder
        .map(|folder| format!(" in {folder}"))
        .unwrap_or_default();
    // A long query is cut: the line is one line under the empty state's title.
    let mut query: String = text.chars().take(40).collect();
    if query.len() < text.len() {
        query.push('…');
    }
    format!("No {what}{place} match “{query}”.")
}

/// Where and when a primary button event was: the window surface's coordinates, its time (ms).
type Click = (f64, f64, u32);

/// The primary button's presses and releases as Focused mode's double click sees them
/// ([`MessageList::swallow_double_clicks`]): the last release, the release of the click that
/// last opened a row, and whether a second press was swallowed, whose release goes too.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct DoubleClicks {
    last_release: Option<Click>,
    opened: Option<Click>,
    swallowing: bool,
}

impl DoubleClicks {
    /// The click just released opened a row.
    fn opened(&mut self) {
        self.opened = self.last_release;
    }

    /// A key between two presses makes the next one a click of its own.
    fn key(&mut self) {
        self.opened = None;
    }

    /// A press: swallowed when it is the second of a double click (within `most_ms` and
    /// `most_px` of the release that opened a row). Any press starts afresh: a swallowed press
    /// whose release never came (the window unmapped, a grab broken) doesn't eat this one's
    /// (phase B's review).
    fn press(&mut self, (x, y, time): Click, most_ms: i32, most_px: i32) -> bool {
        let second = self.opened.take().is_some_and(|(at_x, at_y, at)| {
            i64::from(time.wrapping_sub(at)) <= i64::from(most_ms)
                && (x - at_x).abs() <= f64::from(most_px)
                && (y - at_y).abs() <= f64::from(most_px)
        });
        self.swallowing = second;
        second
    }

    /// A release: swallowed with its press.
    fn release(&mut self, click: Click) -> bool {
        if std::mem::take(&mut self.swallowing) {
            return true;
        }
        self.last_release = Some(click);
        false
    }
}

/// `current` moved by `delta`, kept inside a list of `total` rows.
fn offset(current: u32, delta: i64, total: u32) -> u32 {
    (i64::from(current) + delta).clamp(0, i64::from(total) - 1) as u32
}

/// The keys GtkListView moves its focus row by: with Ctrl, without selecting.
fn is_cursor_key(key: gdk::Key) -> bool {
    matches!(
        key,
        gdk::Key::Up
            | gdk::Key::Down
            | gdk::Key::Page_Up
            | gdk::Key::Page_Down
            | gdk::Key::Home
            | gdk::Key::End
            | gdk::Key::KP_Up
            | gdk::Key::KP_Down
            | gdk::Key::KP_Page_Up
            | gdk::Key::KP_Page_Down
            | gdk::Key::KP_Home
            | gdk::Key::KP_End
    )
}

/// The one-key keys of Next and Previous Message, which extend the selection with Shift.
fn extend_keys(keymap: &Keymap) -> Vec<(gdk::Key, i64)> {
    [(ActionId::NextMessage, 1), (ActionId::PreviousMessage, -1)]
        .into_iter()
        .flat_map(|(id, delta)| {
            keymap
                .keys(id)
                .into_iter()
                .filter_map(|binding| binding.single().cloned())
                .filter(|key| key.is_one_key() && !key.shift)
                .filter_map(|key| gdk::Key::from_name(&key.key))
                .map(move |key| (key.to_lower(), delta))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_double_clicks_second_press_is_swallowed_with_its_release_only() {
        let mut clicks = DoubleClicks::default();
        // A click opens a row; the second press, close and soon, goes, its release too.
        assert!(!clicks.press((10.0, 10.0, 1000), 400, 5));
        assert!(!clicks.release((10.0, 10.0, 1050)));
        clicks.opened();
        assert!(clicks.press((11.0, 10.0, 1200), 400, 5));
        assert!(clicks.release((11.0, 10.0, 1250)));
        // The next click is a click.
        assert!(!clicks.press((11.0, 10.0, 1300), 400, 5));
        assert!(!clicks.release((11.0, 10.0, 1350)));
        // Too late, too far, or after a key: a click of its own.
        clicks.opened();
        assert!(!clicks.press((11.0, 10.0, 1900), 400, 5));
        assert!(!clicks.release((11.0, 10.0, 1950)));
        clicks.opened();
        assert!(!clicks.press((40.0, 10.0, 2000), 400, 5));
        assert!(!clicks.release((40.0, 10.0, 2050)));
        clicks.opened();
        clicks.key();
        assert!(!clicks.press((40.0, 10.0, 2100), 400, 5));
    }

    #[test]
    fn a_swallowed_press_whose_release_never_came_eats_nothing_more() {
        let mut clicks = DoubleClicks::default();
        clicks.release((10.0, 10.0, 1000));
        clicks.opened();
        // The second press is swallowed, and its release never reaches the window.
        assert!(clicks.press((10.0, 10.0, 1100), 400, 5));
        // An unrelated click later, elsewhere: its press and its release both go through.
        assert!(!clicks.press((300.0, 200.0, 9000), 400, 5));
        assert!(
            !clicks.release((300.0, 200.0, 9050)),
            "the next release isn't eaten for the lost one"
        );
    }

    #[test]
    fn offsets_stay_inside_the_list() {
        assert_eq!(offset(0, -1, 10), 0);
        assert_eq!(offset(9, 1, 10), 9);
        assert_eq!(offset(4, 1, 10), 5);
    }

    #[test]
    fn shift_extends_with_next_and_previous_messages_one_key_keys() {
        assert_eq!(
            extend_keys(&Keymap::default()),
            [(gdk::Key::j, 1), (gdk::Key::k, -1)]
        );
        let rebound = Keymap::with_overrides([(
            ActionId::NextMessage,
            vec![sealwax_domain::keymap::Binding::parse("n").unwrap()],
        )]);
        assert_eq!(extend_keys(&rebound), [(gdk::Key::n, 1), (gdk::Key::k, -1)]);
    }

    #[test]
    fn search_headings_count_their_results() {
        let heading = |results, more, searching| SearchHeading {
            results,
            more,
            searching,
            everywhere: false,
        };
        assert_eq!(heading(0, false, true).subtitle(), "Searching…");
        assert_eq!(heading(0, false, false).subtitle(), "No results");
        assert_eq!(heading(1, false, false).subtitle(), "1 result");
        assert_eq!(heading(12, false, true).subtitle(), "12 results");
        assert_eq!(heading(50, true, false).subtitle(), "50+ results");
        let everywhere = |results, more, searching| SearchHeading {
            everywhere: true,
            ..heading(results, more, searching)
        };
        assert_eq!(
            everywhere(0, false, true).subtitle(),
            "Searching all folders…"
        );
        assert_eq!(
            everywhere(0, false, false).subtitle(),
            "No results in all folders"
        );
        assert_eq!(
            everywhere(50, true, false).subtitle(),
            "50+ results in all folders"
        );
    }

    #[test]
    fn an_empty_search_says_what_narrowed_it() {
        let conversations = ViewGrouping::Conversations;
        assert_eq!(
            no_results("contract", Some("Inbox"), ViewFilter::Unread, conversations),
            "No unread conversations in Inbox match “contract”."
        );
        assert_eq!(
            no_results("contract", None, ViewFilter::All, ViewGrouping::Messages),
            "No messages match “contract”."
        );
        assert_eq!(
            no_results("x", Some("Archive"), ViewFilter::Attachments, conversations),
            "No conversations with attachments in Archive match “x”."
        );
        let long = "a".repeat(60);
        assert!(no_results(&long, None, ViewFilter::Starred, conversations).ends_with("a…”."));
    }

    #[test]
    fn filters_name_the_store_filters() {
        assert_eq!(filter_name(ViewFilter::Unread), "Unread");
        assert_eq!(FILTERS.len(), 4);
        // The Split Inbox's sides as filters (phase B's backend) never show as "All".
        assert_eq!(filter_name(ViewFilter::Important), "Important");
        assert_eq!(filter_name(ViewFilter::Other), "Other");
    }
}
