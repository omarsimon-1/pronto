//! The registry's actions in GTK (ADR-017): GActions on the application and the window,
//! application accelerators, scoped shortcuts on the list, reader and composer widgets, and the
//! window's keys that yield to the focused widget (`Scope::Window`: Ctrl+K, on the window in the
//! bubble phase). Nothing here decides what exists; it only wires up `sealwax_domain::actions`.

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use sealwax_domain::actions::{ActionId, ActionKind, Scope};
use sealwax_domain::keymap::Keymap;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The registry's actions on one action map: the application's (`app.`) or a window's
/// (`win.`). Every action starts disabled; it becomes available when the app connects a
/// handler, and an action that needs a selection is enabled only while there is one.
pub struct ActionSet {
    actions: Vec<(ActionId, gio::SimpleAction)>,
    available: RefCell<HashSet<ActionId>>,
    has_selection: Cell<bool>,
    /// Commands that don't apply to what is selected, such as Archive in the Archive folder.
    blocked: RefCell<HashSet<ActionId>>,
}

impl ActionSet {
    fn install(map: &impl IsA<gio::ActionMap>, ids: impl Iterator<Item = ActionId>) -> Self {
        let actions = ids
            .map(|id| {
                let action = match id.kind() {
                    ActionKind::Command => gio::SimpleAction::new(id.name(), None),
                    ActionKind::Toggle => {
                        gio::SimpleAction::new_stateful(id.name(), None, &false.to_variant())
                    }
                };
                action.set_enabled(false);
                map.add_action(&action);
                (id, action)
            })
            .collect();
        Self {
            actions,
            available: RefCell::new(HashSet::new()),
            has_selection: Cell::new(false),
            blocked: RefCell::new(HashSet::new()),
        }
    }

    pub fn action(&self, id: ActionId) -> Option<&gio::SimpleAction> {
        self.actions
            .iter()
            .find(|(action_id, _)| *action_id == id)
            .map(|(_, action)| action)
    }

    pub fn ids(&self) -> impl Iterator<Item = ActionId> + '_ {
        self.actions.iter().map(|(id, _)| *id)
    }

    /// Runs `handler` when the command is activated, and makes it available.
    pub fn connect(&self, id: ActionId, handler: impl Fn() + 'static) {
        let Some(action) = self.action(id) else {
            tracing::warn!(action = id.name(), "not in this action set");
            return;
        };
        action.connect_activate(move |_, _| handler());
        self.set_available(id, true);
    }

    /// Makes the toggle `id` available with `state`; `handler` gets every new state.
    pub fn connect_toggle(&self, id: ActionId, state: bool, handler: impl Fn(bool) + 'static) {
        let Some(action) = self.action(id) else {
            tracing::warn!(action = id.name(), "not in this action set");
            return;
        };
        action.set_state(&state.to_variant());
        action.connect_change_state(move |action, value| {
            if let Some(on) = value.and_then(glib::Variant::get::<bool>) {
                action.set_state(&on.to_variant());
                handler(on);
            }
        });
        self.set_available(id, true);
    }

    /// Shows a toggle's state without running its handler, such as a sidebar a breakpoint hid.
    pub fn set_toggle_state(&self, id: ActionId, on: bool) {
        if let Some(action) = self.action(id) {
            action.set_state(&on.to_variant());
        }
    }

    /// Whether the app has a handler for `id` and wants it shown: an action can be taken away
    /// for a while, such as Refresh while offline.
    pub fn set_available(&self, id: ActionId, available: bool) {
        if available {
            self.available.borrow_mut().insert(id);
        } else {
            self.available.borrow_mut().remove(&id);
        }
        self.refresh(id);
    }

    pub fn is_available(&self, id: ActionId) -> bool {
        self.available.borrow().contains(&id)
    }

    /// Whether messages are selected; the actions that need a selection follow it.
    pub fn set_has_selection(&self, has_selection: bool) {
        self.has_selection.set(has_selection);
        for id in self.ids().filter(|id| id.spec().needs_selection) {
            self.refresh(id);
        }
    }

    pub fn is_enabled(&self, id: ActionId) -> bool {
        self.action(id).is_some_and(|action| action.is_enabled())
    }

    /// Whether `id` applies to what is selected now; an available command that doesn't is
    /// disabled until it does, and the keyboard shortcuts dialog still lists it as built.
    pub fn set_applies(&self, id: ActionId, applies: bool) {
        let changed = if applies {
            self.blocked.borrow_mut().remove(&id)
        } else {
            self.blocked.borrow_mut().insert(id)
        };
        if changed {
            self.refresh(id);
        }
    }

    fn refresh(&self, id: ActionId) {
        let enabled = self.is_available(id)
            && (!id.spec().needs_selection || self.has_selection.get())
            && !self.blocked.borrow().contains(&id);
        if let Some(action) = self.action(id) {
            action.set_enabled(enabled);
        }
    }
}

/// Adds the application's actions (`app.quit`, `app.preferences`, ...) to `app`.
pub fn install_app_actions(app: &impl IsA<gio::ActionMap>) -> ActionSet {
    ActionSet::install(
        app,
        ActionId::ALL
            .into_iter()
            .filter(|id| id.scope() == Scope::App),
    )
}

/// Adds every other action (`win.archive`, ...) to `window`.
pub fn install_window_actions(window: &impl IsA<gio::ActionMap>) -> ActionSet {
    ActionSet::install(
        window,
        ActionId::ALL
            .into_iter()
            .filter(|id| id.scope() != Scope::App),
    )
}

/// Adds the composer's actions (`win.send`, `win.bold`, ...) to `group`, which a composer card
/// inserts as its own `win` group: inside the card they hide the window's of the same names.
pub fn install_composer_actions(group: &impl IsA<gio::ActionMap>) -> ActionSet {
    ActionSet::install(
        group,
        ActionId::ALL
            .into_iter()
            .filter(|id| id.scope() == Scope::Composer),
    )
}

/// Installs the keymap's application accelerators; actions without any get none.
pub fn apply_accels(app: &impl IsA<gtk::Application>, keymap: &Keymap) {
    for id in ActionId::ALL {
        let accels = keymap.accels(id);
        let accels: Vec<&str> = accels.iter().map(String::as_str).collect();
        app.set_accels_for_action(&id.detailed_name(), &accels);
    }
}

/// How long the second key of a sequence (`g` then `i`) is waited for.
pub const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(1500);

/// What a key press does in a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The first key of a sequence: wait for the second.
    Arm,
    Run {
        action: ActionId,
        sequence: bool,
    },
    /// A key after a sequence's first key that ends no sequence: nothing runs, so a mistyped
    /// sequence never archives or deletes.
    Swallow,
    /// Not a key of the scope.
    Pass,
}

/// A scope's keys by their canonical spelling, and the pending first key of a sequence.
#[derive(Debug, Default)]
struct Dispatch {
    singles: HashMap<String, ActionId>,
    sequences: HashMap<String, HashMap<String, ActionId>>,
    pending: Option<(String, Instant)>,
}

impl Dispatch {
    fn new(scope: Scope, keymap: &Keymap) -> Self {
        let mut dispatch = Self::default();
        for (binding, action) in keymap.scoped_in(scope) {
            let first = binding.first.canonical();
            match &binding.then {
                None => {
                    dispatch.singles.insert(first, action);
                }
                Some(second) => {
                    dispatch
                        .sequences
                        .entry(first)
                        .or_default()
                        .insert(second.canonical(), action);
                }
            }
        }
        dispatch
    }

    /// Every key that needs a shortcut: single keys, and both keys of every sequence.
    fn triggers(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .singles
            .keys()
            .chain(self.sequences.keys())
            .chain(self.sequences.values().flat_map(HashMap::keys))
            .cloned()
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    fn press(&mut self, key: &str, now: Instant) -> Outcome {
        if let Some((first, at)) = self.pending.take()
            && now.duration_since(at) <= SEQUENCE_TIMEOUT
        {
            if let Some(action) = self
                .sequences
                .get(&first)
                .and_then(|seconds| seconds.get(key))
            {
                return Outcome::Run {
                    action: *action,
                    sequence: true,
                };
            }
            if !self.sequences.contains_key(key) {
                return Outcome::Swallow;
            }
        }
        if self.sequences.contains_key(key) {
            self.pending = Some((key.to_owned(), now));
            return Outcome::Arm;
        }
        match self.singles.get(key) {
            Some(action) => Outcome::Run {
                action: *action,
                sequence: false,
            },
            None => Outcome::Pass,
        }
    }

    fn pending(&self, now: Instant) -> Option<&str> {
        self.pending
            .as_ref()
            .filter(|(_, at)| now.duration_since(*at) <= SEQUENCE_TIMEOUT)
            .map(|(key, _)| key.as_str())
    }

    /// Whether `key` does something here: a command's own key, the first key of a sequence,
    /// or the second of the sequence waiting for it.
    fn handles(&self, key: &str, now: Instant) -> bool {
        self.singles.contains_key(key)
            || self.sequences.contains_key(key)
            || self
                .pending(now)
                .and_then(|first| self.sequences.get(first))
                .is_some_and(|seconds| seconds.contains_key(key))
    }
}

/// The scoped shortcuts of one scope (ADR-017): one-key shortcuts, two-key sequences and the
/// scope's keys with modifiers. Every widget they are attached to shares one list of
/// shortcuts, which [`ScopedShortcuts::update`] refills when `keys.toml` changes. The window's
/// (`Scope::Window`) are the keys that yield, attached to the window in the bubble phase: they
/// run after every focused widget that doesn't keep the key.
pub struct ScopedShortcuts {
    scope: Scope,
    model: gio::ListStore,
    dispatch: Rc<RefCell<Dispatch>>,
    /// Hear of a key whose command is off now ([`ScopedShortcuts::connect_refused`]).
    refused: Rc<RefCell<Vec<Refused>>>,
}

/// Told of a command whose key was pressed while it is off; `true` when it said why, which
/// keeps the key.
type Refused = Rc<dyn Fn(ActionId) -> bool>;

impl ScopedShortcuts {
    pub fn new(scope: Scope, keymap: &Keymap) -> Self {
        let shortcuts = Self {
            scope,
            model: gio::ListStore::new::<gtk::Shortcut>(),
            dispatch: Rc::new(RefCell::new(Dispatch::default())),
            refused: Rc::default(),
        };
        shortcuts.update(keymap);
        shortcuts
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The shortcuts every attached controller uses.
    pub fn model(&self) -> &gio::ListStore {
        &self.model
    }

    /// Whether the scope's shortcuts act on `key` (canonical: `<Control>a`) now.
    pub fn handles(&self, key: &str) -> bool {
        self.dispatch.borrow().handles(key, Instant::now())
    }

    /// Runs `handler` with the command of a key pressed while the command is off (it doesn't
    /// apply to the selection): it may say why, and returns whether it did, which keeps the key
    /// (phase C: `h` where nothing can be reminded). Otherwise the key goes on, as before. Each
    /// handler connected is asked in turn until one says why.
    pub fn connect_refused(&self, handler: impl Fn(ActionId) -> bool + 'static) {
        self.refused.borrow_mut().push(Rc::new(handler));
    }

    /// The first key of a sequence waiting for its second, such as `g`.
    pub fn pending(&self) -> Option<String> {
        self.dispatch
            .borrow()
            .pending(Instant::now())
            .map(str::to_owned)
    }

    /// Takes the keys from `keymap`, for every attached widget.
    pub fn update(&self, keymap: &Keymap) {
        let dispatch = Dispatch::new(self.scope, keymap);
        let triggers = dispatch.triggers();
        self.dispatch.replace(dispatch);
        let shortcuts: Vec<gtk::Shortcut> = triggers
            .into_iter()
            .filter_map(|key| {
                let Some(trigger) = gtk::ShortcutTrigger::parse_string(&key) else {
                    tracing::warn!(key, "GTK cannot parse a scoped key");
                    return None;
                };
                let dispatch = self.dispatch.clone();
                let refused = self.refused.clone();
                let action = gtk::CallbackAction::new(move |widget, _| {
                    let outcome = dispatch.borrow_mut().press(&key, Instant::now());
                    match outcome {
                        Outcome::Arm | Outcome::Swallow => glib::Propagation::Stop,
                        Outcome::Pass => glib::Propagation::Proceed,
                        Outcome::Run { action, sequence } => {
                            let named = gtk::NamedAction::new(&action.detailed_name());
                            let ran =
                                named.activate(gtk::ShortcutActionFlags::empty(), widget, None);
                            let handlers = if ran {
                                Vec::new()
                            } else {
                                refused.borrow().clone()
                            };
                            let explained = handlers.iter().any(|handler| handler(action));
                            if ran || sequence || explained {
                                glib::Propagation::Stop
                            } else {
                                glib::Propagation::Proceed
                            }
                        }
                    }
                });
                Some(gtk::Shortcut::new(Some(trigger), Some(action)))
            })
            .collect();
        self.model.splice(0, self.model.n_items(), &shortcuts);
    }

    /// Handles the scope's keys on `widget`. In the bubble phase the focused widget sees every
    /// key first, so an entry inside `widget` keeps its letters; in the capture phase the keys
    /// win over `widget`'s own bindings (GtkListView's Enter and Space), which suits a widget
    /// with no text input inside.
    pub fn attach(
        &self,
        widget: &impl IsA<gtk::Widget>,
        phase: gtk::PropagationPhase,
    ) -> gtk::ShortcutController {
        self.attach_model(widget, phase, &self.model)
    }

    /// [`ScopedShortcuts::attach`] in the bubble phase for a pane beside the scope's own (the
    /// folder list beside the message list), without the scope's Tab and Shift+Tab, which stay
    /// the keyboard focus's there, and without its Enter and Space, which are that pane's
    /// rows' and buttons' own (GtkListBox activates a row and lets the key go on: Enter on a
    /// folder also opened the selected message).
    pub fn attach_beside(&self, widget: &impl IsA<gtk::Widget>) -> gtk::ShortcutController {
        let filter = gtk::CustomFilter::new(|item| {
            let key = item
                .downcast_ref::<gtk::Shortcut>()
                .and_then(gtk::Shortcut::trigger)
                .and_downcast::<gtk::KeyvalTrigger>()
                .map(|trigger| trigger.keyval());
            !matches!(
                key,
                Some(
                    gdk::Key::Tab
                        | gdk::Key::ISO_Left_Tab
                        | gdk::Key::KP_Tab
                        | gdk::Key::Return
                        | gdk::Key::KP_Enter
                        | gdk::Key::ISO_Enter
                        | gdk::Key::space
                        | gdk::Key::KP_Space
                )
            )
        });
        let model = gtk::FilterListModel::new(Some(self.model.clone()), Some(filter));
        self.attach_model(widget, gtk::PropagationPhase::Bubble, &model)
    }

    fn attach_model(
        &self,
        widget: &impl IsA<gtk::Widget>,
        phase: gtk::PropagationPhase,
        model: &impl IsA<gio::ListModel>,
    ) -> gtk::ShortcutController {
        let controller = gtk::ShortcutController::for_model(model);
        controller.set_propagation_phase(phase);
        controller.set_name(Some(&format!("sealwax-{}-keys", scope_name(self.scope))));
        widget.add_controller(controller.clone());

        // A key that is none of the scope's ends a pending sequence, so `g`, Down, `i` doesn't
        // go to the Inbox.
        let cancel = gtk::EventControllerKey::new();
        cancel.set_propagation_phase(phase);
        let dispatch = self.dispatch.clone();
        let model = self.model.clone();
        cancel.connect_key_pressed(move |cancel, _, _, _| {
            if dispatch.borrow().pending.is_none() {
                return glib::Propagation::Proceed;
            }
            let Some(event) = cancel.current_event() else {
                return glib::Propagation::Proceed;
            };
            if event
                .downcast_ref::<gdk::KeyEvent>()
                .is_some_and(gdk::KeyEvent::is_modifier)
            {
                return glib::Propagation::Proceed;
            }
            let ours = (0..model.n_items())
                .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
                .filter_map(|shortcut| shortcut.trigger())
                .any(|trigger| trigger.trigger(&event, false) == gdk::KeyMatch::Exact);
            if !ours {
                dispatch.borrow_mut().pending = None;
            }
            glib::Propagation::Proceed
        });
        widget.add_controller(cancel);
        controller
    }
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::App => "app",
        Scope::Window => "window",
        Scope::List => "list",
        Scope::Reader => "reader",
        Scope::Composer => "composer",
    }
}

/// Attaches the scoped shortcuts of `scope` to `widget` in the bubble phase, so text fields
/// inside it keep their keys. Keep the result: it attaches the same shortcuts to more widgets,
/// and [`ScopedShortcuts::update`] applies `keys.toml` changes.
pub fn install_scoped(
    widget: &impl IsA<gtk::Widget>,
    scope: Scope,
    keymap: &Keymap,
) -> ScopedShortcuts {
    let shortcuts = ScopedShortcuts::new(scope, keymap);
    shortcuts.attach(widget, gtk::PropagationPhase::Bubble);
    shortcuts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GTK's own accelerator parser, called through the C API: it needs no display, unlike the
    /// Rust wrapper, which asserts that GTK is initialised.
    fn gtk_parses(accel: &str) -> Option<(u32, u32)> {
        let text = std::ffi::CString::new(accel).ok()?;
        let (mut key, mut mods) = (0, 0);
        // SAFETY: a NUL-terminated string and two out-parameters; no display is used.
        let parsed =
            unsafe { gtk4::ffi::gtk_accelerator_parse(text.as_ptr(), &mut key, &mut mods) };
        (parsed != 0 && key != 0).then_some((key, mods))
    }

    #[test]
    fn every_registry_key_parses_as_a_gtk_accelerator() {
        let keymap = Keymap::default();
        for id in ActionId::ALL {
            for binding in keymap.shown(id) {
                for step in binding.steps() {
                    assert!(
                        gtk_parses(&step.canonical()).is_some(),
                        "{id:?}: GTK rejects {}",
                        step.canonical()
                    );
                }
            }
        }
        assert!(gtk_parses("<Control>notakey").is_none());
    }

    #[test]
    fn gtk_sees_no_accelerator_twice() {
        let keymap = Keymap::default();
        let mut seen = HashMap::new();
        for id in ActionId::ALL {
            for accel in keymap.accels(id) {
                let parsed = gtk_parses(&accel).unwrap();
                if let Some(other) = seen.insert(parsed, id) {
                    panic!("{accel} is bound to both {other:?} and {id:?}");
                }
            }
        }
        for scope in Scope::SCOPED {
            let mut seen = HashMap::new();
            for (binding, id) in keymap.scoped_in(scope) {
                let steps: Vec<_> = binding
                    .steps()
                    .map(|step| gtk_parses(&step.canonical()).unwrap())
                    .collect();
                if let Some(other) = seen.insert(steps, id) {
                    panic!("{binding} is bound to both {other:?} and {id:?} in {scope:?}");
                }
            }
        }
    }

    #[test]
    fn single_keys_run_their_action() {
        let mut dispatch = Dispatch::new(Scope::List, &Keymap::default());
        let now = Instant::now();
        assert_eq!(
            dispatch.press("e", now),
            Outcome::Run {
                action: ActionId::Archive,
                sequence: false
            }
        );
        assert_eq!(
            dispatch.press("<Control>r", now),
            Outcome::Run {
                action: ActionId::Reply,
                sequence: false
            }
        );
        assert_eq!(
            dispatch.press("space", now),
            Outcome::Run {
                action: ActionId::Select,
                sequence: false
            }
        );
        assert_eq!(dispatch.press("w", now), Outcome::Pass);
        // Phase C: `i` shows or hides the contact pane, and after `g` it is still Go to Inbox.
        assert_eq!(
            dispatch.press("i", now),
            Outcome::Run {
                action: ActionId::ContactDetails,
                sequence: false
            }
        );
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(
            dispatch.press("i", now),
            Outcome::Run {
                action: ActionId::GoToInbox,
                sequence: true
            }
        );
        let mut reader = Dispatch::new(Scope::Reader, &Keymap::default());
        // Space is Select's in the list alone: in the reader it scrolls a page.
        assert_eq!(
            reader.press("space", now),
            Outcome::Run {
                action: ActionId::ScrollMessagePageDown,
                sequence: false
            }
        );
        assert_eq!(
            reader.press("<Shift>space", now),
            Outcome::Run {
                action: ActionId::ScrollMessagePageUp,
                sequence: false
            }
        );
        // The Superhuman pass: `u` marks read or unread wherever mail is; Escape goes back.
        assert_eq!(
            reader.press("u", now),
            Outcome::Run {
                action: ActionId::ToggleRead,
                sequence: false
            }
        );
        assert_eq!(
            reader.press("Escape", now),
            Outcome::Run {
                action: ActionId::BackToList,
                sequence: false
            }
        );
        for (key, action) in [
            ("u", ActionId::ToggleRead),
            ("h", ActionId::RemindMe),
            ("<Shift>u", ActionId::ShowUnreadOnly),
            ("<Shift>s", ActionId::ShowStarredOnly),
            ("<Control>a", ActionId::SelectAll),
        ] {
            assert_eq!(
                dispatch.press(key, now),
                Outcome::Run {
                    action,
                    sequence: false
                },
                "{key}"
            );
        }
        assert_eq!(reader.press("<Shift>u", now), Outcome::Pass);
        assert_eq!(reader.press("<Control>a", now), Outcome::Pass);
    }

    #[test]
    fn the_window_keeps_the_keys_that_yield() {
        let mut window = Dispatch::new(Scope::Window, &Keymap::default());
        let now = Instant::now();
        // Ctrl+Shift+P yields too: in a draft it is Pop Draft Out or In (the fork's phase B).
        assert_eq!(window.triggers(), ["<Control><Shift>p", "<Control>k"]);
        assert_eq!(
            window.press("<Control>k", now),
            Outcome::Run {
                action: ActionId::CommandPalette,
                sequence: false
            }
        );
        assert_eq!(window.press("e", now), Outcome::Pass);
        // Its other keys are accelerators, and the composer keeps its own Ctrl+K.
        assert!(
            !Dispatch::new(Scope::List, &Keymap::default())
                .triggers()
                .contains(&"<Control>k".to_owned())
        );
        assert!(
            Dispatch::new(Scope::Composer, &Keymap::default())
                .triggers()
                .contains(&"<Control>k".to_owned())
        );
        // keys.toml moves it like any other key.
        let moved = Keymap::with_overrides([(
            ActionId::CommandPalette,
            vec![
                sealwax_domain::keymap::Binding::parse("<Control>b").unwrap(),
                sealwax_domain::keymap::Binding::parse("F1").unwrap(),
            ],
        )]);
        assert_eq!(
            Dispatch::new(Scope::Window, &moved).triggers(),
            ["<Control>b"]
        );
    }

    #[test]
    fn the_new_go_to_sequences_resolve() {
        let mut dispatch = Dispatch::new(Scope::Reader, &Keymap::default());
        let now = Instant::now();
        for (second, action) in [("e", ActionId::GoToArchive), ("l", ActionId::GoToFolder)] {
            assert_eq!(dispatch.press("g", now), Outcome::Arm);
            assert_eq!(
                dispatch.press(second, now),
                Outcome::Run {
                    action,
                    sequence: true
                }
            );
        }
    }

    /// The list lets GtkListView's Ctrl+A and Ctrl+Shift+A through only to a command of its
    /// own (`MessageList::selection_key`): Select All has Ctrl+A, nothing has Ctrl+Shift+A.
    #[test]
    fn the_list_knows_which_keys_are_its_own() {
        let now = Instant::now();
        let mut list = Dispatch::new(Scope::List, &Keymap::default());
        assert!(list.handles("<Control>a", now));
        assert!(!list.handles("<Control><Shift>a", now));
        assert!(list.handles("g", now));
        assert!(
            !list.handles("t", now),
            "a sequence's second key waits for its first"
        );
        assert_eq!(list.press("g", now), Outcome::Arm);
        assert!(list.handles("t", now));
        assert!(!list.handles("t", now + SEQUENCE_TIMEOUT * 2));
        // keys.toml can give Select All Ctrl+Shift+A instead.
        let moved = Keymap::with_overrides([(
            ActionId::SelectAll,
            vec![sealwax_domain::keymap::Binding::parse("<Control><Shift>a").unwrap()],
        )]);
        let list = Dispatch::new(Scope::List, &moved);
        assert!(list.handles("<Control><Shift>a", now));
        assert!(!list.handles("<Control>a", now));
    }

    #[test]
    fn sequences_resolve_within_the_timeout() {
        let mut dispatch = Dispatch::new(Scope::List, &Keymap::default());
        let now = Instant::now();
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(dispatch.pending(now), Some("g"));
        assert_eq!(
            dispatch.press("s", now + Duration::from_millis(400)),
            Outcome::Run {
                action: ActionId::GoToStarred,
                sequence: true
            }
        );
        assert_eq!(dispatch.pending(now), None);
        // Without the first key, s stars.
        assert_eq!(
            dispatch.press("s", now),
            Outcome::Run {
                action: ActionId::Star,
                sequence: false
            }
        );
        // Too late: the second key counts on its own.
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(
            dispatch.press("s", now + SEQUENCE_TIMEOUT + Duration::from_millis(1)),
            Outcome::Run {
                action: ActionId::Star,
                sequence: false
            }
        );
    }

    #[test]
    fn a_mistyped_sequence_runs_nothing() {
        let mut dispatch = Dispatch::new(Scope::List, &Keymap::default());
        let now = Instant::now();
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(dispatch.press("y", now), Outcome::Swallow);
        assert_eq!(
            dispatch.press("e", now),
            Outcome::Run {
                action: ActionId::Archive,
                sequence: false
            }
        );
        // `g g i` starts over.
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(
            dispatch.press("i", now),
            Outcome::Run {
                action: ActionId::GoToInbox,
                sequence: true
            }
        );
    }

    #[test]
    fn every_scoped_key_has_a_trigger() {
        let dispatch = Dispatch::new(Scope::Reader, &Keymap::default());
        let triggers = dispatch.triggers();
        for key in [
            "g",
            "i",
            "s",
            "t",
            "d",
            "j",
            "u",
            "h",
            "e",
            "l",
            "Escape",
            "<Control>r",
            "question",
        ] {
            assert!(triggers.contains(&key.to_owned()), "{key}");
        }
        // Select's `x` is the list's alone; its Space scrolls a page here.
        assert!(!triggers.contains(&"x".to_owned()));
        assert!(triggers.contains(&"space".to_owned()));
        assert!(triggers.contains(&"<Shift>space".to_owned()));
        assert!(
            !triggers.contains(&"<Control>n".to_owned()),
            "an accelerator"
        );
        assert!(triggers.contains(&"<Control><Shift>e".to_owned()));
        // Enter in a conversation replies under it (the fork's phase B).
        assert!(triggers.contains(&"Return".to_owned()));
        let composer = Dispatch::new(Scope::Composer, &Keymap::default());
        assert_eq!(
            composer.triggers(),
            [
                "<Control><Alt>a",
                "<Control><Shift>Return",
                "<Control><Shift>a",
                "<Control><Shift>b",
                "<Control><Shift>c",
                "<Control><Shift>d",
                "<Control><Shift>f",
                "<Control><Shift>l",
                "<Control><Shift>o",
                "<Control><Shift>p",
                "<Control><Shift>v",
                "<Control><Shift>z",
                "<Control>Return",
                "<Control>b",
                "<Control>i",
                "<Control>k",
                "<Control>u",
                "<Control>w",
                "<Control>y",
                "<Control>z",
            ]
        );
    }
}
