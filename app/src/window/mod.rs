//! The main window (ML-021): folders, the message list and the reader (DESIGN.md "Layout").
//! Since the Superhuman pass (2026-10-28) it shows one thing at a time by default: the list
//! fills the window, and opening a conversation shows it in the list's place (Focused); the
//! reading pane, the reader beside the list, is a toggle (Split, [`ViewMode`]). The sidebar is
//! a toggle too, hidden by default: beside the list in a wide window (≥ 1280 px), over it in a
//! narrower one. Below 900 px both modes show one pane at a time. On Hyprland there is no title
//! bar and there are no window controls; elsewhere the pane bars carry the controls at the
//! window's edges and drag the window.
//!
//! The `nav` action group drives the navigation: `nav.folder` (the folder shown, by key),
//! `nav.pane` (`folders`, `list` or `reader`: what one pane shows, and the sidebar over the
//! list), `nav.more` (opens or closes an account's "More" folders) and `nav.collapse` (folds an
//! account's folders away under its heading, or opens them).

pub mod banner;
pub mod dock;
pub mod empty;
pub mod glyph;
mod mail_panes;
pub mod margin;
pub mod panes;
pub mod resize;
pub mod sidebar;
pub mod split_tabs;
pub mod tooltips;

use banner::Banner;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use mail_panes::MailPanes;
use panes::{PaneBar, ReaderColumn, classed, icon, icon_button, menu_button, named, title_block};
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use sealwax_domain::layout::{LIST_WIDTHS, SIDEBAR_WIDTHS};
use sealwax_domain::split::Split;
use sealwax_domain::theme::Theme;
use sealwax_infrastructure::store::SplitCounts;
use sidebar::{COMPOSE_ICONS, Sidebar};
use split_tabs::SplitTabs;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tooltips::Tooltips;

pub use banner::{Attention, BannerAction};
pub use panes::READER_COLUMN;
pub use sealwax_domain::layout::ViewMode;
pub use sidebar::{
    ALL_INBOXES, AccountStatus, FolderRole, SidebarAccount, SidebarFolder, SidebarModel,
};

/// Registry actions the window's buttons are bound to (ML-022). A button whose action is not
/// installed stays insensitive: compose until the composer exists (M6).
pub const COMPOSE_ACTION: &str = "win.compose";
pub const OPEN_CONFIG_ACTION: &str = "app.preferences";
/// The welcome page's "Add an account": the setup assistant (ML-033).
pub const ADD_ACCOUNT_ACTION: &str = "win.add-account";
/// The list bar's sidebar button and its search button.
pub const TOGGLE_SIDEBAR_ACTION: &str = "win.toggle-sidebar";
pub const SEARCH_ACTION: &str = "win.search";

/// Pane widths in px (DESIGN.md "Layout").
pub const SIDEBAR_WIDTH: f64 = 220.0;
pub const LIST_WIDTH: f64 = 380.0;
/// The least room a dragged border leaves the reader, in px; more when its toolbar needs it.
pub const READER_MIN_WIDTH: f64 = 360.0;
/// Narrower windows show the sidebar over the list instead of beside it (the 56 px rail of
/// icons went with the Superhuman pass)…
pub const WIDE_WIDTH: f64 = 1280.0;
/// …and narrower still, one pane at a time in Split too.
pub const NARROW_WIDTH: f64 = 900.0;
/// One pane still fits (GNOME's smallest window, 360×294).
pub const MIN_SIZE: (i32, i32) = (360, 294);
pub const DEFAULT_SIZE: (i32, i32) = (1280, 800);

/// The window's width class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// ≥ 1280 px: the sidebar, when shown, sits beside the list.
    Wide,
    /// 900–1279 px: the sidebar, when shown, lies over the list.
    Medium,
    /// < 900 px: one pane at a time.
    Narrow,
}

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Self::Wide => "wide",
            Self::Medium => "medium",
            Self::Narrow => "narrow",
        }
    }
}

/// What one pane shows (the list or the reader), or the folders over it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Folders,
    List,
    Reader,
}

impl Pane {
    pub fn name(self) -> &'static str {
        match self {
            Self::Folders => "folders",
            Self::List => "list",
            Self::Reader => "reader",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Folders, Self::List, Self::Reader]
            .into_iter()
            .find(|pane| pane.name() == name)
    }
}

/// A stop of Next Pane and Previous Pane (F6, Shift+F6; phase C): the window's panes in reading
/// order, the contact pane in the conversation's margin last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneStop {
    Folders,
    List,
    Conversation,
    Contact,
}

/// Where the sidebar is: beside the list, over it, or nowhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarPlace {
    Inline,
    Overlay,
    Hidden,
}

impl SidebarPlace {
    pub fn name(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Overlay => "overlay",
            Self::Hidden => "hidden",
        }
    }
}

/// Hyprland tiles windows and draws no title bars, so Pronto shows none either (DESIGN.md
/// "Layout"); detected as in Stet.
pub fn tiling_desktop() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        || std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktop| desktop.contains("Hyprland"))
}

type FolderHandler = Box<dyn Fn(&Window, &str)>;
type WidthsHandler = Rc<dyn Fn(f64, f64)>;
type FlagHandler = Rc<dyn Fn(bool)>;
type SidebarHandler = Rc<dyn Fn(bool, bool)>;
type Handler = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct Window {
    inner: Rc<Inner>,
}

struct Inner {
    window: adw::ApplicationWindow,
    /// Folders | mail: beside each other in a wide window, the folders over the mail below.
    panes: adw::OverlaySplitView,
    /// List | reader, or one of them.
    mail: MailPanes,
    medium: adw::Breakpoint,
    narrow: adw::Breakpoint,
    sidebar: Rc<Sidebar>,
    /// `welcome` (no account) or `mail`.
    content: gtk::Stack,
    list_pane: adw::ToolbarView,
    list_bar: PaneBar,
    /// The list bar's sidebar button, and the dot it shows while an account needs the user and
    /// the sidebar is hidden.
    list_sidebar: gtk::ToggleButton,
    attention: gtk::Box,
    list_title: gtk::Label,
    list_subtitle: gtk::Label,
    /// The title, the subtitle and the filter pill: the search field takes their place while a
    /// search shows, the select mode's bar while rows are chosen (phase B).
    list_heading: gtk::Box,
    /// The Split Inbox's tabs, in the title's and the subtitle's place in an Inbox view with
    /// the splits on (phase C), with a pill of their own after them (the filter on the side
    /// shown); whether they show, and the filter the pills name.
    split_tabs: SplitTabs,
    split_pill: gtk::Button,
    split_pill_label: gtk::Label,
    shows_splits: Cell<bool>,
    filter_name: RefCell<Option<String>>,
    /// The sidebar's accounts: with several, the tabs say whose Inbox they are.
    accounts: Cell<usize>,
    /// Holds the list's search field and scope (`set_list_search`), which the list shows while
    /// a search does; it steps aside while the select mode's bar fills the row.
    list_search_slot: gtk::Box,
    list_search: gtk::Button,
    list_compose: gtk::Button,
    /// The list shows conversations, and how many once known: the heading counts them.
    list_count: Cell<(bool, Option<u32>)>,
    /// The filter the list shows, as a pill that removes it.
    filter_pill: gtk::Button,
    filter_label: gtk::Label,
    /// The list's search shows in the header row.
    list_searching: Cell<bool>,
    /// The reader before a message is picked; blank beside a search that found nothing.
    reader_empty: gtk::Widget,
    batch_handlers: RefCell<Vec<Handler>>,
    list_slot: adw::Bin,
    list_menu: gtk::MenuButton,
    welcome_menu: gtk::MenuButton,
    welcome_bar: PaneBar,
    reader_area: gtk::Overlay,
    reader_pane: adw::ToolbarView,
    reader_bar: PaneBar,
    reader_toolbar: adw::Bin,
    /// Holds Back and the toolbar to the thread column's width.
    reader_strip_clamp: adw::Clamp,
    /// `empty`, `message` or `batch`.
    reader: gtk::Stack,
    column: ReaderColumn,
    /// The `message` page: the column's scroller, and the contact pane in its right margin.
    margin: margin::ColumnMargin,
    /// The reader while several messages are selected.
    batch: empty::BatchPage,
    welcome: adw::ToolbarView,
    /// "or press Ctrl+Shift+P / F1 for every command", with the keys in use.
    welcome_hint: gtk::Label,
    /// The most important account or desktop state (ML-032, ML-048), over every pane.
    banner: Banner,
    /// The composer's cards over the panes.
    dock: dock::Dock,
    toasts: adw::ToastOverlay,
    folder: gio::SimpleAction,
    pane: gio::SimpleAction,
    tooltips: Rc<Tooltips>,
    /// Widgets the self-test finds by name.
    named: Vec<gtk::Widget>,
    folder_handlers: RefCell<Vec<FolderHandler>>,
    /// No view was picked yet but the one the window opens on while its first account has no
    /// Inbox (All Inboxes: its folders come a server round trip after it): the first model
    /// with one shows the window's own pick ([`SidebarModel::default_selection`]), however the
    /// reads were batched (phase C's deterministic first view).
    provisional: Cell<bool>,
    /// The widths the sidebar and the message list were dragged to (DESIGN.md "Layout").
    sidebar_width: Cell<f64>,
    list_width: Cell<f64>,
    /// The drag handles on the left edges of the mail area and the reader.
    sidebar_handle: gtk::Box,
    list_handle: gtk::Box,
    /// Told the widths when a drag ends or a double click resets one.
    widths_changed: RefCell<Option<WidthsHandler>>,
    mode: Cell<ViewMode>,
    /// The sidebar beside the list while the window is wide (Toggle Sidebar, layout.toml).
    sidebar_wanted: Cell<bool>,
    /// What one pane shows: the list or the reader.
    page: Cell<Pane>,
    /// The list shows an empty state: Split gives it the whole area, no reader beside it.
    list_empty: Cell<bool>,
    /// An account needs the user (the sidebar says which).
    needs_attention: Cell<bool>,
    /// The reader is on screen, which marks what it shows read (ML-057).
    on_screen: Cell<bool>,
    screen_handlers: RefCell<Vec<FlagHandler>>,
    /// Told whether the sidebar shows, and whether to remember it; what they were told last.
    sidebar_handlers: RefCell<Vec<SidebarHandler>>,
    sidebar_told: Cell<Option<bool>>,
    filter_handlers: RefCell<Vec<Handler>>,
    /// The chrome is being set: the split view's own notifications are ours.
    applying: Cell<bool>,
    /// The width class is changing while libadwaita hides the window's content for a frame
    /// (`switch_class`); then a switch waits for the content to show again, and a move of
    /// the focus that was asked for meanwhile is made then.
    switching: Cell<bool>,
    switched: Cell<bool>,
    refocus: Cell<bool>,
}

impl Window {
    /// `tiling`: no title bar and no window controls (Hyprland, [`tiling_desktop`]).
    pub fn new(app: &adw::Application, tiling: bool) -> Self {
        let tooltips = Rc::new(Tooltips::default());
        let sidebar = Sidebar::new(tiling, &tooltips);

        // The list's one header row: the sidebar button in the rows' unread-dot gutter, the
        // view's name and its quiet count on the names' column, the filter it shows, then
        // search, compose and the main menu, their icons ending where the rows' dates do (phase
        // B, the visual QA). The search field (the list's, `set_list_search`) and the select
        // mode's bar take the name's place while they show.
        let list_bar = PaneBar::new(tiling, None, &["sw-list-bar"]);
        let (list_sidebar, attention) = sidebar_toggle(&tooltips);
        list_bar.content.append(&list_sidebar);
        let list_search_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        list_bar.content.append(&list_search_slot);
        let list_title = named(gtk::Label::new(None), "list-title");
        let list_subtitle = named(gtk::Label::new(None), "list-subtitle");
        let heading = title_block(&list_title, &list_subtitle);
        let split_tabs = SplitTabs::new();
        heading.append(&split_tabs);
        let (split_pill, split_pill_label) = filter_pill();
        split_pill.set_widget_name("list-split-filter");
        split_pill_label.set_widget_name("list-split-filter-name");
        split_tabs.set_pill(&split_pill);
        let (filter_pill, filter_label) = filter_pill();
        heading.append(&filter_pill);
        list_bar.content.append(&heading);
        let batch = empty::BatchPage::new();
        batch.widget.set_widget_name("reader-batch");
        list_bar.content.append(&batch.bar.widget);
        let list_search = named(
            icon_button(&["system-search-symbolic", "edit-find-symbolic"], "Search"),
            "list-search-button",
        );
        list_search.set_action_name(Some(SEARCH_ACTION));
        list_search.set_valign(gtk::Align::Center);
        tooltips.add(&list_search, "Search", ActionId::Search);
        let list_compose = named(icon_button(COMPOSE_ICONS, "New message"), "list-compose");
        list_compose.set_action_name(Some(COMPOSE_ACTION));
        list_compose.set_valign(gtk::Align::Center);
        tooltips.add(&list_compose, "New message", ActionId::Compose);
        let list_menu = named(menu_button(), "list-menu");
        list_menu.set_valign(gtk::Align::Center);
        let tools = classed(
            gtk::Box::builder()
                .halign(gtk::Align::End)
                .hexpand(true)
                .build(),
            &["sw-list-tools"],
        );
        tools.append(&list_search);
        tools.append(&list_compose);
        tools.append(&list_menu);
        list_bar.content.append(&tools);
        let list_slot = named(
            adw::Bin::builder()
                .vexpand(true)
                .child(
                    empty::folder("No messages", "Messages in this folder appear here.").widget(),
                )
                .build(),
            "list-slot",
        );
        let list_pane = named(
            classed(
                adw::ToolbarView::builder().content(&list_slot).build(),
                &["sw-list-pane"],
            ),
            "list-pane",
        );
        list_pane.add_top_bar(&list_bar.root);

        let reader_back = named(
            icon_button(&["go-previous-symbolic"], "Back to the list"),
            "reader-back",
        );
        // A click leaves the focus where it was; the keyboard's Back is Escape.
        reader_back.set_focus_on_click(false);
        // As high as wide, centred in the bar, as the toolbar's buttons are.
        reader_back.set_valign(gtk::Align::Center);
        tooltips.add(&reader_back, "Back to the list", ActionId::BackToList);
        let reader_bar = PaneBar::new(
            tiling,
            Some((reader_back, Pane::List.name())),
            &["sw-toolbar"],
        );
        let reader_toolbar = named(adw::Bin::builder().hexpand(true).build(), "reader-toolbar");
        // Back and the toolbar line up with the thread's column (the Superhuman pass's review:
        // at 1440 px they sat 330 px out from the text on each side): a clamp as wide as the
        // conversation, centred as it is ([`Window::follow_reader_column`]).
        let reader_strip = gtk::Box::builder().hexpand(true).build();
        if let Some(back) = &reader_bar.back {
            back.unparent();
            reader_strip.append(back);
        }
        reader_strip.append(&reader_toolbar);
        // Its margins (style.css `.sw-reader-strip`) and the bar's padding are the column's own,
        // so a pane narrower than the conversation narrows both alike (Split at 1100 px: Next
        // ended 10 px past the column).
        let reader_strip_clamp = classed(
            adw::Clamp::builder()
                .maximum_size(READER_COLUMN)
                .tightening_threshold(READER_COLUMN)
                .unit(adw::LengthUnit::Px)
                .hexpand(true)
                .child(&reader_strip)
                .build(),
            &["sw-reader-strip"],
        );
        reader_bar.content.append(&reader_strip_clamp);
        let column = ReaderColumn::new();
        column.slot.set_widget_name("reader-column");
        let margin = named(
            margin::ColumnMargin::new(&column.root, &column.scroller, &column.inset),
            "reader-margin",
        );
        let reader_empty = named(empty::no_message(), "reader-empty");
        let reader = gtk::Stack::builder().vexpand(true).build();
        reader.add_named(&reader_empty, Some("empty"));
        reader.add_named(&margin, Some("message"));
        reader.add_named(&batch.widget, Some("batch"));
        reader.set_visible_child_name("empty");
        let reader_pane = named(
            classed(
                adw::ToolbarView::builder().content(&reader).build(),
                &["sw-reader"],
            ),
            "reader-pane",
        );
        reader_pane.add_top_bar(&reader_bar.root);
        let list_handle = resize::handle("list-handle");
        let reader_area = gtk::Overlay::builder()
            .child(&reader_pane)
            .hexpand(true)
            .build();
        reader_area.add_overlay(&list_handle);

        let welcome_bar = PaneBar::new(tiling, None, &[]);
        let welcome_menu = named(menu_button(), "welcome-menu");
        welcome_menu.set_hexpand(true);
        welcome_menu.set_halign(gtk::Align::End);
        welcome_bar.content.append(&welcome_menu);
        let no_account = empty::no_account();
        let add_account = named(no_account.add, "add-account");
        let welcome_hint = named(no_account.hint, "welcome-hint");
        let welcome = named(
            classed(
                adw::ToolbarView::builder()
                    .content(&no_account.page)
                    .build(),
                &["sw-welcome"],
            ),
            "welcome",
        );
        welcome.add_top_bar(&welcome_bar.root);

        let mail = named(
            MailPanes::new(&list_pane, &reader_area, LIST_WIDTH as i32),
            "mail-panes",
        );
        let content = gtk::Stack::new();
        content.add_named(&welcome, Some("welcome"));
        content.add_named(&mail, Some("mail"));
        content.set_visible_child_name("welcome");
        let sidebar_handle = resize::handle("sidebar-handle");
        let mail_area = gtk::Overlay::builder().child(&content).build();
        mail_area.add_overlay(&sidebar_handle);
        // The sidebar's visibility is the window's to set (`apply_chrome`): pinned, it doesn't
        // follow `collapsed` on its own.
        let panes = adw::OverlaySplitView::builder()
            .sidebar(&sidebar.root)
            .content(&mail_area)
            .sidebar_width_unit(adw::LengthUnit::Px)
            .min_sidebar_width(SIDEBAR_WIDTH)
            .max_sidebar_width(SIDEBAR_WIDTH)
            .pin_sidebar(true)
            .show_sidebar(false)
            .build();
        panes.set_widget_name("panes");
        let banner = Banner::new();
        panes.set_vexpand(true);
        let dock = dock::Dock::new(&panes);
        // A card open over the panes would cover the contact pane's lower half: it hides.
        let blocked = margin.downgrade();
        dock.connect_open(move |open| {
            if let Some(margin) = blocked.upgrade() {
                margin.set_blocked(open);
            }
        });
        let stacked = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stacked.append(&banner.root);
        stacked.append(dock.widget());
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stacked));
        dock.keep_toasts_clear(&toasts);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Pronto")
            .default_width(DEFAULT_SIZE.0)
            .default_height(DEFAULT_SIZE.1)
            .width_request(MIN_SIZE.0)
            .height_request(MIN_SIZE.1)
            .content(&toasts)
            .build();
        window.add_css_class("sw-window");
        crate::accessibility::forget_stale_focus(&window);

        // Width classes only: what each shows is computed from the class, the mode, the
        // sidebar and the page together (`apply_chrome`), as a setter would put back the value
        // from before it applied, not the one chosen since.
        let medium = breakpoint(WIDE_WIDTH);
        let narrow = breakpoint(NARROW_WIDTH);
        window.add_breakpoint(medium.clone());
        window.add_breakpoint(narrow.clone());

        let folder = gio::SimpleAction::new_stateful(
            "folder",
            Some(glib::VariantTy::STRING),
            &"".to_variant(),
        );
        let pane = gio::SimpleAction::new_stateful(
            "pane",
            Some(glib::VariantTy::STRING),
            &Pane::List.name().to_variant(),
        );
        let more = gio::SimpleAction::new("more", Some(glib::VariantTy::STRING));
        let collapse = gio::SimpleAction::new("collapse", Some(glib::VariantTy::STRING));
        let group = gio::SimpleActionGroup::new();
        group.add_action(&folder);
        group.add_action(&pane);
        group.add_action(&more);
        group.add_action(&collapse);
        window.insert_action_group("nav", Some(&group));

        let named = [
            sidebar.root.clone().upcast::<gtk::Widget>(),
            sidebar.compose.clone().upcast(),
            list_pane.clone().upcast(),
            reader_pane.clone().upcast(),
            reader_empty.clone().upcast(),
            column.slot.clone().upcast(),
            welcome.clone().upcast(),
            add_account.upcast(),
            welcome_hint.clone().upcast(),
            list_sidebar.clone().upcast(),
            attention.clone().upcast(),
            list_search.clone().upcast(),
            list_compose.clone().upcast(),
            list_title.clone().upcast(),
            list_subtitle.clone().upcast(),
            filter_pill.clone().upcast(),
            list_slot.clone().upcast(),
            reader_toolbar.clone().upcast(),
        ]
        .into_iter()
        .chain(reader_bar.back.clone().map(|back| back.upcast()))
        .collect();

        let this = Self {
            inner: Rc::new(Inner {
                window,
                panes,
                mail,
                medium,
                narrow,
                sidebar,
                content,
                list_pane,
                list_bar,
                list_sidebar,
                attention,
                list_title,
                list_subtitle,
                list_heading: heading,
                split_tabs,
                split_pill,
                split_pill_label,
                shows_splits: Cell::new(false),
                filter_name: RefCell::new(None),
                accounts: Cell::new(0),
                list_search_slot,
                list_search,
                list_compose,
                list_count: Cell::new((false, None)),
                filter_pill,
                filter_label,
                list_searching: Cell::new(false),
                reader_empty,
                batch_handlers: RefCell::new(Vec::new()),
                list_slot,
                list_menu,
                welcome_menu,
                welcome_bar,
                reader_area,
                reader_pane,
                reader_bar,
                reader_toolbar,
                reader_strip_clamp,
                reader,
                column,
                margin,
                batch,
                welcome,
                welcome_hint,
                banner,
                dock,
                toasts,
                folder,
                pane,
                tooltips,
                named,
                folder_handlers: RefCell::new(Vec::new()),
                provisional: Cell::new(true),
                sidebar_width: Cell::new(SIDEBAR_WIDTH),
                list_width: Cell::new(LIST_WIDTH),
                sidebar_handle,
                list_handle,
                widths_changed: RefCell::new(None),
                mode: Cell::new(ViewMode::default()),
                sidebar_wanted: Cell::new(false),
                page: Cell::new(Pane::List),
                list_empty: Cell::new(false),
                needs_attention: Cell::new(false),
                on_screen: Cell::new(false),
                screen_handlers: RefCell::new(Vec::new()),
                sidebar_handlers: RefCell::new(Vec::new()),
                sidebar_told: Cell::new(None),
                filter_handlers: RefCell::new(Vec::new()),
                applying: Cell::new(false),
                switching: Cell::new(false),
                switched: Cell::new(false),
                refocus: Cell::new(false),
            }),
        };
        this.connect_actions(&more, &collapse);
        this.update_heading();
        this.apply_chrome();
        this
    }

    fn connect_actions(&self, more: &gio::SimpleAction, collapse: &gio::SimpleAction) {
        let inner = &self.inner;
        inner.folder.connect_activate(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, parameter| {
                if let Some(key) = parameter.and_then(|parameter| parameter.str()) {
                    Window { inner }.select_folder(key);
                }
            }
        ));
        inner.pane.connect_activate(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, parameter| {
                match parameter
                    .and_then(|parameter| parameter.str())
                    .and_then(Pane::from_name)
                {
                    Some(pane) => Window { inner }.show_pane(pane),
                    None => tracing::warn!("nav.pane takes folders, list or reader"),
                }
            }
        ));
        more.connect_activate(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, parameter| {
                if let Some(account) = parameter.and_then(|parameter| parameter.str()) {
                    inner.sidebar.toggle_more(account);
                }
            }
        ));
        collapse.connect_activate(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, parameter| {
                if let Some(account) = parameter.and_then(|parameter| parameter.str()) {
                    inner.sidebar.toggle_collapsed(account);
                }
            }
        ));
        // The sidebar over the list closes on a click beside it or Escape (libadwaita's own),
        // which leaves the focus on the list bar's first button: it goes back to what shows.
        inner.panes.connect_show_sidebar_notify(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |panes| {
                if inner.applying.get() {
                    return;
                }
                let window = Window { inner };
                let parked = GtkWindowExt::focus(&window.inner.window).is_none_or(|focus| {
                    focus.is_ancestor(&window.inner.list_bar.root) || window.sidebar_has_focus()
                });
                window.apply_chrome();
                if !panes.shows_sidebar() && parked {
                    window.focus_front();
                }
            }
        ));
        inner.reader_area.connect_map(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_| Window { inner }.sync_on_screen()
        ));
        inner.reader_area.connect_unmap(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_| Window { inner }.sync_on_screen()
        ));
        for pill in [&inner.filter_pill, &inner.split_pill] {
            pill.connect_clicked(glib::clone!(
                #[weak(rename_to = inner)]
                self.inner,
                move |_| {
                    let handlers = inner.filter_handlers.borrow().clone();
                    for handler in handlers {
                        handler();
                    }
                }
            ));
        }
        inner.batch.bar.clear.connect_clicked(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_| {
                let handlers = inner.batch_handlers.borrow().clone();
                for handler in handlers {
                    handler();
                }
            }
        ));
        resize::connect(
            &inner.panes,
            vec![
                (inner.sidebar_handle.clone(), self.handle_moves(true)),
                (inner.list_handle.clone(), self.handle_moves(false)),
            ],
        );
        inner.window.connect_current_breakpoint_notify(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_| {
                let window = Window { inner };
                tracing::debug!(layout = window.layout().name(), "layout");
                window.switch_class();
            }
        ));
        // The content shows again after a switch: whether the reader is on screen is settled,
        // and once that frame is done, and libadwaita has given the focus back, the focus is
        // checked (`after_switch`).
        inner.toasts.connect_map(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_| {
                if !inner.switched.replace(false) {
                    return;
                }
                Window {
                    inner: inner.clone(),
                }
                .sync_on_screen();
                let weak = Rc::downgrade(&inner);
                glib::idle_add_local_once(move || {
                    if let Some(inner) = weak.upgrade() {
                        Window { inner }.after_switch();
                    }
                });
            }
        ));
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.inner.window
    }

    /// Where the composer's cards go (the user's UI tweak, 2026-10-08).
    pub fn dock(&self) -> &dock::Dock {
        &self.inner.dock
    }

    /// The sidebar's and the message list's widths, as dragged (the defaults until then).
    pub fn pane_widths(&self) -> (f64, f64) {
        (self.inner.sidebar_width.get(), self.inner.list_width.get())
    }

    /// Sets the sidebar's and the message list's widths, each held to its range (as a
    /// remembered layout does at start).
    pub fn set_pane_widths(&self, sidebar: f64, list: f64) {
        let inner = &self.inner;
        inner.sidebar_width.set(clamp_to(sidebar, &SIDEBAR_WIDTHS));
        inner.list_width.set(clamp_to(list, &LIST_WIDTHS));
        self.apply_widths();
    }

    /// Runs `handler` with the widths when a drag ends or a double click resets one.
    pub fn connect_pane_widths(&self, handler: impl Fn(f64, f64) + 'static) {
        self.inner.widths_changed.replace(Some(Rc::new(handler)));
    }

    /// The widths on the split views; the sidebar over the list keeps its width too.
    fn apply_widths(&self) {
        let inner = &self.inner;
        let folders = inner.sidebar_width.get();
        inner.panes.set_min_sidebar_width(folders);
        inner.panes.set_max_sidebar_width(folders);
        inner
            .mail
            .set_list_width(inner.list_width.get().round() as i32);
    }

    /// What the handle of the sidebar's border (`sidebar`) or the message list's does: drags
    /// it within its range, short of leaving the reader less than [`READER_MIN_WIDTH`] or its
    /// own minimum.
    fn handle_moves(&self, sidebar: bool) -> resize::Moves {
        let inner = &self.inner;
        let width = move |inner: &Inner| {
            if sidebar {
                inner.sidebar_width.get()
            } else {
                inner.list_width.get()
            }
        };
        let weak = Rc::downgrade(inner);
        let start = move || weak.upgrade().map_or(0.0, |inner| width(&inner));
        let weak = Rc::downgrade(inner);
        let drag = move |wanted: f64| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let window = f64::from(inner.window.width());
            let shows_list_beside = !inner.mail.is_one_pane();
            let (range, others) = if sidebar {
                (
                    &SIDEBAR_WIDTHS,
                    if shows_list_beside {
                        inner.list_width.get()
                    } else {
                        0.0
                    },
                )
            } else {
                let inline = (Window {
                    inner: inner.clone(),
                })
                .sidebar_place()
                    == SidebarPlace::Inline;
                let folders = if inline {
                    inner.sidebar_width.get()
                } else {
                    0.0
                };
                (&LIST_WIDTHS, folders)
            };
            let (reader, ..) = inner.reader_pane.measure(gtk::Orientation::Horizontal, -1);
            let room = window - others - READER_MIN_WIDTH.max(f64::from(reader));
            // No narrower than the pane's content allows, so the width kept is the width shown.
            let pane: &gtk::Widget = if sidebar {
                inner.sidebar.root.upcast_ref()
            } else {
                inner.list_pane.upcast_ref()
            };
            let (minimum, ..) = pane.measure(gtk::Orientation::Horizontal, -1);
            let width = clamp_to(wanted.min(room), range).max(f64::from(minimum));
            if sidebar {
                inner.sidebar_width.set(width);
            } else {
                inner.list_width.set(width);
            }
            Window { inner }.apply_widths();
        };
        let weak = Rc::downgrade(inner);
        let done = move || {
            if let Some(inner) = weak.upgrade() {
                Window { inner }.widths_changed();
            }
        };
        let weak = Rc::downgrade(inner);
        let reset = move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if sidebar {
                inner.sidebar_width.set(SIDEBAR_WIDTH);
            } else {
                inner.list_width.set(LIST_WIDTH);
            }
            let window = Window { inner };
            window.apply_widths();
            window.widths_changed();
        };
        resize::Moves {
            start: Box::new(start),
            drag: Box::new(drag),
            done: Box::new(done),
            reset: Box::new(reset),
        }
    }

    fn widths_changed(&self) {
        let handler = self.inner.widths_changed.borrow().clone();
        if let Some(handler) = handler {
            let (sidebar, list) = self.pane_widths();
            handler(sidebar, list);
        }
    }

    pub fn present(&self) {
        self.inner.window.present();
    }

    /// The message list (ML-026) goes here, under the list pane's bar.
    pub fn set_list(&self, list: &impl IsA<gtk::Widget>) {
        self.inner.list_slot.set_child(Some(list));
    }

    /// Shows a message (ML-027) in the reader's centred column, scrolled to its top. This
    /// doesn't bring the reader to the front; `show_pane(Pane::Reader)` does.
    pub fn set_reader(&self, reader: &impl IsA<gtk::Widget>) {
        let inner = &self.inner;
        inner.column.slot.set_child(Some(reader));
        inner.column.scroller.vadjustment().set_value(0.0);
        inner.reader.set_visible_child_name("message");
        self.apply_chrome();
    }

    /// Back to "No message selected".
    pub fn clear_reader(&self) {
        let inner = &self.inner;
        inner.reader.set_visible_child_name("empty");
        inner.column.slot.set_child(None::<&gtk::Widget>);
        self.apply_chrome();
    }

    /// The reader's page for the select mode: "N messages selected" with the batch actions
    /// (ML-054). The message shown before goes; `set_reader` brings one back.
    pub fn show_batch(&self, count: u32, conversations: bool) {
        let inner = &self.inner;
        inner.batch.set_count(count, conversations);
        inner.reader.set_visible_child_name("batch");
        inner.column.slot.set_child(None::<&gtk::Widget>);
        self.apply_chrome();
    }

    /// The batch page, for its commands' titles and tooltips.
    pub fn batch_page(&self) -> &empty::BatchPage {
        &self.inner.batch
    }

    /// What the reader pane shows: `empty`, `message` or `batch`.
    pub fn reader_page(&self) -> String {
        self.inner
            .reader
            .visible_child_name()
            .map(|name| name.to_string())
            .unwrap_or_default()
    }

    /// The reader's actions (archive, delete, … "n of N"), in its bar; the bar stretches.
    pub fn set_reader_toolbar(&self, toolbar: &impl IsA<gtk::Widget>) {
        self.inner.reader_toolbar.set_child(Some(toolbar));
    }

    /// The contact pane (phase C, docs/superhuman/contact.md), in the right margin of the
    /// conversation's column where it has room.
    pub fn set_contact_pane(&self, pane: &impl IsA<gtk::Widget>) {
        self.inner.margin.set_side(pane);
    }

    /// Contact Details: whether the contact pane shows where the margin has room for it.
    pub fn set_contact_wanted(&self, wanted: bool) {
        self.inner.margin.set_wanted(wanted);
    }

    /// The contact pane is laid out beside the column: on screen with the conversation, or
    /// warm behind a Focused list with it.
    pub fn contact_shown(&self) -> bool {
        self.inner.margin.side_shown()
    }

    /// Runs `handler` when the contact pane hides with the keyboard focus in it.
    pub fn connect_contact_left(&self, handler: impl Fn() + 'static) {
        self.inner.margin.connect_side_left(handler);
    }

    /// Whether the conversation's margin has the contact pane's room at the width the reader
    /// was last laid out at, Contact Details on or off; `None` before it was laid out.
    pub fn contact_has_room(&self) -> Option<bool> {
        self.inner.margin.has_room()
    }

    /// The stops of Next Pane and Previous Pane on screen now, in reading order: the sidebar
    /// while it shows (beside the mail or over it), the list and the reader while they show
    /// (one of them in Focused mode or a narrow window), and the contact pane while it shows
    /// beside an open conversation. None while the window shows no mail (no account yet).
    pub fn pane_stops(&self) -> Vec<PaneStop> {
        let inner = &self.inner;
        if inner.content.visible_child_name().as_deref() != Some("mail") {
            return Vec::new();
        }
        let mut stops = Vec::new();
        if self.sidebar_place() != SidebarPlace::Hidden {
            stops.push(PaneStop::Folders);
        }
        if inner.mail.shows_list() {
            stops.push(PaneStop::List);
        }
        if inner.mail.shows_reader() {
            stops.push(PaneStop::Conversation);
            if inner.reader.visible_child_name().as_deref() == Some("message")
                && inner.margin.side_shown()
            {
                stops.push(PaneStop::Contact);
            }
        }
        stops
    }

    /// The stop `widget` is in: the contact pane, the reader's pane (its toolbar, the
    /// conversation, the reply under it), the list's pane (its bar too) or the sidebar; `None`
    /// elsewhere (a composer card, the banner).
    pub fn pane_stop_of(&self, widget: &gtk::Widget) -> Option<PaneStop> {
        let inner = &self.inner;
        let within = |pane: &gtk::Widget| widget == pane || widget.is_ancestor(pane);
        if inner.margin.side().is_some_and(|side| within(&side)) {
            Some(PaneStop::Contact)
        } else if within(inner.reader_area.upcast_ref()) {
            Some(PaneStop::Conversation)
        } else if within(inner.list_pane.upcast_ref()) {
            Some(PaneStop::List)
        } else if within(inner.sidebar.root.upcast_ref()) {
            Some(PaneStop::Folders)
        } else {
            None
        }
    }

    /// The stop the keyboard focus is in ([`Window::pane_stop_of`]).
    pub fn focused_pane_stop(&self) -> Option<PaneStop> {
        GtkWindowExt::focus(&self.inner.window).and_then(|focus| self.pane_stop_of(&focus))
    }

    /// The keyboard focus is over the window's content, not in it: the command palette, a
    /// picker or a dialog over the window, which keep it.
    pub fn focus_over_content(&self) -> bool {
        let content: &gtk::Widget = self.inner.toasts.upcast_ref();
        GtkWindowExt::focus(&self.inner.window)
            .is_some_and(|focus| &focus != content && !focus.is_ancestor(content))
    }

    /// The keyboard focus to the folder the list shows, in the sidebar; false when it doesn't
    /// show.
    pub fn focus_folders(&self) -> bool {
        self.sidebar_place() != SidebarPlace::Hidden && self.inner.sidebar.focus_current()
    }

    /// The keyboard focus to the first control of what the reader shows (the batch page's
    /// buttons; the conversation's first control); false when nothing there takes it.
    pub fn focus_reader_page(&self) -> bool {
        let reader = &self.inner.reader;
        reader.is_mapped() && reader.child_focus(gtk::DirectionType::TabForward)
    }

    /// The sidebar lying over the mail (a narrower window) closes, as picking a folder there
    /// closes it; false when it doesn't lie there.
    pub fn close_folders_over_mail(&self) -> bool {
        if self.sidebar_place() != SidebarPlace::Overlay {
            return false;
        }
        self.show_pane(self.inner.page.get());
        true
    }

    /// Whether the reader has nothing left to paint ([`crate::reader::Reader::is_quiet`]):
    /// kept warm behind the list, it is drawn only until then.
    pub fn set_reader_quiet(&self, quiet: impl Fn() -> bool + 'static) {
        self.inner.mail.set_reader_quiet(quiet);
    }

    /// The warm reader drawn behind the list so far, and left undrawn because it was quiet:
    /// for checks.
    pub fn warm_reader_draws(&self) -> (u64, u64) {
        self.inner.mail.reader_draws()
    }

    /// Keeps Back and the reader's toolbar to the width of the thread's column
    /// ([`crate::reader::column_width`]), centred over the conversation, as `theme`'s reading
    /// size changes, for as long as the window lives; the contact pane's margin follows the
    /// column too.
    pub fn follow_reader_column(&self, theme: &Rc<crate::theme::ThemeController>) {
        let fit = |clamp: &adw::Clamp, margin: &margin::ColumnMargin, theme: &Theme| {
            let width = crate::reader::column_width(theme.scale.reading);
            clamp.set_maximum_size(width);
            clamp.set_tightening_threshold(width);
            // `.sw-reader-clamp`'s margin: twice the `xxl` space on either side.
            let gutter = 2 * theme.spacing.token("xxl").unwrap_or(12);
            margin.set_column(width, i32::try_from(gutter).unwrap_or(i32::MAX));
        };
        fit(
            &self.inner.reader_strip_clamp,
            &self.inner.margin,
            &theme.theme(),
        );
        let clamp = self.inner.reader_strip_clamp.downgrade();
        let margin = self.inner.margin.downgrade();
        let id = theme.connect_changed(move |theme| {
            if let (Some(clamp), Some(margin)) = (clamp.upgrade(), margin.upgrade()) {
                fit(&clamp, &margin, theme);
            }
        });
        let theme = Rc::downgrade(theme);
        self.inner.window.connect_destroy(move |_| {
            if let Some(theme) = theme.upgrade() {
                theme.disconnect(id);
            }
        });
    }

    /// Shows `model` in the sidebar. The folder shown stays when the model still has it, else
    /// the window picks its account's Inbox (a deleted folder's), else the first account's;
    /// with no account it shows "No accounts yet". Until the first account has its folders,
    /// the window's own pick follows them (`provisional`).
    pub fn set_sidebar(&self, model: SidebarModel) {
        let inner = &self.inner;
        let current = self.selected_folder();
        let selected = if inner.provisional.get()
            && let Some(own) = model.own_selection(&current)
        {
            own
        } else if model.contains(&current) {
            current.clone()
        } else {
            inner
                .sidebar
                .account_of(&current)
                .and_then(|account| model.accounts.iter().find(|a| a.key == account))
                .and_then(|account| account.inbox())
                .map(|inbox| inbox.key.clone())
                .or_else(|| model.default_selection())
                .unwrap_or_default()
        };
        inner
            .content
            .set_visible_child_name(if model.accounts.is_empty() {
                "welcome"
            } else {
                "mail"
            });
        self.set_attention(model.attention());
        inner.accounts.set(model.accounts.len());
        inner.provisional.set(model.awaits_inbox(&selected));
        inner.sidebar.set_model(model, &selected);
        inner.folder.set_state(&selected.to_variant());
        self.update_heading();
        self.apply_chrome();
        if selected != current && !selected.is_empty() {
            self.emit_folder(&selected);
        }
    }

    /// The sidebar button's dot and words for an account that needs the user: the words in its
    /// tooltip and its description, never the colour alone.
    fn set_attention(&self, attention: Option<(AccountStatus, String)>) {
        let inner = &self.inner;
        inner.needs_attention.set(attention.is_some());
        for class in sidebar::STATUS_CLASSES {
            inner.attention.remove_css_class(class);
        }
        inner.tooltips.set_note(
            &inner.list_sidebar,
            attention.as_ref().map(|(_, words)| words.as_str()),
        );
        match attention {
            Some((status, words)) => {
                if let Some(class) = status.class() {
                    inner.attention.add_css_class(class);
                }
                inner
                    .list_sidebar
                    .update_property(&[gtk::accessible::Property::Description(&words)]);
            }
            None => inner
                .list_sidebar
                .reset_property(gtk::AccessibleProperty::Description),
        }
    }

    /// The key of the folder shown, or "" before there is one.
    pub fn selected_folder(&self) -> String {
        self.inner
            .folder
            .state()
            .and_then(|state| state.str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Shows the folder `key` (as picking it in the sidebar does), and the list in front; the
    /// sidebar over the list closes. Unknown keys are ignored.
    pub fn select_folder(&self, key: &str) {
        let inner = &self.inner;
        if !inner.sidebar.contains(key) {
            tracing::warn!("asked for a folder the sidebar doesn't have");
            return;
        }
        inner.provisional.set(false);
        let changed = self.selected_folder() != key;
        inner.folder.set_state(&key.to_variant());
        inner.sidebar.set_selected(key);
        if changed {
            // Another folder's conversations aren't counted yet.
            let (conversations, _) = inner.list_count.get();
            inner.list_count.set((conversations, None));
        }
        self.update_heading();
        if changed {
            self.emit_folder(key);
        }
        self.show_pane(Pane::List);
    }

    /// Runs `handler` with the key of every folder the window starts showing.
    pub fn connect_folder_selected(&self, handler: impl Fn(&Window, &str) + 'static) {
        self.inner
            .folder_handlers
            .borrow_mut()
            .push(Box::new(handler));
    }

    fn emit_folder(&self, key: &str) {
        for handler in self.inner.folder_handlers.borrow().iter() {
            handler(self, key);
        }
    }

    /// Overrides the list pane's title, as for search results.
    pub fn set_list_heading(&self, title: &str, subtitle: &str) {
        let inner = &self.inner;
        inner.list_title.set_text(title);
        inner.list_subtitle.set_text(subtitle);
        let splits = inner.shows_splits.get();
        inner.list_title.set_visible(!splits);
        inner
            .list_subtitle
            .set_visible(!subtitle.is_empty() && !splits);
        self.update_split_place();
    }

    /// Whose Inbox the Split Inbox's tabs are: the tab list's accessible name ("Inbox,
    /// Personal", "All Inboxes"), and after the tabs with several accounts, where the view
    /// name's subtitle would say it.
    fn update_split_place(&self) {
        let inner = &self.inner;
        let key = self.selected_folder();
        let (name, account) = if key == ALL_INBOXES {
            ("All Inboxes".to_owned(), "All Inboxes".to_owned())
        } else {
            match inner.sidebar.heading_counted(&key, None) {
                Some((title, account)) => (format!("{title}, {account}"), account),
                None => ("Inbox".to_owned(), String::new()),
            }
        };
        let several = inner.accounts.get() > 1 && !account.is_empty();
        inner
            .split_tabs
            .set_place(&name, several.then_some(account.as_str()));
    }

    /// The Split Inbox's tabs in the title's place, with `current` the side shown and each
    /// side's total (`conversations`: rows are conversations, else messages); `None` shows the
    /// title again (phase C). The account's name goes after them only with several accounts
    /// (the visual QA's 380 px list needs the room), and goes first where the row is short.
    pub fn set_split_tabs(&self, tabs: Option<(Split, Option<SplitCounts>, bool)>) {
        let inner = &self.inner;
        if let Some((current, counts, conversations)) = tabs {
            let unit = if conversations {
                ("conversation", "conversations")
            } else {
                ("message", "messages")
            };
            inner.split_tabs.set(current, counts, unit);
            self.update_split_place();
        }
        inner.shows_splits.set(tabs.is_some());
        inner.split_tabs.set_visible(tabs.is_some());
        inner.list_title.set_visible(tabs.is_none());
        inner
            .list_subtitle
            .set_visible(tabs.is_none() && !inner.list_subtitle.text().is_empty());
        self.show_filter_pill();
    }

    /// Runs `handler` with the side a click on its tab picked.
    pub fn connect_split_picked(&self, handler: impl Fn(Split) + 'static) {
        self.inner.split_tabs.connect_picked(handler);
    }

    /// The Split Inbox's tabs: whether they are on screen (not while a search or the select
    /// mode's bar takes the header row), and whether their totals are (the self-test).
    pub fn split_tabs_shown(&self) -> (bool, bool) {
        let tabs = &self.inner.split_tabs;
        (tabs.is_mapped(), tabs.is_mapped() && tabs.shows_totals())
    }

    fn update_heading(&self) {
        let (title, subtitle) = self.folder_heading().unwrap_or_default();
        self.set_list_heading(&title, &subtitle);
    }

    /// The heading of the folder shown: its unread messages, or with conversation rows the
    /// conversations the list holds ("Inbox", "21 · Personal").
    pub fn folder_heading(&self) -> Option<(String, String)> {
        let key = self.selected_folder();
        match self.inner.list_count.get() {
            (true, count) => self.inner.sidebar.heading_counted(&key, count),
            (false, _) => self.inner.sidebar.heading(&key),
        }
    }

    /// Whether the list shows conversations, and how many (`None` while they are read): the
    /// heading counts them.
    pub fn set_list_count(&self, conversations: bool, count: Option<u32>) {
        self.inner.list_count.set((conversations, count));
    }

    /// The filter the list shows, as a pill in its bar (`None`: all its messages); a click on
    /// the pill runs the handlers of [`Window::connect_filter_cleared`].
    pub fn set_filter_pill(&self, name: Option<&str>) {
        let inner = &self.inner;
        if let Some(name) = name {
            for (pill, label) in [
                (&inner.filter_pill, &inner.filter_label),
                (&inner.split_pill, &inner.split_pill_label),
            ] {
                label.set_text(name);
                pill.update_property(&[gtk::accessible::Property::Label(&format!(
                    "{name} only: show all messages"
                ))]);
                pill.set_tooltip_text(Some(&format!("{name} only: click to show all")));
            }
        }
        inner.filter_name.replace(name.map(str::to_owned));
        self.show_filter_pill();
    }

    /// The filter's pill after the view's name, or after the Split Inbox's tabs.
    fn show_filter_pill(&self) {
        let inner = &self.inner;
        let filtered = inner.filter_name.borrow().is_some();
        let splits = inner.shows_splits.get();
        inner.filter_pill.set_visible(filtered && !splits);
        inner.split_pill.set_visible(filtered && splits);
    }

    pub fn connect_filter_cleared(&self, handler: impl Fn() + 'static) {
        self.inner
            .filter_handlers
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when the header's select-mode bar is cleared (its Clear).
    pub fn connect_batch_cleared(&self, handler: impl Fn() + 'static) {
        self.inner
            .batch_handlers
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// The list's search field and its scope go in the header row, after the sidebar button:
    /// while they show, they take the view name's place (the visual QA: the query in the header
    /// as in Superhuman, not a full-width field and a row of chips under it). While rows are
    /// chosen the select mode's bar takes the row instead, the search's results still listed
    /// (phase B's review: both crowded the header, the count cut to "…" at 560 px).
    pub fn set_list_search(&self, search: &impl IsA<gtk::Widget>) {
        self.inner.list_search_slot.append(search);
    }

    /// Whether the list's search shows: the header hides the view's name and the Search
    /// button meanwhile, and a Split window keeps its two panes whatever the results, so the
    /// panes never jump as a search is typed.
    pub fn set_list_searching(&self, searching: bool) {
        if self.inner.list_searching.replace(searching) != searching {
            self.apply_chrome();
        }
    }

    /// The list shows Inbox Zero: its tint takes the whole pane, the header row too.
    pub fn set_inbox_zero(&self, zero: bool) {
        let pane = &self.inner.list_pane;
        if zero {
            pane.add_css_class("sw-zero");
        } else {
            pane.remove_css_class("sw-zero");
        }
    }

    /// Focused or Split.
    pub fn view_mode(&self) -> ViewMode {
        self.inner.mode.get()
    }

    /// Switches between the reader in the list's place (Focused) and beside it (Split). Into
    /// Focused, the reader stays in front only while it has the keyboard focus.
    pub fn set_view_mode(&self, mode: ViewMode) {
        let inner = &self.inner;
        if inner.mode.replace(mode) == mode {
            return;
        }
        if mode == ViewMode::Focused {
            inner.page.set(if self.reader_has_focus() {
                Pane::Reader
            } else {
                Pane::List
            });
        }
        self.apply_chrome();
    }

    /// The sidebar beside the list while the window is wide, as `layout.toml` remembers it;
    /// nothing is told (the window opens this way).
    pub fn set_sidebar_wanted(&self, shown: bool) {
        let inner = &self.inner;
        if inner.sidebar_wanted.replace(shown) == shown {
            return;
        }
        if self.layout() == Layout::Wide {
            inner.applying.set(true);
            inner.panes.set_show_sidebar(shown);
            inner.applying.set(false);
        }
        self.apply_chrome();
    }

    /// Whether the sidebar shows beside a wide window's list (the remembered choice).
    pub fn sidebar_wanted(&self) -> bool {
        self.inner.sidebar_wanted.get()
    }

    /// Toggle Sidebar: beside the list in a wide window, which is remembered, else over it until
    /// a folder is picked or it is closed.
    pub fn set_sidebar_shown(&self, shown: bool) {
        let inner = &self.inner;
        let wide = self.layout() == Layout::Wide;
        if wide {
            inner.sidebar_wanted.set(shown);
        }
        inner.applying.set(true);
        inner.panes.set_show_sidebar(shown);
        inner.applying.set(false);
        self.apply_chrome();
        if wide {
            self.emit_sidebar(true);
        }
    }

    /// Whether the sidebar shows now, beside the list or over it.
    pub fn sidebar_shown(&self) -> bool {
        self.inner.panes.shows_sidebar()
    }

    pub fn sidebar_place(&self) -> SidebarPlace {
        let panes = &self.inner.panes;
        match (panes.shows_sidebar(), panes.is_collapsed()) {
            (false, _) => SidebarPlace::Hidden,
            (true, true) => SidebarPlace::Overlay,
            (true, false) => SidebarPlace::Inline,
        }
    }

    /// Runs `handler` with whether the sidebar shows whenever that changes, and with `true`
    /// for a choice to remember (Toggle Sidebar in a wide window).
    pub fn connect_sidebar_shown(&self, handler: impl Fn(bool, bool) + 'static) {
        self.inner
            .sidebar_handlers
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Tells the handlers whether the sidebar shows: a change, or a choice to remember.
    fn emit_sidebar(&self, remember: bool) {
        let shown = self.sidebar_shown();
        if self.inner.sidebar_told.replace(Some(shown)) == Some(shown) && !remember {
            return;
        }
        let handlers = self.inner.sidebar_handlers.borrow().clone();
        for handler in handlers {
            handler(shown, remember);
        }
    }

    /// The list shows its empty state: Split then gives it the whole mail area, so there is
    /// never a second empty state beside it.
    pub fn set_list_empty(&self, empty: bool) {
        if self.inner.list_empty.replace(empty) != empty {
            self.apply_chrome();
        }
    }

    /// Shows `pane` while one pane fits (Focused, or a narrow window); in Split the page is
    /// remembered for a narrower window. Folders shows the sidebar; the list or the reader
    /// closes the sidebar that lies over them.
    pub fn show_pane(&self, pane: Pane) {
        let inner = &self.inner;
        match pane {
            Pane::Folders => {
                if !self.sidebar_shown() {
                    self.set_sidebar_shown(true);
                }
            }
            Pane::List | Pane::Reader => {
                inner.page.set(pane);
                // A folder picked over the list: libadwaita would leave the focus on the list
                // bar's first button; it goes to what shows instead.
                let mut refocus = false;
                if inner.panes.is_collapsed() && inner.panes.shows_sidebar() {
                    refocus = self.sidebar_has_focus();
                    inner.applying.set(true);
                    inner.panes.set_show_sidebar(false);
                    inner.applying.set(false);
                }
                self.apply_chrome();
                if refocus {
                    self.focus_front();
                }
            }
        }
        self.sync_pane();
    }

    pub fn pane(&self) -> Pane {
        let inner = &self.inner;
        if inner.panes.is_collapsed() && inner.panes.shows_sidebar() {
            Pane::Folders
        } else {
            inner.page.get()
        }
    }

    fn sync_pane(&self) {
        let pane = self.pane().name();
        let current = self.inner.pane.state();
        if current.as_ref().and_then(|state| state.str()) != Some(pane) {
            self.inner.pane.set_state(&pane.to_variant());
        }
    }

    pub fn layout(&self) -> Layout {
        let inner = &self.inner;
        match inner.window.current_breakpoint() {
            Some(current) if current == inner.narrow => Layout::Narrow,
            Some(current) if current == inner.medium => Layout::Medium,
            _ => Layout::Wide,
        }
    }

    /// One pane fills the mail area: the list or the reader alone.
    pub fn is_one_pane(&self) -> bool {
        self.inner.mail.is_one_pane()
    }

    /// The reader is on screen: what it shows counts as read (ML-057). A reader kept warm
    /// behind a Focused list isn't.
    pub fn reader_on_screen(&self) -> bool {
        self.inner.on_screen.get()
    }

    /// Runs `handler` with whether the reader is on screen whenever that changes.
    pub fn connect_reader_on_screen(&self, handler: impl Fn(bool) + 'static) {
        self.inner
            .screen_handlers
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// The reader is laid out behind the list, unseen.
    pub fn reader_is_warm(&self) -> bool {
        self.inner.mail.reader_is_warm()
    }

    /// `widget` is in the reader laid out warm behind the list: mapped, drawn into nothing.
    pub fn is_behind_list(&self, widget: &gtk::Widget) -> bool {
        let area: &gtk::Widget = self.inner.reader_area.upcast_ref();
        self.reader_is_warm() && (widget == area || widget.is_ancestor(area))
    }

    /// The keyboard focus to what shows: the list, which takes it once the frame is done
    /// (`MessageList::keep_focus`), or the reader's bar. While the width class changes, once
    /// the new layout shows (`after_switch`).
    fn focus_front(&self) {
        let inner = &self.inner;
        if inner.switching.get() {
            inner.refocus.set(true);
        } else if inner.mail.shows_list() {
            GtkWindowExt::set_focus(&inner.window, None::<&gtk::Widget>);
        } else {
            self.focus_reader_bar();
        }
    }

    /// A new width class. libadwaita switches by drawing the old layout for one more frame
    /// with the window's content hidden, and GTK moves the keyboard focus off hidden content
    /// when a frame ends, on from the window's first control: from a list row to the list
    /// bar's search button, and `j` stopped moving the selection. A focus set again where it is
    /// stays (GTK leaves a focus alone that is set while it waits, unless it is on a hidden
    /// control: that one waits on the list bar's first button), libadwaita gives it back when
    /// the content shows, and what the new layout no longer shows gives it up then
    /// (`after_switch`).
    fn switch_class(&self) {
        let inner = &self.inner;
        // Not hidden: the window's first layout.
        let hidden = self.content_hidden();
        inner.switching.set(hidden);
        self.apply_chrome();
        inner.switching.set(false);
        if !hidden {
            return;
        }
        inner.switched.set(true);
        if let Some(focus) = GtkWindowExt::focus(&inner.window)
            && focus.is_ancestor(&inner.toasts)
        {
            let kept = if focus.is_visible() {
                focus
            } else {
                inner.refocus.set(true);
                inner.list_sidebar.clone().upcast()
            };
            gtk::prelude::RootExt::set_focus(&inner.window, Some(&kept));
        }
    }

    /// libadwaita hides the window's content for the frame of a width switch: `toasts`, or
    /// what holds it (the command palette's overlay, `palette::install`, is the window's
    /// content and holds `toasts`).
    fn content_hidden(&self) -> bool {
        let window: &gtk::Widget = self.inner.window.upcast_ref();
        let mut part = Some(self.inner.toasts.clone().upcast::<gtk::Widget>());
        while let Some(widget) = part.filter(|widget| widget != window) {
            if !widget.is_child_visible() {
                return true;
            }
            part = widget.parent();
        }
        false
    }

    /// The new width class shows: a focus left on what it no longer shows (the sidebar it
    /// closed, the pane it leaves out, a control it hides), or that the switch was to move,
    /// goes to what shows.
    fn after_switch(&self) {
        let inner = &self.inner;
        let moved = inner.refocus.replace(false);
        match GtkWindowExt::focus(&inner.window) {
            Some(focus) if !moved && self.shows_focus(&focus) => {}
            None if !moved => {}
            _ => self.focus_front(),
        }
    }

    /// The keyboard focus is on something the window shows: not in the sidebar while it is
    /// hidden, in a pane one pane at a time leaves out or the reader warm behind the list, nor
    /// on a hidden control. Outside the window's content (a dialog) it isn't the window's to
    /// say.
    fn shows_focus(&self, focus: &gtk::Widget) -> bool {
        let inner = &self.inner;
        let within = |pane: &gtk::Widget| focus == pane || focus.is_ancestor(pane);
        if (within(inner.sidebar.root.upcast_ref()) && self.sidebar_place() == SidebarPlace::Hidden)
            || (within(inner.list_pane.upcast_ref()) && !inner.mail.shows_list())
            || (within(inner.reader_area.upcast_ref()) && !inner.mail.shows_reader())
        {
            return false;
        }
        let content: &gtk::Widget = inner.toasts.upcast_ref();
        let mut part = Some(focus.clone());
        while let Some(current) = part {
            if &current == content {
                return true;
            }
            if !current.is_visible() || !current.is_child_visible() || !current.can_focus() {
                return false;
            }
            part = current.parent();
        }
        true
    }

    /// The keyboard focus is in the sidebar.
    fn sidebar_has_focus(&self) -> bool {
        let sidebar = &self.inner.sidebar.root;
        GtkWindowExt::focus(&self.inner.window).is_some_and(|focus| {
            &focus == sidebar.upcast_ref::<gtk::Widget>() || focus.is_ancestor(sidebar)
        })
    }

    /// The keyboard focus is in the reader's pane (its bar, toolbar or conversation).
    pub fn reader_has_focus(&self) -> bool {
        let area = &self.inner.reader_area;
        GtkWindowExt::focus(&self.inner.window).is_some_and(|focus| {
            &focus == area.upcast_ref::<gtk::Widget>() || focus.is_ancestor(area)
        })
    }

    /// The keyboard focus is on the reader's Back button ([`Window::focus_reader_bar`]).
    pub fn back_has_focus(&self) -> bool {
        self.inner.reader_bar.back.as_ref().is_some_and(|back| {
            GtkWindowExt::focus(&self.inner.window).as_ref()
                == Some(back.upcast_ref::<gtk::Widget>())
        })
    }

    /// While the reader is alone and its conversation isn't on screen yet, the focus waits on
    /// its Back button, where the reader's keys work too; false when the button isn't shown.
    pub fn focus_reader_bar(&self) -> bool {
        self.inner
            .reader_bar
            .back
            .as_ref()
            .is_some_and(|back| back.is_mapped() && back.grab_focus())
    }

    fn sync_on_screen(&self) {
        let inner = &self.inner;
        // The window's content hidden for the frame of a width switch takes nothing off
        // screen: what the switch changed is settled once it shows again.
        if self.content_hidden() {
            return;
        }
        let on = inner.reader_area.is_mapped() && inner.mail.shows_reader();
        if inner.on_screen.replace(on) == on {
            return;
        }
        let handlers = inner.screen_handlers.borrow().clone();
        for handler in handlers {
            handler(on);
        }
    }

    /// What every part shows, from the width class, the mode, the sidebar, the page and
    /// whether the list is empty.
    fn apply_chrome(&self) {
        let inner = &self.inner;
        let layout = self.layout();
        let wide = layout == Layout::Wide;
        let narrow = layout == Layout::Narrow;
        let mode = inner.mode.get();

        // The sidebar: beside the list in a wide window when wanted, over it below; crossing
        // 1280 px closes the overlay and brings back what is wanted.
        inner.applying.set(true);
        if inner.panes.is_collapsed() == wide {
            inner.panes.set_collapsed(!wide);
            inner
                .panes
                .set_show_sidebar(wide && inner.sidebar_wanted.get());
        }
        inner.applying.set(false);
        let place = self.sidebar_place();
        let inline = place == SidebarPlace::Inline;

        // The list and the reader. An empty list with nothing in the reader shows alone, in
        // Split too and whatever was open before (the last conversation archived from the
        // reader), so its empty state (Inbox Zero) never sits beside the reader's. Not while a
        // search shows: the panes stay as they are while it is typed, and the reader beside a
        // search that found nothing is blank, its "No message selected" unsaid.
        let page = inner.reader.visible_child_name();
        let empty = page.as_deref() == Some("empty");
        let searching = inner.list_searching.get();
        let alone = inner.list_empty.get() && empty && !searching;
        if alone {
            inner.page.set(Pane::List);
        }
        // Blank, its words hidden from a screen reader too: the page's content goes, the page
        // stays the stack's.
        let blank = inner.list_empty.get() && searching;
        if let Some(content) = inner.reader_empty.first_child() {
            content.set_visible(!blank);
        }
        let one_pane = narrow || mode == ViewMode::Focused || alone;
        let reader_front = one_pane && inner.page.get() == Pane::Reader;
        let focus_in_reader = self.reader_has_focus();
        inner
            .mail
            .set_layout(one_pane, reader_front, mode == ViewMode::Focused);
        if one_pane && !reader_front && focus_in_reader {
            self.focus_front();
        }
        let list_shown = inner.mail.shows_list();
        let reader_shown = inner.mail.shows_reader();
        if place == SidebarPlace::Hidden && self.sidebar_has_focus() {
            // The sidebar hidden with the focus in it: the focus goes to what shows.
            self.focus_front();
        }
        if one_pane {
            inner.window.add_css_class("sw-one-pane");
        } else {
            inner.window.remove_css_class("sw-one-pane");
        }

        // The reader's bar: Back while the reader is alone, and no toolbar over an empty page.
        // The bar itself stays: its line continues the list's, and outside Hyprland it holds
        // the end window controls and drags the window.
        if let Some(back) = &inner.reader_bar.back {
            back.set_visible(one_pane);
        }
        inner.reader_toolbar.set_visible(!empty);

        // Handles, and the window controls at the window's edges: the start ones on the
        // sidebar while it shows (beside the list or over it), else on the pane at the start.
        inner.sidebar_handle.set_visible(inline);
        inner.list_handle.set_visible(!one_pane);
        let welcome = inner.content.visible_child_name().as_deref() == Some("welcome");
        let sidebar_shown = place != SidebarPlace::Hidden;
        inner.sidebar.bar.show_controls(sidebar_shown, false);
        inner.welcome_bar.show_controls(!sidebar_shown, true);
        inner
            .list_bar
            .show_controls(list_shown && !sidebar_shown, list_shown && !reader_shown);
        inner
            .reader_bar
            .show_controls(reader_front && !sidebar_shown, reader_shown);
        if welcome {
            inner.list_bar.show_controls(false, false);
            inner.reader_bar.show_controls(false, false);
        }

        // The sidebar button: pressed while the sidebar shows; its dot while it doesn't.
        inner
            .attention
            .set_visible(inner.needs_attention.get() && place == SidebarPlace::Hidden);

        // The header row: the select mode's bar where the reader's batch page isn't seen, the
        // search field while a search shows, else the view's name. The filter's pill follows
        // the name, or the search's scope while a search shows, as the filter narrows the
        // search too (phase C, the visual QA: it hid with the name, and a search under Unread
        // found nothing with no word of why).
        let batch = page.as_deref() == Some("batch") && list_shown && !reader_shown;
        inner.batch.bar.widget.set_visible(batch);
        inner.list_search_slot.set_visible(!batch);
        inner.list_heading.set_visible(!batch && !searching);
        let pill_home = if searching {
            &inner.list_search_slot
        } else {
            &inner.list_heading
        };
        if inner.filter_pill.parent().as_ref() != Some(pill_home.upcast_ref()) {
            if let Some(parent) = inner.filter_pill.parent().and_downcast::<gtk::Box>() {
                parent.remove(&inner.filter_pill);
            }
            pill_home.append(&inner.filter_pill);
            // On the name's baseline beside it, centred as the scope's words are beside them.
            inner.filter_pill.set_valign(if searching {
                gtk::Align::Center
            } else {
                gtk::Align::BaselineCenter
            });
        }
        inner.list_search.set_visible(!searching);
        // Compose waits on its key while the search or the select mode fills the row.
        inner.list_compose.set_visible(!batch && !searching);

        self.sync_pane();
        self.sync_on_screen();
        self.emit_sidebar(false);
    }

    /// The main menu (ML-022), at the end of the list pane's bar, or of the welcome page's while
    /// there is no account.
    pub fn set_menu_model(&self, menu: Option<&gio::MenuModel>) {
        self.inner.list_menu.set_menu_model(menu);
        self.inner.welcome_menu.set_menu_model(menu);
    }

    /// Shows the keys of `keymap` (the registry with `keys.toml` over it) in every tooltip and
    /// hint that names a command.
    pub fn set_keymap(&self, keymap: &Keymap) {
        self.inner.tooltips.set_keymap(keymap);
        self.inner
            .welcome_hint
            .set_text(&empty::palette_hint(keymap));
    }

    /// Gives `widget`, an icon-only button, the tooltip `label (keys)` with the keys of `id`,
    /// kept in step with `keys.toml` (DESIGN.md "Accessibility"): for the reader's toolbar
    /// (ML-027) and the list's own buttons (ML-026).
    pub fn add_shortcut_tooltip(&self, widget: &impl IsA<gtk::Widget>, label: &str, id: ActionId) {
        self.inner.tooltips.add(widget, label, id);
    }

    /// Shows the first of `queue` in the banner and counts the others, or hides the banner when
    /// `queue` is empty (ML-048).
    pub fn show_banner(&self, queue: &[Attention]) {
        self.inner.banner.show(queue);
    }

    pub fn shows_banner(&self) -> bool {
        self.inner.banner.is_shown()
    }

    /// Runs `handler` with what each banner button clicked asks for.
    pub fn connect_banner(&self, handler: impl Fn(BannerAction) + 'static) {
        self.inner.banner.connect(handler);
    }

    pub fn add_toast(&self, toast: adw::Toast) {
        self.inner.toasts.add_toast(toast);
    }

    /// Where toasts appear, for a dialog that reports its outcome after it closes.
    pub fn toasts(&self) -> adw::ToastOverlay {
        self.inner.toasts.clone()
    }

    pub fn sidebar(&self) -> &Sidebar {
        &self.inner.sidebar
    }

    /// A pane's root widget: `sidebar`, `list`, `reader` or `welcome`.
    pub fn pane_widget(&self, name: &str) -> Option<gtk::Widget> {
        let inner = &self.inner;
        Some(match name {
            "sidebar" => inner.sidebar.root.clone().upcast(),
            "list" => inner.list_pane.clone().upcast(),
            "reader" => inner.reader_pane.clone().upcast(),
            "welcome" => inner.welcome.clone().upcast(),
            _ => return None,
        })
    }

    /// A widget by its name (`gtk::Widget::widget_name`): the window's named parts, then
    /// anything in the window's tree.
    pub fn widget(&self, name: &str) -> Option<gtk::Widget> {
        if let Some(widget) = self.pane_widget(name) {
            return Some(widget);
        }
        self.inner
            .named
            .iter()
            .find(|widget| widget.widget_name() == name)
            .cloned()
            .or_else(|| find_named(self.inner.window.upcast_ref(), name))
    }
}

/// The list bar's sidebar button (Toggle Sidebar), pressed while the sidebar shows, with the
/// dot it carries while an account needs the user and the sidebar is hidden.
fn sidebar_toggle(tooltips: &Tooltips) -> (gtk::ToggleButton, gtk::Box) {
    let attention = named(
        classed(
            gtk::Box::builder()
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .can_target(false)
                .visible(false)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build(),
            &["sw-status-dot", "sw-attention-dot"],
        ),
        "list-attention",
    );
    let overlay = gtk::Overlay::builder()
        .child(&icon(&[
            "sidebar-show-symbolic",
            "view-sidebar-start-symbolic",
        ]))
        .build();
    overlay.add_overlay(&attention);
    let button = named(
        classed(
            gtk::ToggleButton::builder()
                .child(&overlay)
                .action_name(TOGGLE_SIDEBAR_ACTION)
                .valign(gtk::Align::Center)
                .build(),
            &["sw-icon-button", "sw-sidebar-toggle"],
        ),
        "list-sidebar",
    );
    button.update_property(&[gtk::accessible::Property::Label("Sidebar")]);
    tooltips.add(&button, "Toggle Sidebar", ActionId::ToggleSidebar);
    (button, attention)
}

/// The filter the list shows as a small pill (Unread ×); hidden while it shows everything. Its
/// words sit on the title's baseline (the visual QA: 2 px above it, centred).
fn filter_pill() -> (gtk::Button, gtk::Label) {
    let label = named(
        gtk::Label::builder()
            .valign(gtk::Align::BaselineCenter)
            .build(),
        "list-filter-name",
    );
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&label);
    content.append(&classed(
        icon(&["window-close-symbolic"]),
        &["sw-filter-pill-close"],
    ));
    let pill = named(
        classed(
            gtk::Button::builder()
                .child(&content)
                .valign(gtk::Align::BaselineCenter)
                .visible(false)
                .build(),
            &["sw-filter-pill"],
        ),
        "list-filter",
    );
    (pill, label)
}

/// `width` held to `range`, in px.
fn clamp_to(width: f64, range: &std::ops::RangeInclusive<u32>) -> f64 {
    width
        .round()
        .clamp(f64::from(*range.start()), f64::from(*range.end()))
}

/// Applies while the window is narrower than `width`.
fn breakpoint(width: f64) -> adw::Breakpoint {
    adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        width - 1.0,
        adw::LengthUnit::Px,
    ))
}

fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    // A conversation the reader laid out unseen isn't on screen: its widgets aren't the ones meant.
    if widget.has_css_class(crate::reader::STAGED) {
        return None;
    }
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_named(&current, name) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::actions::ActionId;

    #[test]
    fn buttons_are_bound_to_the_registry_names() {
        assert_eq!(COMPOSE_ACTION, ActionId::Compose.detailed_name());
        assert_eq!(OPEN_CONFIG_ACTION, ActionId::Preferences.detailed_name());
        assert_eq!(ADD_ACCOUNT_ACTION, ActionId::AddAccount.detailed_name());
        assert_eq!(
            TOGGLE_SIDEBAR_ACTION,
            ActionId::ToggleSidebar.detailed_name()
        );
        assert_eq!(SEARCH_ACTION, ActionId::Search.detailed_name());
    }

    #[test]
    fn width_classes_and_places_have_names() {
        assert_eq!(Layout::Medium.name(), "medium");
        assert_eq!(SidebarPlace::Overlay.name(), "overlay");
        const { assert!(NARROW_WIDTH < WIDE_WIDTH) };
    }
}
