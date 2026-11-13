//! The main window's compose commands (M6: ML-068, ML-069, ML-074): Compose, Reply, Reply All,
//! Forward and Edit Draft from the list, the reader, the palette and their keys, and what the
//! reader asks for itself (the cards' Forward and Edit draft, the inline reply's Pop Out). The
//! inline reply's Send goes through [`send_quick`], the seam phase B wires to the outbox.
//!
//! - **Compose** writes from the account the list shows; in All Inboxes from the selected
//!   message's account, else the first account (`ComposeRequest::New { account: None }`).
//! - **Reply, Reply All and Reply to Sender** answer under the conversation (the fork's phase B,
//!   Superhuman's reply flow): the reader's inline reply takes the focus, answering the message
//!   the user is on there (the focused card, or the one `n` or `p` reached), else the latest that
//!   isn't the user's own; a Focused list opens the conversation first. Reply (and Reply to
//!   Sender) answers the sender, Reply All everyone; Enter in a conversation answers as
//!   `[reader] reply` says, everyone by default (Superhuman's Enter, the fork's phase C; the
//!   reader's Quick Reply). Without a conversation to answer under they open a composer card, as
//!   **Forward** always does (its caret in To). They don't apply to several messages, nor to a
//!   draft.
//! - **Send and Archive** (Ctrl+Shift+Enter, in the inline reply or a reply card) sends, then
//!   archives the conversation as `e` does (the same targets and command path) and the list
//!   moves on; the archive's Undo takes the send back too.
//! - **Edit Draft** opens the selected message of a Drafts folder.

use crate::actions::ActionSet;
use crate::application::{AppState, ProntoApplication};
use crate::composer::backend::{Refusal, Unsaved};
use crate::composer::{self, ComposeRequest};
use crate::list::{MessageList, Selected};
use crate::outbox;
use crate::reader::{QuickDraft, Reader, SendMode};
use crate::triage::Target;
use crate::window::{Pane, Window};
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::actions::ActionId;
use sealwax_domain::compose::{Draft, ReplyKind};
use sealwax_domain::model::{AccountId, DraftId, Folder, FolderId, FolderRole, MessageId};
use sealwax_infrastructure::store::{ListRow, ViewScope};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module handles.
pub const COMMANDS: [ActionId; 6] = [
    ActionId::Compose,
    ActionId::Reply,
    ActionId::ReplyAll,
    ActionId::ReplySender,
    ActionId::Forward,
    ActionId::EditDraft,
];

/// Connects the compose commands to the window's actions.
pub fn install(state: &Rc<AppState>, window: &Window, actions: &ActionSet) {
    for id in COMMANDS {
        let weak = Rc::downgrade(state);
        let target = window.window().downgrade();
        actions.connect(id, move || {
            let (Some(state), Some(window)) = (weak.upgrade(), target.upgrade()) else {
                return;
            };
            if let Some(kind) = inline_kind(id)
                && reply_inline(&state, kind)
            {
                return;
            }
            if let Some(request) = request_for(&state, id) {
                open(&window, request);
            }
        });
    }
}

/// How Reply, Reply All and Reply to Sender answer under the conversation; `None` for the
/// commands that always open a composer card.
fn inline_kind(id: ActionId) -> Option<ReplyKind> {
    match id {
        ActionId::Reply => Some(ReplyKind::Reply),
        ActionId::ReplyAll => Some(ReplyKind::ReplyAll),
        ActionId::ReplySender => Some(ReplyKind::Reply),
        _ => None,
    }
}

/// The inline reply under the selected message's conversation takes the focus, answering as
/// `kind`; a Focused list (or a narrow window's) opens the conversation first. `false` when
/// there is none to take it: a composer card opens instead.
fn reply_inline(state: &AppState, kind: ReplyKind) -> bool {
    let (Some(list), Some(reader), Some(window)) = (state.list(), state.reader(), state.window())
    else {
        return false;
    };
    let Selected::One(selected) = list.reported() else {
        return false;
    };
    // The message the user moved to in the conversation on screen, when they did.
    let message = (reader.message() == Some(selected))
        .then(|| reader.chosen_message())
        .flatten();
    if window.is_one_pane() && window.pane() == Pane::List {
        list.open_selected();
    }
    // Opening may report the conversation at its new first unread message (`open_selected`).
    let selected = match list.reported() {
        Selected::One(opened) => opened,
        _ => selected,
    };
    reader.reply_inline(selected, message, kind)
}

/// The reader's own requests open composer windows too; its inline reply sends, and keeps what
/// it holds as a draft, through the composer's backend (`composer::backend`).
pub fn connect_reader(reader: &Reader, window: &Window) {
    let target = window.window().downgrade();
    reader.connect_compose(move |request| {
        if let Some(window) = target.upgrade() {
            open(&window, request);
        }
    });
    let Some(app) = window
        .window()
        .application()
        .and_downcast::<ProntoApplication>()
    else {
        return;
    };
    let weak_ai = app.downgrade();
    reader.set_saved_reply_opener(move |message| {
        weak_ai
            .upgrade()
            .and_then(|app| app.state().ai())
            .is_some_and(|ai| ai.open_ready(message))
    });
    let weak = app.downgrade();
    reader.set_quick_signature(move |summary, headers| {
        let app = weak.upgrade()?;
        let state = app.state();
        let mailbox = state.mailbox();
        let folder = mailbox.folder(summary.folder)?;
        let account = mailbox
            .accounts()
            .into_iter()
            .find(|account| account.id == folder.account)?;
        let settings = state.preferences()?.accounts();
        let senders = composer::store_backend::senders(&account, &settings);
        headers
            .and_then(|headers| sealwax_domain::compose::identity::reply_sender(&senders, headers))
            .or_else(|| senders.first())
            .and_then(|sender| sender.signature.clone())
    });
    let weak = app.downgrade();
    reader.set_quick_send(move |draft, mode| {
        let app = weak.upgrade();
        // Send and Archive takes its rows now, whatever the selection does meanwhile.
        let targets = match (&app, mode) {
            (Some(app), SendMode::SendAndArchive) => Some(done_rows(&app.state(), draft.message)),
            _ => None,
        };
        let at = match mode {
            SendMode::Later(at) => Some(at),
            _ => None,
        };
        Box::pin(async move {
            match app {
                Some(app) => send_quick(&app, draft, targets, at).await,
                None => false,
            }
        })
    });
    // Send Later in the inline reply: the time picker over the window, then the send at that
    // time.
    let weak = app.downgrade();
    let target = window.window().downgrade();
    reader.set_quick_later(move || {
        let (Some(app), Some(window)) = (weak.upgrade(), target.upgrade()) else {
            return;
        };
        let state = Rc::downgrade(&app.state());
        crate::time_picker::pick(
            &window,
            crate::time_picker::SEND_LATER,
            &sealwax_domain::when::Preset::ALL,
            move |chosen| {
                if let Some(reader) = state.upgrade().and_then(|state| state.reader()) {
                    reader.send_quick_reply_at(chosen.at);
                }
            },
        );
    });
    let keeper = Rc::new(QuickDrafts {
        app: app.downgrade(),
        kept: RefCell::new(HashMap::new()),
    });
    KEEPERS.with(|keepers| {
        let mut keepers = keepers.borrow_mut();
        keepers.retain(|kept| kept.strong_count() > 0);
        keepers.push(Rc::downgrade(&keeper));
    });
    // A card that opens an inline reply's draft (Edit Draft on its Drafts-folder copy, Enter on
    // its row) takes it from the keeper.
    composer::on_draft_opened(|id| {
        for keeper in keepers() {
            keeper.give(id);
        }
    });
    // The window closing (into background mode, or for good) first saves what its inline
    // replies hold, as its cards' drafts are (`composer::keep_drafts_on_close`): the keeper goes
    // with the reader, and its saves wait for the typing to rest. It waits once, at most
    // `SAVE_ON_QUIT`, as Quit does; then the window closes, saved or not (its cards' drafts
    // may hold it a moment longer: that close goes too). Before, a save that kept failing held
    // the window open for good.
    let weak = Rc::downgrade(&keeper);
    let waited = Rc::new(Cell::new(false));
    window.window().connect_close_request(move |window| {
        if waited.get() {
            return glib::Propagation::Proceed;
        }
        let Some(keeper) = weak.upgrade().filter(|keeper| keeper.unsaved()) else {
            return glib::Propagation::Proceed;
        };
        let hold = window.application().map(|app| app.hold());
        let target = window.downgrade();
        let waited = waited.clone();
        glib::spawn_future_local(async move {
            keeper.save_now_or_give_up().await;
            if let Some(window) = target.upgrade() {
                waited.set(true);
                window.close();
            }
            drop(hold);
        });
        glib::Propagation::Stop
    });
    let saver = keeper.clone();
    reader.connect_quick_text(move |draft| saver.changed(draft));
    reader.set_quick_finder(move |message| keeper.find(message));
}

thread_local! {
    /// Every window's keeper of its inline replies' drafts, for Quit and the cards that take a
    /// draft.
    static KEEPERS: RefCell<Vec<std::rc::Weak<QuickDrafts>>> = const { RefCell::new(Vec::new()) };
}

fn keepers() -> Vec<Rc<QuickDrafts>> {
    KEEPERS.with(|keepers| {
        keepers
            .borrow()
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .collect()
    })
}

/// Whether an inline reply holds a change its draft doesn't have yet.
pub fn quick_drafts_unsaved() -> bool {
    keepers().iter().any(|keeper| keeper.unsaved())
}

/// Saves what every inline reply holds now, rather than once the typing has rested (Quit,
/// before the windows go), waiting at most `composer::SAVE_ON_QUIT` for them all.
pub async fn save_quick_drafts() {
    let saving = async {
        for keeper in keepers() {
            keeper.save_now().await;
        }
    };
    if crate::mailbox::with_timeout(saving, composer::SAVE_ON_QUIT)
        .await
        .is_none()
    {
        tracing::warn!("the inline replies' drafts weren't all saved in time");
    }
}

/// What Send and Archive archives with a reply to `message`, taken when the send is asked for:
/// the selected rows when the reader shows the reply's conversation as the selected one (the
/// rows `e` takes, a conversation row standing for its members in the view's folders), else
/// `None`: the app reads the conversation's Inbox messages from the store once the send is
/// queued.
pub fn done_rows(state: &AppState, message: MessageId) -> Option<Vec<ListRow>> {
    let (list, reader) = (state.list()?, state.reader()?);
    let Selected::One(selected) = list.reported() else {
        return None;
    };
    (reader.message() == Some(selected) && reader.shows_message(message))
        .then(|| list.selected_rows().ok())
        .flatten()
}

/// Reply, Reply All, Reply to Sender and Forward apply to one selected message that isn't a
/// draft; Edit Draft to one in a Drafts folder. `targets` are the selected messages whose rows
/// are loaded.
pub fn refresh(actions: &ActionSet, list: &MessageList, targets: &[Target], folders: &[Folder]) {
    let one = matches!(list.reported(), Selected::One(_));
    let draft = one
        && targets
            .first()
            .and_then(|target| folders.iter().find(|folder| folder.id == target.folder))
            .is_some_and(|folder| folder.role == FolderRole::Drafts);
    for id in [
        ActionId::Reply,
        ActionId::ReplyAll,
        ActionId::ReplySender,
        ActionId::Forward,
    ] {
        actions.set_applies(id, one && !draft);
    }
    actions.set_applies(ActionId::EditDraft, draft);
}

/// Opening a message of a Drafts folder (Enter, `o`, a double click) edits it in the composer
/// rather than showing it (the user's choice at the M6 gate); true when it did.
pub fn open_if_draft(state: &AppState, window: &Window, message: MessageId) -> bool {
    let Some(list) = state.list() else {
        return false;
    };
    let folders = state.mailbox().folders();
    let in_drafts = list
        .selected_targets()
        .ok()
        .and_then(|targets| targets.into_iter().find(|target| target.id == message))
        .and_then(|target| folders.iter().find(|folder| folder.id == target.folder))
        .is_some_and(|folder| folder.role == FolderRole::Drafts);
    if in_drafts {
        open(window.window(), ComposeRequest::ServerDraft(message));
    }
    in_drafts
}

fn open(window: &adw::ApplicationWindow, request: ComposeRequest) {
    if let Some(app) = window.application().and_downcast::<ProntoApplication>() {
        composer::open(&app, request);
    }
}

/// What command `id` asks the composer for now; `None` when it doesn't apply.
fn request_for(state: &AppState, id: ActionId) -> Option<ComposeRequest> {
    let list = state.list()?;
    let kind = match id {
        ActionId::Compose => {
            let selected = list
                .selected_targets()
                .ok()
                .and_then(|targets| targets.first().map(|target| target.folder));
            let account = compose_account(list.scope(), selected, &state.mailbox().folders());
            return Some(ComposeRequest::New { account });
        }
        ActionId::EditDraft => {
            return match list.reported() {
                Selected::One(message) => Some(ComposeRequest::ServerDraft(message)),
                _ => None,
            };
        }
        ActionId::Reply => ReplyKind::Reply,
        ActionId::ReplyAll => ReplyKind::ReplyAll,
        ActionId::Forward => ReplyKind::Forward,
        _ => ReplyKind::Reply,
    };
    let message = message_for(&list, state.reader().as_ref())?;
    Some(ComposeRequest::Reply { message, kind })
}

/// The message Reply, Reply All and Forward act on: the selected one, or, once the reader shows
/// its conversation, the one the user is on there.
fn message_for(list: &MessageList, reader: Option<&Reader>) -> Option<MessageId> {
    let Selected::One(selected) = list.reported() else {
        return None;
    };
    match reader {
        Some(reader) if reader.message() == Some(selected) => reader.current_message(),
        _ => Some(selected),
    }
}

/// The account a new message is written from: the one whose folder the list shows; in All
/// Inboxes the selected message's (`selected` is its folder); `None` leaves it to the composer,
/// which takes the first account.
pub fn compose_account(
    scope: Option<ViewScope>,
    selected: Option<FolderId>,
    folders: &[Folder],
) -> Option<AccountId> {
    let account_of = |id: FolderId| {
        folders
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.account)
    };
    match scope {
        Some(ViewScope::Folder(folder)) => account_of(folder),
        _ => selected.and_then(account_of),
    }
}

thread_local! {
    /// Every quick reply Send was asked for in this process, oldest first.
    static QUICK_SENDS: RefCell<Vec<QuickDraft>> = const { RefCell::new(Vec::new()) };
}

/// Every inline reply Send was asked for in this process, oldest first; the self-tests read it.
pub fn quick_sends() -> Vec<QuickDraft> {
    QUICK_SENDS.with(|sends| sends.borrow().clone())
}

/// The inline reply's Send (M6 phase B): the reply prepared by the compose rules
/// (`ComposeRequest::QuickReply`, with the recipients the user chose), saved, and handed to the
/// backend's send, which queues it with Undo send (`outbox::queued`). With `done` (Send and
/// Archive: the rows taken when it was asked for, or `None` inside to read them) the
/// conversation is archived once it is queued; with `at` (Send Later) it waits until then
/// (`outbox::scheduled`). `false` puts the draft back in the box: the window says why. Only the
/// message's id is logged, never the text.
async fn send_quick(
    app: &ProntoApplication,
    draft: QuickDraft,
    done: Option<Option<Vec<ListRow>>>,
    at: Option<i64>,
) -> bool {
    tracing::info!(
        id = draft.message.0,
        archive = done.is_some(),
        later = at.is_some(),
        "quick reply send requested"
    );
    QUICK_SENDS.with(|sends| sends.borrow_mut().push(draft.clone()));
    let backend = composer::backend(app);
    let refuse = |why: &str| {
        let toast = adw::Toast::builder()
            .title(format!("The reply wasn't sent. {why}"))
            .use_markup(false)
            .timeout(outbox::FAILED_SHOWN.as_secs() as u32)
            .build();
        app.state().show_toast(&toast, false);
        false
    };
    let request = ComposeRequest::QuickReply {
        message: draft.message,
        text: draft.text.clone(),
        kind: draft.kind,
        recipients: draft.recipients.clone(),
        signature: draft.signature.clone(),
    };
    let prepared = match backend.prepare(request).await {
        Ok(prepared) => prepared,
        Err(why) => return refuse(&why),
    };
    if let Err(why) = backend.save(prepared.id, prepared.draft.clone()).await {
        let _ = backend.discard(prepared.id).await;
        return refuse(&why.to_string());
    }
    let message = draft.message;
    outbox::from_quick_reply(prepared.id, draft);
    if done.is_some() {
        outbox::hold(prepared.id);
    }
    match backend.send(prepared.id, prepared.draft, at).await {
        Ok(()) => {
            if let Some(rows) = done {
                app.state().archive_after_send(prepared.id, message, rows);
            }
            true
        }
        Err(Refusal(why)) => {
            outbox::take_held(prepared.id);
            outbox::forget_quick(prepared.id);
            let _ = backend.discard(prepared.id).await;
            refuse(&why)
        }
    }
}

/// How long the quick reply's text rests before it is saved as a draft, and the most it waits
/// while the typing goes on (PRD C5: at most 5 s behind the typing).
pub const QUICK_SAVE_IDLE: Duration = Duration::from_secs(2);
pub const QUICK_SAVE_AT_MOST: Duration = Duration::from_secs(5);
/// The longest a save that failed waits before it is tried again ([`retry_after`]).
pub const QUICK_RETRY_AT_MOST: Duration = Duration::from_secs(60);

/// How long the save after `failures` failed ones in a row waits at least, counted from the
/// last failure: 2 s, then 4, 8, 16… at most [`QUICK_RETRY_AT_MOST`], so a store that keeps
/// failing is asked now and then, never in a loop (while the typing goes on too).
pub fn retry_after(failures: u32) -> Duration {
    match failures {
        0 => Duration::ZERO,
        failures => QUICK_SAVE_IDLE
            .saturating_mul(1 << (failures - 1).min(16))
            .min(QUICK_RETRY_AT_MOST),
    }
}

thread_local! {
    /// How many times the inline replies' drafts were saved or discarded (tried) in this
    /// process; the self-tests read it.
    static QUICK_TRIES: Cell<u32> = const { Cell::new(0) };
}

/// How many times the draft keeper saved or discarded an inline reply's draft (or tried to).
pub fn quick_draft_tries() -> u32 {
    QUICK_TRIES.with(Cell::get)
}

/// One message's inline reply as a draft.
#[derive(Default)]
struct Kept {
    /// The draft as it was last saved; `None` until the first save, and once a card took it.
    draft: Option<(DraftId, Draft)>,
    /// What the inline reply holds, and as it was last saved (`None`: in no draft of the box's).
    held: Option<QuickDraft>,
    saved: Option<QuickDraft>,
    timer: Option<glib::SourceId>,
    first_unsaved: Option<Instant>,
    busy: bool,
    /// Saves that failed in a row, and when the last one did: the next waits its turn.
    failures: u32,
    failed: Option<Instant>,
    /// A card took the draft while a save of it was under way: once that is done, the draft is
    /// the card's ([`QuickDrafts::give`]).
    given: Option<DraftId>,
}

impl Kept {
    fn is_empty(&self) -> bool {
        self.held.as_ref().is_none_or(QuickDraft::is_empty)
    }
}

/// Keeps every quick reply's text as a draft through the composer's backend: prepared as a
/// reply to its message (`ComposeRequest::QuickReply` with no text yet), again when it answers
/// another way ([`keep`]), saved with the text as its body (the quote comes back when it is
/// sent or opened in a composer, from the compose rules), closed so no composer window reopens
/// it, and discarded when the box empties; let go when a card opens it ([`QuickDrafts::give`])
/// or it is out of the box's hands ([`keep`]). After a restart the reader finds it again through
/// the store's newest closed reply draft to the message ([`QuickDrafts::find`]).
struct QuickDrafts {
    app: glib::WeakRef<ProntoApplication>,
    kept: RefCell<HashMap<MessageId, Kept>>,
}

impl QuickDrafts {
    /// Whether a change waits to be saved (or a save that failed, to be tried again), or is
    /// being saved.
    fn unsaved(&self) -> bool {
        self.kept
            .borrow()
            .values()
            .any(|entry| entry.timer.is_some() || entry.busy)
    }

    /// Saves every change now, without waiting for the typing to rest (or a failed save's
    /// turn); resolves once none is under way. Each is tried once: one that fails meanwhile
    /// waits for its turn again ([`retry_after`]), and isn't tried in a loop.
    async fn save_now(self: &Rc<Self>) {
        let start = Instant::now();
        let failed_since = |entry: &Kept| entry.failed.is_some_and(|failed| failed >= start);
        loop {
            let due: Vec<MessageId> = self
                .kept
                .borrow_mut()
                .iter_mut()
                .filter(|(_, entry)| !failed_since(entry))
                .filter_map(|(message, entry)| {
                    let timer = entry.timer.take()?;
                    timer.remove();
                    Some(*message)
                })
                .collect();
            for message in due {
                self.flush(message);
            }
            let waiting = self
                .kept
                .borrow()
                .values()
                .any(|entry| entry.busy || (entry.timer.is_some() && !failed_since(entry)));
            if !waiting {
                break;
            }
            glib::timeout_future(Duration::from_millis(10)).await;
        }
    }

    /// [`QuickDrafts::save_now`], waiting at most `composer::SAVE_ON_QUIT` (a window closing).
    async fn save_now_or_give_up(self: &Rc<Self>) {
        if crate::mailbox::with_timeout(self.save_now(), composer::SAVE_ON_QUIT)
            .await
            .is_none()
        {
            tracing::warn!("the inline replies' drafts weren't all saved in time");
        }
    }

    fn changed(self: &Rc<Self>, draft: &QuickDraft) {
        let message = draft.message;
        let mut kept = self.kept.borrow_mut();
        let entry = kept.entry(message).or_default();
        entry.held = Some(draft.clone());
        if let Some(timer) = entry.timer.take() {
            timer.remove();
        }
        let unchanged = match (&entry.saved, &entry.held) {
            (Some(saved), Some(held)) => saved == held,
            (None, Some(held)) => held.is_empty(),
            _ => true,
        };
        if unchanged && !entry.busy {
            entry.first_unsaved = None;
            return;
        }
        let now = Instant::now();
        let first = *entry.first_unsaved.get_or_insert(now);
        let mut due = (now + QUICK_SAVE_IDLE).min(first + QUICK_SAVE_AT_MOST);
        // After a failed save the next waits its turn, whatever the typing does.
        if let Some(failed) = entry.failed {
            due = due.max(failed + retry_after(entry.failures));
        }
        let delay = due.saturating_duration_since(now);
        let weak = Rc::downgrade(self);
        entry.timer = Some(glib::timeout_add_local_once(delay, move || {
            if let Some(keeper) = weak.upgrade() {
                keeper.flush(message);
            }
        }));
    }

    fn flush(self: &Rc<Self>, message: MessageId) {
        let (held, draft) = {
            let mut kept = self.kept.borrow_mut();
            let Some(entry) = kept.get_mut(&message) else {
                return;
            };
            entry.timer = None;
            if entry.busy {
                return;
            }
            let Some(held) = entry.held.clone() else {
                return;
            };
            entry.busy = true;
            (held, entry.draft.clone())
        };
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let keeper = self.clone();
        glib::spawn_future_local(async move {
            let (draft, done) = keep(&app, &held, draft).await;
            let again = {
                let mut kept = keeper.kept.borrow_mut();
                let Some(entry) = kept.get_mut(&message) else {
                    return;
                };
                entry.busy = false;
                // A card took the draft meanwhile: it is the card's, and the box's text goes
                // into a new one.
                let given = entry.given.take();
                let taken = given.is_some() && draft.as_ref().map(|(id, _)| *id) == given;
                entry.draft = if taken { None } else { draft };
                if done && !taken {
                    entry.saved = Some(held);
                    entry.first_unsaved = None;
                    entry.failures = 0;
                    entry.failed = None;
                } else {
                    if !done {
                        entry.failures = entry.failures.saturating_add(1);
                        entry.failed = Some(Instant::now());
                    }
                    if entry.draft.is_none() {
                        entry.saved = None;
                    }
                }
                (entry.held != entry.saved)
                    .then(|| entry.held.clone())
                    .flatten()
            };
            if let Some(held) = again {
                keeper.changed(&held);
            }
        });
    }

    /// A composer card opened draft `id`, written before (Edit Draft on the Drafts-folder copy
    /// of an inline reply's draft, Enter on its row): when it is an inline reply's, it is the
    /// card's from now on, so one draft never has two writers. The keeper lets it go, never to
    /// save or discard it again, and the box that holds just what the draft does empties, as
    /// after Pop Out: its text is in the card. A box holding more (typing not saved yet, or a
    /// save under way) keeps it, for a new draft. Before, the box and the card both wrote the
    /// draft, the box kept text the card had sent or discarded, and emptying it could discard
    /// a send the card had queued.
    fn give(self: &Rc<Self>, id: DraftId) {
        let emptied = {
            let mut kept = self.kept.borrow_mut();
            let Some(entry) = kept
                .values_mut()
                .find(|entry| entry.draft.as_ref().is_some_and(|(kept, _)| *kept == id))
            else {
                return;
            };
            tracing::info!(draft = id.0, "an inline reply's draft opened in a card");
            entry.draft = None;
            entry.failures = 0;
            entry.failed = None;
            let same = !entry.busy && entry.timer.is_none() && entry.held == entry.saved;
            entry.saved = None;
            if entry.busy {
                entry.given = Some(id);
            }
            if same {
                entry.first_unsaved = None;
                entry.held.take()
            } else {
                None
            }
        };
        let reader = self.app.upgrade().and_then(|app| app.state().reader());
        if let (Some(held), Some(reader)) = (emptied, reader) {
            reader.quick_reply_taken(&held);
        }
    }

    /// What the quick reply held for `message` before this session: the text of its draft,
    /// when the draft's body is the quick reply's plain text, how it answered, and whom the
    /// draft goes to (the box keeps them when the user chose them).
    fn find(self: &Rc<Self>, message: MessageId) -> crate::reader::Local<Option<QuickDraft>> {
        let known = self.kept.borrow().contains_key(&message);
        let reply = self.app.upgrade().filter(|_| !known).and_then(|app| {
            app.state()
                .mailbox()
                .store()
                .map(|store| store.reply_draft(message))
        });
        let keeper = self.clone();
        Box::pin(async move {
            let stored = reply?.recv().await.ok().flatten()?;
            // A generated draft waiting for this message is its card's (Reply opens it there),
            // not text the user wrote under the conversation.
            let generated = keeper
                .app
                .upgrade()
                .and_then(|app| app.state().ai())
                .and_then(|ai| ai.ready_draft(message));
            if generated == Some(stored.id) {
                return None;
            }
            let (text, signature) = quick_body_parts(&stored.draft.html)?;
            let kind = stored
                .draft
                .source
                .as_ref()
                .map_or(ReplyKind::Reply, |source| source.kind);
            let found = QuickDraft {
                message,
                kind,
                text: text.clone(),
                signature: signature.clone(),
                recipients: Some(composer::Recipients {
                    to: stored.draft.to.clone(),
                    cc: stored.draft.cc.clone(),
                    bcc: stored.draft.bcc.clone(),
                }),
            };
            let mut kept = keeper.kept.borrow_mut();
            let entry = kept.entry(message).or_default();
            if entry.draft.is_none() && entry.is_empty() {
                let held = QuickDraft {
                    message,
                    kind,
                    text,
                    signature,
                    recipients: None,
                };
                entry.draft = Some((stored.id, stored.draft));
                entry.held = Some(held.clone());
                entry.saved = Some(held);
            }
            Some(found)
        })
    }
}

/// Saves what the inline reply holds (`held`) as its draft; empty, discards it. Returns the
/// box's draft after it and whether the text is in it: when the backend failed, it stays in
/// memory and is tried again ([`retry_after`]). The recipients the user chose go into the
/// draft; the quote comes back from the compose rules when it is sent or opened in a composer.
///
/// A draft made for another kind than the reply answers as now (Reply, then `a`) is made again
/// for this one, so its recipients are the compose rules' for it (unless the user chose them)
/// and it says how it answers: after a restart the box comes back as it was left, and never
/// goes to someone Reply took out. The old draft (and its Drafts-folder copy) goes once the new
/// one is saved; a new draft whose save failed goes too, so no open draft is left to reopen in
/// a composer at the next start.
///
/// A draft that is out of the box's hands (`Unsaved::Gone`: sent or discarded from a card, or
/// being sent) is let go: the text goes into a new draft, and emptying the box discards
/// nothing. Before, its save failed forever, every change and every retry tried it again (the
/// retries in a tight loop once 5 s had passed), and a window closing waited for it for good.
async fn keep(
    app: &ProntoApplication,
    held: &QuickDraft,
    draft: Option<(DraftId, Draft)>,
) -> (Option<(DraftId, Draft)>, bool) {
    QUICK_TRIES.with(|tries| tries.set(tries.get().saturating_add(1)));
    let backend = composer::backend(app);
    let message = held.message;
    if held.is_empty() {
        let Some((id, body)) = draft else {
            return (None, true);
        };
        return match backend.discard(id).await {
            Ok(()) | Err(Unsaved::Gone(_)) => (None, true),
            Err(Unsaved::Failed(why)) => {
                tracing::warn!(
                    id = message.0,
                    "the quick reply's draft couldn't be discarded: {why}"
                );
                (Some((id, body)), false)
            }
        };
    }
    let fill = |mut body: Draft| {
        body.html = quick_body(&held.text, held.signature.as_deref());
        if let Some(recipients) = &held.recipients {
            recipients.apply(&mut body);
        }
        body
    };
    // The draft so far, when it answers as the box does now: the text goes into it.
    let stale = match draft {
        Some((id, body)) if answers_as(&body, held.kind) => {
            let body = fill(body);
            match backend.save(id, body.clone()).await {
                Ok(()) => {
                    backend.close(id);
                    return (Some((id, body)), true);
                }
                Err(Unsaved::Gone(why)) => {
                    tracing::info!(
                        id = message.0,
                        "the quick reply's draft is out of its hands ({why}): a new one is made"
                    );
                    None
                }
                Err(Unsaved::Failed(why)) => {
                    tracing::warn!(
                        id = message.0,
                        "the quick reply's draft couldn't be saved: {why}"
                    );
                    return (Some((id, body)), false);
                }
            }
        }
        other => other,
    };
    let request = ComposeRequest::QuickReply {
        message,
        text: String::new(),
        kind: held.kind,
        recipients: None,
        signature: None,
    };
    let prepared = match backend.prepare(request).await {
        Ok(prepared) => prepared,
        Err(why) => {
            tracing::warn!(
                id = message.0,
                "the quick reply's draft couldn't be made: {why}"
            );
            return (stale, false);
        }
    };
    let (id, body) = (prepared.id, fill(prepared.draft));
    if let Err(why) = backend.save(id, body.clone()).await {
        tracing::warn!(
            id = message.0,
            "the quick reply's draft couldn't be saved: {why}"
        );
        let _ = backend.discard(id).await;
        return (stale, false);
    }
    backend.close(id);
    if let Some((old, _)) = stale
        && let Err(Unsaved::Failed(why)) = backend.discard(old).await
    {
        tracing::warn!(
            id = message.0,
            "the quick reply's earlier draft couldn't be discarded: {why}"
        );
    }
    (Some((id, body)), true)
}

/// Whether `draft` was made as a reply of `kind`.
fn answers_as(draft: &Draft, kind: ReplyKind) -> bool {
    draft.source.as_ref().map(|source| source.kind) == Some(kind)
}

/// Keeps an inline draft's editable signature with its text, including explicit removal.
fn quick_body(text: &str, signature: Option<&str>) -> String {
    use sealwax_domain::compose::{EMPTY_LINE, identity};
    let mut html = sealwax_domain::compose::text::to_html(text);
    if let Some(signature) = signature {
        html.push_str(EMPTY_LINE);
        html.push_str(&identity::signature(signature).map_or_else(
            || "<div class=\"sw-signature\"></div>".to_owned(),
            |signature| signature.html,
        ));
    }
    html
}

fn quick_body_parts(html: &str) -> Option<(String, Option<String>)> {
    use sealwax_domain::compose::{EMPTY_LINE, identity::SEPARATOR_HTML};
    let Some((typed, signature)) = html.split_once("<div class=\"sw-signature\">") else {
        return typed_text(html).map(|text| (text, None));
    };
    let text = typed_text(typed.strip_suffix(EMPTY_LINE)?)?;
    let signature = signature.strip_suffix("</div>")?;
    let signature = if signature.is_empty() {
        String::new()
    } else {
        typed_text(signature.strip_prefix(SEPARATOR_HTML)?)?
    };
    Some((text, Some(signature)))
}

/// The plain text of a body that is nothing but the editor's lines (`compose::text::to_html`'s
/// output); `None` for anything else (formatting, a quote, a signature).
pub fn typed_text(html: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let inner = rest.strip_prefix("<div>")?;
        let end = inner.find("</div>")?;
        let line = &inner[..end];
        rest = &inner[end + "</div>".len()..];
        if line == "<br>" {
            lines.push(String::new());
            continue;
        }
        if line.contains('<') {
            return None;
        }
        lines.push(
            line.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .replace("&nbsp;", " ")
                .replace("&amp;", "&"),
        );
    }
    let text = lines.join("\n");
    (sealwax_domain::compose::text::to_html(&text) == html).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, account: i64, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: format!("{role:?}"),
            delimiter: Some('/'),
            role,
            selectable: true,
            inferiors: true,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn a_quick_replys_draft_gives_its_text_back_and_nothing_else() {
        use sealwax_domain::compose::text::to_html;
        for text in [
            "See you Friday!",
            "Two lines\n\nand a gap",
            "  indented <b> & \"quoted\" 'text'",
            "æøå 😀",
        ] {
            assert_eq!(typed_text(&to_html(text)).as_deref(), Some(text), "{text}");
        }
        assert_eq!(typed_text("<div><b>bold</b></div>"), None);
        assert_eq!(
            typed_text("<div>Hi</div><div><br></div><blockquote type=\"cite\">x</blockquote>"),
            None
        );
        assert_eq!(typed_text("<p>Hi</p>"), None);
        assert_eq!(typed_text("<div>a</div>trailing"), None);
        for text in ["", "Reply\nMore"] {
            for signature in [None, Some(""), Some("Ada\n<Research> & Co.")] {
                let html = quick_body(text, signature);
                assert_eq!(
                    quick_body_parts(&html),
                    Some((text.to_owned(), signature.map(str::to_owned))),
                );
            }
        }
    }

    #[test]
    fn a_draft_made_for_another_kind_is_made_again() {
        use sealwax_domain::compose::DraftSource;
        let made = |kind: Option<ReplyKind>| Draft {
            source: kind.map(|kind| DraftSource {
                message: MessageId(6),
                kind,
            }),
            ..Draft::default()
        };
        assert!(answers_as(&made(Some(ReplyKind::Reply)), ReplyKind::Reply));
        assert!(answers_as(
            &made(Some(ReplyKind::ReplyAll)),
            ReplyKind::ReplyAll
        ));
        // Reply, then `a` (and back): the keeper prepares it again for the new kind.
        assert!(!answers_as(
            &made(Some(ReplyKind::Reply)),
            ReplyKind::ReplyAll
        ));
        assert!(!answers_as(
            &made(Some(ReplyKind::ReplyAll)),
            ReplyKind::Reply
        ));
        assert!(!answers_as(&made(None), ReplyKind::Reply));
    }

    #[test]
    fn a_failed_save_waits_longer_each_time() {
        assert_eq!(retry_after(0), Duration::ZERO);
        let waits: Vec<u64> = (1..=7).map(|n| retry_after(n).as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 16, 32, 60, 60]);
        // Never more than a minute, however long the store fails.
        assert_eq!(retry_after(u32::MAX), QUICK_RETRY_AT_MOST);
    }

    #[test]
    fn compose_writes_from_the_account_shown() {
        let folders = [
            folder(1, 1, FolderRole::Inbox),
            folder(2, 1, FolderRole::Drafts),
            folder(3, 2, FolderRole::Inbox),
        ];
        let one = Some(ViewScope::Folder(FolderId(3)));
        assert_eq!(compose_account(one, None, &folders), Some(AccountId(2)));
        assert_eq!(
            compose_account(one, Some(FolderId(1)), &folders),
            Some(AccountId(2))
        );
        let all = Some(ViewScope::UnifiedInbox);
        assert_eq!(
            compose_account(all, Some(FolderId(3)), &folders),
            Some(AccountId(2))
        );
        assert_eq!(compose_account(all, None, &folders), None);
        assert_eq!(compose_account(None, None, &folders), None);
        let gone = Some(ViewScope::Folder(FolderId(9)));
        assert_eq!(compose_account(gone, None, &folders), None);
    }
}
