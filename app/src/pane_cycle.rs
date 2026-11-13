//! Next Pane and Previous Pane (F6 and Shift+F6, or Ctrl+Alt+Right and Left; the fork's
//! Superhuman pass, phase C, docs/superhuman/contact.md): the keyboard focus from pane to pane,
//! those on screen in reading order and round again: the sidebar, the list, the conversation,
//! the contact pane. Each takes it where it means something: the folder shown, the list's
//! selected row, the conversation where the user left it (else the open message), the contact
//! pane's address. A sidebar lying over the mail closes as the focus leaves it. It is the way
//! into the contact pane without Tab going through the whole conversation, and a way out of a
//! split Inbox's list, whose Tab switches its tabs.

use crate::actions::ActionSet;
use crate::application::AppState;
use crate::reader::Reader;
use crate::window::{PaneStop, Window};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::MessageId;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Where the keyboard focus was last in the conversation's pane, and the message the reader
/// showed then: coming back to the same conversation puts it there again.
type Left = Rc<RefCell<Option<(glib::WeakRef<gtk::Widget>, MessageId)>>>;

/// Connects Next Pane and Previous Pane to `window`.
pub fn install(state: &Rc<AppState>, window: &Window, actions: &ActionSet) {
    let left: Left = Rc::default();
    // Looked at in an idle, once a move is done: the reader moves the focus while it changes
    // the page, which it holds meanwhile.
    let due = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(state);
    let kept = left.clone();
    window.window().connect_focus_widget_notify(move |_| {
        if due.replace(true) {
            return;
        }
        let (due, weak, kept) = (due.clone(), weak.clone(), kept.clone());
        glib::idle_add_local_once(move || {
            due.set(false);
            let Some(state) = weak.upgrade() else {
                return;
            };
            if let Some(window) = state.window() {
                remember(&state, &window, &kept);
            }
        });
    });
    for (id, forward) in [(ActionId::NextPane, true), (ActionId::PreviousPane, false)] {
        let weak = Rc::downgrade(state);
        let left = left.clone();
        actions.connect(id, move || {
            if let Some(state) = weak.upgrade() {
                cycle(&state, forward, &left);
            }
        });
    }
}

/// Notes where the keyboard focus is when it is in the conversation's pane.
fn remember(state: &AppState, window: &Window, left: &Left) {
    let Some(focus) = GtkWindowExt::focus(window.window()) else {
        return;
    };
    if window.pane_stop_of(&focus) != Some(PaneStop::Conversation) {
        return;
    }
    if let Some(shown) = state.reader().and_then(|reader| reader.message()) {
        left.replace(Some((focus.downgrade(), shown)));
    }
}

/// The panes after the one the focus is in (`forward`) or before it, round again, without it;
/// from outside them all (a composer card, nothing focused), the first or the last onwards.
fn order(stops: &[PaneStop], from: Option<PaneStop>, forward: bool) -> Vec<PaneStop> {
    let count = stops.len();
    let at = from.and_then(|from| stops.iter().position(|stop| *stop == from));
    (1..=count)
        .map(|step| {
            let index = match (at, forward) {
                (Some(at), true) => (at + step) % count,
                (Some(at), false) => (at + count - step) % count,
                (None, true) => step - 1,
                (None, false) => count - step,
            };
            stops[index]
        })
        .filter(|stop| Some(*stop) != from)
        .collect()
}

/// Moves the keyboard focus to the next pane on screen (`forward`) or the previous one; a pane
/// where nothing takes it (the reader's "No message selected") is passed. The palette, a
/// picker or a dialog over the window keeps the focus.
fn cycle(state: &Rc<AppState>, forward: bool, left: &Left) {
    let Some(window) = state.window() else {
        return;
    };
    if window.focus_over_content() {
        return;
    }
    let from = window.focused_pane_stop();
    let next = order(&window.pane_stops(), from, forward);
    if next.is_empty() {
        return;
    }
    match from {
        // Where it leaves the conversation, as it leaves: a key's move just before may not
        // have been looked at yet.
        Some(PaneStop::Conversation) => remember(state, &window, left),
        Some(PaneStop::Folders) => {
            window.close_folders_over_mail();
        }
        _ => {}
    }
    for stop in next {
        if focus(state, &window, stop, left) {
            return;
        }
    }
}

/// The keyboard focus into `stop`; false when nothing there takes it.
fn focus(state: &Rc<AppState>, window: &Window, stop: PaneStop, left: &Left) -> bool {
    match stop {
        PaneStop::Folders => window.focus_folders(),
        PaneStop::List => {
            let Some(list) = state.list() else {
                return false;
            };
            list.focus_selected();
            window.focused_pane_stop() == Some(PaneStop::List)
        }
        PaneStop::Conversation => {
            let reader = state.reader();
            let shown = reader.as_ref().and_then(Reader::message);
            let on_message = window.reader_page() == "message";
            // Where it was left in this conversation, while that is still on screen.
            let kept = left
                .borrow()
                .as_ref()
                .filter(|(_, message)| on_message && Some(*message) == shown)
                .and_then(|(widget, _)| widget.upgrade());
            if let Some(widget) = kept
                && widget.is_mapped()
                && widget.is_sensitive()
                && widget.is_focusable()
                && window.pane_stop_of(&widget) == Some(PaneStop::Conversation)
                && widget.grab_focus()
            {
                return true;
            }
            (on_message
                && reader
                    .zip(shown)
                    .is_some_and(|(reader, shown)| reader.focus_message(shown)))
                || window.focus_reader_bar()
                || window.focus_reader_page()
        }
        PaneStop::Contact => state.contact().is_some_and(|pane| pane.focus_first()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PaneStop::{Contact, Conversation, Folders, List};

    #[test]
    fn the_panes_come_round_in_reading_order() {
        let all = [Folders, List, Conversation, Contact];
        assert_eq!(
            order(&all, Some(Conversation), true),
            [Contact, Folders, List]
        );
        assert_eq!(
            order(&all, Some(Conversation), false),
            [List, Folders, Contact]
        );
        assert_eq!(
            order(&all, Some(Contact), true),
            [Folders, List, Conversation]
        );
        assert_eq!(
            order(&all, Some(Folders), false),
            [Contact, Conversation, List]
        );
        // From outside them all (a composer card): the first onwards, or the last backwards.
        assert_eq!(order(&all, None, true), all);
        assert_eq!(
            order(&all, None, false),
            [Contact, Conversation, List, Folders]
        );
        // Focused mode's open conversation beside its contact pane: the two in turn.
        let focused = [Conversation, Contact];
        assert_eq!(order(&focused, Some(Conversation), true), [Contact]);
        assert_eq!(order(&focused, Some(Conversation), false), [Contact]);
        assert_eq!(order(&focused, Some(Contact), true), [Conversation]);
        // One pane alone: nowhere to go, unless the focus is outside it.
        assert!(order(&[List], Some(List), true).is_empty());
        assert_eq!(order(&[List], None, false), [List]);
        assert!(order(&[], None, true).is_empty());
        // The focus in a pane that isn't on screen (the reader warm behind a Focused list).
        assert_eq!(
            order(&[Folders, List], Some(Conversation), true),
            [Folders, List]
        );
    }
}
