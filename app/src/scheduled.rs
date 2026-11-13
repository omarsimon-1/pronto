//! Scheduled Messages (the fork's phase B): the outbox the window shows. The sends Send Later
//! queued, soonest first, each with when it goes, its subject and whom it goes to, and Edit (back
//! to a draft, opened in a composer card, or under its conversation when it was written there
//! this session) and Send Now (it goes after the undo window, as Send does). Read from the store
//! when the dialog opens and again whenever the outbox changes. Nothing here logs a subject or an
//! address.

use crate::actions::ActionSet;
use crate::application::AppState;
use crate::outbox;
use crate::window::panes::{classed, named};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::{Address, DraftId};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const TITLE: &str = "Scheduled Messages";
pub const EMPTY_TITLE: &str = "Nothing scheduled";
pub const EMPTY_TEXT: &str =
    "Send Later (Ctrl+Shift+L in a draft) keeps a message here until its time.";

/// One scheduled send as the dialog shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scheduled {
    pub draft: DraftId,
    /// When it goes, Unix ms.
    pub at: i64,
    pub subject: String,
    pub to: Vec<Address>,
}

/// The dialog on screen: its list and the stack that shows the list or the empty page.
struct Shown {
    dialog: glib::WeakRef<adw::Dialog>,
    list: glib::WeakRef<gtk::ListBox>,
    pages: glib::WeakRef<gtk::Stack>,
}

thread_local! {
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    /// The store's outbox changes refill the dialog: one listener for the process.
    static LISTENING: Cell<bool> = const { Cell::new(false) };
    /// The sends it showed last, for checks.
    static LAST: RefCell<Vec<Scheduled>> = const { RefCell::new(Vec::new()) };
}

/// Connects Scheduled Messages to the window's action.
pub fn install(state: &Rc<AppState>, actions: &ActionSet) {
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::ShowScheduled, move || {
        if let Some(state) = weak.upgrade() {
            present(&state);
        }
    });
    if LISTENING.with(|listening| listening.replace(true)) {
        return;
    }
    let weak = Rc::downgrade(state);
    state.mailbox().connect_event(move |event| {
        if event.outbox.is_empty() {
            return;
        }
        if let Some(state) = weak.upgrade() {
            refresh(&state);
        }
    });
}

/// Reads the sends again when the dialog shows (the outbox changed, or a send's time is near:
/// it is no longer listed).
pub fn refresh(state: &Rc<AppState>) {
    let shown = SHOWN.with(|shown| {
        shown
            .borrow()
            .as_ref()
            .is_some_and(|shown| shown.dialog.upgrade().is_some())
    });
    if shown {
        refill(state);
    }
}

/// The sends shown last, soonest first.
pub fn shown() -> Vec<Scheduled> {
    LAST.with(|last| last.borrow().clone())
}

/// Opens the dialog over the window.
pub fn present(state: &Rc<AppState>) {
    let Some(window) = state.window() else {
        return;
    };
    let list = classed(
        gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .valign(gtk::Align::Start)
            .build(),
        &["boxed-list", "sw-scheduled-list"],
    );
    list.update_property(&[gtk::accessible::Property::Label(TITLE)]);
    let clamp = adw::Clamp::builder().maximum_size(640).child(&list).build();
    clamp.add_css_class("sw-scheduled");
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    let empty = adw::StatusPage::builder()
        .icon_name("document-open-recent-symbolic")
        .title(EMPTY_TITLE)
        .description(EMPTY_TEXT)
        .build();
    empty.add_css_class("compact");
    let pages = gtk::Stack::new();
    pages.add_named(&scrolled, Some("list"));
    pages.add_named(&empty, Some("empty"));
    pages.set_visible_child_name("empty");
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&pages));
    let dialog = adw::Dialog::builder()
        .title(TITLE)
        .content_width(560)
        .content_height(440)
        .child(&view)
        .build();
    dialog.set_widget_name("scheduled-messages");
    SHOWN.with(|shown| {
        shown.replace(Some(Shown {
            dialog: dialog.downgrade(),
            list: list.downgrade(),
            pages: pages.downgrade(),
        }))
    });
    dialog.present(Some(window.window()));
    refill(state);
}

/// Reads the scheduled sends from the store and shows them.
fn refill(state: &Rc<AppState>) {
    let Some(store) = state.mailbox().shared_store() else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let Ok(entries) = store.outbox(None).recv().await else {
            tracing::warn!("the scheduled sends couldn't be read");
            return;
        };
        let now = glib::real_time() / 1000;
        let mut sends = Vec::new();
        for entry in entries
            .into_iter()
            .filter(|entry| outbox::is_scheduled(entry, now))
        {
            let to = match store.draft(entry.draft).recv().await {
                Ok(Some(stored)) => stored.draft.to,
                _ => Vec::new(),
            };
            sends.push(Scheduled {
                draft: entry.draft,
                at: entry.not_before,
                subject: entry.subject,
                to,
            });
        }
        sends.sort_by_key(|send| send.at);
        if let Some(state) = weak.upgrade() {
            show(&state, sends);
        }
    });
}

/// "Sending tomorrow 08:00 · to Tomas Berg and 2 others" (`when` says when it goes, as the
/// send's toast and its copy's row in Drafts do since the fork's phase C).
pub fn subtitle(when: &str, to: &[Address]) -> String {
    match to.first() {
        None => when.to_owned(),
        Some(first) => {
            let name = first.display_name();
            match to.len() - 1 {
                0 => format!("{when} · to {name}"),
                1 => format!("{when} · to {name} and 1 other"),
                rest => format!("{when} · to {name} and {rest} others"),
            }
        }
    }
}

/// The button that has the keyboard focus in the list, as which send's and which of its
/// buttons (`scheduled-edit-2`: Edit of the third send shown).
fn focused_button(list: &gtk::ListBox, shown: &[Scheduled]) -> Option<(DraftId, &'static str)> {
    let window = list.root().and_downcast::<gtk::Window>()?;
    let focus = gtk::prelude::GtkWindowExt::focus(&window)?;
    let name = std::iter::successors(Some(focus), gtk::Widget::parent)
        .take_while(|part| part != list.upcast_ref::<gtk::Widget>())
        .map(|part| part.widget_name().to_string())
        .find(|name| {
            name.starts_with("scheduled-edit-") || name.starts_with("scheduled-send-now-")
        })?;
    let (kind, index) = name.rsplit_once('-')?;
    let kind = if kind == "scheduled-edit" {
        "scheduled-edit"
    } else {
        "scheduled-send-now"
    };
    Some((shown.get(index.parse::<usize>().ok()?)?.draft, kind))
}

fn show(state: &Rc<AppState>, sends: Vec<Scheduled>) {
    let before = LAST.with(|last| last.replace(sends.clone()));
    let parts = SHOWN.with(|shown| {
        shown
            .borrow()
            .as_ref()
            .and_then(|shown| Some((shown.list.upgrade()?, shown.pages.upgrade()?)))
    });
    let Some((list, pages)) = parts else {
        return;
    };
    // The same sends: the rows stay, and the keyboard focus on one of their buttons.
    if before == sends && (sends.is_empty() || list.first_child().is_some()) {
        return;
    }
    let focused = focused_button(&list, &before);
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    pages.set_visible_child_name(if sends.is_empty() { "empty" } else { "list" });
    // The focus goes back to the same button of the same send, else of the send now in its
    // place; with none left, where the dialog puts it.
    let refocus = focused.filter(|_| !sends.is_empty()).map(|(draft, kind)| {
        let at = before
            .iter()
            .position(|send| send.draft == draft)
            .unwrap_or(0);
        let index = sends
            .iter()
            .position(|send| send.draft == draft)
            .unwrap_or_else(|| at.min(sends.len() - 1));
        format!("{kind}-{index}")
    });
    for (index, send) in sends.into_iter().enumerate() {
        let subject = send.subject.trim();
        let title = if subject.is_empty() {
            "No subject".to_owned()
        } else {
            subject.to_owned()
        };
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&title).as_str())
            .subtitle(
                glib::markup_escape_text(&subtitle(
                    &crate::outbox::scheduled_text(&crate::time_picker::describe(send.at)),
                    &send.to,
                ))
                .as_str(),
            )
            .build();
        row.set_widget_name(&format!("scheduled-row-{index}"));
        let edit = named(
            classed(
                gtk::Button::builder()
                    .label("Edit")
                    .valign(gtk::Align::Center)
                    .tooltip_text("Cancel the schedule and open the draft")
                    .build(),
                &["flat"],
            ),
            &format!("scheduled-edit-{index}"),
        );
        let now = named(
            classed(
                gtk::Button::builder()
                    .label("Send Now")
                    .valign(gtk::Align::Center)
                    .build(),
                &["flat"],
            ),
            &format!("scheduled-send-now-{index}"),
        );
        let draft = send.draft;
        let weak = Rc::downgrade(state);
        edit.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                close();
                outbox::unschedule(&state, draft);
            }
        });
        let weak = Rc::downgrade(state);
        now.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                outbox::send_now(&state, draft);
            }
        });
        // A screen reader hears which send each button is for: the name says it, in place of
        // the button's own label (which names a button through a relation GTK sets).
        for (button, name) in [
            (&edit, format!("Edit “{title}”")),
            (&now, format!("Send “{title}” now")),
        ] {
            button.reset_relation(gtk::AccessibleRelation::LabelledBy);
            button.update_property(&[gtk::accessible::Property::Label(&name)]);
        }
        row.add_suffix(&edit);
        row.add_suffix(&now);
        list.append(&row);
    }
    if let Some(button) = refocus.and_then(|name| {
        std::iter::successors(list.first_child(), gtk::Widget::next_sibling)
            .find_map(|row| find_named(&row, &name))
    }) {
        button.grab_focus();
    }
}

/// The descendant of `widget` (itself included) named `name`.
fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    std::iter::successors(widget.first_child(), gtk::Widget::next_sibling)
        .find_map(|child| find_named(&child, name))
}

/// Closes the dialog, when it shows.
pub fn close() {
    let dialog = SHOWN.with(|shown| {
        shown
            .borrow()
            .as_ref()
            .and_then(|shown| shown.dialog.upgrade())
    });
    if let Some(dialog) = dialog {
        dialog.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_says_when_and_to_whom() {
        let to = |names: &[&str]| -> Vec<Address> {
            names
                .iter()
                .map(|name| Address::new(*name, "x@example.test"))
                .collect()
        };
        assert_eq!(
            subtitle("Sending tomorrow 08:00", &[]),
            "Sending tomorrow 08:00"
        );
        assert_eq!(
            subtitle("Sending Tue 08:00", &to(&["Tomas Berg"])),
            "Sending Tue 08:00 · to Tomas Berg"
        );
        assert_eq!(
            subtitle("Tue 08:00", &to(&["Tomas Berg", "Kari"])),
            "Tue 08:00 · to Tomas Berg and 1 other"
        );
        assert_eq!(
            subtitle("Tue 08:00", &to(&["Tomas Berg", "Kari", "Ola"])),
            "Tue 08:00 · to Tomas Berg and 2 others"
        );
    }
}
