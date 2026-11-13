//! A password account's keyring is read once a session (the fork's Superhuman pass), through
//! the engine service against the fake IMAP server: the command, IDLE and body connections and
//! every reconnect share the password read at the first login, and a refused login forgets it,
//! so the next login reads the keyring again. A test binary of its own: it counts the engine's
//! log lines. Synthetic data only.

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, LoginFailure};
use sealwax_domain::model::{AccountId, MessageId};
use sealwax_engine::api::{Command, Event, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::SyncOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing_subscriber::fmt::MakeWriter;

const WAIT: Duration = Duration::from_secs(20);
const READ: &str = "the account's password was read from the keyring";

/// Every log line, as the formatter wrote it.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn reads(&self) -> usize {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .matches(READ)
            .count()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

fn logs() -> &'static Captured {
    static LOGS: OnceLock<Captured> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Captured::default();
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .finish(),
        )
        .unwrap();
        logs
    })
}

async fn next<T>(
    events: &async_channel::Receiver<Event>,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> T {
    tokio::time::timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("the engine stopped");
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    })
    .await
    .expect("no such event in time")
}

async fn settled(events: &async_channel::Receiver<Event>, account: AccountId) -> SyncStatus {
    next(events, |event| match event {
        Event::AccountStatus {
            account: id,
            status,
        } if *id == account && *status != SyncStatus::Syncing => Some(*status),
        _ => None,
    })
    .await
}

async fn until(fake: &Fake, check: impl Fn(&fake::World) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check(&fake.world()) {
        assert!(Instant::now() < deadline, "not in time");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_keyring_is_read_once_a_session_and_again_after_a_refusal() {
    let logs = logs();
    let fake = Fake::start(Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.append_many("INBOX", 5, |_| 1);
    }
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("keyring-reads-")
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
        sync: SyncConfig {
            options: SyncOptions {
                now: Some(NOW),
                ..SyncOptions::default()
            },
            account_pass: Duration::from_secs(60),
            idle_restart: Duration::from_secs(60),
            backoff_min: Duration::from_millis(100),
            backoff_max: Duration::from_millis(800),
            logout_wait: Duration::from_millis(300),
            ..SyncConfig::default()
        },
    })
    .unwrap();
    let events = engine.events();
    let send = |command| engine.commands().try_send(command).unwrap();
    send(Command::AddAccount {
        request: 1,
        settings: AccountSettings {
            key: AccountKey(String::new()),
            name: "Keyring".into(),
            label: None,
            color: None,
            address: "keyring@example.test".into(),
            incoming: fake.endpoint(),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: fake.endpoint(),
            username: "keyring@example.test".into(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        },
        password: Password::new("fake-password".into()),
        sign_in: None,
    });
    let account = next(&events, |event| match event {
        Event::AccountAdded { request: 1, result } => Some(result.clone().unwrap()),
        _ => None,
    })
    .await;
    assert_eq!(settled(&events, account).await, SyncStatus::Synced);
    until(&fake, |world| world.count("IDLE") >= 1).await;

    // A body download logs in on a connection of its own.
    let inbox = store
        .account_folders(account)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .find(|folder| folder.name_raw == "INBOX")
        .unwrap()
        .folder;
    let message: MessageId = store.folder_messages(inbox).recv().await.unwrap()[0].id;
    send(Command::FetchBodies {
        messages: vec![message],
    });
    let fetched = next(&events, |event| match event {
        Event::BodyFetched {
            message: id,
            result,
        } if *id == message => Some(*result),
        _ => None,
    })
    .await;
    assert_eq!(fetched, Ok(()));
    // Every connection logs in again, twice.
    for _ in 0..2 {
        let idles = fake.world().count("IDLE");
        send(Command::Reconnect);
        assert_eq!(settled(&events, account).await, SyncStatus::Synced);
        until(&fake, |world| world.count("IDLE") > idles).await;
    }
    let logins = fake.world().count("AUTHENTICATE");
    assert!(logins >= 7, "{logins} logins");
    assert_eq!(logs.reads(), 1, "one keyring read for {logins} logins");

    // A refused login forgets the password; the next login reads the keyring again, once for
    // both connections that log in together.
    fake.world().refuse_login = true;
    send(Command::Reconnect);
    assert_eq!(
        settled(&events, account).await,
        SyncStatus::NeedsSignIn(LoginFailure::WrongPassword)
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let refused = logs.reads();
    fake.world().refuse_login = false;
    let idles = fake.world().count("IDLE");
    send(Command::Reconnect);
    assert_eq!(settled(&events, account).await, SyncStatus::Synced);
    until(&fake, |world| world.count("IDLE") > idles).await;
    assert_eq!(logs.reads(), refused + 1, "read again after the refusal");
    tokio::task::block_in_place(|| engine.stop());
}
