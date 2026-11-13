//! The conversation the reader shows (ML-057; ADR-016's M5 amendment; DESIGN.md "Reader"): one
//! subject heading, then every message oldest first, or newest first as `config.toml`'s
//! `conversation_order` says (issue #3: the page's model stays oldest first; only the order on
//! screen turns, [`Page::place`]). The selected message, every unread one and the latest are
//! expanded cards; the others are quiet one-line rows (sender, snippet, time) that expand when
//! clicked. A run of [`FOLD_LEAST`] or more collapsed messages folds into one "N more messages"
//! row, keeping the conversation's first message and the [`KEEP_BEFORE`] right before each
//! expanded one in sight (as Gmail and Superhuman fold a thread); a click reveals the fold, a
//! long one [`GAP_STEP`] messages at a time. This module holds the page's model and its rows;
//! the reader (`mod.rs`, `cards.rs`) loads and shows the cards.

use super::ShownBody;
use super::engine::InlineParts;
use super::format;
use super::page::CardWidgets;
use super::view::MessageView;
use super::widgets::BodyBox;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::message::base_subject;
use sealwax_domain::model::{FolderRole, MessageId, MessageSummary, ThreadId};
use sealwax_domain::theme::BodyStyle;
use sealwax_infrastructure::store::ConversationMessage;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// Collapsed messages kept in sight right before an expanded one, in a run that folds.
pub const KEEP_BEFORE: usize = 1;
/// The fewest messages a fold holds: a shorter run stays as rows.
pub const FOLD_LEAST: usize = 3;
/// Messages one click on "N more messages" reveals, the newest of them first (a proposal); a
/// fold with at most half a step more is revealed whole.
pub const GAP_STEP: usize = 50;

/// Which messages expand when the conversation is shown: the selected one (the user picked
/// it), every unread one and the latest.
pub fn initially_expanded(messages: &[ConversationMessage], selected: MessageId) -> Vec<bool> {
    let last = messages.len().saturating_sub(1);
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            message.summary.id == selected || !message.summary.flags.is_seen() || index == last
        })
        .collect()
}

/// Which messages show as rows or cards rather than inside a fold: every expanded one, the
/// conversation's first, and in each run of collapsed messages the [`KEEP_BEFORE`] last; the
/// rest of a run folds when it is at least [`FOLD_LEAST`] long.
pub fn initially_shown(expanded: &[bool]) -> Vec<bool> {
    let count = expanded.len();
    let mut shown = vec![true; count];
    let mut index = 0;
    while index < count {
        if expanded[index] {
            index += 1;
            continue;
        }
        let start = index;
        while index < count && !expanded[index] {
            index += 1;
        }
        let from = start.max(1);
        let to = index.saturating_sub(KEEP_BEFORE).max(from);
        if to - from >= FOLD_LEAST {
            shown[from..to].iter_mut().for_each(|shown| *shown = false);
        }
    }
    shown
}

/// The runs of messages that aren't shown, as ranges of positions.
pub fn folds(shown: &[bool]) -> Vec<Range<usize>> {
    let mut folds = Vec::new();
    let mut start = None;
    for (index, shown) in shown.iter().enumerate() {
        match (shown, start) {
            (false, None) => start = Some(index),
            (true, Some(from)) => {
                folds.push(from..index);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        folds.push(from..shown.len());
    }
    folds
}

/// What one click on a fold of `fold` reveals: its newest [`GAP_STEP`] messages, the ones next
/// to what follows it, or all of it when little more is left.
pub fn reveal(fold: Range<usize>) -> Range<usize> {
    if fold.len() <= GAP_STEP + GAP_STEP / 2 {
        fold
    } else {
        fold.end - GAP_STEP..fold.end
    }
}

/// "1 more message", "138 more messages".
pub fn fold_text(count: usize) -> String {
    format!("{count} {}", fold_words(count))
}

/// What follows a fold's count: "more message", "more messages".
pub fn fold_words(count: usize) -> &'static str {
    if count == 1 {
        "more message"
    } else {
        "more messages"
    }
}

/// "Conversation, 12 messages", for screen readers.
pub fn size_text(count: usize) -> String {
    if count == 1 {
        "1 message".to_owned()
    } else {
        format!("{count} messages")
    }
}

/// An expanded HTML body: its frame on the page, the box that keeps its height, and the view it
/// holds while it is near the visible part of the column ([`super::live`]).
pub struct HtmlBody {
    pub frame: gtk::Box,
    pub holder: BodyBox,
    pub view: Option<MessageView>,
    pub style: BodyStyle,
    pub html: Arc<str>,
    pub parts: Rc<InlineParts>,
    pub remote_images: bool,
    pub remote_allowed: bool,
    /// Its quoted history collapses behind "•••".
    pub collapse_quotes: bool,
    /// The user opened the collapsed quotes of the view's current load: heights measured now
    /// aren't kept, the cache's key says collapsed.
    pub quotes_open: bool,
    /// The height last measured, and the width it was measured at.
    pub measured: Option<(i32, i32)>,
    /// The view's current load has fired its `load` event.
    pub loaded: bool,
    /// The view's current load has painted a frame.
    pub painted: bool,
    /// When the view's current load last changed: attached, loaded, or measured at another
    /// height or width. Laid out unseen, it is left undrawn only [`super::live::SETTLE`] after
    /// (a late image or transition is measured while it waits).
    pub changed: Instant,
    /// The view went back to the pool while the reader was hidden.
    pub released: bool,
    /// Tags the events of the view's current load.
    pub token: u64,
}

impl HtmlBody {
    /// Measured at `width` (within a pixel), as the reader shows it now.
    pub fn measured_at(&self, width: i32) -> bool {
        self.measured
            .is_some_and(|(_, measured)| width > 1 && (measured - width).abs() <= 1)
    }
}

/// An expanded message's card once its message is read.
pub struct Card {
    pub widgets: CardWidgets,
    pub body: ShownBody,
    pub html: Option<HtmlBody>,
    pub attachments: usize,
    /// The sender's address, lower case, for the senders whose images load (ML-058).
    pub sender: String,
    /// Its body has remote images (ML-058).
    pub remote_images: bool,
    /// The header's Reply, Reply all and Forward buttons, or Edit draft (M6).
    pub replies: Vec<(sealwax_domain::actions::ActionId, gtk::Button)>,
    /// Its headers: whom the inline reply answers, as Reply or Reply All (issue #8; the fork's
    /// phase B).
    pub headers: sealwax_domain::model::Headers,
}

impl Card {
    /// What takes the focus when the reader moves it into this card: its body once it has one,
    /// else its header.
    pub fn focus_target(&self) -> Option<gtk::Widget> {
        match self.html.as_ref().and_then(|html| html.view.as_ref()) {
            Some(view) => Some(view.web_view().clone().upcast()),
            None => self
                .widgets
                .focus
                .clone()
                .or_else(|| Some(self.widgets.header.clone().upcast())),
        }
    }

    /// Its body, once it has one to take the focus (the HTML view, a plain body's text): Enter
    /// there is the conversation's (Quick Reply), where on the header it collapses the card.
    pub fn body_target(&self) -> Option<gtk::Widget> {
        match self.html.as_ref() {
            Some(html) => html
                .view
                .as_ref()
                .map(|view| view.web_view().clone().upcast()),
            None => self
                .widgets
                .focus
                .clone()
                .filter(|focus| focus.is_ancestor(&self.widgets.body)),
        }
    }
}

/// One message of the page: its row, and its card while expanded.
pub struct Item {
    pub message: ConversationMessage,
    /// Holds the row or the card, in the page's list.
    pub slot: gtk::Box,
    pub row: gtk::Button,
    pub expanded: bool,
    pub card: Option<Card>,
    /// The newest load of its card; older answers are dropped.
    pub load: u64,
    /// Its body was reported shown ([`super::Opened`]).
    pub opened: bool,
}

impl Item {
    pub fn id(&self) -> MessageId {
        self.message.summary.id
    }
}

/// Messages folded into one "N earlier messages" row.
pub struct Fold {
    pub row: gtk::Button,
    pub messages: Vec<ConversationMessage>,
}

pub enum Entry {
    Message(Box<Item>),
    Fold(Fold),
}

/// The conversation on screen, or being laid out unseen.
pub struct Page {
    /// The `show` it came from, or the warm-up that built it: card loads and view events of
    /// another page are dropped.
    pub key: u64,
    pub selected: MessageId,
    pub thread: Option<ThreadId>,
    /// The thread's column (what the stage holds): the content at a comfortable width, centred.
    pub root: adw::Clamp,
    /// The heading, the conversation and the quick reply, one under the other.
    pub content: gtk::Box,
    pub heading: Option<gtk::Label>,
    /// The quiet line under the heading while the conversation waits in Reminders (Remind Me,
    /// phase C): when it comes back.
    pub reminder: Option<gtk::Box>,
    pub list: gtk::Box,
    pub entries: Vec<Entry>,
    /// Every row of the conversation (copies and rows left out too): a change event that names
    /// one of them changes what the page shows.
    pub members: HashSet<MessageId>,
    /// The page shows that the message is gone.
    pub missing: bool,
    /// The message the user last moved to: with Next or Previous Message in Conversation, a
    /// press in it, or Tab into it from another of its messages (`Inner::moved_to`); `None`
    /// before any move, and again once the conversation is left (`Inner::start_afresh`): `n`/`p`
    /// then start from the message whose body holds the focus, else the selected one, and the
    /// inline reply answers its own target (the selected message while the list's rows are
    /// messages).
    pub current: Option<MessageId>,
    /// The conversation's subject, as its list row says it (the store's `Conversation::subject`);
    /// empty when it has none: the heading then says the selected message's, as it does while
    /// the list's rows are messages (`show::relabel`).
    pub subject: String,
    /// The quick reply under the conversation (M6, ML-068), once its messages are read; over
    /// it, newest first.
    pub quick: Option<Rc<super::reply::QuickReply>>,
    /// The newest message is on top: `entries` stay oldest first, the screen shows them turned.
    pub newest_first: bool,
    /// Its first cards are in: a page laid out ahead can come on screen at once.
    pub built: bool,
}

impl Page {
    /// Puts `widget` after `after` in the conversation's order (`None`: before every other),
    /// which newest first shows above it.
    pub fn place(&self, widget: &impl IsA<gtk::Widget>, after: Option<&gtk::Widget>) {
        match (self.newest_first, after) {
            (false, after) => self.list.insert_child_after(widget, after),
            (true, Some(after)) => widget.insert_before(&self.list, Some(after)),
            (true, None) => self.list.append(widget),
        }
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.entries.iter().filter_map(|entry| match entry {
            Entry::Message(item) => Some(&**item),
            Entry::Fold(_) => None,
        })
    }

    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut Item> {
        self.entries.iter_mut().filter_map(|entry| match entry {
            Entry::Message(item) => Some(&mut **item),
            Entry::Fold(_) => None,
        })
    }

    pub fn item(&self, id: MessageId) -> Option<&Item> {
        self.items().find(|item| item.id() == id)
    }

    pub fn item_mut(&mut self, id: MessageId) -> Option<&mut Item> {
        self.items_mut().find(|item| item.id() == id)
    }

    /// The selected message's card, once loaded.
    pub fn selected_card(&self) -> Option<&Card> {
        self.item(self.selected).and_then(|item| item.card.as_ref())
    }

    /// The messages of the conversation, folded ones included.
    pub fn message_count(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| match entry {
                Entry::Message(_) => 1,
                Entry::Fold(fold) => fold.messages.len(),
            })
            .sum()
    }

    /// Whether an expanded message's body is HTML (its height comes from WebKit).
    pub fn has_html(&self) -> bool {
        self.items()
            .any(|item| item.card.as_ref().is_some_and(|card| card.html.is_some()))
    }

    /// Expanded items whose card is in the page.
    pub fn expanded(&self) -> Vec<MessageId> {
        self.items()
            .filter(|item| item.expanded)
            .map(Item::id)
            .collect()
    }

    /// The messages of the conversation, oldest first, folded ones included.
    pub fn messages(&self) -> Vec<&ConversationMessage> {
        let mut messages = Vec::new();
        for entry in &self.entries {
            match entry {
                Entry::Message(item) => messages.push(&item.message),
                Entry::Fold(fold) => messages.extend(fold.messages.iter()),
            }
        }
        messages
    }

    /// Whether every expanded message shows its body (not "Downloading…", a failure, or a
    /// message gone): only such a page is kept laid out ahead.
    pub fn complete(&self) -> bool {
        !self.missing
            && self
                .items()
                .filter(|item| item.expanded)
                .all(|item| item.card.as_ref().is_some_and(|card| card.body.is_shown()))
    }

    /// Says every date on the page as of `now`: its rows' times and its cards' dates were
    /// written when it was built, which for a page laid out ahead was a while ago.
    pub fn redate(&self, now: i64) {
        for item in self.items() {
            match &item.card {
                Some(card) => card.widgets.redate(now),
                None => redate_row(&item.row, &item.message, now),
            }
        }
    }

    /// Every HTML body that holds a view.
    pub fn live_bodies(&self) -> Vec<MessageId> {
        self.items()
            .filter(|item| {
                item.card
                    .as_ref()
                    .and_then(|card| card.html.as_ref())
                    .is_some_and(|html| html.view.is_some())
            })
            .map(Item::id)
            .collect()
    }
}

/// The page's top: the subject as the conversation's heading (the selected message's subject).
pub fn heading(subject: &str) -> gtk::Label {
    let heading = gtk::Label::builder()
        .label(heading_text(subject))
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .css_classes(["sw-subject-heading"])
        .build();
    heading.set_widget_name("reader-subject");
    heading
}

/// The quiet line under the heading of a conversation waiting in Reminders (phase C): a clock
/// and when it comes back ([`show_reminder`]); hidden while it has no reminder.
pub fn reminder_line() -> gtk::Box {
    let line = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .visible(false)
        .css_classes(["sw-reader-reminder"])
        .build();
    line.set_widget_name("reader-reminder");
    let clock = gtk::Image::from_gicon(&gtk4::gio::ThemedIcon::from_names(
        &crate::list::row::REMIND_ICONS,
    ));
    clock.set_accessible_role(gtk::AccessibleRole::Presentation);
    line.append(&clock);
    line.append(&gtk::Label::builder().xalign(0.0).build());
    line
}

/// Shows `text` on a [`reminder_line`] ("Reminder set for tomorrow 08:00"), or hides it.
pub fn show_reminder(line: &gtk::Box, text: Option<&str>) {
    if let Some(label) = line.last_child().and_downcast::<gtk::Label>()
        && label.label() != text.unwrap_or_default()
    {
        label.set_label(text.unwrap_or_default());
    }
    if line.is_visible() != text.is_some() {
        line.set_visible(text.is_some());
    }
}

/// The text a [`reminder_line`] shows, while it shows (the self-test).
pub fn reminder_text(line: &gtk::Box) -> Option<String> {
    line.is_visible()
        .then(|| line.last_child().and_downcast::<gtk::Label>())
        .flatten()
        .map(|label| label.label().to_string())
}

/// What the heading says for `subject`: the conversation's subject without "Re:" or "Fwd:"
/// (and their Nordic, German, French and Chinese twins), as the list's rows show it.
pub fn heading_text(subject: &str) -> &str {
    match base_subject(subject).trim() {
        "" => "(No subject)",
        subject => subject,
    }
}

fn sender_name(summary: &MessageSummary) -> &str {
    if summary.sender_name.trim().is_empty() {
        &summary.sender_email
    } else {
        &summary.sender_name
    }
}

/// What a draft says in place of its date, in the draft colour, as the list's rows mark a
/// conversation holding one (Gmail's): an unsent reply read as a sent message (phase C's visual
/// QA).
pub const DRAFT: &str = "Draft";

/// Whether `message` is the user's unsent draft (a Drafts folder's copy).
pub fn is_draft(message: &ConversationMessage) -> bool {
    message.role == FolderRole::Drafts
}

/// A collapsed message: one quiet line with the sender, the snippet and the time, a button that
/// expands it (click, Enter or Space).
pub fn collapsed_row(message: &ConversationMessage, now: i64) -> gtk::Button {
    let row = gtk::Button::builder()
        .css_classes(["flat", "sw-collapsed-message"])
        .build();
    row.set_widget_name("reader-row");
    row.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
    fill_row(&row, message, now);
    row
}

/// Puts `message`'s line in `row`, again when its flags change: an unread one gets the dot and
/// the sender in the stronger weight; a draft says so in place of its time.
pub fn fill_row(row: &gtk::Button, message: &ConversationMessage, now: i64) {
    let summary = &message.summary;
    let draft = is_draft(message);
    let zone = glib::TimeZone::local();
    let name = sender_name(summary);
    let unread = !summary.flags.is_seen();
    let sender = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .width_chars(14)
        .max_width_chars(22)
        .css_classes(if unread {
            vec!["sw-reader-row-sender", "sw-strong"]
        } else {
            vec!["sw-reader-row-sender"]
        })
        .build();
    sender.set_widget_name("reader-row-sender");
    let snippet = gtk::Label::builder()
        .label(summary.snippet.lines().next().unwrap_or_default())
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .width_chars(1)
        .css_classes(["sw-secondary", "sw-reader-row-snippet"])
        .build();
    let time = if draft {
        DRAFT.to_owned()
    } else {
        format::short_date(summary.date, now, &zone)
    };
    let when = gtk::Label::builder()
        .label(&time)
        .css_classes(if draft {
            ["sw-reader-draft", "numeric", "sw-reader-row-date"]
        } else {
            ["sw-secondary", "numeric", "sw-reader-row-date"]
        })
        .tooltip_text(format::full_date(summary.date, &zone))
        .build();
    when.set_widget_name("reader-row-date");
    let line = gtk::Box::builder()
        .css_classes(["sw-reader-row-line"])
        .build();
    if unread {
        let dot = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["sw-unread-dot", "sw-reader-row-dot"])
            .build();
        line.append(&dot);
    }
    line.append(&sender);
    line.append(&snippet);
    line.append(&when);
    row.set_child(Some(&line));
    row.update_property(&[
        gtk::accessible::Property::Label(&row_label(name, &time, unread)),
        gtk::accessible::Property::Description(summary.snippet.lines().next().unwrap_or_default()),
    ]);
}

/// Says a collapsed row's time as of `now` (a page laid out ahead, shown later); a draft's says
/// Draft whenever it is.
pub fn redate_row(row: &gtk::Button, message: &ConversationMessage, now: i64) {
    if is_draft(message) {
        return;
    }
    let summary = &message.summary;
    let time = format::short_date(summary.date, now, &glib::TimeZone::local());
    let label = row
        .child()
        .and_then(|line| line.last_child())
        .and_downcast::<gtk::Label>()
        .filter(|label| label.widget_name() == "reader-row-date" && label.label() != time);
    if let Some(label) = label {
        label.set_label(&time);
        row.update_property(&[gtk::accessible::Property::Label(&row_label(
            sender_name(summary),
            &time,
            !summary.flags.is_seen(),
        ))]);
    }
}

/// What a screen reader says for a collapsed row: the sender, the time, unread, collapsed.
pub fn row_label(sender: &str, time: &str, unread: bool) -> String {
    let unread = if unread { ", unread" } else { "" };
    format!("{sender}, {time}{unread}, collapsed")
}

/// The row that stands for folded messages, between hairlines: their count in a quiet round
/// badge, then "more messages" (Superhuman's fold; a glyph of stacked rules read as a menu).
pub fn fold_row(count: usize) -> gtk::Button {
    let badge = gtk::Label::builder()
        .valign(gtk::Align::Center)
        .css_classes(["sw-small", "numeric", "sw-reader-fold-badge"])
        .build();
    badge.set_widget_name("reader-fold-badge");
    let words = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["sw-small", "sw-reader-fold-count"])
        .build();
    let line = gtk::Box::builder()
        .css_classes(["sw-reader-fold-line"])
        .build();
    line.append(&badge);
    line.append(&words);
    let row = gtk::Button::builder()
        .child(&line)
        .css_classes(["flat", "sw-reader-fold"])
        .build();
    row.set_widget_name("reader-fold");
    set_fold_count(&row, count);
    row
}

/// A fold row's count, in its badge, its words and what a screen reader says.
pub fn set_fold_count(row: &gtk::Button, count: usize) {
    let line = row.child();
    let labels = std::iter::successors(line.and_then(|line| line.first_child()), |child| {
        child.next_sibling()
    })
    .filter_map(|child| child.downcast::<gtk::Label>().ok());
    for (index, label) in labels.enumerate() {
        match index {
            0 => label.set_label(&count.to_string()),
            _ => label.set_label(fold_words(count)),
        }
    }
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}, folded; show them",
        fold_text(count)
    ))]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Flags, FolderId, FolderRole};

    fn message(id: i64, read: bool) -> ConversationMessage {
        ConversationMessage {
            summary: MessageSummary {
                id: MessageId(id),
                folder: FolderId(1),
                thread: ThreadId(1),
                sender_name: "Kari Nordmann".into(),
                sender_email: "kari@example.test".into(),
                subject: "Hyttetur".into(),
                snippet: "Hei".into(),
                date: id,
                flags: if read { Flags::SEEN } else { Flags::NONE },
                has_attachments: false,
                thread_count: 1,
            },
            role: FolderRole::Inbox,
            copies: Vec::new(),
        }
    }

    #[test]
    fn the_selected_the_unread_and_the_latest_expand() {
        let messages: Vec<ConversationMessage> =
            (1..=6).map(|id| message(id, id != 3 && id != 4)).collect();
        assert_eq!(
            initially_expanded(&messages, MessageId(1)),
            [true, false, true, true, false, true]
        );
        let all_read: Vec<ConversationMessage> = (1..=3).map(|id| message(id, true)).collect();
        assert_eq!(
            initially_expanded(&all_read, MessageId(3)),
            [false, false, true]
        );
        assert_eq!(initially_expanded(&all_read[..1], MessageId(1)), [true]);
    }

    #[test]
    fn runs_of_read_messages_fold_keeping_the_first_and_the_one_before_each_card() {
        // The six-message trip: the unread fifth and the latest expanded; four rows stay rows.
        let trip = [false, false, false, false, true, true];
        assert_eq!(initially_shown(&trip), vec![true; 6]);
        // Nine messages, the latest alone expanded: the first, "6 more messages", the eighth.
        let mut nine = vec![false; 9];
        nine[8] = true;
        let shown = initially_shown(&nine);
        assert_eq!(folds(&shown), vec![1..7]);
        // Two runs, each folding on its own; a run of two between cards stays.
        let mut runs = vec![false; 15];
        runs[6] = true;
        runs[9] = true;
        runs[14] = true;
        assert_eq!(folds(&initially_shown(&runs)), [1..5, 10..13]);
        // 150 messages, the latest expanded: everything but the first and the 149th folds.
        let mut long = vec![false; 150];
        long[149] = true;
        let shown = initially_shown(&long);
        assert_eq!(shown.iter().filter(|shown| **shown).count(), 3);
        assert_eq!(folds(&shown), vec![1..148]);
        assert_eq!(reveal(1..148), 98..148);
        assert_eq!(reveal(1..98), 48..98);
        assert_eq!(reveal(1..48), 1..48);
        assert_eq!(folds(&[false, true, false]), [0..1, 2..3]);
        assert_eq!(initially_shown(&[]), Vec::<bool>::new());
        assert_eq!(fold_text(147), "147 more messages");
        assert_eq!(fold_text(1), "1 more message");
    }

    #[test]
    fn the_heading_is_the_subject_without_reply_prefixes() {
        assert_eq!(
            heading_text("Re: Cabin trip in November?"),
            "Cabin trip in November?"
        );
        assert_eq!(heading_text("SV: Fwd: RE: Lunsj"), "Lunsj");
        assert_eq!(heading_text("  Reply all  "), "Reply all");
        assert_eq!(heading_text("Re:"), "(No subject)");
        assert_eq!(heading_text(""), "(No subject)");
    }

    #[test]
    fn rows_say_who_when_and_that_they_are_collapsed() {
        assert_eq!(
            row_label("Kari Nordmann", "09:30", true),
            "Kari Nordmann, 09:30, unread, collapsed"
        );
        assert_eq!(row_label("Ola", "Mon", false), "Ola, Mon, collapsed");
        assert_eq!(size_text(12), "12 messages");
    }
}
