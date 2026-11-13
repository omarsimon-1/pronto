//! The composer (M6, ML-067): a card for each draft, docked at the main window's bottom right
//! as Gmail's composer (the user's UI tweak, 2026-10-08; a window of its own for each draft
//! since M6's start). [`open`] is the one way in: the main window's Compose, Reply, Reply All,
//! Forward and Edit Draft, the quick reply's expansion, `mailto:` links, and drafts reopened
//! after a restart or an Undo send. With no main window shown it shows one first.
//!
//! Everything a card needs from outside goes through [`backend::Backend`]: phase A's
//! [`backend::StandIn`] until phase B sets the real one with [`set_backend`]. Quit, and the
//! main window closing, save every open draft first ([`save_all`], [`keep_drafts_on_close`]).

pub mod attachments;
pub mod backend;
pub mod card;
pub mod editor;
mod js;
pub mod recipients;
pub mod store_backend;

use crate::application::ProntoApplication;
use backend::{Backend, StandIn};
pub use card::{Composer, Ending};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::compose::{Draft, ReplyKind};
use sealwax_domain::mailto::Mailto;
use sealwax_domain::model::{AccountId, Address, DraftId, MessageId};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use store_backend::StoreBackend;

/// What a composer starts from.
#[derive(Clone)]
pub enum ComposeRequest {
    /// A new message from `account`: the one the list shows, or the first account.
    New { account: Option<AccountId> },
    /// A reply, reply all or forward of a stored message.
    Reply { message: MessageId, kind: ReplyKind },
    /// A reply to `message` that starts with what the quick reply under its conversation holds
    /// (ML-068): plain text, typed by the user. `kind` is the quick reply's: Reply or Reply
    /// All (`r`, `a`, Enter: `[reader] reply`, issue #8); `recipients` the ones the user put in
    /// place of the compose rules' (the fork's phase B), if any.
    QuickReply {
        message: MessageId,
        text: String,
        kind: ReplyKind,
        recipients: Option<Recipients>,
        /// The inline composer's edited signature; empty removes it, `None` uses the account's.
        signature: Option<String>,
    },
    /// A `mailto:` link (PRD C8): its safe fields only (`domain::mailto`).
    Mailto(Mailto),
    /// A draft the store keeps: reopened after a restart, after Undo send, or after a send
    /// failed.
    Draft(DraftId),
    /// A message in a Drafts folder, written in Pronto or elsewhere (Edit Draft).
    ServerDraft(MessageId),
}

/// Whom a draft goes to, when the user chose it in place of what the compose rules plan (the
/// quick reply's recipients line, the fork's phase B).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Recipients {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
}

impl Recipients {
    pub fn is_empty(&self) -> bool {
        self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty()
    }

    /// Puts them in `draft`'s place.
    pub fn apply(&self, draft: &mut Draft) {
        draft.to = self.to.clone();
        draft.cc = self.cc.clone();
        draft.bcc = self.bcc.clone();
    }
}

/// The counts only: addresses are the user's mail.
impl fmt::Debug for Recipients {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Recipients(to {}, cc {}, bcc {})",
            self.to.len(),
            self.cc.len(),
            self.bcc.len()
        )
    }
}

/// The request's kind only: what it carries is the user's mail.
impl fmt::Debug for ComposeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComposeRequest::New { account } => write!(f, "New({account:?})"),
            ComposeRequest::Reply { message, kind } => write!(f, "Reply({message}, {kind:?})"),
            ComposeRequest::QuickReply { message, kind, .. } => {
                write!(f, "QuickReply({message}, {kind:?})")
            }
            ComposeRequest::Mailto(_) => f.write_str("Mailto"),
            ComposeRequest::Draft(draft) => write!(f, "Draft({draft})"),
            ComposeRequest::ServerDraft(message) => write!(f, "ServerDraft({message})"),
        }
    }
}

/// How long Quit waits for the open drafts to be saved before it goes anyway.
pub const SAVE_ON_QUIT: Duration = Duration::from_secs(3);

thread_local! {
    static REQUESTS: RefCell<Vec<ComposeRequest>> = const { RefCell::new(Vec::new()) };
    static OPEN: RefCell<Vec<Composer>> = const { RefCell::new(Vec::new()) };
    static BACKEND: RefCell<Option<Rc<dyn Backend>>> = const { RefCell::new(None) };
    static STAND_IN: RefCell<Option<Rc<StandIn>>> = const { RefCell::new(None) };
    static STORE: RefCell<Option<Rc<StoreBackend>>> = const { RefCell::new(None) };
    static CHOOSER: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
    static KEYS_FOLLOWED: Cell<bool> = const { Cell::new(false) };
    /// Whether spare editors are made ([`prewarm`]); the self-tests switch it off to time the
    /// cards without one.
    static PREWARM: Cell<bool> = const { Cell::new(true) };
    /// Hears a card take a draft written before ([`on_draft_opened`]).
    static ON_DRAFT_OPENED: RefCell<Option<DraftListener>> = const { RefCell::new(None) };
    /// Whether Send asks first about a missing subject or attachment ([`send_checks`]); unset,
    /// it does unless a self-test script runs.
    static SEND_CHECKS: Cell<Option<bool>> = const { Cell::new(None) };
}

type DraftListener = Rc<dyn Fn(DraftId)>;

/// `listener` hears a card open a draft written before (Edit Draft, a draft left open, Undo
/// send), once the card has it: the draft is the card's from then on, so the inline reply's
/// keeper lets go of it (`compose_actions`; one draft never has two writers). Replaces the
/// listener set before.
pub fn on_draft_opened(listener: impl Fn(DraftId) + 'static) {
    ON_DRAFT_OPENED.with(|hook| hook.replace(Some(Rc::new(listener))));
}

/// A card opened draft `id`, written before.
fn draft_opened(id: DraftId) {
    let listener = ON_DRAFT_OPENED.with(|hook| hook.borrow().clone());
    if let Some(listener) = listener {
        listener(id);
    }
}

/// Opens a composer card for `request` in the main window, shown first if it isn't, and puts
/// the keyboard focus in it. The card shows at once; the backend fills it.
pub fn open(app: &ProntoApplication, request: ComposeRequest) {
    open_card(app, request, true);
}

fn open_card(app: &ProntoApplication, request: ComposeRequest, focus: bool) {
    tracing::info!(kind = ?request, "compose requested");
    REQUESTS.with(|requests| requests.borrow_mut().push(request.clone()));
    follow_keys(app);
    // A draft a card holds already shows there: one draft never has two writers.
    if let Some(composer) = composers()
        .into_iter()
        .find(|composer| composer.holds_request(&request))
    {
        if focus {
            composer.present();
        }
        return;
    }
    let state = app.state();
    let window = match state.window() {
        Some(window) => window,
        None => {
            state.show_main_window(app);
            let Some(window) = state.window() else {
                tracing::warn!("composer: no main window to dock in");
                return;
            };
            window
        }
    };
    let backend = backend(app);
    let composer = Composer::open(app, window.dock(), backend, request, focus);
    OPEN.with(|open| open.borrow_mut().push(composer));
}

/// How long after the window shows the first spare editor is made: out of the way of the first
/// frames and the first conversation.
pub const PREWARM_AFTER: Duration = Duration::from_millis(1500);

/// Makes a spare editor for the next card, its page loaded, at low priority (the fork's phase
/// B): `c`, `f` and Pop Out then open a card whose body is ready about as soon as its draft.
/// Nothing when one waits already.
pub fn prewarm(state: &crate::application::AppState) {
    if !PREWARM.with(Cell::get) {
        return;
    }
    let engine = editor::EditorEngine::shared(&state.config().dirs.cache);
    if engine.spare().is_some() {
        return;
    }
    let theme = card::current_theme(state);
    glib::idle_add_local_full(glib::Priority::LOW, move || {
        let engine = engine.clone();
        let theme = theme.clone();
        glib::spawn_future_local(async move { engine.prewarm(&theme).await });
        glib::ControlFlow::Break
    });
}

/// The main window closed (background mode): no spare editor waits, and with no card open the
/// composer's web process may end; the next window makes a spare again ([`prewarm`]).
pub fn rest(state: &crate::application::AppState) {
    editor::EditorEngine::shared(&state.config().dirs.cache).rest(is_open());
}

/// Self-tests: whether spare editors are made; off, the one waiting goes too.
pub fn set_prewarm(state: &crate::application::AppState, on: bool) {
    PREWARM.with(|prewarm| prewarm.set(on));
    let engine = editor::EditorEngine::shared(&state.config().dirs.cache);
    if on {
        prewarm(state);
    } else if let Some(spare) = engine.take_spare(&card::current_theme(state)) {
        spare.release();
    }
}

/// Every open card shows or hides its formatting toolbar.
pub fn show_formatting(on: bool) {
    for composer in composers() {
        composer.show_formatting(on);
    }
}

/// Every request [`open`] was given in this process, oldest first; the self-tests read it.
pub fn requests() -> Vec<ComposeRequest> {
    REQUESTS.with(|requests| requests.borrow().clone())
}

/// The composers open now, oldest first.
pub fn composers() -> Vec<Composer> {
    OPEN.with(|open| open.borrow().clone())
}

/// Whether a composer is open.
pub fn is_open() -> bool {
    OPEN.with(|open| !open.borrow().is_empty())
}

/// The composer holding the keyboard focus of `window`.
pub fn focused(window: &impl IsA<gtk::Window>) -> Option<Composer> {
    let focus = gtk::prelude::GtkWindowExt::focus(window.as_ref())?;
    composers()
        .into_iter()
        .find(|composer| composer.holds(&focus))
}

/// Makes `backend` the one new composers use (phase B: the store and the engine).
pub fn set_backend(backend: Rc<dyn Backend>) {
    BACKEND.with(|current| current.replace(Some(backend)));
}

/// The phase A stand-in, when it is the backend in use.
pub fn stand_in() -> Option<Rc<StandIn>> {
    STAND_IN.with(|stand_in| stand_in.borrow().clone())
}

/// The stand-in, made the backend now if no backend is set yet (self-tests set it up before
/// the first composer opens).
pub fn ensure_stand_in(app: &ProntoApplication) -> Option<Rc<StandIn>> {
    backend(app);
    stand_in()
}

/// Makes phase A's in-memory stand-in the backend (self-tests that check its records).
pub fn use_stand_in(app: &ProntoApplication) -> Rc<StandIn> {
    let stand_in = StandIn::new(&app.state());
    STAND_IN.with(|current| current.replace(Some(stand_in.clone())));
    set_backend(stand_in.clone());
    stand_in
}

/// Makes the store's backend the one new composers use: the app does at startup.
pub fn use_store(app: &ProntoApplication) -> Rc<StoreBackend> {
    let store = StoreBackend::new(&app.state());
    STORE.with(|current| current.replace(Some(store.clone())));
    set_backend(store.clone());
    store
}

/// The store's backend, when the app set it.
pub fn store_backend() -> Option<Rc<StoreBackend>> {
    STORE.with(|store| store.borrow().clone())
}

/// Reopens the drafts whose composers were open when Pronto last stopped (a crash, a kill, a
/// logout): after the first frame, once the store is ready, open in the dock but leaving the
/// keyboard focus where it is. Drafts a composer shows already are left alone.
pub fn reopen_drafts(app: &ProntoApplication) {
    let ready = app.state().mailbox().wait_ready();
    let app = app.downgrade();
    gtk4::glib::spawn_future_local(async move {
        ready.await;
        let Some(app) = app.upgrade() else {
            return;
        };
        let Some(store) = app.state().mailbox().shared_store() else {
            return;
        };
        let Ok(open_ids) = store.open_drafts().recv().await else {
            tracing::warn!("composer: the drafts left open couldn't be read");
            return;
        };
        let shown: Vec<DraftId> = composers().iter().filter_map(Composer::id).collect();
        let reopened: Vec<DraftId> = open_ids
            .into_iter()
            .filter(|id| !shown.contains(id))
            .collect();
        if !reopened.is_empty() {
            tracing::info!(
                drafts = reopened.len(),
                "composer: reopening drafts left open"
            );
        }
        for id in reopened {
            open_card(&app, ComposeRequest::Draft(id), false);
        }
    });
}

/// The backend in use, made the stand-in when none is set yet. The quick reply prepares, saves
/// and queues its replies through it too (M6 phase B).
pub fn backend(app: &ProntoApplication) -> Rc<dyn Backend> {
    if let Some(backend) = BACKEND.with(|current| current.borrow().clone()) {
        return backend;
    }
    let stand_in = StandIn::new(&app.state());
    STAND_IN.with(|current| current.replace(Some(stand_in.clone())));
    let backend: Rc<dyn Backend> = stand_in;
    BACKEND.with(|current| current.replace(Some(backend.clone())));
    backend
}

/// Whether Send asks first when the subject is empty or the text names an attachment that
/// isn't there. Self-test scripts answer no dialog: off for them until one asks
/// ([`set_send_checks`]).
pub fn send_checks() -> bool {
    SEND_CHECKS.with(Cell::get).unwrap_or_else(|| {
        let on = !std::env::args().any(|arg| arg == "--self-test");
        set_send_checks(on);
        on
    })
}

/// Self-tests: Send asks ([`send_checks`]) or doesn't.
pub fn set_send_checks(on: bool) {
    SEND_CHECKS.with(|checks| checks.set(Some(on)));
}

/// The other of Reply and Reply All (Switch Reply or Reply All); a forward stays one.
pub fn switched(kind: ReplyKind) -> ReplyKind {
    match kind {
        ReplyKind::Reply => ReplyKind::ReplyAll,
        ReplyKind::ReplyAll => ReplyKind::Reply,
        ReplyKind::Forward => ReplyKind::Forward,
    }
}

/// Whether `text` (what the user typed, not what it quotes) names an attachment: "attached",
/// "attachment", "enclosed"…
pub fn mentions_attachment(text: &str) -> bool {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .flat_map(|line| line.split(|c: char| !c.is_alphabetic()))
        .map(str::to_lowercase)
        .any(|word| word.starts_with("attach") || word == "enclosed")
}

/// What Send asks about before a message with `subject` and the typed `text` goes with
/// `attachments` files: `None` when nothing is missing.
pub fn send_doubt(subject: &str, text: &str, attachments: usize) -> Option<&'static str> {
    let subject = subject.trim().is_empty();
    let attachment = attachments == 0 && mentions_attachment(text);
    match (subject, attachment) {
        (true, true) => Some("No subject, no attachment. Send?"),
        (true, false) => Some("No subject. Send?"),
        (false, true) => Some("No attachment. Send?"),
        (false, false) => None,
    }
}

/// Asks `doubt` ([`send_doubt`]) over `parent`: Enter sends, Escape goes back to the draft.
pub async fn confirm_send(parent: &impl IsA<gtk::Widget>, doubt: &str) -> bool {
    let dialog = adw::AlertDialog::builder()
        .heading(doubt)
        .default_response("send")
        .close_response("cancel")
        .build();
    dialog.set_widget_name("composer-send-dialog");
    dialog.add_responses(&[("cancel", "Cancel"), ("send", "Send")]);
    dialog.choose_future(Some(parent)).await == "send"
}

/// Self-tests: the next Attach File… takes `paths` instead of asking the file chooser.
pub fn answer_chooser(paths: Vec<PathBuf>) {
    CHOOSER.with(|chooser| chooser.replace(Some(paths)));
}

fn chooser_answer() -> Option<Vec<PathBuf>> {
    CHOOSER.with(|chooser| chooser.take())
}

/// Forgets a card that went.
fn forget(root: &gtk::Box) {
    OPEN.with(|open| open.borrow_mut().retain(|composer| composer.root() != root));
}

/// Saves every open draft and lets the cards go; Quit waits for it (at most [`SAVE_ON_QUIT`])
/// before it destroys the windows.
pub async fn save_all() {
    save(composers()).await;
}

/// The main window closing (into background mode, or for good) first saves the drafts its
/// cards hold and lets them go, at most [`SAVE_ON_QUIT`]; Quit has saved them already.
pub fn keep_drafts_on_close(window: &adw::ApplicationWindow) {
    window.connect_close_request(|window| {
        let held: Vec<Composer> = composers()
            .into_iter()
            .filter(|composer| composer.window().as_ref() == Some(window))
            .collect();
        if held.is_empty() {
            return glib::Propagation::Proceed;
        }
        let hold = window.application().map(|app| app.hold());
        let target = window.downgrade();
        glib::spawn_future_local(async move {
            save(held).await;
            if let Some(window) = target.upgrade() {
                window.close();
            }
            drop(hold);
        });
        glib::Propagation::Stop
    });
    // A window destroyed without closing (Quit has saved the drafts) has let its cards go by
    // then.
    window.connect_destroy(|window| {
        for composer in composers() {
            if composer.window().is_none_or(|docked| &docked == window) {
                composer.release();
            }
        }
    });
}

async fn save(composers: Vec<Composer>) {
    let saving = async move {
        for composer in composers {
            composer.save_and_release().await;
        }
    };
    let saved = crate::mailbox::with_timeout(saving, SAVE_ON_QUIT).await;
    if saved.is_none() {
        tracing::warn!("composer: Quit stopped waiting for the drafts to be saved");
    }
}

/// `keys.toml` changes reach every open composer.
fn follow_keys(app: &ProntoApplication) {
    if KEYS_FOLLOWED.with(|followed| followed.replace(true)) {
        return;
    }
    let Some(prefs) = app.state().preferences() else {
        KEYS_FOLLOWED.with(|followed| followed.set(false));
        return;
    };
    let weak = Rc::downgrade(&prefs);
    prefs.connect_changed(move |update| {
        if !update.keymap_changed {
            return;
        }
        if let Some(prefs) = weak.upgrade() {
            let keymap = prefs.keymap();
            for composer in composers() {
                composer.update_keys(&keymap);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_asks_about_a_missing_subject_or_attachment() {
        assert_eq!(send_doubt("Hytta", "Vi ses fredag.", 0), None);
        assert_eq!(
            send_doubt("  ", "Vi ses fredag.", 0),
            Some("No subject. Send?")
        );
        assert_eq!(
            send_doubt("Hytta", "The map is attached.", 0),
            Some("No attachment. Send?")
        );
        assert_eq!(send_doubt("Hytta", "The map is attached.", 1), None);
        assert_eq!(
            send_doubt("", "See attachment", 0),
            Some("No subject, no attachment. Send?")
        );
    }

    #[test]
    fn a_reply_switches_between_the_sender_and_everyone() {
        assert_eq!(switched(ReplyKind::Reply), ReplyKind::ReplyAll);
        assert_eq!(switched(ReplyKind::ReplyAll), ReplyKind::Reply);
        assert_eq!(switched(ReplyKind::Forward), ReplyKind::Forward);
    }

    #[test]
    fn only_the_typed_words_name_an_attachment() {
        for text in [
            "attached",
            "See Attached.",
            "the attachment",
            "I'm attaching it",
            "Enclosed: the plan",
        ] {
            assert!(mentions_attachment(text), "{text}");
        }
        for text in [
            "",
            "Vi ses fredag.",
            "reattach the hose",
            "an enclosure",
            "> see attached",
        ] {
            assert!(!mentions_attachment(text), "{text}");
        }
    }
}
