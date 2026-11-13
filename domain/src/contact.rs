//! The contact pane (the fork's Superhuman pass, docs/superhuman/contact.md): who the open
//! conversation is with, and the pane's words about them. Pure: the app reads what the store
//! holds from them and lays the pane out.

use crate::model::Address;

/// One message of the open conversation, as the pane weighs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Said<'a> {
    pub name: &'a str,
    pub email: &'a str,
    /// The user wrote it: one of their addresses, or a copy in a Sent or Drafts folder (which
    /// covers the identities the store doesn't know), as the list's participants say.
    pub mine: bool,
    /// A draft (in a Drafts folder, or flagged `\Draft`): never whom the pane shows.
    pub draft: bool,
}

/// Whose the pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// The sender of the message at this index.
    Sender(usize),
    /// Every message is the user's: whom the message at this index was written to.
    RecipientOf(usize),
    Nobody,
}

/// Whose the pane shows for `messages` (oldest first): the sender of `focused` (the message
/// the user moved to, or whose card has the keyboard focus) unless the user wrote it, else the
/// latest sender who isn't the user, as Superhuman's pane does; when the user wrote all of it,
/// whom they wrote to last. Drafts never count.
pub fn person(messages: &[Said<'_>], focused: Option<usize>) -> Pick {
    let theirs = |said: &Said<'_>| !said.mine && !said.draft && !said.email.trim().is_empty();
    if let Some(index) = focused.filter(|index| messages.get(*index).is_some_and(theirs)) {
        return Pick::Sender(index);
    }
    if let Some(index) = messages.iter().rposition(theirs) {
        return Pick::Sender(index);
    }
    match messages.iter().rposition(|said| !said.draft) {
        Some(index) => Pick::RecipientOf(index),
        None => Pick::Nobody,
    }
}

/// Whom a message of the user's was written to: its first recipient (To, then Cc) who isn't
/// the user.
pub fn recipient<'a>(
    to: &'a [Address],
    cc: &'a [Address],
    mine: impl Fn(&str) -> bool,
) -> Option<&'a Address> {
    to.iter()
        .chain(cc)
        .find(|address| !address.email.trim().is_empty() && !mine(&address.email))
}

/// The name the pane gives a person: their display name without quotes, else the address.
pub fn display_name<'a>(name: &'a str, email: &'a str) -> &'a str {
    let name = name.trim().trim_matches(['"', '\'']).trim();
    if name.is_empty() { email.trim() } else { name }
}

/// The one quiet line about how long the store has known them: "212 messages since Mar 2019"
/// (`since` is the first message's month as the locale abbreviates it, with its year), "1
/// message, Mar 2019"; the count alone when none of their mail is dated (no INTERNALDATE, no
/// Date header); nothing when the store holds none from them. Digits come in groups of three
/// past 9,999 (a narrow space, as the SI writes it, which reads in every locale).
pub fn history(messages: u32, since: Option<&str>) -> Option<String> {
    match (messages, since) {
        (0, _) => None,
        (1, Some(since)) => Some(format!("1 message, {since}")),
        (1, None) => Some("1 message".to_owned()),
        (_, Some(since)) => Some(format!("{} messages since {since}", grouped(messages))),
        (_, None) => Some(format!("{} messages", grouped(messages))),
    }
}

fn grouped(number: u32) -> String {
    let digits = number.to_string();
    if number < 10_000 {
        return digits;
    }
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(digit);
    }
    out
}

/// What "Search all mail from" runs: the search language's `from:` with the address, quoted
/// when it has a space or a quote in it.
pub fn search_query(email: &str) -> String {
    let email = email.trim();
    if email.contains(|c: char| c.is_whitespace() || c == '"') {
        format!("from:\"{}\"", email.replace('"', ""))
    } else {
        format!("from:{email}")
    }
}

/// The search link's words.
pub fn search_label(name: &str) -> String {
    format!("Search all mail from {name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(email: &'static str, mine: bool) -> Said<'static> {
        Said {
            name: "",
            email,
            mine,
            draft: false,
        }
    }

    #[test]
    fn the_latest_sender_who_isnt_me_by_default() {
        let messages = [
            said("ingrid@example.test", false),
            said("tomas@example.test", false),
            said("me@example.test", true),
        ];
        assert_eq!(person(&messages, None), Pick::Sender(1));
        // A draft of mine after it changes nothing, nor does one of theirs.
        let with_draft = [
            said("ingrid@example.test", false),
            Said {
                draft: true,
                ..said("sana@example.test", false)
            },
        ];
        assert_eq!(person(&with_draft, None), Pick::Sender(0));
    }

    #[test]
    fn the_focused_message_wins_unless_its_mine() {
        let messages = [
            said("ingrid@example.test", false),
            said("me@example.test", true),
            said("tomas@example.test", false),
        ];
        assert_eq!(person(&messages, Some(0)), Pick::Sender(0));
        assert_eq!(person(&messages, Some(1)), Pick::Sender(2));
        assert_eq!(person(&messages, Some(9)), Pick::Sender(2));
    }

    #[test]
    fn a_conversation_all_mine_shows_whom_i_wrote_to() {
        let messages = [said("me@example.test", true), said("me@example.test", true)];
        assert_eq!(person(&messages, Some(0)), Pick::RecipientOf(1));
        assert_eq!(person(&[], None), Pick::Nobody);
        let draft = [Said {
            draft: true,
            ..said("me@example.test", true)
        }];
        assert_eq!(person(&draft, None), Pick::Nobody);
        let mine = |email: &str| email.eq_ignore_ascii_case("me@example.test");
        let to = [
            Address::new("Me", "ME@example.test"),
            Address::new("Kari", "kari@example.test"),
        ];
        let cc = [Address::new("Lena", "lena@example.test")];
        assert_eq!(
            recipient(&to, &cc, mine).map(|a| a.name.as_str()),
            Some("Kari")
        );
        assert_eq!(
            recipient(&to[..1], &cc, mine).map(|a| a.name.as_str()),
            Some("Lena")
        );
        assert_eq!(recipient(&to[..1], &[], mine), None);
    }

    #[test]
    fn names_and_lines() {
        assert_eq!(
            display_name(" \"Ingrid Solberg\" ", "i@x.test"),
            "Ingrid Solberg"
        );
        assert_eq!(display_name("", " i@x.test "), "i@x.test");
        assert_eq!(history(0, Some("Mar 2019")), None);
        assert_eq!(history(0, None), None);
        assert_eq!(
            history(1, Some("Oct 2026")).as_deref(),
            Some("1 message, Oct 2026")
        );
        assert_eq!(
            history(212, Some("Mar 2019")).as_deref(),
            Some("212 messages since Mar 2019")
        );
        assert_eq!(
            history(9_999, Some("Mar 2019")).as_deref(),
            Some("9999 messages since Mar 2019")
        );
        assert_eq!(
            history(21_042, Some("Jan 2020")).as_deref(),
            Some("21\u{202f}042 messages since Jan 2020")
        );
        // Undated mail alone: how many, not since when.
        assert_eq!(history(1, None).as_deref(), Some("1 message"));
        assert_eq!(
            history(12_345, None).as_deref(),
            Some("12\u{202f}345 messages")
        );
        assert_eq!(grouped(1_234_567), "1\u{202f}234\u{202f}567");
        assert_eq!(
            search_query(" ingrid@example.test "),
            "from:ingrid@example.test"
        );
        assert_eq!(
            search_query("odd \"one\"@x.test"),
            "from:\"odd one@x.test\""
        );
        assert_eq!(
            search_label("Ingrid Solberg"),
            "Search all mail from Ingrid Solberg"
        );
    }

    #[test]
    fn the_search_query_parses_as_one_sender() {
        let query = crate::search::parse("from:ingrid@example.test");
        assert_eq!(query.from.len(), 1);
        assert!(query.text.is_empty());
    }
}
