//! Gmail as the fork's owner uses it (the Superhuman pass), through the engine service against
//! the fake IMAP server's Gmail flavour (CI, no network beyond 127.0.0.1): an Archive takes the
//! message out of the Inbox without ever syncing All Mail, and Undo inside its window keeps it
//! off the server; Gmail's one HIGHESTMODSEQ for the whole account no longer passes every
//! synced folder at every account pass, while the flag sweep still finds flag-only changes.
//! Synthetic data only.

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, Security};
use sealwax_domain::compose::Draft;
use sealwax_domain::model::{AccountId, Address, FolderId, MessageId};
use sealwax_domain::triage::Triage;
use sealwax_engine::api::{Command, Event, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::{self, FolderLocks, SyncOptions, SyncReport};
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    DraftSaved, FirstSyncStep, FolderSync, Lost, NewAccount, QueueTriage, Queued, SaveDraft,
    SendQueued, Store, StoreOptions, SyncState,
};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const WAIT: Duration = Duration::from_secs(20);
const ALL_MAIL: &str = "[Gmail]/All Mail";
const SENT: &str = "[Gmail]/Sent Mail";
/// The sync's header FETCH, and only it (a move's COPYUID check fetches Message-IDs alone).
const HEADERS: &str = "HEADER.FIELDS (DATE";

/// Account passes and IDLE restarts rare, so a test sees what it causes itself.
fn config() -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            now: Some(NOW),
            ..SyncOptions::default()
        },
        account_pass: Duration::from_secs(60),
        inbox_poll: Duration::from_millis(300),
        opened_poll: Duration::from_millis(300),
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

struct Running {
    _dir: tempfile::TempDir,
    engine: Option<Engine>,
    store: Arc<Store>,
    events: async_channel::Receiver<Event>,
}

impl Running {
    fn start(fake: &Fake, sync: SyncConfig) -> Running {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("gmail-")
            .tempdir_in(base)
            .unwrap();
        let dirs = SealwaxDirs::under(dir.path());
        let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
        let store = Arc::new(
            Store::open(
                &dirs.store_file(),
                StoreOptions {
                    blobs: Some(blobs.clone()),
                    ..StoreOptions::default()
                },
            )
            .unwrap(),
        );
        let engine = Engine::start(EngineConfig {
            application: "io.github.omarsimon1.Pronto.Test".into(),
            store: store.clone(),
            blobs,
            paths: AccountPaths::new(&dirs),
            trust: fake.trust.clone(),
            secrets: SecretBackend::Memory,
            detect: DetectOptions::default(),
            microsoft_client_id: None,
            google_client: None,
            sign_in: SignInOptions::default(),
            sync,
        })
        .unwrap();
        let events = engine.events();
        Running {
            _dir: dir,
            engine: Some(engine),
            store,
            events,
        }
    }

    fn send(&self, command: Command) {
        self.engine
            .as_ref()
            .unwrap()
            .commands()
            .try_send(command)
            .unwrap();
    }

    /// The next event `pick` accepts within `within`.
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

    /// Adds the account on `fake` and waits for its first "synced".
    async fn add(&mut self, fake: &Fake) -> AccountId {
        self.add_with(fake, fake.endpoint()).await
    }

    /// [`Running::add`] with `smtp` as its submission server.
    async fn add_with(&mut self, fake: &Fake, smtp: Endpoint) -> AccountId {
        let settings = AccountSettings {
            key: AccountKey(String::new()),
            name: "Gmail".into(),
            label: None,
            color: None,
            address: "gmail@example.test".into(),
            incoming: fake.endpoint(),
            receive: sealwax_domain::account::Receive::Imap,
            smtp,
            username: "gmail@example.test".into(),
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
                Event::AccountAdded { request: 1, result } => Some(result.clone().unwrap()),
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

    async fn folder(&self, account: AccountId, name: &str) -> FolderId {
        self.state_named(account, name).await.folder
    }

    async fn state_named(&self, account: AccountId, name: &str) -> FolderSync {
        self.store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|state| state.name_raw == name)
            .unwrap_or_else(|| panic!("{name} not stored"))
    }

    async fn state(&self, folder: FolderId) -> FolderSync {
        self.store
            .folder_sync(folder)
            .recv()
            .await
            .unwrap()
            .unwrap()
    }

    async fn rows(&self, folder: FolderId) -> Vec<MessageId> {
        self.store
            .folder_messages(folder)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .map(|message| message.id)
            .collect()
    }

    /// The messages `folder` shows, the pending moves' overlay included.
    async fn shown(&self, folder: FolderId) -> u32 {
        self.store
            .folders()
            .recv()
            .await
            .unwrap()
            .value
            .into_iter()
            .find(|found| found.id == folder)
            .unwrap()
            .total
    }

    async fn pending(&self, account: AccountId) -> u32 {
        self.store.pending_ops(account).recv().await.unwrap()
    }

    /// Queues `action` as the app does, its undo window ending at `not_before`.
    async fn queue(&self, action: Triage, messages: Vec<MessageId>, not_before: i64) -> Queued {
        let queued = self
            .store
            .queue_triage(QueueTriage {
                action,
                messages,
                not_before,
            })
            .recv()
            .await
            .unwrap();
        for account in &queued.accounts {
            self.send(Command::OpsQueued { account: *account });
        }
        queued
    }
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
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// A Gmail account: INBOX, the `[Gmail]` folders, 300 archived messages in All Mail only and 5
/// in the Inbox (in All Mail too, as Gmail keeps them).
async fn gmail() -> Fake {
    let fake = Fake::start(Flavor::Gmail).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.create("[Gmail]", &["\\Noselect"]);
        world.create(ALL_MAIL, &["\\All"]);
        world.create("[Gmail]/Drafts", &["\\Drafts"]);
        world.create(SENT, &["\\Sent"]);
        world.create("[Gmail]/Spam", &["\\Junk"]);
        world.create("[Gmail]/Trash", &["\\Trash"]);
        for _ in 0..300 {
            world.deliver(ALL_MAIL, 30, &["\\Seen"]);
        }
        for _ in 0..5 {
            world.deliver("INBOX", 1, &[]);
        }
    }
    fake
}

async fn idling(fake: &Fake) {
    assert!(
        within(WAIT, || async { fake.world().count("IDLE") >= 1 })
            .await
            .is_some(),
        "the IDLE connection idles"
    );
}

/// Archive on Gmail is a MOVE into All Mail, which the store never synced: the message leaves
/// the Inbox here and on the server, All Mail keeps it under the UID it had, and All Mail is
/// never first-synced for it (before the Superhuman pass, the op runner passed the target,
/// fetching the window's headers, up to 5,000, on the command connection).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_leaves_the_inbox_without_syncing_all_mail() {
    let fake = gmail().await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let all = run.folder(account, ALL_MAIL).await;
    idling(&fake).await;
    assert_eq!(run.state(all).await.state, SyncState::New);
    assert_eq!(
        run.shown(all).await,
        305,
        "All Mail shows its STATUS counts"
    );
    let headers = fake.world().count(HEADERS);
    let archived: Vec<MessageId> = run.rows(inbox).await.into_iter().take(2).collect();
    let started = Instant::now();
    run.queue(Triage::Archive, archived, now_ms()).await;
    let done = within(WAIT, || async {
        run.pending(account).await == 0 && run.state(inbox).await.messages == 3
    })
    .await;
    assert!(done.is_some(), "archived");
    println!("archive of 2 on Gmail done after {:?}", started.elapsed());
    // Whatever comes next isn't held up: a flag change goes straight through.
    let starred = run.rows(inbox).await[0];
    let started = Instant::now();
    run.queue(Triage::Star(true), vec![starred], 0).await;
    assert!(
        within(Duration::from_secs(5), || async {
            fake.world().count("+FLAGS") >= 1 && run.pending(account).await == 0
        })
        .await
        .is_some(),
        "the next op ran"
    );
    println!("the op after it done after {:?}", started.elapsed());
    {
        let world = fake.world();
        assert_eq!(
            world.mailboxes["INBOX"].messages.len(),
            3,
            "gone from the Inbox"
        );
        assert_eq!(
            world.mailboxes[ALL_MAIL].messages.len(),
            305,
            "All Mail kept them, once each"
        );
        assert_eq!(world.count("UID MOVE"), 1);
        assert_eq!(
            world.count(HEADERS),
            headers,
            "no headers were fetched: All Mail wasn't synced"
        );
    }
    let state = run.state(all).await;
    assert_eq!(
        (state.state, state.messages, state.uidvalidity),
        (SyncState::New, 0, None)
    );
    assert_eq!(run.shown(all).await, 305);
    assert_eq!(run.shown(inbox).await, 3);
    assert_eq!(
        run.store.check().recv().await.unwrap(),
        Vec::<String>::new()
    );
}

/// All Mail opened once and its first sync left unfinished (Gmail's pace; the user moved on):
/// an Archive still passes no target into it, which would go on with that first sync, up to the
/// window's 5,000 headers, on the command connection (the Superhuman pass's review).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_doesnt_go_on_with_all_mails_unfinished_first_sync() {
    let fake = gmail().await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let all = run.folder(account, ALL_MAIL).await;
    idling(&fake).await;
    // As a first sync leaves All Mail when it is cut short: started, nothing fetched yet.
    let (uidvalidity, top) = {
        let world = fake.world();
        let mailbox = &world.mailboxes[ALL_MAIL];
        (mailbox.uidvalidity, mailbox.uidnext)
    };
    run.store
        .first_sync_step(
            all,
            FirstSyncStep::Start {
                uidvalidity,
                top,
                modseq: 0,
            },
        )
        .recv()
        .await
        .unwrap();
    assert!(matches!(
        run.state(all).await.state,
        SyncState::Initial { .. }
    ));
    let headers = fake.world().count(HEADERS);
    let archived: Vec<MessageId> = run.rows(inbox).await.into_iter().take(2).collect();
    run.queue(Triage::Archive, archived, now_ms()).await;
    let done = within(WAIT, || async {
        run.pending(account).await == 0 && run.state(inbox).await.messages == 3
    })
    .await;
    assert!(done.is_some(), "archived");
    assert_eq!(fake.world().count("UID MOVE"), 1);
    assert_eq!(
        fake.world().count(HEADERS),
        headers,
        "no headers were fetched: All Mail's first sync didn't go on"
    );
    assert!(matches!(
        run.state(all).await.state,
        SyncState::Initial { .. }
    ));
}

/// Undo inside the Archive's window takes it back before anything reaches Gmail; once the window
/// has passed the Archive is on the server, and as before the Superhuman pass All Mail (never
/// synced, and keeping the message's UID below the mark the move took) can't give it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undo_inside_the_window_keeps_an_archive_off_the_server() {
    let fake = gmail().await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    idling(&fake).await;
    let rows = run.rows(inbox).await;
    let queued = run
        .queue(Triage::Archive, vec![rows[0]], now_ms() + 1_500)
        .await;
    assert_eq!(run.shown(inbox).await, 4, "out of the Inbox at once");
    let undone = run.store.undo(queued.batch.unwrap()).recv().await.unwrap();
    assert_eq!((undone.restored, undone.lost.clone()), (1, Vec::new()));
    for account in &undone.accounts {
        run.send(Command::OpsQueued { account: *account });
    }
    assert_eq!(run.shown(inbox).await, 5, "back at once");
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    assert_eq!(fake.world().count("UID MOVE"), 0, "nothing reached Gmail");
    assert_eq!(fake.world().mailboxes["INBOX"].messages.len(), 5);
    assert_eq!(run.pending(account).await, 0);

    let queued = run.queue(Triage::Archive, vec![rows[1]], now_ms()).await;
    assert!(
        within(WAIT, || async { run.pending(account).await == 0 })
            .await
            .is_some()
    );
    assert_eq!(fake.world().mailboxes["INBOX"].messages.len(), 4);
    let undone = run.store.undo(queued.batch.unwrap()).recv().await.unwrap();
    assert_eq!(undone.lost, vec![(Lost::Unknown, 1)]);
    assert_eq!(
        run.store.check().recv().await.unwrap(),
        Vec::<String>::new()
    );
}

/// SMTP submission over implicit TLS with the fake's TLS identity (so the engine's trust takes
/// it): any login, any envelope; it keeps every message it takes.
async fn smtp(fake: &Fake) -> (Endpoint, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tokio_rustls::TlsAcceptor::from(fake.tls.clone());
    let taken = Arc::new(Mutex::new(Vec::new()));
    let kept = taken.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let Ok(stream) = acceptor.accept(tcp).await else {
                continue;
            };
            let kept = kept.clone();
            tokio::spawn(async move {
                let mut io = BufReader::new(stream);
                let mut data: Option<String> = None;
                let mut reply = String::from("220 mock ESMTP\r\n");
                loop {
                    if !reply.is_empty() {
                        let stream = io.get_mut();
                        if stream.write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                        let _ = stream.flush().await;
                        reply.clear();
                    }
                    let mut line = String::new();
                    if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if let Some(message) = data.as_mut() {
                        if line == ".\r\n" {
                            kept.lock().unwrap().push(data.take().unwrap());
                            reply.push_str("250 2.0.0 queued\r\n");
                        } else {
                            message.push_str(&line);
                        }
                        continue;
                    }
                    let upper = line.to_ascii_uppercase();
                    reply.push_str(if upper.starts_with("EHLO") {
                        "250-mock\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n"
                    } else if upper.starts_with("AUTH") {
                        "235 2.7.0 Authentication successful\r\n"
                    } else if upper.starts_with("DATA") {
                        data = Some(String::new());
                        "354 go ahead\r\n"
                    } else if upper.starts_with("QUIT") {
                        "221 bye\r\n"
                    } else if ["MAIL", "RCPT", "RSET", "NOOP"]
                        .iter()
                        .any(|verb| upper.starts_with(verb))
                    {
                        "250 ok\r\n"
                    } else {
                        "502 unknown\r\n"
                    });
                }
            });
        }
    });
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    };
    (endpoint, taken)
}

/// A reply sent joins its conversation within a moment, not at the next account pass: the Sent
/// folder is passed as soon as its copy is appended (and, where the server files its own copy
/// as Gmail does, a moment after the submission).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_sent_joins_its_conversation_at_once() {
    let fake = gmail().await;
    let (submission, taken) = smtp(&fake).await;
    let mut run = Running::start(&fake, config());
    let account = run.add_with(&fake, submission).await;
    let inbox = run.folder(account, "INBOX").await;
    let sent = run.folder(account, SENT).await;
    idling(&fake).await;
    let original = run.rows(inbox).await[0];
    let answered = run
        .store
        .message(original)
        .recv()
        .await
        .unwrap()
        .value
        .unwrap()
        .headers
        .message_id
        .unwrap();
    let draft = Draft {
        account,
        from: Address::new("Gmail", "gmail@example.test"),
        to: vec![Address::new("Fake Sender", "sender@example.test")],
        subject: "Re: Fake message 1".into(),
        html: "<p>Thanks, that works.</p>".into(),
        message_id: "reply.1@example.test".into(),
        in_reply_to: Some(answered.clone()),
        references: vec![answered],
        ..Draft::default()
    };
    let saved = run
        .store
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: true,
            now: now_ms(),
        })
        .recv()
        .await
        .unwrap();
    let DraftSaved::Saved { id, .. } = saved else {
        panic!("not saved: {saved:?}");
    };
    let started = Instant::now();
    match run.store.queue_send(id, now_ms()).recv().await.unwrap() {
        SendQueued::Queued { account, .. } => run.send(Command::OutboxQueued { account }),
        other => panic!("not queued: {other:?}"),
    }
    run.event(WAIT, |event| match event {
        Event::Sent { draft, .. } if *draft == id => Some(()),
        _ => None,
    })
    .await;
    let joined = within(Duration::from_secs(5), || async {
        run.store
            .conversation(original)
            .recv()
            .await
            .unwrap()
            .value
            .is_some_and(|conversation| conversation.members().count() == 2)
    })
    .await;
    assert!(
        joined.is_some(),
        "the reply joined its conversation, well before the 60 s account pass"
    );
    println!(
        "a reply sent joined its conversation {:?} after it was queued",
        started.elapsed()
    );
    assert_eq!(taken.lock().unwrap().len(), 1, "sent once");
    assert_eq!(run.state(sent).await.messages, 1);
    assert_eq!(
        fake.world().mailboxes[SENT].messages.len(),
        1,
        "one Sent copy"
    );
}

/// The first account pass goes INBOX first, then Sent, then the rest in LIST order (Gmail lists
/// Drafts before Sent), and the IDLE connection idles on INBOX before the other folders' first
/// syncs are done (it waited for the whole account pass before the Superhuman pass). Every
/// login sends its response with AUTHENTICATE (SASL-IR), a round trip fewer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_inbox_syncs_first_and_idles_before_the_rest_is_synced() {
    let fake = gmail().await;
    {
        let mut world = fake.world();
        world.append_many("[Gmail]/Drafts", 600, |_| 2);
        world.append_many(SENT, 600, |_| 2);
        world.append_many("[Gmail]/Trash", 600, |_| 2);
    }
    let mut run = Running::start(&fake, config());
    run.add(&fake).await;
    let commands = fake.world().commands.clone();
    let examined: Vec<&str> = commands
        .iter()
        .filter_map(|command| command.strip_prefix("EXAMINE "))
        .map(|rest| rest.trim_end_matches(" (CONDSTORE)"))
        .collect();
    assert_eq!(examined.first(), Some(&"\"INBOX\""), "{examined:?}");
    let rest: Vec<&str> = examined
        .iter()
        .copied()
        .filter(|name| *name != "\"INBOX\"")
        .collect();
    assert_eq!(
        rest,
        [
            "\"[Gmail]/Sent Mail\"",
            "\"[Gmail]/Drafts\"",
            "\"[Gmail]/Spam\"",
            "\"[Gmail]/Trash\""
        ],
        "Sent next, then LIST order; never All Mail"
    );
    let logins: Vec<&String> = commands
        .iter()
        .filter(|command| command.starts_with("AUTHENTICATE"))
        .collect();
    assert!(
        logins.len() >= 2 && logins.iter().all(|login| login.ends_with("SASL-IR")),
        "each login sent its response with the command: {logins:?}"
    );
    let at = |needle: &str| {
        commands
            .iter()
            .position(|command| command.starts_with(needle))
    };
    let idle = at("IDLE").expect("idled");
    let trash = at("EXAMINE \"[Gmail]/Trash\"").unwrap();
    assert!(
        idle < trash,
        "IDLE ({idle}) before the last folder's first sync ({trash})"
    );
}

fn store() -> (tempfile::TempDir, Store, AccountId) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("gmail-sync-")
        .tempdir_in(base)
        .unwrap();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Gmail".into(),
            address: "gmail@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    (dir, store, account)
}

/// Gmail's HIGHESTMODSEQ is the account's, so a change anywhere changed every folder's: every
/// account pass passed every synced folder. Outside the flag sweep the STATUS check now passes
/// only the folders whose messages, UIDNEXT, UIDVALIDITY or UNSEEN (against the last pass's)
/// changed; the sweep compares HIGHESTMODSEQ as before and finds any other flag change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gmails_account_wide_modseq_passes_only_the_folders_that_changed() {
    let fake = gmail().await;
    fake.world().append_many(SENT, 3, |_| 1);
    let (_dir, store, account) = store();
    let mut conn = fake.connect().await;
    let unseen = Arc::default();
    let pass = |sweep: bool| SyncOptions {
        now: Some(NOW),
        flag_sweep: sweep,
        unseen: Some(Arc::clone(&unseen)),
        ..SyncOptions::default()
    };
    let locks = FolderLocks::default();
    let mut account_pass = async |options: SyncOptions| -> SyncReport {
        imap::sync_account(&mut conn, &store, account, &options, &locks, |_, _| {})
            .await
            .unwrap()
    };
    let first = account_pass(pass(true)).await;
    assert_eq!(first.stats.passes, 5, "INBOX, Drafts, Sent, Spam and Trash");

    // A flag changed in INBOX: every folder's HIGHESTMODSEQ moved.
    fake.world().set_flags("INBOX", 1, &["\\Flagged"]);
    let quiet = account_pass(pass(false)).await;
    assert_eq!(
        (quiet.stats.passes, quiet.stats.status_skips),
        (0, 5),
        "nothing new anywhere, nothing read: a star is IDLE's or the sweep's"
    );
    // The message read elsewhere: INBOX's UNSEEN moved.
    fake.world().set_flags("INBOX", 1, &["\\Flagged", "\\Seen"]);
    let read = account_pass(pass(false)).await;
    assert_eq!(
        (
            read.stats.passes,
            read.stats.status_skips,
            read.stats.flag_changes
        ),
        (1, 4, 1)
    );
    // New mail in Sent: only Sent.
    fake.world().deliver(SENT, 0, &[]);
    let sent = account_pass(pass(false)).await;
    assert_eq!(
        (
            sent.stats.passes,
            sent.stats.status_skips,
            sent.stats.stored
        ),
        (1, 4, 1)
    );
    // Any other flag change (a star in Sent) waits for the sweep, which passes every folder.
    let uid = fake.world().uids(SENT)[0];
    fake.world().set_flags(SENT, uid, &["\\Flagged"]);
    let starred = account_pass(pass(false)).await;
    assert_eq!((starred.stats.passes, starred.stats.status_skips), (0, 5));
    let swept = account_pass(pass(true)).await;
    assert_eq!(swept.stats.passes, 5, "the sweep compares HIGHESTMODSEQ");
    assert_eq!(swept.stats.flag_changes, 1, "{:?}", swept.stats);
    let sent_folder = store
        .account_folders(account)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .find(|state| state.name_raw == SENT)
        .unwrap()
        .folder;
    let flagged = store
        .folder_messages(sent_folder)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .any(|message| message.uid == uid && message.flags.is_flagged());
    assert!(flagged, "the sweep stored the flag");
    assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
}
