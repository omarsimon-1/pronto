//! The message row (the Superhuman pass, 2026-10-28; docs/superhuman/rows.md): one line of
//! unread dot · sender column (the participants, a quiet message count, a Draft marker) ·
//! subject, then the snippet · paperclip and star · the date, right-aligned. A list too narrow
//! for that line (the split layout's 380 px pane) shows each row on two lines instead: the
//! sender column and the date, then the subject and the snippet. No avatars unless
//! [`RowContext::avatars`] asks for them. What a row shows comes as [`RowFields`], from a
//! message today and from a conversation once the list has conversation rows.
//!
//! [`RowLayout`] places the parts: the sender column is `--sw-size-list-sender` wide, less in a
//! list too narrow for that to leave the subject its room ([`SENDER_SHARE`], phase B), its
//! names the longest text of whole names that fits it ([`participants::columns`], phase C), the
//! subject takes what it needs but at most [`SUBJECT_SHARE`] of the room when the snippet doesn't
//! fit beside it, the date has a column as wide as the widest date can be, so the paperclip and
//! the star line up on every row, and a row's height never depends on its text or on the fonts it
//! falls back to (`--sw-size-list-row`, one and a half of it on two lines), so rows never change
//! size.
//!
//! Binding is kept cheap, because GtkListView lays out up to ~205 rows in one frame on a jump
//! (S3): text is cut to what can show before it is set (−15 % layout), each text is one label
//! (one Pango layout), the list has no ligatures (style.css), weight comes from Pango
//! attributes, the dot and the indicators fade with opacity, and nothing changes unless it
//! differs from what the row shows. The only class that changes is the row's state,
//! `sw-unread`, which colours its text. While the list moves faster than ~30,000 px/s rows show
//! their placeholder look instead of text (S3).
//!
//! In the select mode (ML-054) a light checkbox shows round the unread dot's place, so the text
//! doesn't move and an unread row keeps its dot; it follows the row's selection and toggles it.
//! Hovering a row shows its quick actions (Done, Remind Me, Delete, Star: the toolbar's and the
//! batch bar's order) over the date; only the pointer shows them (phase B: the selected row
//! keeps its date). They are built the first time a row shows them and never move the row's
//! text.
//!
//! Remind Me (phase C, docs/superhuman/remind.md): in an Inbox a conversation back from
//! Reminders shows a clock in the reminder's purple before the paperclip, in a slot every row of
//! that Inbox keeps while one does (so the paperclips and stars stay in line), and the Reminders
//! view's date column says when each conversation comes back ("TUE 08:00", "SOMEDAY"), sized
//! for those ([`RowReminders`]).

use super::item::MessageItem;
use super::sections::LABELS;
use crate::window::glyph;
use crate::window::panes::icon_button;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gio, glib, pango};
use sealwax_domain::actions::ActionId;
use sealwax_domain::message::base_subject;
use sealwax_domain::model::{ConversationSummary, MessageId, MessageSummary, ThreadId};
pub use sealwax_domain::participants::Person as Participant;
use sealwax_domain::participants::{self, Emphasis};
use sealwax_domain::remind;
use sealwax_domain::theme::{avatar_index, initials};
use sealwax_infrastructure::store::{ListRow, RowKey};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A row's quick actions, shown while the pointer is over it (DESIGN.md "Message list").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quick {
    Archive,
    Remind,
    Delete,
    Star,
}

impl Quick {
    /// In the order they show: Done (Archive) and Remind Me first, as Superhuman's check and
    /// clock, then Delete and Star, in the reader's toolbar's and the batch bar's order (phase C,
    /// the visual QA: one order everywhere).
    pub const ALL: [Quick; 4] = [Quick::Archive, Quick::Remind, Quick::Delete, Quick::Star];

    pub fn action(self) -> ActionId {
        match self {
            Quick::Archive => ActionId::Archive,
            Quick::Remind => ActionId::RemindMe,
            Quick::Delete => ActionId::Delete,
            Quick::Star => ActionId::Star,
        }
    }

    /// Archive is Superhuman's Done, a check mark (Adwaita has no `mail-archive-symbolic`);
    /// Remind Me its clock.
    fn icons(self, starred: bool) -> &'static [&'static str] {
        match self {
            Quick::Archive => &["object-select-symbolic"],
            Quick::Remind => &REMIND_ICONS,
            Quick::Delete => &["user-trash-symbolic"],
            Quick::Star if starred => &["starred-symbolic"],
            Quick::Star => &["non-starred-symbolic", "starred-symbolic"],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Quick::Archive => "archive",
            Quick::Remind => "remind",
            Quick::Delete => "delete",
            Quick::Star => "star",
        }
    }
}

/// Remind Me's clock, the quick action's and the returned mark's: Adwaita's alarm clock, one
/// name, so a theme without it shows nothing rather than another glyph.
pub const REMIND_ICONS: [&str; 1] = ["alarm-symbolic"];

/// The class of the clock quick action where nothing can be reminded ([`RowReminders::off`]).
const QUICK_OFF: &str = "sw-quick-off";

/// Dims the clock quick action of `quick` when Remind Me is `off` for a reason, which its
/// tooltip then names as the toolbar's does ("Remind Me… (h) — mail in Sent can't be reminded":
/// the third review); `tooltip` is what it says otherwise.
fn show_quick_off(quick: &QuickActions, off: Option<&str>, tooltip: &str) {
    let Some((_, button)) = quick
        .buttons
        .iter()
        .find(|(action, _)| *action == Quick::Remind)
    else {
        return;
    };
    if button.has_css_class(QUICK_OFF) != off.is_some() {
        if off.is_some() {
            button.add_css_class(QUICK_OFF);
        } else {
            button.remove_css_class(QUICK_OFF);
        }
    }
    let text = match off {
        Some(reason) => format!("{tooltip} — {reason}"),
        None => tooltip.to_owned(),
    };
    if button.tooltip_text().as_deref() != Some(text.as_str()) {
        button.set_tooltip_text(Some(&text));
        button.update_property(&[gtk::accessible::Property::Label(&text)]);
    }
}

/// What a list's rows show of Remind Me (phase C).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowReminders {
    /// The list shows an Inbox or All Inboxes and a conversation there is back from Reminders:
    /// it carries the clock mark, in a slot every row keeps while one does (so paperclips and
    /// stars stay in line; without one, no row gives up the room).
    pub marks: bool,
    /// The list shows a Reminders folder's conversations: the date column is sized for wake
    /// times, and the rows have no date headers.
    pub wakes: bool,
    /// The conversations back from Reminders.
    pub returned: HashSet<ThreadId>,
    /// Why nothing the list shows can be reminded (Sent, Drafts, Trash, Junk:
    /// `remind::unavailable`): the rows' clock quick action is dimmed as the toolbar's Remind
    /// Me is, its tooltip names the reason, and a click on it says why (the second review: it
    /// looked as if it worked).
    pub off: Option<&'static str>,
    /// The reminders waiting in the Reminders folder the list shows: while its view lists none
    /// of them, they are on their way there (a server account's moves wait for the undo window
    /// and the server), and the empty state says so rather than "No reminders" (the third
    /// review).
    pub arriving: usize,
}

/// What one row shows of Remind Me.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowReminder {
    /// Back from Reminders: the clock mark.
    pub returned: bool,
    /// When it comes back, shown in the date's place (the Reminders view).
    pub wake: Option<i64>,
}

type QuickCallback = Rc<dyn Fn(ActionId, ListRow)>;

/// A row's quick actions: their box, each button, and whether the star shows starred.
struct QuickActions {
    actions: gtk::Box,
    buttons: Vec<(Quick, gtk::Button)>,
    starred: Cell<bool>,
}

/// Characters a row shows at most (S3): the sender column's, whatever the list's width (it has
/// a fixed width), and the subject's and the snippet's at a 400 px list, scaled with the list.
/// One line of a 400 px list holds about 60 of them.
const CUT: (usize, usize, usize) = (48, 72, 72);

/// A row takes one line when its content is at least this many sender columns wide past the
/// gutter: the column itself, about one for the subject and snippet and a half for the date.
const ONE_LINE_COLUMNS: f64 = 2.5;
/// When the subject and the snippet don't both fit, the subject takes at most this share of
/// their room, so a long subject never hides the snippet.
pub const SUBJECT_SHARE: f64 = 0.7;
/// The sender column takes this share of a one-line row's content, between
/// [`SENDER_FLOOR`] of its full width and the full width (`--sw-size-list-sender`): full from
/// about 1,000 px, 134 px in the owner's 710 px half tile, where a fixed column left the
/// subject and the snippet half the row (Superhuman's names take about a fifth of its list).
pub const SENDER_SHARE: f64 = 0.2;
pub const SENDER_FLOOR: f64 = 0.7;

const TINTS: [&str; 7] = [
    "sw-avatar-tint-0",
    "sw-avatar-tint-1",
    "sw-avatar-tint-2",
    "sw-avatar-tint-3",
    "sw-avatar-tint-4",
    "sw-avatar-tint-5",
    "sw-avatar-tint-6",
];

/// What the rows of one list share.
pub struct RowContext {
    /// Rows bound now show their placeholder look: the list moves too fast to read.
    pub skeleton: Cell<bool>,
    /// How much more text fits than at 400 px (the list's width / 400, in half steps, ≥ 1).
    pub scale: Cell<f32>,
    /// This year, for dates.
    pub year: Cell<i32>,
    pub bound: RefCell<Vec<MessageRow>>,
    pub headers: RefCell<Vec<SectionHeader>>,
    /// Rows bound so far.
    pub binds: Cell<u64>,
    /// Row widgets created so far.
    pub rows_created: Cell<u64>,
    /// Rows show a checkbox in the unread dot's place.
    pub select_mode: Cell<bool>,
    /// Rows created from now on show the sender's avatar after the dot (off: the Superhuman
    /// pass dropped them from the list; the reader keeps its own).
    pub avatars: Cell<bool>,
    /// The date column's width for a font: the widest date a row can show (in the Reminders
    /// view, the widest wake time: `true`).
    date_column: RefCell<Option<(pango::FontDescription, bool, i32)>>,
    /// Remind Me's marks and wake times (phase C).
    reminders: RefCell<RowReminders>,
    /// "Now" when the list has a clock of its own (the self-test's), for wake times.
    pub clock: Cell<Option<i64>>,
    /// The sender column's width as a row was last laid out (0 before), the same on every row
    /// of the list: a row bound since picks its names for it.
    column: Cell<i32>,
    /// Draws nothing: its width is the sender column's at its widest, gap included
    /// (`--sw-size-list-sender`), which the column takes in a wide list ([`SENDER_SHARE`]).
    /// One for the list, beside its rows, so no row carries a widget for it.
    ruler: OnceCell<gtk::Widget>,
    quick: RefCell<Option<QuickCallback>>,
    /// Each quick action's tooltip, with its keys.
    quick_tooltips: RefCell<Vec<(Quick, String)>>,
    /// When a scheduled send goes (Unix ms), by its Drafts-folder copy (`outbox`): that copy's
    /// row, and a conversation's whose latest message it is (a Drafts view's), say when in
    /// their date's place (BRIEF-C: "Sending Tue 08:00").
    scheduled: RefCell<HashMap<MessageId, i64>>,
    ai_drafts: RefCell<std::collections::HashSet<sealwax_domain::model::ThreadId>>,
}

impl RowContext {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            skeleton: Cell::new(false),
            scale: Cell::new(1.0),
            year: Cell::new(
                glib::DateTime::now_local()
                    .map(|now| now.year())
                    .unwrap_or_default(),
            ),
            bound: RefCell::new(Vec::new()),
            headers: RefCell::new(Vec::new()),
            binds: Cell::new(0),
            rows_created: Cell::new(0),
            select_mode: Cell::new(false),
            avatars: Cell::new(false),
            date_column: RefCell::new(None),
            reminders: RefCell::new(RowReminders::default()),
            clock: Cell::new(None),
            column: Cell::new(0),
            ruler: OnceCell::new(),
            quick: RefCell::new(None),
            quick_tooltips: RefCell::new(Vec::new()),
            scheduled: RefCell::new(HashMap::new()),
            ai_drafts: RefCell::default(),
        })
    }

    /// When the scheduled send whose Drafts-folder copy is `message` goes, if it is one.
    fn scheduled_at(&self, message: MessageId) -> Option<i64> {
        self.scheduled.borrow().get(&message).copied()
    }

    /// The scheduled sends' Drafts-folder copies and when each goes ([`RowContext::scheduled`]);
    /// the rows bound now show it at once.
    pub fn set_scheduled(&self, scheduled: HashMap<MessageId, i64>) {
        if *self.scheduled.borrow() == scheduled {
            return;
        }
        self.scheduled.replace(scheduled);
        for row in self.bound.borrow().iter() {
            row.refresh();
        }
    }

    pub fn set_ai_drafts(
        &self,
        drafts: std::collections::HashSet<sealwax_domain::model::ThreadId>,
    ) {
        if *self.ai_drafts.borrow() != drafts {
            self.ai_drafts.replace(drafts);
            for row in self.bound.borrow().iter() {
                row.refresh();
            }
        }
    }

    /// Runs `callback` when a row's quick action is clicked, with that row.
    pub fn set_quick(&self, callback: impl Fn(ActionId, ListRow) + 'static) {
        self.quick.replace(Some(Rc::new(callback)));
    }

    pub fn set_quick_tooltips(&self, tooltips: Vec<(Quick, String)>) {
        self.quick_tooltips.replace(tooltips);
        for row in self.bound.borrow().iter() {
            row.update_quick_tooltips();
        }
    }

    fn quick_tooltip(&self, quick: Quick) -> String {
        self.quick_tooltips
            .borrow()
            .iter()
            .find(|(candidate, _)| *candidate == quick)
            .map_or_else(
                || quick.action().title().to_owned(),
                |(_, text)| text.clone(),
            )
    }

    /// The date column's width in `time`'s font: the widest of the dates a row can show
    /// ([`date_samples`]) with the label's own margins, measured once a font, so every row's
    /// date column, and the indicators before it, line up (the visual QA: a paperclip 16 px
    /// further left on the rows that said "YESTERDAY"). The Reminders view measures its wake
    /// times instead ([`wake_samples`]: "WED 00:00" is wider than any date).
    fn date_width(&self, time: &gtk::Label) -> i32 {
        let font = time.pango_context().font_description().unwrap_or_default();
        let wakes = self.reminders.borrow().wakes;
        if let Some((measured, measured_wakes, width)) = self.date_column.borrow().as_ref()
            && *measured == font
            && *measured_wakes == wakes
        {
            return *width;
        }
        let shown = time.layout();
        // The label's margins and padding: its natural width past its text's.
        let (_, natural, _, _) = time.measure(gtk::Orientation::Horizontal, -1);
        let edges = (natural - shown.pixel_size().0).max(0);
        let layout = shown.copy();
        let samples = if wakes {
            wake_samples(self.year.get())
        } else {
            date_samples(self.year.get())
        };
        let widest = samples
            .iter()
            .map(|text| {
                layout.set_text(text);
                layout.pixel_size().0
            })
            .max()
            .unwrap_or_default();
        let width = widest + edges;
        self.date_column.replace(Some((font, wakes, width)));
        width
    }

    /// Remind Me's marks and wake times for the rows; the bound rows show them at once. Replies
    /// whether anything changed.
    pub fn set_reminders(&self, reminders: RowReminders) -> bool {
        if *self.reminders.borrow() == reminders {
            return false;
        }
        self.reminders.replace(reminders);
        let rows = self.bound.borrow().clone();
        for row in rows {
            row.refresh();
        }
        true
    }

    /// What the rows show of Remind Me now.
    pub fn reminders(&self) -> RowReminders {
        self.reminders.borrow().clone()
    }

    /// Why nothing the list shows can be reminded ([`RowReminders::off`]).
    fn remind_off(&self) -> Option<&'static str> {
        self.reminders.borrow().off
    }

    /// What `row` shows of Remind Me. A waiting conversation's row in its Reminders folder is
    /// dated by its wait key (the store's order there): it shows the wake time that stands
    /// for, wherever such a row shows, so it is never read as a date. Message rows keep their
    /// own dates, which their order follows (Group by Conversation off).
    fn reminder_of(&self, row: &ListRow) -> RowReminder {
        let reminders = self.reminders.borrow();
        let (thread, wake) = match row {
            ListRow::Message(summary) => (summary.thread, None),
            ListRow::Conversation(summary) => (summary.thread, remind::wake_of_key(summary.date)),
        };
        RowReminder {
            returned: reminders.marks && reminders.returned.contains(&thread),
            wake,
        }
    }

    /// "Now" for wake times: the list's clock, else the real one.
    fn now(&self) -> i64 {
        self.clock.get().unwrap_or_else(|| {
            glib::DateTime::now_local()
                .map(|now| now.to_unix())
                .unwrap_or_default()
        })
    }

    /// The widget whose CSS width is the sender column's at its widest (`.sw-row-ruler`):
    /// laid out in the rows' list, never drawn.
    pub fn set_ruler(&self, ruler: &impl IsA<gtk::Widget>) {
        let _ = self.ruler.set(ruler.clone().upcast());
    }

    /// The sender column at its widest, gap included (GTK keeps the ruler's measure until its
    /// CSS changes).
    fn sender_width(&self) -> i32 {
        self.ruler
            .get()
            .map_or(0, |ruler| measure(ruler, gtk::Orientation::Horizontal).0)
    }

    fn limits(&self) -> (usize, usize, usize) {
        let scale = self.scale.get();
        let cut = |chars: usize| (chars as f32 * scale) as usize;
        (CUT.0, cut(CUT.1), cut(CUT.2))
    }
}

fn cut(text: &str, chars: usize) -> &str {
    match text.char_indices().nth(chars) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// How wide `label` would show `text` with `emphasis`, laid out as the label lays it out: its
/// font and its CSS's attributes (the list's font features), its own weights replaced.
fn text_width(label: &gtk::Label, text: &str, emphasis: &Emphasis) -> i32 {
    let layout = label.layout().copy();
    layout.set_width(-1);
    layout.set_ellipsize(pango::EllipsizeMode::None);
    let attributes = layout.attributes().unwrap_or_default();
    let _ = attributes.filter(|attribute| attribute.type_() == pango::AttrType::Weight);
    let bold = |start: u32, end: u32| {
        let mut weight = pango::AttrInt::new_weight(pango::Weight::Bold);
        weight.set_start_index(start);
        weight.set_end_index(end);
        attributes.insert(weight);
    };
    match emphasis {
        Emphasis::None => {}
        Emphasis::All => bold(0, u32::MAX),
        Emphasis::Ranges(ranges) => {
            for (start, end) in ranges {
                bold(*start, *end);
            }
        }
    }
    layout.set_text(text);
    layout.set_attributes(Some(&attributes));
    layout.pixel_size().0
}

/// The date column (Superhuman's): "10:32" today, "OCT 28" before this day this year, the month
/// as the locale abbreviates it, in capitals, and "10/4/25" before, its numbers in the locale's
/// order. `section` is the row's date section, so a row needs no clock of its own. Yesterday is
/// a date too and an older year a short one (phase B, the visual QA): "YESTERDAY" on fifteen
/// rows under a "Yesterday" header was the loudest thing in the list, and its width moved the
/// indicators; "OCT 4 2025" would have made the column half as wide again for every row.
pub fn time_text(date: i64, section: usize, year: i32) -> String {
    date_text(date, section, year, true)
}

/// The dates the column must hold whatever the rows: a time, each month's widest day this
/// year, and the widest date before it (digits are tabular, so one day and one year stand for
/// all).
fn date_samples(year: i32) -> Vec<String> {
    let mut samples = vec!["00:00".to_owned()];
    for month in 1..=12 {
        let Ok(day) = glib::DateTime::from_local(year, month, 28, 12, 0, 0.0) else {
            continue;
        };
        samples.push(time_text(day.to_unix(), 2, year));
    }
    if let Ok(day) = glib::DateTime::from_local(year - 1, 12, 28, 12, 0, 0.0) {
        samples.push(time_text(day.to_unix(), 2, year));
    }
    samples
}

/// When a waiting conversation comes back, as the Reminders view's date column says it (phase C):
/// "SOMEDAY", "17:30" today, "TUE 08:00" within the week, "NOV 12" later this year, the short
/// date after (the list's forms, in the same capitals).
pub fn wake_text(wake: i64, now: i64, year: i32) -> String {
    wake_words(wake, now, year, true)
}

/// The same as a screen reader says it in the date's place, saying that it is when the
/// conversation comes back (the final review): "back someday", "back today 17:30", "back
/// Tuesday 08:00", "back Nov 12".
pub fn wake_spoken(wake: i64, now: i64, year: i32) -> String {
    format!("back {}", wake_words(wake, now, year, false))
}

fn wake_words(wake: i64, now: i64, year: i32, capitals: bool) -> String {
    if remind::is_someday(wake) {
        return if capitals { "SOMEDAY" } else { "someday" }.to_owned();
    }
    let (Ok(at), Ok(today)) = (
        glib::DateTime::from_unix_local(wake),
        glib::DateTime::from_unix_local(now),
    ) else {
        return String::new();
    };
    let clock = at
        .format("%H:%M")
        .map(|text| text.to_string())
        .unwrap_or_default();
    let day = |date: &glib::DateTime| (date.year(), date.day_of_year());
    let days = (at.to_unix() - today.to_unix()) / 86_400;
    if day(&at) == day(&today) {
        return if capitals {
            clock
        } else {
            format!("today {clock}")
        };
    }
    if at > today && days < 6 {
        let weekday = at
            .format(if capitals { "%a" } else { "%A" })
            .map(|text| text.trim().trim_end_matches('.').to_owned())
            .unwrap_or_default();
        let weekday = if capitals {
            weekday.to_uppercase()
        } else {
            weekday
        };
        return format!("{weekday} {clock}");
    }
    date_text(wake, 2, year, capitals)
}

/// The wake times the Reminders view's date column must hold: a time, each weekday with a
/// time, the dates of [`date_samples`] and Someday.
fn wake_samples(year: i32) -> Vec<String> {
    let mut samples = date_samples(year);
    // 2 to 8 March 2026 are a Monday to a Sunday.
    for day in 2..=8 {
        if let Ok(date) = glib::DateTime::from_local(2026, 3, day, 0, 0, 0.0) {
            let weekday = date
                .format("%a")
                .map(|text| text.trim().trim_end_matches('.').to_uppercase())
                .unwrap_or_default();
            samples.push(format!("{weekday} 00:00"));
        }
    }
    samples.push("SOMEDAY".to_owned());
    samples
}

/// The parts of a date in the order the locale writes them (`%x`), and what goes between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Month,
    Day,
    Year,
}

thread_local! {
    static NUMERIC: ([Part; 3], char) = numeric_order(
        &glib::DateTime::from_utc(2001, 2, 3, 12, 0, 0.0)
            .ok()
            .and_then(|date| date.format("%x").ok())
            .map(|text| text.to_string())
            .unwrap_or_default(),
    );
}

/// The order and the separator of the locale's own date (`%x` of 3 February 2001): month
/// first ("02/03/2001"), day first ("03.02.01") or year first ("2001-02-03"); month first and
/// "/" when it says neither.
fn numeric_order(sample: &str) -> ([Part; 3], char) {
    // A mark between the numbers ("/", ".", "-"); a word or unit after each ("2001年02月03日",
    // "2001년 02월 03일") isn't one, and the numbers take "/" in the locale's order.
    let separator = sample
        .chars()
        .find(|character| !character.is_ascii_digit() && !character.is_whitespace())
        .filter(char::is_ascii_punctuation)
        .unwrap_or('/');
    let mut found: Vec<(usize, Part)> = Vec::new();
    let mut start = None;
    for (index, character) in sample
        .char_indices()
        .chain(std::iter::once((sample.len(), ' ')))
    {
        match (character.is_ascii_digit(), start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                let part = match sample[from..index].trim_start_matches('0') {
                    "2" => Some(Part::Month),
                    "3" => Some(Part::Day),
                    "1" | "2001" => Some(Part::Year),
                    _ => None,
                };
                if let Some(part) = part.filter(|part| !found.iter().any(|(_, p)| p == part)) {
                    found.push((from, part));
                }
                start = None;
            }
            _ => {}
        }
    }
    match found.as_slice() {
        [(_, a), (_, b), (_, c)] => ([*a, *b, *c], separator),
        // The month spelled out ("03. feb. 2001"): the day first, as such locales write it.
        [(_, Part::Day), (_, Part::Year)] => ([Part::Day, Part::Month, Part::Year], '.'),
        _ => ([Part::Month, Part::Day, Part::Year], '/'),
    }
}

/// "10/4/25": a date before this year, short, with its day (the reader's collapsed rows say it
/// the same way).
pub fn numeric_date(time: &glib::DateTime) -> String {
    NUMERIC.with(|(order, separator)| {
        order
            .iter()
            .map(|part| match part {
                Part::Month => time.month().to_string(),
                Part::Day => time.day_of_month().to_string(),
                Part::Year => format!("{:02}", time.year().rem_euclid(100)),
            })
            .collect::<Vec<_>>()
            .join(&separator.to_string())
    })
}

/// What a scheduled send's draft says in its date's place: "Sending tomorrow 08:00", "Sending
/// Tue 08:00", as the send's toast and Scheduled Messages say when (`time_picker::describe`).
pub fn scheduled_text(at: i64) -> String {
    crate::outbox::scheduled_text(&crate::time_picker::describe(at))
}

/// The same date as a screen reader says it: "10:32", "Yesterday", "Oct 4", "Oct 4 2025".
pub fn spoken_date(date: i64, section: usize, year: i32) -> String {
    date_text(date, section, year, false)
}

fn date_text(date: i64, section: usize, year: i32, capitals: bool) -> String {
    let Ok(time) = glib::DateTime::from_unix_local(date) else {
        return String::new();
    };
    let month = || {
        let month = time
            .format("%b")
            .map(|month| month.trim().trim_end_matches('.').to_owned())
            .unwrap_or_default();
        if capitals {
            month.to_uppercase()
        } else {
            month
        }
    };
    match section {
        0 => time
            .format("%H:%M")
            .map(|text| text.to_string())
            .unwrap_or_default(),
        1 if !capitals => "Yesterday".to_owned(),
        _ if time.year() == year => format!("{} {}", month(), time.day_of_month()),
        _ if capitals => numeric_date(&time),
        _ => format!("{} {} {}", month(), time.day_of_month(), time.year()),
    }
}

/// A message's sender, as one person of a row's sender column.
pub fn sender(row: &MessageSummary) -> Participant<'_> {
    let name = if row.sender_name.trim().is_empty() {
        &row.sender_email
    } else {
        &row.sender_name
    };
    Participant {
        name,
        me: false,
        unread: !row.flags.is_seen(),
        last: row.date,
    }
}

/// A conversation's people, oldest first, as its row names them (the store gives at most four:
/// the first and the three who wrote last).
pub fn people(row: &ConversationSummary) -> Vec<Participant<'_>> {
    row.participants
        .iter()
        .map(|person| Participant {
            name: if person.name.trim().is_empty() {
                &person.email
            } else {
                &person.name
            },
            me: person.is_me,
            unread: person.unread,
            last: person.last,
        })
        .collect()
}

/// What a row shows (docs/superhuman/rows.md, "The conversation hook"): a message row fills it
/// from its summary ([`RowFields::message`]); a conversation row from the conversation, with its
/// participants, size and draft ([`RowFields::conversation`]).
#[derive(Clone, Copy, Debug)]
pub struct RowFields<'a> {
    /// The people, oldest first: a message's sender, or a conversation's senders with the
    /// user as `me`.
    pub participants: &'a [Participant<'a>],
    /// Everyone who wrote, when `participants` names only some of them.
    pub writers: usize,
    /// The first participant's address, for the avatar when [`RowContext::avatars`] is on.
    pub address: &'a str,
    /// Messages in the conversation; shown, quietly, when more than one.
    pub count: u32,
    /// The conversation holds a draft (a message row: the message is one).
    pub draft: bool,
    /// The subject as stored; the row leaves out reply and forward prefixes.
    pub subject: &'a str,
    pub snippet: &'a str,
    /// Unix seconds, with the row's date section deciding the date's form.
    pub date: i64,
    pub unread: bool,
    pub starred: bool,
    pub attachment: bool,
    /// The row is a scheduled send's draft: when it goes (Unix ms), said in the date's place.
    pub scheduled: Option<i64>,
}

impl<'a> RowFields<'a> {
    /// A message row: its sender (`sender`, from [`sender`]) and its conversation's size.
    pub fn message(row: &'a MessageSummary, sender: &'a [Participant<'a>]) -> Self {
        Self {
            participants: sender,
            writers: sender.len(),
            address: &row.sender_email,
            count: row.thread_count,
            draft: row.flags.is_draft(),
            subject: &row.subject,
            snippet: &row.snippet,
            date: row.date,
            unread: !row.flags.is_seen(),
            starred: row.flags.is_flagged(),
            attachment: row.has_attachments,
            scheduled: None,
        }
    }

    /// A conversation row (docs/superhuman/conversations.md): its people (from [`people`]),
    /// its size, its latest message's date and snippet, and its members' state in the view.
    pub fn conversation(row: &'a ConversationSummary, people: &'a [Participant<'a>]) -> Self {
        Self {
            participants: people,
            writers: (row.participant_count as usize).max(people.len()),
            address: row
                .participants
                .first()
                .map_or(row.sender_email.as_str(), |person| person.email.as_str()),
            count: row.messages,
            draft: row.has_draft,
            subject: &row.subject,
            snippet: &row.snippet,
            date: row.date,
            unread: row.unread > 0,
            starred: row.flagged > 0,
            attachment: row.attachments > 0,
            scheduled: None,
        }
    }

    /// The subject the row shows: without reply and forward prefixes, "(no subject)" when
    /// there is none.
    fn subject(&self) -> &'a str {
        match base_subject(self.subject) {
            "" => "(no subject)",
            subject => subject,
        }
    }
}

/// What a screen reader says for the row, its state in words as well as in weight and colour:
/// "Unread, Ingrid, Tomas and you, 3 messages, Trip to Bergen, Oct 4, starred"; back from
/// Reminders, "…, back from a reminder" (phase C).
fn accessible_text(fields: &RowFields, date: &str, reminder: RowReminder) -> String {
    let mut text = String::new();
    if fields.unread {
        text.push_str("Unread, ");
    }
    if fields.draft {
        text.push_str("Draft, ");
    }
    text.push_str(&participants::spoken(fields.participants, fields.writers));
    if fields.count > 1 {
        text.push_str(&format!(", {} messages", fields.count));
    }
    text.push_str(", ");
    text.push_str(match base_subject(fields.subject) {
        "" => "no subject",
        subject => subject,
    });
    text.push_str(", ");
    text.push_str(date);
    if fields.starred {
        text.push_str(", starred");
    }
    if fields.attachment {
        text.push_str(", has attachments");
    }
    if reminder.returned {
        text.push_str(", back from a reminder");
    }
    text
}

thread_local! {
    static BOLD: pango::AttrList = {
        let list = pango::AttrList::new();
        list.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        list
    };
}

struct Parts {
    /// The unread dot's slot, `--sw-size-list-gutter` wide and `--sw-size-list-row` high: the
    /// one-line row's height comes from it. The select mode's checkbox lies round the dot.
    gutter: gtk::Overlay,
    dot: gtk::Box,
    check: gtk::CheckButton,
    avatar: Option<gtk::Label>,
    /// The sender column: the names, then the Draft marker or else the count.
    who: gtk::Box,
    sender: gtk::Label,
    draft: gtk::Label,
    count: gtk::Label,
    subject: gtk::Label,
    snippet: gtk::Label,
    clip: gtk::Image,
    star: gtk::Image,
    /// Remind Me's clock (phase C): laid out only in an Inbox's rows while a conversation there
    /// is back from Reminders ([`RowReminders::marks`]), shown on those.
    clock: gtk::Image,
    time: gtk::Label,
}

struct Bound {
    item: MessageItem,
    list_item: glib::WeakRef<gtk::ListItem>,
    handlers: [glib::SignalHandlerId; 2],
    selected: Option<glib::SignalHandlerId>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageRow {
        pub(super) parts: OnceCell<Parts>,
        pub(super) context: OnceCell<Rc<RowContext>>,
        pub(super) bound: RefCell<Option<Bound>>,
        /// Shows the placeholder look although its row is loaded (the list was moving fast).
        pub(super) skeleton: Cell<bool>,
        /// The row's state class, `sw-unread`, is on.
        pub(super) unread: Cell<bool>,
        pub(super) names: RefCell<Emphasis>,
        /// The sender column's texts, longest first, and the room (px) the one shown was
        /// picked for ([`participants::columns`]).
        pub(super) names_texts: RefCell<Vec<(String, Emphasis)>>,
        pub(super) names_room: Cell<Option<i32>>,
        pub(super) subject_bold: Cell<bool>,
        /// What the star and the paperclip show, when the date shows.
        pub(super) starred: Cell<bool>,
        pub(super) attachment: Cell<bool>,
        /// The clock mark shows, when the date shows (phase C).
        pub(super) returned: Cell<bool>,
        pub(super) tint: Cell<Option<usize>>,
        /// The quick actions, once the row first showed them.
        pub(super) quick: RefCell<Option<QuickActions>>,
        pub(super) hovered: Cell<bool>,
        /// The checkbox is being set from the selection, not clicked.
        pub(super) syncing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageRow {
        const NAME: &'static str = "ProntoMessageRow";
        type Type = super::MessageRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<super::RowLayout>();
        }
    }

    impl ObjectImpl for MessageRow {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for MessageRow {}
}

glib::wrapper! {
    pub struct MessageRow(ObjectSubclass<imp::MessageRow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

fn line(class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .single_line_mode(true)
        .build();
    label.add_css_class(class);
    label
}

fn quiet(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .single_line_mode(true)
        .visible(false)
        .build();
    label.add_css_class(class);
    label
}

impl MessageRow {
    pub fn new(context: Rc<RowContext>) -> Self {
        context.rows_created.set(context.rows_created.get() + 1);
        let row: Self = glib::Object::new();
        row.add_css_class("sw-message-row");
        let dot = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .can_target(false)
            .opacity(0.0)
            .build();
        dot.add_css_class("sw-unread-dot");
        // Centred where the dot is, the dot over it while the row isn't ticked: an unread row
        // keeps its dot in the select mode (the visual QA).
        let check = gtk::CheckButton::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .focusable(false)
            .visible(false)
            .build();
        check.add_css_class("sw-row-check");
        check.update_property(&[gtk::accessible::Property::Label("Selected")]);
        let gutter = gtk::Overlay::builder().child(&check).build();
        gutter.add_css_class("sw-row-gutter");
        gutter.add_overlay(&dot);
        gutter.set_parent(&row);
        let avatar = context.avatars.get().then(|| {
            let avatar = gtk::Label::builder()
                .valign(gtk::Align::Center)
                .halign(gtk::Align::Center)
                .xalign(0.5)
                .build();
            avatar.add_css_class("sw-avatar");
            avatar.add_css_class("sw-avatar-small");
            avatar.set_parent(&row);
            avatar
        });
        let sender = line("sw-sender");
        let draft = quiet("Draft", "sw-row-draft");
        let count = quiet("", "sw-thread-count");
        let who = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        who.add_css_class("sw-row-who");
        who.append(&sender);
        who.append(&draft);
        who.append(&count);
        who.set_parent(&row);
        let subject = line("sw-subject");
        subject.set_parent(&row);
        let snippet = line("sw-snippet");
        snippet.set_parent(&row);
        let clip = gtk::Image::builder()
            .icon_name("mail-attachment-symbolic")
            .opacity(0.0)
            .build();
        clip.add_css_class("sw-indicator");
        clip.set_parent(&row);
        let star = gtk::Image::builder()
            .icon_name("starred-symbolic")
            .opacity(0.0)
            .build();
        star.add_css_class("sw-star");
        star.set_parent(&row);
        let clock = gtk::Image::builder()
            .gicon(&gio::ThemedIcon::from_names(&REMIND_ICONS))
            .opacity(0.0)
            .visible(false)
            .build();
        clock.add_css_class("sw-indicator");
        clock.add_css_class("sw-reminded");
        clock.set_parent(&row);
        let time = gtk::Label::builder()
            .xalign(1.0)
            .single_line_mode(true)
            .build();
        time.add_css_class("sw-time");
        time.set_parent(&row);
        check.connect_toggled(glib::clone!(
            #[weak]
            row,
            move |_| row.check_toggled()
        ));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter(glib::clone!(
            #[weak]
            row,
            move |_, _, _| row.set_hovered(true)
        ));
        motion.connect_leave(glib::clone!(
            #[weak]
            row,
            move |_| row.set_hovered(false)
        ));
        row.add_controller(motion);
        let imp = row.imp();
        let _ = imp.parts.set(Parts {
            gutter,
            dot,
            check,
            avatar,
            who,
            sender,
            draft,
            count,
            subject,
            snippet,
            clip,
            star,
            clock,
            time,
        });
        let _ = imp.context.set(context);
        row
    }

    /// The user clicked the checkbox: the row's selection follows, the others stay.
    fn check_toggled(&self) {
        if self.imp().syncing.get() {
            return;
        }
        let Some(position) = self.position() else {
            return;
        };
        if self
            .activate_action(
                "list.select-item",
                Some(&(position, true, false).to_variant()),
            )
            .is_err()
        {
            tracing::warn!("a row's checkbox has no list.select-item");
        }
    }

    fn list_item(&self) -> Option<gtk::ListItem> {
        self.imp()
            .bound
            .borrow()
            .as_ref()
            .and_then(|bound| bound.list_item.upgrade())
    }

    /// The row's selection changed: the checkbox follows it.
    fn selection_changed(&self) {
        self.sync_check();
    }

    /// The checkbox shows whether the row is selected; a ticked one hides the unread dot.
    fn sync_check(&self) {
        let (Some(parts), Some(list_item)) = (self.imp().parts.get(), self.list_item()) else {
            return;
        };
        let selected = list_item.is_selected();
        if parts.check.is_active() != selected {
            self.imp().syncing.set(true);
            parts.check.set_active(selected);
            self.imp().syncing.set(false);
        }
        self.show_dot(parts);
    }

    /// The unread dot shows on an unread row, in the select mode too, unless its checkbox is
    /// ticked (the tick says the row is chosen; its weight still says unread).
    fn show_dot(&self, parts: &Parts) {
        let ticked = parts.check.is_visible() && parts.check.is_active();
        set_opacity(&parts.dot, self.imp().unread.get() && !ticked);
    }

    fn set_hovered(&self, hovered: bool) {
        if self.imp().hovered.replace(hovered) != hovered {
            self.show_quick();
        }
    }

    /// The quick actions show outside the select mode on a loaded row under the pointer, over
    /// the date and the indicators; only the pointer shows them (BRIEF-B: the selected row
    /// keeps its date while the pointer is elsewhere, in the header say).
    fn show_quick(&self) {
        let Some(parts) = self.imp().parts.get() else {
            return;
        };
        let context = self.context();
        let shown =
            !context.select_mode.get() && !self.is_placeholder() && self.imp().hovered.get();
        if shown && self.imp().quick.borrow().is_none() {
            let actions = gtk::Box::builder()
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            actions.add_css_class("sw-quick-actions");
            let mut buttons = Vec::new();
            for action in Quick::ALL {
                let button = icon_button(action.icons(false), &context.quick_tooltip(action));
                // The outline star at full strength (phase C: Adwaita's looked disabled).
                if let Some(image) = button.child().and_downcast::<gtk::Image>() {
                    glyph::set_icon(&image, action.icons(false));
                }
                button.add_css_class("sw-quick-action");
                button.set_focusable(false);
                button.set_widget_name(&format!("quick-{}", action.name()));
                button.connect_clicked(glib::clone!(
                    #[weak(rename_to = row)]
                    self,
                    move |_| row.quick_clicked(action)
                ));
                actions.append(&button);
                buttons.push((action, button));
            }
            // Last, so it draws over the date and the indicators.
            actions.set_parent(self);
            self.imp().quick.replace(Some(QuickActions {
                actions,
                buttons,
                starred: Cell::new(false),
            }));
        }
        if let Some(quick) = self.imp().quick.borrow().as_ref() {
            if shown {
                self.show_quick_star(quick);
                show_quick_off(
                    quick,
                    context.remind_off(),
                    &context.quick_tooltip(Quick::Remind),
                );
            }
            if quick.actions.is_visible() != shown {
                quick.actions.set_visible(shown);
            }
        }
        // The date and the indicators make way, by opacity, so nothing moves.
        let imp = self.imp();
        set_opacity(&parts.time, !shown);
        set_opacity(&parts.star, !shown && imp.starred.get());
        set_opacity(&parts.clip, !shown && imp.attachment.get());
        set_opacity(&parts.clock, !shown && imp.returned.get());
    }

    /// Whether the clock quick action is dimmed, for checks ([`RowReminders::off`]).
    pub fn quick_off(&self) -> bool {
        self.quick_button(Quick::Remind)
            .is_some_and(|button| button.has_css_class(QUICK_OFF))
    }

    /// The quick Star shows a full star on a starred row (it unstars) and an outline otherwise.
    fn show_quick_star(&self, quick: &QuickActions) {
        let starred = self.imp().starred.get();
        if quick.starred.replace(starred) == starred {
            return;
        }
        if let Some(image) = quick
            .buttons
            .iter()
            .find(|(action, _)| *action == Quick::Star)
            .and_then(|(_, button)| button.child().and_downcast::<gtk::Image>())
        {
            glyph::set_icon(&image, Quick::Star.icons(starred));
        }
    }

    fn update_quick_tooltips(&self) {
        if let Some(quick) = self.imp().quick.borrow().as_ref() {
            let context = self.context();
            for (action, button) in &quick.buttons {
                let text = context.quick_tooltip(*action);
                button.set_tooltip_text(Some(&text));
                button.update_property(&[gtk::accessible::Property::Label(&text)]);
            }
            // The clock's names why Remind Me is off, where it is.
            show_quick_off(
                quick,
                context.remind_off(),
                &context.quick_tooltip(Quick::Remind),
            );
        }
    }

    fn quick_clicked(&self, action: Quick) {
        let row = self.item().and_then(|item| item.row().clone());
        let callback = self.context().quick.borrow().clone();
        if let (Some(row), Some(callback)) = (row, callback) {
            callback(action.action(), row);
        }
    }

    /// Whether the row shows its quick actions now (for the self-test).
    pub fn shows_quick(&self) -> bool {
        self.imp()
            .quick
            .borrow()
            .as_ref()
            .is_some_and(|quick| quick.actions.is_visible())
    }

    /// The quick action button for `action`, once the row has shown them.
    pub fn quick_button(&self, action: Quick) -> Option<gtk::Button> {
        self.imp().quick.borrow().as_ref().and_then(|quick| {
            quick
                .buttons
                .iter()
                .find(|(candidate, _)| *candidate == action)
                .map(|(_, button)| button.clone())
        })
    }

    /// The checkbox, while the row shows it (the self-test clicks it).
    pub fn check_button(&self) -> Option<gtk::CheckButton> {
        let parts = self.imp().parts.get()?;
        parts.check.is_visible().then(|| parts.check.clone())
    }

    /// Whether the row shows its checkbox, and whether it is ticked.
    pub fn check_state(&self) -> Option<bool> {
        let parts = self.imp().parts.get()?;
        parts.check.is_visible().then(|| parts.check.is_active())
    }

    fn context(&self) -> &Rc<RowContext> {
        self.imp()
            .context
            .get()
            .expect("rows are built with a context")
    }

    pub fn bind(&self, list_item: &gtk::ListItem, item: &MessageItem) {
        self.unbind();
        let changed = item.connect_changed(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |_| row.show()
        ));
        let section = item.connect_section_changed(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |_| row.show()
        ));
        let selected = list_item.connect_selected_notify(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |_| row.selection_changed()
        ));
        self.imp().bound.replace(Some(Bound {
            item: item.clone(),
            list_item: list_item.downgrade(),
            handlers: [changed, section],
            selected: Some(selected),
        }));
        let context = self.context();
        context.binds.set(context.binds.get() + 1);
        context.bound.borrow_mut().push(self.clone());
        self.show();
    }

    pub fn unbind(&self) {
        let Some(bound) = self.imp().bound.take() else {
            return;
        };
        for handler in bound.handlers {
            bound.item.disconnect(handler);
        }
        if let (Some(list_item), Some(handler)) = (bound.list_item.upgrade(), bound.selected) {
            list_item.disconnect(handler);
        }
        self.context().bound.borrow_mut().retain(|row| row != self);
    }

    pub fn item(&self) -> Option<MessageItem> {
        self.imp()
            .bound
            .borrow()
            .as_ref()
            .map(|bound| bound.item.clone())
    }

    /// The row's position in the list now.
    pub fn position(&self) -> Option<u32> {
        let list_item = self.list_item()?;
        Some(list_item.position()).filter(|position| *position != gtk::INVALID_LIST_POSITION)
    }

    /// The row's identity: its message, or its conversation.
    pub fn key(&self) -> Option<RowKey> {
        self.item().and_then(|item| item.key())
    }

    /// The message the reader shows for the row (a conversation's anchor).
    pub fn message(&self) -> Option<MessageId> {
        self.item().and_then(|item| item.message())
    }

    /// Whether the row shows its placeholder look: its row isn't loaded, or the list moved fast.
    pub fn is_placeholder(&self) -> bool {
        self.imp().skeleton.get() || self.item().is_none_or(|item| !item.is_loaded())
    }

    /// Shows what the row is bound to again, as after a fast scroll or a resize.
    pub fn refresh(&self) {
        self.show();
    }

    pub fn shows_skeleton(&self) -> bool {
        self.imp().skeleton.get()
    }

    /// The count the sender column shows ("" for one message), for the self-test.
    pub fn count_text(&self) -> String {
        self.imp()
            .parts
            .get()
            .filter(|parts| parts.count.is_visible() && parts.count.opacity() > 0.0)
            .map(|parts| parts.count.text().to_string())
            .unwrap_or_default()
    }

    /// Whether the names show whole: not ellipsized, and no wider than their text, so the
    /// Draft marker or the count follows them at once (the self-test, phase C).
    pub fn names_whole(&self) -> bool {
        let Some(parts) = self.imp().parts.get() else {
            return false;
        };
        let names = &parts.sender;
        let (_, natural) = measure(names, gtk::Orientation::Horizontal);
        !names.layout().is_ellipsized() && names.width() <= natural
    }

    /// What a screen reader says for the row, for the self-test.
    pub fn accessible_text(&self) -> Option<String> {
        self.list_item()
            .map(|list_item| list_item.accessible_label().to_string())
    }

    /// Where one of the row's parts is, in `target`'s coordinates (the self-test's geometry
    /// checks): `gutter`, `dot`, `check`, `sender` (the column), `names`, `subject`, `snippet`,
    /// `clip`, `star`, `clock` (Remind Me's mark), `time` or `quick` (the quick actions); `None`
    /// for another name or a part not laid out.
    pub fn part_bounds(
        &self,
        part: &str,
        target: &impl IsA<gtk::Widget>,
    ) -> Option<gtk::graphene::Rect> {
        if part == "quick" {
            let quick = self.imp().quick.borrow();
            let actions = &quick.as_ref()?.actions;
            return actions
                .should_layout()
                .then(|| actions.compute_bounds(target))
                .flatten();
        }
        let parts = self.imp().parts.get()?;
        let widget: &gtk::Widget = match part {
            "gutter" => parts.gutter.upcast_ref(),
            "dot" => parts.dot.upcast_ref(),
            "check" => parts.check.upcast_ref(),
            "sender" => parts.who.upcast_ref(),
            "names" => parts.sender.upcast_ref(),
            "subject" => parts.subject.upcast_ref(),
            "snippet" => parts.snippet.upcast_ref(),
            "clip" => parts.clip.upcast_ref(),
            "star" => parts.star.upcast_ref(),
            "clock" => parts.clock.upcast_ref(),
            "time" => parts.time.upcast_ref(),
            _ => return None,
        };
        widget
            .should_layout()
            .then(|| widget.compute_bounds(target))
            .flatten()
    }

    /// Whether the row shows Remind Me's clock mark now (phase C).
    pub fn shows_reminder_mark(&self) -> bool {
        self.imp()
            .parts
            .get()
            .is_some_and(|parts| parts.clock.is_visible() && parts.clock.opacity() > 0.0)
    }

    /// Whether the row shows its unread dot now.
    pub fn shows_dot(&self) -> bool {
        self.imp()
            .parts
            .get()
            .is_some_and(|parts| parts.dot.opacity() > 0.0)
    }

    /// The texts the row shows: sender column, subject, snippet, date (for the self-test).
    pub fn texts(&self) -> [String; 4] {
        let Some(parts) = self.imp().parts.get() else {
            return Default::default();
        };
        [
            parts.sender.text().to_string(),
            parts.subject.text().to_string(),
            parts.snippet.text().to_string(),
            parts.time.text().to_string(),
        ]
    }

    fn show(&self) {
        let Some(item) = self.item() else {
            return;
        };
        let skeleton = self.context().skeleton.get();
        let row = item.row();
        let reminder = row
            .as_ref()
            .map(|row| self.context().reminder_of(row))
            .unwrap_or_default();
        match row.as_ref() {
            Some(ListRow::Message(summary)) if !skeleton => {
                self.imp().skeleton.set(false);
                let sender = [sender(summary)];
                let fields = RowFields {
                    scheduled: self.context().scheduled_at(summary.id),
                    ..RowFields::message(summary, &sender)
                };
                self.show_fields(&fields, item.section(), reminder);
            }
            Some(ListRow::Conversation(conversation)) if !skeleton => {
                self.imp().skeleton.set(false);
                let people = people(conversation);
                // A Drafts view's conversation row: its latest message is its newest draft.
                let fields = RowFields {
                    scheduled: self.context().scheduled_at(conversation.latest),
                    ..RowFields::conversation(conversation, &people)
                };
                self.show_fields(&fields, item.section(), reminder);
            }
            Some(_) => {
                self.imp().skeleton.set(true);
                self.show_placeholder();
            }
            None => {
                self.imp().skeleton.set(false);
                self.show_placeholder();
            }
        }
    }

    /// The row's one state class: `sw-unread` colours an unread row's text (style.css).
    fn set_unread(&self, unread: bool) {
        if self.imp().unread.replace(unread) == unread {
            return;
        }
        if unread {
            self.add_css_class("sw-unread");
        } else {
            self.remove_css_class("sw-unread");
        }
    }

    fn set_names_emphasis(&self, parts: &Parts, emphasis: Emphasis) {
        if *self.imp().names.borrow() == emphasis {
            return;
        }
        match &emphasis {
            Emphasis::None => parts.sender.set_attributes(None),
            Emphasis::All => BOLD.with(|list| parts.sender.set_attributes(Some(list))),
            Emphasis::Ranges(ranges) => {
                let list = pango::AttrList::new();
                for (start, end) in ranges {
                    let mut weight = pango::AttrInt::new_weight(pango::Weight::Bold);
                    weight.set_start_index(*start);
                    weight.set_end_index(*end);
                    list.insert(weight);
                }
                parts.sender.set_attributes(Some(&list));
            }
        }
        self.imp().names.replace(emphasis);
    }

    fn set_subject_bold(&self, parts: &Parts, bold: bool) {
        if self.imp().subject_bold.replace(bold) == bold {
            return;
        }
        if bold {
            BOLD.with(|list| parts.subject.set_attributes(Some(list)));
        } else {
            parts.subject.set_attributes(None);
        }
    }

    fn set_tint(&self, avatar: &gtk::Label, tint: usize) {
        let previous = self.imp().tint.replace(Some(tint));
        if previous == Some(tint) {
            return;
        }
        if let Some(previous) = previous {
            avatar.remove_css_class(TINTS[previous]);
        }
        avatar.add_css_class(TINTS[tint]);
    }

    /// Shows `fields` in a row no list binds: the self-test's sample list, for the theme's
    /// screenshots. `section` picks the date's form.
    pub fn show_unbound(&self, fields: &RowFields, section: usize) {
        self.show_fields(fields, section, RowReminder::default());
    }

    /// Shows `fields`, the one way a row is drawn (docs/superhuman/rows.md), with what Remind Me
    /// adds (`reminder`, phase C).
    fn show_fields(&self, fields: &RowFields, section: usize, reminder: RowReminder) {
        let Some(parts) = self.imp().parts.get() else {
            return;
        };
        let (sender_chars, subject_chars, snippet_chars) = self.context().limits();
        self.set_unread(fields.unread);
        let texts = participants::columns(fields.participants, fields.writers, fields.unread)
            .into_iter()
            .map(|(names, emphasis)| {
                let names = cut(&names, sender_chars);
                let emphasis = match emphasis {
                    // A cut can end inside a range; Pango clamps it.
                    Emphasis::Ranges(ranges) => Emphasis::Ranges(
                        ranges
                            .into_iter()
                            .filter(|(start, _)| (*start as usize) < names.len())
                            .collect(),
                    ),
                    other => other,
                };
                (names.to_owned(), emphasis)
            })
            .collect();
        self.imp().names_texts.replace(texts);
        self.set_subject_bold(parts, fields.unread);
        self.show_check(parts, self.context().select_mode.get());
        self.show_dot(parts);
        if let Some(avatar) = &parts.avatar {
            set_opacity(avatar, true);
            self.set_tint(avatar, avatar_index(fields.address));
            let name = fields.participants.first().map_or("", |person| person.name);
            avatar.set_text(&initials(name, fields.address));
        }
        // The Draft marker takes the count's place (phase C: both beside the names left them
        // too little at 1440 px), then the names that fit the column as it was last laid out.
        parts.draft.set_visible(fields.draft);
        if fields.count > 1 && !fields.draft {
            parts.count.set_text(&fields.count.to_string());
            parts.count.set_visible(true);
        } else {
            parts.count.set_visible(false);
        }
        let room = self.names_room_in(parts, self.context().column.get());
        self.show_names(parts, room);
        let year = self.context().year.get();
        let marks = self.context().reminders.borrow().marks;
        if parts.clock.is_visible() != marks {
            parts.clock.set_visible(marks);
        }
        // A scheduled send's draft says when it goes, a phrase wider than the date column
        // (the row's subject and snippet make room). Waiting conversations keep their wake
        // times wherever their reminder date key is shown.
        let (time, spoken) = match (reminder.wake, fields.scheduled) {
            (Some(wake), _) => {
                let now = self.context().now();
                (wake_text(wake, now, year), wake_spoken(wake, now, year))
            }
            (None, Some(at)) => {
                let when = scheduled_text(at);
                (when.clone(), when)
            }
            (None, None) => (
                time_text(fields.date, section, year),
                spoken_date(fields.date, section, year),
            ),
        };
        let draft_ready = self
            .item()
            .and_then(|item| item.row().clone())
            .is_some_and(|row| {
                let thread = match row {
                    ListRow::Message(m) => m.thread,
                    ListRow::Conversation(c) => c.thread,
                };
                self.context().ai_drafts.borrow().contains(&thread)
            });
        let time = if draft_ready {
            "Draft ready".to_owned()
        } else {
            time
        };
        let spoken = if draft_ready {
            format!("{spoken}, Draft ready")
        } else {
            spoken
        };
        parts.time.set_text(&time);
        parts.subject.set_text(cut(fields.subject(), subject_chars));
        parts
            .snippet
            .set_text(cut(fields.snippet.trim(), snippet_chars));
        if let Some(list_item) = self.list_item() {
            list_item.set_accessible_label(&accessible_text(fields, &spoken, reminder));
        }
        self.imp().starred.set(fields.starred);
        self.imp().attachment.set(fields.attachment);
        self.imp().returned.set(reminder.returned);
        self.show_quick();
    }

    /// The room the names have in a sender column `column` px wide, its gaps, margins, Draft
    /// marker and count taken off; `None` while no column is known, or while the row has one
    /// text only (one person, a placeholder: a long name is ellipsized).
    fn names_room_in(&self, parts: &Parts, column: i32) -> Option<i32> {
        if column <= 0 || self.imp().names_texts.borrow().len() < 2 {
            return None;
        }
        let (_, who) = measure(&parts.who, gtk::Orientation::Horizontal);
        let (_, names) = measure(&parts.sender, gtk::Orientation::Horizontal);
        Some(column - (who - names))
    }

    /// Shows the longest of the row's sender texts that fits `room` (the first while the room
    /// isn't known), whole names only: never "Marius … To…" (phase C, the visual QA). The
    /// shortest, "Marius +7", names how many others wrote: the message count after it ("Marius
    /// +7 10") would read as one more number of the same kind (phase C's review), so the count
    /// makes way, by opacity, keeping the room the names were picked for.
    fn show_names(&self, parts: &Parts, room: Option<i32>) {
        let imp = self.imp();
        let (text, emphasis, others) = {
            let texts = imp.names_texts.borrow();
            let Some(last) = texts.len().checked_sub(1) else {
                return;
            };
            let chosen = room.map_or(0, |room| {
                texts
                    .iter()
                    .position(|(text, emphasis)| text_width(&parts.sender, text, emphasis) <= room)
                    .unwrap_or(last)
            });
            let (text, emphasis) = texts[chosen].clone();
            (text, emphasis, last > 0 && chosen == last)
        };
        if parts.sender.text() != text {
            parts.sender.set_text(&text);
        }
        self.set_names_emphasis(parts, emphasis);
        set_opacity(&parts.count, !others);
        imp.names_room.set(room);
    }

    /// Laying out a sender column `column` px wide: the names are picked again when their room
    /// isn't the one they were picked for (the first layout, a resize), before the column is
    /// laid out, and the column is noted for the rows bound next.
    fn fit_names(&self, parts: &Parts, column: i32) {
        self.context().column.set(column);
        let room = self.names_room_in(parts, column);
        if room.is_none() || room == self.imp().names_room.get() {
            return;
        }
        self.show_names(parts, room);
        // Measured again before it is laid out, with the text it now has.
        let _ = measure(&parts.who, gtk::Orientation::Horizontal);
    }

    /// In the select mode the checkbox shows round the unread dot.
    fn show_check(&self, parts: &Parts, select_mode: bool) {
        if parts.check.is_visible() != select_mode {
            parts.check.set_visible(select_mode);
        }
        if select_mode {
            self.sync_check();
        }
    }

    /// The row's shape without its text: a row that is loading, or one the list moves past too
    /// fast to read.
    fn show_placeholder(&self) {
        let Some(parts) = self.imp().parts.get() else {
            return;
        };
        self.set_unread(false);
        self.set_names_emphasis(parts, Emphasis::None);
        self.imp().names_texts.borrow_mut().clear();
        self.imp().names_room.set(None);
        self.set_subject_bold(parts, false);
        set_opacity(&parts.dot, false);
        if let Some(avatar) = &parts.avatar {
            avatar.set_text("");
            avatar.set_opacity(0.4);
        }
        parts.sender.set_text(" ");
        parts.time.set_text(" ");
        parts.subject.set_text(" ");
        parts.snippet.set_text(" ");
        parts.draft.set_visible(false);
        parts.count.set_visible(false);
        self.imp().starred.set(false);
        self.imp().attachment.set(false);
        self.imp().returned.set(false);
        self.show_check(parts, false);
        self.show_quick();
    }

    /// The row's geometry from its parts' CSS: the gutter's width and the one-line height (the
    /// gutter's minimum height), the avatar's width and the sender column's at its widest (the
    /// list's ruler, [`RowContext::set_ruler`]), never under what the column's own parts need.
    fn columns(&self, parts: &Parts) -> Columns {
        let gutter = measure(&parts.gutter, gtk::Orientation::Horizontal).0;
        let row = measure(&parts.gutter, gtk::Orientation::Vertical).0;
        let avatar = parts
            .avatar
            .as_ref()
            .filter(|avatar| avatar.should_layout())
            .map_or(0, |avatar| measure(avatar, gtk::Orientation::Horizontal).1);
        let least = measure(&parts.who, gtk::Orientation::Horizontal).0;
        let sender = self.context().sender_width().max(least);
        Columns {
            gutter,
            row,
            avatar,
            sender,
            least,
        }
    }

    fn measure_parts(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32) {
        let Some(parts) = self.imp().parts.get() else {
            return (0, 0);
        };
        let columns = self.columns(parts);
        match orientation {
            gtk::Orientation::Horizontal => {
                let width = columns.gutter + columns.avatar + columns.sender;
                (width, width)
            }
            _ => {
                let height = if for_size < 0 || columns.one_line(for_size) {
                    columns.row
                } else {
                    columns.two_lines()
                };
                (height, height)
            }
        }
    }

    fn allocate_parts(&self, width: i32, height: i32) {
        let Some(parts) = self.imp().parts.get() else {
            return;
        };
        let rtl = self.direction() == gtk::TextDirection::Rtl;
        let place = |child: &gtk::Widget, x: i32, y: i32, w: i32, h: i32| {
            let x = if rtl { width - x - w } else { x };
            child.size_allocate(&gtk::Allocation::new(x, y, w.max(0), h.max(0)), -1);
        };
        let columns = self.columns(parts);
        let one_line = columns.one_line(width);
        // The first line's slot: the whole row on one line, else the one-line row's height at
        // the top, its text centred where a one-line row's is.
        let line = if one_line {
            height
        } else {
            columns.row.min(height)
        };
        place(parts.gutter.upcast_ref(), 0, 0, columns.gutter, line);
        let mut x = columns.gutter;
        if let Some(avatar) = parts
            .avatar
            .as_ref()
            .filter(|avatar| avatar.should_layout())
        {
            place(avatar.upcast_ref(), x, 0, columns.avatar, line);
            x += columns.avatar;
        }
        let start = x;
        // The date in its column, as wide on every row; the star and the paperclip before it,
        // and in an Inbox Remind Me's clock, in a slot of its own (phase C).
        let date = self
            .context()
            .date_width(&parts.time)
            .max(measure(&parts.time, gtk::Orientation::Horizontal).1);
        place(parts.time.upcast_ref(), width - date, 0, date, line);
        let mut right = width - date;
        for part in [
            parts.star.upcast_ref::<gtk::Widget>(),
            parts.clip.upcast_ref(),
            parts.clock.upcast_ref(),
        ] {
            if !part.should_layout() {
                continue;
            }
            let part_width = measure(part, gtk::Orientation::Horizontal).1;
            place(part, right - part_width, 0, part_width, line);
            right -= part_width;
        }
        if one_line {
            let sender = columns.sender_at(width - x);
            self.fit_names(parts, sender);
            place(parts.who.upcast_ref(), x, 0, sender, line);
            x += sender;
            self.share(parts, &place, x, right, 0, line);
        } else {
            self.fit_names(parts, right - x);
            place(parts.who.upcast_ref(), x, 0, right - x, line);
            let (top, second) = columns.second_line();
            self.share(parts, &place, start, width, top, second);
        }
        if let Some(quick) = self.imp().quick.borrow().as_ref()
            && quick.actions.should_layout()
        {
            let (_, quick_width) = measure(&quick.actions, gtk::Orientation::Horizontal);
            let (_, quick_height) = measure(&quick.actions, gtk::Orientation::Vertical);
            let top = (line - quick_height) / 2;
            place(
                quick.actions.upcast_ref(),
                width - quick_width,
                top,
                quick_width,
                quick_height,
            );
        }
    }

    /// The subject and the snippet between `start` and `end`: the subject takes what it needs
    /// when both fit, else at most [`SUBJECT_SHARE`] (more when the snippet is short), and the
    /// snippet the rest.
    fn share(
        &self,
        parts: &Parts,
        place: &dyn Fn(&gtk::Widget, i32, i32, i32, i32),
        start: i32,
        end: i32,
        top: i32,
        height: i32,
    ) {
        let room = (end - start).max(0);
        let (_, subject) = measure(&parts.subject, gtk::Orientation::Horizontal);
        let (_, snippet) = measure(&parts.snippet, gtk::Orientation::Horizontal);
        let subject = if subject + snippet <= room {
            subject
        } else {
            let share = (f64::from(room) * SUBJECT_SHARE) as i32;
            subject.min(share.max(room - snippet))
        }
        .clamp(0, room);
        place(parts.subject.upcast_ref(), start, top, subject, height);
        place(
            parts.snippet.upcast_ref(),
            start + subject,
            top,
            room - subject,
            height,
        );
    }
}

/// The row's fixed geometry, in px, from its parts' CSS (see [`MessageRow::columns`]).
struct Columns {
    gutter: i32,
    /// A one-line row's height.
    row: i32,
    avatar: i32,
    /// The sender column at its widest, gap included.
    sender: i32,
    /// What the sender column's own parts need (a Draft marker and a count beside the names).
    least: i32,
}

impl Columns {
    fn one_line(&self, width: i32) -> bool {
        f64::from(width - self.gutter - self.avatar) >= f64::from(self.sender) * ONE_LINE_COLUMNS
    }

    /// The sender column of a one-line row whose content past the gutter is `content` wide
    /// ([`SENDER_SHARE`]).
    fn sender_at(&self, content: i32) -> i32 {
        sender_column(content, self.sender, self.least)
    }

    /// A two-line row's height: one and a half one-line rows (57 px at the default text size).
    /// The first line's text sits where a one-line row's does, centred in its height; the
    /// second is centred on the one-line row's bottom edge, so text and padding come from the
    /// one size token, never from the text or the fonts it falls back to.
    fn two_lines(&self) -> i32 {
        self.row + self.row / 2
    }

    /// The second line's top and height.
    fn second_line(&self) -> (i32, i32) {
        (self.row - self.row / 4, self.row / 2)
    }
}

/// The sender column for `content` px past the gutter: [`SENDER_SHARE`] of it, at least
/// [`SENDER_FLOOR`] of `widest` and what its parts need (`least`), at most `widest`.
fn sender_column(content: i32, widest: i32, least: i32) -> i32 {
    let share = (f64::from(content) * SENDER_SHARE).round() as i32;
    let floor = ((f64::from(widest) * SENDER_FLOOR).round() as i32).max(least);
    share.clamp(floor.min(widest), widest)
}

/// A child's minimum and natural size, margins included; nothing for a hidden one.
fn measure(widget: &impl IsA<gtk::Widget>, orientation: gtk::Orientation) -> (i32, i32) {
    if !widget.should_layout() {
        return (0, 0);
    }
    let (minimum, natural, _, _) = widget.measure(orientation, -1);
    (minimum, natural)
}

fn set_opacity(widget: &impl IsA<gtk::Widget>, shown: bool) {
    let opacity = if shown { 1.0 } else { 0.0 };
    if widget.opacity() != opacity {
        widget.set_opacity(opacity);
    }
}

mod layout_imp {
    use super::*;

    /// Lays out a [`MessageRow`](super::MessageRow): see `MessageRow::allocate_parts`.
    #[derive(Default)]
    pub struct RowLayout;

    #[glib::object_subclass]
    impl ObjectSubclass for RowLayout {
        const NAME: &'static str = "ProntoRowLayout";
        type Type = super::RowLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for RowLayout {}

    impl LayoutManagerImpl for RowLayout {
        fn request_mode(&self, _widget: &gtk::Widget) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(
            &self,
            widget: &gtk::Widget,
            orientation: gtk::Orientation,
            for_size: i32,
        ) -> (i32, i32, i32, i32) {
            let (minimum, natural) = widget
                .downcast_ref::<super::MessageRow>()
                .map_or((0, 0), |row| row.measure_parts(orientation, for_size));
            (minimum, natural, -1, -1)
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, _baseline: i32) {
            if let Some(row) = widget.downcast_ref::<super::MessageRow>() {
                row.allocate_parts(width, height);
            }
        }
    }
}

glib::wrapper! {
    /// The message row's layout: one line, or two in a narrow list.
    pub struct RowLayout(ObjectSubclass<layout_imp::RowLayout>)
        @extends gtk::LayoutManager;
}

mod header_imp {
    use super::*;

    #[derive(Default)]
    pub struct SectionHeader {
        pub(super) bound: RefCell<Option<(MessageItem, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SectionHeader {
        const NAME: &'static str = "ProntoSectionHeader";
        type Type = super::SectionHeader;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for SectionHeader {}
    impl WidgetImpl for SectionHeader {}
    impl BoxImpl for SectionHeader {}
}

glib::wrapper! {
    /// "TODAY", "YESTERDAY", …: the header GtkListView shows above each section, a quiet
    /// caption (the Superhuman pass).
    pub struct SectionHeader(ObjectSubclass<header_imp::SectionHeader>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for SectionHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl SectionHeader {
    pub fn new() -> Self {
        let header: Self = glib::Object::new();
        let label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        label.add_css_class("sw-section-header");
        header.append(&label);
        header
    }

    fn label(&self) -> Option<gtk::Label> {
        self.first_child().and_downcast::<gtk::Label>()
    }

    pub fn bind(&self, item: &MessageItem, context: &RowContext) {
        self.unbind(context);
        let handler = item.connect_section_changed(glib::clone!(
            #[weak(rename_to = header)]
            self,
            move |item| header.show(item.section())
        ));
        self.imp().bound.replace(Some((item.clone(), handler)));
        context.headers.borrow_mut().push(self.clone());
        self.show(item.section());
    }

    pub fn unbind(&self, context: &RowContext) {
        if let Some((item, handler)) = self.imp().bound.take() {
            item.disconnect(handler);
            context.headers.borrow_mut().retain(|header| header != self);
        }
    }

    fn show(&self, section: usize) {
        if let Some(label) = self.label() {
            label.set_text(&LABELS[section].to_uppercase());
        }
    }

    pub fn text(&self) -> String {
        self.label()
            .map(|label| label.text().to_string())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{AccountId, Flags, FolderId, Participant as Writer, ThreadId};
    use sealwax_domain::theme::avatar_class;

    fn person(name: &str, unread: bool) -> Participant<'_> {
        Participant {
            name,
            me: false,
            unread,
            last: 0,
        }
    }

    #[test]
    fn text_is_cut_at_characters_not_bytes() {
        assert_eq!(cut("blåbærsyltetøy", 5), "blåbæ");
        assert_eq!(cut("会议纪要", 2), "会议");
        assert_eq!(cut("short", 48), "short");
    }

    #[test]
    fn tint_classes_are_the_themes() {
        for address in ["ingrid@example.test", "", "LEAH@example.test"] {
            assert_eq!(TINTS[avatar_index(address)], avatar_class(address));
        }
    }

    #[test]
    fn dates_read_as_superhuman_shows_them() {
        let local = glib::TimeZone::local();
        let at = |year, month, day, hour| {
            glib::DateTime::new(&local, year, month, day, hour, 5, 0.0).unwrap()
        };
        // The month as the locale abbreviates it ("May" in English).
        let may = at(2026, 5, 3, 9)
            .format("%b")
            .unwrap()
            .trim()
            .trim_end_matches('.')
            .to_owned();
        let october = at(2026, 10, 27, 9)
            .format("%b")
            .unwrap()
            .trim()
            .trim_end_matches('.')
            .to_uppercase();
        assert_eq!(time_text(at(2026, 10, 28, 9).to_unix(), 0, 2026), "09:05");
        // Yesterday is a date like the others, never a word in capitals (the visual QA).
        assert_eq!(
            time_text(at(2026, 10, 27, 9).to_unix(), 1, 2026),
            format!("{october} 27")
        );
        assert_eq!(
            time_text(at(2026, 5, 3, 9).to_unix(), 3, 2026),
            format!("{} 3", may.to_uppercase())
        );
        // Before this year: short, with its day, in the locale's order.
        assert!(
            ["5/13/25", "13/5/25", "13.5.25", "25-5-13"]
                .contains(&time_text(at(2025, 5, 13, 9).to_unix(), 4, 2026).as_str())
        );
        // Spoken, in mixed case.
        assert_eq!(
            spoken_date(at(2026, 10, 27, 9).to_unix(), 1, 2026),
            "Yesterday"
        );
        assert_eq!(
            spoken_date(at(2026, 5, 3, 9).to_unix(), 3, 2026),
            format!("{may} 3")
        );
        assert_eq!(spoken_date(at(2026, 10, 28, 9).to_unix(), 0, 2026), "09:05");
    }

    #[test]
    fn the_date_column_holds_every_form() {
        let samples = date_samples(2026);
        // A time, each month this year, and the widest date before it.
        assert_eq!(samples.len(), 14);
        assert_eq!(samples[0], "00:00");
        assert!(samples[1..13].iter().all(|date| date.ends_with(" 28")));
        assert!(samples[13].contains("12") && samples[13].contains("28"));
        assert!(samples[13].contains("25"));
    }

    #[test]
    fn older_dates_follow_the_locales_order() {
        use Part::{Day, Month, Year};
        assert_eq!(numeric_order("02/03/2001"), ([Month, Day, Year], '/'));
        assert_eq!(numeric_order("03/02/01"), ([Day, Month, Year], '/'));
        assert_eq!(numeric_order("03.02.2001"), ([Day, Month, Year], '.'));
        assert_eq!(numeric_order("2001-02-03"), ([Year, Month, Day], '-'));
        assert_eq!(numeric_order("03. feb. 2001"), ([Day, Month, Year], '.'));
        assert_eq!(numeric_order(""), ([Month, Day, Year], '/'));
        // Units after the numbers (ja_JP, zh_CN, ko_KR): the numbers in their order, with "/",
        // never "25年10年4".
        assert_eq!(numeric_order("2001年02月03日"), ([Year, Month, Day], '/'));
        assert_eq!(numeric_order("2001년 02월 03일"), ([Year, Month, Day], '/'));
        assert_eq!(numeric_order("2001. 02. 03."), ([Year, Month, Day], '.'));
    }

    #[test]
    fn the_sender_column_narrows_with_the_list() {
        // 192 px at its widest (180 and the gap), as the default text size has it.
        assert_eq!(sender_column(1392, 192, 60), 192, "1440 px: full");
        assert_eq!(sender_column(960, 192, 60), 192);
        assert_eq!(sender_column(800, 192, 60), 160);
        assert_eq!(sender_column(662, 192, 60), 134, "the 710 px half tile");
        assert_eq!(sender_column(400, 192, 60), 134, "never under the floor");
        // What the column's parts need wins over the floor, never over the widest.
        assert_eq!(sender_column(400, 192, 150), 150);
        assert_eq!(sender_column(400, 192, 250), 192);
    }

    fn summary() -> MessageSummary {
        MessageSummary {
            id: MessageId(1),
            folder: FolderId(1),
            thread: ThreadId(1),
            sender_name: String::new(),
            sender_email: "leah@example.test".into(),
            subject: "Re: SV: Plans".into(),
            snippet: String::new(),
            date: 0,
            flags: Flags::FLAGGED,
            has_attachments: true,
            thread_count: 3,
        }
    }

    #[test]
    fn message_rows_fill_the_fields() {
        let row = summary();
        let sender = [sender(&row)];
        let fields = RowFields::message(&row, &sender);
        assert_eq!(fields.participants[0].name, "leah@example.test");
        assert!(fields.unread && fields.participants[0].unread);
        assert_eq!(fields.subject(), "Plans");
        assert_eq!((fields.count, fields.draft), (3, false));
        let draft = MessageSummary {
            subject: "Re:".into(),
            flags: Flags::SEEN.union(Flags::DRAFT),
            ..summary()
        };
        let sender = [super::sender(&draft)];
        let fields = RowFields::message(&draft, &sender);
        assert!(fields.draft && !fields.unread);
        assert_eq!(fields.subject(), "(no subject)");
    }

    fn writer(name: &str, email: &str, is_me: bool, unread: bool) -> Writer {
        Writer {
            name: name.into(),
            email: email.into(),
            is_me,
            unread,
            last: 0,
        }
    }

    fn trip() -> ConversationSummary {
        ConversationSummary {
            thread: ThreadId(7),
            account: AccountId(1),
            anchor: MessageId(71),
            folder: FolderId(1),
            last_member: MessageId(76),
            latest: MessageId(77),
            date: 1_791_072_000,
            sender_name: "Alex Berg".into(),
            sender_email: "alex.berg@example.test".into(),
            snippet: "See you Friday.".into(),
            subject: "Trip to Bergen".into(),
            participants: vec![
                writer("Ingrid Solberg", "ingrid@example.test", false, true),
                writer("", "tomas@example.test", false, false),
                writer("Alex Berg", "alex.berg@example.test", true, false),
            ],
            participant_count: 3,
            messages: 6,
            members: 5,
            unread: 2,
            flagged: 1,
            attachments: 0,
            has_draft: true,
        }
    }

    #[test]
    fn conversation_rows_fill_the_fields_from_the_conversation() {
        let row = trip();
        let people = people(&row);
        // A writer without a name is named by the address.
        assert_eq!(people[1].name, "tomas@example.test");
        assert!(people[2].me && people[0].unread);
        let fields = RowFields::conversation(&row, &people);
        assert_eq!(
            (fields.count, fields.draft, fields.writers),
            (6, true, 3),
            "the count is the conversation's messages, not its members here"
        );
        assert!(fields.unread && fields.starred && !fields.attachment);
        assert_eq!(fields.address, "ingrid@example.test");
        assert_eq!(fields.snippet, "See you Friday.");
        let (text, emphasis) = participants::column(fields.participants, fields.unread);
        assert_eq!(text, "Ingrid, tomas, me");
        assert_eq!(emphasis, Emphasis::Ranges(vec![(0, 6)]));
        let read = ConversationSummary {
            unread: 0,
            flagged: 0,
            has_draft: false,
            messages: 1,
            ..trip()
        };
        let fields = RowFields::conversation(&read, &people);
        assert!(!fields.unread && !fields.starred && !fields.draft);
    }

    #[test]
    fn rows_say_their_state_in_words() {
        let row = MessageSummary {
            subject: String::new(),
            ..summary()
        };
        let sender = [sender(&row)];
        assert_eq!(
            accessible_text(
                &RowFields::message(&row, &sender),
                "10:32",
                RowReminder::default()
            ),
            "Unread, leah@example.test, 3 messages, no subject, 10:32, starred, has attachments"
        );
        let conversation = trip();
        let people = people(&conversation);
        assert_eq!(
            accessible_text(
                &RowFields::conversation(&conversation, &people),
                "Oct 4",
                RowReminder::default()
            ),
            "Unread, Draft, Ingrid, tomas and you, 6 messages, Trip to Bergen, Oct 4, starred"
        );
        // Past the four people a row carries, the others are counted.
        let crowded = ConversationSummary {
            participant_count: 7,
            ..trip()
        };
        let fields = RowFields::conversation(&crowded, &people);
        assert!(
            accessible_text(&fields, "Oct 4", RowReminder::default())
                .contains("Ingrid, tomas, you and 4 others")
        );
        let alone = [person("Ingrid Solberg", false)];
        let fields = RowFields {
            participants: &alone,
            writers: 1,
            unread: false,
            draft: false,
            count: 1,
            starred: false,
            attachment: false,
            ..RowFields::conversation(&conversation, &people)
        };
        assert_eq!(
            accessible_text(&fields, "Yesterday", RowReminder::default()),
            "Ingrid Solberg, Trip to Bergen, Yesterday"
        );
        // Back from Reminders (phase C), it says so.
        let back = RowReminder {
            returned: true,
            wake: None,
        };
        assert_eq!(
            accessible_text(&fields, "Yesterday", back),
            "Ingrid Solberg, Trip to Bergen, Yesterday, back from a reminder"
        );
    }

    #[test]
    fn wake_times_read_as_the_date_column_shows_dates() {
        let local = glib::TimeZone::local();
        let at = |month, day, hour, minute| {
            glib::DateTime::new(&local, 2026, month, day, hour, minute, 0.0)
                .unwrap()
                .to_unix()
        };
        // Thursday 29 October 2026, 14:20.
        let now = at(10, 29, 14, 20);
        let weekday = |unix: i64, format: &str| {
            glib::DateTime::from_unix_local(unix)
                .unwrap()
                .format(format)
                .unwrap()
                .trim()
                .trim_end_matches('.')
                .to_owned()
        };
        assert_eq!(wake_text(remind::SOMEDAY, now, 2026), "SOMEDAY");
        assert_eq!(wake_spoken(remind::SOMEDAY, now, 2026), "back someday");
        // Today: the time.
        assert_eq!(wake_text(at(10, 29, 17, 30), now, 2026), "17:30");
        assert_eq!(
            wake_spoken(at(10, 29, 17, 30), now, 2026),
            "back today 17:30"
        );
        // Within the week: the weekday and the time, in the list's capitals.
        let tuesday = at(11, 3, 8, 0);
        assert_eq!(
            wake_text(tuesday, now, 2026),
            format!("{} 08:00", weekday(tuesday, "%a").to_uppercase())
        );
        assert_eq!(
            wake_spoken(tuesday, now, 2026),
            format!("back {} 08:00", weekday(tuesday, "%A"))
        );
        // Later: the date, as any row's.
        let later = at(11, 12, 8, 0);
        assert_eq!(wake_text(later, now, 2026), time_text(later, 2, 2026));
        assert_eq!(
            wake_spoken(later, now, 2026),
            format!("back {}", spoken_date(later, 2, 2026))
        );
        // The column holds every form, and Someday.
        let samples = wake_samples(2026);
        assert!(samples.contains(&"SOMEDAY".to_owned()));
        assert_eq!(
            samples
                .iter()
                .filter(|text| text.ends_with(" 00:00"))
                .count(),
            7
        );
    }
}
