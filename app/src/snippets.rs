//! Use Snippet… (Ctrl+; or Ctrl+', the fork's phase B, Superhuman's): a picker
//! ([`crate::picker`]) over the snippets `snippets.toml` holds (`domain::snippets`, read by
//! `Preferences` and read again when the file changes), found by name, then by what they say.
//! The chosen snippet goes in at the caret of the draft that has the focus, the inline reply
//! under a conversation or a composer card, its variables filled in from the draft's first
//! recipient and its sender, the caret where its `{cursor}` was; its subject fills an empty one
//! (a card's) and its recipients join the draft's. Without a draft in focus (the list, the
//! reader) it starts a new message from the account the list shows. With no snippets yet, the
//! picker offers Edit Snippets, which creates the file with examples.

use crate::actions::ActionSet;
use crate::application::{AppState, ProntoApplication};
use crate::composer::{self, ComposeRequest, Composer, Recipients};
use crate::picker::{self, Row, Spec};
use crate::window::Window;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::{AccountId, DraftId, MessageId};
use sealwax_domain::snippets::{self, Snippet, Vars};
use std::cell::RefCell;
use std::rc::Rc;

/// The picker's heading.
pub const TITLE: &str = "Use snippet";
pub const PLACEHOLDER: &str = "Find a snippet";
/// The picker's widget name.
pub const PICKER: &str = "snippet-picker";
/// What the picker says before there is a snippet.
pub const NONE_YET: &str = "No snippets yet";
/// The preview of a snippet's text, in characters.
const PREVIEW_CHARS: usize = 64;

/// Where a chosen snippet goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// The composer card of this draft.
    Card(DraftId),
    /// The inline reply answering this message.
    Reply(MessageId),
    /// A new message from this account (`None`: the composer's first).
    New(Option<AccountId>),
}

/// Connects Use Snippet… to the window's action and builds the window's picker, hidden.
pub fn install(state: &Rc<AppState>, window: &Window, actions: &ActionSet) {
    picker::of(window.window());
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::UseSnippet, move || {
        if let Some(state) = weak.upgrade() {
            open(&state);
        }
    });
}

/// Where Use Snippet… puts a snippet now: the draft that has the focus, else a new message.
pub fn destination(state: &AppState) -> Destination {
    if let Some(window) = state.window()
        && let Some(card) = composer::focused(window.window())
        && let Some(id) = card.id()
    {
        return Destination::Card(id);
    }
    if let Some(quick) = state.reader().and_then(|reader| reader.quick_reply())
        && quick.holds_focus
    {
        return Destination::Reply(quick.target);
    }
    Destination::New(list_account(state))
}

/// The account the list shows (in All Inboxes the selected message's), as Compose takes it.
fn list_account(state: &AppState) -> Option<AccountId> {
    let list = state.list()?;
    let selected = list
        .selected_targets()
        .ok()
        .and_then(|targets| targets.first().map(|target| target.folder));
    crate::compose_actions::compose_account(list.scope(), selected, &state.mailbox().folders())
}

/// The account of the selected message: the inline reply's.
fn selected_account(state: &AppState) -> Option<AccountId> {
    let folder = state.list()?.selected_targets().ok()?.first()?.folder;
    state
        .mailbox()
        .folders()
        .iter()
        .find(|known| known.id == folder)
        .map(|known| known.account)
}

/// The name `account` (else the first) sends as: its settings' sender name, else its own.
fn sender_name(state: &AppState, account: Option<AccountId>) -> String {
    let accounts = state.mailbox().accounts();
    let Some(account) = account
        .and_then(|id| accounts.iter().find(|known| known.id == id))
        .or_else(|| accounts.first())
    else {
        return String::new();
    };
    let settings = state
        .preferences()
        .map(|prefs| prefs.accounts())
        .unwrap_or_default();
    account
        .key
        .as_ref()
        .and_then(|key| settings.iter().find(|known| &known.key == key))
        .map_or_else(|| account.name.clone(), |known| known.name.clone())
}

/// `{date}` today: "29 October 2026".
pub fn today() -> String {
    let now = crate::time_picker::now();
    snippets::date_text(now.year(), now.month() as u8, now.day_of_month() as u8)
}

/// Who a snippet would be written to and from, as the draft it goes to says now: its previews
/// read as it would go in ("Hi Ingrid, Thanks!…"), not as the file writes it ("Hi {first_name},
/// Thanks! {cursor}"; phase C's visual QA).
#[derive(Clone, Debug, Default)]
struct Context {
    /// The draft's first recipient; a snippet's own To stands in when it has none.
    recipient: Option<sealwax_domain::model::Address>,
    me: String,
    date: String,
}

impl Context {
    fn vars(&self, snippet: &Snippet) -> Vars {
        let recipient = self.recipient.as_ref().or_else(|| snippet.to.first());
        Vars::new(recipient, &self.me, &self.date)
    }
}

/// Where `destination` is written to and from, now.
fn context(state: &AppState, destination: Destination) -> Context {
    let date = today();
    match destination {
        Destination::Reply(_) => Context {
            recipient: state
                .reader()
                .and_then(|reader| reader.quick_reply())
                .and_then(|quick| quick.recipients)
                .and_then(|recipients| recipients.to.into_iter().next()),
            me: sender_name(state, selected_account(state)),
            date,
        },
        Destination::Card(draft) => composer::composers()
            .into_iter()
            .find(|card| card.id() == Some(draft))
            .map(|card| Context {
                recipient: card
                    .field("to")
                    .and_then(|field| field.addresses().into_iter().next()),
                me: card
                    .sender()
                    .map(|sender| {
                        if sender.address.name.trim().is_empty() {
                            sender.label
                        } else {
                            sender.address.name
                        }
                    })
                    .unwrap_or_default(),
                date: date.clone(),
            })
            .unwrap_or(Context {
                date,
                ..Context::default()
            }),
        Destination::New(account) => Context {
            recipient: None,
            me: sender_name(state, account),
            date,
        },
    }
}

/// The rows for `query`, and the snippet each is (`None` for Edit Snippets and for a hint). A
/// preview is the snippet as it would go in: its variables filled in from `context`, `{cursor}`
/// gone.
fn listing(all: &[Snippet], query: &str, context: &Context) -> (Vec<Row>, Vec<Option<usize>>) {
    if all.is_empty() {
        return (
            vec![Row::new(ActionId::EditSnippets.title(), NONE_YET)],
            vec![None],
        );
    }
    let found = snippets::find(query, all);
    if found.is_empty() {
        return (
            vec![Row::hint(format!("No snippet matches “{}”", query.trim()))],
            vec![None],
        );
    }
    let rows = found
        .iter()
        .map(|&index| {
            let snippet = &all[index];
            let body = snippets::expand(&snippet.body, &context.vars(snippet));
            Row::new(
                snippet.name.clone(),
                snippets::preview(&body.text, PREVIEW_CHARS),
            )
        })
        .collect();
    (rows, found.into_iter().map(Some).collect())
}

/// Opens the snippet picker over the window; the chosen snippet goes where [`destination`]
/// says now. Its key again closes it, as the palette's does, and it stays shut while a dialog
/// is open.
pub fn open(state: &Rc<AppState>) {
    let (Some(window), Some(prefs)) = (state.window(), state.preferences()) else {
        return;
    };
    let picker = picker::of(window.window());
    if picker.name().as_deref() == Some(PICKER) {
        picker.close();
        return;
    }
    if window.window().visible_dialog().is_some() {
        return;
    }
    // Over the command palette or the time picker it takes their place: they close first, so
    // the focus is back in the draft the snippet goes to.
    crate::palette::close_in(window.window());
    picker.close();
    let destination = destination(state);
    let all = Rc::new(prefs.snippets());
    let shown: Rc<RefCell<Vec<Option<usize>>>> = Rc::default();
    let (listed, found) = (shown.clone(), all.clone());
    let context = context(state, destination);
    let rows = move |query: &str| {
        let (rows, snippets) = listing(&found, query, &context);
        listed.replace(snippets);
        rows
    };
    let weak = Rc::downgrade(state);
    let choose = move |index: usize, _: &str| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        let chosen = shown.borrow().get(index).copied();
        match chosen {
            Some(Some(snippet)) => {
                if let Some(snippet) = all.get(snippet) {
                    put(&state, destination, snippet.clone());
                }
            }
            Some(None) => {
                if let Some(app) = gio::Application::default() {
                    app.activate_action(ActionId::EditSnippets.name(), None);
                }
            }
            None => {}
        }
    };
    picker.open(Spec {
        title: TITLE.to_owned(),
        placeholder: PLACEHOLDER.to_owned(),
        name: PICKER,
        width: crate::palette::WIDTH,
        icon: picker::FIND_ICON,
        details: picker::Details::Column,
        rows: Box::new(rows),
        choose: Box::new(choose),
    });
}

/// `snippet`'s recipients, which join the draft's.
fn recipients(snippet: &Snippet) -> Recipients {
    Recipients {
        to: snippet.to.clone(),
        cc: snippet.cc.clone(),
        bcc: snippet.bcc.clone(),
    }
}

/// Puts `snippet` where `destination` says, its variables filled in. Only the snippet's name is
/// logged.
pub fn put(state: &Rc<AppState>, destination: Destination, snippet: Snippet) {
    tracing::info!(?destination, name = snippet.name, "snippet used");
    match destination {
        Destination::Reply(message) => {
            let Some(reader) = state.reader() else {
                return;
            };
            let Some(quick) = reader.quick_reply().filter(|quick| quick.target == message) else {
                say(state, "The reply under the conversation is gone");
                return;
            };
            let recipient = quick
                .recipients
                .as_ref()
                .and_then(|recipients| recipients.to.first().cloned())
                .or_else(|| snippet.to.first().cloned());
            let me = sender_name(state, selected_account(state));
            let vars = Vars::new(recipient.as_ref(), &me, &today());
            let body = snippets::expand(&snippet.body, &vars);
            reader.insert_in_quick_reply(&body.text, body.cursor, &recipients(&snippet));
        }
        Destination::Card(draft) => {
            let card = composer::composers()
                .into_iter()
                .find(|card| card.id() == Some(draft));
            match card {
                Some(card) => {
                    glib::spawn_future_local(into_card(card, snippet));
                }
                None => say(state, "That draft's composer is closed"),
            }
        }
        Destination::New(account) => {
            let Some(app) = gio::Application::default().and_downcast::<ProntoApplication>() else {
                return;
            };
            composer::open(&app, ComposeRequest::New { account });
            if let Some(card) = composer::composers().pop() {
                glib::spawn_future_local(into_card(card, snippet));
            }
        }
    }
}

/// The snippet in `card` once its body is ready: its variables from the card's first recipient
/// (the snippet's own when the card has none yet) and its sender.
async fn into_card(card: Composer, snippet: Snippet) {
    if !card.wait_ready().await {
        return;
    }
    let recipient = card
        .field("to")
        .and_then(|field| field.addresses().into_iter().next())
        .or_else(|| snippet.to.first().cloned());
    let me = card
        .sender()
        .map(|sender| {
            if sender.address.name.trim().is_empty() {
                sender.label
            } else {
                sender.address.name
            }
        })
        .unwrap_or_default();
    let vars = Vars::new(recipient.as_ref(), &me, &today());
    let body = snippets::expand(&snippet.body, &vars);
    let subject = snippet
        .subject
        .as_deref()
        .map(|subject| snippets::expand(subject, &vars).text);
    card.insert_snippet(
        &body.text,
        body.cursor,
        subject.as_deref(),
        &recipients(&snippet),
    )
    .await;
}

fn say(state: &AppState, text: &str) {
    let toast = adw::Toast::builder()
        .title(text)
        .use_markup(false)
        .timeout(4)
        .build();
    state.show_toast(&toast, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(name: &str, body: &str) -> Snippet {
        Snippet {
            name: name.to_owned(),
            body: body.to_owned(),
            subject: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
        }
    }

    #[test]
    fn the_picker_lists_snippets_with_a_preview() {
        let all = [
            snippet(
                "Thanks",
                "Hi {first_name},\n\nThanks! {cursor}\n\n{my_name}",
            ),
            snippet("Out of office", "I'm away until Monday"),
        ];
        let none = Context::default();
        let (rows, found) = listing(&all, "", &none);
        // Previews read as the snippet would go in: no braces, no gap for an unknown name.
        assert_eq!(rows[0], Row::new("Thanks", "Hi, Thanks!"));
        assert_eq!(found, [Some(0), Some(1)]);
        let to_ingrid = Context {
            recipient: Some(sealwax_domain::model::Address::new(
                "Ingrid Solberg",
                "ingrid@example.test",
            )),
            me: "Alex Berg".to_owned(),
            date: "29 October 2026".to_owned(),
        };
        let (rows, _) = listing(&all, "", &to_ingrid);
        assert_eq!(rows[0], Row::new("Thanks", "Hi Ingrid, Thanks! Alex Berg"));
        // A snippet's own To stands in for a draft without one.
        let mut intro = snippet("Introduction", "Hi {first_name}, meet Ola.");
        intro.to = vec![sealwax_domain::model::Address::new(
            "Tomas Berg",
            "tomas@example.test",
        )];
        let (rows, _) = listing(&[intro], "", &none);
        assert_eq!(rows[0].detail, "Hi Tomas, meet Ola.");
        let (rows, found) = listing(&all, "monday", &none);
        assert_eq!(rows[0].title, "Out of office");
        assert_eq!(found, [Some(1)]);
        let (rows, found) = listing(&all, "zzz", &none);
        assert!(rows[0].hint);
        assert_eq!(found, [None]);
        // With none yet, Edit Snippets makes the file.
        let (rows, found) = listing(&[], "", &none);
        assert_eq!(rows, [Row::new("Edit Snippets (snippets.toml)", NONE_YET)]);
        assert_eq!(found, [None]);
    }
}
