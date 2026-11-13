//! Remind Me (the fork's Superhuman pass, docs/superhuman/remind.md): the user sends a
//! conversation away until a time, and it comes back to the top of the Inbox then. Its messages
//! wait in a folder of the account's own, "Reminders" (a label on Gmail), so other mail programs
//! see them gone from the Inbox too; the store keeps when each conversation is due and when it
//! came back, and the conversation sorts by that time until the user acts on it. These are the
//! pure parts: the folder's name, the sort date, which actions end the reminder's place at
//! the top, and (phase C) Someday and the order of the conversations waiting in Reminders.

use crate::folders;
use crate::model::FolderRole;
use crate::triage::Plan;

/// The folder a reminded conversation waits in, at the top of the account.
pub const FOLDER: &str = "Reminders";

/// The wake time of a reminder that never comes back on its own (Superhuman's "Someday",
/// phase C): 9999-12-31 23:59:59 UTC, past any time the timer waits for, so the store needs no
/// column of its own for it. It waits until the user removes it, or a reply ends it.
pub const SOMEDAY: i64 = 253_402_300_799;

/// Whether a reminder's wake time is [`SOMEDAY`].
pub const fn is_someday(wake_at: i64) -> bool {
    wake_at >= SOMEDAY
}

/// Where the sort keys of the conversations waiting in a Reminders folder start (phase C): past
/// any message date, so they come first there, before mail that is there for another reason.
pub const WAIT_BASE: i64 = 1 << 40;

/// The sort key of a waiting conversation's row in its Reminders folder: the soonest due
/// highest, so the newest-first list shows the soonest first and Someday last (the Reminders
/// view, `g h`). The row's date carries it, and [`wake_of_key`] reads the wake time back.
pub const fn wait_key(wake_at: i64) -> i64 {
    let wake_at = if wake_at < 0 {
        0
    } else if wake_at > SOMEDAY {
        SOMEDAY
    } else {
        wake_at
    };
    WAIT_BASE + (SOMEDAY - wake_at)
}

/// The wake time a row's sort key (`ConversationSummary::date`) stands for, when the row is a
/// waiting conversation's in its Reminders folder ([`wait_key`]).
pub const fn wake_of_key(key: i64) -> Option<i64> {
    if key >= WAIT_BASE && key <= WAIT_BASE + SOMEDAY {
        Some(SOMEDAY - (key - WAIT_BASE))
    } else {
        None
    }
}

/// Where a reminder stands, as the store keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReminderState {
    /// The conversation waits in Reminders until its time.
    Waiting,
    /// Its time came: the conversation is back in the Inbox and sorts by when it came back.
    Reminded,
    /// Cancelled, or acted on after it came back: kept only while Undo may restore it.
    Ended,
}

impl ReminderState {
    pub const fn code(self) -> u8 {
        match self {
            ReminderState::Waiting => 0,
            ReminderState::Reminded => 1,
            ReminderState::Ended => 2,
        }
    }

    /// Unknown codes are `Ended`.
    pub const fn from_code(code: u8) -> ReminderState {
        match code {
            0 => ReminderState::Waiting,
            1 => ReminderState::Reminded,
            _ => ReminderState::Ended,
        }
    }
}

/// The wire name and hierarchy delimiter of a new Reminders folder among an account's folders
/// (`name`, `delimiter` as the server lists them), as folder management names a new folder at
/// the top of the account: under the Inbox where every other folder is under it (`INBOX.`
/// servers), else at the top, with the Inbox's delimiter, else any folder's.
pub fn new_folder(listed: &[(&str, Option<char>)]) -> (String, Option<char>) {
    let parent = folders::top_parent(listed).map(|index| listed[index]);
    let delimiter = match parent {
        Some((_, delimiter)) => delimiter,
        None => listed
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("INBOX"))
            .and_then(|(_, delimiter)| *delimiter)
            .or_else(|| listed.iter().find_map(|(_, delimiter)| *delimiter)),
    };
    let name = folders::child(parent.map(|(name, _)| name), delimiter, FOLDER);
    (name, delimiter)
}

/// Whether a folder of the account is its Reminders folder: the name a new one would get
/// ([`new_folder`]), or "Reminders" at the top, in any case (servers differ on case, and the
/// user may have made it by hand).
pub fn is_folder(name: &str, listed: &[(&str, Option<char>)]) -> bool {
    name.eq_ignore_ascii_case(FOLDER) || name.eq_ignore_ascii_case(&new_folder(listed).0)
}

/// Where a conversation sorts in an Inbox: by its latest message, or by when it came back from
/// Reminders when that is later (Unix seconds both).
pub fn sort_date(latest: i64, reminded: Option<i64>) -> i64 {
    reminded.map_or(latest, |reminded| latest.max(reminded))
}

/// Whether an action (`plan`) on a conversation's messages in `source` ends its reminder in
/// `state`: once it is back, an action that takes it out of the Inbox (Done, Delete, Junk,
/// Move, Remind again) ends its place at the top; while it waits, one that takes its messages
/// out of the Reminders folder it waits in (`its_folder`) ends the wait. Read and star changes
/// end nothing: Superhuman keeps a conversation's place when it is read or starred, and opening
/// a conversation marks it read through the same queue.
pub fn ends(state: ReminderState, plan: Plan, source: FolderRole, its_folder: bool) -> bool {
    plan.leaves_folder()
        && match state {
            ReminderState::Reminded => source == FolderRole::Inbox,
            ReminderState::Waiting => its_folder,
            ReminderState::Ended => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Flags, FolderId};

    #[test]
    fn a_new_folder_goes_where_folder_management_puts_one() {
        // Folders beside the Inbox: at the top, with the Inbox's delimiter.
        let gmail = [
            ("INBOX", Some('/')),
            ("[Gmail]/Sent Mail", Some('/')),
            ("[Gmail]/All Mail", Some('/')),
        ];
        assert_eq!(new_folder(&gmail), ("Reminders".into(), Some('/')));
        // Every folder under the Inbox (Courier, older Dovecot): under it.
        let prefixed = [
            ("INBOX", Some('.')),
            ("INBOX.Sent", Some('.')),
            ("INBOX.Trash", Some('.')),
        ];
        assert_eq!(new_folder(&prefixed), ("INBOX.Reminders".into(), Some('.')));
        // The Inbox alone tells nothing: at the top.
        assert_eq!(
            new_folder(&[("INBOX", Some('.'))]),
            ("Reminders".into(), Some('.'))
        );
        // No delimiter anywhere: a flat account.
        assert_eq!(new_folder(&[("INBOX", None)]), ("Reminders".into(), None));
        assert_eq!(new_folder(&[]), ("Reminders".into(), None));
    }

    #[test]
    fn the_reminders_folder_is_found_in_any_case() {
        let prefixed = [("INBOX", Some('.')), ("INBOX.Sent", Some('.'))];
        assert!(is_folder("INBOX.Reminders", &prefixed));
        assert!(is_folder("inbox.reminders", &prefixed));
        assert!(is_folder("Reminders", &prefixed));
        assert!(is_folder("REMINDERS", &[("INBOX", Some('/'))]));
        assert!(!is_folder("Work/Reminders", &[("INBOX", Some('/'))]));
        assert!(!is_folder("Reminder", &[("INBOX", Some('/'))]));
    }

    #[test]
    fn a_reminded_conversation_sorts_by_the_later_of_its_mail_and_its_return() {
        assert_eq!(sort_date(1_000, None), 1_000);
        assert_eq!(sort_date(1_000, Some(5_000)), 5_000);
        // A reply after it came back sorts it by the reply.
        assert_eq!(sort_date(9_000, Some(5_000)), 9_000);
    }

    #[test]
    fn only_actions_that_take_it_away_end_a_reminder() {
        use ReminderState::{Ended, Reminded, Waiting};
        let moved = Plan::Move {
            to: FolderId(2),
            add: Flags::NONE,
            remove: Flags::NONE,
        };
        let read = Plan::Flags {
            add: Flags::SEEN,
            remove: Flags::NONE,
        };
        let star = Plan::Flags {
            add: Flags::FLAGGED,
            remove: Flags::NONE,
        };
        // Back in the Inbox: taken out of it, the conversation loses its place at the top.
        assert!(ends(Reminded, moved, FolderRole::Inbox, false));
        assert!(ends(Reminded, Plan::Expunge, FolderRole::Inbox, false));
        assert!(!ends(Reminded, read, FolderRole::Inbox, false));
        assert!(!ends(Reminded, star, FolderRole::Inbox, false));
        // Elsewhere (a copy in Archive, a reply in Sent) the Inbox keeps it.
        assert!(!ends(Reminded, moved, FolderRole::Archive, false));
        assert!(!ends(Reminded, Plan::Expunge, FolderRole::Sent, false));
        // Waiting: taken out of its Reminders folder (archived there, say), the wait ends; a
        // reply that came meanwhile and is archived from the Inbox leaves it waiting.
        assert!(ends(Waiting, moved, FolderRole::Other, true));
        assert!(!ends(Waiting, read, FolderRole::Other, true));
        assert!(!ends(Waiting, moved, FolderRole::Inbox, false));
        assert!(!ends(Ended, moved, FolderRole::Inbox, true));
    }

    #[test]
    fn waiting_rows_sort_soonest_first_and_someday_last() {
        let (tuesday, friday) = (1_793_000_000, 1_793_300_000);
        // Newest first is soonest first.
        assert!(wait_key(tuesday) > wait_key(friday));
        assert!(wait_key(friday) > wait_key(SOMEDAY));
        // Above any message's date, so they come before other mail in the folder.
        assert!(wait_key(SOMEDAY) > 4_102_444_800);
        // The key gives the wake time back; a message date is no key.
        for at in [0, tuesday, friday, SOMEDAY] {
            assert_eq!(wake_of_key(wait_key(at)), Some(at));
        }
        assert_eq!(wake_of_key(1_793_000_000), None);
        assert_eq!(wake_of_key(wait_key(SOMEDAY + 9)), Some(SOMEDAY));
        assert!(is_someday(SOMEDAY) && !is_someday(friday));
    }

    #[test]
    fn states_round_trip_through_their_codes() {
        for state in [
            ReminderState::Waiting,
            ReminderState::Reminded,
            ReminderState::Ended,
        ] {
            assert_eq!(ReminderState::from_code(state.code()), state);
        }
        assert_eq!(ReminderState::from_code(9), ReminderState::Ended);
    }
}
