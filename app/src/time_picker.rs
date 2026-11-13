//! The time picker (the fork's phase B): Send Later's, and Remind Me's (phase C,
//! [`pick_reminder`]). A [`crate::picker`] titled by its caller, with presets (Later today,
//! Tomorrow morning, This weekend, Next week) showing when they are, and a field that reads a
//! typed time live ("tomorrow 9am", "mon", "in 3 days", "2h", `domain::when`): what it reads is
//! the first row, with its time; presets whose names match stay below. Enter takes the selected
//! row. Remind Me's names Tomorrow morning "Tomorrow", and adds Someday (no time: it comes back
//! only when removed; "someday" and "never" typed choose it) and, for a conversation that has a
//! reminder, Remove Reminder.
//!
//! `domain::when` works on the wall clock; this module turns its times into instants in the
//! local zone (GLib's, which gets summer time right), and says them back ([`describe`]).

use crate::picker::{self, Row, Spec};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::palette;
use sealwax_domain::when::{self, Civil, Preset, Problem, Target};
use std::cell::RefCell;
use std::rc::Rc;

/// Send Later's title (the composer's and the inline reply's Ctrl+Shift+L).
pub const SEND_LATER: &str = "Send later";
/// Remind Me's title (`h`, phase C).
pub const REMIND_ME: &str = "Remind Me";
/// Remind Me's Someday row: its title and its detail.
pub const SOMEDAY: &str = "Someday";
pub const SOMEDAY_DETAIL: &str = "no date";
/// Remind Me's row for a conversation that has a reminder.
pub const REMOVE: &str = "Remove Reminder";
/// The picker's width: times are short.
pub const WIDTH: i32 = 480;
/// What the field offers before anything is typed.
pub const PLACEHOLDER: &str = "Type a time, like tomorrow 9am";
/// The hint while what is typed isn't a time.
pub const UNREADABLE: &str =
    "Not a time Pronto reads: try tomorrow 9am, mon 8:30, in 3 days or nov 5";
pub const PASSED: &str = "That time has passed";

/// A time the user chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    /// When, in Unix milliseconds.
    pub at: i64,
    /// How it was said from the moment it was chosen: "tomorrow 08:00", "Tue 08:00".
    pub label: String,
}

/// The local time now.
pub fn now() -> glib::DateTime {
    glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("a clock"))
}

/// `time` on the wall clock, to the minute.
pub fn civil(time: &glib::DateTime) -> Civil {
    Civil {
        year: time.year(),
        month: time.month() as u8,
        day: time.day_of_month() as u8,
        hour: time.hour() as u8,
        minute: time.minute() as u8,
    }
}

/// The instant `target` names from `now`, in the local zone: a wall-clock time at its first
/// second, a span added to the instant (so a summer-time change between doesn't shift it).
pub fn instant(target: Target, now: &glib::DateTime) -> Option<glib::DateTime> {
    match target {
        Target::At(at) => glib::DateTime::from_local(
            at.year,
            i32::from(at.month),
            i32::from(at.day),
            i32::from(at.hour),
            i32::from(at.minute),
            0.0,
        )
        .ok(),
        Target::In(minutes) => now.add_minutes(i32::try_from(minutes).ok()?).ok(),
    }
}

fn unix_ms(time: &glib::DateTime) -> i64 {
    time.to_unix() * 1000 + i64::from(time.microsecond()) / 1000
}

/// "tomorrow 08:00", "Tue 08:00", "Thu, Nov 5, 08:00": `at` (Unix ms) said from now.
pub fn describe(at: i64) -> String {
    let now = now();
    match glib::DateTime::from_unix_local(at.div_euclid(1000)) {
        Ok(time) => when::label(civil(&time), civil(&now)),
        Err(_) => "later".to_owned(),
    }
}

/// One row's choice: when, or nothing (a hint).
type Choice = Option<Chosen>;

/// What Remind Me's picker chooses (phase C).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reminding {
    /// Back at a time.
    At(Chosen),
    /// Never on its own: it waits until the user removes it.
    Someday,
    /// The conversation's reminder ends; one waiting comes back now.
    Remove,
}

impl Reminding {
    /// The wake time the store keeps (Unix seconds); none to remove one.
    pub fn wake_at(&self) -> Option<i64> {
        match self {
            Reminding::At(chosen) => Some(chosen.at.div_euclid(1000)),
            Reminding::Someday => Some(sealwax_domain::remind::SOMEDAY),
            Reminding::Remove => None,
        }
    }

    /// When, as a toast says it: "tomorrow 08:00", "someday".
    pub fn label(&self) -> String {
        match self {
            Reminding::At(chosen) => chosen.label.clone(),
            Reminding::Someday | Reminding::Remove => "someday".to_owned(),
        }
    }
}

/// A reminder's time as the picker and the reader say it: `describe`'s, or "someday".
pub fn describe_wake(wake_at: i64) -> String {
    if sealwax_domain::remind::is_someday(wake_at) {
        "someday".to_owned()
    } else {
        describe(wake_at.saturating_mul(1000))
    }
}

/// A preset as Remind Me names it (phase C): "Tomorrow", as Superhuman and the brief name it,
/// the time beside it saying the morning; the others as Send Later names them.
pub fn remind_title(preset: Preset) -> &'static str {
    match preset {
        Preset::TomorrowMorning => "Tomorrow",
        preset => preset.title(),
    }
}

/// The rows for `query` at `now`, and what each chooses.
fn listing(query: &str, presets: &[Preset], now: &glib::DateTime) -> (Vec<Row>, Vec<Choice>) {
    named_listing(query, presets, Preset::title, now)
}

/// [`listing`], each preset named by `title`.
fn named_listing(
    query: &str,
    presets: &[Preset],
    title: fn(Preset) -> &'static str,
    now: &glib::DateTime,
) -> (Vec<Row>, Vec<Choice>) {
    let today = civil(now);
    let resolved: Vec<Option<(Row, Chosen)>> = presets
        .iter()
        .map(|preset| {
            let at = preset.resolve(today)?;
            let time = instant(Target::At(at), now)?;
            // A row's words start with a capital, as the typed time's row does ("Tomorrow
            // 08:00" beside "Tomorrow 09:00"); a sentence takes the label ("Sending tomorrow").
            Some((
                Row::new(title(*preset), when::title(at, today)),
                Chosen {
                    at: unix_ms(&time),
                    label: when::label(at, today),
                },
            ))
        })
        .collect();
    let mut rows = Vec::new();
    let mut choices: Vec<Choice> = Vec::new();
    if query.trim().is_empty() {
        for (row, chosen) in resolved.into_iter().flatten() {
            rows.push(row);
            choices.push(Some(chosen));
        }
        return (rows, choices);
    }
    let read = when::parse(query, today);
    if let Ok(target) = read
        && let Some(time) = instant(target, now)
    {
        let at = civil(&time);
        // Its day beside it ("Tomorrow 09:00 · Fri, Oct 30"), unless it says the day already
        // ("Thu, Nov 12, 09:00").
        let (title, day) = (when::title(at, today), when::day(at, today));
        let detail = if title.starts_with(&day) {
            String::new()
        } else {
            day
        };
        rows.push(Row::new(title, detail));
        choices.push(Some(Chosen {
            at: unix_ms(&time),
            label: when::label(at, today),
        }));
    }
    // The presets the query names, best first; one the typed time already is shows once.
    let titles = presets.iter().map(|preset| title(*preset));
    for index in palette::rank(query, titles) {
        let Some((row, chosen)) = resolved[index].clone() else {
            continue;
        };
        if choices.iter().flatten().any(|known| known.at == chosen.at) {
            continue;
        }
        rows.push(row);
        choices.push(Some(chosen));
    }
    if rows.is_empty() {
        let hint = match read {
            Err(Problem::Passed) => PASSED,
            _ => UNREADABLE,
        };
        rows.push(Row::hint(hint));
        choices.push(None);
    }
    (rows, choices)
}

/// Remind Me's rows for `query` at `now` (phase C): Send Later's ([`listing`]: the presets with
/// their times, named as Remind Me names them, [`remind_title`]; what is typed read as a time),
/// then Someday, and Remove Reminder when the conversation has a reminder (`removing`: its
/// detail, "set for Tue 08:00", or "3 reminders"). "someday" or "never" typed puts Someday
/// first; a typed word names Someday or Remove Reminder as it names a preset. The hint shows
/// only when nothing at all matches.
fn remind_listing(
    query: &str,
    presets: &[Preset],
    removing: Option<&str>,
    now: &glib::DateTime,
) -> (Vec<Row>, Vec<Option<Reminding>>) {
    let (timed, chosen) = named_listing(query, presets, remind_title, now);
    let mut rows = Vec::new();
    let mut choices = Vec::new();
    let someday_typed = when::is_someday(query);
    if someday_typed {
        rows.push(Row::new(SOMEDAY, SOMEDAY_DETAIL));
        choices.push(Some(Reminding::Someday));
    }
    let mut hint = None;
    for (row, choice) in timed.into_iter().zip(chosen) {
        if row.hint {
            hint = Some(row);
            continue;
        }
        rows.push(row);
        choices.push(choice.map(Reminding::At));
    }
    let mut extra = vec![(Row::new(SOMEDAY, SOMEDAY_DETAIL), Reminding::Someday)];
    if let Some(removing) = removing {
        extra.push((Row::new(REMOVE, removing), Reminding::Remove));
    }
    let named: Vec<usize> = if query.trim().is_empty() {
        (0..extra.len()).collect()
    } else {
        palette::rank(query, extra.iter().map(|(row, _)| row.title.as_str()))
    };
    for index in named {
        let (row, choice) = extra[index].clone();
        if someday_typed && choice == Reminding::Someday {
            continue;
        }
        rows.push(row);
        choices.push(Some(choice));
    }
    if rows.is_empty() {
        rows.push(hint.unwrap_or_else(|| Row::hint(UNREADABLE)));
        choices.push(None);
    }
    (rows, choices)
}

/// Opens Remind Me's picker over `window` (phase C): titled "Remind Me", with `presets`, Someday
/// and, when the conversation has a reminder, Remove Reminder saying `removing` ("set for
/// tomorrow 08:00"); `chosen` gets what the user picks. Escape, or a click beside it, picks
/// nothing.
pub fn pick_reminder(
    window: &impl IsA<gtk::Window>,
    presets: &[Preset],
    removing: Option<String>,
    chosen: impl FnOnce(Reminding) + 'static,
) {
    let presets = presets.to_vec();
    let choices: Rc<RefCell<Vec<Option<Reminding>>>> = Rc::default();
    let listed = choices.clone();
    let rows = move |query: &str| {
        let (rows, found) = remind_listing(query, &presets, removing.as_deref(), &now());
        listed.replace(found);
        rows
    };
    let chosen = RefCell::new(Some(chosen));
    let choose = move |index: usize, _query: &str| {
        let choice = choices.borrow().get(index).cloned().flatten();
        if let (Some(choice), Some(chosen)) = (choice, chosen.borrow_mut().take()) {
            chosen(choice);
        }
    };
    picker::of(window).open(Spec {
        title: REMIND_ME.to_owned(),
        placeholder: PLACEHOLDER.to_owned(),
        name: "remind-picker",
        width: WIDTH,
        icon: picker::TIME_ICON,
        details: picker::Details::End,
        rows: Box::new(rows),
        choose: Box::new(choose),
    });
}

/// Opens the time picker over `window`, titled `title` ("Send later"), with `presets`;
/// `chosen` gets the time the user picks. Escape, or a click beside it, picks nothing.
pub fn pick(
    window: &impl IsA<gtk::Window>,
    title: &str,
    presets: &[Preset],
    chosen: impl FnOnce(Chosen) + 'static,
) {
    let presets = presets.to_vec();
    let choices: Rc<RefCell<Vec<Choice>>> = Rc::default();
    let listed = choices.clone();
    let rows = move |query: &str| {
        let (rows, found) = listing(query, &presets, &now());
        listed.replace(found);
        rows
    };
    let chosen = RefCell::new(Some(chosen));
    let choose = move |index: usize, _query: &str| {
        let choice = choices.borrow().get(index).cloned().flatten();
        if let (Some(choice), Some(chosen)) = (choice, chosen.borrow_mut().take()) {
            chosen(choice);
        }
    };
    picker::of(window).open(Spec {
        title: title.to_owned(),
        placeholder: PLACEHOLDER.to_owned(),
        name: "time-picker",
        width: WIDTH,
        icon: picker::TIME_ICON,
        details: picker::Details::End,
        rows: Box::new(rows),
        choose: Box::new(choose),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> glib::DateTime {
        glib::DateTime::from_local(year, month, day, hour, minute, 0.0).unwrap()
    }

    fn titles(rows: &[Row]) -> Vec<(String, String)> {
        rows.iter()
            .map(|row| (row.title.clone(), row.detail.clone()))
            .collect()
    }

    #[test]
    fn presets_show_when_they_are() {
        // Thursday 29 October 2026, 14:20.
        let now = local(2026, 10, 29, 14, 20);
        let (rows, choices) = listing("", &Preset::ALL, &now);
        assert_eq!(
            titles(&rows),
            [
                ("Later today".to_owned(), "Today 17:30".to_owned()),
                ("Tomorrow morning".to_owned(), "Tomorrow 08:00".to_owned()),
                ("This weekend".to_owned(), "Sat 08:00".to_owned()),
                ("Next week".to_owned(), "Mon 08:00".to_owned()),
            ]
        );
        assert_eq!(
            choices[1].as_ref().unwrap().at,
            unix_ms(&local(2026, 10, 30, 8, 0))
        );
        // A sentence ("Sending tomorrow 08:00") takes the label in lower case.
        assert_eq!(choices[1].as_ref().unwrap().label, "tomorrow 08:00");
        // Late in the evening there is no later today.
        let (rows, _) = listing("", &Preset::ALL, &local(2026, 10, 29, 22, 0));
        assert_eq!(rows[0].title, "Tomorrow morning");
    }

    #[test]
    fn a_typed_time_is_the_first_row() {
        let now = local(2026, 10, 29, 14, 20);
        let (rows, choices) = listing("tomorrow 9am", &Preset::ALL, &now);
        assert_eq!(
            titles(&rows)[0],
            ("Tomorrow 09:00".to_owned(), "Fri, Oct 30".to_owned())
        );
        assert_eq!(
            choices[0].as_ref().unwrap().at,
            unix_ms(&local(2026, 10, 30, 9, 0))
        );
        assert_eq!(choices[0].as_ref().unwrap().label, "tomorrow 09:00");
        // A span is from now, to the second.
        let (rows, choices) = listing("2h", &Preset::ALL, &now);
        assert_eq!(rows[0].title, "Today 16:20");
        assert_eq!(choices[0].as_ref().unwrap().at, unix_ms(&now) + 7_200_000);
        // A preset's name finds the preset; one the typed time already is shows once.
        let (rows, _) = listing("week", &Preset::ALL, &now);
        assert_eq!(rows.len(), 2, "{:?}", titles(&rows));
        assert!(rows.iter().any(|row| row.title == "Next week"));
        assert!(rows.iter().any(|row| row.title == "This weekend"));
        let (rows, _) = listing("next week", &Preset::ALL, &now);
        assert_eq!(
            titles(&rows),
            [("Mon 08:00".to_owned(), "Mon, Nov 2".to_owned())]
        );
        // A date that names its day already says it once.
        let (rows, _) = listing("nov 12 9am", &Preset::ALL, &now);
        assert_eq!(
            titles(&rows)[0],
            ("Thu, Nov 12, 09:00".to_owned(), String::new())
        );
        // What isn't a time says so, and can't be chosen.
        let (rows, choices) = listing("banana", &Preset::ALL, &now);
        assert_eq!(rows, [Row::hint(UNREADABLE)]);
        assert_eq!(choices, [None]);
        let (rows, _) = listing("today 9am", &Preset::ALL, &now);
        assert_eq!(rows, [Row::hint(PASSED)]);
    }

    #[test]
    fn remind_me_adds_someday_and_remove() {
        let now = local(2026, 10, 29, 14, 20);
        let (rows, choices) = remind_listing("", &Preset::ALL, None, &now);
        // Tomorrow as Superhuman names it; Send Later keeps "Tomorrow morning".
        assert_eq!(
            titles(&rows)[..4],
            [
                ("Later today".to_owned(), "Today 17:30".to_owned()),
                ("Tomorrow".to_owned(), "Tomorrow 08:00".to_owned()),
                ("This weekend".to_owned(), "Sat 08:00".to_owned()),
                ("Next week".to_owned(), "Mon 08:00".to_owned()),
            ]
        );
        assert_eq!(
            titles(&rows)[4..],
            [(SOMEDAY.to_owned(), SOMEDAY_DETAIL.to_owned())]
        );
        assert_eq!(choices[4], Some(Reminding::Someday));
        assert_eq!(
            choices[1].as_ref().and_then(Reminding::wake_at),
            Some(local(2026, 10, 30, 8, 0).to_unix())
        );
        // A conversation with a reminder: Remove Reminder, saying when it is set for.
        let (rows, choices) = remind_listing("", &Preset::ALL, Some("set for Tue 08:00"), &now);
        assert_eq!(
            titles(&rows).last().unwrap(),
            &(REMOVE.to_owned(), "set for Tue 08:00".to_owned())
        );
        assert_eq!(choices.last().unwrap(), &Some(Reminding::Remove));
        // Typed: "someday" and "never" are Someday; a word finds it or Remove by name.
        for typed in ["someday", "never"] {
            let (rows, choices) = remind_listing(typed, &Preset::ALL, None, &now);
            assert_eq!(rows[0].title, SOMEDAY, "{typed}");
            assert_eq!(choices[0], Some(Reminding::Someday));
            assert_eq!(rows.iter().filter(|row| row.title == SOMEDAY).count(), 1);
        }
        let (rows, _) = remind_listing("some", &Preset::ALL, None, &now);
        assert_eq!(
            titles(&rows),
            [(SOMEDAY.to_owned(), SOMEDAY_DETAIL.to_owned())]
        );
        let (rows, choices) = remind_listing("remove", &Preset::ALL, Some("3 reminders"), &now);
        assert_eq!(rows[0], Row::new(REMOVE, "3 reminders"));
        assert_eq!(choices[0], Some(Reminding::Remove));
        // A typed time first, its day beside it; what isn't a time says so.
        let (rows, choices) = remind_listing("mon 9", &Preset::ALL, None, &now);
        assert_eq!(rows[0].title, "Mon 09:00");
        assert_eq!(
            choices[0].as_ref().map(Reminding::label).as_deref(),
            Some("Mon 09:00")
        );
        let (rows, choices) = remind_listing("banana", &Preset::ALL, None, &now);
        assert_eq!(rows, [Row::hint(UNREADABLE)]);
        assert_eq!(choices, [None]);
        // Its name finds Tomorrow (the typed day, at 08:00, is the same time, shown once).
        let (rows, _) = remind_listing("tomorrow", &Preset::ALL, None, &now);
        assert_eq!(
            titles(&rows),
            [("Tomorrow 08:00".to_owned(), "Fri, Oct 30".to_owned())]
        );
        let (rows, _) = remind_listing("tom", &Preset::ALL, None, &now);
        assert!(rows.iter().any(|row| row.title == "Tomorrow"), "{rows:?}");
        assert_eq!(Reminding::Someday.label(), "someday");
        assert_eq!(describe_wake(sealwax_domain::remind::SOMEDAY), "someday");
    }

    #[test]
    fn instants_are_in_the_local_zone() {
        let now = local(2026, 10, 29, 14, 20);
        let at = instant(
            Target::At(Civil {
                year: 2026,
                month: 11,
                day: 5,
                hour: 8,
                minute: 0,
            }),
            &now,
        )
        .unwrap();
        assert_eq!(civil(&at).hour, 8);
        assert_eq!(civil(&at).day, 5);
        let soon = instant(Target::In(30), &now).unwrap();
        assert_eq!(unix_ms(&soon) - unix_ms(&now), 30 * 60_000);
        assert!(describe(unix_ms(&super::now()) + 3_600_000).contains(':'));
    }
}
