//! `list-replay SEEDS STEPS`: the list's model against the store, the app's twin of the store's
//! own `conversation_events_keep_list_models_exact` (a self-test, since GTK's section model
//! needs GTK running). Random new conversations, replies (from the user too, in Sent), batches,
//! flag changes and removals go through a scratch store of their own, and every change event is
//! applied to the app's models of three conversation views, each with moves told GTK at once and
//! row by row. After every step each model equals its view read fresh, row for row, with the same
//! date sections. Every message is fictional.

use super::{StepResult, number};
use crate::list::model::{MOVE_SPAN, MessageModel};
use crate::list::sections::{Bounds, Thresholds};
use crate::mailbox::Mailbox;
use sealwax_domain::model::{Address, Flags, FolderId, FolderRole, Headers, MessageId};
use sealwax_infrastructure::store::{
    FolderSpec, ListRow, NewAccount, NewMessage, OpenView, Store, StoreOptions, StoreVersion,
    ViewFilter, ViewSpec,
};
use std::rc::Rc;
use std::sync::Arc;

/// SplitMix64, for repeatable runs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const DAY: i64 = 86_400;
const BASE: i64 = 1_790_000_000;
const PEOPLE: [(&str, &str); 4] = [
    ("Ingrid Solberg", "ingrid@example.test"),
    ("Tomas Berg", "tomas@example.test"),
    ("Sana Malik", "sana@example.test"),
    ("Marius Dahl", "marius@example.test"),
];
const ME: (&str, &str) = ("Alex Berg", "alex@example.test");

/// A stored message the steps can answer, flag or remove.
#[derive(Clone)]
struct Sent {
    id: MessageId,
    message_id: String,
    date: i64,
}

struct World {
    store: Store,
    events: async_channel::Receiver<Arc<sealwax_infrastructure::store::ChangeEvent>>,
    inbox: FolderId,
    sent_folder: FolderId,
    messages: Vec<Sent>,
    serial: u32,
}

impl World {
    fn new(path: &std::path::Path) -> Result<Self, String> {
        let store = Store::open(&path.join("store.sqlite"), StoreOptions::default())
            .map_err(|error| error.to_string())?;
        let events = store.subscribe();
        let account = store
            .add_account(NewAccount {
                name: "Alex".into(),
                address: ME.1.into(),
            })
            .recv_blocking()
            .map_err(|error| error.to_string())?
            .value;
        let folders = store
            .upsert_folders(vec![
                FolderSpec::local(account, "INBOX", FolderRole::Inbox),
                FolderSpec::local(account, "Sent", FolderRole::Sent),
            ])
            .recv_blocking()
            .map_err(|error| error.to_string())?
            .value;
        Ok(Self {
            store,
            events,
            inbox: folders[0],
            sent_folder: folders[1],
            messages: Vec::new(),
            serial: 0,
        })
    }

    /// A new message in the Inbox, or the user's in Sent, answering `parent` when given.
    fn message(&mut self, rng: &mut Rng, parent: Option<&Sent>, mine: bool) -> NewMessage {
        self.serial += 1;
        let message_id = format!("replay-{}@example.test", self.serial);
        let date = match parent {
            // Mostly newer than what it answers (the conversation moves up), now and then
            // older (a late copy): either way the store places it.
            Some(parent) if rng.below(5) > 0 => parent.date + 1 + rng.below(3 * DAY as u64) as i64,
            _ => BASE + rng.below(11 * DAY as u64) as i64,
        };
        let (name, email) = if mine {
            ME
        } else {
            PEOPLE[rng.below(PEOPLE.len() as u64) as usize]
        };
        let mut flags = Flags::NONE;
        if mine || rng.below(2) == 0 {
            flags = flags | Flags::SEEN;
        }
        if rng.below(7) == 0 {
            flags = flags | Flags::FLAGGED;
        }
        let references: Vec<String> = parent.map(|p| p.message_id.clone()).into_iter().collect();
        NewMessage {
            folder: if mine { self.sent_folder } else { self.inbox },
            uid: None,
            flags,
            date,
            headers: Headers {
                from: vec![Address::new(name, email)],
                subject: format!("Subject {}", self.serial),
                date: Some(date),
                message_id: Some(message_id),
                in_reply_to: references.clone(),
                references,
                ..Headers::default()
            },
            snippet: format!("Snippet {}", self.serial),
            has_attachments: rng.below(6) == 0,
            ..NewMessage::default()
        }
    }

    fn ingest(&mut self, messages: Vec<NewMessage>) -> Result<StoreVersion, String> {
        let ingested = self
            .store
            .ingest(messages.clone())
            .recv_blocking()
            .map_err(|error| error.to_string())?;
        for (message, id) in messages.iter().zip(&ingested.ids) {
            if let Some(id) = id {
                self.messages.push(Sent {
                    id: *id,
                    message_id: message.headers.message_id.clone().unwrap_or_default(),
                    date: message.date,
                });
            }
        }
        Ok(ingested.version)
    }
}

/// A model of `spec` and how it tells moves, opened as the list opens views.
struct Shown {
    spec: ViewSpec,
    model: MessageModel,
}

fn open(
    world: &World,
    spec: ViewSpec,
    thresholds: Thresholds,
    move_span: u32,
) -> Result<Shown, String> {
    let opened = world
        .store
        .open_view(OpenView {
            spec,
            first_page: 1_000,
            section_starts: thresholds.starts(),
        })
        .recv_blocking()
        .map_err(|error| error.to_string())?;
    // No store of its own: every row comes with the view's first page or an event.
    let model = MessageModel::new(Rc::new(Mailbox::default()));
    model.set_move_span(move_span);
    model.adopt(spec, thresholds, opened);
    Ok(Shown { spec, model })
}

/// Applies every change event up to `version` to each model.
fn catch_up(world: &World, shown: &[Shown], version: StoreVersion) -> StepResult {
    loop {
        let applied = shown
            .iter()
            .filter_map(|shown| shown.model.version())
            .min()
            .unwrap_or_default();
        if applied >= version {
            return Ok(());
        }
        let event = world
            .events
            .recv_blocking()
            .map_err(|_| "the store's change events ended".to_owned())?;
        for shown in shown {
            shown.model.apply_event(&event);
        }
    }
}

fn check(world: &World, shown: &Shown, seed: u64, step: usize) -> StepResult {
    let page = world
        .store
        .page(shown.spec, 0, 10_000)
        .recv_blocking()
        .map_err(|error| error.to_string())?;
    let context = format!("seed {seed} step {step} {:?}", shown.spec.filter);
    if shown.model.version() != Some(page.version) {
        return Err(format!(
            "{context}: the model is at {:?}, the store at {:?}",
            shown.model.version(),
            page.version
        ));
    }
    let rows = page.into_rows();
    if shown.model.total() as usize != rows.len() {
        return Err(format!(
            "{context}: {} rows, the store has {}",
            shown.model.total(),
            rows.len()
        ));
    }
    for (position, row) in rows.iter().enumerate() {
        let held = shown.model.list_row_at(position as u32);
        if held.as_ref() != Some(row) {
            return Err(format!(
                "{context}: row {position} is {:?}, the store's {:?}",
                held.map(|held| (held.key(), held.date(), held.message())),
                (row.key(), row.date(), row.message())
            ));
        }
    }
    let sections = Bounds::from_dates(rows.iter().map(ListRow::date), &shown.model.thresholds());
    if shown.model.section_counts() != sections.counts() {
        return Err(format!(
            "{context}: sections {:?}, the store's rows {:?}",
            shown.model.section_counts(),
            sections.counts()
        ));
    }
    Ok(())
}

/// One seed's run: `steps` random changes, each model checked after each.
fn run(seed: u64, steps: usize) -> Result<(u64, u64), String> {
    let dir = std::env::temp_dir().join(format!(
        "sealwax-list-replay-{}-{seed}-{}",
        std::process::id(),
        gtk4::glib::random_int()
    ));
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let mut rng = Rng(seed);
    let mut world = World::new(&dir)?;
    // A few conversations to start with.
    let first: Vec<NewMessage> = (0..6)
        .map(|_| world.message(&mut rng, None, false))
        .collect();
    world.ingest(first)?;
    let thresholds = Thresholds::new(
        BASE + 10 * DAY,
        BASE + 9 * DAY,
        BASE + 7 * DAY,
        BASE + 3 * DAY,
    );
    let inbox = world.inbox;
    let mut shown = Vec::new();
    for spec in [
        ViewSpec::folder(inbox).conversations(),
        ViewSpec::folder(inbox)
            .with_filter(ViewFilter::Unread)
            .conversations(),
        ViewSpec::unified_inbox()
            .with_filter(ViewFilter::Starred)
            .conversations(),
    ] {
        for move_span in [MOVE_SPAN, 0] {
            shown.push(open(&world, spec, thresholds, move_span)?);
        }
    }
    for step in 0..steps {
        let pick = rng.below(100);
        let known = world.messages.len() as u64;
        let version = if pick < 20 || known == 0 {
            let message = world.message(&mut rng, None, false);
            world.ingest(vec![message])?
        } else if pick < 55 {
            let parent = world.messages[rng.below(known) as usize].clone();
            let mine = rng.below(4) == 0;
            let message = world.message(&mut rng, Some(&parent), mine);
            world.ingest(vec![message])?
        } else if pick < 65 {
            // A batch: several replies in one commit, one event.
            let count = 2 + rng.below(4);
            let batch: Vec<NewMessage> = (0..count)
                .map(|_| {
                    let parent = world.messages[rng.below(known) as usize].clone();
                    let mine = rng.below(5) == 0;
                    world.message(&mut rng, Some(&parent), mine)
                })
                .collect();
            world.ingest(batch)?
        } else if pick < 90 {
            let id = world.messages[rng.below(known) as usize].id;
            let flag = if rng.below(2) == 0 {
                Flags::SEEN
            } else {
                Flags::FLAGGED
            };
            let (add, remove) = if rng.below(2) == 0 {
                (flag, Flags::NONE)
            } else {
                (Flags::NONE, flag)
            };
            world
                .store
                .set_flags(vec![id], add, remove)
                .recv_blocking()
                .map_err(|error| error.to_string())?
        } else {
            let at = rng.below(known) as usize;
            let gone = world.messages.remove(at);
            world
                .store
                .remove(vec![gone.id])
                .recv_blocking()
                .map_err(|error| error.to_string())?
                .version
        };
        catch_up(&world, &shown, version)?;
        for shown in &shown {
            check(&world, shown, seed, step)?;
        }
    }
    let moves: (u64, u64) = shown.iter().fold((0, 0), |(once, rows), shown| {
        let stats = shown.model.stats();
        (once + stats.moves_at_once, rows + stats.moves_by_rows)
    });
    drop(shown);
    drop(world);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(moves)
}

/// `list-replay SEEDS STEPS`: seeds 1 to SEEDS, STEPS changes each; every conversation that
/// moved was told GTK at once in one model and row by row in its twin, both checked.
pub fn replay(args: &[String]) -> StepResult {
    let seeds = number(&args[0])? as u64;
    let steps = number(&args[1])?;
    let (mut at_once, mut by_rows) = (0, 0);
    for seed in 1..=seeds {
        let (once, rows) = run(seed, steps)?;
        at_once += once;
        by_rows += rows;
    }
    println!(
        "  # {seeds} seeds × {steps} steps: every model exact; {at_once} moves told at once, \
         {by_rows} row by row"
    );
    if at_once == 0 || by_rows == 0 {
        return Err("no conversation moved both ways".to_owned());
    }
    Ok(())
}
