//! The Split Inbox's commands (the fork's phase C, docs/superhuman/split.md "UI"): the toggle,
//! the sides' keys and go-tos, and the user's side for a sender.
//!
//! - **Split Inbox** turns the sides on or off (`[inbox] split` in config.toml; the toggle is
//!   remembered in layout.toml until config.toml's value changes).
//! - **Next Split** and **Previous Split** (Tab and Shift+Tab in the list, wrapping) move to
//!   the other side with the cursor on its top row; **Go to Important** (and `g i`, Go to
//!   Inbox) and **Go to Other** (`g o`) go to the Inbox's side as it was left, from anywhere.
//! - **Always Important**, **Always Other** and **Reset Sender Split** set or forget the side
//!   of the selected conversation's sender (its exact address; a conversation's first writer
//!   who isn't the user, as the rules take it). The conversation moves at once: it is the
//!   first the store classifies again, and the list moves on as after Archive; the sender's
//!   other mail follows in the store's background parts. A toast says so, with Undo, which `z`
//!   reaches too while the choice is the last action (ADR-015).
//! - The choices the store keeps (earlier runs' too) are read once it is open; until then the
//!   three sender commands don't apply, so none is made, or undone, against a side not known.

use crate::actions::ActionSet;
use crate::application::AppState;
use crate::list::MessageList;
use crate::window::split_tabs::side_name;
use crate::window::{Pane, Window};
use gtk4::glib;
use libadwaita as adw;
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::{FolderRole, ThreadId};
use sealwax_domain::split::{self, Split};
use sealwax_infrastructure::store::{ListRow, RowKey};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// The commands this module runs.
pub const COMMANDS: [ActionId; 8] = [
    ActionId::SplitInbox,
    ActionId::NextSplit,
    ActionId::PreviousSplit,
    ActionId::GoToImportant,
    ActionId::GoToOther,
    ActionId::AlwaysImportant,
    ActionId::AlwaysOther,
    ActionId::ResetSenderSplit,
];

/// A choice the last action made, which Undo puts back: the sender's side before it, and the
/// conversations it was made on (they move back first).
#[derive(Clone, Debug)]
struct Revert {
    address: String,
    before: Option<Split>,
    threads: Vec<ThreadId>,
}

#[derive(Default)]
struct Shared {
    state: RefCell<Weak<AppState>>,
    /// The user's side for each sender, by address as the store keeps it (lower case): read
    /// from the store once it is open, then following the app's own choices at once. `None`
    /// until it is read.
    choices: RefCell<Option<HashMap<String, Split>>>,
    /// The app's choices sent to the store and not answered yet: the store's table is read
    /// again once none is (so a write that failed doesn't leave the map wrong).
    writing: Cell<u32>,
    /// Bumped by every choice the app makes: a read that started before one doesn't undo it.
    generation: Cell<u64>,
    last: RefCell<Option<Revert>>,
    expiry: RefCell<Option<glib::SourceId>>,
}

thread_local! {
    static SHARED: Rc<Shared> = Rc::new(Shared::default());
}

fn shared() -> Rc<Shared> {
    SHARED.with(Rc::clone)
}

fn app_state() -> Option<Rc<AppState>> {
    shared().state.borrow().upgrade()
}

/// Connects the commands on the main window's action set, the tabs' clicks, and the list's
/// sides to the header.
pub fn install(state: &Rc<AppState>, window: &Window, list: &MessageList, actions: &ActionSet) {
    shared().state.replace(Rc::downgrade(state));
    let weak = Rc::downgrade(state);
    actions.connect_toggle(ActionId::SplitInbox, state.splits_enabled(), move |on| {
        if let Some(state) = weak.upgrade() {
            state.choose_splits(on);
        }
    });
    for (id, forward) in [
        (ActionId::NextSplit, true),
        (ActionId::PreviousSplit, false),
    ] {
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
                list.step_split(forward);
            }
        });
    }
    for (id, side) in [
        (ActionId::GoToImportant, Split::Important),
        (ActionId::GoToOther, Split::Other),
    ] {
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            if let Some(state) = weak.upgrade() {
                go_to(&state, side);
            }
        });
    }
    for (id, choice) in [
        (ActionId::AlwaysImportant, Some(Split::Important)),
        (ActionId::AlwaysOther, Some(Split::Other)),
        (ActionId::ResetSenderSplit, None),
    ] {
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            if let Some(state) = weak.upgrade() {
                choose(&state, choice);
            }
        });
    }
    let weak = Rc::downgrade(state);
    window.connect_split_picked(move |side| {
        if let Some(list) = weak.upgrade().and_then(|state| state.list()) {
            list.show_split(side, false);
            list.focus_selected();
        }
    });
    let weak = Rc::downgrade(state);
    list.connect_split_changed(move |side, counts| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        if let (Some(window), Some(list)) = (state.window(), state.list()) {
            window.set_split_tabs(side.map(|side| (side, counts, list.shows_conversations())));
        }
        // Tab goes to the other side from the key on (it moves the focus elsewhere).
        state.refresh_commands();
    });
    // The store opens on a thread of its own, after the window is built: its choices (an
    // earlier run's too) are read once it is open.
    let ready = state.mailbox().wait_ready();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        ready.await;
        if let Some(state) = weak.upgrade() {
            read_choices(&state).await;
        }
    });
    // Until the list shows a side, Tab moves the focus as before.
    refresh(state, actions);
}

/// Reads the user's choices from the store into the map the sender commands go by (their
/// titles, whether they apply, the side Undo puts back); the commands follow. Not while one of
/// the app's own choices is on its way: its answer reads them again, and a choice made while a
/// read was on its way wins over that read.
pub async fn read_choices(state: &Rc<AppState>) {
    let shared = shared();
    if shared.writing.get() > 0 {
        return;
    }
    let generation = shared.generation.get();
    let Some(reply) = state.mailbox().store().map(|store| store.sender_splits()) else {
        return;
    };
    match reply.recv().await {
        Ok(choices) if shared.generation.get() == generation => {
            shared.choices.replace(Some(choices.into_iter().collect()));
            state.refresh_commands();
        }
        Ok(_) => {}
        Err(error) => tracing::warn!("the senders' sides weren't read: {error}"),
    }
}

/// The side the store keeps for `address` (lower case), once the choices are read.
fn stored(address: &str) -> Option<Option<Split>> {
    shared()
        .choices
        .borrow()
        .as_ref()
        .map(|choices| choices.get(address).copied())
}

/// Where Go to Important and Go to Other lead: the side of the Inbox shown, else of the Inbox
/// Go to Inbox shows (opened on that side at once); the list in front.
fn go_to(state: &Rc<AppState>, side: Split) {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return;
    };
    window.show_pane(Pane::List);
    if list.split().is_none() {
        // Another folder: the Inbox `g i` goes to.
        crate::go_to::go_to_inbox(state, side);
    } else {
        if !list.query().is_empty() {
            list.clear_search();
        }
        list.show_split(side, false);
    }
    list.focus_selected();
}

/// The selected conversation's (or message's) sender: its address as the store keeps it, and
/// the conversations to move first.
fn picked(list: &MessageList) -> Option<(String, Vec<RowKey>, Vec<ThreadId>)> {
    let rows = list.selected_rows().ok()?;
    let mut address: Option<String> = None;
    let mut keys = Vec::new();
    let mut threads = Vec::new();
    for row in &rows {
        let sender = split::sender_key(&sender_of(row))?;
        match &address {
            Some(known) if *known != sender => return None,
            _ => address = Some(sender),
        }
        keys.push(row.key());
        threads.push(match row {
            ListRow::Message(row) => row.thread,
            ListRow::Conversation(row) => row.thread,
        });
    }
    Some((address?, keys, threads))
}

/// A row's sender: a message's own, a conversation's first writer who isn't the user (as
/// the rules' sender commands take it).
fn sender_of(row: &ListRow) -> String {
    match row {
        ListRow::Message(row) => row.sender_email.clone(),
        ListRow::Conversation(row) => row
            .participants
            .iter()
            .find(|person| !person.is_me)
            .map_or_else(|| row.sender_email.clone(), |person| person.email.clone()),
    }
}

/// The selected conversation's sender and the side the user chose for it, if any; `None`
/// also until the store's choices are read.
pub fn chosen(state: &AppState) -> Option<(String, Option<Split>)> {
    let list = state.list()?;
    let (address, ..) = picked(&list)?;
    let choice = stored(&address)?;
    Some((address, choice))
}

/// Which of the commands apply now, for the action set.
pub fn refresh(state: &AppState, actions: &ActionSet) {
    let Some(list) = state.list() else {
        return;
    };
    let splits = list.splits_on();
    actions.set_toggle_state(ActionId::SplitInbox, splits);
    // Not in the select mode: its bar hides the tabs, and Tab goes on to it as before.
    let shows = list.shows_splits() && !list.is_select_mode();
    actions.set_applies(ActionId::NextSplit, shows);
    actions.set_applies(ActionId::PreviousSplit, shows);
    let inbox = splits
        && state
            .mailbox()
            .folders()
            .iter()
            .any(|folder| folder.role == FolderRole::Inbox);
    actions.set_applies(ActionId::GoToImportant, inbox);
    actions.set_applies(ActionId::GoToOther, inbox);
    let chosen = chosen(state).filter(|_| splits);
    let can = |side: Split| chosen.as_ref().is_some_and(|(_, now)| *now != Some(side));
    actions.set_applies(ActionId::AlwaysImportant, can(Split::Important));
    actions.set_applies(ActionId::AlwaysOther, can(Split::Other));
    actions.set_applies(
        ActionId::ResetSenderSplit,
        chosen.is_some_and(|(_, now)| now.is_some()),
    );
}

/// The commands' titles with the sender, for the palette: "Always Other: news@example.com".
pub fn titles(state: &AppState) -> Vec<(ActionId, String)> {
    let Some((address, _)) = chosen(state) else {
        return Vec::new();
    };
    [
        ActionId::AlwaysImportant,
        ActionId::AlwaysOther,
        ActionId::ResetSenderSplit,
    ]
    .into_iter()
    .map(|id| (id, format!("{}: {address}", id.title())))
    .collect()
}

/// "Mail from news@example.com now goes to Other".
pub fn toast_text(address: &str, choice: Option<Split>) -> String {
    match choice {
        Some(side) => format!("Mail from {address} now goes to {}", side_name(side)),
        None => format!("Mail from {address} is sorted by its headers again"),
    }
}

/// Always Important, Always Other or Reset Sender Split on the selected conversation's sender.
/// Nothing happens unless it changes the side the store keeps: no toast, and no Undo that would
/// take a stored choice away.
fn choose(state: &Rc<AppState>, choice: Option<Split>) {
    let Some(list) = state.list() else {
        return;
    };
    let Some((address, keys, threads)) = picked(&list) else {
        return;
    };
    let Some(before) = stored(&address) else {
        return;
    };
    if before == choice {
        return;
    }
    // The conversation leaves the side shown: the list moves on, as after Archive.
    if list
        .split()
        .is_some_and(|shown| choice.is_some_and(|choice| choice != shown))
    {
        list.advance_after(&keys);
    }
    apply(state, &address, choice, threads.clone());
    let revert = Revert {
        address,
        before,
        threads,
    };
    remember(state, revert, choice);
}

/// Stores the choice (the store moves `first` at once, then the sender's other mail) and keeps
/// it for the commands; once the store has answered every choice on its way, its table is read
/// again.
fn apply(state: &Rc<AppState>, address: &str, choice: Option<Split>, first: Vec<ThreadId>) {
    let shared = shared();
    if let Some(choices) = shared.choices.borrow_mut().as_mut() {
        match choice {
            Some(side) => choices.insert(address.to_owned(), side),
            None => choices.remove(address),
        };
    }
    shared.generation.set(shared.generation.get() + 1);
    tracing::info!(side = ?choice, "a sender's side was chosen");
    let Some(reply) = state
        .mailbox()
        .store()
        .map(|store| store.set_sender_split_first(address.to_owned(), choice, first))
    else {
        return;
    };
    shared.writing.set(shared.writing.get() + 1);
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        if let Err(error) = reply.recv().await {
            tracing::warn!("a sender's side wasn't stored: {error}");
        }
        let shared = self::shared();
        shared.writing.set(shared.writing.get().saturating_sub(1));
        if let Some(state) = weak.upgrade() {
            read_choices(&state).await;
        }
    });
}

/// The choice becomes the last action: its toast's Undo, `z` and Ctrl+Z take it back within
/// the undo window (ADR-015), as the rules' sender lists do.
fn remember(state: &Rc<AppState>, revert: Revert, choice: Option<Split>) {
    let window = state
        .triage()
        .map_or(crate::triage::UNDO_WINDOW, |triage| triage.window());
    let toast = adw::Toast::builder()
        .title(toast_text(&revert.address, choice))
        .use_markup(false)
        .timeout(window.as_millis().div_ceil(1000).max(1) as u32)
        .priority(adw::ToastPriority::High)
        .button_label("Undo")
        .build();
    if let Some(triage) = state.triage() {
        triage.forget_last();
    }
    crate::outbox::forget_last();
    crate::rules::forget_last();
    let shared = shared();
    shared.last.replace(Some(revert));
    let expiry = glib::timeout_add_local_once(window, || {
        let held = self::shared();
        held.expiry.replace(None);
        held.last.replace(None);
        if let Some(state) = app_state() {
            state.refresh_commands();
        }
    });
    if let Some(old) = shared.expiry.replace(Some(expiry)) {
        old.remove();
    }
    let weak = Rc::downgrade(state);
    toast.connect_button_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            undo_last(&state);
        }
    });
    state.show_toast(&toast, true);
    state.refresh_commands();
}

/// Whether Undo would take back a sender's side: the last action chose one.
pub fn can_undo() -> bool {
    shared().last.borrow().is_some()
}

/// Another action became the last one: the choice's Undo no longer works.
pub fn forget_last() {
    let shared = shared();
    shared.last.replace(None);
    if let Some(expiry) = shared.expiry.take() {
        expiry.remove();
    }
}

/// Undo (`z`, Ctrl+Z, the toast): puts the sender's side back as it was if the last action
/// chose it; `false` leaves Undo to the others.
pub fn undo_last(state: &Rc<AppState>) -> bool {
    let shared = shared();
    let Some(revert) = shared.last.take() else {
        return false;
    };
    if let Some(expiry) = shared.expiry.take() {
        expiry.remove();
    }
    apply(state, &revert.address, revert.before, revert.threads);
    let toast = adw::Toast::builder()
        .title("Undone")
        .use_markup(false)
        .build();
    state.show_toast(&toast, true);
    state.refresh_commands();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{
        AccountId, ConversationSummary, Flags, FolderId, MessageId, MessageSummary, Participant,
    };

    #[test]
    fn a_conversations_sender_is_its_first_writer_who_isnt_the_user() {
        let person = |email: &str, is_me: bool| Participant {
            name: String::new(),
            email: email.to_owned(),
            is_me,
            unread: false,
            last: 0,
        };
        let conversation = ListRow::Conversation(ConversationSummary {
            thread: ThreadId(1),
            account: AccountId(1),
            anchor: MessageId(1),
            folder: FolderId(1),
            last_member: MessageId(1),
            latest: MessageId(1),
            date: 0,
            sender_name: String::new(),
            sender_email: "me@example.test".into(),
            snippet: String::new(),
            subject: String::new(),
            participants: vec![
                person("me@example.test", true),
                person("News@Example.test", false),
            ],
            participant_count: 2,
            messages: 2,
            members: 2,
            unread: 0,
            flagged: 0,
            attachments: 0,
            has_draft: false,
        });
        assert_eq!(sender_of(&conversation), "News@Example.test");
        let message = ListRow::Message(MessageSummary {
            id: MessageId(2),
            folder: FolderId(1),
            thread: ThreadId(2),
            sender_name: String::new(),
            sender_email: "kari@example.test".into(),
            subject: String::new(),
            snippet: String::new(),
            date: 0,
            flags: Flags::NONE,
            has_attachments: false,
            thread_count: 1,
        });
        assert_eq!(sender_of(&message), "kari@example.test");
    }

    #[test]
    fn the_toast_says_where_the_senders_mail_goes() {
        assert_eq!(
            toast_text("news@example.test", Some(Split::Other)),
            "Mail from news@example.test now goes to Other"
        );
        assert_eq!(
            toast_text("kari@example.test", Some(Split::Important)),
            "Mail from kari@example.test now goes to Important"
        );
        assert_eq!(
            toast_text("kari@example.test", None),
            "Mail from kari@example.test is sorted by its headers again"
        );
    }
}
