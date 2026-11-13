//! The window's layout between runs (the user's UI tweaks, 2026-10-08): the widths the user
//! dragged the sidebar and the message list to, the accounts folded in the sidebar, the
//! accounts whose "More folders" are open, the folders whose subfolders it folds away (M9,
//! folders; the sidebar comes back as the user left it, their word on 2026-10-09), the view
//! the window last showed, which a new window opens on (issue #5, 2026-10-11), and since the
//! Superhuman pass (2026-10-28) whether the sidebar shows and whether the reader sits beside
//! the list ([`ViewMode`]), and whether the list groups messages into conversations and the
//! composer card shows its formatting toolbar (phase B), and whether the Inbox shows the Split
//! Inbox's tabs and the contact pane (phase C), in
//! `$XDG_STATE_HOME/pronto/layout.toml`. Pronto writes it; it
//! isn't a setting, so it is read leniently: anything it doesn't know, or a width out of range,
//! is left out and the default applies.

use crate::account::AccountKey;
use crate::accounts_file::is_valid_key;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;
use toml::Value;

pub const LAYOUT_FILE: &str = "layout.toml";
/// The widths the sidebar and the message list can be dragged to, in px.
pub const SIDEBAR_WIDTHS: RangeInclusive<u32> = 160..=360;
pub const LIST_WIDTHS: RangeInclusive<u32> = 280..=640;

/// How the message list and the reader share the window (the Superhuman pass, 2026-10-28):
/// Focused shows one at a time, the conversation in the list's place; Split shows the reader
/// beside the list (the reading pane).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Focused,
    Split,
}

impl ViewMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Focused => "focused",
            Self::Split => "split",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Focused, Self::Split]
            .into_iter()
            .find(|mode| mode.name() == name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// The full sidebar's width; `None` for the default.
    pub sidebar_width: Option<u32>,
    /// The message list's width beside the reader; `None` for the default.
    pub list_width: Option<u32>,
    /// Focused or Split, as the user last chose; `None` until they do (the window's default).
    pub mode: Option<ViewMode>,
    /// Whether the sidebar shows beside the list in a wide window, as the user last chose;
    /// `None` until they do (hidden by default).
    pub sidebar: Option<bool>,
    /// Whether the list shows one row per conversation (Group by Conversation), as the user
    /// last chose; `None` until they do (on by default).
    pub conversations: Option<bool>,
    /// Whether the composer card shows its formatting toolbar, as the user last chose; `None`
    /// until they do (hidden by default, the fork's phase B: its keys work all the same).
    pub formatting: Option<bool>,
    /// Whether the Inbox shows the Split Inbox's tabs, as the user last switched it (Split
    /// Inbox in the palette, the fork's phase C); `None` until they do: `config.toml`'s
    /// `[inbox] split` decides, and a change there forgets this.
    pub split: Option<bool>,
    /// Whether an open conversation shows the contact pane where its margin has room (Contact
    /// Details), as the user last chose; `None` until they do (shown by default, phase C).
    pub contact: Option<bool>,
    /// The accounts whose folders the sidebar folds away, by key.
    pub collapsed: Vec<AccountKey>,
    /// The accounts whose "More folders" are open, by key.
    pub more_open: Vec<AccountKey>,
    /// The folders whose subfolders the sidebar folds away: each account's by key, by the
    /// folder's whole name.
    pub folded: BTreeMap<AccountKey, BTreeSet<String>>,
    /// The view the window last showed; `None`: the first account's Inbox.
    pub view: Option<View>,
}

/// A view a new window can open on: All Inboxes, or an account's folder by its whole name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    AllInboxes,
    Folder { account: AccountKey, name: String },
}

const ALL_INBOXES: &str = "all-inboxes";

impl Layout {
    pub fn parse(text: &str) -> Self {
        let Ok(table) = text.parse::<toml::Table>() else {
            return Self::default();
        };
        let width = |name: &str, range: &RangeInclusive<u32>| {
            table
                .get(name)
                .and_then(Value::as_integer)
                .and_then(|width| u32::try_from(width).ok())
                .filter(|width| range.contains(width))
        };
        let keys = |name: &str| {
            let mut keys: Vec<AccountKey> = Vec::new();
            for key in table
                .get(name)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|key| is_valid_key(key))
            {
                if !keys.iter().any(|known| known.0 == key) {
                    keys.push(AccountKey(key.to_owned()));
                }
            }
            keys
        };
        let mut folded: BTreeMap<AccountKey, BTreeSet<String>> = BTreeMap::new();
        for (key, names) in table
            .get("folded")
            .and_then(Value::as_table)
            .into_iter()
            .flatten()
            .filter(|(key, _)| is_valid_key(key))
        {
            let names: BTreeSet<String> = names
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect();
            if !names.is_empty() {
                folded.insert(AccountKey(key.clone()), names);
            }
        }
        let view = match table.get("view") {
            Some(Value::String(view)) if view == ALL_INBOXES => Some(View::AllInboxes),
            Some(Value::Table(view)) => {
                match (
                    view.get("account").and_then(Value::as_str),
                    view.get("folder").and_then(Value::as_str),
                ) {
                    (Some(account), Some(name)) if is_valid_key(account) && !name.is_empty() => {
                        Some(View::Folder {
                            account: AccountKey(account.to_owned()),
                            name: name.to_owned(),
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        Self {
            sidebar_width: width("sidebar_width", &SIDEBAR_WIDTHS),
            list_width: width("list_width", &LIST_WIDTHS),
            mode: table
                .get("mode")
                .and_then(Value::as_str)
                .and_then(ViewMode::from_name),
            sidebar: table.get("sidebar").and_then(Value::as_bool),
            conversations: table.get("conversations").and_then(Value::as_bool),
            formatting: table.get("formatting").and_then(Value::as_bool),
            split: table.get("split").and_then(Value::as_bool),
            contact: table.get("contact").and_then(Value::as_bool),
            collapsed: keys("collapsed"),
            more_open: keys("more_open"),
            folded,
            view,
        }
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from(
            "# Pronto keeps the window's layout here: the widths the sidebar and the message\n\
             # list were dragged to, whether the sidebar, the reading pane and the composer's\n\
             # formatting toolbar and contact pane show, whether the list groups messages into conversations and\n\
             # splits the Inbox, the accounts and folders folded or open in the sidebar, and the\n\
             # view the window last showed.\n\
             # It isn't a setting; delete the file to go back to the defaults.\n",
        );
        if let Some(width) = self.sidebar_width {
            text.push_str(&format!("sidebar_width = {width}\n"));
        }
        if let Some(width) = self.list_width {
            text.push_str(&format!("list_width = {width}\n"));
        }
        if let Some(mode) = self.mode {
            text.push_str(&format!("mode = \"{}\"\n", mode.name()));
        }
        if let Some(shown) = self.sidebar {
            text.push_str(&format!("sidebar = {shown}\n"));
        }
        if let Some(grouped) = self.conversations {
            text.push_str(&format!("conversations = {grouped}\n"));
        }
        if let Some(shown) = self.formatting {
            text.push_str(&format!("formatting = {shown}\n"));
        }
        if let Some(split) = self.split {
            text.push_str(&format!("split = {split}\n"));
        }
        if let Some(shown) = self.contact {
            text.push_str(&format!("contact = {shown}\n"));
        }
        let keys: Vec<String> = self
            .collapsed
            .iter()
            .map(|key| format!("\"{}\"", key.0))
            .collect();
        text.push_str(&format!("collapsed = [{}]\n", keys.join(", ")));
        if !self.more_open.is_empty() {
            let keys: Vec<String> = self
                .more_open
                .iter()
                .map(|key| format!("\"{}\"", key.0))
                .collect();
            text.push_str(&format!("more_open = [{}]\n", keys.join(", ")));
        }
        match &self.view {
            Some(View::AllInboxes) => text.push_str(&format!("view = \"{ALL_INBOXES}\"\n")),
            Some(View::Folder { account, name }) => text.push_str(&format!(
                "view = {{ account = \"{}\", folder = {} }}\n",
                account.0,
                Value::String(name.clone())
            )),
            None => {}
        }
        if !self.folded.is_empty() {
            text.push_str("\n[folded]\n");
            for (key, names) in &self.folded {
                let names: Vec<String> = names
                    .iter()
                    .map(|name| Value::String(name.clone()).to_string())
                    .collect();
                text.push_str(&format!("{} = [{}]\n", key.0, names.join(", ")));
            }
        }
        text
    }

    /// Whether the folder `name` of `key`'s account has its subfolders folded away.
    pub fn is_folded(&self, key: &AccountKey, name: &str) -> bool {
        self.folded
            .get(key)
            .is_some_and(|names| names.contains(name))
    }

    /// Folds the subfolders of `key`'s folder `name` away or opens them again; true when that
    /// changed anything.
    pub fn set_folded(&mut self, key: &AccountKey, name: &str, folded: bool) -> bool {
        if folded {
            return self
                .folded
                .entry(key.clone())
                .or_default()
                .insert(name.to_owned());
        }
        let Some(names) = self.folded.get_mut(key) else {
            return false;
        };
        let changed = names.remove(name);
        if names.is_empty() {
            self.folded.remove(key);
        }
        changed
    }

    /// Opens `key`'s account's "More folders" or closes them; true when that changed anything.
    pub fn set_more_open(&mut self, key: &AccountKey, open: bool) -> bool {
        let known = self.more_open.iter().position(|known| known == key);
        match (known, open) {
            (None, true) => self.more_open.push(key.clone()),
            (Some(index), false) => {
                self.more_open.remove(index);
            }
            _ => return false,
        }
        true
    }

    /// `key`'s folder `old` is now `new`: its fold, its subfolders' and the remembered view follow
    /// it. True when that changed anything.
    pub fn rename_folded(
        &mut self,
        key: &AccountKey,
        old: &str,
        new: &str,
        delimiter: Option<char>,
    ) -> bool {
        let mut changed = false;
        if let Some(View::Folder { account, name }) = &mut self.view
            && account == key
            && let Some(to) = crate::folders::renamed(name, old, new, delimiter)
        {
            *name = to;
            changed = true;
        }
        let Some(names) = self.folded.get_mut(key) else {
            return changed;
        };
        let moved: Vec<(String, String)> = names
            .iter()
            .filter_map(|name| {
                crate::folders::renamed(name, old, new, delimiter).map(|to| (name.clone(), to))
            })
            .collect();
        for (from, to) in &moved {
            names.remove(from);
            names.insert(to.clone());
        }
        changed || !moved.is_empty()
    }

    /// Folds `key`'s account away or opens it again; true when that changed anything.
    pub fn set_collapsed(&mut self, key: &AccountKey, collapsed: bool) -> bool {
        let known = self.collapsed.iter().position(|known| known == key);
        match (known, collapsed) {
            (None, true) => self.collapsed.push(key.clone()),
            (Some(index), false) => {
                self.collapsed.remove(index);
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_broken_file_is_the_default() {
        assert_eq!(Layout::parse(""), Layout::default());
        assert_eq!(Layout::parse("not toml ["), Layout::default());
    }

    #[test]
    fn round_trips() {
        let layout = Layout {
            sidebar_width: Some(240),
            list_width: Some(420),
            mode: Some(ViewMode::Split),
            sidebar: Some(true),
            conversations: Some(false),
            formatting: Some(true),
            split: Some(false),
            contact: Some(false),
            collapsed: vec![AccountKey("work".into()), AccountKey("k3j5x8m2q9wa".into())],
            more_open: vec![AccountKey("home".into())],
            folded: BTreeMap::from([(
                AccountKey("work".into()),
                BTreeSet::from([
                    "Kunder".to_owned(),
                    "INBOX/Prosjekter \"Høst\" 📬".to_owned(),
                    "a\\b".to_owned(),
                ]),
            )]),
            view: Some(View::AllInboxes),
        };
        assert_eq!(Layout::parse(&layout.to_text()), layout);
        assert_eq!(
            Layout::parse(&Layout::default().to_text()),
            Layout::default()
        );
    }

    #[test]
    fn what_doesnt_fit_is_left_out() {
        let layout = Layout::parse(
            "sidebar_width = 9999\nlist_width = \"wide\"\nother = 1\n\
             collapsed = [\"work\", 3, \"Not a key\", \"work\", \"home\"]\n",
        );
        assert_eq!(
            layout,
            Layout {
                sidebar_width: None,
                list_width: None,
                mode: None,
                sidebar: None,
                conversations: None,
                formatting: None,
                split: None,
                contact: None,
                collapsed: vec![AccountKey("work".into()), AccountKey("home".into())],
                more_open: Vec::new(),
                folded: BTreeMap::new(),
                view: None,
            }
        );
        assert_eq!(
            Layout::parse("more_open = [\"work\", \"Not a key\", 4, \"work\"]").more_open,
            [AccountKey("work".into())]
        );
        let folded = Layout::parse(
            "[folded]\nwork = [\"Kunder\", 3, \"\"]\n\"Not a key\" = [\"Kunder\"]\nhome = []\n\
             club = \"Kunder\"\n",
        );
        assert_eq!(
            folded.folded,
            BTreeMap::from([(
                AccountKey("work".into()),
                BTreeSet::from(["Kunder".to_owned()])
            )])
        );
        assert_eq!(Layout::parse("list_width = 280").list_width, Some(280));
        assert_eq!(Layout::parse("list_width = 279").list_width, None);
    }

    #[test]
    fn the_sidebar_and_the_reading_pane_round_trip_and_unknown_values_are_left_out() {
        for (mode, sidebar) in [
            (Some(ViewMode::Focused), Some(false)),
            (Some(ViewMode::Split), Some(true)),
            (None, Some(true)),
            (Some(ViewMode::Split), None),
        ] {
            let layout = Layout {
                mode,
                sidebar,
                ..Layout::default()
            };
            assert_eq!(Layout::parse(&layout.to_text()), layout);
        }
        let written = Layout {
            sidebar_width: Some(300),
            list_width: Some(500),
            mode: Some(ViewMode::Split),
            sidebar: Some(true),
            ..Layout::default()
        }
        .to_text();
        assert!(
            written.contains(
                "sidebar_width = 300\nlist_width = 500\nmode = \"split\"\nsidebar = true\n"
            ),
            "{written}"
        );
        assert!(!Layout::default().to_text().contains("mode"));
        assert!(!Layout::default().to_text().contains("sidebar ="));
        for broken in [
            "mode = \"wide\"",
            "mode = 1",
            "mode = true",
            "sidebar = \"yes\"",
            "sidebar = 1",
        ] {
            let layout = Layout::parse(broken);
            assert_eq!((layout.mode, layout.sidebar), (None, None), "{broken}");
        }
        assert_eq!(ViewMode::default(), ViewMode::Focused);
        assert_eq!(ViewMode::from_name("split"), Some(ViewMode::Split));
        assert_eq!(ViewMode::from_name("Split"), None);
    }

    #[test]
    fn grouping_round_trips_and_is_left_out_until_chosen() {
        for conversations in [Some(true), Some(false), None] {
            let layout = Layout {
                conversations,
                ..Layout::default()
            };
            assert_eq!(Layout::parse(&layout.to_text()), layout);
        }
        assert!(
            Layout {
                conversations: Some(false),
                ..Layout::default()
            }
            .to_text()
            .contains("conversations = false\n")
        );
        assert!(!Layout::default().to_text().contains("conversations ="));
        for broken in ["conversations = \"yes\"", "conversations = 1"] {
            assert_eq!(Layout::parse(broken).conversations, None, "{broken}");
        }
    }

    #[test]
    fn the_contact_pane_round_trips_and_is_left_out_until_chosen() {
        for contact in [Some(true), Some(false), None] {
            let layout = Layout {
                contact,
                ..Layout::default()
            };
            assert_eq!(Layout::parse(&layout.to_text()), layout);
        }
        assert!(
            Layout {
                contact: Some(false),
                ..Layout::default()
            }
            .to_text()
            .contains("contact = false\n")
        );
        assert!(!Layout::default().to_text().contains("contact ="));
        for broken in ["contact = \"no\"", "contact = 0"] {
            assert_eq!(Layout::parse(broken).contact, None, "{broken}");
        }
    }

    #[test]
    fn folding_changes_the_list_once() {
        let mut layout = Layout::default();
        let work = AccountKey("work".into());
        assert!(layout.set_collapsed(&work, true));
        assert!(!layout.set_collapsed(&work, true));
        assert_eq!(layout.collapsed, std::slice::from_ref(&work));
        assert!(layout.set_collapsed(&work, false));
        assert!(!layout.set_collapsed(&work, false));
        assert!(layout.collapsed.is_empty());
        assert!(layout.set_folded(&work, "Kunder", true));
        assert!(!layout.set_folded(&work, "Kunder", true));
        assert!(layout.is_folded(&work, "Kunder"));
        assert!(!layout.is_folded(&work, "Kunder/Acme"));
        assert!(layout.set_folded(&work, "Kunder", false));
        assert!(!layout.set_folded(&work, "Kunder", false));
        assert!(
            layout.folded.is_empty(),
            "an account without folds leaves the table"
        );
    }

    #[test]
    fn more_folders_open_and_closed() {
        let mut layout = Layout::default();
        let work = AccountKey("work".into());
        assert!(layout.set_more_open(&work, true));
        assert!(!layout.set_more_open(&work, true));
        assert!(layout.to_text().contains("more_open = [\"work\"]\n"));
        assert!(layout.set_more_open(&work, false));
        assert!(!layout.to_text().contains("more_open"));
    }

    #[test]
    fn the_view_round_trips_and_follows_a_rename() {
        let work = AccountKey("work".into());
        for view in [
            View::AllInboxes,
            View::Folder {
                account: work.clone(),
                name: "Kunder/\"Acme\" & co".into(),
            },
        ] {
            let layout = Layout {
                view: Some(view),
                folded: BTreeMap::from([(work.clone(), BTreeSet::from(["Kunder".into()]))]),
                ..Layout::default()
            };
            assert_eq!(Layout::parse(&layout.to_text()), layout);
        }
        for broken in [
            "view = \"inbox\"",
            "view = 3",
            "view = { account = \"not a key!\", folder = \"INBOX\" }",
            "view = { account = \"work\", folder = \"\" }",
            "view = { account = \"work\" }",
        ] {
            assert_eq!(Layout::parse(broken).view, None, "{broken}");
        }
        let mut layout = Layout {
            view: Some(View::Folder {
                account: work.clone(),
                name: "Kunder/Acme".into(),
            }),
            ..Layout::default()
        };
        assert!(layout.rename_folded(&work, "Kunder", "Clients", Some('/')));
        assert_eq!(
            layout.view,
            Some(View::Folder {
                account: work.clone(),
                name: "Clients/Acme".into(),
            })
        );
        assert!(!layout.rename_folded(&AccountKey("home".into()), "Clients", "X", Some('/')));
    }

    #[test]
    fn a_renamed_folder_keeps_its_fold_and_its_subfolders() {
        let work = AccountKey("work".into());
        let mut layout = Layout::default();
        layout.set_folded(&work, "INBOX.Kunder", true);
        layout.set_folded(&work, "INBOX.Kunder.Acme", true);
        layout.set_folded(&work, "INBOX.Kunder2", true);
        assert!(layout.rename_folded(&work, "INBOX.Kunder", "INBOX.Klienter", Some('.')));
        assert_eq!(
            layout.folded[&work],
            BTreeSet::from([
                "INBOX.Klienter".to_owned(),
                "INBOX.Klienter.Acme".to_owned(),
                "INBOX.Kunder2".to_owned(),
            ])
        );
        assert!(!layout.rename_folded(&work, "Nowhere", "Else", Some('.')));
        assert!(!layout.rename_folded(
            &AccountKey("home".into()),
            "INBOX.Klienter",
            "X",
            Some('.')
        ));
    }
}
