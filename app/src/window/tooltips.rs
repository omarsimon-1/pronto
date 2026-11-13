//! Tooltips that name a command's keys (DESIGN.md "Accessibility": icon-only buttons have
//! tooltips with their shortcut). The keys come from the keymap in use, so a `keys.toml`
//! change shows up in every tooltip registered here.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use std::cell::RefCell;

struct Entry {
    widget: glib::WeakRef<gtk::Widget>,
    label: String,
    id: ActionId,
    /// A line above the command, such as the account the list bar's sidebar button points out.
    note: Option<String>,
}

impl Entry {
    fn text(&self, keymap: &Keymap) -> String {
        noted(
            self.note.as_deref(),
            with_keys(&self.label, &keymap.display(self.id)),
        )
    }
}

/// The widgets whose tooltips name a command's keys, and the keymap they show.
#[derive(Default)]
pub struct Tooltips {
    keymap: RefCell<Keymap>,
    entries: RefCell<Vec<Entry>>,
}

impl Tooltips {
    /// Gives `widget` the tooltip `label (keys of id)` now and after every keymap change.
    pub fn add(&self, widget: &impl IsA<gtk::Widget>, label: &str, id: ActionId) {
        let widget = widget.as_ref();
        widget.set_tooltip_text(Some(&with_keys(label, &self.keymap.borrow().display(id))));
        let mut entries = self.entries.borrow_mut();
        entries.retain(|entry| entry.widget.upgrade().is_some());
        entries.push(Entry {
            widget: widget.downgrade(),
            label: label.to_owned(),
            id,
            note: None,
        });
    }

    /// Puts `note` on a line above the command in `widget`'s tooltip (`None`: the command
    /// alone), so what a coloured mark means is in words too (DESIGN.md "Accessibility").
    pub fn set_note(&self, widget: &impl IsA<gtk::Widget>, note: Option<&str>) {
        let widget = widget.as_ref();
        let keymap = self.keymap.borrow();
        for entry in self.entries.borrow_mut().iter_mut() {
            // Unchanged, the tooltip is left alone: GTK asks the widget under the pointer for
            // its tooltip again on every change, and the sidebar's model comes often.
            if entry.widget.upgrade().as_ref() == Some(widget) && entry.note.as_deref() != note {
                entry.note = note.map(str::to_owned);
                widget.set_tooltip_text(Some(&entry.text(&keymap)));
            }
        }
    }

    pub fn set_keymap(&self, keymap: &Keymap) {
        self.keymap.replace(keymap.clone());
        self.entries
            .borrow_mut()
            .retain(|entry| match entry.widget.upgrade() {
                Some(widget) => {
                    widget.set_tooltip_text(Some(&entry.text(keymap)));
                    true
                }
                None => false,
            });
    }
}

/// `New message (c, Ctrl+N)`, or just the label when the command has no keys.
pub fn with_keys(label: &str, keys: &str) -> String {
    if keys.is_empty() {
        label.to_owned()
    } else {
        format!("{label} ({keys})")
    }
}

/// `note` on a line over `command`, or the command alone.
fn noted(note: Option<&str>, command: String) -> String {
    match note {
        Some(note) => format!("{note}\n{command}"),
        None => command,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::keymap::Binding;

    #[test]
    fn tooltips_name_the_keys_in_use() {
        let keymap = Keymap::default();
        assert_eq!(
            with_keys("New message", &keymap.display(ActionId::Compose)),
            "New message (c, Ctrl+N)"
        );
        assert_eq!(
            with_keys("Open config.toml", &keymap.display(ActionId::Preferences)),
            "Open config.toml (Ctrl+,)"
        );
        let changed = Keymap::with_overrides([(
            ActionId::Compose,
            vec![Binding::parse("<Control>m").unwrap()],
        )]);
        assert_eq!(
            with_keys("New message", &changed.display(ActionId::Compose)),
            "New message (Ctrl+M)"
        );
        let unbound = Keymap::with_overrides([(ActionId::Compose, Vec::new())]);
        assert_eq!(
            with_keys("New message", &unbound.display(ActionId::Compose)),
            "New message"
        );
    }

    #[test]
    fn a_note_goes_over_the_command() {
        let keymap = Keymap::default();
        let command = || with_keys("Toggle Sidebar", &keymap.display(ActionId::ToggleSidebar));
        assert_eq!(noted(None, command()), "Toggle Sidebar (Ctrl+Shift+S)");
        assert_eq!(
            noted(Some("Work: offline"), command()),
            "Work: offline\nToggle Sidebar (Ctrl+Shift+S)"
        );
    }
}
