//! Fictional placeholder content for the self-test's screenshots, taken from the approved design
//! mock (every name, address and message there is made up): two accounts for the sidebar, a
//! message list and a message, in the theme's `sw-` classes. They stand in until the store
//! (ML-023), the list (ML-026) and the reader (ML-027) are connected.

use crate::list::row::{MessageRow, Participant, RowContext, RowFields};
use crate::list::sections::LABELS;
use crate::window::panes::{classed, icon_button};
use crate::window::{
    AccountStatus, FolderRole, SidebarAccount, SidebarFolder, SidebarModel, Window,
};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::theme::{avatar_class, initials};
use std::rc::Rc;

pub fn model() -> SidebarModel {
    SidebarModel {
        preferred: None,
        accounts: vec![
            account(
                "personal",
                "Personal",
                AccountStatus::Synced,
                &[12, 2, 0, 0, 0, 0],
                &[("Receipts", 0), ("Travel", 3)],
            ),
            account(
                "work",
                "Work",
                AccountStatus::Syncing,
                &[4, 0, 0, 0, 1, 0],
                &[("Projects", 1), ("Reports", 0)],
            ),
        ],
    }
}

fn account(
    key: &str,
    name: &str,
    status: AccountStatus,
    counts: &[u32; 6],
    more: &[(&str, u32)],
) -> SidebarAccount {
    let roles = [
        FolderRole::Inbox,
        FolderRole::Drafts,
        FolderRole::Sent,
        FolderRole::Archive,
        FolderRole::Junk,
        FolderRole::Trash,
    ];
    SidebarAccount {
        key: key.to_owned(),
        name: name.to_owned(),
        color: None,
        collapsed: false,
        more_open: None,
        status,
        folders: roles
            .into_iter()
            .zip(counts)
            .map(|(role, &count)| SidebarFolder {
                key: format!("{key}/{}", role.label().to_lowercase()),
                name: role.label().to_owned(),
                role: Some(role),
                count,
                children: Vec::new(),
                folded: false,
            })
            .collect(),
        more: more
            .iter()
            .map(|&(folder, count)| SidebarFolder {
                key: format!("{key}/{}", folder.to_lowercase()),
                name: folder.to_owned(),
                role: None,
                count,
                children: Vec::new(),
                folded: false,
            })
            .collect(),
        reminders: None,
    }
}

struct Message {
    section: &'static str,
    from: &'static str,
    /// Days before today, hour, minute.
    at: (i32, i32, i32),
    subject: &'static str,
    snippet: &'static str,
    unread: bool,
    starred: bool,
    attachment: bool,
    thread: &'static str,
}

const MESSAGES: &[Message] = &[
    Message {
        section: "TODAY",
        from: "Ingrid Solberg",
        at: (0, 10, 32),
        subject: "Cabin weekend — final plan",
        snippet: "Hi! I've booked the cabin in Hemsedal for the 17th–19th. It sleeps six, has a \
                  sauna and the road is cleared…",
        unread: true,
        starred: true,
        attachment: true,
        thread: "3",
    },
    Message {
        section: "",
        from: "Northwind Weekly",
        at: (0, 8, 15),
        subject: "This week in design systems",
        snippet: "Tokens, typography and twelve small utilities that make a big difference — plus \
                  a reader question about dark mode.",
        unread: true,
        starred: false,
        attachment: false,
        thread: "",
    },
    Message {
        section: "",
        from: "Marius Dahl",
        at: (0, 7, 50),
        subject: "Re: Quarterly report draft",
        snippet: "Looks good. Two small comments on page 4 — the churn chart uses last year's \
                  scale…",
        unread: false,
        starred: false,
        attachment: true,
        thread: "5",
    },
    Message {
        section: "YESTERDAY",
        from: "Fjord Cycling Club",
        at: (1, 18, 44),
        subject: "Saturday ride: route and coffee stop",
        snippet: "We meet at 09:00 by the fountain. 62 km, mostly flat, coffee in Drøbak at the \
                  halfway point.",
        unread: false,
        starred: false,
        attachment: false,
        thread: "",
    },
    Message {
        section: "",
        from: "Leah Kim",
        at: (1, 13, 2),
        subject: "Invoice #2041",
        snippet: "Attached is the invoice for September. Payment terms are the usual 14 days — \
                  thanks again!",
        unread: true,
        starred: false,
        attachment: true,
        thread: "",
    },
    Message {
        section: "",
        from: "Aker Library",
        at: (1, 9, 30),
        subject: "Your reservation is ready",
        snippet: "'The Lighthouse Keeper's Atlas' is ready for pickup at the main desk until \
                  Friday.",
        unread: false,
        starred: false,
        attachment: false,
        thread: "",
    },
    Message {
        section: "THIS WEEK",
        from: "Tomas Berg",
        at: (2, 16, 5),
        subject: "Photos from Saturday",
        snippet: "A few favourites — the rest are in the shared album. The one by the lighthouse \
                  is my pick.",
        unread: false,
        starred: false,
        attachment: true,
        thread: "2",
    },
    Message {
        section: "",
        from: "Sana Iqbal",
        at: (2, 12, 40),
        subject: "Lunch Thursday?",
        snippet: "Are you free at 12? There's a new ramen place on Grünerløkka I've been meaning \
                  to try.",
        unread: false,
        starred: true,
        attachment: false,
        thread: "",
    },
];

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    classed(
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
        classes,
    )
}

/// A stand-in for the message list: the search field, the filter chips and rows, nothing
/// behind them. The field takes no focus, so screenshots show it at rest; the real list
/// (ML-026) decides what has the focus first.
pub fn list() -> gtk::Widget {
    let search = classed(
        gtk::SearchEntry::builder()
            .placeholder_text("Search — from: has:attachment is:unread")
            .can_focus(false)
            .build(),
        &["sw-search"],
    );
    let chips = gtk::Box::builder().spacing(6).build();
    for (text, active) in [
        ("All", true),
        ("Unread", false),
        ("Starred", false),
        ("Attachments", false),
    ] {
        chips.append(&classed(
            gtk::ToggleButton::builder()
                .label(text)
                .active(active)
                .build(),
            &["sw-chip"],
        ));
    }
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_start(14)
        .margin_end(14)
        .margin_bottom(4)
        .build();
    header.append(&search);
    header.append(&chips);

    let rows = classed(
        gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build(),
        &["sw-message-list"],
    );
    let mut selected = None;
    let context = RowContext::new();
    let ruler = classed(
        gtk::Box::builder()
            .can_target(false)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build(),
        &["sw-row-ruler"],
    );
    context.set_ruler(&ruler);
    let mut section = 0;
    for message in MESSAGES {
        if !message.section.is_empty() {
            section = LABELS
                .iter()
                .position(|label| label.to_uppercase() == message.section)
                .unwrap_or_default();
            rows.append(
                &gtk::ListBoxRow::builder()
                    .activatable(false)
                    .selectable(false)
                    .child(&label(message.section, &["sw-section-header"]))
                    .build(),
            );
        }
        let row = gtk::ListBoxRow::builder()
            .child(&message_row(&context, message, section))
            .build();
        if message.from == "Marius Dahl" {
            selected = Some(row.clone());
        }
        rows.append(&row);
    }
    rows.select_row(selected.as_ref());
    let list = classed(
        gtk::Box::new(gtk::Orientation::Vertical, 0),
        &["sw-message-list"],
    );
    list.append(&header);
    list.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&rows)
            .build(),
    );
    list.append(&ruler);
    list.upcast()
}

fn avatar(name: &str, address: &str, size: &str) -> gtk::Label {
    let avatar = classed(
        gtk::Label::builder()
            .label(initials(name, address))
            .valign(gtk::Align::Start)
            .build(),
        &["sw-avatar", &avatar_class(address)],
    );
    if !size.is_empty() {
        avatar.add_css_class(size);
    }
    avatar
}

/// A message row as the list draws it (list/row.rs), from a sample message in `section`.
fn message_row(context: &Rc<RowContext>, message: &Message, section: usize) -> MessageRow {
    let row = MessageRow::new(context.clone());
    let sender = [Participant {
        name: message.from,
        me: false,
        unread: message.unread,
        last: 0,
    }];
    row.show_unbound(
        &RowFields {
            participants: &sender,
            writers: 1,
            address: "",
            count: message.thread.parse().unwrap_or(1),
            draft: false,
            subject: message.subject,
            snippet: message.snippet,
            date: sample_date(message.at),
            unread: message.unread,
            starred: message.starred,
            attachment: message.attachment,
            scheduled: None,
        },
        section,
    );
    row
}

/// `days` before today at `hour:minute`, local time, in Unix seconds.
fn sample_date((days, hour, minute): (i32, i32, i32)) -> i64 {
    glib::DateTime::now_local()
        .and_then(|now| {
            glib::DateTime::new(
                &now.timezone(),
                now.year(),
                now.month(),
                now.day_of_month(),
                hour,
                minute,
                0.0,
            )
        })
        .and_then(|today| today.add_days(-days))
        .map_or(0, |date| date.to_unix())
}

/// A stand-in for the reader: one fictional message on a card, with its attachment. The
/// reply buttons name their keys through the window.
pub fn reader(window: &Window) -> gtk::Widget {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .build();
    let subject = label("Re: Quarterly report draft", &["sw-subject-heading"]);
    subject.set_wrap(true);
    column.append(&subject);
    let tags = gtk::Box::builder().spacing(6).build();
    for tag in ["Inbox", "Work"] {
        tags.append(&classed(gtk::Label::new(Some(tag)), &["sw-tag"]));
    }
    column.append(&tags);

    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    let from = gtk::Box::builder().spacing(6).build();
    from.append(&label("Marius Dahl", &["sw-strong", "sw-subtitle"]));
    from.append(&label("<marius@stavtech.example>", &["sw-address"]));
    names.append(&from);
    names.append(&label("to me", &["sw-secondary", "sw-small"]));
    let header = classed(
        gtk::Box::new(gtk::Orientation::Horizontal, 0),
        &["sw-message-header"],
    );
    header.append(&avatar(
        "Marius Dahl",
        "marius@stavtech.example",
        "sw-avatar-large",
    ));
    header.append(&names);
    header.append(&label("07:50 (3 h ago)", &["sw-time"]));
    let actions = gtk::Box::builder()
        .spacing(2)
        .valign(gtk::Align::Center)
        .build();
    for (icons, text, id) in [
        (
            &["mail-reply-sender-symbolic"][..],
            "Reply",
            ActionId::Reply,
        ),
        (
            &["mail-reply-all-symbolic"][..],
            "Reply all",
            ActionId::ReplyAll,
        ),
        (&["mail-forward-symbolic"][..], "Forward", ActionId::Forward),
    ] {
        let button = icon_button(icons, text);
        window.add_shortcut_tooltip(&button, text, id);
        actions.append(&button);
    }
    header.append(&actions);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    for paragraph in [
        "Looks good. Two small comments:",
        "Page 4: the churn chart uses last year's scale — can we align it with the other \
         charts?",
        "The summary could lead with the support numbers; they're the headline this quarter.",
        "Otherwise ready to send on Thursday.\nMarius",
    ] {
        let text = label(paragraph, &["sw-reading"]);
        text.set_wrap(true);
        text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        text.set_ellipsize(gtk::pango::EllipsizeMode::None);
        body.append(&text);
    }

    let file = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    file.append(&label("Q3-report-v2.pdf", &["sw-attachment-name"]));
    file.append(&label("1.4 MB · Open · Save", &["sw-attachment-meta"]));
    let attachment = classed(
        gtk::Box::builder().halign(gtk::Align::Start).build(),
        &["sw-attachment-card"],
    );
    attachment.append(&classed(
        gtk::Label::new(Some("PDF")),
        &["sw-attachment-type"],
    ));
    attachment.append(&file);

    let card = classed(
        gtk::Box::new(gtk::Orientation::Vertical, 0),
        &["sw-message-card"],
    );
    card.append(&header);
    card.append(&body);
    card.append(&attachment);
    column.append(&card);
    column.upcast()
}

/// A stand-in for the reader's actions: buttons with nothing behind them, whose tooltips name
/// the registry's keys.
pub fn reader_toolbar(window: &Window) -> gtk::Widget {
    let bar = gtk::Box::builder().spacing(2).build();
    let button = |icons: &[&str], text: &str, id: Option<ActionId>| {
        let button = icon_button(icons, text);
        if let Some(id) = id {
            window.add_shortcut_tooltip(&button, text, id);
        }
        button
    };
    for (icons, text, id) in [
        (
            &["mail-archive-symbolic", "folder-documents-symbolic"][..],
            "Archive",
            ActionId::Archive,
        ),
        (&["user-trash-symbolic"][..], "Delete", ActionId::Delete),
        (
            &["mail-mark-junk-symbolic"][..],
            "Move to Junk",
            ActionId::Junk,
        ),
        (&["folder-symbolic"][..], "Move to…", ActionId::MoveTo),
    ] {
        bar.append(&button(icons, text, Some(id)));
    }
    bar.append(&classed(
        gtk::Separator::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build(),
        &["sw-toolbar-separator"],
    ));
    bar.append(&button(
        &["mail-unread-symbolic"],
        "Mark as unread",
        Some(ActionId::MarkUnread),
    ));
    bar.append(&button(&["starred-symbolic"], "Star", Some(ActionId::Star)));
    bar.append(&button(
        &["view-more-horizontal-symbolic", "view-more-symbolic"],
        "More actions",
        None,
    ));
    let position = label("3 of 128", &["sw-secondary"]);
    position.set_hexpand(true);
    position.set_xalign(1.0);
    bar.append(&position);
    bar.append(&button(
        &["pan-start-symbolic"],
        "Previous message",
        Some(ActionId::PreviousMessage),
    ));
    bar.append(&button(
        &["pan-end-symbolic"],
        "Next message",
        Some(ActionId::NextMessage),
    ));
    bar.upcast()
}
