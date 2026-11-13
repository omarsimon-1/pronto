//! The GTK side of one expanded message (DESIGN.md "Reader"; the fork's Superhuman pass): no box
//! of its own, a hairline between messages; the sender block (the name, "to me, Tomas and 2
//! others", the date at the right, the address and expandable details), its own
//! remote-content banner, the body and the attachment cards. The sender block is a button that
//! collapses the card; the card's actions (Details, Reply, Reply all, Forward) sit beside the
//! date and show while the pointer or the focus is in the card (they stay in the Tab chain).
//! The banner (or the line of a sender whose images load) is [`super::remote`]'s, the cards
//! [`super::attachments`]'s and a plain-text body [`super::plain`]'s (M5's owners); each card
//! calls them in one place, [`card`].

use super::Reader;
use super::attachments::attachment_cards;
use super::format;
use super::remote::{Banner, Notice, banner, sender_note};
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::{Attachment, MessageDetail};

/// The widgets of one message's card that the reader changes after building it.
pub struct CardWidgets {
    pub root: gtk::Box,
    /// The sender block: a click, Enter or Space collapses the card.
    pub header: gtk::Button,
    /// The card's actions beside the date: Details, then the reply buttons the reader adds.
    pub actions: gtk::Box,
    /// Where the body goes.
    pub body: gtk::Box,
    pub banner: Option<Banner>,
    /// What the card says about its remote images, as it was built.
    pub notice: Notice,
    /// What takes the focus when the reader keeps it across messages: the body once it has
    /// one, else the details button.
    pub focus: Option<gtk::Widget>,
    /// The date beside the sender, so a page laid out ahead says it afresh when shown
    /// ([`CardWidgets::redate`]).
    pub date: CardDate,
}

/// The date beside a card's sender, and what the reader needs to say it again.
pub struct CardDate {
    pub label: gtk::Label,
    /// The message's date, in seconds since the epoch.
    pub at: i64,
    /// The sender's name, for the header's accessible label.
    pub sender: String,
    /// A draft's card says Draft there, whenever it is.
    pub draft: bool,
}

impl CardWidgets {
    /// Says the date as of `now`: "25 min ago" and a time today move on (to a date) while a
    /// page waits laid out ahead.
    pub fn redate(&self, now: i64) {
        let CardDate {
            label,
            at,
            sender,
            draft,
        } = &self.date;
        if *draft {
            return;
        }
        let text = format::date_line(*at, now, &glib::TimeZone::local());
        if label.label() != text {
            label.set_label(&text);
            self.header
                .update_property(&[gtk::accessible::Property::Label(&format!(
                    "{sender}, {text}, expanded"
                ))]);
        }
    }
}

pub struct Header<'a> {
    pub detail: &'a MessageDetail,
    /// The user's own addresses, for "to me".
    pub mine: &'a [String],
    pub now: i64,
    /// The message is the user's unsent draft: Draft in the draft colour where the date goes.
    pub draft: bool,
}

pub fn card(
    reader: &Reader,
    header: &Header<'_>,
    attachments: &[Attachment],
    notice: Notice,
) -> CardWidgets {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sw-message-card", "sw-reader-card"])
        .build();
    root.set_widget_name("reader-card");
    let (sender, header_button, details, actions, date) = sender_block(header);
    root.append(&sender);
    let banner = match &notice {
        Notice::Hidden(address) => Some(banner(reader, address)),
        Notice::None | Notice::SenderAllowed(_) => None,
    };
    if let Some(banner) = &banner {
        root.append(&banner.root);
    }
    if let Notice::SenderAllowed(address) = &notice {
        root.append(&sender_note(reader, address));
    }
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sw-reader-body"])
        .build();
    body.set_widget_name("reader-body");
    root.append(&body);
    if !attachments.is_empty() {
        root.append(&attachment_cards(
            reader,
            header.detail.summary.id,
            attachments,
        ));
    }
    CardWidgets {
        root,
        header: header_button,
        actions,
        body,
        banner,
        notice,
        focus: Some(details.upcast()),
        date,
    }
}

fn selectable(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .selectable(true)
        .css_classes(classes.to_vec())
        .build()
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(classes.to_vec())
        .build()
}

/// The sender block, the button it makes (the names), its details button, the box of the
/// card's actions (the details button first), and its date ([`CardWidgets::date`]).
fn sender_block(
    header: &Header<'_>,
) -> (gtk::Box, gtk::Button, gtk::ToggleButton, gtk::Box, CardDate) {
    let headers = &header.detail.headers;
    let sender = headers.from.first().cloned().unwrap_or_default();
    let name = if sender.name.trim().is_empty() {
        sender.email.clone()
    } else {
        sender.name.clone()
    };
    let name_label = label(&name, &["sw-reader-sender"]);
    name_label.set_widget_name("reader-sender");
    let first_line = gtk::Box::builder()
        .css_classes(["sw-reader-sender-line"])
        .build();
    first_line.append(&name_label);
    if !sender.email.is_empty() && sender.email != name {
        let address = label(&format!("<{}>", sender.email), &["sw-address"]);
        address.set_widget_name("reader-address");
        first_line.append(&address);
    }
    let recipients = label(
        &format::recipients_line(&headers.to, &headers.cc, header.mine),
        &["sw-small", "sw-secondary", "sw-reader-recipients"],
    );
    recipients.set_widget_name("reader-recipients");
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .css_classes(["sw-reader-names"])
        .build();
    names.append(&first_line);
    names.append(&recipients);
    let date = header
        .detail
        .headers
        .date
        .unwrap_or(header.detail.summary.date);
    let zone = glib::TimeZone::local();
    let date_text = if header.draft {
        super::conversation::DRAFT.to_owned()
    } else {
        format::date_line(date, header.now, &zone)
    };
    let classes: &[&str] = if header.draft {
        &["sw-small", "sw-reader-draft", "sw-reader-date"]
    } else {
        &["sw-small", "sw-secondary", "numeric", "sw-reader-date"]
    };
    let when = label(&date_text, classes);
    when.set_widget_name("reader-date");
    when.set_valign(gtk::Align::Center);
    when.set_tooltip_text(Some(&format::full_date(date, &zone)));
    let details = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&details_grid(header.detail, date, &zone))
        .build();
    let toggle = gtk::ToggleButton::builder()
        .child(&icon(&["pan-down-symbolic"]))
        .tooltip_text("Details")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "sw-icon-button", "sw-card-action"])
        .build();
    toggle.set_widget_name("reader-details-toggle");
    toggle.update_property(&[gtk::accessible::Property::Label("Details")]);
    toggle
        .bind_property("active", &details, "reveal-child")
        .sync_create()
        .build();
    let button = gtk::Button::builder()
        .child(&names)
        .hexpand(true)
        .css_classes(["flat", "sw-reader-card-header"])
        .build();
    button.set_widget_name("reader-card-header");
    button.update_property(&[gtk::accessible::Property::Label(&format!(
        "{name}, {date_text}, expanded"
    ))]);
    button.update_state(&[gtk::accessible::State::Expanded(Some(true))]);
    let actions = gtk::Box::builder()
        .valign(gtk::Align::Center)
        .css_classes(["sw-card-actions"])
        .build();
    actions.append(&toggle);
    let row = gtk::Box::builder()
        .css_classes(["sw-message-header"])
        .build();
    row.append(&button);
    row.append(&actions);
    row.append(&when);
    let block = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    block.append(&row);
    block.append(&details);
    let date = CardDate {
        label: when,
        at: date,
        sender: name,
        draft: header.draft,
    };
    (block, button, toggle, actions, date)
}

fn details_grid(detail: &MessageDetail, date: i64, zone: &glib::TimeZone) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(4)
        .css_classes(["sw-reader-details"])
        .build();
    grid.set_widget_name("reader-details");
    let headers = &detail.headers;
    let rows = [
        ("From", format::address_list(&headers.from)),
        ("Reply to", format::address_list(&headers.reply_to)),
        ("To", format::address_list(&headers.to)),
        ("Cc", format::address_list(&headers.cc)),
        ("Date", format::full_date(date, zone)),
        ("List", headers.list_id.clone().unwrap_or_default()),
    ];
    for (row, (title, value)) in rows
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .enumerate()
    {
        let row = row as i32;
        grid.attach(&label(title, &["sw-small", "sw-secondary"]), 0, row, 1, 1);
        let value = selectable(&value, &["sw-small"]);
        value.set_wrap(true);
        value.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        value.set_hexpand(true);
        grid.attach(&value, 1, row, 1, 1);
    }
    grid
}

/// A short line in place of a body: no text, not downloaded yet, or not readable.
pub fn notice(text: &str) -> gtk::Label {
    let notice = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["sw-secondary", "sw-reader-notice"])
        .build();
    notice.set_widget_name("reader-notice");
    notice
}
