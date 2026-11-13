//! The reader's toolbar (DESIGN.md "Reader"; the fork's Superhuman pass): Archive (Done, the
//! check mark), Remind Me (the clock, phase C), Delete, Move to and Star on the bar, in the
//! list's quick actions' and batch bar's order, bound to the registry's window actions,
//! which are sensitive where they apply to the selected messages (M4), then More, then the
//! position "n of N" with previous and next. Each tooltip names the command, its keys, and why
//! it doesn't apply when it doesn't; Star's icon is the outline until the selection is starred.
//! More holds the read toggle (Mark as Read or Mark as Unread, `u`), Move to Junk (Not Junk in a
//! Junk folder) and Print, whose titles follow the selection and which show their key, then
//! M5's Show Original, Save as .eml, Expand All and Collapse All Messages, Show Images and
//! Always Show Images from This Sender.
//!
//! The buttons are as high as wide, centred in the bar at every width (phase C: in a 710 px
//! window they filled the bar's height, a hovered one a block hanging from the window's top), and
//! previous and next keep to the column's end whether "n of N" shows or not (a spacer of their
//! own; in select mode they sat right after More).

use crate::window::glyph;
use crate::window::panes::icon_button;
use gtk4 as gtk;
use gtk4::gio;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// The message commands on the bar, in order, each with an icon Adwaita has (no fallback that
/// draws something else: `mail-archive-symbolic` isn't in Adwaita, a blank page showed). Star's
/// is the outline while the selection isn't starred ([`star_icon`]).
const ACTIONS: [(ActionId, &[&str]); 5] = [
    (ActionId::Archive, &["object-select-symbolic"]),
    (ActionId::RemindMe, &crate::list::row::REMIND_ICONS),
    (ActionId::Delete, &["user-trash-symbolic"]),
    (ActionId::MoveTo, &["folder-symbolic"]),
    (ActionId::Star, &["non-starred-symbolic"]),
];

/// The message commands in More, whose titles follow the selection: the read toggle says what
/// `u` would do (Mark as Unread on a read message), Junk is Not Junk in a Junk folder.
const MORE_COMMANDS: [ActionId; 3] = [ActionId::ToggleRead, ActionId::Junk, ActionId::Print];

/// Star's icon: the filled star, in the star colour (style.css), while the selection is
/// starred (the command would unstar it); the outline otherwise, as the list's quick action
/// shows it, drawn at full strength ([`glyph`]: Adwaita's own outline paints at 35 %, so an
/// unstarred Star read as disabled, 2.4:1 against its neighbours' 10:1 in Adwaita dark).
pub fn star_icon(starred: bool) -> &'static str {
    if starred {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    }
}

/// What More shows beside a command: its first one-step key as GTK writes an accelerator
/// (`u`, `<Control>p`); a command reached only by a sequence shows none. GTK draws a letter's
/// accelerator in capitals ("U", which reads as Shift+U, the Unread filter): [`more_button`]
/// writes a lone letter as the key is typed again ([`accel_label`]).
pub fn menu_accel(keymap: &Keymap, id: ActionId) -> Option<String> {
    keymap
        .shown(id)
        .iter()
        .find_map(|binding| binding.single().map(|key| key.canonical()))
}

/// More's other sections (the approved mock's "Show original, Save as .eml", then M5's
/// conversation and remote-content commands).
const MORE: [&[ActionId]; 4] = [
    &[ActionId::ShowOriginal, ActionId::SaveMessage],
    &[ActionId::ExpandAll, ActionId::CollapseAll],
    &[ActionId::ShowImages, ActionId::AlwaysShowImages],
    // The conversation's sender's side of the Split Inbox (the fork's phase C).
    &SPLIT_COMMANDS,
];

/// More's Split Inbox commands, which it leaves out while they don't apply rather than show
/// greyed: only an Inbox's conversation has a side, with the splits on, and Reset Sender Split
/// applies only while the sender has a choice.
const SPLIT_COMMANDS: [ActionId; 3] = [
    ActionId::AlwaysImportant,
    ActionId::AlwaysOther,
    ActionId::ResetSenderSplit,
];

/// Where the shown message is in its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// 0 for the first message.
    pub index: u32,
    pub total: u32,
}

type Callbacks = Rc<RefCell<Vec<Box<dyn Fn()>>>>;

pub struct Toolbar {
    root: gtk::Box,
    buttons: Vec<(ActionId, gtk::Button)>,
    /// More's first section: [`MORE_COMMANDS`] under their current titles.
    commands_menu: gio::Menu,
    more: gtk::MenuButton,
    position: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    at: Cell<Option<Position>>,
    /// Whether Star shows the filled star, once set: an unchanged state leaves the icon alone
    /// (it runs on every selection and store change, and a new icon queues a resize).
    starred: Cell<Option<bool>>,
    on_previous: Callbacks,
    on_next: Callbacks,
    keymap: RefCell<Keymap>,
    /// Each command's title here and why it doesn't apply, when it doesn't.
    commands: RefCell<HashMap<ActionId, (String, Option<String>)>>,
}

impl Toolbar {
    pub fn new(keymap: &Keymap) -> Rc<Self> {
        let root = gtk::Box::builder()
            .spacing(2)
            .hexpand(true)
            .css_classes(["sw-reader-toolbar"])
            .build();
        root.set_widget_name("reader-actions");
        let mut buttons = Vec::new();
        for (id, icons) in ACTIONS {
            let button = icon_button(icons, id.title());
            button.add_css_class("sw-icon-button");
            button.set_valign(gtk::Align::Center);
            button.set_action_name(Some(&id.detailed_name()));
            button.set_widget_name(&format!("reader-{}", id.name()));
            if let Some(image) = button.child().and_downcast::<gtk::Image>() {
                glyph::set_icon(&image, icons);
            }
            root.append(&button);
            buttons.push((id, button));
        }
        let commands_menu = gio::Menu::new();
        let more = more_button(&commands_menu);
        root.append(&more);
        // Previous and next keep to the bar's end whether or not "n of N" shows.
        root.append(&gtk::Box::builder().hexpand(true).build());
        let position = gtk::Label::builder()
            .xalign(1.0)
            .margin_end(6)
            .css_classes(["sw-secondary", "numeric"])
            .visible(false)
            .build();
        position.set_widget_name("reader-position");
        root.append(&position);
        let previous = icon_button(&["go-up-symbolic"], "Previous conversation");
        let next = icon_button(&["go-down-symbolic"], "Next conversation");
        previous.set_widget_name("reader-previous");
        next.set_widget_name("reader-next");
        for button in [&previous, &next] {
            button.add_css_class("sw-icon-button");
            button.set_valign(gtk::Align::Center);
            button.set_sensitive(false);
            root.append(button);
        }
        let toolbar = Rc::new(Self {
            root,
            buttons,
            commands_menu,
            more,
            position,
            previous,
            next,
            at: Cell::new(None),
            starred: Cell::new(Some(false)),
            on_previous: Callbacks::default(),
            on_next: Callbacks::default(),
            keymap: RefCell::new(keymap.clone()),
            commands: RefCell::new(HashMap::new()),
        });
        for (button, callbacks) in [
            (&toolbar.previous, toolbar.on_previous.clone()),
            (&toolbar.next, toolbar.on_next.clone()),
        ] {
            button.connect_clicked(move |_| {
                for callback in callbacks.borrow().iter() {
                    callback();
                }
            });
        }
        toolbar.update_keys(keymap);
        toolbar.refresh_menu();
        toolbar
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Tooltips name each command's keys (DESIGN.md "Accessibility"); call again when
    /// `keys.toml` changes.
    pub fn update_keys(&self, keymap: &Keymap) {
        self.keymap.replace(keymap.clone());
        for (id, _) in &self.buttons {
            self.refresh_command(*id);
        }
        self.refresh_menu();
        self.previous.set_tooltip_text(Some(&tooltip(
            keymap,
            ActionId::PreviousMessage,
            "Previous conversation",
            None,
        )));
        self.next.set_tooltip_text(Some(&tooltip(
            keymap,
            ActionId::NextMessage,
            "Next conversation",
            None,
        )));
    }

    /// A command's title for the selected messages (Not Junk, Unstar) and why it doesn't apply.
    pub fn set_command(&self, id: ActionId, title: &str, reason: Option<&str>) {
        let new = (title.to_owned(), reason.map(str::to_owned));
        let old = self.commands.borrow_mut().insert(id, new.clone());
        if old.as_ref() != Some(&new) {
            if MORE_COMMANDS.contains(&id) && old.map(|(title, _)| title) != Some(new.0) {
                self.refresh_menu();
            }
            self.refresh_command(id);
        }
    }

    /// The title More shows for `id`, as the selection names it.
    pub fn more_title(&self, id: ActionId) -> Option<String> {
        self.more_attribute(id, "label")
    }

    /// The key More shows beside `id` (`u`), as GTK writes an accelerator.
    pub fn more_key(&self, id: ActionId) -> Option<String> {
        self.more_attribute(id, "accel")
    }

    /// The key drawn beside `id`'s row in More ("u", "Ctrl+P"): the popover's rows are made with
    /// its menu, before it opens.
    pub fn more_key_shown(&self, id: ActionId) -> Option<String> {
        let title = self.more_title(id)?;
        let popover = self.more.popover()?;
        let mut rows = Vec::new();
        descendants(popover.upcast_ref(), &mut rows);
        let row = rows.into_iter().find(|widget| {
            widget.css_name() == "modelbutton"
                && widget.property::<Option<String>>("text").as_deref() == Some(title.as_str())
        })?;
        let mut parts = Vec::new();
        descendants(&row, &mut parts);
        parts
            .into_iter()
            .find(|widget| widget.css_name() == "accelerator")
            .and_downcast::<gtk::Label>()
            .map(|label| label.label().to_string())
    }

    fn more_attribute(&self, id: ActionId, attribute: &str) -> Option<String> {
        let menu = &self.commands_menu;
        (0..menu.n_items()).find_map(|index| {
            let action = menu.item_attribute_value(index, "action", None)?;
            (action.str() == Some(id.detailed_name().as_str()))
                .then(|| menu.item_attribute_value(index, attribute, None))
                .flatten()
                .and_then(|value| value.str().map(str::to_owned))
        })
    }

    /// More's first section again, under the commands' current titles, with their keys.
    fn refresh_menu(&self) {
        let commands = self.commands.borrow();
        let keymap = self.keymap.borrow();
        self.commands_menu.remove_all();
        for id in MORE_COMMANDS {
            let title = commands
                .get(&id)
                .map_or(id.title(), |(title, _)| title.as_str());
            let item = gio::MenuItem::new(Some(title), Some(&id.detailed_name()));
            if let Some(accel) = menu_accel(&keymap, id) {
                item.set_attribute_value("accel", Some(&accel.to_variant()));
            }
            self.commands_menu.append_item(&item);
        }
        // The popover made its rows again, their keys in GTK's capitals.
        if let Some(popover) = self.more.popover() {
            write_keys(popover.upcast_ref());
        }
    }

    fn refresh_command(&self, id: ActionId) {
        let Some((_, button)) = self.buttons.iter().find(|(candidate, _)| *candidate == id) else {
            return;
        };
        let commands = self.commands.borrow();
        let (title, reason) = commands
            .get(&id)
            .map_or((id.title(), None), |(title, reason)| {
                (title.as_str(), reason.as_deref())
            });
        let text = tooltip(&self.keymap.borrow(), id, title, reason);
        button.set_tooltip_text(Some(&text));
        button.update_property(&[gtk::accessible::Property::Label(title)]);
    }

    /// Whether the selection is starred: Star shows the filled star in the star colour, else
    /// the outline ([`star_icon`]).
    pub fn set_starred(&self, starred: bool) {
        if self.starred.replace(Some(starred)) == Some(starred) {
            return;
        }
        let Some((_, button)) = self
            .buttons
            .iter()
            .find(|(candidate, _)| *candidate == ActionId::Star)
        else {
            return;
        };
        if let Some(image) = button.child().and_downcast::<gtk::Image>() {
            glyph::set_icon(&image, &[star_icon(starred)]);
        }
        if starred {
            button.add_css_class("sw-starred");
        } else {
            button.remove_css_class("sw-starred");
        }
    }

    /// Shows "n of N", or nothing; previous and next follow it.
    pub fn set_position(&self, position: Option<Position>) {
        self.at.set(position);
        match position {
            Some(Position { index, total }) => {
                self.position.set_text(&format!("{} of {total}", index + 1));
                self.position.set_visible(true);
            }
            None => self.position.set_visible(false),
        }
        self.refresh();
    }

    pub fn connect_previous(&self, callback: impl Fn() + 'static) {
        self.on_previous.borrow_mut().push(Box::new(callback));
        self.refresh();
    }

    pub fn connect_next(&self, callback: impl Fn() + 'static) {
        self.on_next.borrow_mut().push(Box::new(callback));
        self.refresh();
    }

    fn refresh(&self) {
        let (has_previous, has_next) = match self.at.get() {
            Some(Position { index, total }) => (index > 0, index + 1 < total),
            None => (true, true),
        };
        self.previous
            .set_sensitive(has_previous && !self.on_previous.borrow().is_empty());
        self.next
            .set_sensitive(has_next && !self.on_next.borrow().is_empty());
    }
}

/// The More menu, bound to the registry's window actions: `commands` first (the toolbar fills
/// it), then [`MORE`].
fn more_button(commands: &gio::Menu) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("More")
        .menu_model(&more_menu(commands))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "sw-icon-button"])
        .build();
    button.set_widget_name("reader-more");
    button.update_property(&[gtk::accessible::Property::Label("More")]);
    // The rows are made with the menu, and again when a title changes (`refresh_menu` writes
    // their keys then); opening writes them once more, should GTK have made them again.
    if let Some(popover) = button.popover() {
        popover.connect_map(|popover| write_keys(popover.upcast_ref()));
    }
    button
}

/// The More menu's model: `commands`, then [`MORE`]'s sections.
fn more_menu(commands: &gio::Menu) -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append_section(None, commands);
    for section in MORE {
        let items = gio::Menu::new();
        for id in section {
            let item = gio::MenuItem::new(Some(id.title()), Some(&id.detailed_name()));
            if SPLIT_COMMANDS.contains(id) {
                item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
            }
            items.append_item(&item);
        }
        menu.append_section(None, &items);
    }
    menu
}

/// The keys `widget`'s menu rows show, as typed: GTK writes an accelerator's letter in capitals.
fn write_keys(widget: &gtk::Widget) {
    let mut all = Vec::new();
    descendants(widget, &mut all);
    for widget in all {
        if widget.css_name() == "accelerator"
            && let Some(label) = widget.downcast_ref::<gtk::Label>()
            && let Some(text) = accel_label(&label.label())
        {
            label.set_label(&text);
        }
    }
}

/// Every widget under `widget`, depth first.
fn descendants(widget: &gtk::Widget, into: &mut Vec<gtk::Widget>) {
    let mut child = widget.first_child();
    while let Some(widget) = child {
        into.push(widget.clone());
        descendants(&widget, into);
        child = widget.next_sibling();
    }
}

/// A lone letter as it is typed ("U" is `u`; Shift+U would say so); `None` for any other key.
pub fn accel_label(shown: &str) -> Option<String> {
    let mut chars = shown.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), None) if letter.is_ascii_uppercase() => {
            Some(letter.to_ascii_lowercase().to_string())
        }
        _ => None,
    }
}

/// "Archive (e, y)", and why it doesn't apply: "Archive (e, y) — already archived".
pub fn tooltip(keymap: &Keymap, id: ActionId, title: &str, reason: Option<&str>) -> String {
    let keys = keymap.display(id);
    let named = if keys.is_empty() {
        title.to_owned()
    } else {
        format!("{title} ({keys})")
    };
    match reason {
        Some(reason) => format!("{named} — {reason}"),
        None => named,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_keeps_five_commands_and_more_the_rest() {
        let bar: Vec<ActionId> = ACTIONS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            bar,
            [
                ActionId::Archive,
                ActionId::RemindMe,
                ActionId::Delete,
                ActionId::MoveTo,
                ActionId::Star
            ]
        );
        assert_eq!(
            MORE_COMMANDS,
            [ActionId::ToggleRead, ActionId::Junk, ActionId::Print]
        );
        let more: Vec<ActionId> = MORE
            .iter()
            .flat_map(|section| section.iter())
            .copied()
            .collect();
        for id in MORE_COMMANDS.iter().chain(&more) {
            assert!(!bar.contains(id), "{id:?} is on the bar and in More");
        }
        // One icon each, so a theme without it shows nothing rather than another glyph.
        assert!(ACTIONS.iter().all(|(_, icons)| icons.len() == 1));
    }

    #[test]
    fn more_leaves_out_the_split_inbox_commands_that_dont_apply() {
        let menu = more_menu(&gio::Menu::new());
        let mut hidden = Vec::new();
        let mut shown = Vec::new();
        for section in 1..menu.n_items() {
            let items = menu
                .item_link(section, gio::MENU_LINK_SECTION)
                .expect("a section");
            for index in 0..items.n_items() {
                let action = items
                    .item_attribute_value(index, gio::MENU_ATTRIBUTE_ACTION, None)
                    .and_then(|value| value.get::<String>())
                    .expect("an action");
                let when = items
                    .item_attribute_value(index, "hidden-when", None)
                    .and_then(|value| value.get::<String>());
                match when.as_deref() {
                    Some("action-disabled") => hidden.push(action),
                    Some(other) => panic!("{action} hides when {other}"),
                    None => shown.push(action),
                }
            }
        }
        assert_eq!(
            hidden,
            [
                "win.always-important",
                "win.always-other",
                "win.reset-sender-split"
            ]
        );
        // The others show greyed while they don't apply, as before.
        assert!(shown.contains(&"win.show-original".to_owned()));
    }

    #[test]
    fn star_shows_the_outline_until_the_selection_is_starred() {
        assert_eq!(star_icon(false), "non-starred-symbolic");
        assert_eq!(star_icon(true), "starred-symbolic");
        assert_eq!(ACTIONS[4].1, [star_icon(false)], "the bar starts unstarred");
    }

    #[test]
    fn a_lone_letter_shows_as_it_is_typed() {
        assert_eq!(accel_label("U").as_deref(), Some("u"));
        assert_eq!(accel_label("u"), None, "already as typed");
        for shown in ["Ctrl+P", "Shift+U", "!", "Delete", "F1", ""] {
            assert_eq!(accel_label(shown), None, "{shown}");
        }
    }

    #[test]
    fn more_shows_the_read_toggles_key() {
        let keymap = Keymap::default();
        assert_eq!(
            menu_accel(&keymap, ActionId::ToggleRead).as_deref(),
            Some("u")
        );
        assert_eq!(
            menu_accel(&keymap, ActionId::Print).as_deref(),
            Some("<Control>p")
        );
        assert!(MORE_COMMANDS.contains(&ActionId::ToggleRead));
        assert!(!MORE_COMMANDS.contains(&ActionId::MarkUnread));
    }

    #[test]
    fn tooltips_name_the_keys_and_the_reason() {
        let keymap = Keymap::default();
        assert_eq!(
            tooltip(&keymap, ActionId::Archive, "Archive", None),
            "Archive (e, y)"
        );
        assert_eq!(
            tooltip(&keymap, ActionId::Junk, "Not Junk", None),
            "Not Junk (!, Ctrl+J)"
        );
        assert_eq!(
            tooltip(
                &keymap,
                ActionId::Archive,
                "Archive",
                Some("already archived")
            ),
            "Archive (e, y) — already archived"
        );
        assert_eq!(
            tooltip(
                &keymap,
                ActionId::MarkFolderRead,
                "Mark Folder as Read",
                None
            ),
            "Mark Folder as Read"
        );
    }
}
