//! Self-test commands for the Split Inbox (the fork's Superhuman pass, docs/superhuman/split.md):
//! the backend's (the list filtered to one side through the store's split filters, the store's
//! counts of each side, the user's choice for a sender) and phase C's tabs (the side the list
//! shows, the header's tabs and their totals, Inbox Zero's line, switching times).
//!
//! | Command | What it does |
//! | --- | --- |
//! | `split important\|other\|all` | Filters the list to one side of the Split Inbox, or to all of it, through the store's split filters (phase B's backend; the tabs show the sides without a filter) |
//! | `mail-sender-split ADDRESS important\|other\|none` | Chooses a side for every message from ADDRESS, or forgets the choice, through the store; the app's commands read the store's choices again |
//! | `stored-sender-split ADDRESS important\|other` | A choice an earlier run left in the store: written before Pronto opens it (a leading step, after `desktop`, `layout-defaults`, `store-dir` and `fixtures`) |
//! | `assert-split-counts IMPORTANT OTHER` | The rows each side of the list's view holds (its scope and grouping), as the store counts them |
//! | `assert-split-choice ADDRESS important\|other\|none` | The choice the store keeps for ADDRESS |
//! | `split-inbox on\|off\|config` | The Inbox shows its sides, or one Inbox (the harness's default: off), or as `config.toml` and `layout.toml` say; a leading one (before the window's steps, after `fixtures`) from the start, so the store reads the first view ahead as the app does |
//! | `mark-list-opens` | Notes how many views the list has opened (adopting a view opened ahead counts) |
//! | `assert-list-opens N` | The views the list opened since `mark-list-opens` |
//! | `assert-first-view adopted\|reopened\|opened\|none` | Whether the window's first view was the one the store read ahead when it opened, the list showing it still (`reopened`: it has opened another view since), or another (`opened`); `none`: none was read ahead (no account then) |
//! | `split-side important\|other [top]` | Shows that side of the Inbox shown, as a click on its tab does; with `top`, as Tab does |
//! | `assert-split-side important\|other\|none` | The side the list shows |
//! | `assert-split-tabs hidden\|totals\|bare` | The header's tabs: hidden, shown with their totals, or shown without them (a header too narrow) |
//! | `assert-split-totals IMPORTANT OTHER\|unknown` | The sides' totals the tabs show, as the list last counted them |
//! | `assert-zero-other TEXT\|none` | Inbox Zero's line on the other side's mail |
//! | `measure-split KEY COUNT MS` | Presses KEY (Tab, Shift+Tab) COUNT times and times each from the key press to the first frame painted with the new side's rows; fails when a switch opened the other side's view instead of adopting the one opened ahead, or when the median is over MS. Ends on the side it started on |

use super::script::Step;
use super::{Harness, StepResult, WAIT_TIMEOUT, equal, number};
use crate::list::model::Content;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::split::Split;
use sealwax_infrastructure::store::{Store, StoreOptions, ViewFilter};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("split", 1, 1),
    ("mail-sender-split", 2, 2),
    // Before the app starts.
    ("stored-sender-split", 2, 2),
    ("assert-split-counts", 2, 2),
    ("assert-split-choice", 2, 2),
    ("split-inbox", 1, 1),
    ("split-side", 1, 2),
    ("assert-split-side", 1, 1),
    ("assert-split-tabs", 1, 1),
    ("assert-split-totals", 1, 2),
    ("assert-zero-other", 1, 1),
    ("measure-split", 3, 3),
    ("assert-first-view", 1, 1),
    ("mark-list-opens", 0, 0),
    ("assert-list-opens", 1, 1),
];

thread_local! {
    /// The views the list had opened at `mark-list-opens`.
    static OPENS_MARK: Cell<u64> = const { Cell::new(0) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

/// A side as the commands name it: `important`, `other`, or `none` (`all` for the filter).
fn side(word: &str) -> Result<Option<Split>, String> {
    match word {
        "important" => Ok(Some(Split::Important)),
        "other" => Ok(Some(Split::Other)),
        "none" | "all" => Ok(None),
        other => Err(format!("use important, other or none, not {other}")),
    }
}

fn name(split: Option<Split>) -> &'static str {
    match split {
        Some(Split::Important) => "important",
        Some(Split::Other) => "other",
        None => "none",
    }
}

/// The Split Inbox at startup: off, one Inbox, as the scripts before it were written for, unless
/// a leading `split-inbox` says otherwise (the first view is then read ahead as the app reads it).
pub fn startup_default(steps: &[Step]) -> Option<bool> {
    for step in steps {
        match step.command.as_str() {
            "desktop" | "layout-defaults" | "store-dir" | "fixtures" | "stored-sender-split" => {}
            "split-inbox" => match step.args[0].as_str() {
                "on" => return Some(true),
                "config" => return None,
                _ => break,
            },
            _ => break,
        }
    }
    Some(false)
}

/// Runs the leading `stored-sender-split` steps before the app opens its store at `dirs`, as an
/// earlier run of Pronto leaves its choices: the store opened, the choices written, closed.
pub fn before_start(steps: &[Step], dirs: &SealwaxDirs) -> Result<(), String> {
    let mut leading = true;
    let mut choices = Vec::new();
    for step in steps {
        match step.command.as_str() {
            "desktop" | "layout-defaults" | "store-dir" | "fixtures" | "split-inbox" => {}
            "stored-sender-split" if !leading => {
                return Err(format!(
                    "line {}: stored-sender-split must come before the window's steps",
                    step.line
                ));
            }
            "stored-sender-split" => {
                let side = side(&step.args[1])
                    .ok()
                    .flatten()
                    .ok_or_else(|| format!("line {}: use important or other", step.line))?;
                choices.push((step.args[0].clone(), side));
            }
            _ => leading = false,
        }
    }
    if choices.is_empty() {
        return Ok(());
    }
    let store = Store::open(&dirs.store_file(), StoreOptions::default())
        .map_err(|error| format!("stored-sender-split: {error}"))?;
    for (address, side) in choices {
        store
            .set_sender_split(address, Some(side))
            .recv_blocking()
            .map_err(|error| format!("stored-sender-split: {error}"))?;
    }
    store
        .close()
        .map_err(|error| format!("stored-sender-split: {error}"))
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |n: usize| args.get(n).map(String::as_str).unwrap_or_default();
    let mailbox = harness.state.mailbox();
    let store = || {
        mailbox
            .store()
            .ok_or_else(|| "the store is not open".to_owned())
    };
    match command {
        "split" => {
            let list = super::mail::list(harness)?;
            list.set_filter(side(arg(0))?.map_or(ViewFilter::All, ViewFilter::of_split));
            super::mail::settled(harness).await
        }
        "mail-sender-split" => {
            let reply = store()?.set_sender_split(arg(0).to_owned(), side(arg(1))?);
            reply.recv().await.map_err(|error| error.to_string())?;
            // The app knows it as it knows the store's choices when it starts.
            crate::split_inbox::read_choices(&harness.state).await;
            super::mail::settled(harness).await
        }
        "stored-sender-split" => Ok(()),
        "mark-list-opens" => {
            let opens = super::mail::list(harness)?.model().stats().opens;
            OPENS_MARK.with(|mark| mark.set(opens));
            Ok(())
        }
        "assert-list-opens" => {
            let opens = super::mail::list(harness)?.model().stats().opens;
            let since = opens - OPENS_MARK.with(Cell::get);
            equal("views the list opened", since, number(arg(0))? as u64)
        }
        "assert-first-view" => {
            let opens = super::mail::list(harness)?.model().stats().opens;
            let outcome = match mailbox.first_view_adopted() {
                Some(true) if opens == 1 => "adopted",
                Some(true) => "reopened",
                Some(false) => "opened",
                None => "none",
            };
            equal("the first view", outcome, arg(0))
        }
        "assert-split-counts" => {
            let list = super::mail::list(harness)?;
            let Content::View(spec) = list.model().content() else {
                return Err("the list shows no folder view".to_owned());
            };
            // The store's borrow ends before the reply is awaited.
            let reply = store()?.split_counts(spec);
            let counts = reply.recv().await.map_err(|error| error.to_string())?.value;
            equal(
                "Important and Other",
                (counts.important, counts.other),
                (number(arg(0))? as u32, number(arg(1))? as u32),
            )
        }
        "assert-split-choice" => {
            let reply = store()?.sender_splits();
            let choices = reply.recv().await.map_err(|error| error.to_string())?;
            let wanted = arg(0).to_ascii_lowercase();
            let found = choices
                .iter()
                .find(|(address, _)| *address == wanted)
                .map(|(_, split)| *split);
            equal("choice", name(found), name(side(arg(1))?))
        }
        "split-inbox" => {
            let default = match arg(0) {
                "on" => Some(true),
                "off" => Some(false),
                "config" => None,
                other => return Err(format!("use on, off or config, not {other}")),
            };
            harness.state.set_split_default(default);
            super::mail::settled(harness).await
        }
        "split-side" => {
            let list = super::mail::list(harness)?;
            let wanted = side(arg(0))?.ok_or("name a side: important or other")?;
            let top = match args.get(1).map(String::as_str) {
                None => false,
                Some("top") => true,
                Some(other) => return Err(format!("use top or nothing, not {other}")),
            };
            if !list.show_split(wanted, top) {
                return Err("the list shows no Inbox with its sides".to_owned());
            }
            super::mail::settled(harness).await
        }
        "assert-split-side" => {
            let list = super::mail::list(harness)?;
            equal("the side shown", name(list.split()), name(side(arg(0))?))
        }
        "assert-split-tabs" => {
            let (shown, totals) = harness.window()?.split_tabs_shown();
            let state = match (shown, totals) {
                (false, _) => "hidden",
                (true, true) => "totals",
                (true, false) => "bare",
            };
            equal("the tabs", state, arg(0))
        }
        "assert-split-totals" => {
            let list = super::mail::list(harness)?;
            let shown = list
                .split_counts()
                .map(|counts| format!("{} {}", counts.important, counts.other))
                .unwrap_or_else(|| "unknown".to_owned());
            let wanted = args.join(" ");
            equal("the sides' totals", shown.as_str(), wanted.as_str())
        }
        "assert-zero-other" => {
            let list = super::mail::list(harness)?;
            let shown = list.zero_other();
            let shown = if shown.is_empty() {
                "none".to_owned()
            } else {
                shown
            };
            equal("Inbox Zero's other line", shown.as_str(), arg(0))
        }
        "measure-split" => {
            measure(
                harness,
                arg(0),
                number(arg(1))?,
                Duration::from_millis(number(arg(2))? as u64),
            )
            .await
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// `measure-split KEY COUNT MS`: each press of KEY switches the side; the time from the key to
/// the first frame painted with the other side's rows. It fails when a switch opened the other
/// side's view instead of adopting the one opened ahead, or when the median is over MS (one
/// slow frame of a loaded machine isn't the switch's).
async fn measure(harness: &Harness, key: &str, count: usize, bound: Duration) -> StepResult {
    let window = harness.window()?;
    let list = super::mail::list(harness)?;
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let painted: Rc<Cell<Option<Instant>>> = Rc::default();
    let mark = painted.clone();
    let paint = clock.connect_after_paint(move |_| {
        if mark.get().is_none() {
            mark.set(Some(Instant::now()));
        }
    });
    clock.begin_updating();
    let before = list.split_switches();
    let start = list.split();
    let ms = |time: Duration| time.as_secs_f64() * 1000.0;
    let mut times = Vec::new();
    let mut result = Ok(());
    for round in 0..count {
        let Some(from) = list.split() else {
            result = Err("the list shows no Inbox with its sides".to_owned());
            break;
        };
        let pressed = Instant::now();
        if let Err(error) = super::triage::press(key) {
            result = Err(error);
            break;
        }
        // The other side's rows in the model, then the frame that paints them.
        let switched = loop {
            let shows = matches!(list.model().content(), Content::View(spec)
                if spec.split.is_some() && spec.split != Some(from))
                && !list.model().is_pending();
            if shows {
                break Some(Instant::now());
            }
            if pressed.elapsed() > WAIT_TIMEOUT {
                break None;
            }
            glib::timeout_future(Duration::from_millis(1)).await;
        };
        let Some(switched) = switched else {
            result = Err(format!("round {round}: {key} never switched the side"));
            break;
        };
        painted.set(None);
        window.window().queue_draw();
        while painted.get().is_none() && pressed.elapsed() < WAIT_TIMEOUT {
            glib::timeout_future(Duration::from_millis(1)).await;
        }
        let Some(drawn) = painted.get() else {
            result = Err(format!("round {round}: no frame after the switch"));
            break;
        };
        let took = drawn.duration_since(pressed);
        println!(
            "  # round {round}: {:.1} ms from {key} to the frame ({:.1} ms to the rows)",
            ms(took),
            ms(switched.duration_since(pressed))
        );
        times.push(ms(took));
        if let Err(error) = super::mail::settled(harness).await {
            result = Err(error);
            break;
        }
    }
    clock.end_updating();
    clock.disconnect(paint);
    result?;
    let after = list.split_switches();
    // Back to the side it started on.
    if let Some(start) = start
        && list.split() != Some(start)
    {
        list.show_split(start, true);
        super::mail::settled(harness).await?;
    }
    let (adopted, opened) = (after.0 - before.0, after.1 - before.1);
    times.sort_by(f64::total_cmp);
    let median = times.get(times.len() / 2).copied().unwrap_or_default();
    let worst = times.last().copied().unwrap_or_default();
    println!(
        "  # {} switches: median {median:.1} ms, worst {worst:.1} ms; {adopted} adopted the \
         view opened ahead, {opened} opened it",
        times.len()
    );
    if opened > 0 {
        return Err(format!(
            "{opened} of {} switches opened the other side's view instead of adopting the one \
             opened ahead",
            adopted + opened
        ));
    }
    if median > ms(bound) {
        return Err(format!(
            "the median switch took {median:.1} ms, over {} ms",
            bound.as_millis()
        ));
    }
    Ok(())
}
