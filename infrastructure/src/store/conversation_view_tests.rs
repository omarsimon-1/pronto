//! The fork's conversation views (docs/superhuman/store.md): rows, order and filters, Trash,
//! Junk and drafts aside, the fork migration track, member expansion, conversation search, and
//! change events that keep list models exact under random writes (the twin of
//! `change_events_keep_list_models_exact`), every page checked against a recount from the raw
//! `messages` table.

use super::ops_tests::ready;
use super::tests::{Model, Rng, scratch, whole};
use super::*;
use rusqlite::{Connection, OpenFlags};
use sealwax_domain::account::AccountKey;
use sealwax_domain::message::base_subject;
use sealwax_domain::model::{
    Address, ConversationSummary, FolderRole, Headers, Participant, ThreadId,
};
use sealwax_domain::remind::{SOMEDAY, wait_key};
use sealwax_domain::split::{HeaderFacts, Split};
use sealwax_domain::triage::Triage;
use std::collections::{BTreeMap, HashMap};

const ME: &str = "test@example.test";

struct Fx {
    dir: tempfile::TempDir,
    store: Store,
    account: AccountId,
    inbox: FolderId,
    archive: FolderId,
    sent: FolderId,
    trash: FolderId,
    drafts: FolderId,
}

fn open(dir: &tempfile::TempDir) -> Store {
    Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap()
}

fn folder(store: &Store, account: AccountId, name: &str, role: FolderRole) -> FolderId {
    store
        .upsert_folder(FolderSpec::local(account, name, role))
        .recv_blocking()
        .unwrap()
        .value
}

fn account(store: &Store, name: &str, address: &str) -> AccountId {
    store
        .add_account(NewAccount {
            name: name.into(),
            address: address.into(),
        })
        .recv_blocking()
        .unwrap()
        .value
}

fn fixture() -> Fx {
    let dir = scratch();
    let store = open(&dir);
    let account = account(&store, "Test", ME);
    Fx {
        inbox: folder(&store, account, "INBOX", FolderRole::Inbox),
        archive: folder(&store, account, "Archive", FolderRole::Archive),
        sent: folder(&store, account, "Sent", FolderRole::Sent),
        trash: folder(&store, account, "Trash", FolderRole::Trash),
        drafts: folder(&store, account, "Drafts", FolderRole::Drafts),
        dir,
        store,
        account,
    }
}

/// A message `key` (its Message-ID `key@example.test`) from `from`, replying to `parents`.
fn mail(
    folder: FolderId,
    key: &str,
    date: i64,
    from: (&str, &str),
    parents: &[&str],
) -> NewMessage {
    let parents: Vec<String> = parents
        .iter()
        .map(|parent| format!("{parent}@example.test"))
        .collect();
    NewMessage {
        folder,
        date,
        flags: Flags::SEEN,
        headers: Headers {
            from: vec![Address::new(from.0, from.1)],
            subject: if parents.is_empty() {
                format!("Subject {key}")
            } else {
                format!(
                    "Re: Subject {}",
                    parents[0].trim_end_matches("@example.test")
                )
            },
            date: Some(date),
            message_id: Some(format!("{key}@example.test")),
            in_reply_to: parents.last().cloned().into_iter().collect(),
            references: parents,
            ..Headers::default()
        },
        snippet: format!("Snippet {key}"),
        search_text: format!("body of {key}"),
        size: 100,
        ..NewMessage::default()
    }
}

const INGRID: (&str, &str) = ("Ingrid Berg", "ingrid@example.test");
const TOMAS: (&str, &str) = ("Tomas Lie", "tomas@example.test");
const KARI: (&str, &str) = ("", "kari@example.test");
const SELF: (&str, &str) = ("Test Person", ME);

fn ingest(store: &Store, messages: Vec<NewMessage>) -> Vec<MessageId> {
    store
        .ingest(messages)
        .recv_blocking()
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

fn rows(store: &Store, spec: ViewSpec) -> Vec<ConversationSummary> {
    let page = whole(store, spec.conversations());
    assert!(page.rows.is_empty());
    page.conversations
}

fn latest_ids(rows: &[ConversationSummary]) -> Vec<MessageId> {
    rows.iter().map(|row| row.latest).collect()
}

fn clean(store: &Store) {
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}

fn names(row: &ConversationSummary) -> Vec<(&str, bool)> {
    row.participants
        .iter()
        .map(|person| (person.email.as_str(), person.is_me))
        .collect()
}

#[test]
fn a_reply_in_sent_moves_its_conversation_up_and_shows_the_user() {
    let f = fixture();
    let first = ingest(
        &f.store,
        vec![
            mail(f.inbox, "a1", 100, INGRID, &[]),
            mail(f.inbox, "b1", 200, TOMAS, &[]),
            mail(f.inbox, "c1", 300, KARI, &[]),
        ],
    );
    let spec = ViewSpec::folder(f.inbox).conversations();
    let events = f.store.subscribe();
    let opened = f
        .store
        .open_view(OpenView {
            spec,
            first_page: 10,
            section_starts: vec![250],
        })
        .recv_blocking()
        .unwrap();
    assert_eq!((opened.total, opened.section_counts.clone()), (3, vec![1]));
    assert!(opened.rows.is_empty());
    assert_eq!(
        latest_ids(&opened.conversations),
        [first[2], first[1], first[0]]
    );
    let a = &opened.conversations[2];
    assert_eq!(
        (
            a.subject.as_str(),
            a.messages,
            a.members,
            a.anchor,
            a.folder
        ),
        ("Subject a1", 1, 1, first[0], f.inbox)
    );

    // The user's reply, filed in Sent, takes Ingrid's conversation to the top.
    let reply = ingest(&f.store, vec![mail(f.sent, "a2", 400, SELF, &["a1"])])[0];
    let event = events.recv_blocking().unwrap();
    let ViewDiff::ConversationSplice {
        removed,
        inserted,
        changed,
        total,
    } = &event.views[0].diff
    else {
        panic!("{:?}", event.views);
    };
    assert_eq!((removed.as_slice(), total), ([2].as_slice(), &3));
    assert!(changed.is_empty());
    assert_eq!(inserted[0].position, 0);
    let a = &inserted[0].row;
    assert_eq!(
        (
            a.latest,
            a.date,
            a.sender_email.as_str(),
            a.snippet.as_str()
        ),
        (reply, 400, ME, "Snippet a2")
    );
    assert_eq!(
        (a.messages, a.members, a.last_member, a.anchor),
        (2, 1, first[0], first[0])
    );
    assert_eq!(names(a), [("ingrid@example.test", false), (ME, true)]);
    assert_eq!(a.subject, "Subject a1", "no Re:");

    // Ingrid answers: the conversation at the top stays there, changed in place.
    let mut answer = mail(f.inbox, "a3", 500, INGRID, &["a2"]);
    answer.flags = Flags::NONE;
    let answer = ingest(&f.store, vec![answer])[0];
    let event = events.recv_blocking().unwrap();
    let ViewDiff::ConversationSplice {
        removed,
        inserted,
        changed,
        ..
    } = &event.views[0].diff
    else {
        panic!("{:?}", event.views);
    };
    assert!(
        removed.is_empty() && inserted.is_empty(),
        "{:?}",
        event.views
    );
    assert_eq!(changed.len(), 1);
    let a = &changed[0].row;
    assert_eq!((changed[0].position, a.latest), (0, answer));
    assert_eq!(
        (a.messages, a.members, a.unread, a.anchor, a.last_member),
        (3, 2, 1, answer, answer),
        "the reader opens at the first unread"
    );
    assert!(a.participants[0].unread && !a.participants[1].unread);
    assert_eq!(rows(&f.store, ViewSpec::folder(f.inbox))[0], *a);
    clean(&f.store);
}

#[test]
fn filters_match_the_members_in_the_views_folders() {
    let f = fixture();
    let mut unread_archived = mail(f.archive, "x2", 300, INGRID, &["x1"]);
    unread_archived.flags = Flags::NONE;
    let mut starred = mail(f.inbox, "y1", 200, TOMAS, &[]);
    starred.flags = Flags::SEEN | Flags::FLAGGED;
    let mut attached = mail(f.inbox, "z1", 100, KARI, &[]);
    attached.has_attachments = true;
    let mut unread = mail(f.inbox, "z2", 150, KARI, &["z1"]);
    unread.flags = Flags::NONE;
    let ids = ingest(
        &f.store,
        vec![
            mail(f.inbox, "x1", 50, INGRID, &[]),
            unread_archived,
            starred,
            attached,
            unread,
        ],
    );
    let inbox = ViewSpec::folder(f.inbox);
    assert_eq!(rows(&f.store, inbox).len(), 3);
    let only = |filter| latest_ids(&rows(&f.store, inbox.with_filter(filter)));
    // x's unread message is in Archive: not an unread conversation of the Inbox.
    assert_eq!(only(ViewFilter::Unread), [ids[4]]);
    assert_eq!(only(ViewFilter::Starred), [ids[2]]);
    assert_eq!(only(ViewFilter::Attachments), [ids[4]]);
    let x = &rows(&f.store, inbox)[0];
    assert_eq!(
        (x.latest, x.unread, x.members, x.messages),
        (ids[1], 0, 1, 2)
    );
    let archive = rows(
        &f.store,
        ViewSpec::folder(f.archive).with_filter(ViewFilter::Unread),
    );
    assert_eq!((archive.len(), archive[0].anchor), (1, ids[1]));
    let z = &rows(&f.store, inbox.with_filter(ViewFilter::Attachments))[0];
    assert_eq!((z.attachments, z.unread, z.anchor), (1, 1, ids[4]));
    clean(&f.store);
}

#[test]
fn trash_junk_and_drafts_stand_aside() {
    let f = fixture();
    let mut draft = mail(f.drafts, "d2", 900, SELF, &["d1"]);
    draft.flags = Flags::SEEN | Flags::DRAFT;
    let ids = ingest(
        &f.store,
        vec![
            mail(f.inbox, "d1", 100, INGRID, &[]),
            draft,
            mail(f.inbox, "t1", 200, TOMAS, &[]),
            mail(f.trash, "t2", 800, KARI, &["t1"]),
            mail(f.inbox, "c1", 300, KARI, &[]),
            // A copy of c1 in Archive: one message, two members.
            mail(f.archive, "c1", 300, KARI, &[]),
        ],
    );
    let inbox = rows(&f.store, ViewSpec::folder(f.inbox));
    // c1's latest copy is the Archive one (the same date, a higher id): the same message.
    assert_eq!(
        latest_ids(&inbox),
        [ids[5], ids[2], ids[0]],
        "neither the draft nor the trashed reply moves a conversation"
    );
    let (c, t, d) = (&inbox[0], &inbox[1], &inbox[2]);
    assert_eq!((c.messages, c.members), (1, 1), "each Message-ID once");
    assert_eq!((t.messages, t.has_draft), (1, false));
    assert_eq!((d.messages, d.has_draft, d.date), (1, true, 100));
    assert_eq!(names(t), [("tomas@example.test", false)]);
    // In Trash and Drafts a conversation sits by its own members there.
    let trash = rows(&f.store, ViewSpec::folder(f.trash));
    assert_eq!(
        (trash[0].latest, trash[0].date, trash[0].messages),
        (ids[3], 800, 1)
    );
    let drafts = rows(&f.store, ViewSpec::folder(f.drafts));
    assert_eq!((drafts[0].latest, drafts[0].date), (ids[1], 900));
    // Back in the Inbox, the trashed reply joins it again: now it's the latest.
    let moved = f
        .store
        .queue_triage(QueueTriage {
            action: Triage::MoveTo(f.inbox),
            messages: vec![ids[3]],
            not_before: 0,
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(moved.applied, 1);
    let inbox = rows(&f.store, ViewSpec::folder(f.inbox));
    assert_eq!(latest_ids(&inbox), [ids[3], ids[5], ids[0]]);
    assert_eq!((inbox[0].messages, inbox[0].members), (2, 2));
    assert!(rows(&f.store, ViewSpec::folder(f.trash)).is_empty());
    clean(&f.store);
}

#[test]
fn all_inboxes_merge_accounts_and_pages_match_at_any_offset() {
    let f = fixture();
    let other = account(&f.store, "Other", "other@example.test");
    let other_inbox = folder(&f.store, other, "INBOX", FolderRole::Inbox);
    let mut rng = Rng(5);
    let mut messages = Vec::new();
    for n in 0..300u64 {
        let target = [f.inbox, other_inbox, f.archive][(n % 3) as usize];
        let key = format!("p{n}");
        let parent = (n > 10 && rng.below(3) == 0).then(|| format!("p{}", rng.below(n)));
        let parents: Vec<&str> = parent.iter().map(String::as_str).collect();
        messages.push(mail(
            target,
            &key,
            1_000 + rng.below(500) as i64,
            INGRID,
            &parents,
        ));
    }
    ingest(&f.store, messages);
    let spec = ViewSpec::unified_inbox().conversations();
    let all = rows(&f.store, spec);
    assert!(all.len() > 150, "{}", all.len());
    assert!(
        all.windows(2)
            .all(|pair| (pair[0].date, pair[0].latest) > (pair[1].date, pair[1].latest))
    );
    for offset in [
        0,
        1,
        37,
        99,
        100,
        101,
        150,
        all.len() as u32 - 7,
        all.len() as u32,
    ] {
        let page = f.store.page(spec, offset, 13).recv_blocking().unwrap();
        let want: Vec<ConversationSummary> =
            all.iter().skip(offset as usize).take(13).cloned().collect();
        assert_eq!(page.conversations, want, "offset {offset}");
    }
    let start = 1_250;
    let opened = f
        .store
        .open_view(OpenView {
            spec,
            first_page: 5,
            section_starts: vec![start],
        })
        .recv_blocking()
        .unwrap();
    let newer = all.iter().filter(|row| row.date >= start).count() as u32;
    assert_eq!(
        (opened.total, opened.section_counts),
        (all.len() as u32, vec![newer])
    );
    let accounts: Vec<AccountId> = all.iter().map(|row| row.account).collect();
    assert!(accounts.contains(&f.account) && accounts.contains(&other));
    clean(&f.store);
}

#[test]
fn members_expand_a_conversation_in_the_views_folders() {
    let f = fixture();
    let mut unread = mail(f.inbox, "m3", 300, INGRID, &["m2"]);
    unread.flags = Flags::NONE;
    let ids = ingest(
        &f.store,
        vec![
            mail(f.inbox, "m1", 100, INGRID, &[]),
            mail(f.sent, "m2", 200, SELF, &["m1"]),
            unread,
            mail(f.archive, "n1", 50, TOMAS, &[]),
        ],
    );
    let row = &rows(&f.store, ViewSpec::folder(f.inbox))[0];
    let members = |spec: ViewSpec| -> Vec<MessageId> {
        f.store
            .conversation_members(vec![row.thread], spec)
            .recv_blocking()
            .unwrap()
            .value
            .iter()
            .map(|member| member.id)
            .collect()
    };
    assert_eq!(members(ViewSpec::folder(f.inbox)), [ids[0], ids[2]]);
    assert_eq!(
        members(ViewSpec::folder(f.inbox).with_filter(ViewFilter::Unread)),
        [ids[2]]
    );
    assert_eq!(members(ViewSpec::unified_inbox()), [ids[0], ids[2]]);
    assert_eq!(members(ViewSpec::folder(f.sent)), [ids[1]]);
    let all = f
        .store
        .conversation_members(vec![row.thread], ViewSpec::folder(f.inbox))
        .recv_blocking()
        .unwrap()
        .value;
    assert_eq!(
        all.iter()
            .map(|m| (m.folder, m.flags.is_seen()))
            .collect::<Vec<_>>(),
        [(f.inbox, true), (f.inbox, false)]
    );
    // Archiving the conversation from the Inbox: one batch for both members.
    let queued = f
        .store
        .queue_triage(QueueTriage {
            action: Triage::Archive,
            messages: all.iter().map(|member| member.id).collect(),
            not_before: 0,
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(queued.applied, 2);
    assert!(rows(&f.store, ViewSpec::folder(f.inbox)).is_empty());
    let archived = rows(&f.store, ViewSpec::folder(f.archive));
    assert_eq!(archived.len(), 2);
    assert_eq!((archived[0].members, archived[0].messages), (2, 3));
    clean(&f.store);
}

#[test]
fn conversation_search_groups_matches_newest_first() {
    let f = fixture();
    let mut messages = Vec::new();
    for n in 0..30i64 {
        let key = format!("s{n}");
        let parent = (n >= 3).then(|| format!("s{}", n % 3));
        let parents: Vec<&str> = parent.iter().map(String::as_str).collect();
        let mut m = mail(f.inbox, &key, 1_000 + n, INGRID, &parents);
        m.search_text = format!("fjelltur {key}");
        messages.push(m);
    }
    messages.push(mail(f.trash, "s99", 5_000, KARI, &["s0"]));
    let ids = ingest(&f.store, messages);
    let hits = f
        .store
        .search_conversations("fjelltur", None, 10)
        .recv_blocking()
        .unwrap()
        .value;
    assert_eq!(hits.len(), 3, "three conversations");
    assert_eq!(
        hits.iter().map(|hit| hit.matched).collect::<Vec<_>>(),
        [ids[29], ids[28], ids[27]],
        "the newest match of each, newest first"
    );
    assert!(hits.iter().all(|hit| hit.conversation.messages == 10));
    let one = f
        .store
        .search_conversations("fjelltur", Some(f.inbox), 2)
        .recv_blocking()
        .unwrap()
        .value;
    assert_eq!(one.len(), 2);
    let row = &one[0].conversation;
    assert_eq!((row.thread, row.members), (hits[0].conversation.thread, 10));
    assert!(
        f.store
            .search_conversations("  ", None, 10)
            .recv_blocking()
            .unwrap()
            .value
            .is_empty()
    );
    // A newer draft of a reply (searched, as Drafts is) leaves the row's latest message as the
    // Inbox shows it.
    let mut draft = mail(f.drafts, "s100", 9_000, SELF, &["s29"]);
    draft.flags = Flags::SEEN | Flags::DRAFT;
    draft.search_text = "fjelltur utkast".into();
    let draft = ingest(&f.store, vec![draft])[0];
    let hits = f
        .store
        .search_conversations("fjelltur", None, 1)
        .recv_blocking()
        .unwrap()
        .value;
    let row = &hits[0].conversation;
    assert_eq!(
        (
            hits[0].matched,
            row.latest,
            row.has_draft,
            row.members,
            row.messages
        ),
        (draft, ids[29], true, 11, 10)
    );
    let inbox = &rows(&f.store, ViewSpec::folder(f.inbox))[0];
    assert_eq!(
        (row.date, &row.snippet, &row.participants, inbox.members),
        (inbox.date, &inbox.snippet, &inbox.participants, 10)
    );
}

#[test]
fn the_fork_track_recounts_older_stores_and_refuses_a_newer_one() {
    let f = fixture();
    let mut messages = Vec::new();
    for n in 0..40u64 {
        let parent = (n > 0 && n % 4 != 0).then(|| format!("o{}", n - 1));
        let parents: Vec<&str> = parent.iter().map(String::as_str).collect();
        let target = [f.inbox, f.archive, f.sent][(n % 3) as usize];
        messages.push(mail(
            target,
            &format!("o{n}"),
            100 + n as i64,
            TOMAS,
            &parents,
        ));
    }
    ingest(&f.store, messages);
    let before = rows(&f.store, ViewSpec::folder(f.inbox));
    let path = f.store.path().to_owned();
    let Fx { dir, store, .. } = f;
    store.close().unwrap();
    // A store this build's upstream wrote: no fork track at all.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE fork_thread_folders; DELETE FROM meta WHERE key LIKE 'fork_%';")
        .unwrap();
    drop(conn);
    let store = open(&dir);
    assert_eq!(rows(&store, ViewSpec::folder(before[0].folder)), before);
    clean(&store);
    store.close().unwrap();
    // An older build wrote since (bodies came in with attachments): recounted at open.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "UPDATE messages SET has_attachments = 1; \
         UPDATE store_version SET version = version + 1;",
    )
    .unwrap();
    drop(conn);
    let store = open(&dir);
    let after = rows(&store, ViewSpec::folder(before[0].folder));
    assert_eq!(after.len(), before.len());
    assert!(after.iter().all(|row| row.attachments == row.members));
    clean(&store);
    store.close().unwrap();
    // A newer fork schema is refused and left as it is.
    let conn = Connection::open(&path).unwrap();
    conn.execute("UPDATE meta SET value = 99 WHERE key = 'fork_schema'", [])
        .unwrap();
    drop(conn);
    let error = Store::open(&path, StoreOptions::default()).err().unwrap();
    assert!(
        matches!(
            error,
            StoreError::NewerForkSchema {
                found: 99,
                supported: super::fork::VERSION
            }
        ),
        "{error}"
    );
}

/// A conversation list model as the GTK side would keep it, fed only by change events.
pub(super) struct Conversations {
    pub(super) view: ViewHandle,
    pub(super) rows: Vec<ConversationSummary>,
    pub(super) version: StoreVersion,
}

impl Conversations {
    pub(super) fn open(store: &Store, spec: ViewSpec) -> Self {
        let opened = store
            .open_view(OpenView {
                spec,
                first_page: u32::MAX,
                section_starts: Vec::new(),
            })
            .recv_blocking()
            .unwrap();
        assert_eq!(opened.conversations.len(), opened.total as usize);
        Self {
            view: opened.view,
            rows: opened.conversations,
            version: opened.version,
        }
    }

    /// Applies an event; replies whether the view had a splice, a move or a reload in it.
    pub(super) fn apply(&mut self, store: &Store, event: &ChangeEvent) -> (bool, bool, bool) {
        assert_eq!(
            event.version.0,
            self.version.0 + 1,
            "events arrive in order"
        );
        self.version = event.version;
        let mut seen = (false, false, false);
        for change in event
            .views
            .iter()
            .filter(|change| change.view == self.view.id())
        {
            match &change.diff {
                ViewDiff::ConversationSplice {
                    removed,
                    inserted,
                    changed,
                    total,
                } => {
                    seen.0 = true;
                    assert!(removed.windows(2).all(|pair| pair[0] > pair[1]));
                    let gone: Vec<ThreadId> = removed
                        .iter()
                        .map(|position| self.rows.remove(*position as usize).thread)
                        .collect();
                    assert!(
                        inserted
                            .windows(2)
                            .all(|pair| pair[0].position < pair[1].position)
                    );
                    for item in inserted {
                        seen.1 |= gone.contains(&item.row.thread);
                        self.rows.insert(item.position as usize, item.row.clone());
                    }
                    for item in changed {
                        assert_eq!(self.rows[item.position as usize].thread, item.row.thread);
                        self.rows[item.position as usize] = item.row.clone();
                    }
                    assert_eq!(self.rows.len(), *total as usize);
                }
                ViewDiff::Reload { total } => {
                    seen.2 = true;
                    let page = whole(store, self.view.spec());
                    assert_eq!(page.version, event.version);
                    assert_eq!(page.conversations.len(), *total as usize);
                    self.rows = page.conversations;
                }
                ViewDiff::Splice { .. } => panic!("a conversation view's message splice"),
            }
        }
        seen
    }
}

/// A shown or hidden row of `messages`, as the reference reads it.
struct Raw {
    id: MessageId,
    folder: FolderId,
    thread: ThreadId,
    flags: u32,
    attachments: bool,
    date: i64,
    name: String,
    email: String,
    snippet: String,
    subject: String,
    message_id: Option<String>,
}

/// Everything the reference needs, read straight from the tables: the messages, what the
/// Split Inbox was told of them (facts and categories as ingested or set, the user's choices),
/// and when reminded conversations came back.
struct World {
    messages: Vec<Raw>,
    roles: HashMap<FolderId, FolderRole>,
    threads: HashMap<ThreadId, (AccountId, String)>,
    own: HashMap<AccountId, String>,
    inboxes: Vec<FolderId>,
    kept: HashMap<MessageId, (HeaderFacts, Option<Split>)>,
    choices: HashMap<String, Split>,
    /// Remind Me: each reminded conversation's threads (those of the account's messages,
    /// hidden or not, with one of its Message-IDs; its thread without any) and when it came
    /// back, the latest should two find one thread.
    bumps: HashMap<ThreadId, i64>,
    /// Each waiting conversation's threads, by the same rules, with the Reminders folder it
    /// waits in and when it is due, the soonest should two find one (phase C).
    waits: HashMap<(ThreadId, FolderId), i64>,
}

impl World {
    /// Where the Split Inbox should put each shown message, by `split::classify_reminded`'s
    /// rules from the raw rows: a conversation back from Reminders, the user's choice for its
    /// sender, a thread with a shown message in Sent that isn't a draft, the category, the
    /// facts.
    fn splits(&self) -> HashMap<MessageId, Split> {
        let sent: std::collections::BTreeSet<ThreadId> = self
            .messages
            .iter()
            .filter(|m| m.flags & 280 == 0 && self.roles[&m.folder] == FolderRole::Sent)
            .map(|m| m.thread)
            .collect();
        self.messages
            .iter()
            .filter(|m| m.flags & 264 == 0)
            .map(|m| {
                let (facts, category) = self.kept[&m.id];
                let choice = sealwax_domain::split::sender_key(&m.email)
                    .and_then(|key| self.choices.get(&key).copied());
                let split = choice.unwrap_or(if sent.contains(&m.thread) {
                    Split::Important
                } else {
                    category.unwrap_or(if facts.is_empty() {
                        Split::Important
                    } else {
                        Split::Other
                    })
                });
                if self.bumps.contains_key(&m.thread) {
                    return (m.id, Split::Important);
                }
                (m.id, split)
            })
            .collect()
    }
}

fn world(path: &std::path::Path) -> World {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let messages: Vec<Raw> = conn
        .prepare(
            "SELECT id, folder_id, thread_id, flags, has_attachments, date, from_name, \
               from_addr, snippet, subject, message_id FROM messages",
        )
        .unwrap()
        .query_map([], |row| {
            Ok(Raw {
                id: MessageId(row.get(0)?),
                folder: FolderId(row.get(1)?),
                thread: ThreadId(row.get(2)?),
                flags: row.get(3)?,
                attachments: row.get(4)?,
                date: row.get(5)?,
                name: row.get(6)?,
                email: row.get(7)?,
                snippet: row.get(8)?,
                subject: row.get(9)?,
                message_id: row.get(10)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let roles = conn
        .prepare("SELECT id, role FROM folders")
        .unwrap()
        .query_map([], |row| {
            Ok((
                FolderId(row.get(0)?),
                FolderRole::parse(&row.get::<_, String>(1)?),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let threads = conn
        .prepare("SELECT id, account_id, subject FROM threads")
        .unwrap()
        .query_map([], |row| {
            Ok((ThreadId(row.get(0)?), (AccountId(row.get(1)?), row.get(2)?)))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let own = conn
        .prepare("SELECT id, lower(address) FROM accounts")
        .unwrap()
        .query_map([], |row| Ok((AccountId(row.get(0)?), row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let inboxes = conn
        .prepare(
            "SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id \
             WHERE f.role = 'inbox' AND a.detached = 0 ORDER BY f.id",
        )
        .unwrap()
        .query_map([], |row| row.get(0).map(FolderId))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let kept = conn
        .prepare("SELECT id, facts, category FROM fork_message_split")
        .unwrap()
        .query_map([], |row| {
            Ok((
                MessageId(row.get(0)?),
                (
                    HeaderFacts::from_bits(row.get(1)?),
                    row.get::<_, Option<bool>>(2)?.map(Split::of),
                ),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let choices = conn
        .prepare("SELECT address, important FROM fork_sender_split")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, Split::of(row.get(1)?))))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let owners: HashMap<FolderId, AccountId> = conn
        .prepare("SELECT id, account_id FROM folders")
        .unwrap()
        .query_map([], |row| {
            Ok((FolderId(row.get(0)?), AccountId(row.get(1)?)))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let reminded: Vec<(i64, AccountId, Option<ThreadId>, i64)> = conn
        .prepare(
            "SELECT id, account_id, thread_id, reminded_at FROM fork_reminders WHERE state = 1",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                AccountId(row.get(1)?),
                row.get::<_, Option<i64>>(2)?.map(ThreadId),
                row.get(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    // A reminder's conversation: the threads of the account's messages with one of its
    // Message-IDs, or its thread when it has none.
    let threads_of = |id: i64, account: AccountId, thread: Option<ThreadId>| -> Vec<ThreadId> {
        let keys: Vec<String> = conn
            .prepare("SELECT message_id FROM fork_reminder_keys WHERE reminder = ?1")
            .unwrap()
            .query_map([id], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        if keys.is_empty() {
            thread.into_iter().collect()
        } else {
            messages
                .iter()
                .filter(|m: &&Raw| owners[&m.folder] == account)
                .filter(|m| m.message_id.as_ref().is_some_and(|key| keys.contains(key)))
                .map(|m| m.thread)
                .collect()
        }
    };
    let mut bumps: HashMap<ThreadId, i64> = HashMap::new();
    for (id, account, thread, at) in reminded {
        for thread in threads_of(id, account, thread) {
            let bump = bumps.entry(thread).or_insert(at);
            *bump = (*bump).max(at);
        }
    }
    let waiting: Vec<(i64, AccountId, Option<ThreadId>, FolderId, i64)> = conn
        .prepare(
            "SELECT id, account_id, thread_id, folder_id, wake_at FROM fork_reminders \
             WHERE state = 0 AND folder_id IS NOT NULL",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                AccountId(row.get(1)?),
                row.get::<_, Option<i64>>(2)?.map(ThreadId),
                FolderId(row.get(3)?),
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut waits: HashMap<(ThreadId, FolderId), i64> = HashMap::new();
    for (id, account, thread, folder, at) in waiting {
        for thread in threads_of(id, account, thread) {
            let wait = waits.entry((thread, folder)).or_insert(at);
            *wait = (*wait).min(at);
        }
    }
    World {
        messages,
        roles,
        threads,
        own,
        inboxes,
        kept,
        choices,
        bumps,
        waits,
    }
}

/// Whether a raw message passes a filter that isn't a split filter.
fn passes_filter(m: &Raw, filter: ViewFilter) -> bool {
    match filter {
        ViewFilter::All | ViewFilter::Important | ViewFilter::Other => true,
        ViewFilter::Unread => m.flags & 1 == 0,
        ViewFilter::Starred => m.flags & 4 != 0,
        ViewFilter::Attachments => m.attachments,
    }
}

/// The rows a message view of `folders` keeping a side (and maybe a filter on top) should
/// list, from the reference's classification, newest first.
fn expected_messages(world: &World, folders: &[FolderId], spec: ViewSpec) -> Vec<MessageId> {
    let splits = world.splits();
    let mut rows: Vec<(i64, MessageId)> = world
        .messages
        .iter()
        .filter(|m| folders.contains(&m.folder))
        .filter(|m| splits.get(&m.id).copied() == spec.side())
        .filter(|m| passes_filter(m, spec.base_filter()))
        .map(|m| (m.date, m.id))
        .collect();
    rows.sort_by(|a, b| b.cmp(a));
    rows.into_iter().map(|(_, id)| id).collect()
}

/// The rows a conversation view of `folders` should list, recounted from the raw messages by
/// the rules of docs/superhuman/store.md (independently of `fork_thread_folders`): one of its
/// members there passes the filter, and it is on the view's side, Important with an Important
/// member there and Other with none (docs/superhuman/split.md).
fn expected(world: &World, folders: &[FolderId], spec: ViewSpec) -> Vec<ConversationSummary> {
    let binned = |folder: FolderId| {
        matches!(
            world.roles[&folder],
            FolderRole::Trash | FolderRole::Junk | FolderRole::Drafts
        )
    };
    let aside = |m: &Raw| binned(m.folder) || m.flags & 16 != 0;
    let splits = world.splits();
    let mut threads: BTreeMap<ThreadId, Vec<&Raw>> = BTreeMap::new();
    for m in world.messages.iter().filter(|m| m.flags & 264 == 0) {
        threads.entry(m.thread).or_default().push(m);
    }
    let mut out = Vec::new();
    for (thread, mut shown) in threads {
        shown.sort_by_key(|m| (m.date, m.id));
        let members: Vec<&Raw> = shown
            .iter()
            .filter(|m| folders.contains(&m.folder))
            .copied()
            .collect();
        let important = |m: &&Raw| splits[&m.id] == Split::Important;
        let filtered = members.iter().any(|m| passes_filter(m, spec.base_filter()));
        // Important with an Important member there, Other with none.
        let held = filtered
            && match spec.side() {
                None => true,
                Some(Split::Important) => members.iter().any(important),
                Some(Split::Other) => !members.iter().any(important),
            };
        if !held {
            continue;
        }
        let kept: Vec<&Raw> = shown.iter().filter(|m| !aside(m)).copied().collect();
        let tier = if kept.is_empty() { shown.clone() } else { kept };
        let newest = |list: &[&Raw]| list.iter().map(|m| (m.date, m.id)).max().unwrap();
        let conversation_latest = newest(&tier);
        // Seen from a Trash, Junk or Drafts folder alone, its own newest member there. Back
        // from Reminders, it sorts in an Inbox by when it came back while that is later;
        // waiting in its Reminders folder, there by when it is due (phase C).
        let in_inbox = members
            .iter()
            .any(|m| world.roles[&m.folder] == FolderRole::Inbox);
        let waited = members
            .iter()
            .filter_map(|m| world.waits.get(&(thread, m.folder)))
            .map(|at| wait_key(*at))
            .max();
        let latest = if members.iter().any(|m| !binned(m.folder)) {
            match (waited, world.bumps.get(&thread)) {
                (Some(key), _) => (key, conversation_latest.1),
                (None, Some(at)) if in_inbox => {
                    (conversation_latest.0.max(*at), conversation_latest.1)
                }
                _ => conversation_latest,
            }
        } else {
            newest(&members)
        };
        let latest_message = shown.iter().find(|m| m.id == latest.1).unwrap();
        let last = members.last().unwrap();
        let anchor = members
            .iter()
            .find(|m| m.flags & 1 == 0)
            .copied()
            .unwrap_or(last);
        let mut ids: Vec<&str> = tier
            .iter()
            .filter_map(|m| m.message_id.as_deref())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let messages = ids.len() + tier.iter().filter(|m| m.message_id.is_none()).count();
        let (account, subject) = world.threads[&thread].clone();
        let own = &world.own[&account];
        // Each person with the place of their newest message: the first and the three who
        // wrote last are named.
        let mut people: Vec<(String, Participant, usize)> = Vec::new();
        for (index, m) in tier.iter().enumerate() {
            let key = if m.email.is_empty() {
                m.name.to_lowercase()
            } else {
                m.email.to_lowercase()
            };
            if !people.iter().any(|(known, _, _)| *known == key) {
                people.push((
                    key.clone(),
                    Participant {
                        name: String::new(),
                        email: m.email.clone(),
                        is_me: *own == key,
                        unread: false,
                        last: m.date,
                    },
                    index,
                ));
            }
            let person = people
                .iter_mut()
                .find(|(known, _, _)| *known == key)
                .unwrap();
            if !m.name.is_empty() {
                person.1.name.clone_from(&m.name);
            }
            person.1.last = m.date;
            person.2 = index;
        }
        for m in &shown {
            let key = if m.email.is_empty() {
                m.name.to_lowercase()
            } else {
                m.email.to_lowercase()
            };
            if let Some((_, person, _)) = people.iter_mut().find(|(known, _, _)| *known == key) {
                person.is_me |= matches!(
                    world.roles[&m.folder],
                    FolderRole::Sent | FolderRole::Drafts
                );
                person.unread |= m.flags & 1 == 0 && folders.contains(&m.folder);
            }
        }
        let participant_count = people.len() as u32;
        let mut newest: Vec<usize> = people.iter().skip(1).map(|(_, _, last)| *last).collect();
        newest.sort_unstable_by(|a, b| b.cmp(a));
        newest.truncate(3);
        let participants: Vec<Participant> = people
            .into_iter()
            .enumerate()
            .filter(|(at, (_, _, last))| {
                *at == 0 || participant_count <= 4 || newest.contains(last)
            })
            .map(|(_, (_, person, _))| person)
            .collect();
        out.push(ConversationSummary {
            thread,
            account,
            anchor: anchor.id,
            folder: anchor.folder,
            last_member: last.id,
            latest: latest.1,
            date: latest.0,
            sender_name: latest_message.name.clone(),
            sender_email: latest_message.email.clone(),
            snippet: latest_message.snippet.clone(),
            subject: if subject.is_empty() {
                base_subject(&latest_message.subject).to_owned()
            } else {
                subject
            },
            participants,
            participant_count,
            messages: messages as u32,
            members: members.len() as u32,
            unread: members.iter().filter(|m| m.flags & 1 == 0).count() as u32,
            flagged: members.iter().filter(|m| m.flags & 4 != 0).count() as u32,
            attachments: members.iter().filter(|m| m.attachments).count() as u32,
            has_draft: shown
                .iter()
                .any(|m| world.roles[&m.folder] == FolderRole::Drafts || m.flags & 16 != 0),
        });
    }
    out.sort_by_key(|row| std::cmp::Reverse((row.date, row.latest)));
    out
}

/// What the replay picks from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Local,
    Other,
    Server,
}

#[test]
fn conversation_events_keep_list_models_exact() {
    // 1051, 1093 and 1101 merged a conversation away and gave its id to a new one in the same
    // commit, which the message views' model mapped into the other (`writer::retire_merged`).
    for seed in [2026, 7, 4242, 1051, 1093, 1101] {
        replay(seed);
    }
}

/// More seeds of the replay, for a release run:
/// `PRONTO_REPLAY_SEEDS=100..160 cargo test --release -p sealwax-infrastructure -- --ignored
/// more_seeds` (100..160 by default).
#[test]
#[ignore]
fn conversation_events_keep_list_models_exact_more_seeds() {
    let seeds = std::env::var("PRONTO_REPLAY_SEEDS").unwrap_or_else(|_| "100..160".into());
    let (from, to) = seeds.split_once("..").expect("seeds as from..to");
    for seed in from.parse::<u64>().unwrap()..to.parse::<u64>().unwrap() {
        replay(seed);
    }
}

/// Random ingests (replies, copies, merges, drafts, mail of the user's in Sent), flag changes,
/// removals, moves done at once (a local account's) and pending (a server account's, then
/// undone), and a folder's role changing, with conversation views of every kind open (two
/// Inbox folders of one account fold in All Inboxes) beside message views. After each step,
/// every model equals a fresh page, and every page equals the reference recount.
fn replay(seed: u64) {
    let dir = scratch();
    let store = open(&dir);
    let path = store.path().to_owned();
    let local = account(&store, "Test", ME);
    let inbox = folder(&store, local, "INBOX", FolderRole::Inbox);
    let inbox2 = folder(&store, local, "Inbox 2", FolderRole::Inbox);
    let archive = folder(&store, local, "Archive", FolderRole::Archive);
    let sent = folder(&store, local, "Sent", FolderRole::Sent);
    let trash = folder(&store, local, "Trash", FolderRole::Trash);
    let drafts = folder(&store, local, "Drafts", FolderRole::Drafts);
    let projects = folder(&store, local, "Projects", FolderRole::Other);
    let other = account(&store, "Other", "other@example.test");
    let other_inbox = folder(&store, other, "INBOX", FolderRole::Inbox);
    let server = store
        .ensure_account(
            AccountKey("conversation-test".into()),
            NewAccount {
                name: "Server".into(),
                address: "server@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    let server_folders = store
        .upsert_folders(vec![
            FolderSpec::local(server, "INBOX", FolderRole::Inbox),
            FolderSpec::local(server, "Archive", FolderRole::Archive),
            FolderSpec::local(server, "Trash", FolderRole::Trash),
        ])
        .recv_blocking()
        .unwrap()
        .value;
    for id in &server_folders {
        ready(&store, *id, 7);
    }
    let (server_inbox, server_archive) = (server_folders[0], server_folders[1]);
    let targets = [
        (inbox, Owner::Local),
        (inbox, Owner::Local),
        (inbox2, Owner::Local),
        (archive, Owner::Local),
        (sent, Owner::Local),
        (trash, Owner::Local),
        (drafts, Owner::Local),
        (projects, Owner::Local),
        (other_inbox, Owner::Other),
        (other_inbox, Owner::Other),
        (server_inbox, Owner::Server),
        (server_inbox, Owner::Server),
        (server_archive, Owner::Server),
    ];
    let senders = [INGRID, TOMAS, KARI, ("Ola", "ola@example.test"), SELF];
    let mut rng = Rng(seed);
    // The Split Inbox's choices come from a second generator, and Remind Me's from a third, so
    // the rest of a seed's steps are the same as before them.
    let mut splits = Rng(seed ^ 0x5b11_7000);
    let mut reminds = Rng(seed ^ 0x2e41_d000);
    let mut serial = 0u64;
    let mut uid = 0u32;
    // Every message so far: its key, owner and stored id.
    let mut known: Vec<(String, Owner, MessageId)> = Vec::new();
    let mut fresh =
        |rng: &mut Rng, splits: &mut Rng, known: &[(String, Owner, MessageId)], count: usize| {
            (0..count)
                .map(|_| {
                    serial += 1;
                    let (target, owner) = targets[rng.below(targets.len() as u64) as usize];
                    let same: Vec<&(String, Owner, MessageId)> =
                        known.iter().filter(|(_, o, _)| *o == owner).collect();
                    let pick =
                        |rng: &mut Rng| same[rng.below(same.len() as u64) as usize].0.clone();
                    let copy = !same.is_empty() && rng.below(12) == 0;
                    let key = if copy {
                        pick(rng)
                    } else {
                        format!("r{seed}-{serial}")
                    };
                    let mut parents = Vec::new();
                    if !copy && !same.is_empty() && rng.below(2) == 0 {
                        parents.push(pick(rng));
                        if rng.below(8) == 0 {
                            parents.push(pick(rng));
                        }
                    }
                    let from = if target == sent || target == drafts {
                        SELF
                    } else {
                        senders[rng.below(senders.len() as u64) as usize]
                    };
                    let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
                    let mut m = mail(target, &key, 10_000 + rng.below(600) as i64, from, &parents);
                    m.flags = if rng.below(3) == 0 {
                        Flags::NONE
                    } else {
                        Flags::SEEN
                    };
                    if rng.below(6) == 0 {
                        m.flags = m.flags | Flags::FLAGGED;
                    }
                    if target == drafts || rng.below(25) == 0 {
                        m.flags = m.flags | Flags::DRAFT;
                    }
                    m.has_attachments = rng.below(4) == 0;
                    if owner == Owner::Server {
                        uid += 1;
                        m.uid = Some(uid);
                    }
                    if splits.below(3) == 0 {
                        m.headers.facts = HeaderFacts::from_bits(1 + splits.below(31) as u8);
                    }
                    m.category = match splits.below(5) {
                        0 => Some(Split::Important),
                        1 => Some(Split::Other),
                        _ => None,
                    };
                    (m, owner, key)
                })
                .collect::<Vec<_>>()
        };
    let store_batch = |store: &Store,
                       known: &mut Vec<(String, Owner, MessageId)>,
                       batch: Vec<(NewMessage, Owner, String)>| {
        let owners: Vec<(Owner, String)> = batch
            .iter()
            .map(|(_, owner, key)| (*owner, key.clone()))
            .collect();
        let reply = store
            .ingest(batch.into_iter().map(|(m, ..)| m).collect())
            .recv_blocking()
            .unwrap();
        for ((owner, key), id) in owners.into_iter().zip(&reply.ids) {
            if let Some(id) = id {
                known.push((key, owner, *id));
            }
        }
        reply.version
    };
    let batch = fresh(&mut rng, &mut splits, &known, 80);
    store_batch(&store, &mut known, batch);

    let events = store.subscribe();
    let specs = [
        ViewSpec::folder(inbox),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Unread),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Starred),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Attachments),
        ViewSpec::unified_inbox(),
        ViewSpec::unified_inbox().with_filter(ViewFilter::Unread),
        ViewSpec::folder(archive),
        ViewSpec::folder(sent),
        ViewSpec::folder(trash),
        ViewSpec::folder(drafts),
        ViewSpec::folder(projects),
        ViewSpec::folder(server_inbox),
        ViewSpec::folder(server_archive).with_filter(ViewFilter::Unread),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Important),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Other),
        ViewSpec::unified_inbox().with_filter(ViewFilter::Important),
        ViewSpec::unified_inbox().with_filter(ViewFilter::Other),
        ViewSpec::folder(server_inbox).with_filter(ViewFilter::Other),
        ViewSpec::folder(archive).with_filter(ViewFilter::Important),
        // A side under another filter (phase C: the Important split's unread conversations).
        ViewSpec::folder(inbox)
            .with_filter(ViewFilter::Unread)
            .with_split(Some(Split::Important)),
        ViewSpec::folder(inbox)
            .with_filter(ViewFilter::Starred)
            .with_split(Some(Split::Other)),
        ViewSpec::unified_inbox()
            .with_filter(ViewFilter::Unread)
            .with_split(Some(Split::Other)),
        ViewSpec::unified_inbox()
            .with_filter(ViewFilter::Attachments)
            .with_split(Some(Split::Important)),
        ViewSpec::folder(server_inbox)
            .with_filter(ViewFilter::Unread)
            .with_split(Some(Split::Important)),
        ViewSpec::unified_inbox().with_split(Some(Split::Other)),
    ];
    let mut models: Vec<Conversations> = specs
        .iter()
        .map(|spec| Conversations::open(&store, spec.conversations()))
        .collect();
    let mut messages: Vec<Model> = [
        ViewSpec::folder(inbox),
        ViewSpec::unified_inbox(),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Important),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Other),
        ViewSpec::unified_inbox().with_filter(ViewFilter::Important),
        ViewSpec::unified_inbox().with_filter(ViewFilter::Other),
        ViewSpec::folder(server_inbox).with_filter(ViewFilter::Important),
        ViewSpec::folder(sent).with_filter(ViewFilter::Other),
        ViewSpec::folder(inbox)
            .with_filter(ViewFilter::Unread)
            .with_split(Some(Split::Important)),
        ViewSpec::unified_inbox()
            .with_filter(ViewFilter::Starred)
            .with_split(Some(Split::Other)),
        ViewSpec::folder(server_inbox)
            .with_filter(ViewFilter::Unread)
            .with_split(Some(Split::Other)),
        ViewSpec::unified_inbox()
            .with_filter(ViewFilter::Attachments)
            .with_split(Some(Split::Important)),
        ViewSpec::folder(inbox).with_split(Some(Split::Other)),
    ]
    .iter()
    .map(|spec| {
        let opened = store
            .open_view(OpenView {
                spec: *spec,
                first_page: u32::MAX,
                section_starts: Vec::new(),
            })
            .recv_blocking()
            .unwrap();
        Model {
            view: opened.view,
            rows: opened.rows,
            version: opened.version,
        }
    })
    .collect();
    let mut batches = Vec::new();
    let mut seen = (0, 0, 0);
    // Reminders woken, and steps with a conversation sorting by when it came back.
    let (mut reminded, mut bumped) = (0, 0);
    // The Reminders folders with a view of their own.
    let mut watched: Vec<FolderId> = Vec::new();
    for step in 0..150 {
        let pick = |rng: &mut Rng, owner: Owner, count: u64| -> Vec<MessageId> {
            let of: Vec<MessageId> = known
                .iter()
                .filter(|(_, o, _)| *o == owner)
                .map(|(.., id)| *id)
                .collect();
            (0..1 + rng.below(count))
                .map(|_| of[rng.below(of.len() as u64) as usize])
                .collect()
        };
        let owner =
            [Owner::Local, Owner::Local, Owner::Other, Owner::Server][rng.below(4) as usize];
        let version = match rng.below(12) {
            _ if step % 50 == 49 => {
                let batch = fresh(&mut rng, &mut splits, &known, 150);
                store_batch(&store, &mut known, batch)
            }
            0..=3 => {
                let count = 1 + rng.below(3) as usize;
                let batch = fresh(&mut rng, &mut splits, &known, count);
                store_batch(&store, &mut known, batch)
            }
            4..=6 => {
                let picks = pick(&mut rng, owner, 4);
                let (add, remove) = match rng.below(5) {
                    0 => (Flags::SEEN, Flags::NONE),
                    1 => (Flags::NONE, Flags::SEEN),
                    2 => (Flags::FLAGGED, Flags::NONE),
                    3 => (Flags::DRAFT, Flags::NONE),
                    _ => (Flags::NONE, Flags::FLAGGED | Flags::SEEN | Flags::DRAFT),
                };
                store.set_flags(picks, add, remove).recv_blocking().unwrap()
            }
            7 => {
                let picks = pick(&mut rng, owner, 2);
                store.remove(picks).recv_blocking().unwrap().version
            }
            8 | 9 => {
                let owner = if rng.below(2) == 0 {
                    Owner::Local
                } else {
                    Owner::Server
                };
                let picks = pick(&mut rng, owner, 3);
                let action = match (owner, rng.below(5)) {
                    (_, 0) => Triage::Archive,
                    (_, 1) => Triage::Delete,
                    (_, 2) => Triage::Read(rng.below(2) == 0),
                    (_, 3) => Triage::Star(rng.below(2) == 0),
                    (Owner::Server, _) => Triage::MoveTo(server_inbox),
                    _ => Triage::MoveTo([inbox, projects, sent][rng.below(3) as usize]),
                };
                let queued = store
                    .queue_triage(QueueTriage {
                        action,
                        messages: picks,
                        not_before: 0,
                    })
                    .recv_blocking()
                    .unwrap();
                batches.extend(queued.batch);
                queued.version
            }
            10 if !batches.is_empty() => {
                let batch = batches.swap_remove(rng.below(batches.len() as u64) as usize);
                store.undo(batch).recv_blocking().unwrap().version
            }
            _ => {
                // Projects becomes a Trash folder, or an ordinary one again.
                let role = if rng.below(2) == 0 {
                    FolderRole::Trash
                } else {
                    FolderRole::Other
                };
                store
                    .upsert_folder(FolderSpec::local(local, "Projects", role))
                    .recv_blocking()
                    .unwrap()
                    .version
            }
        };
        let mut catch_up = |version: StoreVersion| {
            while models[0].version < version {
                let event = events.recv_blocking().unwrap();
                for model in &mut models {
                    let (splice, moved, reload) = model.apply(&store, &event);
                    seen.0 += usize::from(splice);
                    seen.1 += usize::from(moved);
                    seen.2 += usize::from(reload);
                }
                for model in &mut messages {
                    model.apply(&store, &event);
                }
            }
        };
        catch_up(version);
        // Now and then the user chooses a side for a sender, or Gmail tells categories.
        let version = match splits.below(6) {
            0 => {
                let (_, address) = senders[splits.below(senders.len() as u64) as usize];
                let choice =
                    [None, Some(Split::Important), Some(Split::Other)][splits.below(3) as usize];
                store
                    .set_sender_split(address.to_uppercase(), choice)
                    .recv_blocking()
                    .unwrap()
                    .max(version)
            }
            1 => {
                let categories = (0..1 + splits.below(4))
                    .map(|_| {
                        let (.., id) = known[splits.below(known.len() as u64) as usize];
                        (id, Split::of(splits.below(2) == 0))
                    })
                    .collect();
                store
                    .set_categories(categories)
                    .recv_blocking()
                    .unwrap()
                    .max(version)
            }
            _ => version,
        };
        catch_up(version);
        // Now and then a conversation is sent to Reminders (Undo may take it back), the clock
        // moves on through the messages' dates and due ones come back, or one is cancelled.
        let clock = 10_000 + 4 * step as i64;
        let version = match reminds.below(5) {
            0 | 1 => {
                let (.., id) = known[reminds.below(known.len() as u64) as usize];
                let scope = [
                    ViewScope::UnifiedInbox,
                    ViewScope::Folder(inbox),
                    ViewScope::Folder(server_inbox),
                    ViewScope::Folder(archive),
                ][reminds.below(4) as usize];
                // Now and then Someday, which no wake brings back (phase C).
                let wake_at = match reminds.below(12) {
                    0 => SOMEDAY,
                    _ => clock + reminds.below(40) as i64,
                };
                match store.message(id).recv_blocking().unwrap().value {
                    Some(detail) => {
                        let reminded = store
                            .remind(Remind {
                                threads: vec![detail.summary.thread],
                                scope,
                                wake_at,
                                not_before: 0,
                            })
                            .recv_blocking()
                            .unwrap();
                        batches.extend(reminded.batch);
                        reminded.version.max(version)
                    }
                    None => version,
                }
            }
            2 => {
                let woken = store.wake_reminders(clock).recv_blocking().unwrap();
                reminded += woken.woken.len();
                woken.version.max(version)
            }
            3 if reminds.below(3) == 0 => {
                let (.., id) = known[reminds.below(known.len() as u64) as usize];
                match store.message(id).recv_blocking().unwrap().value {
                    Some(detail) => {
                        let cancelled = store
                            .cancel_reminders(vec![detail.summary.thread], 0)
                            .recv_blocking()
                            .unwrap();
                        batches.extend(cancelled.batch);
                        cancelled.version.max(version)
                    }
                    None => version,
                }
            }
            _ => version,
        };
        catch_up(version);
        let world = world(&path);
        // Whether a conversation back from Reminders sorts above its mail in an Inbox now.
        bumped += usize::from(world.bumps.iter().any(|(thread, at)| {
            let shown = || {
                world
                    .messages
                    .iter()
                    .filter(move |m| m.thread == *thread && m.flags & 264 == 0)
            };
            shown().any(|m| world.roles[&m.folder] == FolderRole::Inbox)
                && shown().all(|m| m.date < *at)
        }));
        for model in &models {
            let spec = model.view.spec();
            let page = whole(&store, spec);
            assert_eq!(page.version, model.version, "step {step}");
            assert_eq!(
                page.conversations, model.rows,
                "seed {seed}, step {step}, {spec:?}"
            );
            let folders = match spec.scope {
                ViewScope::Folder(folder) => vec![folder],
                ViewScope::UnifiedInbox => world.inboxes.clone(),
            };
            assert_eq!(
                page.conversations,
                expected(&world, &folders, spec),
                "seed {seed}, step {step}, {spec:?} against the recount"
            );
        }
        for model in &messages {
            let spec = model.view.spec();
            let page = whole(&store, spec).rows;
            assert_eq!(page, model.rows, "seed {seed}, step {step}, {spec:?}");
            if spec.side().is_some() {
                let folders = match spec.scope {
                    ViewScope::Folder(folder) => vec![folder],
                    ViewScope::UnifiedInbox => world.inboxes.clone(),
                };
                let ids: Vec<MessageId> = page.iter().map(|row| row.id).collect();
                assert_eq!(
                    ids,
                    expected_messages(&world, &folders, spec),
                    "seed {seed}, step {step}, {spec:?} against the recount"
                );
            }
        }
        // Each Reminders folder gets a conversation view once it is made: its waiting
        // conversations sort by when they are due (phase C).
        for folder in store.folders().recv_blocking().unwrap().value {
            if folder.name == sealwax_domain::remind::FOLDER && !watched.contains(&folder.id) {
                watched.push(folder.id);
                models.push(Conversations::open(
                    &store,
                    ViewSpec::folder(folder.id).conversations(),
                ));
            }
        }
        if step % 25 == 0 {
            clean(&store);
        }
    }
    assert!(!watched.is_empty(), "seed {seed}: no Reminders folder made");
    assert!(
        seen.0 > 150 && seen.1 > 5 && seen.2 > 0,
        "seed {seed}: {} splices, {} moves, {} reloads",
        seen.0,
        seen.1,
        seen.2
    );
    assert!(
        reminded > 3 && bumped > 10,
        "seed {seed}: {reminded} reminders woken, {bumped} steps with one on top"
    );
    clean(&store);
}

#[test]
fn chained_merges_point_at_the_conversation_they_end_in() {
    let merge = |from, into| ThreadMerge {
        from: ThreadId(from),
        into: ThreadId(into),
    };
    // 28 into 15, then 15 into 14; 9 into 3 stays; a later 3 doesn't follow an earlier merge.
    let mut merges = vec![merge(9, 3), merge(28, 15), merge(15, 14), merge(14, 2)];
    super::writer::final_merges(&mut merges);
    assert_eq!(
        merges,
        [merge(9, 3), merge(28, 2), merge(15, 2), merge(14, 2)]
    );
    let mut reused = vec![merge(3, 7), merge(5, 3), merge(3, 8)];
    super::writer::final_merges(&mut reused);
    assert_eq!(reused, [merge(3, 7), merge(5, 8), merge(3, 8)]);
}
