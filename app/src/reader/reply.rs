//! Replying from the reader (M6: ML-068, ML-069; DESIGN.md "Reader" and "Composer", the approved
//! mock; the fork's Superhuman pass, phase B). Each expanded card's header ends with Reply, Reply
//! all and Forward (a draft's card with Edit draft instead), acting on that card's message. Under
//! the conversation sits the inline reply: a quiet one-line box saying whom it answers ("Reply to
//! Tomas Berg…") that opens when it takes the focus or holds text, into its recipients line (a
//! button that shows the To, Cc and Bcc fields), a plain-text area ("Write a reply…"), Send
//! (Ctrl+Enter, its keys beside it) and Pop Out. It answers the latest message of the
//! conversation that isn't the user's own ([`reply_target`]), or the one the user chose
//! (`n`/`p`, a click; the message whose row they opened while the list's rows are messages; its
//! line names that one at once: [`user_message`]), or the message the user replies to from its
//! card, as Reply or Reply All (Enter and the box itself as `[reader] reply` says: everyone by
//! default since phase C); once it holds text it keeps answering that message whatever
//! arrives. Quoted history isn't shown: the compose rules add it
//! when the reply is sent or popped out.
//!
//! What a box holds (its text, how it answers, whom the user chose) is kept per message it
//! answers while Pronto runs, so a conversation laid out again gets it back as it was (and the
//! app keeps it as a draft, made again when it answers another way). The box is the Composer
//! scope: Ctrl+Enter runs `win.send`, Ctrl+Shift+Enter `win.send-and-archive`, Ctrl+Shift+P
//! pops it out; the reader's own keys stop at its edge (a text field keeps its letters, and
//! Ctrl+Z, Ctrl+J or Ctrl+R never reach the mail commands from it), except Open Reply in Composer
//! (Ctrl+Shift+E). Escape in an empty box closes it and gives the focus back to where the reply
//! started; in one with text it leaves the draft for the conversation. Every window opens through
//! [`ComposeRequest`]s the app hands to `composer::open`; Send goes through a seam the app sets
//! ([`Reader::set_quick_send`]).

use super::conversation::{Item, Page};
use super::{Inner, Reader, tooltip};
use crate::composer::recipients::RecipientField;
use crate::composer::{ComposeRequest, Recipients};
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use sealwax_domain::account::Identity;
use sealwax_domain::actions::ActionId;
use sealwax_domain::compose::reply as compose_reply;
use sealwax_domain::compose::{ReplyKind, address};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{Address, FolderRole, Headers, MessageId, MessageSummary};
use sealwax_infrastructure::store::ConversationMessage;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// The text area's placeholder (the mock's).
pub const PLACEHOLDER: &str = "Write a reply…";

/// A card button: the command it stands for, its icons and its accessible label.
type Spec = (ActionId, &'static [&'static str], &'static str);

const REPLY_ICONS: &[&str] = &["mail-reply-sender-symbolic"];
const REPLY_ALL_ICONS: &[&str] = &["mail-reply-all-symbolic", "mail-reply-sender-symbolic"];
const POP_OUT_ICONS: &[&str] = &[
    "view-fullscreen-symbolic",
    "window-maximize-symbolic",
    "go-up-symbolic",
];

/// The cards' buttons (the mock's "Reply", "Reply all", "Forward"): they answer as `r` and `a`
/// do, whatever Enter's reply is (`[reader] reply`; upstream's issue #8 turned the first into
/// Reply all and the second into Reply to sender with `all`, the fork's default since phase C).
const REPLIES: [Spec; 3] = [
    (ActionId::Reply, REPLY_ICONS, "Reply"),
    (ActionId::ReplyAll, REPLY_ALL_ICONS, "Reply all"),
    (ActionId::Forward, &["mail-forward-symbolic"], "Forward"),
];

/// A draft's card offers Edit draft instead: replying to one's own unsent draft makes no sense.
const EDIT: Spec = (
    ActionId::EditDraft,
    &["document-edit-symbolic", "mail-message-new-symbolic"],
    "Edit draft",
);

/// A card button's accessible label.
fn button_label(action: ActionId) -> &'static str {
    REPLIES
        .iter()
        .chain([&EDIT])
        .find(|(id, _, _)| *id == action)
        .map_or(action.title(), |(_, _, label)| label)
}

/// The user's addresses as the compose rules take them.
fn identities(mine: &[String]) -> Vec<Identity> {
    mine.iter()
        .map(|address| Identity {
            name: String::new(),
            address: address.clone(),
            signature: None,
        })
        .collect()
}

/// Whom a reply of `kind` to a message with `headers` goes to, To then Cc, `mine` being the
/// user's addresses: the compose rules' own choice (`compose::reply::recipients`).
pub fn planned(headers: &Headers, kind: ReplyKind, mine: &[String]) -> Recipients {
    let (to, cc) = compose_reply::recipients(headers, kind, &identities(mine));
    Recipients {
        to,
        cc,
        bcc: Vec::new(),
    }
}

/// Whether `a` and `b` send to the same addresses in To, Cc and Bcc, whatever their names and
/// order: a draft's recipients that are the compose rules' plan aren't the user's choice.
pub fn same_people(a: &Recipients, b: &Recipients) -> bool {
    let emails = |list: &[Address]| {
        let mut emails: Vec<String> = list
            .iter()
            .map(|address| address.email.trim().to_lowercase())
            .collect();
        emails.sort();
        emails
    };
    emails(&a.to) == emails(&b.to)
        && emails(&a.cc) == emails(&b.cc)
        && emails(&a.bcc) == emails(&b.bcc)
}

/// The recipients line: "Reply to Tomas Berg", "Reply to Tomas Berg and 2 others"; "Reply" for
/// no one.
pub fn recipients_label(recipients: &[Address]) -> String {
    let Some(first) = recipients.first() else {
        return "Reply".to_owned();
    };
    let name = first.display_name();
    match recipients.len() - 1 {
        0 => format!("Reply to {name}"),
        1 => format!("Reply to {name} and 1 other"),
        rest => format!("Reply to {name} and {rest} others"),
    }
}

/// The message a quick reply answers, of `messages` oldest first: the latest that isn't the
/// user's own (`mine`, lower case) and isn't in a Drafts folder; when every message is the
/// user's own, the latest that isn't a draft; `None` when there is only drafts.
pub fn reply_target(messages: &[ConversationMessage], mine: &[String]) -> Option<MessageId> {
    let candidates: Vec<&ConversationMessage> = messages
        .iter()
        .filter(|message| message.role != FolderRole::Drafts)
        .collect();
    candidates
        .iter()
        .rev()
        .find(|message| !is_mine(&message.summary, mine))
        .or(candidates.last())
        .map(|message| message.summary.id)
}

/// The message of `page` the user chose, when it isn't a draft: the one they moved to (`n`/`p`,
/// a press in it, Tab into it: `Page::current`), else, with `message_rows` (the list's rows are
/// messages, Group by Conversation off), the selected one, whose row they picked. The inline
/// reply answers it rather than its own target, and Reply, Reply All and Forward act on it
/// (phase C's review: with a row per message, Forward from Tomas's older row sent his latest).
fn user_message(page: &Page, message_rows: bool) -> Option<MessageId> {
    let chosen = |id: MessageId| {
        page.item(id)
            .filter(|item| item.message.role != FolderRole::Drafts)
            .map(Item::id)
    };
    page.current
        .and_then(chosen)
        .or_else(|| message_rows.then(|| chosen(page.selected)).flatten())
}

fn is_mine(summary: &MessageSummary, mine: &[String]) -> bool {
    mine.iter()
        .any(|address| address.eq_ignore_ascii_case(summary.sender_email.trim()))
}

/// "Reply to Tomas Berg" (the sender's name, else the address); just "Reply" for the user's own
/// message, whose reply goes to its recipients: the label until the message's headers are read.
pub fn reply_label(summary: &MessageSummary, mine: &[String]) -> String {
    if is_mine(summary, mine) {
        return "Reply".to_owned();
    }
    let name = summary.sender_name.trim();
    let name = if name.is_empty() {
        summary.sender_email.trim()
    } else {
        name
    };
    if name.is_empty() {
        "Reply".to_owned()
    } else {
        format!("Reply to {name}")
    }
}

/// Pop Out's tooltip: "Pop out to the composer (Ctrl+Shift+P)", with the keys in use.
pub fn pop_out_tooltip(keymap: &Keymap) -> String {
    tooltip(
        keymap,
        ActionId::PopOutDraft,
        "Pop out to the composer",
        None,
    )
}

/// The recipients button's tooltip: the keys that edit To, Cc and Bcc.
fn recipients_tooltip(keymap: &Keymap) -> String {
    tooltip(keymap, ActionId::EditTo, "Edit recipients", None)
}

/// What the window's commands need to know of the quick reply on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuickState {
    /// A conversation with a quick reply is on screen.
    pub shown: bool,
    /// It holds text to send.
    pub has_text: bool,
}

/// What an inline reply holds: the message it answers, how, its text, and the recipients the
/// user chose in place of the compose rules' (`None`: theirs). `Debug` leaves the text out.
#[derive(Clone, PartialEq, Eq)]
pub struct QuickDraft {
    pub message: MessageId,
    pub kind: ReplyKind,
    pub text: String,
    pub recipients: Option<Recipients>,
    /// The visible signature; an empty string explicitly removes it, `None` uses the account's.
    pub signature: Option<String>,
}

impl QuickDraft {
    /// An explicitly edited signature is a draft even before a message body is written.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.signature.is_none()
    }
}

impl fmt::Debug for QuickDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuickDraft")
            .field("message", &self.message)
            .field("kind", &self.kind)
            .field("text_len", &self.text.len())
            .field("recipients", &self.recipients)
            .finish()
    }
}

/// What became of a reply card's draft popped back in ([`Reader::pop_in`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopIn {
    /// It is under its conversation, focused.
    Done,
    /// The reader doesn't show its conversation.
    NotShown,
    /// The reply under its conversation holds another draft.
    Busy,
}

/// How the inline reply's Send goes: Send (Ctrl+Enter), Send and Archive (Ctrl+Shift+Enter,
/// Superhuman's Send + Mark Done), or Send Later (Ctrl+Shift+L) at a time (Unix ms).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
    Send,
    SendAndArchive,
    Later(i64),
}

/// The quick reply on screen, for checks. `Debug` leaves the text and names out: the user's
/// mail.
#[derive(Clone, PartialEq, Eq)]
pub struct QuickReport {
    pub target: MessageId,
    pub label: String,
    /// What the closed box says ("Reply to Tomas Berg…"): whom Enter and a click answer.
    pub placeholder: String,
    pub text: String,
    pub focused: bool,
    pub send_sensitive: bool,
    pub kind: ReplyKind,
    pub open: bool,
    /// The recipients' fields show.
    pub editing: bool,
    /// The keyboard focus is anywhere in it: its text or its fields.
    pub holds_focus: bool,
    /// Where its caret is, in characters from the start of its text.
    pub caret: usize,
    /// Whom it goes to now, as planned or as chosen; `None` until the target's headers are
    /// read.
    pub recipients: Option<Recipients>,
    pub chosen: bool,
}

impl fmt::Debug for QuickReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuickReport")
            .field("target", &self.target)
            .field("text_len", &self.text.len())
            .field("focused", &self.focused)
            .field("send_sensitive", &self.send_sensitive)
            .field("kind", &self.kind)
            .field("open", &self.open)
            .finish()
    }
}

type ComposeListener = Rc<dyn Fn(ComposeRequest)>;
type StateListener = Rc<dyn Fn(QuickState)>;
/// A future that resolves on the GTK thread.
pub type Local<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T>>>;
/// Sends an inline reply as a reply to its message; `true` when it was taken (queued), `false`
/// when it was refused (the app said why), so the text goes back into the box.
pub type QuickSend = Rc<dyn Fn(QuickDraft, SendMode) -> Local<bool>>;
/// Hears every change of an inline reply (its text, how it answers, its recipients), to keep it
/// as a draft.
type TextListener = Rc<dyn Fn(&QuickDraft)>;
/// Finds what a quick reply held for a message in an earlier session (its draft): its text,
/// how it answered and whom its draft goes to (`recipients`: the draft's own, which the box
/// keeps when they aren't what the compose rules plan).
pub type QuickFinder = Rc<dyn Fn(MessageId) -> Local<Option<QuickDraft>>>;
type SavedReplyOpener = Rc<dyn Fn(MessageId) -> bool>;

type SignatureProvider = Rc<dyn Fn(&MessageSummary, Option<&Headers>) -> Option<String>>;

/// A reply the user asked for before its conversation was on screen (`r` right after `j`): the
/// selected message whose conversation it is under, the message to answer and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wanted {
    selected: MessageId,
    message: Option<MessageId>,
    kind: ReplyKind,
}

/// The reader's side of replying: what the quick replies hold, and where requests go.
#[derive(Default)]
pub struct Quick {
    /// What each message's quick reply holds while it holds text: the text, how it answers and
    /// the recipients the user chose, so a conversation laid out again (or a send taken back
    /// while another shows) gets its reply back as it was ([`Inner::hold`]).
    drafts: RefCell<HashMap<MessageId, QuickDraft>>,
    on_compose: RefCell<Vec<ComposeListener>>,
    send: RefCell<Option<QuickSend>>,
    on_state: RefCell<Vec<StateListener>>,
    state: Cell<QuickState>,
    on_text: RefCell<Vec<TextListener>>,
    finder: RefCell<Option<QuickFinder>>,
    open_saved: RefCell<Option<SavedReplyOpener>>,
    signature: RefCell<Option<SignatureProvider>>,
    wanted: Cell<Option<Wanted>>,
    /// Asks when Send Later sends (the app's time picker), then `send_quick_reply_at`.
    later: RefCell<Option<Rc<dyn Fn()>>>,
}

/// The To, Cc and Bcc fields of an inline reply, made when the user first edits them.
struct Fields {
    to: Rc<RecipientField>,
    cc: Rc<RecipientField>,
    bcc: Rc<RecipientField>,
}

/// One conversation's inline reply.
pub struct QuickReply {
    pub root: gtk::Box,
    /// The recipients line ("Reply to Tomas Berg"): a click shows the fields.
    heading: gtk::Button,
    label: gtk::Label,
    fields_box: gtk::Box,
    fields: RefCell<Option<Fields>>,
    view: gtk::TextView,
    signature: gtk::TextView,
    signature_area: gtk::Box,
    signature_separator: gtk::Label,
    signature_override: RefCell<Option<String>>,
    signature_provider: Option<SignatureProvider>,
    placeholder: gtk::Label,
    send: gtk::Button,
    send_keys: gtk::Label,
    pop: gtk::Button,
    /// Send, its keys and Pop Out, shown while the box is open.
    actions: gtk::Box,
    target: Cell<MessageId>,
    kind: Cell<ReplyKind>,
    /// The target's headers, once read: whom each kind answers.
    headers: RefCell<Option<Headers>>,
    /// The target's sender as the list knows it: the label until the headers are read.
    summary: RefCell<MessageSummary>,
    /// The user's addresses, for whom a reply leaves out.
    mine: RefCell<Vec<String>>,
    /// The recipients the user put in the fields, in place of the compose rules'.
    chosen: RefCell<Option<Recipients>>,
    /// An earlier session's draft: how it answered and whom it went to, until the headers say
    /// whether those were the plan's or the user's choice ([`QuickReply::restore_recipients`]).
    restored: RefCell<Option<(ReplyKind, Recipients)>>,
    /// The fields are being filled by the box itself: no edit.
    filling: Cell<bool>,
    /// Its text is on its way to the send seam.
    sending: Cell<bool>,
    /// Send's question is on screen ([`Inner::check_quick_send`]): a second Send asks no
    /// second one.
    asking: Cell<bool>,
    /// Where the focus was when a command brought it in (`r`, `a`, Enter): Escape in the empty
    /// box takes it back there.
    return_focus: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    /// Hears the user change the recipients (the reader tells the draft keeper).
    on_change: RefCell<Option<Rc<dyn Fn()>>>,
}

/// What the closed quick reply says: "Reply to Tomas Berg…".
pub fn closed_text(label: &str) -> String {
    format!("{label}…")
}

impl QuickReply {
    fn new(
        target: MessageId,
        summary: &MessageSummary,
        kind: ReplyKind,
        text: &str,
        keymap: &Keymap,
        mine: &[String],
        signature_provider: Option<SignatureProvider>,
    ) -> Rc<Self> {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sw-quick-reply"])
            .build();
        root.set_widget_name("quick-reply");
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["sw-small", "sw-secondary", "sw-quick-reply-label"])
            .build();
        label.set_widget_name("quick-reply-label");
        let heading_box = gtk::Box::builder().spacing(4).build();
        heading_box.append(&label);
        heading_box.append(&icon(&["pan-down-symbolic"]));
        let heading = gtk::Button::builder()
            .child(&heading_box)
            .halign(gtk::Align::Start)
            .css_classes(["flat", "sw-quick-reply-heading"])
            .build();
        heading.set_widget_name("quick-reply-recipients");
        root.append(&heading);
        let fields_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .css_classes(["sw-quick-reply-fields"])
            .build();
        fields_box.set_widget_name("quick-reply-fields");
        root.append(&fields_box);
        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .hexpand(true)
            .css_classes(["sw-reading", "sw-quick-reply-text"])
            .build();
        view.set_widget_name("quick-reply-text");
        view.buffer().set_text(text);
        view.update_property(&[
            gtk::accessible::Property::Placeholder(PLACEHOLDER),
            gtk::accessible::Property::MultiLine(true),
        ]);
        let placeholder = gtk::Label::builder()
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .can_target(false)
            .visible(text.is_empty())
            .css_classes(["sw-reading", "sw-secondary", "sw-quick-reply-placeholder"])
            .build();
        placeholder.set_widget_name("quick-reply-placeholder");
        placeholder.set_accessible_role(gtk::AccessibleRole::Presentation);
        let area = gtk::Overlay::builder().child(&view).build();
        area.add_overlay(&placeholder);
        root.append(&area);
        let signature = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .hexpand(true)
            .css_classes(["sw-reading", "sw-quick-reply-text"])
            .build();
        signature.set_widget_name("quick-reply-signature");
        signature.update_property(&[
            gtk::accessible::Property::Label("Signature"),
            gtk::accessible::Property::MultiLine(true),
        ]);
        let signature_area = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_top(12)
            .visible(false)
            .build();
        let signature_separator = gtk::Label::builder().label("-- ").xalign(0.0).build();
        signature_area.append(&signature_separator);
        signature_area.append(&signature);
        root.append(&signature_area);
        // Send in the primary fill with its label alone; its keys sit beside it, quiet, and
        // Pop Out at the far end (the QA of phase A: no hint sentence, no keys in the button).
        let send = gtk::Button::builder()
            .label("Send")
            .valign(gtk::Align::Center)
            .action_name(ActionId::Send.detailed_name())
            .css_classes(["sw-quick-reply-send"])
            .build();
        send.set_widget_name("quick-reply-send");
        send.update_property(&[gtk::accessible::Property::Label("Send")]);
        let send_keys = gtk::Label::builder()
            .valign(gtk::Align::Center)
            .css_classes(["sw-small", "sw-secondary", "sw-quick-reply-send-keys"])
            .build();
        send_keys.set_widget_name("quick-reply-send-keys");
        send_keys.set_accessible_role(gtk::AccessibleRole::Presentation);
        let pop = gtk::Button::builder()
            .child(&icon(POP_OUT_ICONS))
            .valign(gtk::Align::Center)
            .action_name(ActionId::PopOutDraft.detailed_name())
            .css_classes(["flat", "sw-icon-button", "sw-quick-reply-pop"])
            .build();
        pop.set_widget_name("quick-reply-expand");
        pop.update_property(&[gtk::accessible::Property::Label("Pop out to the composer")]);
        let spacer = gtk::Box::builder().hexpand(true).build();
        let row = gtk::Box::builder()
            .css_classes(["sw-quick-reply-actions"])
            .build();
        row.append(&send);
        row.append(&send_keys);
        row.append(&spacer);
        row.append(&pop);
        root.append(&row);
        root.update_property(&[gtk::accessible::Property::Label("Quick reply")]);
        let quick = Rc::new(Self {
            root,
            heading,
            label,
            fields_box,
            fields: RefCell::new(None),
            view,
            signature,
            signature_area,
            signature_separator,
            signature_override: RefCell::new(None),
            signature_provider,
            placeholder,
            send,
            send_keys,
            pop,
            actions: row,
            target: Cell::new(target),
            kind: Cell::new(kind),
            headers: RefCell::new(None),
            summary: RefCell::new(summary.clone()),
            mine: RefCell::new(mine.to_vec()),
            chosen: RefCell::new(None),
            restored: RefCell::new(None),
            filling: Cell::new(false),
            sending: Cell::new(false),
            asking: Cell::new(false),
            return_focus: RefCell::new(None),
            on_change: RefCell::new(None),
        });
        quick.update_keys(keymap);
        quick.relabel();
        quick.load_signature();
        quick.set_open(!text.is_empty());
        let weak = Rc::downgrade(&quick);
        quick.signature.buffer().connect_changed(move |_| {
            if let Some(quick) = weak.upgrade().filter(|quick| !quick.filling.get()) {
                quick
                    .signature_override
                    .replace(Some(quick.signature_text()));
                quick
                    .signature_separator
                    .set_visible(!quick.signature_text().trim().is_empty());
                quick.tell_change();
            }
        });
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&quick);
        focus.connect_contains_focus_notify(move |focus| {
            if let Some(quick) = weak.upgrade() {
                quick.set_open(focus.contains_focus() || quick.has_draft() || quick.editing());
            }
        });
        quick.root.add_controller(focus);
        let weak = Rc::downgrade(&quick);
        quick.heading.connect_clicked(move |_| {
            if let Some(quick) = weak.upgrade() {
                quick.edit_recipients("to");
            }
        });
        quick
    }

    /// Opens the box (its recipients line, room to write, Send and Pop Out) or closes it to its
    /// one quiet line.
    fn set_open(&self, open: bool) {
        if open && !self.is_open() {
            self.load_signature();
        }
        if open {
            self.root.add_css_class("sw-quick-reply-open");
        } else {
            self.root.remove_css_class("sw-quick-reply-open");
        }
        self.heading.set_visible(open && !self.editing());
        self.actions.set_visible(open);
        self.signature_area.set_visible(
            open && (!self.signature_text().is_empty()
                || self.signature_override.borrow().is_some()),
        );
        let placeholder = if open {
            PLACEHOLDER.to_owned()
        } else {
            closed_text(&self.label.label())
        };
        self.placeholder.set_label(&placeholder);
    }

    pub(super) fn is_open(&self) -> bool {
        self.root.has_css_class("sw-quick-reply-open")
    }

    pub fn target(&self) -> MessageId {
        self.target.get()
    }

    pub fn kind(&self) -> ReplyKind {
        self.kind.get()
    }

    pub fn text(&self) -> String {
        let buffer = self.view.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn has_text(&self) -> bool {
        !self.text().trim().is_empty()
    }

    fn has_draft(&self) -> bool {
        self.has_text() || self.signature_override.borrow().is_some()
    }

    /// Where the caret is, in characters from the start of the text.
    fn caret(&self) -> usize {
        let buffer = self.view.buffer();
        usize::try_from(buffer.iter_at_mark(&buffer.get_insert()).offset()).unwrap_or(0)
    }

    fn set_text(&self, text: &str) {
        self.view.buffer().set_text(text);
    }

    fn signature_text(&self) -> String {
        let buffer = self.signature.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn load_signature(&self) {
        let signature = self.signature_override.borrow().clone().or_else(|| {
            self.signature_provider.as_ref().and_then(|provider| {
                provider(&self.summary.borrow(), self.headers.borrow().as_ref())
            })
        });
        self.filling.set(true);
        self.signature
            .buffer()
            .set_text(signature.as_deref().unwrap_or_default());
        self.filling.set(false);
        self.signature_separator.set_visible(
            signature
                .as_ref()
                .is_some_and(|text| !text.trim().is_empty()),
        );
        self.signature_area
            .set_visible(self.is_open() && signature.is_some());
    }

    /// What it holds, for the send seam and the draft keeper.
    fn draft(&self) -> QuickDraft {
        QuickDraft {
            message: self.target(),
            kind: self.kind(),
            text: self.text(),
            recipients: self.chosen.borrow().clone(),
            signature: self.signature_override.borrow().clone().or_else(|| {
                let text = self.signature_text();
                // An untouched default alone must not create a draft for every conversation.
                (self.has_text() && !text.is_empty()).then_some(text)
            }),
        }
    }

    /// Whom it goes to now: the chosen recipients, else the compose rules' for its kind once
    /// the target's headers are read.
    fn recipients(&self) -> Option<Recipients> {
        if let Some(chosen) = self.chosen.borrow().clone() {
            return Some(chosen);
        }
        let mine = self.mine.borrow();
        self.headers
            .borrow()
            .as_ref()
            .map(|headers| planned(headers, self.kind(), &mine))
    }

    /// The recipients line and the closed box's text follow whom it goes to.
    fn relabel(&self) {
        let label = match self.recipients() {
            Some(recipients) => {
                let all: Vec<Address> = recipients
                    .to
                    .iter()
                    .chain(&recipients.cc)
                    .chain(&recipients.bcc)
                    .cloned()
                    .collect();
                recipients_label(&all)
            }
            None => reply_label(&self.summary.borrow(), &self.mine.borrow()),
        };
        self.label.set_label(&label);
        self.view
            .update_property(&[gtk::accessible::Property::Label(&label)]);
        if !self.is_open() {
            self.placeholder.set_label(&closed_text(&label));
        }
    }

    /// Answers `target` (its `summary`) as `kind` from now on; the chosen recipients go, and
    /// the headers until they are read for it.
    fn retarget(&self, target: MessageId, summary: &MessageSummary, kind: ReplyKind) {
        if self.target.replace(target) != target {
            self.headers.replace(None);
            self.summary.replace(summary.clone());
        }
        self.kind.set(kind);
        self.chosen.replace(None);
        self.restored.replace(None);
        self.fill_fields();
        self.relabel();
    }

    /// An earlier session's draft answered as `kind`, to `stored`: whom the user chose stays
    /// chosen (a recipient they took out doesn't come back), whom the compose rules plan stays
    /// planned (Reply All still follows). Told apart once the headers are read
    /// ([`QuickReply::settle_restored`]).
    fn restore_recipients(&self, kind: ReplyKind, stored: Option<Recipients>) {
        self.kind.set(kind);
        self.restored.replace(
            stored
                .filter(|stored| !stored.is_empty())
                .map(|stored| (kind, stored)),
        );
        self.settle_restored();
        self.fill_fields();
        self.relabel();
    }

    /// With the headers read, restored recipients become the box's choice when they aren't the
    /// compose rules' plan for how their draft answered (names and order aside; the user may
    /// answer another way by now). `true` when they did.
    fn settle_restored(&self) -> bool {
        let chosen = {
            let headers = self.headers.borrow();
            let Some(headers) = headers.as_ref() else {
                return false;
            };
            let Some((kind, stored)) = self.restored.take() else {
                return false;
            };
            let plan = planned(headers, kind, &self.mine.borrow());
            (self.chosen.borrow().is_none() && !same_people(&plan, &stored)).then_some(stored)
        };
        let settled = chosen.is_some();
        if settled {
            self.chosen.replace(chosen);
        }
        settled
    }

    /// Reply or Reply All from now on: the recipients follow unless the user chose them.
    fn set_kind(&self, kind: ReplyKind) {
        if self.kind.replace(kind) != kind && self.chosen.borrow().is_none() {
            self.fill_fields();
        }
        self.relabel();
    }

    /// The target's headers are read: whom each kind answers. Restored recipients that were the
    /// user's choice become it now, a change of the draft.
    fn set_headers(&self, headers: Headers) {
        self.headers.replace(Some(headers));
        self.load_signature();
        let chose = self.settle_restored();
        if chose || self.chosen.borrow().is_none() {
            self.fill_fields();
        }
        self.relabel();
        if chose {
            self.tell_change();
        }
    }

    /// Tells the reader the user's draft changed (it keeps it).
    fn tell_change(&self) {
        let listener = self.on_change.borrow().clone();
        if let Some(listener) = listener {
            listener();
        }
    }

    /// Puts what `draft` holds in the box (its message is the box's target): how it answers,
    /// the recipients the user chose, its text.
    fn take(&self, draft: &QuickDraft) {
        self.kind.set(draft.kind);
        self.restored.replace(None);
        self.chosen.replace(draft.recipients.clone());
        self.signature_override.replace(draft.signature.clone());
        self.load_signature();
        self.fill_fields();
        self.relabel();
        self.set_text(&draft.text);
        self.tell_change();
    }

    fn editing(&self) -> bool {
        self.fields_box.is_visible()
    }

    /// A snippet's recipients join whom it goes to (the user's choice from then on).
    fn add_recipients(&self, add: &Recipients) {
        if add.is_empty() {
            return;
        }
        let mut recipients = self.recipients().unwrap_or_default();
        let join = |list: &mut Vec<Address>, more: &[Address]| {
            *list = address::dedupe(list.iter().chain(more).cloned());
        };
        join(&mut recipients.to, &add.to);
        join(&mut recipients.cc, &add.cc);
        join(&mut recipients.bcc, &add.bcc);
        self.chosen.replace(Some(recipients));
        self.fill_fields();
        self.relabel();
        self.tell_change();
    }

    /// Puts `text` in at the caret, over a selection, the caret `cursor` characters into it
    /// (else after it).
    fn insert_at_caret(&self, text: &str, cursor: Option<usize>) {
        let buffer = self.view.buffer();
        buffer.delete_selection(true, true);
        let start = buffer.iter_at_mark(&buffer.get_insert()).offset();
        buffer.insert_at_cursor(text);
        if let Some(cursor) = cursor.and_then(|cursor| i32::try_from(cursor).ok()) {
            buffer.place_cursor(&buffer.iter_at_offset(start + cursor));
        }
        self.view.scroll_mark_onscreen(&buffer.get_insert());
    }

    /// The fields hold whom it goes to now (planned, until the user changes them).
    fn fill_fields(&self) {
        let fields = self.fields.borrow();
        let Some(fields) = fields.as_ref() else {
            return;
        };
        let recipients = self.recipients().unwrap_or_default();
        self.filling.set(true);
        fields.to.set_addresses(&recipients.to);
        fields.cc.set_addresses(&recipients.cc);
        fields.bcc.set_addresses(&recipients.bcc);
        self.filling.set(false);
    }

    /// Shows the To, Cc and Bcc fields, filled, the focus in `field` (`to`, `cc`, `bcc`).
    fn edit_recipients(self: &Rc<Self>, field: &str) {
        if self.fields.borrow().is_none() {
            let make = |label: &str, name: &str| {
                let field = RecipientField::new(label, name);
                self.fields_box.append(field.widget());
                let weak = Rc::downgrade(self);
                field.connect_changed(move || {
                    if let Some(quick) = weak.upgrade() {
                        quick.fields_changed();
                    }
                });
                field
            };
            let fields = Fields {
                to: make("To", "quick-reply-to"),
                cc: make("Cc", "quick-reply-cc"),
                bcc: make("Bcc", "quick-reply-bcc"),
            };
            self.fields.replace(Some(fields));
            self.fill_fields();
        }
        self.fields_box.set_visible(true);
        self.set_open(true);
        if let Some(fields) = self.fields.borrow().as_ref() {
            let target = match field {
                "cc" => &fields.cc,
                "bcc" => &fields.bcc,
                _ => &fields.to,
            };
            target.entry().grab_focus();
        }
    }

    /// Before a send or Pop Out: what is typed in the fields becomes chips, so it goes along
    /// (a card's Send does the same). Why the reply can't be sent as it is, if it can't: a chip
    /// that is no address, or no one to send to.
    fn settle_fields(&self) -> Option<&'static str> {
        let all = {
            let fields = self.fields.borrow();
            let fields = fields.as_ref()?;
            [fields.to.clone(), fields.cc.clone(), fields.bcc.clone()]
        };
        for field in &all {
            field.commit();
        }
        if all.iter().any(|field| field.invalid() > 0) {
            return Some("Fix or remove the address marked in red first");
        }
        let chosen = self.chosen.borrow().is_some();
        if chosen && all.iter().all(|field| field.addresses().is_empty()) {
            return Some("Add a recipient first");
        }
        None
    }

    /// The user changed the fields: they are whom it goes to from now on.
    fn fields_changed(&self) {
        if self.filling.get() {
            return;
        }
        let chosen = self.fields.borrow().as_ref().map(|fields| Recipients {
            to: fields.to.addresses(),
            cc: fields.cc.addresses(),
            bcc: fields.bcc.addresses(),
        });
        self.chosen.replace(chosen);
        self.relabel();
        self.tell_change();
    }

    /// Hides the fields again, keeping what they hold.
    fn close_fields(&self) {
        if let Some(fields) = self.fields.borrow().as_ref() {
            for field in [&fields.to, &fields.cc, &fields.bcc] {
                field.close_suggestions();
            }
        }
        self.fields_box.set_visible(false);
        self.heading.set_visible(self.is_open());
    }

    fn update_keys(&self, keymap: &Keymap) {
        let keys = keymap.display(ActionId::Send);
        self.send_keys.set_label(&keys);
        self.send_keys.set_visible(!keys.is_empty());
        self.send
            .set_tooltip_text(Some(&tooltip(keymap, ActionId::Send, "Send", None)));
        self.pop.set_tooltip_text(Some(&pop_out_tooltip(keymap)));
        self.heading
            .set_tooltip_text(Some(&recipients_tooltip(keymap)));
    }

    fn is_focused(&self) -> bool {
        self.view.has_focus()
    }

    /// Whether the keyboard focus is anywhere in the box.
    fn holds_focus(&self) -> bool {
        self.root
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus.is_ancestor(&self.root))
    }
}

/// The messages of `page`, oldest first, folded ones included.
fn page_messages(page: &Page) -> Vec<ConversationMessage> {
    page.messages().into_iter().cloned().collect()
}

/// Whether `event` is one of `triggers`' keys.
fn matches(event: &gdk::Event, triggers: &[gtk::ShortcutTrigger]) -> bool {
    triggers
        .iter()
        .any(|trigger| trigger.trigger(event, false) == gdk::KeyMatch::Exact)
}

/// The scoped keys of `id` in `keymap`, as GTK triggers.
fn triggers(keymap: &Keymap, id: ActionId) -> Vec<gtk::ShortcutTrigger> {
    keymap
        .scoped(id)
        .iter()
        .filter_map(|binding| binding.single())
        .filter_map(|key| gtk::ShortcutTrigger::parse_string(&key.canonical()))
        .collect()
}

impl Reader {
    /// Runs `listener` with every composer window the reader asks for: a card's Forward or Edit
    /// draft, and the quick reply's expansion. The app opens them (`composer::open`).
    pub fn connect_compose(&self, listener: impl Fn(ComposeRequest) + 'static) {
        self.inner
            .quick
            .on_compose
            .borrow_mut()
            .push(Rc::new(listener));
    }

    /// What the inline reply's Send does with it (prepared as a reply, saved and queued with
    /// Undo send; with [`SendMode::SendAndArchive`] the conversation is archived too). Without
    /// one, Send keeps the text.
    pub fn set_quick_send(&self, send: impl Fn(QuickDraft, SendMode) -> Local<bool> + 'static) {
        self.inner.quick.send.replace(Some(Rc::new(send)));
    }

    /// Runs `listener` with every change of an inline reply (the app keeps it as a draft at most
    /// 5 s behind the typing).
    pub fn connect_quick_text(&self, listener: impl Fn(&QuickDraft) + 'static) {
        self.inner
            .quick
            .on_text
            .borrow_mut()
            .push(Rc::new(listener));
    }

    /// Where a quick reply that holds nothing this session looks for what it held before (its
    /// draft, after a restart).
    pub fn set_quick_finder(
        &self,
        finder: impl Fn(MessageId) -> Local<Option<QuickDraft>> + 'static,
    ) {
        self.inner.quick.finder.replace(Some(Rc::new(finder)));
    }

    /// Resolves the reply sender's account signature for the inline composer.
    pub fn set_quick_signature(
        &self,
        provider: impl Fn(&MessageSummary, Option<&Headers>) -> Option<String> + 'static,
    ) {
        self.inner.quick.signature.replace(Some(Rc::new(provider)));
    }

    /// Puts `draft` back in the inline reply that answers its message (Undo send): in its box,
    /// focused when it shows, or kept for when its conversation shows again; `false` when that
    /// box holds another draft, and `draft` opened in a composer card instead.
    pub fn restore_quick_reply(&self, draft: QuickDraft) -> bool {
        self.inner.restore_quick_reply(draft, true)
    }

    /// Runs `listener` when the quick reply on screen comes or goes, or gets or loses its text.
    pub fn connect_quick_state(&self, listener: impl Fn(QuickState) + 'static) {
        self.inner
            .quick
            .on_state
            .borrow_mut()
            .push(Rc::new(listener));
    }

    pub fn quick_state(&self) -> QuickState {
        self.inner.quick.state.get()
    }

    /// Runs `listener` when the column scrolls while the inline reply is open, moving it (the
    /// window's toasts keep off it: [`Reader::quick_reply_area`]).
    pub fn connect_reply_moved(&self, listener: impl Fn() + 'static) {
        self.inner
            .on_reply_moved
            .borrow_mut()
            .push(Rc::new(listener));
    }

    /// The quick reply on screen, for checks.
    pub fn quick_reply(&self) -> Option<QuickReport> {
        let quick = self.inner.shown_quick()?;
        Some(QuickReport {
            target: quick.target(),
            label: quick.label.label().to_string(),
            placeholder: quick.placeholder.label().to_string(),
            text: quick.text(),
            focused: quick.is_focused(),
            send_sensitive: quick.send.is_sensitive(),
            kind: quick.kind(),
            open: quick.is_open(),
            editing: quick.editing(),
            holds_focus: quick.holds_focus(),
            caret: quick.caret(),
            recipients: quick.recipients(),
            chosen: quick.chosen.borrow().is_some(),
        })
    }

    /// Opens an existing generated draft in the composer before starting an inline reply.
    pub fn set_saved_reply_opener(&self, opener: impl Fn(MessageId) -> bool + 'static) {
        self.inner.quick.open_saved.replace(Some(Rc::new(opener)));
    }

    /// Quick Reply (Enter): focuses the inline reply, unless a saved reply opens in a card.
    pub fn focus_quick_reply(&self) {
        let Some(quick) = self.inner.shown_quick() else {
            return;
        };
        let kind = (!quick.has_draft()).then(|| self.inner.default_kind());
        self.inner.open_reply(&quick, None, kind);
    }

    /// Reply, Reply All and Reply to Sender in a conversation (`r`, `a`; the fork's phase B): the
    /// inline reply under the conversation of `selected` takes the focus, answering `message`
    /// (else what it answers already) as `kind`. A conversation still being read takes it once
    /// it is on screen. `false` when the reader shows, and reads, no conversation of `selected`
    /// with an inline reply: the app opens a composer card instead.
    pub fn reply_inline(
        &self,
        selected: MessageId,
        message: Option<MessageId>,
        kind: ReplyKind,
    ) -> bool {
        self.inner.reply_inline(selected, message, kind)
    }

    /// Pop Draft Out or In from a reply card (the fork's phase B): `draft` goes back under its
    /// conversation, focused, when the reader shows it and its inline reply is empty. The
    /// selected sender's default distinguishes an absent signature from its explicit removal.
    pub fn pop_in(&self, draft: QuickDraft, sender_has_signature: bool) -> PopIn {
        self.inner.pop_in(draft, sender_has_signature)
    }

    /// The inline reply's draft holding `draft` opened in a composer card (Edit Draft on its
    /// Drafts-folder copy): the box that holds just that text empties, as after Pop Out, so
    /// the draft is written in one place; a box holding something else keeps it.
    pub fn quick_reply_taken(&self, draft: &QuickDraft) {
        self.inner.quick_reply_taken(draft);
    }

    /// Edit To, Edit Cc and Edit Bcc in the inline reply: its fields show, the focus in `field`
    /// (`to`, `cc`, `bcc`).
    pub fn edit_quick_recipients(&self, field: &str) {
        if let Some(quick) = self.inner.shown_quick() {
            quick.edit_recipients(field);
            self.inner.reveal_in_column(&quick.root);
        }
    }

    /// Pop Draft Out (Ctrl+Shift+P) and Open Reply in Composer (Ctrl+Shift+E): what the inline
    /// reply holds goes to a composer card as a reply to its message, and the box empties.
    pub fn expand_quick_reply(&self) {
        self.inner.expand_quick_reply();
    }

    /// Send (Ctrl+Enter) from the inline reply: its text goes to the send seam, and the box
    /// empties at once; a refusal puts it back.
    pub fn send_quick_reply(&self) {
        self.inner.send_quick_reply(SendMode::Send);
    }

    /// Send and Archive (Ctrl+Shift+Enter): the same, and the app archives the conversation.
    pub fn send_quick_reply_and_archive(&self) {
        self.inner.send_quick_reply(SendMode::SendAndArchive);
    }

    /// What Send Later in the inline reply does: asks when (the app's time picker), then
    /// [`Reader::send_quick_reply_at`].
    pub fn set_quick_later(&self, ask: impl Fn() + 'static) {
        self.inner.quick.later.replace(Some(Rc::new(ask)));
    }

    /// Send Later (Ctrl+Shift+L) from the inline reply that holds text: the app asks when,
    /// once the reply could go ([`Inner::check_quick_send`]).
    pub fn send_quick_reply_later(&self) {
        let quick = self.inner.shown_quick().filter(|quick| quick.has_text());
        let ask = self.inner.quick.later.borrow().clone();
        if let (Some(quick), Some(ask)) = (quick, ask) {
            self.inner.check_quick_send(&quick, move |_| ask());
        }
    }

    /// Switch Reply or Reply All (Ctrl+Alt+A) in the inline reply: it answers the other way,
    /// to whom the compose rules plan for that (recipients the user chose go).
    pub fn switch_quick_reply(&self) {
        let Some(quick) = self.inner.shown_quick() else {
            return;
        };
        quick.chosen.replace(None);
        quick.restored.replace(None);
        quick.set_kind(crate::composer::switched(quick.kind()));
        if quick.has_draft() {
            self.inner.hold(&quick.draft());
        }
    }

    /// Sends what the inline reply holds at `at` (Unix ms): the box empties at once, as for
    /// Send, and the outbox keeps the reply until then.
    pub fn send_quick_reply_at(&self, at: i64) {
        self.inner.send_quick_reply(SendMode::Later(at));
    }

    /// Use Snippet… in the inline reply (the fork's phase B): `text` goes in at its caret, the
    /// caret `cursor` characters into it (else after it), `add` joins whom it goes to, and the
    /// focus is in it. `false` when no inline reply shows.
    pub fn insert_in_quick_reply(
        &self,
        text: &str,
        cursor: Option<usize>,
        add: &Recipients,
    ) -> bool {
        let Some(quick) = self.inner.shown_quick() else {
            return false;
        };
        quick.add_recipients(add);
        quick.insert_at_caret(text, cursor);
        quick.set_open(true);
        quick.view.grab_focus();
        true
    }

    /// The message of the shown conversation Forward, and Reply in a composer card, act on: the
    /// one the user chose ([`Reader::chosen_message`]), else the one the inline reply answers
    /// (the latest that isn't the user's own), as `r`, `a` and Enter do; the selected one
    /// without either.
    pub fn current_message(&self) -> Option<MessageId> {
        self.inner.current_message()
    }

    /// The message of the shown conversation the user chose: the one they moved to with Next
    /// or Previous Message in Conversation (`n`, `p`), a press in it, or Tab into it from
    /// another of its messages (`Page::current`); else, while the list's rows are messages
    /// (Group by Conversation off, [`Reader::set_message_rows`]), the selected one, whose row
    /// they picked; never one of the user's drafts. `None` when there is none (the inline reply
    /// then answers its own target).
    pub fn chosen_message(&self) -> Option<MessageId> {
        self.inner.chosen_message()
    }

    /// Where the inline reply is while it is open on screen, where `relative_to` shows it, as
    /// it will be once open and in view; toasts keep off it (`Dock::keep_toasts_off`).
    pub fn quick_reply_area(&self, relative_to: &gtk::Widget) -> Option<gtk::graphene::Rect> {
        self.inner.quick_area(relative_to)
    }

    /// Whether the conversation on screen holds `message`.
    pub fn shows_message(&self, message: MessageId) -> bool {
        !self.inner.hidden.get()
            && self
                .inner
                .shown
                .borrow()
                .as_ref()
                .is_some_and(|page| !page.missing && page.item(message).is_some())
    }
}

impl Inner {
    fn compose(&self, request: ComposeRequest) {
        let listeners = self.quick.on_compose.borrow().clone();
        for listener in listeners {
            listener(request.clone());
        }
    }

    /// How Enter and the box's own reply answer: everyone with `[reader] reply = "all"` (the
    /// default), else the sender.
    fn default_kind(&self) -> ReplyKind {
        if self.enter_all.get() {
            ReplyKind::ReplyAll
        } else {
            ReplyKind::Reply
        }
    }

    /// The buttons among message `id`'s card actions (`row`, after Details): Reply, Reply all
    /// and Forward, or Edit draft for a draft. Reply and Reply all answer inline.
    pub(super) fn reply_buttons(
        self: &Rc<Self>,
        row: &gtk::Box,
        id: MessageId,
        draft: bool,
    ) -> Vec<(ActionId, gtk::Button)> {
        let specs: Vec<Spec> = if draft { vec![EDIT] } else { REPLIES.to_vec() };
        let keymap = self.keymap.borrow().clone();
        specs
            .into_iter()
            .map(|(action, icons, label)| {
                let button = gtk::Button::builder()
                    .child(&icon(icons))
                    .valign(gtk::Align::Center)
                    .tooltip_text(tooltip(&keymap, action, label, None))
                    .css_classes(["flat", "sw-icon-button", "sw-card-reply", "sw-card-action"])
                    .build();
                button.set_widget_name(&format!("reader-card-{}", action.name()));
                button.update_property(&[gtk::accessible::Property::Label(label)]);
                // A draft's Edit draft stays in view beside its "Draft", not only on hover.
                if action == ActionId::EditDraft {
                    button.add_css_class("sw-card-edit");
                }
                let weak = Rc::downgrade(self);
                button.connect_clicked(move |_| {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let kind = match action {
                        ActionId::ReplyAll => ReplyKind::ReplyAll,
                        ActionId::Reply | ActionId::ReplySender => ReplyKind::Reply,
                        ActionId::Forward => {
                            inner.compose(ComposeRequest::Reply {
                                message: id,
                                kind: ReplyKind::Forward,
                            });
                            return;
                        }
                        _ => {
                            inner.compose(ComposeRequest::ServerDraft(id));
                            return;
                        }
                    };
                    match inner.shown_quick() {
                        Some(quick) => inner.open_reply(&quick, Some(id), Some(kind)),
                        None => inner.compose(ComposeRequest::Reply { message: id, kind }),
                    }
                });
                row.append(&button);
                (action, button)
            })
            .collect()
    }

    /// `keys.toml` changed: the cards' buttons and the quick reply name the new keys.
    pub(super) fn update_reply_keys(&self, keymap: &Keymap) {
        self.keymap.replace(keymap.clone());
        self.composer_keys.update(keymap);
        let update = |page: &Page| {
            for item in page.items() {
                for (action, button) in item.card.iter().flat_map(|card| &card.replies) {
                    let label = button_label(*action);
                    button.set_tooltip_text(Some(&tooltip(keymap, *action, label, None)));
                }
            }
            if let Some(quick) = &page.quick {
                quick.update_keys(keymap);
            }
        };
        for pages in self.pages() {
            if let Some(page) = pages.borrow().as_ref() {
                update(page);
            }
        }
        for page in self.warm.borrow().iter() {
            update(page);
        }
    }

    /// The quick reply of the conversation on screen.
    fn shown_quick(&self) -> Option<Rc<QuickReply>> {
        if self.hidden.get() {
            return None;
        }
        self.shown
            .borrow()
            .as_ref()
            .and_then(|page| page.quick.clone())
    }

    /// The summary of message `id` on page `key`.
    fn summary_on(&self, key: u64, id: MessageId) -> Option<MessageSummary> {
        self.with_page(key, |page| {
            page.messages()
                .into_iter()
                .find(|message| message.summary.id == id)
                .map(|message| message.summary.clone())
        })
        .flatten()
    }

    /// The headers of message `id` on page `key`, when its card has read them.
    fn headers_on(&self, key: u64, id: MessageId) -> Option<Headers> {
        self.with_page(key, |page| {
            page.item(id)
                .and_then(|item| item.card.as_ref())
                .map(|card| card.headers.clone())
        })
        .flatten()
    }

    /// Puts a quick reply under page `key`'s conversation, answering the message the user chose
    /// ([`user_message`]: the opened row's, while the list's rows are messages), else
    /// [`reply_target`], with what it held this session (how it answered and whom the user
    /// chose too); one left holding a reply to another message of the conversation answers
    /// that one again.
    pub(super) fn attach_quick_reply(self: &Rc<Self>, key: u64) {
        let mine = self.mine.borrow().clone();
        let message_rows = self.message_rows.get();
        let target = self
            .with_page(key, |page| {
                if page.quick.is_some() || page.missing {
                    return None;
                }
                let messages = page_messages(page);
                let latest =
                    user_message(page, message_rows).or_else(|| reply_target(&messages, &mine))?;
                let drafts = self.quick.drafts.borrow();
                let target = Some(latest)
                    .filter(|latest| drafts.contains_key(latest))
                    .or_else(|| {
                        messages
                            .iter()
                            .rev()
                            .filter(|message| message.role != FolderRole::Drafts)
                            .map(|message| message.summary.id)
                            .find(|id| drafts.contains_key(id))
                    })
                    .unwrap_or(latest);
                let summary = messages
                    .iter()
                    .find(|message| message.summary.id == target)?
                    .summary
                    .clone();
                Some((target, summary))
            })
            .flatten();
        let Some((target, summary)) = target else {
            return;
        };
        let held = self.quick.drafts.borrow().get(&target).cloned();
        let kind = held.as_ref().map_or(self.default_kind(), |held| held.kind);
        let text = held.as_ref().map_or("", |held| held.text.as_str());
        let keymap = self.keymap.borrow().clone();
        let quick = QuickReply::new(
            target,
            &summary,
            kind,
            text,
            &keymap,
            &mine,
            self.quick.signature.borrow().clone(),
        );
        if let Some(held) = &held {
            quick.signature_override.replace(held.signature.clone());
            quick.load_signature();
            quick.set_open(!held.is_empty());
        }
        if let Some(chosen) = held.as_ref().and_then(|held| held.recipients.clone()) {
            quick.chosen.replace(Some(chosen));
            quick.relabel();
        }
        self.learn_headers(key, &quick);
        self.wire_quick_reply(key, &quick);
        self.with_page(key, |page| {
            if page.newest_first {
                // Under the heading, by the newest message.
                page.content
                    .insert_child_after(&quick.root, page.content.first_child().as_ref());
            } else {
                page.content.append(&quick.root);
            }
            page.quick = Some(quick.clone());
        });
        if self.is_shown(key) {
            self.quick_changed();
        }
        if !quick.has_draft() {
            self.find_quick_text(&quick);
        }
    }

    /// Gives the quick reply its target's headers: from the target's card on page `key`, else
    /// read from the store.
    fn learn_headers(self: &Rc<Self>, key: u64, quick: &Rc<QuickReply>) {
        let target = quick.target();
        if let Some(headers) = self.headers_on(key, target) {
            quick.set_headers(headers);
            return;
        }
        let Some(reads) = self.store.read(target) else {
            return;
        };
        let weak = Rc::downgrade(quick);
        glib::spawn_future_local(async move {
            let detail = reads
                .detail
                .recv()
                .await
                .ok()
                .and_then(|versioned| versioned.value);
            let (Some(detail), Some(quick)) = (detail, weak.upgrade()) else {
                return;
            };
            if quick.target() == target && quick.headers.borrow().is_none() {
                quick.set_headers(detail.headers);
            }
        });
    }

    /// Asks the finder for what the quick reply held before; it fills the box if it is still
    /// empty and answers the same message: its text, how it answered, and the recipients the
    /// user chose then.
    fn find_quick_text(self: &Rc<Self>, quick: &Rc<QuickReply>) {
        let Some(finder) = self.quick.finder.borrow().clone() else {
            return;
        };
        let target = quick.target();
        let found = finder(target);
        let weak = Rc::downgrade(quick);
        glib::spawn_future_local(async move {
            let (Some(found), Some(quick)) = (found.await, weak.upgrade()) else {
                return;
            };
            // The text is a change of the box: the reader keeps it ([`Inner::hold`]).
            if quick.target() == target && !quick.has_draft() && !found.is_empty() {
                quick.restore_recipients(found.kind, found.recipients);
                quick.signature_override.replace(found.signature);
                quick.load_signature();
                quick.set_text(&found.text);
                quick.tell_change();
                quick.set_open(true);
            }
        });
    }

    /// Puts `draft` back in the quick reply that answers its message (Undo send, a refused
    /// send, Pop In): in the box of the page that holds its conversation when that box is
    /// empty, whether the page is on screen (focused with `focus`) or laid out unseen; with no
    /// such page, kept for when the conversation is laid out. A box holding another draft
    /// keeps it, and `draft` opens in a composer card instead (`false`): it is never lost.
    fn restore_quick_reply(self: &Rc<Self>, draft: QuickDraft, focus: bool) -> bool {
        match self.quick_holding(draft.message) {
            Some((key, quick)) if !quick.has_draft() => {
                self.put_draft(key, &quick, &draft);
                if focus && self.is_shown(key) && !self.hidden.get() {
                    quick.view.grab_focus();
                    self.reveal_in_column(&quick.root);
                }
                true
            }
            Some(_) => {
                self.compose(ComposeRequest::QuickReply {
                    message: draft.message,
                    text: draft.text,
                    kind: draft.kind,
                    recipients: draft.recipients,
                    signature: draft.signature,
                });
                false
            }
            None => {
                self.hold(&draft);
                true
            }
        }
    }

    /// Puts `draft` in `quick`, page `key`'s inline reply: answering its message as it did, to
    /// whom the user chose, open. Its text is a change the reader keeps.
    fn put_draft(self: &Rc<Self>, key: u64, quick: &Rc<QuickReply>, draft: &QuickDraft) {
        if quick.target() != draft.message
            && let Some(summary) = self.summary_on(key, draft.message)
        {
            quick.retarget(draft.message, &summary, draft.kind);
            self.learn_headers(key, quick);
        }
        quick.take(draft);
        quick.set_open(true);
    }

    /// The key and inline reply of the page whose conversation holds `message`: the one on
    /// screen, else one laid out unseen (being laid out, or ahead).
    fn quick_holding(&self, message: MessageId) -> Option<(u64, Rc<QuickReply>)> {
        let holding = |page: &Page| {
            page.quick
                .clone()
                .filter(|quick| {
                    !page.missing && (quick.target() == message || page.item(message).is_some())
                })
                .map(|quick| (page.key, quick))
        };
        let shown = self.shown.borrow().as_ref().and_then(holding);
        shown
            .or_else(|| self.pending.borrow().as_ref().and_then(holding))
            .or_else(|| self.warm.borrow().iter().find_map(holding))
    }

    /// Whether `message` of the page on screen is in a Drafts folder: never answered.
    fn is_draft(&self, message: MessageId) -> bool {
        self.shown.borrow().as_ref().is_some_and(|page| {
            page.item(message)
                .is_some_and(|item| item.message.role == FolderRole::Drafts)
        })
    }

    /// The key of the page on screen.
    fn shown_key(&self) -> Option<u64> {
        self.shown.borrow().as_ref().map(|page| page.key)
    }

    /// Whether the page on screen holds `message`.
    fn shown_holds(&self, message: MessageId) -> bool {
        self.shown
            .borrow()
            .as_ref()
            .is_some_and(|page| page.item(message).is_some())
    }

    fn wire_quick_reply(self: &Rc<Self>, key: u64, quick: &Rc<QuickReply>) {
        let weak = Rc::downgrade(self);
        let shown = Rc::downgrade(quick);
        quick.view.buffer().connect_changed(move |_| {
            if let (Some(inner), Some(quick)) = (weak.upgrade(), shown.upgrade()) {
                inner.quick_text_changed(key, &quick);
            }
        });
        self.composer_keys
            .attach(&quick.root, gtk::PropagationPhase::Capture);
        // With nothing to send, Send's keys do nothing (GtkTextView would take Ctrl+Enter as a
        // new line).
        let empty = gtk::EventControllerKey::new();
        empty.set_propagation_phase(gtk::PropagationPhase::Capture);
        empty.set_name(Some("sealwax-quick-reply-send"));
        let weak = Rc::downgrade(self);
        let shown = Rc::downgrade(quick);
        empty.connect_key_pressed(move |controller, _, _, _| {
            let (Some(inner), Some(quick), Some(event)) =
                (weak.upgrade(), shown.upgrade(), controller.current_event())
            else {
                return glib::Propagation::Proceed;
            };
            let keymap = inner.keymap.borrow();
            let send = [ActionId::Send, ActionId::SendAndArchive]
                .into_iter()
                .any(|id| matches(&event, &triggers(&keymap, id)));
            if send && !quick.has_text() {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        quick.root.add_controller(empty);
        // The reader's keys stop at the box's edge: what the text area didn't take (Escape,
        // Ctrl+Z with nothing to undo, Ctrl+J, Ctrl+R…) never reaches a mail command. Open Reply
        // in Composer passes. Escape closes an empty box, else leaves the draft for the
        // conversation; with the recipients' fields open it closes them first.
        let guard = gtk::EventControllerKey::new();
        guard.set_name(Some("sealwax-quick-reply-guard"));
        let weak = Rc::downgrade(self);
        let shown = Rc::downgrade(quick);
        guard.connect_key_pressed(move |controller, key, _, state| {
            let (Some(inner), Some(event)) = (weak.upgrade(), controller.current_event()) else {
                return glib::Propagation::Proceed;
            };
            if key == gdk::Key::Escape && state.is_empty() {
                if let Some(quick) = shown.upgrade() {
                    inner.escape_quick_reply(&quick);
                }
                return glib::Propagation::Stop;
            }
            let expand = triggers(&inner.keymap.borrow(), ActionId::ReplyInComposer);
            if matches(&event, &expand) {
                return glib::Propagation::Proceed;
            }
            let model = inner.keys.model();
            let ours = (0..model.n_items())
                .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
                .filter_map(|shortcut| shortcut.trigger())
                .any(|trigger| trigger.trigger(&event, false) == gdk::KeyMatch::Exact);
            if ours {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        quick.root.add_controller(guard);
        // The box opens when the focus comes in (QuickReply::new): the column shows it whole.
        let opened = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        let shown = Rc::downgrade(quick);
        opened.connect_enter(move |_| {
            if let (Some(inner), Some(quick)) = (weak.upgrade(), shown.upgrade())
                && inner.is_shown(key)
            {
                inner.reveal_in_column(&quick.root);
            }
        });
        quick.root.add_controller(opened);
        // A change of the recipients is a change of the draft.
        let weak = Rc::downgrade(self);
        let shown = Rc::downgrade(quick);
        quick.on_change.replace(Some(Rc::new(move || {
            if let (Some(inner), Some(quick)) = (weak.upgrade(), shown.upgrade()) {
                inner.hold(&quick.draft());
            }
        })));
    }

    fn quick_text_changed(self: &Rc<Self>, key: u64, quick: &QuickReply) {
        let text = quick.text();
        quick.placeholder.set_visible(text.is_empty());
        if !text.trim().is_empty() && !quick.is_open() {
            quick.set_open(true);
        }
        self.hold(&quick.draft());
        if self.is_shown(key) {
            self.quick_changed();
            if quick.is_focused() {
                self.reveal_in_column(&quick.root);
            }
        }
    }

    /// What the inline reply for `draft.message` holds now (its text, how it answers, the
    /// recipients the user chose): kept while it holds text, and told to the app, which keeps
    /// it as a draft. Every change of a box comes through here.
    fn hold(&self, draft: &QuickDraft) {
        {
            let mut drafts = self.quick.drafts.borrow_mut();
            if draft.is_empty() {
                drafts.remove(&draft.message);
            } else {
                drafts.insert(draft.message, draft.clone());
            }
        }
        let listeners = self.quick.on_text.borrow().clone();
        for listener in listeners {
            listener(draft);
        }
    }

    /// Tells the listeners what the quick reply on screen is now, when that changed; a reply
    /// asked for before its conversation was on screen ([`Inner::reply_inline`]) takes the focus
    /// now.
    pub(super) fn quick_changed(self: &Rc<Self>) {
        let state = self
            .shown_quick()
            .map_or(QuickState::default(), |quick| QuickState {
                shown: true,
                has_text: quick.has_text() && !quick.sending.get(),
            });
        // Its page is the one on screen now (the borrow ends before the reply opens).
        let wanted = self.quick.wanted.get().filter(|wanted| {
            self.shown
                .borrow()
                .as_ref()
                .is_some_and(|page| page.selected == wanted.selected)
        });
        if let Some(wanted) = wanted
            && let Some(quick) = self.shown_quick()
        {
            self.quick.wanted.set(None);
            self.open_reply(&quick, wanted.message, Some(wanted.kind));
        }
        if self.quick.state.replace(state) == state {
            return;
        }
        let listeners = self.quick.on_state.borrow().clone();
        for listener in listeners {
            listener(state);
        }
    }

    /// The shown conversation changed in place: an empty quick reply follows the message it
    /// should answer now (a reply arrived), with what it held for that one this session; one
    /// with text keeps its message.
    pub(super) fn retarget_quick_reply(self: &Rc<Self>, key: u64) {
        let has_quick = self
            .with_page(key, |page| page.quick.is_some())
            .unwrap_or(false);
        if !has_quick {
            self.attach_quick_reply(key);
            return;
        }
        let mine = self.mine.borrow().clone();
        let message_rows = self.message_rows.get();
        let change = self
            .with_page(key, |page| {
                let quick = page.quick.clone()?;
                if quick.has_draft() || quick.chosen.borrow().is_some() {
                    return None;
                }
                let messages = page_messages(page);
                let target =
                    user_message(page, message_rows).or_else(|| reply_target(&messages, &mine))?;
                if target == quick.target() {
                    return None;
                }
                let summary = messages
                    .iter()
                    .find(|message| message.summary.id == target)?
                    .summary
                    .clone();
                Some((quick, target, summary))
            })
            .flatten();
        if let Some((quick, target, summary)) = change {
            let held = self.quick.drafts.borrow().get(&target).cloned();
            match held {
                Some(held) => self.put_draft(key, &quick, &held),
                None => {
                    quick.retarget(target, &summary, quick.kind());
                    quick.set_text("");
                    self.learn_headers(key, &quick);
                }
            }
        }
        if self.is_shown(key) {
            self.quick_changed();
        }
    }

    /// `r`, `a`, Reply from a card: see [`Reader::reply_inline`].
    fn reply_inline(
        self: &Rc<Self>,
        selected: MessageId,
        message: Option<MessageId>,
        kind: ReplyKind,
    ) -> bool {
        let shown = self
            .shown
            .borrow()
            .as_ref()
            .filter(|page| !page.missing && !self.hidden.get() && page.item(selected).is_some())
            .and_then(|page| page.quick.clone());
        if let Some(quick) = shown {
            self.quick.wanted.set(None);
            self.open_reply(&quick, message, Some(kind));
            return true;
        }
        // Its conversation is still being read: the reply opens once it is on screen.
        if self.target.get() == Some(selected) {
            self.quick.wanted.set(Some(Wanted {
                selected,
                message,
                kind,
            }));
            return true;
        }
        false
    }

    /// Opens `quick` with the focus in it: answering `message` when it names another message of
    /// the conversation (its text, if any, goes along), as `kind` when given.
    fn open_reply(
        self: &Rc<Self>,
        quick: &Rc<QuickReply>,
        message: Option<MessageId>,
        kind: Option<ReplyKind>,
    ) {
        // A reply the user is writing here stays theirs: a generated draft opens in its card
        // only in place of an empty box.
        if !quick.has_draft()
            && self
                .quick
                .open_saved
                .borrow()
                .as_ref()
                .is_some_and(|open| open(message.unwrap_or_else(|| quick.target())))
        {
            return;
        }
        let key = self.shown_key();
        let moved = message
            .filter(|message| *message != quick.target() && !self.is_draft(*message))
            .and_then(|message| {
                let key = key?;
                Some((key, self.summary_on(key, message)?))
            });
        if let Some((key, summary)) = moved {
            // The draft moves to the message it answers now.
            let old = quick.draft();
            let held = !old.is_empty();
            if held {
                self.hold(&QuickDraft {
                    text: String::new(),
                    signature: None,
                    ..old
                });
            }
            quick.retarget(summary.id, &summary, kind.unwrap_or(quick.kind()));
            self.learn_headers(key, quick);
            if held {
                self.hold(&quick.draft());
            }
        } else if let Some(kind) = kind {
            quick.set_kind(kind);
            if quick.has_draft() {
                self.hold(&quick.draft());
            }
        }
        quick.load_signature();
        if !quick.holds_focus() {
            let focus = quick
                .root
                .root()
                .and_then(|root| root.focus())
                .map(|focus| focus.downgrade());
            quick.return_focus.replace(focus);
        }
        quick.set_open(true);
        quick.view.grab_focus();
        // Once it has opened: the column shows all of it.
        self.reveal_in_column(&quick.root);
    }

    /// Escape in the inline reply: the recipients' fields close first; then an empty box
    /// closes and the focus goes back where the reply started (or to the message it answers);
    /// one with text stays, a draft, and the focus goes to the conversation.
    fn escape_quick_reply(self: &Rc<Self>, quick: &Rc<QuickReply>) {
        if quick.editing() {
            quick.close_fields();
            quick.view.grab_focus();
            return;
        }
        self.end_reveal(&quick.root);
        if quick.has_draft() {
            self.leave_quick_reply();
            return;
        }
        // The box forgets what it was asked: the next reply starts from the defaults.
        self.reset_quick_reply(quick);
        let back = quick
            .return_focus
            .take()
            .and_then(|weak| weak.upgrade())
            .filter(|widget| {
                !widget.is_ancestor(&quick.root) && widget.is_mapped() && widget.is_sensitive()
            });
        let moved = back.is_some_and(|widget| widget.grab_focus());
        if !moved {
            self.leave_quick_reply();
        }
        quick.set_open(quick.holds_focus());
    }

    /// An empty inline reply goes back to its defaults: the message the user chose
    /// ([`user_message`]), else the latest that isn't the user's own, answered as Enter answers
    /// (`[reader] reply`), to whom the compose rules plan.
    fn reset_quick_reply(self: &Rc<Self>, quick: &Rc<QuickReply>) {
        quick.signature_override.replace(None);
        let mine = self.mine.borrow().clone();
        let message_rows = self.message_rows.get();
        let default = self
            .shown
            .borrow()
            .as_ref()
            .filter(|page| {
                page.quick
                    .as_ref()
                    .is_some_and(|shown| Rc::ptr_eq(shown, quick))
            })
            .and_then(|page| {
                let messages = page_messages(page);
                let target =
                    user_message(page, message_rows).or_else(|| reply_target(&messages, &mine))?;
                let summary = messages
                    .iter()
                    .find(|message| message.summary.id == target)?
                    .summary
                    .clone();
                Some((page.key, summary))
            });
        let kind = self.default_kind();
        match default {
            Some((key, summary)) if summary.id != quick.target() => {
                quick.retarget(summary.id, &summary, kind);
                self.learn_headers(key, quick);
            }
            _ => {
                quick.kind.set(kind);
                quick.chosen.replace(None);
                quick.restored.replace(None);
                quick.fill_fields();
                quick.relabel();
                quick.load_signature();
            }
        }
    }

    /// What the quick reply holds goes to a composer card (Pop Draft Out); with nothing typed,
    /// a plain reply.
    fn expand_quick_reply(&self) {
        let Some(quick) = self.shown_quick() else {
            return;
        };
        if quick.sending.get() {
            return;
        }
        // A recipient still being typed goes along; one that is no address stays behind, as
        // in the fields.
        let _ = quick.settle_fields();
        let draft = quick.draft();
        let request = if draft.text.trim().is_empty()
            && draft.recipients.is_none()
            && draft.signature.is_none()
        {
            ComposeRequest::Reply {
                message: draft.message,
                kind: draft.kind,
            }
        } else {
            ComposeRequest::QuickReply {
                message: draft.message,
                text: draft.text,
                kind: draft.kind,
                recipients: draft.recipients,
                signature: draft.signature,
            }
        };
        quick.signature_override.replace(None);
        quick.load_signature();
        quick.set_text("");
        quick.tell_change();
        quick.chosen.replace(None);
        quick.restored.replace(None);
        quick.close_fields();
        quick.relabel();
        self.end_reveal(&quick.root);
        self.compose(request);
    }

    /// See [`Reader::quick_reply_taken`]. Emptying the box is a change the reader keeps (the
    /// app's keeper has let the draft go, so it saves nothing).
    fn quick_reply_taken(self: &Rc<Self>, draft: &QuickDraft) {
        match self.quick_holding(draft.message) {
            Some((key, quick)) if quick.target() == draft.message => {
                if quick.text() != draft.text
                    || quick.draft().signature != draft.signature
                    || quick.sending.get()
                {
                    return;
                }
                quick.signature_override.replace(None);
                quick.load_signature();
                quick.set_text("");
                quick.tell_change();
                quick.chosen.replace(None);
                quick.restored.replace(None);
                quick.close_fields();
                quick.relabel();
                self.end_reveal(&quick.root);
                quick.set_open(quick.holds_focus());
                if self.is_shown(key) {
                    self.quick_changed();
                }
            }
            _ => {
                let mut drafts = self.quick.drafts.borrow_mut();
                if drafts.get(&draft.message).is_some_and(|kept| {
                    kept.text == draft.text && kept.signature == draft.signature
                }) {
                    drafts.remove(&draft.message);
                }
            }
        }
    }

    /// A reply card's draft back under its conversation: see [`Reader::pop_in`].
    fn pop_in(self: &Rc<Self>, mut draft: QuickDraft, sender_has_signature: bool) -> PopIn {
        let Some(quick) = self
            .shown_quick()
            .filter(|_| self.shown_holds(draft.message))
        else {
            return PopIn::NotShown;
        };
        if quick.has_draft() {
            return PopIn::Busy;
        }
        let key = self.shown_key();
        let headers = key.and_then(|key| self.headers_on(key, draft.message));
        // The card's selected sender and the inline reply's account can differ. An absent
        // signature needs no override only when neither has a default to suppress. Unknown
        // target headers keep the override, since they may select another account identity.
        if draft.signature.as_deref() == Some("") && !sender_has_signature {
            let summary = key.and_then(|key| self.summary_on(key, draft.message));
            let signature_headers = headers.clone().or_else(|| {
                (quick.target() == draft.message)
                    .then(|| quick.headers.borrow().clone())
                    .flatten()
            });
            if let (Some(summary), Some(headers), Some(provider)) = (
                summary,
                signature_headers.as_ref(),
                quick.signature_provider.as_ref(),
            ) && provider(&summary, Some(headers))
                .is_none_or(|signature| signature.trim().is_empty())
            {
                draft.signature = None;
            }
        }
        // The card's recipients come along only when they aren't what the rules would plan
        // (names and order aside, as after a restart).
        let planned = headers
            .as_ref()
            .map(|headers| planned(headers, draft.kind, &self.mine.borrow()));
        let recipients = draft.recipients.clone().filter(|recipients| {
            !planned
                .as_ref()
                .is_some_and(|planned| same_people(planned, recipients))
        });
        quick.return_focus.replace(None);
        let draft = QuickDraft {
            recipients,
            ..draft
        };
        self.restore_quick_reply(draft, true);
        PopIn::Done
    }

    /// Runs `go` once the inline reply could be sent, as a card's Send checks: what is typed
    /// in the fields becomes chips, each an address and at least one; and a text that names an
    /// attachment (the inline reply has none) asks first (`composer::send_doubt`).
    fn check_quick_send(
        self: &Rc<Self>,
        quick: &Rc<QuickReply>,
        go: impl FnOnce(&Rc<Self>) + 'static,
    ) {
        if let Some(problem) = quick.settle_fields() {
            quick.edit_recipients("to");
            super::files::toast_over(&quick.root, problem);
            return;
        }
        // A reply has its subject.
        let doubt = crate::composer::send_checks()
            .then(|| crate::composer::send_doubt("Re", &quick.text(), 0))
            .flatten();
        let Some(doubt) = doubt else {
            go(self);
            return;
        };
        if quick.asking.replace(true) {
            return;
        }
        let inner = Rc::downgrade(self);
        let quick = quick.clone();
        glib::spawn_future_local(async move {
            let send = crate::composer::confirm_send(&quick.root, doubt).await;
            quick.asking.set(false);
            if let (true, Some(inner)) = (send, inner.upgrade()) {
                go(&inner);
            }
        });
    }

    fn send_quick_reply(self: &Rc<Self>, mode: SendMode) {
        let Some(quick) = self.shown_quick() else {
            return;
        };
        if !quick.has_text() || quick.sending.get() {
            return;
        }
        // Send Later checked before its picker.
        if matches!(mode, SendMode::Later(_)) {
            self.send_quick_reply_now(mode);
        } else {
            self.check_quick_send(&quick, move |inner| inner.send_quick_reply_now(mode));
        }
    }

    fn send_quick_reply_now(self: &Rc<Self>, mode: SendMode) {
        let Some(quick) = self.shown_quick() else {
            return;
        };
        if !quick.has_text() || quick.sending.get() {
            return;
        }
        let Some(send) = self.quick.send.borrow().clone() else {
            tracing::warn!("reader: the quick reply has nowhere to send to; its text stays");
            return;
        };
        let draft = quick.draft();
        // The box empties at once (Superhuman: a send never waits on screen); a refusal puts
        // the draft back.
        quick.sending.set(true);
        quick.signature_override.replace(None);
        quick.set_text("");
        quick.close_fields();
        self.reset_quick_reply(&quick);
        quick.sending.set(false);
        self.end_reveal(&quick.root);
        if quick.holds_focus() {
            self.leave_quick_reply();
        }
        quick.set_open(quick.holds_focus());
        self.quick_changed();
        let taken = send(draft.clone(), mode);
        let inner = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let taken = taken.await;
            let Some(inner) = inner.upgrade() else {
                return;
            };
            if !taken {
                let _ = inner.restore_quick_reply(draft, false);
            }
            inner.quick_changed();
        });
    }

    /// Leaving the quick reply: the focus goes to the message it answers, else the selected
    /// one, into a body where one has it, so Enter replies again (a reply that came in while
    /// the conversation was open is a collapsed row, a button Enter would expand). Not a move
    /// of the user's ([`Inner::give_focus`]): the box answers whom it would have, and `n`/`p`
    /// go on from that message.
    fn leave_quick_reply(&self) {
        let shown = self.shown.borrow();
        let Some(page) = shown.as_ref() else {
            return;
        };
        let key = page.key;
        let target = page.quick.as_ref().map(|quick| quick.target());
        let items: Vec<&Item> = [target, Some(page.selected)]
            .into_iter()
            .flatten()
            .filter_map(|id| page.item(id))
            .collect();
        let focus = items
            .iter()
            .find_map(|item| Some((item.card.as_ref()?.body_target()?, item.id())))
            .or_else(|| {
                items.iter().find_map(|item| match &item.card {
                    Some(card) => card.focus_target().map(|target| (target, item.id())),
                    None => Some((item.row.clone().upcast(), item.id())),
                })
            });
        drop(shown);
        match focus {
            Some((widget, id)) => {
                self.give_focus(key, id, &widget);
            }
            None => {
                self.stage.grab_focus();
            }
        }
    }

    fn current_message(&self) -> Option<MessageId> {
        let shown = self.shown.borrow();
        let page = shown.as_ref().filter(|page| !page.missing)?;
        if let Some(chosen) = self.choice(page) {
            return Some(chosen);
        }
        let target = page.quick.as_ref().map(|quick| quick.target()).or_else(|| {
            let messages = page_messages(page);
            reply_target(&messages, &self.mine.borrow())
        });
        Some(target.unwrap_or(page.selected))
    }

    fn chosen_message(&self) -> Option<MessageId> {
        let shown = self.shown.borrow();
        let page = shown
            .as_ref()
            .filter(|page| !page.missing && !self.hidden.get())?;
        self.choice(page)
    }

    /// The message of `page` the user chose: the one they moved to (`Page::current`), else,
    /// while the list's rows are messages, the selected one, whose row they picked; never a
    /// draft ([`user_message`]: `n` onto the user's unsent reply, then `f`, forwarded the draft).
    /// Only that: where the focus is says nothing more (the conversation opens focused in its
    /// first unread message; the reader puts the focus back after a send).
    fn choice(&self, page: &Page) -> Option<MessageId> {
        user_message(page, self.message_rows.get())
    }

    /// The user moved to another message of page `key` (`Page::current`, [`Inner::moved_to`]):
    /// an empty inline reply answers it from now on (what it held for it this session comes
    /// back), so its line ("Reply to Ingrid Solberg…") names whom `r`, `a` and Enter answer. A
    /// box holding text, recipients the user chose, or a send keeps its message; a draft is
    /// never answered.
    pub(super) fn follow_choice(self: &Rc<Self>, key: u64) {
        let message_rows = self.message_rows.get();
        let change = self
            .with_page(key, |page| {
                let quick = page.quick.clone()?;
                if quick.has_draft() || quick.chosen.borrow().is_some() || quick.sending.get() {
                    return None;
                }
                let item = page.item(user_message(page, message_rows)?)?;
                if item.id() == quick.target() {
                    return None;
                }
                Some((quick, item.message.summary.clone()))
            })
            .flatten();
        let Some((quick, summary)) = change else {
            return;
        };
        let held = self.quick.drafts.borrow().get(&summary.id).cloned();
        match held {
            Some(held) => self.put_draft(key, &quick, &held),
            None => {
                quick.retarget(summary.id, &summary, quick.kind());
                self.learn_headers(key, &quick);
            }
        }
        if self.is_shown(key) {
            self.quick_changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Flags, FolderId, ThreadId};

    /// Whom Reply All answers of a message with `headers`, To then Cc.
    fn reply_all_to(headers: &Headers, mine: &[String]) -> Vec<Address> {
        let planned = planned(headers, ReplyKind::ReplyAll, mine);
        planned.to.into_iter().chain(planned.cc).collect()
    }

    fn message(id: i64, from: &str, role: FolderRole) -> ConversationMessage {
        ConversationMessage {
            summary: MessageSummary {
                id: MessageId(id),
                folder: FolderId(1),
                thread: ThreadId(1),
                sender_name: String::new(),
                sender_email: from.into(),
                subject: "Hyttetur".into(),
                snippet: String::new(),
                date: id,
                flags: Flags::SEEN,
                has_attachments: false,
                thread_count: 1,
            },
            role,
            copies: Vec::new(),
        }
    }

    #[test]
    fn a_draft_to_whom_the_rules_plan_is_no_choice() {
        let people = |to: &[(&str, &str)], cc: &[(&str, &str)]| Recipients {
            to: to
                .iter()
                .map(|(name, email)| Address::new(*name, *email))
                .collect(),
            cc: cc
                .iter()
                .map(|(name, email)| Address::new(*name, *email))
                .collect(),
            bcc: Vec::new(),
        };
        let plan = people(
            &[("Tomas Berg", "tomas@example.test")],
            &[("Sana", "sana@example.test"), ("", "leah@example.test")],
        );
        // Names, case and order aside, the same people.
        assert!(same_people(
            &plan,
            &people(
                &[("", "Tomas@Example.test")],
                &[("Leah Kim", "leah@example.test"), ("", "sana@example.test")]
            )
        ));
        // Someone taken out, or moved to Cc: the user's choice.
        assert!(!same_people(
            &plan,
            &people(&[("", "tomas@example.test")], &[("", "sana@example.test")])
        ));
        assert!(!same_people(
            &plan,
            &people(
                &[],
                &[
                    ("", "tomas@example.test"),
                    ("", "sana@example.test"),
                    ("", "leah@example.test")
                ]
            )
        ));
    }

    #[test]
    fn reply_all_names_everyone_but_the_user() {
        let mine = vec!["Alex@example.test".to_owned()];
        let headers = Headers {
            from: vec![Address::new("Tomas Berg", "tomas@example.test")],
            to: vec![
                Address::new("Alex", "alex@example.test"),
                Address::new("Leah Kim", "leah@example.test"),
            ],
            cc: vec![
                Address::new("", "sana@example.test"),
                Address::new("Leah", "LEAH@example.test"),
            ],
            ..Headers::default()
        };
        let everyone = reply_all_to(&headers, &mine);
        let emails: Vec<&str> = everyone.iter().map(|a| a.email.as_str()).collect();
        assert_eq!(
            emails,
            [
                "tomas@example.test",
                "leah@example.test",
                "sana@example.test"
            ]
        );
        assert_eq!(
            recipients_label(&everyone),
            "Reply to Tomas Berg and 2 others"
        );
        assert_eq!(
            recipients_label(&everyone[..2]),
            "Reply to Tomas Berg and 1 other"
        );
        let alone = Headers {
            from: vec![Address::new("Tomas Berg", "tomas@example.test")],
            to: vec![Address::new("", "alex@example.test")],
            ..Headers::default()
        };
        assert_eq!(
            recipients_label(&reply_all_to(&alone, &mine)),
            "Reply to Tomas Berg"
        );
        // Reply answers the sender alone, Reply All everyone, To then Cc.
        let reply = planned(&headers, ReplyKind::Reply, &mine);
        assert_eq!(reply.to, [Address::new("Tomas Berg", "tomas@example.test")]);
        assert!(reply.cc.is_empty() && reply.bcc.is_empty());
        let all = planned(&headers, ReplyKind::ReplyAll, &mine);
        assert_eq!(all.to.len() + all.cc.len(), 3);
        assert_eq!(recipients_label(&[]), "Reply");
    }

    #[test]
    fn the_cards_buttons_answer_as_r_and_a_do() {
        // Whatever Enter answers (`[reader] reply`): Reply is the sender, Reply all everyone.
        let ids = REPLIES.map(|(id, _, _)| id);
        assert_eq!(
            ids,
            [ActionId::Reply, ActionId::ReplyAll, ActionId::Forward]
        );
        assert_eq!(button_label(ActionId::Reply), "Reply");
        assert_eq!(button_label(ActionId::ReplyAll), "Reply all");
        assert_eq!(button_label(ActionId::EditDraft), "Edit draft");
    }

    #[test]
    fn the_quick_reply_answers_the_latest_message_not_the_users_own() {
        let mine = vec!["alex@example.test".to_owned()];
        let inbox = FolderRole::Inbox;
        let conversation = [
            message(1, "ingrid@example.test", inbox),
            message(2, "tomas@example.test", inbox),
            message(3, "Alex@Example.test", FolderRole::Sent),
        ];
        assert_eq!(reply_target(&conversation, &mine), Some(MessageId(2)));
        // A draft is never answered, the user's own or not.
        let with_draft = [
            message(1, "ingrid@example.test", inbox),
            message(2, "sana@example.test", FolderRole::Drafts),
        ];
        assert_eq!(reply_target(&with_draft, &mine), Some(MessageId(1)));
        // Only the user's own messages: the latest of them.
        let own = [
            message(1, "alex@example.test", FolderRole::Sent),
            message(2, "alex@example.test", FolderRole::Sent),
            message(3, "alex@example.test", FolderRole::Drafts),
        ];
        assert_eq!(reply_target(&own, &mine), Some(MessageId(2)));
        let drafts = [message(1, "alex@example.test", FolderRole::Drafts)];
        assert_eq!(reply_target(&drafts, &mine), None);
        assert_eq!(reply_target(&[], &mine), None);
        // Without the user's addresses, the latest.
        assert_eq!(reply_target(&conversation, &[]), Some(MessageId(3)));
    }

    #[test]
    fn labels_name_whom_the_reply_goes_to() {
        let mine = vec!["alex@example.test".to_owned()];
        let mut summary = message(1, "tomas@example.test", FolderRole::Inbox).summary;
        assert_eq!(reply_label(&summary, &mine), "Reply to tomas@example.test");
        summary.sender_name = "Tomas Berg".into();
        assert_eq!(reply_label(&summary, &mine), "Reply to Tomas Berg");
        summary.sender_email = "alex@example.test".into();
        assert_eq!(reply_label(&summary, &mine), "Reply");
        let nobody = message(2, "", FolderRole::Inbox).summary;
        assert_eq!(reply_label(&nobody, &mine), "Reply");
    }

    #[test]
    fn the_closed_box_names_whom_it_answers() {
        assert_eq!(closed_text("Reply to Tomas Berg"), "Reply to Tomas Berg…");
        assert_eq!(closed_text("Reply"), "Reply…");
    }

    #[test]
    fn pop_out_names_the_keys_in_use() {
        let keymap = Keymap::default();
        assert_eq!(
            pop_out_tooltip(&keymap),
            "Pop out to the composer (Ctrl+Shift+P)"
        );
        let unbound = Keymap::with_overrides([(ActionId::PopOutDraft, Vec::new())]);
        assert_eq!(pop_out_tooltip(&unbound), "Pop out to the composer");
    }

    #[test]
    fn a_report_prints_no_text() {
        let report = QuickReport {
            target: MessageId(4),
            label: "Reply to Tomas Berg".into(),
            placeholder: "Reply to Tomas Berg…".into(),
            text: "See you Friday".into(),
            focused: true,
            send_sensitive: true,
            kind: ReplyKind::Reply,
            open: true,
            editing: false,
            holds_focus: true,
            caret: 3,
            recipients: Some(Recipients {
                to: vec![Address::new("Tomas Berg", "tomas@example.test")],
                ..Recipients::default()
            }),
            chosen: false,
        };
        let printed = format!("{report:?}");
        assert!(
            !printed.contains("Friday") && !printed.contains("Tomas"),
            "{printed}"
        );
        let draft = QuickDraft {
            message: MessageId(4),
            kind: ReplyKind::ReplyAll,
            text: "See you Friday".into(),
            recipients: report.recipients.clone(),
            signature: None,
        };
        let printed = format!("{draft:?}");
        assert!(
            !printed.contains("Friday") && !printed.contains("tomas"),
            "{printed}"
        );
    }
}
