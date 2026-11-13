//! Self-test commands for the writing track's part 2 (the fork's phase B): the pickers over the
//! window (the time picker of Send Later, the snippet picker of Use Snippet…), the snippets
//! `snippets.toml` gave, where the inline reply's caret is, when the stand-in's last send was to
//! go, and Scheduled Messages. Keys go through Broadway as real presses (`key`, `type`); these
//! commands read and drive the picker itself. Every name and text is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `picker-type TEXT` | Puts TEXT in the open picker's field, as typing it would |
//! | `picker-move STEP` | Moves the picker's choice as Down (positive) or Up (negative) does |
//! | `picker-choose [TITLE]` | Chooses the chosen row, as Enter does, or the row titled TITLE |
//! | `picker-close` | Closes the picker without a choice |
//! | `open-draft NAME` | Opens the outbox's draft NAME (`outbox-name`) in a composer card, as Edit Draft does |
//! | `picker-shown-in MS` | The picker's last opening reached the screen within MS, from its command to its first painted frame |
//! | `assert-picker FIELD VALUE [DETAIL]` | The picker: `shown BOOL`, `name NAME` (`time-picker`, `snippet-picker`), `title TEXT`, `query TEXT`, `rows N`, `row N TITLE`, `has TITLE [DETAIL]` (a row so titled, with that detail), `lacks TITLE`, `chosen TITLE` (the row Enter chooses), `icon NAME` (the field's glyph), `details column\|end` (the rows' details in one column, each starting at the same x, or at their ends) |
//! | `assert-snippets N` | `snippets.toml` gave N snippets (the last read without mistakes) |
//! | `assert-quick-caret N` | The inline reply's caret is N characters into its text |
//! | `assert-sent-at WHEN\|none` | The stand-in's last send was to go at WHEN, as the window says it (`tomorrow 09:00`), or after the undo window |
//! | `assert-scheduled N` | Scheduled Messages shows N sends |
//! | `assert-scheduled-sends N` | The outbox, as the window last read it, holds N scheduled sends |
//! | `send-window MS\|setting` | Sends wait MS (`triage-undo-window` sets the triage window and this one), or `[compose] undo_send_seconds` again |
//! | `assert-toast-clear` | A toast shows, and no toast on screen overlaps a composer card the dock shows, nor the inline reply while it is open |
//! | `quick-send-at past\|soon\|MS` | Sends the inline reply later, as the time picker's choice does, at a minute ago, in 30 s, or MS from now |

use super::script::boolean;
use super::{Harness, StepResult, equal, number};
use crate::picker::{self, Picker};

/// The commands and their argument counts, for the script parser.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("picker-type", 1, 1),
    ("picker-move", 1, 1),
    ("picker-choose", 0, 1),
    ("picker-close", 0, 0),
    ("picker-shown-in", 1, 1),
    ("open-draft", 1, 1),
    ("assert-picker", 2, 3),
    ("assert-snippets", 1, 1),
    ("assert-quick-caret", 1, 1),
    ("assert-sent-at", 1, 1),
    ("assert-scheduled", 1, 1),
    ("assert-scheduled-sends", 1, 1),
    ("send-window", 1, 1),
    ("assert-toast-clear", 0, 0),
    ("quick-send-at", 1, 1),
];

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

fn picker(harness: &Harness) -> Result<Picker, String> {
    Ok(picker::of(harness.window()?.window()))
}

fn open_picker(harness: &Harness) -> Result<Picker, String> {
    let picker = picker(harness)?;
    if picker.is_open() {
        Ok(picker)
    } else {
        Err("no picker is open".to_owned())
    }
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "picker-type" => {
            open_picker(harness)?.set_query(arg(0));
            Ok(())
        }
        "picker-move" => {
            let step: i32 = arg(0)
                .parse()
                .map_err(|_| format!("expected a step, got {}", arg(0)))?;
            open_picker(harness)?.move_selection(step);
            Ok(())
        }
        "picker-choose" => {
            let picker = open_picker(harness)?;
            if let Some(title) = args.first() {
                let index = picker
                    .rows()
                    .iter()
                    .position(|row| row.title == *title)
                    .ok_or_else(|| format!("no row titled {title}"))?;
                if !picker.select(index) {
                    return Err(format!("{title} can't be chosen"));
                }
            }
            if picker.activate_selected() {
                Ok(())
            } else {
                Err("no row can be chosen".to_owned())
            }
        }
        "picker-close" => {
            open_picker(harness)?.close();
            Ok(())
        }
        "send-window" => {
            let window = match arg(0) {
                "setting" => None,
                ms => Some(std::time::Duration::from_millis(number(ms)? as u64)),
            };
            crate::outbox::set_send_window(window);
            Ok(())
        }
        "open-draft" => {
            let (id, _) = super::outbox::draft(arg(0))?;
            crate::composer::open(&harness.app, crate::composer::ComposeRequest::Draft(id));
            Ok(())
        }
        "picker-shown-in" => {
            let shown = picker(harness)?
                .shown_in()
                .ok_or("the picker hasn't been painted yet")?;
            let ms = shown.as_secs_f64() * 1000.0;
            println!("  # picker on screen in {ms:.1} ms");
            if ms > number(arg(0))? as f64 {
                return Err(format!(
                    "the picker took {ms:.1} ms, more than {} ms",
                    arg(0)
                ));
            }
            Ok(())
        }
        "assert-picker" => assert_picker(harness, args),
        "assert-snippets" => {
            let prefs = harness
                .state
                .preferences()
                .ok_or("the settings aren't read")?;
            equal("snippets", prefs.snippets().len(), number(arg(0))?)
        }
        "assert-quick-caret" => {
            let quick = harness
                .state
                .reader()
                .and_then(|reader| reader.quick_reply())
                .ok_or("no inline reply shows")?;
            equal("the inline reply's caret", quick.caret, number(arg(0))?)
        }
        "assert-sent-at" => {
            let stand_in = crate::composer::stand_in().ok_or("the stand-in isn't the backend")?;
            let at = stand_in
                .sent_at()
                .pop()
                .ok_or("the stand-in sent nothing")?;
            let when = at.map_or_else(|| "none".to_owned(), crate::time_picker::describe);
            equal("when the last send was to go", when.as_str(), arg(0))
        }
        "assert-scheduled" => equal(
            "sends Scheduled Messages shows",
            crate::scheduled::shown().len(),
            number(arg(0))?,
        ),
        "assert-scheduled-sends" => equal(
            "scheduled sends",
            crate::outbox::scheduled_sends().len(),
            number(arg(0))?,
        ),
        "assert-toast-clear" => toast_clear(harness),
        "quick-send-at" => {
            let reader = harness.state.reader().ok_or("there is no reader")?;
            let now = gtk4::glib::real_time() / 1000;
            let at = match arg(0) {
                "past" => now - 60_000,
                "soon" => now + 30_000,
                ms => now + number(ms)? as i64,
            };
            reader.send_quick_reply_at(at);
            Ok(())
        }
        other => Err(format!("writing: unknown command {other}")),
    }
}

/// Every toast on screen is clear of the composer's cards (`dock::keep_toasts_clear`).
fn toast_clear(harness: &Harness) -> StepResult {
    use gtk4::prelude::*;
    let window = harness.window()?;
    let root = window.window();
    let toast_type =
        gtk4::glib::Type::from_name("AdwToastWidget").ok_or("no toast has been shown")?;
    let mut toasts = Vec::new();
    super::collect(root.upcast_ref(), &mut |widget| {
        if widget.type_().is_a(toast_type) && widget.is_mapped() {
            toasts.push(widget.clone());
        }
    });
    if toasts.is_empty() {
        return Err("no toast shows".to_owned());
    }
    let mut cards: Vec<_> = window
        .dock()
        .cards()
        .into_iter()
        .filter(|card| card.is_mapped() && card.is_child_visible())
        .filter_map(|card| card.compute_bounds(root))
        .collect();
    // The inline reply while it is open, as the cards (phase C).
    let open = harness
        .state
        .reader()
        .and_then(|reader| reader.quick_reply())
        .is_some_and(|quick| quick.open);
    if open
        && let Ok(reply) = harness.widget("quick-reply")
        && let Some(bounds) = reply.compute_bounds(root)
    {
        println!(
            "  # the open reply from y {:.0} to {:.0}",
            bounds.y(),
            bounds.y() + bounds.height()
        );
        cards.push(bounds);
    }
    for toast in toasts {
        // Its box, without the margins around it.
        let bounds = toast.compute_bounds(root).ok_or("a toast has no bounds")?;
        let (left, right) = (bounds.x(), bounds.x() + bounds.width());
        println!("  # toast from x {left:.0} to {right:.0}");
        for card in &cards {
            let (card_left, card_right) = (card.x(), card.x() + card.width());
            let (top, bottom) = (bounds.y(), bounds.y() + bounds.height());
            let overlaps = left < card_right
                && card_left < right
                && top < card.y() + card.height()
                && card.y() < bottom;
            if overlaps {
                return Err(format!(
                    "a toast ({left:.0}–{right:.0}, y {top:.0}–{bottom:.0}) covers a card or the \
                     reply ({card_left:.0}–{card_right:.0}, y {:.0}–{:.0})",
                    card.y(),
                    card.y() + card.height()
                ));
            }
        }
    }
    Ok(())
}

fn assert_picker(harness: &Harness, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    let picker = picker(harness)?;
    if arg(0) == "shown" {
        return equal("the picker shows", picker.is_open(), boolean(arg(1))?);
    }
    if !picker.is_open() {
        return Err("no picker is open".to_owned());
    }
    let rows = picker.rows();
    let detail = args.get(2).map(String::as_str);
    let titles = || {
        rows.iter()
            .map(|row| format!("{} [{}]", row.title, row.detail))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    match arg(0) {
        "name" => equal(
            "the picker",
            picker.name().unwrap_or_default().as_str(),
            arg(1),
        ),
        "title" => equal(
            "the picker's title",
            picker.title().unwrap_or_default().as_str(),
            arg(1),
        ),
        "query" => equal("the picker's field", picker.query().as_str(), arg(1)),
        "rows" => equal("rows", rows.len(), number(arg(1))?)
            .map_err(|error| format!("{error}: {}", titles())),
        "row" => {
            let index = number(arg(1))?;
            let row = rows
                .get(index)
                .ok_or_else(|| format!("no row {index}: {}", titles()))?;
            equal(&format!("row {index}"), row.title.as_str(), arg(2))
        }
        "has" => {
            let found = rows
                .iter()
                .find(|row| row.title == arg(1))
                .ok_or_else(|| format!("no row titled {}: {}", arg(1), titles()))?;
            match detail {
                Some(detail) => equal(
                    &format!("{}'s detail", arg(1)),
                    found.detail.as_str(),
                    detail,
                ),
                None => Ok(()),
            }
        }
        "lacks" => match rows.iter().find(|row| row.title == arg(1)) {
            Some(_) => Err(format!("a row is titled {}: {}", arg(1), titles())),
            None => Ok(()),
        },
        "chosen" => {
            let chosen = picker
                .selected()
                .and_then(|index| rows.get(index))
                .map(|row| row.title.clone())
                .unwrap_or_default();
            equal("the row Enter chooses", chosen.as_str(), arg(1))
        }
        "icon" => equal(
            "the field's icon",
            picker.field_icon().unwrap_or_default().as_str(),
            arg(1),
        ),
        "details" => {
            let column = picker.details() == picker::Details::Column;
            let wanted = match arg(1) {
                "column" => true,
                "end" => false,
                other => return Err(format!("details are column or end, not {other}")),
            };
            equal("details in a column", column, wanted)?;
            if column {
                // Every detail drawn starts where the first does.
                let starts = picker.detail_starts();
                println!("  # details start at {starts:?}");
                let first = starts.first().copied().ok_or("no detail is drawn")?;
                if starts.iter().any(|start| (start - first).abs() > 0.5) {
                    return Err(format!("details start at {starts:?}"));
                }
            }
            Ok(())
        }
        other => Err(format!(
            "assert-picker checks shown, name, title, query, rows, row, has, lacks, chosen, icon \
             or details, not {other}"
        )),
    }
}
