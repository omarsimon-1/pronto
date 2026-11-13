//! The Split Inbox's sorting (the fork's Superhuman pass, docs/superhuman/split.md): every
//! message is Important or Other. What its header says about how it was sent
//! ([`HeaderFacts`]) sorts it by the header rules; the server's own category, where it gives one
//! (Gmail's Primary tab), wins over them; a conversation the user wrote in is Important; and
//! the user's choice for a sender ("Always Important", "Always Other") wins over everything but
//! a conversation back from Reminders, which is Important until the user acts on it (Remind Me,
//! phase C: [`classify_reminded`]). The store keeps the facts, categories and choices and
//! applies [`classify_reminded`] to each message.

use std::fmt;

/// Where the Split Inbox puts a message, and a conversation: Important when any of its
/// messages in the view is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Split {
    Important,
    Other,
}

impl Split {
    pub fn is_important(self) -> bool {
        self == Split::Important
    }

    /// `Important` for `true`.
    pub fn of(important: bool) -> Split {
        if important {
            Split::Important
        } else {
            Split::Other
        }
    }
}

/// What a message's header says about how it was sent, as bits the store keeps: the inputs of
/// the header rules. Each one sorts the message into Other. A message stored before the fork
/// read them has only what the store kept of its header: its List-Id and sender
/// ([`HeaderFacts::LIST_ID`], [`HeaderFacts::ROBOT`]).
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct HeaderFacts(u8);

impl HeaderFacts {
    /// A List-Id field (RFC 2919): a mailing list's copy.
    pub const LIST_ID: HeaderFacts = HeaderFacts(1);
    /// A List-Unsubscribe field (RFC 2369, RFC 8058): mail sent in bulk, which one can leave.
    pub const LIST_UNSUBSCRIBE: HeaderFacts = HeaderFacts(2);
    /// `Precedence: bulk`, `list` or `junk` (RFC 2076, RFC 3834).
    pub const BULK: HeaderFacts = HeaderFacts(4);
    /// `Auto-Submitted` with any value but `no` (RFC 3834): written by a program.
    pub const AUTO_SUBMITTED: HeaderFacts = HeaderFacts(8);
    /// The sender's address takes no replies or sends notifications ([`is_robot`]).
    pub const ROBOT: HeaderFacts = HeaderFacts(16);
    pub const NONE: HeaderFacts = HeaderFacts(0);
    const KNOWN: u8 = 31;

    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Unknown bits are dropped.
    pub const fn from_bits(bits: u8) -> HeaderFacts {
        HeaderFacts(bits & Self::KNOWN)
    }

    pub const fn contains(self, other: HeaderFacts) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: HeaderFacts) -> HeaderFacts {
        HeaderFacts(self.0 | other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// What the header rules make of a message: Other with any fact, Important with none.
    pub fn split(self) -> Split {
        Split::of(self.is_empty())
    }

    /// The facts a message's stored columns still tell: whether it has a List-Id, and its
    /// sender. For a message stored before the fork read the other fields.
    pub fn stored(list_id: bool, sender: &str) -> HeaderFacts {
        facts(list_id, None, None, None, sender)
    }
}

impl fmt::Debug for HeaderFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = [
            (HeaderFacts::LIST_ID, "LIST_ID"),
            (HeaderFacts::LIST_UNSUBSCRIBE, "LIST_UNSUBSCRIBE"),
            (HeaderFacts::BULK, "BULK"),
            (HeaderFacts::AUTO_SUBMITTED, "AUTO_SUBMITTED"),
            (HeaderFacts::ROBOT, "ROBOT"),
        ];
        let set: Vec<&str> = names
            .iter()
            .filter(|(fact, _)| self.contains(*fact))
            .map(|(_, name)| *name)
            .collect();
        write!(f, "HeaderFacts({})", set.join(" | "))
    }
}

impl std::ops::BitOr for HeaderFacts {
    type Output = HeaderFacts;

    fn bitor(self, other: HeaderFacts) -> HeaderFacts {
        self.union(other)
    }
}

/// The facts of a header: whether it has a List-Id, the raw values of its List-Unsubscribe,
/// Precedence and Auto-Submitted fields where present, and the sender's address. Empty values
/// count as absent.
pub fn facts(
    list_id: bool,
    list_unsubscribe: Option<&str>,
    precedence: Option<&str>,
    auto_submitted: Option<&str>,
    sender: &str,
) -> HeaderFacts {
    let mut out = HeaderFacts::NONE;
    if list_id {
        out = out | HeaderFacts::LIST_ID;
    }
    if list_unsubscribe.is_some_and(|value| !value.trim().is_empty()) {
        out = out | HeaderFacts::LIST_UNSUBSCRIBE;
    }
    if precedence
        .and_then(first_word)
        .is_some_and(|word| matches!(word.as_str(), "bulk" | "list" | "junk"))
    {
        out = out | HeaderFacts::BULK;
    }
    if auto_submitted
        .and_then(first_word)
        .is_some_and(|word| word != "no")
    {
        out = out | HeaderFacts::AUTO_SUBMITTED;
    }
    if is_robot(sender) {
        out = out | HeaderFacts::ROBOT;
    }
    out
}

/// A field's value up to its first space, `;` or comment, in lower case: `auto-generated` of
/// `Auto-Submitted: auto-generated; owner-email="x@example.test"`. `None` when empty.
fn first_word(value: &str) -> Option<String> {
    let word = value
        .trim()
        .split(|c: char| c.is_whitespace() || c == ';' || c == '(')
        .next()?;
    (!word.is_empty()).then(|| word.to_ascii_lowercase())
}

/// Words of a local part that name a program's mailbox: notifications, alerts, mailers and
/// bounces, newsletters and digests, receipts and invoices.
const ROBOT_WORDS: [&str; 27] = [
    "alert",
    "alerts",
    "auto",
    "automated",
    "autoreply",
    "billing",
    "bot",
    "bounce",
    "bounces",
    "daemon",
    "digest",
    "invoice",
    "invoices",
    "mailer",
    "news",
    "newsletter",
    "newsletters",
    "notification",
    "notifications",
    "notifier",
    "notify",
    "postmaster",
    "receipt",
    "receipts",
    "reminder",
    "reminders",
    "updates",
];

/// Spellings of "don't reply" a local part may hold anywhere once its separators are gone:
/// `noreply`, `no-reply`, `no_reply`, `do-not-reply`, `DoNotReply`, `billing-noreply`.
const NO_REPLY: [&str; 4] = ["noreply", "donotreply", "dontreply", "noresponse"];

/// Whether `address` reads as a program's rather than a person's: its local part asks for no
/// reply anywhere (`noreply@`, `comments-noreply@`, `do_not_reply@`), or one of its words
/// (split at `.`, `-`, `_`, `=` and digits) names a notification, alert, mailer, newsletter or
/// receipt mailbox (`notifications@`, `calendar-notification@`, `mailer-daemon@`). Plus
/// addressing (`receipts+acct_1@`) is ignored. A person's address (`ingrid.berg@`,
/// `support@`, `info@`) is not.
pub fn is_robot(address: &str) -> bool {
    let address = address.trim();
    let local = match address.rfind('@') {
        Some(at) => &address[..at],
        None => return false,
    };
    let local = local
        .split('+')
        .next()
        .unwrap_or(local)
        .to_ascii_lowercase();
    let squashed: String = local.chars().filter(char::is_ascii_alphabetic).collect();
    if NO_REPLY.iter().any(|spelling| squashed.contains(spelling)) {
        return true;
    }
    local
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|word| ROBOT_WORDS.binary_search(&word).is_ok())
}

/// Where a message goes. In order: the user's `choice` for its sender; Important in a
/// conversation the user sent a message in (`sent_in`); the server's `category`, where it gave
/// one; else the header rules ([`HeaderFacts::split`]).
pub fn classify(
    facts: HeaderFacts,
    category: Option<Split>,
    sent_in: bool,
    choice: Option<Split>,
) -> Split {
    if let Some(choice) = choice {
        return choice;
    }
    if sent_in {
        return Split::Important;
    }
    category.unwrap_or_else(|| facts.split())
}

/// Where a message goes with Remind Me counted (the fork's phase C, docs/superhuman/remind.md):
/// Important while its conversation is back from Reminders (`reminded`, until the user acts
/// on it), whatever the rules of [`classify`] say, the user's choice for its sender included:
/// the user asked to see this conversation again. Else [`classify`].
pub fn classify_reminded(
    reminded: bool,
    facts: HeaderFacts,
    category: Option<Split>,
    sent_in: bool,
    choice: Option<Split>,
) -> Split {
    if reminded {
        return Split::Important;
    }
    classify(facts, category, sent_in, choice)
}

/// How a sender's address is compared with the user's choices: trimmed, in ASCII lower case
/// (as SQLite's `lower`, which the store compares with); `None` for an empty one.
pub fn sender_key(address: &str) -> Option<String> {
    let address = address.trim();
    (!address.is_empty()).then(|| address.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::parse;

    fn crlf(text: &str) -> Vec<u8> {
        text.replace('\n', "\r\n").into_bytes()
    }

    /// The facts of a raw message as the parser reads them.
    fn read(text: &str) -> HeaderFacts {
        parse(&crlf(text)).headers.facts
    }

    #[test]
    fn robot_words_are_sorted_for_the_search() {
        let mut sorted = ROBOT_WORDS;
        sorted.sort_unstable();
        assert_eq!(sorted, ROBOT_WORDS);
    }

    #[test]
    fn a_person_writing_from_a_company_domain_is_important() {
        let facts = read(
            "From: Ingrid Berg <ingrid.berg@acme-consulting.example.test>
To: ola@example.test
Subject: Kontrakten
Message-ID: <kontrakt-1@acme-consulting.example.test>
Content-Type: text/plain; charset=utf-8

Hei Ola, her er kontrakten.
",
        );
        assert_eq!(facts, HeaderFacts::NONE);
        assert_eq!(classify(facts, None, false, None), Split::Important);
        // Shared mailboxes people answer from are people too.
        for person in [
            "support@acme.example.test",
            "info@bakery.example.test",
            "hello@startup.example.test",
            "team@club.example.test",
            "abbott@example.test",
            "newsom@example.test",
            "automotive@dealer.example.test",
        ] {
            assert!(!is_robot(person), "{person}");
        }
    }

    #[test]
    fn newsletters_are_other() {
        let facts = read(
            "From: Fjellposten <nyhetsbrev@fjellposten.example.test>
To: ola@example.test
Subject: Ukens turer
List-Unsubscribe: <https://fjellposten.example.test/u/123>,
 <mailto:unsubscribe@fjellposten.example.test?subject=unsubscribe>
List-Unsubscribe-Post: List-Unsubscribe=One-Click
Precedence: bulk
Content-Type: text/plain; charset=utf-8

Ukens turer.
",
        );
        assert_eq!(facts, HeaderFacts::LIST_UNSUBSCRIBE | HeaderFacts::BULK);
        assert_eq!(classify(facts, None, false, None), Split::Other);
        // A newsletter address alone is enough.
        let facts = read("From: Weekly <newsletter@shop.example.test>\nSubject: x\n\n.\n");
        assert_eq!(facts, HeaderFacts::ROBOT);
    }

    #[test]
    fn github_and_jira_notifications_are_other() {
        let github = read(
            "From: Ingrid Berg <notifications@github.com>
Reply-To: owner/repo <reply+ABCDEF@reply.github.com>
To: owner/repo <repo@noreply.github.com>
Cc: Mention <mention@noreply.github.com>
Subject: Re: [owner/repo] Fix the parser (PR #42)
Message-ID: <owner/repo/pull/42/c1@github.com>
In-Reply-To: <owner/repo/pull/42@github.com>
List-ID: owner/repo <repo.owner.github.com>
List-Archive: https://github.com/owner/repo
List-Unsubscribe: <mailto:unsub+ABCDEF@reply.github.com>,
 <https://github.com/notifications/unsubscribe-auth/ABCDEF>
Precedence: list
X-GitHub-Sender: ingrid
Content-Type: text/plain; charset=UTF-8

Looks good to me.
",
        );
        assert_eq!(
            github,
            HeaderFacts::LIST_ID
                | HeaderFacts::LIST_UNSUBSCRIBE
                | HeaderFacts::BULK
                | HeaderFacts::ROBOT
        );
        let jira = read(
            "From: \"Tomas Lie (Jira)\" <jira@acme.atlassian.net>
To: ola@example.test
Subject: [JIRA] (OPS-12) Disk full on build-3
Auto-Submitted: auto-generated
Precedence: bulk
Content-Type: text/plain; charset=UTF-8

Tomas Lie updated OPS-12.
",
        );
        assert_eq!(jira, HeaderFacts::BULK | HeaderFacts::AUTO_SUBMITTED);
        assert_eq!(classify(jira, None, false, None), Split::Other);
    }

    #[test]
    fn mailing_lists_are_other_until_the_user_writes_in_the_thread() {
        let facts = read(
            "From: Kari Nordmann <kari@example.test>
To: rust-users@lists.example.test
Subject: Re: Borrowing in closures
List-Id: Rust users <rust-users.lists.example.test>
List-Post: <mailto:rust-users@lists.example.test>
Precedence: list
Content-Type: text/plain; charset=utf-8

Try a move closure.
",
        );
        assert_eq!(facts, HeaderFacts::LIST_ID | HeaderFacts::BULK);
        assert_eq!(classify(facts, None, false, None), Split::Other);
        assert_eq!(classify(facts, None, true, None), Split::Important);
    }

    #[test]
    fn calendar_invites_from_people_are_important_and_reminders_other() {
        let invite = read(
            "From: Ingrid Berg <ingrid@acme.example.test>
Sender: Google Calendar <calendar-notification@google.com>
To: ola@example.test
Subject: Invitation: Planning @ Thu Oct 29, 2026 10am - 11am (CET)
Content-Type: multipart/mixed; boundary=\"cal\"

--cal
Content-Type: text/calendar; charset=UTF-8; method=REQUEST

BEGIN:VCALENDAR
METHOD:REQUEST
END:VCALENDAR
--cal--
",
        );
        assert_eq!(invite, HeaderFacts::NONE, "the organizer is the From");
        let reminder = read(
            "From: Google Calendar <calendar-notification@google.com>
To: ola@example.test
Subject: Notification: Planning @ Thu Oct 29, 2026 10am
Auto-Submitted: auto-generated
Content-Type: text/plain; charset=UTF-8

Planning starts in 10 minutes.
",
        );
        assert_eq!(reminder, HeaderFacts::AUTO_SUBMITTED | HeaderFacts::ROBOT);
    }

    #[test]
    fn receipts_and_no_reply_senders_are_other() {
        for sender in [
            "receipts+acct_1Abc@stripe.com",
            "noreply@uber.com",
            "no_reply@email.apple.com",
            "no-reply@accounts.google.com",
            "DoNotReply@bank.example.test",
            "do-not-reply@tickets.example.test",
            "comments-noreply@docs.google.com",
            "auto-confirm@amazon.example.test",
            "MAILER-DAEMON@mx.example.test",
            "postmaster@example.test",
            "msprvs1=19ab=bounces-1234@mail.example.test",
            "billing@hosting.example.test",
            "invoice@utility.example.test",
            "alerts@bank.example.test",
        ] {
            assert!(is_robot(sender), "{sender}");
        }
        let receipt = read(
            "From: Acme <receipts+acct_1Abc@stripe.com>
To: ola@example.test
Subject: Your receipt from Acme #1234-5678
Content-Type: text/plain; charset=utf-8

Amount paid: NOK 120,00
",
        );
        assert_eq!(receipt, HeaderFacts::ROBOT);
        assert_eq!(classify(receipt, None, false, None), Split::Other);
        assert!(!is_robot("no at sign"));
        assert!(!is_robot(""));
    }

    #[test]
    fn auto_submitted_no_and_odd_precedence_are_not_bulk() {
        assert_eq!(
            facts(false, None, Some(" first-class"), Some(" no"), "a@b.test"),
            HeaderFacts::NONE
        );
        assert_eq!(
            facts(false, Some("  "), Some(""), Some(""), "a@b.test"),
            HeaderFacts::NONE,
            "empty fields count as absent"
        );
        assert_eq!(
            facts(
                false,
                None,
                Some("JUNK"),
                Some("Auto-Replied (vacation)"),
                "a@b.test"
            ),
            HeaderFacts::BULK | HeaderFacts::AUTO_SUBMITTED
        );
        // An out-of-office reply.
        let vacation = read(
            "From: Tomas Lie <tomas@example.test>
To: ola@example.test
Subject: Automatic reply: Kontrakten
Auto-Submitted: auto-replied
Content-Type: text/plain

I am away until Monday.
",
        );
        assert_eq!(vacation, HeaderFacts::AUTO_SUBMITTED);
    }

    #[test]
    fn the_order_of_the_rules() {
        let bulk = HeaderFacts::LIST_UNSUBSCRIBE;
        let none = HeaderFacts::NONE;
        // Gmail's category wins over the header rules, both ways.
        assert_eq!(
            classify(bulk, Some(Split::Important), false, None),
            Split::Important
        );
        assert_eq!(
            classify(none, Some(Split::Other), false, None),
            Split::Other
        );
        // A conversation the user wrote in wins over the category.
        assert_eq!(
            classify(none, Some(Split::Other), true, None),
            Split::Important
        );
        // The user's choice wins over everything.
        assert_eq!(
            classify(none, Some(Split::Important), true, Some(Split::Other)),
            Split::Other
        );
        assert_eq!(
            classify(bulk, Some(Split::Other), false, Some(Split::Important)),
            Split::Important
        );
        // Back from Reminders (phase C), a conversation is Important over the user's choice
        // too; once acted on, the rules apply again.
        assert_eq!(
            classify_reminded(true, bulk, Some(Split::Other), false, Some(Split::Other)),
            Split::Important
        );
        assert_eq!(
            classify_reminded(false, bulk, Some(Split::Other), false, Some(Split::Other)),
            Split::Other
        );
    }

    #[test]
    fn stored_facts_and_bits() {
        assert_eq!(
            HeaderFacts::stored(true, "noreply@x.test"),
            HeaderFacts::LIST_ID | HeaderFacts::ROBOT
        );
        assert_eq!(HeaderFacts::stored(false, "kari@x.test"), HeaderFacts::NONE);
        let all = HeaderFacts::from_bits(255);
        assert_eq!(all.bits(), 31, "unknown bits are dropped");
        assert_eq!(
            format!("{:?}", HeaderFacts::LIST_ID | HeaderFacts::ROBOT),
            "HeaderFacts(LIST_ID | ROBOT)"
        );
        assert_eq!(
            sender_key("  Kari@Example.TEST "),
            Some("kari@example.test".into())
        );
        assert_eq!(sender_key(" "), None);
    }
}
