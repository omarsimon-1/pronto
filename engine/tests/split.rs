//! The Split Inbox's backend in the sync engine (the fork's Superhuman pass,
//! docs/superhuman/split.md), against the fake IMAP server (no network beyond 127.0.0.1, in CI):
//! the header facts come with the header FETCH, Gmail's own categories (its Primary tab) win
//! over them for INBOX mail, asked for with the rows that bring them, and stored mail without
//! one catches up once per connection; elsewhere, and where Gmail refuses the search, the header
//! rules sort the mail. Synthetic data only.

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_engine::imap::{Connection, FolderLocks, SyncOptions, sync_account, sync_folder};
use sealwax_infrastructure::store::{
    NewAccount, OpenView, Store, StoreOptions, ViewFilter, ViewSpec,
};
use std::collections::BTreeMap;
use std::path::Path;

/// A newsletter's header lines: Other by the header rules.
const NEWSLETTER: &str = "List-Unsubscribe: <mailto:leave@news.example.test>\r\n\
    Precedence: bulk\r\n";

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

fn store() -> (tempfile::TempDir, Store, AccountId) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("split-")
        .tempdir_in(base)
        .unwrap();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Fake".into(),
            address: "test@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    (dir, store, account)
}

fn options() -> SyncOptions {
    SyncOptions {
        now: Some(NOW),
        ..SyncOptions::default()
    }
}

async fn account_pass(conn: &mut Connection, store: &Store, account: AccountId) {
    sync_account(
        conn,
        store,
        account,
        &options(),
        &FolderLocks::default(),
        |_, _| {},
    )
    .await
    .unwrap();
}

async fn folder_pass(conn: &mut Connection, store: &Store, folder: FolderId) {
    sync_folder(
        conn,
        store,
        folder,
        &options(),
        &FolderLocks::default(),
        |_| {},
    )
    .await
    .unwrap();
}

async fn inbox(store: &Store, account: AccountId) -> FolderId {
    let folders = store.folders().recv().await.unwrap().value;
    folders
        .iter()
        .find(|f| f.account == account && f.name == "INBOX")
        .unwrap()
        .id
}

/// The UIDs of `folder`'s messages each side of the Split Inbox holds, ascending.
async fn sides(store: &Store, folder: FolderId) -> (Vec<u32>, Vec<u32>) {
    let uids: BTreeMap<MessageId, u32> = store
        .folder_messages(folder)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .map(|message| (message.id, message.uid))
        .collect();
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
        side.extend(opened.rows.iter().map(|row| uids[&row.id]));
        side.sort_unstable();
    }
    let [important, other] = out;
    (important, other)
}

/// The commands the fake received that contain `needle`.
fn sent(fake: &Fake, needle: &str) -> Vec<String> {
    fake.world()
        .commands
        .iter()
        .filter(|command| command.contains(needle))
        .cloned()
        .collect()
}

#[test]
fn gmails_categories_sort_inbox_mail_and_win_over_the_header_rules() {
    runtime().block_on(async {
        let fake = Fake::start(Flavor::Gmail).await;
        {
            let mut world = fake.world();
            world.create("INBOX", &[]);
            // A person in Primary; a newsletter in Promotions; a plain message Gmail put in
            // Updates; a newsletter Gmail put in Primary.
            world.append_tab("INBOX", "", true);
            world.append_tab("INBOX", NEWSLETTER, false);
            world.append_tab("INBOX", "", false);
            world.append_tab("INBOX", NEWSLETTER, true);
        }
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &store, account).await;
        let inbox = inbox(&store, account).await;
        assert_eq!(sides(&store, inbox).await, (vec![1, 4], vec![2, 3]));
        // The header FETCH brings the facts; the categories come with the batch, asked once.
        let fetches = sent(&fake, "HEADER.FIELDS (DATE");
        assert!(
            fetches
                .iter()
                .all(|command| command
                    .contains("LIST-ID LIST-UNSUBSCRIBE PRECEDENCE AUTO-SUBMITTED")),
            "{fetches:?}"
        );
        assert_eq!(
            sent(&fake, "X-GM-RAW"),
            ["UID SEARCH UID 1:4 X-GM-RAW \"category:primary\""]
        );

        // New mail is stored on its side at once: its category comes with its rows.
        {
            let mut world = fake.world();
            world.append_tab("INBOX", NEWSLETTER, false);
            world.append_tab("INBOX", "", true);
        }
        folder_pass(&mut conn, &store, inbox).await;
        assert_eq!(sides(&store, inbox).await, (vec![1, 4, 6], vec![2, 3, 5]));
        assert_eq!(
            sent(&fake, "X-GM-RAW").last().map(String::as_str),
            Some("UID SEARCH UID 5:6 X-GM-RAW \"category:primary\"")
        );
        // Nothing new: nothing asked.
        folder_pass(&mut conn, &store, inbox).await;
        assert_eq!(sent(&fake, "X-GM-RAW").len(), 2);
        assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
    });
}

#[test]
fn the_header_rules_sort_mail_where_gmails_categories_are_not_there() {
    runtime().block_on(async {
        // Not Gmail: the facts alone, and Gmail's search never asked.
        let fake = Fake::start(Flavor::Qresync).await;
        {
            let mut world = fake.world();
            world.create("INBOX", &[]);
            world.append_extra("INBOX", "");
            world.append_extra("INBOX", "Precedence: bulk\r\n");
            world.append_extra("INBOX", "Auto-Submitted: auto-generated\r\n");
            world.append_extra("INBOX", "List-Id: Turlaget <turlaget.example.test>\r\n");
            world.append_extra("INBOX", "List-Unsubscribe: <mailto:u@example.test>\r\n");
            world.append_extra("INBOX", "Auto-Submitted: no\r\nPrecedence: first-class\r\n");
        }
        let (_dir, other, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &other, account).await;
        let inbox = inbox(&other, account).await;
        assert_eq!(sides(&other, inbox).await, (vec![1, 6], vec![2, 3, 4, 5]));
        assert!(sent(&fake, "X-GM-RAW").is_empty());

        // Gmail refusing the search: the header rules, and no more asking on that connection.
        let fake = Fake::start(Flavor::Gmail).await;
        {
            let mut world = fake.world();
            world.create("INBOX", &[]);
            world.append_tab("INBOX", "", false);
            world.append_tab("INBOX", NEWSLETTER, true);
            world.refuse("X-GM-RAW", "NO [UNAVAILABLE] Try again later");
        }
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &store, account).await;
        let inbox = self::inbox(&store, account).await;
        assert_eq!(sides(&store, inbox).await, (vec![1], vec![2]));
        fake.world().append_tab("INBOX", "", false);
        folder_pass(&mut conn, &store, inbox).await;
        assert_eq!(sides(&store, inbox).await, (vec![1, 3], vec![2]));
        assert_eq!(sent(&fake, "X-GM-RAW").len(), 1, "asked once, refused");
        assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
    });
}

#[test]
fn stored_inbox_mail_catches_up_with_gmail_once_per_connection() {
    runtime().block_on(async {
        let fake = Fake::start(Flavor::Gmail).await;
        {
            let mut world = fake.world();
            world.create("INBOX", &[]);
            // Every third message is in another tab; every fifth is a newsletter.
            for n in 1..=1_200u32 {
                let extra = if n % 5 == 0 { NEWSLETTER } else { "" };
                world.append_tab("INBOX", extra, n % 3 != 0);
            }
            // The first connection's first search is refused: the mail is stored without
            // categories, as by a build before the Split Inbox.
            world.refuse("X-GM-RAW", "NO [UNAVAILABLE] Try again later");
        }
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &store, account).await;
        let inbox = inbox(&store, account).await;
        let (important, other) = sides(&store, inbox).await;
        assert_eq!(
            (important.len(), other.len()),
            (960, 240),
            "the header rules"
        );
        assert_eq!(sent(&fake, "X-GM-RAW").len(), 1);
        drop(conn);

        // A new connection asks about the stored mail after its first pass, 500 UIDs a search.
        let mut conn = fake.connect().await;
        folder_pass(&mut conn, &store, inbox).await;
        let (important, other) = sides(&store, inbox).await;
        assert_eq!((important.len(), other.len()), (800, 400), "Gmail's tabs");
        assert!(important.iter().all(|uid| uid % 3 != 0));
        assert_eq!(sent(&fake, "X-GM-RAW").len(), 4);
        // Once: a later pass on the same connection asks no more.
        folder_pass(&mut conn, &store, inbox).await;
        assert_eq!(sent(&fake, "X-GM-RAW").len(), 4);
        assert!(
            store
                .uncategorized(inbox, 10)
                .recv()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
    });
}
