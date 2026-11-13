//! Self-test commands for the contact pane (the fork's Superhuman pass, phase C;
//! docs/superhuman/contact.md). The pane's widgets are named (`contact-pane`, `contact-name`,
//! `contact-address`, `contact-address-text`, `contact-history`, `contact-recent-N`,
//! `contact-subject-N`, `contact-date-N`, `contact-search`), so `click`, `assert-shown`,
//! `assert-label` and `assert-focus` reach them; these check the rest.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `assert-contact NAME ADDRESS` | The pane shows NAME and ADDRESS and what the store holds from them is in; `none none`: it shows nobody |
//! | `assert-contact-history N` | Its line says N messages (the month depends on the day the samples were filed); `0`: no line |
//! | `assert-contact-recent N [SUBJECT…]` | Its Recent list holds N conversations (with these subjects, in order) |
//! | `assert-contact-column LEFT RIGHT` | The thread's column lies from LEFT to RIGHT px in the window, the pane shown or not |
//! | `assert-contact-side right\|left` | The pane lies wholly right (left to right) or left (right to left) of the thread's column, with the gap between them; however far the column is scrolled |
//! | `assert-clipboard TEXT` | The clipboard holds TEXT |
//! | `contact-hold true\|false` | `true`: what the reads for whom the user's own conversation was written to find waits, as a slow reader thread keeps it; `false` lets it go, in order |
//! | `assert-contact-held N` | At least N such answers wait (a request may be made twice); `0`: none |
//! | `wheel-over NAME up\|down [COUNT]` | Turns the wheel COUNT notches (1) over NAME's centre |
//! | `assert-wheel-alike A B` | From the conversation's top, one notch down over A's centre and over B's scroll it as far (and some way); each waits for its scroll to land (over a message's body WebKit hands the wheel back first) |

use super::{Harness, StepResult, TOLERANCE, boolean, broadway, equal, number};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use std::time::Duration;

pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("assert-contact", 2, 2),
    ("assert-contact-history", 1, 1),
    ("assert-contact-recent", 1, usize::MAX),
    ("assert-contact-column", 2, 2),
    ("assert-contact-side", 1, 1),
    ("assert-clipboard", 1, 1),
    ("contact-hold", 1, 1),
    ("assert-contact-held", 1, 1),
    ("wheel-over", 2, 3),
    ("assert-wheel-alike", 2, 2),
];

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |n: usize| args[n].as_str();
    let pane = || {
        harness
            .state
            .contact()
            .ok_or_else(|| "there is no contact pane".to_owned())
    };
    match command {
        "assert-contact" => {
            let pane = pane()?;
            if pane.is_pending() {
                return Err("the pane waits for the reader's frame".to_owned());
            }
            match pane.shown() {
                None => equal(
                    "the person shown",
                    "none none",
                    &format!("{} {}", arg(0), arg(1)),
                ),
                Some((person, filled)) => {
                    equal(
                        "the person shown",
                        format!("{} {}", person.name, person.email).as_str(),
                        format!("{} {}", arg(0), arg(1)).as_str(),
                    )?;
                    equal("their facts in", filled, true)
                }
            }
        }
        "assert-contact-history" => {
            let label = harness
                .widget("contact-history")?
                .downcast::<gtk::Label>()
                .map_err(|_| "contact-history is no label".to_owned())?;
            let text = label.label().to_string();
            println!("  # {text:?}");
            let count = number(arg(0))?;
            let says = if !label.is_visible() || text.is_empty() {
                0
            } else {
                text.split(' ')
                    .next()
                    .and_then(|count| count.replace('\u{202f}', "").parse().ok())
                    .unwrap_or(usize::MAX)
            };
            equal("messages the line counts", says, count)
        }
        "assert-contact-recent" => {
            let pane = pane()?;
            let subjects: Vec<String> = pane
                .recent()
                .into_iter()
                .map(|found| found.subject)
                .collect();
            println!("  # {subjects:?}");
            equal("conversations listed", subjects.len(), number(arg(0))?)?;
            if args.len() > 1 {
                equal("their subjects", subjects.as_slice(), &args[1..])?;
            }
            Ok(())
        }
        "assert-contact-side" => {
            let window = harness.window()?;
            let column = column_bounds(harness)?;
            let pane = harness.widget("contact-pane")?;
            if !harness.shown(&pane)? {
                return Err("the contact pane is not on screen".to_owned());
            }
            let pane = pane
                .compute_bounds(window.window())
                .ok_or("the contact pane has no bounds")?;
            let (right, left) = (
                pane.x() - (column.x() + column.width()),
                column.x() - (pane.x() + pane.width()),
            );
            println!("  # the pane starts {right} px right of the column, ends {left} px left");
            match arg(0) {
                "right" if right >= 0.0 => Ok(()),
                "left" if left >= 0.0 => Ok(()),
                "right" | "left" => Err(format!("the pane isn't {} of the column", arg(0))),
                other => Err(format!("use right or left, not {other}")),
            }
        }
        "assert-contact-column" => {
            let bounds = column_bounds(harness)?;
            let (left, right) = (bounds.x(), bounds.x() + bounds.width());
            println!("  # the thread's column lies from {left} to {right} px");
            let (want_left, want_right) = (number(arg(0))? as f32, number(arg(1))? as f32);
            if (left - want_left).abs() <= TOLERANCE && (right - want_right).abs() <= TOLERANCE {
                Ok(())
            } else {
                Err(format!(
                    "the column lies from {left} to {right} px, not {want_left} to {want_right}"
                ))
            }
        }
        "assert-clipboard" => {
            let window = harness.window()?;
            let clipboard = gtk::prelude::WidgetExt::clipboard(window.window());
            let text = clipboard
                .read_text_future()
                .await
                .map_err(|error| error.to_string())?
                .map(|text| text.to_string())
                .unwrap_or_default();
            equal("the clipboard", text.as_str(), arg(0))
        }
        "contact-hold" => {
            pane()?.hold_recipients(boolean(arg(0))?);
            harness.wait_until_settled().await;
            Ok(())
        }
        "assert-contact-held" => {
            let held = pane()?.held_recipients();
            let wanted = number(arg(0))?;
            println!("  # {held} answers held");
            if (wanted == 0 && held == 0) || (wanted > 0 && held >= wanted) {
                Ok(())
            } else {
                Err(format!("{held} answers held, not {wanted}"))
            }
        }
        "wheel-over" => {
            let widget = harness.widget(arg(0))?;
            let down = match arg(1) {
                "down" => true,
                "up" => false,
                other => return Err(format!("use up or down, not {other}")),
            };
            let notches = args.get(2).map_or(Ok(1), |count| number(count))?;
            for _ in 0..notches {
                let (x, y) = super::triage::centre(harness, &widget)?;
                broadway::wheel(x, y, down).map_err(|error| error.to_string())?;
                glib::timeout_future(Duration::from_millis(50)).await;
            }
            harness.wait_until_settled().await;
            Ok(())
        }
        "assert-wheel-alike" => {
            let adjustment = harness
                .widget("reader-thread")?
                .ancestor(gtk::ScrolledWindow::static_type())
                .and_then(|scroller| scroller.downcast::<gtk::ScrolledWindow>().ok())
                .ok_or("the conversation has no scroller")?
                .vadjustment();
            let mut moved = Vec::new();
            for name in [arg(0), arg(1)] {
                let widget = harness.widget(name)?;
                adjustment.set_value(adjustment.lower());
                harness.wait_until_settled().await;
                let (x, y) = super::triage::centre(harness, &widget)?;
                broadway::wheel(x, y, true).map_err(|error| error.to_string())?;
                // Until it moves, then until it stays where it went.
                let started = std::time::Instant::now();
                while adjustment.value() <= adjustment.lower()
                    && started.elapsed() < Duration::from_secs(3)
                {
                    glib::timeout_future(Duration::from_millis(20)).await;
                }
                loop {
                    let at = adjustment.value();
                    glib::timeout_future(Duration::from_millis(250)).await;
                    harness.wait_until_settled().await;
                    if (adjustment.value() - at).abs() < 0.5 {
                        break;
                    }
                }
                moved.push(adjustment.value() - adjustment.lower());
            }
            adjustment.set_value(adjustment.lower());
            println!(
                "  # one notch over {} scrolls {:.1} px, over {} {:.1} px",
                arg(0),
                moved[0],
                arg(1),
                moved[1]
            );
            if moved[0] > 0.0 && (moved[0] - moved[1]).abs() <= 1.0 {
                Ok(())
            } else {
                Err(format!(
                    "one notch scrolls {:.1} px over {} and {:.1} px over {}",
                    moved[0],
                    arg(0),
                    moved[1],
                    arg(1)
                ))
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// Where the thread's column lies in the window: the clamp is as wide as its room, the thread
/// is its child. Its height runs past the window in a long conversation: only its sides count.
fn column_bounds(harness: &Harness) -> Result<gtk::graphene::Rect, String> {
    let window = harness.window()?;
    harness
        .widget("reader-thread")?
        .first_child()
        .ok_or("the thread's column is empty")?
        .compute_bounds(window.window())
        .ok_or_else(|| "the thread's column has no bounds".to_owned())
}
