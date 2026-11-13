//! The Split Inbox's header facts on real servers (the fork's Superhuman pass,
//! docs/superhuman/split.md): Dovecot (full and IMAP4rev1-only) and Stalwart answer the header
//! FETCH's LIST-ID, LIST-UNSUBSCRIBE, PRECEDENCE and AUTO-SUBMITTED fields, and a folder of
//! newsletters, notifications and a person's mail syncs into the right side of the Split Inbox.
//! Each run appends to a folder of its own under the user `spike` and deletes it at its end (and
//! those an earlier run left). It needs the Docker testbed running and seeded:
//!
//! PRONTO_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test split_testbed -- --ignored --test-threads=1 --nocapture
//!
//! Gmail's own categories can't be met here; the fake server's Gmail flavour covers them
//! (`split.rs`). Synthetic data only.

mod common;
mod snapshot;
mod testbed_client;

use async_imap::imap_proto::{MailboxDatum, Response};
use common::{SERVERS, Server};
use sealwax_domain::account::Security;
use sealwax_domain::model::{FolderId, FolderRole};
use sealwax_engine::imap::{FolderLocks, SyncOptions, sync_folder};
use sealwax_infrastructure::store::{FolderSpec, OpenView, Store, ViewFilter, ViewSpec};
use std::time::{SystemTime, UNIX_EPOCH};
use testbed_client::Client;

const USER: &str = "spike";
/// Every folder this suite makes starts so.
const PREFIX: &str = "Pronto split ";

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

async fn client(server: Server) -> Client {
    Client::connect(server.tls, &common::ca(), USER)
        .await
        .unwrap_or_else(|error| panic!("test client on {}: {error:?}", server.name))
}

/// The suite's folders the server holds, by wire name, with the hierarchy delimiter.
async fn ours(client: &mut Client) -> Vec<(String, Option<char>)> {
    let mut names = Vec::new();
    client
        .run("LIST \"\" \"*\"", |response| {
            if let Response::MailboxData(MailboxDatum::List(list)) = response
                && list.name.contains(PREFIX)
            {
                let delimiter = list
                    .delimiter
                    .as_deref()
                    .and_then(|text| text.chars().next());
                names.push((list.name.to_string(), delimiter));
            }
        })
        .await
        .unwrap();
    names
}

async fn clean(client: &mut Client) {
    for (name, _) in ours(client).await {
        let quoted = format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""));
        let _ = client.run(&format!("DELETE {quoted}"), |_| {}).await;
    }
}

/// A message `n` from `from` with `extra` header lines (each ending in CRLF).
fn message(n: u32, from: &str, extra: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: Spike <spike@example.test>\r\nSubject: Split {n}\r\n\
         Date: Thu, 29 Oct 2026 08:00:00 +0000\r\nMessage-ID: <split-{n}@example.test>\r\n\
         {extra}MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Synthetic message {n} of the Split Inbox suite.\r\n"
    )
    .into_bytes()
}

/// What the suite appends: whether each should come out Important, and the message.
fn messages() -> Vec<(bool, Vec<u8>)> {
    vec![
        (true, message(1, "Kari Nordmann <kari@example.test>", "")),
        (
            false,
            message(
                2,
                "Turlaget <post@turlaget.example.test>",
                "List-Id: Turlaget <turlaget.example.test>\r\n\
                 List-Post: <mailto:post@turlaget.example.test>\r\n",
            ),
        ),
        (
            false,
            message(
                3,
                "Fjellposten <post@fjellposten.example.test>",
                "List-Unsubscribe: <https://fjellposten.example.test/u?id=1>,\r\n \
                 <mailto:leave@fjellposten.example.test>\r\n\
                 List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
            ),
        ),
        (
            false,
            message(
                4,
                "Kampanje <tilbud@shop.example.test>",
                "Precedence: bulk\r\n",
            ),
        ),
        (
            false,
            message(
                5,
                "Tomas Lie (Jira) <jira@acme.example.test>",
                "Auto-Submitted: auto-generated\r\n",
            ),
        ),
        (false, message(6, "Banken <noreply@bank.example.test>", "")),
        (
            true,
            message(
                7,
                "Ingrid Berg <ingrid@acme.example.test>",
                "Auto-Submitted: no\r\n",
            ),
        ),
    ]
}

/// The subjects each side of `folder`'s message views holds, in order.
async fn sides(store: &Store, folder: FolderId) -> (Vec<String>, Vec<String>) {
    let mut out = [Vec::new(), Vec::new()];
    for (side, filter) in out
        .iter_mut()
        .zip([ViewFilter::Important, ViewFilter::Other])
    {
        let opened = store
            .open_view(OpenView {
                spec: ViewSpec::folder(folder).with_filter(filter),
                first_page: u32::MAX,
                section_starts: Vec::new(),
            })
            .recv()
            .await
            .unwrap();
        side.extend(opened.rows.iter().map(|row| row.subject.clone()));
        side.sort();
    }
    let [important, other] = out;
    (important, other)
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn header_facts_sort_a_folder_on_every_server() {
    runtime().block_on(async {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        for server in SERVERS {
            let mut client = client(server).await;
            clean(&mut client).await;
            client.create(&format!("{PREFIX}{stamp}")).await.unwrap();
            let listed = ours(&mut client).await;
            let [(name_raw, delimiter)] = listed.as_slice() else {
                panic!("{}: the suite's folder isn't listed once", server.name);
            };
            let (name_raw, delimiter) = (name_raw.clone(), *delimiter);
            let expected = messages();
            for (_, raw) in &expected {
                client.append(&name_raw, "", raw).await.unwrap();
            }

            let (_dir, store, account) = common::store();
            let folder = store
                .upsert_folder(FolderSpec {
                    delimiter,
                    name_raw: name_raw.clone(),
                    ..FolderSpec::local(account, &name_raw, FolderRole::Other)
                })
                .recv()
                .await
                .unwrap()
                .value;
            let mut conn = common::login(server, Security::Tls, USER).await;
            sync_folder(
                &mut conn,
                &store,
                folder,
                &SyncOptions::default(),
                &FolderLocks::default(),
                |_| {},
            )
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", server.name));
            let subject = |n: usize| format!("Split {n}");
            let wanted = |important: bool| {
                let mut subjects: Vec<String> = expected
                    .iter()
                    .enumerate()
                    .filter(|(_, (side, _))| *side == important)
                    .map(|(n, _)| subject(n + 1))
                    .collect();
                subjects.sort();
                subjects
            };
            assert_eq!(
                sides(&store, folder).await,
                (wanted(true), wanted(false)),
                "{}",
                server.name
            );
            assert_eq!(
                store.check().recv().await.unwrap(),
                Vec::<String>::new(),
                "{}",
                server.name
            );
            println!(
                "{}: {} Important, {} Other",
                server.name,
                wanted(true).len(),
                wanted(false).len()
            );
            conn.logout().await;
            clean(&mut client).await;
        }
    });
}
