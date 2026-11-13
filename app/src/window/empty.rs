//! Empty states (DESIGN.md "Feedback"): no account yet, no message selected, an empty folder,
//! and Inbox Zero. As in the mock: a quiet icon, a title, a line of help and, for "No accounts
//! yet", the way forward; centred, and scrolled when the window is too small for them. Inbox
//! Zero (the Superhuman pass, 2026-10-28) is calmer and larger: the whole list area in a soft
//! tint of the theme's accent, a light title, the day and how to write.

use super::panes::{classed, icon};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::{Binding, Keymap};

/// "No accounts yet", with "Add an account" (the setup assistant, `win.add-account`) and a
/// hint with the command palette's keys.
pub struct NoAccount {
    pub page: gtk::Widget,
    pub add: gtk::Button,
    pub hint: gtk::Label,
}

pub fn no_account() -> NoAccount {
    let add = classed(
        gtk::Button::builder()
            .label("Add an account")
            .halign(gtk::Align::Center)
            .action_name(super::ADD_ACCOUNT_ACTION)
            .build(),
        &["sw-primary", "sw-add-account"],
    );
    let hint = classed(
        gtk::Label::builder()
            .label(palette_hint(&Keymap::default()))
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build(),
        &["sw-empty-hint"],
    );
    let state = empty_state(
        &["mail-unread-symbolic"],
        "No accounts yet",
        "Add your first account. An address and a password, or Microsoft sign-in, is all it \
         takes.",
        "sw-no-account",
    );
    state.append(&add);
    state.append(&hint);
    NoAccount {
        page: centred(&state),
        add,
        hint,
    }
}

/// `or press Ctrl+K for every command`, with the palette's first key in use.
pub fn palette_hint(keymap: &Keymap) -> String {
    match keymap.shown(ActionId::CommandPalette).first() {
        Some(key) => format!("or press {} for every command", key.display()),
        None => "or open the command palette for every command".to_owned(),
    }
}

/// The reader before a message is picked.
pub fn no_message() -> gtk::Widget {
    let state = empty_state(
        &["mail-read-symbolic"],
        "No message selected",
        "Pick a message in the list to read it here.",
        "sw-no-message",
    );
    state.set_widget_name("reader-empty-state");
    centred(&state)
}

/// An Inbox or All Inboxes with nothing in it (and no filter or search): "Inbox Zero", the day
/// and "Press c to write", with the Compose key in use. It can hold the keyboard focus, so the
/// list's keys (`c` among them) still work with no row to focus; a screen reader hears it all.
#[derive(Clone)]
pub struct InboxZero {
    widget: gtk::Widget,
    area: gtk::Box,
    title: gtk::Label,
    date: gtk::Label,
    /// "Press", the key as a keycap, "to write"; or "Nothing left to read" alone.
    hint: [gtk::Label; 3],
    /// The Split Inbox's other side, while this one is empty and it isn't (the fork's phase
    /// C): "Other has 4 conversations" and Next Split's key as a keycap.
    other: gtk::Box,
    other_text: gtk::Label,
    other_key: gtk::Label,
}

pub const INBOX_ZERO: &str = "Inbox Zero";

pub fn inbox_zero() -> InboxZero {
    let mark = classed(
        icon(&[
            "weather-clear-symbolic",
            "emblem-ok-symbolic",
            "object-select-symbolic",
        ]),
        &["sw-inbox-zero-mark"],
    );
    mark.set_accessible_role(gtk::AccessibleRole::Presentation);
    let [title, date] = [
        (INBOX_ZERO, "sw-inbox-zero-title"),
        ("", "sw-inbox-zero-date"),
    ]
    .map(|(text, class)| {
        classed(
            gtk::Label::builder()
                .label(text)
                .wrap(true)
                .justify(gtk::Justification::Center)
                .build(),
            &[class],
        )
    });
    title.set_widget_name("inbox-zero-title");
    date.set_widget_name("inbox-zero-date");
    let hint = ["", "", ""].map(|text| gtk::Label::new(Some(text)));
    hint[1].add_css_class("sw-inbox-zero-key");
    hint[1].set_widget_name("inbox-zero-key");
    let hint_row = classed(
        gtk::Box::builder().halign(gtk::Align::Center).build(),
        &["sw-inbox-zero-hint"],
    );
    hint_row.set_widget_name("inbox-zero-hint");
    for part in &hint {
        hint_row.append(part);
    }
    let other_text = gtk::Label::new(None);
    other_text.set_widget_name("inbox-zero-other-text");
    let other_key = classed(gtk::Label::new(None), &["sw-inbox-zero-key"]);
    other_key.set_widget_name("inbox-zero-other-key");
    let other = classed(
        gtk::Box::builder()
            .halign(gtk::Align::Center)
            .visible(false)
            .build(),
        &["sw-inbox-zero-other"],
    );
    other.set_widget_name("inbox-zero-other");
    other.append(&other_text);
    other.append(&other_key);
    let content = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .build(),
        &["sw-inbox-zero-content"],
    );
    content.append(&mark);
    content.append(&title);
    content.append(&date);
    content.append(&other);
    content.append(&hint_row);
    // The tint fills the area, the words sit in its middle.
    let area = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .accessible_role(gtk::AccessibleRole::Status)
            .build(),
        &["sw-inbox-zero"],
    );
    area.set_widget_name("inbox-zero-area");
    area.append(&content);
    let widget = crate::accessibility::tab_through(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&area)
            .build(),
    );
    let zero = InboxZero {
        widget,
        area,
        title,
        date,
        hint,
        other,
        other_text,
        other_key,
    };
    zero.set_keymap(&Keymap::default());
    zero
}

impl InboxZero {
    pub fn widget(&self) -> &gtk::Widget {
        &self.widget
    }

    pub fn title(&self) -> glib::GString {
        self.title.label()
    }

    pub fn date(&self) -> glib::GString {
        self.date.label()
    }

    /// "Wednesday, October 28": the day as the list's clock has it.
    pub fn set_date(&self, now: &glib::DateTime) {
        let day = now
            .format("%A, %B %-d")
            .map(|text| text.to_string())
            .unwrap_or_default();
        if self.date.label() != day {
            self.date.set_label(&day);
            self.label();
        }
    }

    /// The Split Inbox's other side holds mail while this one is empty: one quiet line says so
    /// ("Other has 4 conversations"), with the key that goes there; `None` hides it.
    pub fn set_other(&self, text: Option<&str>) {
        let text = text.unwrap_or_default();
        if self.other_text.label() != text {
            self.other_text.set_label(text);
            self.other.set_visible(!text.is_empty());
            self.label();
        }
    }

    /// The other side's line, as shown (empty when hidden).
    pub fn other(&self) -> glib::GString {
        self.other_text.label()
    }

    /// "Press c to write", the key as a keycap, with the Compose key in use.
    pub fn set_keymap(&self, keymap: &Keymap) {
        let next_split = keymap
            .shown(ActionId::NextSplit)
            .first()
            .map(Binding::display)
            .unwrap_or_default();
        self.other_key.set_label(&next_split);
        self.other_key.set_visible(!next_split.is_empty());
        let [before, key, after] = &self.hint;
        match compose_key(keymap) {
            Some(shown) => {
                before.set_label("Press");
                key.set_label(&shown);
                after.set_label("to write");
            }
            None => {
                before.set_label(&compose_hint(keymap));
                key.set_label("");
                after.set_label("");
            }
        }
        key.set_visible(!key.label().is_empty());
        after.set_visible(!after.label().is_empty());
        self.label();
    }

    /// Takes the keyboard focus; false when it isn't on screen.
    pub fn focus(&self) -> bool {
        self.area.is_mapped() && self.area.grab_focus()
    }

    /// "Inbox Zero. Wednesday, October 28. Other has 4 conversations. Press c to write."
    fn label(&self) {
        let hint: Vec<String> = self
            .hint
            .iter()
            .map(|part| part.label().to_string())
            .filter(|text| !text.is_empty())
            .collect();
        let parts: Vec<String> = [
            self.title.label().to_string(),
            self.date.label().to_string(),
            self.other_text.label().to_string(),
        ]
        .into_iter()
        .chain(std::iter::once(hint.join(" ")))
        .filter(|text| !text.is_empty())
        .collect();
        self.area
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "{}.",
                parts.join(". ")
            ))]);
    }
}

/// "Press c to write", with the first of Compose's keys in use.
pub fn compose_hint(keymap: &Keymap) -> String {
    match compose_key(keymap) {
        Some(key) => format!("Press {key} to write"),
        None => "Nothing left to read".to_owned(),
    }
}

/// The first of Compose's keys in use, as the keymap shows it.
fn compose_key(keymap: &Keymap) -> Option<String> {
    keymap
        .shown(ActionId::Compose)
        .first()
        .map(Binding::display)
}

/// The batch actions the reader's page offers while several messages are selected (ML-054),
/// in the toolbar's order.
pub const BATCH_ACTIONS: [ActionId; 7] = [
    ActionId::Archive,
    ActionId::Delete,
    ActionId::Junk,
    ActionId::MoveTo,
    ActionId::Star,
    ActionId::MarkRead,
    ActionId::MarkUnread,
];

/// The batch actions of the list's header in the select mode (phase B), Superhuman's few: Done,
/// Remind Me (phase C), Delete, Move, Read or Unread, Star or Unstar, in the reader's toolbar's
/// and the rows' quick actions' order; the rest are on the keys, the row menu and the palette.
pub const BAR_ACTIONS: [ActionId; 6] = [
    ActionId::Archive,
    ActionId::RemindMe,
    ActionId::Delete,
    ActionId::MoveTo,
    ActionId::ToggleRead,
    ActionId::Star,
];

/// An icon for a bar action, as the command's title has it now (the quick Star's look: a
/// full star unstars).
fn bar_icons(id: ActionId, title: &str) -> &'static [&'static str] {
    match id {
        ActionId::Archive => &["object-select-symbolic"],
        ActionId::RemindMe => &crate::list::row::REMIND_ICONS,
        ActionId::Delete => &["user-trash-symbolic"],
        ActionId::MoveTo => &["folder-symbolic"],
        ActionId::ToggleRead if title == ActionId::MarkUnread.title() => &["mail-unread-symbolic"],
        ActionId::ToggleRead => &["mail-read-symbolic"],
        ActionId::Star if title == "Unstar" => &["starred-symbolic"],
        _ => &["non-starred-symbolic", "starred-symbolic"],
    }
}

/// The select mode in the list's header row, where the reader's batch page isn't seen (Focused,
/// or one pane at a time): the count, the batch actions as quiet icons bound to the registry's
/// commands, and Clear, which leaves the select mode (`Window::connect_batch_cleared`).
pub struct BatchBar {
    pub widget: gtk::Box,
    count: gtk::Label,
    pub clear: gtk::Button,
    buttons: Vec<(ActionId, gtk::Button)>,
}

impl BatchBar {
    fn new() -> Self {
        let count = classed(
            gtk::Label::builder()
                .xalign(0.0)
                .valign(gtk::Align::BaselineCenter)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
            &["sw-pane-title", "sw-batch-count"],
        );
        count.set_widget_name("list-batch-count");
        let widget = classed(
            gtk::Box::builder()
                .valign(gtk::Align::Center)
                .visible(false)
                .accessible_role(gtk::AccessibleRole::Toolbar)
                .build(),
            &["sw-batch-bar"],
        );
        widget.set_widget_name("list-batch");
        widget.update_property(&[gtk::accessible::Property::Label("Selection")]);
        widget.append(&count);
        let mut buttons = Vec::new();
        for id in BAR_ACTIONS {
            let button = super::panes::icon_button(bar_icons(id, id.title()), id.title());
            // Star and Read at full strength (phase C: Adwaita's own looked disabled).
            if let Some(image) = button.child().and_downcast::<gtk::Image>() {
                super::glyph::set_icon(&image, bar_icons(id, id.title()));
            }
            button.set_action_name(Some(&id.detailed_name()));
            button.set_valign(gtk::Align::Center);
            button.set_widget_name(&format!("list-batch-{}", id.name()));
            widget.append(&button);
            buttons.push((id, button));
        }
        let clear = classed(
            gtk::Button::builder()
                .label("Clear")
                .valign(gtk::Align::Center)
                .tooltip_text("Clear the selection (Escape)")
                .build(),
            &["sw-batch-clear"],
        );
        clear.set_widget_name("list-batch-clear");
        widget.append(&clear);
        Self {
            widget,
            count,
            clear,
            buttons,
        }
    }

    /// A command's title (Unstar, Mark as Unread): the icon and the tooltip follow it.
    fn set_command(&self, id: ActionId, title: &str, tooltip: &str) {
        if let Some((_, button)) = self.buttons.iter().find(|(candidate, _)| *candidate == id) {
            if let Some(image) = button.child().and_downcast::<gtk::Image>() {
                super::glyph::set_icon(&image, bar_icons(id, title));
            }
            button.set_tooltip_text(Some(tooltip));
            button.update_property(&[gtk::accessible::Property::Label(title)]);
        }
    }
}

/// The reader in the select mode: "3 messages selected", what applies to them, and the batch
/// actions as buttons bound to the registry's commands. Its twin in the list's header, the
/// [`BatchBar`], says and does the same where the reader isn't seen.
pub struct BatchPage {
    pub widget: gtk::Widget,
    title: gtk::Label,
    buttons: Vec<(ActionId, gtk::Button)>,
    pub bar: BatchBar,
}

impl BatchPage {
    pub fn new() -> Self {
        let (state, title, _) = empty_state_parts(
            &["object-select-symbolic", "checkbox-checked-symbolic"],
            "No messages selected",
            "Every action here applies to all of them. Escape leaves the select mode.",
            "sw-batch-page",
        );
        let row = classed(
            gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .halign(gtk::Align::Center)
                .max_children_per_line(4)
                .homogeneous(false)
                .build(),
            &["sw-batch-actions"],
        );
        let mut buttons = Vec::new();
        for id in BATCH_ACTIONS {
            let button = classed(
                gtk::Button::builder()
                    .label(id.title())
                    .action_name(id.detailed_name())
                    .build(),
                &["sw-batch-action"],
            );
            button.set_widget_name(&format!("batch-{}", id.name()));
            row.insert(&button, -1);
            buttons.push((id, button));
        }
        state.append(&row);
        title.set_widget_name("batch-title");
        Self {
            widget: centred(&state),
            title,
            buttons,
            bar: BatchBar::new(),
        }
    }

    /// "1 message selected", "3 messages selected"; with conversation rows "3 conversations
    /// selected". The header's bar says the same.
    pub fn set_count(&self, count: u32, conversations: bool) {
        let text = selected_text(count, conversations);
        self.title.set_label(&text);
        self.bar.count.set_label(&text);
    }

    /// A command's title here and in the header's bar (Not Junk, Unstar) and its tooltip.
    pub fn set_command(&self, id: ActionId, title: &str, tooltip: &str) {
        if let Some((_, button)) = self.buttons.iter().find(|(candidate, _)| *candidate == id) {
            button.set_label(title);
            button.set_tooltip_text(Some(tooltip));
        }
        self.bar.set_command(id, title, tooltip);
    }
}

/// "3 conversations selected", "1 message selected".
pub fn selected_text(count: u32, conversations: bool) -> String {
    let noun = match (count, conversations) {
        (1, false) => "message",
        (1, true) => "conversation",
        (_, false) => "messages",
        (_, true) => "conversations",
    };
    format!("{count} {noun} selected")
}

impl Default for BatchPage {
    fn default() -> Self {
        Self::new()
    }
}

/// A folder without messages, such as "Nothing archived yet"; the list changes its text for
/// filters and searches. Like Inbox Zero it can hold the keyboard focus (the Superhuman pass:
/// with no row left, the list's keys still work there), and says its text when it does.
pub fn folder(title: &str, description: &str) -> EmptyState {
    let (state, title, description) =
        empty_state_parts(&FOLDER_ICONS, title, description, "sw-empty-folder");
    state.set_focusable(true);
    state.set_accessible_role(gtk::AccessibleRole::Status);
    // The icon is the state's first child (`empty_state_parts`).
    let icon = state.first_child().and_downcast::<gtk::Image>();
    let empty = EmptyState {
        widget: centred(&state),
        state,
        icon,
        title,
        description,
    };
    empty.label();
    empty
}

/// A folder's empty state's icon.
pub const FOLDER_ICONS: [&str; 1] = ["folder-symbolic"];

/// An empty state whose text can change.
#[derive(Clone)]
pub struct EmptyState {
    widget: gtk::Widget,
    state: gtk::Box,
    icon: Option<gtk::Image>,
    title: gtk::Label,
    description: gtk::Label,
}

impl EmptyState {
    pub fn widget(&self) -> &gtk::Widget {
        &self.widget
    }

    pub fn title(&self) -> glib::GString {
        self.title.label()
    }

    pub fn description(&self) -> Option<glib::GString> {
        let text = self.description.label();
        (!text.is_empty()).then_some(text)
    }

    pub fn set_title(&self, title: &str) {
        self.title.set_label(title);
        self.label();
    }

    pub fn set_description(&self, description: Option<&str>) {
        self.description.set_label(description.unwrap_or_default());
        self.description.set_visible(description.is_some());
        self.label();
    }

    /// Shows the first of `icons` the theme has above the text: a folder's, or the Reminders
    /// view's clock (phase C).
    pub fn set_icons(&self, icons: &[&str]) {
        let Some(icon) = &self.icon else {
            return;
        };
        let shown = icon
            .gicon()
            .and_downcast::<gtk::gio::ThemedIcon>()
            .map(|themed| themed.names());
        if shown.is_some_and(|names| {
            names
                .iter()
                .map(|name| name.as_str())
                .eq(icons.iter().copied())
        }) {
            return;
        }
        icon.set_from_gicon(&gtk::gio::ThemedIcon::from_names(icons));
    }

    /// The first of the icons shown, for checks.
    pub fn icon_name(&self) -> Option<String> {
        self.icon
            .as_ref()?
            .gicon()
            .and_downcast::<gtk::gio::ThemedIcon>()?
            .names()
            .first()
            .map(|name| name.to_string())
    }

    /// The state's own widget, which holds the focus (`focus`), as the self-test names it.
    pub fn set_name(&self, name: &str) {
        self.state.set_widget_name(name);
    }

    /// Takes the keyboard focus; false when it isn't on screen.
    pub fn focus(&self) -> bool {
        self.state.is_mapped() && self.state.grab_focus()
    }

    /// "No messages. Messages in this folder appear here."
    fn label(&self) {
        let text = match self.description() {
            Some(description) => format!("{}. {description}", self.title()),
            None => format!("{}.", self.title()),
        };
        self.state
            .update_property(&[gtk::accessible::Property::Label(&text)]);
    }
}

fn empty_state(icons: &[&str], title: &str, description: &str, class: &str) -> gtk::Box {
    empty_state_parts(icons, title, description, class).0
}

fn empty_state_parts(
    icons: &[&str],
    title: &str,
    description: &str,
    class: &str,
) -> (gtk::Box, gtk::Label, gtk::Label) {
    let state = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
        &["sw-empty-state", class],
    );
    state.append(&classed(icon(icons), &["sw-empty-icon"]));
    let [title, description] =
        [(title, "sw-empty-title"), (description, "sw-empty-body")].map(|(text, class)| {
            classed(
                gtk::Label::builder()
                    .label(text)
                    .wrap(true)
                    .max_width_chars(48)
                    .justify(gtk::Justification::Center)
                    .build(),
                &[class],
            )
        });
    state.append(&title);
    state.append(&description);
    (state, title, description)
}

/// Scrolls when the window is too low for it; never a Tab stop of its own, which a screen
/// reader would find unnamed (its buttons, if any, still are).
fn centred(state: &gtk::Box) -> gtk::Widget {
    crate::accessibility::tab_through(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(state)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_zero_names_the_compose_key_in_use() {
        assert_eq!(compose_hint(&Keymap::default()), "Press c to write");
        let rebound = Keymap::with_overrides([(
            ActionId::Compose,
            vec![sealwax_domain::keymap::Binding::parse("<Control>m").unwrap()],
        )]);
        assert_eq!(compose_hint(&rebound), "Press Ctrl+M to write");
        let unbound = Keymap::with_overrides([(ActionId::Compose, Vec::new())]);
        assert_eq!(compose_hint(&unbound), "Nothing left to read");
    }

    #[test]
    fn the_select_mode_counts_rows_and_its_icons_follow_the_titles() {
        assert_eq!(selected_text(1, true), "1 conversation selected");
        assert_eq!(selected_text(3, false), "3 messages selected");
        assert_eq!(
            bar_icons(ActionId::ToggleRead, ActionId::MarkUnread.title()),
            ["mail-unread-symbolic"]
        );
        assert_eq!(
            bar_icons(ActionId::ToggleRead, ActionId::MarkRead.title()),
            ["mail-read-symbolic"]
        );
        assert_eq!(bar_icons(ActionId::Star, "Unstar"), ["starred-symbolic"]);
        assert_eq!(bar_icons(ActionId::Star, "Star")[0], "non-starred-symbolic");
        // Each of the bar's commands is one the app keeps a title and an availability for:
        // triage's, or Remind Me's (phase C).
        for id in BAR_ACTIONS {
            assert!(
                crate::triage::COMMANDS.contains(&id) || crate::remind::COMMANDS.contains(&id),
                "{id:?}"
            );
        }
        // One order everywhere (the visual QA): Done, then Remind Me, as the rows' quick
        // actions and the reader's toolbar have them.
        assert_eq!(BAR_ACTIONS[..2], [ActionId::Archive, ActionId::RemindMe]);
        assert_eq!(
            bar_icons(ActionId::RemindMe, "Remind Me…")[0],
            "alarm-symbolic"
        );
    }

    #[test]
    fn the_hint_names_the_palette_keys_in_use() {
        assert_eq!(
            palette_hint(&Keymap::default()),
            "or press Ctrl+K for every command"
        );
        let unbound = Keymap::with_overrides([(ActionId::CommandPalette, Vec::new())]);
        assert_eq!(
            palette_hint(&unbound),
            "or open the command palette for every command"
        );
    }
}
