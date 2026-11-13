# Superhuman pass: Split Inbox backend

*2026-10-29, phase B track C, part 1.* Superhuman's Split Inbox puts mail from people on one side
(**Important**) and everything else (newsletters, notifications, receipts) on the other
(**Other**). This is its backend: the rules (`domain/src/split.rs`), the store's classification,
views, counts and diffs (`infrastructure/src/store/fork.rs`, `conversations.rs`, `views.rs`,
`writer/split.rs`, `reader/split.rs`), and the engine's header fields and Gmail categories
(`engine/src/imap/fetch.rs`, `sync/view.rs`, `sync/folder.rs`, `first_sync.rs`). The tabs are
phase C's; the app only got what it needs to compile, plus self-test hooks.

## The rules (`split::classify`)

Each message, in this order:

1. **The user's choice for its sender** ("Always Important" or "Always Other"): the exact From
   address, trimmed and in ASCII lower case (`split::sender_key`, as SQLite's `lower`). It wins
   over everything, the next rule included.
2. **Important in a conversation the user sent in**: its thread has a shown message in a Sent
   folder that isn't a draft.
3. **The server's category, where it gave one**: Gmail's Primary tab is Important, any other tab
   Other.
4. **The header rules**: Other with any header fact, else Important. The facts
   (`HeaderFacts`, kept as bits):
   - `LIST_ID`: a List-Id field (RFC 2919).
   - `LIST_UNSUBSCRIBE`: a non-empty List-Unsubscribe field (RFC 2369, 8058).
   - `BULK`: Precedence `bulk`, `list` or `junk` (its first word, any case).
   - `AUTO_SUBMITTED`: Auto-Submitted with any value but `no` (RFC 3834).
   - `ROBOT`: the From address reads as a program's (`split::is_robot`): its local part, plus
     addressing dropped, holds `noreply`, `donotreply`, `dontreply` or `noresponse` once its
     separators are gone, or one of its words is a notification, alert, mailer, bounce,
     newsletter, digest, receipt or invoice word (`notifications@`, `calendar-notification@`,
     `mailer-daemon@`, `receipts+acct@`, `auto-confirm@`). The list is short on purpose:
     `info@`, `support@`, `hello@` and `team@` are people often enough, and a person's mail in
     Other costs more than a robot's in Important.

A **conversation** is Important in a view when any of its members in the view's folders is, and
Other when it has members there and none is. Important and Other share a view's rows out between
them.

Decisions:
- The choice beats "the user sent in it": the user who once answered a notification thread and
  then says "Always Other" for that sender means it.
- "Sent in" is a copy in a Sent folder, not the From address: a forged From can't make spam
  Important, and every send lands in Sent (the engine looks in Sent right after a send on Gmail).
  A conversation the user wrote in long ago is Important only while its Sent copy is stored
  (Sent's window: 90 days, at least its newest 200).
- Gmail's category wins over the header rules both ways: a newsletter Gmail puts in Primary is
  Important, a plain message it files under Updates is Other.
- The facts are presence bits only. The List-Unsubscribe URL isn't kept; a one-key unsubscribe
  would fetch the field on demand.

Realistic-header unit tests: a person writing from a company domain, newsletters, GitHub and
Jira notifications, mailing lists (and a thread the user wrote in), Google Calendar invitations
(the organizer is the From: Important) and reminders (Other), receipts and no-reply senders,
out-of-office replies, odd `Precedence` and `Auto-Submitted: no`.

## Store

### Schema: fork step 2 (`fork.rs`, `meta('fork_schema')` = 2)

- `fork_message_split`: one row per message, keyed by its id.
  - Kept from its ingest: `facts` (HeaderFacts bits), `category` (NULL where the server gave
    none, 1 Important, 0 Other), `sender` (the From address as `sender_key` keeps it).
  - Where the last commit published it: `thread_id`, `folder_id`, `date`, and `split` (1
    Important, 2 Other, 0 while hidden by a pending op or `\Deleted`).
  - Indexes: only the message views' walks, `(folder_id, date, id)` partial on `split = 1` and
    on `split = 2`. A thread or sender index would cost every ingest a write per message; the
    recount reaches split rows by message id.
- `fork_folder_split(folder_id, important, other)`: each folder's Important and Other messages.
- `fork_sender_split(address, important, added)`: the user's choices (the user's, not an
  account's, like `remote_senders`).
- `fork_thread_folders` gains `important` (the row's Important members) and two partial indexes
  (`important > 0`, `important = 0`); `fork_folder_counts` gains `important` and `other`
  (conversations with and without one). The step drops and makes both again (they are derived),
  so it is idempotent.
- A step now leaves `fork_through` cleared in its own transaction, so the recount that fills the
  tables runs at that open, or at the next one if Sealwax stops between them.

### Keeping it exact

- `insert_message` notes a new message's facts, category and sender in the TEMP
  `fork_new_facts`; the delete trigger notes deleted ids in TEMP `fork_gone`. The recount of
  the dirty threads (`recount_dirty`, before the commit, as for the conversation table) drops the
  deleted messages' split rows, then classifies every message of each dirty thread again and
  writes the split rows that changed. Until then the table holds what the last commit published:
  the "before" of the diffs.
- The thread-level rule is why classification lives in the recount: a reply stored in Sent
  dirties its thread, and every member flips at once. A category or a choice dirties the
  threads concerned (`fork::touch`, no content change) and goes through the same path.
- **Row ids come back.** SQLite reuses the newest row's id after it is deleted, so a sync commit
  that expunges the newest message and stores a new one gives the new message the old id in the
  same transaction. The deleted rows' split rows go first, and a noted new message always takes
  its own facts. (The ops property test found this through `check`; a store test pins it.)
- `check()` recounts every split row with an independent SQL formulation (`SPLIT_ALL`), checks
  every message has one and names its own sender, recounts the folders' split counts, and
  recounts `fork_thread_folders.important` through `RECOUNT_ALL`.

### Older messages (the backfill)

The step gives every stored message a split row in one pass (`split_rows`). Before this build the
store kept only a message's **List-Id and From address** of the fields the rules read, so an older
message gets `HeaderFacts::stored`: `LIST_ID` and `ROBOT` where they apply, never
`LIST_UNSUBSCRIBE`, `BULK` or `AUTO_SUBMITTED`, and no category. So an older newsletter with a
List-Id or a no-reply sender is Other, while one with only List-Unsubscribe stays Important until
the user chooses (or, in a Gmail INBOX, until the engine catches up its category, below). The
user-sent rule and choices apply to them as to any message. New mail always comes with all five
facts.

The same pass runs whenever the store is recounted at open (an upstream migration ran, or an
older build wrote since): a split row that no longer names its message's sender and date (an
older build reused its id) takes the stored facts too.

### API

```rust
ViewSpec::folder(inbox).with_filter(ViewFilter::Important)                  // message rows
ViewSpec::unified_inbox().with_filter(ViewFilter::Other).conversations()    // conversation rows
store.split_counts(spec)          // Versioned<SplitCounts { important, other }> in spec's grouping
store.set_sender_split(address, Some(Split::Other))   // or None to forget; -> StoreVersion
store.sender_splits()             // Vec<(String, Split)>, by address
store.set_categories(vec![(id, Split::Important)])    // the engine's, after headers were stored
store.uncategorized(folder, limit)                    // Vec<Uncategorized { message, uid }>
```

- `ViewFilter::Important` and `Other` are filters like Unread: one at a time. `ViewFilter::split()`
  and `of_split()` map them to `Split`. Combining a side with Unread would need a split
  dimension on `ViewSpec` (and its partial indexes): phase C's call.
- `NewMessage::category: Option<Split>` carries the server's category into an ingest;
  `Headers::facts` carries the header facts (the parser fills them).
- Totals come from the maintained counts (`fork_folder_split`, `fork_folder_counts`): constant
  time. Pages walk the partial indexes: O(page) at any offset (measured below).
- A choice or a category re-classifies in **one transaction** whose change event carries every
  open view's diff; the same choice again changes nothing (no event).
- `MessageDetail.headers.facts` reads the kept facts.
- `conversation_members` with a split filter gives the members on that side; triage keeps
  store.md's recipes (`All` for Archive and Move, `Unread` for Mark read), which act on a
  conversation's members whatever their side.

### Change events

- Conversation views of a split filter are diffed as before (`writer/conversations.rs`); a
  conversation enters or leaves when its rows' `important` counts move it across. A row whose
  only change is which members are Important is not `changed` in any view (`ForkRow::shown`):
  it shows nothing of it.
- **Message views of a split filter** (`writer/split.rs`) can't be diffed from the rows a
  transaction touched: a reply in Sent moves messages nobody touched. They are diffed like the
  conversation views, from the dirty threads' split rows before and after the recount: a message
  that enters is inserted, one that leaves is removed (its position before the commit is its rank
  now, less the dirty messages above it now, plus those above it before), and a message the
  transaction touched that stays is changed. Past 64 rows, a reload.
- The upstream message views (All, Unread, Starred, Attachments) keep their row diffs.

## Engine

- The header FETCH asks for three more fields, `LIST-UNSUBSCRIBE PRECEDENCE AUTO-SUBMITTED`
  (`fetch::FIELDS`; LIST-ID was there). The parser turns them into facts; nothing else changes
  on the wire.
- **Gmail's categories** (X-GM-EXT-1, the INBOX only): whenever a pass fetches INBOX rows (first
  sync batches, new mail, UID diffs, older mail) it asks
  `UID SEARCH UID <the fetched UIDs> X-GM-RAW "category:primary"` and stores the rows with their
  category in the same commit, so new mail lands on its side at once (no Important-then-Other
  flicker, and a notification sees the final side). One plain SEARCH per fetch batch, only when
  rows were fetched; it runs while the rows are parsed (`fetch_rows`) or while the previous batch
  commits (first sync).
- A refused search (NO or BAD) is logged without server text, the rows keep to the header rules,
  and that connection stops asking (`Connection::categories.refused`).
- **Catch-up**: after its first INBOX pass, each connection asks once about the INBOX's stored
  messages without a category (`Store::uncategorized`, newest first, at most 5,000, 500 UIDs a
  SEARCH) and stores the answers with `set_categories`. This covers mail from before this build
  (the owner's INBOX on the first start) and mail stored while the search was refused.
- Not done: a message moved between Gmail tabs after it arrived keeps its first category (Gmail
  says nothing over IMAP when that happens); the user's choice for its sender is the remedy.

## Self-test hooks (`app/src/selftest/split.rs`)

| Command | What it does |
| --- | --- |
| `split important\|other\|all` | Filters the list to one side (or all of it) |
| `mail-sender-split ADDRESS important\|other\|none` | Chooses a side for a sender through the store, or forgets it |
| `assert-split-counts IMPORTANT OTHER` | The rows each side of the list's view holds, in its grouping |
| `assert-split-choice ADDRESS important\|other\|none` | The choice the store keeps |

`tests/selftest/split-backend.sealwax-test` (34 steps) uses them on message rows: a List-Id and a
receipts robot in Other, people in Important, a choice moving a sender over and back.

App glue: the two exhaustive `ViewFilter` matches (`list::model::passes` keeps every row for a
split filter, the store decides; the empty state's text).

## Measured (100,000-message fixture, release, load average 10–26 from other builds)

`sealwax-fixtures import --count 100000 --seed 1 --folders spread`, then
`bench-split --cold 5 --warm 30 --inserts 200` on a copy (new, `devtools/fixtures/src/split.rs`;
it marks two in five messages bulk first: 79,563 Important, 20,437 Other, since the fixture's
threads with Sent replies stay Important).

| measure | result | bar |
| --- | --- | --- |
| fork step 2 at open (split rows from what the store kept, recount), cache evicted | p50 2.5 s (2.45–2.89 s over three rounds) | |
| … of which the conversation recount (`RECOUNT_ALL`, sqlite3 CLI) | 1.03 s, against 1.00 s without the split join | |
| store open after it | p50 5 ms | |
| recount at open after an older build wrote (warm) | 1.8–2.0 s | |
| split rows and their two indexes | 8.5 MiB (12.2 MiB with thread and sender indexes, dropped) | |
| view open, warm (count + 4 sections + first 100): message views | p50 0.24–0.34 ms | ≤ 100 ms |
| view open, warm: conversation views (INBOX, unified) | p50 0.70–1.32 ms | ≤ 100 ms |
| view open, cold | 5–22 ms | ≤ 100 ms |
| page of 100, message views, any offset | p50 0.20–0.81 ms | ≤ 5 ms |
| page of 100, conversation views, any offset | p50 0.75–3.75 ms | ≤ 5 ms |
| one new message, 3 split views open (writer transaction incl. recount and diffs) | p50 1.24 / p95 3.40 / p99 5.70 ms | ≤ 20 ms |
| a choice for the fixture's busiest sender (21,043 messages) → change event | p50 885 ms (about 42 µs per message classified again) | |
| `check` | 2.5 s | |

Against the phase A build (its fixtures tool built from the base tree; the same commands,
interleaved, at load average 4–17):

- Bulk ingest, five 30k imports each, header transactions: base min 1,596 / median 1,791 ms,
  this build min 1,686 / median 1,898 ms: **+6%**, about 0.6 ms per 200-message batch. With
  thread and sender indexes on the split rows and `GROUP BY` counts per batch it was +16%; both
  went (the recount counts in Rust, and finds deleted messages through a TEMP list the delete
  trigger fills).
- Upstream `bench` (one new message, 3 message views; 4 runs each), writer transaction p50: base
  0.63–0.69 ms, this build 0.76–0.79 ms; p95 2.3 vs 2.6 ms. Mark read → change event p50:
  0.95–0.99 vs 0.86–1.15 ms (no change).
- A first version joined the TEMP list of deleted ids the planner's way and scanned every split
  row in each transaction (+4.5 ms per write); `CROSS JOIN` pins the order, as for `fork_dirty`.

## Tests

- `domain/src/split.rs`: 10 tests (the realistic headers above, the rule order, bits).
- `infrastructure/src/store/split_tests.rs`: rules in message and conversation views and counts;
  a reply in Sent; a choice in one commit with every open view's diff; the same choice again;
  categories; the same-commit row id; the step's backfill from a phase A store and an older
  build's reused id; All Inboxes folding two inboxes (Other only without an Important member);
  `uncategorized`.
- The replay (`conversation_events_keep_list_models_exact`) now also gives messages random facts
  and categories, makes random choices (mixed-case addresses) and category changes, and holds 6
  split conversation views (All Inboxes folding two inboxes among them) and 6 split message views
  beside the old ones. Every model equals a fresh page after every step, and every split page
  equals an independent recount (`World::splits`). The split choices come from a second
  generator, so each seed's other steps are what they were. `..._more_seeds` (ignored) runs
  `SEALWAX_REPLAY_SEEDS=from..to`, by default 100..160.
  - In release: seeds 100–159 and 2006–2039 pass. Seed 2005 fails at step 99 in an upstream
    message view (All Inboxes, message rows: a row's `thread` after a merge) on the phase A
    build as well: the `merged_threads` ambiguity when a merged-away thread id comes back in the
    same commit, which is track P's.
- `engine/src/imap/sync/view.rs`: a scripted Gmail (its greeting, capabilities and replies) for
  `primary`: Primary and other tabs, an empty answer, a BAD that stops the asking.
- `engine/tests/split.rs` against the fake's Gmail flavour (`X-GM-RAW "category:primary"`, a
  per-message tab): categories winning over facts both ways, asked once per batch and for new
  mail with its rows; header rules without X-GM-EXT-1 and after a refusal; a 1,200-message
  INBOX stored without categories caught up in three searches, once per connection.
- `engine/tests/split_testbed.rs` (ignored, the testbed): the facts from Dovecot, Dovecot
  IMAP4rev1 and Stalwart into the right sides.
- `tests/selftest/split-backend.sealwax-test`.

## Left for later (phase B; done in phase C, below)

- Phase C: the tabs (the filter pill names a split filter "All" today: `list::filter_name`
  reads `FILTERS`); a notification click while a split filter hides the message (the app's
  `passes` can't tell a row's side); whether a side combines with Unread (a `ViewSpec` dimension).
- A choice for a sender with tens of thousands of messages takes about a second; the UI can show
  it at once and let the event follow.
- Part 2 of track C, the Remind Me backend, is in `docs/superhuman/remind.md` (fork step 3).

## UI (phase C, track S)

*2026-10-29.* The tabs, their keys, the setting, the sender's side in the app, and what the
store gained for them. BRIEF-C's decisions; how they were built and measured.

### The setting

- `config.toml`: `[inbox] split = true` (on by default; the file Preferences opens documents
  it), and `[notifications] other = false`.
- **Split Inbox** (registry toggle `split-inbox`, View menu and palette; aliases "tabs",
  "priority inbox") switches it and `layout.toml` remembers it as `split = true|false`. Which
  wins: the toggle's last choice, until `config.toml`'s `[inbox] split` changes (that change
  forgets the toggle's, so editing the file always does what it says). `AppState::
  splits_enabled`: layout's, else the self-test harness's default, else config's.
- The store's read-ahead of the first window's view (S6) opens the Inbox's Important side when
  the splits will show, from layout.toml and config.toml, so the first frame still adopts it.

### What shows (`app/src/list/split.rs`, `window/split_tabs.rs`)

- An Inbox view (an account's Inbox or All Inboxes) with the splits on shows one side: the
  store's view with `ViewSpec::split` set, under the filter shown. `MessageList::split()` says
  which; other folders, search results and Reminders have none.
- **The header** replaces the view's name and its subtitle with the tabs, a widget of its own
  (`SplitTabs`): the names at the title's size (16 px) in regular weight, the side shown in the
  text's colour, the other in the secondary; each total after it at 12 px in the secondary,
  baseline-aligned, nothing when zero (Superhuman's: no underline, pill, icon, box or colour).
  The account's name goes (the visual QA's "· Personal") while there is one account. With
  several, it follows the tabs as the view name's subtitle did ("· Work", "· All Inboxes"),
  since with the sidebar hidden (the default) nothing else says whose Inbox it is (the
  review's two-account probe: All Inboxes and each Inbox had the same header).
- **Fit** (the visual QA's 380 px list): the widget lays its children out itself. Where the row
  can't hold everything, the account's name goes first, then the gap between the tabs narrows
  from 24 to 14 px, then the totals go (`assert-split-tabs bare`); the tab's accessible name
  keeps its total and the tab list's name keeps the account. A filter's pill
  (Unread ×) sits right after the tabs inside the widget (a second pill instance, named
  `list-split-filter`), so the space the totals free isn't left between "Other" and the pill.
  Measured in Adwaita dark: Split's 380 px list header with the Unread pill shows
  "Important  Other  [Unread ×]" ending at x 252 (the search icon at 279); with no pill the
  totals show; Focused at 1440 px and the 710 px half tile show everything.
- **A tab list** to a screen reader: the widget's role is `tab-list`, named for its Inbox
  ("Inbox, Personal", "All Inboxes"), each tab a `tab` named "Important, 4 conversations" ("no
  conversations"; "messages" with message rows), the side shown `selected`; the totals and the
  account's name after the tabs are presentation only. A click on a tab (or on its
  total: the tab spans it) picks it; the keyboard focus stays in the list.
- While the list shows a side, its accessible description says "Tab and Shift+Tab switch
  between Important and Other; Ctrl+Tab leaves the list", with the keys `keys.toml` gives Next
  Split and Previous Split (`list::split::split_keys`; Ctrl+Tab only while Tab is one of them),
  and switching announces "Other, 4 conversations".
- The search field and the select mode's bar take the header row as before: the tabs are in
  the heading they replace, so they hide with it.
- The keyboard's focus ring on a tab sits 2 px out from the words (`--sw-space-xxs`) and the
  tabs start 1 px further in (the first word at x 38): measured in Adwaita dark, the ring's
  2 px at x 33–34 leave 3 px to the sidebar button's glyph (ending at x 29) and 3 px to the
  word; it was 1 px from the glyph. The names column's start moves by that 1 px only.
- The reader's More menu leaves the three sender commands out while they don't apply
  (`hidden-when = action-disabled`), rather than greyed: with the splits off, outside an Inbox,
  and Reset Sender Split for a sender with no choice.
- The reader's "n of N" counts the rows a reload brings when the conversation shown stays
  selected (Split Inbox turned on or off, a filter): `MessageList::connect_reset`; before, it
  said "1 of 10" over a 6-row side.

### Keys and commands (registry block at the end of `ActionId::ALL`, 110 → 118)

| Command | Key | Notes |
| --- | --- | --- |
| Split Inbox (`split-inbox`) | — | toggle, View menu |
| Next Split (`next-split`) | Tab | List scope, wraps; applies only while the list shows a side, else Tab goes on to GTK |
| Previous Split (`previous-split`) | Shift+Tab | the same |
| Go to Important (`go-to-important`) | — | `g i` (Go to Inbox) lands on Important too |
| Go to Other (`go-to-other`) | g then o | free in the audit; from anywhere, back to the list |
| Always Important / Always Other (`always-important`, `always-other`) | — | on the selection's sender; palette and the reader's More menu |
| Reset Sender Split (`reset-sender-split`) | — | while the sender has a choice |

- **The audit**: Tab and Shift+Tab are focus keys (`keymap::FOCUS_KEYS`); `ActionSpec::
  takes_tab` lets the two split commands have them (only them: a test pins it). The scoped
  shortcut runs only while the command applies (`NamedAction` returns false when it is
  disabled), so where there are no sides Tab moves the focus as before: a11y-window's Tab
  chains are unchanged. Nor in the select mode: its bar hides the tabs, and Tab goes on to it
  (the review's probe: Tab there switched sides and dropped the rows chosen).
- **The way out**: GTK's own Ctrl+Tab and Ctrl+Shift+Tab move the focus out of a widget that
  takes Tab; the list doesn't take them (`selection_key` lets Ctrl keys through), so they leave
  it (`split-inbox` checks it). No F6 pane cycling was added: the contact pane's track may add
  one; Ctrl+Tab is GTK's documented way and needs nothing new.
- `g i` always shows Important; `g o`, Go to Important and Go to Other go to the Inbox first
  when another folder shows. The Inbox then opens on the side asked for at once
  (`MessageList::want_split`, taken by the next Inbox `show`): one view, where showing the
  side last shown and then switching opened two or three.
- The View menu puts Split Inbox after Group by Conversation, with the layout toggles
  (`menu::PLACED_AFTER`), though the registry keeps the phase's commands in one block at its
  end.

### Switching (under 50 ms on a small Inbox; over it at 100,000 messages, see Measured)

- **Warm**: after a side shows, `warm_other` opens the other side's view in the background
  (`Store::open_view`, its first page) and keeps every change event since; a switch adopts it
  (`MessageModel::adopt_with`, the events replayed as for a view still opening) in the key's
  own frame, then warms the side it left. A Reload for it, more than 128 events, a filter,
  grouping or day change, or splits off drop it; a switch without it opens as before.
- **Places**: each side's cursor row and scroll, per scope, while the app runs (`Place`). Tab
  and Shift+Tab put the cursor on the new side's top row (Superhuman's); a click on a tab,
  `g i`, `g o`, the palette, and coming back to the Inbox from another folder find the side as
  it was left (the row selected again, the scroll restored after layout). In Split (the reading
  pane) Tab selects nothing, as a folder opening there doesn't: the keyboard focus waits on the
  top row and `j` picks it.
- The tabs' totals come from the store (`split_counts`, constant time), once at a time, again
  after each change event while a side shows: totals, not the filter's rows. Another Inbox or
  another grouping forgets the last totals until its own are counted (the tabs show their
  names alone for that moment), so a total is never another view's.

### Inbox Zero per side

The side shown is an Inbox view with filter All, so its emptiness is Inbox Zero
(`is_inbox_view` as before). One quiet line under the date says what the other side holds
("Other has 4 conversations", or "messages") with Next Split's key as a keycap; it is in Inbox
Zero's accessible name too (`InboxZero::set_other`).

### Filters, search, grouping

- `ViewSpec` gained `split: Option<Split>` (`with_split`, `side()`, `base_filter()`; a split
  filter is the side alone and wins). Unread, Starred and Attachments combine with the side
  shown; the pill names the filter and never says "All" (splits are not filters; the
  backend's split filters, which no control sets, are named "Important" and "Other" by
  `list::filter_name`).
- Search ignores the sides (it doesn't read `split`); the list keeps the side while it runs and
  shows it again when it ends.
- Group by Conversation off: the same sides with message rows (the tabs count messages).

### The store's side dimension

- Conversation views: `Shape` holds the filter and the side (`views::Kept`). One folder (or
  folders of different accounts): the filter's partial index walked with the side's condition
  on the same row (a row is the conversation in that folder). Folded views (an account with two
  Inbox folders): `GROUP BY thread_id HAVING max(<filter column>) > 0 AND max(important) …`,
  since the filter's member and the side's may be in different folders. A side under a filter
  has no maintained count: the filter's index is walked (unread, starred and attachment
  conversations are few).
- Message views: the filter's index on `messages` with `EXISTS (SELECT 1 FROM
  fork_message_split s WHERE s.id = messages.id AND s.split = N)`; diffed by
  `writer/split.rs` as the split views are, the filter checked on each dirty thread's message
  with its flags before and after (`writer::Touched`: a move's two row changes merge into one).
- The replay test holds 6 such conversation views and 5 message views more (All Inboxes
  folding two inboxes among them): seeds 2026, 7, 4242, 1051, 1093, 1101 and 100–159 pass in
  release, every model equal to a fresh page and to the reference recount after every step.

### The sender's side

- **Optimistic**: the commands take the selection's sender (a conversation's first writer who
  isn't the user, as the rules do), call `Store::set_sender_split_first` with the selected
  conversations' threads, and when the conversation leaves the side shown the list moves on
  as after Archive (`advance_after`). The store classifies those threads again in its **first
  transaction**, so they move with the first change event; the toast says "Mail from x@y now
  goes to Other" with Undo. The choice is the last action: `z` and the toast's Undo put the
  previous side back (`split_inbox::undo_last`, joining outbox's and rules' last actions).
- **In parts** (`writer/split.rs`): the first transaction stores the choice and classifies the
  threads acted on plus as many as fit `CHOICE_CHUNK` (500 of the sender's messages), the
  threads the open views show first; the others follow in transactions of their own, which
  the writer runs only when no interactive job waits (a lane between the interactive jobs and
  full-text work). So a triage after a choice for a 21,000-message sender waits for one part.
  A new choice for the same sender supersedes the parts left (their waiters hear the new
  one's end). `meta('fork_split_pending')` is set while parts are left; an open that finds it
  (a crash between parts) recounts the store whole, so nothing stays on the wrong side. A
  normal quit finishes the parts first (the writer runs until its lanes are empty).
- `Store::set_sender_split` (the backend's) is the same with no threads first; its reply
  comes with the last part, so callers that wait see every view settled.
- **What the commands know** (the review's blocker): the choices the store keeps, an earlier
  run's too, are read once it is open (`Mailbox::wait_ready`: the store opens on a thread of
  its own after the window is built, so reading them at install found no store). Until then
  the three commands don't apply. A choice that would change nothing does nothing (no toast,
  no Undo that would take the stored one away), and the menus offer only those that change
  something: Reset Sender Split while the sender has a choice. The app's map follows its own
  choices at once and is read again from the store whenever none of them is on its way (a
  write that failed), a generation number keeping a read that crossed a choice from undoing
  it. Undo stores the side before, its conversations first.
- Undo selects what it brings back as triage's Undo does: from Inbox Zero, the conversation
  comes back selected; in a list with rows, the cursor stays where the list moved on (probed
  both ways, in the list and from the open conversation).

### Notifications

- With the splits on, `notify::arrived` keeps only the new messages whose conversation has an
  Important member in that Inbox (`Store::conversation_members` with the Important filter),
  unless `[notifications] other = true`; a reply in an Important conversation notifies even
  from a robot. A store that can't say keeps none quiet (mail from people must not be
  missed). Splits off: as before.
- A click on a notification shows the side that holds the message's conversation (its own side
  with message rows) before selecting its row (`notify::side_of`).

### Self-test

New commands (`selftest/split.rs`): `split-inbox on|off|config`, `split-side SIDE [top]`,
`assert-split-side`, `assert-split-tabs hidden|totals|bare`, `assert-split-totals I O|unknown`,
`assert-zero-other TEXT|none`, `measure-split KEY COUNT MS` (fails when a switch opened its
view instead of adopting the one opened ahead, or when the median is over MS);
`assert-notify-settings` takes a third word, `other|quiet`; after the review:
`stored-sender-split ADDRESS SIDE` (a leading step: the choice written to the store before
Sealwax opens it, as an earlier run leaves it), `assert-first-view adopted|reopened|opened|none`
(the window's first view was the one read ahead, and still the only one the list opened),
`mark-list-opens` and `assert-list-opens N`; `mail-sender-split` now has the app read the
store's choices again, as it reads them at startup. The list is idle for the harness
(`settled`) once the sides' totals are counted and the other side's view is open. The harness
keeps **one Inbox** by default, which every script before this was written for, `layout-defaults
app` included; `split-inbox on` shows the sides, and a leading one (before the window's steps,
after `fixtures`) from the start (`selftest::split::startup_default`), so the first view is read
ahead as the app reads it.

- `split-inbox.sealwax-test` (253 steps): tabs and totals, the tab list's roles, names and
  states, Tab and Shift+Tab with the cursor on the top row and the announcement, wrapping,
  `g o`/`g i` and clicks finding a side as it was left, `g o` and Go to Important from another
  folder in one view, switching under 50 ms (every switch adopted the warm view), Tab in the
  select mode, Unread on a side with its pill, search ignoring the sides and coming back,
  Inbox Zero per side with its line and accessible name, Always Other with the toast and Undo,
  Always Important from the palette and Reset Sender Split, Split's Tab, Ctrl+Tab out of the
  list, message rows, the toggle off and on with the reader's "n of N", two accounts (the
  account's name after the tabs and in the tab list's).
- `split-choices.sealwax-test` (58 steps): a choice an earlier run stored: the commands know
  it from the start (Reset applies, Always Other doesn't, the palette names the address),
  Always Other again does nothing, and Undo of Always Important and `z` after Reset put the
  earlier run's Other back. With the old code it fails five steps (the review's repro).
- `split-startup.sealwax-test` (15 steps): the sides on from the start, as in the app: the
  first view is the Important side the store read ahead (with the old harness default it
  failed: "reopened"), and the first Tabs adopt the other side's view.
- `split-notify.sealwax-test` (42 steps): Other quiet, an Important conversation's robot reply
  notifying, `other = true`, splits off, a click switching to Important.
- `perf-split.sealwax-test` (by hand, release, like `perf-list`; not in `tools/ci.sh`):
  switching at 100,000 messages in Focused, with message rows and in Split.
- Store tests (`split_tests.rs`): `a_side_combines_with_the_other_filters`,
  `a_choice_for_a_busy_sender_goes_in_parts_with_other_writes_between` (it holds SQLite's write
  lock while it queues the choice and Kari's flag, so no part can run before the flag is
  queued, however loaded the machine), `a_choice_cut_short_is_finished_when_the_store_opens_again`;
  the replay (above).
- Unit tests: the config keys, the audit's Tab exception and the actions' keys, `g o`, the
  layout key, the menu and its order, the sender and the toast's words, the list's
  description with moved keys, the More menu's hidden commands.

### Measured

All headless (Broadway paints in software), the machine shared with three other tracks' builds.

- **Switching, a small Inbox** (`split-inbox.sealwax-test`, debug build, 7 conversations):
  key → first frame painted with the other side, over six Tabs: median 11 ms (8–17 ms) run
  alone, median 14–23 ms (worst 49 ms) with two other self-tests running beside it; the rows
  are in 2–12 ms; every switch adopted the warm view.
- **Switching at 100,000 messages** (release; every switch adopted the warm view, the model's
  part under 1 ms; what costs is GTK's):
  - the bench store (`bench-split`'s copy, folders spread: an Inbox of 25,351 conversations,
    15,429 Important and 9,922 Other), load 6–8: key → painted, median 53 ms in Focused, 37–48
    ms in Split, 55 ms with message rows; 89–130 ms at load 15. `items_changed(0, 15,429, 64)`
    takes 18–30 ms (GtkListView releases the ~200 rows it keeps realized and makes new ones;
    GTK's test accessibility backend, which the harness uses, adds a third), then the first
    frame's layout and software paint. Tried and dropped: binding fewer rows first (28: no
    change) and rebinding every realized row (240: the reset doubles). A folder open pays the
    same reset (perf-list: 53 ms at low load).
  - `perf-split.sealwax-test` on perf-list's store (all 100,000 in one Inbox: 41,336
    conversations, 31,755 Important and 9,581 Other), load 9–10: median 92 ms (Tab) and 93 ms
    (Shift+Tab) in Focused at 1440×900, 94 ms with message rows, 55 ms in Split; the rows were
    in after 19–44 ms (median 27 ms), the rest is the frame. At load 25 (three other tracks
    building): 230, 262, 270 and 95 ms. Over the bar: see Left for later.
- **A choice in parts** (`bench-split`, release, the 100k fixture store, load 16–19):
  - the fixture's busiest sender (21,043 messages, all in Sent): its first part (the
    conversation acted on) → change event p50 49 ms; a triage (Mark as Read on another
    sender's Inbox mail) queued right behind it → its change event p50 23 ms, max 25 ms (it
    waits for one part at most, where one transaction held the writer ~1.2 s); all 42 parts
    p50 1.29 s.
  - the busiest Inbox sender (961 messages, 597 in the Inbox, both sides open): first part p50
    48 ms, a triage behind it p50 36 ms, 2 parts in 85 ms.
  - `check` after it: ok.

### Left for later

- **Switching at 100,000 messages is over the 50 ms bar** (median 53 ms on a 25,000-conversation
  Inbox at load 6–8, 92 ms on a 41,000-conversation one at load 9–10, 55 ms there in Split),
  the small Inbox well under it. The store's part is warm already; what is left is GTK's reset of a long list and
  the frame. A list view per side, kept realized behind a stack, would leave only the frame:
  the list's code holds one view, one selection and one model throughout (`list/mod.rs`, which
  L1 edits too), so that is a refactor of its own, best done after L1 lands. The review
  reproduced it (release, perf-list's store, load about 6): Focused Tab median 95.7 ms (worst
  143), Shift+Tab 91.4 ms (worst 109), Split 47.3 ms, every switch adopting the view opened
  ahead and its rows in after 20–36 ms; over BRIEF.md's "never > 100 ms" at that scale. The
  owner's Inbox (about 21 conversations) switches in about 13 ms. After the review's fixes
  (release, the same 41,000-conversation Inbox, load 7–10 from other tracks' builds), runs
  alternating with and without the reader's new reset hook: Focused Tab median 78 and 60 ms
  with it, 67 and 88 ms without; Split 47 and 45 ms with it, 54 and 84 ms without: no cost
  that the load's noise doesn't swamp.
- `split-inbox`'s `measure-split Tab 6 50` holds the brief's bar in CI, and like
  triage-frames' bar it needs a quiet machine: debug build, alone, median 15.3 ms (worst 49
  ms) at load 2.5; 59.8 and 67.1 ms at load 9–10 (other tracks' builds), and 109 ms in a sweep
  of three self-test shards at once. The bar is the product's, so it stays; every switch
  adopted the view opened ahead in all of these runs.
- A choice for a sender that Sealwax quits in the middle of is finished at the next open by a
  whole recount (seconds at 100k); finishing only that sender's threads would need its address
  kept (a fork step: not this track's).
- `layout.toml`'s toggle wins over a `config.toml` edited while Sealwax wasn't running (a change
  is seen only while it runs).

### Decisions to know

- Tab goes to the top of the new side; the other ways find it as it was left (the brief's two
  rules, both kept).
- In Split, Tab selects nothing (no conversation is shown or read by switching tabs).
- Sender choices are toast-and-`z` undoable, outside the triage controller's batches.
- Counts count conversations (messages with message rows), not unread ones (zero is the goal).
- The Split Inbox toggle sits with the layout toggles in the View menu (`menu::PLACED_AFTER`),
  the registry's own order unchanged.
