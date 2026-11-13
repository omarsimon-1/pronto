//! Remind Me on real servers (the fork's Superhuman pass, docs/superhuman/remind.md), end to end
//! through the engine service and the store: on Dovecot, Dovecot IMAP4rev1 (no MOVE, no
//! UIDPLUS: COPY, then `\Deleted` left on the copy behind) and Stalwart, a conversation of two
//! messages is sent to Reminders (the folder made on the server first: CREATE, SUBSCRIBE), and
//! at its time the engine's timer brings it back to the INBOX, where it tops the store's
//! conversation view, dated when it came back. Phase C: someone else's answer to a waiting
//! conversation, appended to the INBOX by another client, ends its reminder long before its
//! time, and what waited comes back beside it ("if no reply"). It uses only the user `ops`,
//! whose folders it empties and fills with synthetic messages, and deletes the Reminders folder
//! at its end:
//!
//! PRONTO_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test remind_testbed -- --ignored --test-threads=1 --nocapture
//!
//! Gmail's labels can't be met here; the fake server's Gmail flavour covers them (`remind.rs`).

mod common;
mod mutator;
mod snapshot;
mod testbed_client;
mod testbed_engine;

use async_imap::imap_proto::{MailboxDatum, Response};
use common::{DOVECOT, DOVECOT_REV1, STALWART, Server};
use mutator::{client, reset_account};
use sealwax_domain::model::{Flags, FolderRole, ThreadId};
use sealwax_domain::remind::ReminderState;
use sealwax_engine::api::Command;
use sealwax_engine::imap::{ConnectOptions, SyncOptions};
use sealwax_engine::service::SyncConfig;
use sealwax_infrastructure::store::{Remind, ViewScope, ViewSpec};
use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use testbed_client::Client;
use testbed_engine::{TestEngine, until};

const WAIT: Duration = Duration::from_secs(40);
const FOLDERS: [&str; 3] = ["INBOX", "Arkiv", "Papirkurv"];

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn config() -> SyncConfig {
    SyncConfig {
        options: SyncOptions::default(),
        account_pass: Duration::from_secs(3),
        inbox_poll: Duration::from_secs(1),
        opened_poll: Duration::from_secs(1),
        idle_restart: Duration::from_secs(20),
        reuse_check: Duration::from_secs(5),
        backoff_min: Duration::from_millis(200),
        backoff_max: Duration::from_secs(2),
        connect: ConnectOptions::default(),
        ..SyncConfig::default()
    }
}

/// A synthetic message `n` of the suite, answering `parent` when given.
fn message(n: u32, parent: Option<u32>) -> Vec<u8> {
    let reply = parent.map_or(String::new(), |parent| {
        format!(
            "In-Reply-To: <remind-{parent}@example.test>\r\n\
             References: <remind-{parent}@example.test>\r\n"
        )
    });
    format!(
        "From: \"Kari Nordmann\" <kari@example.test>\r\nTo: <ops@example.test>\r\n\
         Subject: Remind {n}\r\nDate: Thu, 29 Oct 2026 08:0{n}:00 +0000\r\n\
         Message-ID: <remind-{n}@example.test>\r\n{reply}MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nSynthetic message {n} of the Remind Me suite.\r\n"
    )
    .into_bytes()
}

/// The live (not `\Deleted`) copies of each Message-ID on the server, by folder.
async fn copies(client: &mut Client) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, folder) in client.snapshot().await.unwrap() {
        for (flags, _, id) in folder.messages.into_values() {
            if let Some(id) = id
                && !flags.contains(Flags::DELETED)
            {
                out.entry(id).or_default().push(name.clone());
            }
        }
    }
    out
}

/// Waits until `wanted` holds of the server's live copies.
async fn on_server(
    client: &mut Client,
    wanted: impl Fn(&BTreeMap<String, Vec<String>>) -> bool,
) -> Option<Duration> {
    let started = Instant::now();
    loop {
        if wanted(&copies(client).await) {
            return Some(started.elapsed());
        }
        if started.elapsed() > WAIT {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn delete_reminders(client: &mut Client) {
    for (name, _) in client.list().await.unwrap() {
        if name.eq_ignore_ascii_case("Reminders") {
            let _ = client.run("UNSUBSCRIBE \"Reminders\"", |_| {}).await;
            let _ = client.run(&format!("DELETE \"{name}\""), |_| {}).await;
        }
    }
}

fn round_trip(server: Server) {
    runtime().block_on(async {
        let mut test = client(server.tls, "ops").await;
        delete_reminders(&mut test).await;
        reset_account(&mut test, &FOLDERS).await.unwrap();
        for (n, parent) in [(1, None), (2, None), (3, Some(1))] {
            test.append("INBOX", "", &message(n, parent)).await.unwrap();
        }
        let mut engine = TestEngine::start(config());
        let account = engine.add(1, "ops", server.tls).await;
        let status = engine.settled(account, Duration::from_secs(60)).await;
        assert!(
            matches!(status, Some(sealwax_engine::api::SyncStatus::Synced)),
            "{}: {status:?}",
            server.name
        );
        let folders = engine.store.account_folders(account).recv().await.unwrap();
        let inbox = folders
            .iter()
            .find(|folder| folder.role == FolderRole::Inbox)
            .unwrap()
            .folder;
        let stored = engine.store.folder_messages(inbox).recv().await.unwrap();
        assert_eq!(stored.len(), 3, "{}", server.name);
        // By Message-ID: the threads of the suite's messages.
        let mut by_id: BTreeMap<String, ThreadId> = BTreeMap::new();
        for message in &stored {
            let detail = engine.store.message(message.id).recv().await.unwrap().value;
            let detail = detail.unwrap();
            let id = detail.headers.message_id.unwrap_or_default();
            by_id.insert(
                id.trim_matches(['<', '>']).to_owned(),
                detail.summary.thread,
            );
        }
        let thread = |n: u32| by_id[&format!("remind-{n}@example.test")];
        assert_eq!(thread(1), thread(3), "{}: one conversation", server.name);
        assert_ne!(thread(1), thread(2), "{}", server.name);
        let first = thread(1);

        let started = Instant::now();
        let wake_at = now_ms() / 1000 + 8;
        let reminded = engine
            .store
            .remind(Remind {
                threads: vec![first],
                scope: ViewScope::Folder(inbox),
                wake_at,
                not_before: now_ms(),
            })
            .recv()
            .await
            .unwrap();
        assert_eq!(reminded.moved, 2, "{}", server.name);
        engine.send(Command::OpsQueued { account });
        engine.send(Command::RemindersChanged);
        let ids = ["remind-1@example.test", "remind-3@example.test"];
        let only = |folder: &str| {
            let folder = folder.to_owned();
            move |copies: &BTreeMap<String, Vec<String>>| {
                ids.iter()
                    .all(|id| copies.get(*id).is_some_and(|at| *at == [folder.clone()]))
            }
        };
        let away = on_server(&mut test, only("Reminders")).await;
        assert!(away.is_some(), "{}: sent to Reminders", server.name);
        println!(
            "{}: in Reminders after {:.0} ms",
            server.name,
            started.elapsed().as_secs_f64() * 1000.0
        );
        let subscribed = {
            let mut found = false;
            test.run("LSUB \"\" \"*\"", |response| {
                if let Response::MailboxData(MailboxDatum::List(list)) = response {
                    found |= list.name.eq_ignore_ascii_case("Reminders");
                }
            })
            .await
            .unwrap();
            found
        };
        assert!(subscribed, "{}: subscribed", server.name);
        let waiting = engine.store.reminders().recv().await.unwrap();
        assert_eq!(waiting[0].state, ReminderState::Waiting, "{}", server.name);

        let back = on_server(&mut test, only("INBOX")).await;
        assert!(back.is_some(), "{}: back in the INBOX", server.name);
        assert!(
            now_ms() / 1000 >= wake_at,
            "{}: not before its time",
            server.name
        );
        println!(
            "{}: back after {:.0} ms",
            server.name,
            started.elapsed().as_secs_f64() * 1000.0
        );
        let settled = until(WAIT, || async {
            let rows = engine
                .store
                .page(ViewSpec::folder(inbox).conversations(), 0, 10)
                .recv()
                .await
                .unwrap()
                .conversations;
            rows.len() == 2 && rows[0].members == 2 && rows[0].date >= wake_at
        })
        .await;
        assert!(
            settled.is_some(),
            "{}: on top of the Inbox, dated when it came back",
            server.name
        );
        let reminders = engine.store.reminders().recv().await.unwrap();
        assert_eq!(
            reminders[0].state,
            ReminderState::Reminded,
            "{}",
            server.name
        );
        assert_eq!(
            engine.store.check().recv().await.unwrap(),
            Vec::<String>::new(),
            "{}",
            server.name
        );
        drop(engine);
        delete_reminders(&mut test).await;
        test.logout().await;
    });
}

/// Superhuman's "if no reply" (phase C): another client files an answer to a conversation that
/// waits in Reminders for an hour; the engine's new mail ends the reminder and moves what waited
/// back to the INBOX at once.
fn answered(server: Server) {
    runtime().block_on(async {
        let mut test = client(server.tls, "ops").await;
        delete_reminders(&mut test).await;
        reset_account(&mut test, &FOLDERS).await.unwrap();
        for (n, parent) in [(1, None), (2, None), (3, Some(1))] {
            test.append("INBOX", "", &message(n, parent)).await.unwrap();
        }
        let mut engine = TestEngine::start(config());
        let account = engine.add(1, "ops", server.tls).await;
        let status = engine.settled(account, Duration::from_secs(60)).await;
        assert!(
            matches!(status, Some(sealwax_engine::api::SyncStatus::Synced)),
            "{}: {status:?}",
            server.name
        );
        let folders = engine.store.account_folders(account).recv().await.unwrap();
        let inbox = folders
            .iter()
            .find(|folder| folder.role == FolderRole::Inbox)
            .unwrap()
            .folder;
        let stored = engine.store.folder_messages(inbox).recv().await.unwrap();
        let mut first = None;
        for message in &stored {
            let detail = engine.store.message(message.id).recv().await.unwrap().value;
            let detail = detail.unwrap();
            if detail.headers.message_id.as_deref() == Some("remind-1@example.test") {
                first = Some(detail.summary.thread);
            }
        }
        let first = first.expect("message 1 is stored");
        let reminded = engine
            .store
            .remind(Remind {
                threads: vec![first],
                scope: ViewScope::Folder(inbox),
                wake_at: now_ms() / 1000 + 3_600,
                not_before: now_ms(),
            })
            .recv()
            .await
            .unwrap();
        assert_eq!(reminded.moved, 2, "{}", server.name);
        engine.send(Command::OpsQueued { account });
        engine.send(Command::RemindersChanged);
        let ids = ["remind-1@example.test", "remind-3@example.test"];
        let only = |folder: &str| {
            let folder = folder.to_owned();
            move |copies: &BTreeMap<String, Vec<String>>| {
                ids.iter()
                    .all(|id| copies.get(*id).is_some_and(|at| *at == [folder.clone()]))
            }
        };
        assert!(
            on_server(&mut test, only("Reminders")).await.is_some(),
            "{}: sent to Reminders",
            server.name
        );
        // Kari answers the newest message, from another client.
        let started = Instant::now();
        test.append("INBOX", "", &message(4, Some(3)))
            .await
            .unwrap();
        let back = on_server(&mut test, only("INBOX")).await;
        assert!(back.is_some(), "{}: back in the INBOX", server.name);
        println!(
            "{}: answered, back in the INBOX after {:.0} ms",
            server.name,
            started.elapsed().as_secs_f64() * 1000.0
        );
        let ended = until(WAIT, || async {
            engine.store.reminders().recv().await.unwrap().is_empty()
        })
        .await;
        assert!(ended.is_some(), "{}: the reminder ended", server.name);
        let settled = until(WAIT, || async {
            let rows = engine
                .store
                .page(ViewSpec::folder(inbox).conversations(), 0, 10)
                .recv()
                .await
                .unwrap()
                .conversations;
            rows.len() == 2 && rows[0].thread == first && rows[0].members == 3
        })
        .await;
        assert!(
            settled.is_some(),
            "{}: on top of the Inbox with the answer",
            server.name
        );
        assert_eq!(
            engine.store.check().recv().await.unwrap(),
            Vec::<String>::new(),
            "{}",
            server.name
        );
        drop(engine);
        delete_reminders(&mut test).await;
        test.logout().await;
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn an_answer_brings_it_back_on_dovecot() {
    answered(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn an_answer_brings_it_back_on_dovecot_imap4rev1() {
    answered(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn an_answer_brings_it_back_on_stalwart() {
    answered(STALWART);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn a_reminder_goes_and_comes_back_on_dovecot() {
    round_trip(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn a_reminder_goes_and_comes_back_on_dovecot_imap4rev1() {
    round_trip(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn a_reminder_goes_and_comes_back_on_stalwart() {
    round_trip(STALWART);
}
