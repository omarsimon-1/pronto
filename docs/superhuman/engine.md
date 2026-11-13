# Superhuman pass: engine (IMAP sync speed, Gmail)

The owner's account is Gmail over IMAP (app password): INBOX ~27 messages, Sent 201 stored,
All Mail (42k) never synced. What changed, why, and what was measured.

## Changes

1. **Archive never first-syncs All Mail.** A Gmail Archive is a MOVE into `\All`; the op runner
   then passed the target to place the row, i.e. All Mail's first sync (up to 5,000 headers,
   minutes) on the command connection, holding every op and send. `domain::sync::place_after_move`
   skips that pass for a never-synced `\All` folder. The row leaves the Inbox as before; Undo
   inside the toast's window never reaches the server. Undo after it reports `Lost::Unknown`,
   as it already did on real Gmail (Gmail keeps the All Mail UID, below the move's mark).
2. **Passwords held for the session** (`service::Passwords`, zeroized): one keyring read per
   account, shared by every login and reconnect; forgotten on `store_password`, account removal,
   an account dropped from `accounts.toml`, and any refused login (IMAP, POP3, SMTP).
3. **Sent looked in after a send** (`SentLook`): at once when the engine appended the copy,
   1.5 s after the send where the server files it (Gmail), again 4 s later if nothing new.
4. **Body prefetch**: the whole Inbox newest first, then the rest of its conversations (Sent
   replies, Archive copies; never Trash/Junk), any age; one copy per Message-ID outside the
   Inbox (Gmail counts downloads against a daily limit). Planned after an actor's first account
   pass even when it stored nothing, after any folder pass that stored rows, and when
   `[storage]` arrives late. Reader requests go in batches of their own; background batches stay
   under 2 MiB (one larger message goes alone). `body_linger` 60 s -> 12 min, kept warm by NOOP
   every `reuse_check`; the connection goes at once when the account's actor stops.
5. **First rows sooner**: first-sync batches ramp 25 -> 100 -> 200; account pass order INBOX,
   Sent, rest; the IDLE connection starts right after the command connection's login (on a new
   account as soon as the folder list is stored). Only the command connection's pass says
   "synced".
6. **New mail's body on the IDLE connection** (`service::bodies::fetch_arrived`, no EXAMINE):
   up to 1 MiB each, 2 MiB in all, before it idles again. Two review fixes:
   - Notices the body FETCH reads (EXISTS of mail that came meanwhile, flags) mark the
     connection `noticed` (`imap::Connection`, cleared by SELECT/EXAMINE); the IDLE loop then
     passes again instead of NOOP + IDLE, which would never hear of them (ADR-006, S1).
   - Its claims sit in a drop guard (`Claim`): a reconnect, stop or abort mid-fetch hands what
     is unanswered to the body worker, so no message stays "asked" with nobody fetching it. A
     message the reader asks for while its lookup is in flight is queued in front.
7. **Gmail's account-wide HIGHESTMODSEQ** (`X-GM-EXT-1`): the STATUS check compares MESSAGES,
   UIDNEXT, UIDVALIDITY and UNSEEN (against the last account pass's, kept in the actor:
   `SyncOptions::unseen`) and leaves HIGHESTMODSEQ to a sweep every 10 min and at the first pass
   after a login. A read or unread elsewhere shows at the next account pass; any other flag
   change outside INBOX (a star) within 10 min.
8. **SASL-IR** for PLAIN and XOAUTH2 (one round trip less per login). CAPABILITY from the
   AUTHENTICATE OK needs async-imap changes; not done.

## Config and API

- No new config keys. `prefetch_days`: 0 still turns prefetching off; any other value now means
  the whole Inbox and its conversations. The doc and template in `domain/src/config.rs` say so;
  **`docs/USER_GUIDE.md`'s `[storage]` row still says "Inbox mail from this many days"** (left for
  the integrator; no test pins it).
- `SyncConfig::flag_sweep` (10 min, new); `SyncConfig::body_linger` 12 min (was 60 s).
- `SyncOptions::inbox`, `flag_sweep`, `unseen` (new; defaults keep the old behaviour).
- `Store::body_candidates` keeps its signature (query rewritten). No Command or Event changed.

## Measured (fake Gmail, 20 ms round trip, 5 ms per header)

`cargo test -p sealwax-engine --test timings -- --ignored --nocapture --test-threads=1`;
"before" is the same file on base 6a16609 with this pass's fake.

| Step | Before | After |
| --- | --- | --- |
| Login (median of 5) | 111 ms | 88 ms |
| New account (INBOX 300, Drafts 400, Sent 400): first INBOX rows | 1,285 ms | 346 ms |
| ... IDLE on INBOX | 6,415 ms | 1,851 ms |
| ... account synced | 6,260 ms | 6,262 ms |
| Archive (All Mail 1,005, never synced), then a star: star on the server | 5,402 ms | 69 ms |
| Account pass after `\Seen` changed in INBOX | 11 commands, 5 folders, 229 ms | 4 commands, 1 folder (INBOX: UNSEEN moved), 68 ms |
| New mail through IDLE: body stored | 254 ms, one more login | 133 ms, no connection |
| A reply sent: in its conversation | next account pass (up to 120 s) | 228 ms |

Real Gmail fetches headers ~13x slower (200 Sent headers took 17 s), so All Mail's first sync
would have held ops for minutes, and a new account's first INBOX rows come after ~25 headers.

## Tests

- `engine/tests/gmail.rs` (new): archive without All Mail's sync, Undo, reply joins at once,
  INBOX first and early IDLE with SASL-IR, the STATUS skip with UNSEEN and the sweep.
- `engine/tests/keyring_reads.rs` (new): one keyring read for 7+ logins; again after a refusal.
- `engine/tests/actor.rs`: warm body connection; removed account logs it out at once; whole
  Inbox prefetch; body through IDLE; mail arriving during that fetch stored at once; a
  reconnect during it (background, and with the reader waiting) still answered.
- Unit: `place_after_move`, `Passwords`, `refused`, `SentLook`, ramp, reader/background batches,
  queueing an announced message in front, the `noticed` flag on a scripted server, prefetch
  policy, conversation and one-copy candidates through the real store, SASL-IR.
- Changed on purpose: the cut first sync test cuts the first 200-batch; prefetch tests (any age).
  The testbed's twins too (the stabilizer, 2026-10-29): `sync_testbed`'s mid-FETCH first sync
  commits 125 (25 + 100) before its cut and the retry stores 1,075, and `testbed`'s first Inbox
  sync reports 0, 25 and 30 of 30, on Dovecot, IMAP4rev1 Dovecot and Stalwart.

## Left for later

- A sparse All Mail (Undo after the window, full-thread context); a backfill connection so no
  first sync holds ops; moves into large never-opened labels still first-sync the label.
- `load_older` still commits 200 at a time; SASL-IR, early IDLE and the modseq skip are checked
  only against the fake (the ramp is checked on the testbed as well, above).
- A busy body worker keeps the stopped actor's settings until its queue is done (upstream).

## Phase C (track L step 2)

- **The IDLE connection's inline body fetch waits as long as its bytes take on a slow link**:
  `SyncConfig::inline_bodies` (8 s) plus the bytes it claimed at 128 KiB/s
  (`bodies::arrived_limit`): 8.5 s for a typical 64 KiB, 24 s for the 2 MiB it claims at most. A
  fixed 8 s cut a large arrival off on links under about 2 Mbit/s: the IDLE connection connected
  again and the body connection downloaded the message once more (phase B's review). A stalled
  FETCH still ends within it (`actor::a_stalled_inline_body_fetch_hands_the_body_on_in_time`).
