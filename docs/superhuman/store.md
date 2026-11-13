# Store: conversation views (Superhuman pass)

This fork lists **one row per conversation**, which reverses ADR-016's one-row-per-message list.
The store now offers both kinds of view. A `ViewSpec` has a `grouping`. `Messages` is the default,
so every existing caller, test and self-test behaves as before. `Conversations` gives
`ConversationSummary` rows with exact change events. This page is the API contract for the list
engineer, followed by the decisions taken and the measurements.

## Schema: the fork migration track

- Upstream's `user_version`/`MIGRATIONS` are untouched. Before this change the store was at v12.
  The fork's own steps are in `infrastructure/src/store/fork.rs`. They run after `migrate()` on
  the writer connection and are counted in `meta('fork_schema')`, which is 3 now (step 2 is the
  Split Inbox: docs/superhuman/split.md; step 3 Remind Me: docs/superhuman/remind.md). Each
  step is idempotent (`IF NOT EXISTS`). Readers still check only `user_version`.
- A store with a newer fork schema is refused with `StoreError::NewerForkSchema` and left
  untouched, the same way upstream refuses a newer schema.
- `meta('fork_through')` records the store version at which the table was last exact. Every
  publishing commit sets it. Two things trigger a full recount at open: a store version that
  differs from it (an older build without the fork wrote to the store since), or an upstream
  migration that ran during this open. Switching between builds is therefore safe.
- `fork_thread_folders` has one row per **(conversation, folder)** where the conversation has a
  shown message in that folder (`flags & 264 = 0`). It is `WITHOUT ROWID`, its key is
  `(thread_id, folder_id)`, and it has no foreign keys (see below).
  - Per folder: `messages`, `unread`, `flagged` and `attachments` (counts of the members there),
    `last_date`/`last_id` (the newest member there), `unread_date`/`unread_id` (the oldest unread
    member there), and `bin` (the folder is a Trash, Junk or Drafts folder).
  - Per conversation, repeated on each of its rows:
    - `sort_date`/`sort_id`, the row's sort key: the conversation's **latest message**, or, in
      a `bin` folder, the row's own newest member.
    - `shown`: its message count.
    - `drafts`: its drafts.
  - Indexes: `fork_thread_folders_list(folder_id, sort_date, sort_id)`, plus partial indexes of
    the same shape `WHERE unread > 0`, `flagged > 0` and `attachments > 0`. They mirror the
    message views' partial indexes.
- `fork_folder_counts(folder_id, conversations, unread, flagged, attachments)` counts each
  folder's rows, and those with unread, starred and attachment members. A view's total is read
  from it in constant time.
- Size at 100,000 messages: 58,306 rows for 41,336 conversations, about 5.3 MiB with the indexes
  (205.7 → 210.8 MiB store).

## Keeping it exact

- TEMP triggers on the writer's connection (`fork.rs` `TEMP`) record every thread whose messages
  change: inserts, deletes (cascades from a folder or an account included), moves (rehome),
  thread merges, date, attachments, snippet, subject or sender changes, flag changes, and a
  folder's role changing. No write path needs to know about them.
  - Flag changes that only touch read/starred are recorded with their folder. Changes that hide
    a row, show it again, or set or clear `\Draft` count as *content*, because they can change
    the participants without changing any count.
- Before the transaction commits, `writer/conversations.rs::settle` recounts only the dirty
  threads from `messages`. It reads their shown rows with one `messages_thread` read and does
  the rest in Rust (`fork::recount_dirty`). It then adjusts `fork_folder_counts` and diffs the
  open conversation views.
  - Every path that writes messages goes through it: `view_changes` (ingest, sync, flags,
    removal, ops, bodies, POP3, compose), and the reload paths (folder upsert or role change,
    account removal or reconcile, folder deletion).
  - `State::commit` has a safety net: it recounts anything still dirty. In debug builds that
    asserts, and no existing test ever hits it.
- `check` recounts the table and the folder counts with an independent SQL formulation (window
  functions over every message, `fork::RECOUNT_ALL`). The migration's backfill uses the same SQL.
  Every store test that asserts `check()` is clean therefore verifies the incremental path.
  - Mutation check: with the delete trigger removed, five tests in `store::tests` fail on
    "conversation rows disagree".

## API (`sealwax_infrastructure::store`, `sealwax_domain::model`)

### Opening and paging

```rust
let spec = ViewSpec::folder(inbox).conversations();          // or .with_grouping(ViewGrouping::Conversations)
let spec = ViewSpec::unified_inbox().with_filter(ViewFilter::Unread).conversations();
let mut opened = store.open_view(OpenView { spec, first_page: 100, section_starts }).recv().await?;
// opened.total, opened.section_counts: conversations (newest first)
// opened.conversations: Vec<ConversationSummary>; opened.rows is empty for a conversation view
let page = store.page(spec, offset, 100).recv().await?;     // page.conversations
let rows: Vec<ListRow> = page.into_rows();                   // or opened.take_rows(): either grouping
```

- **Order:** newest first by `(date, latest)` descending. This is the row's sort key, and
  `date` places a row in its date section, exactly as `MessageSummary::date` does.
- **Membership:** a conversation is in a view when at least one of its shown messages is in the
  view's folders. All Inboxes covers the Inbox-role folders of attached accounts.
- **Filters** (All/Unread/Starred/Attachments) keep a conversation when one of its members in the
  view's folders passes the filter. The Split Inbox's Important and Other filters work the same
  way (Other: no member there is Important), in docs/superhuman/split.md.
- **Totals** (`total`, section counts) count conversations. The sidebar's folder counts remain
  message counts.
- `ListRow { Message(MessageSummary), Conversation(ConversationSummary) }`:
  - `key()` returns a `RowKey::Message(id)` or `RowKey::Conversation(thread)`. This is the row's
    identity across events, so use it to keep the selection.
  - `date()` and `message()` are also available. `message()` is the message itself, or a
    conversation's `anchor`.

### `ConversationSummary` (domain `model.rs`)

| field | meaning |
|---|---|
| `thread`, `account` | the conversation and its account |
| `anchor`, `folder` | where the reader opens: the **oldest unread member in the view's folders**, else the newest member there, and its folder |
| `last_member` | the newest member in the view's folders (target of Mark unread and Star) |
| `latest`, `date`, `sender_name`, `sender_email`, `snippet` | the **latest message**: the newest shown message outside Trash, Junk and drafts, from any folder (a reply in Sent counts, so it moves the conversation up and shows "me"). In a view of a Trash, Junk or Drafts folder alone, it is the newest member there instead |
| `subject` | the conversation's subject without Re:/Fwd:/SV:/AW: prefixes (`threads.subject`, the earliest message's `base_subject`) |
| `participants: Vec<Participant>` | distinct senders in the order they first wrote, at most 4: the first one and the 3 who wrote last (phase C; it was the 3 who joined last, which could leave out the newest writer). `Participant { name, email, is_me, unread, last }`. `name` is the newest non-empty name. `is_me` means the account's address, or the sender of a copy in a Sent or Drafts folder (this covers identities the store doesn't know). `unread` means the sender has an unread member in the view's folders. `last` is the date of the sender's newest copy that counts (Unix seconds) |
| `participant_count` | distinct senders in all |
| `messages` | shown messages outside Trash, Junk and drafts, **each Message-ID once**. When the conversation has none there, all its shown messages count |
| `members` | its shown messages in the view's folders: what triage acts on |
| `unread`, `flagged`, `attachments` | members in the view's folders that are unread, starred, or have attachments (counts; `> 0` gives the dot, the star and the clip) |
| `has_draft` | a shown message in a Drafts folder, or one flagged `\Draft` |

### Change events (the diff contract)

Each commit's `ChangeEvent.views` holds at most one entry per open view. For a conversation view
it is one of:

- `ViewDiff::ConversationSplice { removed, inserted, changed, total }`. Apply it like `Splice`:
  1. Apply `removed` one by one. These are positions **before** the commit, in descending order.
  2. Apply `inserted`, a `Vec<Positioned<ConversationSummary>>` of positions **after** the
     commit, in ascending order.
  3. Replace each `changed` row in place (positions after the commit). The row at that position
     is the same `thread`.
  4. Check the result against `total`.
- `ViewDiff::Reload { total }`: open the view again. A view gets this past 64 changed rows
  (`MAX_SPLICE`; a move counts twice), and when its folder list or the user's addresses may have
  changed (folder upsert, account changes, folder deletion).

What each kind of change produces:

- A conversation that leaves the view is removed. One that enters is inserted.
- A conversation whose sort key changed (a reply bumps it) **moves**: it is removed at its old
  position and inserted at its new one, as the same `thread`.
  - To keep the selection without reloading the reader, re-select by `RowKey::Conversation(thread)`
    after the splice.
  - A lone move that keeps its place among the rows that stay comes as a `changed` row instead
    (a reply to the conversation already at the top, for example).
- A conversation that stays is `changed` when its rows in the view's folders changed, when
  anything but read/starred flags changed in it, or when a member's flags in the view's folders
  changed.
- Merges are diffed directly. The merged-away conversation is removed, and the target is
  changed, moved or inserted. `ChangeEvent.merged_threads` and `threads` are for message rows
  only, so skip `apply_threads` for conversation rows. The one use of `merged_threads` for a
  conversation view is to map a selected `from` thread to `into`.
- `ViewDiff::into_splice()` turns either kind of splice into one `Splice { removed, inserted:
  Vec<Positioned<ListRow>>, changed, total }`, or `Err(total)` for a reload.

### Triage: member expansion

```rust
let members = store.conversation_members(vec![row.thread], spec).recv().await?.value;
// Vec<ConversationMember { thread, id, folder, flags, date, has_attachments }>, the shown members
// in the folders of spec.scope that pass spec.filter, each conversation's oldest first
```

The brief's rules, built on this:

| action | targets |
|---|---|
| Archive, Delete, Junk, Move | every member, using `spec` with `ViewFilter::All` |
| Mark read | the members with `flags` unseen, or `with_filter(ViewFilter::Unread)` |
| Mark unread, Star | `row.last_member` (no read needed) |
| Unstar | `with_filter(ViewFilter::Starred)` |

Queue them with one `QueueTriage`, which makes one transaction and one Undo batch.

### Conversation search

```rust
let hits = store.search_conversations(text, folder, limit).recv().await?.value;
// Vec<ConversationHit { matched, matched_date, conversation: ConversationSummary }>
```

- It uses the same query language and folders as `search`: one folder, or every folder except
  Trash and Junk.
- Results are the newest `limit` conversations, ordered by their **newest match**, newest first.
  `matched` is that message, so open the reader at it.
- The rows describe the conversation as a view of the searched folders shows it.
- It reads 4 × `limit` matches at a time (at least 50, at most 20,000) until it has enough
  conversations.

## Decisions (and why)

- **Drafts stand aside** together with Trash and Junk. A copy in a Drafts folder, or one flagged
  `\Draft`, does not move a conversation, give it its snippet, or count in `messages`.
  `has_draft` marks it instead. Otherwise an autosaved reply would jump its conversation to the
  top while the user writes. The brief named only Trash and Junk, so this is a deliberate
  refinement.
- In a view of a Trash, Junk or Drafts folder alone, rows sort by and show their own newest
  member there. Without that, a draft replying to a two-year-old thread would sit two years deep
  in Drafts.
- Copies of one Message-ID count once in `messages`, while `members` counts copies (the rows
  triage acts on). When two copies share a date, the higher id is the "latest", which may be the
  copy in another folder: it is the same message.
- An account with two Inbox-role folders: All Inboxes lists each conversation once ("fold"
  walks with `count(DISTINCT)`). Otherwise the walks merge per-folder indexes.
- No foreign keys on `fork_thread_folders`. A merged or emptied thread keeps its rows until the
  recount in the same transaction, so the diff still knows where the conversation was.
- Chained merges in one batch (A into B, then B into C) are reported as A into C and B into C
  (`writer::final_merges`). The app's `apply_threads` and the test model follow one hop, so
  before this change message rows were left pointing at a deleted thread. The replay found it.

## Measured (100,000-message fixture, release, tmpfs)

These numbers come from `sealwax-fixtures import --count 100000 --seed 1 --folders spread`,
then `bench-threads`. The machine had a load average of 13–25 from other builds, so p95 and p99
are noisy.

| measure | result | bar |
|---|---|---|
| fork migration, one-time recount at first open (create, recount, index) | p50 0.89 s | |
| store open after it | p50 2.7 ms | |
| conversation view open (count + 4 sections + first 100), INBOX 25,301 | cold p50 1.75 ms, warm p50 1.22 ms | ≤ 100 ms |
| conversation view open, unified inbox | cold p50 1.38 ms, warm p50 1.09 ms | ≤ 100 ms |
| conversation view open, INBOX unread | warm p50 1.30 ms | ≤ 100 ms |
| conversation view open, INBOX starred | warm p50 2.12 ms | ≤ 100 ms |
| page of 100 conversations, INBOX, any offset | p50 1.0–1.3 ms, p95 ≤ 3.0 ms | ≤ 5 ms |
| page of 100 conversations, unified inbox, any offset | p50 1.15–1.6 ms | ≤ 5 ms |
| one new message, 3 open conversation views (writer transaction incl. recount and diffs) | p50 0.92 / p95 3.5 / p99 4.6 / max 6.1 ms; "old, deep" p50 2.4 ms | ≤ 20 ms |
| one new message, no view open (triggers + recount) | p50 0.36 ms | |
| mark read → event, 3 conversation views | p50 1.8 ms | |
| `check` at 100k (all checks) | 3.3 s | |

Write overhead against the pristine build, on the same store:

- Upstream `bench`, one new message with 3 open *message* views:
  - writer transaction: p50 0.24 → 0.36 ms, p95 1.10 → 1.16 ms
  - mark read → event: p50 0.58 → 0.66 ms
- Bulk ingest (30k import, four interleaved runs): the fastest header-transaction time was
  2,393 ms pristine against 2,478 ms here, about +3.5%, or about +0.6 ms per 200-message batch.

A first version recounted dirty threads with the window-function SQL. It cost about 6 ms per
200-message batch (+50% header time), and moving that recount to Rust fixed it.

## Tests

`infrastructure/src/store/conversation_view_tests.rs`:

- Rows, order and the move or in-place change when a reply in Sent arrives.
- Filters by members.
- Trash, Junk and drafts standing aside.
- All Inboxes paging at any offset.
- Member expansion feeding one Archive batch.
- Conversation search, including a newer draft.
- The fork track: backfilling a pre-fork store, the recount after an older build wrote, and
  refusing a newer fork schema.
- `conversation_events_keep_list_models_exact`, the twin of upstream's replay. It runs 3 seeds
  × 150 random steps:
  - Ingests with replies, copies, multi-parent merges, drafts and the user's own mail.
  - Flag and `\Draft` toggles and removals.
  - Moves done at once (a local account) and pending moves (a server account, later undone).
  - A folder's role flipping between Other and Trash.
  - 13 conversation views (two Inbox folders of one account fold in All Inboxes) beside 2
    message views.
  - After every step, each model equals a fresh page, and each page equals an independent Rust
    recount from the raw `messages` table.
  - 60 extra seeds (100–159) passed in release on the final code.
- `conversations::tests` covers participants: order, names, the user, the cap, and (phase C)
  the cap keeping the latest writers with when they last wrote.
- `chained_merges_point_at_the_conversation_they_end_in` covers `final_merges`.
- Unchanged and passing:
  - every existing store test, including upstream's replay, the `check()` assertions and the
    migration fixtures;
  - `cargo test --workspace`;
  - these self-tests, headless (message rows, the app's default for now): `list-insert`,
    `list-churn`, `list-chips`, `list-select`, `triage-actions`, `triage-undo`.
- `engine/tests/pop3_accounts.rs` sometimes times out waiting for `OlderLoaded` after a detach.
  It also failed 4 of 40 runs on the pristine base, so it is an engine race that predates this
  change.

## Left for the app (list engineer)

- Hold `ListRow`s, key slots by `RowKey`, and apply `ConversationSplice` with the same run
  helpers.
- Re-select a moved conversation by key.
- Expand triage targets with `conversation_members`. Use `ConversationSummary::folder` and the
  counts for synchronous availability.
- Group search with `search_conversations`.
- Default the list to `ViewGrouping::Conversations`, keeping a self-test switch for message rows.

## Phase C (track L step 2)

- `Conversation::subject` (new field): the conversation's subject as its list row says it,
  `threads.subject`, read in the conversation's own snapshot (`reader/conversation.rs`, a join on
  the message's thread). The reader heads the conversation with it whichever message it was
  read from (the heading took the selected, first unread, message's subject). `assemble` leaves
  it empty; `conversation_tests::a_conversation_has_its_rows_subject_whatever_message_is_read`.
- `Store::draft_copies(drafts) -> Vec<(DraftId, MessageId)>` (new read, the review's second
  round): the copies of local drafts in a folder with the Drafts role, by Message-ID and account,
  the reverse of `draft_for_message`; one `messages_message_id` lookup a draft. The outbox reads
  the scheduled sends' copies with it, so their list rows say when they go (writing.md).
  `compose_tests` (the synced copies of a draft; none for a draft never uploaded).
