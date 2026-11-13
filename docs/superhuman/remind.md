# Superhuman pass: Remind Me

*2026-10-29, phase B track C, part 2 (the backend); phase C track R (the UI and the backend's
follow-ups, [UI](#ui) and [Phase C in the backend](#phase-c-in-the-backend)).* Superhuman's
Remind Me (`h`) sends a conversation away until a time; then it comes back to the top of the
Inbox. The backend: the rules (`domain/src/remind.rs`), the store's reminders, moves, sort key
and diffs (`infrastructure/src/store/fork.rs` step 3, `remind.rs`, `remind_api.rs`,
`writer/remind.rs`, `reader/remind.rs`), and the engine's timer and folder
(`engine/src/service/reminders.rs`, `service/actor/ops.rs`, `imap/manage.rs`). The UI:
`app/src/remind.rs`, the time picker's Remind Me (`app/src/time_picker.rs`), the rows
(`app/src/list/row.rs`), the reader's line, the notification and the Reminders view.

## What happens

1. **Remind** (`Store::remind`): the conversation's messages in the view's folders (an Inbox,
   All Inboxes, or any folder; never its copies in Sent, Drafts, Trash or Junk) move to the
   account's **Reminders** folder, as triage moves messages: one transaction, one Undo batch, a
   server account's moves after the undo window. The reminder is kept beside them in the same
   transaction, so a crash leaves both or neither, and a retried move finds what the server
   already did as any move's does (ADR-015).
2. **The Reminders folder**: the account's folder named as folder management names a new
   folder at its top ("Reminders", or "INBOX.Reminders" where every folder is under the
   Inbox), in any case; one the user made by hand counts. When the account has none, the store
   makes its row at once (the sidebar shows it, the moves have a target). For a server account
   it is marked (`fork_folder_creates`) and the engine creates it before the first move into it
   runs: `CREATE`, then `SUBSCRIBE`; a folder of that name the server already has counts
   (`[ALREADYEXISTS]`, or a refusal without that code where `LIST` names it). On Gmail it is a
   label. Until the server lists it, the folder list keeps it while a move into it waits
   (`remind::keep_unlisted` in `replace_folders`), and an Undo before any move ran drops it.
3. **The wake** (`Store::wake_reminders(now)`): each waiting reminder due at `now` moves its
   conversation's messages from its Reminders folder (where the op queue has them or takes
   them) back to the Inbox they came from, else the account's first Inbox, due at once, and is
   **reminded at `now`**. Offline, those moves wait in the op queue like any; a move back of a
   message whose move away never ran folds into nothing (A→B→A), so it simply shows again.
4. **The return to the top**: in an Inbox, a reminded conversation sorts by the later of its
   latest message and when it came back (`remind::sort_date`), until it leaves the Inbox again.
   Mail that arrives later goes above it; a reply sorts it by the reply. Done, Delete, Junk,
   Move and Remind again end it (`remind::ends`); **reading and starring keep it**, and Undo of
   the action that ended it restores it.
5. **Cancel** (`Store::cancel_reminders`, the UI's Remove Reminder): a waiting conversation
   comes back to its Inbox now (one Undo batch, after the undo window), without the return to
   the top; a reminded one stops sorting by its return. Undo restores either: since phase C a
   cancel that moves nothing (only reminded ones) still has a batch, an empty one that
   `ops::prune` keeps while it ended a reminder (until it ages out with the others), so its Undo
   restores the reminder (before, it had none, and the docs said otherwise: the final review).
6. **"If no reply"** (phase C): the wake also ends every waiting reminder whose conversation
   someone else answered meanwhile, and moves what waited back beside the answer (below).

## Decisions

- **"Any triage" ends the return to the top, except read and star changes.** The brief said any
  triage. But opening a conversation marks it read through the same triage queue (B1's
  mark-read-on-open queues `Triage::Read(true)`), so "any" would drop a reminded conversation
  from the top the moment the user opened it. Superhuman keeps a conversation's place when it
  is read or starred, and removes it when it is done. So the actions that take the conversation
  out of an Inbox end it (`remind::ends`, one line to change).
- **Acting on a waiting conversation in Reminders ends the wait.** Archived or deleted from
  the Reminders folder, its reminder ends, so nothing comes back; Undo restores it.
- **Reminding a waiting conversation again gives it the new time** (what came to the scope
  meanwhile, a reply, goes too). The new time isn't an Undo entry; the moves are. Since phase C
  the UI makes a new time the last action, with nothing to undo ([UI](#ui)).
- **A reply that arrives while it waits ends the reminder** (phase C, Superhuman's default "if
  no reply"; phase B kept it waiting): the reply lands in the Inbox as any mail does, and the
  next wake ends the reminder (`ended`, no Undo: no one asked for it) and moves the messages that
  waited back to the Inbox at once, so the conversation is whole again at the top, sorted by the
  reply. A reply is a shown message of the conversation in an Inbox of its account whose
  Message-ID isn't one of the reminder's keys (so it came after the reminder was set) and that
  the user didn't write (its sender isn't the account's address, and no copy is in Sent):
  `remind::replied`. The engine runs the wake whenever new mail arrives (below), so it happens
  within the new mail's own pass. Superhuman's "regardless" mode isn't offered.
- **A conversation is found by its messages' Message-IDs** (`fork_reminder_keys`), not its
  thread id: a merge folds it into another thread, and a move the store can't place (no
  COPYUID, or a folder never synced) deletes the rows, and with them an emptied thread, until
  the target's sync stores them again under a new id. The reminder resolves to the threads that
  hold a message of the account with one of its Message-IDs, hidden or not. A conversation whose
  messages have no Message-ID is found by its thread, which it follows through merges; once that
  thread is gone it can't be found again, and the reminder goes.
- **The engine syncs a never-synced Reminders folder before the first move into it**, so the
  moves land in rows of their own (COPYUID) and the conversation never leaves the store on its
  way. Where a move still can't be placed (no UIDPLUS and no Message-ID match), a due reminder
  whose messages are in transit (nothing in Reminders while a message of it isn't stored) waits
  for them, a day at most, and the timer looks again every minute meanwhile.
- **The sort date is the return's time; `latest` stays the latest message.** A reminded row's
  `ConversationSummary::date` is when it came back (so it sits in Today's section and shows that
  time), while `latest`, its sender and snippet stay its latest message's. `ConversationSummary`
  gained no field: B1 builds it by literal. Phase C can mark reminded rows from
  `Store::reminders()` (by thread) or add a field.
- **Only conversation views move.** Message rows keep their own dates (the harness's message
  list shows a reminded message where its date puts it).
- **A refused folder puts the mail back, and the reminder stays.** The moves into a folder the
  server won't create are refused as for a folder gone from the server (the messages show again
  where they are, `Event::OpsRefused`); at its time the conversation still comes back to the
  top, as it never left the Inbox. Reminded again meanwhile, it gets a new Reminders folder.
- **A reminder lets go of what goes.** Thread, folder and op batch ids are SQLite rowids, and a
  row inserted after the newest one is deleted takes its id again. So a reminder's `thread_id`,
  `folder_id`, `batch` and `ended` are foreign keys that go NULL with their row (one mechanism
  for every path that deletes one: a refused or deleted folder, an emptied thread, a pruned
  batch). Without that (the first review found it), a folder the server listed next took a
  refused Reminders folder's id, and reminding the conversation again moved its mail into that
  unrelated folder (and its wake moved the conversation's mail out of it); and a remind batch
  emptied by a refusal and pruned at the wake gave its id to the next batch, whose Undo then
  deleted the reminder. `origin` only orders the account's Inboxes, so it stays a plain id.
- **The change event says when reminders change** (phase C): `ChangeEvent::reminders` is set
  by every commit that sets, changes or lets go of a reminder that counts (TEMP triggers on
  `fork_reminders` fill `fork_reminders_touched`, so no path can forget it, an account's
  cascade included), and the app reads `Store::reminders()` again then. The picker, the
  Reminders view, `g h`, the clock mark and the notification are the [UI](#ui)'s.

## Store

### Schema: fork step 3 (`meta('fork_schema')` = 3)

- `fork_reminders(id, account_id → accounts ON DELETE CASCADE, thread_id → threads,
  folder_id → folders, origin, wake_at, state, reminded_at, batch → op_batches,
  ended → op_batches, ended_from, created)`, the four references `ON DELETE SET NULL`:
  `wake_at` and `reminded_at` are Unix seconds (UTC), as message dates; `state` is
  `remind::ReminderState` (0 waiting, 1 reminded, 2 ended); `batch` is the op batch that set it
  (Undo deletes it); `ended`/`ended_from` the batch that ended it and the state it ended from
  (Undo restores it; ending one that has ended changes nothing). A partial index on `wake_at`
  for the waiting ones, and one on each reference.
- `fork_reminder_keys(reminder → fork_reminders ON DELETE CASCADE, message_id)`: the
  conversation's Message-IDs when it was set (and those of replies sent along later), also
  indexed by `message_id`.
- `fork_folder_creates(folder_id → folders ON DELETE CASCADE)`: Reminders folders the store made
  for a server account that the server doesn't have yet.
- The step changes no derived table, so it doesn't recount at open (`MIGRATIONS` now says per
  step whether it does; steps 1 and 2 do). Ended reminders go with their batch, and those
  without Message-IDs with their thread (`remind::prune`, from `ops::prune`).
- Step 3's table changed after the first review (the references). No released build has the
  step, but a store that an earlier build of this track opened (a scratch fixture) keeps the
  older table: make it again.

### The sort bump

The writer's recount (`recount_dirty`) reads when the **dirty** threads' reminded
conversations came back (`fork::DIRTY_BUMPS`: from the threads' messages through the keys'
`message_id` index, so its cost follows the transaction, not the reminders kept; nothing at all
while no reminder is reminded) and gives an **Inbox** row of such a thread the sort key
`(max(latest date, reminded_at), latest id)`; Trash, Junk and Drafts rows keep their own newest
member, other rows the latest message. `RECOUNT_ALL` (the rebuild's and `check`'s formulation)
does the same in SQL for every thread (`bumps!`), so `check` verifies the incremental path.
Every change of a reminder's state marks its conversation dirty (`fork::touch`), so the same
transaction's recount moves the row and the conversation views get exact diffs (a move, or a
lone in-place change). `commit_rows` no longer skips its event when only a thread was marked
dirty. The other lookups go the same way: the triage hook and a remind find the reminders of
the conversations concerned through their messages (`remind::of_threads`), the wake reads the
due ones through `fork_reminders_waiting`.

### API (`sealwax_infrastructure::store`, `remind_api.rs`)

```rust
store.remind(Remind { threads, scope: ViewScope::Folder(inbox), wake_at, not_before })
    // -> Reminded { version, batch, reminders, moved, not_applied, accounts }
store.cancel_reminders(threads, not_before)        // -> Queued (Undo restores)
store.wake_reminders(now)                          // -> Woken { version, woken, accounts, next }
store.reminders()                                  // -> Vec<Reminder> (waiting and reminded;
                                                   //    `folder` None once that folder went)
store.folders_to_prepare(account, now_ms)          // engine: Reminders folders to CREATE or sync
store.folder_created(folder) / folder_refused(folder)
```

After `remind` and `cancel_reminders` the app sends `Command::OpsQueued` for each account in
the reply and `Command::RemindersChanged` once. `Store::undo` takes back a remind or a cancel
like any batch.

## Engine

- **The timer** (`service/reminders.rs`, one task beside the account lane): calls
  `wake_reminders(now)` at the engine's start (overdue ones), then sleeps until the next one is
  due (`Woken::next`), at most a minute while one waits (a wall clock that moved without a
  reconnect), or until `Command::RemindersChanged` or `Command::Reconnect` (the app sends it on
  resume: a sleeping timer doesn't count a suspend). It wakes the op runners of the accounts with
  moves queued.
- **The folder** (`Poller::prepare_folders`, before each round of due ops, with the same `now`
  as `due_ops`): creates a marked Reminders folder that a due move goes into
  (`imap::manage::create_reminders`), and gives a never-synced one its first pass. A refusal
  drops the folder in the store (`folder_refused`), which refuses the moves; the next read of
  the due ops reports them (`Event::OpsRefused`).
- New: `Command::RemindersChanged` (the self-test's fake engine names it `reminders-changed`).

## Self-test hooks (`app/src/selftest/remind.rs`)

| Command | What it does |
| --- | --- |
| `remind SUBJECT WHEN` | Sends the list view's conversation with SUBJECT to Reminders until WHEN, one Undo batch; tells the engine |
| `remind-cancel SUBJECT` | Cancels its reminder |
| `remind-undo` | Undoes the last `remind` or `remind-cancel` |
| `wake-reminders [WHEN]` | The wake at WHEN (the script's clock: the list's clock's now without it) |
| `assert-reminder SUBJECT waiting\|reminded\|none [WHEN]` | Its state, and when it is due or came back |
| `assert-conversations FOLDER [SUBJECT…]` | The store's conversation rows of FOLDER (`all-inboxes`, `[ACCOUNT:]ROLE\|NAME`), in order |
| `assert-conversation-date FOLDER SUBJECT WHEN` | The date a row sorts and shows by |
| `remind-reply SUBJECT FROM` | An answer from FROM to the conversation arrives in its Inbox now (phase C) |
| `assert-reminder-at SUBJECT LABEL` | When its reminder comes back, as the picker says it (`tomorrow 08:00`, `someday`) |
| `assert-row-reminder POS BOOL` | Whether the list's row shows Remind Me's clock |
| `assert-reminders-shown marks\|wakes\|none` | What the list's rows show of Remind Me |
| `assert-reader-reminder TEXT\|none` | The reader's quiet line |
| `assert-quick-order POS NAMES` | A row's quick actions, left to right (`archive,remind,delete,star`) |
| `assert-quick-off POS BOOL` | Whether a row's clock quick action is dimmed (Sent, Drafts, Trash, Junk; the second review) |
| `assert-empty-icon NAME` | The list's empty state's icon (`alarm-symbolic` in the Reminders view; the final review) |
| `assert-empty-text TITLE DESCRIPTION` | The list's empty state's words (the third review) |
| `assert-quick-tooltip POS NAME TEXT` | A row's quick action's tooltip, with the reason where Remind Me is off |
| `remind-server-account NAME` | A server account (keyed, as the engine's are), its role folders synced once and empty; its mail gets UIDs |
| `remind-server-run NAME` | A stand-in server with UIDPLUS carries out the account's ops due now, as the engine's op runner would (CREATE and a first pass of a Reminders folder, moves with COPYUID) |
| `remind-new-session SINCE` | The app's reminders begin anew as at a start begun at SINCE (its first read; what came back before SINCE is an earlier session's) |

WHEN is ISO 8601 (local time unless it names a zone), `now`, `someday`, or `+30m`/`+2h`/`+1d`
from the list's clock. `click-quick POS remind` clicks a row's clock.

`tests/selftest/remind-backend.sealwax-test` (49 steps): sent away, Undo, not due before its
time, back on top dated when it came back, later mail above it, starred keeps its place, Done
ends it, cancel.

## Tests

Phase C:

- `domain`: `remind::waiting_rows_sort_soonest_first_and_someday_last` (the wait key and its
  inverse), `when::someday_is_no_time_at_all`, `split`'s rule order with
  `classify_reminded`, the keymap (`g h`, Shift+E, the `g` prefix audit),
  `theme::the_reminder_mark_reads_on_every_row_surface` (3:1 in every theme, both list
  surfaces; the theme snapshots gained the `reminder` lines and variables only).
- `infrastructure/src/store/remind_tests.rs` (21): new `someday_waits_until_it_is_removed`,
  `reminders_list_the_soonest_due_first` (the order, the keys' wake times, a new time moving a
  row, the change event's flag), `a_reply_from_someone_else_ends_the_reminder` (the user's own
  answers in Sent and Cc'd into the Inbox don't), `a_conversation_back_from_reminders_is_important`
  (over an "Always Other", until Done), `a_relisted_reminders_folder_still_brings_its_conversation_back`,
  `acting_on_it_in_a_relisted_reminders_folder_ends_the_wait`,
  `a_reminder_whose_folder_went_with_its_messages_ends_at_its_time`; changed on purpose: the
  cancel test (Remove Reminder on a returned one is undoable) and the transit test (gone for
  good, it ends). Each new behaviour was seen failing with its line taken out (the fallback in
  `back()` and in `triaged()`, the ending, the cancel's batch, the reply check, the wait key, the
  Important rule, Someday's `next`, the touch on a new time). `fork::tests` keeps the SQL wait
  key equal to the domain's.
- The replay (`conversation_events_keep_list_models_exact`) now opens a conversation view on
  each Reminders folder as it is made, reminds Someday now and then, and its reference sorts
  waiting rows by their keys and puts reminded conversations in Important; the "if no reply"
  ending happens in it whenever random mail answers a waiting conversation.
- `engine/tests/remind.rs` (7): new `an_answer_ends_the_reminder_and_brings_the_conversation_back`
  (an answer through IDLE: back on the server in 22 ms; a Someday reminder beside it stays);
  seen failing without the new-mail nudge (the timer's minute).
- `engine/tests/remind_testbed.rs` (6): new `an_answer_brings_it_back_on_{dovecot,
  dovecot_imap4rev1, stalwart}` (another client appends the answer).
- App unit tests: the picker's rows (`remind_me_adds_someday_and_remove`), the rows' wake times
  and their column, the accessible label, the toasts, the Reminders folder by account, the
  toolbar's and the batch bar's order; after the final review the clock's slot by account
  (`each_account_has_its_reminders_folder`), where nothing can be reminded, Go to Reminders
  among the palette's places, the sidebar's `INBOX.Reminders`, the reminders' count's words.
- `remind_tests::a_search_dates_a_waiting_conversation_by_its_latest_message` (the final
  review's first blocker: everywhere and in Reminders alone, and answered before the wake);
  it fails without `Summaries::dated` (the date was the wait key, 1351123898575).
- Self-tests: new `remind-ui` (306 steps; 206 before the final review: the quick actions' order,
  the picker's presets and typed times, a choice leaving at once with the cursor moving on and
  Undo, from an open conversation too, several at once, Someday, the sidebar row, `g h` and the
  Reminders view's order and labels and aligned date column, the reader's line, a new time and
  Remove Reminder from the picker with Undo, Shift+E, the palette's Remove Reminder, the wake with
  the clock, the notification and its click, read and star keeping the clock, Done ending it, "if
  no reply"; since the final review: Go to Reminders once in the palette, a search dating a
  waiting conversation in the Reminders view and from All Inboxes, its message rows dated with
  Group by Conversation off, Remind Me off in Sent and a row's clock's toast there, no clock slot
  until one is back, Done withdrawing a reminder's notification, four back at once as one count,
  the empty Reminders view's clock, and turning notifications off withdrawing what showed);
  `preview-remind` (not CI) takes the review's screenshots. The palette and search steps fail
  without their fixes ("Go to Reminders|Go to Reminders"; "TUE 08:00" for "OCT 23" with only the
  app's decoding of the key left, a blank date before either). Updated on purpose: `triage-keys`
  (`h` runs Remind Me and opens its picker), `palette-keys` (Go to Reminders among the go-to
  commands).
- After the second review: `remind-ui` (357 steps: Done, then a new time, then `z` brings
  nothing back and Undo is off; the Unread and Starred filters' empty states in the Reminders
  view; "Tomorrow" in the picker; `h` in Sent, in the list and the reader, and the dimmed clock
  there, not in the Inbox) and new `remind-undo` (50 steps: a send in its undo window, then a new
  time, then `z` leaves the send queued; Remove Reminder is the last action again). Seen failing
  without each fix: Undo stayed on and `z` brought "Lunch on Friday?" back to the Inbox, and in
  `remind-undo` took the send back to a draft; the filters' empty states said "No reminders"
  under the clock; `h` showed no toast, in the list and in the reader. App unit tests:
  `what_came_back_notifies` (the first read notifies what came back since the session began),
  the dimmed clock's folders, the picker's "Tomorrow" (Send Later keeps "Tomorrow morning").
- After the third review: new `remind-server` (104 steps, in CI: a server account through the
  stand-in server; `h` and "On the way" in the Reminders view until the moves ran, then the row
  with "SOMEDAY"; a wake whose moves back wait: nothing in the Inbox and no notification, then
  "Reminder: Review trip" from Tomas Berg once they land, its click, the clock, Done ending it;
  at a new start, one back in an earlier session that lands now notifies, one already back
  doesn't again, one the start brings back notifies when it lands). Seen failing with each of
  the change event's trigger, the earlier session's reminders and the quiet rule taken out.
  `remind-ui` (374 steps): `h` on an archived conversation in a search of every folder says why
  with no picker; the clock's tooltip in the Inbox, and with its reason in Sent. New by hand:
  `testbed-remind` (the real engine on Dovecot's `ops`, `tools/testbed/remind_mailbox.py fill
  dovecot` first): 59 of 59. App unit tests: `a_reminder_back_waits_to_notify_until_it_ends_or_too_long`
  (`still_awaited`: ended, waiting again, a day on, unsure kept), `back_before` in
  `what_came_back_notifies`, `remove_reminder_says_when_or_how_many`, the waiting count of a
  Reminders folder (`arriving`) and the clock's reason, "had ended already", the notification's
  subject without "Re:" (`notify::reminder_subject`).

Phase B:

- `domain/src/remind.rs`: the folder's name (Gmail, `INBOX.` servers, flat), finding it in any
  case, the sort date, which actions end a reminder, the state codes.
- `infrastructure/src/store/remind_tests.rs` (14): a remind and its Undo (the user's reply in Sent
  stays, a new folder in the event); the wake to the top, later and earlier mail, read, unread
  and star keeping it, Done ending it and Undo restoring it, a move back without it; cancel and
  its Undo; a new time, a reply sent along, a return ended by reminding again and restored by
  Undo, a later reply; archived in Reminders; a merge and a thread made anew; a server account's
  folder kept by the folder list until listed, created, synced; a refused folder; waiting for
  messages in transit, a day at most. Rowid reuse, each test seen failing before the fix: a
  refused folder's id taken by the user's next folder (reminded again, a new Reminders folder;
  the wake moves nothing out of it); an emptied remind batch's id taken by a star, whose Undo
  keeps the reminder; a gone thread's id taken by a new conversation, which isn't reminded. And
  a reminder without Message-IDs through a merge, and one ended twice in one batch (each fails
  with its line taken out). Every list model is fed only by change events and checked against
  a fresh page, and `check` recounts the sort keys independently. Mutation check: with the bump
  left out of the Rust recount, six of the first nine fail on "conversation rows disagree".
- The replay (`conversation_events_keep_list_models_exact`) now also reminds random
  conversations (local and server accounts, four scopes), wakes them on a moving clock through
  the messages' dates, cancels some, and undoes reminds and cancels among the triage batches;
  the reference (`World::bumps`) resolves reminders by Message-ID on its own. Seed 2026 woke 10
  reminders, with one on top in 87 of 150 steps; a third generator keeps the other steps.
- `engine/tests/remind.rs` (fake server, 6): CREATE and SUBSCRIBE before the MOVE, the timer at
  its time and not before, a reply moved along, the rows kept (synced first); overdue at a
  reconnect and at the next start; offline the moves wait; Undo inside the window never reaches
  the server and drops the folder; `[ALREADYEXISTS]` counts; a refused CREATE puts the mail back
  (`OpsRefused`); Gmail (a label at the top, All Mail never first-synced).
- `engine/tests/remind_testbed.rs` (ignored, the testbed, user `ops`): Dovecot, Dovecot
  IMAP4rev1 and Stalwart, a two-message conversation away and back. The suites whose paths this
  touches pass in release too: `folders_testbed`, `sync_testbed`, `ops_testbed`.
- `service::reminders::tests`: the timer's sleep.
- The whole CI self-test selection (90 scripts with `remind-backend`) passes headless.

## Measured

Phase C, `bench-remind` (now also: one new message while 300 conversations wait, and a wake
with nothing due while they wait), release, three runs on fresh copies of the 100k fixture,
load average 15-16 (other tracks' builds):

| measure | result | bar |
| --- | --- | --- |
| a remind of one conversation → change event | p50 1.34-1.97 ms, p95 3.6-5.2 ms | ≤ 20 ms |
| a wake of one conversation → change event (the reply check over the waiting ones included) | p50 1.58-1.89 ms, p95 3.3-6.4 ms | ≤ 20 ms |
| one new message while 300 wait (`DIRTY_WAITS` in the recount) | p50 1.32-1.61 ms, p95 2.1-3.9 ms | ≤ 20 ms |
| … with no reminder | p50 1.50-1.84 ms | |
| a wake with nothing due while 300 wait (each new mail runs one) | p50 1.43-2.09 ms, p95 1.7-2.8 ms (the first ~30 ms, cold) | ≤ 20 ms |
| one remind of 300 conversations / their wake | 92-106 ms / 98-121 ms | |
| `check` | 3.6-4.4 s, ok | |

On the testbed (`remind_testbed`, release, phase C): a two-message conversation away and back
as before (in Reminders 129-193 ms after the remind; back 8.05-8.41 s after a wake set 8 s
out), and the new answer: another client's reply appended to the INBOX, the waiting messages
back in the INBOX on the server 452 ms later on Dovecot, 1,004 ms on Dovecot IMAP4rev1 (no
IDLE there: its poll) and 112 ms on Stalwart; `ops_testbed` 12 of 12, and
`randomized_triage_stalwart` four runs more, no refusal met (the phase B test-side fix, its
own client picking again once and then leaving a refused change of its own flags, holds).

The first version of the reply check let SQLite choose its joins: without statistics for the
fork's tables it walked every message of the account's folders, and a wake took 55-100 ms with
30 reminders waiting (1.1 s cold); the pinned joins (`CROSS JOIN` from the waiting reminders)
brought it to the figures above. The headless self-test measured `h` → the picker on screen in
51 ms from an open conversation (debug build, load ~15; Send Later's picker 8-54 ms in phase B).
After the final review, ten more (debug build, load 8-17, five from the list and five from an
open conversation): 13.0 ms the first time, then 6.5-7.9 ms; `remind-ui`'s own step 7.6 ms.
After the third review, the review's own probe (release, five opens from the list and five from
an open conversation, three runs, load average 10.6-11.0 with other tracks building; not the
quiet machine the review asked for, which wasn't to be had): the first, cold open of each run
45.2, 68.3 and 44.4 ms, the other 27 17.5-59.3 ms, the median of all 30 21.1 ms. The 100 ms
ceiling holds, the median meets 50 ms, and the cold open is GTK's first map of the picker's
layer (its rows are pooled already); warming it would mean mapping an invisible picker at
startup, which isn't worth its risk to focus and screen readers. On the testbed (Dovecot, the
real engine, debug): the review's probes `e2e` 34 of 34 (its failing `assert-notify-count 1`
now passes) and `e2e-b` 43 of 43; `e2e-c` 52 of 53, its last step expecting "Review trip" above
"Review invoice" after Remove Reminder, which brings it back without the return to the top (as
in the review's own run); `testbed-remind` 59 of 59 (its first run, before the subject fix,
got "Reminder: Re: Review trip" and nothing else wrong), and 59 of 59 again on the IMAP4rev1
Dovecot (port 15993: no UIDPLUS, MOVE or IDLE, so the rows come back through the Inbox's sync,
under new rows), the mailboxes emptied after (`remind_mailbox.py clean`).

Phase B: `sealwax-fixtures bench-remind` (new, `devtools/fixtures/src/remind.rs`) on a copy of the
100,000-message fixture (`import --count 100000 --seed 1 --folders spread`), release, three
runs on fresh copies at load average 28-29 (other builds); three conversation views open
(INBOX, All Inboxes, Archive), a local account (the store's side of every account's moves).

| measure | result | bar |
| --- | --- | --- |
| a remind of one conversation (1-3 messages) → change event | p50 0.9-2.0 ms, p95 5.2-7.1 ms | ≤ 20 ms |
| a wake of one conversation → change event (back on top) | p50 1.9-2.1 ms, p95 3.3-6.3 ms | ≤ 20 ms |
| one new message, writer transaction, no reminder | p50 1.36-1.45 ms, p95 3.8-4.3 ms | ≤ 20 ms |
| … with 300 conversations back from Reminders | p50 0.98-1.45 ms, p95 2.5-5.5 ms | ≤ 20 ms |
| one remind of 300 conversations (534 messages), one transaction | 75-102 ms | |
| one wake of those 300 | 113-163 ms | |
| `check` with them | 3.3-3.8 s | |
| fork step 3 at open | no recount: three tables and three indexes | |

After the review's fixes (the reminder's references as foreign keys, `ON DELETE SET NULL`),
five runs on fresh copies, each applying the new step 3 at open, at load average 8.4-11
(other tracks' builds): a remind → change event p50 0.88-1.66 ms, p95 2.4-4.5 ms; a wake p50
0.99-1.87 ms, p95 2.6-4.1 ms; a remind of 300 conversations 69-123 ms and their wake 96-165
ms; one insert with them back p50 0.78-2.07 ms; `check` ok. The no-reminder insert moved with
the load (p50 0.91-2.18 ms), so the spread is the machine's, not the keys'.

A first version read every reminded conversation in each recount (the whole `bumps!` join):
with 300 of them a new message's transaction went from p50 0.87 to 2.60 ms. Reading only the
dirty threads' (`DIRTY_BUMPS`) took that away.

On the testbed (`remind_testbed`, release): CREATE, SUBSCRIBE, the new folder's first sync and
the MOVE of a two-message conversation took 152 ms (Dovecot), 148 ms (Dovecot IMAP4rev1, COPY
and `\Deleted`) and 160 ms (Stalwart) from the remind; with the wake 8 s out, the messages
were back in the INBOX 8.05-8.12 s after it (the timer's second, then the move). A debug build
took 130-171 ms and 8.05-8.11 s. After the review's fixes (release): 135-169 ms and 8.11-8.18
s, and `ops_testbed` 12 of 12.

## Also in this part

- The known Stalwart flake of `ops_testbed`'s `randomized_triage_stalwart` (1 run in 6): the
  test's simulated other client got a NO on its own flag change. That client now picks its
  messages again once and then leaves a refused change of its own flags alone (the server keeps
  what it refused unchanged, and the model tracks none of those flags), printing the server's
  words; `testbed_client::Failure::No` keeps them. 12 seed runs since: no difference, no refusal
  met.

## Phase C in the backend

- **Someday** is a wake time, `remind::SOMEDAY` (9999-12-31 23:59:59 UTC): no wake reaches it,
  `Woken::next` leaves it out (the timer sleeps until a nudge rather than looking every minute
  for it), and it sorts last in the Reminders view. No schema change: step 3 shipped.
- **The Reminders view's order.** A waiting conversation's row in the Reminders folder it waits
  in sorts by `remind::wait_key(wake_at)` = `WAIT_BASE + SOMEDAY - wake_at` (`WAIT_BASE` is
  2^40): above any message date, so waiting rows come first there, and the newest-first view
  lists the soonest due first and Someday last. The row's `ConversationSummary::date` is that
  key in that folder's own view, and `remind::wake_of_key` gives the time back (the rows' date
  column uses it, so it is exact with the view's own snapshot). A search through that folder
  dates the row by the conversation's latest message instead (`Summaries::dated`: the final
  review found the key shown as a blank date), so no other consumer meets the key. The recount reads the dirty threads' waits
  (`DIRTY_WAITS`, only while any reminder waits, at a cost that follows the dirty threads'
  messages), `RECOUNT_ALL` does the same in SQL (`waits!`), and `check` compares them. A new
  time marks the conversation dirty (`fork::touch`), so the row moves in the same commit. The
  view itself is the folder's: no new scope, filter or index; paging, diffs and counts are the
  conversation views' own. Message rows (Group by Conversation off) keep their dates; the app
  labels them from `Store::reminders()`.
- **Back from Reminders is Important** (the Split Inbox): `split::classify_reminded` puts every
  message of a reminded conversation in Important, over the user's choice for its sender too
  (they asked to see it again), until it is acted on; the recount reads it from the same bumps,
  `SPLIT_ALL` and the rebuild's `split_rows` too, and the replay's reference.
- **Reminders that can never come back end** at their time (the final review of phase B): a
  due reminder with nothing to bring back and no message of its conversation in an Inbox (its
  Reminders folder went with its messages; or it left the Inbox while its folder was refused)
  ends instead of being reminded with nothing to bump, and goes with that transaction's prune.
  Messages can't be on their way to a folder that is gone, so such a reminder no longer waits
  a day for them.
- **A relisted Reminders folder** (the final review: hidden from IMAP and shown again, renamed
  and back, missing from one listing): once a reminder's own folder row is gone, `back()` (the
  wake, a cancel) and `triaged()` (acting on it in Reminders ends the wait) look in the
  account's Reminders folder by name (`remind::waits_in`, `remind::reminders_folder`, the rule
  `remind()` uses), so a conversation the folder's sync stored again still comes back. A name
  can't match the refused folder's id taken by "Receipts" (phase B's first blocker).
- **The engine** wakes the timer on new mail too (`Shared::nudge_reminders` from
  `forward_new_mail`, after `Event::NewMail`): the wake ends what was answered. A burst of
  nudges is one wake. Measured against the fake server: an answer appended through IDLE to the
  waiting messages back in the INBOX on the server in 22 ms.

## UI

`app/src/remind.rs` (the reminders the UI shows, the commands, the toasts, the notification's
read), `app/src/time_picker.rs` (`pick_reminder`), `app/src/list/row.rs` (the clock mark, the
wake times and their column), `app/src/reader/` (the line), `app/src/notify.rs`
(`notify::reminders`), the sidebar's row, the `superhuman: remind` CSS block and the theme's
`RowRoles::reminder`.

- **`h`** (Remind Me…, `remind-me`) on the selected conversations in the list, the open one in
  the reader, or a row's clock (the quick actions) opens the time picker titled **Remind Me**:
  Send Later's presets with their times (Later today; **Tomorrow**, which Send Later calls
  Tomorrow morning, `time_picker::remind_title`; This weekend; Next week) and **Someday** ("no
  date"); typed text reads live as Send Later's does (`when.rs`: a
  day without a time is 08:00), and "someday" or "never" choose Someday. On a conversation that
  waits already the picker adds **Remove Reminder** ("set for Tue 08:00"). Enter or a click
  chooses; Escape closes. In a view or search of Sent, Drafts, Trash or Junk, whose copies a
  remind never sends away, Remind Me is off and its buttons say why ("mail in Sent can't be
  reminded"; `remind::unavailable`), rather than opening the picker for a toast after the
  choice (the final review). There `h` (in the list and the reader) and a row's clock, dimmed
  as the toolbar's button is, say it in a toast (the second review: `h` did nothing, and the
  clock looked as if it worked): `remind::explain`, through `ScopedShortcuts::connect_refused`,
  which hears of a scoped key whose command is off (other commands' keys go on as before), and
  `RowReminders::off` (the `sw-quick-off` class, half opacity; since the third review the
  reason itself, which the clock's tooltip names as the toolbar's does: "Remind Me… (h) —
  mail in Sent can't be reminded"). A search of every folder (from All Inboxes) sends only its
  conversations' Inbox mail away, so `h` there first reads which selected conversations have
  any (`conversation_members` over the Inboxes; a waiting one counts too, for a new time) and
  leaves the others out; with none left it says "Only a conversation in an Inbox can be
  reminded from here" at once, with no picker (the third review: the picker, then that toast).
- **On a choice** the store's `remind` sends the conversations away (their messages in the
  view's folders; a search's folder, or for a search of every folder the Inboxes) with the undo
  window as `not_before`, and the list's `advance_after` moves the cursor on as triage does, so
  they leave at once (the change event, a millisecond or two after the key) and an open
  conversation moves on to the next; the toast says "Reminder set for tomorrow 08:00" (or "3
  reminders set for …") with **Undo**, which is the triage controller's (`adopt`): `z` and
  Ctrl+Z take it back too. A conversation waiting already gets the new time in place ("Reminder
  moved to …", no Undo: the store keeps no earlier time); in the Reminders view it moves to its
  new place without leaving. The cursor moves on only past rows that leave: a folder's rows
  do, and nothing in the Reminders view, where a remind moves nothing, or in a search of every
  folder, which finds the mail in Reminders as well (the third review: a mixed selection in
  the Reminders view moved the cursor on from rows that all stayed). The new time is the last action all the same (ADR-015: its toast
  takes the last one's place), so `z` and Ctrl+Z have nothing to undo after it:
  `remind::forget_last` drops the triage controller's, the outbox's and the rules' last action,
  as a send or a list change does (`outbox::announce`, `rules::remember`). "Nothing here to remind"
  changes nothing and leaves the last action as it was, as triage's "Nothing to change" does.
- **Remove Reminder** (`remove-reminder`, Shift+E, Superhuman's Mark Not Done; the palette; the
  picker): a waiting conversation comes back to its Inbox now (it leaves the Reminders view), a
  returned one loses its place on top and its clock; "Reminder removed" with Undo. Reminders
  that ended meanwhile (an answer, another window), which the store finds none of, change
  nothing: "The reminder had ended already", and the last action stays as it was, as after
  "Nothing here to remind" (the third review: it said "Reminder removed" and dropped the last
  action). The picker's Remove Reminder says when ("set for Tue 08:00"), or for several set
  for different times how many ("3 reminders"; it named the first one's). It applies
  while a selected conversation has a reminder. Shift+E was free in the List and Reader scopes;
  the audit passes. Mark Not Done from the Archive (back to the Inbox) isn't built.
- **The Reminders view** is the shown account's Reminders folder (`go-to-reminders`, `g h`,
  the palette's Go to Reminders; with several accounts the shown one's, else the first that has
  one; none yet says so in a toast) and the sidebar's **Reminders** row, right under the Inbox
  with a clock and no count (`SidebarAccount::reminders`; on a server whose folders are all
  under the Inbox, `INBOX.Reminders` leaves the Inbox's subfolders for that row). Its rows come in the store's wait
  order, its date column says when each comes back ("17:30" today, "TUE 08:00" within the week,
  "NOV 12", "SOMEDAY", in the list's capitals; spoken "back today 17:30", "back Tuesday 08:00",
  "back someday"),
  sized for those forms (`RowContext::date_width` measures `wake_samples` there: "WED 00:00"
  and "SOMEDAY" are wider than any date), no date headers in Split, and its empty state is "No
  reminders" under a clock, "Press h on a conversation to set a reminder." (one line at 1440
  px); under a filter the filter's own says why ("No unread messages", "No starred messages").
  While reminders wait in that folder that it doesn't list yet (a server account's moves wait
  for the undo window and the server: [Known limits](#known-limits)), it says "On the way",
  "Your reminder shows here once the mail server has moved it." (`RowReminders::arriving`, the
  waiting reminders whose folder it is; the third review: right after `h` it told the user to
  press `h`). Spoken, the wake time says what it is: "Marius, Taxes, back someday", "…, back Tuesday
  08:00". With Group by Conversation off its message rows keep their dates, which their order
  follows (the store sorts message rows by date). The palette's Go to Reminders stands for
  the folder with one account (`go_to::command_places`), so it is one row there. `e` archives
  as anywhere, which ends the reminder.
- **Back in the Inbox**, a reminded conversation's row shows a small clock in the reminder's
  purple until it is triaged (Done, Delete, Junk, Move, Remind again; reading and starring keep
  it, the store's `remind::ends`), and its screen-reader label ends "back from a reminder". The
  clock has a slot of its own before the paperclip on every row of an Inbox (and All Inboxes,
  and the split views) while a conversation of its account(s) is back, so paperclips and stars
  stay in line (the visual QA); without one, and in other views, there is no slot, so an
  Inbox's columns sit where every other folder's do (the final review). Its colour is `RowRoles::reminder` (`--sw-reminder-color`, and
  `--sw-window-reminder-color` on the window's surface): the palette's magenta, mixed toward
  the text colour in 1 % steps until it reaches 3:1 on the window, the list, a hovered and the
  selected row (at most 60 %), else the accent the same way; `theme.rs`
  `the_reminder_mark_reads_on_every_row_surface` holds it in every theme. Adwaita dark #dc8add,
  light #813d9c and Tokyo Night #bb9af7 take the magenta as it is.
- **It isn't marked unread.** A desktop notification **"Reminder: <subject>"** with its latest
  sender (the count mode: "Reminder", "A conversation is back in your Inbox") under the
  new-mail settings and quiet for an Inbox the focused window shows; a click opens the
  conversation as a new message's does (`notify::OPEN_ACTION`, its newest message in the Inbox).
  The UI notifies for a reminder it saw waiting and now sees reminded (`remind::came_back`
  compares reads), and at its first read for those reminded since Sealwax started
  (`Known::since`, set in `remind::install` before the engine starts): the engine wakes the
  overdue ones at its start, maybe before that read, so a reminder due while Sealwax wasn't
  running notifies at the next start, as new mail that came meanwhile does (the second review;
  before, it showed the clock without a notification). One that came back in an earlier session
  doesn't notify again. It notifies once its conversation has a row in an Inbox again, which
  the notification names and a click opens: on a server account the wake only queues the moves
  back, and until the server has made them the messages wait hidden in Reminders, so a came-back
  reminder waits in `Known::awaited`, and every change event that touches an Inbox of its
  account (the moves landing; the Inbox's sync where a server gives no new UIDs) reads the
  reminders again and looks once more, until it is back, its reminder ends (`still_awaited`),
  or a day passes (`BACK_WAIT`: an offline spell, or moves that never land). Reads and their
  notifications run one at a time, so none notifies twice. At the first read a reminder back
  since before the session began is `Awaited::unsure`: in an Inbox already, it notified then
  (or was seen) and stays quiet; not yet there, its moves back never landed (offline at its
  time, then Sealwax quit), so it never notified anywhere, and it does once they land. The
  third review found it never notified on a server account, the owner's only kind: the app
  looked for the Inbox row once, right after the wake.
  Those back in one read notify together (`notify::reminders`): up to three on screen alone,
  more as one "4 reminders" naming their subjects (the count mode: "Reminders", "4
  conversations are back in your Inbox"), as new mail switches to a count. They are tracked as
  new mail is: each goes when its reminder ends (`remind::apply` hands `notify::reminders_now`
  the conversations still back), when the reader shows its conversation on screen
  (`remind::seen`), when it is clicked, and all go when notifications are turned off or to the
  count (the final review: they lingered in Omarchy's history).
- **The reader** shows one quiet line under the subject while the conversation waits: a small
  clock and "Reminder set for tomorrow 08:00" (`time_picker::describe_wake`), refreshed on every
  page laid out (the shown, the pending and the warm ones) when the reminders change.
- **One order everywhere** (the visual QA): the rows' quick actions are Done, Remind Me, Delete,
  Star (Superhuman's check and clock first); the reader's toolbar Done, Remind Me, Delete,
  Move, Star; the select mode's bar Done, Remind Me, Delete, Move, Read, Star.
- **Keys** (one block at the end of `ActionId::ALL`, now 112): `GoToReminders` (`g h`) and
  `RemoveReminder` (Shift+E); Remind Me keeps `h`. `keys.toml` can move them; the guide's table
  is regenerated.

### UI decisions

- The Reminders view is the Reminders folder, ordered by the store, not a view of its own: its
  rows page, update and triage as any folder's. With several accounts it shows one account's
  reminders (as Go to Archive shows one account's Archive); a unified Reminders view would need
  a scope of its own.
- The clock is a mark in the indicators, not Superhuman's purple dot in the unread dot's place:
  a returned conversation can be unread too, and the brief asks for a clock.
- The wake time's column is the Reminders view's own measure, so the Inbox keeps its tighter
  date column. Drafts and Outbox (L1's "Sending Tue 08:00") can take the same path: a second
  sample set for `date_width`.
- The picker names Send Later's Tomorrow morning **Tomorrow** (the second review: BRIEF-C and
  Superhuman name it so, and "Tomorrow morning … tomorrow 08:00" said the morning twice); the
  other presets keep Send Later's names, and Send Later keeps "Tomorrow morning"
  (`time_picker::remind_title`, Remind Me's listing only).

### The final review (phase C)

Fixed: a search showed a blank date for a conversation waiting in Reminders (the store dates
search hits by the latest message, and a row decodes a wait key wherever it meets one, so the
date is never blank); the palette listed Go to Reminders twice once the folder existed. Also:
Remind Me off with a reason where nothing can be reminded; reminders back together notify as
one count past three, and their notifications are tracked and withdrawn; the clock's slot
only while a conversation is back; message rows in the Reminders view keep their dates; the
spoken wake time says "back …"; the empty Reminders view's clock and one-line text; the
sidebar's `INBOX.Reminders`; doc nits. `h` → picker measured again in a debug build (6.5-13
ms; the first round's 51 ms was under load).

### The second review (phase C)

Fixed: a new time for a waiting conversation ("Reminder moved to …", no Undo) left the action
before it undoable, so `z` brought back an archived conversation, or took back a send still in
its undo window; now it is the last action with nothing to undo (`remind::forget_last`, also for
a removal the store found nothing to end; `remind-ui`, and `remind-undo` with a send). And the
Reminders view under a filter said "No reminders" beside the filter's pill; now the filter's own
empty state shows. Also: the picker's "Tomorrow" (`time_picker::remind_title`); `h` and a dimmed
row clock say why in Sent, Drafts, Trash and Junk; a reminder the engine wakes at startup
notifies; a reminder notified again replaces its earlier notification properly (it was dropped
from tracking without being withdrawn or recounted); the engine test
`overdue_reminders_come_back_at_start_and_at_a_reconnect` waits for the whole conversation's moves
back (message 5 too) before it restarts (18 of 18 runs, six at a time, and 12 of 12 runs of the
whole file, four at a time); doc nits. Measured `h` → picker on screen in a release build, under
a load average of about 11 (other tracks building): median 25.4 ms over 21 opens (from the list,
from a settled conversation, right after opening one), 17 under 30 ms, the first (cold) 46.5 ms
and three at 62.8-64.3 ms; the same probe in a debug build at the same time, median 39.0 ms and
at most 102 ms. The 50 ms bar holds for the median; the outliers came with the load (the
debug build's were larger), and a quiet machine wasn't available to measure.

### The third review (phase C)

Fixed, the blocker: "Reminder: <subject>" never notified on a server account (the owner's
Gmail; the testbed's Dovecot with the real engine reproduced it: `assert-notify-count 1` got
0, 4 s after the wake and 3 s after the row was back in the Inbox). The app looked for the
conversation's Inbox row once, right after the read in which the reminder turned reminded, but
on a server account the wake only queues the moves back, and the commit that lands them sets
no `reminders` flag. Now a came-back reminder waits in `Known::awaited` until its conversation
has a row in an Inbox (each change event that touches an Inbox of its account reads again),
its reminder ends, or a day passes; at runtime and at startup alike (the engine's start wake
waits for the connection the same way), and at a start after a session that ended between a
wake and its moves (`Awaited::unsure`). New `remind-server` (104 steps, in CI) runs it against
the store's real op queue through a stand-in server (`remind-server-account`,
`remind-server-run`) and a new start (`remind-new-session`): no notification while the moves
back wait, "Reminder: Review trip" once they land (the conversation's subject, from the
answer that is its newest message; it said "Reminder: Re: Review trip", which the testbed run
found), its click opening it with the clock; then the three cases of a start. Seen failing
with each part taken out: the event trigger (`wait-until assert-notify-count 1`), the earlier
session's reminders (no "Reminder: Ferry tickets"), the quiet one already back (a third
notification). New `testbed-remind` (by hand, `tools/testbed/remind_mailbox.py`) does it end
to end with the real engine on Dovecot, and the review's own probes pass (below).

Also: `g h` right after `h` on a server account says "On the way" instead of "No reminders …
Press h"; `h` on a search of every folder whose conversations have no Inbox mail says why
before any picker; a Remove Reminder that finds every reminder ended says so and keeps the last
action; the dimmed clock's tooltip names its reason; the cursor moves on only past rows that
leave (a mixed selection in the Reminders view moved it on; a search of every folder too);
the picker's Remove Reminder counts several reminders set for different times;
`refresh_commands` reads each selected row's conversation in place (`MessageModel::thread_at`)
against one set of the reminders' threads, instead of copying every selected row and scanning
every reminder for each (a Someday reminder made that cost permanent). The CI self-tests of
this area (38, the review's list with `remind-server`) pass headless; `palette-keys`'s 250 ms
bar for the palette missed once with three run side by side (358 ms) and passed twice alone.

## Known limits

- **On a server account, a conversation reminded just now shows in the Reminders view only
  once its moves ran** (seen in phase C's second round; the owner's Gmail is such an account):
  the remind's moves wait for the undo window (7 s) and then the server, as every move does,
  and upstream's overlay shows a move in its target folder only once the server has it (the
  COPYUID row), as an archived conversation shows in the Archive. So `g h` right after `h`
  lists nothing for it for about 7 s plus the move (the testbed's 123-971 ms, measured with
  no undo window), and says "On the way" meanwhile (since the third review; "No reminders …
  Press h" before); the Inbox, the toast, its Undo and the reader's line know it at once. The
  same goes the other way at the wake: the conversation is back in the Inbox, and notifies,
  once the moves back ran (the testbed's Dovecot: within a second; offline, at the
  reconnect). Local accounts (and the self-tests' `mail-account` accounts) move at once;
  `remind-server-account` holds a server's moves until the script runs them. Closing it needs
  the view to show a pending move in its target (a store change for every move) or a
  Reminders view built from `Store::reminders()`.
- **A conversation without a Message-ID on a server without UIDPLUS** (the final review of
  phase B): the move into Reminders deletes its row and empties its thread, `thread_id` goes
  NULL, and `remind::prune` drops the reminder; once the Reminders folder's sync stores the
  message again nothing moves it back, and it stays in Reminders until the user moves it. The
  owner's Gmail can't meet it (UIDPLUS, Message-IDs).
- **A wake inside the remind's own undo window** (a server account, a wake time seconds away):
  the moves fold to nothing (A→B→A) and the store stays exact, but the engine prepares folders
  before it reads the due ops, so it still creates, subscribes and syncs an empty Reminders
  folder. Mirroring the op queue's folding in `folders_to_prepare` (or preparing only the due
  groups' targets) isn't worth it for that case.
- **A reminded conversation archived from another program isn't ended**: only Sealwax's own
  triage ends a return to the top. Its sort is unaffected (only Inbox rows are bumped), but the
  reminder stays reminded. Ending it when the conversation has no Inbox member would end it
  too while the wake's own move back is in transit on a server without UIDPLUS (the rows
  leave the store until the Inbox's sync), so that needs the transit rule's care.

## Left for later

- Superhuman's "regardless" (a reminder that comes back even when someone answered), Remind Me
  from the composer (Ctrl+Shift+H), automatic reminders, and a Reminders split.
- Undo of a new time for a waiting conversation (the store keeps no earlier time).
- A unified Reminders view across accounts.
- A Remind that the user makes from a message list (message rows) reminds the whole
  conversation, as `Remind` takes threads.
