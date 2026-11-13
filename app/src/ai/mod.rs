//! Optional AI work. One cancellable queue; only Store::save_draft, never a send command.
mod local_style;
mod settings;
mod style;
mod testing;
mod text;
use crate::application::AppState;
use crate::composer::store_backend::{message_id, now_ms, senders};
use crate::engine_link::Subscription;
use gtk4::{gio, glib};
use sealwax_domain::compose::{ReplyKind, reply};
use sealwax_domain::model::{
    Account, AccountId, DraftId, FolderId, FolderRole, MessageId, ThreadId,
};
use sealwax_engine::api::{Command, Event};
use sealwax_infrastructure::{
    agent::{Agent, Cancel},
    store::{BodyState, DraftSaved, SaveDraft, Store, StoreVersion, ViewFilter, ViewSpec},
};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::PathBuf,
    rc::{Rc, Weak},
    time::Duration,
};
use style::{
    DEFAULT_STYLE_MESSAGES, DEFAULT_STYLE_MONTHS, DEFAULT_STYLE_REFRESH_MESSAGES,
    MORE_STYLE_MESSAGES, MORE_STYLE_MONTHS, Style,
};

/// Starts the Message-ID of a draft the agent wrote, so a crash between saving the draft and its
/// marker can be recovered. Recipients see it, so it names neither the app nor the agent.
const AI_DRAFT_PREFIX: &str = "r1.";
/// The most messages one "Draft for unread in Inbox now" queues, newest first.
const UNREAD_NOW_MESSAGES: u32 = 50;
/// The gap between two steps of the worker, and how often saved drafts are checked.
const TICK: Duration = Duration::from_secs(1);
/// A timed wait is re-evaluated at least this often: timers do not count time spent suspended.
const LONGEST_WAIT: Duration = Duration::from_secs(300);
/// Failures after which a job is dropped while the agent answers other mail.
const JOB_ATTEMPTS: u32 = 3;
/// Failures after which a job is dropped even though nothing else succeeded in between.
const OUTAGE_ATTEMPTS: u32 = 8;
/// How often a finished draft is rechecked and saved again when unrelated mail changed the
/// store between the check and the save.
const SAVE_ATTEMPTS: u32 = 5;

/// Whether a Message-ID is one of an agent-written draft, under this or the earlier prefix.
fn ai_draft(message_id: &str) -> bool {
    message_id.starts_with(AI_DRAFT_PREFIX) || message_id.starts_with("dispatch-ai-")
}

/// Whether a settings change invalidates work in flight: another agent, model or style
/// switch changes the result, and turning drafting off abandons it. Limits and caps do not.
fn restarts_work(before: &Options, after: &Options) -> bool {
    before.agent != after.agent
        || before.model != after.model
        || before.style != after.style
        || before.auto && !after.auto
}

/// The wait in milliseconds before a failed job is tried again, or `None` to drop it.
/// `attempts` are the job's earlier failures, `streak` the failures in a row across the
/// queue before this one. A job is dropped after `JOB_ATTEMPTS` only when the agent answered
/// in between (the message is the problem, not the agent); in an outage the wait doubles
/// from 30 seconds to 30 minutes and holds the whole queue.
fn retry_delay(attempts: u32, streak: u32) -> Option<i64> {
    let failed = attempts + 1;
    if failed >= OUTAGE_ATTEMPTS || failed >= JOB_ATTEMPTS && streak == 0 {
        return None;
    }
    Some((30_000_i64 << streak.min(6)).min(1_800_000))
}

/// The newest `cap` of the unread rows `(date, message, account)`, newest first.
fn newest_unread(mut rows: Vec<(i64, i64, AccountId)>, cap: usize) -> Vec<(AccountId, MessageId)> {
    rows.sort_by_key(|row| std::cmp::Reverse((row.0, row.1)));
    rows.dedup_by_key(|row| row.1);
    rows.truncate(cap);
    rows.into_iter()
        .map(|(_, message, account)| (account, MessageId(message)))
        .collect()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub agent: String,
    pub model: String,
    pub style: bool,
    pub auto: bool,
    pub daily_cap: u32,
    pub enabled_at: i64,
    pub style_messages: u32,
    pub style_months: u32,
    pub style_refresh_messages: u32,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            agent: "default".into(),
            model: String::new(),
            style: false,
            auto: false,
            daily_cap: 200,
            enabled_at: 0,
            style_messages: DEFAULT_STYLE_MESSAGES,
            style_months: DEFAULT_STYLE_MONTHS,
            style_refresh_messages: DEFAULT_STYLE_REFRESH_MESSAGES,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Job {
    message: i64,
    account: i64,
    attempts: u32,
    retry_at: i64,
    #[serde(default)]
    explicit: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Ready {
    message: i64,
    thread: i64,
    draft: i64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    options: Options,
    paused: bool,
    queue: Vec<Job>,
    ready: Vec<Ready>,
    styles: BTreeMap<i64, Style>,
    day: i64,
    used: u32,
    style_sampling_version: u32,
    legacy_style_cache: bool,
}
impl Saved {
    /// Queues the messages that are neither queued nor drafted already.
    fn enqueue(&mut self, jobs: impl IntoIterator<Item = (AccountId, MessageId)>, explicit: bool) {
        for (account, message) in jobs {
            if !self.queue.iter().any(|j| j.message == message.0)
                && !self.ready.iter().any(|r| r.message == message.0)
            {
                self.queue.push(Job {
                    message: message.0,
                    account: account.0,
                    attempts: 0,
                    retry_at: 0,
                    explicit,
                });
            }
        }
    }
    /// When the account's style sheet next needs the agent, or `None` when it is finished.
    fn style_pending(&self, account: i64) -> Option<i64> {
        match self.styles.get(&account) {
            None => Some(0),
            Some(s) => (s.built == 0 || s.prepared || s.refresh_requested || s.rebuild_requested)
                .then_some(s.retry_at),
        }
    }
    /// How long the worker sleeps after a step, or `None` to sleep until it is woken: nothing
    /// is queued or pending, work is paused, or no agent can run. `working` is whether an
    /// agent is available, `held_until` the end of the queue's back-off after a failure.
    fn next_step(
        &self,
        working: bool,
        accounts: &[i64],
        held_until: i64,
        now: i64,
    ) -> Option<Duration> {
        // Saved drafts are watched: sending or discarding one removes its "Draft ready" mark.
        if !self.ready.is_empty() {
            return Some(TICK);
        }
        if self.paused || !working {
            return None;
        }
        let mut due = None::<i64>;
        if self.options.auto
            && let Some(retry_at) = self.queue.iter().map(|j| j.retry_at).min()
        {
            let day = now / 86_400_000;
            due = Some(if self.day == day && self.used >= self.options.daily_cap {
                (day + 1) * 86_400_000
            } else {
                retry_at.max(held_until)
            });
        }
        if self.options.style {
            for at in accounts.iter().filter_map(|a| self.style_pending(*a)) {
                due = Some(due.map_or(at, |due| due.min(at)));
            }
        }
        let wait = Duration::from_millis((due? - now).max(0) as u64);
        Some(wait.clamp(TICK, LONGEST_WAIT))
    }
}

struct Checkpoint {
    saved: Saved,
    reply: Option<async_channel::Sender<Result<(), String>>>,
}

pub struct Controller {
    state: Weak<AppState>,
    saved: RefCell<Saved>,
    agents: RefCell<Vec<Agent>>,
    default_agent: RefCell<Option<String>>,
    loaded: Cell<bool>,
    pub status: RefCell<String>,
    cancel: RefCell<Cancel>,
    /// Cancelled only when the app stops; agent detection outlives option changes.
    quit: Cancel,
    stopped: Cell<bool>,
    in_flight: Cell<bool>,
    detected: Cell<bool>,
    detecting: Cell<bool>,
    settings_opened: Cell<bool>,
    /// Failures in a row across the queue, and the end of the back-off they caused.
    failures: Cell<u32>,
    held_until: Cell<i64>,
    wake: async_channel::Sender<()>,
    subscription: RefCell<Option<Subscription>>,
    writes: async_channel::Sender<Checkpoint>,
    style_dir: PathBuf,
    style_examples: RefCell<BTreeMap<i64, std::collections::VecDeque<String>>>,
    style_request: Cell<u64>,
    open_targets: RefCell<std::collections::HashMap<MessageId, DraftId>>,
}
impl Controller {
    pub fn start(state: &Rc<AppState>) -> Rc<Self> {
        let path = state.config().dirs.state.join("ai.json");
        let style_dir = state
            .config()
            .dirs
            .data
            .parent()
            .unwrap()
            .join("pronto/style");
        let (writes, receiver) = async_channel::unbounded::<Checkpoint>();
        let (loaded_tx, loaded_rx) = async_channel::bounded(1);
        let (wake, woken) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let loaded = match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|_| "AI settings could not be read; AI is disabled".to_owned()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Saved::default()),
                Err(_) => Err("AI settings could not be read; AI is disabled".to_owned()),
            };
            let _ = loaded_tx.send_blocking(loaded);
            while let Ok(checkpoint) = receiver.recv_blocking() {
                let result =
                    sealwax_infrastructure::xdg::ensure_private_dir(path.parent().unwrap())
                        .and_then(|_| {
                            sealwax_infrastructure::accounts::replace(
                                &path,
                                &serde_json::to_string(&checkpoint.saved).unwrap(),
                            )
                        });
                if result.is_err() {
                    tracing::warn!("AI checkpoint could not be saved");
                }
                if let Some(reply) = checkpoint.reply {
                    let _ = reply.send_blocking(
                        result.map_err(|_| "Cannot save AI progress; work paused".into()),
                    );
                }
            }
        });
        let this = Rc::new(Self {
            state: Rc::downgrade(state),
            saved: RefCell::default(),
            agents: RefCell::default(),
            default_agent: RefCell::default(),
            loaded: Cell::new(false),
            status: RefCell::new("Loading AI settings…".into()),
            cancel: RefCell::new(Cancel::default()),
            quit: Cancel::default(),
            stopped: Cell::new(false),
            in_flight: Cell::new(false),
            detected: Cell::new(false),
            detecting: Cell::new(false),
            settings_opened: Cell::new(false),
            failures: Cell::new(0),
            held_until: Cell::new(0),
            wake,
            subscription: RefCell::default(),
            writes,
            style_dir,
            style_examples: RefCell::default(),
            style_request: Cell::new(0),
            open_targets: RefCell::default(),
        });
        let weak = Rc::downgrade(&this);
        this.subscription
            .replace(Some(state.engine().subscribe(move |event| {
                if let Some(this) = weak.upgrade() {
                    this.event(event);
                }
            })));
        let weak = Rc::downgrade(&this);
        glib::spawn_future_local(async move {
            let loaded = loaded_rx.recv().await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            match loaded {
                Ok(Ok(mut saved)) => {
                    saved.migrate_style_sampling();
                    this.saved.replace(saved);
                }
                _ => {
                    this.status
                        .replace("AI settings could not be read; AI is disabled".into());
                    return;
                }
            }
            this.loaded.set(true);
            this.persist();
            this.status.replace("AI ready".into());
            // No CLI is run for a user who has both features off and never opens Settings.
            let options = this.options();
            if options.auto || options.style || this.settings_opened.get() {
                this.detect().await;
            }
            drop(this);
            loop {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                if this.stopped.get() {
                    return;
                }
                this.in_flight.set(true);
                if let Err(error) = this.step().await {
                    this.status.replace(error);
                }
                this.in_flight.set(false);
                drop(this);
                glib::timeout_future(TICK).await;
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let wait = this.next_step();
                drop(this);
                match wait {
                    // With nothing to do there is no timer: the worker sleeps until woken.
                    None => {
                        if woken.recv().await.is_err() {
                            return;
                        }
                    }
                    Some(wait) if wait > TICK => {
                        let _ = glib::future_with_timeout(wait - TICK, woken.recv()).await;
                    }
                    Some(_) => {}
                }
            }
        });
        this
    }
    /// Runs the installed CLIs' `--help` once, off the main loop: when a feature is on at
    /// startup, or when Settings is first opened.
    async fn detect(&self) {
        if self.detected.get() || self.detecting.replace(true) {
            return;
        }
        let cancel = self.quit.clone();
        let found = gio::spawn_blocking(move || {
            let agents = sealwax_infrastructure::agent::detect(&cancel);
            let default = sealwax_infrastructure::agent::default_agent(&agents);
            (agents, default)
        })
        .await;
        self.detecting.set(false);
        // The self-test's fixture agent may have been installed meanwhile.
        if !self.detected.get()
            && !self.quit.cancelled()
            && let Ok((agents, default)) = found
        {
            self.agents.replace(agents);
            self.default_agent.replace(default);
            self.detected.set(true);
        }
        self.wake();
    }
    /// Ends the worker's sleep: something was queued, requested, resumed or changed.
    fn wake(&self) {
        let _ = self.wake.try_send(());
    }
    fn next_step(&self) -> Option<Duration> {
        let state = self.state.upgrade()?;
        // The store opens after the first frame; until then there is nothing to read.
        if state.mailbox().shared_store().is_none() {
            return Some(TICK);
        }
        let accounts: Vec<i64> = state.mailbox().accounts().iter().map(|a| a.id.0).collect();
        self.saved.borrow().next_step(
            self.agent().is_some(),
            &accounts,
            self.held_until.get(),
            now_ms(),
        )
    }
    fn persist(&self) {
        if !self.loaded.get() {
            return;
        }
        let _ = self.writes.try_send(Checkpoint {
            saved: self.saved.borrow().clone(),
            reply: None,
        });
    }
    pub async fn checkpoint(&self) -> Result<(), String> {
        if !self.loaded.get() {
            return Ok(());
        }
        let (tx, rx) = async_channel::bounded(1);
        self.writes
            .try_send(Checkpoint {
                saved: self.saved.borrow().clone(),
                reply: Some(tx),
            })
            .map_err(|_| "AI checkpoint worker stopped")?;
        rx.recv()
            .await
            .map_err(|_| "AI checkpoint worker stopped")?
    }
    pub fn stop(&self) {
        self.stopped.set(true);
        self.style_examples.borrow_mut().clear();
        self.cancel.borrow().cancel();
        self.quit.cancel();
        self.persist();
        self.wake();
    }
    pub async fn finish(&self) {
        self.stop();
        // Cancellation is polled by the CLI worker every 25 ms and sync waits every 100 ms.
        while self.in_flight.get() {
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        let _ = self.checkpoint().await;
    }
    pub fn options(&self) -> Options {
        self.saved.borrow().options.clone()
    }
    pub fn set_options(&self, mut options: Options) {
        if !self.loaded.get() {
            return;
        }
        options.style_messages = options.style_messages.clamp(1, MORE_STYLE_MESSAGES);
        options.style_months = options.style_months.clamp(1, MORE_STYLE_MONTHS);
        options.style_refresh_messages = options
            .style_refresh_messages
            .clamp(1, DEFAULT_STYLE_REFRESH_MESSAGES);
        let before = self.options();
        if options.auto && !before.auto {
            options.enabled_at = now_ms();
        }
        if !options.style {
            self.style_examples.borrow_mut().clear();
        }
        if !options.auto {
            self.saved.borrow_mut().queue.clear();
        }
        // Paused work stays paused, and a limit or cap does not interrupt the agent.
        if restarts_work(&before, &options) {
            self.cancel.borrow().cancel();
            self.cancel.replace(Cancel::default());
        }
        self.saved.borrow_mut().options = options;
        self.persist();
        self.wake();
    }
    fn selected_agent(&self) -> Option<Agent> {
        let options = self.options();
        let name = if options.agent == "default" {
            self.default_agent.borrow().clone()?
        } else {
            options.agent
        };
        self.agents
            .borrow()
            .iter()
            .find(|a| a.name == name)
            .cloned()
    }
    fn agent(&self) -> Option<Agent> {
        self.selected_agent().filter(|a| a.available)
    }
    fn event(&self, event: &Event) {
        if !self.loaded.get() {
            return;
        }
        // A new account has no style sheet yet; the sleeping worker would not notice it.
        if matches!(
            event,
            Event::AccountAdded { .. } | Event::FoldersSynced { .. }
        ) && self.options().style
        {
            self.wake();
        }
        if !self.options().auto {
            return;
        }
        if let Event::NewMail {
            account,
            folder,
            messages,
        } = event
        {
            let Some(state) = self.state.upgrade() else {
                return;
            };
            if !state.mailbox().folder(*folder).is_some_and(|f| {
                !matches!(
                    f.role,
                    FolderRole::Sent | FolderRole::Drafts | FolderRole::Trash | FolderRole::Junk
                )
            }) {
                return;
            }
            self.enqueue(messages.iter().map(|m| (*account, *m)), false);
        }
    }
    fn enqueue(&self, jobs: impl IntoIterator<Item = (AccountId, MessageId)>, explicit: bool) {
        self.saved.borrow_mut().enqueue(jobs, explicit);
        self.persist();
        self.wake();
    }
    pub fn unread_now(self: &Rc<Self>) {
        if !self.options().auto || self.agent().is_none() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(state) = this.state.upgrade() else {
                return;
            };
            let Some(store) = state.mailbox().shared_store() else {
                return;
            };
            // More than a day's allowance could not be drafted anyway.
            let cap = UNREAD_NOW_MESSAGES.min(this.options().daily_cap);
            let mut rows = Vec::new();
            for folder in state
                .mailbox()
                .folders()
                .into_iter()
                .filter(|f| f.role == FolderRole::Inbox)
            {
                // A view is newest first, so each Inbox's first page holds its candidates.
                if let Ok(page) = store
                    .page(
                        ViewSpec::folder(folder.id).with_filter(ViewFilter::Unread),
                        0,
                        cap,
                    )
                    .recv()
                    .await
                {
                    rows.extend(page.rows.iter().map(|m| (m.date, m.id.0, folder.account)));
                }
            }
            if this.options().auto {
                this.enqueue(newest_unread(rows, cap as usize), true);
            }
        });
    }
    pub fn style_path(&self, account: &Account) -> PathBuf {
        // Byte encoding preserves uniqueness without allowing account keys to escape the directory.
        let key = account
            .key
            .as_ref()
            .map(|k| k.0.clone())
            .unwrap_or_else(|| account.id.0.to_string());
        let safe: String = key
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                    (b as char).to_string()
                } else {
                    format!("%{b:02x}")
                }
            })
            .collect();
        self.style_dir.join(format!("{safe}.md"))
    }
    pub fn present(self: &Rc<Self>, window: &impl gtk4::prelude::IsA<gtk4::Widget>) {
        self.settings_opened.set(true);
        if self.loaded.get() && !self.detected.get() {
            let this = self.clone();
            glib::spawn_future_local(async move { this.detect().await });
        }
        settings::present(self, window);
    }
    pub fn ready_draft(&self, message: MessageId) -> Option<DraftId> {
        self.saved
            .borrow()
            .ready
            .iter()
            .find(|r| r.message == message.0)
            .map(|r| DraftId(r.draft))
            .or_else(|| self.open_targets.borrow().get(&message).copied())
    }
    pub fn open_ready(self: &Rc<Self>, message: MessageId) -> bool {
        let Some(id) = self.ready_draft(message) else {
            return false;
        };
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(state) = this.state.upgrade() else {
                return;
            };
            let Some(store) = state.mailbox().shared_store() else {
                return;
            };
            if store
                .draft(id)
                .recv()
                .await
                .ok()
                .flatten()
                .is_some_and(|d| d.send.is_none())
            {
                use gtk4::prelude::*;
                if let Some(app) = state
                    .window()
                    .and_then(|w| w.window().application())
                    .and_downcast::<crate::application::ProntoApplication>()
                {
                    crate::composer::open(&app, crate::composer::ComposeRequest::Draft(id));
                }
            } else {
                this.saved.borrow_mut().ready.retain(|r| r.draft != id.0);
                this.open_targets
                    .borrow_mut()
                    .retain(|_, draft| *draft != id);
                this.persist();
                if let Some(reader) = state.reader() {
                    reader.reply_inline(message, None, ReplyKind::Reply);
                }
            }
        });
        true
    }
    async fn refresh_ready(&self, store: &Store) {
        let ready = self.saved.borrow().ready.clone();
        let mut valid = Vec::new();
        for item in ready {
            if store
                .draft(DraftId(item.draft))
                .recv()
                .await
                .ok()
                .flatten()
                .is_some_and(|d| d.send.is_none())
            {
                valid.push(item);
            }
        }
        let mut targets = std::collections::HashMap::new();
        for item in &valid {
            if let Ok(thread) = store.conversation(MessageId(item.message)).recv().await
                && let Some(thread) = thread.value
            {
                for message in thread.members() {
                    targets.insert(message, DraftId(item.draft));
                }
            }
        }
        self.open_targets.replace(targets);
        if valid.len() != self.saved.borrow().ready.len() {
            self.saved.borrow_mut().ready = valid;
            self.persist();
        }
        if let Some(list) = self.state.upgrade().and_then(|s| s.list()) {
            list.set_ai_drafts(
                self.saved
                    .borrow()
                    .ready
                    .iter()
                    .map(|r| ThreadId(r.thread))
                    .collect(),
            );
        }
    }
    async fn step(&self) -> Result<(), String> {
        let Some(state) = self.state.upgrade() else {
            return Ok(());
        };
        let Some(store) = state.mailbox().shared_store() else {
            return Ok(());
        };
        self.refresh_ready(&store).await;
        if self.saved.borrow().paused {
            return Ok(());
        }
        let Some(agent) = self.agent() else {
            return Ok(());
        };
        let cancel = self.cancel.borrow().clone();
        let options = self.options();
        if options.auto {
            let day = now_ms() / 86_400_000;
            {
                let mut saved = self.saved.borrow_mut();
                if saved.day != day {
                    saved.day = day;
                    saved.used = 0;
                }
            }
            // After a failure the whole queue waits, so an outage costs one attempt per wait.
            let next = (self.held_until.get() <= now_ms())
                .then(|| {
                    self.saved
                        .borrow()
                        .queue
                        .iter()
                        .find(|j| j.retry_at <= now_ms())
                        .cloned()
                })
                .flatten();
            if let Some(job) = next {
                if self.saved.borrow().used >= options.daily_cap {
                    self.status.replace(
                        "Daily draft cap reached; queued replies wait until tomorrow".into(),
                    );
                } else {
                    self.status.replace(format!(
                        "Drafting reply · {} queued",
                        self.saved.borrow().queue.len()
                    ));
                    let result = self.draft(&state, &store, &agent, &job, &cancel).await;
                    if cancel.cancelled() {
                        return Ok(());
                    }
                    let mut saved = self.saved.borrow_mut();
                    if let Some(index) = saved.queue.iter().position(|j| j.message == job.message) {
                        let mut job = saved.queue.remove(index);
                        match &result {
                            // Only an answer shows the agent works; a skipped message does not.
                            Ok(true) => self.failures.set(0),
                            Ok(false) => {}
                            Err(_) => {
                                let streak = self.failures.get();
                                self.failures.set(streak.saturating_add(1));
                                if let Some(delay) = retry_delay(job.attempts, streak) {
                                    // To the back, so one bad message cannot block the others.
                                    job.attempts += 1;
                                    job.retry_at = now_ms() + delay;
                                    self.held_until.set(job.retry_at);
                                    saved.queue.push(job);
                                }
                            }
                        }
                    }
                    let left = saved.queue.len();
                    drop(saved);
                    self.persist();
                    if let Err(error) = result {
                        return Err(if left == 0 {
                            error
                        } else {
                            format!("{error} · {left} queued, will retry")
                        });
                    }
                    self.status.replace("Draft queue updated".into());
                    return Ok(());
                }
            }
        }
        if options.style {
            for account in state.mailbox().accounts() {
                self.saved
                    .borrow_mut()
                    .styles
                    .entry(account.id.0)
                    .or_default();
                let style = self.saved.borrow().styles[&account.id.0].clone();
                if (style.built == 0
                    || style.prepared
                    || style.refresh_requested
                    || style.rebuild_requested)
                    && style.retry_at <= now_ms()
                {
                    let result = self
                        .style_step(&state, &store, &agent, &account, style, &cancel)
                        .await;
                    if result.is_err() && !cancel.cancelled() {
                        if let Some(style) = self.saved.borrow_mut().styles.get_mut(&account.id.0) {
                            style.retry_at = now_ms() + 60_000;
                        }
                        self.persist();
                    }
                    return result;
                }
            }
        }
        Ok(())
    }
    async fn generate(
        &self,
        agent: &Agent,
        prompt: String,
        cancel: &Cancel,
    ) -> Result<String, String> {
        let agent = agent.clone();
        let model = self.options().model;
        let token = cancel.clone();
        let result = gio::spawn_blocking(move || agent.generate(&model, &prompt, &token))
            .await
            .map_err(|_| "AI worker stopped")?;
        if cancel.cancelled() {
            return Err("AI work cancelled".into());
        }
        result
    }
    /// Shared by all AI draft/compose prompts; edited sheets are read fresh for each request.
    async fn style_reference(&self, account: &Account) -> Result<String, String> {
        if !self.options().style {
            return Ok(String::new());
        }
        let path = self.style_path(account);
        gio::spawn_blocking(move || match std::fs::read_to_string(path) {
            Ok(text) => Ok(text.chars().take(24_000).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(_) => Err("Cannot read the writing-style sheet".into()),
        })
        .await
        .map_err(|_| "Cannot read style sheet")?
    }
    async fn draft(
        &self,
        state: &AppState,
        store: &Store,
        agent: &Agent,
        job: &Job,
        cancel: &Cancel,
    ) -> Result<bool, String> {
        let id = MessageId(job.message);
        let Some(account) = state
            .mailbox()
            .accounts()
            .into_iter()
            .find(|a| a.id.0 == job.account)
        else {
            return Ok(false);
        };
        let settings = state
            .preferences()
            .map(|p| p.accounts())
            .unwrap_or_default();
        let identities = senders(&account, &settings);
        let own: Vec<String> = state
            .mailbox()
            .accounts()
            .iter()
            .flat_map(|a| senders(a, &settings))
            .map(|i| i.address)
            .collect();
        let Some(conversation) = store
            .conversation(id)
            .recv()
            .await
            .map_err(|_| "Cannot read thread")?
            .value
        else {
            return Ok(false);
        };
        if !job.explicit
            && conversation
                .messages
                .iter()
                .find(|m| m.summary.id == id)
                .is_none_or(|m| m.summary.date < self.options().enabled_at / 1000)
        {
            return Ok(false);
        }
        if text::already_replied(&conversation, id, &own) {
            return Ok(false);
        }
        for member in &conversation.messages {
            if let Some(existing) = store
                .reply_draft(member.summary.id)
                .recv()
                .await
                .map_err(|_| "Cannot check existing draft")?
            {
                // Recover the tiny crash window between saving the normal draft and its marker.
                if ai_draft(&existing.draft.message_id)
                    && !self
                        .saved
                        .borrow()
                        .ready
                        .iter()
                        .any(|r| r.draft == existing.id.0)
                {
                    self.saved.borrow_mut().ready.push(Ready {
                        message: member.summary.id.0,
                        thread: conversation.thread.0,
                        draft: existing.id.0,
                    });
                    self.checkpoint().await?;
                }
                return Ok(false);
            }
        }
        let source_raw = raw(state, store, id, cancel).await?;
        let check = source_raw.clone();
        let own_copy = own.clone();
        if !gio::spawn_blocking(move || text::eligible(&check, &own_copy))
            .await
            .unwrap_or(false)
        {
            return Ok(false);
        }
        let original = gio::spawn_blocking(move || reply::Original::parse(&source_raw))
            .await
            .map_err(|_| "Cannot parse mail")?;
        let plan = reply::plan(&original, ReplyKind::Reply, &identities);
        let boundary = text::boundary()?;
        let mut thread = String::new();
        for member in conversation.messages.iter().rev().take(12).rev() {
            let raw = raw(state, store, member.summary.id, cancel).await?;
            let token = boundary.clone();
            let block = gio::spawn_blocking(move || text::thread_message(&raw, &token))
                .await
                .map_err(|_| "Cannot read thread")?;
            thread.push_str(&block);
        }
        let style = self.style_reference(&account).await?;
        let signature = plan.sender.signature.clone().unwrap_or_default();
        let prompt = text::draft_prompt(&thread, &signature, &style, &boundary);
        self.saved.borrow_mut().used += 1;
        self.checkpoint().await?;
        let generated = self.generate(agent, prompt, cancel).await?;
        if text::no_reply(&generated) {
            return Ok(true);
        }
        let typed = text::plain_html(&generated);
        let mut draft = plan.draft(
            account.id,
            id,
            format!("{AI_DRAFT_PREFIX}{}", message_id(&plan.sender.address)),
            &typed,
            vec![],
        );
        // Local normal reply with a tagged editable signature. No downloaded quote attachments.
        draft.html = sealwax_domain::compose::body(&typed, plan.signature().as_ref(), None);
        // The store saves only at the version the check read. Unrelated writes (mail arriving
        // in another folder) move the version too, so a refused save is checked again rather
        // than discarding the finished draft; the check decides whether it is still wanted.
        let mut refused = None;
        for _ in 0..SAVE_ATTEMPTS {
            let Some(version) = self
                .still_wanted(state, store, id, account.id, &own, cancel)
                .await?
            else {
                return Ok(true);
            };
            // Refused at a version that still stands: the store's own check found a draft in
            // the thread that the reader does not report (one open in a composer, a forward).
            if refused == Some(version) {
                return Ok(true);
            }
            refused = Some(version);
            if let DraftSaved::Saved { id: draft, .. } = store
                .save_auto_reply(
                    SaveDraft {
                        id: None,
                        draft: draft.clone(),
                        open: false,
                        now: now_ms(),
                    },
                    version,
                )
                .recv()
                .await
                .map_err(|_| "Cannot save AI draft")?
            {
                self.saved.borrow_mut().ready.push(Ready {
                    message: id.0,
                    thread: conversation.thread.0,
                    draft: draft.0,
                });
                self.persist();
                let _ = state.engine().send(Command::DraftsChanged {
                    account: account.id,
                });
                return Ok(true);
            }
        }
        Err("Mail kept changing while the draft was saved".into())
    }
    /// The check after the model wait, and again after a refused save: the user may have
    /// replied, composed, or removed the account. `Some` carries the store version the answer
    /// was read at; `None` means the draft is no longer wanted.
    async fn still_wanted(
        &self,
        state: &AppState,
        store: &Store,
        id: MessageId,
        account: AccountId,
        own: &[String],
        cancel: &Cancel,
    ) -> Result<Option<StoreVersion>, String> {
        let checked = store
            .conversation(id)
            .recv()
            .await
            .map_err(|_| "Cannot recheck thread")?;
        let version = checked.version;
        let Some(current) = checked.value else {
            return Ok(None);
        };
        if state
            .reader()
            .and_then(|r| r.quick_reply())
            .is_some_and(|q| {
                !q.text.trim().is_empty() && current.members().any(|id| id == q.target)
            })
        {
            return Ok(None);
        }
        if text::already_replied(&current, id, own) || cancel.cancelled() || !self.options().auto {
            return Ok(None);
        }
        for member in &current.messages {
            if store
                .reply_draft(member.summary.id)
                .recv()
                .await
                .map_err(|_| "Cannot recheck drafts")?
                .is_some()
            {
                return Ok(None);
            }
        }
        if !state.mailbox().accounts().iter().any(|a| a.id == account) {
            return Ok(None);
        }
        Ok(Some(version))
    }
}

async fn raw(
    state: &AppState,
    store: &Store,
    message: MessageId,
    cancel: &Cancel,
) -> Result<Vec<u8>, String> {
    if cancel.cancelled() {
        return Err("Cancelled".into());
    }
    let targets = store
        .body_targets(vec![message])
        .recv()
        .await
        .map_err(|_| "Cannot read message")?;
    let target = targets.first().ok_or("Message was removed")?;
    if !matches!(target.body_state, BodyState::Full | BodyState::Text) {
        let (tx, rx) = async_channel::bounded(1);
        let _subscription = state.engine().subscribe(move |event| {
            if let Event::BodyFetched {
                message: id,
                result,
            } = event
                && *id == message
            {
                let _ = tx.try_send(result.is_ok());
            }
        });
        state
            .engine()
            .send(Command::FetchBodies {
                messages: vec![message],
            })
            .map_err(|_| "Waiting for mail sync")?;
        if wait(rx, cancel).await != Some(true) {
            return Err("Message download failed; will retry".into());
        }
    }
    store
        .raw_message(message)
        .recv()
        .await
        .map_err(|_| "Cannot read message")?
        .ok_or_else(|| "Message body is unavailable".into())
}
async fn wait<T>(rx: async_channel::Receiver<T>, cancel: &Cancel) -> Option<T> {
    for _ in 0..300 {
        if cancel.cancelled() {
            return None;
        }
        if let Ok(value) = rx.try_recv() {
            return Some(value);
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(message: i64, retry_at: i64) -> Job {
        Job {
            message,
            account: 1,
            attempts: 0,
            retry_at,
            explicit: false,
        }
    }
    fn drafting() -> Saved {
        Saved {
            options: Options {
                auto: true,
                ..Options::default()
            },
            ..Saved::default()
        }
    }
    const DAY: i64 = 86_400_000;
    const NOW: i64 = 20_000 * DAY + 3_600_000;

    #[test]
    fn only_options_that_change_the_result_restart_work() {
        let before = Options {
            auto: true,
            ..Options::default()
        };
        let changed = |edit: fn(&mut Options)| {
            let mut after = before.clone();
            edit(&mut after);
            restarts_work(&before, &after)
        };
        assert!(!changed(|_| {}));
        assert!(changed(|o| o.agent = "grok".into()));
        assert!(changed(|o| o.model = "small".into()));
        assert!(changed(|o| o.style = true));
        assert!(changed(|o| o.auto = false));
        assert!(!changed(|o| o.daily_cap = 5));
        assert!(!changed(|o| o.style_messages = 10));
        assert!(!changed(|o| o.style_months = 3));
        assert!(!changed(|o| o.style_refresh_messages = 7));
        assert!(!changed(|o| o.enabled_at = 9));
        // Turning drafting on starts new work without touching a style build in flight.
        assert!(!restarts_work(&Options::default(), &before));
    }

    #[test]
    fn an_outage_backs_off_and_keeps_the_job() {
        // Failure after failure with no answer in between: the wait doubles to 30 minutes.
        let waits: Vec<_> = (0..7).map(|n| retry_delay(n, n)).collect();
        assert_eq!(
            waits,
            [
                30_000, 60_000, 120_000, 240_000, 480_000, 960_000, 1_800_000
            ]
            .map(Some)
        );
        assert_eq!(retry_delay(3, 40), Some(1_800_000));
        assert_eq!(retry_delay(OUTAGE_ATTEMPTS - 1, 7), None);
    }

    #[test]
    fn a_message_that_keeps_failing_while_the_agent_works_is_dropped() {
        assert_eq!(retry_delay(0, 0), Some(30_000));
        assert_eq!(retry_delay(1, 0), Some(30_000));
        assert_eq!(retry_delay(JOB_ATTEMPTS - 1, 0), None);
        // The same third failure during an outage is kept.
        assert!(retry_delay(JOB_ATTEMPTS - 1, 1).is_some());
    }

    #[test]
    fn unread_is_capped_newest_first_across_inboxes() {
        let (a, b) = (AccountId(1), AccountId(2));
        let rows = vec![(10, 1, a), (30, 2, a), (20, 3, b), (30, 4, b), (5, 5, b)];
        assert_eq!(
            newest_unread(rows.clone(), 3),
            [(b, MessageId(4)), (a, MessageId(2)), (b, MessageId(3))]
        );
        assert_eq!(newest_unread(rows.clone(), 50).len(), 5);
        assert!(newest_unread(rows, 0).is_empty());
        assert!(UNREAD_NOW_MESSAGES <= Options::default().daily_cap);
    }

    #[test]
    fn queueing_skips_queued_and_drafted_messages() {
        let mut saved = Saved::default();
        saved.ready.push(Ready {
            message: 3,
            thread: 1,
            draft: 1,
        });
        let jobs = [1, 2, 2, 3].map(|id| (AccountId(7), MessageId(id)));
        saved.enqueue(jobs, true);
        saved.enqueue(jobs, false);
        let queued: Vec<_> = saved
            .queue
            .iter()
            .map(|j| (j.message, j.account, j.explicit, j.attempts, j.retry_at))
            .collect();
        assert_eq!(queued, [(1, 7, true, 0, 0), (2, 7, true, 0, 0)]);
    }

    #[test]
    fn worker_sleeps_without_a_timer_when_there_is_nothing_to_do() {
        // Both features off, or on with nothing queued or pending.
        assert_eq!(Saved::default().next_step(true, &[1], 0, NOW), None);
        assert_eq!(drafting().next_step(true, &[1], 0, NOW), None);
        let mut saved = drafting();
        saved.queue.push(job(1, 0));
        assert_eq!(saved.next_step(true, &[1], 0, NOW), Some(TICK));
        // Paused, or without a usable agent, queued work does not tick either.
        assert_eq!(saved.next_step(false, &[1], 0, NOW), None);
        saved.paused = true;
        assert_eq!(saved.next_step(true, &[1], 0, NOW), None);
        // A queue left behind when drafting is off is not work.
        saved.paused = false;
        saved.options.auto = false;
        assert_eq!(saved.next_step(true, &[1], 0, NOW), None);
        // A saved draft is watched until it is sent or discarded, whatever else is off.
        saved.paused = true;
        saved.ready.push(Ready {
            message: 1,
            thread: 1,
            draft: 1,
        });
        assert_eq!(saved.next_step(false, &[], 0, NOW), Some(TICK));
    }

    #[test]
    fn worker_waits_for_retries_the_back_off_and_the_next_day() {
        let mut saved = drafting();
        saved.queue = vec![job(1, NOW + 90_000), job(2, NOW + 30_000)];
        assert_eq!(
            saved.next_step(true, &[], 0, NOW),
            Some(Duration::from_secs(30))
        );
        // The queue-wide back-off holds a job that is itself due.
        saved.queue.push(job(3, 0));
        assert_eq!(saved.next_step(true, &[], 0, NOW), Some(TICK));
        assert_eq!(
            saved.next_step(true, &[], NOW + 120_000, NOW),
            Some(Duration::from_secs(120))
        );
        // Long waits are re-evaluated, never slept through in one timer.
        assert_eq!(
            saved.next_step(true, &[], NOW + 3_600_000, NOW),
            Some(LONGEST_WAIT)
        );
        // At the daily cap the queue waits for the next UTC day; yesterday's count is stale.
        saved.used = saved.options.daily_cap;
        saved.day = NOW / DAY;
        assert_eq!(saved.next_step(true, &[], 0, NOW), Some(LONGEST_WAIT));
        assert_eq!(
            saved.next_step(true, &[], 0, (NOW / DAY + 1) * DAY - 5_000),
            Some(Duration::from_secs(5))
        );
        saved.day -= 1;
        assert_eq!(saved.next_step(true, &[], 0, NOW), Some(TICK));
    }

    #[test]
    fn worker_wakes_for_pending_style_sheets_only() {
        let mut saved = Saved::default();
        saved.options.style = true;
        // An account without a sheet is pending; a finished one is not.
        assert_eq!(saved.next_step(true, &[1], 0, NOW), Some(TICK));
        assert_eq!(saved.next_step(true, &[], 0, NOW), None);
        saved.styles.entry(1).or_default().built = NOW;
        assert_eq!(saved.next_step(true, &[1], 0, NOW), None);
        saved.styles.get_mut(&1).unwrap().refresh_requested = true;
        saved.styles.get_mut(&1).unwrap().retry_at = NOW + 60_000;
        assert_eq!(
            saved.next_step(true, &[1], 0, NOW),
            Some(Duration::from_secs(60))
        );
        saved.options.style = false;
        assert_eq!(saved.next_step(true, &[1], 0, NOW), None);
    }

    #[test]
    fn agent_drafts_are_recognised_by_either_prefix() {
        assert!(ai_draft(&format!("{AI_DRAFT_PREFIX}abc@example.test")));
        assert!(ai_draft("dispatch-ai-abc@example.test"));
        assert!(!ai_draft("abc@example.test"));
    }

    #[test]
    fn saved_state_reads_older_files_and_keeps_a_pause() {
        let saved: Saved = serde_json::from_str(
            r#"{"options":{"auto":true},"paused":true,"queue":[{"message":4,"account":1,"attempts":1,"retry_at":9}]}"#,
        )
        .unwrap();
        assert!(saved.paused && saved.options.auto && !saved.options.style);
        assert_eq!(saved.options.daily_cap, 200);
        assert!(!saved.queue[0].explicit);
        let again: Saved = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert!(again.paused);
        assert_eq!(again.queue[0].attempts, 1);
    }
}
