//! The outbox in the main window (M6 phase B, ML-076): the toast with Undo that follows a draft
//! queued to send, from a composer window or the inline reply, and what the window says about a
//! send that failed or may have gone out.
//!
//! - **Queued** ([`queued`]): one toast, "Sending…" with Undo for the undo window
//!   (`[compose] undo_send_seconds`, 10 s by default; 0 sends at once without Undo, the fork's
//!   phase B); the send becomes the last action, so `z` and Ctrl+Z take it back too while it can
//!   be (ADR-015: only the last action). Undo asks the store (`undo_send`): back to a draft, it
//!   reopens where it was written, in a composer card or the inline reply under its
//!   conversation; too late, it says it is being sent. When the window ends the toast loses its
//!   Undo and stays until the server takes the message: then it says "Message sent".
//! - **Send and Archive** ([`hold`]): the send is queued without a toast of its own; the
//!   archive's toast says it, shows while either can be taken back, and its Undo takes the send
//!   back too ([`undo_send`]); without an undo window, the archive alone.
//! - **Send Later** ([`scheduled`], the fork's phase B): the toast says when ("Sending tomorrow
//!   08:00") with Undo while it shows; the send waits in the outbox until then (its
//!   `not_before`), and Scheduled Messages (`crate::scheduled`) lists it with Edit, which takes it
//!   back to a draft, and Send Now. Its draft's copy in the Drafts folder says when it goes on
//!   its list row ([`scheduled_copies`], the fork's phase C).
//! - **Sent**: the sending toast says so, or a short toast when it is gone. **Failed for good**:
//!   a toast for 10 s, like refused changes, with Open (the store kept the draft and why). **May
//!   have gone out**: the window's banner, whose Choose… asks the user to send it again, file it
//!   as sent, or take it back to a draft.
//! - The offline banner's "N waiting" counts the sends waiting in the outbox too: a scheduled one
//!   from when its time is near.
//!
//! The outbox is read off the GTK thread when the store says it changed. Nothing here logs a
//! recipient, a subject or a text: drafts are named by id.

use crate::application::{AppState, ProntoApplication};
use crate::composer::{self, ComposeRequest};
use crate::engine_link::Subscription;
use crate::reader::QuickDraft;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::model::{AccountId, BlobKey, DraftId, MessageId};
use sealwax_engine::api::{Command, Event, SendError, UncertainReason};
use sealwax_infrastructure::store::{
    OutboxEntry, Resolution, SendQueued, SendRecorded, SendState, SendUndone,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::Duration;

/// How long a failed send's toast shows (as refused changes').
pub const FAILED_SHOWN: Duration = Duration::from_secs(10);
/// How long "Message sent" shows on a toast of its own.
pub const SENT_SHOWN: Duration = Duration::from_secs(4);
/// How long the sending toast says "Message sent" once the server took it.
pub const SENT_SAID: Duration = Duration::from_secs(2);
/// How long the sending toast waits for the server after the undo window: then it goes (the
/// offline banner counts the send as waiting all along).
pub const SENDING_GRACE: Duration = Duration::from_secs(8);

/// The sending toast's text (Superhuman's; "Message queued" until the fork's phase B).
pub const SENDING_TEXT: &str = "Sending…";
/// How long a scheduled send's toast shows, with its Undo.
pub const SCHEDULED_SHOWN: Duration = Duration::from_secs(10);
/// A send that goes later than this after it was queued is a scheduled one (Send Later), not one
/// in the undo window (30 s at most): Scheduled Messages lists it until its time is this near,
/// and from then on the offline banner counts it as waiting.
pub const SCHEDULED_AFTER: Duration = Duration::from_secs(60);
/// The longest the outbox's wake-up for a scheduled send's time sleeps at once (a suspend stops
/// GLib's clock, not the wall clock's): it looks again after that.
const DUE_NAP: Duration = Duration::from_secs(15 * 60);
pub const SENT_TEXT: &str = "Message sent";
/// The toast of an undone send, wherever its draft went back (under its conversation or in a
/// card, where the user sees it): short (phase C's visual QA: "Sending undone: the reply is back
/// under its conversation" ran across the reply's footer).
pub const UNDONE_TEXT: &str = "Send undone";

/// What the uncertain-send dialog's answers do, by response id.
pub const ANSWERS: [(&str, &str); 3] = [
    ("back-to-draft", "Back to Drafts"),
    ("was-sent", "It Was Sent"),
    ("send-again", "Send Again"),
];

#[derive(Default)]
struct Outbox {
    state: RefCell<Weak<AppState>>,
    /// The send Undo takes back while it is the last action.
    last: Cell<Option<DraftId>>,
    expiry: RefCell<Option<glib::SourceId>>,
    /// The sending toast on screen and its send.
    sending: RefCell<Option<(DraftId, adw::Toast)>>,
    /// Sends queued for Send and Archive: no toast of their own ([`hold`]); once queued, their
    /// account.
    held: RefCell<HashSet<DraftId>>,
    held_queued: RefCell<HashMap<DraftId, AccountId>>,
    /// The self-tests' undo window, in place of `[compose] undo_send_seconds`.
    window: Cell<Option<Duration>>,
    /// Sends that came from the inline reply: what it held, for Undo.
    from_quick: RefCell<HashMap<DraftId, QuickDraft>>,
    /// Why each send that may have gone out is uncertain, as the engine said this session;
    /// one from an earlier session has none.
    reasons: RefCell<HashMap<DraftId, UncertainReason>>,
    /// The outbox as last read.
    entries: RefCell<Vec<OutboxEntry>>,
    /// Wakes when the soonest scheduled send's time is near ([`SCHEDULED_AFTER`]).
    due: RefCell<Option<glib::SourceId>>,
    reading: Cell<bool>,
    read_again: Cell<bool>,
    events: RefCell<Option<Subscription>>,
    /// When each scheduled send goes (Unix ms), by its draft's copies in a Drafts folder, as
    /// last read ([`mark_copies`]): the list says so on their rows.
    copies: RefCell<HashMap<MessageId, i64>>,
    reading_copies: Cell<bool>,
    read_copies_again: Cell<bool>,
}

thread_local! {
    static OUTBOX: Rc<Outbox> = Rc::default();
}

fn shared() -> Rc<Outbox> {
    OUTBOX.with(Rc::clone)
}

fn now_ms() -> i64 {
    glib::real_time() / 1000
}

fn app() -> Option<ProntoApplication> {
    gio::Application::default().and_downcast::<ProntoApplication>()
}

/// Follows the engine's sends and the store's outbox, once at startup.
pub fn install(state: &Rc<AppState>) {
    let outbox = shared();
    outbox.state.replace(Rc::downgrade(state));
    let weak = Rc::downgrade(state);
    let subscription = state.engine().subscribe(move |event| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        match event {
            Event::Sent { draft, .. } => sent(&state, *draft),
            Event::SendFailed {
                account,
                draft,
                error,
            } => failed(&state, *account, *draft, error),
            Event::SendUncertain { draft, reason, .. } => {
                tracing::info!(draft = draft.0, ?reason, "a send may have gone out");
                if let Some(toast) = sending_toast(*draft) {
                    toast.dismiss();
                }
                shared().reasons.borrow_mut().insert(*draft, *reason);
                settle(&state, *draft);
                read(&state);
            }
            _ => {}
        }
    });
    outbox.events.replace(Some(subscription));
    let weak = Rc::downgrade(state);
    state.mailbox().connect_event(move |event| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        if !event.outbox.is_empty() {
            read(&state);
        } else if !(event.inserted.is_empty() && event.removed.is_empty())
            && !scheduled_sends().is_empty()
        {
            // A scheduled draft's copy may have come into its Drafts folder (or gone).
            mark_copies(&state);
        }
    });
    let ready = state.mailbox().wait_ready();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        ready.await;
        if let Some(state) = weak.upgrade() {
            read(&state);
        }
    });
}

/// Reads the outbox again, at most one read at a time, and updates the banner.
fn read(state: &Rc<AppState>) {
    let outbox = shared();
    if outbox.reading.replace(true) {
        outbox.read_again.set(true);
        return;
    }
    let Some(reply) = state.mailbox().store().map(|store| store.outbox(None)) else {
        outbox.reading.set(false);
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let entries = reply.recv().await;
        let outbox = shared();
        outbox.reading.set(false);
        match entries {
            Ok(entries) => {
                outbox.entries.replace(entries);
            }
            Err(error) => tracing::warn!(%error, "the outbox couldn't be read"),
        }
        let Some(state) = weak.upgrade() else {
            return;
        };
        if outbox.read_again.replace(false) {
            read(&state);
        }
        state.update_banner();
        wake_when_due(&state);
        mark_copies(&state);
    });
}

/// Reads which copies in a Drafts folder the scheduled sends' drafts have, and has the list say
/// when each goes on its row ("Sending Tue 08:00"); one read at a time. A draft never uploaded
/// has none; a send no longer scheduled (sent, taken back, due within the minute) loses its mark.
fn mark_copies(state: &Rc<AppState>) {
    let outbox = shared();
    if outbox.reading_copies.replace(true) {
        outbox.read_copies_again.set(true);
        return;
    }
    let goes: HashMap<DraftId, i64> = scheduled_sends()
        .into_iter()
        .map(|entry| (entry.draft, entry.not_before))
        .collect();
    let reply = if goes.is_empty() {
        None
    } else {
        let mailbox = state.mailbox();
        let store = mailbox.store();
        store.map(|store| store.draft_copies(goes.keys().copied().collect()))
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let copies = match reply {
            Some(reply) => match reply.recv().await {
                Ok(copies) => Some(
                    copies
                        .into_iter()
                        .filter_map(|(draft, copy)| Some((copy, *goes.get(&draft)?)))
                        .collect::<HashMap<_, _>>(),
                ),
                Err(error) => {
                    tracing::warn!(%error, "the scheduled drafts' copies couldn't be read");
                    None
                }
            },
            None => Some(HashMap::new()),
        };
        let outbox = shared();
        outbox.reading_copies.set(false);
        let Some(state) = weak.upgrade() else {
            return;
        };
        if let Some(copies) = copies
            && *outbox.copies.borrow() != copies
        {
            outbox.copies.replace(copies.clone());
            if let Some(list) = state.list() {
                list.set_scheduled(copies);
            }
        }
        if outbox.read_copies_again.replace(false) {
            mark_copies(&state);
        }
    });
}

/// The scheduled sends' copies in a Drafts folder and when each goes (Unix ms), as last read:
/// a list made after the read starts from them.
pub fn scheduled_copies() -> HashMap<MessageId, i64> {
    shared().copies.borrow().clone()
}

/// Whether `entry` is a send scheduled for later (Send Later) at `now` (Unix ms): queued, never
/// tried, and due in more than [`SCHEDULED_AFTER`]. A send in its undo window, or waiting to be
/// tried again, isn't.
pub fn is_scheduled(entry: &OutboxEntry, now: i64) -> bool {
    entry.state == SendState::Queued
        && entry.attempts == 0
        && entry.not_before > now + SCHEDULED_AFTER.as_millis() as i64
}

/// Sends waiting for a server, for the offline banner's count: every one being sent or queued
/// (in its undo window too, as before the fork's phase B), except one scheduled for later until
/// its time is near ([`wake_when_due`] counts it then).
pub fn waiting_sends() -> u32 {
    let now = now_ms();
    shared()
        .entries
        .borrow()
        .iter()
        .filter(|entry| {
            matches!(entry.state, SendState::Queued | SendState::Sending)
                && !is_scheduled(entry, now)
        })
        .count() as u32
}

/// Wakes when the soonest scheduled send's time is near, as the outbox was last read: from then
/// on it is waiting like any send, so the banner counts it (offline, it can't go) and Scheduled
/// Messages no longer lists it. Nothing else would say so: the outbox is read again only when the
/// store changes it, and the engine sends only while it is connected.
fn wake_when_due(state: &Rc<AppState>) {
    let outbox = shared();
    if let Some(waiting) = outbox.due.take() {
        waiting.remove();
    }
    let now = now_ms();
    let soonest = outbox
        .entries
        .borrow()
        .iter()
        .filter(|entry| is_scheduled(entry, now))
        .map(|entry| entry.not_before - SCHEDULED_AFTER.as_millis() as i64)
        .min();
    let Some(soonest) = soonest else {
        return;
    };
    // A millisecond past it, so the entry is no longer scheduled when it wakes.
    let wait = Duration::from_millis(u64::try_from(soonest - now).unwrap_or(0) + 1).min(DUE_NAP);
    let weak = Rc::downgrade(state);
    let source = glib::timeout_add_local_once(wait, move || {
        // Its source is done: forgotten, not removed.
        shared().due.replace(None);
        if let Some(state) = weak.upgrade() {
            state.update_banner();
            crate::scheduled::refresh(&state);
            wake_when_due(&state);
            mark_copies(&state);
        }
    });
    outbox.due.replace(Some(source));
}

/// The sends scheduled for later ([`is_scheduled`]), soonest first, as the outbox was last read.
pub fn scheduled_sends() -> Vec<OutboxEntry> {
    let now = now_ms();
    let mut entries: Vec<OutboxEntry> = shared()
        .entries
        .borrow()
        .iter()
        .filter(|entry| is_scheduled(entry, now))
        .cloned()
        .collect();
    entries.sort_by_key(|entry| entry.not_before);
    entries
}

/// When `draft` is scheduled to go, when it is a scheduled send ([`scheduled_sends`]).
pub fn scheduled_at(draft: DraftId) -> Option<i64> {
    scheduled_sends()
        .into_iter()
        .find(|entry| entry.draft == draft)
        .map(|entry| entry.not_before)
}

/// "Sending tomorrow 08:00": the scheduled send's toast, `when` said by
/// `time_picker::describe`.
pub fn scheduled_text(when: &str) -> String {
    format!("Sending {when}")
}

/// A send that may have gone out (issue #2): its draft and account, why when the engine said
/// it this session, and what names it for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unconfirmed {
    pub draft: DraftId,
    pub account: AccountId,
    pub reason: Option<UncertainReason>,
    pub subject: String,
    /// When its send started (Unix ms).
    pub started: Option<i64>,
}

/// The sends that may have gone out, oldest first.
pub fn uncertain() -> Vec<Unconfirmed> {
    let outbox = shared();
    let reasons = outbox.reasons.borrow();
    outbox
        .entries
        .borrow()
        .iter()
        .filter(|entry| entry.state == SendState::Uncertain)
        .map(|entry| Unconfirmed {
            draft: entry.draft,
            account: entry.account,
            reason: reasons.get(&entry.draft).copied(),
            subject: entry.subject.clone(),
            started: entry.started,
        })
        .collect()
}

/// The outbox as last read, for checks.
pub fn entries() -> Vec<OutboxEntry> {
    shared().entries.borrow().clone()
}

/// A send from the inline reply: Undo puts what it held back under its conversation.
pub fn from_quick_reply(draft: DraftId, quick: QuickDraft) {
    shared().from_quick.borrow_mut().insert(draft, quick);
}

/// How long a send waits while Undo can take it back: `[compose] undo_send_seconds`, or the
/// self-tests' window.
pub fn send_window(state: &AppState) -> Duration {
    if let Some(window) = shared().window.get() {
        return window;
    }
    let seconds = state
        .preferences()
        .map_or(sealwax_domain::config::DEFAULT_UNDO_SEND_SECONDS, |prefs| {
            prefs.config().compose.undo_send_seconds
        });
    Duration::from_secs(u64::from(seconds))
}

/// Self-tests: sends wait `window` (the triage window the script set), or the setting again.
pub fn set_send_window(window: Option<Duration>) {
    shared().window.set(window);
}

/// Send and Archive: `draft`'s send is queued without a toast of its own; [`take_held`] says
/// whether it was.
pub fn hold(draft: DraftId) {
    shared().held.borrow_mut().insert(draft);
}

/// The account `draft` was queued from while held; `None` when it wasn't queued (the backend
/// refused it, or queues nothing, as phase A's stand-in). It is no longer held.
pub fn take_held(draft: DraftId) -> Option<AccountId> {
    let outbox = shared();
    outbox.held.borrow_mut().remove(&draft);
    outbox.held_queued.borrow_mut().remove(&draft)
}

/// The send left the outbox's hands: it no longer goes back to the quick reply, nor is it the
/// one Undo takes back.
fn settle(state: &AppState, draft: DraftId) {
    let outbox = shared();
    outbox.from_quick.borrow_mut().remove(&draft);
    if outbox.last.get() == Some(draft) {
        outbox.last.set(None);
        state.refresh_commands();
    }
}

/// A refused send of the quick reply: forget it.
pub fn forget_quick(draft: DraftId) {
    shared().from_quick.borrow_mut().remove(&draft);
}

/// `draft` was queued to send with the undo window (the backend's `send` calls it after the
/// store's `queue_send` and `Command::OutboxQueued`): the sending toast and the last action, or
/// nothing yet for a held send (Send and Archive).
pub fn queued(state: &AppState, draft: DraftId, account: AccountId) {
    tracing::info!(draft = draft.0, account = account.0, "send queued");
    let outbox = shared();
    if outbox.held.borrow().contains(&draft) {
        outbox.held_queued.borrow_mut().insert(draft, account);
        return;
    }
    announce(state, draft);
}

/// The sending toast for `draft`: "Sending…" with Undo for the undo window, while the send is
/// the last action; then it waits for the server without Undo ([`SENDING_GRACE`] at most).
/// Without a window, no Undo.
pub fn announce(state: &AppState, draft: DraftId) {
    let outbox = shared();
    let window = send_window(state);
    let undoable = !window.is_zero();
    if undoable {
        if let Some(triage) = state.triage() {
            triage.forget_last();
        }
        crate::rules::forget_last();
        crate::split_inbox::forget_last();
        outbox.last.set(Some(draft));
    }
    let expiry = glib::timeout_add_local_once(window, move || {
        let outbox = shared();
        outbox.expiry.replace(None);
        if outbox.last.get() == Some(draft) {
            outbox.last.set(None);
            if let Some(state) = outbox.state.borrow().upgrade() {
                state.refresh_commands();
            }
        }
        // The window is over: no more Undo, and the toast waits a while for the server.
        let shown = outbox
            .sending
            .borrow()
            .as_ref()
            .filter(|(shown, _)| *shown == draft)
            .map(|(_, toast)| toast.clone());
        if let Some(toast) = shown {
            toast.set_button_label(None);
            glib::timeout_add_local_once(SENDING_GRACE, move || {
                if toast.title().as_deref() == Some(SENDING_TEXT) {
                    toast.dismiss();
                }
            });
        }
    });
    if let Some(old) = outbox.expiry.replace(Some(expiry)) {
        old.remove();
    }
    let toast = adw::Toast::builder()
        .title(SENDING_TEXT)
        .use_markup(false)
        .timeout(0)
        .priority(adw::ToastPriority::High)
        .build();
    if undoable {
        toast.set_button_label(Some("Undo"));
        toast.connect_button_clicked(move |_| {
            if let Some(state) = shared().state.borrow().upgrade() {
                undo(&state, draft);
            }
        });
    }
    toast.connect_dismissed(move |toast| {
        let outbox = shared();
        let mut sending = outbox.sending.borrow_mut();
        if sending.as_ref().is_some_and(|(_, shown)| shown == toast) {
            sending.take();
        }
    });
    if let Some((_, old)) = outbox.sending.replace(Some((draft, toast.clone()))) {
        old.dismiss();
    }
    state.show_toast(&toast, true);
    state.refresh_commands();
}

/// Send Later: `draft` waits in the outbox until `at` (Unix ms; the backend queued it with that
/// `not_before`). One toast says when, with Undo while it shows ([`SCHEDULED_SHOWN`]), and the
/// send is the last action meanwhile, so `z` takes it back too; after that, Scheduled Messages
/// does. Nothing waits for the server here: "Message sent" says it when it goes.
pub fn scheduled(state: &AppState, draft: DraftId, account: AccountId, at: i64) {
    tracing::info!(draft = draft.0, account = account.0, "send scheduled");
    let outbox = shared();
    if let Some(triage) = state.triage() {
        triage.forget_last();
    }
    crate::rules::forget_last();
    crate::split_inbox::forget_last();
    outbox.last.set(Some(draft));
    let toast = toast(
        &scheduled_text(&crate::time_picker::describe(at)),
        SCHEDULED_SHOWN,
    );
    toast.set_button_label(Some("Undo"));
    // Its Undo works while the send waits, whatever came after it.
    toast.connect_button_clicked(move |_| {
        if let Some(state) = shared().state.borrow().upgrade() {
            unschedule(&state, draft);
        }
    });
    toast.connect_dismissed(move |_| {
        let outbox = shared();
        if outbox.last.get() == Some(draft) {
            outbox.last.set(None);
            if let Some(state) = outbox.state.borrow().upgrade() {
                state.refresh_commands();
            }
        }
    });
    state.show_toast(&toast, true);
    state.refresh_commands();
}

/// Scheduled Messages' Edit: takes the scheduled `draft` back to a draft, which opens where it
/// was written (a composer card, or the inline reply this session).
pub fn unschedule(state: &AppState, draft: DraftId) {
    let outbox = shared();
    if outbox.last.get() == Some(draft) {
        outbox.last.set(None);
        state.refresh_commands();
    }
    take_back(state, draft, true);
}

/// Scheduled Messages' Send Now: the scheduled `draft` goes back to a draft and is queued again
/// to go at once (after the undo window, with the sending toast).
pub fn send_now(state: &AppState, draft: DraftId) {
    let Some(undone) = state.mailbox().store().map(|store| store.undo_send(draft)) else {
        return;
    };
    let weak = shared().state.borrow().clone();
    glib::spawn_future_local(async move {
        let Ok(SendUndone::Undone { orphaned_blobs, .. }) = undone.recv().await else {
            if let Some(state) = weak.upgrade() {
                state.show_toast(
                    &toast(
                        "Too late to change: it is being sent",
                        Duration::from_secs(4),
                    ),
                    true,
                );
                read(&state);
            }
            return;
        };
        let Some(state) = weak.upgrade() else {
            return;
        };
        remove_blobs(&state, orphaned_blobs);
        shared().from_quick.borrow_mut().remove(&draft);
        let window = send_window(&state).as_millis() as i64;
        let Some(queueing) = state
            .mailbox()
            .store()
            .map(|store| store.queue_send(draft, now_ms() + window))
        else {
            return;
        };
        match queueing.recv().await {
            Ok(SendQueued::Queued { account, .. }) => {
                wake(&state, Command::OutboxQueued { account });
                queued(&state, draft, account);
            }
            _ => {
                if let Some(app) = app() {
                    composer::open(&app, ComposeRequest::Draft(draft));
                }
            }
        }
        read(&state);
    });
}

/// The sending toast of `draft`, while it shows.
fn sending_toast(draft: DraftId) -> Option<adw::Toast> {
    shared()
        .sending
        .borrow()
        .as_ref()
        .filter(|(shown, _)| *shown == draft)
        .map(|(_, toast)| toast.clone())
}

fn toast(text: &str, shown: Duration) -> adw::Toast {
    adw::Toast::builder()
        .title(text)
        .use_markup(false)
        .timeout(shown.as_millis().div_ceil(1000).max(1) as u32)
        .priority(adw::ToastPriority::High)
        .build()
}

/// Whether Undo would take back a send: the last action is one still in its window.
pub fn can_undo() -> bool {
    shared().last.get().is_some()
}

/// Another action became the last one: the send's Undo no longer works.
pub fn forget_last() {
    shared().last.set(None);
}

/// Undo (`z`, Ctrl+Z, the palette): takes back the last send if the last action is one;
/// `false` leaves Undo to triage.
pub fn undo_last(state: &AppState) -> bool {
    match shared().last.get() {
        Some(draft) => {
            undo(state, draft);
            true
        }
        None => false,
    }
}

/// Takes `draft` back from the outbox, if it is still the last action.
fn undo(state: &AppState, draft: DraftId) {
    let outbox = shared();
    if outbox.last.get() != Some(draft) {
        return;
    }
    outbox.last.set(None);
    state.refresh_commands();
    take_back(state, draft, true);
}

/// Send and Archive's Undo: the archive is the last action, and its Undo takes `draft`'s send
/// back with it; the archive's own toast says it ("Undone"), so this one says nothing unless it
/// came too late.
pub fn undo_send(state: &AppState, draft: DraftId) {
    if shared().last.get() == Some(draft) {
        shared().last.set(None);
        state.refresh_commands();
    }
    take_back(state, draft, false);
}

/// Asks the store for `draft` back; it reopens where it was written. With `say`, a toast says
/// so.
fn take_back(state: &AppState, draft: DraftId, say: bool) {
    let outbox = shared();
    if let Some(toast) = sending_toast(draft) {
        toast.dismiss();
    }
    let Some(reply) = state.mailbox().store().map(|store| store.undo_send(draft)) else {
        return;
    };
    let weak = outbox.state.borrow().clone();
    glib::spawn_future_local(async move {
        let answer = reply.recv().await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let text = match answer {
            Ok(SendUndone::Undone {
                account,
                orphaned_blobs,
                ..
            }) => {
                tracing::info!(draft = draft.0, "send undone");
                remove_blobs(&state, orphaned_blobs);
                wake(&state, Command::OutboxQueued { account });
                let text = reopen(&state, draft);
                if !say {
                    return;
                }
                text
            }
            Ok(SendUndone::TooLate(SendState::Sent)) => {
                "Too late to undo: the message was sent".to_owned()
            }
            Ok(SendUndone::TooLate(SendState::Uncertain)) => {
                "Too late to undo: the message may have been sent".to_owned()
            }
            Ok(SendUndone::TooLate(_)) => "Too late to undo: the message is being sent".to_owned(),
            Ok(SendUndone::NotQueued) => "The message wasn't waiting to be sent".to_owned(),
            Err(error) => {
                tracing::warn!(%error, "a send couldn't be undone");
                "Sending couldn't be undone".to_owned()
            }
        };
        state.show_toast(&toast(&text, Duration::from_secs(4)), true);
    });
}

/// An undone send goes back where it was written: what the inline reply held back under its
/// conversation (its draft, with the quote, is discarded: the inline reply keeps its own; when
/// that box holds another draft, or no window shows, it opens in a composer card), or its
/// draft to a composer card. Returns the toast's text.
fn reopen(state: &Rc<AppState>, draft: DraftId) -> String {
    let quick = shared().from_quick.borrow_mut().remove(&draft);
    match quick {
        Some(quick) => {
            if let Some(reply) = state
                .mailbox()
                .store()
                .map(|store| store.discard_draft(draft))
            {
                let weak = Rc::downgrade(state);
                glib::spawn_future_local(async move {
                    if let (Ok(discarded), Some(state)) = (reply.recv().await, weak.upgrade()) {
                        remove_blobs(&state, discarded.orphaned_blobs);
                        if let (true, Some(account)) = (discarded.server_copy, discarded.account) {
                            wake(&state, Command::DraftsChanged { account });
                        }
                    }
                });
            }
            match state.reader() {
                Some(reader) => {
                    reader.restore_quick_reply(quick);
                }
                None => {
                    if let Some(app) = app() {
                        composer::open(
                            &app,
                            ComposeRequest::QuickReply {
                                message: quick.message,
                                text: quick.text,
                                kind: quick.kind,
                                recipients: quick.recipients,
                                signature: quick.signature,
                            },
                        );
                    }
                }
            }
            UNDONE_TEXT.to_owned()
        }
        None => {
            if let Some(app) = app() {
                composer::open(&app, ComposeRequest::Draft(draft));
            }
            UNDONE_TEXT.to_owned()
        }
    }
}

fn wake(state: &AppState, command: Command) {
    if state.engine().send(command).is_err() {
        tracing::debug!("no engine to tell about the outbox");
    }
}

/// Deletes blobs nothing references any more, off the GTK thread.
fn remove_blobs(state: &AppState, keys: Vec<BlobKey>) {
    if keys.is_empty() {
        return;
    }
    let Some(blobs) = state.mailbox().blobs() else {
        return;
    };
    gio::spawn_blocking(move || {
        for key in keys {
            if let Err(error) = blobs.remove(&key) {
                tracing::warn!(%error, "a blob nothing needs couldn't be removed");
            }
        }
    });
}

/// The server took `draft`: its sending toast says so, or a toast of its own when that went.
fn sent(state: &Rc<AppState>, draft: DraftId) {
    tracing::info!(draft = draft.0, "sent");
    settle(state, draft);
    match sending_toast(draft) {
        Some(toast) => {
            toast.set_title(SENT_TEXT);
            toast.set_button_label(None);
            glib::timeout_add_local_once(SENT_SAID, move || toast.dismiss());
        }
        None => state.show_toast(&toast(SENT_TEXT, SENT_SHOWN), false),
    }
    read(state);
}

/// What a failed send's toast says (never the recipients or the subject).
pub fn failure_text(error: &SendError) -> String {
    let why = match error {
        SendError::RecipientsRefused(refused) if refused.len() == 1 => {
            "the server refused 1 recipient".to_owned()
        }
        SendError::RecipientsRefused(refused) => {
            format!("the server refused {} recipients", refused.len())
        }
        SendError::TooLarge { limit: Some(limit) } => format!(
            "it is larger than the server takes ({})",
            glib::format_size(*limit)
        ),
        SendError::TooLarge { limit: None } => "it is larger than the server takes".to_owned(),
        SendError::Refused => "the server refused it".to_owned(),
        SendError::NeedsSignIn => "the account needs you to sign in again".to_owned(),
        SendError::Offline => "the mail server can't be reached".to_owned(),
        SendError::Interrupted => "Pronto stopped while preparing it".to_owned(),
        SendError::Store | SendError::Other => String::new(),
    };
    if why.is_empty() {
        "Your message wasn't sent. It's back in your drafts.".to_owned()
    } else {
        format!("Your message wasn't sent: {why}. It's back in your drafts.")
    }
}

/// The server refused `draft` for good: the store put it back as a draft that says why.
fn failed(state: &Rc<AppState>, account: AccountId, draft: DraftId, error: &SendError) {
    tracing::info!(draft = draft.0, account = account.0, ?error, "send failed");
    settle(state, draft);
    if let Some(toast) = sending_toast(draft) {
        toast.dismiss();
    }
    let toast = toast(&failure_text(error), FAILED_SHOWN);
    toast.set_button_label(Some("Open"));
    toast.connect_button_clicked(move |_| {
        if let Some(app) = app() {
            composer::open(&app, ComposeRequest::Draft(draft));
        }
    });
    state.show_toast(&toast, false);
    read(state);
}

/// The banner's Choose… for the first send that may have gone out: send it again, file it as
/// sent, or take it back to a draft.
pub fn decide(state: &Rc<AppState>) {
    let Some(Unconfirmed {
        draft,
        account,
        reason,
        subject,
        started,
    }) = uncertain().into_iter().next()
    else {
        return;
    };
    let Some(window) = state.window() else {
        return;
    };
    let name = state
        .mailbox()
        .accounts()
        .into_iter()
        .find(|known| known.id == account)
        .map(|known| known.name)
        .unwrap_or_default();
    let dialog = adw::AlertDialog::builder()
        .heading("Was the message sent?")
        .body(decide_text(
            &described(&subject, &name, started, &now()),
            reason,
        ))
        .close_response("later")
        .default_response("later")
        .build();
    dialog.set_widget_name("uncertain-send");
    dialog.add_response("later", "Later");
    for (id, label) in ANSWERS {
        dialog.add_response(id, label);
    }
    dialog.set_response_appearance("send-again", adw::ResponseAppearance::Destructive);
    let weak = Rc::downgrade(state);
    dialog.connect_response(None, move |_, response| {
        let resolution = match response {
            "send-again" => Resolution::SendAgain {
                not_before: now_ms(),
            },
            "was-sent" => Resolution::WasSent,
            "back-to-draft" => Resolution::BackToDraft,
            _ => return,
        };
        if let Some(state) = weak.upgrade() {
            resolve(&state, draft, account, resolution);
        }
    });
    dialog.present(Some(window.window()));
}

/// What names a send that may have gone out: its subject, the account (`account` empty:
/// unnamed) and when it started, as "“Quarterly report” from Work (today at 14:32)".
pub fn described(
    subject: &str,
    account: &str,
    started: Option<i64>,
    now: &glib::DateTime,
) -> String {
    let subject = crate::notify::clean(subject, SUBJECT_CHARS);
    let mut text = if subject.is_empty() {
        "a message with no subject".to_owned()
    } else {
        format!("\u{201c}{subject}\u{201d}")
    };
    if !account.is_empty() {
        text.push_str(&format!(" from {account}"));
    }
    if let Some(when) = started.and_then(|ms| when_text(ms / 1000, now)) {
        text.push_str(&format!(" ({when})"));
    }
    text
}

/// The subject's length in the banner and the question; longer ones end in "…".
const SUBJECT_CHARS: usize = 60;

/// "today at 14:32", "yesterday at 09:05", "on 4 Oct at 14:32", "on 04.10.2025 at 14:32".
fn when_text(secs: i64, now: &glib::DateTime) -> Option<String> {
    let time = glib::DateTime::from_unix_local(secs).ok()?;
    let clock = time.format("%H:%M").ok()?;
    let yesterday = now.add_days(-1).ok()?;
    Some(if time.ymd() == now.ymd() {
        format!("today at {clock}")
    } else if time.ymd() == yesterday.ymd() {
        format!("yesterday at {clock}")
    } else {
        let day = time
            .format(if time.year() == now.year() {
                "%e %b"
            } else {
                "%d.%m.%Y"
            })
            .ok()?;
        format!("on {} at {clock}", day.trim())
    })
}

/// The local time now, for [`described`].
pub fn now() -> glib::DateTime {
    glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("a clock"))
}

/// What happened to `what` ([`described`]), a send that may have gone out, by why its outcome
/// is unknown; `None` when the engine said it in an earlier session (issue #2).
pub fn uncertain_cause(what: &str, reason: Option<UncertainReason>) -> String {
    match reason {
        Some(UncertainReason::Stopped) => {
            format!("Pronto stopped while it was sending {what}, so the server may have it.")
        }
        Some(UncertainReason::NoReply) => {
            format!("The server didn't answer when Pronto sent {what}, so it may have it.")
        }
        Some(UncertainReason::UnreadableReply) => format!(
            "Pronto couldn't read the server's answer to {what}, so it can't tell whether the \
             server took it."
        ),
        None => format!("Pronto couldn't confirm that {what} was sent."),
    }
}

/// The banner's text for `what`, a send that may have gone out.
pub fn uncertain_banner(what: &str, reason: Option<UncertainReason>) -> String {
    format!(
        "{} Look in your Sent folder, or ask a recipient, before you send it again.",
        uncertain_cause(what, reason)
    )
}

/// The dialog's text: what happened, how to tell, and what each answer does.
pub fn decide_text(what: &str, reason: Option<UncertainReason>) -> String {
    format!(
        "{} Look in your Sent folder, or ask a recipient.\n\nSend Again sends the same message, \
         which its recipients may then get twice. It Was Sent files it as sent. Back to Drafts \
         opens it, to change or send it later.",
        uncertain_cause(what, reason)
    )
}

fn resolve(state: &Rc<AppState>, draft: DraftId, account: AccountId, resolution: Resolution) {
    let Some(reply) = state
        .mailbox()
        .store()
        .map(|store| store.resolve_uncertain(draft, resolution, now_ms()))
    else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let answer = reply.recv().await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let recorded: SendRecorded = match answer {
            Ok(recorded) => recorded,
            Err(error) => {
                tracing::warn!(%error, "an uncertain send couldn't be resolved");
                state.show_toast(
                    &toast("Pronto couldn't record your answer", Duration::from_secs(4)),
                    false,
                );
                return;
            }
        };
        tracing::info!(
            draft = draft.0,
            applied = recorded.applied,
            "uncertain send resolved"
        );
        remove_blobs(&state, recorded.orphaned_blobs.clone());
        if !recorded.applied {
            state.show_toast(
                &toast(
                    "That message was already dealt with",
                    Duration::from_secs(4),
                ),
                false,
            );
            read(&state);
            return;
        }
        let text = match resolution {
            Resolution::SendAgain { .. } => {
                wake(&state, Command::OutboxQueued { account });
                Some("Sending it again")
            }
            Resolution::WasSent => {
                wake(&state, Command::OutboxQueued { account });
                Some("Filed as sent")
            }
            Resolution::BackToDraft => {
                if let Some(app) = app() {
                    composer::open(&app, ComposeRequest::Draft(draft));
                }
                None
            }
        };
        if recorded.ops_queued {
            wake(&state, Command::OpsQueued { account });
        }
        if recorded.copies_to_delete {
            wake(&state, Command::DraftsChanged { account });
        }
        if let Some(text) = text {
            state.show_toast(&toast(text, Duration::from_secs(4)), false);
        }
        read(&state);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_send_says_why_without_its_recipients() {
        let refused = SendError::RecipientsRefused(vec![
            "kari@example.test".into(),
            "ola@example.test".into(),
        ]);
        let text = failure_text(&refused);
        assert_eq!(
            text,
            "Your message wasn't sent: the server refused 2 recipients. It's back in your drafts."
        );
        assert!(!text.contains("example.test"));
        assert_eq!(
            failure_text(&SendError::RecipientsRefused(vec!["a@example.test".into()])),
            "Your message wasn't sent: the server refused 1 recipient. It's back in your drafts."
        );
        // GLib may put a no-break space between the number and the unit.
        assert!(
            failure_text(&SendError::TooLarge {
                limit: Some(26_214_400)
            })
            .contains("(26.2")
        );
        assert!(failure_text(&SendError::TooLarge { limit: None }).contains("larger"));
        assert_eq!(
            failure_text(&SendError::Other),
            "Your message wasn't sent. It's back in your drafts."
        );
        assert_eq!(
            failure_text(&SendError::Interrupted),
            "Your message wasn't sent: Pronto stopped while preparing it. It's back in your drafts."
        );
    }

    #[test]
    fn the_uncertain_question_names_the_message_and_each_answer() {
        let what = "\u{201c}Plans\u{201d} from Work";
        let text = decide_text(what, Some(UncertainReason::Stopped));
        assert!(
            text.starts_with(
                "Pronto stopped while it was sending \u{201c}Plans\u{201d} from Work,"
            )
        );
        for (_, label) in ANSWERS {
            assert!(text.contains(label), "{label}");
        }
    }

    #[test]
    fn the_uncertain_cause_says_why_only_when_it_is_known() {
        let what = "\u{201c}Plans\u{201d} from Work";
        assert_eq!(
            uncertain_cause(what, Some(UncertainReason::NoReply)),
            "The server didn't answer when Pronto sent \u{201c}Plans\u{201d} from Work, so it may have it."
        );
        assert_eq!(
            uncertain_cause(what, Some(UncertainReason::UnreadableReply)),
            "Pronto couldn't read the server's answer to \u{201c}Plans\u{201d} from Work, so it can't \
             tell whether the server took it."
        );
        assert_eq!(
            uncertain_cause(what, None),
            "Pronto couldn't confirm that \u{201c}Plans\u{201d} from Work was sent."
        );
        for reason in [None, Some(UncertainReason::UnreadableReply)] {
            assert!(!uncertain_cause(what, reason).contains("stopped"));
        }
    }

    #[test]
    fn a_send_is_named_by_its_subject_account_and_time() {
        let now = glib::DateTime::from_local(2026, 10, 11, 15, 0, 0.0).unwrap();
        let at = |year, month, day, hour| {
            let time = glib::DateTime::from_local(year, month, day, hour, 0, 0.0).unwrap();
            Some(time.to_unix() * 1000)
        };
        assert_eq!(
            described("Quarterly report", "Work", at(2026, 10, 11, 14), &now),
            "\u{201c}Quarterly report\u{201d} from Work (today at 14:00)"
        );
        assert_eq!(
            described("  ", "Work", at(2026, 10, 10, 19), &now),
            "a message with no subject from Work (yesterday at 19:00)"
        );
        assert_eq!(described("Hi", "", None, &now), "\u{201c}Hi\u{201d}");
        let this_year = described("Hi", "Work", at(2026, 9, 11, 15), &now);
        assert!(
            this_year.contains("(on ") && this_year.ends_with(" at 15:00)"),
            "{this_year}"
        );
        assert!(
            described("Hi", "Work", at(2025, 9, 6, 15), &now).ends_with("(on 06.09.2025 at 15:00)")
        );
        let long = described(&"word ".repeat(40), "Work", None, &now);
        assert!(long.chars().count() < 90, "{long}");
    }
}
