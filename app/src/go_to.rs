//! Go to Inbox, Starred, Sent and Drafts (`g` then `i`, `s`, `t`, `d`) and Back to List
//! (Escape in the reader): the keymap's navigation commands, built in M9 (ML-092) with
//! the keys the registry already had. The Superhuman pass adds Go to Archive (`g e`:
//! Superhuman's Done, the account's Archive or, where it has none, the folder Archive moves
//! mail to, Gmail's All Mail), Go to Folder… (`g l`: a picker over every folder, as Move to…
//! is) and the palette's places (every folder by its name), and leaves Go to Account N to the
//! accounts there are. Go to All Inboxes (`g a`) and Go to Account 1 to 9
//! (`g 1` to `g 9`, that account's Inbox in the sidebar's order) followed for issue #12
//! (2026-10-19). A go-to shows the account's folder of that role: the
//! account the list shows, in All Inboxes the selected message's, else the first account's;
//! Go to Inbox from All Inboxes stays there, and Go to Starred turns on the Starred filter, as
//! there is no Starred folder. Back to List puts the keyboard focus in the list, on the row
//! the conversation was opened from (or moved on to), showing the list first when one pane
//! shows (the Superhuman pass's Focused layout).

use crate::application::AppState;
use crate::compose_actions::compose_account;
use crate::mailbox::folder_key;
use crate::triage::folder_title;
use crate::window::{ALL_INBOXES, Pane};
use sealwax_domain::actions::ActionId;
use sealwax_domain::folders;
use sealwax_domain::model::{AccountId, Folder, FolderId, FolderRole};
use sealwax_domain::split::Split;
use sealwax_infrastructure::store::{ViewFilter, ViewScope};
use std::rc::Rc;

/// The commands this module runs.
pub const COMMANDS: [ActionId; 17] = [
    ActionId::GoToInbox,
    ActionId::GoToStarred,
    ActionId::GoToSent,
    ActionId::GoToDrafts,
    ActionId::GoToArchive,
    ActionId::GoToFolder,
    ActionId::GoToAllInboxes,
    ActionId::GoToAccount1,
    ActionId::GoToAccount2,
    ActionId::GoToAccount3,
    ActionId::GoToAccount4,
    ActionId::GoToAccount5,
    ActionId::GoToAccount6,
    ActionId::GoToAccount7,
    ActionId::GoToAccount8,
    ActionId::GoToAccount9,
    ActionId::BackToList,
];

/// Where a go-to command leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    AllInboxes,
    Folder(FolderId),
}

/// The folder of `role` for the list showing `scope`, with `selected` the selected message's
/// folder: the shown account's, else the selected message's, else the first account's that
/// has one. Inbox from All Inboxes stays in All Inboxes.
pub fn destination(
    role: FolderRole,
    scope: Option<ViewScope>,
    selected: Option<FolderId>,
    folders: &[Folder],
) -> Option<Destination> {
    if role == FolderRole::Inbox && scope == Some(ViewScope::UnifiedInbox) {
        return Some(Destination::AllInboxes);
    }
    let account = compose_account(scope, selected, folders);
    let mut candidates: Vec<&Folder> = folders
        .iter()
        .filter(|folder| folder.role == role && folder.selectable)
        .collect();
    candidates.sort_by_key(|folder| (folder.account.0, folder.id.0));
    candidates
        .iter()
        .find(|folder| Some(folder.account) == account)
        .or_else(|| candidates.first())
        .map(|folder| Destination::Folder(folder.id))
}

/// Where Go to Archive leads for the list showing `scope`: the shown account's Archive, else
/// the folder Archive moves its mail to (`domain::triage`: Gmail's All Mail), else the first
/// account's that has either.
pub fn archive(
    scope: Option<ViewScope>,
    selected: Option<FolderId>,
    folders: &[Folder],
) -> Option<FolderId> {
    let account = compose_account(scope, selected, folders);
    let done = |account: AccountId| {
        [FolderRole::Archive, FolderRole::All]
            .into_iter()
            .find_map(|role| {
                folders
                    .iter()
                    .filter(|folder| {
                        folder.account == account && folder.role == role && folder.selectable
                    })
                    .min_by_key(|folder| folder.id.0)
                    .map(|folder| folder.id)
            })
    };
    let mut accounts: Vec<AccountId> = folders.iter().map(|folder| folder.account).collect();
    accounts.sort_by_key(|account| account.0);
    accounts.dedup();
    account
        .and_then(done)
        .or_else(|| accounts.into_iter().find_map(done))
}

/// The go-to commands that lead to a folder of their own with one account (its Inbox, Sent,
/// Drafts and Archive, and its Reminders folder once Remind Me made it: phase C): the palette
/// lists them, with their keys, in place of those folders. With several accounts they lead to
/// the shown account's, so none does.
pub fn command_places(accounts: usize, folders: &[Folder]) -> Vec<(ActionId, FolderId)> {
    if accounts != 1 {
        return Vec::new();
    }
    let reminders = folders
        .first()
        .and_then(|folder| crate::remind::reminders_folder(folder.account, folders));
    [
        (ActionId::GoToInbox, FolderRole::Inbox),
        (ActionId::GoToSent, FolderRole::Sent),
        (ActionId::GoToDrafts, FolderRole::Drafts),
    ]
    .into_iter()
    .filter_map(|(id, role)| match destination(role, None, None, folders) {
        Some(Destination::Folder(folder)) => Some((id, folder)),
        _ => None,
    })
    .chain(archive(None, None, folders).map(|folder| (ActionId::GoToArchive, folder)))
    .chain(reminders.map(|folder| (ActionId::GoToReminders, folder)))
    .collect()
}

/// A folder as the palette names it: its role's name ("Sent"), else its whole path in the
/// sidebar's words, the folders above it that hold mail first ("Inbox / Receipts",
/// "Kunder / 2026 / Acme"); a container that holds none (Gmail's `[Gmail]`) is left out.
pub fn folder_name(folder: &Folder, known: &[Folder]) -> String {
    if !matches!(folder.role, FolderRole::Other | FolderRole::All) {
        return folder_title(folder);
    }
    let mut parts = vec![folders::leaf(&folder.name, folder.delimiter).to_owned()];
    let mut name = folder.name.as_str();
    while let Some(parent) = folders::parent(name, folder.delimiter) {
        let above = known
            .iter()
            .find(|candidate| candidate.account == folder.account && candidate.name == parent);
        match above {
            Some(above) if above.selectable => parts.push(match above.role {
                FolderRole::Other | FolderRole::All => {
                    folders::leaf(parent, folder.delimiter).to_owned()
                }
                _ => folder_title(above),
            }),
            Some(_) => {}
            None => parts.push(folders::leaf(parent, folder.delimiter).to_owned()),
        }
        name = parent;
    }
    parts.reverse();
    parts.join(" / ")
}

/// Go to Account 1 to `count` work; the others wait for an account in their place.
pub fn show_accounts(actions: &crate::actions::ActionSet, count: usize) {
    for number in 1..=9 {
        if let Some(id) = ActionId::go_to_account(number) {
            actions.set_available(id, number <= count);
        }
    }
}

/// The Inbox of the sidebar's account in place `number` (from 1) of `accounts`, in their order.
pub fn account_inbox(
    number: usize,
    accounts: &[AccountId],
    folders: &[Folder],
) -> Option<FolderId> {
    let account = *accounts.get(number.checked_sub(1)?)?;
    folders
        .iter()
        .filter(|folder| {
            folder.account == account && folder.role == FolderRole::Inbox && folder.selectable
        })
        .min_by_key(|folder| folder.id.0)
        .map(|folder| folder.id)
}

/// Connects the commands on the main window's action set.
pub fn install(state: &Rc<AppState>, actions: &crate::actions::ActionSet) {
    for (id, role) in [
        (ActionId::GoToInbox, FolderRole::Inbox),
        (ActionId::GoToSent, FolderRole::Sent),
        (ActionId::GoToDrafts, FolderRole::Drafts),
    ] {
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            if let Some(state) = weak.upgrade() {
                go_to(&state, role);
            }
        });
    }
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::GoToAllInboxes, move || {
        if let Some(state) = weak.upgrade() {
            show(&state, ALL_INBOXES);
        }
    });
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::GoToArchive, move || {
        let Some(state) = weak.upgrade() else {
            return;
        };
        let Some(list) = state.list() else {
            return;
        };
        let selected = list
            .selected_targets()
            .ok()
            .and_then(|targets| targets.first().map(|target| target.folder));
        if let Some(folder) = archive(list.scope(), selected, &state.mailbox().folders()) {
            show_folder(&state, folder);
        }
    });
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::GoToFolder, move || {
        if let Some(state) = weak.upgrade() {
            pick_folder(&state);
        }
    });
    for number in 1..=9 {
        let Some(id) = ActionId::go_to_account(number) else {
            continue;
        };
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let accounts: Vec<AccountId> = state
                .mailbox()
                .accounts()
                .iter()
                .map(|account| account.id)
                .collect();
            if let Some(inbox) = account_inbox(number, &accounts, &state.mailbox().folders()) {
                show(&state, &folder_key(inbox));
            }
        });
    }
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::GoToStarred, move || {
        if let Some(state) = weak.upgrade()
            && let (Some(window), Some(list)) = (state.window(), state.list())
        {
            list.set_filter(ViewFilter::Starred);
            window.show_pane(Pane::List);
            list.focus_selected();
        }
    });
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::BackToList, move || {
        if let Some(state) = weak.upgrade() {
            back_to_list(&state);
        }
    });
    show_accounts(actions, state.mailbox().accounts().len());
}

/// Go to Folder…: Move to…'s picker over every folder that holds mail, each account's role
/// folders first, the account named beside them when there are several.
fn pick_folder(state: &Rc<AppState>) {
    let Some(window) = state.window() else {
        return;
    };
    let known = state.mailbox().folders();
    let accounts = state.mailbox().accounts();
    let mut choices: Vec<Folder> = known
        .iter()
        .filter(|folder| folder.selectable)
        .cloned()
        .collect();
    let place = |account: AccountId| {
        accounts
            .iter()
            .position(|known| known.id == account)
            .unwrap_or(usize::MAX)
    };
    choices.sort_by(|a, b| {
        place(a.account)
            .cmp(&place(b.account))
            .then_with(|| crate::triage::role_order(a.role).cmp(&crate::triage::role_order(b.role)))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    // One account needs no name beside its folders.
    let names: Vec<(AccountId, String)> = if accounts.len() > 1 {
        accounts
            .iter()
            .map(|account| (account.id, account.name.clone()))
            .collect()
    } else {
        Vec::new()
    };
    let weak = Rc::downgrade(state);
    crate::move_to::present_go_to(window.window(), &choices, &known, &names, move |folder| {
        if let Some(state) = weak.upgrade() {
            show_folder(&state, folder);
        }
    });
}

/// Shows `folder` in the list, which takes the keyboard focus (the palette's places, Go to
/// Archive, Go to Folder…).
pub fn show_folder(state: &Rc<AppState>, folder: FolderId) {
    show(state, &folder_key(folder));
}

fn go_to(state: &Rc<AppState>, role: FolderRole) {
    // The Inbox's Important side, where the Split Inbox shows (Superhuman's `g i`, phase C).
    let side = (role == FolderRole::Inbox).then_some(Split::Important);
    go_to_side(state, role, side);
}

/// The Inbox Go to Inbox shows, on the Split Inbox's `side` as it was left (Go to Important,
/// Go to Other).
pub(crate) fn go_to_inbox(state: &Rc<AppState>, side: Split) {
    go_to_side(state, FolderRole::Inbox, Some(side));
}

fn go_to_side(state: &Rc<AppState>, role: FolderRole, side: Option<Split>) {
    let Some(list) = state.list() else {
        return;
    };
    let selected = list
        .selected_targets()
        .ok()
        .and_then(|targets| targets.first().map(|target| target.folder));
    let key = match destination(role, list.scope(), selected, &state.mailbox().folders()) {
        Some(Destination::AllInboxes) => ALL_INBOXES.to_owned(),
        Some(Destination::Folder(folder)) => folder_key(folder),
        None => return,
    };
    // An Inbox opens on `side` at once, not on the side it showed last and then this one.
    list.want_split(side);
    show(state, &key);
    if let Some(side) = side {
        // The Inbox was shown already: it switches.
        list.show_split(side, false);
    }
    list.want_split(None);
}

/// Shows the sidebar's folder `key` in the list, which takes the keyboard focus.
fn show(state: &Rc<AppState>, key: &str) {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return;
    };
    // The folder itself, as picking it in the sidebar shows it: not only its starred mail.
    list.set_filter(ViewFilter::All);
    window.select_folder(key);
    list.focus_selected();
}

/// Leaves the reader for the list: the select mode ends as Escape ends it there, and the
/// list, shown first when one pane shows, takes the keyboard focus on the selected row.
fn back_to_list(state: &Rc<AppState>) {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return;
    };
    if list.is_select_mode() {
        list.leave_select_mode();
    }
    window.show_pane(Pane::List);
    list.focus_selected();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, account: i64, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: format!("folder {id}"),
            delimiter: Some('/'),
            role,
            selectable: true,
            inferiors: true,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn go_to_leads_to_the_shown_accounts_folder() {
        let folders = [
            folder(1, 1, FolderRole::Inbox),
            folder(2, 1, FolderRole::Sent),
            folder(3, 2, FolderRole::Inbox),
            folder(4, 2, FolderRole::Sent),
            folder(5, 2, FolderRole::Drafts),
        ];
        let second = Some(ViewScope::Folder(FolderId(3)));
        assert_eq!(
            destination(FolderRole::Sent, second, None, &folders),
            Some(Destination::Folder(FolderId(4)))
        );
        assert_eq!(
            destination(
                FolderRole::Inbox,
                Some(ViewScope::Folder(FolderId(4))),
                None,
                &folders
            ),
            Some(Destination::Folder(FolderId(3)))
        );
        // The first account has no Drafts: the first account that has one.
        let first = Some(ViewScope::Folder(FolderId(1)));
        assert_eq!(
            destination(FolderRole::Drafts, first, None, &folders),
            Some(Destination::Folder(FolderId(5)))
        );
        let all = Some(ViewScope::UnifiedInbox);
        assert_eq!(
            destination(FolderRole::Inbox, all, Some(FolderId(3)), &folders),
            Some(Destination::AllInboxes)
        );
        assert_eq!(
            destination(FolderRole::Sent, all, Some(FolderId(3)), &folders),
            Some(Destination::Folder(FolderId(4)))
        );
        assert_eq!(
            destination(FolderRole::Sent, all, None, &folders),
            Some(Destination::Folder(FolderId(2)))
        );
        assert_eq!(destination(FolderRole::Junk, first, None, &folders), None);
    }

    #[test]
    fn go_to_archive_is_the_accounts_archive_else_where_archive_moves_mail() {
        let folders = [
            folder(1, 1, FolderRole::Inbox),
            folder(2, 1, FolderRole::Archive),
            folder(3, 2, FolderRole::Inbox),
            folder(4, 2, FolderRole::All),
            folder(5, 3, FolderRole::Inbox),
        ];
        let shown = |id| Some(ViewScope::Folder(FolderId(id)));
        assert_eq!(archive(shown(1), None, &folders), Some(FolderId(2)));
        // Gmail: no Archive, so All Mail.
        assert_eq!(archive(shown(3), None, &folders), Some(FolderId(4)));
        // An account with neither: the first account's.
        assert_eq!(archive(shown(5), None, &folders), Some(FolderId(2)));
        assert_eq!(
            archive(Some(ViewScope::UnifiedInbox), Some(FolderId(3)), &folders),
            Some(FolderId(4))
        );
        assert_eq!(archive(None, None, &[]), None);
    }

    #[test]
    fn with_one_account_the_go_to_commands_stand_for_their_folders() {
        let folders = [
            folder(1, 1, FolderRole::Inbox),
            folder(2, 1, FolderRole::Sent),
            folder(3, 1, FolderRole::All),
            folder(4, 1, FolderRole::Other),
            folder(5, 1, FolderRole::Sent),
        ];
        assert_eq!(
            command_places(1, &folders),
            [
                (ActionId::GoToInbox, FolderId(1)),
                (ActionId::GoToSent, FolderId(2)),
                (ActionId::GoToArchive, FolderId(3)),
            ]
        );
        assert!(command_places(2, &folders).is_empty());
        assert!(command_places(1, &[]).is_empty());
        // Remind Me's folder, once there is one, is Go to Reminders' (the final review of
        // phase C: the palette listed it twice).
        let mut folders = folders.to_vec();
        folders.push(Folder {
            name: "Reminders".to_owned(),
            ..folder(6, 1, FolderRole::Other)
        });
        assert_eq!(
            command_places(1, &folders).last(),
            Some(&(ActionId::GoToReminders, FolderId(6)))
        );
    }

    #[test]
    fn the_palette_names_folders_by_role_or_by_their_path() {
        let named = |id: i64, account: i64, role: FolderRole, name: &str| Folder {
            name: name.to_owned(),
            ..folder(id, account, role)
        };
        let mut known = vec![
            named(1, 1, FolderRole::Inbox, "INBOX"),
            named(2, 1, FolderRole::Other, "INBOX/Receipts"),
            named(3, 1, FolderRole::Other, "Kunder"),
            named(4, 1, FolderRole::Other, "Kunder/2026/Acme"),
            named(5, 1, FolderRole::Sent, "[Gmail]/Sent Mail"),
            named(6, 1, FolderRole::Other, "[Gmail]"),
            named(7, 1, FolderRole::All, "[Gmail]/All Mail"),
        ];
        known[5].selectable = false;
        let name = |index: usize| folder_name(&known[index], &known);
        assert_eq!(name(0), "Inbox");
        assert_eq!(name(1), "Inbox / Receipts");
        assert_eq!(name(3), "Kunder / 2026 / Acme");
        assert_eq!(name(4), "Sent");
        assert_eq!(
            name(6),
            "All Mail",
            "a container that holds no mail is left out"
        );
    }

    #[test]
    fn go_to_account_n_is_the_inbox_of_the_nth_account_shown() {
        let mut folders = vec![
            folder(1, 7, FolderRole::Inbox),
            folder(2, 7, FolderRole::Sent),
            folder(3, 4, FolderRole::Inbox),
            folder(4, 9, FolderRole::Sent),
        ];
        let accounts = [AccountId(7), AccountId(4), AccountId(9)];
        assert_eq!(account_inbox(1, &accounts, &folders), Some(FolderId(1)));
        assert_eq!(account_inbox(2, &accounts, &folders), Some(FolderId(3)));
        assert_eq!(account_inbox(3, &accounts, &folders), None, "no Inbox");
        assert_eq!(
            account_inbox(4, &accounts, &folders),
            None,
            "no fourth account"
        );
        assert_eq!(account_inbox(0, &accounts, &folders), None);
        folders[2].selectable = false;
        assert_eq!(account_inbox(2, &accounts, &folders), None);
    }
}
