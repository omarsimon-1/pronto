# Contact pane (Superhuman pass, phase C, track K)

Superhuman's right-hand pane, local only: beside an open conversation, the person behind it.
What was built, the decisions taken, and what was measured. Code: `app/src/contact.rs` (the
pane), `app/src/window/margin.rs` (its place), `app/src/pane_cycle.rs` (F6, Next Pane),
`infrastructure/src/store/contact_api.rs` and `reader/contact.rs` (the read),
`domain/src/contact.rs` (who, and the words).

## What it shows

- **Who.** By default the conversation's latest sender who isn't the user
  (`domain::contact::person`). Once the user moves to a message, its sender: `n`/`p`
  (`Reader::connect_moved`), or the keyboard focus going into another message's card than the
  one opening put it in (the window's focus, looked at in an idle: the reader moves the focus
  while it changes the page). Moves count only within the conversation they were made in; the
  next conversation starts from the default. A message of the user's (one of their addresses,
  or a copy in Sent or Drafts, as the list's participants say) never counts; drafts never do.
  A conversation that is all the user's shows whom its latest message was written to (its
  first To, then Cc, that isn't the user, read from the store). That read's answer is taken
  only while its request is the latest one applied and nothing newer waits (`Inner::applied`
  counts the requests): the store has two reader threads, so an answer can come after the
  next conversation's person, and it used to put the wrong person beside it.
- **Contents**, all from the local store: a monogram (`theme::initials` on the address's avatar
  tint, `sw-avatar-tint-N`, the composer chips' colour for the same address), the name, the
  address (a click copies it, toast "Copied ingrid@example.test"), one quiet line ("212
  messages since Mar 2019", "1 message, Oct 2026"; none when the store holds nothing from
  them), **Recent**: up to five other conversations they wrote in, subject and the date of
  their latest message there (a click or Enter opens one), and **Search all mail from
  <name>**. Nothing else; no network, no remote images, no photos.
- **The address** shows whole: a long one goes on to a second line, broken between any two
  characters, with no hyphen added (`insert_hyphens` off). It had a middle ellipsis, which
  showed a gap after the "…" at some widths ("bartholomewmontgo…   eaves@example.test" at
  1,360 px): Pango pads the ellipsis glyph to fill the line, and in the middle that padding
  shows. Past two lines it ends in an ellipsis, where the padding falls at the end.
- **Undated mail** (stored at 0: neither an INTERNALDATE nor a Date header) counts, but dates
  nothing: the first and latest dates are those of their dated mail, the line says "3
  messages" when none is dated, and a Recent row of an undated message has no date. It said
  "since Jan 1970" ("Dec 1969" west of UTC).
- **Recent opens** a conversation as a notification's click does (`notify::select`, now
  `pub(crate)`): in its folder unless All Inboxes shows it, no search or filter hiding it, open
  at their message. The pane goes on showing the same person (the open counts as a move to
  their message), now listing the conversation that was open before.
- **The search link** shows the list (from an open conversation in Focused) and searches every
  folder for `from:<address>` (`MessageList::search_for`): the results, the first selected,
  the list focused, as Enter in the field does.

## Where it lives

`window::margin::ColumnMargin` is the reader stack's `message` page: the thread's scroller and,
in the column's right margin, the pane.

- **The column never moves.** The pane shows when the room right of the column's own gutter
  (`.sw-reader-clamp`'s margin, twice the `xxl` space) is at least `SIDE_MIN` (296 px, its
  padding included). It takes that room up to `SIDE_MAX` (360 px), beside the column.
- The room is the page's own allocation, not the window's, so:
  - Focused with the sidebar hidden shows it from 1,360 px.
  - At 1440 px the pane is x 1104–1440, its content x 1120–1416. The column stays at 360–1080.
  - The sidebar beside the list (a 1,220 px reader) hides it.
  - Split shows it only when the reader pane is as wide (a 1,740 px window with the 380 px
    list).
  - Under 900 px one pane at a time never has the room.
- **Its own scroller, the conversation's scrollbar beside the column** (the visual QA's
  constraint 1). With the pane shown the scroller ends at the column's gutter, so its overlay
  scrollbar sits between the column and the pane, not beyond the pane at the window's edge.
  - The scroller's content (`margin::Inset`) moves right by what the scroller lost, so the
    column stays centred in the whole page, under the toolbar's clamp.
  - Scrolling over the margin left of the column still scrolls the conversation.
  - The inset never counts in a width asked for, and it changes no property in an allocation
    (the margin sets it before it allocates the scroller).
- **The wheel over the pane.** The pane's scroller scrolls it while it has more to show. A
  wheel or a touchpad over it otherwise (it fits, the usual case), or over the room past it,
  scrolls the conversation, as the whole page did before the pane: `ColumnMargin` takes what
  the pane leaves (its own scroll controller, in the bubble phase) and moves the thread's
  adjustment. A wheel's notch goes as far as over the column (GTK's step, the page's height
  to the power 2/3: 89.8 px at 1440x900, measured the same over both); a touchpad's pixels
  as they come, without the fling GTK adds at the end of a swipe. Over the column its own
  scroller has it (a motion controller says where the pointer is), so nothing scrolls twice.
- **Composer cards** (the visual QA's constraint 3): the pane hides while a card is open or
  expanded (`Dock::connect_open`), since an open card covers its lower half. A card that is
  only minimized leaves it. The other choice, the card left of the pane, would have covered
  the column.
- **Right to left**: mirrored, the pane in the column's left margin, the column where it was,
  and its padding too (`.sw-contact-pane:dir(rtl)`): 24 px from the window's edge and 40 px
  from the column, as left to right.
- **The pane hides with the keyboard focus in it** (a narrower window, Contact Details off):
  the focus goes to the conversation's message (`Reader::focus_message`), where GTK would have
  left it on the window's first control. That is no move of the user's: the pane keeps whom it
  showed.
- Behind a Focused list the reader is warm (mapped, drawn into nothing), and so is the pane.
  Enter shows it complete.
- `reader-column` (the 1,200 px clamp's child) is as wide as the thread's column while the pane
  shows: the margin is the pane's. `window-layout.sealwax-test` checks both widths.

## Contact Details

- Palette **Contact Details** (aliases person, sender, profile, right pane) and **`i`**, in the
  list and the reader. On by default; `layout.toml`'s `contact = true|false` remembers it.
- The key audit: `i` is free in both scopes and unshifted on the US and Norwegian layouts.
  Superhuman's own `i` expands an AI summary Sealwax doesn't have, and its pane key `0` swaps
  in a calendar, which Sealwax doesn't have either. Shift+I stays Mark as Read.
- Not in the main menu: the menu tests pin its sections, and the other phase C tracks change
  them too.
- Turned on where the margin has no room (a narrower window, Split's narrower reader), it says
  so: toast "Contact details show in a wider window" (`Window::contact_has_room`). Turned
  off, nothing needs saying.

## Next Pane and Previous Pane (F6)

The contact pane must be reachable by keyboard without Tab going through every control of
the conversation (14 presses in the trip sample), and the split Inbox (track S) gives Tab to
its tabs in the list; S left pane cycling to this track (`split.md`).

- **Next Pane** (F6, Ctrl+Alt+Right) and **Previous Pane** (Shift+F6, Ctrl+Alt+Left), window
  actions `win.next-pane` and `win.previous-pane` in K's block of `ActionId::ALL` (113 with
  Contact Details). Application accelerators: they work anywhere in the window, a draft's
  editor and the search field included.
- **The stops**, those on screen, in reading order, round again (`Window::pane_stops`): the
  sidebar while it shows (beside the mail or over it), the list while it shows, the
  conversation (the reader's pane: its toolbar, the thread, the reply under it), then the
  contact pane while it shows beside an open conversation. Focused mode's open conversation
  is two stops, the conversation and the pane; Split at 1,920 px three; a Focused list one
  (with the sidebar, two).
- **Where each takes the focus**: the folder shown (`Sidebar::focus_current`), the list's
  selected row (`MessageList::focus_selected`, as Back does), the conversation where the
  user left it (`pane_cycle` notes the focus there, in an idle and as F6 leaves; only while
  the reader still shows that message), else the open message (`Reader::focus_message`),
  else the reader's Back or its page's first control; the contact pane's address
  (`ContactPane::focus_first`). A stop where nothing takes it is passed.
- Into the pane and back with F6 and Shift+F6 is no move of the user's: the focus comes back
  to the card it left, so the person shown stays.
- A sidebar lying over the mail closes as the focus leaves it, as picking a folder there
  closes it. F6 never opens a hidden sidebar or the reader behind a Focused list.
- From outside every stop (a composer card, nothing focused) F6 goes to the first and
  Shift+F6 to the last. Over the window's content (the palette, a picker, a dialog) they do
  nothing: those keep the focus. Run from the palette, the palette gives the focus back
  first, so it moves from where it was.
- **The key audit**: F6 and Shift+F6 are free (not in `keymap::GRABBED`, `FOCUS_KEYS` or
  `TEXT_KEYS`; nothing binds them; there is no `GtkPaned`, whose F6 is its own). An action
  with only F-keys fails the audit (`Fault::OnlyFunctionKeys`: many laptops send media keys
  unless Fn is held, the owner's among them), so each has Ctrl+Alt with an arrow too: free,
  not a text key (those are Ctrl and Ctrl+Shift with the arrows), on every layout, and
  Omarchy binds no Ctrl+Alt arrow (only SUPER combinations, Ctrl+Alt+Tab and
  Ctrl+Alt+Delete). The arrow says the reading order, as the panes lie left to right; right
  to left it is the same order.

## Speed

- **After the frame.** `ContactPane::show` records whom to show. The pane applies it after the
  frame clock's next paint, in an idle, so the reader's swap paints first and `j`, `k` and
  Enter never wait on it.
  - Who they are (name, address, monogram) shows at once.
  - What the store holds from them comes from a cache by address (256 people, the least
    recently read from the store out first; a read again moves to the back). An entry made
    old by a later commit is shown while it is read again.
  - Otherwise it is read off the GTK thread and shown when it comes; an answer for someone no
    longer shown is dropped.
  - While the pane isn't laid out (no room, Contact Details off) nothing is done. The pane
    catches up when it maps again.
- **The read** (`Store::contact(address, 6)`, one snapshot) reads the fourth fork step's index
  alone: `fork_message_split_by_sender` on `fork_message_split(sender, date, thread_id,
  folder_id, split) WHERE split <> 0`, shown messages by `split::sender_key`.
  - **Its name is new.** Track C's phase-B builds of the second step made an index
    `fork_message_split_sender` on `sender` alone (later taken out of the step), and their
    stores (`_scratch/c-backend/fx100k`, `r-remind/bench`, `review-c2/bench`) still carry
    it. Under that name `CREATE INDEX IF NOT EXISTS` kept the old index while `fork_schema`
    became 4, and neither read was covering any more (16 + 5.7 ms for the busiest sender
    there). The step now drops `fork_message_split_sender` where it is (each ingest paid a
    write for it) and makes its own under the new name, in the step's one transaction. The
    owner's store has no fork tables yet, so it gets the step as written.
  - The count is a `DISTINCT (date, thread_id)` over the sender's range, in the index's order
    (no temporary B-tree), outside Trash and Junk and detached accounts (as a search of every
    folder).
  - The conversations are a walk from the end of the range, stopping after six distinct ones
    (at most 4,096 entries).
  - Copies of a message (Gmail's labels) come side by side in the walk. Each conversation
    opens at the copy `conversation::rank` puts first (an Inbox's before a label's).
  - `split` is in the index because SQLite reads a partial index's own condition from the row
    unless its column is there. Without it neither read was covering.
  - The writer keeps the index as it keeps the split rows; the step changes no derived table
    (no recount).

### Measured

`sealwax-fixtures bench-contact` (release, on a copy of track C's 100,000-message fixture,
imported with `--folders spread`, load 9–15). Warm is the page cache warm (the steady state);
cold is the store's files evicted, then reopened:

| | warm p50 / p95 / max | cold p50 |
| --- | --- | --- |
| the busiest sender: the fixture's own account, 21,043 rows | 1.97 / 2.72 / 3.96 ms | 12.4 ms |
| the busiest correspondent: a newsletter, 961 rows | 0.18 / 0.23 / 0.30 ms | 3.4 ms |
| a middling sender, 38 rows | 0.05 / 0.06 / 0.18 ms | 1.2 ms |
| the least busy, 8 rows | 0.05 / 0.08 / 0.27 ms | 1.3 ms |

- The pane never shows the user's own address. A user's alias it doesn't know would be the
  first row, still under the 5 ms bar.
- On a fresh copy of the fixture as track C left it (`fork_schema` 2, with the old
  `fork_message_split_sender (sender)`), the first open drops that index and builds the new
  one (`fork_schema` 4; `check` consistent; both reads `COVERING INDEX
  fork_message_split_by_sender`, no temporary B-tree). Then, at a load of about 8: the
  busiest sender warm p50 2.46 / p95 3.53 / max 3.95 ms, the busiest correspondent 0.22 /
  0.45 / 0.79 ms (the review measured 16 + 5.7 ms and 2.3 ms there with the old name), the
  step at open 238 ms, once.
- The fork step at open, building the index: p50 211 ms, once (store open after it: 2.8 ms).
- The index: 4.2 MiB (the split rows themselves are 5.0 MiB).
- What it costs a new message, the writer's transaction with and without the index in
  interleaved rounds (160 each): p50 0.60 vs 0.54 ms, p95 1.85 vs 1.70 ms.
- The store's `check` stays clean.
- The folders are filtered with `folder_id IN (subquery)`: a literal `NOT IN` or a chain of
  `<>` measured the same (2.4–2.6 ms best of 30 for the 21k range, against 1.8 ms with no
  filter). The `DISTINCT` is about 1 ms of it.

`tests/selftest/perf-contact.sealwax-test` (release, Broadway, 2,000 messages, Focused at
1440x900, the pane shown and Contact Details off in alternating blocks within one run, load
about 5). Key to the next conversation painted:

| | with the pane | without |
| --- | --- | --- |
| `j` p50 / p95 (30 each) | 26.6 / 78.2, 31.8 / 81.1 ms | 30.3 / 104.7, 29.1 / 91.9 ms |
| `k` p50 / p95 (15 each) | 33.8 / 93.5, 36.1 / 161.4 ms | 34.3 / 102.1, 31.7 / 88.3 ms |
| archive p50 / p95 (10 each) | 20.5 / 40.5, 40.8 / 160.4 ms | 36.9 / 96.6, 32.4 / 99.4 ms |
| Enter, key to frame, median of 6 | 22–23 ms | 21 ms |
| Escape, key to frame, median of 6 | 48–50 ms | 47 ms |

The differences are within the runs' own spread: what is left is Broadway's software frame
(30 Hz) and its key path, as in docs/superhuman/reader.md. A second run, at a load of about 22
(other tracks building), put every block, with the pane and without it, at 51–115 ms p50: no
difference there either. The pane's own work runs in the idle after the frame that shows the
conversation; behind a Focused list it runs there too, so Enter finds it filled.

## Accessibility

- The pane's content is a **region named "Contact details"**, described as "Who the open
  conversation is with, and their other conversations". ARIA calls this a complementary
  landmark; GTK 4 has no complementary role and its `landmark` role is abstract (setting it
  is a critical), while a named region is a landmark in ARIA too.
- **Keyboard**: the pane comes after the conversation in the Tab order (its address, its
  Recent rows, the search link), and Next Pane (F6) reaches its address from anywhere.
- Every control has a name:
  - the address button: "Copy address ingrid@example.test", with the tooltip "Copy address";
  - the Recent list: a list "Recent conversations"; each row a list item holding a button
    "Subject, Oct 27", so a screen reader says it can be pressed (a button that was itself
    the list item was announced as a list item alone);
  - the search link: its words.
- "Recent" is a heading; the monogram is presentation.
- **Pane cycling**: Next Pane and Previous Pane, above.

## Decisions and limits

- **"With this address" means "from".** The count, the first date and Recent are the
  messages the person sent, as the search link's `from:` finds them.
  - A conversation where only the user wrote to them isn't listed.
  - The store indexes no recipients outside the full-text index. A To/Cc walk would need a
    second index and a dedup across both.
- Copies are one message: the same conversation at the same date (the split rows carry no
  Message-ID). Two messages of theirs in one conversation within one second count once.
- The name is the one the shown message gave. The address is exact (no aliases merged), as the
  Split Inbox's sender choices are.
- The history line's month is the locale's abbreviation, without its full stop, with the
  year. Counts past 9,999 group their digits with a narrow space.
- Hover brightens the address and the Recent rows to the text colour, with no fill (no boxes).
  The keyboard's ring sits a little outside the words (`outline-offset`), so the words still
  line up with the name.

## Tests

- domain `contact::tests`: who (default, moves, the user's own, drafts, recipients), names,
  the line, the query (and that it parses as one sender). Also `layout`'s `contact` round
  trip, and `actions`' key, kind, scope and category for Contact Details.
- store `contact_tests`:
  - one count per message: copies, Trash, Junk and a hidden message left out;
  - first and latest dates; undated mail counted, dating nothing;
  - Recent newest first, at the Inbox's copy;
  - the limit, any case of the address, the user's own, a stranger's, no address;
  - the index following mail that comes, hides, shows again and goes, and a detached account;
  - the fork step on a store of step 3, applied once;
  - the fork step on a store of step 3 that carries the old `fork_message_split_sender
    (sender)`: dropped, the new index in its place, both reads covering on it (on the old
    step's SQL it fails as the review found: the old index kept);
  - both reads covering on the index, no temporary B-tree (`EXPLAIN QUERY PLAN`).
- app `contact::tests` (whom the pane shows from a reader's conversation, the cache's bound
  and order), `pane_cycle::tests` (the stops' order either way round, from outside them, one
  stop, none),
  `window::margin::tests` (the room at 1440, 1360, 1358, the sidebar's and Split's readers),
  and `actions::tests`: `i` runs Contact Details, and after `g` it is still Go to Inbox (two
  tests used `i` as a key nothing had; they use `w` and `t` now).
- domain `actions::tests`: Next Pane's and Previous Pane's keys, scope, category, kind and
  that every key is an accelerator; the audit and the USER_GUIDE table (`user_guide_keys`)
  hold them.
- `tests/selftest/contact-pane.sealwax-test` (180 steps):
  - laid out behind the Focused list;
  - Enter shows it with Tomas, its contents, the column at 360–1080 under the toolbar, 336
    px wide, everything labelled, the Recent rows buttons, the window's accessibility audit
    clean;
  - F6 from the conversation lands on the address, Shift+F6 goes back to the message body it
    left; either key twice comes round; Ctrl+Alt with the arrows the same;
  - `p`/`n` follow the message moved to; into the pane and back with F6 keeps the person;
  - the address copies (clipboard and toast);
  - Tab reaches it; hidden with the focus in it at 1280 px, the focus goes back to the
    message, and the person shown stays;
  - 1360 shows, 1358 hides; `i` turned on there says why nothing shows;
  - `i` and the palette's command toggle it, with `layout.toml`;
  - right to left;
  - a composer card hides it;
  - Recent opens a conversation and the person stays;
  - a notch of the wheel over the pane scrolls the conversation as far as over the column
    (89.8 px), down three notches and back up to the top;
  - the search link lists their conversations from every folder;
  - an all-mine conversation (in Sent) with its recipient's read held, `j` to the trip, the
    held answer let go: the trip's Tomas stays (on the old guard it fails: "got Selftest");
    `k` back, unheld: Selftest, whom it was written to;
  - Split at 1440 has no room, at 1920 it does; F6 there goes list, conversation, pane, round,
    Shift+F6 the other way; with the sidebar beside them (the pane then has no room) the
    folders come first.
- New commands: `assert-contact`, `assert-contact-history`, `assert-contact-recent`,
  `assert-contact-column`, `assert-contact-side`, `assert-clipboard`, `contact-hold`,
  `assert-contact-held`, `wheel-over`, `assert-wheel-alike` (`app/src/selftest/contact.rs`;
  the wheel is Broadway's scroll event, `broadway::wheel`). The column's checks read its sides only: a long
  conversation runs past the window, which `assert-right-of` counts as off screen.
- Updated on purpose: `window-layout` checks the column's 1,200 px room with Contact Details
  off, and 720 px with it on.

## Measuring again

```sh
cargo build --release -p sealwax -p sealwax-fixtures
cp fixture/store.sqlite /scratch/copy/store.sqlite   # a 100,000-message fixture, on a copy
target/release/sealwax-fixtures bench-contact --store /scratch/copy/store.sqlite --cold 5 --warm 100 --inserts 320
tools/headless.sh 67 target/release/sealwax --self-test tests/selftest/perf-contact.sealwax-test
```
