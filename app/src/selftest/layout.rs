//! Self-test commands for the window's layout since the Superhuman pass (2026-10-28): Focused
//! or Split, the sidebar beside the list, over it or hidden, and the reader on screen or warm
//! behind the list. The harness starts every window in Split with the sidebar shown, which the
//! scripts before the pass were written for; a script about the new defaults starts with
//! `layout-defaults app`.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `layout-defaults app\|harness` | Before the window's steps: the window starts as the app's defaults have it (Focused, the sidebar hidden, one row per conversation), or as the harness's (Split, the sidebar shown, one row per message; without the step) |
//! | `view-mode split\|focused` | Turns the reading pane on or off, as the Reading Pane toggle does (remembered in layout.toml) |
//! | `grouping conversations\|messages` | One list row per conversation or per message, as Group by Conversation does (remembered in layout.toml) |
//! | `sidebar shown\|hidden` | Shows or hides the sidebar, as Toggle Sidebar does (`sidebar sample\|none` sets its model, mod.rs) |
//! | `assert-view-mode split\|focused` | The window's mode |
//! | `assert-sidebar inline\|overlay\|hidden` | Where the sidebar is |
//! | `assert-reader-on-screen BOOL` | The reader is on screen (what marks a conversation read) |
//! | `assert-reader-warm BOOL` | The reader is laid out behind the list, unseen |
//! | `assert-search-shown BOOL` | The list's search field shows |
//! | `assert-action-state NAME BOOL` | A toggle's state (`win.toggle-sidebar`) |
//! | `assert-focus-row POS` | The keyboard focus is on the list's row POS |
//! | `measure-open COUNT [MS]` | Opens the selected conversation (Enter) and goes back (Escape) COUNT times, each round after the first on the next row once the reader has laid it out behind the list (`j`), timing each from the pane's change to the first frame painted with it, and from the key press; with MS, fails when an open took longer |
//! | `assert-row-part POS PART EDGE PX` | Row POS's PART (`gutter`, `dot`, `check`, `sender`, `names`, `subject`, `snippet`, `clip`, `star`, `time`, `quick`) has its `left` or `right` edge at PX in the window, or is PX `width` or `height`, within a pixel (phase B's geometry locks) |
//! | `assert-rows-aligned PART` | Every loaded row bound now has PART at the same left and right edges |
//! | `assert-row-heights PX` | Every loaded row bound now is PX high (38 on one line, 57 on two, whatever its text) |
//! | `assert-quick-rows POS,…\|none` | The rows bound now that show their quick actions |
//! | `assert-row-dot POS BOOL` | Row POS shows its unread dot |
//! | `text-direction ltr\|rtl` | Lays every widget out left to right or right to left, as a right-to-left locale does |
//! | `assert-edge NAME left\|right\|top\|bottom\|width\|height PX [icon]` | The widget NAME (with `icon`, the glyph of the first icon in it) has that edge at PX in the window, within a pixel |
//! | `assert-drawn-inside NAME PX` | Drawn alone, the widget NAME leaves its PX px nearest its left and right edges blank (its focus ring kept inside it, phase C) |
//! | `assert-drawn NAME BOOL` | Drawn alone, the widget NAME draws something (a CSS opacity of 0 draws nothing: a card's actions until hovered) |
//! | `assert-seamless NAME PX` | Drawn alone, the widget NAME is one colour down the middle of its top PX px: no line across it (Inbox Zero's tint under the list's header, phase C) |
//! | `assert-glyph NAME star-outline\|mail-read\|theme` | The first icon in the widget NAME is one of the full-strength glyphs drawn for Adwaita's faint ones (`window::glyph`, phase C), or a theme icon |

use super::script::{Step, boolean};
use super::{Harness, StepResult, WAIT_TIMEOUT, equal, number};
use crate::list::row::MessageRow;
use crate::window::ViewMode;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const COMMANDS: &[(&str, usize, usize)] = &[
    // Before the app starts; only before every other step but `desktop`.
    ("layout-defaults", 1, 1),
    ("view-mode", 1, 1),
    ("grouping", 1, 1),
    ("assert-view-mode", 1, 1),
    ("assert-sidebar", 1, 1),
    ("assert-reader-on-screen", 1, 1),
    ("assert-reader-warm", 1, 1),
    ("assert-search-shown", 1, 1),
    ("assert-action-state", 2, 2),
    ("assert-focus-row", 1, 1),
    ("measure-open", 1, 2),
    ("assert-row-part", 4, 4),
    ("assert-rows-aligned", 1, 1),
    ("assert-row-heights", 1, 1),
    ("assert-quick-rows", 1, 1),
    ("assert-row-dot", 2, 2),
    ("assert-edge", 3, 4),
    ("assert-glyph", 2, 2),
    ("assert-seamless", 2, 2),
    ("assert-drawn-inside", 2, 2),
    ("assert-drawn", 2, 2),
    ("text-direction", 1, 1),
];

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

/// Whether the script asks for the app's own defaults (a leading `layout-defaults app`); an
/// unknown word, or the step after the window's first, is an error.
pub fn app_defaults(steps: &[Step]) -> Result<bool, String> {
    let mut leading = true;
    let mut app = false;
    for step in steps {
        match step.command.as_str() {
            "desktop" | "store-dir" | "fixtures" => {}
            "layout-defaults" if !leading => {
                return Err(format!(
                    "line {}: layout-defaults must come before the window's steps",
                    step.line
                ));
            }
            "layout-defaults" => {
                app = match step.args[0].as_str() {
                    "app" => true,
                    "harness" => false,
                    other => {
                        return Err(format!(
                            "line {}: layout-defaults takes app or harness, not {other}",
                            step.line
                        ));
                    }
                };
            }
            _ => leading = false,
        }
    }
    Ok(app)
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    let window = harness.window()?;
    match command {
        // Applied before the app started (`app_defaults`).
        "layout-defaults" => Ok(()),
        "view-mode" => {
            let split = match ViewMode::from_name(arg(0)) {
                Some(mode) => mode == ViewMode::Split,
                None => return Err(format!("use split or focused, not {}", arg(0))),
            };
            set_toggle(harness, "toggle-reading-pane", split).await
        }
        "grouping" => {
            let conversations = match arg(0) {
                "conversations" => true,
                "messages" => false,
                other => return Err(format!("use conversations or messages, not {other}")),
            };
            set_toggle(harness, "group-by-conversation", conversations).await?;
            super::mail::settled(harness).await
        }
        "assert-view-mode" => equal("mode", window.view_mode().name(), arg(0)),
        "assert-sidebar" => equal("sidebar", window.sidebar_place().name(), arg(0)),
        "assert-reader-on-screen" => equal(
            "reader on screen",
            window.reader_on_screen(),
            boolean(arg(0))?,
        ),
        "assert-reader-warm" => equal(
            "reader warm behind the list",
            window.reader_is_warm(),
            boolean(arg(0))?,
        ),
        "assert-search-shown" => equal(
            "search field shown",
            super::mail::list(harness)?.shows_search(),
            boolean(arg(0))?,
        ),
        "assert-action-state" => {
            let name = arg(0).strip_prefix("win.").unwrap_or(arg(0));
            let state = window
                .window()
                .lookup_action(name)
                .and_then(|action| action.state())
                .and_then(|state| state.get::<bool>())
                .ok_or_else(|| format!("{} is no toggle of the window", arg(0)))?;
            equal(&format!("{}'s state", arg(0)), state, boolean(arg(1))?)
        }
        "assert-focus-row" => {
            let focus = gtk::prelude::GtkWindowExt::focus(window.window())
                .ok_or("nothing has the focus")?;
            // GtkListView focuses the item's own widget, whose child the row is.
            let row = focus
                .clone()
                .downcast::<MessageRow>()
                .ok()
                .or_else(|| focus.ancestor(MessageRow::static_type()).and_downcast())
                .or_else(|| focus.first_child().and_downcast::<MessageRow>());
            let position = row.and_then(|row| row.position()).ok_or_else(|| {
                format!(
                    "the focus is on {} {}, no row",
                    focus.type_().name(),
                    focus.widget_name()
                )
            })?;
            equal("focused row", position, number(arg(0))? as u32)
        }
        "measure-open" => {
            let bound = args.get(1).map(|ms| number(ms)).transpose()?;
            measure_open(harness, number(arg(0))?, bound).await
        }
        "assert-row-part" => {
            let list = super::mail::list(harness)?;
            let position = number(arg(0))? as u32;
            let row = list
                .row_widget(position)
                .ok_or_else(|| format!("row {position} isn't bound"))?;
            let bounds = row
                .part_bounds(arg(1), window.window())
                .ok_or_else(|| format!("row {position} lays out no {}", arg(1)))?;
            let value = edge(&bounds, arg(2))?;
            near(
                &format!("row {position}'s {} {}", arg(1), arg(2)),
                value,
                arg(3),
            )
        }
        "assert-rows-aligned" => {
            let rows = loaded_rows(harness)?;
            let mut edges: Vec<(u32, f32, f32)> = Vec::new();
            for row in &rows {
                let Some(bounds) = row.part_bounds(arg(0), window.window()) else {
                    continue;
                };
                edges.push((
                    row.position().unwrap_or_default(),
                    bounds.x(),
                    bounds.x() + bounds.width(),
                ));
            }
            let Some((first, left, right)) = edges.first().copied() else {
                return Err(format!("no loaded row lays out {}", arg(0)));
            };
            println!("  # {} rows' {} at {left}..{right} px", edges.len(), arg(0));
            match edges.iter().find(|(_, l, r)| {
                (l - left).abs() > super::TOLERANCE || (r - right).abs() > super::TOLERANCE
            }) {
                Some((position, l, r)) => Err(format!(
                    "row {position}'s {} is at {l}..{r} px, row {first}'s at {left}..{right}",
                    arg(0)
                )),
                None => Ok(()),
            }
        }
        "assert-row-heights" => {
            let wanted = number(arg(0))? as i32;
            let rows = loaded_rows(harness)?;
            println!("  # {} loaded rows bound", rows.len());
            match rows.iter().find(|row| row.height() != wanted) {
                Some(row) => Err(format!(
                    "row {} is {} px high, not {wanted}",
                    row.position().unwrap_or_default(),
                    row.height()
                )),
                None => Ok(()),
            }
        }
        "assert-quick-rows" => {
            let list = super::mail::list(harness)?;
            let mut shown: Vec<u32> = list
                .bound_rows()
                .iter()
                .filter_map(|bound| bound.position)
                .filter(|position| {
                    list.row_widget(*position)
                        .is_some_and(|row| row.shows_quick())
                })
                .collect();
            shown.sort_unstable();
            let text = if shown.is_empty() {
                "none".to_owned()
            } else {
                shown
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            };
            equal("rows showing quick actions", text.as_str(), arg(0))
        }
        "assert-row-dot" => {
            let position = number(arg(0))? as u32;
            let row = super::mail::list(harness)?
                .row_widget(position)
                .ok_or_else(|| format!("row {position} isn't bound"))?;
            equal(
                &format!("row {position}'s unread dot"),
                row.shows_dot(),
                boolean(arg(1))?,
            )
        }
        "text-direction" => {
            gtk::Widget::set_default_direction(match arg(0) {
                "ltr" => gtk::TextDirection::Ltr,
                "rtl" => gtk::TextDirection::Rtl,
                other => return Err(format!("use ltr or rtl, not {other}")),
            });
            harness.wait_until_settled().await;
            Ok(())
        }
        "assert-edge" => {
            let widget = harness.widget(arg(0))?;
            let target = match args.get(3).map(String::as_str) {
                None => widget,
                Some("icon") => {
                    first_icon(&widget).ok_or_else(|| format!("{} holds no icon", arg(0)))?
                }
                Some(other) => return Err(format!("use icon or nothing, not {other}")),
            };
            let mut bounds = target
                .compute_bounds(window.window())
                .ok_or_else(|| format!("{} has no bounds", arg(0)))?;
            // An icon draws its glyph centred in what it is given: the glyph's own edges.
            if target.is::<gtk::Image>() {
                let (_, glyph, _, _) = target.measure(gtk::Orientation::Horizontal, -1);
                let glyph = glyph as f32;
                bounds = gtk::graphene::Rect::new(
                    bounds.x() + (bounds.width() - glyph) / 2.0,
                    bounds.y(),
                    glyph,
                    bounds.height(),
                );
            }
            near(
                &format!("{}'s {} edge", arg(0), arg(1)),
                edge(&bounds, arg(1))?,
                arg(2),
            )
        }
        "assert-drawn-inside" => {
            let widget = harness.widget(arg(0))?;
            let drawing = super::probe::Drawing::of(&widget)?;
            let margin = number(arg(1))?.min(drawing.width / 2);
            let columns = (0..margin).chain(drawing.width - margin..drawing.width);
            for x in columns {
                if let Some(y) = (0..drawing.height).find(|y| drawing.pixel(x, *y).1 > 0) {
                    return Err(format!("{} draws at x {x}, y {y}", arg(0)));
                }
            }
            Ok(())
        }
        "assert-drawn" => {
            // Whether the widget draws anything at all (a CSS opacity of 0 draws nothing).
            let widget = harness.widget(arg(0))?;
            let drawing = super::probe::Drawing::of(&widget)?;
            let drawn =
                (0..drawing.height).any(|y| (0..drawing.width).any(|x| drawing.pixel(x, y).1 > 0));
            equal(&format!("{} drawn", arg(0)), drawn, boolean(arg(1))?)
        }
        "assert-seamless" => {
            let widget = harness.widget(arg(0))?;
            let drawing = super::probe::Drawing::of(&widget)?;
            let depth = number(arg(1))?.min(drawing.height);
            let x = drawing.width / 2;
            let top = drawing.pixel(x, 0);
            match (0..depth).find(|y| drawing.pixel(x, *y) != top) {
                None => Ok(()),
                Some(y) => Err(format!(
                    "{} is {:?} at y {y}, {:?} above it",
                    arg(0),
                    drawing.pixel(x, y),
                    top
                )),
            }
        }
        "assert-glyph" => {
            let widget = harness.widget(arg(0))?;
            let image = first_icon(&widget)
                .and_downcast::<gtk::Image>()
                .ok_or_else(|| format!("{} holds no icon", arg(0)))?;
            let shown = match crate::window::glyph::shown(&image) {
                Some(crate::window::glyph::Glyph::StarOutline) => "star-outline",
                Some(crate::window::glyph::Glyph::MailRead) => "mail-read",
                None => "theme",
            };
            equal(&format!("{}'s glyph", arg(0)), shown, arg(1))
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// The rows bound now whose row is loaded (not a placeholder).
fn loaded_rows(harness: &Harness) -> Result<Vec<MessageRow>, String> {
    let list = super::mail::list(harness)?;
    Ok(list
        .bound_rows()
        .iter()
        .filter(|bound| !bound.placeholder)
        .filter_map(|bound| bound.position)
        .filter_map(|position| list.row_widget(position))
        .filter(|row| row.is_mapped())
        .collect())
}

/// One edge or size of `bounds`, in px.
fn edge(bounds: &gtk::graphene::Rect, which: &str) -> Result<f32, String> {
    Ok(match which {
        "left" => bounds.x(),
        "right" => bounds.x() + bounds.width(),
        "top" => bounds.y(),
        "bottom" => bounds.y() + bounds.height(),
        "width" => bounds.width(),
        "height" => bounds.height(),
        other => {
            return Err(format!(
                "use left, right, top, bottom, width or height, not {other}"
            ));
        }
    })
}

/// `value` is `wanted` px, within the harness's tolerance.
fn near(what: &str, value: f32, wanted: &str) -> StepResult {
    let wanted = number(wanted)? as f32;
    println!("  # {what}: {value} px");
    if (value - wanted).abs() <= super::TOLERANCE {
        Ok(())
    } else {
        Err(format!("{what} is at {value} px, not {wanted}"))
    }
}

/// The first icon in `widget`, depth first.
fn first_icon(widget: &gtk::Widget) -> Option<gtk::Widget> {
    if widget.is::<gtk::Image>() {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = first_icon(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// `measure-open`: Enter and Escape, `count` times, each timed to the first frame painted with
/// the reader on screen, or with the list again.
async fn measure_open(harness: &Harness, count: usize, bound: Option<usize>) -> StepResult {
    let window = harness.window()?;
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    // When the reader came on screen or left it, and the first frame painted since.
    let changed: Rc<Cell<Option<Instant>>> = Rc::default();
    let painted: Rc<Cell<Option<Instant>>> = Rc::default();
    let mark = changed.clone();
    window.connect_reader_on_screen(move |_| mark.set(Some(Instant::now())));
    let (seen, paint_mark) = (changed.clone(), painted.clone());
    let paint = clock.connect_after_paint(move |_| {
        if seen.get().is_some() && paint_mark.get().is_none() {
            paint_mark.set(Some(Instant::now()));
        }
    });
    clock.begin_updating();
    let ms = |from: Instant, to: Instant| to.duration_since(from).as_secs_f64() * 1000.0;
    let mut opens = Vec::new();
    let mut backs = Vec::new();
    let mut result = Ok(());
    let list = super::mail::list(harness)?;
    let reader = harness.state.reader().ok_or("the reader isn't installed")?;
    'rounds: for round in 0..count {
        // The next conversation, laid out unseen behind the list before it is opened.
        if round > 0 {
            if let Err(error) = super::triage::press("j") {
                result = Err(error);
                break 'rounds;
            }
            let started = Instant::now();
            while (list.selected().is_none() || reader.message() != list.selected())
                && started.elapsed() < WAIT_TIMEOUT
            {
                glib::timeout_future(Duration::from_millis(5)).await;
            }
            super::mail::settled(harness).await?;
        }
        for (key, on, times) in [("Return", true, &mut opens), ("Escape", false, &mut backs)] {
            changed.set(None);
            painted.set(None);
            let pressed = Instant::now();
            if let Err(error) = super::triage::press(key) {
                result = Err(error);
                break 'rounds;
            }
            while painted.get().is_none() && pressed.elapsed() < WAIT_TIMEOUT {
                glib::timeout_future(Duration::from_millis(1)).await;
            }
            let (Some(at), Some(drawn)) = (changed.get(), painted.get()) else {
                result = Err(format!("round {round}: {key} never changed the pane"));
                break 'rounds;
            };
            if window.reader_on_screen() != on {
                result = Err(format!(
                    "round {round}: {key} left the reader on screen {}",
                    !on
                ));
                break 'rounds;
            }
            println!(
                "  # round {round}: {key} {:.1} ms from the pane's change to its frame, {:.1} ms \
                 from the key press",
                ms(at, drawn),
                ms(pressed, drawn)
            );
            times.push(ms(at, drawn));
            super::mail::settled(harness).await?;
        }
    }
    clock.end_updating();
    clock.disconnect(paint);
    result?;
    let worst = |times: &[f64]| times.iter().copied().fold(0.0_f64, f64::max);
    println!(
        "  # open: worst {:.1} ms; back: worst {:.1} ms",
        worst(&opens),
        worst(&backs)
    );
    match bound {
        Some(bound) if worst(&opens) > bound as f64 => Err(format!(
            "an open took {:.1} ms to its frame, over {bound} ms",
            worst(&opens)
        )),
        _ => Ok(()),
    }
}

/// Sets the window's toggle `name` (`toggle-sidebar`) to `on`, as its menu item does.
pub async fn set_toggle(harness: &Harness, name: &str, on: bool) -> StepResult {
    let window = harness.window()?;
    let action = window
        .window()
        .lookup_action(name)
        .ok_or_else(|| format!("no action win.{name}"))?;
    if !action.is_enabled() {
        return Err(format!("win.{name} is disabled"));
    }
    gio::prelude::ActionExt::change_state(&action, &on.to_variant());
    harness.wait_until_settled().await;
    Ok(())
}
