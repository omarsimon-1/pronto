//! Sign in with Google through the engine service, offline (ADR-023): a fake Google, a simulated
//! browser and scripted IMAP and SMTP servers, all on 127.0.0.1. Three Gmail accounts sign in
//! one after another, as their owner's do; each keeps its own refresh token, syncs as its own
//! address with its own access tokens, and refreshes without touching the others. One revoked
//! account asks for a new sign-in while the other two keep syncing.

mod mock;

use mock::{CLIENT, Microsoft, Server, Tls};
use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, LoginError, LoginFailure, OAuthProvider, Receive,
};
use sealwax_domain::model::AccountId;
use sealwax_engine::api::{AccountError, Command, Event, Password, RequestId, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::oauth::{Provider, Registration};
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::{self, AccountPaths};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);
const SECRET: &str = "GOCSPX-mock-secret";
const USERS: [&str; 3] = ["one@gmail.com", "two@gmail.com", "three@workspace.test"];

struct Setup {
    _root: tempfile::TempDir,
    dirs: SealwaxDirs,
    tls: Tls,
    google: Microsoft,
    imap: Server,
    smtp: Server,
}

async fn setup() -> Setup {
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let tls = mock::tls();
    let google = Microsoft::google(USERS[0]).await;
    google.state().secret = Some(SECRET.into());
    let imap = mock::imap(&tls, &google).await;
    let smtp = mock::smtp(&tls, &google).await;
    Setup {
        _root: root,
        dirs,
        tls,
        google,
        imap,
        smtp,
    }
}

impl Setup {
    fn start(&self, google_client: Option<Registration>) -> Engine {
        let blobs = BlobStore::open(&self.dirs.blobs_dir()).unwrap();
        let options = StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        };
        let store = Store::open(&self.dirs.store_file(), options).unwrap();
        Engine::start(EngineConfig {
            application: "io.github.omarsimon1.Pronto.Test".into(),
            store: Arc::new(store),
            blobs,
            paths: AccountPaths::new(&self.dirs),
            trust: self.tls.trust.clone(),
            secrets: SecretBackend::Memory,
            detect: DetectOptions::default(),
            microsoft_client_id: None,
            google_client,
            sign_in: SignInOptions {
                microsoft: Provider::microsoft(),
                google: self.google.provider(),
                servers: Some((self.imap.endpoint(), self.smtp.endpoint())),
                wait: Duration::from_secs(10),
            },
            sync: SyncConfig::default(),
        })
        .unwrap()
    }

    fn settings(&self, address: &str) -> AccountSettings {
        AccountSettings {
            key: AccountKey(String::new()),
            name: "Omar".into(),
            label: None,
            color: None,
            address: address.into(),
            incoming: self.imap.endpoint(),
            receive: Receive::Imap,
            smtp: self.smtp.endpoint(),
            username: address.into(),
            auth: AuthMethod::OAuth(OAuthProvider::Google),
            signature: None,
            identities: Vec::new(),
        }
    }

    fn listed(&self) -> Vec<AccountSettings> {
        accounts::read(&self.dirs.accounts_file()).unwrap()
    }
}

fn registration() -> Option<Registration> {
    Registration::new(Some(CLIENT), Some(SECRET))
}

async fn next<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let events = engine.events();
    tokio::time::timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("the engine stopped");
            if let Some(found) = pick(event) {
                return found;
            }
        }
    })
    .await
    .expect("no such event in time")
}

fn send(engine: &Engine, command: Command) {
    engine.commands().try_send(command).unwrap();
}

fn stop(engine: Engine) {
    tokio::task::block_in_place(|| engine.stop());
}

async fn open(
    engine: &Engine,
    request: RequestId,
    address: &str,
    account: Option<AccountKey>,
) -> String {
    send(
        engine,
        Command::SignIn {
            request,
            provider: OAuthProvider::Google,
            address: address.into(),
            account,
        },
    );
    next(engine, |event| match event {
        Event::SignInOpen { request: id, url } if id == request => Some(url),
        _ => None,
    })
    .await
}

async fn signed_in(engine: &Engine, request: RequestId) -> Result<String, LoginError> {
    next(engine, |event| match event {
        Event::SignedIn {
            request: id,
            result,
        } if id == request => Some(result),
        _ => None,
    })
    .await
}

async fn add(
    engine: &Engine,
    setup: &Setup,
    request: RequestId,
    address: &str,
    sign_in: RequestId,
) -> Result<AccountId, AccountError> {
    send(
        engine,
        Command::AddAccount {
            request,
            settings: setup.settings(address),
            password: Password::new(String::new()),
            sign_in: Some(sign_in),
        },
    );
    next(engine, |event| match event {
        Event::AccountAdded {
            request: id,
            result,
        } if id == request => Some(result),
        _ => None,
    })
    .await
}

/// Waits until every account in `accounts` has synced or failed, and returns how each ended.
async fn settled(
    engine: &Engine,
    accounts: &[AccountId],
) -> HashMap<AccountId, Result<(), LoginFailure>> {
    let mut ended = HashMap::new();
    let events = engine.events();
    tokio::time::timeout(WAIT, async {
        while ended.len() < accounts.len() {
            if let Event::AccountStatus { account, status } =
                events.recv().await.expect("the engine stopped")
                && accounts.contains(&account)
            {
                match status {
                    SyncStatus::Synced => {
                        ended.insert(account, Ok(()));
                    }
                    status => {
                        if let Some(failure) = status.failure() {
                            ended.insert(account, Err(failure));
                        }
                    }
                }
            }
        }
    })
    .await
    .expect("the accounts didn't settle in time");
    ended
}

async fn sync_again(
    engine: &Engine,
    setup: &Setup,
    accounts: &[AccountId],
) -> HashMap<AccountId, Result<(), LoginFailure>> {
    send(
        engine,
        Command::Accounts {
            accounts: setup.listed(),
        },
    );
    send(engine, Command::Reconnect);
    settled(engine, accounts).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sign_in_page_is_googles_with_a_127_0_0_1_redirect_and_offline_access() {
    let setup = setup().await;
    let engine = setup.start(registration());
    let url = open(&engine, 1, USERS[0], None).await;
    let query: HashMap<String, String> = url::Url::parse(&url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert!(query["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert_eq!(query["scope"], "https://mail.google.com/ openid email");
    assert_eq!(query["access_type"], "offline");
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!url.contains(SECRET) && !url.contains("gmail.com"));
    let page = tokio::spawn(mock::browse(url));
    assert_eq!(signed_in(&engine, 1).await, Ok(USERS[0].to_owned()));
    assert!(page.await.unwrap().contains("Pronto has the sign-in."));
    stop(engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_google_client_sign_in_is_unavailable() {
    let setup = setup().await;
    let engine = setup.start(None);
    send(
        &engine,
        Command::SignIn {
            request: 1,
            provider: OAuthProvider::Google,
            address: USERS[0].into(),
            account: None,
        },
    );
    let result = signed_in(&engine, 1).await;
    assert_eq!(
        result.map_err(|error| error.failure),
        Err(LoginFailure::OAuthRequired)
    );

    // `config.toml` gains a client: the next sign-in works.
    send(
        &engine,
        Command::GoogleClient {
            client: registration(),
        },
    );
    let url = open(&engine, 2, USERS[0], None).await;
    tokio::spawn(mock::browse(url));
    assert_eq!(signed_in(&engine, 2).await, Ok(USERS[0].to_owned()));
    stop(engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_gmail_accounts_sign_in_and_sync_without_clobbering_each_other() {
    let setup = setup().await;
    // Every access token runs low at once, so every sync refreshes.
    setup.google.state().lifetime = 60;
    let engine = setup.start(registration());

    let mut accounts = Vec::new();
    for (n, user) in USERS.iter().enumerate() {
        let request = 10 * (n as u64 + 1);
        setup.google.state().username = (*user).to_owned();
        let url = open(&engine, request, user, None).await;
        tokio::spawn(mock::browse(url));
        assert_eq!(signed_in(&engine, request).await, Ok((*user).to_owned()));
        let account = add(&engine, &setup, request + 1, user, request)
            .await
            .unwrap();
        assert_eq!(settled(&engine, &[account]).await[&account], Ok(()));
        accounts.push(account);
    }
    let listed = setup.listed();
    assert_eq!(listed.len(), 3);
    let keys: Vec<AccountKey> = listed.iter().map(|account| account.key.clone()).collect();
    assert!(keys[0] != keys[1] && keys[1] != keys[2] && keys[0] != keys[2]);
    for (account, user) in listed.iter().zip(USERS) {
        assert_eq!(account.username, user);
        assert_eq!(account.auth, AuthMethod::OAuth(OAuthProvider::Google));
    }

    // Each account refreshes with its own refresh token and logs in as itself.
    let ended = sync_again(&engine, &setup, &accounts).await;
    assert!(ended.values().all(Result::is_ok), "{ended:?}");
    let refreshed: Vec<String> = {
        let state = setup.google.state();
        state
            .grants
            .iter()
            .filter_map(|grant| grant.strip_prefix("refresh "))
            .map(|token| state.owners[token].clone())
            .collect()
    };
    for user in USERS {
        assert!(refreshed.contains(&user.to_owned()), "{user} refreshed");
    }
    for (user, token) in setup.imap.logins() {
        assert_eq!(
            setup.google.state().owners.get(&token),
            Some(&user),
            "every IMAP login used the account's own token"
        );
    }

    // The second account's grant is revoked: only it must sign in again.
    {
        let mut state = setup.google.state();
        let owners = state.owners.clone();
        state
            .refresh
            .retain(|token| owners.get(token).map(String::as_str) != Some(USERS[1]));
    }
    let ended = sync_again(&engine, &setup, &accounts).await;
    assert_eq!(ended[&accounts[0]], Ok(()));
    assert_eq!(ended[&accounts[1]], Err(LoginFailure::OAuthRequired));
    assert_eq!(ended[&accounts[2]], Ok(()));

    // Signing the second account in again as the first account's user is refused, so one
    // account's sign-in never lands in another's keyring entry.
    setup.google.state().username = USERS[0].to_owned();
    let url = open(&engine, 100, USERS[1], Some(keys[1].clone())).await;
    tokio::spawn(mock::browse(url));
    assert_eq!(
        signed_in(&engine, 100).await.map_err(|error| error.failure),
        Err(LoginFailure::WrongPassword)
    );

    // Signed in again as itself, it syncs once more; the others never noticed.
    setup.google.state().username = USERS[1].to_owned();
    let url = open(&engine, 101, USERS[1], Some(keys[1].clone())).await;
    tokio::spawn(mock::browse(url));
    assert_eq!(signed_in(&engine, 101).await, Ok(USERS[1].to_owned()));
    let ended = sync_again(&engine, &setup, &accounts).await;
    assert!(ended.values().all(Result::is_ok), "{ended:?}");
    for (user, token) in setup.imap.logins() {
        assert_eq!(setup.google.state().owners.get(&token), Some(&user));
    }
    stop(engine);
}
