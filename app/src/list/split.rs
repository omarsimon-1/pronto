//! The Split Inbox in the list (the fork's phase C, docs/superhuman/split.md "UI"). An Inbox or
//! All Inboxes shows one side at a time, Important or Other: the store's view of that side
//! (`ViewSpec::split`) under the filter shown, so Unread or Starred narrow the side. Other
//! folders, search results and the splits switched off show everything, as before.
//!
//! - **Tabs**: the window's header shows both sides with their totals
//!   ([`MessageList::connect_split_changed`]), counted by the store whatever the filter.
//! - **Switching** (Tab, Shift+Tab, a click on a tab, `g i`, `g o`, the palette) is instant: the
//!   other side's view is kept open ahead, with the change events since ([`Warm`]), and the
//!   model adopts it in the key's own frame. Tab puts the cursor on the new side's top row
//!   (Superhuman's); the other ways, and coming back from another folder, find the side as it
//!   was left, its row under the cursor and its scroll, while the app runs.
//! - **Inbox Zero** shows for the side that is empty, with a line on the other side's mail.

use super::MessageList;
use super::model::Content;
use super::sections::Thresholds;
use crate::accessibility;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::{Binding, Keymap};
use sealwax_domain::model::FolderRole;
use sealwax_domain::split::Split;
use sealwax_infrastructure::store::{
    ChangeEvent, OpenView, RowKey, SplitCounts, ViewDiff, ViewFilter, ViewGrouping, ViewOpened,
    ViewScope, ViewSpec,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Change events a warm view keeps for its adoption; past them it is opened again, which costs
/// less than replaying them.
const WARM_EVENTS: usize = 128;

/// The list's accessible description while it shows a side, with Next Split's and Previous
/// Split's keys as `keys.toml` gives them: "Tab and Shift+Tab switch between Important and
/// Other; Ctrl+Tab leaves the list" by default (Ctrl+Tab while Tab is one of them). Empty when
/// neither has a key.
pub(super) fn split_keys(keymap: &Keymap) -> String {
    let ids = [ActionId::NextSplit, ActionId::PreviousSplit];
    let keys: Vec<String> = ids
        .iter()
        .filter_map(|id| keymap.shown(*id).first().map(Binding::display))
        .collect();
    let switch = match keys.as_slice() {
        [] => return String::new(),
        [one] => format!("{one} switches between Important and Other"),
        [next, previous, ..] => format!("{next} and {previous} switch between Important and Other"),
    };
    let takes_tab = ids.iter().any(|id| {
        keymap
            .shown(*id)
            .iter()
            .any(|key| matches!(key.display().as_str(), "Tab" | "Shift+Tab"))
    });
    if takes_tab {
        format!("{switch}; Ctrl+Tab leaves the list")
    } else {
        switch
    }
}

type SplitHandler = Rc<dyn Fn(Option<Split>, Option<SplitCounts>)>;

/// What the split handlers were told: the side shown, the totals, and the rows they count.
type Told = (Option<Split>, Option<SplitCounts>, ViewGrouping);

/// A side's place: the row under the cursor and how far the list was scrolled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Place {
    key: Option<RowKey>,
    scroll: f64,
}

/// What the side shown next does once its rows are in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Arrival {
    /// The cursor on the top row (Tab).
    Top,
    /// As the side was left.
    At(Place),
}

/// The other side's view, opened ahead of a switch, and the change events since: adopting it
/// replays them, as for a view that was opening.
struct Warm {
    spec: ViewSpec,
    thresholds: Thresholds,
    opened: Option<ViewOpened>,
    events: Vec<Arc<ChangeEvent>>,
    /// Bumped by every open, so a late reply for an older one is dropped.
    generation: u64,
}

/// What the list keeps of the Split Inbox.
#[derive(Default)]
pub(super) struct Splits {
    /// The Inbox views show their sides (`[inbox] split`, Split Inbox in the palette).
    on: Cell<bool>,
    /// The side shown, in an Inbox view with the splits on; `None` elsewhere.
    pub(super) shown: Cell<Option<Split>>,
    /// Each scope's side last shown (Important until another was).
    last: RefCell<HashMap<ViewScope, Split>>,
    /// The side the next Inbox shown opens on, rather than its last ([`MessageList::want_split`]).
    wanted: Cell<Option<Split>>,
    /// Each side's place in each scope, while the app runs.
    places: RefCell<HashMap<(ViewScope, Split), Place>>,
    pub(super) arriving: Cell<Option<Arrival>>,
    /// The sides' totals as the store last counted them; a count on its way, and another
    /// wanted when it comes.
    counts: Cell<Option<SplitCounts>>,
    counting: Cell<bool>,
    count_again: Cell<bool>,
    /// What the handlers were told last, with the grouping the totals count in.
    told: Cell<Option<Told>>,
    warm: RefCell<Option<Warm>>,
    warm_generation: Cell<u64>,
    /// The list's accessible description while it shows a side ([`split_keys`]).
    keys: RefCell<String>,
    /// Switches that adopted the warm view, and those that opened it (the self-test).
    adopted: Cell<u64>,
    opened: Cell<u64>,
    handlers: RefCell<Vec<SplitHandler>>,
}

impl MessageList {
    /// Shows the Inbox views' sides (`true`) or everything in them, as now.
    pub fn set_splits(&self, on: bool) {
        let splits = &self.inner.splits;
        if splits.on.replace(on) == on {
            return;
        }
        let scope = self.scope();
        self.leave_split();
        splits.shown.set(self.split_for(scope));
        splits.arriving.set(self.arrival_for(scope));
        if !on {
            splits.warm.take();
        }
        if scope.is_some() {
            self.carry(self.grouping());
            self.reload();
        }
        self.tell_split();
    }

    /// Whether the Inbox views show their sides.
    pub fn splits_on(&self) -> bool {
        self.inner.splits.on.get()
    }

    /// The side the list shows: in an Inbox view with the splits on, also while a search runs
    /// from it (leaving the search shows it again).
    pub fn split(&self) -> Option<Split> {
        self.inner.splits.shown.get()
    }

    /// Whether the list shows one side now, no search over it: Tab switches the sides.
    pub fn shows_splits(&self) -> bool {
        self.split().is_some() && self.inner.query.borrow().is_empty()
    }

    /// Each side's total, as the store last counted it (rows of the list's grouping).
    pub fn split_counts(&self) -> Option<SplitCounts> {
        self.inner.splits.counts.get()
    }

    /// Whether the sides' totals are being counted, or the other side's view opened ahead (the
    /// self-test waits for both).
    pub fn splits_busy(&self) -> bool {
        let splits = &self.inner.splits;
        splits.counting.get()
            || splits
                .warm
                .borrow()
                .as_ref()
                .is_some_and(|warm| warm.opened.is_none())
    }

    /// Runs `handler` with the side shown and the sides' totals, whenever either changes.
    pub fn connect_split_changed(
        &self,
        handler: impl Fn(Option<Split>, Option<SplitCounts>) + 'static,
    ) {
        self.inner
            .splits
            .handlers
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Shows `side` of the Inbox shown: with `top`, the cursor on its top row (Tab); else as
    /// it was left. False outside an Inbox view with the splits on.
    pub fn show_split(&self, side: Split, top: bool) -> bool {
        let splits = &self.inner.splits;
        let Some(shown) = splits.shown.get() else {
            return false;
        };
        if !self.inner.query.borrow().is_empty() {
            // Over a search, the side is where the search's end goes back to.
            splits.shown.set(Some(side));
            if let Some(scope) = self.scope() {
                splits.last.borrow_mut().insert(scope, side);
            }
            self.tell_split();
            return true;
        }
        if shown == side {
            if top {
                self.arrive(Arrival::Top);
            }
            return true;
        }
        self.leave_split();
        splits.shown.set(Some(side));
        if let Some(scope) = self.scope() {
            splits.last.borrow_mut().insert(scope, side);
        }
        splits.arriving.set(if top {
            Some(Arrival::Top)
        } else {
            self.arrival_for(self.scope())
        });
        self.inner.carried.borrow_mut().clear();
        self.reload();
        self.announce_split();
        self.tell_split();
        true
    }

    /// The next side (Tab), or the previous (Shift+Tab); with two, the other one. The cursor
    /// goes to its top row.
    pub fn step_split(&self, forward: bool) -> bool {
        let Some(shown) = self.split() else {
            return false;
        };
        let sides = [Split::Important, Split::Other];
        let at = sides.iter().position(|side| *side == shown).unwrap_or(0);
        let next = if forward {
            (at + 1) % sides.len()
        } else {
            (at + sides.len() - 1) % sides.len()
        };
        self.show_split(sides[next], true)
    }

    /// The next Inbox [`MessageList::show`] shows opens on `side` (as that side was left), not
    /// on the side it showed last: one view where Go to Other from another folder would open
    /// two. `None` forgets it.
    pub fn want_split(&self, side: Option<Split>) {
        self.inner.splits.wanted.set(side);
    }

    /// The side asked for with [`MessageList::want_split`] becomes `scope`'s, when it has sides.
    pub(super) fn take_wanted_split(&self, scope: Option<ViewScope>) {
        let splits = &self.inner.splits;
        if let Some(side) = splits.wanted.take()
            && let Some(scope) =
                scope.filter(|scope| splits.on.get() && self.is_inbox_scope(*scope))
        {
            splits.last.borrow_mut().insert(scope, side);
        }
    }

    /// Switches that adopted the other side's warm view, and switches that opened theirs.
    pub fn split_switches(&self) -> (u64, u64) {
        let splits = &self.inner.splits;
        (splits.adopted.get(), splits.opened.get())
    }

    /// The side `scope` shows: the one it showed last, Important at first; none outside an
    /// Inbox view or with the splits off.
    pub(super) fn split_for(&self, scope: Option<ViewScope>) -> Option<Split> {
        let splits = &self.inner.splits;
        let scope = scope.filter(|scope| splits.on.get() && self.is_inbox_scope(*scope))?;
        Some(
            splits
                .last
                .borrow()
                .get(&scope)
                .copied()
                .unwrap_or(Split::Important),
        )
    }

    /// How `scope`'s side arrives: at its place when it has one.
    pub(super) fn arrival_for(&self, scope: Option<ViewScope>) -> Option<Arrival> {
        let side = self.split_for(scope)?;
        let place = self
            .inner
            .splits
            .places
            .borrow()
            .get(&(scope?, side))
            .copied()?;
        Some(Arrival::At(place))
    }

    /// Whether `scope` is an Inbox (an account's, or All Inboxes): the views with sides.
    fn is_inbox_scope(&self, scope: ViewScope) -> bool {
        match scope {
            ViewScope::UnifiedInbox => true,
            ViewScope::Folder(folder) => self
                .inner
                .mailbox
                .folder(folder)
                .is_some_and(|folder| folder.role == FolderRole::Inbox),
        }
    }

    /// Notes the place of the side shown, before the list shows something else.
    pub(super) fn leave_split(&self) {
        let inner = &self.inner;
        // Still opening, the rows and the scroll are the last content's.
        if inner.model.is_pending() {
            return;
        }
        let Content::View(spec) = inner.model.content() else {
            return;
        };
        let Some(side) = spec.split else {
            return;
        };
        let key = inner
            .cursor
            .get()
            .or_else(|| self.selected_keys_if_few().first().copied());
        let place = Place {
            key,
            scroll: inner.scroller.vadjustment().value(),
        };
        inner
            .splits
            .places
            .borrow_mut()
            .insert((spec.scope, side), place);
    }

    /// After a side's rows came in (a new content's reset): its cursor and scroll as it
    /// arrives. False when it arrives as any new content does.
    pub(super) fn arrive_split(&self) -> bool {
        let Some(arrival) = self.inner.splits.arriving.take() else {
            return false;
        };
        let Content::View(spec) = self.inner.model.content() else {
            return false;
        };
        if spec.split.is_none() {
            return false;
        }
        self.arrive(arrival)
    }

    fn arrive(&self, arrival: Arrival) -> bool {
        let inner = &self.inner;
        let model = &inner.model;
        match arrival {
            Arrival::Top => {
                self.scroll_after_layout(0.0);
                if model.total() == 0 {
                    return true;
                }
                if inner.keeps_cursor.get() {
                    self.select_position(0);
                } else {
                    // Split: nothing shown before it is picked, as a folder opens there; the
                    // keyboard focus waits on the top row.
                    inner.selection.unselect_all();
                    if self.has_focus_within() {
                        inner.view.scroll_to(0, gtk::ListScrollFlags::FOCUS, None);
                    }
                }
                true
            }
            Arrival::At(place) => {
                let Some(position) = place.key.and_then(|key| model.position_of_key(key)) else {
                    return false;
                };
                self.select_position(position);
                self.scroll_after_layout(place.scroll);
                true
            }
        }
    }

    /// Asks the store for the sides' totals in the scope shown, once at a time.
    pub(super) fn count_splits(&self) {
        let splits = &self.inner.splits;
        let Some(scope) = self.scope().filter(|_| splits.shown.get().is_some()) else {
            if splits.counts.take().is_some() {
                self.tell_split();
            }
            return;
        };
        if splits.counting.replace(true) {
            splits.count_again.set(true);
            return;
        }
        let spec = ViewSpec {
            scope,
            filter: ViewFilter::All,
            grouping: self.grouping(),
            split: None,
        };
        let Some(reply) = self
            .inner
            .mailbox
            .store()
            .map(|store| store.split_counts(spec))
        else {
            splits.counting.set(false);
            return;
        };
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let counted = reply.recv().await;
            Self::with(&weak, |list| {
                let splits = &list.inner.splits;
                splits.counting.set(false);
                // A count for another scope or grouping than the list's now is stale; one
                // overtaken by a change shows until the next count, asked for now, comes.
                let again = splits.count_again.take();
                if let Ok(counted) = counted
                    && list.scope() == Some(scope)
                    && list.grouping() == spec.grouping
                    && list.split().is_some()
                    && splits.counts.replace(Some(counted.value)) != Some(counted.value)
                {
                    list.update_zero_other();
                    list.tell_split();
                }
                if again {
                    list.count_splits();
                }
            });
        });
    }

    /// Drops the totals counted for another Inbox or grouping, until the next count.
    pub(super) fn forget_split_counts(&self) {
        let splits = &self.inner.splits;
        if splits.counts.take().is_some() {
            self.update_zero_other();
        }
    }

    /// A store change: the warm view keeps it for its adoption, and the tabs count again.
    pub(super) fn split_event(&self, event: &Arc<ChangeEvent>) {
        let splits = &self.inner.splits;
        let mut drop_warm = false;
        if let Some(warm) = splits.warm.borrow_mut().as_mut() {
            let id = warm.opened.as_ref().map(|opened| opened.view.id());
            let reload = event.views.iter().any(|change| {
                Some(change.view) == id && matches!(change.diff, ViewDiff::Reload { .. })
            });
            if reload || warm.events.len() >= WARM_EVENTS {
                drop_warm = true;
            } else if warm
                .opened
                .as_ref()
                .is_none_or(|opened| event.version > opened.version)
            {
                warm.events.push(event.clone());
            }
        }
        if drop_warm {
            splits.warm.take();
            self.warm_other();
        }
        if splits.shown.get().is_some() {
            self.count_splits();
        }
    }

    /// Opens the other side's view ahead, unless it is open already for the view shown.
    pub(super) fn warm_other(&self) {
        let inner = &self.inner;
        let splits = &inner.splits;
        let Content::View(spec) = inner.model.content() else {
            splits.warm.take();
            return;
        };
        let Some(side) = spec.split else {
            splits.warm.take();
            return;
        };
        let other = spec.with_split(Some(match side {
            Split::Important => Split::Other,
            Split::Other => Split::Important,
        }));
        let thresholds = self.thresholds_now();
        if splits
            .warm
            .borrow()
            .as_ref()
            .is_some_and(|warm| warm.spec == other && warm.thresholds == thresholds)
        {
            return;
        }
        let generation = splits.warm_generation.get() + 1;
        splits.warm_generation.set(generation);
        let Some(reply) = inner.mailbox.store().map(|store| {
            store.open_view(OpenView {
                spec: other,
                first_page: super::model::PAGE,
                section_starts: thresholds.starts(),
            })
        }) else {
            splits.warm.take();
            return;
        };
        splits.warm.replace(Some(Warm {
            spec: other,
            thresholds,
            opened: None,
            events: Vec::new(),
            generation,
        }));
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let opened = reply.recv().await;
            Self::with(&weak, |list| {
                let mut slot = list.inner.splits.warm.borrow_mut();
                let Some(warm) = slot.as_mut().filter(|warm| warm.generation == generation) else {
                    return;
                };
                match opened {
                    Ok(opened) => {
                        warm.events.retain(|event| event.version > opened.version);
                        warm.opened = Some(opened);
                    }
                    Err(error) => {
                        // The switch opens it then.
                        tracing::debug!("the other side's view could not be opened: {error}");
                        slot.take();
                    }
                }
            });
        });
    }

    /// The warm view of `spec`, opened with `thresholds`, with the events since, if there is
    /// one: the next switch adopts it. The other side is warmed again once it shows.
    pub(super) fn take_warm(
        &self,
        spec: ViewSpec,
        thresholds: Thresholds,
    ) -> Option<(ViewOpened, Vec<Arc<ChangeEvent>>)> {
        let splits = &self.inner.splits;
        let usable = splits.warm.borrow().as_ref().is_some_and(|warm| {
            warm.spec == spec && warm.thresholds == thresholds && warm.opened.is_some()
        });
        if spec.split.is_some() {
            if usable {
                splits.adopted.set(splits.adopted.get() + 1);
            } else {
                splits.opened.set(splits.opened.get() + 1);
            }
        }
        if !usable {
            return None;
        }
        let warm = splits.warm.take()?;
        Some((warm.opened?, warm.events))
    }

    /// "Other has 4 conversations": the other side's mail, on the empty side's Inbox Zero.
    pub(super) fn zero_other_line(&self) -> Option<String> {
        let side = self.split()?;
        let counts = self.split_counts()?;
        let (name, count) = match side {
            Split::Important => ("Other", counts.other),
            Split::Other => ("Important", counts.important),
        };
        (count > 0).then(|| format!("{name} has {}", self.rows_text(count)))
    }

    /// "4 conversations", "1 message": rows of the list's grouping.
    pub(super) fn rows_text(&self, count: u32) -> String {
        let (one, many) = match self.grouping() {
            ViewGrouping::Conversations => ("conversation", "conversations"),
            ViewGrouping::Messages => ("message", "messages"),
        };
        match count {
            1 => format!("1 {one}"),
            count => format!("{count} {many}"),
        }
    }

    /// Inbox Zero's line on the other side's mail, as it shows (empty when it doesn't).
    pub fn zero_other(&self) -> String {
        let zero = &self.inner.zero;
        if self.inner.stack.visible_child_name().as_deref() == Some("zero") {
            zero.other().to_string()
        } else {
            String::new()
        }
    }

    fn update_zero_other(&self) {
        self.inner.zero.set_other(self.zero_other_line().as_deref());
    }

    /// Tells a screen reader the side the list now shows: "Other, 4 conversations".
    fn announce_split(&self) {
        let (Some(side), Some(counts)) = (self.split(), self.split_counts()) else {
            return;
        };
        let (name, count) = match side {
            Split::Important => ("Important", counts.important),
            Split::Other => ("Other", counts.other),
        };
        accessibility::announce(
            &self.inner.view,
            &format!("{name}, {}", self.rows_text(count)),
        );
    }

    /// Next Split's and Previous Split's keys changed (`keys.toml`): the list says the new ones.
    pub(super) fn set_split_keys(&self, keymap: &Keymap) {
        let keys = split_keys(keymap);
        if *self.inner.splits.keys.borrow() != keys {
            self.inner.splits.keys.replace(keys);
            self.describe_split_keys();
        }
    }

    /// The list's accessible description: how to switch sides, and how to leave it while Tab
    /// switches them; none outside the sides.
    fn describe_split_keys(&self) {
        let view = &self.inner.view;
        let keys = self.inner.splits.keys.borrow();
        if self.split().is_some() && !keys.is_empty() {
            view.update_property(&[gtk::accessible::Property::Description(&keys)]);
        } else {
            view.reset_property(gtk::AccessibleProperty::Description);
        }
    }

    /// Tells the handlers the side shown and the totals, when either changed, or the rows
    /// they count; a screen reader hears from the list how to switch sides, and how to leave
    /// it now that Tab switches them.
    pub(super) fn tell_split(&self) {
        let splits = &self.inner.splits;
        let state = (splits.shown.get(), splits.counts.get(), self.grouping());
        let before = splits.told.replace(Some(state));
        if before == Some(state) {
            return;
        }
        if before.map(|told| told.0.is_some()) != Some(state.0.is_some()) {
            self.describe_split_keys();
        }
        let handlers = splits.handlers.borrow().clone();
        for handler in handlers {
            handler(state.0, state.1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_says_the_keys_that_switch_sides() {
        assert_eq!(
            split_keys(&Keymap::default()),
            "Tab and Shift+Tab switch between Important and Other; Ctrl+Tab leaves the list"
        );
        // Moved off Tab in keys.toml: the keys it has, and Tab leaves the list as before.
        let key = |text: &str| vec![Binding::parse(text).expect("a key")];
        let moved = Keymap::with_overrides([
            (ActionId::NextSplit, key("bracketright")),
            (ActionId::PreviousSplit, key("bracketleft")),
        ]);
        assert_eq!(
            split_keys(&moved),
            "] and [ switch between Important and Other"
        );
        let one = Keymap::with_overrides([
            (ActionId::NextSplit, key("<Control>bracketright")),
            (ActionId::PreviousSplit, Vec::new()),
        ]);
        assert_eq!(
            split_keys(&one),
            "Ctrl+] switches between Important and Other"
        );
        let none = Keymap::with_overrides([
            (ActionId::NextSplit, Vec::new()),
            (ActionId::PreviousSplit, Vec::new()),
        ]);
        assert_eq!(split_keys(&none), "");
    }
}
