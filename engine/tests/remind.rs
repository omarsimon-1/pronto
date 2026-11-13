//! Remind Me through the engine (the fork's Superhuman pass, docs/superhuman/remind.md), against
//! the fake IMAP server (no network beyond 127.0.0.1, in CI): a remind's moves reach the server
//! after the Reminders folder is created there (CREATE, SUBSCRIBE; one the server already has
//! counts); the timer brings a conversation back to the INBOX at its time, not before; overdue
//! reminders come back at the engine's start and at a reconnect; offline, the moves wait in the
//! queue; Undo within the window never reaches the server; a folder the server refuses puts the
//! conversation back; on Gmail the folder is a label. Phase C: an answer that arrives while a
//! conversation waits ends its reminder and brings it back beside the answer ("if no reply"),
//! and Someday is never waited for. Synthetic data only.

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod};
use sealwax_domain::model::{AccountId, FolderId, MessageId, ThreadId};
use sealwax_domain::remind::ReminderState;
use sealwax_engine::api::{Command, Event, Password, Refused, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::SyncOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    Remind, Reminded, Reminder, Store, StoreOptions, ViewScope, ViewSpec,
};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WAIT: Duration = Duration::from_secs(20);

fn config() -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            now: Some(NOW),
            ..SyncOptions::default()
        },
        account_pass: Duration::from_secs(60),
        inbox_poll: Duration::from_secs(60),
        opened_poll: Duration::from_secs(60),
        idle_restart: Duration::from_secs(60),
        reuse_check: Duration::from_secs(60),
        noop_timeout: Duration::from_secs(2),
        backoff_min: Duration::from_millis(100),
        backoff_max: Duration::from_millis(800),
        logout_wait: Duration::from_millis(300),
        ..SyncConfig::default()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn now_s() -> i64 {
    now_ms() / 1000
}

struct Running {
    dir: tempfile::TempDir,
    engine: Option<Engine>,
    store: Arc<Store>,
    events: async_channel::Receiver<Event>,
    trust: sealwax_engine::net::Trust,
}

impl Running {
    fn start(fake: &Fake) -> Running {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("remind-")
            .tempdir_in(base)
            .unwrap();
        let dirs = SealwaxDirs::under(dir.path());
        let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
        let store = Arc::new(
            Store::open(
                &dirs.store_file(),
                StoreOptions {
                    blobs: Some(blobs),
                    ..StoreOptions::default()
                },
            )
            .unwrap(),
        );
        let (engine, events) = engine(&dirs, store.clone(), fake.trust.clone());
        Running {
            dir,
            engine: Some(engine),
            store,
            events,
            trust: fake.trust.clone(),
        }
    }

    /// Stops the engine and starts another on the same store, as Pronto's next start.
    fn restart(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
        }
        let dirs = SealwaxDirs::under(self.dir.path());
        let (engine, events) = engine(&dirs, self.store.clone(), self.trust.clone());
        self.engine = Some(engine);
        self.events = events;
    }

    fn send(&self, command: Command) {
        self.engine
            .as_ref()
            .unwrap()
            .commands()
            .try_send(command)
            .unwrap();
    }

    async fn event<T>(&mut self, within: Duration, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = tokio::time::timeout(left, self.events.recv())
                .await
                .expect("no such event in time")
                .expect("the engine stopped");
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    }

    async fn add(&mut self, fake: &Fake) -> AccountId {
        let settings = AccountSettings {
            key: AccountKey(String::new()),
            name: "remind".into(),
            label: None,
            color: None,
            address: "remind@example.test".into(),
            incoming: fake.endpoint(),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: fake.endpoint(),
            username: "remind@example.test".into(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        };
        self.send(Command::AddAccount {
            request: 1,
            settings,
            password: Password::new("fake-password".into()),
            sign_in: None,
        });
        let account = self
            .event(WAIT, |event| match event {
                Event::AccountAdded { result, .. } => Some(result.clone().unwrap()),
                _ => None,
            })
            .await;
        let status = self
            .event(WAIT, |event| match event {
                Event::AccountStatus {
                    account: id,
                    status,
                } if *id == account && *status != SyncStatus::Syncing => Some(*status),
                _ => None,
            })
            .await;
        assert_eq!(status, SyncStatus::Synced);
        account
    }

    async fn folder(&self, account: AccountId, name: &str) -> Option<FolderId> {
        self.store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|state| state.name_raw == name)
            .map(|state| state.folder)
    }

    /// The thread of the message with `uid` in `folder`.
    async fn thread(&self, folder: FolderId, uid: u32) -> ThreadId {
        let id: MessageId = self
            .store
            .folder_messages(folder)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|message| message.uid == uid)
            .unwrap_or_else(|| panic!("UID {uid} not stored"))
            .id;
        self.store
            .message(id)
            .recv()
            .await
            .unwrap()
            .value
            .unwrap()
            .summary
            .thread
    }

    /// Sends `threads` to Reminders until `wake_at` with an undo window of `window_ms`, and
    /// tells the engine, as the app does.
    async fn remind(
        &self,
        threads: Vec<ThreadId>,
        scope: ViewScope,
        wake_at: i64,
        window_ms: i64,
    ) -> Reminded {
        let reminded = self
            .store
            .remind(Remind {
                threads,
                scope,
                wake_at,
                not_before: now_ms() + window_ms,
            })
            .recv()
            .await
            .unwrap();
        for account in &reminded.accounts {
            self.send(Command::OpsQueued { account: *account });
        }
        self.send(Command::RemindersChanged);
        reminded
    }

    async fn reminders(&self) -> Vec<Reminder> {
        self.store.reminders().recv().await.unwrap()
    }

    async fn pending(&self, account: AccountId) -> u32 {
        self.store.pending_ops(account).recv().await.unwrap()
    }

    async fn check(&self) {
        let problems = self.store.check().recv().await.unwrap();
        assert!(problems.is_empty(), "store check: {problems:?}");
    }
}

fn engine(
    dirs: &SealwaxDirs,
    store: Arc<Store>,
    trust: sealwax_engine::net::Trust,
) -> (Engine, async_channel::Receiver<Event>) {
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let engine = Engine::start(EngineConfig {
        application: "io.github.omarsimon1.Pronto.Test".into(),
        store,
        blobs,
        paths: AccountPaths::new(dirs),
        trust,
        secrets: SecretBackend::Memory,
        detect: DetectOptions::default(),
        microsoft_client_id: None,
        google_client: None,
        sign_in: SignInOptions::default(),
        sync: config(),
    })
    .unwrap();
    let events = engine.events();
    (engine, events)
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
        }
    }
}

async fn within<F: Future<Output = bool>>(
    within: Duration,
    mut check: impl FnMut() -> F,
) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < within {
        if check().await {
            return Some(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// INBOX with 5 messages (UID 5 answers UID 1), Arkiv (`\Archive`) and Papirkurv (`\Trash`).
async fn fake(flavor: Flavor) -> Fake {
    let fake = Fake::start(flavor).await;
    {
        let mut world = fake.world();
        world.moves = true;
        world.create("INBOX", &[]);
        world.create("Arkiv", &["\\Archive"]);
        world.create("Papirkurv", &["\\Trash"]);
        world.append_many("INBOX", 4, |_| 1);
        world.append_extra(
            "INBOX",
            "In-Reply-To: <fake.1@example.test>\r\nReferences: <fake.1@example.test>\r\n",
        );
    }
    fake
}

/// Where the fake keeps the message with this Message-ID: the mailboxes, sorted.
fn whereabouts(fake: &Fake, message_id: &str) -> Vec<String> {
    let world = fake.world();
    world
        .mailboxes
        .keys()
        .filter(|name| world.ids(name).iter().any(|id| id == message_id))
        .cloned()
        .collect()
}

fn message_id(fake: &Fake, folder: &str, uid: u32) -> String {
    fake.world().mailboxes[folder].messages[&uid]
        .message_id
        .clone()
}

fn sent(fake: &Fake, verb: &str) -> Vec<String> {
    fake.world()
        .commands
        .iter()
        .filter(|command| command.starts_with(verb))
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reminder_waits_in_a_folder_made_on_the_server_and_comes_back_at_its_time() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await.unwrap();
    let thread = run.thread(inbox, 1).await;
    assert_eq!(run.thread(inbox, 5).await, thread, "one conversation");
    let ids = [message_id(&fake, "INBOX", 1), message_id(&fake, "INBOX", 5)];
    let wake_at = now_s() + 4;
    let reminded = run
        .remind(vec![thread], ViewScope::Folder(inbox), wake_at, 0)
        .await;
    assert_eq!(
        (reminded.moved, reminded.accounts.clone()),
        (2, vec![account])
    );

    // Made on the server and subscribed, before the move into it.
    let moved = within(WAIT, || async {
        ids.iter().all(|id| whereabouts(&fake, id) == ["Reminders"])
            && run.pending(account).await == 0
    })
    .await;
    assert!(moved.is_some(), "the move reaches the server");
    assert_eq!(sent(&fake, "CREATE"), ["CREATE \"Reminders\""]);
    assert!(fake.world().subscribed.contains("Reminders"));
    let commands = fake.world().commands.clone();
    let created = commands
        .iter()
        .position(|c| c.starts_with("CREATE"))
        .unwrap();
    let moving = commands
        .iter()
        .position(|c| c.starts_with("UID MOVE"))
        .unwrap();
    assert!(created < moving, "{commands:?}");
    let folder = run.folder(account, "Reminders").await.expect("stored");
    // Synced before the move, it took the rows themselves (COPYUID): the same conversation.
    let waiting = run.store.folder_messages(folder).recv().await.unwrap();
    assert_eq!(waiting.len(), 2);
    let reminder = &run.reminders().await[0];
    assert_eq!(
        (reminder.state, reminder.threads.clone()),
        (ReminderState::Waiting, vec![thread]),
        "not before its time"
    );

    // Its time: back in the INBOX on the server, and on top of the store's Inbox.
    let back = within(WAIT, || async {
        ids.iter().all(|id| whereabouts(&fake, id) == ["INBOX"]) && run.pending(account).await == 0
    })
    .await;
    assert!(back.is_some(), "the wake's move reaches the server");
    assert!(now_s() >= wake_at, "not before its time");
    let reminder = &run.reminders().await[0];
    assert_eq!(reminder.state, ReminderState::Reminded);
    assert!(reminder.reminded_at.unwrap() >= wake_at);
    let rows = run
        .store
        .page(ViewSpec::folder(inbox).conversations(), 0, 10)
        .recv()
        .await
        .unwrap()
        .conversations;
    assert_eq!(
        (rows[0].thread, rows[0].date),
        (reminder.threads[0], reminder.reminded_at.unwrap())
    );
    let stored = run.store.folder_messages(folder).recv().await.unwrap();
    assert!(stored.is_empty(), "nothing waits in Reminders");
    assert_eq!(sent(&fake, "CREATE").len(), 1, "made once");
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overdue_reminders_come_back_at_start_and_at_a_reconnect() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await.unwrap();
    let first = run.thread(inbox, 1).await;
    let third = run.thread(inbox, 3).await;
    // The first conversation holds message 5 too, which comes and goes with it. Their ids are
    // read before the moves take them out of the INBOX.
    assert_eq!(run.thread(inbox, 5).await, first, "one conversation");
    let ids = [message_id(&fake, "INBOX", 1), message_id(&fake, "INBOX", 3)];
    let fifth = message_id(&fake, "INBOX", 5);
    let later = now_s() + 3_600;
    run.remind(vec![first, third], ViewScope::Folder(inbox), later, 0)
        .await;
    let moved = within(WAIT, || async {
        ids.iter()
            .chain([&fifth])
            .all(|id| whereabouts(&fake, id) == ["Reminders"])
            && run.pending(account).await == 0
            && run
                .reminders()
                .await
                .iter()
                .all(|reminder| !reminder.threads.is_empty())
    })
    .await;
    assert!(moved.is_some());
    // Synced before the moves, the Reminders folder took the messages' rows as they were.
    let now: Vec<ThreadId> = run
        .reminders()
        .await
        .iter()
        .map(|reminder| reminder.threads[0])
        .collect();
    assert_eq!(now, [first, third]);

    // The machine slept through the first one's time: the timer, asleep, never knew; the
    // reconnect after the resume brings it back.
    let due = now_s();
    run.store
        .remind(Remind {
            threads: vec![first],
            scope: ViewScope::Folder(inbox),
            wake_at: due,
            not_before: 0,
        })
        .recv()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(run.reminders().await[0].state, ReminderState::Waiting);
    run.send(Command::Reconnect);
    // The whole conversation, both moves recorded as done: a restart before the store records
    // message 5's would find it pending below (the final review of phase C: a flake).
    let back = within(WAIT, || async {
        [&ids[0], &fifth]
            .iter()
            .all(|id| whereabouts(&fake, id) == ["INBOX"])
            && run.pending(account).await == 0
    })
    .await;
    assert!(back.is_some(), "back after the reconnect");

    // Pronto was closed at the other's time: it comes back at the next start.
    run.store
        .remind(Remind {
            threads: vec![third],
            scope: ViewScope::Folder(inbox),
            wake_at: now_s(),
            not_before: 0,
        })
        .recv()
        .await
        .unwrap();
    run.restart();
    let woken = within(WAIT, || async {
        run.reminders()
            .await
            .iter()
            .all(|reminder| reminder.state == ReminderState::Reminded)
    })
    .await;
    assert!(woken.is_some(), "woken at the start");
    // Its move back waits for the account to connect, as any queued op.
    assert_eq!(run.pending(account).await, 1);
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_a_reminders_moves_wait_in_the_queue() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await.unwrap();
    let thread = run.thread(inbox, 4).await;
    let id = message_id(&fake, "INBOX", 4);
    fake.world().black_hole(true);
    run.send(Command::Reconnect);
    run.remind(vec![thread], ViewScope::Folder(inbox), now_s() + 3_600, 0)
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(run.pending(account).await, 1, "the move waits");
    assert!(sent(&fake, "CREATE").is_empty());
    fake.world().black_hole(false);
    run.send(Command::Reconnect);
    let done = within(WAIT, || async {
        run.pending(account).await == 0 && whereabouts(&fake, &id) == ["Reminders"]
    })
    .await;
    assert!(done.is_some(), "made and moved once the account is back");
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undo_within_the_window_never_reaches_the_server() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await.unwrap();
    let thread = run.thread(inbox, 2).await;
    let reminded = run
        .remind(
            vec![thread],
            ViewScope::Folder(inbox),
            now_s() + 3_600,
            1_500,
        )
        .await;
    assert!(run.folder(account, "Reminders").await.is_some());
    tokio::time::sleep(Duration::from_millis(500)).await;
    let undone = run
        .store
        .undo(reminded.batch.unwrap())
        .recv()
        .await
        .unwrap();
    assert_eq!(undone.restored, 1);
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    assert!(sent(&fake, "CREATE").is_empty() && sent(&fake, "UID MOVE").is_empty());
    assert_eq!(run.folder(account, "Reminders").await, None);
    assert!(run.reminders().await.is_empty());
    assert_eq!(run.pending(account).await, 0);
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_the_server_has_counts_and_one_it_refuses_puts_the_mail_back() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await.unwrap();
    // Made on the server since the store last listed the folders.
    fake.world().create("Reminders", &[]);
    let thread = run.thread(inbox, 3).await;
    let id = message_id(&fake, "INBOX", 3);
    run.remind(vec![thread], ViewScope::Folder(inbox), now_s() + 3_600, 0)
        .await;
    let moved = within(WAIT, || async {
        whereabouts(&fake, &id) == ["Reminders"] && run.pending(account).await == 0
    })
    .await;
    assert!(moved.is_some(), "[ALREADYEXISTS] counts as made");
    assert_eq!(sent(&fake, "CREATE").len(), 1);

    // Another account whose server refuses the folder.
    let other = self::fake(Flavor::Qresync).await;
    other
        .world()
        .refuse("CREATE", "NO [NOPERM] Not allowed to create mailboxes");
    let mut run = Running::start(&other);
    let account = run.add(&other).await;
    let inbox = run.folder(account, "INBOX").await.unwrap();
    let thread = run.thread(inbox, 3).await;
    run.remind(vec![thread], ViewScope::Folder(inbox), now_s() + 3_600, 0)
        .await;
    let refused = run
        .event(WAIT, |event| match event {
            Event::OpsRefused {
                change, messages, ..
            } => Some((*change, *messages)),
            _ => None,
        })
        .await;
    assert_eq!(refused, (Refused::Move, 1));
    assert!(sent(&other, "UID MOVE").is_empty());
    assert_eq!(run.folder(account, "Reminders").await, None);
    let shown = run
        .store
        .page(ViewSpec::folder(inbox), 0, 10)
        .recv()
        .await
        .unwrap()
        .rows;
    assert_eq!(shown.len(), 5, "shown again where it was");
    assert_eq!(run.reminders().await[0].state, ReminderState::Waiting);
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_gmail_the_reminders_folder_is_a_label() {
    let fake = Fake::start(Flavor::Gmail).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.create("[Gmail]", &["\\Noselect"]);
        world.create("[Gmail]/All Mail", &["\\All"]);
        world.create("[Gmail]/Trash", &["\\Trash"]);
        world.append_many("INBOX", 3, |_| 1);
    }
    let mut run = Running::start(&fake);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await.unwrap();
    let thread = run.thread(inbox, 2).await;
    let id = message_id(&fake, "INBOX", 2);
    run.remind(vec![thread], ViewScope::Folder(inbox), now_s() + 3, 0)
        .await;
    let moved = within(WAIT, || async {
        whereabouts(&fake, &id).contains(&"Reminders".to_owned())
            && !whereabouts(&fake, &id).contains(&"INBOX".to_owned())
    })
    .await;
    assert!(moved.is_some(), "labelled Reminders, out of the INBOX");
    // A label at the top of the account, beside [Gmail]: not under the INBOX.
    assert_eq!(sent(&fake, "CREATE"), ["CREATE \"Reminders\""]);
    let back = within(WAIT, || async {
        whereabouts(&fake, &id).contains(&"INBOX".to_owned())
            && !whereabouts(&fake, &id).contains(&"Reminders".to_owned())
            && run.pending(account).await == 0
    })
    .await;
    assert!(back.is_some(), "back in the INBOX at its time");
    assert_eq!(run.reminders().await[0].state, ReminderState::Reminded);
    // All Mail was never first-synced for it.
    let all = run.folder(account, "[Gmail]/All Mail").await.unwrap();
    assert!(
        run.store
            .folder_messages(all)
            .recv()
            .await
            .unwrap()
            .is_empty()
    );
    run.check().await;
}

/// Superhuman's "if no reply" (phase C): someone answers a conversation waiting in Reminders;
/// the answer arrives in the INBOX (through IDLE here), the engine's new mail wakes the timer,
/// the reminder ends, and what waited moves back to the INBOX beside the answer, long before
/// its time. Someday's reminder beside it isn't waited for, and stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_ends_the_reminder_and_brings_the_conversation_back() {
    use sealwax_domain::remind::SOMEDAY;
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await.unwrap();
    let thread = run.thread(inbox, 1).await;
    let other = run.thread(inbox, 3).await;
    let ids = [message_id(&fake, "INBOX", 1), message_id(&fake, "INBOX", 5)];
    let later = now_s() + 3_600;
    run.remind(vec![thread], ViewScope::Folder(inbox), later, 0)
        .await;
    run.remind(vec![other], ViewScope::Folder(inbox), SOMEDAY, 0)
        .await;
    let moved = within(WAIT, || async {
        ids.iter().all(|id| whereabouts(&fake, id) == ["Reminders"])
            && run.pending(account).await == 0
    })
    .await;
    assert!(moved.is_some(), "both wait in Reminders");
    assert!(
        within(WAIT, || async { fake.world().count("IDLE") >= 1 })
            .await
            .is_some(),
        "the IDLE connection idles"
    );

    // An answer to the newest message, from someone else.
    let answered = Instant::now();
    fake.world().append_extra(
        "INBOX",
        &format!(
            "In-Reply-To: <{}>\r\nReferences: <{}> <{}>\r\n",
            ids[1], ids[0], ids[1]
        ),
    );
    let back = within(WAIT, || async {
        ids.iter().all(|id| whereabouts(&fake, id) == ["INBOX"]) && run.pending(account).await == 0
    })
    .await;
    assert!(back.is_some(), "what waited is back on the server");
    println!("answered: back in the INBOX after {:?}", answered.elapsed());
    let reminders = run.reminders().await;
    assert_eq!(reminders.len(), 1, "the answered one ended: {reminders:?}");
    assert_eq!(
        (reminders[0].state, reminders[0].wake_at),
        (ReminderState::Waiting, SOMEDAY)
    );
    let rows = run
        .store
        .page(ViewSpec::folder(inbox).conversations(), 0, 10)
        .recv()
        .await
        .unwrap()
        .conversations;
    assert_eq!(rows[0].thread, thread, "the answer sorts it to the top");
    assert_eq!(rows[0].members, 3);
    run.check().await;
}
