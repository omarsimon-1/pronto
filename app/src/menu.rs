//! The primary menu, generated from the registry: one section per `MenuSection`, the actions
//! in registry order. Rebuild it when `keys.toml` changes.

use gtk4::gio;
use gtk4::prelude::*;
use sealwax_domain::actions::{ActionId, MenuSection};
use sealwax_domain::keymap::Keymap;

/// The primary menu. GTK shows an action's first application accelerator by itself; an action
/// with scoped keys only shows its first single key, and one whose first key yields to the
/// focused widget shows that one (the palette's Ctrl+K). Items of disabled actions are hidden,
/// so commands that aren't built yet stay out of the menu.
pub fn primary_menu(keymap: &Keymap) -> gio::Menu {
    let menu = gio::Menu::new();
    for section in MenuSection::ALL {
        let items = gio::Menu::new();
        for id in section_order(section) {
            items.append_item(&item(id, keymap));
        }
        if items.n_items() > 0 {
            menu.append_section(None, &items);
        }
    }
    menu
}

/// Commands the menu shows right after another of their section rather than in registry order
/// (a later block of the registry, kept together there for merges): the Split Inbox toggle
/// with the other layout toggles (the fork's phase C).
const PLACED_AFTER: [(ActionId, ActionId); 1] =
    [(ActionId::SplitInbox, ActionId::GroupByConversation)];

/// `section`'s commands in registry order, with [`PLACED_AFTER`]'s moved after their anchors.
fn section_order(section: MenuSection) -> Vec<ActionId> {
    let mut ids: Vec<ActionId> = ActionId::ALL
        .into_iter()
        .filter(|id| id.spec().menu == Some(section))
        .collect();
    for (placed, anchor) in PLACED_AFTER {
        if let (Some(from), true) = (
            ids.iter().position(|id| *id == placed),
            ids.contains(&anchor),
        ) {
            ids.remove(from);
            let to = ids
                .iter()
                .position(|id| *id == anchor)
                .map_or(ids.len(), |at| at + 1);
            ids.insert(to, placed);
        }
    }
    ids
}

fn item(id: ActionId, keymap: &Keymap) -> gio::MenuItem {
    let item = gio::MenuItem::new(
        Some(&id.title().replace('_', "__")),
        Some(&id.detailed_name()),
    );
    let yielding = keymap
        .yielding(id)
        .into_iter()
        .find_map(|binding| binding.single().cloned());
    let scoped = || {
        keymap
            .scoped(id)
            .iter()
            .find_map(|binding| binding.single().cloned())
    };
    if let Some(key) = yielding.or_else(|| keymap.accels(id).is_empty().then(scoped).flatten()) {
        item.set_attribute_value("accel", Some(&key.canonical().to_variant()));
    }
    item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
    item
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::keymap::Binding;

    fn attribute(menu: &gio::MenuModel, index: i32, name: &str) -> Option<String> {
        menu.item_attribute_value(index, name, None)
            .and_then(|value| value.get::<String>())
    }

    #[test]
    fn sections_follow_the_registry() {
        let menu = primary_menu(&Keymap::default());
        assert_eq!(menu.n_items(), 4);
        let actions: Vec<Vec<String>> = (0..menu.n_items())
            .map(|section| {
                let items = menu.item_link(section, "section").unwrap();
                (0..items.n_items())
                    .map(|index| attribute(&items, index, "action").unwrap())
                    .collect()
            })
            .collect();
        assert_eq!(
            actions,
            [
                vec![
                    "win.refresh",
                    "win.toggle-sidebar",
                    "win.toggle-reading-pane",
                    "win.group-by-conversation",
                    "win.split-inbox",
                    "win.command-palette",
                ],
                vec![
                    "win.add-account",
                    "win.edit-account",
                    "win.remove-account",
                    "app.shortcuts",
                    "app.preferences",
                    "app.customize-shortcuts",
                    "app.start-at-login",
                    "win.edit-rules",
                ],
                vec!["app.check-for-updates"],
                vec!["app.quit"],
            ]
        );
        let settings = menu.item_link(1, "section").unwrap();
        assert_eq!(
            attribute(&settings, 0, "label").as_deref(),
            Some("Add Account")
        );
        assert_eq!(
            attribute(&settings, 4, "label").as_deref(),
            Some("Preferences (config.toml)")
        );
        assert_eq!(
            attribute(&settings, 0, "hidden-when").as_deref(),
            Some("action-disabled")
        );
        // Accelerators show by themselves; nothing here sets an accel attribute.
        assert_eq!(attribute(&settings, 1, "accel"), None);
        // The palette's Ctrl+K yields: it is no accelerator, so the menu is told it.
        let view = menu.item_link(0, "section").unwrap();
        assert_eq!(attribute(&view, 5, "accel").as_deref(), Some("<Control>k"));
        assert_eq!(attribute(&view, 0, "accel"), None);
    }

    #[test]
    fn scoped_keys_show_when_there_is_no_accelerator() {
        let keymap = Keymap::with_overrides([(
            ActionId::KeyboardShortcuts,
            vec![
                Binding::parse("question").unwrap(),
                Binding::parse("h").unwrap(),
            ],
        )]);
        let menu = primary_menu(&keymap);
        let settings = menu.item_link(1, "section").unwrap();
        assert_eq!(
            attribute(&settings, 3, "accel").as_deref(),
            Some("question")
        );
    }
}
