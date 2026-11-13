//! The GApplication (ML-020): one instance per app-id. A second `pronto mailto:…` hands its
//! command line to the running instance and exits; launchers activate it over D-Bus
//! (`DBusActivatable=true`: `Activate` and `Open`). The `command-line`, `open` and `activate`
//! handlers build the window synchronously: GApplication shuts down when one of them returns
//! with neither a window nor a hold (ADR-003 amendment, S6). Until the window is presented the
//! GTK thread does nothing but build it. SIGTERM, SIGHUP and SIGINT quit cleanly.

use crate::actions::{self, ActionSet, ScopedShortcuts};
use crate::background;
use crate::cli;
use crate::compose_actions;
use crate::composer::{self, ComposeRequest};
use crate::config;
use crate::default_handler;
use crate::desktop::{self, DesktopEvent, DesktopWatch};
use crate::engine::{self, EngineOptions};
use crate::engine_link::{EngineLink, Subscription};
use crate::list::{Content, MessageList, Quick, Selected};
use crate::mailbox::{self, Latch, Mailbox};
use crate::menu;
use crate::move_to;
use crate::notify;
use crate::oauth;
use crate::outbox;
use crate::palette;
use crate::pop3;
use crate::preferences::{self, Preferences, SettingsFile, Update};
use crate::reader::{self, MessageSource, Opened, Position, Reader};
use crate::search;
use crate::setup::{self, Again, edit::Editable, remove::Removable};
use crate::shortcuts;
use crate::status::{self, Waiting};
use crate::theme::{self, ThemeController};
use crate::triage::{
    self, Controller, Directory, MARK_READ_ON_OPEN, Notice, OpsBackend, Origin, Target,
};
use crate::updates;
use crate::window::{self, Attention, BannerAction, Pane, ViewMode, Window};
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;
use sealwax_domain::account::{AccountKey, OAuthProvider};
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::layout::Layout;
use sealwax_domain::mailto::{self, Mailto};
use sealwax_domain::model::{AccountId, DraftId, FolderId, FolderRole, MessageId, ThreadId};
use sealwax_domain::palette::{PALETTE_FILE, Recent};
use sealwax_domain::triage::Triage;
use sealwax_engine::api::{Command, Event};
use sealwax_engine::oauth::Registration;
use sealwax_engine::service::Engine;
use sealwax_infrastructure::store::{
    ListRow, RowKey, ViewFilter, ViewGrouping, ViewScope, ViewSpec,
};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::{Cell, OnceCell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The installed icon (hicolor), the same for debug and release builds.
pub const ICON_NAME: &str = "io.github.omarsimon1.Pronto";

/// Test hook: overrides the application id, so a self-test and the instances it starts share
/// an id that no real Pronto uses.
pub const APP_ID_ENV: &str = "PRONTO_APP_ID";

/// The signals that end a session: uwsm stops app units with SIGTERM at logout and reboot, a
/// closing terminal sends SIGHUP, Ctrl+C SIGINT. Linux's numbers.
const QUIT_SIGNALS: [(i32, &str); 3] = [(15, "SIGTERM"), (1, "SIGHUP"), (2, "SIGINT")];

/// How long the first window waits for `config.toml` and `keys.toml`, so its first frame already
/// has the user's corners, fonts and keys. The first read takes a few milliseconds.
const SETTINGS_WAIT: Duration = Duration::from_millis(150);

/// How long the first window waits for the store's first rows (S6: at most 300 ms), so its
/// first frame never shows an empty list. The store opens in about 2 ms.
const FIRST_ROWS_WAIT: Duration = Duration::from_millis(300);

/// How long an engine started without a window waits for the settings files: the accounts it
/// syncs come from `accounts.toml` (ML-084).
const ENGINE_SETTINGS_WAIT: Duration = Duration::from_secs(5);

/// How long the pending-op count waits after an action, an Undo or a change event, so a burst
/// of them is one count.
const PENDING_COUNT_DELAY: Duration = Duration::from_millis(200);

/// How often the pending-op count is read again while ops wait.
const PENDING_RECHECK: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub app_id: String,
    pub dirs: SealwaxDirs,
    pub exit_after: Option<Duration>,
    /// No title bar and no window controls ([`window::tiling_desktop`]).
    pub tiling: bool,
    /// Starts the engine after the first frame; self-tests start it from their script.
    pub engine: Option<EngineOptions>,
    /// The build's Microsoft client ID ([`oauth::BUILD_MICROSOFT_CLIENT_ID`]), which
    /// `config.toml` overrides. Self-tests run without one, whatever the build had.
    pub microsoft_client_id: Option<String>,
    /// The Google client from the environment or the build ([`oauth::google_client_from_env`]),
    /// which `config.toml`'s `[oauth.google]` overrides. Self-tests run without one.
    pub google_client: Option<Registration>,
}

impl AppConfig {
    pub fn from_env(exit_after: Option<Duration>) -> Self {
        Self {
            app_id: app_id_from_env(),
            dirs: SealwaxDirs::from_env(),
            exit_after,
            tiling: window::tiling_desktop(),
            engine: Some(EngineOptions::from_env()),
            microsoft_client_id: oauth::BUILD_MICROSOFT_CLIENT_ID.map(str::to_owned),
            google_client: oauth::google_client_from_env(),
        }
    }
}

pub fn app_id_from_env() -> String {
    std::env::var(APP_ID_ENV)
        .ok()
        .filter(|id| gio::Application::id_is_valid(id))
        .unwrap_or_else(|| config::APP_ID.to_owned())
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ProntoApplication {
        pub state: OnceCell<Rc<AppState>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProntoApplication {
        const NAME: &'static str = "ProntoApplication";
        type Type = super::ProntoApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for ProntoApplication {}

    impl ApplicationImpl for ProntoApplication {
        fn startup(&self) {
            let state = self.state();
            for hook in state.early_startup.take() {
                hook();
            }
            self.parent_startup();
            state.startup(&self.obj());
        }

        fn shutdown(&self) {
            self.state().stop_engine();
            self.state().mailbox.close();
            self.parent_shutdown();
        }

        fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
            self.state().command_line(&self.obj(), command_line)
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            self.state().open(&self.obj(), files);
        }

        fn activate(&self) {
            let state = self.state();
            let window = state.window_or_new(&self.obj());
            state.present(&window);
        }
    }

    impl GtkApplicationImpl for ProntoApplication {}
    impl AdwApplicationImpl for ProntoApplication {}

    impl ProntoApplication {
        fn state(&self) -> Rc<AppState> {
            self.state
                .get()
                .expect("the state is set when the application is built")
                .clone()
        }
    }
}

glib::wrapper! {
    pub struct ProntoApplication(ObjectSubclass<imp::ProntoApplication>)
        @extends adw::Application, gtk4::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl ProntoApplication {
    pub fn new(config: AppConfig, started: Instant) -> Self {
        let flags =
            gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::HANDLES_OPEN;
        let app: Self = glib::Object::builder()
            .property("application-id", config.app_id.as_str())
            .property("flags", flags)
            .build();
        let state = Rc::new(AppState {
            config,
            launched: started,
            started: Cell::new(Some(started)),
            window: RefCell::new(None),
            first_frame: Rc::new(Cell::new(None)),
            after_first_frame: Rc::default(),
            early_startup: RefCell::new(Vec::new()),
            on_first_window: RefCell::new(None),
            present_gate: RefCell::new(None),
            activations: Cell::new(0),
            background: RefCell::new(None),
            hidden: RefCell::new(None),
            quitting: Cell::new(false),
            theme: OnceCell::new(),
            prefs: OnceCell::new(),
            ai: OnceCell::new(),
            app_actions: OnceCell::new(),
            win_actions: RefCell::new(None),
            mailbox: Mailbox::new(),
            first_rows: Latch::new(),
            list: RefCell::new(None),
            store_warned: Cell::new(false),
            reader: RefCell::new(None),
            contact: RefCell::new(None),
            contact_selected: Cell::new(None),
            contact_moved: Cell::new(None),
            contact_opened: Cell::new(None),
            reader_shows: Cell::new(None),
            engine: EngineLink::new(),
            engine_service: RefCell::new(None),
            engine_events: RefCell::new(None),
            engine_pending: Cell::new(false),
            triage_listening: Cell::new(false),
            reader_listening: Cell::new(false),
            accounts_read: Cell::new(false),
            layout: RefCell::new(None),
            keyring_missing: Cell::new(false),
            keyring_dismissed: Cell::new(false),
            network: Cell::new(true),
            desktop: RefCell::new(None),
            triage: RefCell::new(None),
            toast: RefCell::new(None),
            mark_read_on_open: Cell::new(MARK_READ_ON_OPEN),
            read_on_leave: RefCell::new((None, Vec::new())),
            read_when_shown: RefCell::new(Vec::new()),
            focus_on_shown: Cell::new(None),
            layout_defaults: Cell::new((ViewMode::Focused, false)),
            grouping_default: Cell::new(ViewGrouping::Conversations),
            split_default: Cell::new(None),
            triage_started: Cell::new(None),
            pending_ops: Cell::new(0),
            pending_queued: Cell::new(false),
            pending_recheck: Cell::new(false),
            palette: RefCell::new(None),
            window_keys: RefCell::new(None),
            refresh_queued: Cell::new(false),
        });
        state
            .mailbox
            .set_default_grouping(state.grouping_default.get());
        let mailbox = state.mailbox.clone();
        let dirs = state.config.dirs.clone();
        state.connect_early_startup(move || mailbox.open(&dirs));
        if app.imp().state.set(state).is_err() {
            unreachable!("a new application has no state yet");
        }
        app
    }

    pub fn state(&self) -> Rc<AppState> {
        self.imp()
            .state
            .get()
            .expect("the state is set when the application is built")
            .clone()
    }
}

/// `layout.toml`'s writes, numbered as they are asked for, and the newest one on disk.
static LAYOUT_WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAYOUT_WRITTEN: std::sync::Mutex<u64> = std::sync::Mutex::new(0);

pub fn run(config: AppConfig, started: Instant) -> glib::ExitCode {
    ProntoApplication::new(config, started).run()
}

type Hook = Box<dyn FnOnce()>;
type WindowHook = Box<dyn FnOnce(Window)>;
type PresentGate = Pin<Box<dyn Future<Output = ()>>>;

pub struct AppState {
    config: AppConfig,
    /// When the process started.
    launched: Instant,
    started: Cell<Option<Instant>>,
    window: RefCell<Option<Window>>,
    first_frame: Rc<Cell<Option<Instant>>>,
    /// Work that waits until the first frame is painted, so it never delays it.
    after_first_frame: Rc<RefCell<Vec<Hook>>>,
    early_startup: RefCell<Vec<Hook>>,
    on_first_window: RefCell<Option<WindowHook>>,
    present_gate: RefCell<Option<PresentGate>>,
    activations: Cell<u32>,
    background: RefCell<Option<gio::ApplicationHoldGuard>>,
    /// Started with `--background`: held until a main window shows, whatever `[background]`
    /// says (ML-084).
    hidden: RefCell<Option<gio::ApplicationHoldGuard>>,
    quitting: Cell<bool>,
    theme: OnceCell<Rc<ThemeController>>,
    prefs: OnceCell<Rc<Preferences>>,
    ai: OnceCell<Rc<crate::ai::Controller>>,
    app_actions: OnceCell<ActionSet>,
    win_actions: RefCell<Option<ActionSet>>,
    mailbox: Rc<Mailbox>,
    /// Opens when the list has the first rows of its first folder, or there is nothing to show.
    first_rows: Rc<Latch>,
    list: RefCell<Option<MessageList>>,
    store_warned: Cell<bool>,
    reader: RefCell<Option<Reader>>,
    /// The contact pane beside the reader's conversation (phase C).
    contact: RefCell<Option<crate::contact::ContactPane>>,
    /// The conversation's message the contact pane last showed for (the reader's selected
    /// one), and the explicit message opened from Recent, until the user chooses another
    /// message. The reader tracks those choices, including pointer and keyboard moves.
    contact_selected: Cell<Option<MessageId>>,
    contact_moved: Cell<Option<MessageId>>,
    /// A conversation of the pane's Recent list being opened, at this message of the person
    /// shown: they stay shown, as if the user had moved to it.
    contact_opened: Cell<Option<MessageId>>,
    /// The message the reader shows; `None` while the window shows "No message selected".
    reader_shows: Cell<Option<MessageId>>,
    /// The engine's commands and events (ADR-003); not connected until an engine is attached.
    engine: EngineLink,
    /// The engine this application started; the setup self-tests attach a fake one instead.
    engine_service: RefCell<Option<Engine>>,
    engine_events: RefCell<Option<Subscription>>,
    /// An engine started without a window waits for the store and the settings.
    engine_pending: Cell<bool>,
    /// The mail actions' and the reader's store listeners are connected (once per process).
    triage_listening: Cell<bool>,
    reader_listening: Cell<bool>,
    /// `accounts.toml` has been read without mistakes at least once.
    accounts_read: Cell<bool>,
    /// The window's remembered layout (`layout.toml`), once read.
    layout: RefCell<Option<Layout>>,
    /// The engine keeps secrets in memory: no Secret Service answered (ML-048).
    keyring_missing: Cell<bool>,
    /// The no-keyring banner was dismissed for this session.
    keyring_dismissed: Cell<bool>,
    /// The desktop reports a network; true until it says otherwise.
    network: Cell<bool>,
    /// logind and the network monitor, watched once this application's engine starts.
    desktop: RefCell<Option<Rc<DesktopWatch>>>,
    /// The mail actions (ML-052 to ML-055), once there is a window.
    triage: RefCell<Option<Controller>>,
    /// The toast of the last action, which a new one replaces.
    toast: RefCell<Option<adw::Toast>>,
    /// Whether a message the reader shows is marked read ([`MARK_READ_ON_OPEN`]; the self-test
    /// turns it off and on).
    mark_read_on_open: Cell<bool>,
    /// Unread messages a conversation shown in the Unread view expanded, marked read once the
    /// selection leaves the message it was shown for.
    read_on_leave: RefCell<(Option<MessageId>, Vec<Target>)>,
    /// Unread messages the reader showed while it was off screen (behind a Focused list, or
    /// one pane at a time), marked read when the reader is on screen.
    read_when_shown: RefCell<Vec<Target>>,
    /// A conversation opened before the reader had it on screen: the focus goes into it then.
    focus_on_shown: Cell<Option<MessageId>>,
    /// The reading pane and the sidebar of a window whose `layout.toml` doesn't say: Focused
    /// with the sidebar hidden (the Superhuman pass); the self-test's harness asks for Split
    /// with the sidebar shown, which its scripts were written for.
    layout_defaults: Cell<(ViewMode, bool)>,
    /// The list's rows while `layout.toml` doesn't say: one per conversation (the Superhuman
    /// pass, phase B); the self-test's harness asks for one per message, which its scripts
    /// were written for.
    grouping_default: Cell<ViewGrouping>,
    /// Whether the Inbox shows the Split Inbox's sides while `layout.toml` doesn't say, over
    /// `config.toml`'s `[inbox] split` (phase C): the self-test's harness turns them off, which
    /// its scripts were written for.
    split_default: Cell<Option<bool>>,
    /// When the last mail command's handler ran, and the window's frame then (measurements).
    triage_started: Cell<Option<(Instant, i64)>>,
    /// The ops waiting for a server, for the offline banner.
    pending_ops: Cell<u32>,
    pending_queued: Cell<bool>,
    pending_recheck: Cell<bool>,
    /// The window's command palette (Superhuman pass), built with the window.
    palette: RefCell<Option<palette::Palette>>,
    /// The window's keys that yield to the focused widget (Ctrl+K), on the window itself.
    window_keys: RefCell<Option<ScopedShortcuts>>,
    /// A store change asked for the commands' sensitivity; it is worked out once, before the
    /// next frame.
    refresh_queued: Cell<bool>,
}

impl AppState {
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn window(&self) -> Option<Window> {
        self.window.borrow().clone()
    }

    /// Runs `hook` first in `startup`, before GTK and libadwaita initialise (≈ 45 ms, S6): the
    /// place to start the store's reader thread (ADR-003), so the first page is ready before
    /// the window needs it.
    pub fn connect_early_startup(&self, hook: impl FnOnce() + 'static) {
        self.early_startup.borrow_mut().push(Box::new(hook));
    }

    /// Runs `hook` once the first window has been presented (the self-test starts there).
    pub fn connect_first_window(&self, hook: impl FnOnce(Window) + 'static) {
        self.on_first_window.replace(Some(Box::new(hook)));
    }

    /// Keeps the first window hidden until `gate` resolves, such as the theme's first read
    /// (ADR-004: at most 500 ms) or the store's first page (S6: at most 300 ms); with several
    /// gates, until all of them have. The window is still built synchronously.
    pub fn hold_first_window(&self, gate: impl Future<Output = ()> + 'static) {
        let gate: PresentGate = match self.present_gate.take() {
            Some(earlier) => Box::pin(both(earlier, gate)),
            None => Box::pin(gate),
        };
        self.present_gate.replace(Some(gate));
    }

    /// The message store (ADR-005), opening from `startup` on.
    pub fn mailbox(&self) -> Rc<Mailbox> {
        self.mailbox.clone()
    }

    /// The window's message list, once there is a window.
    pub fn list(&self) -> Option<MessageList> {
        self.list.borrow().clone()
    }

    /// The reader (ML-027), once the first window exists.
    pub fn reader(&self) -> Option<Reader> {
        self.reader.borrow().clone()
    }

    /// The contact pane beside the reader's conversation (phase C), once the reader exists.
    pub fn contact(&self) -> Option<crate::contact::ContactPane> {
        self.contact.borrow().clone()
    }

    /// The link to the engine; `EngineLink::attach` connects it.
    pub fn engine(&self) -> EngineLink {
        self.engine.clone()
    }

    /// The Microsoft client ID in use (ADR-008): `config.toml`'s, else the build's. Microsoft
    /// sign-in is offered only with one.
    pub fn microsoft_client_id(&self) -> Option<String> {
        let config = self
            .preferences()
            .map(|prefs| prefs.config())
            .unwrap_or_default();
        oauth::microsoft_client_id(&config, self.config.microsoft_client_id.as_deref())
    }

    /// The Google client in use (ADR-023): `config.toml`'s, else the environment's or the
    /// build's. Sign in with Google is offered only with one.
    pub fn google_client(&self) -> Option<Registration> {
        let config = self
            .preferences()
            .map(|prefs| prefs.config())
            .unwrap_or_default();
        oauth::google_client(&config, self.config.google_client.as_ref())
    }

    /// For the setup assistant: whether a provider's client is in use, asked when it draws.
    fn sign_in_available(self: &Rc<Self>) -> setup::SignInAvailable {
        let weak = Rc::downgrade(self);
        Rc::new(move |provider| {
            weak.upgrade().is_some_and(|state| match provider {
                OAuthProvider::Microsoft => state.microsoft_client_id().is_some(),
                OAuthProvider::Google => state.google_client().is_some(),
            })
        })
    }

    /// What the window's banner shows (ML-048), most important first: the keyring, then the
    /// accounts `accounts.toml` lists by the state the engine reported, then the network.
    pub fn waiting(&self) -> Vec<Waiting> {
        let listed = self
            .preferences()
            .map(|prefs| prefs.accounts())
            .unwrap_or_default();
        let known = self.mailbox.accounts();
        let accounts: Vec<status::Account<'_>> = known
            .iter()
            .filter_map(|account| {
                let key = account.key.as_ref()?;
                Some(status::Account {
                    name: &account.name,
                    status: self.mailbox.status(account.id),
                    settings: listed.iter().find(|settings| &settings.key == key)?,
                })
            })
            .collect();
        let now = outbox::now();
        let uncertain: Vec<_> = outbox::uncertain()
            .into_iter()
            .map(|send| {
                let name = known
                    .iter()
                    .find(|known| known.id == send.account)
                    .map(|known| known.name.as_str())
                    .unwrap_or_default();
                let what = outbox::described(&send.subject, name, send.started, &now);
                (what, send.reason)
            })
            .collect();
        let mut queue = status::queue(
            &accounts,
            self.keyring_missing.get() && !self.keyring_dismissed.get(),
            self.network.get(),
            self.pending_ops.get() + outbox::waiting_sends(),
            &uncertain,
        );
        crate::rules::add_waiting(self, &mut queue);
        queue
    }

    /// The accounts that must sign in again, in the sidebar's order: with Microsoft in the
    /// browser (ML-032) or with their password typed again (ML-048).
    pub fn sign_in_needed(&self) -> Vec<Again> {
        self.waiting()
            .into_iter()
            .filter(|waiting| matches!(waiting.attention, Attention::SignIn { .. }))
            .filter_map(|waiting| self.again(waiting.account.as_ref()?))
            .collect()
    }

    /// The account `key` as the setup assistant and the certificate's details need it.
    fn again(&self, key: &AccountKey) -> Option<Again> {
        let settings = self
            .preferences()?
            .accounts()
            .into_iter()
            .find(|settings| &settings.key == key)?;
        let account = self
            .mailbox
            .accounts()
            .into_iter()
            .find(|account| account.key.as_ref() == Some(key))?;
        Some(Again {
            key: key.clone(),
            name: account.name,
            address: settings.address,
            incoming: settings.incoming,
            receive: settings.receive,
            smtp: settings.smtp,
            auth: settings.auth,
        })
    }

    /// One banner shows the most important state and counts the others.
    pub(crate) fn update_banner(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let queue: Vec<Attention> = self
            .waiting()
            .into_iter()
            .map(|waiting| waiting.attention)
            .collect();
        window.show_banner(&queue);
    }

    /// A banner button, for the state the banner shows.
    fn banner_action(self: &Rc<Self>, action: BannerAction) {
        let Some(window) = self.window() else {
            return;
        };
        let again = self
            .waiting()
            .into_iter()
            .next()
            .and_then(|first| self.again(first.account.as_ref()?));
        match (action, again) {
            (BannerAction::SignIn, Some(again)) => {
                setup::sign_in_again(&window, &self.engine, self.sign_in_available(), again);
            }
            (BannerAction::TypePassword, Some(again)) => {
                setup::type_password_again(&window, &self.engine, self.sign_in_available(), again);
            }
            (BannerAction::Details, Some(again)) => {
                let weak = Rc::downgrade(self);
                status::certificate_details(&window, &again, move || {
                    if let Some(state) = weak.upgrade() {
                        state.retry();
                    }
                });
            }
            (BannerAction::Retry, _) => self.retry(),
            (BannerAction::Decide, _) => outbox::decide(self),
            (BannerAction::OpenRules, _) => crate::rules::open_shown(self),
            (BannerAction::Dismiss, _) => {
                self.keyring_dismissed.set(true);
                self.update_banner();
            }
            (BannerAction::SignIn | BannerAction::TypePassword | BannerAction::Details, None) => {
                tracing::warn!("a banner button for an account accounts.toml doesn't list");
            }
        }
    }

    /// A folder (not All Inboxes) is shown: the engine syncs a server's folder now and keeps it
    /// current while shown (ML-042); a POP3 account's local folder has nothing to sync and
    /// nothing older to load (M8).
    fn folder_opened(&self, folder: FolderId) {
        if pop3::is_local_folder(self, folder) {
            if let Some(list) = self.list() {
                list.older_loaded(folder, 0, true);
            }
        } else {
            let _ = self.engine.send(Command::FolderOpened { folder });
        }
    }

    /// The list reached the end of a folder's first-sync window (ML-043): the engine loads
    /// older mail. A local folder (M8) has none, so the list stops asking at once.
    fn older_wanted(&self, folder: FolderId) {
        let local = pop3::is_local_folder(self, folder);
        if (local || self.engine.send(Command::LoadOlder { folder }).is_err())
            && let Some(list) = self.list()
        {
            list.older_loaded(folder, 0, local);
        }
    }

    /// Retry: reconnect now, and say so.
    fn retry(&self) {
        self.reconnect("retry");
        if let Some(window) = self.window() {
            window.add_toast(adw::Toast::new("Connecting again…"));
        }
    }

    /// Refresh (M8): every account checks for new mail now and keeps its connections; an IMAP
    /// account runs an account pass, a POP3 account a pass.
    fn check_mail(&self) {
        match self.engine.send(Command::CheckMail) {
            Ok(()) => tracing::info!("checking for new mail"),
            Err(_) => tracing::debug!("no engine to check mail"),
        }
    }

    /// Drops every connection and makes it again (ADR-003 amendment, S6), without waiting for
    /// a socket to fail.
    fn reconnect(&self, why: &'static str) {
        match self.engine.send(Command::Reconnect) {
            Ok(()) => tracing::info!(why, "reconnecting"),
            Err(_) => tracing::debug!(why, "no engine to reconnect"),
        }
    }

    /// The machine resumed, or the desktop's network came or went (ML-048): reconnect when
    /// it is back, and show whether it is.
    pub fn desktop_event(&self, event: DesktopEvent) {
        match event {
            DesktopEvent::Resumed => self.reconnect("resumed"),
            DesktopEvent::Network(available) => {
                let was = self.network.replace(available);
                if available && !was {
                    self.reconnect("network back");
                }
                if available != was {
                    tracing::info!(available, "the desktop's network changed");
                    self.update_banner();
                }
            }
        }
    }

    /// logind's resume and the network monitor, for the engine this application started.
    fn watch_desktop(self: &Rc<Self>) {
        if self.desktop.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let watch = desktop::watch(move |event| {
            if let Some(state) = weak.upgrade() {
                state.desktop_event(event);
            }
        });
        self.desktop.replace(Some(watch));
    }

    /// When the first window's first frame was painted.
    pub fn first_frame(&self) -> Option<Instant> {
        self.first_frame.get()
    }

    /// The `mailto:` links handed to the composer so far, oldest first (`composer::requests`).
    pub fn compose_requests(&self) -> Vec<Mailto> {
        composer::requests()
            .into_iter()
            .filter_map(|request| match request {
                ComposeRequest::Mailto(mailto) => Some(mailto),
                _ => None,
            })
            .collect()
    }

    /// How many launches and activations showed the window.
    pub fn activations(&self) -> u32 {
        self.activations.get()
    }

    /// Background mode (D6) is an application hold: with it, closing the window leaves the
    /// process running, and the next activation builds a new window (ADR-003 amendment).
    pub fn set_background(&self, app: &ProntoApplication, on: bool) {
        if on {
            if self.background.borrow().is_none() {
                self.background.replace(Some(app.hold()));
            }
        } else {
            self.background.take();
        }
    }

    pub fn is_background(&self) -> bool {
        self.background.borrow().is_some()
    }

    /// Whether Quit is under way: windows closing then are not closing into background mode.
    pub fn is_quitting(&self) -> bool {
        self.quitting.get()
    }

    /// Whether `--background` holds Pronto until a main window shows.
    pub fn is_hidden_start(&self) -> bool {
        self.hidden.borrow().is_some()
    }

    /// Shows the main window, built anew when it was closed (background mode): what an
    /// activation and a click on the background notice do.
    pub fn show_main_window(self: &Rc<Self>, app: &ProntoApplication) {
        let window = self.window_or_new(app);
        self.present(&window);
    }

    /// The theme follower (ADR-004), from `startup` on.
    pub fn theme(&self) -> Option<Rc<ThemeController>> {
        self.theme.get().cloned()
    }

    /// `config.toml` and `keys.toml` (ADR-009), from `startup` on.
    pub fn preferences(&self) -> Option<Rc<Preferences>> {
        self.prefs.get().cloned()
    }

    /// The application's commands (`app.*`), from `startup` on: M7's modules connect theirs.
    pub fn app_actions(&self) -> Option<&ActionSet> {
        self.app_actions.get()
    }

    /// Whether a command is built and usable now, in either action group, or, for the
    /// composer's commands, in every composer window.
    pub fn is_available(&self, id: ActionId) -> bool {
        self.app_actions
            .get()
            .is_some_and(|set| set.is_available(id))
            || self
                .win_actions
                .borrow()
                .as_ref()
                .is_some_and(|set| set.is_available(id))
            || (id.scope() == Scope::Composer && crate::composer::card::runs(id))
    }

    /// Quits the application: the one path for signals, the exit timer and the Quit action.
    /// Open composer windows save their drafts first (`composer::save_all`), and so do the
    /// inline replies (`compose_actions::save_quick_drafts`), side by side: Quit waits at most
    /// `composer::SAVE_ON_QUIT` for both.
    pub fn quit(&self, app: &ProntoApplication) {
        if self.quitting.replace(true) {
            return;
        }
        if let Some(ai) = self.ai.get() {
            ai.stop();
        }
        self.background.take();
        self.hidden.take();
        let app = app.clone();
        let finish = move || {
            for window in app.windows() {
                window.destroy();
            }
            app.quit();
        };
        let ai = self.ai();
        glib::spawn_future_local(async move {
            let cards = glib::spawn_future_local(crate::composer::save_all());
            compose_actions::save_quick_drafts().await;
            let _ = cards.await;
            if let Some(ai) = ai {
                ai.finish().await;
            }
            finish();
        });
    }

    fn startup(self: &Rc<Self>, app: &ProntoApplication) {
        gtk4::Window::set_default_icon_name(ICON_NAME);
        self.start_settings(app);
        let _ = self.ai.set(crate::ai::Controller::start(self));
        // Composer windows draft into the store; the ones open when Pronto last stopped
        // reopen once the first frame is painted.
        composer::use_store(app);
        let reopen = app.downgrade();
        self.run_after_first_frame(move || {
            if let Some(app) = reopen.upgrade() {
                composer::reopen_drafts(&app);
            }
        });
        let weak = Rc::downgrade(self);
        self.mailbox.connect_folders_changed(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            state.update_account_commands();
            if let (Some(window), Some(list)) = (state.window(), state.list()) {
                state.fill_sidebar(&window, &list);
            }
            state.refresh_commands();
            state.update_banner();
        });
        // The sidebar's account states and the banner follow whatever engine the link has: the
        // one started below, or a self-test's scripted one.
        let weak = Rc::downgrade(self);
        let subscription = self.engine.subscribe(move |event| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if let Some((account, status)) = engine::account_status(event) {
                state.mailbox.set_status(account, status);
            }
            if let Event::KeyringUnavailable = event {
                tracing::info!("secrets stay in memory: no keyring answered");
                state.keyring_missing.set(true);
                state.update_banner();
            }
            if let Event::OlderLoaded {
                folder,
                fetched,
                complete,
            } = event
                && let Some(list) = state.list()
            {
                list.older_loaded(*folder, *fetched, *complete);
            }
            if let Event::OpsRefused {
                account,
                change,
                messages,
            } = event
                && let Some(triage) = state.triage()
            {
                tracing::info!(account = account.0, messages, "the server refused changes");
                let notice = triage.refused(*account, *change, *messages);
                state.show_notice(&notice, false);
            }
        });
        self.engine_events.replace(Some(subscription));
        outbox::install(self);
        notify::install(self);
        crate::remind::install(self);
        search::install(self);
        updates::install(self);
        crate::rules::install(self);
        background::install(app, self);
        if let Some(options) = self.config.engine.clone() {
            let ready = self.mailbox.wait_ready();
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                ready.await;
                if let Some(state) = weak.upgrade() {
                    let started = Rc::downgrade(&state);
                    state.run_after_first_frame(move || {
                        if let Some(state) = started.upgrade() {
                            state.start_engine(&options);
                            state.watch_desktop();
                        }
                    });
                }
            });
        }
        let ready = self.mailbox.wait_ready();
        let first_rows = self.first_rows.wait();
        self.hold_first_window(async move {
            let rows = async move {
                ready.await;
                first_rows.await;
            };
            if mailbox::with_timeout(rows, FIRST_ROWS_WAIT).await.is_none() {
                tracing::debug!("the first window stopped waiting for the list's first rows");
            }
        });
        // A closed window leaves the application here; GTK emits `destroy` only once nothing
        // holds the window, and this state does.
        app.connect_window_removed(glib::clone!(
            #[weak(rename_to = state)]
            self,
            move |_, removed| {
                let ours =
                    state.window.borrow().as_ref().is_some_and(|window| {
                        window.window().upcast_ref::<gtk4::Window>() == removed
                    });
                if ours {
                    state.main_window_closed();
                }
            }
        ));
        for (signal, name) in QUIT_SIGNALS {
            glib_unix::unix_signal_add_local(
                signal,
                glib::clone!(
                    #[weak]
                    app,
                    #[weak(rename_to = state)]
                    self,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || {
                        tracing::info!("{name}: quitting");
                        state.quit(&app);
                        glib::ControlFlow::Continue
                    }
                ),
            );
        }
        if let Some(delay) = self.config.exit_after {
            glib::timeout_add_local_once(
                delay,
                glib::clone!(
                    #[weak]
                    app,
                    #[weak(rename_to = state)]
                    self,
                    move || {
                        tracing::info!("{} elapsed; quitting", config::EXIT_AFTER_ENV);
                        state.quit(&app);
                    }
                ),
            );
        }
    }

    fn command_line(
        self: &Rc<Self>,
        app: &ProntoApplication,
        command_line: &gio::ApplicationCommandLine,
    ) -> glib::ExitCode {
        let parsed = match cli::parse(command_line.arguments()) {
            Ok(parsed) => parsed,
            Err(error) => {
                let text = error.render().to_string();
                if error.use_stderr() {
                    command_line.printerr_literal(&text);
                } else {
                    command_line.print_literal(&text);
                }
                return glib::ExitCode::from(error.exit_code() as u8);
            }
        };
        if parsed.self_test.is_some() {
            command_line.printerr_literal(
                "pronto: --self-test starts its own instance; it cannot be sent to a running \
                 one\n",
            );
            return glib::ExitCode::from(2);
        }
        if parsed.background {
            self.start_hidden(app);
        } else if parsed.compose && parsed.uris.is_empty() {
            self.compose_new(app);
        } else {
            let uris: Vec<&str> = parsed.uris.iter().map(String::as_str).collect();
            self.compose_links(app, &uris);
        }
        glib::ExitCode::SUCCESS
    }

    /// `--background` (ML-084): no window. A Pronto with no window yet holds itself until a
    /// main window shows, so it runs with `[background]` off too, and starts its engine without
    /// waiting for a first frame; a running one shows nothing.
    fn start_hidden(self: &Rc<Self>, app: &ProntoApplication) {
        if app.windows().is_empty() && self.hidden.borrow().is_none() {
            self.hidden.replace(Some(app.hold()));
            tracing::info!("started without a window");
        }
        if self.window().is_none() {
            if let Some(theme) = self.theme() {
                reader::prewarm(&self.config.dirs.cache, &theme.theme());
            }
            self.start_engine_without_window();
        }
    }

    /// `--compose` (ML-084): a new message, the window's Compose (from the account the list
    /// shows). The composer docks in the main window (the user's UI tweak, 2026-10-08), so the
    /// window shows first when it isn't.
    fn compose_new(self: &Rc<Self>, app: &ProntoApplication) {
        let window = self.window_or_new(app);
        self.present(&window);
        let compose = ActionId::Compose.detailed_name();
        if WidgetExt::activate_action(window.window(), &compose, None).is_err() {
            tracing::warn!("the window has no Compose command");
        }
    }

    /// `mailto:` links from the command line or D-Bus `Open`: a composer for each in the main
    /// window, shown first when it isn't (a link from a browser, or background mode after the
    /// window closed).
    fn compose_links(self: &Rc<Self>, app: &ProntoApplication, uris: &[&str]) {
        let window = self.window_or_new(app);
        for uri in uris {
            compose(app, uri);
        }
        self.present(&window);
    }

    fn open(self: &Rc<Self>, app: &ProntoApplication, files: &[gio::File]) {
        let uris: Vec<String> = files
            .iter()
            .map(|file| file.uri().to_string())
            .filter(|uri| {
                let link = mailto::is_mailto(uri);
                if !link {
                    tracing::warn!("ignored an open request that is not a mailto: link");
                }
                link
            })
            .collect();
        let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
        self.compose_links(app, &uris);
    }

    /// The theme, the settings files and the application's commands. The theme read blocks
    /// for at most `theme::STARTUP_WAIT` (about 9 ms here); the settings are read off the GTK
    /// thread, and the first window waits up to `SETTINGS_WAIT` for them.
    fn start_settings(self: &Rc<Self>, app: &ProntoApplication) {
        self.read_layout();
        let prefs = Preferences::start(&self.config.dirs);
        let theme = ThemeController::start(prefs.config().appearance, theme::STARTUP_WAIT);
        let app_actions = actions::install_app_actions(app);
        let weak_app = app.downgrade();
        let weak = Rc::downgrade(self);
        app_actions.connect(ActionId::Quit, move || {
            if let (Some(app), Some(state)) = (weak_app.upgrade(), weak.upgrade()) {
                state.quit(&app);
            }
        });
        for (id, file) in [
            (ActionId::Preferences, SettingsFile::Config),
            (ActionId::CustomizeShortcuts, SettingsFile::Keys),
            (ActionId::EditSnippets, SettingsFile::Snippets),
        ] {
            let weak = Rc::downgrade(self);
            app_actions.connect(id, move || {
                if let Some(state) = weak.upgrade() {
                    state.open_settings(file);
                }
            });
        }
        let weak = Rc::downgrade(self);
        app_actions.connect(ActionId::KeyboardShortcuts, move || {
            if let Some(state) = weak.upgrade() {
                state.show_shortcuts();
            }
        });
        let weak = Rc::downgrade(self);
        app_actions.connect(ActionId::DefaultMailApp, move || {
            if let Some(state) = weak.upgrade()
                && let Some(window) = state.window()
            {
                default_handler::run(&window, &state.config.app_id);
            }
        });
        let weak_app = app.downgrade();
        let weak = Rc::downgrade(self);
        prefs.connect_changed(move |update| {
            if let (Some(app), Some(state)) = (weak_app.upgrade(), weak.upgrade()) {
                state.settings_changed(&app, update);
            }
        });
        let ready = prefs.clone();
        self.hold_first_window(async move { ready.ready(SETTINGS_WAIT).await });
        let _ = self.theme.set(theme);
        let _ = self.prefs.set(prefs);
        let _ = self.app_actions.set(app_actions);
    }

    /// `config.toml` or `keys.toml` changed, or one of them has a mistake.
    fn settings_changed(&self, app: &ProntoApplication, update: &Update) {
        let Some(prefs) = self.preferences() else {
            return;
        };
        if update.config_before.is_some() {
            if let Some(theme) = self.theme() {
                theme.set_options(prefs.config().appearance);
            }
            if let Some(reader) = self.reader() {
                reader.set_conversation_order(prefs.config().reader.conversation_order);
                reader.set_default_reply(prefs.config().reader.reply);
            }
            // A client ID added or removed makes Microsoft or Google sign-in available or not.
            setup::refresh();
            self.send_microsoft_client();
            self.send_google_client();
            self.send_storage();
            // `[inbox] split` changed: it decides again, over Split Inbox's last choice.
            if update
                .config_before
                .as_ref()
                .is_some_and(|before| before.inbox.split != prefs.config().inbox.split)
            {
                self.remember(|layout| layout.split = None);
            }
            self.apply_splits();
        }
        if update.keymap_changed {
            let keymap = prefs.keymap();
            actions::apply_accels(app, &keymap);
            if let Some(window) = self.window() {
                window.set_menu_model(Some(menu::primary_menu(&keymap).upcast_ref()));
                window.set_keymap(&keymap);
            }
            if let Some(keys) = self.window_keys.borrow().as_ref() {
                keys.update(&keymap);
            }
            if let Some(list) = self.list() {
                list.update_keys(&keymap);
                list.set_quick_tooltips(quick_tooltips(&keymap));
            }
            if let Some(reader) = self.reader() {
                reader.update_keys(&keymap);
            }
            self.refresh_commands();
        }
        if update.accounts_changed {
            self.accounts_read.set(true);
            self.update_account_commands();
            self.mailbox.set_colors(
                prefs
                    .accounts()
                    .into_iter()
                    .filter_map(|account| Some((account.key, account.color?)))
                    .collect(),
            );
            self.send_accounts();
            self.update_banner();
        }
        for problem in &update.problems {
            tracing::warn!("{problem}");
            if let Some(window) = self.window() {
                window.add_toast(adw::Toast::new(problem));
            }
        }
    }

    /// The window's commands: the palette, and the main menu built from the registry.
    fn install_window(self: &Rc<Self>, app: &ProntoApplication, window: &Window) {
        let Some(prefs) = self.preferences() else {
            return;
        };
        let keymap = prefs.keymap();
        let win_actions = actions::install_window_actions(window.window());
        self.install_palette(window, &keymap);
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::CommandPalette, move || {
            if let Some(state) = weak.upgrade() {
                state.toggle_palette();
            }
        });
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::AddAccount, move || {
            if let Some(state) = weak.upgrade()
                && let Some(window) = state.window()
            {
                let used: Vec<_> = state
                    .preferences()
                    .map(|prefs| prefs.accounts())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|account| account.color)
                    .collect();
                setup::open(&window, &state.engine, state.sign_in_available(), &used);
            }
        });
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::Refresh, move || {
            if let Some(state) = weak.upgrade() {
                state.check_mail();
            }
        });
        let weak = Rc::downgrade(self);
        window.connect_banner(move |action| {
            if let Some(state) = weak.upgrade() {
                state.banner_action(action);
            }
        });
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::RemoveAccount, move || {
            if let Some(state) = weak.upgrade()
                && let Some(window) = state.window()
            {
                setup::remove::confirm(&window, &state.engine, state.removable_accounts());
            }
        });
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::EditAccount, move || {
            if let Some(state) = weak.upgrade() {
                let account = state.list().and_then(|list| {
                    let selected = list
                        .selected_targets()
                        .ok()
                        .and_then(|targets| targets.first().map(|target| target.folder));
                    compose_actions::compose_account(
                        list.scope(),
                        selected,
                        &state.mailbox.folders(),
                    )
                });
                state.edit_account(account);
            }
        });
        let weak = Rc::downgrade(self);
        window.sidebar().connect_edit_account(move |key| {
            if let Some(state) = weak.upgrade() {
                state.edit_account(mailbox::account_for_key(key));
            }
        });
        win_actions.set_available(
            ActionId::RemoveAccount,
            !self.removable_accounts().is_empty(),
        );
        win_actions.set_available(ActionId::EditAccount, !self.editable_accounts().is_empty());
        self.install_layout_toggles(window, &win_actions);
        crate::pane_cycle::install(self, window, &win_actions);
        window.set_menu_model(Some(menu::primary_menu(&keymap).upcast_ref()));
        window.set_keymap(&keymap);
        actions::apply_accels(app, &keymap);
        self.install_list(window, &win_actions, &keymap);
        compose_actions::install(self, window, &win_actions);
        crate::snippets::install(self, window, &win_actions);
        crate::scheduled::install(self, &win_actions);
        crate::go_to::install(self, &win_actions);
        crate::remind::install_commands(self, &win_actions);
        crate::folders::install(self, window, &win_actions);
        crate::rules::install_window(self, window, &win_actions);
        self.win_actions.replace(Some(win_actions));
    }

    /// Toggle Sidebar and the Reading Pane (the Superhuman pass, 2026-10-28): the window does
    /// what they ask, and `layout.toml` remembers the sidebar of a wide window and the mode.
    fn install_layout_toggles(self: &Rc<Self>, window: &Window, actions: &ActionSet) {
        let weak = Rc::downgrade(self);
        actions.connect_toggle(
            ActionId::ToggleSidebar,
            window.sidebar_shown(),
            move |shown| {
                if let Some(window) = weak.upgrade().and_then(|state| state.window()) {
                    window.set_sidebar_shown(shown);
                }
            },
        );
        let weak = Rc::downgrade(self);
        actions.connect_toggle(
            ActionId::ToggleReadingPane,
            window.view_mode() == ViewMode::Split,
            move |split| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let mode = if split {
                    ViewMode::Split
                } else {
                    ViewMode::Focused
                };
                if let Some(window) = state.window() {
                    window.set_view_mode(mode);
                }
                if let Some(list) = state.list() {
                    list.set_focused(mode == ViewMode::Focused);
                }
                state.remember(|layout| layout.mode = Some(mode));
            },
        );
        // Contact Details (phase C): the contact pane in the column's margin, where it fits.
        let weak = Rc::downgrade(self);
        actions.connect_toggle(
            ActionId::ContactDetails,
            self.contact_details(),
            move |on| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if let Some(window) = state.window() {
                    window.set_contact_wanted(on);
                    // Turned on where the conversation's margin has no room for it: why
                    // nothing shows.
                    if on && window.contact_has_room() == Some(false) {
                        state.show_toast(
                            &adw::Toast::new("Contact details show in a wider window"),
                            true,
                        );
                    }
                }
                state.remember(|layout| layout.contact = Some(on));
            },
        );
        let weak = Rc::downgrade(self);
        actions.connect_toggle(
            ActionId::GroupByConversation,
            self.grouping() == ViewGrouping::Conversations,
            move |on| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if let Some(list) = state.list() {
                    list.set_grouping(grouping_of(on));
                }
                if let Some(reader) = state.reader() {
                    reader.set_message_rows(!on);
                }
                state.follow_contact();
                state.remember(|layout| layout.conversations = Some(on));
            },
        );
        // The sidebar also hides by itself: over the list, a click beside it or Escape closes
        // it, and a narrower window takes it from beside the list.
        let weak = Rc::downgrade(self);
        window.connect_sidebar_shown(move |shown, remember| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if let Some(actions) = state.win_actions.borrow().as_ref() {
                actions.set_toggle_state(ActionId::ToggleSidebar, shown);
            }
            if remember {
                state.remember(|layout| layout.sidebar = Some(shown));
            }
        });
    }

    /// The window's remembered (else default) reading pane and sidebar, on it and on its
    /// toggles.
    fn apply_window_layout(&self, window: &Window) {
        let (mode, sidebar) = {
            let (default_mode, default_sidebar) = self.layout_defaults.get();
            let layout = self.layout.borrow();
            let layout = layout.as_ref();
            (
                layout
                    .and_then(|layout| layout.mode)
                    .unwrap_or(default_mode),
                layout
                    .and_then(|layout| layout.sidebar)
                    .unwrap_or(default_sidebar),
            )
        };
        window.set_view_mode(mode);
        window.set_sidebar_wanted(sidebar);
        window.set_contact_wanted(self.contact_details());
        let grouping = self.grouping();
        if let Some(list) = self.list() {
            list.set_grouping(grouping);
            list.set_focused(mode == ViewMode::Focused);
        }
        self.apply_splits();
        if let Some(reader) = self.reader() {
            reader.set_message_rows(grouping == ViewGrouping::Messages);
        }
        self.follow_contact();
        if let Some(actions) = self.win_actions.borrow().as_ref() {
            actions.set_toggle_state(ActionId::ToggleReadingPane, mode == ViewMode::Split);
            actions.set_toggle_state(ActionId::ToggleSidebar, window.sidebar_shown());
            actions.set_toggle_state(
                ActionId::GroupByConversation,
                grouping == ViewGrouping::Conversations,
            );
            actions.set_toggle_state(ActionId::ContactDetails, self.contact_details());
        }
    }

    /// The reading pane and sidebar a window opens with while `layout.toml` names neither (the
    /// self-test's harness: Split with the sidebar shown).
    pub fn set_layout_defaults(&self, mode: ViewMode, sidebar: bool) {
        self.layout_defaults.set((mode, sidebar));
        if let Some(window) = self.window() {
            self.apply_window_layout(&window);
        }
    }

    /// The list's rows while `layout.toml` doesn't say (the self-test's harness: one per
    /// message); the view the store reads ahead for the first window follows it.
    pub fn set_grouping_default(&self, grouping: ViewGrouping) {
        self.grouping_default.set(grouping);
        self.mailbox.set_default_grouping(grouping);
        if let Some(window) = self.window() {
            self.apply_window_layout(&window);
        }
    }

    /// Whether the Inbox shows the Split Inbox's sides: Split Inbox's last choice
    /// (`layout.toml`), else the harness's default, else `config.toml`'s `[inbox] split`.
    pub fn splits_enabled(&self) -> bool {
        self.layout
            .borrow()
            .as_ref()
            .and_then(|layout| layout.split)
            .or(self.split_default.get())
            .unwrap_or_else(|| {
                self.preferences()
                    .is_none_or(|prefs| prefs.config().inbox.split)
            })
    }

    /// The splits while `layout.toml` doesn't say, over `config.toml` (`None`: as it says);
    /// the view the store reads ahead for the first window follows it.
    pub fn set_split_default(&self, splits: Option<bool>) {
        self.split_default.set(splits);
        self.mailbox.set_default_splits(splits);
        self.apply_splits();
    }

    /// Split Inbox (the palette, the View menu): on or off, remembered in `layout.toml`.
    pub fn choose_splits(&self, on: bool) {
        self.remember(|layout| layout.split = Some(on));
        self.apply_splits();
    }

    /// The list shows the sides, or not, as [`AppState::splits_enabled`] says.
    fn apply_splits(&self) {
        let on = self.splits_enabled();
        if let Some(list) = self.list() {
            list.set_splits(on);
        }
        if let Some(actions) = self.win_actions.borrow().as_ref() {
            actions.set_toggle_state(ActionId::SplitInbox, on);
        }
    }

    /// One row per conversation or per message: `layout.toml`'s choice, else the default.
    pub fn grouping(&self) -> ViewGrouping {
        self.layout
            .borrow()
            .as_ref()
            .and_then(|layout| layout.conversations)
            .map_or(self.grouping_default.get(), grouping_of)
    }

    /// Whether an open conversation shows the contact pane where it fits (`layout.toml`; shown
    /// until the user turns Contact Details off, phase C).
    pub fn contact_details(&self) -> bool {
        self.layout
            .borrow()
            .as_ref()
            .and_then(|layout| layout.contact)
            .unwrap_or(true)
    }

    /// Changes the remembered layout, once it has been read (writing it before would drop what
    /// it holds).
    fn remember(&self, change: impl FnOnce(&mut Layout)) {
        if self.layout.borrow().is_some() {
            self.change_layout(change);
        }
    }

    /// Whether composer cards show their formatting toolbar (`layout.toml`; hidden until the
    /// user shows it, the fork's phase B).
    pub fn formatting_toolbar(&self) -> bool {
        self.layout
            .borrow()
            .as_ref()
            .and_then(|layout| layout.formatting)
            .unwrap_or(false)
    }

    /// Formatting Toolbar from a card's menu: every open card follows, and `layout.toml`
    /// remembers it.
    pub fn set_formatting_toolbar(&self, on: bool) {
        self.remember(|layout| layout.formatting = Some(on));
        composer::show_formatting(on);
    }

    /// Opening a conversation puts the keyboard focus in it, now or once it is on screen; while
    /// the reader is alone, its Back button holds the focus meanwhile (the reader's keys work
    /// there too).
    fn focus_reader(&self, id: MessageId) {
        let Some(window) = self.window() else {
            return;
        };
        if self.reader().is_some_and(|reader| reader.focus_message(id)) {
            self.focus_on_shown.set(None);
            return;
        }
        self.focus_on_shown.set(Some(id));
        window.focus_reader_bar();
    }

    /// The message list (ML-026) in the window's list slot. It shows the folder picked in the
    /// sidebar, which the store fills and keeps current; its keys move and open the selection.
    fn install_list(self: &Rc<Self>, window: &Window, win_actions: &ActionSet, keymap: &Keymap) {
        let window_opened = Instant::now();
        let list = MessageList::new(self.mailbox.clone());
        // Before it shows anything: the first view it opens is the one the store read ahead.
        list.set_grouping(self.grouping());
        list.set_splits(self.splits_enabled());
        // Known to the state before anything below runs: the handlers look it up there.
        self.list.replace(Some(list.clone()));
        list.set_first_rows(self.first_rows.clone());
        list.install_keys(keymap);
        // Its keys work from the folder list too, and one whose command is off says why.
        if let Some(keys) = list.keys() {
            window.sidebar().attach_keys(&keys);
            crate::triage::explain_keys(self, &keys);
        }
        list.set_quick_tooltips(quick_tooltips(keymap));
        list.set_scheduled(crate::outbox::scheduled_copies());
        window.set_list(list.widget());
        window.set_list_search(list.search_bar());
        GtkWindowExt::set_focus(window.window(), Some(list.list_view()));
        list.keep_focus(window.window());
        list.swallow_double_clicks(window.window());
        if let Some(palette) = self.palette() {
            palette.set_fallback_focus(list.list_view());
        }
        self.install_triage(window, &list, win_actions);
        crate::message_menu::install(self, &list);
        // The list's filters as commands, and Select All (Superhuman pass): every row, the rows
        // not loaded yet first, as triage waits for them (ML-054); the list's selection handling
        // puts it in the select mode.
        self.install_filters(win_actions);
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::SelectAll, move || {
            if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
                list.model().complete_growth();
                list.selection().select_all();
            }
        });

        // The window's handlers find the window and the list through the state, never by a
        // clone of their own: a clone would keep a window closed into background mode alive.
        for (id, run) in [
            (
                ActionId::NextMessage,
                MessageList::select_next as fn(&MessageList),
            ),
            (ActionId::PreviousMessage, MessageList::select_previous),
            (ActionId::OpenMessage, MessageList::open_selected),
        ] {
            let weak = Rc::downgrade(self);
            win_actions.connect(id, move || {
                if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
                    run(&list);
                }
            });
        }
        // Search from a conversation open in the list's place shows the list first.
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::Search, move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if let Some(window) = state.window()
                && window.is_one_pane()
                && window.pane() == Pane::Reader
            {
                window.show_pane(Pane::List);
            }
            if let Some(list) = state.list() {
                list.focus_search();
            }
        });
        // Open shows the conversation (in the list's place while one pane shows) and puts the
        // keyboard focus in it, so the reader's keys work at once.
        let weak = Rc::downgrade(self);
        list.connect_message_opened(move |message| {
            if let Some(state) = weak.upgrade()
                && let Some(pane) = state.window()
                && !compose_actions::open_if_draft(&state, &pane, message)
            {
                pane.show_pane(Pane::Reader);
                if !pane.reader_has_focus() {
                    state.focus_reader(message);
                }
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_message_selected(move |id| {
            if let Some(state) = weak.upgrade() {
                state.leave_opened(Some(id));
                if state
                    .focus_on_shown
                    .get()
                    .is_some_and(|opened| opened != id)
                {
                    state.focus_on_shown.set(None);
                }
                state.set_has_selection(true);
                state.refresh_commands();
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_selection_cleared(move || {
            if let Some(state) = weak.upgrade() {
                state.leave_opened(None);
                state.focus_on_shown.set(None);
                state.set_has_selection(false);
                if let Some(shown) = state.window() {
                    if shown.reader_page() == "batch" {
                        shown.clear_reader();
                    }
                    // Nothing left to read in the reader alone (the last conversation archived,
                    // Inbox Zero): back to the list, with the focus in it.
                    if shown.is_one_pane() && shown.pane() == Pane::Reader {
                        shown.show_pane(Pane::List);
                        if let Some(list) = state.list() {
                            list.focus_selected();
                        }
                    }
                }
                state.refresh_commands();
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_batch_selected(move |count| {
            if let Some(state) = weak.upgrade() {
                state.leave_opened(None);
                if state.reader_shows.take().is_some()
                    && let Some(reader) = state.reader()
                {
                    reader.clear();
                    reader.set_position(None);
                }
                if let (Some(shown), Some(list)) = (state.window(), state.list()) {
                    shown.show_batch(count, list.shows_conversations());
                }
                state.set_has_selection(true);
                state.refresh_commands();
            }
        });
        // The filter shows as a pill in the list's bar, which removes it; an empty list shows
        // alone, never beside the reader's own empty page.
        let weak = Rc::downgrade(self);
        list.connect_filter_changed(move |filter| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if let Some(window) = state.window() {
                window.set_filter_pill(
                    (filter != ViewFilter::All).then(|| crate::list::filter_name(filter)),
                );
            }
            // Show All Messages applies while a filter is on: at once, not with the store's
            // view event a frame later.
            state.refresh_commands();
        });
        let weak = Rc::downgrade(self);
        window.connect_filter_cleared(move || {
            if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
                list.set_filter(ViewFilter::All);
                list.focus_selected();
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_empty_changed(move |empty| {
            if let Some(window) = weak.upgrade().and_then(|state| state.window()) {
                window.set_list_empty(empty);
            }
        });
        // The search in the header row, in the view name's place; Inbox Zero's tint over the
        // whole pane; the select mode's bar's Clear, as Escape in the list.
        let weak = Rc::downgrade(self);
        list.connect_searching(move |searching| {
            if let Some(window) = weak.upgrade().and_then(|state| state.window()) {
                window.set_list_searching(searching);
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_inbox_zero(move |zero| {
            if let Some(window) = weak.upgrade().and_then(|state| state.window()) {
                window.set_inbox_zero(zero);
            }
        });
        let weak = Rc::downgrade(self);
        window.connect_batch_cleared(move || {
            if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
                list.leave_select_mode();
                list.focus_selected();
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_search_changed(move |search| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let (Some(heading), Some(list)) = (state.window(), state.list()) else {
                return;
            };
            match search {
                Some(search) => heading.set_list_heading("Search", &search.subtitle()),
                None => {
                    // With conversation rows the heading counts them ("Inbox 21").
                    let model = list.model();
                    let counted = matches!(model.content(), Content::View(_))
                        && model.content().grouping() == ViewGrouping::Conversations;
                    heading.set_list_count(
                        list.shows_conversations(),
                        (counted && !model.is_pending()).then(|| model.total()),
                    );
                    let (title, subtitle) = heading.folder_heading().unwrap_or_default();
                    heading.set_list_heading(&title, &subtitle);
                }
            }
        });

        // A folder (not All Inboxes) is synced now and kept current while it is shown (ML-042);
        // at the end of its first-sync window the list asks for older mail (ML-043).
        let weak = Rc::downgrade(self);
        window.connect_folder_selected(move |_, key| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let scope = state.mailbox.scope_for_key(key);
            if let Some(ViewScope::Folder(folder)) = scope {
                state.folder_opened(folder);
            }
            if let Some(shown) = state.list() {
                shown.show(scope);
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_older_wanted(move |folder| {
            if let Some(state) = weak.upgrade() {
                state.older_wanted(folder);
            }
        });
        crate::split_inbox::install(self, window, &list, win_actions);
        if self.mailbox.is_ready() {
            self.fill_sidebar(window, &list);
        }
        // The reader's WebKit engine is created after the first frame: it isn't needed to show
        // the list, and creating it inside the window build cost startup time (M1, live). It
        // takes the keymap current by then, since `settings_changed` only updates a reader
        // that exists. A window built again from background mode waits for its own first frame.
        let weak = Rc::downgrade(self);
        let built = window.window().downgrade();
        self.run_after_window_frame(window, move || {
            if let Some(state) = weak.upgrade()
                && let Some(prefs) = state.preferences()
                && let (Some(target), Some(shown)) = (state.window(), state.list())
                && built.upgrade().as_ref() == Some(target.window())
            {
                state.install_reader(&target, &shown, &prefs.keymap(), window_opened);
            }
        });
    }

    /// The mail actions (ML-052 to ML-055): the registry's commands on the list's selection,
    /// the rows' quick actions and the sidebar's Mark Folder as Read, through a
    /// [`triage::Controller`] over the store's op queue ([`OpsBackend`]); toasts for its
    /// notices.
    fn install_triage(self: &Rc<Self>, window: &Window, list: &MessageList, actions: &ActionSet) {
        let directory: Rc<dyn Directory> = self.mailbox.clone();
        let controller = Controller::new(
            Rc::new(OpsBackend::new(self.mailbox.clone())),
            directory,
            self.engine.clone(),
        );
        let weak = Rc::downgrade(self);
        controller.connect_notice(move |notice| {
            if let Some(state) = weak.upgrade() {
                state.show_notice(notice, true);
            }
        });
        let weak = Rc::downgrade(self);
        controller.connect_changed(move || {
            if let Some(state) = weak.upgrade() {
                if state.triage().is_some_and(|triage| triage.can_undo()) {
                    outbox::forget_last();
                    crate::rules::forget_last();
                    crate::split_inbox::forget_last();
                }
                state.refresh_commands();
            }
        });
        let weak = Rc::downgrade(self);
        controller.connect_answered(move || {
            if let Some(state) = weak.upgrade() {
                state.queue_pending_count();
            }
        });
        for id in triage::COMMANDS {
            let weak = Rc::downgrade(self);
            actions.connect(id, move || {
                if let Some(state) = weak.upgrade() {
                    state.triage_command(id);
                }
            });
        }
        let weak = Rc::downgrade(self);
        actions.connect(ActionId::MarkFolderRead, move || {
            if let Some(state) = weak.upgrade()
                && let Some(list) = state.list()
                && list.query().is_empty()
            {
                state.mark_read(list.scope());
            }
        });
        let weak = Rc::downgrade(self);
        actions.connect(ActionId::Select, move || {
            if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
                list.toggle_selection();
            }
        });
        let weak = Rc::downgrade(self);
        actions.connect(ActionId::Undo, move || {
            if let Some(state) = weak.upgrade()
                && !outbox::undo_last(&state)
                && !crate::rules::undo_last(&state)
                && !crate::split_inbox::undo_last(&state)
                && let Some(triage) = state.triage()
            {
                triage.undo(None);
            }
        });
        // Escape leaves the select mode wherever the focus is, as it does in the list: on the
        // batch page, or nowhere once a button there went insensitive under it. Text fields and
        // dialogs take their Escape first.
        let escape = gtk4::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        escape.connect_key_pressed(move |_, key, _, modifiers| {
            match weak.upgrade().and_then(|state| state.list()) {
                Some(target)
                    if key == gtk4::gdk::Key::Escape
                        && modifiers.is_empty()
                        && target.is_select_mode() =>
                {
                    target.leave_select_mode();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        window.window().add_controller(escape);
        let weak = Rc::downgrade(self);
        list.connect_quick_action(move |id, row| {
            if let Some(state) = weak.upgrade() {
                state.mark_started();
                // Remind Me's clock opens its picker for the row (phase C).
                if id == ActionId::RemindMe {
                    crate::remind::ask(&state, vec![row]);
                } else {
                    state.triage_rows(id, vec![row]);
                }
            }
        });
        let (applies, activate) = (Rc::downgrade(self), Rc::downgrade(self));
        window.sidebar().connect_mark_read(
            move |key| {
                applies.upgrade().is_some_and(|state| {
                    let scope = state.mailbox.scope_for_key(key);
                    state.unread_in(scope) > 0
                })
            },
            move |key| {
                if let Some(state) = activate.upgrade() {
                    let scope = state.mailbox.scope_for_key(key);
                    state.mark_read(scope);
                }
            },
        );
        // Once per process: a window built again from background mode uses the same listener,
        // as the store has no way to drop one.
        if !self.triage_listening.replace(true) {
            let weak = Rc::downgrade(self);
            self.mailbox.connect_event(move |_| {
                if let Some(state) = weak.upgrade() {
                    state.queue_refresh_commands();
                    state.queue_pending_count();
                }
            });
        }
        // Mark Folder as Read follows the folder shown, also when no selection changes with it.
        let weak = Rc::downgrade(self);
        window.connect_folder_selected(move |_, _| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(state) = weak.upgrade() {
                    state.refresh_commands();
                }
            });
        });
        self.triage.replace(Some(controller));
        self.queue_pending_count();
    }

    /// Counts the ops waiting for a server, for the offline banner's "3 waiting", a moment
    /// after the last action, Undo or change event: one read per account with a server, on the
    /// store's reader, the replies awaited on the main loop.
    fn queue_pending_count(self: &Rc<Self>) {
        if self.pending_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(PENDING_COUNT_DELAY, move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            state.pending_queued.set(false);
            let Some(triage) = state.triage() else {
                return;
            };
            let backend = triage.backend();
            let counts: Vec<_> = state
                .mailbox
                .accounts()
                .into_iter()
                .filter(|account| account.key.is_some())
                .map(|account| backend.pending(account.id))
                .collect();
            let weak = Rc::downgrade(&state);
            glib::spawn_future_local(async move {
                let mut total = 0;
                for count in counts {
                    total += count.await.unwrap_or(0);
                }
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.pending_ops.replace(total) != total {
                    state.update_banner();
                }
                // An op the server carried out without changing what is shown (a flag the
                // overlay showed already) makes no change event: count again while any wait.
                if total > 0 && !state.pending_recheck.replace(true) {
                    let weak = Rc::downgrade(&state);
                    glib::timeout_add_local_once(PENDING_RECHECK, move || {
                        if let Some(state) = weak.upgrade() {
                            state.pending_recheck.set(false);
                            state.queue_pending_count();
                        }
                    });
                }
            });
        });
    }

    /// The ops waiting for a server, as last counted.
    pub fn pending_ops(&self) -> u32 {
        self.pending_ops.get()
    }

    /// The mail actions, once there is a window.
    pub fn triage(&self) -> Option<Controller> {
        self.triage.borrow().clone()
    }

    /// Whether a message shown in the reader is marked read; the self-test switches it. Off, it
    /// also drops the reads still waiting (for a reader laid out behind a Focused list to come
    /// on screen, or for the selection to leave an Unread view's message): the Focused cursor
    /// lays a conversation out as soon as the list fills, which can be before a script switches
    /// this off, and nothing shown is read once it is off.
    pub fn set_mark_read_on_open(&self, on: bool) {
        self.mark_read_on_open.set(on);
        if !on {
            self.read_when_shown.borrow_mut().clear();
            self.read_on_leave.replace((None, Vec::new()));
        }
    }

    /// When the last mail command's handler ran, and the window's frame counter then.
    pub fn triage_started(&self) -> Option<(Instant, i64)> {
        self.triage_started.get()
    }

    fn mark_started(&self) {
        let frame = self
            .window()
            .and_then(|window| window.window().frame_clock())
            .map_or(0, |clock| clock.frame_counter());
        self.triage_started.set(Some((Instant::now(), frame)));
    }

    /// A triage command on the selected rows; selected rows far down a range load first.
    fn triage_command(self: &Rc<Self>, id: ActionId) {
        self.mark_started();
        let Some(list) = self.list() else {
            return;
        };
        if id == ActionId::MoveTo {
            self.open_move_to(&list);
            return;
        }
        match list.selected_rows() {
            Ok(rows) => self.triage_rows(id, rows),
            Err(_) => {
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let rows = list.wait_rows().await;
                    let Some(state) = weak.upgrade() else {
                        return;
                    };
                    match rows {
                        Some(rows) => state.triage_rows(id, rows),
                        None => state.show_notice(
                            &Notice::plain(
                                "Some selected messages didn't load in time; nothing changed"
                                    .to_owned(),
                            ),
                            true,
                        ),
                    }
                });
            }
        }
    }

    /// The command `id` on `rows`: Star and Mark as Read or Unread toggle by what the rows
    /// show (a conversation is unread while a member is, starred while one is).
    fn triage_rows(self: &Rc<Self>, id: ActionId, rows: Vec<ListRow>) {
        let shown: Vec<Target> = rows.iter().map(Target::of_row).collect();
        let action = match id {
            ActionId::Archive => Triage::Archive,
            ActionId::Delete => Triage::Delete,
            ActionId::Junk => Triage::Junk,
            ActionId::Star => triage::star_action(&shown),
            ActionId::ToggleRead => triage::read_action(&shown),
            ActionId::MarkRead => Triage::Read(true),
            ActionId::MarkUnread => Triage::Read(false),
            _ => return,
        };
        self.queue_rows(action, rows);
    }

    /// Queues `action` on `rows` as one batch, one Undo: message rows act on their messages;
    /// conversation rows on their members in the view's folders, read from the store first
    /// (docs/superhuman/conversations.md, `triage::conversation_targets`). The list moves on
    /// to the row after them once they leave it.
    fn queue_rows(self: &Rc<Self>, action: Triage, rows: Vec<ListRow>) {
        let (Some(triage), Some(list)) = (self.triage(), self.list()) else {
            return;
        };
        let keys: Vec<RowKey> = rows.iter().map(ListRow::key).collect();
        list.advance_after(&keys);
        let conversations = rows
            .iter()
            .filter(|row| matches!(row, ListRow::Conversation(_)))
            .count() as u32;
        if conversations == 0 {
            // Message rows: queued now, in the frame the key was pressed in.
            let targets: Vec<Target> = rows.iter().map(Target::of_row).collect();
            self.forget_read_on_leave(action, &targets);
            triage.queue(action, targets, Origin::User);
            return;
        }
        let members = self.expand_rows(action, &rows);
        let weak = Rc::downgrade(self);
        let queue = triage.clone();
        // Counted as in flight while the members are read, as the queue is after.
        triage.spawn(async move {
            let Some(targets) = members.await else {
                return;
            };
            let Some(state) = weak.upgrade() else {
                return;
            };
            state.forget_read_on_leave(action, &targets);
            queue.queue_counted(
                action,
                targets,
                Origin::User,
                triage::Counted::Conversations(conversations),
            );
        });
    }

    /// The messages `action` on `rows` acts on: a message row's own; a conversation row's
    /// members in the list's folders ([`AppState::member_views`]), as
    /// `triage::conversation_targets` picks them. The store gets the reads now; `None` when
    /// they fail.
    pub(crate) fn expand_rows(
        &self,
        action: Triage,
        rows: &[ListRow],
    ) -> Pin<Box<dyn Future<Output = Option<Vec<Target>>>>> {
        let mut targets: Vec<Target> = Vec::new();
        let mut threads: Vec<ThreadId> = Vec::new();
        let mut accounts: Vec<AccountId> = Vec::new();
        for row in rows {
            match row {
                ListRow::Message(_) => targets.push(Target::of_row(row)),
                ListRow::Conversation(row) => {
                    threads.push(row.thread);
                    accounts.push(row.account);
                }
            }
        }
        if threads.is_empty() {
            return Box::pin(std::future::ready(Some(targets)));
        }
        let (Some(list), Some(store)) = (self.list(), self.mailbox.shared_store()) else {
            return Box::pin(std::future::ready(None));
        };
        let replies: Vec<_> = self
            .member_views(&list, action, &accounts)
            .into_iter()
            .map(|spec| store.conversation_members(threads.clone(), spec))
            .collect();
        Box::pin(async move {
            let mut members = Vec::new();
            for reply in replies {
                match reply.recv().await {
                    Ok(read) => members.extend(read.value),
                    Err(error) => {
                        tracing::warn!("a conversation's messages couldn't be read: {error}");
                        return None;
                    }
                }
            }
            targets.extend(triage::conversation_targets(action, &members));
            Some(targets)
        })
    }

    /// The views whose members an action on the list's conversation rows acts on: the view
    /// shown (every member, whatever its filter); a search's folder; for a search of every
    /// folder, each searched folder of the conversations' accounts, except Sent and Drafts
    /// for a move (archiving a conversation found anywhere leaves the user's own copies
    /// where they are).
    fn member_views(
        &self,
        list: &MessageList,
        action: Triage,
        accounts: &[AccountId],
    ) -> Vec<ViewSpec> {
        match list.model().content() {
            Content::View(spec) => vec![spec.with_filter(ViewFilter::All)],
            Content::Search {
                folder: Some(folder),
                ..
            } => vec![ViewSpec::folder(folder)],
            Content::Search { folder: None, .. } | Content::Nothing => {
                let moves = !matches!(action, Triage::Read(_) | Triage::Star(_));
                self.mailbox
                    .folders()
                    .into_iter()
                    .filter(|folder| accounts.contains(&folder.account) && folder.selectable)
                    .filter(|folder| !matches!(folder.role, FolderRole::Trash | FolderRole::Junk))
                    .filter(|folder| {
                        !moves || !matches!(folder.role, FolderRole::Sent | FolderRole::Drafts)
                    })
                    .map(|folder| ViewSpec::folder(folder.id))
                    .collect()
            }
        }
    }

    /// Messages the user marks read or unread aren't marked read again when the selection
    /// leaves the conversation they were opened in.
    fn forget_read_on_leave(&self, action: Triage, targets: &[Target]) {
        if matches!(action, Triage::Read(_)) {
            self.read_on_leave
                .borrow_mut()
                .1
                .retain(|pending| targets.iter().all(|target| target.id != pending.id));
        }
    }

    /// Send and Archive (the fork's phase B), once `draft`, a reply to `message`, is queued: its
    /// conversation is archived as `e` archives it (`rows`, the selection's taken when the send
    /// was asked for, a conversation row standing for its members in the view's folders;
    /// `None`: its Inbox messages, read from the store) and the list moves on; the archive's
    /// toast says the send goes and its Undo takes the send back too. With nothing to archive,
    /// the send gets its own toast.
    pub fn archive_after_send(
        self: &Rc<Self>,
        draft: DraftId,
        message: MessageId,
        rows: Option<Vec<ListRow>>,
    ) {
        self.mark_started();
        let held = outbox::take_held(draft);
        if let Some(rows) = rows {
            let keys: Vec<RowKey> = rows.iter().map(ListRow::key).collect();
            let conversations = rows
                .iter()
                .filter(|row| matches!(row, ListRow::Conversation(_)))
                .count() as u32;
            let counted = match conversations {
                0 => triage::Counted::Messages,
                count => triage::Counted::Conversations(count),
            };
            let members = self.expand_rows(Triage::Archive, &rows);
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let targets = members.await.unwrap_or_default();
                if let Some(state) = weak.upgrade() {
                    state.archive_sent(draft, held, targets, &keys, counted);
                }
            });
            return;
        }
        let Some(reply) = self
            .mailbox
            .store()
            .map(|store| store.conversation(message))
        else {
            self.archive_sent(draft, held, Vec::new(), &[], triage::Counted::Messages);
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let conversation = reply.recv().await.ok().and_then(|read| read.value);
            let Some(state) = weak.upgrade() else {
                return;
            };
            let mut keys = Vec::new();
            let targets: Vec<Target> = conversation
                .map(|conversation| {
                    keys.push(RowKey::Conversation(conversation.thread));
                    conversation
                        .messages
                        .iter()
                        .filter(|member| member.role == FolderRole::Inbox)
                        .map(|member| Target {
                            id: member.summary.id,
                            folder: member.summary.folder,
                            flags: member.summary.flags,
                        })
                        .collect()
                })
                .unwrap_or_default();
            // The list may show it as one conversation row or as its message rows.
            keys.extend(targets.iter().map(|target| RowKey::Message(target.id)));
            let counted = match state.list() {
                Some(list) if list.shows_conversations() => triage::Counted::Conversations(1),
                _ => triage::Counted::Messages,
            };
            state.archive_sent(draft, held, targets, &keys, counted);
        });
    }

    /// Archives `targets` once `draft` is sent (`held`: its send waits out the undo window),
    /// the list moving on from the rows of `keys`; the toast counts what `counted` says.
    fn archive_sent(
        self: &Rc<Self>,
        draft: DraftId,
        held: Option<AccountId>,
        targets: Vec<Target>,
        keys: &[RowKey],
        counted: triage::Counted,
    ) {
        let folders = self.mailbox.folders();
        let archivable = !targets.is_empty()
            && triage::availability(ActionId::Archive, &targets, &folders).enabled();
        if !archivable {
            if held.is_some() {
                outbox::announce(self, draft);
            }
            return;
        }
        let (Some(triage), Some(list)) = (self.triage(), self.list()) else {
            return;
        };
        list.advance_after(keys);
        if held.is_none() {
            triage.queue_counted(Triage::Archive, targets, Origin::User, counted);
            return;
        }
        // The toast shows while either can be undone: the send's window may be longer than
        // the archive's. Without one (`undo_send_seconds = 0`) the send went at once: Undo
        // takes back the archive alone.
        let window = outbox::send_window(self);
        let weak = Rc::downgrade(self);
        let undo: Also = if window.is_zero() {
            Rc::new(|| {})
        } else {
            Rc::new(move || {
                if let Some(state) = weak.upgrade() {
                    outbox::undo_send(&state, draft);
                }
            })
        };
        triage.queue_with_shown(
            Triage::Archive,
            targets,
            counted,
            outbox::SENDING_TEXT.to_owned(),
            undo,
            window,
        );
    }

    /// Move to…: the picker over the account's folders; insensitive across accounts.
    fn open_move_to(self: &Rc<Self>, list: &MessageList) {
        let (Some(window), Ok(rows)) = (self.window(), list.selected_rows()) else {
            return;
        };
        let targets: Vec<Target> = rows.iter().map(Target::of_row).collect();
        let folders = self.mailbox.folders();
        let Ok(choices) = triage::move_choices(&targets, &folders) else {
            return;
        };
        if choices.is_empty() {
            return;
        }
        let weak = Rc::downgrade(self);
        let count = rows.len() as u32;
        let conversations = list.shows_conversations();
        move_to::present(
            window.window(),
            &choices,
            &folders,
            count,
            conversations,
            move |folder| {
                if let Some(state) = weak.upgrade() {
                    state.mark_started();
                    state.queue_rows(Triage::MoveTo(folder), rows.clone());
                }
            },
        );
    }

    /// The folders Mark Folder as Read marks for what the list shows, and its name: a folder,
    /// or every account's Inbox for All Inboxes.
    fn read_folders(&self, scope: Option<ViewScope>) -> (Vec<FolderId>, String) {
        read_folders(scope, &self.mailbox.folders())
    }

    /// Unread messages the sidebar counts for what `scope` shows.
    fn unread_in(&self, scope: Option<ViewScope>) -> u32 {
        unread_in(scope, &self.mailbox.folders())
    }

    fn mark_read(&self, scope: Option<ViewScope>) {
        let Some(triage) = self.triage() else {
            return;
        };
        let (folders, name) = self.read_folders(scope);
        if !folders.is_empty() {
            self.mark_started();
            triage.mark_folders_read(folders, name);
        }
    }

    /// The window's palette, built hidden with it, its recent commands read from
    /// `palette.toml` and saved there when it runs one; and the window's keys that yield to the
    /// focused widget, its Ctrl+K (Superhuman pass).
    fn install_palette(self: &Rc<Self>, window: &Window, keymap: &Keymap) {
        let palette = palette::install(window.window());
        let path = self.config.dirs.state.join(PALETTE_FILE);
        let read = path.clone();
        let weak = palette.downgrade();
        glib::spawn_future_local(async move {
            let recent = gio::spawn_blocking(move || {
                sealwax_infrastructure::settings_files::read_optional(&read)
                    .ok()
                    .flatten()
                    .map(|text| Recent::parse(&text))
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            if let Some(palette) = weak.upgrade() {
                palette.set_recent(recent);
            }
        });
        palette.connect_recent_changed(move |recent| {
            let (path, text) = (path.clone(), recent.to_text());
            glib::spawn_future_local(async move {
                let written = gio::spawn_blocking(move || {
                    if let Some(dir) = path.parent() {
                        sealwax_infrastructure::xdg::ensure_private_dir(dir)?;
                    }
                    sealwax_infrastructure::accounts::replace(&path, &text)
                })
                .await;
                if let Ok(Err(error)) = written {
                    tracing::warn!(%error, "the palette's recent commands can't be saved");
                }
            });
        });
        self.palette.replace(Some(palette));
        let keys = actions::ScopedShortcuts::new(Scope::Window, keymap);
        keys.attach(window.window(), gtk4::PropagationPhase::Bubble);
        self.window_keys.replace(Some(keys));
    }

    /// The window's command palette.
    pub fn palette(&self) -> Option<palette::Palette> {
        self.palette.borrow().clone()
    }

    /// The palette's key: opens it over the window with what suits the focus and the selection,
    /// or closes it when it is open, so it never stacks. It stays shut while a dialog is open,
    /// as Superhuman's does in its settings: it would open under the dialog.
    fn toggle_palette(self: &Rc<Self>) {
        let requested = Instant::now();
        let (Some(window), Some(palette), Some(prefs)) =
            (self.window(), self.palette(), self.preferences())
        else {
            return;
        };
        if palette.is_open() {
            palette.close();
            return;
        }
        if window.window().visible_dialog().is_some() {
            return;
        }
        // A store change queued the commands' refresh for the next frame: the palette lists
        // what applies now.
        if self.refresh_queued.get() {
            self.refresh_commands();
        }
        // A picker over the window (Send Later's, the snippets') gives way, the focus back in
        // the draft it was for, before the palette sees where the focus is (the fork's phase B).
        crate::picker::close_in(window.window());
        // In a composer the palette lists and runs its commands too.
        let composer = composer::focused(window.window()).map(|composer| composer.action_group());
        let selection = self
            .list()
            .is_some_and(|list| list.reported() != Selected::Nothing);
        palette.open(palette::Context {
            keymap: prefs.keymap(),
            composer,
            titles: self.command_titles(),
            selection,
            places: self.palette_places(),
            requested: Some(requested),
        });
    }

    /// The folders the palette goes to once something is typed: every folder that holds mail,
    /// by its sidebar name and, with several accounts, its account's; Go to Inbox and the
    /// others stand for their own folders.
    fn palette_places(self: &Rc<Self>) -> Vec<palette::Place> {
        let folders = self.mailbox.folders();
        let accounts = self.mailbox.accounts();
        let commands = crate::go_to::command_places(accounts.len(), &folders);
        folders
            .iter()
            .filter(|folder| folder.selectable)
            .map(|folder| {
                let name = crate::go_to::folder_name(folder, &folders);
                let title = match accounts.iter().find(|account| account.id == folder.account) {
                    Some(account) if accounts.len() > 1 => {
                        format!("Go to {name} — {}", account.name)
                    }
                    _ => format!("Go to {name}"),
                };
                let (weak, id) = (Rc::downgrade(self), folder.id);
                palette::Place {
                    key: mailbox::folder_key(folder.id),
                    title,
                    command: commands
                        .iter()
                        .find(|(_, place)| *place == folder.id)
                        .map(|(command, _)| *command),
                    run: Rc::new(move || {
                        if let Some(state) = weak.upgrade() {
                            crate::go_to::show_folder(&state, id);
                        }
                    }),
                }
            })
            .collect()
    }

    /// The list's filters as commands (Superhuman pass; the chips' until then): Show Unread
    /// Only and Show Starred Only show everything again when they are on, Show All Messages
    /// clears any filter. The list keeps the keyboard focus, as Go to Starred leaves it there.
    fn install_filters(self: &Rc<Self>, actions: &ActionSet) {
        for (id, filter) in [
            (ActionId::ShowUnreadOnly, ViewFilter::Unread),
            (ActionId::ShowStarredOnly, ViewFilter::Starred),
            (ActionId::ShowAttachmentsOnly, ViewFilter::Attachments),
            (ActionId::ShowAllMessages, ViewFilter::All),
        ] {
            let weak = Rc::downgrade(self);
            actions.connect(id, move || {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if let Some(list) = state.list() {
                    let wanted = if list.filter() == filter {
                        ViewFilter::All
                    } else {
                        filter
                    };
                    list.set_filter(wanted);
                    list.focus_selected();
                }
                state.refresh_commands();
            });
        }
    }

    /// Commands' titles where they differ from the registry's, for the palette; Go to Account
    /// 1 to 9 by the accounts' names.
    fn command_titles(&self) -> Vec<(ActionId, String)> {
        let Some(list) = self.list() else {
            return Vec::new();
        };
        let targets = loaded_targets(&list);
        let folders = self.mailbox.folders();
        let accounts: Vec<(ActionId, String)> = self
            .mailbox
            .accounts()
            .iter()
            .enumerate()
            .filter_map(|(index, account)| {
                Some((
                    ActionId::go_to_account(index + 1)?,
                    format!("Go to {}", account.name),
                ))
            })
            .collect();
        triage::COMMANDS
            .into_iter()
            .map(|id| (id, triage::availability(id, &targets, &folders).title))
            .filter(|(id, title)| *title != id.title())
            .map(|(id, title)| (id, title.to_owned()))
            .chain(crate::rules::titles(self))
            .chain(crate::split_inbox::titles(self))
            .chain(accounts)
            .collect()
    }

    /// Works out the commands' sensitivity once before the next frame, however many store
    /// changes ask for it until then (a sync sends them in bursts). A selection change still
    /// refreshes at once, so a key pressed right after it sees the right commands.
    fn queue_refresh_commands(self: &Rc<Self>) {
        if self.refresh_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        let run = move || {
            if let Some(state) = weak.upgrade()
                && state.refresh_queued.get()
            {
                state.refresh_commands();
            }
        };
        match self.window().filter(|window| window.window().is_mapped()) {
            Some(window) => {
                window.window().add_tick_callback(move |_, _| {
                    run();
                    glib::ControlFlow::Break
                });
            }
            // No frames without a window on screen: on the next idle instead.
            None => {
                glib::idle_add_local_once(run);
            }
        }
    }

    /// Enables the mail commands where they apply to the selection, with their titles and
    /// reasons in the reader's toolbar and on the batch page (ML-052).
    pub(crate) fn refresh_commands(&self) {
        // Whatever was queued is done by this one; a tick still to come finds nothing to do.
        self.refresh_queued.set(false);
        let (Some(list), Some(window)) = (self.list(), self.window()) else {
            return;
        };
        let actions = self.win_actions.borrow();
        let Some(actions) = actions.as_ref() else {
            return;
        };
        let keymap = self
            .preferences()
            .map(|prefs| prefs.keymap())
            .unwrap_or_default();
        let targets = loaded_targets(&list);
        let folders = self.mailbox.folders();
        let reader = self.reader();
        if let Some(reader) = &reader {
            // Star is filled while it would unstar: every selected message is starred.
            reader.set_starred(triage::star_action(&targets) == Triage::Star(false));
        }
        for id in triage::COMMANDS {
            let availability = triage::availability(id, &targets, &folders);
            actions.set_applies(id, availability.enabled());
            if let Some(reader) = &reader {
                reader.set_command(id, availability.title, availability.reason);
            }
            window.batch_page().set_command(
                id,
                availability.title,
                &reader::tooltip(&keymap, id, availability.title, availability.reason),
            );
        }
        // Remind Me's clock on the toolbar and the batch bar names its key, and says why it is
        // off where nothing can be reminded (phase C).
        let remind = ActionId::RemindMe;
        let reason = crate::remind::unavailable(&list, &folders);
        if let Some(reader) = &reader {
            reader.set_command(remind, remind.title(), reason);
        }
        window.batch_page().set_command(
            remind,
            remind.title(),
            &reader::tooltip(&keymap, remind, remind.title(), reason),
        );
        let one = matches!(list.reported(), Selected::One(_));
        for id in [
            ActionId::OpenMessage,
            ActionId::Print,
            ActionId::ShowOriginal,
            ActionId::SaveMessage,
        ] {
            actions.set_applies(id, one);
        }
        // Select All isn't held to the rows: a filter's reload would leave it stale, and on an
        // empty list it selects nothing.
        actions.set_applies(ActionId::Select, list.model().total() > 0);
        actions.set_applies(
            ActionId::MarkFolderRead,
            list.query().is_empty() && unread_in(list.scope(), &folders) > 0,
        );
        actions.set_applies(ActionId::ShowAllMessages, list.filter() != ViewFilter::All);
        let undo = self.triage().is_some_and(|triage| triage.can_undo())
            || outbox::can_undo()
            || crate::rules::can_undo()
            || crate::split_inbox::can_undo();
        actions.set_applies(ActionId::Undo, undo);
        compose_actions::refresh(actions, &list, &targets, &folders);
        crate::folders::refresh(self, actions);
        crate::rules::refresh(self, actions);
        crate::split_inbox::refresh(self, actions);
        crate::remind::refresh_commands(actions, &list, &folders);
    }

    /// Shows `notice` as a toast; with `replaces`, it takes the last action's toast's place
    /// (ADR-015: only the last action is undoable from the toast).
    pub(crate) fn show_notice(self: &Rc<Self>, notice: &Notice, replaces: bool) {
        let toast = adw::Toast::builder()
            .title(notice.text.as_str())
            .use_markup(false)
            .timeout(notice.shown.as_millis().div_ceil(1000).max(1) as u32)
            .priority(adw::ToastPriority::High)
            .build();
        if let Some(batch) = notice.undo {
            toast.set_button_label(Some("Undo"));
            let weak = Rc::downgrade(self);
            toast.connect_button_clicked(move |_| {
                if let Some(state) = weak.upgrade()
                    && let Some(triage) = state.triage()
                {
                    triage.undo(Some(batch));
                }
            });
        }
        self.show_toast(&toast, replaces);
    }

    /// Shows `toast` in the main window; with `replaces`, it takes the last action's toast's
    /// place (M6: the outbox's Undo-send toast is the last action's too).
    pub fn show_toast(&self, toast: &adw::Toast, replaces: bool) {
        let Some(window) = self.window() else {
            return;
        };
        if replaces && let Some(previous) = self.toast.take() {
            previous.dismiss();
        }
        window.add_toast(toast.clone());
        if replaces {
            self.toast.replace(Some(toast.clone()));
        }
    }

    /// The reader showed message bodies of the selected message's conversation (ML-057): the
    /// unread ones are read now, or in the Unread view once the selection leaves the message, or
    /// when a reader off screen comes on screen ([`MARK_READ_ON_OPEN`]). A reader laid out
    /// behind a Focused list isn't on screen: moving the list's cursor reads nothing.
    fn messages_opened(&self, opened: &Opened) {
        if !self.mark_read_on_open.get() || self.reader_shows.get() != Some(opened.selected) {
            return;
        }
        let targets: Vec<Target> = opened
            .messages
            .iter()
            .filter(|message| !message.flags.is_seen())
            .map(|message| Target {
                id: message.id,
                folder: message.folder,
                flags: message.flags,
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let visible = self
            .window()
            .is_some_and(|window| window.reader_on_screen());
        if visible {
            self.read_opened(opened.selected, targets);
        } else {
            self.read_when_shown.borrow_mut().extend(targets);
        }
    }

    /// The reader came on screen (opened in the list's place): the messages it showed are read
    /// now.
    fn reader_on_screen(&self) {
        let waiting = self.read_when_shown.take();
        if let Some(selected) = self.reader_shows.get()
            && !waiting.is_empty()
        {
            self.read_opened(selected, waiting);
        }
    }

    fn read_opened(&self, selected: MessageId, targets: Vec<Target>) {
        let (Some(list), Some(triage)) = (self.list(), self.triage()) else {
            return;
        };
        if list.filter() == ViewFilter::Unread {
            let mut leave = self.read_on_leave.borrow_mut();
            leave.0 = Some(selected);
            leave.1.extend(targets);
        } else {
            triage.queue(Triage::Read(true), targets, Origin::Opened);
        }
    }

    /// The selection left the message shown in the Unread view: what it opened is read now.
    fn leave_opened(&self, now: Option<MessageId>) {
        self.read_when_shown.borrow_mut().clear();
        let opened_for = self.read_on_leave.borrow().0;
        if now.is_some() && now == opened_for {
            return;
        }
        let (_, targets) = self.read_on_leave.replace((None, Vec::new()));
        if !targets.is_empty()
            && let Some(triage) = self.triage()
        {
            triage.queue(Triage::Read(true), targets, Origin::Opened);
        }
    }

    /// Starts the engine on the open store and attaches it to the app's link. Once
    /// `accounts.toml` has been read, the engine gets its accounts and syncs them.
    pub fn start_engine(self: &Rc<Self>, options: &EngineOptions) {
        if self.engine_service.borrow().is_some() || self.quitting.get() {
            return;
        }
        let (Some(store), Some(blobs)) = (self.mailbox.shared_store(), self.mailbox.blobs()) else {
            tracing::warn!("the engine needs the store, which isn't open");
            return;
        };
        let engine = match engine::start(
            &self.config.app_id,
            &self.config.dirs,
            store,
            blobs,
            options,
            self.microsoft_client_id(),
            self.google_client(),
        ) {
            Ok(engine) => engine,
            Err(error) => {
                tracing::error!("the engine could not start: {error}");
                return;
            }
        };
        self.engine.attach(engine.commands(), engine.events());
        self.engine_service.replace(Some(engine));
        tracing::info!("engine started");
        self.send_storage();
        self.send_accounts();
    }

    /// Hands `config.toml`'s `[storage]` to the engine this application started, once the file
    /// has been read: the disk cap is enforced as soon as it arrives.
    fn send_storage(&self) {
        let Some(prefs) = self.preferences().filter(|prefs| prefs.is_loaded()) else {
            return;
        };
        if self.engine_service.borrow().is_none() {
            return;
        }
        let storage = prefs.config().storage;
        if self.engine.send(Command::Storage { storage }).is_err() {
            tracing::warn!("the engine stopped listening");
        }
    }

    /// Hands the Microsoft client ID in use to the engine this application started.
    fn send_microsoft_client(&self) {
        if self.engine_service.borrow().is_none() {
            return;
        }
        let client_id = self.microsoft_client_id();
        if self
            .engine
            .send(Command::MicrosoftClient { client_id })
            .is_err()
        {
            tracing::warn!("the engine stopped listening");
        }
    }

    /// Hands the Google client in use to the engine this application started.
    fn send_google_client(&self) {
        if self.engine_service.borrow().is_none() {
            return;
        }
        let client = self.google_client();
        if self.engine.send(Command::GoogleClient { client }).is_err() {
            tracing::warn!("the engine stopped listening");
        }
    }

    /// Reads `layout.toml` off the GTK thread; the first window waits up to `SETTINGS_WAIT`
    /// for it, so its panes start at the widths they were dragged to.
    fn read_layout(self: &Rc<Self>) {
        let path = self.config.dirs.layout_file();
        let weak = Rc::downgrade(self);
        let (sender, receiver) = async_channel::bounded(1);
        glib::spawn_future_local(async move {
            let layout = gio::spawn_blocking(move || sealwax_infrastructure::layout::read(&path))
                .await
                .unwrap_or_default();
            if let Some(state) = weak.upgrade() {
                state.apply_layout(layout);
            }
            let _ = sender.send(()).await;
        });
        self.hold_first_window(async move {
            if mailbox::with_timeout(receiver.recv(), SETTINGS_WAIT)
                .await
                .is_none()
            {
                tracing::debug!("the first window stopped waiting for its layout");
            }
        });
    }

    /// The remembered layout on the window and in the sidebar's model.
    fn apply_layout(&self, layout: Layout) {
        self.mailbox
            .set_collapsed(layout.collapsed.iter().cloned().collect());
        self.mailbox
            .set_more_open(layout.more_open.iter().cloned().collect());
        self.mailbox
            .set_folded(layout.folded.clone().into_iter().collect());
        self.mailbox.set_view(layout.view.clone());
        let window = self.window();
        if let Some(window) = &window {
            window.set_pane_widths(
                layout
                    .sidebar_width
                    .map_or(window::SIDEBAR_WIDTH, f64::from),
                layout.list_width.map_or(window::LIST_WIDTH, f64::from),
            );
        }
        self.layout.replace(Some(layout));
        if let Some(window) = window {
            self.apply_window_layout(&window);
        }
    }

    /// Changes the remembered layout with `change` and writes it off the GTK thread.
    fn change_layout(&self, change: impl FnOnce(&mut Layout)) {
        let mut layout = self.layout.borrow().clone().unwrap_or_default();
        change(&mut layout);
        self.mailbox
            .set_collapsed(layout.collapsed.iter().cloned().collect());
        self.mailbox
            .set_more_open(layout.more_open.iter().cloned().collect());
        self.mailbox
            .set_folded(layout.folded.clone().into_iter().collect());
        self.mailbox.set_view(layout.view.clone());
        self.layout.replace(Some(layout.clone()));
        let path = self.config.dirs.layout_file();
        // Writes run one at a time, and one older than what is on disk is skipped: two changes
        // in a row (both widths reset by double clicks) could land in either order.
        let generation = LAYOUT_WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        glib::spawn_future_local(async move {
            let written = gio::spawn_blocking(move || {
                let mut on_disk = LAYOUT_WRITTEN
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *on_disk > generation {
                    return Ok(());
                }
                *on_disk = generation;
                sealwax_infrastructure::layout::write(&path, &layout)
            })
            .await;
            if let Ok(Err(error)) = written {
                tracing::warn!(%error, "the window's layout can't be saved");
            }
        });
    }

    /// The window's dragged widths, folded accounts and the view it shows go into the
    /// remembered layout.
    fn remember_layout(self: &Rc<Self>, window: &Window) {
        let weak = Rc::downgrade(self);
        window.connect_folder_selected(move |_, key| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            // Before layout.toml is read, writing it would drop what it holds.
            let Some(remembered) = state
                .layout
                .borrow()
                .as_ref()
                .map(|layout| layout.view.clone())
            else {
                return;
            };
            let view = state.mailbox.view_of_key(key);
            if view != remembered {
                state.change_layout(|layout| layout.view = view);
            }
        });
        if let Some(layout) = self.layout.borrow().as_ref() {
            window.set_pane_widths(
                layout
                    .sidebar_width
                    .map_or(window::SIDEBAR_WIDTH, f64::from),
                layout.list_width.map_or(window::LIST_WIDTH, f64::from),
            );
        }
        let weak = Rc::downgrade(self);
        window.connect_pane_widths(move |sidebar, list| {
            if let Some(state) = weak.upgrade() {
                state.change_layout(|layout| {
                    let width = |width: f64, default: f64| {
                        (width != default).then_some(width.round() as u32)
                    };
                    layout.sidebar_width = width(sidebar, window::SIDEBAR_WIDTH);
                    layout.list_width = width(list, window::LIST_WIDTH);
                });
            }
        });
        let weak = Rc::downgrade(self);
        window
            .sidebar()
            .connect_collapsed(move |account, collapsed| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let key = mailbox::account_for_key(account).and_then(|id| {
                    state
                        .mailbox
                        .accounts()
                        .into_iter()
                        .find(|known| known.id == id)
                        .and_then(|known| known.key)
                });
                if let Some(key) = key {
                    state.change_layout(|layout| {
                        layout.set_collapsed(&key, collapsed);
                    });
                }
            });
        let weak = Rc::downgrade(self);
        window.sidebar().connect_more(move |account, open| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let key = mailbox::account_for_key(account).and_then(|id| {
                state
                    .mailbox
                    .accounts()
                    .into_iter()
                    .find(|known| known.id == id)
                    .and_then(|known| known.key)
            });
            if let Some(key) = key {
                state.change_layout(|layout| {
                    layout.set_more_open(&key, open);
                });
            }
        });
        let weak = Rc::downgrade(self);
        window.sidebar().connect_folded(move |folder, folded| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let Some(ViewScope::Folder(id)) = state.mailbox.scope_for_key(folder) else {
                return;
            };
            let Some(folder) = state.mailbox.folder(id) else {
                return;
            };
            let key = state
                .mailbox
                .accounts()
                .into_iter()
                .find(|known| known.id == folder.account)
                .and_then(|known| known.key);
            if let Some(key) = key {
                state.change_layout(|layout| {
                    layout.set_folded(&key, &folder.name, folded);
                });
            }
        });
    }

    /// A folder renamed in Pronto: its fold and its subfolders' follow it in the remembered
    /// layout, so the sidebar shows them as the user left them.
    pub(crate) fn folder_renamed(
        &self,
        account: AccountId,
        old: &str,
        new: &str,
        delimiter: Option<char>,
    ) {
        let key = self
            .mailbox
            .accounts()
            .into_iter()
            .find(|known| known.id == account)
            .and_then(|known| known.key);
        let Some(key) = key else {
            return;
        };
        self.change_layout(|layout| {
            layout.rename_folded(&key, old, new, delimiter);
        });
    }

    /// Remove Account and Edit Account are available while there is an account to offer, and
    /// Go to Account 1 to 9 up to the number of accounts there are.
    fn update_account_commands(&self) {
        if let Some(actions) = self.win_actions.borrow().as_ref() {
            actions.set_available(
                ActionId::RemoveAccount,
                !self.removable_accounts().is_empty(),
            );
            actions.set_available(ActionId::EditAccount, !self.editable_accounts().is_empty());
            crate::go_to::show_accounts(actions, self.mailbox.accounts().len());
        }
    }

    /// The accounts Edit Account offers: those both `accounts.toml` and the store list, with
    /// their label and colour as the file has them, in the sidebar's order.
    fn editable_accounts(&self) -> Vec<Editable> {
        let listed = self
            .preferences()
            .map(|prefs| prefs.accounts())
            .unwrap_or_default();
        self.mailbox
            .accounts()
            .iter()
            .filter_map(|account| {
                let key = account.key.as_ref()?;
                let settings = listed.iter().find(|settings| &settings.key == key)?;
                Some(Editable {
                    key: key.clone(),
                    address: settings.address.clone(),
                    label: settings.label.clone(),
                    color: settings.color,
                    signature: settings.signature.clone(),
                })
            })
            .collect()
    }

    /// Edit Account, at `account` when it is one the dialog offers, else at the first.
    fn edit_account(&self, account: Option<AccountId>) {
        let Some(window) = self.window() else {
            return;
        };
        let accounts = self.editable_accounts();
        let key = account.and_then(|id| {
            self.mailbox
                .accounts()
                .into_iter()
                .find(|known| known.id == id)
                .and_then(|known| known.key)
        });
        let chosen = accounts
            .iter()
            .position(|editable| Some(&editable.key) == key.as_ref())
            .unwrap_or_default();
        setup::edit::open(&window, &self.engine, accounts, chosen);
    }

    /// The accounts Remove Account offers: those `accounts.toml` lists, so not the local
    /// accounts of fixtures and self-tests.
    fn removable_accounts(&self) -> Vec<Removable> {
        let known = self.mailbox.accounts();
        let listed = self
            .preferences()
            .map(|prefs| prefs.accounts())
            .unwrap_or_default();
        known
            .iter()
            .filter_map(|account| {
                Some(Removable {
                    key: account.key.clone()?,
                    name: account.name.clone(),
                    address: account.address.clone(),
                    pop3: pop3::receives_by_pop3(account.id, &known, &listed),
                })
            })
            .collect()
    }

    /// Hands the accounts from `accounts.toml` to the engine this application started. Their
    /// states come from the engine: an account whose sync starts or restarts reports syncing,
    /// and one already running keeps its state (M3: `Accounts` no longer syncs every account
    /// again).
    fn send_accounts(&self) {
        if !self.accounts_read.get() || self.engine_service.borrow().is_none() {
            return;
        }
        let Some(prefs) = self.preferences() else {
            return;
        };
        let accounts = prefs.accounts();
        if self.engine.send(Command::Accounts { accounts }).is_err() {
            tracing::warn!("the engine stopped listening");
        }
    }

    /// Stops the engine before the store closes: its connections end and its writes finish.
    fn stop_engine(&self) {
        self.engine_events.take();
        let Some(engine) = self.engine_service.take() else {
            return;
        };
        self.engine.detach();
        let started = Instant::now();
        engine.stop();
        tracing::info!(
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "engine stopped"
        );
    }

    /// The engine without a first frame (ML-084). When Pronto starts with no main window
    /// (`--background`, or a `mailto:` link or `--compose` opening a composer alone), the
    /// engine starts once the store is open and the settings are read, with the watch on resume
    /// and the network, so mail syncs and notifications come without a window. A main window
    /// shown later finds it running and never starts a second one.
    pub fn start_engine_without_window(self: &Rc<Self>) {
        let Some(options) = self.config.engine.clone() else {
            return;
        };
        if self.engine_service.borrow().is_some() || self.engine_pending.replace(true) {
            return;
        }
        let ready = self.mailbox.wait_ready();
        let prefs = self.preferences();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            ready.await;
            if let Some(prefs) = prefs {
                prefs.ready(ENGINE_SETTINGS_WAIT).await;
            }
            let Some(state) = weak.upgrade() else {
                return;
            };
            state.engine_pending.set(false);
            if state.engine_service.borrow().is_some() {
                return;
            }
            state.start_engine(&options);
            state.watch_desktop();
            if state.engine_service.borrow().is_some() {
                let accounts = state
                    .preferences()
                    .filter(|_| state.accounts_read.get())
                    .map_or(0, |prefs| prefs.accounts().len());
                tracing::info!(
                    ms = state.launched.elapsed().as_secs_f64() * 1000.0,
                    accounts,
                    accounts_sent = state.accounts_read.get(),
                    "engine started without a window"
                );
            }
        });
    }

    /// The main window closed (into background mode, or before Quit): what only it used goes
    /// with it, so the window, its list and its reader are freed (their views back in the
    /// reader's pool) and a window built again costs no more memory. A message the reader left
    /// to be marked read on leaving is left now.
    fn main_window_closed(self: &Rc<Self>) {
        self.leave_opened(None);
        self.window.replace(None);
        self.list.replace(None);
        if let Some(reader) = self.reader.take() {
            reader.rest();
        }
        self.reader_shows.set(None);
        self.toast.replace(None);
        self.win_actions.replace(None);
        self.palette.replace(None);
        self.window_keys.replace(None);
        // The composer's spare editor and web process too, once the window's cards have gone
        // (they go as it is destroyed), unless a window shows again meanwhile.
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(state) = weak.upgrade()
                && state.window().is_none()
            {
                composer::rest(&state);
            }
        });
    }

    /// Runs `hook` once `window` has painted its first frame: the first window's with
    /// [`Self::run_after_first_frame`], a window built later (from background mode) after its
    /// own, so its first frame waits for nothing it doesn't show.
    fn run_after_window_frame(&self, window: &Window, hook: impl FnOnce() + 'static) {
        if self.first_frame.get().is_none() {
            self.run_after_first_frame(hook);
            return;
        }
        let hook: Rc<RefCell<Option<Hook>>> = Rc::new(RefCell::new(Some(Box::new(hook))));
        let mapped: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let handler = window.window().connect_map(glib::clone!(
            #[strong]
            mapped,
            move |window| {
                if let Some(handler) = mapped.take() {
                    window.disconnect(handler);
                }
                let Some(clock) = window.frame_clock() else {
                    if let Some(hook) = hook.take() {
                        hook();
                    }
                    return;
                };
                let painted: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
                let hook = hook.clone();
                let handler = clock.connect_after_paint(glib::clone!(
                    #[strong]
                    painted,
                    move |clock| {
                        if let Some(handler) = painted.take() {
                            clock.disconnect(handler);
                        }
                        if let Some(hook) = hook.take() {
                            glib::idle_add_local_once(hook);
                        }
                    }
                ));
                painted.replace(Some(handler));
            }
        ));
        mapped.replace(Some(handler));
    }

    /// Runs `hook` once the first window's first frame is painted, or at once if it already is.
    fn run_after_first_frame(&self, hook: impl FnOnce() + 'static) {
        if self.first_frame.get().is_some() {
            hook();
        } else {
            self.after_first_frame.borrow_mut().push(Box::new(hook));
        }
    }

    /// The reader (ML-027) shows the list's selection, with its place in the list ("n of N")
    /// and previous and next. Clearing goes reader first, then the window: the other order
    /// moves GTK's focus through a view already back in the pool.
    fn install_reader(
        self: &Rc<Self>,
        window: &Window,
        list: &MessageList,
        keymap: &Keymap,
        window_opened: Instant,
    ) {
        let Some(theme) = self.theme() else {
            return;
        };
        window.follow_reader_column(&theme);
        let source: Rc<dyn MessageSource> = self.mailbox.clone();
        let reader = Reader::new(source, theme, keymap, &self.config.dirs.cache);
        reader.set_window_opened(window_opened);
        if let Some(prefs) = self.preferences() {
            reader.set_conversation_order(prefs.config().reader.conversation_order);
            reader.set_default_reply(prefs.config().reader.reply);
        }
        reader.set_message_rows(self.grouping() == ViewGrouping::Messages);
        reader.connect_engine(&self.engine);
        reader.prewarm_in(window.dock().widget());
        // The composer's spare editor, out of the way of the first frames (the fork's phase B).
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_full(composer::PREWARM_AFTER, glib::Priority::LOW, move || {
            if let Some(state) = weak.upgrade() {
                composer::prewarm(&state);
            }
            glib::ControlFlow::Break
        });
        window.set_reader_toolbar(&reader.toolbar());
        window.set_reader_quiet(reader.quiet_check());
        // Toasts keep off the inline reply while it is open (phase C's visual QA: "Sending
        // undone" covered the restored reply's footer), placed again when it changes.
        let shown = reader.clone();
        window
            .dock()
            .keep_toasts_off(move |overlay| shown.quick_reply_area(overlay));
        let dock = window.dock().downgrade();
        reader.connect_quick_state(move |_| {
            if let Some(dock) = dock.upgrade() {
                dock.place_toasts_again();
            }
        });
        // And as the column scrolls it (phase C's review: a toast placed off the reply could
        // cover it after a scroll).
        let dock = window.dock().downgrade();
        reader.connect_reply_moved(move || {
            if let Some(dock) = dock.upgrade() {
                dock.place_toasts_again();
            }
        });
        // The batch page stands in for the reader: the Reader scope's keys work there too. They
        // work in the whole reader pane, its bar's Back button included, so a focus resting
        // there doesn't swallow them.
        reader
            .keys()
            .attach(&window.batch_page().widget, gtk4::PropagationPhase::Bubble);
        if let Some(pane) = window.pane_widget("reader") {
            reader.keys().attach(&pane, gtk4::PropagationPhase::Bubble);
        }
        // `h` where nothing can be reminded says why, in the reader too (phase C).
        crate::remind::explain_keys(self, reader.keys());
        crate::triage::explain_keys(self, reader.keys());

        let weak = Rc::downgrade(self);
        list.connect_message_selected(move |id| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let (Some(reader), Some(target)) = (state.reader(), state.window()) else {
                return;
            };
            if state.reader_shows.replace(Some(id)).is_none() {
                target.set_reader(&reader.widget());
            }
            reader.show_next(id);
            state.update_position();
            state.prefetch_neighbours();
        });
        let weak = Rc::downgrade(self);
        list.connect_selection_cleared(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.reader_shows.take().is_some() {
                if let Some(reader) = state.reader() {
                    reader.clear();
                    reader.set_position(None);
                }
                if let Some(target) = state.window() {
                    target.clear_reader();
                }
            }
        });
        // The same conversation still selected over rows read anew (a filter, Split Inbox turned
        // on or off): "1 of N" counts the new rows.
        let weak = Rc::downgrade(self);
        list.connect_reset(move || {
            if let Some(state) = weak.upgrade() {
                state.update_position();
            }
        });
        if let Some(id) = list.selected() {
            self.reader_shows.set(Some(id));
            window.set_reader(&reader.widget());
            reader.show(id);
        }
        let weak = Rc::downgrade(self);
        reader.connect_previous(move || {
            if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
                list.select_previous();
            }
        });
        let weak = Rc::downgrade(self);
        reader.connect_next(move || {
            if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
                list.select_next();
            }
        });
        let weak = Rc::downgrade(self);
        reader.connect_opened(move |opened| {
            if let Some(state) = weak.upgrade() {
                state.messages_opened(opened);
            }
        });
        // On screen (opened in the list's place, or beside it), what the reader showed is read,
        // and says its dates as of now (laid out behind a Focused list, it may have waited
        // there a while); alone in front, it takes the keyboard focus unless it has it. Off
        // screen (the list in its place), the conversation it keeps starts afresh: opened
        // again, its reply answers the latest message, not one the user moved to before.
        let weak = Rc::downgrade(self);
        window.connect_reader_on_screen(move |on| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if !on {
                if let Some(reader) = state.reader() {
                    reader.left_screen();
                }
                state.contact_moved.set(None);
                state.follow_contact();
                return;
            }
            if let Some(reader) = state.reader() {
                reader.redate_shown();
            }
            state.reader_on_screen();
            crate::remind::seen(&state);
            if let (Some(window), Some(list)) = (state.window(), state.list())
                && window.is_one_pane()
                && !window.reader_has_focus()
                && let Some(id) = list.selected()
            {
                state.focus_reader(id);
            }
        });
        let weak = Rc::downgrade(self);
        reader.connect_shown(move |shown| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            crate::remind::seen(&state);
            if state.focus_on_shown.get() != Some(shown.id) {
                return;
            }
            state.focus_on_shown.set(None);
            // Only while the focus still waits where opening left it: a search field or a
            // folder picked meanwhile keeps it.
            if let (Some(window), Some(reader), Some(list)) =
                (state.window(), state.reader(), state.list())
                && window.reader_on_screen()
                && list.selected() == Some(shown.id)
                && focus_waits(&window, &list)
            {
                reader.focus_message(shown.id);
            }
        });
        if !self.reader_listening.replace(true) {
            let weak = Rc::downgrade(self);
            self.mailbox.connect_event(move |event| {
                if let Some(state) = weak.upgrade() {
                    crate::part_cache::forget(&event.removed);
                    state.update_position();
                    if let Some(reader) = state.reader() {
                        reader.store_changed(event);
                    }
                    if let Some(pane) = state.contact() {
                        pane.store_changed(event.version.0);
                    }
                }
            });
        }
        if let Some(actions) = self.win_actions.borrow().as_ref() {
            crate::reader::commands::connect(actions, &reader);
        }
        compose_actions::connect_reader(&reader, window);
        self.install_contact(window, &reader);
        self.reader.replace(Some(reader));
        self.refresh_commands();
    }

    /// The contact pane (phase C, docs/superhuman/contact.md) in the conversation's right
    /// margin. It follows the conversation the reader shows (laid out behind a Focused list
    /// too, so Enter shows it complete), the message `n` or `p` moved to and the message whose
    /// card the user put the keyboard focus in; a Recent conversation opens as a notification's
    /// does, in its folder; the search link searches every folder from the list.
    fn install_contact(self: &Rc<Self>, window: &Window, reader: &Reader) {
        let pane = crate::contact::ContactPane::new(self.mailbox.clone());
        window.set_contact_pane(&pane.widget());
        window.set_contact_wanted(self.contact_details());
        // Hidden with the focus in it: the focus goes to the conversation's message, and the
        // pane keeps whom it showed.
        let weak = Rc::downgrade(self);
        window.connect_contact_left(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let focused = match (state.reader(), state.reader_shows.get()) {
                (Some(reader), Some(id)) => reader.focus_message(id),
                _ => false,
            };
            if !focused && let Some(window) = state.window() {
                window.focus_reader_bar();
            }
        });
        let weak = Rc::downgrade(self);
        pane.connect_open(move |found| {
            if let Some(state) = weak.upgrade() {
                state.open_contact_conversation(found.message);
            }
        });
        let weak = Rc::downgrade(self);
        pane.connect_search(move |person| {
            if let Some(state) = weak.upgrade() {
                state.search_all_mail_from(&person.email);
            }
        });
        let weak = Rc::downgrade(self);
        pane.connect_copied(move |person| {
            if let Some(state) = weak.upgrade() {
                state.show_toast(&adw::Toast::new(&format!("Copied {}", person.email)), true);
            }
        });
        let weak = Rc::downgrade(self);
        reader.connect_shown(move |shown| {
            if let Some(state) = weak.upgrade() {
                if state.contact_selected.replace(Some(shown.id)) != Some(shown.id) {
                    let opened = state.contact_opened.take();
                    state
                        .contact_moved
                        .set(opened.filter(|opened| *opened == shown.id));
                }
                state.follow_contact();
            }
        });
        // A reply that came, or the conversation gone (0).
        let weak = Rc::downgrade(self);
        reader.connect_size(move |_| {
            if let Some(state) = weak.upgrade() {
                state.follow_contact();
            }
        });
        let weak = Rc::downgrade(self);
        reader.connect_moved(move |_| {
            if let Some(state) = weak.upgrade() {
                state.contact_moved.set(None);
                state.follow_contact();
            }
        });
        self.contact.replace(Some(pane));
    }

    /// The contact pane shows whom the reader's conversation is with, now.
    fn follow_contact(&self) {
        let (Some(reader), Some(pane)) = (self.reader(), self.contact()) else {
            return;
        };
        let Some(conversation) = reader.conversation() else {
            pane.show(None, None);
            return;
        };
        let mine: Vec<String> = self
            .mailbox
            .accounts()
            .into_iter()
            .map(|account| account.address.to_lowercase())
            .collect();
        let target = crate::contact::target(
            &conversation,
            reader.chosen_message().or(self.contact_moved.get()),
            &mine,
        );
        pane.show(target, conversation.thread);
    }

    /// A conversation of the contact pane's Recent list: shown in the list (its folder, unless
    /// All Inboxes shows it) and open at `message`, as a notification's click opens one; the
    /// pane goes on showing the person, as if the user had moved to their message.
    fn open_contact_conversation(self: &Rc<Self>, message: MessageId) {
        let Some(reply) = self.mailbox.store().map(|store| store.message(message)) else {
            return;
        };
        self.contact_opened.set(Some(message));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let summary = reply
                .recv()
                .await
                .ok()
                .and_then(|found| found.value)
                .map(|detail| detail.summary);
            let Some(state) = weak.upgrade() else {
                return;
            };
            match summary {
                Some(summary) => {
                    notify::select(&state, &summary).await;
                }
                None => state.show_toast(
                    &adw::Toast::new("That conversation is no longer here."),
                    true,
                ),
            }
        });
    }

    /// The contact pane's "Search all mail from …": the list shows the search's results from
    /// every folder, the first selected.
    fn search_all_mail_from(&self, email: &str) {
        if let Some(window) = self.window()
            && window.is_one_pane()
            && window.pane() == Pane::Reader
        {
            window.show_pane(Pane::List);
        }
        if let Some(list) = self.list() {
            list.search_for(&sealwax_domain::contact::search_query(email));
        }
    }

    /// The reader lays out ahead the conversations next to the selection: where `j`, `k`,
    /// archive and delete go (the list's next row first), so moving there is one swap.
    fn prefetch_neighbours(&self) {
        let (Some(reader), Some(list)) = (self.reader(), self.list()) else {
            return;
        };
        let Some(position) = list.selected_position() else {
            return;
        };
        // The messages the list reports for those rows (a conversation's anchor), which are the
        // ones `show_next` gets when the selection moves there.
        let ids: Vec<MessageId> = [position.checked_add(1), position.checked_sub(1)]
            .into_iter()
            .flatten()
            .filter_map(|position| list.message_at(position))
            .collect();
        reader.prefetch(&ids);
    }

    /// "n of N" for the message the reader shows; new mail above it moves it.
    fn update_position(&self) {
        let (Some(reader), Some(list), Some(id)) =
            (self.reader(), self.list(), self.reader_shows.get())
        else {
            return;
        };
        let position = list.position(id).map(|(n, total)| Position {
            index: n.saturating_sub(1),
            total,
        });
        reader.set_position(position);
    }

    /// The store's accounts and folders in the window's sidebar; with no account, the first
    /// window waits no longer for rows.
    fn fill_sidebar(&self, window: &Window, list: &MessageList) {
        if let Some(error) = self.mailbox.error()
            && !self.store_warned.replace(true)
        {
            window.add_toast(adw::Toast::new(&format!(
                "The mail store could not be opened: {error}"
            )));
        }
        let model = self.mailbox.sidebar_model();
        if model.accounts.is_empty() {
            self.first_rows.open();
        }
        window.set_sidebar(model);
        list.refresh_heading();
    }

    /// Whether a message is selected, for the commands that act on one.
    fn set_has_selection(&self, has_selection: bool) {
        if let Some(actions) = self.win_actions.borrow().as_ref() {
            actions.set_has_selection(has_selection);
        }
    }

    /// Optional AI jobs and settings, shared by the window and reader.
    pub fn ai(&self) -> Option<Rc<crate::ai::Controller>> {
        self.ai.get().cloned()
    }

    /// Preferences opens Settings → AI; other configuration files open in the text editor.
    fn open_settings(&self, file: SettingsFile) {
        if file == SettingsFile::Config {
            if let (Some(ai), Some(window)) = (self.ai(), self.window()) {
                ai.present(window.window());
            }
            return;
        }
        let (Some(prefs), Some(window)) = (self.preferences(), self.window()) else {
            return;
        };
        let path = prefs.path(file);
        glib::spawn_future_local(async move {
            let message = preferences::open_in_editor(window.window(), path, file).await;
            window.add_toast(adw::Toast::new(&message));
        });
    }

    fn show_shortcuts(self: &Rc<Self>) {
        let (Some(prefs), Some(window)) = (self.preferences(), self.window()) else {
            return;
        };
        let state = self.clone();
        shortcuts::present(window.window(), &prefs.keymap(), &move |id| {
            state.is_available(id)
        });
    }

    /// The window, built on the first request.
    fn window_or_new(self: &Rc<Self>, app: &ProntoApplication) -> Window {
        if let Some(window) = self.window() {
            return window;
        }
        let window = Window::new(app.upcast_ref(), self.config.tiling);
        // Known to the state before its handlers are installed: they look it up there.
        self.window.replace(Some(window.clone()));
        self.install_window(app, &window);
        composer::keep_drafts_on_close(window.window());
        self.remember_layout(&window);
        self.apply_window_layout(&window);
        self.update_banner();
        window
    }

    /// Shows the window. The first time, after the gate (if any), with the first frame logged.
    fn present(self: &Rc<Self>, window: &Window) {
        self.activations.set(self.activations.get() + 1);
        if window.window().is_visible() {
            window.present();
            return;
        }
        match self.present_gate.take() {
            Some(gate) => {
                let state = self.clone();
                let window = window.clone();
                glib::spawn_future_local(async move {
                    gate.await;
                    state.show(&window);
                });
            }
            None => self.show(window),
        }
    }

    fn show(&self, window: &Window) {
        self.hidden.take();
        if let Some(started) = self.started.take() {
            let exec_ns = std::env::var(config::EXEC_NS_ENV)
                .ok()
                .and_then(|value| value.parse::<i64>().ok());
            if std::env::var_os(config::START_FULLSCREEN_ENV).is_some_and(|value| value == "1") {
                window.window().fullscreen();
            }
            log_first_frame(
                window.window(),
                started,
                exec_ns,
                self.first_frame.clone(),
                self.after_first_frame.clone(),
            );
        }
        window.present();
        if let Some(hook) = self.on_first_window.take() {
            hook(window.clone());
        }
    }
}

/// A `mailto:` link to write a message from (PRD C8): its safe fields go to a composer window
/// (`domain::mailto` drops `attach` and the rest); only the request's kind is logged.
fn compose(app: &ProntoApplication, uri: &str) {
    match mailto::parse(uri) {
        Ok(mailto) => composer::open(app, ComposeRequest::Mailto(mailto)),
        Err(error) => tracing::warn!("{error}"),
    }
}

/// Group by Conversation's state as the list's grouping.
fn grouping_of(conversations: bool) -> ViewGrouping {
    if conversations {
        ViewGrouping::Conversations
    } else {
        ViewGrouping::Messages
    }
}

/// The rows' quick actions' tooltips, with their keys.
fn quick_tooltips(keymap: &Keymap) -> Vec<(Quick, String)> {
    Quick::ALL
        .into_iter()
        .map(|quick| {
            let id = quick.action();
            (quick, reader::tooltip(keymap, id, id.title(), None))
        })
        .collect()
}

/// The folders Mark Folder as Read marks for what the list shows, and its name: a folder, or
/// every account's Inbox for All Inboxes.
fn read_folders(
    scope: Option<ViewScope>,
    folders: &[sealwax_domain::model::Folder],
) -> (Vec<FolderId>, String) {
    match scope {
        Some(ViewScope::Folder(id)) => (
            vec![id],
            folders
                .iter()
                .find(|folder| folder.id == id)
                .map(triage::folder_title)
                .unwrap_or_default(),
        ),
        Some(ViewScope::UnifiedInbox) => (
            folders
                .iter()
                .filter(|folder| folder.role == FolderRole::Inbox)
                .map(|folder| folder.id)
                .collect(),
            "All Inboxes".to_owned(),
        ),
        None => (Vec::new(), String::new()),
    }
}

/// Unread messages the sidebar counts for what `scope` shows.
fn unread_in(scope: Option<ViewScope>, folders: &[sealwax_domain::model::Folder]) -> u32 {
    let (ids, _) = read_folders(scope, folders);
    folders
        .iter()
        .filter(|folder| ids.contains(&folder.id))
        .map(|folder| folder.unread)
        .sum()
}

/// The keyboard focus still waits where opening a conversation left it: nowhere, on the
/// reader's Back button, or on the list's rows.
fn focus_waits(window: &Window, list: &MessageList) -> bool {
    let view: &gtk4::Widget = list.list_view().upcast_ref();
    match GtkWindowExt::focus(window.window()) {
        None => true,
        Some(focus) => window.back_has_focus() || &focus == view || focus.is_ancestor(view),
    }
}

/// The selected messages whose rows are loaded.
fn loaded_targets(list: &MessageList) -> Vec<Target> {
    list.selected_positions()
        .into_iter()
        .filter_map(|position| list.target_at(position))
        .collect()
}

/// What a triage command's Undo also runs (Send and Archive: the send goes back too).
type Also = Rc<dyn Fn()>;

/// Resolves once both futures have, polling them together.
async fn both(first: impl Future<Output = ()>, second: impl Future<Output = ()>) {
    let mut first = Box::pin(first);
    let mut second = Box::pin(second);
    let (mut first_done, mut second_done) = (false, false);
    std::future::poll_fn(|context| {
        first_done = first_done || first.as_mut().poll(context).is_ready();
        second_done = second_done || second.as_mut().poll(context).is_ready();
        if first_done && second_done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
}

/// Logs `first-frame <ms since process start>` after the first paint that follows `window`
/// mapping, and records when it was. The CI smoke run greps for it.
fn log_first_frame(
    window: &adw::ApplicationWindow,
    started: Instant,
    exec_ns: Option<i64>,
    painted: Rc<Cell<Option<Instant>>>,
    after: Rc<RefCell<Vec<Hook>>>,
) {
    let on_map = Rc::new(RefCell::new(None));
    let handler = window.connect_map(glib::clone!(
        #[strong]
        on_map,
        move |window| {
            if let Some(handler) = on_map.take() {
                window.disconnect(handler);
            }
            let Some(clock) = window.frame_clock() else {
                tracing::warn!("mapped window has no frame clock; first-frame not measured");
                return;
            };
            let on_paint = Rc::new(RefCell::new(None));
            let painted = painted.clone();
            let after = after.clone();
            let handler = clock.connect_after_paint(glib::clone!(
                #[strong]
                on_paint,
                move |clock| {
                    if let Some(handler) = on_paint.take() {
                        clock.disconnect(handler);
                    }
                    painted.set(Some(Instant::now()));
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    tracing::info!("first-frame {ms:.1}");
                    if let Some(exec_ns) = exec_ns {
                        let since = (glib::monotonic_time() * 1000 - exec_ns) as f64 / 1e6;
                        tracing::info!("first-frame-from-exec {since:.1}");
                    }
                    let hooks = std::mem::take(&mut *after.borrow_mut());
                    if !hooks.is_empty() {
                        glib::idle_add_local_once(move || {
                            for hook in hooks {
                                hook();
                            }
                        });
                    }
                }
            ));
            on_paint.replace(Some(handler));
        }
    ));
    on_map.replace(Some(handler));
}
