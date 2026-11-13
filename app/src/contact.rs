//! The contact pane (the fork's Superhuman pass, phase C; docs/superhuman/contact.md):
//! Superhuman's right-hand pane, local only. Beside an open conversation, in the reading
//! column's right margin where it has room (`window::margin`), the person behind it: by default
//! the conversation's latest sender who isn't the user; the sender of the message `n` or `p`
//! moved to, or whose card has the keyboard focus (`domain::contact::person`). A monogram (the
//! initials on the address's quiet theme tint), the name, the address (a click copies it), the
//! conversations they last wrote in (a click or Enter opens one), one line on how long the
//! store has known them, and a link that searches all mail from them. Nothing else, and nothing
//! from the network: the store holds it all.
//!
//! Speed: the reader's swap paints first. The pane follows after that frame (its paint, then
//! an idle): the name and the address at once; what the store holds from the person from a
//! cache by address, shown while a newer commit is read again, else read off the GTK thread
//! (`Store::contact`, well under a millisecond for a real correspondent at 100,000 messages).

use crate::mailbox::Mailbox;
use crate::reader::{ShownConversation, short_date};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use sealwax_domain::contact::{self, Pick, Said};
use sealwax_domain::model::{FolderRole, MessageId, ThreadId};
use sealwax_domain::split::sender_key;
use sealwax_domain::theme::{avatar_class, initials};
use sealwax_infrastructure::store::{ContactConversation, ContactFacts};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

/// Conversations the pane lists at most.
pub const RECENT: usize = 5;
/// People whose facts are kept, the least recently read going first.
const CACHED: usize = 256;

/// Whom the pane shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
}

/// What the app asks the pane to show: a person, or whom the user's own message was written to
/// (read from the store), beside the open conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Person(Person),
    RecipientOf(MessageId),
}

/// Whom the pane shows for `conversation`, by `domain::contact::person`'s rules: `moved` is the
/// message the user moved to (`n`, `p`, or the keyboard focus into its card); `mine` are the
/// user's addresses, lower case. A message in a Sent or Drafts folder is the user's too (it
/// covers the identities the store doesn't know), as the list's participants say.
pub fn target(
    conversation: &ShownConversation,
    moved: Option<MessageId>,
    mine: &[String],
) -> Option<Target> {
    let said: Vec<Said<'_>> = conversation
        .messages
        .iter()
        .map(|message| {
            let summary = &message.summary;
            Said {
                name: &summary.sender_name,
                email: &summary.sender_email,
                mine: matches!(message.role, FolderRole::Sent | FolderRole::Drafts)
                    || mine
                        .iter()
                        .any(|address| address.eq_ignore_ascii_case(summary.sender_email.trim())),
                draft: message.role == FolderRole::Drafts
                    || summary.flags.contains(sealwax_domain::model::Flags::DRAFT),
            }
        })
        .collect();
    let index_of = |id: MessageId| {
        conversation
            .messages
            .iter()
            .position(|message| message.summary.id == id)
    };
    match contact::person(&said, moved.and_then(index_of)) {
        Pick::Sender(index) => {
            let summary = &conversation.messages[index].summary;
            Some(Target::Person(Person {
                name: contact::display_name(&summary.sender_name, &summary.sender_email).to_owned(),
                email: summary.sender_email.trim().to_owned(),
            }))
        }
        Pick::RecipientOf(index) => {
            Some(Target::RecipientOf(conversation.messages[index].summary.id))
        }
        Pick::Nobody => None,
    }
}

type OpenHandler = Rc<dyn Fn(&ContactConversation)>;
type PersonHandler = Rc<dyn Fn(&Person)>;
/// Whom to show beside which conversation (`None`: nobody).
type Request = Option<(Target, Option<ThreadId>)>;

/// The pane's facts by address, the least recently read from the store first out.
#[derive(Default)]
struct Cache {
    facts: HashMap<String, (u64, ContactFacts)>,
    order: VecDeque<String>,
}

impl Cache {
    fn get(&self, key: &str) -> Option<&(u64, ContactFacts)> {
        self.facts.get(key)
    }

    fn put(&mut self, key: String, version: u64, facts: ContactFacts) {
        if self.facts.insert(key.clone(), (version, facts)).is_some() {
            // Read again: last out now.
            self.order.retain(|kept| *kept != key);
        }
        self.order.push_back(key);
        while self.order.len() > CACHED {
            if let Some(old) = self.order.pop_front() {
                self.facts.remove(&old);
            }
        }
    }
}

/// One conversation of the Recent list: its subject and the date of their latest message, a
/// button in a list item (a screen reader says it can be pressed).
struct RecentRow {
    item: gtk::Box,
    button: gtk::Button,
    subject: gtk::Label,
    date: gtk::Label,
    conversation: RefCell<Option<ContactConversation>>,
}

/// What a read for whom the user's own conversation was written to found, waiting while
/// checks hold it ([`ContactPane::hold_recipients`]).
type Answer = Box<dyn FnOnce(&Rc<Inner>)>;

#[derive(Clone)]
pub struct ContactPane {
    inner: Rc<Inner>,
}

struct Inner {
    mailbox: Rc<Mailbox>,
    root: gtk::ScrolledWindow,
    content: gtk::Box,
    monogram: gtk::Label,
    tint: RefCell<Option<String>>,
    name: gtk::Label,
    address: gtk::Button,
    address_label: gtk::Label,
    history: gtk::Label,
    recent_heading: gtk::Label,
    recent: gtk::Box,
    rows: Vec<RecentRow>,
    search: gtk::Button,
    search_label: gtk::Label,
    /// The person shown and the conversation beside them, and whether their facts are in.
    shown: RefCell<Option<(Person, Option<ThreadId>)>>,
    filled: Cell<bool>,
    /// What was asked for last, applied after the reader's frame (`Some(None)`: nobody).
    wanted: RefCell<Option<Request>>,
    scheduled: Cell<bool>,
    /// Counts the requests applied: what a read made for one found (whom the user's own
    /// conversation was written to) is dropped once a later one was applied or asked for. The
    /// store has two reader threads, so answers can come late and out of order.
    applied: Cell<u64>,
    /// For checks: those answers wait here while held, in the order they came.
    held: RefCell<Option<Vec<Answer>>>,
    cache: RefCell<Cache>,
    /// The newest store version a change event named: older facts are read again.
    latest: Cell<u64>,
    /// Tags reads, so a late answer for someone the pane no longer shows is dropped.
    reads: Cell<u64>,
    on_open: RefCell<Option<OpenHandler>>,
    on_search: RefCell<Option<PersonHandler>>,
    on_copied: RefCell<Option<PersonHandler>>,
}

fn label(classes: &[&str], name: &str) -> gtk::Label {
    let label = gtk::Label::builder().xalign(0.0).build();
    for class in classes {
        label.add_css_class(class);
    }
    label.set_widget_name(name);
    label
}

impl ContactPane {
    pub fn new(mailbox: Rc<Mailbox>) -> Self {
        let monogram = label(&["sw-avatar", "sw-contact-monogram"], "contact-monogram");
        monogram.set_xalign(0.5);
        monogram.set_halign(gtk::Align::Start);
        monogram.set_accessible_role(gtk::AccessibleRole::Presentation);
        let name = label(&["sw-contact-name"], "contact-name");
        name.set_wrap(true);
        name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        name.set_lines(2);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // The whole address, on a second line when it is long, broken between any two
        // characters and with no hyphen the address doesn't have. A middle ellipsis showed a
        // gap after its "…": Pango pads the ellipsis to fill the line.
        let address_label = label(&["sw-contact-address-text"], "contact-address-text");
        address_label.set_wrap(true);
        address_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        address_label.set_lines(2);
        address_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let unhyphenated = gtk::pango::AttrList::new();
        unhyphenated.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
        address_label.set_attributes(Some(&unhyphenated));
        let address = gtk::Button::builder()
            .child(&address_label)
            .halign(gtk::Align::Start)
            .tooltip_text("Copy address")
            .css_classes(["flat", "sw-contact-address"])
            .build();
        address.set_widget_name("contact-address");
        let history = label(&["sw-contact-history"], "contact-history");
        history.set_wrap(true);
        let recent_heading = label(&["sw-contact-heading"], "contact-recent-heading");
        recent_heading.set_label("Recent");
        recent_heading.set_accessible_role(gtk::AccessibleRole::Heading);
        let recent = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sw-contact-recent"])
            .accessible_role(gtk::AccessibleRole::List)
            .build();
        recent.update_property(&[gtk::accessible::Property::Label("Recent conversations")]);
        let rows: Vec<RecentRow> = (0..RECENT)
            .map(|index| {
                let subject = label(&["sw-contact-subject"], &format!("contact-subject-{index}"));
                subject.set_hexpand(true);
                subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
                let date = label(&["sw-contact-date"], &format!("contact-date-{index}"));
                date.set_xalign(1.0);
                let line = gtk::Box::builder()
                    .css_classes(["sw-contact-row-line"])
                    .build();
                line.append(&subject);
                line.append(&date);
                let button = gtk::Button::builder()
                    .child(&line)
                    .hexpand(true)
                    .css_classes(["flat", "sw-contact-row"])
                    .build();
                button.set_widget_name(&format!("contact-recent-{index}"));
                let item = gtk::Box::builder()
                    .accessible_role(gtk::AccessibleRole::ListItem)
                    .visible(false)
                    .build();
                item.append(&button);
                recent.append(&item);
                RecentRow {
                    item,
                    button,
                    subject,
                    date,
                    conversation: RefCell::new(None),
                }
            })
            .collect();
        let search_label = label(&["sw-contact-link-text"], "contact-search-text");
        search_label.set_wrap(true);
        let search = gtk::Button::builder()
            .child(&search_label)
            .halign(gtk::Align::Start)
            .css_classes(["flat", "sw-contact-link"])
            .build();
        search.set_widget_name("contact-search");
        // ARIA's complementary landmark: GTK 4 has no such role (and its `landmark` is
        // abstract), so a named region, which ARIA counts as a landmark too.
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sw-contact-pane"])
            .accessible_role(gtk::AccessibleRole::Region)
            .build();
        content.update_property(&[
            gtk::accessible::Property::Label("Contact details"),
            gtk::accessible::Property::Description(
                "Who the open conversation is with, and their other conversations",
            ),
        ]);
        let person = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sw-contact-person"])
            .build();
        person.append(&monogram);
        person.append(&name);
        person.append(&address);
        person.append(&history);
        content.append(&person);
        content.append(&recent_heading);
        content.append(&recent);
        content.append(&search);
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .css_classes(["sw-contact-scroller"])
            .build();
        root.set_widget_name("contact-pane");
        let inner = Rc::new(Inner {
            mailbox,
            root,
            content,
            monogram,
            tint: RefCell::new(None),
            name,
            address,
            address_label,
            history,
            recent_heading,
            recent,
            rows,
            search,
            search_label,
            shown: RefCell::new(None),
            filled: Cell::new(false),
            wanted: RefCell::new(None),
            scheduled: Cell::new(false),
            applied: Cell::new(0),
            held: RefCell::new(None),
            cache: RefCell::new(Cache::default()),
            latest: Cell::new(0),
            reads: Cell::new(0),
            on_open: RefCell::new(None),
            on_search: RefCell::new(None),
            on_copied: RefCell::new(None),
        });
        Inner::connect(&inner);
        inner.clear();
        Self { inner }
    }

    /// What the window holds in the column's margin.
    pub fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }

    /// Shows `target` beside the conversation `thread` (left out of its Recent list), or
    /// nobody: after the frame the reader paints now, so `j`, `k` and Enter never wait on it.
    /// While the pane isn't laid out (no room, Contact Details off) it waits until it is.
    pub fn show(&self, target: Option<Target>, thread: Option<ThreadId>) {
        self.inner
            .wanted
            .replace(Some(target.map(|target| (target, thread))));
        if self.inner.root.is_mapped() {
            Inner::schedule(&self.inner);
        }
    }

    /// A store commit: facts read before it are read again when next shown, and the shown
    /// person's now (they stay on screen meanwhile).
    pub fn store_changed(&self, version: u64) {
        let inner = &self.inner;
        if version <= inner.latest.replace(version.max(inner.latest.get())) {
            return;
        }
        let shown = inner.shown.borrow().clone();
        if let Some((person, _)) = shown
            && inner.root.is_mapped()
        {
            Inner::read(inner, person);
        }
    }

    /// Opening one of the Recent conversations (a click, or Enter on it).
    pub fn connect_open(&self, handler: impl Fn(&ContactConversation) + 'static) {
        self.inner.on_open.replace(Some(Rc::new(handler)));
    }

    /// "Search all mail from …".
    pub fn connect_search(&self, handler: impl Fn(&Person) + 'static) {
        self.inner.on_search.replace(Some(Rc::new(handler)));
    }

    /// The address was copied to the clipboard.
    pub fn connect_copied(&self, handler: impl Fn(&Person) + 'static) {
        self.inner.on_copied.replace(Some(Rc::new(handler)));
    }

    /// The person shown, and whether what the store holds from them is in: for checks.
    pub fn shown(&self) -> Option<(Person, bool)> {
        let shown = self.inner.shown.borrow();
        shown
            .as_ref()
            .map(|(person, _)| (person.clone(), self.inner.filled.get()))
    }

    /// The Recent list's conversations as shown: for checks.
    pub fn recent(&self) -> Vec<ContactConversation> {
        self.inner
            .rows
            .iter()
            .filter(|row| row.item.is_visible())
            .filter_map(|row| row.conversation.borrow().clone())
            .collect()
    }

    /// The keyboard focus to the pane's first control, the address (Next Pane); false while
    /// it shows nobody or isn't on screen.
    pub fn focus_first(&self) -> bool {
        let inner = &self.inner;
        inner.content.is_visible() && inner.address.is_mapped() && inner.address.grab_focus()
    }

    /// Whether a request waits for the reader's frame: for checks.
    pub fn is_pending(&self) -> bool {
        self.inner.scheduled.get()
    }

    /// For checks: while held, what the reads for whom the user's own conversation was written
    /// to found waits (as a slow reader thread would keep it); let go, it comes in the order it
    /// was read, and the pane takes it only if no later request was applied or asked for.
    pub fn hold_recipients(&self, held: bool) {
        if held {
            self.inner.held.borrow_mut().get_or_insert_with(Vec::new);
            return;
        }
        let waiting = self.inner.held.take().unwrap_or_default();
        for answer in waiting {
            answer(&self.inner);
        }
    }

    /// For checks: how many of those answers wait.
    pub fn held_recipients(&self) -> usize {
        self.inner.held.borrow().as_ref().map_or(0, Vec::len)
    }
}

impl Inner {
    fn connect(inner: &Rc<Inner>) {
        let weak = Rc::downgrade(inner);
        inner.address.connect_clicked(move |button| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let Some((person, _)) = inner.shown.borrow().clone() else {
                return;
            };
            button.clipboard().set_text(&person.email);
            let handler = inner.on_copied.borrow().clone();
            if let Some(handler) = handler {
                handler(&person);
            }
        });
        for row in &inner.rows {
            let weak = Rc::downgrade(inner);
            let button = row.button.clone();
            row.button.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let conversation = inner
                    .rows
                    .iter()
                    .find(|row| row.button == button)
                    .and_then(|row| row.conversation.borrow().clone());
                let handler = inner.on_open.borrow().clone();
                if let (Some(conversation), Some(handler)) = (conversation, handler) {
                    handler(&conversation);
                }
            });
        }
        let weak = Rc::downgrade(inner);
        inner.search.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let person = inner
                .shown
                .borrow()
                .as_ref()
                .map(|(person, _)| person.clone());
            let handler = inner.on_search.borrow().clone();
            if let (Some(person), Some(handler)) = (person, handler) {
                handler(&person);
            }
        });
        // Laid out again (room came back, Contact Details turned on, the reader shown): whom it
        // was asked for meanwhile, or facts a commit made old, are read now.
        let weak = Rc::downgrade(inner);
        inner.root.connect_map(move |_| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if inner.wanted.borrow().is_some() {
                Inner::schedule(&inner);
                return;
            }
            let shown = inner.shown.borrow().clone();
            if let Some((person, _)) = shown {
                Inner::read(&inner, person);
            }
        });
    }

    /// Applies the newest request once the frame the reader asked for is painted: the frame
    /// clock's after-paint, then an idle (so it lands in the next frame); with no frame clock
    /// (the pane not realized), an idle.
    fn schedule(inner: &Rc<Inner>) {
        if inner.scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(inner);
        let apply = move || {
            if let Some(inner) = weak.upgrade() {
                inner.scheduled.set(false);
                Inner::apply(&inner);
            }
        };
        let Some(clock) = inner.root.frame_clock() else {
            glib::idle_add_local_full(glib::Priority::DEFAULT_IDLE, move || {
                apply();
                glib::ControlFlow::Break
            });
            return;
        };
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let held = handler.clone();
        let apply = RefCell::new(Some(apply));
        let id = clock.connect_after_paint(move |clock| {
            if let Some(id) = held.take() {
                clock.disconnect(id);
            }
            if let Some(apply) = apply.take() {
                glib::idle_add_local_full(glib::Priority::DEFAULT_IDLE, move || {
                    apply();
                    glib::ControlFlow::Break
                });
            }
        });
        handler.replace(Some(id));
        clock.request_phase(gdk::FrameClockPhase::AFTER_PAINT);
    }

    fn apply(inner: &Rc<Inner>) {
        let Some(wanted) = inner.wanted.take() else {
            return;
        };
        let request = inner.applied.get() + 1;
        inner.applied.set(request);
        match wanted {
            None => {
                inner.shown.replace(None);
                inner.clear();
            }
            Some((Target::Person(person), thread)) => Inner::present(inner, person, thread),
            Some((Target::RecipientOf(id), thread)) => {
                let reply = inner.mailbox.store().map(|store| store.message(id));
                let mine: Vec<String> = inner
                    .mailbox
                    .accounts()
                    .into_iter()
                    .map(|account| account.address.to_lowercase())
                    .collect();
                let weak = Rc::downgrade(inner);
                glib::spawn_future_local(async move {
                    let detail = match reply {
                        Some(reply) => reply.recv().await.ok().and_then(|found| found.value),
                        None => None,
                    };
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let person = detail.and_then(|detail| {
                        let headers = detail.headers;
                        contact::recipient(&headers.to, &headers.cc, |email| {
                            mine.iter()
                                .any(|address| address.eq_ignore_ascii_case(email.trim()))
                        })
                        .map(|address| Person {
                            name: contact::display_name(&address.name, &address.email).to_owned(),
                            email: address.email.trim().to_owned(),
                        })
                    });
                    let answer: Answer = Box::new(move |inner: &Rc<Inner>| {
                        Inner::recipient_found(inner, request, person, thread);
                    });
                    // Taken now, unless checks hold it.
                    let now = match inner.held.borrow_mut().as_mut() {
                        Some(held) => {
                            held.push(answer);
                            None
                        }
                        None => Some(answer),
                    };
                    if let Some(answer) = now {
                        answer(&inner);
                    }
                });
            }
        }
    }

    /// Whom the user's own message was written to, found for `request`: shown beside `thread`
    /// (nobody when it names no one else) unless a later request was applied or asked for
    /// meanwhile, whose person this would replace.
    fn recipient_found(
        inner: &Rc<Inner>,
        request: u64,
        person: Option<Person>,
        thread: Option<ThreadId>,
    ) {
        if inner.applied.get() != request || inner.wanted.borrow().is_some() {
            return;
        }
        match person {
            Some(person) => Inner::present(inner, person, thread),
            None => {
                inner.shown.replace(None);
                inner.clear();
            }
        }
    }

    /// Shows `person` beside `thread`: who they are at once, what the store holds from them
    /// from the cache (read again when a commit made it old) or from a read.
    fn present(inner: &Rc<Inner>, person: Person, thread: Option<ThreadId>) {
        let same = inner
            .shown
            .borrow()
            .as_ref()
            .is_some_and(|shown| *shown == (person.clone(), thread));
        if same {
            return;
        }
        let same_person = inner
            .shown
            .borrow()
            .as_ref()
            .is_some_and(|(shown, _)| *shown == person);
        inner.shown.replace(Some((person.clone(), thread)));
        if !same_person {
            inner.fill_person(&person);
        }
        let key = sender_key(&person.email).unwrap_or_default();
        let cached = inner.cache.borrow().get(&key).cloned();
        match cached {
            Some((version, facts)) => {
                inner.fill_facts(&facts, thread);
                if version < inner.latest.get() {
                    Inner::read(inner, person);
                }
            }
            None => {
                if !same_person {
                    inner.fill_facts(&ContactFacts::default(), thread);
                    inner.filled.set(false);
                }
                Inner::read(inner, person);
            }
        }
    }

    /// Reads what the store holds from `person` off the GTK thread, and shows it if they are
    /// still the one shown.
    fn read(inner: &Rc<Inner>, person: Person) {
        let key = sender_key(&person.email).unwrap_or_default();
        let fresh = inner
            .cache
            .borrow()
            .get(&key)
            .is_some_and(|(version, _)| *version >= inner.latest.get());
        if fresh {
            return;
        }
        let Some(reply) = inner
            .mailbox
            .store()
            .map(|store| store.contact(&person.email, RECENT as u32 + 1))
        else {
            return;
        };
        let token = inner.reads.get() + 1;
        inner.reads.set(token);
        let weak = Rc::downgrade(inner);
        glib::spawn_future_local(async move {
            let Ok(found) = reply.recv().await else {
                return;
            };
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner
                .cache
                .borrow_mut()
                .put(key, found.version.0, found.value.clone());
            let current = inner.shown.borrow().clone();
            if inner.reads.get() == token
                && let Some((shown, thread)) = current
                && shown == person
            {
                inner.fill_facts(&found.value, thread);
            }
        });
    }

    /// Nothing shown: the pane is empty, and says nothing to a screen reader.
    fn clear(&self) {
        self.filled.set(false);
        self.content.set_visible(false);
        for row in &self.rows {
            row.conversation.replace(None);
            row.item.set_visible(false);
        }
    }

    fn fill_person(&self, person: &Person) {
        self.content.set_visible(true);
        self.monogram
            .set_label(&initials(&person.name, &person.email));
        let tint = avatar_class(&person.email);
        if let Some(old) = self.tint.replace(Some(tint.clone())) {
            self.monogram.remove_css_class(&old);
        }
        self.monogram.add_css_class(&tint);
        self.name.set_label(&person.name);
        self.name.set_visible(person.name != person.email);
        self.address_label.set_label(&person.email);
        self.address
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "Copy address {}",
                person.email
            ))]);
        self.search_label
            .set_label(&contact::search_label(&person.name));
    }

    /// What the store holds from the person shown: the history line, and their conversations
    /// but `open` (the one beside the pane).
    fn fill_facts(&self, facts: &ContactFacts, open: Option<ThreadId>) {
        self.filled.set(true);
        let since = facts
            .first
            .map(month_year)
            .filter(|since| !since.is_empty());
        let line = contact::history(facts.messages, since.as_deref());
        self.history.set_visible(line.is_some());
        self.history.set_label(line.as_deref().unwrap_or_default());
        let listed: Vec<&ContactConversation> = facts
            .recent
            .iter()
            .filter(|found| Some(found.thread) != open)
            .take(RECENT)
            .collect();
        self.recent_heading.set_visible(!listed.is_empty());
        self.recent.set_visible(!listed.is_empty());
        let now = glib::real_time() / 1_000_000;
        let zone = glib::TimeZone::local();
        for (index, row) in self.rows.iter().enumerate() {
            let Some(found) = listed.get(index) else {
                row.conversation.replace(None);
                row.item.set_visible(false);
                continue;
            };
            let subject = if found.subject.trim().is_empty() {
                "(no subject)"
            } else {
                found.subject.as_str()
            };
            // Undated mail (stored at 0) shows no date.
            let date = if found.date > 0 {
                short_date(found.date, now, &zone)
            } else {
                String::new()
            };
            row.subject.set_label(subject);
            row.date.set_label(&date);
            row.date.set_visible(!date.is_empty());
            row.button.set_tooltip_text(Some(subject));
            let name = if date.is_empty() {
                subject.to_owned()
            } else {
                format!("{subject}, {date}")
            };
            row.button
                .update_property(&[gtk::accessible::Property::Label(&name)]);
            row.conversation.replace(Some((*found).clone()));
            row.item.set_visible(true);
        }
    }
}

/// "Mar 2019": the month as the locale abbreviates it (without its full stop, `okt.`, as the
/// list writes it), and the year.
fn month_year(time: i64) -> String {
    glib::DateTime::from_unix_local(time)
        .ok()
        .map(|date| {
            let month = date
                .format("%b")
                .map(|month| month.trim().trim_end_matches('.').to_owned())
                .unwrap_or_default();
            format!("{month} {}", date.year())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Flags, FolderId, MessageSummary};
    use sealwax_infrastructure::store::ConversationMessage;

    fn message(id: i64, name: &str, email: &str, role: FolderRole) -> ConversationMessage {
        ConversationMessage {
            summary: MessageSummary {
                id: MessageId(id),
                folder: FolderId(1),
                thread: ThreadId(7),
                sender_name: name.into(),
                sender_email: email.into(),
                subject: "Cabin trip".into(),
                snippet: String::new(),
                date: id,
                flags: Flags::SEEN,
                has_attachments: false,
                thread_count: 3,
            },
            role,
            copies: Vec::new(),
        }
    }

    fn conversation(messages: Vec<ConversationMessage>) -> ShownConversation {
        ShownConversation {
            thread: Some(ThreadId(7)),
            selected: messages[0].summary.id,
            messages,
        }
    }

    fn person(name: &str, email: &str) -> Option<Target> {
        Some(Target::Person(Person {
            name: name.into(),
            email: email.into(),
        }))
    }

    #[test]
    fn the_pane_shows_the_latest_sender_who_isnt_me_and_follows_a_move() {
        let mine = vec!["me@example.test".to_owned()];
        let shown = conversation(vec![
            message(
                1,
                "Ingrid Solberg",
                "ingrid@example.test",
                FolderRole::Inbox,
            ),
            message(2, "Tomas Berg", "tomas@example.test", FolderRole::Inbox),
            message(3, "Me", "ME@example.test", FolderRole::Inbox),
            message(4, "Alias", "alias@example.test", FolderRole::Sent),
        ]);
        assert_eq!(
            target(&shown, None, &mine),
            person("Tomas Berg", "tomas@example.test")
        );
        assert_eq!(
            target(&shown, Some(MessageId(1)), &mine),
            person("Ingrid Solberg", "ingrid@example.test")
        );
        // The focus on a message of mine: the default again.
        assert_eq!(
            target(&shown, Some(MessageId(4)), &mine),
            person("Tomas Berg", "tomas@example.test")
        );
    }

    #[test]
    fn a_conversation_of_mine_asks_for_its_recipient_and_names_fall_back() {
        let mine = vec!["me@example.test".to_owned()];
        let shown = conversation(vec![
            message(1, "Me", "me@example.test", FolderRole::Sent),
            message(2, "", "me@example.test", FolderRole::Drafts),
        ]);
        assert_eq!(
            target(&shown, None, &mine),
            Some(Target::RecipientOf(MessageId(1)))
        );
        let shown = conversation(vec![message(
            1,
            " ",
            "kari@example.test",
            FolderRole::Inbox,
        )]);
        assert_eq!(
            target(&shown, None, &mine),
            person("kari@example.test", "kari@example.test")
        );
    }

    #[test]
    fn the_cache_keeps_the_newest_people() {
        let mut cache = Cache::default();
        for n in 0..CACHED + 3 {
            cache.put(format!("p{n}"), 1, ContactFacts::default());
        }
        assert_eq!(cache.facts.len(), CACHED);
        assert!(cache.get("p0").is_none());
        assert!(cache.get(&format!("p{}", CACHED + 2)).is_some());
        cache.put("p3".into(), 9, ContactFacts::default());
        assert_eq!(cache.get("p3").map(|(version, _)| *version), Some(9));
        assert_eq!(cache.order.len(), CACHED);
        // Read again, p3 is the newest: the next one in pushes p4 out, the oldest now.
        cache.put("q".into(), 1, ContactFacts::default());
        assert!(cache.get("p3").is_some());
        assert!(cache.get("p4").is_none());
        assert_eq!(cache.order.back().map(String::as_str), Some("q"));
        assert_eq!(cache.facts.len(), CACHED);
        assert_eq!(cache.order.len(), CACHED);
    }
}
