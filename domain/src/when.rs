//! Times typed in plain words, for Send Later and Remind Me (the fork's phase B, Superhuman's
//! "tomorrow 9am"): [`parse`] reads "tomorrow", "tomorrow 9am", "tonight", "this weekend", "next
//! week", "mon", "monday 8:30", "in 3 days", "2h", "30m", "nov 5", "5 nov", 24-hour and am/pm
//! times; [`Preset`] is the picker's suggestions (Later today, Tomorrow morning, This weekend,
//! Next week); [`label`] says a time back ("tomorrow 08:00", "Tue 08:00").
//!
//! Pure: everything is in the user's wall-clock time ([`Civil`]), and "now" is the caller's, so
//! the tests run on a fixed now. The app turns a [`Target`] into an instant in the local zone
//! (`app/src/time_picker.rs`, GLib's), which handles summer time.
//!
//! The choices, where words leave room (docs/superhuman/writing.md lists them too):
//! - A day without a time is at 08:00 (Superhuman's default); "morning" is 08:00, "afternoon"
//!   13:00, "evening" 18:00, "night" and "tonight" 20:00.
//! - A time without "am" or "pm" is 24-hour ("5" is 05:00, "17" is 17:00), as Pronto shows
//!   times; after "afternoon", "evening", "night" or "tonight" an hour before 12 is the
//!   afternoon's ("tonight 9" is 21:00), and 12 or midnight after "evening", "night" or
//!   "tonight" is the night's end, the next day's 00:00 ("tonight 12"; "tonight 12pm" is refused).
//!   A night runs past midnight: 1 to 4 after "night" or "tonight" (or 1am to 4am) are its small
//!   hours, the next day's ("tonight 1" is 01:00 tomorrow); 5 to 11 are the evening's. In the
//!   small hours themselves, the night still on is meant while its time is ahead ("tonight 1"
//!   at 00:30 is in half an hour, not a day later). "evening 1" to "evening 4" are refused: an
//!   evening doesn't run past midnight, and 13:00 isn't the evening.
//! - "midnight" is a day's end, the next day's 00:00: alone, tonight's; "tomorrow midnight" is
//!   tomorrow night's, "fri midnight" Friday night's (as "due Friday at midnight" says).
//! - A dotted number is a time, as the colon form ("9.30", and "5.11" is 05:11): a date is
//!   written with its month's name ("5 nov"), since the order of a numeric date's day and month
//!   (5/11, 5.11) depends on where one lives. The picker says the date it read beside the time.
//! - A time that has passed today means tomorrow ("9am" at 10:00), a weekday is the next one
//!   after today ("mon" on a Monday is next week's), a date without a year that has passed is
//!   next year's. "today" or "tonight" with a time that has passed is refused rather than moved:
//!   the words say today. "tonight" alone once 20:00 is near is the next whole hour at least 30
//!   minutes away, while that is still today.
//! - "next" with a weekday is that day of next week, weeks starting on Monday, as "fri next week"
//!   says: on a Thursday "fri" and "this fri" are tomorrow, "next fri" a week later.
//! - Hours and minutes ("2h", "in 30 minutes") are exact spans; days, weeks and months ("3d",
//!   "in 2 weeks", "1mo") move the date and keep the day's 08:00 (or the time given). A span of
//!   nothing ("0h") isn't a time.
//! - "this weekend" is Saturday; on a Saturday once its time has passed, Sunday; on a Sunday
//!   once its time has passed, the next Saturday. "next week" is the next Monday.
//! - "Later today" is three hours from now, rounded up to the next half hour, while that is
//!   still today; it isn't offered later in the evening ("this evening", 18:00, is a word).
//! - A bare number is an hour ("10" is 10:00), not Superhuman's 10 October.

use std::fmt;

/// A wall-clock time in the user's zone, to the minute. Ordered as time goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Civil {
    pub year: i32,
    /// 1 to 12.
    pub month: u8,
    /// 1 to the month's length.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

/// A day of the week.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    /// Monday is 0.
    pub const fn index(self) -> i64 {
        self as i64
    }

    /// "Mon", as the labels write it.
    pub const fn short(self) -> &'static str {
        match self {
            Self::Monday => "Mon",
            Self::Tuesday => "Tue",
            Self::Wednesday => "Wed",
            Self::Thursday => "Thu",
            Self::Friday => "Fri",
            Self::Saturday => "Sat",
            Self::Sunday => "Sun",
        }
    }

    /// The names a user may type for it: the whole name, or its first three letters and more.
    fn read(word: &str) -> Option<Self> {
        const NAMES: [&str; 7] = [
            "monday",
            "tuesday",
            "wednesday",
            "thursday",
            "friday",
            "saturday",
            "sunday",
        ];
        (word.len() >= 3)
            .then(|| NAMES.iter().position(|name| name.starts_with(word)))
            .flatten()
            .map(|index| Self::ALL[index])
    }
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// "Jan" to "Dec".
pub fn month_short(month: u8) -> &'static str {
    const SHORT: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    SHORT[usize::from(month.clamp(1, 12)) - 1]
}

/// "January" to "December".
pub fn month_name(month: u8) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[usize::from(month.clamp(1, 12)) - 1]
}

/// A month's name as typed: the whole name, or its first three letters and more ("sept").
fn read_month(word: &str) -> Option<u8> {
    (word.len() >= 3)
        .then(|| MONTHS.iter().position(|name| name.starts_with(word)))
        .flatten()
        .map(|index| index as u8 + 1)
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// How many days `month` of `year` has.
pub fn month_length(year: i32, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Civil {
    /// The time, when it exists.
    pub fn new(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> Option<Self> {
        ((1..=12).contains(&month)
            && day >= 1
            && day <= month_length(year, month)
            && hour < 24
            && minute < 60)
            .then_some(Self {
                year,
                month,
                day,
                hour,
                minute,
            })
    }

    /// Days since 1 January 1970 (Howard Hinnant's `days_from_civil`).
    pub fn days(self) -> i64 {
        let year = i64::from(self.year) - i64::from(self.month <= 2);
        let era = year.div_euclid(400);
        let yoe = year.rem_euclid(400);
        let month = i64::from(self.month);
        let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5
            + i64::from(self.day)
            - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// The date `days` after 1 January 1970, at `hour`:`minute`.
    pub fn from_days(days: i64, hour: u8, minute: u8) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year: year as i32,
            month: month as u8,
            day: day as u8,
            hour,
            minute,
        }
    }

    pub fn weekday(self) -> Weekday {
        Weekday::ALL[(self.days() + 3).rem_euclid(7) as usize]
    }

    /// Minutes since 1 January 1970 00:00 on the wall clock (no zone): for comparing and adding.
    pub fn minutes(self) -> i64 {
        self.days() * 1_440 + i64::from(self.hour) * 60 + i64::from(self.minute)
    }

    /// The wall-clock time `minutes` after 1 January 1970 00:00.
    pub fn from_minutes(minutes: i64) -> Self {
        let days = minutes.div_euclid(1_440);
        let rest = minutes.rem_euclid(1_440);
        Self::from_days(days, (rest / 60) as u8, (rest % 60) as u8)
    }

    /// The same date at `hour`:`minute`.
    pub fn at(self, hour: u8, minute: u8) -> Self {
        Self {
            hour,
            minute,
            ..self
        }
    }

    /// The same time of day, `days` later.
    pub fn plus_days(self, days: i64) -> Self {
        Self::from_days(self.days() + days, self.hour, self.minute)
    }

    /// `months` later, on the same day or the month's last.
    pub fn plus_months(self, months: i64) -> Self {
        let index = i64::from(self.year) * 12 + i64::from(self.month) - 1 + months;
        let year = index.div_euclid(12) as i32;
        let month = index.rem_euclid(12) as u8 + 1;
        Self {
            year,
            month,
            day: self.day.min(month_length(year, month)),
            ..self
        }
    }
}

impl fmt::Display for Civil {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute
        )
    }
}

/// The hour a day starts at when no time is given (Superhuman's 8 AM).
pub const DAY_START: u8 = 8;
/// "afternoon", "evening", "night" and "tonight".
pub const AFTERNOON: u8 = 13;
pub const EVENING: u8 = 18;
pub const NIGHT: u8 = 20;
/// Later today: this much from now, rounded up to the next half hour.
pub const LATER_TODAY_MINUTES: i64 = 180;
/// The longest span a time may be away: about five years.
const MAX_MINUTES: i64 = 5 * 366 * 1_440;

/// What a typed time means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A wall-clock time in the user's zone.
    At(Civil),
    /// So many minutes from now: hours and minutes are exact spans ("2h", "in 30 minutes").
    In(i64),
}

impl Target {
    /// The wall-clock time it names, from `now` (a span is added on the wall clock: the app adds
    /// it to the instant instead, so a summer-time change between doesn't shift it).
    pub fn civil(self, now: Civil) -> Civil {
        match self {
            Self::At(at) => at,
            Self::In(minutes) => Civil::from_minutes(now.minutes() + minutes),
        }
    }
}

/// Why a text isn't a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Nothing typed.
    Empty,
    /// Not a time this parser reads.
    Unknown,
    /// A time today ("today 9am", "tonight 7pm") that has passed, or a date with its year in
    /// the past.
    Passed,
}

/// The picker's suggestions, with what they resolve to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// Three hours from now, rounded up to the next half hour, while that is still today.
    LaterToday,
    /// Tomorrow at 08:00.
    TomorrowMorning,
    /// Saturday at 08:00 (see the module's choices).
    ThisWeekend,
    /// Monday at 08:00.
    NextWeek,
}

impl Preset {
    /// Send Later's suggestions, in their order (Remind Me reuses them).
    pub const ALL: [Self; 4] = [
        Self::LaterToday,
        Self::TomorrowMorning,
        Self::ThisWeekend,
        Self::NextWeek,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::LaterToday => "Later today",
            Self::TomorrowMorning => "Tomorrow morning",
            Self::ThisWeekend => "This weekend",
            Self::NextWeek => "Next week",
        }
    }

    /// When it is from `now`; `None` when it doesn't exist now (Later today late in the evening).
    pub fn resolve(self, now: Civil) -> Option<Civil> {
        match self {
            Self::LaterToday => later_today(now),
            Self::TomorrowMorning => Some(now.plus_days(1).at(DAY_START, 0)),
            Self::ThisWeekend => Some(weekend(now, false, (DAY_START, 0))),
            Self::NextWeek => Some(next_week(now).at(DAY_START, 0)),
        }
    }
}

fn later_today(now: Civil) -> Option<Civil> {
    let minutes = now.minutes() + LATER_TODAY_MINUTES;
    let rounded = minutes + (30 - minutes.rem_euclid(30)) % 30;
    let at = Civil::from_minutes(rounded);
    (at.days() == now.days()).then_some(at)
}

/// The next Saturday at `time`, or this weekend's day that is still ahead; with `next`, the
/// weekend after that.
fn weekend(now: Civil, next: bool, time: (u8, u8)) -> Civil {
    let today = on(now, time);
    let this = match now.weekday() {
        Weekday::Saturday | Weekday::Sunday if today > now => today,
        Weekday::Saturday => today.plus_days(1),
        Weekday::Sunday => today.plus_days(6),
        weekday => today.plus_days(Weekday::Saturday.index() - weekday.index()),
    };
    if !next {
        return this;
    }
    // The weekend after this one: a week after its Saturday.
    let saturday = match this.weekday() {
        Weekday::Sunday => this.plus_days(-1),
        _ => this,
    };
    saturday.plus_days(7)
}

/// The next Monday after today.
fn next_week(now: Civil) -> Civil {
    now.plus_days(7 - now.weekday().index())
}

/// The next day with `weekday` after today.
fn next_weekday(now: Civil, weekday: Weekday) -> Civil {
    let ahead = (weekday.index() - now.weekday().index()).rem_euclid(7);
    now.plus_days(if ahead == 0 { 7 } else { ahead })
}

/// "today 18:00", "tomorrow 08:00", "Tue 08:00" within the week, "Thu, Nov 5, 08:00" this year,
/// "Thu, Nov 5 2027, 08:00" after: when `at` is, seen from `now`. Month before day, as the list
/// and the reader write a date ("OCT 28", "Oct 28, 12:04"; phase C's visual QA).
pub fn label(at: Civil, now: Civil) -> String {
    let clock = format!("{:02}:{:02}", at.hour, at.minute);
    let weekday = at.weekday().short();
    match at.days() - now.days() {
        0 => format!("today {clock}"),
        1 => format!("tomorrow {clock}"),
        2..=6 => format!("{weekday} {clock}"),
        _ => format!("{}, {clock}", day(at, now)),
    }
}

/// The day of `at`, written out: "Fri, Oct 30", with the year when it isn't `now`'s ("Sat, Jan
/// 2 2027"): beside a relative label, it says which day "tomorrow" or "mon" is.
pub fn day(at: Civil, now: Civil) -> String {
    let weekday = at.weekday().short();
    let month = month_short(at.month);
    if at.year == now.year {
        format!("{weekday}, {month} {}", at.day)
    } else {
        format!("{weekday}, {month} {} {}", at.day, at.year)
    }
}

/// `label` with a capital first letter, for a heading or a row ("Tomorrow 08:00").
pub fn title(at: Civil, now: Civil) -> String {
    let text = label(at, now);
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

/// A part of the day a word names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Morning,
    Afternoon,
    Evening,
    Night,
}

impl Part {
    const fn hour(self) -> u8 {
        match self {
            Self::Morning => DAY_START,
            Self::Afternoon => AFTERNOON,
            Self::Evening => EVENING,
            Self::Night => NIGHT,
        }
    }

    fn read(word: &str) -> Option<Self> {
        Some(match word {
            "morning" => Self::Morning,
            "afternoon" => Self::Afternoon,
            "evening" | "eve" => Self::Evening,
            "night" => Self::Night,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Day {
    Today,
    Tomorrow,
    Weekday(Weekday),
    Date {
        month: u8,
        day: u8,
        year: Option<i32>,
    },
    Weekend {
        next: bool,
    },
    NextWeek,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Meridiem {
    Am,
    Pm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Clock {
    hour: u8,
    minute: u8,
    meridiem: Option<Meridiem>,
}

/// What the words said, before it is resolved against now.
#[derive(Debug, Default)]
struct Phrase {
    day: Option<Day>,
    /// A weekday written beside a date ("fri nov 6"): it must agree.
    weekday: Option<Weekday>,
    clock: Option<Clock>,
    part: Option<Part>,
    tonight: bool,
    later: bool,
    /// Exact minutes ("2h", "30m").
    minutes: i64,
    /// Days and months ("3d", "2w", "1mo").
    days: i64,
    months: i64,
    span: bool,
}

type Read<T> = Result<T, Problem>;

impl Phrase {
    fn set_day(&mut self, day: Day) -> Read<()> {
        if self.day.is_some() {
            return Err(Problem::Unknown);
        }
        self.day = Some(day);
        Ok(())
    }

    fn set_clock(&mut self, clock: Clock) -> Read<()> {
        if self.clock.is_some() {
            return Err(Problem::Unknown);
        }
        self.clock = Some(clock);
        Ok(())
    }

    fn set_part(&mut self, part: Part) -> Read<()> {
        if self.part.is_some_and(|known| known != part) {
            return Err(Problem::Unknown);
        }
        self.part = Some(part);
        Ok(())
    }

    fn add_span(&mut self, count: i64, unit: Unit) -> Read<()> {
        let days = MAX_MINUTES / 1_440;
        let (total, by, limit) = match unit {
            Unit::Minute => (&mut self.minutes, 1, MAX_MINUTES),
            Unit::Hour => (&mut self.minutes, 60, MAX_MINUTES),
            Unit::Day => (&mut self.days, 1, days),
            Unit::Week => (&mut self.days, 7, days),
            Unit::Month => (&mut self.months, 1, 60),
        };
        *total = count
            .checked_mul(by)
            .and_then(|more| total.checked_add(more))
            .filter(|sum| *sum <= limit)
            .ok_or(Problem::Unknown)?;
        self.span = true;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Minute,
    Hour,
    Day,
    Week,
    Month,
}

impl Unit {
    fn read(word: &str) -> Option<Self> {
        Some(match word {
            "m" | "min" | "mins" | "minute" | "minutes" => Self::Minute,
            "h" | "hr" | "hrs" | "hour" | "hours" => Self::Hour,
            "d" | "day" | "days" => Self::Day,
            "w" | "wk" | "wks" | "week" | "weeks" => Self::Week,
            "mo" | "mos" | "month" | "months" => Self::Month,
            _ => return None,
        })
    }
}

/// Spans written together, "2h", "1h30m", "3d": the number and unit pairs, or `None`.
fn compact_span(word: &str) -> Option<Vec<(i64, Unit)>> {
    let mut spans = Vec::new();
    let mut rest = word;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 || digits > 6 {
            return None;
        }
        let count: i64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let letters = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = Unit::read(&rest[..letters])?;
        rest = &rest[letters..];
        spans.push((count, unit));
    }
    (!spans.is_empty()).then_some(spans)
}

/// An hour and minutes: "9", "21", "9:30", "9.30", "21:05".
fn read_clock(word: &str) -> Option<(u8, Option<u8>)> {
    let (hour, minute) = match word.split_once([':', '.']) {
        Some((hour, minute)) => (hour, Some(minute)),
        None => (word, None),
    };
    if hour.is_empty() || hour.len() > 2 || !hour.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hour: u8 = hour.parse().ok()?;
    let minute = match minute {
        Some(minute) => {
            if minute.len() != 2 || !minute.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some(minute.parse::<u8>().ok().filter(|minute| *minute < 60)?)
        }
        None => None,
    };
    (hour < 24).then_some((hour, minute))
}

/// A day of the month, 1 to 31, with an ordinal ending or without ("5", "5th", "1st").
fn read_day_number(word: &str) -> Option<u8> {
    let digits = word
        .strip_suffix("st")
        .or_else(|| word.strip_suffix("nd"))
        .or_else(|| word.strip_suffix("rd"))
        .or_else(|| word.strip_suffix("th"))
        .unwrap_or(word);
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|day| (1..=31).contains(day))
}

/// A year written after a date: four digits, 2000 or later.
fn read_year(word: &str) -> Option<i32> {
    (word.len() == 4 && word.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| word.parse().ok())
        .flatten()
        .filter(|year| (2000..=9999).contains(year))
}

/// The words, lower case, with "9am", "9:30pm" and "5th" split from their endings and filler
/// words left out.
fn words(text: &str) -> Vec<String> {
    let lower = text
        .to_lowercase()
        .replace("a.m.", "am")
        .replace("p.m.", "pm")
        .replace([',', ';'], " ");
    let mut out = Vec::new();
    for word in lower.split_whitespace() {
        let word = word.trim_end_matches('.');
        let split = ["am", "pm"].iter().find_map(|ending| {
            word.strip_suffix(ending)
                .filter(|start| read_clock(start).is_some())
                .map(|start| (start, *ending))
        });
        match split {
            Some((clock, ending)) => {
                out.push(clock.to_owned());
                out.push(ending.to_owned());
            }
            None if matches!(word, "at" | "on" | "by" | "the" | "of" | "in" | "") => {}
            None => out.push(word.to_owned()),
        }
    }
    out
}

/// Whether `text` asks for no time at all: Remind Me's Someday ("someday", "some day", or
/// Superhuman's "never"; phase C). [`parse`] refuses these, as Send Later can't take them.
pub fn is_someday(text: &str) -> bool {
    let words = words(text);
    matches!(
        words
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["someday"] | ["some", "day"] | ["never"]
    )
}

/// Reads `text` as a time from `now`, in the user's wall-clock time. See the module's notes for
/// what each form means.
pub fn parse(text: &str, now: Civil) -> Result<Target, Problem> {
    let words = words(text);
    if words.is_empty() {
        return if text.trim().is_empty() {
            Err(Problem::Empty)
        } else {
            Err(Problem::Unknown)
        };
    }
    let phrase = read(&words)?;
    resolve(&phrase, now)
}

fn read(words: &[String]) -> Read<Phrase> {
    let mut phrase = Phrase::default();
    let mut index = 0;
    let word = |at: usize| words.get(at).map(String::as_str);
    while let Some(current) = word(index) {
        index += 1;
        match current {
            "today" | "tod" => phrase.set_day(Day::Today)?,
            "tonight" | "tonite" => {
                phrase.set_day(Day::Today)?;
                phrase.set_part(Part::Night)?;
                phrase.tonight = true;
            }
            "tomorrow" | "tmrw" | "tmr" | "tomorow" => phrase.set_day(Day::Tomorrow)?,
            "later" => phrase.later = true,
            // 12pm: never a night's midnight.
            "noon" | "midday" => phrase.set_clock(Clock {
                hour: 12,
                minute: 0,
                meridiem: Some(Meridiem::Pm),
            })?,
            // A day's end: the next day's 00:00 (hour 24).
            "midnight" => phrase.set_clock(Clock {
                hour: 24,
                minute: 0,
                meridiem: None,
            })?,
            "weekend" => phrase.set_day(Day::Weekend { next: false })?,
            "next" | "this" => {
                let next = current == "next";
                let Some(following) = word(index) else {
                    return Err(Problem::Unknown);
                };
                index += 1;
                match following {
                    "week" if next => phrase.set_day(Day::NextWeek)?,
                    "weekend" => phrase.set_day(Day::Weekend { next })?,
                    "month" if next => phrase.add_span(1, Unit::Month)?,
                    _ if !next && Part::read(following).is_some() => {
                        phrase.set_day(Day::Today)?;
                        phrase.set_part(Part::read(following).ok_or(Problem::Unknown)?)?;
                    }
                    _ => match Weekday::read(following) {
                        // "next fri": that day of next week, as "fri next week" says.
                        Some(weekday) if next => {
                            if phrase.weekday.is_some() {
                                return Err(Problem::Unknown);
                            }
                            phrase.set_day(Day::NextWeek)?;
                            phrase.weekday = Some(weekday);
                        }
                        Some(weekday) => phrase.set_day(Day::Weekday(weekday))?,
                        None => return Err(Problem::Unknown),
                    },
                }
            }
            "a" | "an" | "one" => match word(index).and_then(Unit::read) {
                Some(unit) => {
                    index += 1;
                    phrase.add_span(1, unit)?;
                }
                None => return Err(Problem::Unknown),
            },
            "half" => {
                // "half an hour", "half hour".
                if word(index) == Some("an") {
                    index += 1;
                }
                match word(index).and_then(Unit::read) {
                    Some(Unit::Hour) => {
                        index += 1;
                        phrase.add_span(30, Unit::Minute)?;
                    }
                    _ => return Err(Problem::Unknown),
                }
            }
            "am" | "pm" => {
                // After a time ("9 am") it says which half of the day; alone it can't be read.
                let meridiem = if current == "am" {
                    Meridiem::Am
                } else {
                    Meridiem::Pm
                };
                match &mut phrase.clock {
                    Some(clock) if clock.meridiem.is_none() && clock.hour <= 12 => {
                        clock.meridiem = Some(meridiem);
                    }
                    _ => return Err(Problem::Unknown),
                }
            }
            _ => {
                if let Some(part) = Part::read(current) {
                    phrase.set_part(part)?;
                } else if let Some(spans) = compact_span(current) {
                    for (count, unit) in spans {
                        phrase.add_span(count, unit)?;
                    }
                } else if let Some(month) = read_month(current) {
                    // "nov 5", "nov 5th 2027".
                    let day = word(index)
                        .and_then(read_day_number)
                        .ok_or(Problem::Unknown)?;
                    index += 1;
                    let year = word(index).and_then(read_year);
                    if year.is_some() {
                        index += 1;
                    }
                    phrase.set_day(Day::Date { month, day, year })?;
                } else if let Some(weekday) = Weekday::read(current) {
                    if phrase.weekday.is_some() {
                        return Err(Problem::Unknown);
                    }
                    phrase.weekday = Some(weekday);
                } else if let Some(unit) = word(index).and_then(Unit::read)
                    && current.bytes().all(|byte| byte.is_ascii_digit())
                    && current.len() <= 6
                {
                    // "3 days", "90 min".
                    index += 1;
                    phrase.add_span(current.parse().map_err(|_| Problem::Unknown)?, unit)?;
                } else if let Some(month) = word(index).and_then(read_month)
                    && let Some(day) = read_day_number(current)
                {
                    // "5 nov", "5th november 2027".
                    index += 1;
                    let year = word(index).and_then(read_year);
                    if year.is_some() {
                        index += 1;
                    }
                    phrase.set_day(Day::Date { month, day, year })?;
                } else if let Some((hour, minute)) = read_clock(current) {
                    phrase.set_clock(Clock {
                        hour,
                        minute: minute.unwrap_or(0),
                        meridiem: None,
                    })?;
                } else {
                    return Err(Problem::Unknown);
                }
            }
        }
    }
    // A weekday alone is the day; beside another day it must agree with it (checked later).
    if phrase.day.is_none()
        && let Some(weekday) = phrase.weekday.take()
    {
        phrase.day = Some(Day::Weekday(weekday));
    }
    Ok(phrase)
}

/// The hour and minute the phrase gives, if any: the clock (with am/pm, and an afternoon,
/// evening or night word making an hour before 12 the afternoon's, or a night's small hours
/// the next day's), else its part of the day. Hours from 24 are the next day's.
fn time_of_day(phrase: &Phrase) -> Read<Option<(u8, u8)>> {
    if let Some(clock) = phrase.clock {
        let late = matches!(
            phrase.part,
            Some(Part::Afternoon | Part::Evening | Part::Night)
        );
        let night = matches!(phrase.part, Some(Part::Evening | Part::Night));
        // "tonight 1" to "tonight 4", "friday night 2am": past the night's midnight.
        let small = phrase.part == Some(Part::Night) && (1..=4).contains(&clock.hour);
        let hour = match clock.meridiem {
            Some(Meridiem::Am) if clock.hour == 12 => 0,
            Some(Meridiem::Am) if small => clock.hour + 24,
            Some(Meridiem::Am) => clock.hour,
            Some(Meridiem::Pm) if clock.hour == 12 => 12,
            Some(Meridiem::Pm) => clock.hour + 12,
            // "tonight 12", "tonight 0:15": the night's end, the next day's start (24).
            None if night && matches!(clock.hour, 0 | 12) => clock.hour.max(12) + 12,
            None if small => clock.hour + 24,
            None if clock.hour < 12 && late => clock.hour + 12,
            None => clock.hour,
        };
        // "0am", "tonight 7am", "morning 14", "morning 5pm", "tonight 12pm", "morning
        // midnight", "evening 1" (13:00 isn't the evening, and it doesn't run past midnight):
        // words that disagree.
        let contradicts = match clock.meridiem {
            Some(_) if clock.hour == 0 => true,
            Some(Meridiem::Am) => late && !small,
            Some(Meridiem::Pm) => phrase.part == Some(Part::Morning) || (night && clock.hour == 12),
            None => {
                (phrase.part == Some(Part::Morning) && clock.hour >= 12)
                    || (phrase.part == Some(Part::Evening) && (1..=4).contains(&clock.hour))
            }
        };
        if contradicts {
            return Err(Problem::Unknown);
        }
        return Ok(Some((hour, clock.minute)));
    }
    Ok(phrase.part.map(|part| (part.hour(), 0)))
}

fn resolve(phrase: &Phrase, now: Civil) -> Read<Target> {
    let time = time_of_day(phrase)?;
    if phrase.later {
        // "later", "later today": nothing else.
        let only_today = matches!(phrase.day, None | Some(Day::Today));
        if !only_today || time.is_some() || phrase.span || phrase.tonight {
            return Err(Problem::Unknown);
        }
        return later_today(now).map(Target::At).ok_or(Problem::Passed);
    }
    if phrase.span {
        // "0h", "in 0 days": no time at all.
        if phrase.day.is_some() || phrase.minutes + phrase.days + phrase.months == 0 {
            return Err(Problem::Unknown);
        }
        if phrase.minutes > 0 {
            // Hours and minutes are exact; days beside them count as 24 hours.
            if time.is_some() || phrase.months > 0 {
                return Err(Problem::Unknown);
            }
            let minutes = phrase.minutes + phrase.days * 1_440;
            return (minutes > 0 && minutes <= MAX_MINUTES)
                .then_some(Target::In(minutes))
                .ok_or(Problem::Unknown);
        }
        let at = on(
            now.plus_months(phrase.months).plus_days(phrase.days),
            time.unwrap_or((DAY_START, 0)),
        );
        return if at > now {
            Ok(Target::At(at))
        } else {
            Err(Problem::Passed)
        };
    }
    let Some(day) = phrase.day else {
        // A time alone: today, or tomorrow once it has passed.
        let today = tonight_or_on(now, time.ok_or(Problem::Unknown)?);
        return Ok(Target::At(if today > now {
            today
        } else {
            today.plus_days(1)
        }));
    };
    let time = time.unwrap_or((DAY_START, 0));
    let at = match day {
        Day::Today => {
            let today = tonight_or_on(now, time);
            if today > now {
                today
            } else if phrase.tonight && phrase.clock.is_none() {
                // "tonight" once 20:00 is near: the next whole hour at least 30 minutes away.
                let soon = now.minutes() + 30;
                let hour_start = soon + (60 - soon.rem_euclid(60)) % 60;
                let at = Civil::from_minutes(hour_start);
                if at.days() != now.days() {
                    return Err(Problem::Passed);
                }
                at
            } else {
                return Err(Problem::Passed);
            }
        }
        Day::Tomorrow => on(now.plus_days(1), time),
        Day::Weekday(weekday) => on(next_weekday(now, weekday), time),
        // Next week's Monday, or its day a weekday names ("next fri", "fri next week").
        Day::NextWeek => on(
            next_week(now).plus_days(phrase.weekday.map_or(0, Weekday::index)),
            time,
        ),
        Day::Weekend { next } => weekend(now, next, time),
        Day::Date { month, day, year } => {
            let date = |year: i32| Civil::new(year, month, day, 0, 0).map(|date| on(date, time));
            match year {
                Some(year) => match date(year) {
                    Some(at) if at > now => at,
                    Some(_) => return Err(Problem::Passed),
                    None => return Err(Problem::Unknown),
                },
                None => match date(now.year).filter(|at| *at > now) {
                    Some(at) => at,
                    None => date(now.year + 1).ok_or(Problem::Unknown)?,
                },
            }
        }
    };
    // The day the words name: the one before a night's midnight.
    let named = if time.0 >= 24 { at.plus_days(-1) } else { at };
    if let Some(weekday) = phrase.weekday
        && named.weekday() != weekday
    {
        return Err(Problem::Unknown);
    }
    Ok(Target::At(at))
}

/// Today at `time`; a night's hours (from 24) are the night still on when they are ahead (in
/// its small hours: "tonight 1" at 00:30 is 01:00 today), else tonight's, the next day's.
fn tonight_or_on(now: Civil, time: (u8, u8)) -> Civil {
    let still_on = on(now.plus_days(-1), time);
    if time.0 >= 24 && still_on > now {
        still_on
    } else {
        on(now, time)
    }
}

/// `day` at `time`, where hour 24 is the night's end: the next day's 00:00.
fn on(day: Civil, (hour, minute): (u8, u8)) -> Civil {
    if hour >= 24 {
        day.plus_days(1).at(hour - 24, minute)
    } else {
        day.at(hour, minute)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Thursday 29 October 2026, 14:20.
    fn now() -> Civil {
        civil(2026, 10, 29, 14, 20)
    }

    fn civil(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> Civil {
        Civil::new(year, month, day, hour, minute).unwrap()
    }

    fn at(text: &str, from: Civil) -> Civil {
        match parse(text, from) {
            Ok(Target::At(at)) => at,
            other => panic!("{text:?}: {other:?}"),
        }
    }

    fn after(text: &str) -> i64 {
        match parse(text, now()) {
            Ok(Target::In(minutes)) => minutes,
            other => panic!("{text:?}: {other:?}"),
        }
    }

    #[test]
    fn someday_is_no_time_at_all() {
        for text in ["someday", "Someday", " some day ", "never", "NEVER."] {
            assert!(is_someday(text), "{text}");
            assert!(parse(text, now()).is_err(), "{text}");
        }
        for text in ["", "some", "someday 9am", "tomorrow", "never mind"] {
            assert!(!is_someday(text), "{text}");
        }
    }

    #[test]
    fn calendar_arithmetic_round_trips() {
        assert_eq!(civil(1970, 1, 1, 0, 0).days(), 0);
        assert_eq!(civil(1970, 1, 1, 0, 0).weekday(), Weekday::Thursday);
        assert_eq!(now().weekday(), Weekday::Thursday);
        assert_eq!(civil(2026, 11, 2, 0, 0).weekday(), Weekday::Monday);
        assert_eq!(civil(2000, 2, 29, 0, 0).weekday(), Weekday::Tuesday);
        for days in [-800_000, -1, 0, 1, 59, 60, 365, 10_957, 20_755, 2_932_896] {
            let date = Civil::from_days(days, 7, 30);
            assert_eq!(date.days(), days, "{date}");
            assert_eq!(Civil::from_minutes(date.minutes()), date);
        }
        assert!(Civil::new(2026, 2, 29, 0, 0).is_none());
        assert!(Civil::new(2028, 2, 29, 0, 0).is_some());
        assert!(Civil::new(2026, 4, 31, 0, 0).is_none());
        assert!(Civil::new(2026, 4, 30, 24, 0).is_none());
        assert_eq!(
            civil(2026, 1, 31, 8, 0).plus_months(1),
            civil(2026, 2, 28, 8, 0)
        );
        assert_eq!(
            civil(2027, 12, 15, 8, 0).plus_months(2),
            civil(2028, 2, 15, 8, 0)
        );
        assert_eq!(now().plus_days(3), civil(2026, 11, 1, 14, 20));
        assert!(civil(2026, 10, 29, 14, 21) > now());
        assert!(civil(2026, 10, 30, 0, 0) > civil(2026, 10, 29, 23, 59));
        assert_eq!(now().to_string(), "2026-10-29 14:20");
    }

    #[test]
    fn days_without_a_time_start_at_eight() {
        assert_eq!(at("tomorrow", now()), civil(2026, 10, 30, 8, 0));
        assert_eq!(at("Tomorrow", now()), civil(2026, 10, 30, 8, 0));
        assert_eq!(at("tmrw", now()), civil(2026, 10, 30, 8, 0));
        assert_eq!(at("mon", now()), civil(2026, 11, 2, 8, 0));
        assert_eq!(at("monday", now()), civil(2026, 11, 2, 8, 0));
        assert_eq!(at("next monday", now()), civil(2026, 11, 2, 8, 0));
        assert_eq!(at("this friday", now()), civil(2026, 10, 30, 8, 0));
        assert_eq!(at("tues", now()), civil(2026, 11, 3, 8, 0));
        // A weekday is the next one after today: Thursday's is next week's.
        assert_eq!(at("thu", now()), civil(2026, 11, 5, 8, 0));
        assert_eq!(at("next week", now()), civil(2026, 11, 2, 8, 0));
        // "next" with a weekday is that day of next week (Monday first), on a Thursday too.
        assert_eq!(at("next fri", now()), civil(2026, 11, 6, 8, 0));
        assert_eq!(at("fri next week", now()), civil(2026, 11, 6, 8, 0));
        assert_eq!(at("friday next week 3pm", now()), civil(2026, 11, 6, 15, 0));
        assert_eq!(at("next week fri", now()), civil(2026, 11, 6, 8, 0));
        assert_eq!(at("next thu", now()), civil(2026, 11, 5, 8, 0));
        assert_eq!(at("next sat", now()), civil(2026, 11, 7, 8, 0));
        assert_eq!(at("next sun", now()), civil(2026, 11, 8, 8, 0));
        let sunday = civil(2026, 11, 1, 9, 0);
        assert_eq!(at("next mon", sunday), civil(2026, 11, 2, 8, 0));
        assert_eq!(at("next fri", sunday), civil(2026, 11, 6, 8, 0));
        assert_eq!(at("next sun", sunday), civil(2026, 11, 8, 8, 0));
        assert_eq!(parse("next fri thu", now()), Err(Problem::Unknown));
        assert_eq!(parse("this fri next week", now()), Err(Problem::Unknown));
        assert_eq!(at("nov 5", now()), civil(2026, 11, 5, 8, 0));
        assert_eq!(at("5 nov", now()), civil(2026, 11, 5, 8, 0));
        assert_eq!(at("November 5th", now()), civil(2026, 11, 5, 8, 0));
        assert_eq!(at("5th of november", now()), civil(2026, 11, 5, 8, 0));
    }

    #[test]
    fn times_are_24_hour_unless_am_or_pm_says_otherwise() {
        assert_eq!(at("tomorrow 9am", now()), civil(2026, 10, 30, 9, 0));
        assert_eq!(at("tomorrow at 9 am", now()), civil(2026, 10, 30, 9, 0));
        assert_eq!(at("tomorrow 9", now()), civil(2026, 10, 30, 9, 0));
        assert_eq!(at("tomorrow 9pm", now()), civil(2026, 10, 30, 21, 0));
        assert_eq!(at("tomorrow 9 p.m.", now()), civil(2026, 10, 30, 21, 0));
        assert_eq!(at("monday 8:30", now()), civil(2026, 11, 2, 8, 30));
        assert_eq!(at("mon 8.30", now()), civil(2026, 11, 2, 8, 30));
        assert_eq!(at("mon 5pm", now()), civil(2026, 11, 2, 17, 0));
        assert_eq!(at("21:30", now()), civil(2026, 10, 29, 21, 30));
        assert_eq!(at("17", now()), civil(2026, 10, 29, 17, 0));
        assert_eq!(at("9:30pm", now()), civil(2026, 10, 29, 21, 30));
        assert_eq!(at("at 16:45", now()), civil(2026, 10, 29, 16, 45));
        assert_eq!(at("nov 5 3pm", now()), civil(2026, 11, 5, 15, 0));
        assert_eq!(at("5 nov 15:00", now()), civil(2026, 11, 5, 15, 0));
        assert_eq!(at("tomorrow 12pm", now()), civil(2026, 10, 30, 12, 0));
        assert_eq!(at("tomorrow 12am", now()), civil(2026, 10, 30, 0, 0));
        // A dotted number is a time, never a numeric date (the day's and month's order varies).
        assert_eq!(at("5.11", now()), civil(2026, 10, 30, 5, 11));
        // Not times.
        for text in [
            "25:00",
            "9:75",
            "9:5",
            "13pm",
            "0am",
            "tomorrow 24",
            "morning 14",
        ] {
            assert_eq!(parse(text, now()), Err(Problem::Unknown), "{text}");
        }
    }

    #[test]
    fn past_times_roll_forward() {
        // A time alone that has passed today is tomorrow's.
        assert_eq!(at("9am", now()), civil(2026, 10, 30, 9, 0));
        assert_eq!(at("8", now()), civil(2026, 10, 30, 8, 0));
        assert_eq!(at("14:20", now()), civil(2026, 10, 30, 14, 20));
        assert_eq!(at("14:21", now()), civil(2026, 10, 29, 14, 21));
        assert_eq!(at("noon", now()), civil(2026, 10, 30, 12, 0));
        assert_eq!(at("midnight", now()), civil(2026, 10, 30, 0, 0));
        // A date this year that has passed is next year's.
        assert_eq!(at("oct 1", now()), civil(2027, 10, 1, 8, 0));
        assert_eq!(at("oct 29", now()), civil(2027, 10, 29, 8, 0));
        assert_eq!(at("oct 29 18:00", now()), civil(2026, 10, 29, 18, 0));
        assert_eq!(at("jan 2 2027", now()), civil(2027, 1, 2, 8, 0));
        assert_eq!(parse("jan 2 2026", now()), Err(Problem::Passed));
        // "today" and "tonight" say today: a time that passed is refused, not moved.
        assert_eq!(parse("today 9am", now()), Err(Problem::Passed));
        assert_eq!(parse("tonight 7am", now()), Err(Problem::Unknown));
        assert_eq!(
            parse("tonight 9pm", civil(2026, 10, 29, 22, 0)),
            Err(Problem::Passed)
        );
        assert_eq!(at("today 18:00", now()), civil(2026, 10, 29, 18, 0));
    }

    #[test]
    fn a_nights_midnight_is_its_end() {
        assert_eq!(at("tonight 12", now()), civil(2026, 10, 30, 0, 0));
        assert_eq!(at("tonight at midnight", now()), civil(2026, 10, 30, 0, 0));
        assert_eq!(at("tonight 12:30", now()), civil(2026, 10, 30, 0, 30));
        assert_eq!(at("tonight 0:15", now()), civil(2026, 10, 30, 0, 15));
        assert_eq!(at("friday night 12", now()), civil(2026, 10, 31, 0, 0));
        assert_eq!(at("tomorrow evening 12", now()), civil(2026, 10, 31, 0, 0));
        assert_eq!(at("thu nov 5 night 12", now()), civil(2026, 11, 6, 0, 0));
        assert_eq!(at("night 12", now()), civil(2026, 10, 30, 0, 0));
        // Late in the evening it is still ahead.
        assert_eq!(
            at("tonight 12", civil(2026, 10, 29, 23, 30)),
            civil(2026, 10, 30, 0, 0)
        );
        // Noon isn't tonight; the afternoon's 12 is noon.
        for text in ["tonight 12pm", "tonight noon", "evening 12pm"] {
            assert_eq!(parse(text, now()), Err(Problem::Unknown), "{text}");
        }
        assert_eq!(
            at("tomorrow afternoon 12", now()),
            civil(2026, 10, 30, 12, 0)
        );
    }

    #[test]
    fn a_nights_small_hours_are_the_next_days() {
        // Thursday 29 October, 14:20: tonight's 1 to 4 are Friday's.
        assert_eq!(at("tonight 1", now()), civil(2026, 10, 30, 1, 0));
        assert_eq!(at("tonight 2", now()), civil(2026, 10, 30, 2, 0));
        assert_eq!(at("tonight at 4:30", now()), civil(2026, 10, 30, 4, 30));
        assert_eq!(at("tonight 2am", now()), civil(2026, 10, 30, 2, 0));
        assert_eq!(at("friday night 1", now()), civil(2026, 10, 31, 1, 0));
        assert_eq!(at("thu nov 5 night 3am", now()), civil(2026, 11, 6, 3, 0));
        // 5 to 11 are the evening's.
        assert_eq!(at("tonight 5", now()), civil(2026, 10, 29, 17, 0));
        assert_eq!(at("tonight 11", now()), civil(2026, 10, 29, 23, 0));
        // An evening doesn't run past midnight, and 5am isn't the night's; 13:00 isn't the
        // evening.
        for text in [
            "tonight 5am",
            "evening 2am",
            "afternoon 1am",
            "evening 1",
            "this evening 4",
        ] {
            assert_eq!(parse(text, now()), Err(Problem::Unknown), "{text}");
        }
        assert_eq!(at("evening 5", now()), civil(2026, 10, 29, 17, 0));
        assert_eq!(at("afternoon 1", now()), civil(2026, 10, 30, 13, 0));
    }

    #[test]
    fn in_the_small_hours_the_night_still_on_is_meant() {
        // Friday 30 October, 00:30: Thursday's night is still on.
        let small = civil(2026, 10, 30, 0, 30);
        assert_eq!(at("tonight 1", small), civil(2026, 10, 30, 1, 0));
        assert_eq!(at("tonight 2am", small), civil(2026, 10, 30, 2, 0));
        assert_eq!(at("tonight 0:45", small), civil(2026, 10, 30, 0, 45));
        assert_eq!(at("night 3", small), civil(2026, 10, 30, 3, 0));
        // Its midnight has passed: the coming one.
        assert_eq!(at("tonight 12", small), civil(2026, 10, 31, 0, 0));
        assert_eq!(at("midnight", small), civil(2026, 10, 31, 0, 0));
        // The evening is the coming one.
        assert_eq!(at("tonight", small), civil(2026, 10, 30, 20, 0));
        assert_eq!(at("tonight 9", small), civil(2026, 10, 30, 21, 0));
        // Once its time has passed, the coming night's.
        let late = civil(2026, 10, 30, 3, 0);
        assert_eq!(at("tonight 1", late), civil(2026, 10, 31, 1, 0));
        assert_eq!(at("tonight 4", late), civil(2026, 10, 30, 4, 0));
    }

    #[test]
    fn midnight_is_a_days_end() {
        assert_eq!(at("midnight", now()), civil(2026, 10, 30, 0, 0));
        assert_eq!(at("today midnight", now()), civil(2026, 10, 30, 0, 0));
        assert_eq!(at("tomorrow midnight", now()), civil(2026, 10, 31, 0, 0));
        assert_eq!(at("fri midnight", now()), civil(2026, 10, 31, 0, 0));
        assert_eq!(at("nov 5 at midnight", now()), civil(2026, 11, 6, 0, 0));
        // Just before it, it is still ahead.
        assert_eq!(
            at("midnight", civil(2026, 10, 29, 23, 59)),
            civil(2026, 10, 30, 0, 0)
        );
        for text in [
            "midnight am",
            "midnight pm",
            "morning midnight",
            "midnight 5",
        ] {
            assert_eq!(parse(text, now()), Err(Problem::Unknown), "{text}");
        }
    }

    #[test]
    fn parts_of_the_day() {
        assert_eq!(at("tonight", now()), civil(2026, 10, 29, 20, 0));
        assert_eq!(at("tonight 9", now()), civil(2026, 10, 29, 21, 0));
        assert_eq!(at("tonight at 11", now()), civil(2026, 10, 29, 23, 0));
        assert_eq!(at("this evening", now()), civil(2026, 10, 29, 18, 0));
        assert_eq!(at("evening", now()), civil(2026, 10, 29, 18, 0));
        assert_eq!(at("tomorrow morning", now()), civil(2026, 10, 30, 8, 0));
        assert_eq!(at("tomorrow afternoon", now()), civil(2026, 10, 30, 13, 0));
        assert_eq!(at("tomorrow evening 7", now()), civil(2026, 10, 30, 19, 0));
        assert_eq!(at("monday night", now()), civil(2026, 11, 2, 20, 0));
        // A morning that has passed is tomorrow's.
        assert_eq!(at("morning", now()), civil(2026, 10, 30, 8, 0));
        // Once 20:00 is near, "tonight" is the next whole hour at least 30 minutes away.
        assert_eq!(
            at("tonight", civil(2026, 10, 29, 20, 10)),
            civil(2026, 10, 29, 21, 0)
        );
        assert_eq!(
            at("tonight", civil(2026, 10, 29, 22, 29)),
            civil(2026, 10, 29, 23, 0)
        );
        assert_eq!(
            parse("tonight", civil(2026, 10, 29, 23, 10)),
            Err(Problem::Passed)
        );
    }

    #[test]
    fn spans() {
        assert_eq!(after("2h"), 120);
        assert_eq!(after("30m"), 30);
        assert_eq!(after("1h30m"), 90);
        assert_eq!(after("1h 30m"), 90);
        assert_eq!(after("90 min"), 90);
        assert_eq!(after("in 2 hours"), 120);
        assert_eq!(after("in an hour"), 60);
        assert_eq!(after("half an hour"), 30);
        assert_eq!(after("in 45 minutes"), 45);
        assert_eq!(after("1d 2h"), 26 * 60);
        // Days, weeks and months move the date, at 08:00 or the time given.
        assert_eq!(at("in 3 days", now()), civil(2026, 11, 1, 8, 0));
        assert_eq!(at("3 days", now()), civil(2026, 11, 1, 8, 0));
        assert_eq!(at("3d", now()), civil(2026, 11, 1, 8, 0));
        assert_eq!(at("3D", now()), civil(2026, 11, 1, 8, 0));
        assert_eq!(at("in 3 days 5pm", now()), civil(2026, 11, 1, 17, 0));
        assert_eq!(at("in 2 weeks", now()), civil(2026, 11, 12, 8, 0));
        assert_eq!(at("2w", now()), civil(2026, 11, 12, 8, 0));
        assert_eq!(at("1mo", now()), civil(2026, 11, 29, 8, 0));
        assert_eq!(at("in a month", now()), civil(2026, 11, 29, 8, 0));
        assert_eq!(at("next month", now()), civil(2026, 11, 29, 8, 0));
        assert_eq!(
            Target::In(120).civil(now()),
            civil(2026, 10, 29, 16, 20),
            "on the wall clock"
        );
        for text in [
            "in 2h tomorrow",
            "2h 9am",
            "0h",
            "9999999h",
            "in",
            "in a",
            "3 bananas",
        ] {
            assert!(parse(text, now()).is_err(), "{text}");
        }
        // A span of nothing isn't a time, before 08:00 too (it was today 08:00).
        let early = civil(2026, 10, 29, 7, 0);
        for text in ["0h", "0m", "0d", "in 0 days", "0h0m"] {
            assert_eq!(parse(text, early), Err(Problem::Unknown), "{text}");
        }
        assert_eq!(after("1h0m"), 60);
    }

    #[test]
    fn weekends_and_weeks() {
        assert_eq!(at("this weekend", now()), civil(2026, 10, 31, 8, 0));
        assert_eq!(at("weekend", now()), civil(2026, 10, 31, 8, 0));
        assert_eq!(at("next weekend", now()), civil(2026, 11, 7, 8, 0));
        assert_eq!(at("this weekend 10am", now()), civil(2026, 10, 31, 10, 0));
        let saturday_early = civil(2026, 10, 31, 7, 0);
        let saturday = civil(2026, 10, 31, 9, 0);
        let sunday_early = civil(2026, 11, 1, 7, 30);
        let sunday = civil(2026, 11, 1, 9, 0);
        assert_eq!(
            at("this weekend", saturday_early),
            civil(2026, 10, 31, 8, 0)
        );
        assert_eq!(at("this weekend", saturday), civil(2026, 11, 1, 8, 0));
        assert_eq!(at("this weekend", sunday_early), civil(2026, 11, 1, 8, 0));
        assert_eq!(at("this weekend", sunday), civil(2026, 11, 7, 8, 0));
        assert_eq!(at("next weekend", saturday), civil(2026, 11, 7, 8, 0));
        assert_eq!(at("next weekend", sunday), civil(2026, 11, 14, 8, 0));
        // Next week is the next Monday: tomorrow on a Sunday, a week on a Monday.
        assert_eq!(at("next week", sunday), civil(2026, 11, 2, 8, 0));
        assert_eq!(
            at("next week", civil(2026, 11, 2, 7, 0)),
            civil(2026, 11, 9, 8, 0)
        );
        // Every weekday's weekend is the coming Saturday (or this weekend's day still ahead).
        for offset in 0..7 {
            let from = civil(2026, 11, 2, 12, 0).plus_days(offset);
            let weekend = at("this weekend", from);
            assert!(weekend > from, "{from}");
            assert!(
                matches!(weekend.weekday(), Weekday::Saturday | Weekday::Sunday),
                "{from}"
            );
            assert!(weekend.days() - from.days() <= 6, "{from}");
        }
    }

    #[test]
    fn weekdays_beside_dates_must_agree() {
        assert_eq!(at("thu nov 5", now()), civil(2026, 11, 5, 8, 0));
        assert_eq!(at("Thursday, 5 November", now()), civil(2026, 11, 5, 8, 0));
        assert_eq!(parse("fri nov 5", now()), Err(Problem::Unknown));
        assert_eq!(parse("mon tue", now()), Err(Problem::Unknown));
    }

    #[test]
    fn what_is_not_a_time() {
        assert_eq!(parse("", now()), Err(Problem::Empty));
        assert_eq!(parse("   ", now()), Err(Problem::Empty));
        for text in [
            "banana",
            "tomorrow banana",
            "tomorrow today",
            "feb 30",
            "feb 29",
            "nov",
            "nov 32",
            "next",
            "this week",
            "am",
            "9 am pm",
            "the",
            "mo",
            "later tomorrow",
            "later 5pm",
        ] {
            assert!(parse(text, now()).is_err(), "{text}");
        }
        // Feb 29 exists in a leap year ahead.
        assert_eq!(
            at("feb 29", civil(2027, 3, 1, 9, 0)),
            civil(2028, 2, 29, 8, 0)
        );
    }

    #[test]
    fn later_today_is_three_hours_rounded_up_while_it_is_today() {
        assert_eq!(
            Preset::LaterToday.resolve(now()),
            Some(civil(2026, 10, 29, 17, 30))
        );
        assert_eq!(at("later", now()), civil(2026, 10, 29, 17, 30));
        assert_eq!(at("later today", now()), civil(2026, 10, 29, 17, 30));
        assert_eq!(
            Preset::LaterToday.resolve(civil(2026, 10, 29, 9, 0)),
            Some(civil(2026, 10, 29, 12, 0))
        );
        assert_eq!(
            Preset::LaterToday.resolve(civil(2026, 10, 29, 9, 1)),
            Some(civil(2026, 10, 29, 12, 30))
        );
        assert_eq!(
            Preset::LaterToday.resolve(civil(2026, 10, 29, 20, 20)),
            Some(civil(2026, 10, 29, 23, 30))
        );
        assert_eq!(
            Preset::LaterToday.resolve(civil(2026, 10, 29, 20, 40)),
            None
        );
        assert_eq!(
            parse("later", civil(2026, 10, 29, 22, 0)),
            Err(Problem::Passed)
        );
    }

    #[test]
    fn presets_resolve_as_superhumans() {
        let titles: Vec<&str> = Preset::ALL.iter().map(|preset| preset.title()).collect();
        assert_eq!(
            titles,
            [
                "Later today",
                "Tomorrow morning",
                "This weekend",
                "Next week"
            ]
        );
        assert_eq!(
            Preset::TomorrowMorning.resolve(now()),
            Some(civil(2026, 10, 30, 8, 0))
        );
        assert_eq!(
            Preset::ThisWeekend.resolve(now()),
            Some(civil(2026, 10, 31, 8, 0))
        );
        assert_eq!(
            Preset::NextWeek.resolve(now()),
            Some(civil(2026, 11, 2, 8, 0))
        );
        // Typed, the presets' words mean the same.
        for preset in [
            Preset::TomorrowMorning,
            Preset::ThisWeekend,
            Preset::NextWeek,
        ] {
            assert_eq!(
                Some(at(preset.title(), now())),
                preset.resolve(now()),
                "{preset:?}"
            );
        }
        // Every preset is in the future, all week round.
        for hour in [0, 7, 8, 12, 19, 23] {
            for offset in 0..7 {
                let from = civil(2026, 11, 2, hour, 30).plus_days(offset);
                for preset in Preset::ALL {
                    if let Some(when) = preset.resolve(from) {
                        assert!(when > from, "{preset:?} from {from}");
                    }
                }
            }
        }
    }

    proptest::proptest! {
        /// Whatever now is, a clock time is within the next day, a weekday within the next
        /// week, a preset in the future, and a label reads back the same day.
        #[test]
        fn resolved_times_lie_ahead(
            days in 18_000i64..24_000,
            minute_of_day in 0i64..1_440,
            hour in 0u8..24,
            minute in 0u8..60,
            weekday in 0usize..7,
        ) {
            let now = Civil::from_minutes(days * 1_440 + minute_of_day);
            let clock = at(&format!("{hour}:{minute:02}"), now);
            proptest::prop_assert!(clock > now);
            proptest::prop_assert!(clock.minutes() - now.minutes() <= 1_440);
            proptest::prop_assert_eq!((clock.hour, clock.minute), (hour, minute));
            let name = Weekday::ALL[weekday].short().to_lowercase();
            let day = at(&format!("{name} {hour}:{minute:02}"), now);
            proptest::prop_assert_eq!(day.weekday(), Weekday::ALL[weekday]);
            proptest::prop_assert!((1..=7).contains(&(day.days() - now.days())));
            for preset in Preset::ALL {
                if let Some(when) = preset.resolve(now) {
                    proptest::prop_assert!(when > now);
                    proptest::prop_assert!(when.days() - now.days() <= 7);
                }
            }
            let text = label(clock, now);
            let clock_text = format!("{hour:02}:{minute:02}");
            proptest::prop_assert!(text.ends_with(&clock_text), "{}", text);
        }
    }

    #[test]
    fn labels_say_when() {
        let label_of =
            |year, month, day, hour, minute| label(civil(year, month, day, hour, minute), now());
        assert_eq!(label_of(2026, 10, 29, 18, 0), "today 18:00");
        assert_eq!(label_of(2026, 10, 30, 8, 0), "tomorrow 08:00");
        assert_eq!(label_of(2026, 11, 3, 8, 0), "Tue 08:00");
        assert_eq!(label_of(2026, 11, 4, 9, 5), "Wed 09:05");
        assert_eq!(label_of(2026, 11, 5, 8, 0), "Thu, Nov 5, 08:00");
        assert_eq!(label_of(2027, 1, 2, 8, 0), "Sat, Jan 2 2027, 08:00");
        assert_eq!(title(civil(2026, 10, 30, 8, 0), now()), "Tomorrow 08:00");
        assert_eq!(title(civil(2026, 11, 3, 8, 0), now()), "Tue 08:00");
        assert_eq!(day(civil(2026, 10, 30, 9, 0), now()), "Fri, Oct 30");
        assert_eq!(day(civil(2027, 1, 2, 8, 0), now()), "Sat, Jan 2 2027");
        assert_eq!(month_name(11), "November");
        assert_eq!(month_short(1), "Jan");
    }
}
