//! One row of the message list as GTK holds it: a message's or a conversation's summary (the
//! store's [`ListRow`]), or nothing yet while its page is on the way. A placeholder is filled in
//! place (`changed`), so the list never reloads under the user; flag changes, and a
//! conversation's new reply, update the same item. A row's identity across change events is its
//! [`RowKey`]: the message, or the conversation's thread.

use gtk4::glib;
use gtk4::glib::subclass::Signal;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use sealwax_domain::model::{ConversationSummary, MessageId, MessageSummary};
use sealwax_infrastructure::store::{ListRow, RowKey};
use std::cell::{Cell, Ref, RefCell};
use std::sync::OnceLock;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageItem {
        pub row: RefCell<Option<ListRow>>,
        pub section: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageItem {
        const NAME: &'static str = "ProntoMessageItem";
        type Type = super::MessageItem;
    }

    impl ObjectImpl for MessageItem {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("changed").build(),
                    Signal::builder("section-changed").build(),
                ]
            })
        }
    }
}

glib::wrapper! {
    pub struct MessageItem(ObjectSubclass<imp::MessageItem>);
}

impl MessageItem {
    pub fn new(section: usize, row: Option<ListRow>) -> Self {
        let item: Self = glib::Object::new();
        item.imp().section.set(section);
        item.imp().row.replace(row);
        item
    }

    pub fn row(&self) -> Ref<'_, Option<ListRow>> {
        self.imp().row.borrow()
    }

    /// The row's identity: its message, or its conversation.
    pub fn key(&self) -> Option<RowKey> {
        self.imp().row.borrow().as_ref().map(ListRow::key)
    }

    /// The message the reader shows for the row: the message, or the conversation's anchor.
    pub fn message(&self) -> Option<MessageId> {
        self.imp().row.borrow().as_ref().map(ListRow::message)
    }

    /// A message row's summary.
    pub fn summary(&self) -> Option<MessageSummary> {
        match self.imp().row.borrow().as_ref() {
            Some(ListRow::Message(row)) => Some(row.clone()),
            _ => None,
        }
    }

    /// A conversation row's summary.
    pub fn conversation(&self) -> Option<ConversationSummary> {
        match self.imp().row.borrow().as_ref() {
            Some(ListRow::Conversation(row)) => Some(row.clone()),
            _ => None,
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.imp().row.borrow().is_some()
    }

    /// The date section (an index into `sections::LABELS`).
    pub fn section(&self) -> usize {
        self.imp().section.get()
    }

    /// Fills a placeholder or updates the row; bound rows redraw.
    pub fn set_row(&self, row: ListRow) {
        let same = self.imp().row.borrow().as_ref() == Some(&row);
        if same {
            return;
        }
        self.imp().row.replace(Some(row));
        self.emit_by_name::<()>("changed", &[]);
    }

    /// Changes a message row in place with `edit`; bound rows redraw when it changed something.
    pub fn edit(&self, edit: impl FnOnce(&mut MessageSummary) -> bool) {
        let changed = match self.imp().row.borrow_mut().as_mut() {
            Some(ListRow::Message(row)) => edit(row),
            _ => false,
        };
        if changed {
            self.emit_by_name::<()>("changed", &[]);
        }
    }

    pub fn set_section(&self, section: usize) {
        if self.imp().section.replace(section) != section {
            self.emit_by_name::<()>("section-changed", &[]);
        }
    }

    pub fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("changed", false, move |values| {
            if let Some(item) = values.first().and_then(|value| value.get::<Self>().ok()) {
                f(&item);
            }
            None
        })
    }

    pub fn connect_section_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("section-changed", false, move |values| {
            if let Some(item) = values.first().and_then(|value| value.get::<Self>().ok()) {
                f(&item);
            }
            None
        })
    }
}
