# Reader: speed and look (Superhuman pass)

The fork's reader pass: opening a conversation and moving between conversations is one swap,
and the thread reads calmly. Upstream's ADR-011 (views, layout gate) and ADR-016 (conversation
view) still hold; this records what the fork changed, why, and what it measured.

## Speed

- **Layout gate wakes on events, not a poll.** A conversation is laid out unseen until the
  bodies its first screen needs have their heights. The wait used to re-check every 50 ms,
  waking only on WebView events, so a page laid out by GTK sat up to 50 ms. It now wakes on GTK's
  layout of an unseen page (the frame clock's layout phase) and on every body event; a 150 ms
  backstop only guards against a missed wake (`reader/show.rs` `lay_out`, `live.rs` `on_layout`).
- **Conversations laid out ahead.** `Reader::prefetch(ids)` takes the messages next to the
  selection (the app passes the list's next and previous rows from where it reports the
  selection, `application.rs` `prefetch_neighbours`; the list area can pass conversation anchors
  through the same hook). Once the shown conversation is painted, their conversations are read,
  built and laid out unseen, one at a time; the conversation the selection leaves stays laid out
  too (at most 3 pages). `show` of a message whose page is ready swaps it in within the frame
  (`Shown.ahead` says so); one still being laid out finishes as the target. A page laid out ahead
  is never marked read, is hidden from screen readers, takes no pointer or focus, and the
  self-tests' widget walks pass over it (CSS class `sw-reader-staged`). A store change touching
  one drops it (it is laid out again while it is still a neighbour); a page whose expanded
  messages would say "Downloading…" is never kept (the download flow stays as it was).
- **Unseen pages are drawn into an empty clip.** WebKit hands a view its next frame only once
  the previous one was drawn, so views of a page that is never drawn stall after their first
  frame: a swapped-in page showed blank bodies for a frame or more, and a view reused at another
  width never re-reported its height (a 3 s layout timeout, found by `reader_perf`). The stage
  now snapshots its unseen pages under an empty clip; a page counts as ready only when its
  first-screen bodies have painted.
- **Same conversation in place.** Another message of the shown conversation is shown on the same
  page: the messages a fresh `show` would expand are expanded (bodies already shown kept), the
  others collapsed, the heading retitled, the column scrolled to it, the focus carried to it.
- **Held keys coalesce.** `show`s closer than 60 ms read only the conversation the key rests on
  (40 ms of rest); a page laid out ahead still swaps in at once.
- **Views stay warm.** `MAX_LIVE_VIEWS` 4 → 8 for the process, `PAGE_VIEWS` 4 for one page (the
  shown conversation's own share is unchanged), idle views are kept (`IDLE_LIMIT` =
  `MAX_LIVE_VIEWS`: making one costs about 16 ms on the show that pays it), 2 prewarmed.
  Order of the views: what the column shows, the conversation being shown, those laid out ahead
  (next first), then the shown conversation's bodies near the visible part.
- **Parse once, keep what was prepared.** `sealwax_domain::message::Parsed` parses a raw message
  once for every part the reader reads and for `reading::inspect` (an HTML message with five
  inline images was seven parses; a 14 MB message is about 16 ms a parse). Prepared bodies are
  kept in an 8 MiB LRU (`reader/cache.rs`) by message and the stored raw message's blob key: a
  message shown again is checked against the store (one detail read) instead of read and parsed;
  a body downloaded or evicted is another blob, a removed message is dropped. Measured heights
  are kept by message, column width, body style, remote images and quote collapse (cleared on a
  theme change), so a view attaches at its final height and the page has its geometry before
  WebKit answers.
- **The list reports its selection before the redraw** (`list/mod.rs` `queue_settle` at
  `HIGH_IDLE`): the reader's swap paints in the same frame as the list's new selection instead
  of one frame later (one 30 Hz Broadway frame headless, 8 ms at 120 Hz live).
- **Fix: a plain-text body stayed cut after the column narrowed.** A GtkTextView answers a height
  for the width it last had; `reader/widgets.rs` `ReadingBox` measures it again once it has its
  new width. `tests/selftest/reader.sealwax-test` narrows within one layout and checks the last
  line (`assert-reader text-whole`); without the fix the check fails.

## Look

- The thread is one centred column, 48 em of message text wide (about 720 px), themed and
  plain bodies at about 70 characters (`body{max-width:70ch}` in the themed stylesheet, the
  plain view's `ReadingBox`), newsletters on their paper as designed.
- No box around a message: hairlines between messages and rows; quiet one-line rows (sender,
  snippet, time; the dot and bold sender only for an unread one); no avatars.
- Runs of 3 or more read messages fold into "N more messages", keeping the conversation's
  first message and the one before each expanded message (`FOLD_LEAST`, `KEEP_BEFORE`); a long
  fold opens 50 at a time. Expand All reveals the folds of a conversation up to 100 messages.
- The subject is a calm heading (1.5 × the title size, bold, -0.01em) that is never cut in half:
  a conversation opens at its top whenever scrolling to the message would leave the heading
  partly in view.
- Sender block: the name (medium), "to me, Tomas Berg and 2 others", the date at the right.
  Details, Reply, Reply all and Forward sit beside the date and fade in (opacity) while the
  pointer or the focus is in the card; they stay in the Tab chain. The address shows the same
  way beside the name.
- Quoted history is always collapsed behind a small "•••" pill, HTML (QUOTES_JS, themed through
  `--sealwax-quote-fill`/`--sealwax-quote-text`) and plain text alike; a message that is all quote
  keeps it.
- The quick reply is one quiet line, "Reply to Tomas Berg…", that opens (label, four rows, the
  hint and Send) when it takes the focus or holds text, kept in view as it opens. Visual only.
- Toolbar: Archive (the check mark), Delete, Move to (a folder), Star, More, "n of N" with
  previous and next; Mark as Unread, Move to Junk / Not Junk and Print moved into More, their
  titles following the selection. Icons are ones Adwaita has; the toolbar icons are quiet until
  hovered. No GTK Box spacing doubled with CSS border-spacing in the reader.

## Measured

Headless (Broadway, software rendering, scale 1), release builds, this machine, while five other
agents built in parallel; baseline = the pristine tree's binaries, run back to back.

j/k/archive, key → next conversation painted (`measure-leave j 60, k 30, e 20` on the 2,000-message
store, `scratchpad/shots/perf/ab-template.sealwax-test`), three interleaved runs each, load 2.5–8:

| | next p50 | next p95 | reader ready p95 | rounds waiting ≥ 45 ms on layout |
|---|---|---|---|---|
| baseline | 40.0–44.6 ms | 83.0–84.2 ms | 53.5–54.1 ms | 12–13 of 110 |
| fork | 27.8–29.5 ms | 50.5–54.2 ms | 3.0–3.9 ms | 0 of 80–82 |

Per key (fork, p50/p95): j 26.1–27.2/50.5–51.7, k 25.5–29.3/42.5–52.4, archive 26.0–28.4/44.4–51.6 ms
(baseline j 47/84–87, k 35–37/79–81, archive 38–44/58–70). What is left is Broadway's key path
(about 9 ms) and its 30 Hz software frame; the reader's own part is about 0 ms (ready p50 0.0).
The same keys on `tests/selftest/perf-reader.sealwax-test` (a fresh fixture store), back to back
at a load of 7–11: baseline j 50.8/93.8, k 44.8/107.2, archive 49.1/187.2 ms; fork j 28.7/72.3,
k 27.2/45.0, archive 26.2/44.5 ms (one quieter run of the fork: j 12.5/19.7, k 12.9/20.4,
archive 10.3/30.2 ms).

`reader_perf --count 120` (`Reader::show` → on screen, second pass), load about 2–3:

| | p50 | p95 | max | first paint p50/p95 |
|---|---|---|---|---|
| baseline | 19.2 ms | 64.2 ms | 83.4 ms | 41.0 / 98.6 ms |
| fork, cold | 11.8 ms | 33.8 ms | 51.5 ms | 25.3 / 65.2 ms |
| fork, `--prefetch` | 0.1 ms | 25.2 ms | 51.0 ms | 23.9 / 77.7 ms |

`reader_memory` (process tree PSS, MiB; `--prefetch` lays out the neighbours of each of the 200
opens): baseline open 269–278, scrolled 282–292, peak 282–293, growth 22–25; fork open 266,
scrolled 282, peak 282, growth 37 (app 21: the prepared-body cache filling); with `--prefetch`
peak 286, growth 15. Over 600 opens the tree stays within about ±15 MiB (no leak). `TREE_BAR`
is now 450 MB (the fork's budget). Headless numbers are at scale 1: on the owner's scale-2
panel a rendered view costs about four times the pixels, so re-measure live with
`reader_memory --live --prefetch` before raising `MAX_LIVE_VIEWS` further.

## Tests

- New `tests/selftest/reader-ahead.sealwax-test`: neighbours laid out ahead, j/k/archive swap
  them in, a page laid out ahead stays unread and invisible to lookups and the a11y tree,
  another message of the shown conversation shown in place.
- New checks: `assert-conversation ahead N` and `shown-ahead BOOL`, `assert-reader text-whole`,
  `assert-reader-more ACTION TITLE` (the commands that moved into More).
- Updated on purpose: the 150-message fold counts (147 folded, 2 rows), the newest-first order
  test reveals the new fold first, the quick reply's closed placeholder, the process view cap
  (8) and a swapped-back page loading no remote image again in `reader.sealwax-test`,
  `triage-actions` checks Junk and Mark as Unread in More, the themed WebView snapshots.
- New unit tests: fold policy, opening scroll, prepared and height caches, toolbar split,
  `message::Parsed` (also checked against `part` on every fuzzed sample).

## Measuring again

```sh
cargo build --release -p sealwax -p sealwax-fixtures --example reader_perf --example reader_memory
tools/headless.sh 25 target/release/sealwax --self-test tests/selftest/perf-reader.sealwax-test
READER_PERF_ROWS=1 tools/headless.sh 26 target/release/examples/reader_perf \
    --store .local/reader-examples/perf/store.sqlite --count 120 [--prefetch]
tools/headless.sh 26 target/release/examples/reader_memory [--prefetch] [--opens=600]
```

Run a baseline binary back to back with the new one: headless numbers move with the machine's
load (the baseline's j p95 read 84 ms at a load of 3 and 296 ms at a load of 20).

## Phase C, track L step 2: the reader's leftovers (2026-10-29)

BRIEF-C's L2 list, phase B's polish review (`scratchpad/phaseB/p-polish.md`) and the phase A+B
visual QA's L2 issues. Shots: `scratchpad/shots/l/l2-*.png`.

### Whom a reply answers

- **The latest message, unless the user moved** (major). A conversation opens at its first
  unread message, whose card takes the focus, and `r` answered that card while the closed box
  under the last message named the latest sender ("Reply to Tomas Berg…" opened as "Reply to
  Ingrid Solberg"). Now `r`, `a` and Enter answer the box's target, the latest message that
  isn't the user's own, unless the user chose another message (`user_message`, `Inner::choice`):
  a move (`Page::current`, `Inner::moved_to`), and only the user's own moves count: `n`/`p`
  (`step`), a press in a message (a capture-phase `GestureClick` on its slot, any button; the
  one the conversation opened at too), or the keyboard focus going from one message of the page
  to another (Tab, Shift+Tab, WebKit's own Tab out of a body, over a fold's "N earlier
  messages" row too: the window's `focus-widget`, read in a high-priority idle in the order it
  moved; `Inner::focus_went`). Where the reader puts the focus itself is none
  (`Inner::give_focus` notes it where it put it): opening a conversation in its selected
  message, putting it back after a send or Escape (`leave_quick_reply`), moving it into the
  message shown in place (`shown_in_place`), the fold's last step handing it to a row it
  revealed; nor is a Tab in from the list, a popover or a window giving it back (the review's
  probe: the focus the reader gave the answered message after a send pinned the box there, so a
  newer reply that came in wasn't the one Enter answered). Forward and a card's Reply
  (`current_message`) take the same choice, else the box's target, as `r` does.
- **With a row per message the opened row is the choice** (the review's third round: with Group
  by Conversation off, Forward from Tomas's older row sent his latest message, and Reply answered
  it, against `reply-commands`' "the selected message"). While the list's rows are messages
  (`Inner::message_rows`) the selected message is the user's choice when they made no move: the
  box answers it and names its sender, and `r`, `a`, Enter and Forward act on it, from the list
  and in the reader, Split and Focused.
- **Left and opened again, a conversation starts afresh** (`Inner::start_afresh`): its move is
  forgotten and its empty box answers the latest message again (the selected one with message
  rows), whether another conversation replaced it (`keep_warm`) or the list took its place
  (`Reader::left_screen`, on the window's `reader-on-screen` going off: Escape in a Focused or
  narrow window, or the select mode's page there; in Split the batch page takes the page's place,
  which `keep_warm` handles). Kept behind a Focused list, the trip opened again with
  Enter still answered the message `p` had reached (the review). Shown in place at another of
  its messages (another message row, a notification's click), it starts afresh there too, its
  empty box re-targeted (`retarget`).
- **`n`/`p` start where the user is**: from their last move, else from the message being read,
  whose open card (its body, header, actions or attachments) holds the keyboard focus
  (`reading`), else the selected one. After a send the reader puts the focus in the answered
  message's body, and `p` went from the selected message instead, skipping the one above it;
  with the focus on that message's attachments it still did until the review's third round
  (`reading` took only bodies; `reader-conversation-order` checks a header). A row holding the
  focus isn't read: a click on a fold's row leaves it on a row it revealed, and `n` went on from
  there (`reader-conversation-order`).
- **Enter right after a send replies again** (pre-existing): the reader put the focus back on
  the answered message's collapsed row (a reply that came in while the conversation was open),
  a button Enter expanded. It goes to a body now, the answered message's, else the selected
  one's (`Card::body_target`). A message shown in place takes the focus in its body the same
  way once the body is in (its header held it, where Enter folds the card).
- **The closed box names whom they answer**: an empty box follows the user's move at once
  (`follow_choice`: "Reply to Sana Malik…" after `p`), and Escape in an empty box and a reply
  arriving keep it on the user's message (`user_message`). A box with text, chosen recipients or
  a send keeps its message; a draft is never answered.
- **Forward never takes a draft** (the review's third round; pre-existing): `n` onto the user's
  unsent reply, then `f`, forwarded the draft. `Inner::choice` is `user_message` now, which skips
  drafts as Reply already did, so Forward and a card's Reply take the box's target there
  (`reply-target`). `Reader::chosen_message` never names a draft either.
- **The fold's last step keeps the focus in the reader** (pre-existing): it handed the focus to
  the revealed message's slot, a box that can't take it, and the focus fell to the message list.
  It goes to the first revealed row now.
- `reply-target.sealwax-test` (new) covers it, the review's probes included (a send, then a newer
  reply: the box, Enter and `r` answer it; Shift+Tab from one message to another is a move; left
  with Escape and opened again with Enter; a first press in the message the trip opened at; `p`
  after a send; a row per message in Focused; a notification's click on the open conversation);
  `reply-commands` changed on purpose: from the list, Reply answers the message whose card was
  clicked last (it answered the one `p` reached before the clicks), and gained an older
  message's row (Split, message rows); `reader-conversation` gained Tab over the fold's row and
  the fold's last step; `reply-quick` changed on purpose: with a row per message an empty box
  stays on the row the user opened when a reply arrives (it followed the reply), and follows
  each reply once the rows are conversations (a grouping switch re-targets it:
  `Reader::set_message_rows`).
- **For the owner**: with conversation rows, Forward (`f`, from the list or the reader) now sends
  the message Reply would answer, the latest that isn't the user's own, not the first unread one
  the conversation opens at (Superhuman's). In the trip that is Tomas's one-line reply rather
  than Ingrid's message with the map; a press in her message, `n`/`p` or Tab picks hers.

### What the reader shows

- **A draft says so** (major): "Draft" in the draft colour (`--sw-window-draft-fg-color`, the
  list's role on the window surface) where its date goes, on its card and its collapsed row
  (`conversation::DRAFT`, `is_draft`; `CardDate::draft` keeps `redate` off it), and its card's
  Edit draft in view without a hover (`.sw-card-edit`). It read as a sent message.
- **The heading is the conversation's subject** as its list row says it: the store's
  `Conversation::subject` (`threads.subject`, read with the conversation) heads the page and
  stays when another message of it is shown in place; a merge that changes it retitles it; the
  selected message's subject only when the thread has none, or while the list's rows are
  messages (Group by Conversation off, `Reader::set_message_rows`: as the opened row says it;
  `show::relabel` on the pages shown and laid out ahead). The newsletters opened under
  "Orbit Coffee — issue 47" below a row saying "Harbor & Pine — issue 40".
- **One date style with the list**: collapsed rows "09:30" today, else "OCT 28" (no
  "YESTERDAY"), before this year the list's short date ("8/30/25", `list::row::numeric_date`);
  card headers "09:30 (2 h ago)" today, else "Oct 28, 21:41" (no "Yesterday 21:41"). A card
  collapsed into its row says its date as of then (`collapse_item` redates it; a page laid out
  at 23:50 and read after midnight said "23:40" there).
- **Opening the reply never cuts the heading** (`live::reveal_delta`): the column scrolls what
  the reply needs; where that would bring the heading's box nearer the toolbar than
  `HEADING_ROOM` (4 px; its letters start lower), the heading goes out of view whole, the open
  reply at the conversation's foot keeping room below it for that (style.css); only where the
  column can't scroll that far does the reply keep less room below it (12 px to none). Kept
  first, that left the heading 4 px under the toolbar and the reply 4 px above the window's
  foot (the review). The trip at 1440 × 900: 68 px, the reply 52 px above the foot (28 px
  before phase C, the subject's letters on the hairline). Checked by `assert-conversation
  heading-clear` and `reply-in-view`, which wait for the reveal (`Reader::revealing`) instead
  of a fixed sleep.
- **Toasts keep off the open reply** (`Dock::keep_toasts_off`, `toast_room`; the reader's
  `quick_reply_area`): one that would cover it goes to the reader's left margin when it fits
  there (1440 px), else just above it; placed again when the reply's state changes, and when the
  column scrolls it (`Reader::connect_reply_moved`; not covered by a self-test). "Send undone"
  (shorter) sat on its footer between Send and Pop Out. `assert-toast-clear` checks the open
  reply too.

### The toolbar

- **Star at full strength** (major): the outline is `window::glyph`'s (step 1), the filled star
  the theme's in the star colour; `set_starred` compares first (it reset the icon, a resize, on
  every selection and store change). `assert-icon` names the icon a glyph stands for.
- **More's key as typed**: GTK writes an accelerator's letter in capitals ("U" read as Shift+U,
  the Unread filter); the menu's rows are rewritten when they are made (`write_keys`,
  `accel_label`: a lone capital becomes the key as typed, "Ctrl+P" stays). `assert-reader-more`
  checks what is drawn.
- **The same centred 32 px buttons at every width**: the buttons and Back are `valign center`
  (in the 710 px window they filled the bar's 48 px, a hovered one a block hanging from the
  window's top).
- **Previous and next at the column's end** in select mode too: a spacer of their own, not the
  "n of N" label's expansion (hidden with several selected, they sat right after More).
- **The bar keeps to the column in a narrow Split pane**: the strip clamp's CSS margin
  (`.sw-reader-strip`) with the bar's padding is the column's margin, so at 1100 px Next ends with
  the column (10 px past it before). Outside Hyprland the end window controls still sit beside
  the strip, so it centres half their width left of the column there (not on the owner's
  desktop).
- `reader-bar.sealwax-test` (new): heights and tops at 1440 and 710, the glyph, alignment in Split
  at 1100, the arrows in select mode.

### Smaller

- The prepared cache's HTML copy happens only for a body the cache keeps (`cache::keeps`): a
  5 MiB body was copied on the GTK thread (about 1 ms) and then refused by the cache.
- Palette and keymap titles: `j`/`k` are Next and Previous Conversation, `n`/`p` Next and Previous
  Message (keys.md).

### Tests and mutation checks

- New scripts: `reply-target` (whom `r`, `a` and Enter answer; the closed box after `p`, `n` and a
  click; the heading clear when the reply opens; a draft's mark and Edit draft; the heading's
  subject) and `reader-bar` (the toolbar at 1440, 710 and in Split at 1100, the glyph, select
  mode). Extended: `triage-actions` (the glyph; More's keys as drawn), `reader-ahead` (a collapsed
  row's date a day later), `reply-send-done` (the toast off the reply), `composer-drafts`,
  `send-later`, `snippets` (writing.md).
- New harness checks: `assert-quick-reply placeholder`, `assert-conversation date-of`, `draft-of`,
  `heading-clear`, `row-date-lacks`, `assert-reader-more ACTION TITLE KEY SHOWN`, `assert-drawn`,
  `assert-edge … top|bottom`, `assert-picker icon|details`; `assert-icon` names the icon a glyph
  stands for; `assert-toast-clear` counts the open reply; message names from `notify-mail` work
  in the conversation and reply checks.
- Each fix was taken out again and its check seen failing: the opening focus as a move
  (`reply-target` answered trip-5), the box not following a move (target and closed line after
  `p`, `n` and the click), the reveal without the heading (heading-clear), the draft mark, the
  heading's subject ("Orbit Coffee, issue 47"), the buttons' `valign` (48 px high at 1440 and 710),
  the theme's faint Star (glyph "theme"), More's capitals, the strip's margin (10 px off at 710
  and in Split at 1100), the spacer (Next 484 px from the bar's end in select mode), the
  collapsed row's date, the toast (`assert-toast-clear`), the picker's glyph and column, the
  composer's fold.
- The review's second round: `reply-target` gained the review's probe (a send, a newer reply:
  the box, Enter and `r` answer it; the focus back in a body, Enter opening the reply again; a
  conversation opened again starting afresh), a Shift+Tab move, `reply-in-view` and the heading
  with message rows; `reply-commands` changed on purpose (a focus a program puts in a card is no
  move: `a` answers the message `p` reached). Seen failing with the fix taken out: the reader's
  own focus counted as a move (the base build, the review's run), the keyboard moves ignored
  (`reply-target`'s Shift+Tab steps), the left page keeping its move (the box answered trip-1
  when the trip was opened again), the heading's subject with message rows ("Harbor & Pine,
  issue 40"). The reveal's order is `reveal_delta`'s unit test; `reply-in-view` asks the 12 px
  below the reply that keeping the heading first cut to 4 (the review's measure).
- The review's third round: `reply-target` gained the same conversation left with Escape and
  opened again with Enter, a first press in the message it opened at, `p` after a send, a row per
  message in Focused (Sana's older row: the box, `f` and `r`) and a notification's click on the
  open conversation; its `assert-quick-reply focused` checks straight after a key wait for it
  (one failed under load in the review's CI run). `reply-commands` gained Sana's older row in
  Split; `reader-conversation` Tab over the fold's row and the fold's last step. Seen failing
  with each fix taken out (`scratchpad/l4/mut/`): no `left_screen` (the box answered trip-1
  after Escape and Enter), no message-row choice (trip-6 in both scripts), the guard on the
  selected message (the press left the box on trip-6), `n`/`p` from the selected message (`p`
  after a send reached trip-4), the fold's row losing the message (Tab left the box on long-150),
  the fold's hand-over as a move (the box went to long-2), the hand-over to the slot (the focus
  in the list), the in-place show's focus as a move (the box went to trip-4), no re-target in
  place (trip-6 on Sana's row), no re-target on a grouping switch (trip-5 kept). The first full
  sweep found two on purpose: `reply-quick` (above) and `reader-conversation-order`, whose `n`
  started from the row a fold's click left the focus on, which narrowed the rule to bodies.
