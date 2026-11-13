//! Who wrote in a conversation, as its list row names them (the Superhuman pass,
//! docs/superhuman/conversations.md): one person by their whole name, several by first name,
//! the user as "me", each person once, more than three as the first, "…" and the two who wrote
//! last (as Gmail does). In an unread row the people who wrote unread mail are bold. The row's
//! accessible label says the same people in words ("Ingrid, Tomas and you").
//!
//! A column too narrow for every name never cuts one (phase C, the visual QA: "Marius … To…",
//! "Ingrid … me, …"): [`columns`] gives shorter texts made of whole names, the first writer and
//! the one the row is about ("Ingrid … Tomas"), then that one and how many others ("Tomas +3"),
//! and the row shows the longest that fits.

use std::borrow::Cow;

/// One person who wrote in a conversation, in the order they first wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Person<'a> {
    /// The display name, or the address where there is none.
    pub name: &'a str,
    /// The user: one of the account's addresses, or the sender of a copy in its Sent or Drafts.
    pub me: bool,
    /// Wrote a message that is unread in the view.
    pub unread: bool,
    /// When they last wrote (Unix seconds): the latest writers are the ones named.
    pub last: i64,
}

/// How the names of a sender column are weighted: none bold (a read row), all, or the unread
/// people's, as byte ranges of the column's text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Emphasis {
    #[default]
    None,
    All,
    Ranges(Vec<(u32, u32)>),
}

/// The most people a column names; past it, the first, "…" and the two who wrote last.
pub const MOST_NAMED: usize = 3;

/// A display name's first name: "Ingrid" of "Ingrid Solberg", "Tomas" of "Berg, Tomas", the
/// local part of a bare address.
pub fn first_name(name: &str) -> &str {
    let name = name.trim().trim_matches(['"', '\'']).trim();
    if !name.contains(' ')
        && let Some((local, _)) = name.split_once('@')
    {
        return local;
    }
    let given = match name.split_once(',') {
        Some((_, given)) if !given.trim().is_empty() => given,
        _ => name,
    };
    given.split_whitespace().next().unwrap_or(name)
}

/// `people`, each once, in the order they first wrote: the user once (two of their addresses
/// are one "me"), anyone else once by their name, whatever its case (one person from two
/// addresses). A person kept for several is unread when any of them is, and last wrote when
/// the latest of them did.
pub fn distinct<'a>(people: &[Person<'a>]) -> Vec<Person<'a>> {
    let mut kept: Vec<Person<'a>> = Vec::with_capacity(people.len());
    for person in people {
        let same = kept.iter_mut().find(|known| {
            (known.me && person.me)
                || (!known.me && !person.me && same_name(known.name, person.name))
        });
        match same {
            Some(known) => {
                known.unread |= person.unread;
                known.last = known.last.max(person.last);
            }
            None => kept.push(*person),
        }
    }
    kept
}

/// Whether two names are the same whatever their case, without allocating (a row binds this).
fn same_name(a: &str, b: &str) -> bool {
    a.trim()
        .chars()
        .flat_map(char::to_lowercase)
        .eq(b.trim().chars().flat_map(char::to_lowercase))
}

/// The ones a column names: every person while there are at most [`MOST_NAMED`], else the
/// first and the two who wrote last, the latest last (`None` stands for the people left out).
fn named<'p, 'a>(people: &'p [Person<'a>]) -> Vec<Option<&'p Person<'a>>> {
    if people.len() > MOST_NAMED {
        let mut latest: Vec<usize> = (1..people.len()).collect();
        // Who wrote last; of two at the same second, the one who joined later.
        latest.sort_by_key(|at| (people[*at].last, *at));
        let [.., second, last] = latest[..] else {
            unreachable!("more than three people")
        };
        vec![
            Some(&people[0]),
            None,
            Some(&people[second]),
            Some(&people[last]),
        ]
    } else {
        people.iter().map(Some).collect()
    }
}

/// Whom a short column names besides the first writer: in an unread row the latest to write
/// unread mail, else the latest to write who isn't the user, else the latest (phase C: the
/// newest writer is never the one left out).
fn about<'p, 'a>(people: &'p [Person<'a>], unread: bool) -> &'p Person<'a> {
    people
        .iter()
        .enumerate()
        .max_by_key(|(at, person)| (unread && person.unread, !person.me, person.last, *at))
        .map(|(_, person)| person)
        .expect("a column names someone")
}

/// How a person is named: "me", their whole name when alone, else their first name.
fn shown<'a>(person: &Person<'a>, alone: bool) -> &'a str {
    if person.me {
        "me"
    } else if alone {
        person.name.trim()
    } else {
        first_name(person.name)
    }
}

/// The sender column's text and its emphasis: "Ingrid Solberg", "Ingrid, Tomas, me",
/// "Ingrid … Sana, Tomas". An unread row's unread people are bold, all of them when the row
/// doesn't say who (nobody marked, or everybody).
pub fn column<'a>(people: &[Person<'a>], unread: bool) -> (Cow<'a, str>, Emphasis) {
    let people = distinct(people);
    match people.as_slice() {
        [] => (Cow::Borrowed(""), Emphasis::None),
        [one] => (Cow::Borrowed(shown(one, true)), weight(unread)),
        _ => {
            let (text, emphasis) = names(&named(&people), unread);
            (Cow::Owned(text), emphasis)
        }
    }
}

/// The sender column's texts, longest first, each made of whole names: [`column`]'s; then, when
/// that named more than two, the first writer and the one the row is about of the others
/// ("Ingrid … Tomas"); then the one it is about of everyone and how many others wrote ("Tomas
/// +3"). One person has their whole name only (a long one is ellipsized). `writers` counts
/// everyone who wrote, when `people` holds only some of them. A row shows the longest that fits
/// its column (phase C).
pub fn columns<'a>(
    people: &[Person<'a>],
    writers: usize,
    unread: bool,
) -> Vec<(Cow<'a, str>, Emphasis)> {
    let mut texts = vec![column(people, unread)];
    let people = distinct(people);
    if people.len() < 2 {
        return texts;
    }
    if named(&people).iter().flatten().count() > 2 {
        let other = about(&people[1..], unread);
        let (text, emphasis) = names(&[Some(&people[0]), None, Some(other)], unread);
        texts.push((Cow::Owned(text), emphasis));
    }
    let about = about(&people, unread);
    let name = shown(about, false);
    let others = writers.max(people.len()) - 1;
    // Only the name bold: never the count of the others.
    let emphasis = if unread && (about.unread || !people.iter().any(|person| person.unread)) {
        Emphasis::Ranges(vec![(0, name.len() as u32)])
    } else {
        Emphasis::None
    };
    texts.push((Cow::Owned(format!("{name} +{others}")), emphasis));
    texts
}

/// Bold for an unread row's one name, else regular.
fn weight(unread: bool) -> Emphasis {
    if unread {
        Emphasis::All
    } else {
        Emphasis::None
    }
}

/// The names `named` gives, by first name, with "…" for those left out, and their emphasis.
fn names(named: &[Option<&Person>], unread: bool) -> (String, Emphasis) {
    let mut text = String::with_capacity(48);
    let mut ranges = Vec::new();
    for (index, person) in named.iter().enumerate() {
        match person {
            None => text.push_str(" … "),
            Some(person) => {
                if index > 0 && named[index - 1].is_some() {
                    text.push_str(", ");
                }
                let start = text.len() as u32;
                text.push_str(shown(person, false));
                if person.unread {
                    ranges.push((start, text.len() as u32));
                }
            }
        }
    }
    let marked = named
        .iter()
        .flatten()
        .filter(|person| person.unread)
        .count();
    let emphasis = if !unread {
        Emphasis::None
    } else if marked == 0 || marked == named.iter().flatten().count() {
        Emphasis::All
    } else {
        Emphasis::Ranges(ranges)
    };
    (text, emphasis)
}

/// The same people in words, for a screen reader: "Ingrid Solberg", "Ingrid, Tomas and you",
/// "Ingrid, Tomas, you and 2 others". `total` counts everyone who wrote, when the column was
/// given only some of them (a conversation's row carries at most four).
pub fn spoken(people: &[Person], total: usize) -> String {
    let people = distinct(people);
    let alone = people.len() == 1;
    let names: Vec<&str> = named(&people)
        .into_iter()
        .flatten()
        .map(|person| {
            if person.me {
                "you"
            } else {
                shown(person, alone)
            }
        })
        .collect();
    let others = total.max(people.len()).saturating_sub(names.len());
    match (names.as_slice(), others) {
        ([], _) => String::new(),
        ([one], 0) => (*one).to_owned(),
        ([rest @ .., last], 0) => format!("{} and {last}", rest.join(", ")),
        (names, 1) => format!("{} and 1 other", names.join(", ")),
        (names, others) => format!("{} and {others} others", names.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, unread: bool) -> Person<'_> {
        Person {
            name,
            me: false,
            unread,
            last: 0,
        }
    }

    /// `name`, who last wrote at `last`.
    fn wrote(name: &str, unread: bool, last: i64) -> Person<'_> {
        Person {
            last,
            ..person(name, unread)
        }
    }

    const ME: Person<'static> = Person {
        name: "Alex Berg",
        me: true,
        unread: false,
        last: 0,
    };

    #[test]
    fn first_names_come_from_names_and_addresses() {
        assert_eq!(first_name("Ingrid Solberg"), "Ingrid");
        assert_eq!(first_name("Berg, Tomas"), "Tomas");
        assert_eq!(first_name("\"Northwind Weekly\""), "Northwind");
        assert_eq!(first_name("kari@example.test"), "kari");
        assert_eq!(first_name("Cher"), "Cher");
        assert_eq!(first_name("  "), "");
    }

    #[test]
    fn one_person_is_named_whole_and_several_by_first_name() {
        let one = [person("Ingrid Solberg", true)];
        assert_eq!(
            column(&one, true),
            (Cow::Borrowed("Ingrid Solberg"), Emphasis::All)
        );
        assert_eq!(column(&one, false).1, Emphasis::None);
        assert_eq!(column(&[ME], false).0, "me");
        assert_eq!(column(&[], true), (Cow::Borrowed(""), Emphasis::None));
        let three = [
            person("Ingrid Solberg", false),
            person("Tomas Berg", true),
            ME,
        ];
        let (text, emphasis) = column(&three, true);
        assert_eq!(text, "Ingrid, Tomas, me");
        assert_eq!(emphasis, Emphasis::Ranges(vec![(8, 13)]));
        assert_eq!(&text[8..13], "Tomas");
        // A read row has no bold names, whoever wrote what.
        assert_eq!(column(&three, false).1, Emphasis::None);
        // Nobody marked, or everybody: the unread row's names are all bold.
        let unmarked = [person("Ingrid", false), person("Tomas", false)];
        assert_eq!(column(&unmarked, true).1, Emphasis::All);
        let marked = [person("Ingrid", true), person("Tomas", true)];
        assert_eq!(column(&marked, true).1, Emphasis::All);
    }

    #[test]
    fn more_than_three_show_the_first_an_ellipsis_and_the_last_two() {
        let five = [
            person("Ingrid Solberg", true),
            person("Sana Malik", true),
            person("Kari Nordmann", false),
            person("Tomas Berg", true),
            ME,
        ];
        let (text, emphasis) = column(&five, true);
        assert_eq!(text, "Ingrid … Tomas, me");
        assert_eq!(emphasis, Emphasis::Ranges(vec![(0, 6), (11, 16)]));
        assert_eq!(&text[11..16], "Tomas");
        // Exactly three: all of them, no ellipsis.
        assert_eq!(column(&five[..3], false).0, "Ingrid, Sana, Kari");
        // Four (a conversation's row carries the first and the three latest).
        assert_eq!(column(&five[1..], false).0, "Sana … Tomas, me");
    }

    #[test]
    fn people_are_named_once() {
        // The user from two addresses, and one person from two addresses, whatever the case.
        let me_again = Person {
            name: "alex@work.example.test",
            ..ME
        };
        let people = [
            person("Ingrid Solberg", false),
            ME,
            person("INGRID SOLBERG", true),
            me_again,
            person("Tomas Berg", false),
        ];
        let distinct = distinct(&people);
        assert_eq!(distinct.len(), 3);
        assert!(
            distinct[0].unread,
            "the merged person is unread when any copy is"
        );
        assert_eq!(column(&people, false).0, "Ingrid, me, Tomas");
        let (text, emphasis) = column(&people, true);
        assert_eq!(emphasis, Emphasis::Ranges(vec![(0, 6)]));
        assert_eq!(&text[0..6], "Ingrid");
        // Two of the user's addresses alone are one "me", named whole.
        assert_eq!(column(&[ME, me_again], false).0, "me");
        // Two people who share a first name stay two.
        let two = [
            person("Ingrid Solberg", false),
            person("Ingrid Berg", false),
        ];
        assert_eq!(column(&two, false).0, "Ingrid, Ingrid");
    }

    #[test]
    fn the_spoken_names_say_you_and_count_those_left_out() {
        assert_eq!(
            spoken(&[person("Ingrid Solberg", true)], 1),
            "Ingrid Solberg"
        );
        assert_eq!(spoken(&[ME], 1), "you");
        assert_eq!(
            spoken(
                &[
                    person("Ingrid Solberg", true),
                    person("Tomas Berg", false),
                    ME
                ],
                3
            ),
            "Ingrid, Tomas and you"
        );
        let four = [
            person("Ingrid Solberg", true),
            person("Sana Malik", true),
            person("Tomas Berg", true),
            ME,
        ];
        // The row carries four of the six who wrote: three are named, three left out.
        assert_eq!(spoken(&four, 6), "Ingrid, Tomas, you and 3 others");
        assert_eq!(spoken(&four, 4), "Ingrid, Tomas, you and 1 other");
        assert_eq!(spoken(&[], 0), "");
        // A total below the people given counts what was given.
        assert_eq!(spoken(&four[..2], 0), "Ingrid and Sana");
    }

    /// The trip of the self-tests' samples: Ingrid started it, Tomas, the user and Sana joined,
    /// Ingrid and then Tomas wrote the two newest, both unread.
    fn trip() -> [Person<'static>; 4] {
        [
            wrote("Ingrid Solberg", true, 500),
            wrote("Tomas Berg", true, 600),
            Person { last: 300, ..ME },
            wrote("Sana Malik", false, 400),
        ]
    }

    #[test]
    fn past_three_the_column_names_the_two_who_wrote_last() {
        // Not the two who joined last ("Ingrid … me, Sana"): Tomas wrote the newest message.
        let (text, emphasis) = column(&trip(), true);
        assert_eq!(text, "Ingrid … Sana, Tomas");
        assert_eq!(emphasis, Emphasis::Ranges(vec![(0, 6), (17, 22)]));
        assert_eq!(&text[17..22], "Tomas");
        assert_eq!(
            spoken(&trip(), 4),
            "Ingrid, Sana, Tomas and 1 other",
            "the screen reader hears the same names"
        );
    }

    #[test]
    fn shorter_columns_keep_whole_names() {
        let texts: Vec<String> = columns(&trip(), 4, true)
            .into_iter()
            .map(|(text, _)| text.into_owned())
            .collect();
        assert_eq!(
            texts,
            ["Ingrid … Sana, Tomas", "Ingrid … Tomas", "Tomas +3"]
        );
        let short = columns(&trip(), 6, true).pop().unwrap();
        assert_eq!(short.0, "Tomas +5", "everyone who wrote is counted");
        assert_eq!(
            short.1,
            Emphasis::Ranges(vec![(0, 5)]),
            "the name is bold, never the count"
        );
        // Read, nothing bold; the latest writer is still the one kept.
        let read = columns(&trip(), 4, false);
        assert_eq!(read[1].0, "Ingrid … Tomas");
        assert!(read.iter().all(|(_, emphasis)| *emphasis == Emphasis::None));
        // No text cuts a name, and none is empty.
        for (text, _) in columns(&trip(), 4, true) {
            assert!(!text.contains("… ,") && !text.ends_with(", ") && !text.is_empty());
        }
    }

    #[test]
    fn the_short_column_is_about_unread_mail_and_others_than_the_user() {
        // The user wrote last: the column keeps the latest other writer.
        let people = [
            wrote("Ingrid Solberg", false, 100),
            wrote("Tomas Berg", false, 200),
            Person { last: 300, ..ME },
        ];
        let texts: Vec<String> = columns(&people, 3, false)
            .into_iter()
            .map(|(text, _)| text.into_owned())
            .collect();
        assert_eq!(texts, ["Ingrid, Tomas, me", "Ingrid … Tomas", "Tomas +2"]);
        // In an unread row, the latest with unread mail, though someone wrote since.
        let people = [
            wrote("Ingrid Solberg", false, 100),
            wrote("Kari Nordmann", true, 200),
            wrote("Tomas Berg", false, 300),
        ];
        assert_eq!(columns(&people, 3, true)[1].0, "Ingrid … Kari");
        // The first wrote last: the shorter names her and the latest of the others, the
        // shortest her and the others counted.
        let people = [
            wrote("Ingrid Solberg", true, 300),
            wrote("Tomas Berg", false, 200),
            Person { last: 100, ..ME },
        ];
        let texts: Vec<String> = columns(&people, 3, true)
            .into_iter()
            .map(|(text, _)| text.into_owned())
            .collect();
        assert_eq!(texts, ["Ingrid, Tomas, me", "Ingrid … Tomas", "Ingrid +2"]);
        // Two people: nothing between their two names and the shortest.
        let two = [
            wrote("Ingrid Solberg", false, 1),
            wrote("Tomas Berg", false, 2),
        ];
        let texts: Vec<String> = columns(&two, 2, false)
            .into_iter()
            .map(|(text, _)| text.into_owned())
            .collect();
        assert_eq!(texts, ["Ingrid, Tomas", "Tomas +1"]);
        // One person: their whole name, nothing shorter.
        assert_eq!(columns(&[person("Ingrid Solberg", true)], 1, true).len(), 1);
        assert_eq!(columns(&[], 0, true).len(), 1);
    }
}
