//! Self-test commands for replying and composing from the main window (M6: ML-068, ML-069,
//! ML-074, ML-078): what `composer::open` was asked for, the cards' buttons, the quick reply
//! and its Send seam, and Make Pronto the Default Mail App… in a sandboxed XDG home. Message
//! names are the conversation samples' (`trip-6`), or `selected` for the list's selection.
//! Every name, address and text is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `reply-sandbox` | Before the app starts: `HOME`, `XDG_CONFIG_HOME` and `XDG_DATA_HOME` point into the run's scratch directory (`$TMP/home`), so GIO reads and writes the run's own `mimeapps.list` and desktop entries |
//! | `reply-install-entry` | Writes a desktop entry for this run's app-id into the sandbox and waits until GIO lists it |
//! | `reply-click-card MESSAGE reply\|reply-all\|forward\|edit-draft` | A real click on that button of MESSAGE's card |
//! | `assert-compose-requests N` | `composer::open` was asked N times in this run |
//! | `assert-compose-last KIND [ARG] [ARG] [ARG] [ARG]` | The latest request: `new ACCOUNT\|none`, `reply MESSAGE reply\|reply-all\|forward`, `quick-reply MESSAGE TEXT [reply\|reply-all] [TO,…\|none]` (the chosen To), `server-draft MESSAGE`, `mailto TO` |
//! | `assert-quick-reply FIELD VALUE` | The inline reply on screen: `shown BOOL`, `target MESSAGE`, `label TEXT`, `placeholder TEXT` (what the closed box says), `text TEXT`, `focused BOOL`, `send BOOL` (its Send sensitive), `kind reply\|reply-all`, `open BOOL`, `editing BOOL` (its recipients' fields show), `chosen BOOL` (the user chose them), `to\|cc\|bcc A,B\|none` (whom it goes to) |
//! | `assert-quick-sends N` | The inline reply's Send took N replies |
//! | `assert-quick-send-last MESSAGE TEXT [KIND] [TO,…\|none]` | The latest one Send took, and the To it chose (`none`: the compose rules') |
//! | `assert-mailto-handler ID\|ours\|not-ours\|none` | The handler GIO reports for `x-scheme-handler/mailto` (`ours`: this run's desktop entry); prints the sandbox's `mimeapps.list` |
//! | `assert-default-app-dialog heading\|body TEXT` | The default-mail-app dialog on screen: its heading, or text its body holds |
//! | `measure-reply KEY COUNT [BOUND]` | KEY COUNT times in a conversation on screen, each timed from the key reaching the window to the frame with the inline reply open and focused (Escape between); fails over BOUND ms (a round over it is timed once more first: a loaded machine's one-off stall) |
//! | `reply-relayout` | The reader drops every conversation it laid out (as one falls out of its warm set) and shows the selected message again: its conversation is laid out anew, the inline reply with it |
//! | `assert-reply-draft MESSAGE none\|KIND [TO,…] [CC,…\|none]` | The store's newest closed reply draft to MESSAGE (the inline reply's, which its Drafts-folder copy is made from): none, or one made as KIND (`reply`, `reply-all`) to TO, Cc CC; prints whom it goes to |
//! | `quick-tries-mark` | Notes how many saves the inline replies' draft keeper has tried, for `assert-quick-tries` |
//! | `assert-quick-tries MIN MAX` | The keeper tried MIN to MAX saves or discards since `quick-tries-mark` |

use super::script::{Step, boolean};
use super::{Harness, StepResult, equal, number, triage};
use crate::compose_actions;
use crate::composer::{self, ComposeRequest};
use crate::default_handler::{self, MAILTO_TYPE};
use crate::reader::Reader;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::model::MessageId;
use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands and their argument counts, for the script parser.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("reply-sandbox", 0, 0),
    ("reply-install-entry", 0, 0),
    ("reply-click-card", 2, 2),
    ("assert-compose-requests", 1, 1),
    ("assert-compose-last", 1, 5),
    ("assert-quick-reply", 2, 2),
    ("assert-quick-sends", 1, 1),
    ("assert-quick-send-last", 2, 4),
    ("assert-mailto-handler", 1, 1),
    ("assert-default-app-dialog", 2, 2),
    ("measure-reply", 2, 3),
    ("reply-relayout", 0, 0),
    ("assert-reply-draft", 2, 4),
    ("quick-tries-mark", 0, 0),
    ("assert-quick-tries", 2, 2),
];

const WAIT: Duration = Duration::from_secs(10);

thread_local! {
    /// The keeper's tries at `quick-tries-mark`.
    static TRIES_MARK: Cell<u32> = const { Cell::new(0) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

/// `reply-sandbox`, before GTK or any thread starts: the home folder and GIO's per-user
/// configuration and data (`mimeapps.list`, `applications/`) are the run's own, under
/// `root/home`.
pub fn before_start(steps: &[Step], root: &Path) -> Result<(), String> {
    if !steps.iter().any(|step| step.command == "reply-sandbox") {
        return Ok(());
    }
    let home = root.join("home");
    let config = home.join(".config");
    let data = home.join(".local/share");
    for dir in [&config, &data.join("applications")] {
        fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    // SAFETY: the self-test calls this before GTK, the Broadway client or any worker starts, so
    // no other thread reads the environment.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_DATA_HOME", &data);
    }
    Ok(())
}

fn reader(harness: &Harness) -> Result<Reader, String> {
    harness
        .state
        .reader()
        .ok_or_else(|| "there is no reader".to_owned())
}

/// A conversation sample's message, or `selected`.
fn message(harness: &Harness, name: &str) -> Result<MessageId, String> {
    if name == "selected" {
        return super::mail::list(harness)?
            .selected()
            .ok_or_else(|| "no message is selected".to_owned());
    }
    super::conversation::id(name)
}

fn name_of(harness: &Harness, id: MessageId) -> String {
    let selected = super::mail::list(harness)
        .ok()
        .and_then(|list| list.selected());
    match super::conversation::name(id) {
        Some(name) => name,
        None if selected == Some(id) => "selected".to_owned(),
        None => id.0.to_string(),
    }
}

/// A request as the assertions name it; the user's text only as its length.
fn describe(harness: &Harness, request: &ComposeRequest) -> String {
    match request {
        ComposeRequest::New { account } => format!("new {account:?}"),
        ComposeRequest::Reply { message, kind } => {
            format!("reply {} {}", name_of(harness, *message), kind_name(*kind))
        }
        ComposeRequest::QuickReply {
            message,
            text,
            kind,
            recipients,
            ..
        } => format!(
            "quick-reply {} {} ({} characters, {:?})",
            name_of(harness, *message),
            kind_name(*kind),
            text.chars().count(),
            recipients
        ),
        ComposeRequest::Mailto(_) => "mailto".to_owned(),
        ComposeRequest::Draft(draft) => format!("draft {draft}"),
        ComposeRequest::ServerDraft(message) => {
            format!("server-draft {}", name_of(harness, *message))
        }
    }
}

fn kind_name(kind: ReplyKind) -> &'static str {
    match kind {
        ReplyKind::Reply => "reply",
        ReplyKind::ReplyAll => "reply-all",
        ReplyKind::Forward => "forward",
    }
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "reply-sandbox" => {
            let config = glib::user_config_dir();
            if config.starts_with(&harness.root) {
                println!("  # XDG_CONFIG_HOME is {}", config.display());
                Ok(())
            } else {
                Err(format!(
                    "GIO's configuration folder is {}, outside the run's scratch directory",
                    config.display()
                ))
            }
        }
        "reply-install-entry" => install_entry(harness).await,
        "reply-click-card" => click_card(harness, arg(0), arg(1)).await,
        "assert-compose-requests" => equal(
            "compose requests",
            composer::requests().len(),
            number(arg(0))?,
        ),
        "assert-compose-last" => compose_last(harness, args),
        "assert-quick-reply" => quick_reply(harness, arg(0), arg(1)),
        "assert-quick-sends" => equal(
            "quick replies sent",
            compose_actions::quick_sends().len(),
            number(arg(0))?,
        ),
        "assert-quick-send-last" => {
            let sent = compose_actions::quick_sends()
                .pop()
                .ok_or("the quick reply sent nothing")?;
            equal(
                "message answered",
                sent.message,
                self::message(harness, arg(0))?,
            )?;
            equal("text sent", sent.text.as_str(), arg(1))?;
            if let Some(kind) = args.get(2) {
                equal("kind", kind_name(sent.kind), kind.as_str())?;
            }
            match args.get(3) {
                Some(to) => {
                    let chosen = sent
                        .recipients
                        .as_ref()
                        .map_or_else(|| "none".to_owned(), |chosen| emails(&chosen.to));
                    equal("the chosen To", chosen.as_str(), to.as_str())
                }
                None => Ok(()),
            }
        }
        "measure-reply" => {
            let bound = args.get(2).map(|ms| number(ms)).transpose()?;
            measure_reply(harness, arg(0), number(arg(1))?, bound).await
        }
        "reply-relayout" => {
            let selected = message(harness, "selected")?;
            let reader = reader(harness)?;
            reader.clear();
            reader.show(selected);
            Ok(())
        }
        "assert-reply-draft" => reply_draft(harness, args).await,
        "quick-tries-mark" => {
            TRIES_MARK.with(|mark| mark.set(compose_actions::quick_draft_tries()));
            Ok(())
        }
        "assert-quick-tries" => {
            let tries =
                compose_actions::quick_draft_tries().saturating_sub(TRIES_MARK.with(Cell::get));
            println!("  # {tries} tries");
            let (min, max) = (number(&args[0])? as u32, number(&args[1])? as u32);
            if (min..=max).contains(&tries) {
                Ok(())
            } else {
                Err(format!(
                    "the keeper tried {tries} times; wanted {min} to {max}"
                ))
            }
        }
        "assert-mailto-handler" => mailto_handler(harness, arg(0)),
        "assert-default-app-dialog" => {
            let dialog = harness
                .widget("default-mail-app")?
                .downcast::<adw::AlertDialog>()
                .map_err(|_| "default-mail-app isn't an alert dialog".to_owned())?;
            match arg(0) {
                "heading" => equal(
                    "heading",
                    dialog.heading().unwrap_or_default().as_str(),
                    arg(1),
                ),
                "body" if dialog.body().contains(arg(1)) => {
                    println!("  # {}", dialog.body().replace('\n', " "));
                    Ok(())
                }
                "body" => Err(format!("the body lacks {:?}: {}", arg(1), dialog.body())),
                other => Err(format!("use heading or body, not {other}")),
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn compose_last(harness: &Harness, args: &[String]) -> StepResult {
    let requests = composer::requests();
    let last = requests.last().ok_or("nothing was asked of the composer")?;
    let wanted = |index: usize| {
        args.get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("{} needs more arguments", args[0]))
    };
    let mismatch = || {
        Err(format!(
            "the latest request is {}, not {}",
            describe(harness, last),
            args.join(" ")
        ))
    };
    match (args[0].as_str(), last) {
        ("new", ComposeRequest::New { account }) => {
            let name = account.map_or_else(
                || "none".to_owned(),
                |account| {
                    harness
                        .state
                        .mailbox()
                        .accounts()
                        .into_iter()
                        .find(|candidate| candidate.id == account)
                        .map_or_else(|| account.0.to_string(), |found| found.name)
                },
            );
            equal("account", name.as_str(), wanted(1)?)
        }
        ("reply", ComposeRequest::Reply { message, kind }) => {
            equal("message", *message, self::message(harness, wanted(1)?)?)?;
            equal("kind", kind_name(*kind), wanted(2)?)
        }
        (
            "quick-reply",
            ComposeRequest::QuickReply {
                message,
                text,
                kind,
                recipients,
                ..
            },
        ) => {
            equal("message", *message, self::message(harness, wanted(1)?)?)?;
            equal("text", text.as_str(), wanted(2)?)?;
            if let Some(wanted) = args.get(3) {
                equal("kind", kind_name(*kind), wanted.as_str())?;
            }
            match args.get(4) {
                Some(wanted) => equal(
                    "chosen To",
                    recipients
                        .as_ref()
                        .map(|chosen| emails(&chosen.to))
                        .unwrap_or_else(|| "none".to_owned())
                        .as_str(),
                    wanted.as_str(),
                ),
                None => Ok(()),
            }
        }
        ("server-draft", ComposeRequest::ServerDraft(message)) => {
            equal("message", *message, self::message(harness, wanted(1)?)?)
        }
        ("mailto", ComposeRequest::Mailto(mailto)) => {
            equal("to", mailto.to.join(",").as_str(), wanted(1)?)
        }
        _ => mismatch(),
    }
}

/// `assert-reply-draft MESSAGE none|KIND [TO] [CC]`: the inline reply's stored draft.
async fn reply_draft(harness: &Harness, args: &[String]) -> StepResult {
    let message = self::message(harness, &args[0])?;
    let reply = harness
        .state
        .mailbox()
        .store()
        .map(|store| store.reply_draft(message))
        .ok_or("the store is not open")?;
    let stored = reply.recv().await.map_err(|error| error.to_string())?;
    let Some(stored) = stored else {
        return equal("the reply draft", "none", args[1].as_str());
    };
    let draft = &stored.draft;
    let kind = draft
        .source
        .as_ref()
        .map_or("none", |source| kind_name(source.kind));
    let found = [kind.to_owned(), emails(&draft.to), emails(&draft.cc)];
    let wanted = &args[1..];
    let same = wanted
        .iter()
        .zip(&found)
        .all(|(wanted, found)| wanted == found);
    if !same {
        return Err(format!(
            "draft {} is {} to {}, cc {}; wanted {}",
            stored.id.0,
            found[0],
            found[1],
            found[2],
            wanted.join(" ")
        ));
    }
    println!(
        "  # draft {}: {} to {}, cc {}, bcc {}",
        stored.id.0,
        found[0],
        found[1],
        found[2],
        emails(&draft.bcc)
    );
    Ok(())
}

fn quick_reply(harness: &Harness, field: &str, value: &str) -> StepResult {
    let reader = reader(harness)?;
    let report = reader.quick_reply();
    if field == "shown" {
        return equal("a quick reply shown", report.is_some(), boolean(value)?);
    }
    let report = report.ok_or("no quick reply is shown")?;
    match field {
        "target" => equal("answers", report.target, message(harness, value)?),
        "label" => equal("label", report.label.as_str(), value),
        "placeholder" => equal("the closed box", report.placeholder.as_str(), value),
        "text" => equal("text", report.text.as_str(), value),
        "focused" => equal("focus in the text", report.focused, boolean(value)?),
        "send" => equal("Send sensitive", report.send_sensitive, boolean(value)?),
        "kind" => equal("answers as", kind_name(report.kind), value),
        "open" => equal("open", report.open, boolean(value)?),
        "editing" => equal(
            "the recipients' fields show",
            report.editing,
            boolean(value)?,
        ),
        "chosen" => equal("recipients chosen", report.chosen, boolean(value)?),
        "to" | "cc" | "bcc" => {
            let recipients = report.recipients.ok_or("whom it answers isn't known yet")?;
            let list = match field {
                "to" => &recipients.to,
                "cc" => &recipients.cc,
                _ => &recipients.bcc,
            };
            equal(field, emails(list).as_str(), value)
        }
        other => Err(format!("no field {other}")),
    }
}

/// `a@example.test,b@example.test`, or `none`.
fn emails(addresses: &[sealwax_domain::model::Address]) -> String {
    if addresses.is_empty() {
        return "none".to_owned();
    }
    addresses
        .iter()
        .map(|address| address.email.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// `measure-reply KEY COUNT [BOUND]`: in a conversation on screen, KEY (`Return`, `r`, `a`)
/// COUNT times, each timed from the key's arrival in the window to the first frame painted with
/// the inline reply open and focused (and from the press, Broadway's delivery included, which
/// is bimodal: a few ms or about 40); Escape closes it between. Fails when the worst, from the
/// key's arrival, is over BOUND ms. A round over BOUND is timed once more before it counts: on a
/// loaded machine (a sweep's parallel shards, a build) another process can take the CPU between
/// the key and its frame once (104.7 ms against a quiet worst of 14 in the phase B sweep), while
/// a slow path is slow again.
async fn measure_reply(
    harness: &Harness,
    key: &str,
    count: usize,
    bound: Option<usize>,
) -> StepResult {
    let window = harness.window()?;
    let reader = reader(harness)?;
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let painted: Rc<Cell<Option<Instant>>> = Rc::default();
    let focused: Rc<Cell<Option<Instant>>> = Rc::default();
    let (watch, mark, came) = (reader.clone(), painted.clone(), focused.clone());
    let paint = clock.connect_after_paint(move |_| {
        let open = watch
            .quick_reply()
            .is_some_and(|quick| quick.focused && quick.open);
        if open && mark.get().is_none() {
            mark.set(Some(Instant::now()));
        }
    });
    let text = harness.widget("quick-reply-text")?;
    let focus = text.connect_has_focus_notify(move |text| {
        if text.has_focus() && came.get().is_none() {
            came.set(Some(Instant::now()));
        }
    });
    // When the key reached the window: before every handler.
    let arrived: Rc<Cell<Option<Instant>>> = Rc::default();
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let first = arrived.clone();
    keys.connect_key_pressed(move |_, _, _, _| {
        if first.get().is_none() {
            first.set(Some(Instant::now()));
        }
        glib::Propagation::Proceed
    });
    window.window().add_controller(keys.clone());
    clock.begin_updating();
    let ms = |from: Instant, to: Instant| to.duration_since(from).as_secs_f64() * 1000.0;
    let mut times = Vec::new();
    let mut result = Ok(());
    let (mut round, mut again) = (0, false);
    while round < count {
        painted.set(None);
        focused.set(None);
        arrived.set(None);
        let pressed = Instant::now();
        if let Err(error) = triage::press(key) {
            result = Err(error);
            break;
        }
        while painted.get().is_none() && pressed.elapsed() < WAIT {
            glib::timeout_future(Duration::from_millis(1)).await;
        }
        let Some(drawn) = painted.get() else {
            result = Err(format!(
                "round {round}: {key} never opened the inline reply"
            ));
            break;
        };
        let came = focused.get().unwrap_or(drawn);
        let reached = arrived.get().unwrap_or(pressed);
        let took = ms(reached, drawn);
        println!(
            "  # round {round}{}: {key} → the inline reply focused and drawn {took:.1} ms after \
             the key reached the window ({:.1} ms after the press, {:.1} ms after the focus came \
             in)",
            if again { " again" } else { "" },
            ms(pressed, drawn),
            ms(came, drawn)
        );
        if let Err(error) = triage::press("Escape") {
            result = Err(error);
            break;
        }
        harness.wait_until_settled().await;
        // Over the bound the first time: timed once more (see above).
        if !again && bound.is_some_and(|bound| took > bound as f64) {
            again = true;
            continue;
        }
        times.push(took);
        (round, again) = (round + 1, false);
    }
    clock.end_updating();
    clock.disconnect(paint);
    text.disconnect(focus);
    window.window().remove_controller(&keys);
    result?;
    let worst = times.iter().copied().fold(0.0_f64, f64::max);
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    println!(
        "  # {key}: median {:.1} ms, worst {worst:.1} ms from the key reaching the window to \
         the frame",
        sorted.get(sorted.len() / 2).copied().unwrap_or_default()
    );
    match bound {
        Some(bound) if worst > bound as f64 => Err(format!(
            "the inline reply took {worst:.1} ms to its frame, over {bound} ms"
        )),
        _ => Ok(()),
    }
}

/// The run's desktop entry, so GIO has a Pronto to make the handler. It starts nothing:
/// `Exec` is `false`.
async fn install_entry(harness: &Harness) -> StepResult {
    let data = glib::user_data_dir();
    if !data.starts_with(&harness.root) {
        return Err("run reply-sandbox first: the data folder isn't the run's own".to_owned());
    }
    let id = default_handler::desktop_id(&harness.app_id);
    let path = data.join("applications").join(&id);
    fs::write(
        &path,
        "[Desktop Entry]\nType=Application\nName=Pronto (self-test)\nExec=false %u\n\
         NoDisplay=true\nMimeType=x-scheme-handler/mailto;\n",
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;
    let started = Instant::now();
    loop {
        let app_id = harness.app_id.clone();
        let installed = gtk::gio::spawn_blocking(move || default_handler::probe(&app_id))
            .await
            .map_err(|_| "the probe failed".to_owned())?
            .installed;
        if installed {
            println!("  # {} lists {id}", data.join("applications").display());
            return Ok(());
        }
        if started.elapsed() > WAIT {
            return Err(format!("GIO never listed {id}"));
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}

/// The handler GIO reports, and the sandbox's `mimeapps.list` in the report.
fn mailto_handler(harness: &Harness, wanted: &str) -> StepResult {
    let list = glib::user_config_dir().join("mimeapps.list");
    if !list.starts_with(&harness.root) {
        return Err("run reply-sandbox first: mimeapps.list isn't the run's own".to_owned());
    }
    match fs::read_to_string(&list) {
        Ok(text) => {
            println!("  # {}:", list.display());
            for line in text.lines() {
                println!("  #   {line}");
            }
        }
        Err(_) => println!("  # {} doesn't exist", list.display()),
    }
    let current = default_handler::current().map_or_else(|| "none".to_owned(), |h| h.id);
    let ours = default_handler::desktop_id(&harness.app_id);
    println!("  # {MAILTO_TYPE}: {current}");
    match wanted {
        "ours" => equal("mailto: handler", current, ours),
        "not-ours" if current != ours => Ok(()),
        "not-ours" => Err(format!("the handler is {current}")),
        other => equal("mailto: handler", current.as_str(), other),
    }
}

/// A real click on one of MESSAGE's card buttons.
async fn click_card(harness: &Harness, name: &str, which: &str) -> StepResult {
    let id = message(harness, name)?;
    let reader = reader(harness)?;
    let index = reader
        .state()
        .expanded
        .iter()
        .position(|expanded| *expanded == id)
        .ok_or_else(|| format!("{name} isn't expanded"))?;
    let card = named(&reader.widget(), "reader-card")
        .get(index)
        .cloned()
        .ok_or_else(|| format!("{name}'s card isn't on screen"))?;
    let button = named(&card, &format!("reader-card-{which}"))
        .into_iter()
        .next()
        .and_downcast::<gtk::Button>()
        .ok_or_else(|| format!("{name}'s card has no {which} button"))?;
    if let Some(viewport) = button
        .ancestor(gtk::Viewport::static_type())
        .and_downcast::<gtk::Viewport>()
    {
        viewport.scroll_to(&button, None);
    }
    harness.wait_frames(2).await?;
    triage::click_button(harness, &button).await
}

/// Mapped widgets under `root` named `name`, in order.
fn named(root: &gtk::Widget, name: &str) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    super::collect(root, &mut |widget| {
        if widget.widget_name() == name && widget.is_mapped() {
            found.push(widget.clone());
        }
    });
    found
}
