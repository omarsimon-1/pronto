//! Command-palette matching, from Stet. A query matches a text when its characters appear in
//! it in order, ignoring case; whole-substring matches, word starts and runs of consecutive
//! characters score higher. A query of several words is a whole substring when its words, one
//! space apart, are ("mark as read").
//!
//! The Superhuman pass adds what its palette does on top ([`find`], [`browse`]): a command is
//! also found by its aliases ("done" finds Archive, shown as "Archive (done)"), a query with two
//! letters swapped still finds it ("arhcive"), scattered one-letter matches of a longer query
//! are left out, and recently used commands and the ones that suit what is selected rank higher
//! among equal matches. The empty query shows the suggestions, then the recent commands, then
//! the rest; [`Recent`] is what `palette.toml` keeps between runs.

use crate::actions::ActionId;

/// How well `query` matches `text`; `None` when it does not. An empty query matches
/// everything with score 0. Higher is better.
pub fn score(query: &str, text: &str) -> Option<u32> {
    let folded = fold_query(query);
    if folded.is_empty() {
        return Some(0);
    }
    spaced_query(query)
        .and_then(|spaced| matched(&spaced, text))
        .filter(|(class, _)| *class > Class::Scattered)
        .or_else(|| matched(&folded, text))
        .map(|(_, score)| score)
}

/// Where a text matched, from the weakest: its letters scattered through it, or a whole
/// substring inside a word, at a word's start, or at the text's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Scattered,
    Inside,
    WordStart,
    Start,
}

/// How the folded, non-empty `query` matches `text`, and its [`score`]. The text's whitespace
/// reads as plain spaces, so a query of several words ([`spaced_query`]) finds it as typed.
fn matched(query: &[char], text: &str) -> Option<(Class, u32)> {
    let text: Vec<char> = text.chars().collect();
    let lower: Vec<char> = text
        .iter()
        .map(|&c| if c.is_whitespace() { ' ' } else { fold(c) })
        .collect();

    if let Some(start) = find_run(&lower, query) {
        let word_start = is_word_start(&text, start);
        let (class, bonus) = if start == 0 {
            (Class::Start, 3000)
        } else if word_start {
            (Class::WordStart, 2000)
        } else {
            (Class::Inside, 1000)
        };
        return Some((class, bonus + 500u32.saturating_sub(start as u32)));
    }

    let mut total = 0u32;
    let mut next = 0;
    let mut previous: Option<usize> = None;
    for &wanted in query {
        let found = (next..lower.len()).find(|&index| lower[index] == wanted)?;
        total += if previous == Some(found.wrapping_sub(1)) {
            8
        } else if is_word_start(&text, found) {
            6
        } else {
            1
        };
        previous = Some(found);
        next = found + 1;
    }
    Some((Class::Scattered, total))
}

/// The query as it is matched: lower case, without whitespace.
fn fold_query(query: &str) -> Vec<char> {
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(fold)
        .collect()
}

/// A query of several words as typed: lower case, each run of whitespace one space, none at
/// the ends ("mark  as read " is "mark as read"); `None` for a single word. Folded without its
/// spaces it could only ever match scattered, the spaces being letters it skips.
fn spaced_query(query: &str) -> Option<Vec<char>> {
    let mut words = query.split_whitespace();
    let mut spaced: Vec<char> = words.next()?.chars().map(fold).collect();
    let mut several = false;
    for word in words {
        several = true;
        spaced.push(' ');
        spaced.extend(word.chars().map(fold));
    }
    several.then_some(spaced)
}

/// One lowercase character per character, so query and text stay aligned.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn find_run(haystack: &[char], needle: &[char]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn is_word_start(text: &[char], index: usize) -> bool {
    match index.checked_sub(1).map(|before| text[before]) {
        None => true,
        Some(before) => {
            !before.is_alphanumeric() || (before.is_lowercase() && text[index].is_uppercase())
        }
    }
}

/// Indexes of `items` that match `query`, best first; ties keep their original order.
pub fn rank<'a>(query: &str, items: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut scored: Vec<(usize, u32)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, text)| score(query, text).map(|score| (index, score)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(index, _)| index).collect()
}

/// A scattered match of a query this long or longer needs [`SCATTERED_PER_CHAR`] points a
/// character (mostly word starts and runs), so "arch" doesn't find every title with an a, an r,
/// a c and an h in it.
const SCATTERED_FROM: usize = 3;
const SCATTERED_PER_CHAR: u32 = 3;

/// Points for the most recently used command; each older one gets [`RECENT_STEP`] fewer.
pub const RECENT_BOOST: u32 = 360;
pub const RECENT_STEP: u32 = 30;
/// Points for a command that suits what is selected (a [`browse`] suggestion).
pub const SUGGESTED_BOOST: u32 = 150;

/// One thing the palette offers, as it is searched.
#[derive(Debug, Clone, Copy, Default)]
pub struct Entry<'a> {
    pub title: &'a str,
    /// Other words that find it, in lower case.
    pub aliases: &'a [&'a str],
    /// Points added to its match: recently used, or suggested for what is selected.
    pub boost: u32,
    /// Listed only once something is typed (the folders: too many to browse).
    pub typed_only: bool,
}

/// An entry the query finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub index: usize,
    /// The alias it was found by, when that matched in a better class than its title (from
    /// its start, a word's start, inside a word, scattered): the palette shows it beside the
    /// title.
    pub alias: Option<usize>,
}

/// How an entry was found, from the weakest: its letters scattered through a title or an alias,
/// a whole substring once two neighbouring letters are swapped back ("arhcive"; scattered
/// letters after a swap are noise), or a whole substring as typed. [`find`] ranks by this first,
/// so a boost only reorders entries found the same way: no recently used or suggested command
/// found through a typo passes a real substring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Scattered,
    Swapped,
    Substring,
}

/// How `query` matches `entry`: by its title or by an alias, whichever matches in the better
/// class; the title when they match alike, so "inb" shows Go to All Inboxes, not "Go to All
/// Inboxes (unified inbox)". Scattered letters of a longer query need enough points.
fn best_text(query: &[char], entry: &Entry) -> Option<(Class, u32, Option<usize>)> {
    let plausible = |&(class, score): &(Class, u32)| {
        class > Class::Scattered
            || query.len() < SCATTERED_FROM
            || score >= SCATTERED_PER_CHAR * query.len() as u32
    };
    let title = matched(query, entry.title).filter(plausible);
    let alias = entry
        .aliases
        .iter()
        .enumerate()
        .filter_map(|(index, alias)| {
            let (class, score) = matched(query, alias).filter(plausible)?;
            Some((class, score, index))
        })
        .fold(
            None,
            |best: Option<(Class, u32, usize)>, found| match best {
                Some(top) if (top.0, top.1) >= (found.0, found.1) => best,
                _ => Some(found),
            },
        );
    match (title, alias) {
        (Some((class, _)), Some((better, score, index))) if better > class => {
            Some((better, score, Some(index)))
        }
        (Some((class, score)), _) => Some((class, score, None)),
        (None, alias) => alias.map(|(class, score, index)| (class, score, Some(index))),
    }
}

/// How `query` finds `entry`, its score, and the alias it was found by: as typed when that is a
/// substring (words and the spaces between them, then the letters without the spaces), else the
/// best swap of two neighbouring letters that makes one ("oepn" finds Open), else the
/// scattered letters as typed. `spaced` is the query of several words ([`spaced_query`]).
fn entry_score(
    query: &[char],
    spaced: Option<&[char]>,
    entry: &Entry,
) -> Option<(Kind, u32, Option<usize>)> {
    // "mark as read" is Mark as Read itself; folded to "markasread" it is only ever Mark
    // Folder as Read's letters, scattered, and a recent command found that way came first.
    if let Some((class, score, alias)) = spaced.and_then(|spaced| best_text(spaced, entry))
        && class > Class::Scattered
    {
        return Some((Kind::Substring, score, alias));
    }
    let straight = best_text(query, entry);
    if let Some((class, score, alias)) = straight
        && class > Class::Scattered
    {
        return Some((Kind::Substring, score, alias));
    }
    if query.len() >= SCATTERED_FROM {
        let mut swapped = query.to_vec();
        let mut found: Option<(u32, Option<usize>)> = None;
        for at in 0..query.len() - 1 {
            if query[at] == query[at + 1] {
                continue;
            }
            swapped.swap(at, at + 1);
            if let Some((class, score, alias)) = best_text(&swapped, entry)
                && class > Class::Scattered
                && found.is_none_or(|(top, _)| score > top)
            {
                found = Some((score, alias));
            }
            swapped.swap(at, at + 1);
        }
        if let Some((score, alias)) = found {
            return Some((Kind::Swapped, score, alias));
        }
    }
    straight.map(|(_, score, alias)| (Kind::Scattered, score, alias))
}

/// The entries `query` finds, best first: as typed before swapped before scattered, then by the
/// match's score plus the entry's boost, then found by its title before found by an alias
/// ("mark" lists Mark as Read before Archive's "mark done"), ties in the entries' order. The
/// empty query finds every entry that isn't `typed_only`, in order.
pub fn find(query: &str, entries: &[Entry]) -> Vec<Found> {
    let folded = fold_query(query);
    if folded.is_empty() {
        return entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.typed_only)
            .map(|(index, _)| Found { index, alias: None })
            .collect();
    }
    let spaced = spaced_query(query);
    let mut scored: Vec<(Kind, u32, Found)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let (kind, score, alias) = entry_score(&folded, spaced.as_deref(), entry)?;
            Some((kind, score + entry.boost, Found { index, alias }))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(a.2.alias.is_some().cmp(&b.2.alias.is_some()))
            .then(a.2.index.cmp(&b.2.index))
    });
    scored.into_iter().map(|(_, _, found)| found).collect()
}

/// Where a row of the empty query belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// What suits the selection or the place: Archive, Reply, Star… (Superhuman's suggestions).
    Suggested,
    Recent,
    /// Everything else, in the entries' order (the palette heads it by category).
    Other,
}

/// The commands the empty query suggests first, the ones that suit where the palette opened
/// (Superhuman's: Mark Done, Remind Me, Star, Move… on a conversation): a composer's, the
/// selected messages', or the window's. Those not available are left out by the caller.
pub fn suggestions(composer: bool, selection: bool) -> &'static [ActionId] {
    if composer {
        &[
            ActionId::Send,
            ActionId::AttachFile,
            ActionId::ShowCcBcc,
            ActionId::DiscardDraft,
        ]
    } else if selection {
        &[
            ActionId::Archive,
            ActionId::Reply,
            ActionId::Star,
            ActionId::MoveTo,
            ActionId::ToggleRead,
            ActionId::RemindMe,
        ]
    } else {
        &[
            ActionId::Compose,
            ActionId::Search,
            ActionId::GoToFolder,
            ActionId::GoToInbox,
        ]
    }
}

/// How many recently used commands the empty query lists before the rest.
pub const RECENT_SHOWN: usize = 5;

/// The rows of the empty query: `suggested` in their order, then up to [`RECENT_SHOWN`] of
/// `recent` (newest first) that aren't suggested, then every other entry in its order; each
/// once, `typed_only` entries left out unless they were used recently (a folder gone to).
pub fn browse(entries: &[Entry], suggested: &[usize], recent: &[usize]) -> Vec<(usize, Section)> {
    let mut rows: Vec<(usize, Section)> = Vec::with_capacity(entries.len());
    let mut taken = vec![false; entries.len()];
    let shown = |index: usize| entries.get(index).is_some_and(|entry| !entry.typed_only);
    for &index in suggested {
        if shown(index) && !taken[index] {
            taken[index] = true;
            rows.push((index, Section::Suggested));
        }
    }
    let mut recents = 0;
    for &index in recent {
        if recents == RECENT_SHOWN {
            break;
        }
        if index < entries.len() && !taken[index] {
            taken[index] = true;
            recents += 1;
            rows.push((index, Section::Recent));
        }
    }
    for (index, entry) in entries.iter().enumerate() {
        if !entry.typed_only && !taken[index] {
            rows.push((index, Section::Other));
        }
    }
    rows
}

/// Where Pronto keeps the palette's recent commands: `$XDG_STATE_HOME/pronto/`.
pub const PALETTE_FILE: &str = "palette.toml";

/// How many recently run commands and places the palette remembers.
pub const RECENT_MAX: usize = 12;

/// The commands and places the palette ran last, newest first, by their keys: a command's
/// registry name (`archive`), a place's own (`folder-12`). Pronto writes it, so it is read
/// leniently: anything it doesn't know is left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recent {
    keys: Vec<String>,
}

impl Recent {
    pub fn parse(text: &str) -> Self {
        let mut recent = Self::default();
        let Ok(table) = text.parse::<toml::Table>() else {
            return recent;
        };
        let Some(keys) = table.get("recent").and_then(toml::Value::as_array) else {
            return recent;
        };
        for key in keys.iter().filter_map(toml::Value::as_str) {
            let valid = !key.is_empty()
                && key.len() <= 200
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
            if valid && !recent.keys.iter().any(|known| known == key) {
                recent.keys.push(key.to_owned());
            }
            if recent.keys.len() == RECENT_MAX {
                break;
            }
        }
        recent
    }

    pub fn to_text(&self) -> String {
        let mut table = toml::Table::new();
        table.insert(
            "recent".to_owned(),
            toml::Value::Array(
                self.keys
                    .iter()
                    .map(|key| toml::Value::String(key.clone()))
                    .collect(),
            ),
        );
        format!(
            "# Pronto writes this file: the commands the palette ran last, newest first.\n{}",
            toml::to_string(&table).unwrap_or_default()
        )
    }

    /// `key` ran now: it goes first.
    pub fn used(&mut self, key: &str) {
        self.keys.retain(|known| known != key);
        self.keys.insert(0, key.to_owned());
        self.keys.truncate(RECENT_MAX);
    }

    /// How recently `key` ran: 0 is the last.
    pub fn rank(&self, key: &str) -> Option<usize> {
        self.keys.iter().position(|known| known == key)
    }

    /// The points [`find`] adds for `key`: [`RECENT_BOOST`] for the last, less for older ones.
    pub fn boost(&self, key: &str) -> u32 {
        self.rank(key).map_or(0, |rank| {
            RECENT_BOOST.saturating_sub(RECENT_STEP * rank as u32)
        })
    }

    pub fn keys(&self) -> &[String] {
        &self.keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(score("", "Archive"), Some(0));
        assert_eq!(score("  ", ""), Some(0));
    }

    #[test]
    fn matches_in_order_ignoring_case() {
        assert!(score("rpl", "Reply").is_some());
        assert!(score("REPLY", "reply all").is_some());
        assert!(score("gi", "Go to Inbox").is_some());
        assert_eq!(score("lr", "Reply"), None);
        assert_eq!(score("x", "Reply"), None);
    }

    #[test]
    fn substrings_beat_scattered_matches() {
        let exact = score("reply", "Reply All").unwrap();
        let scattered = score("rpal", "Reply All").unwrap();
        assert!(exact > scattered);
        assert!(score("all", "Reply All").unwrap() > score("al", "Go to Inbox").unwrap_or(0));
    }

    #[test]
    fn word_starts_beat_the_middle_of_words() {
        assert!(score("inbox", "Go to Inbox").unwrap() > score("nbox", "Go to Inbox").unwrap());
        assert!(score("gi", "Go to Inbox").unwrap() > score("oi", "Go to Inbox").unwrap());
    }

    #[test]
    fn ranking_is_stable() {
        let items = ["Reply", "Reply All", "Forward", "Archive", "Mark as Read"];
        assert_eq!(rank("reply", items), vec![0, 1]);
        assert_eq!(rank("", items), vec![0, 1, 2, 3, 4]);
        assert_eq!(rank("arch", items), vec![3]);
        assert_eq!(rank("re", items).first(), Some(&0));
    }

    #[test]
    fn unicode_text_does_not_panic() {
        assert!(score("ø", "Åpne fil ø").is_some());
        assert!(score("İst", "İstanbul").is_some());
        assert_eq!(score("日本", "日本語"), Some(3000 + 500));
        let entries = [Entry {
            title: "Åpne fil",
            aliases: &["økt"],
            ..Entry::default()
        }];
        assert_eq!(
            find("øtk", &entries).len(),
            1,
            "a swap in a non-ASCII alias"
        );
    }

    fn entry<'a>(title: &'a str, aliases: &'a [&'a str]) -> Entry<'a> {
        Entry {
            title,
            aliases,
            ..Entry::default()
        }
    }

    fn titles<'a>(query: &str, entries: &[Entry<'a>]) -> Vec<&'a str> {
        find(query, entries)
            .into_iter()
            .map(|found| entries[found.index].title)
            .collect()
    }

    #[test]
    fn aliases_find_commands_and_say_so() {
        let entries = [
            entry("Archive", &["done", "mark done"]),
            entry("Delete", &["trash", "bin"]),
            entry("Move to…", &["label", "folder"]),
            entry("Go to Archive", &["done"]),
            entry("Go to Trash", &[]),
        ];
        let found = find("done", &entries);
        assert_eq!(
            found,
            [
                Found {
                    index: 0,
                    alias: Some(0)
                },
                Found {
                    index: 3,
                    alias: Some(0)
                }
            ]
        );
        // The title wins when it matches as well as the alias.
        assert_eq!(
            find("arch", &entries)[0],
            Found {
                index: 0,
                alias: None
            }
        );
        // "trash" is Delete first: the alias matches from its start, the title in the middle.
        assert_eq!(titles("trash", &entries), ["Delete", "Go to Trash"]);
        assert_eq!(find("bin", &entries)[0].alias, Some(1));
        assert_eq!(titles("label", &entries), ["Move to…"]);
        // An alias that matches no better than the title isn't shown: "inb" starts a word of
        // both, so the row reads Go to All Inboxes.
        let inboxes = [entry("Go to All Inboxes", &["unified inbox"])];
        assert_eq!(find("inb", &inboxes)[0].alias, None);
        assert_eq!(find("unif", &inboxes)[0].alias, Some(0));
        // A title found by scattered letters doesn't hide an alias that starts with them.
        let entries = [entry("Plain Text Editor", &["plan"])];
        assert_eq!(find("plan", &entries)[0].alias, Some(0));
        assert_eq!(find("plai", &entries)[0].alias, None);
    }

    #[test]
    fn a_swapped_pair_of_letters_still_finds_it_but_never_first() {
        let entries = [
            entry("Open Message", &[]),
            entry("Archive", &[]),
            entry("Print…", &[]),
        ];
        assert_eq!(titles("oepn", &entries), ["Open Message"]);
        assert_eq!(titles("arhcive", &entries), ["Archive"]);
        assert_eq!(titles("xq", &entries), Vec::<&str>::new());
        // A straight substring beats a swapped one, and a swap counts only where it makes one.
        let entries = [entry("Pone", &[]), entry("Open", &[])];
        assert_eq!(titles("one", &entries), ["Pone"]);
        assert_eq!(titles("oen", &entries), ["Pone", "Open"]);
        assert_eq!(titles("oepn", &entries), ["Open"]);
        let entries = [
            entry("Move to Junk", &["spam"]),
            entry("Collapse All Messages", &[]),
        ];
        assert_eq!(titles("spam", &entries), ["Move to Junk"]);
        // Letters as typed that are too scattered to count don't stop the swap: "oepn" is in
        // here one letter a word, and "open" is its last word.
        let entries = [entry("Bob Kev Spy Inn Open", &[])];
        assert_eq!(titles("oepn", &entries), ["Bob Kev Spy Inn Open"]);
    }

    #[test]
    fn a_typo_never_passes_a_substring_whatever_its_boosts() {
        // "now" is inside the long title's last word; "Onward" has it only with two letters
        // swapped, from its start, and is both recent and suggested.
        let far = format!("{}Snow", "Weather ".repeat(15));
        let mut entries = [entry("Onward", &[]), entry(&far, &[])];
        entries[0].boost = RECENT_BOOST + SUGGESTED_BOOST;
        assert_eq!(titles("now", &entries), [far.as_str(), "Onward"]);
        // Among typos the boost counts as it does among substrings.
        let mut entries = [entry("Onward", &[]), entry("Onwards", &[])];
        assert_eq!(titles("now", &entries), ["Onward", "Onwards"]);
        entries[1].boost = RECENT_BOOST;
        assert_eq!(titles("now", &entries), ["Onwards", "Onward"]);
    }

    #[test]
    fn scattered_letters_of_a_longer_query_are_left_out() {
        let entries = [
            entry("Archive", &[]),
            entry("Always Show Images from This Sender", &[]),
            entry("Move to…", &[]),
        ];
        assert_eq!(titles("arch", &entries), ["Archive"]);
        // Word starts and runs still count: "mvto" finds Move to….
        assert_eq!(titles("mvto", &entries), ["Move to…"]);
        // Two letters are too few to judge.
        assert!(titles("as", &entries).contains(&"Always Show Images from This Sender"));
    }

    proptest::proptest! {
        /// Whatever the boosts, every entry whose title holds the query as typed is found, and
        /// before every entry found another way (a swap, scattered letters).
        #[test]
        fn substrings_come_first_whatever_the_boosts(
            query in "[a-dA-D ]{1,5}",
            listed in proptest::collection::vec(
                ("[a-d ]{0,10}", 0..=RECENT_BOOST + SUGGESTED_BOOST),
                0..10,
            ),
        ) {
            let entries: Vec<Entry> = listed
                .iter()
                .map(|(title, boost)| Entry {
                    title,
                    boost: *boost,
                    ..Entry::default()
                })
                .collect();
            // As typed: its letters together, or its words with one space between them.
            let typed: String = fold_query(&query).into_iter().collect();
            let words: Option<String> = spaced_query(&query).map(|spaced| spaced.into_iter().collect());
            let holds = |index: usize| {
                let title = entries[index].title;
                !typed.is_empty()
                    && (title.contains(&typed)
                        || words.as_deref().is_some_and(|words| title.contains(words)))
            };
            let found = find(&query, &entries);
            for index in (0..entries.len()).filter(|index| holds(*index)) {
                proptest::prop_assert!(found.iter().any(|found| found.index == index));
            }
            let first_other = found.iter().position(|found| !holds(found.index));
            if let Some(first_other) = first_other {
                proptest::prop_assert!(found[first_other..].iter().all(|found| !holds(found.index)));
            }
        }
    }

    #[test]
    fn several_words_are_found_as_typed_whatever_the_boosts() {
        // Folded to "markasread", the query was only ever scattered letters of both titles, so
        // the recently run Mark Folder as Read came first and Enter marked the whole folder.
        let mut entries = [
            entry("Mark Folder as Read", &[]),
            entry("Mark as Read", &[]),
            entry("Go to All Inboxes", &["unified inbox"]),
            entry("Go to Inbox", &[]),
        ];
        entries[0].boost = RECENT_BOOST;
        entries[2].boost = RECENT_BOOST - RECENT_STEP;
        assert_eq!(titles("mark as read", &entries)[0], "Mark as Read");
        assert_eq!(titles("  Mark   as READ ", &entries)[0], "Mark as Read");
        assert_eq!(titles("go to inbox", &entries)[0], "Go to Inbox");
        assert_eq!(titles("go to inb", &entries)[0], "Go to Inbox");
        // Words that are a substring only of an alias find it by the alias.
        assert_eq!(
            find("unified in", &entries)[0],
            Found {
                index: 2,
                alias: Some(0)
            }
        );
        // Words found apart still find a title, scattered, as before.
        assert_eq!(titles("folder read", &entries), ["Mark Folder as Read"]);
        // The letters without the spaces still count as typed: "re ply" is Reply.
        let entries = [entry("Reply", &[]), entry("Reply All", &[])];
        assert_eq!(titles("re ply", &entries), ["Reply", "Reply All"]);
        assert!(score("reply all", "Reply All").unwrap() > score("rpal", "Reply All").unwrap());
        assert!(
            score("to inbox", "Go to Inbox").unwrap()
                > score("to inbox", "Go to All Inboxes").unwrap_or(0)
        );
    }

    #[test]
    fn a_title_comes_before_an_alias_that_matches_as_well() {
        // "mark" starts Mark as Read's title and Archive's "mark done" alike: the command
        // called that comes first, whatever the registry's order.
        let mut entries = [
            entry("Archive", &["done", "mark done"]),
            entry("Mark as Read", &[]),
            entry("Mark as Unread", &[]),
        ];
        assert_eq!(
            titles("mark", &entries),
            ["Mark as Read", "Mark as Unread", "Archive"]
        );
        // Both suggested: still the title first.
        entries[0].boost = SUGGESTED_BOOST;
        entries[1].boost = SUGGESTED_BOOST;
        assert_eq!(
            titles("mark", &entries)[..2],
            ["Mark as Read", "Archive"],
            "a boost the title lacks still counts"
        );
        // A better match by alias still wins: "done" is Archive's alias from its start.
        let entries = [entry("Go to Done Items", &[]), entry("Archive", &["done"])];
        assert_eq!(titles("done", &entries), ["Archive", "Go to Done Items"]);
    }

    #[test]
    fn boosts_order_equal_matches_only() {
        let mut entries = [
            entry("Go to Inbox", &[]),
            entry("Go to Sent", &[]),
            entry("Forward", &["fwd"]),
        ];
        assert_eq!(titles("go", &entries), ["Go to Inbox", "Go to Sent"]);
        entries[1].boost = RECENT_BOOST;
        assert_eq!(titles("go", &entries), ["Go to Sent", "Go to Inbox"]);
        // A boost doesn't lift a scattered match over a substring.
        entries[2].boost = RECENT_BOOST + SUGGESTED_BOOST;
        assert_eq!(titles("to", &entries)[..2], ["Go to Sent", "Go to Inbox"]);
    }

    #[test]
    fn the_empty_query_browses_suggestions_then_recent_then_the_rest() {
        let mut entries = [
            entry("Next Message", &[]),
            entry("Archive", &[]),
            entry("Reply", &[]),
            entry("Star", &[]),
            entry("Refresh", &[]),
            entry("Go to Projects", &[]),
        ];
        entries[5].typed_only = true;
        let rows = browse(&entries, &[1, 2], &[4, 1, 5]);
        assert_eq!(
            rows,
            [
                (1, Section::Suggested),
                (2, Section::Suggested),
                (4, Section::Recent),
                (5, Section::Recent),
                (0, Section::Other),
                (3, Section::Other),
            ],
            "a folder gone to lately is listed among the recent ones"
        );
        assert_eq!(
            find("", &entries).len(),
            5,
            "typed-only entries wait for a query"
        );
        assert_eq!(titles("proj", &entries), ["Go to Projects"]);
        let recent: Vec<usize> = (0..5).chain(0..5).collect();
        let many = [entry("A", &[]); 8];
        assert_eq!(
            browse(&many, &[], &recent)
                .iter()
                .filter(|(_, section)| *section == Section::Recent)
                .count(),
            RECENT_SHOWN
        );
    }

    #[test]
    fn suggestions_suit_where_the_palette_opened() {
        assert_eq!(
            suggestions(false, true)[..5],
            [
                ActionId::Archive,
                ActionId::Reply,
                ActionId::Star,
                ActionId::MoveTo,
                ActionId::ToggleRead,
            ]
        );
        assert_eq!(suggestions(true, true)[0], ActionId::Send);
        assert_eq!(suggestions(false, false)[0], ActionId::Compose);
        for list in [
            suggestions(true, false),
            suggestions(false, true),
            suggestions(false, false),
        ] {
            for id in list {
                assert!(id.spec().palette, "{id:?}");
            }
        }
    }

    #[test]
    fn recent_commands_are_kept_newest_first_and_read_leniently() {
        let mut recent = Recent::default();
        for key in ["archive", "reply", "archive", "folder-12"] {
            recent.used(key);
        }
        assert_eq!(recent.keys(), ["folder-12", "archive", "reply"]);
        assert_eq!(recent.rank("archive"), Some(1));
        assert_eq!(recent.boost("folder-12"), RECENT_BOOST);
        assert_eq!(recent.boost("archive"), RECENT_BOOST - RECENT_STEP);
        assert_eq!(recent.boost("star"), 0);
        assert_eq!(Recent::parse(&recent.to_text()), recent);
        for index in 0..20 {
            recent.used(&format!("key-{index}"));
        }
        assert_eq!(recent.keys().len(), RECENT_MAX);
        assert_eq!(recent.keys()[0], "key-19");
        assert_eq!(Recent::parse("not toml ["), Recent::default());
        assert_eq!(
            Recent::parse("recent = [\"archive\", 3, \"a b\", \"archive\", \"\", \"star\"]\n")
                .keys(),
            ["archive", "star"]
        );
    }
}
