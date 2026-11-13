//! Timings for the fork's Superhuman pass, against the fake IMAP server's Gmail flavour with a
//! simulated round trip ([`RTT`]) and Gmail's slow header FETCH ([`HEADER`] per message):
//! printed, not asserted. The same file runs on the commit before the pass (with this fake) for
//! the numbers in docs/superhuman/engine.md. Synthetic data only.
//!
//! cargo test -p sealwax-engine --test timings -- --ignored --nocapture --test-threads=1

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, Security};
use sealwax_domain::compose::Draft;
use sealwax_domain::config::Storage;
use sealwax_domain::model::{AccountId, Address, FolderId, MessageId};
use sealwax_domain::triage::Triage;
use sealwax_engine::api::{Command, Event, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::SyncOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    DraftSaved, QueueTriage, SaveDraft, SendQueued, Store, StoreOptions,
};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A round trip to Gmail from Europe, about.
const RTT: Duration = Duration::from_millis(20);
/// Per header a FETCH returns (the owner's Gmail: about 15 a second, so 65 ms; less here to
/// keep the run short).
const HEADER: Duration = Duration::from_millis(5);
const ALL_MAIL: &str = "[Gmail]/All Mail";
const SENT: &str = "[Gmail]/Sent Mail";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// The defaults but for a clock fixed at the fake's, and a body connection that retires quickly
/// (so new mail's body would need a login on the commit before the pass).
fn config() -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            now: Some(NOW),
            ..SyncOptions::default()
        },
        body_linger: Duration::from_secs(1),
        logout_wait: Duration::from_millis(300),
        ..SyncConfig::default()
    }
}

struct Running {
    _dir: tempfile::TempDir,
    engine: Option<Engine>,
    store: Arc<Store>,
    events: async_channel::Receiver<Event>,
}

impl Running {
    fn start(fake: &Fake) -> Running {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("timings-")
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
            sync: config(),
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

    fn add(&self, fake: &Fake, smtp: Endpoint) {
        self.send(Command::AddAccount {
            request: 1,
            settings: AccountSettings {
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
            },
            password: Password::new("fake-password".into()),
            sign_in: None,
        });
    }

    /// Adds the account and waits until it is synced and idling.
    async fn added(&self, fake: &Fake, smtp: Endpoint) -> AccountId {
        self.add(fake, smtp);
        let mut account = None;
        let mut synced = false;
        let deadline = Instant::now() + Duration::from_secs(120);
        while !(synced && fake.world().count("IDLE") >= 1) {
            assert!(Instant::now() < deadline, "not synced in time");
            while let Ok(event) = self.events.try_recv() {
                match event {
                    Event::AccountAdded { result, .. } => account = Some(result.unwrap()),
                    Event::AccountStatus {
                        status: SyncStatus::Synced,
                        ..
                    } => synced = true,
                    _ => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        account.unwrap()
    }

    async fn folder(&self, account: AccountId, name: &str) -> Option<(FolderId, u32)> {
        self.store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|state| state.name_raw == name)
            .map(|state| (state.folder, state.messages))
    }

    async fn rows(&self, folder: FolderId) -> Vec<(MessageId, u32)> {
        self.store
            .folder_messages(folder)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .map(|message| (message.id, message.uid))
            .collect()
    }

    async fn queue(&self, action: Triage, messages: Vec<MessageId>) {
        let queued = self
            .store
            .queue_triage(QueueTriage {
                action,
                messages,
                not_before: now_ms(),
            })
            .recv()
            .await
            .unwrap();
        for account in &queued.accounts {
            self.send(Command::OpsQueued { account: *account });
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
        }
    }
}

/// Polls `check` every 5 ms until it holds; how long that took, or `None` past `cap`.
async fn until<F: std::future::Future<Output = bool>>(
    cap: Duration,
    mut check: impl FnMut() -> F,
) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < cap {
        if check().await {
            return Some(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    None
}

fn ms(duration: Option<Duration>, cap: Duration) -> String {
    match duration {
        Some(duration) => format!("{:.0} ms", duration.as_secs_f64() * 1000.0),
        None => format!("> {:.0} s", cap.as_secs_f64()),
    }
}

/// Gmail's folders, with `archived` messages in All Mail only and `inbox` in the Inbox (and
/// All Mail), a round trip of [`RTT`] and [`HEADER`] per header fetched.
async fn gmail(archived: usize, inbox: usize) -> Fake {
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
        for _ in 0..archived {
            world.deliver(ALL_MAIL, 30, &["\\Seen"]);
        }
        for _ in 0..inbox {
            world.deliver("INBOX", 1, &[]);
        }
        world.latency = RTT;
        world.header_cost = HEADER;
    }
    fake
}

/// SMTP that takes anything, on the fake's TLS identity.
async fn smtp(fake: &Fake) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tokio_rustls::TlsAcceptor::from(fake.tls.clone());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let Ok(stream) = acceptor.accept(tcp).await else {
                continue;
            };
            tokio::spawn(async move {
                let mut io = BufReader::new(stream);
                let mut data = false;
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
                    if data {
                        if line == ".\r\n" {
                            data = false;
                            reply.push_str("250 2.0.0 queued\r\n");
                        }
                        continue;
                    }
                    let upper = line.to_ascii_uppercase();
                    reply.push_str(if upper.starts_with("EHLO") {
                        "250-mock\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n"
                    } else if upper.starts_with("AUTH") {
                        "235 2.7.0 ok\r\n"
                    } else if upper.starts_with("DATA") {
                        data = true;
                        "354 go ahead\r\n"
                    } else if upper.starts_with("QUIT") {
                        "221 bye\r\n"
                    } else {
                        "250 ok\r\n"
                    });
                }
            });
        }
    });
    Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    }
}

async fn login() {
    let fake = gmail(0, 1).await;
    let mut times = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let conn = fake.connect().await;
        times.push(started.elapsed());
        conn.logout_within(Duration::from_millis(100)).await;
    }
    times.sort();
    let (logins, capabilities, ids) = {
        let world = fake.world();
        (
            world.count("AUTHENTICATE"),
            world.count("CAPABILITY"),
            world.count("ID ("),
        )
    };
    println!(
        "login, Gmail flavour: median {} ({logins} AUTHENTICATE, {capabilities} CAPABILITY, \
         {ids} ID)",
        ms(Some(times[2]), Duration::ZERO)
    );
}

async fn fresh_account() {
    let fake = gmail(0, 300).await;
    {
        let mut world = fake.world();
        world.append_many("[Gmail]/Drafts", 400, |_| 2);
        world.append_many(SENT, 400, |_| 2);
    }
    let run = Running::start(&fake);
    let started = Instant::now();
    run.add(&fake, fake.endpoint());
    let cap = Duration::from_secs(60);
    let (mut account, mut rows, mut idle, mut synced) = (None, None, None, None);
    while started.elapsed() < cap && (rows.is_none() || idle.is_none() || synced.is_none()) {
        while let Ok(event) = run.events.try_recv() {
            match event {
                Event::AccountAdded { result, .. } => account = result.ok(),
                Event::AccountStatus {
                    status: SyncStatus::Synced,
                    ..
                } if synced.is_none() => synced = Some(started.elapsed()),
                _ => {}
            }
        }
        if rows.is_none()
            && let Some(account) = account
            && run
                .folder(account, "INBOX")
                .await
                .is_some_and(|(_, messages)| messages > 0)
        {
            rows = Some(started.elapsed());
        }
        if idle.is_none() && fake.world().count("IDLE") >= 1 {
            idle = Some(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    println!(
        "new account (INBOX 300, Drafts 400, Sent 400): first INBOX rows {}, IDLE {}, synced {}",
        ms(rows, cap),
        ms(idle, cap),
        ms(synced, cap)
    );
}

async fn archive() {
    let fake = gmail(1_000, 5).await;
    let run = Running::start(&fake);
    let account = run.added(&fake, fake.endpoint()).await;
    let (inbox, _) = run.folder(account, "INBOX").await.unwrap();
    let rows = run.rows(inbox).await;
    let started = Instant::now();
    run.queue(Triage::Archive, vec![rows[0].0]).await;
    let cap = Duration::from_secs(60);
    let archived = until(cap, || async {
        run.store.pending_ops(account).recv().await.unwrap() == 0
    })
    .await;
    let queued = Instant::now();
    run.queue(Triage::Star(true), vec![rows[1].0]).await;
    let starred = until(cap, || async { fake.world().count("+FLAGS") >= 1 }).await;
    println!(
        "archive on Gmail (All Mail 1,005, never synced): done {}, the star queued after it \
         reached Gmail {} later; headers fetched from All Mail: {}",
        ms(archived, cap),
        ms(starred, cap),
        if run
            .folder(account, ALL_MAIL)
            .await
            .is_some_and(|(_, messages)| messages > 0)
        {
            "yes"
        } else {
            "none"
        }
    );
    let _ = (started, queued);
}

async fn account_pass_after_a_change() {
    let fake = gmail(0, 20).await;
    {
        let mut world = fake.world();
        world.append_many("[Gmail]/Drafts", 30, |_| 2);
        world.append_many(SENT, 30, |_| 2);
        world.append_many("[Gmail]/Trash", 30, |_| 2);
    }
    let run = Running::start(&fake);
    run.added(&fake, fake.endpoint()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    fake.world().set_flags("INBOX", 1, &["\\Seen"]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = fake.world().commands.len();
    let started = Instant::now();
    run.send(Command::CheckMail);
    // Until the account pass has been quiet for half a second.
    let mut last = (before, Instant::now());
    while last.1.elapsed() < Duration::from_millis(500) {
        let now = fake.world().commands.len();
        if now != last.0 {
            last = (now, Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let commands: Vec<String> = fake.world().commands[before..].to_vec();
    let passes = commands
        .iter()
        .filter(|command| command.starts_with("EXAMINE"))
        .count();
    println!(
        "account pass after a flag changed in INBOX (Gmail's one HIGHESTMODSEQ): {} commands, \
         {passes} folders passed, {:.0} ms",
        commands.len(),
        (last.1 - started).as_secs_f64() * 1000.0
    );
}

async fn new_mail_body() {
    let fake = gmail(0, 5).await;
    let run = Running::start(&fake);
    run.send(Command::Storage {
        storage: Storage {
            cache_size: None,
            prefetch_days: 14,
        },
    });
    let account = run.added(&fake, fake.endpoint()).await;
    let (inbox, _) = run.folder(account, "INBOX").await.unwrap();
    let cap = Duration::from_secs(30);
    until(cap, || async {
        run.store.body_usage().recv().await.unwrap().bodies >= 5
            && fake.world().count("LOGOUT") >= 1
    })
    .await;
    let connections = fake.world().connections.len();
    let uid = fake.world().deliver("INBOX", 0, &[]);
    let readable = until(cap, || async {
        let row = run.rows(inbox).await.into_iter().find(|(_, at)| *at == uid);
        match row {
            Some((id, _)) => run.store.raw_message(id).recv().await.unwrap().is_some(),
            None => false,
        }
    })
    .await;
    println!(
        "new mail through IDLE, its body stored after {} ({} new connections)",
        ms(readable, cap),
        fake.world().connections.len() - connections
    );
}

async fn reply_joins() {
    let fake = gmail(0, 3).await;
    let submission = smtp(&fake).await;
    let run = Running::start(&fake);
    let account = run.added(&fake, submission).await;
    let (inbox, _) = run.folder(account, "INBOX").await.unwrap();
    let original = run.rows(inbox).await[0].0;
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
        html: "<p>Thanks.</p>".into(),
        message_id: "reply.timings@example.test".into(),
        in_reply_to: Some(answered.clone()),
        references: vec![answered],
        ..Draft::default()
    };
    let DraftSaved::Saved { id, .. } = run
        .store
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: true,
            now: now_ms(),
        })
        .recv()
        .await
        .unwrap()
    else {
        panic!("not saved");
    };
    let started = Instant::now();
    if let SendQueued::Queued { account, .. } =
        run.store.queue_send(id, now_ms()).recv().await.unwrap()
    {
        run.send(Command::OutboxQueued { account });
    }
    let cap = Duration::from_secs(15);
    let joined = until(cap, || async {
        run.store
            .conversation(original)
            .recv()
            .await
            .unwrap()
            .value
            .is_some_and(|conversation| conversation.members().count() == 2)
    })
    .await;
    println!(
        "a reply sent, in its conversation after {} (from the send queued; the account pass \
         comes every 120 s)",
        ms(joined.map(|_| started.elapsed()), cap)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "timings, by hand: --ignored --nocapture"]
async fn timings() {
    println!("round trip {RTT:?}, {HEADER:?} per header fetched");
    login().await;
    fresh_account().await;
    archive().await;
    account_pass_after_a_change().await;
    new_mail_body().await;
    reply_joins().await;
}
