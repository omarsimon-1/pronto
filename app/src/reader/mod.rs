//! The reader (ML-027, ML-057; ADR-011 approach A; DESIGN.md "Reader" and "HTML mail inside the
//! theme"): the conversation of the selected message, its subject as the heading and its
//! messages oldest first, the selected one, the unread ones and the latest expanded as cards
//! (header, banner and attachment cards drawn by GTK, a plain-text body in a text view, an HTML
//! body in a pooled, hardened WebView, see [`view`], styled from the live theme), the others as
//! one-line rows ([`conversation`]). Everything is read from the store off the GTK thread; the
//! conversation appears once the bodies the first screen needs have their heights, scrolled to
//! the selected message. At most [`MAX_LIVE_VIEWS`] bodies hold a view ([`live`]).
//!
//! Moving between conversations is one swap (the fork's Superhuman pass): the conversations
//! next to the selection ([`Reader::prefetch`]) and the one the selection just left are laid
//! out unseen beside the shown one ("warm" pages), so `j`, `k` and the message archive moves on
//! to show in the frame they are asked for; another message of the shown conversation is shown
//! in place; messages read and prepared, and heights measured, are kept ([`cache`]).
//!
//! The lead wires it: `Reader::new` with the store and the theme, `widget()` into
//! `Window::set_reader`, `toolbar()` into `Window::set_reader_toolbar`, the list's selection
//! into [`Reader::show`], the store's change events into [`Reader::store_changed`], and the
//! list's position into [`Reader::set_position`] with [`Reader::connect_previous`] and
//! [`Reader::connect_next`].

mod attachments;
mod cache;
mod cards;
pub mod commands;
mod conversation;
mod engine;
mod export;
mod files;
mod format;
mod links;
mod live;
mod page;
mod plain;
mod prepare;
mod protection;
mod remote;
mod reply;
pub mod samples;
mod show;
mod source;
mod toolbar;
mod view;
mod widgets;

pub use attachments::CardState;
pub use conversation::{FOLD_LEAST, GAP_STEP, KEEP_BEFORE};
pub use engine::{
    CONTENT_SECURITY_POLICY, CidOutcome, CidRequest, InlinePart, MAX_LIVE_VIEWS, NETWORK_FEATURES,
    PAGE_VIEWS, PartLoader, RETIRED_FEATURES,
};
pub use export::{ORIGINAL_SHOWN, OriginalDialog, original_dialog};
pub use files::FileChoice;
pub use format::short_date;
pub use links::LinkState;
pub use plain::{links_in, shown_text};
pub use protection::{ENCRYPTED, SIGNED, Unreadable};
pub use reply::{
    Local, PLACEHOLDER, PopIn, QuickDraft, QuickReport, QuickState, SendMode, planned,
    pop_out_tooltip, recipients_label, reply_label, reply_target,
};
pub use sealwax_domain::html_scan::{Evidence, Scan, scan as scan_html};
/// A plain-text body's measure, in characters: the themed HTML body's own.
pub use sealwax_domain::theme::READING_MEASURE;
pub use show::column_width;
pub use source::{MessageSource, Reads};
pub use toolbar::{Position, tooltip};
pub use view::{Hover, Link, MAX_VIEW_HEIGHT, Refused, ViewEvent, WORLD, disabled_features};
pub use widgets::STAGED;

use crate::actions::{self, ScopedShortcuts};
use crate::engine_link::{EngineLink, Subscription};
use crate::theme::{ListenerId, ThemeController};
use conversation::{Entry, Item, Page};
use engine::Engine;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::config::{ConversationOrder, DefaultReply};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{MessageId, MessageSummary, ThreadId};
use sealwax_domain::theme::{BodyStyle, Rgb, Theme};
use sealwax_engine::api::{BodyError, Command, Event};
use sealwax_infrastructure::store::ChangeEvent;
use sealwax_infrastructure::store::ConversationMessage;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};
use toolbar::Toolbar;
use widgets::Stage;

/// How long a new conversation may take to lay out while a previous one is on screen.
const READY_TIMEOUT: Duration = Duration::from_secs(3);
/// An empty reader has nothing useful to show during WebKit's first layout. Reveal the
/// conversation progressively after this wait, keeping the selected message anchored while
/// its bodies fill in. Later switches retain [`READY_TIMEOUT`] and [`LEFT_SHOWN`].
const FIRST_READY_TIMEOUT: Duration = Duration::from_millis(450);
/// How long a conversation stays on screen while the next one is laid out, when the selection
/// moves on (`j`/`k`, a click, Previous/Next) or its message left the list (archived, deleted,
/// moved, gone in a sync): PRD's bar for opening cached HTML. Then the next one shows as far as
/// it is laid out, its bodies filling in; one not read yet hides the left one until it is.
pub const LEFT_SHOWN: Duration = Duration::from_millis(150);
/// A reader hidden this long gives its views back (ADR-011: views of hidden bodies are released
/// and re-rendered on return).
const RELEASE_HIDDEN_AFTER: Duration = Duration::from_secs(3);
/// Keys a focused message body keeps from the Reader scope: WebKitGTK 2.52 and the plain-text
/// view take Delete (seen headless).
const KEPT_KEYS: [gdk::Key; 1] = [gdk::Key::Delete];
/// Conversations laid out ahead at most: the two next to the selection and the one it left.
const MAX_WARM_PAGES: usize = 3;
/// `show`s closer together than this are a held key: the conversations between are read only
/// when the key rests for [`COALESCE`] (one laid out ahead still shows at once).
const REPEAT: Duration = Duration::from_millis(60);
const COALESCE: Duration = Duration::from_millis(40);
/// A layout wait wakes on GTK's layout and on its views' events; this is only a backstop.
const LAYOUT_BACKSTOP: Duration = Duration::from_millis(150);

/// What [`Reader::show`] showed, for listeners such as the self-test and measurements.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub id: MessageId,
    /// The selected message's body.
    pub body: ShownBody,
    /// From `show` to the conversation read.
    pub fetched: Duration,
    /// To the expanded messages read and their bodies prepared off the GTK thread.
    pub prepared: Duration,
    /// To the conversation on screen. Usually its first screen's bodies are laid out at
    /// their width with images in; after the layout deadline they can still be filling in.
    pub ready: Duration,
    /// Messages in the conversation, folded ones included.
    pub messages: usize,
    /// Messages shown expanded.
    pub expanded: usize,
    /// It came from a page laid out ahead ([`Reader::prefetch`], or the conversation the
    /// selection left): swapped in, nothing read or laid out on the way.
    pub ahead: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShownBody {
    Html {
        style: BodyStyle,
        remote_images: bool,
        remote_allowed: bool,
        inline_parts: usize,
    },
    Plain,
    /// HTML shown as text because no WebView could show it safely (no content filter, or the
    /// web process ended twice).
    TextFallback,
    Empty,
    /// The body isn't stored and nothing can download it (no engine).
    NotDownloaded,
    /// The engine is downloading the body (ML-044); the snippet shows meanwhile.
    Downloading,
    /// The download failed; the reader says why and offers Retry.
    DownloadFailed(BodyError),
    /// The store no longer has the message, or couldn't read it.
    Missing,
    /// Encrypted, or sealed in an S/MIME signature: a notice instead (ML-064).
    Unreadable(Unreadable),
}

impl ShownBody {
    /// The message's own text is on screen (not a notice in its place); an encrypted message's
    /// notice is all there is of it.
    pub fn is_shown(&self) -> bool {
        matches!(
            self,
            ShownBody::Html { .. }
                | ShownBody::Plain
                | ShownBody::TextFallback
                | ShownBody::Empty
                | ShownBody::Unreadable(_)
        )
    }
}

/// The conversation the reader shows, for the contact pane ([`Reader::conversation`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ShownConversation {
    pub thread: Option<ThreadId>,
    /// The message the reader was asked to show.
    pub selected: MessageId,
    /// Oldest first (as stored, whatever order the screen shows them in), folded ones too.
    pub messages: Vec<ConversationMessage>,
}

/// Bodies the reader put on screen (ML-057), for marking them read: at `show`, every expanded
/// message whose body is shown; later, a message the user expanded, or whose body arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    /// The message the reader was asked to show.
    pub selected: MessageId,
    pub messages: Vec<MessageSummary>,
}

/// The reader's state, for checks. The first fields describe the selected message.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderState {
    pub message: Option<MessageId>,
    pub body: Option<ShownBody>,
    pub banner: bool,
    pub attachments: usize,
    /// The selected message's HTML view: its height (px) and allocated width.
    pub view_size: Option<(i32, i32)>,
    /// The selected message's view went back to the pool while the reader was hidden, and
    /// hasn't been rendered again.
    pub released: bool,
    /// Views alive in the process, idle ones included.
    pub live_views: usize,
    /// Messages in the conversation, folded ones included.
    pub messages: usize,
    /// Expanded messages, in order.
    pub expanded: Vec<MessageId>,
    /// Messages shown as one-line rows, in order.
    pub collapsed: Vec<MessageId>,
    /// "N earlier messages" rows, and the messages they hold.
    pub folds: usize,
    pub folded: usize,
    /// Expanded HTML bodies, and those that hold a view now.
    pub html_bodies: usize,
    pub live_bodies: Vec<MessageId>,
}

/// An expanded HTML body of the shown conversation, for checks: where the column shows it (px
/// from the top of its visible part), whether it holds a view, and its heights.
#[derive(Debug, Clone, PartialEq)]
pub struct BodyReport {
    pub id: MessageId,
    pub top: f64,
    pub bottom: f64,
    pub live: bool,
    /// The view's current load fired its `load` event and painted a frame.
    pub loaded: bool,
    pub painted: bool,
    /// The height last measured, and the height its box has now.
    pub measured: Option<i32>,
    pub height: i32,
}

type ShownListener = Rc<dyn Fn(&Shown)>;
type MovedListener = Rc<dyn Fn(MessageId)>;
type OpenedListener = Rc<dyn Fn(&Opened)>;
type SizeListener = Rc<dyn Fn(usize)>;
type EventListener = Rc<dyn Fn(MessageId, &ViewEvent)>;

/// Asks for the bodies of messages; `false` when nothing can download them.
type Fetcher = Rc<dyn Fn(&[MessageId]) -> bool>;

#[derive(Clone)]
pub struct Reader {
    inner: Rc<Inner>,
}

struct Inner {
    store: Rc<dyn MessageSource>,
    theme: Rc<ThemeController>,
    engine: Rc<Engine>,
    stage: Stage,
    toolbar: Rc<Toolbar>,
    keys: ScopedShortcuts,
    /// The conversation on screen.
    shown: RefCell<Option<Page>>,
    /// The conversation being read and laid out unseen; it replaces the shown one when ready.
    pending: RefCell<Option<Page>>,
    /// Conversations laid out unseen ahead of the selection, and the one it left, most recently
    /// wanted first ([`Reader::prefetch`]); each can replace the shown one at once.
    warm: RefCell<Vec<Page>>,
    /// The messages next to the selection, whose conversations are laid out ahead.
    neighbours: RefCell<Vec<MessageId>>,
    /// The key of the warm page being read and built (one at a time).
    warming: Cell<Option<u64>>,
    /// Hands out page keys; a `show`'s page takes the generation the `show` made.
    serial: Cell<u64>,
    /// Set by every `show` and `clear` from `serial`; a load of an older one is stale.
    generation: Cell<u64>,
    /// When the last `show` came: a held key's `show`s are coalesced.
    last_show: Cell<Option<Instant>>,
    /// This window's construction, including the wait for its first frame before the app
    /// installs the reader. Examples default to the reader's own creation time.
    window_opened: Cell<Instant>,
    /// Report open → selection → shown once per reader/window, even after `clear`.
    first_preview_logged: Cell<bool>,
    /// What the last `show` reported.
    last_shown: RefCell<Option<Shown>>,
    /// Messages read and prepared, and heights measured ([`cache`]).
    prepared: RefCell<cache::PreparedCache>,
    heights: RefCell<cache::Heights>,
    /// Tags card loads and view loads, so late answers are dropped.
    tokens: Cell<u64>,
    /// The compiled content filter, once the engine has it.
    filter: RefCell<Option<webkit6::UserContentFilter>>,
    /// Whose remote images load: messages allowed this session, and senders (ML-058).
    remote: remote::Policy,
    /// Messages whose view's web process ended; a second time, they are shown as text.
    crashed: RefCell<HashMap<MessageId, u32>>,
    listener: Cell<Option<ListenerId>>,
    /// The status strip, the warning and the vetted opener (ML-062); its overlay is the root.
    links: Rc<links::Links>,
    on_shown: RefCell<Vec<ShownListener>>,
    on_opened: RefCell<Vec<OpenedListener>>,
    on_size: RefCell<Vec<SizeListener>>,
    on_event: RefCell<Vec<EventListener>>,
    /// Told the message the user moved to (the contact pane).
    on_moved: RefCell<Vec<MovedListener>>,
    /// Told when the column scrolls while the inline reply is open: what keeps off the reply
    /// (the window's toasts) moves with it.
    on_reply_moved: RefCell<Vec<Rc<dyn Fn()>>>,
    hidden_timer: RefCell<Option<glib::SourceId>>,
    /// The window whose focus the reader follows, and the handlers.
    focus_window: RefCell<Option<(glib::WeakRef<gtk::Window>, Vec<glib::SignalHandlerId>)>>,
    /// Downloads bodies the store doesn't hold (ML-044); without one they are "not downloaded".
    fetcher: RefCell<Option<Fetcher>>,
    /// Bodies asked for and not answered yet.
    fetching: RefCell<HashSet<MessageId>>,
    /// The engine's answer for a message whose card loads again, read by that load.
    answers: RefCell<HashMap<MessageId, Result<(), BodyError>>>,
    /// The message the newest `show` is for.
    target: Cell<Option<MessageId>>,
    engine_events: RefCell<Option<Subscription>>,
    /// The live views' scheduling and the column's scroll ([`live`]).
    scheduling: live::Scheduling,
    /// A change to the shown conversation waits for its re-read.
    refresh_queued: Cell<bool>,
    /// Woken by every body event and GTK's layout of a page laid out unseen, for the layout
    /// wait of a new conversation ([`Inner::wake`]).
    waiters: RefCell<Vec<async_channel::Sender<()>>>,
    /// Attachments, Print, Show Original and Save as .eml (ML-061, ML-063).
    files: files::Files,
    /// The Composer scope's keys, on each conversation's quick reply (M6, ML-068).
    composer_keys: ScopedShortcuts,
    /// The keymap in use, for the cards' Reply buttons and the quick reply.
    keymap: RefCell<Keymap>,
    /// The user's own addresses, as the last card read them: the quick reply answers the
    /// latest message that isn't theirs.
    mine: RefCell<Vec<String>>,
    /// The quick replies' texts, and where compose requests and Send go (M6).
    quick: reply::Quick,
    /// `config.toml`'s `conversation_order` is `newest-first` (issue #3).
    newest_first: Cell<bool>,
    /// `config.toml`'s `[reader] reply` is `all` (the default since phase C, Superhuman's
    /// Enter): Enter in a conversation and a click in its reply box answer everyone; `r` and
    /// the cards' Reply still answer the sender.
    enter_all: Cell<bool>,
    /// The list shows a row per message (Group by Conversation off): the heading says the
    /// selected message's subject, as its row does, not the conversation's.
    message_rows: Cell<bool>,
    /// The shown page the selection moved on from, and when it must be off screen.
    left: Cell<Option<(u64, Instant)>>,
    /// The shown page is hidden: the selection moved on from it and the next page isn't read yet.
    hidden: Cell<bool>,
    /// Pages being laid out never finish, for the self-tests ([`Reader::hold_layout`]).
    layout_held: Cell<bool>,
    /// `n` or `p` put the focus on a card's header while its body loaded: the body takes it
    /// once loaded, if it is there still ([`Inner::focus_reached`]).
    focus_body: Cell<Option<(u64, MessageId)>>,
    /// The message of the shown page the keyboard focus was last in, and the page's key
    /// ([`Inner::follow_focus`]).
    focus_in: Cell<Option<(u64, MessageId)>>,
}

impl Reader {
    /// A reader over `store`, styled by `theme`, with the Reader scope's keys from `keymap`.
    /// `cache` is Pronto's cache folder (`SealwaxDirs::cache`); the compiled content filter
    /// lives there.
    pub fn new(
        store: Rc<dyn MessageSource>,
        theme: Rc<ThemeController>,
        keymap: &Keymap,
        cache: &Path,
    ) -> Reader {
        let opened = Instant::now();
        let stage = Stage::default();
        stage.add_css_class("sw-reader");
        stage.add_css_class("sw-reader-stage");
        stage.set_widget_name("reader-message");
        stage.set_hexpand(true);
        let toolbar = Toolbar::new(keymap);
        let keys = actions::install_scoped(&stage, Scope::Reader, keymap);
        keys.attach(toolbar.widget(), gtk::PropagationPhase::Bubble);
        let links = links::Links::new(&stage, Rc::new(launch));
        let inner = Rc::new(Inner {
            store,
            theme,
            engine: Engine::shared(cache),
            stage,
            toolbar,
            keys,
            shown: RefCell::new(None),
            pending: RefCell::new(None),
            warm: RefCell::new(Vec::new()),
            neighbours: RefCell::new(Vec::new()),
            warming: Cell::new(None),
            serial: Cell::new(0),
            generation: Cell::new(0),
            last_show: Cell::new(None),
            window_opened: Cell::new(opened),
            first_preview_logged: Cell::new(false),
            last_shown: RefCell::new(None),
            prepared: RefCell::default(),
            heights: RefCell::default(),
            tokens: Cell::new(0),
            filter: RefCell::new(None),
            remote: remote::Policy::default(),
            crashed: RefCell::new(HashMap::new()),
            listener: Cell::new(None),
            links,
            on_shown: RefCell::new(Vec::new()),
            on_opened: RefCell::new(Vec::new()),
            on_size: RefCell::new(Vec::new()),
            on_event: RefCell::new(Vec::new()),
            on_moved: RefCell::new(Vec::new()),
            on_reply_moved: RefCell::new(Vec::new()),
            hidden_timer: RefCell::new(None),
            focus_window: RefCell::new(None),
            fetcher: RefCell::new(None),
            fetching: RefCell::new(HashSet::new()),
            answers: RefCell::new(HashMap::new()),
            target: Cell::new(None),
            engine_events: RefCell::new(None),
            scheduling: live::Scheduling::default(),
            refresh_queued: Cell::new(false),
            waiters: RefCell::new(Vec::new()),
            files: files::Files::new(cache),
            composer_keys: actions::ScopedShortcuts::new(Scope::Composer, keymap),
            keymap: RefCell::new(keymap.clone()),
            mine: RefCell::new(Vec::new()),
            quick: reply::Quick::default(),
            newest_first: Cell::new(false),
            enter_all: Cell::new(true),
            message_rows: Cell::new(false),
            left: Cell::new(None),
            hidden: Cell::new(false),
            layout_held: Cell::new(false),
            focus_body: Cell::new(None),
            focus_in: Cell::new(None),
        });
        remote::load(&inner);
        let weak = Rc::downgrade(&inner);
        let id = inner.theme.connect_changed(move |theme| {
            if let Some(inner) = weak.upgrade() {
                inner.restyle(theme);
            }
        });
        inner.listener.set(Some(id));
        let weak = Rc::downgrade(&inner);
        inner.stage.set_quiet(move |page| {
            weak.upgrade()
                .is_some_and(|inner: Rc<Inner>| inner.quiet(page))
        });
        Inner::follow_visibility(&inner);
        Inner::take_kept_keys(&inner);
        Reader { inner }
    }

    /// Includes window construction and the first-frame delay in the first-preview log.
    /// Set once when the application installs this window's reader.
    pub(crate) fn set_window_opened(&self, opened: Instant) {
        self.inner.window_opened.set(opened);
    }

    /// Whether nothing in the reader waits for a frame: what it shows and what it lays out
    /// ahead have settled (loaded, painted, measured at their width and unchanged for a
    /// second). A reader kept warm behind the list is drawn (into nothing) only until then, so
    /// animated mail in it stops costing battery.
    pub fn is_quiet(&self) -> bool {
        self.inner.is_quiet()
    }

    /// [`Reader::is_quiet`] for a holder that mustn't keep the reader alive.
    pub fn quiet_check(&self) -> impl Fn() -> bool + 'static {
        let weak = Rc::downgrade(&self.inner);
        move || weak.upgrade().is_some_and(|inner| inner.is_quiet())
    }

    /// Pages laid out unseen that the stage drew so far, and left undrawn because they had
    /// settled: for checks.
    pub fn staged_draws(&self) -> (u64, u64) {
        self.inner.stage.staged_draws()
    }

    /// Whether every conversation laid out ahead has settled, so the stage no longer draws it,
    /// and there is one: for checks.
    pub fn ahead_settled(&self) -> bool {
        self.inner.ahead_settled()
    }

    /// The HTML bodies of the conversations laid out ahead: each one's measured height, if it
    /// is measured at its width. For checks.
    pub fn ahead_heights(&self) -> Vec<(MessageId, Option<i32>)> {
        self.inner
            .warm
            .borrow()
            .iter()
            .flat_map(|page| {
                page.items().filter_map(|item| {
                    let html = item.card.as_ref()?.html.as_ref()?;
                    let measured = html
                        .measured
                        .filter(|_| html.measured_at(html.holder.width()))
                        .map(|(height, _)| height);
                    Some((item.id(), measured))
                })
            })
            .collect()
    }

    /// Says the shown conversation's dates as of now. Laid out behind a Focused list when the
    /// cursor came to it, or left there by Escape, it comes on screen later.
    pub fn redate_shown(&self) {
        if let Some(page) = self.inner.shown.borrow().as_ref() {
            page.redate(now());
            // "tomorrow 08:00" said yesterday is today's (Remind Me's line, phase C).
            if let (Some(thread), Some(line)) = (page.thread, page.reminder.as_ref()) {
                conversation::show_reminder(line, crate::remind::line_text(thread).as_deref());
            }
        }
    }

    /// The reminders changed (Remind Me, phase C): every page laid out says again when its
    /// conversation comes back (`remind::line_text`), or nothing.
    pub fn reminders_changed(&self) {
        let update = |page: &conversation::Page| {
            if let (Some(thread), Some(line)) = (page.thread, page.reminder.as_ref()) {
                conversation::show_reminder(line, crate::remind::line_text(thread).as_deref());
            }
        };
        if let Some(page) = self.inner.shown.borrow().as_ref() {
            update(page);
        }
        if let Some(page) = self.inner.pending.borrow().as_ref() {
            update(page);
        }
        for page in self.inner.warm.borrow().iter() {
            update(page);
        }
    }

    /// What the shown conversation's reminder line says, while it shows (the self-test).
    pub fn reminder_text(&self) -> Option<String> {
        self.inner
            .shown
            .borrow()
            .as_ref()
            .and_then(|page| page.reminder.as_ref())
            .and_then(conversation::reminder_text)
    }

    /// The reader went off screen, the list shown in its place (Escape in a Focused or narrow
    /// window, or the select mode's page there; in Split the batch page replaces the page,
    /// which leaves it as for another conversation): the conversation it keeps showing there
    /// starts afresh when it comes back, as one left for another does (phase C's review: left
    /// with Escape and opened again with Enter, Reply still answered the message `p` had
    /// reached).
    pub fn left_screen(&self) {
        let key = self.inner.shown.borrow().as_ref().map(|page| page.key);
        if let Some(key) = key {
            self.inner.start_afresh(key);
        }
    }

    /// Starts the web process ahead of the first message, before the reader is first on
    /// screen (it maps with the first selection), so the first message after a start doesn't
    /// wait for it. For a reader in a window.
    pub fn prewarm(&self) {
        let engine = self.inner.engine.clone();
        let fonts = self.inner.theme.theme().webview_fonts();
        glib::idle_add_local_once(move || {
            glib::spawn_future_local(async move { engine.prewarm(&fonts).await });
        });
    }

    /// Warms blank views in an always-mapped part of the window, even while the reader's
    /// empty page hides its normal stage. No pixels or interactive controls are exposed.
    pub fn prewarm_in(&self, overlay: &gtk::Overlay) {
        self.inner.engine.warm_in(overlay);
        self.prewarm();
    }

    /// A window closing into background mode releases its messages and keeps just two blank
    /// views and their shared web process ready for its next opening.
    pub fn rest(&self) {
        self.clear();
        self.inner.engine.rest();
    }

    /// For `Window::set_reader`: the page with the link status strip over it.
    pub fn widget(&self) -> gtk::Widget {
        self.inner.links.root().clone().upcast()
    }

    /// For `Window::set_reader_toolbar`.
    pub fn toolbar(&self) -> gtk::Widget {
        self.inner.toolbar.widget().clone().upcast()
    }

    /// The Reader scope's keys on the reader and its toolbar.
    pub fn keys(&self) -> &ScopedShortcuts {
        &self.inner.keys
    }

    /// The order of a conversation's messages (`config.toml`'s `conversation_order`); the shown
    /// conversation is shown again in it.
    pub fn set_conversation_order(&self, order: ConversationOrder) {
        let newest_first = order == ConversationOrder::NewestFirst;
        if self.inner.newest_first.replace(newest_first) != newest_first {
            self.show_again();
        }
    }

    /// The list's rows are messages (Group by Conversation off): the heading says the selected
    /// message's subject, as its row does, on the page shown and those laid out ahead, and an
    /// empty reply box answers the selected message, the row the user picked, unless they
    /// moved to another (with a row per conversation, the latest again).
    pub fn set_message_rows(&self, message_rows: bool) {
        if self.inner.message_rows.replace(message_rows) == message_rows {
            return;
        }
        let inner = &self.inner;
        let mut boxed = Vec::new();
        for page in inner
            .shown
            .borrow()
            .iter()
            .chain(inner.pending.borrow().iter())
        {
            show::relabel(page, message_rows);
            boxed.extend(page.quick.is_some().then_some(page.key));
        }
        for page in inner.warm.borrow().iter() {
            show::relabel(page, message_rows);
            boxed.extend(page.quick.is_some().then_some(page.key));
        }
        for key in boxed {
            inner.retarget_quick_reply(key);
        }
    }

    /// Whom Enter's reply answers (`config.toml`'s `[reader] reply`: everyone by default): the
    /// inline reply's box follows it (its line names everyone it would answer); the shown
    /// conversation is shown again.
    pub fn set_default_reply(&self, reply: DefaultReply) {
        let all = reply == DefaultReply::All;
        if self.inner.enter_all.replace(all) != all {
            self.show_again();
        }
    }

    /// The shown conversation, shown again from the store.
    fn show_again(&self) {
        let shown = self
            .inner
            .shown
            .borrow()
            .as_ref()
            .filter(|page| !page.missing && !self.inner.hidden.get())
            .map(|page| page.selected);
        if let Some(id) = shown {
            self.inner.drop_warm(|_| true);
            self.inner.read(id, false);
        }
    }

    /// `keys.toml` changed: the Reader scope's keys and the toolbar's tooltips follow.
    pub fn update_keys(&self, keymap: &Keymap) {
        self.inner.keys.update(keymap);
        self.inner.toolbar.update_keys(keymap);
        self.inner.update_reply_keys(keymap);
    }

    /// Shows the conversation of the message `id`, scrolled to it. Returns at once; the
    /// conversation replaces the shown one when it is ready (at once when it is laid out
    /// ahead, or `id` is in the shown one). A newer `show` or [`Reader::clear`] cancels it.
    pub fn show(&self, id: MessageId) {
        self.inner.show(id, false);
    }

    /// [`Reader::show`] in place of the shown message, the selection having moved on from it
    /// or the message having left the list: the left conversation stays on screen at most
    /// [`LEFT_SHOWN`], then the new one shows as far as it is laid out, or the left one is
    /// hidden until the new one is read.
    pub fn show_next(&self, id: MessageId) {
        self.inner.show(id, true);
    }

    /// The messages next to the selection, most wanted first (the list's next and previous
    /// rows: where `j`, `k`, archive and delete go): their conversations are read and laid out
    /// unseen, one at a time once the shown one is on screen, so moving to one is a swap. Pages
    /// laid out for others are dropped. Call after every [`Reader::show_next`].
    pub fn prefetch(&self, ids: &[MessageId]) {
        self.inner.prefetch(ids);
    }

    /// Downloads the bodies the store doesn't hold through `fetch` (ML-044): the reader asks
    /// for the expanded messages' bodies, shows "Downloading…" meanwhile, and each message once
    /// [`Reader::store_changed`] or [`Reader::body_fetched`] says it is there. `fetch` returns
    /// `false` when nothing can download them.
    pub fn set_body_fetcher(&self, fetch: impl Fn(&[MessageId]) -> bool + 'static) {
        self.inner.fetcher.replace(Some(Rc::new(fetch)));
    }

    /// Asks `engine` for bodies and takes its answers.
    pub fn connect_engine(&self, engine: &EngineLink) {
        let link = engine.clone();
        self.set_body_fetcher(move |messages| {
            link.send(Command::FetchBodies {
                messages: messages.to_vec(),
            })
            .is_ok()
        });
        let weak = Rc::downgrade(&self.inner);
        let subscription = engine.subscribe(move |event| {
            if let Event::BodyFetched { message, result } = event
                && let Some(inner) = weak.upgrade()
            {
                Reader { inner }.body_fetched(*message, *result);
            }
        });
        self.inner.engine_events.replace(Some(subscription));
        files::connect_engine(self, engine);
    }

    /// The engine's answer to a download: a failure is shown with Retry, a success shows the
    /// message unless the store's change event already did.
    pub fn body_fetched(&self, id: MessageId, result: Result<(), BodyError>) {
        self.inner.fetching.borrow_mut().remove(&id);
        self.inner.body_answered(id, result);
    }

    /// A store commit (ADR-003's change events): the shown conversation is read again when the
    /// commit touched it (a reply arrived, flags changed, a message moved away or was expunged,
    /// two conversations merged), keeping its bodies and its scroll; a message waiting for its
    /// body, or whose download failed, loads again, a failure staying rather than asking again.
    pub fn store_changed(&self, event: &ChangeEvent) {
        self.inner.store_changed(event);
    }

    /// Messages a store commit changed (M3's hook): [`Reader::store_changed`] for those alone.
    pub fn messages_changed(&self, ids: &[MessageId]) {
        self.inner.store_changed(&ChangeEvent {
            changed: ids.to_vec(),
            ..ChangeEvent::default()
        });
    }

    /// Shows nothing and gives every view back.
    pub fn clear(&self) {
        self.inner.target.set(None);
        self.inner.next_generation();
        self.inner.discard_pending();
        self.inner.neighbours.borrow_mut().clear();
        self.inner.drop_warm(|_| true);
        self.inner.left.set(None);
        self.inner.hidden.set(false);
        self.inner.stage.show(None::<&gtk::Widget>, None, false);
        if let Some(page) = self.inner.shown.take() {
            self.inner.release_page(page);
        }
        self.inner.size_changed();
        self.inner.quick_changed();
    }

    /// The selected message: the one the reader was asked to show, once its conversation is on
    /// screen.
    pub fn message(&self) -> Option<MessageId> {
        if self.inner.hidden.get() {
            return None;
        }
        self.inner.shown.borrow().as_ref().map(|page| page.selected)
    }

    /// The message of the shown conversation the user is on: the one they moved to
    /// ([`Reader::chosen_message`]), else the selected one. Print, Show Original, Save as .eml
    /// and Show Images act on it (they took the selected one, whichever message of the
    /// conversation was being read).
    pub fn message_in_hand(&self) -> Option<MessageId> {
        let selected = self.message()?;
        Some(self.chosen_message().unwrap_or(selected))
    }

    /// Opening a conversation (the Superhuman pass, 2026-10-28): the keyboard focus goes into
    /// the conversation of `id` when it is the one on screen, in the selected message's body,
    /// else its header or row. False while it is still being read or laid out.
    pub fn focus_message(&self, id: MessageId) -> bool {
        let (target, key) = {
            let shown = self.inner.shown.borrow();
            let Some(page) = shown
                .as_ref()
                .filter(|page| page.selected == id && !self.inner.hidden.get())
            else {
                return false;
            };
            let in_message = page
                .selected_card()
                .and_then(|card| card.focus_target())
                .or_else(|| page.item(id).map(|item| item.row.clone().upcast()));
            match in_message {
                Some(target) => (target, Some(page.key)),
                None => (page.root.clone().upcast::<gtk::Widget>(), None),
            }
        };
        // The reader's doing: no move of the user's (the box answers the latest message).
        let given = match key {
            Some(key) => self.inner.give_focus(key, id, &target),
            None => target.grab_focus(),
        };
        given || self.inner.stage.grab_focus()
    }

    /// "n of N" in the toolbar, or nothing; previous and next follow it.
    pub fn set_position(&self, position: Option<Position>) {
        self.inner.toolbar.set_position(position);
    }

    /// A toolbar command's title for the selected messages and why it doesn't apply (M4).
    pub fn set_command(&self, id: ActionId, title: &str, reason: Option<&str>) {
        self.inner.toolbar.set_command(id, title, reason);
    }

    /// Whether the selected messages are starred, for Star's icon in the toolbar.
    pub fn set_starred(&self, starred: bool) {
        self.inner.toolbar.set_starred(starred);
    }

    /// The title the toolbar's More menu shows for `id` (Mark as Read or Unread, Move to Junk or
    /// Not Junk, Print), for checks.
    pub fn more_title(&self, id: ActionId) -> Option<String> {
        self.inner.toolbar.more_title(id)
    }

    /// The key the toolbar's More menu shows beside `id` (`u`), for checks.
    pub fn more_key(&self, id: ActionId) -> Option<String> {
        self.inner.toolbar.more_key(id)
    }

    /// The key drawn beside `id` in More ("u", "Ctrl+P").
    pub fn more_key_shown(&self, id: ActionId) -> Option<String> {
        self.inner.toolbar.more_key_shown(id)
    }

    pub fn connect_previous(&self, callback: impl Fn() + 'static) {
        self.inner.toolbar.connect_previous(callback);
    }

    pub fn connect_next(&self, callback: impl Fn() + 'static) {
        self.inner.toolbar.connect_next(callback);
    }

    /// Shows the remote images (https only) of the message the user is on, in this reader until
    /// Pronto quits; the same as its banner's "Show images".
    pub fn show_remote_images(&self) {
        if let Some(id) = self.message_in_hand() {
            self.inner.allow_remote(id);
        }
    }

    /// Moves the clock the reader writes its dates by `seconds` ahead of the real one, as if
    /// that much time had passed ("25 min ago" becomes a date). For the self-tests.
    pub fn set_clock(&self, seconds: i64) {
        CLOCK.with(|clock| clock.set(seconds));
    }

    /// Holds the layout of the conversations to come unfinished while `held`, as a slow machine
    /// would: they show only when the shown one may stay no longer. For the self-tests.
    pub fn hold_layout(&self, held: bool) {
        self.inner.layout_held.set(held);
        if !held {
            self.inner.wake();
        }
    }

    /// Runs after every conversation shown.
    pub fn connect_shown(&self, listener: impl Fn(&Shown) + 'static) {
        self.inner.on_shown.borrow_mut().push(Rc::new(listener));
    }

    /// Runs when message bodies come on screen ([`Opened`]), for marking them read.
    pub fn connect_opened(&self, listener: impl Fn(&Opened) + 'static) {
        self.inner.on_opened.borrow_mut().push(Rc::new(listener));
    }

    /// Runs when the shown conversation's size changes (0 when nothing is shown), for Expand
    /// All and Collapse All.
    pub fn connect_size(&self, listener: impl Fn(usize) + 'static) {
        self.inner.on_size.borrow_mut().push(Rc::new(listener));
    }

    /// Runs when the user moves to a message of the shown conversation (`n`, `p`, a press,
    /// or Tab from another message): the contact pane follows the reader's choice.
    pub fn connect_moved(&self, listener: impl Fn(MessageId) + 'static) {
        self.inner.on_moved.borrow_mut().push(Rc::new(listener));
    }

    /// The conversation on screen (or laid out behind a Focused list): its thread and its
    /// messages oldest first, folded ones included, with their folders' roles. `None` while
    /// nothing is shown, or the shown one is gone (or while the reader is changing the page).
    pub fn conversation(&self) -> Option<ShownConversation> {
        if self.inner.hidden.get() {
            return None;
        }
        let shown = self.inner.shown.try_borrow().ok()?;
        let page = shown.as_ref().filter(|page| !page.missing)?;
        let mut messages = Vec::new();
        for entry in &page.entries {
            match entry {
                Entry::Message(item) => messages.push(item.message.clone()),
                Entry::Fold(fold) => messages.extend(fold.messages.iter().cloned()),
            }
        }
        Some(ShownConversation {
            thread: page.thread,
            selected: page.selected,
            messages,
        })
    }

    /// The message of the shown conversation whose card or row has the keyboard focus.
    /// `None` too while the reader is changing the page (it moves the focus as it does).
    pub fn focused_message(&self) -> Option<MessageId> {
        let focus = self.inner.stage.root().and_then(|root| root.focus())?;
        if !focus.is_ancestor(&self.inner.stage) {
            return None;
        }
        let shown = self.inner.shown.try_borrow().ok()?;
        let page = shown.as_ref()?;
        page.items()
            .find(|item| {
                focus == *item.slot.upcast_ref::<gtk::Widget>() || focus.is_ancestor(&item.slot)
            })
            .map(Item::id)
    }

    /// Every event of the views that show messages (links, refusals, resource loads): for the
    /// hostile suite and the self-test.
    pub fn connect_view_event(&self, listener: impl Fn(MessageId, &ViewEvent) + 'static) {
        self.inner.on_event.borrow_mut().push(Rc::new(listener));
    }

    /// Replaces how vetted links open (`gtk::UriLauncher` by default), for tests.
    pub fn set_link_opener(&self, opener: impl Fn(&gtk::Widget, &str) + 'static) {
        self.inner.links.set_opener(Rc::new(opener));
    }

    /// Replaces the link-mismatch check (ML-062): called before a vetted link opens; `false`
    /// keeps it closed. By default a link whose text names another domain than its target
    /// asks first, in a dialog with Cancel as the default.
    pub fn set_link_check(&self, check: impl Fn(&Link) -> bool + 'static) {
        self.inner.links.set_check(Some(Rc::new(check)));
    }

    /// What the last `show` reported, for checks.
    pub fn last_shown(&self) -> Option<Shown> {
        self.inner.last_shown.borrow().clone()
    }

    /// The link status strip, the warning and the dialogs shown, for checks.
    pub fn link_state(&self) -> LinkState {
        self.inner.links.state()
    }

    /// The link warning dialog on screen, for checks that answer it.
    pub fn link_dialog(&self) -> Option<libadwaita::AlertDialog> {
        self.inner.links.dialog()
    }

    /// Where `cid:` parts the store doesn't hold come from (ML-060; phase B: `FetchPart`).
    pub fn set_part_loader(&self, loader: Option<PartLoader>) {
        self.inner.engine.set_part_loader(loader);
    }

    /// The WebView of the selected message's HTML body, while it holds one.
    pub fn web_view(&self) -> Option<webkit6::WebView> {
        self.view_of(self.message()?)
    }

    /// The WebView of message `id`'s HTML body in the shown conversation, while it holds one.
    pub fn view_of(&self, id: MessageId) -> Option<webkit6::WebView> {
        self.inner.shown.borrow().as_ref().and_then(|page| {
            page.item(id)
                .and_then(|item| item.card.as_ref())
                .and_then(|card| card.html.as_ref())
                .and_then(|html| html.view.as_ref())
                .map(|view| view.web_view().clone())
        })
    }

    /// Runs `body` (a function body) in Pronto's isolated world of the selected message's
    /// HTML body.
    pub async fn eval(&self, body: &str) -> Result<webkit6::javascriptcore::Value, String> {
        let id = self.message().ok_or("no message is shown")?;
        self.eval_in(id, body).await
    }

    /// [`Reader::eval`] in message `id`'s HTML body.
    pub async fn eval_in(
        &self,
        id: MessageId,
        body: &str,
    ) -> Result<webkit6::javascriptcore::Value, String> {
        let view = self
            .inner
            .shown
            .borrow()
            .as_ref()
            .and_then(|page| page.item(id))
            .and_then(|item| item.card.as_ref())
            .and_then(|card| card.html.as_ref())
            .and_then(|html| html.view.clone())
            .ok_or("no HTML body with a view is shown for that message")?;
        view.eval(body).await.map_err(|error| error.to_string())
    }

    pub fn state(&self) -> ReaderState {
        let shown = self.inner.shown.borrow();
        let page = shown.as_ref().filter(|_| !self.inner.hidden.get());
        let card = page.and_then(Page::selected_card);
        let html = card.and_then(|card| card.html.as_ref());
        let mut state = ReaderState {
            message: page.map(|page| page.selected),
            body: page.map(|page| {
                if page.missing {
                    ShownBody::Missing
                } else {
                    card.map_or(ShownBody::Empty, |card| card.body.clone())
                }
            }),
            banner: card
                .and_then(|card| card.widgets.banner.as_ref())
                .is_some_and(|banner| banner.root.parent().is_some()),
            attachments: card.map_or(0, |card| card.attachments),
            view_size: html
                .and_then(|html| html.view.as_ref())
                .map(|view| (view.height(), view.web_view().width())),
            released: html.is_some_and(|html| html.released),
            live_views: self.inner.engine.live_views(),
            messages: page.map_or(0, Page::message_count),
            expanded: Vec::new(),
            collapsed: Vec::new(),
            folds: 0,
            folded: 0,
            html_bodies: 0,
            live_bodies: page.map(Page::live_bodies).unwrap_or_default(),
        };
        for entry in page.map_or(&[][..], |page| &page.entries[..]) {
            match entry {
                Entry::Message(item) if item.expanded => {
                    state.expanded.push(item.id());
                    if item.card.as_ref().is_some_and(|card| card.html.is_some()) {
                        state.html_bodies += 1;
                    }
                }
                Entry::Message(item) => state.collapsed.push(item.id()),
                Entry::Fold(fold) => {
                    state.folds += 1;
                    state.folded += fold.messages.len();
                }
            }
        }
        state
    }

    /// The shown conversation's expanded HTML bodies, where the column shows them.
    pub fn bodies(&self) -> Vec<BodyReport> {
        self.inner.body_reports()
    }

    /// Where message `id`'s row or card starts in the column, px from the top of what the
    /// column shows; `None` when it isn't in the shown conversation (or folded).
    pub fn message_top(&self, id: MessageId) -> Option<f64> {
        self.inner.message_top(id)
    }

    /// Where the keyboard focus is in message `id` of the conversation on screen, for checks:
    /// `body` (its body), `header` (its sender block or its actions), `row` (its collapsed row),
    /// `card` (elsewhere in its card); `None` when it isn't in that message.
    pub fn focus_in(&self, id: MessageId) -> Option<&'static str> {
        let focus = self.inner.stage.root().and_then(|root| root.focus())?;
        let shown = self.inner.shown.borrow();
        let item = shown.as_ref()?.item(id)?;
        let inside = |widget: &gtk::Widget| focus == *widget || focus.is_ancestor(widget);
        if inside(item.row.upcast_ref()) {
            return Some("row");
        }
        let card = item.card.as_ref()?;
        let view = card.html.as_ref().and_then(|html| html.view.as_ref());
        if view.is_some_and(|view| inside(view.web_view().upcast_ref()))
            || inside(card.widgets.body.upcast_ref())
        {
            Some("body")
        } else if inside(card.widgets.header.upcast_ref())
            || inside(card.widgets.actions.upcast_ref())
        {
            Some("header")
        } else {
            inside(card.widgets.root.upcast_ref()).then_some("card")
        }
    }

    /// Message `id` of the conversation on screen as the keyboard reaches it: its collapsed
    /// row, else its card's header, for checks that put the focus there as a program would
    /// (no press: the reader only learns where the focus went).
    pub fn focus_stop(&self, id: MessageId) -> Option<gtk::Widget> {
        if self.inner.hidden.get() {
            return None;
        }
        let shown = self.inner.shown.borrow();
        let item = shown.as_ref()?.item(id)?;
        Some(match &item.card {
            Some(card) => card.widgets.header.clone().upcast(),
            None => item.row.clone().upcast(),
        })
    }

    /// What message `id` of the shown conversation says where its date goes, on its card when
    /// it has one, else on its row, and whether that is in the draft colour (a draft says
    /// "Draft"), for checks.
    pub fn date_shown(&self, id: MessageId) -> Option<(String, bool)> {
        let shown = self.inner.shown.borrow();
        let item = shown.as_ref()?.item(id)?;
        let label = match &item.card {
            Some(card) => card.widgets.date.label.clone(),
            None => item
                .row
                .child()
                .and_then(|line| line.last_child())
                .and_downcast::<gtk::Label>()
                .filter(|label| label.widget_name() == "reader-row-date")?,
        };
        Some((
            label.label().to_string(),
            label.has_css_class("sw-reader-draft"),
        ))
    }

    /// Whether the column is bringing a widget into view (the inline reply opening), for
    /// checks.
    pub fn revealing(&self) -> bool {
        self.inner.revealing()
    }

    /// The date message `id`'s collapsed row says in the shown conversation (shown or not), for
    /// checks.
    pub fn row_date(&self, id: MessageId) -> Option<String> {
        let shown = self.inner.shown.borrow();
        let row = &shown.as_ref()?.item(id)?.row;
        row.child()
            .and_then(|line| line.last_child())
            .and_downcast::<gtk::Label>()
            .filter(|label| label.widget_name() == "reader-row-date")
            .map(|label| label.label().to_string())
    }

    /// The height kept for message `id`'s HTML body in the shown conversation ([`cache`]), and
    /// the height its view measured last, for checks; `None` when it shows no HTML body.
    pub fn kept_height(&self, id: MessageId) -> Option<(Option<i32>, Option<i32>)> {
        let (style, remote_allowed, collapse_quotes, measured) =
            self.inner.shown.borrow().as_ref().and_then(|page| {
                let html = page.item(id)?.card.as_ref()?.html.as_ref()?;
                Some((
                    html.style,
                    html.remote_allowed,
                    html.collapse_quotes,
                    html.measured.map(|(height, _)| height),
                ))
            })?;
        let kept = self
            .inner
            .height_key(id, style, remote_allowed, collapse_quotes)
            .and_then(|key| self.inner.heights.borrow().get(&key))
            .map(|kept| kept.height);
        Some((kept, measured))
    }

    /// The messages whose conversations are laid out unseen, ready to show at once, for checks.
    pub fn warm(&self) -> Vec<MessageId> {
        self.inner
            .warm
            .borrow()
            .iter()
            .filter(|page| self.inner.ready(page))
            .map(|page| page.selected)
            .collect()
    }

    /// Every `cid:` request the process's views made so far.
    pub fn cid_requests(&self) -> Vec<CidRequest> {
        self.inner.engine.cid_requests()
    }

    /// Downloads refused so far.
    pub fn refused_downloads(&self) -> u32 {
        self.inner.engine.refused_downloads()
    }

    /// What the reader reads through, for the commands on the shown message (M5).
    pub fn source(&self) -> Rc<dyn MessageSource> {
        self.inner.store.clone()
    }

    /// Prints the message the reader shows (ML-063).
    pub fn print(&self) {
        export::print(self);
    }

    /// Shows the raw source of the message the reader shows (ML-063).
    pub fn show_original(&self) {
        export::show_original(self);
    }

    /// Saves the message the reader shows as a `.eml` file (ML-063).
    pub fn save_message(&self) {
        export::save_message(self);
    }

    /// Next Message in Conversation (`n`): the message after the one last moved to expands and
    /// the column scrolls to it.
    pub fn next_in_conversation(&self) {
        self.inner.step(true);
    }

    /// Previous Message in Conversation (`p`).
    pub fn previous_in_conversation(&self) {
        self.inner.step(false);
    }

    /// Expands every message of the shown conversation, folded ones excepted (ML-057).
    pub fn expand_all(&self) {
        self.inner.expand_all();
    }

    /// Collapses every message of the shown conversation but the selected one (ML-057).
    pub fn collapse_all(&self) {
        self.inner.collapse_all();
    }

    /// Expands message `id` of the shown conversation, as a click on its row does.
    pub fn expand(&self, id: MessageId) {
        self.inner.expand(id);
    }

    /// Collapses message `id` of the shown conversation, as a click on its header does; the
    /// only expanded message stays.
    pub fn collapse(&self, id: MessageId) {
        self.inner.collapse(id);
    }

    /// Expand or Collapse Message: the message the user is on ([`Reader::message_in_hand`])
    /// expands, or collapses as a click on its header does.
    pub fn toggle_message(&self) {
        let Some(id) = self.message_in_hand() else {
            return;
        };
        let expanded = self
            .inner
            .shown
            .borrow()
            .as_ref()
            .and_then(|page| page.item(id))
            .map(|item| item.expanded);
        match expanded {
            Some(true) => self.inner.collapse(id),
            Some(false) => self.inner.expand(id),
            None => {}
        }
    }

    /// Message Details: shows or hides the details (From, To, Cc, the full date) of the message
    /// the user is on, as its card's details button does; a collapsed message expands first.
    pub fn toggle_details(&self) {
        let Some(id) = self.message_in_hand() else {
            return;
        };
        let toggle = {
            let shown = self.inner.shown.borrow();
            let Some(item) = shown.as_ref().and_then(|page| page.item(id)) else {
                return;
            };
            item.card.as_ref().map(|card| {
                card.widgets
                    .actions
                    .first_child()
                    .and_downcast::<gtk::ToggleButton>()
                    .filter(|toggle| toggle.widget_name() == "reader-details-toggle")
            })
        };
        match toggle {
            Some(Some(toggle)) => toggle.set_active(!toggle.is_active()),
            Some(None) => {}
            None => self.inner.expand(id),
        }
    }

    /// Reveals one step of the shown conversation's first "N earlier messages" row.
    pub fn unfold(&self) {
        self.inner.unfold_first();
    }

    /// Images from the shown message's sender load from now on (`true`) or are hidden again
    /// (`false`) (ML-058).
    pub fn set_always_show_images(&self, on: bool) {
        remote::set_always(self, on);
    }

    /// The shown conversation's bodies whose images the remote-content policy now allows load
    /// them (ML-058: after a sender was allowed).
    pub fn refresh_remote_images(&self) {
        self.inner.refresh_remote();
    }
}

/// Starts the process's blank reader views without constructing a window or a reader. Used
/// for a background launch; a later reader acquires the same process-scoped pool.
pub(crate) fn prewarm(cache: &Path, theme: &Theme) {
    let engine = Engine::shared(cache);
    let fonts = theme.webview_fonts();
    glib::spawn_future_local(async move { engine.prewarm(&fonts).await });
}

/// Whether a link may open, and as what: absolute http(s) with a host, or mailto.
pub fn openable(uri: &str) -> Option<String> {
    let parsed = glib::Uri::parse(uri.trim(), glib::UriFlags::NONE).ok()?;
    match parsed.scheme().to_ascii_lowercase().as_str() {
        "http" | "https" if parsed.host().is_some_and(|host| !host.is_empty()) => {
            Some(parsed.to_str().to_string())
        }
        "mailto" => Some(parsed.to_str().to_string()),
        _ => None,
    }
}

/// Opens a vetted link in the default browser or mail handler. Never under Broadway: a
/// headless run must not reach the live desktop's browser (S2 once opened a real page).
fn launch(widget: &gtk::Widget, uri: &str) {
    let broadway = gdk::Display::default()
        .is_some_and(|display| display.type_().name() == "GdkBroadwayDisplay");
    if broadway {
        tracing::warn!("reader: links don't open from a headless (Broadway) run");
        return;
    }
    let window = widget.root().and_downcast::<gtk::Window>();
    let widget = widget.clone();
    gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, move |result| {
        if let Err(error) = result {
            tracing::warn!(%error, "reader: the link didn't open");
            if !files::gave_up(&error) {
                files::toast_over(&widget, "Couldn't open the link.");
            }
        }
    });
}

fn rgba(color: Rgb) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        1.0,
    )
}

/// The body's user stylesheet (since M5 `Theme::webview_css` paints the themed root in the
/// theme's background itself).
fn stylesheet(theme: &Theme, style: BodyStyle) -> String {
    theme.webview_css(style)
}

thread_local! {
    /// Seconds added to the clock the reader writes dates by ([`Reader::set_clock`]).
    static CLOCK: Cell<i64> = const { Cell::new(0) };
}

fn now() -> i64 {
    glib::real_time() / 1_000_000 + CLOCK.with(Cell::get)
}

/// Why a body couldn't be downloaded, in plain words.
fn download_failure(error: BodyError) -> &'static str {
    match error {
        BodyError::Offline => {
            "This message couldn't be downloaded: the mail server can't be reached."
        }
        BodyError::NeedsSignIn => {
            "This message couldn't be downloaded: the account needs you to sign in again."
        }
        BodyError::Gone => "This message is no longer on the server.",
        BodyError::Store => "This message couldn't be saved on this computer.",
        BodyError::Other => "This message couldn't be downloaded.",
    }
}

impl Inner {
    /// A new page key.
    fn next_serial(&self) -> u64 {
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        serial
    }

    /// A new generation: every load of an earlier one is stale.
    fn next_generation(&self) -> u64 {
        let generation = self.next_serial();
        self.generation.set(generation);
        generation
    }

    fn stale(&self, generation: u64) -> bool {
        self.generation.get() != generation
    }

    fn next_token(&self) -> u64 {
        let token = self.tokens.get() + 1;
        self.tokens.set(token);
        token
    }

    /// Wakes every layout wait: a body's event, or GTK's layout of a page laid out unseen.
    fn wake(&self) {
        self.waiters
            .borrow_mut()
            .retain(|waiter| waiter.try_send(()).is_ok() || !waiter.is_closed());
    }

    /// A wake-up for one layout wait; dropping it ends the subscription.
    fn waiter(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        self.waiters.borrow_mut().push(sender);
        receiver
    }

    /// Runs `apply` on the page `key` names: shown, being laid out, or laid out ahead.
    fn with_page<R>(&self, key: u64, apply: impl FnOnce(&mut Page) -> R) -> Option<R> {
        let mut shown = self.shown.borrow_mut();
        if let Some(page) = shown.as_mut().filter(|page| page.key == key) {
            return Some(apply(page));
        }
        drop(shown);
        let mut pending = self.pending.borrow_mut();
        if let Some(page) = pending.as_mut().filter(|page| page.key == key) {
            return Some(apply(page));
        }
        drop(pending);
        let mut warm = self.warm.borrow_mut();
        warm.iter_mut().find(|page| page.key == key).map(apply)
    }

    /// Whether page `key` is the one on screen.
    fn is_shown(&self, key: u64) -> bool {
        self.shown
            .borrow()
            .as_ref()
            .is_some_and(|page| page.key == key)
    }

    /// Gives every view of `page` back to the pool.
    fn release_page(&self, mut page: Page) {
        for item in page.items_mut() {
            if let Some(view) = item
                .card
                .as_mut()
                .and_then(|card| card.html.as_mut())
                .and_then(|html| html.view.take())
            {
                self.engine.release(view);
            }
        }
    }

    /// Takes `page` off the stage and gives its views back.
    fn drop_page(&self, page: Page) {
        if self.stage.is_staged(&page.root) {
            self.stage.unstage(&page.root);
        }
        if self.warming.get() == Some(page.key) {
            self.warming.set(None);
        }
        self.release_page(page);
    }

    /// Drops the conversation being laid out, if any.
    fn discard_pending(&self) {
        if let Some(page) = self.pending.take() {
            self.drop_page(page);
        }
    }

    /// Drops the pages laid out ahead that `unwanted` names.
    fn drop_warm(&self, unwanted: impl Fn(&Page) -> bool) {
        let dropped: Vec<Page> = {
            let mut warm = self.warm.borrow_mut();
            let (dropped, kept) = std::mem::take(&mut *warm)
                .into_iter()
                .partition(|page| unwanted(page));
            *warm = kept;
            dropped
        };
        for page in dropped {
            self.drop_page(page);
        }
    }

    /// Every page: shown, being laid out, laid out ahead.
    fn pages(&self) -> [&RefCell<Option<Page>>; 2] {
        [&self.shown, &self.pending]
    }

    fn size_changed(&self) {
        let size = self.shown.borrow().as_ref().map_or(0, Page::message_count);
        let listeners = self.on_size.borrow().clone();
        for listener in listeners {
            listener(size);
        }
    }

    fn opened(&self, opened: Opened) {
        if opened.messages.is_empty() {
            return;
        }
        let listeners = self.on_opened.borrow().clone();
        for listener in listeners {
            listener(&opened);
        }
    }

    fn view_event(self: &Rc<Self>, id: MessageId, event: &ViewEvent) {
        let listeners = self.on_event.borrow().clone();
        for listener in listeners {
            listener(id, event);
        }
    }

    /// The theme changed: every live body takes its new stylesheet, background and fonts in
    /// place. GTK restyles the rest itself.
    fn restyle(&self, theme: &Theme) {
        let fonts = theme.webview_fonts();
        self.heights.borrow_mut().clear();
        let mut views = Vec::new();
        let now = Instant::now();
        let mut collect = |page: &mut Page| {
            for item in page.items_mut() {
                if let Some(html) = item.card.as_mut().and_then(|card| card.html.as_mut())
                    && let Some(view) = &html.view
                {
                    // Restyled, it is laid out anew: a page laid out unseen is drawn until it
                    // settles again.
                    html.changed = now;
                    views.push((view.clone(), html.style));
                }
            }
        };
        for pages in self.pages() {
            if let Some(page) = pages.borrow_mut().as_mut() {
                collect(page);
            }
        }
        for page in self.warm.borrow_mut().iter_mut() {
            collect(page);
        }
        let width = show::column_width(theme.scale.reading);
        let mut columns: Vec<libadwaita::Clamp> = Vec::new();
        for pages in self.pages() {
            columns.extend(pages.borrow().as_ref().map(|page| page.root.clone()));
        }
        columns.extend(self.warm.borrow().iter().map(|page| page.root.clone()));
        for column in columns {
            column.set_maximum_size(width);
            column.set_tightening_threshold(width);
        }
        for (view, style) in views {
            view.set_fonts(&fonts);
            view.set_style(
                &stylesheet(theme, style),
                &rgba(theme.webview_background(style)),
            );
        }
        self.redraw_unseen();
    }

    /// WebKitGTK and the plain-text view keep Delete although a read-only message has no use
    /// for it, so with a body focused the Reader scope's Delete would never run (M4's keyboard
    /// pass): the scope sees the keys of [`KEPT_KEYS`] before the body does. A text field
    /// inside an HTML message loses them.
    fn take_kept_keys(inner: &Rc<Inner>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(inner);
        keys.connect_key_pressed(move |controller, key, _, state| {
            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let in_view = inner
                .stage
                .root()
                .and_then(|root| root.focus())
                .is_some_and(|focus| {
                    focus.is::<webkit6::WebView>()
                        || focus.ancestor(webkit6::WebView::static_type()).is_some()
                        || focus
                            .downcast_ref::<gtk::TextView>()
                            .is_some_and(|view| !view.is_editable())
                });
            let (Some(event), true) = (controller.current_event(), in_view) else {
                return glib::Propagation::Proceed;
            };
            if !KEPT_KEYS.contains(&key) || !state.is_empty() {
                return glib::Propagation::Proceed;
            }
            let model = inner.keys.model();
            let shortcut = (0..model.n_items())
                .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
                .find(|shortcut| {
                    shortcut.trigger().is_some_and(|trigger| {
                        trigger.trigger(&event, false) == gdk::KeyMatch::Exact
                    })
                });
            let ran = shortcut
                .and_then(|shortcut| shortcut.action())
                .is_some_and(|action| {
                    action.activate(
                        gtk::ShortcutActionFlags::empty(),
                        &inner.stage,
                        None::<&glib::Variant>,
                    )
                });
            if ran {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        inner.stage.add_controller(keys);
    }

    /// Releases the views of a reader hidden for [`RELEASE_HIDDEN_AFTER`]; they come back when
    /// it is shown again. While it is shown, the column's scroll and layout drive the views.
    fn follow_visibility(inner: &Rc<Inner>) {
        let weak = Rc::downgrade(inner);
        inner.stage.connect_unmap(glib::clone!(
            #[strong]
            weak,
            move |_| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                inner.unwatch_column();
                let timer_owner = weak.clone();
                let timer = glib::timeout_add_local_once(RELEASE_HIDDEN_AFTER, move || {
                    if let Some(inner) = timer_owner.upgrade() {
                        inner.hidden_timer.replace(None);
                        inner.release_hidden();
                    }
                });
                if let Some(old) = inner.hidden_timer.replace(Some(timer)) {
                    old.remove();
                }
            }
        ));
        inner.stage.connect_map(move |_| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if let Some(timer) = inner.hidden_timer.take() {
                timer.remove();
            }
            inner.follow_focus();
            inner.watch_column();
            inner.schedule();
            inner.warm_next();
            let engine = inner.engine.clone();
            let fonts = inner.theme.theme().webview_fonts();
            glib::idle_add_local_once(move || {
                glib::spawn_future_local(async move { engine.prewarm(&fonts).await });
            });
        });
    }

    /// Scrolling to the focus is the reader's own job: the column's GtkViewport would scroll a
    /// tall body to its end as soon as the body takes the focus (a click into its text, Tab).
    /// The reader scrolls to every other widget that takes the focus. The focus going from one
    /// message of the shown conversation to another (Tab, Shift+Tab; WebKit's own Tab out of a
    /// body too) is the user's move there ([`Inner::moved_to`]); coming in from elsewhere (the
    /// list, the inline reply, a popover) it is none.
    fn follow_focus(self: &Rc<Self>) {
        let Some(viewport) = self
            .stage
            .ancestor(gtk::Viewport::static_type())
            .and_downcast::<gtk::Viewport>()
        else {
            return;
        };
        viewport.set_scroll_to_focus(false);
        let Some(window) = self.stage.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let followed = self
            .focus_window
            .borrow()
            .as_ref()
            .and_then(|(window, _)| window.upgrade())
            .is_some_and(|followed| followed == window);
        if followed {
            return;
        }
        let stage = self.stage.downgrade();
        let viewport = viewport.downgrade();
        let handler = window.connect_focus_widget_notify(move |window| {
            let (Some(stage), Some(viewport), Some(focus)) = (
                stage.upgrade(),
                viewport.upgrade(),
                GtkWindowExt::focus(window),
            ) else {
                return;
            };
            let body = focus.is::<webkit6::WebView>() || focus.is::<gtk::TextView>();
            if focus.is_ancestor(&stage) && !body {
                viewport.scroll_to(&focus, None);
            }
        });
        let weak = Rc::downgrade(self);
        let moves = window.connect_focus_widget_notify(move |window| {
            // Read once GTK and the reader are done (the reader may move the focus while it
            // holds its pages), in the order the focus moved.
            let focus = GtkWindowExt::focus(window).map(|focus| focus.downgrade());
            let weak = weak.clone();
            glib::idle_add_local_full(glib::Priority::HIGH, move || {
                if let Some(inner) = weak.upgrade() {
                    inner.focus_went(focus.as_ref().and_then(glib::WeakRef::upgrade).as_ref());
                }
                glib::ControlFlow::Break
            });
        });
        if let Some((old, handlers)) = self
            .focus_window
            .replace(Some((window.downgrade(), vec![handler, moves])))
            && let Some(old) = old.upgrade()
        {
            for handler in handlers {
                old.disconnect(handler);
            }
        }
    }

    /// The reader has been hidden for a while: every view goes back to the pool, the bodies
    /// keeping their heights.
    fn release_hidden(&self) {
        if self.stage.is_mapped() {
            return;
        }
        let release = |page: &mut Page, released: &mut Vec<view::MessageView>| {
            widgets::hand_over_focus(&page.root, None);
            for item in page.items_mut() {
                if let Some(html) = item.card.as_mut().and_then(|card| card.html.as_mut())
                    && let Some(view) = html.view.take()
                {
                    html.holder
                        .set_placeholder(html.measured.map_or(view.height(), |(height, _)| height));
                    html.released = true;
                    released.push(view);
                }
            }
        };
        let mut released = Vec::new();
        for pages in self.pages() {
            if let Some(page) = pages.borrow_mut().as_mut() {
                release(page, &mut released);
            }
        }
        for page in self.warm.borrow_mut().iter_mut() {
            release(page, &mut released);
        }
        if !released.is_empty() {
            tracing::debug!(
                views = released.len(),
                "reader: hidden, so its views went back to the pool"
            );
        }
        for view in released {
            self.engine.release(view);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(id) = self.listener.take() {
            self.theme.disconnect(id);
        }
        if let Some(timer) = self.hidden_timer.take() {
            timer.remove();
        }
        if let Some((window, handlers)) = self.focus_window.take()
            && let Some(window) = window.upgrade()
        {
            for handler in handlers {
                window.disconnect(handler);
            }
        }
        self.unwatch_column();
        if let Some(page) = self.pending.take() {
            self.release_page(page);
        }
        for page in self.warm.take() {
            self.release_page(page);
        }
        if let Some(page) = self.shown.take() {
            self.release_page(page);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absolute_web_and_mail_links_open() {
        assert_eq!(
            openable("https://example.test/a?b=c#d").as_deref(),
            Some("https://example.test/a?b=c#d")
        );
        assert!(openable("HTTP://example.test/").is_some());
        assert!(openable("mailto:leah@example.test?subject=Hi").is_some());
        for refused in [
            "",
            "about:blank",
            "about:blank#top",
            "cid:logo@example.test",
            "data:text/html,<p>x",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://example.test/",
            "rel-link",
            "//example.test/x",
            "https://",
            "pronto:compose",
        ] {
            assert_eq!(openable(refused), None, "{refused}");
        }
    }

    #[test]
    fn colours_become_rgba() {
        let color = rgba(Rgb::from_hex(0xff8000));
        assert_eq!(
            (color.red(), color.green(), color.blue(), color.alpha()),
            (1.0, 128.0 / 255.0, 0.0, 1.0)
        );
    }
}
