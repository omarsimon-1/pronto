# Superhuman pass: conversation rows and the list (phase B1)

The list shows **one row per conversation** in every folder view, All Inboxes and search
results (the store's `ViewGrouping::Conversations`, docs/superhuman/store.md). Message rows stay
available behind **Group by Conversation** (on by default). This note records how the app holds
conversation rows, what each consumer of the old "row == message" does now, the decisions taken,
the tests and the measurements.

## Rows and keys

- The list's items hold the store's `ListRow` (message or conversation), known by its `RowKey`
  (`Message(id)`, `Conversation(thread)`) across change events (`app/src/list/item.rs`).
- `MessageModel` (`app/src/list/model.rs`) opens views with the list's grouping, pages them and
  applies `ViewDiff::Splice` and `ViewDiff::ConversationSplice` through one path
  (`ViewDiff::into_splice`), sections placed by each row's date (a conversation's latest shown
  message). `apply_threads` only runs for message rows. `Reload` opens the view again.
- A `changed` conversation that keeps its place can still change section (yesterday's top
  conversation answered today): its section moves and GTK is told `sections-changed`.
- **A conversation that moves keeps its row.** A reply comes as a removal and an insertion of
  the same thread. Within `MOVE_SPAN` (256) rows the model keeps the row's item and tells GTK the
  change as **one** `items-changed` over the rows between: GTK 4.24's GtkMultiSelection then
  keeps it selected (it re-selects an item removed and added in one emission) and
  GtkListItemManager keeps its row widget and moves the keyboard focus with it (both match items
  by object; read in GTK 4.24.1's gtkmultiselection.c and gtklistitemmanager.c). A longer move is
  told row by row, since GTK asks for every row of the span when a tracked row is in it; the
  list then selects the row again by its key and puts the focus back (`keep_selected`). Either
  way the selection is the same row, so nothing is reported and the reader isn't asked again.
- A conversation merged into another: the list follows `merged_threads` for the selection and
  the cursor, and quietly re-keys the reported row (the reader already follows the merge).

## Selection, cursor and the reader

- The list reports `Selected::One(message)` with the message the reader shows for the row: a
  message row's own; a conversation's **anchor** (its oldest unread member in the view, else its
  newest); in search results the **newest match**; from a notification the **new message**
  (`MessageList::open_at`). It reports again only when **another row** is selected (`report`
  compares keys): a conversation that moves, gains a message or is read keeps the reader on what
  it shows, which the reader refreshes in place (no flash, scroll kept).
- The selection, the reload restore (`wanted`/`restoring`/`settled`), the select mode's cursor and
  the auto-advance after an action (`Leaving`) are all keyed by `RowKey`.
- `Reader::prefetch` gets the messages the list would report for the next and previous rows
  (`MessageList::message_at`), the ids `show_next` then gets.
- "n of N" counts rows: conversations with conversation rows.
- **A filter or the grouping keeps the selection** where the new content's first rows hold it
  (`carry`/`select_carried`), so Shift+U on an unread conversation keeps it selected, focused and
  shown with no new report. Switching the grouping maps a message row to its conversation and a
  conversation row to the message shown.
- **Focused keeps a cursor row** (`set_keeps_cursor`): when a view first fills, the folder or the
  filter changes and nothing carried over, row 0 is selected (laid out unseen behind the list;
  marking read still waits for Enter). The visual QA's "the Focused list opens with no cursor row".
- **Focused keeps a cursor row when rows come back**: rows arriving in a list with nothing
  selected (Undo from Inbox Zero, new mail there) get the cursor on the row in the last one's
  place (`keep_cursor`), so `e` works at once after an Undo.
- **The keyboard focus follows the selection**: a focus that lands in the list (Tab, the
  palette closing, the reader's Back, a reset under it) lands where GtkListView's own focus row
  was left, which needn't be the selected row; it moves to the selected row when that row is
  wholly in view (`focus_follows_selection`, hooked on the window's focus changes, run once the
  frame is done and again after the next one, for a list a pane just showed). Never by
  scrolling, never for the focus a press gives its row (its release selects that row:
  `pointer_down`, from the list's click gesture's begin to its end), and never for GtkListView's
  Ctrl+arrows, which move its focus row apart from the selection on purpose (`cursor_keys`).
  The list's own takes (`take_focus`, as after Back) put it on the selected row in view. The
  app's own grabs use `focus_selected`, which also gives an empty list's focus to Inbox Zero
  (Escape from a search there). No ring stands apart from the selection (QA: "a square accent
  focus ring round row 0" after Shift+U; the layout review's "Back-click after j/k"); a focused
  row that isn't selected (nothing selected in Split, Ctrl+arrows) shows the bar alone.
- Selections larger than 64 rows (Select All at 100,000) aren't followed through moves, merges
  or filters (`KEPT_SELECTED`): each key would be looked for among every row.

## Rows (RowFields)

- `RowFields::conversation` fills the row from `ConversationSummary`: its people, `messages` as
  the quiet count (> 1), the base subject, the latest message's snippet and date, unread dot,
  star and clip from the members in the view, and the Draft marker for `has_draft`.
- **Participants** are a pure domain function, `sealwax_domain::participants`: one person by
  their whole name, several by first name, the user as "me", each person once (the user's two
  addresses are one "me"; one name from two addresses is one person), more than three as the
  first, "…" and the last two; in an unread row the people with unread mail are bold (all of them
  when the row doesn't say who). `spoken` says the same people for a screen reader, the user as
  "you", and counts those a row's four leave out ("Ingrid, Tomas, you and 2 others").
- **Accessible label**: "Unread, Draft, Ingrid, Tomas and you, 6 messages, Trip to Bergen,
  Oct 4, starred, has attachments": state, people, size, subject, a mixed-case date
  (`spoken_date`: "10:32", "Yesterday", "Oct 4", "Oct 4 2025").

## Triage on conversation rows

Built on `Store::conversation_members` (`AppState::expand_rows`, `triage::conversation_targets`,
a pure, unit-tested function):

| command | acts on |
|---|---|
| Archive, Delete, Move to Junk, Move to… | every member in the view's folders |
| Mark as Read | every unread member |
| Mark as Unread | the newest member of each conversation with nothing unread |
| Star / Unstar | the newest member of each unstarred conversation / every starred member |
| `u` (Mark as Read or Unread), Star's toggle | decided by the conversations: read unless all are read, star unless all are starred |

- One `QueueTriage`: one store transaction, one Undo batch, then the list moves on to the row
  after (auto-advance by key). Message rows are queued at once as before; conversation rows after
  one members read (one per view; about a millisecond).
- **Availability stays synchronous**: a conversation row counts as one target, its anchor in the
  anchor's folder with its members' state (`Target::of_row`), so titles (Unstar, Mark as
  Unread, Not Junk), sensitivity, the reader's toolbar, the batch page and the row menu follow
  the conversations without a store read.
- The toast counts conversations ("Archived 2 conversations"); one conversation's read or star
  change shows no toast, as one message's doesn't. The batch page says "3 conversations
  selected", Move to… "Move 2 conversations to…".
- Quick actions, the row menu, multi-select (x, Shift+J/K, Ctrl+A) and Move to… all go through
  the same expansion. There is no drag and drop in the list.
- **Search results**: a search of one folder acts on the members there; a search of every folder
  acts on the members in every searched folder of the conversations' accounts, **except Sent and
  Drafts for a move** (archiving a conversation found anywhere leaves the user's own copies where
  they are; read and star changes reach them).
- Rules' sender commands take a conversation's first writer who isn't the user, and Always /
  Never Junk moves the whole conversation.
- Mark-read-on-open is unchanged: the reader reports every body it put on screen (all unread
  members are expanded), and those are marked read in one transaction, not an Undo entry, only
  once the reader is on screen.

## Search, heading, notifications

- Search lists conversations (`Store::search_conversations`), each opened at its newest match;
  "Search · N results" counts conversations.
- The list's heading counts the conversations it shows: "Inbox 21 · Personal"
  (`Window::set_list_count`, `SidebarModel::heading_counted`; the sidebar keeps its message
  counts).
- **Notifications**: new messages of one conversation share a notification showing the newest;
  more mail in a conversation whose notification shows replaces it (it keeps the key of the
  message that started it, `message-{head}`); a click opens the conversation's row at that
  message, in the list's place in Focused mode. When the reader has that row already at
  another message (the Focused cursor's row, laid out behind the list), the row is reported
  again at the notification's message (`open_at`; `selected()` prefers it). With conversation
  rows, a notification's Archive and Mark Read act on the conversation's members in that Inbox.

## Group by Conversation

- Registry toggle `group-by-conversation` (View menu, palette; aliases threads, threading,
  conversations; no default key, as Reading Pane). Remembered in `layout.toml` as
  `conversations = true|false`; absent means on. The mailbox's read-ahead of the first view uses
  the same grouping, so the first frame still adopts it.
- Self-test harness: message rows by default (`AppState::set_grouping_default`), which the
  scripts before this were written for; `layout-defaults app` gives the app's defaults
  (conversations, Focused); `grouping conversations|messages` switches.

## Self-test commands added

`grouping`, `list-move-span N` (0 tells every move row by row), `list-replay SEEDS STEPS`,
`assert-list-moves at-once|by-rows N`, `assert-list-row POS kind|members|draft|count|label|label-starts|label-ends|scheduled`,
`conversation-count-shows`, `assert-conversation-shows N`, `assert-row-conversation POS MESSAGE`,
`assert-selected-conversation MESSAGE`, and `notify-mail … reply=NAME`.

## Tests

- New scripts: `conversations` (rows, anchors, a reply moving the selected row at once and row by
  row with the focus kept and no reader show, Archive and Undo, `u`, Star, multi-select, search,
  grouping both ways), `conversations-focused` (the app's defaults: cursor row, filters keeping
  the selection and the focus, Enter / j / Escape), `notify-conversations` (one notification a
  conversation, replacement, the click's anchor, Archive), `list-replay` (below).
- `list-replay` is the app's twin of the store's replay test: a scratch store of its own, random
  new conversations, replies (the user's in Sent), batches, flag changes and removals; every
  event applied to six models (Inbox; Inbox unread; All Inboxes starred; each with moves told at
  once and row by row), each equal to a fresh read, rows and sections, after every step. It is a
  self-test because GTK refuses its section model without `gtk_init`.
- Unit tests: `participants` (column, emphasis, dedupe, cap, spoken), `row.rs` (conversation
  fields, labels, spoken dates), `model.rs` (`move_span` against a spliced list, conversation
  filters), `triage.rs` (expansion rules, a conversation as a target, the toasts' count),
  `layout.rs` (the new key), the registry and menu tests.

## Measured

Release builds, headless Broadway (software rendering, 30 Hz frames), the machine shared with
three other tracks' builds (load average 10–25), so absolute numbers sit well above a quiet
machine's; compare the pairs. Base = the integrated phase A build of the same sources (message
rows); new = this change. Scripts: `perf-list` and `perf-reader`, and their twins with one row a
conversation, `perf-list-conversations` and `perf-reader-conversations` (new; not run by CI).

`perf-list`, 100,000 messages (the fixture's Inbox: **41,336 conversation rows**), list pane
379 px (two-line rows), p50 / p95 ms:

| | base, messages (2 runs) | new, messages | new, conversations (2 runs) | bar |
|---|---|---|---|---|
| jump to end → painted | 60 / 64; 131 / 134 | 104 / 112 | 91 / 184; 91 / 101 | ≤ 100 |
| jump to start → painted | 49 / 119; 110 / 113 | 89 / 95 | 103 / 199; 99 / 110 | |
| folder open → first rows | 59 / 74; 110 / 129 | 98 / 103 | 100 / 147; 95 / 101 | ≤ 100 |
| new mail → its row painted | 6.3 / 45; 6.9 / 54 | 6.3 / 38 | 6.9 / 48; 6.4 / 48 | |
| fling 12,000 px/s, frame interval p99 | 28.5; 36.4 | 28.2 | 64.0; 29.0 | ≤ 16.7 |
| reading 1,500 px/s, interval p99 | 0.9; 2.8 | 0.8 | 1.4; 0.7 | |
| GTK work per frame binding ≥ 50 rows, p50 | 12.1; 10.9 | 9.8 | 11.3; 29.2 (35 frames) | |

The base's own runs differ by 2× (60 against 131 ms to the end), so the conversation rows are
inside the noise: jump and open stay near the 100 ms bar at this load, as the base does, and the
fling bar can't be judged on Broadway's 30 Hz frames (every build's p99 is a frame or two).
Per frame, binding conversation rows costs what binding message rows does (a 41,336-row sweep
binds fewer rows a frame, so the per-row figure, 0.16 against 0.08 ms, carries each frame's fixed
layout). Re-check the bars on the live session (`SEALWAX_SELFTEST_ALLOW_LIVE=1`) when the machine
is quiet.

`perf-reader` (2,000 messages, 1440×900, Split), key → next conversation shown, p50 / p95 ms:

| | j ×60 | k ×30 | e ×20 |
|---|---|---|---|
| base, messages (2 runs) | 46 / 99; 52 / 98 | 50 / 73; 53 / 106 | 45 / 69; 51 / 84 |
| new, messages (2 runs) | 16 / 22; 56 / 104 | 18 / 32; 86 / 147 | 13 / 53; 105 / 208 |
| new, conversations (2 runs) | 50 / 122; 88 / 229 | 46 / 106; 79 / 173 | 47 / 97; 67 / 151 |

- The list's own part, key → the reader asked, is the same in every build at the same load:
  p50 12–28 ms, following the load (base 12 and 17, new conversations 18 and 28).
- With conversation rows **every** round came from a page laid out ahead (110 of 110: the
  prefetch gets the anchors `show_next` then gets); with message rows 39–70 of 84–102.
- The rest is the reader painting the next page in software, which swings with load (the same
  new build measured j at 16 / 22 and 56 / 104 ms a minute apart). Under this load no build
  meets the 50 ms p95 bar headless, the base included. Conversation rows show a whole
  conversation per step where message rows often step within one (shown in place), so their
  painting can't be cheaper.
- `list-replay` (6 seeds × 150 steps, debug build): 332 moves told at once and 332 row by row,
  every model exact after every step.

## Decisions to know

- Group by Conversation sits in the registry's View block, after Reading Pane, not at the end
  of `ActionId::ALL`: `categories_follow_declaration_order` requires each category's commands
  together. `ActionId::ALL` is 100.
- A conversation's notification keeps the key of the message that started it
  (`message-{head}`), so a notification of a one-message conversation is what it was before.
- The Focused cursor row is a selection (the list's one selection signal), laid out unseen and
  unread behind the list, as `j` there always was.
- Triage on search results of every folder leaves Sent and Drafts copies where they are for
  moves (see above); a folder search acts on that folder's members.
- Rules take a conversation's sender as its first writer who isn't the user.

## Part 2: the list's polish

BRIEF-B's list polish and the phase A visual QA's list items (2026-10-29), in the same files.
The shots are in `scratchpad/shots/b1/` (Adwaita dark, 1440×900, the app's defaults).

### Rows

- **Dates** (`row.rs`, `time_text`): today the time ("10:32"), before it this year the month
  and day ("OCT 28"; yesterday too: "YESTERDAY" on fifteen rows under a Yesterday header was the
  loudest thing in the list), before this year a short date with its day in the locale's order
  ("10/4/25", from `%x`'s order and separator, "/" where `%x` writes units rather than a mark
  between the numbers: "2001年02月03日" gives "25/10/4", not "25年10年4"; "OCT 4 2025" would
  have widened the column for every row). Screen readers keep the words ("Yesterday", "Oct 4 2025"). In the snippet's
  colour (tertiary, secondary on an unread row; it was brighter than every snippet), regular
  weight, small, barely tracked.
- **The date column has one width**, the widest date the font can show (a time, each month's
  "MMM 28", "12/28/25"), measured once a font from the label's own layout
  (`RowContext::date_width`), so the paperclip and the star sit at one x on every row: at
  1440 px the dates span 1375–1428, the clips 1339–1351, on every row.
- **The sender column narrows with the list** (`SENDER_SHARE`, `SENDER_FLOOR`): a fifth of the
  row's content, between 70 % of `--sw-size-list-sender` (with its gap, 192 px) and all of it:
  full from about 1,000 px, 134 px (122 px of names) in the 710 px half tile, where the fixed
  column left the subject and snippet half the row. The widest column comes from a ruler, one
  empty box beside the list's rows that draws nothing (`.sw-row-ruler`,
  `RowContext::set_ruler`; one for the list rather than a widget more in every row), since GTK
  can't read a CSS length and allocating the names' box below a CSS `min-width` would be a
  critical in a debug GTK. The two-line threshold is unchanged (the full column × 2.5).
- **Quick actions show on the pointer only** (BRIEF-B): the selected row keeps its date while
  the pointer is in the header or on no row. A hovered icon brightens on the selected row too
  (the selected row's icon rule outranked the hover's).
- **Right to left the row mirrors exactly**: `RowLayout` already mirrored the parts' places;
  their CSS sides (the row's padding, the gaps after the names, subject and snippet, the
  date's gap, the selection's bar, the quick actions' fade and padding, the header's padding,
  the date headers' indent) now follow with `:dir(rtl)`, so names end at 1404 and dates start
  at 12 (`list-polish`, `text-direction rtl`).
- **The select mode**: the checkbox is a light square, two dots wide, a hairline in the
  tertiary text, round the unread dot's place (an overlay, so nothing moves): an unread row
  keeps its dot inside an empty box, a ticked box fills with the accent and hides the dot. The
  tint says chosen and the bar says cursor: chosen rows lose the bar, the focused row takes it,
  never libadwaita's square ring (the QA). No ring outside the select mode at all.
- **Row geometry is locked** by `list-polish`: 38 px on one line and 57 on two for every
  bound row, CJK and emoji subjects and names among them; the date, clip, star and sender
  columns at one place on every row; names at x 36 and dates ending at 1428 (1440 px), 698
  (710 px).

### Focused mode (`MessageList::set_focused`)

- **No date headers**: the list drops its header factory in Focused (Superhuman; BRIEF.md "no
  date section headers in Focused"); Split keeps them, quiet captions over the names.
- **A click opens the row** (Superhuman), once GtkListView has selected it on the release
  (`click_open`, an idle after the gesture's release on the row it pressed); Ctrl+click,
  Shift+click and the checkbox select. Split keeps click = select and show. A plain click in the
  select mode leaves it and opens. A double click opens on its first click, and its second
  press, which would land on the reader now in the list's place (a link, an attachment's Open,
  a collapsed message), is swallowed with its release: a press within GTK's double-click time
  and distance of the click that opened a row, before any key (`swallow_double_clicks`, a
  capture controller on the window).

### The header row (`Window`, style.css's layout block)

- **Lines**: the sidebar button fills the rows' dot gutter (its glyph centred on the dots, x
  14–30), the view's name starts on the names' column (36), the end icons stop where the dates
  do (1428). The icons are in the secondary text and take the text colour on hover or checked.
  The sidebar button's fill is inset (a padding with `background-clip: content-box`; GTK's
  min-width is the content's, so the button stays 28 px wide): 20 × 24 px, 4 px short of the
  name, where the whole button's fill ran into it. Pressed (the sidebar shown), it has no fill
  at all, its glyph in the text's colour, and fills only under the pointer.
- **The filter pill sits on the name's baseline** (baseline alignment; it lends the row none
  of its depth below the baseline, a negative bottom margin, so the name doesn't move 2 px up
  when the pill comes, which baseline alignment alone did).
- **Search in the header row** (the QA): the list's field and scope (`MessageList::search_bar`,
  placed by `Window::set_list_search`) take the name's place while a search shows
  (`connect_searching`); the field starts at 36, is about 52 characters wide at most (528 px at
  the default size) and at least 8, a quiet fill with no accent ring, the query in the rows'
  size; the scope is two quiet words after it ("Inbox", "All folders"), the chosen one in the
  selected chip's pair; no "Search in" caption, no second row. The Search and Compose buttons
  step aside meanwhile (Compose's key still works). The count ("23 results") is the field's
  accessible description and the heading's hidden subtitle.
- **The select mode's bar** (BRIEF-B): where the reader's batch page isn't seen (Focused, or
  one pane at a time), the header shows "3 conversations selected" in the name's place, then
  Done, Delete, Move, Read or Unread, Star or Unstar as the header's quiet icons, bound to the
  registry's commands (their availability, titles and tooltips follow `refresh_commands`
  through `BatchPage::set_command`, which feeds both), and Clear, which leaves the select mode
  as Escape does (`Window::connect_batch_cleared`). `empty::BatchBar`, `BAR_ACTIONS`. During
  a search the bar takes the whole row: the field and its scope step aside while it shows
  (`Window::set_list_search` holds them in a slot of the window's), and come back with the
  select mode's end; together they crowded the row (at 560 px the count was "…").

### Split

- **Typing a search never jumps the panes**: an empty list shows alone only while no search
  shows (`Window::set_list_searching`); the reader beside a search that found nothing is blank,
  so there is still one empty state: its "No message selected" is hidden (not see-through), so
  a screen reader doesn't say it either. Before, the list jumped between 380 px and the whole
  window as results came and went (the layout review's probe).
- **Undo from Inbox Zero selects what it brings back**, in both modes: when an action's rows
  leave the list empty, their keys wait (`returning`) with each row's newest member in the
  view (a message row's message, a conversation's `last_member`). Rows back with the same
  newest member, while nothing else took the cursor, are selected again as they left: one alone
  (shown in Split, under the cursor in Focused), several in the select mode (nothing shown,
  nothing read), and they take the focus from Inbox Zero. A conversation back with another
  newest member is new mail (a reply to one archived): in Split it comes in unselected,
  unshown and unread, as any new mail there. The keys don't expire: Undo works until the next
  action, however late.

### Inbox Zero and the sidebar

- Inbox Zero's tint covers the list's whole pane, its header row too (`Window::set_inbox_zero`,
  `.sw-list-pane.sw-zero`), with no hairline under the header; the sun is a quiet mark in the
  tertiary text.
- The sidebar: Archive is an archive box (`mail-archive-symbolic`, else
  `package-x-generic-symbolic`; Adwaita's fallback was a blank page). New message is quiet: the
  width of its words in the selected chip's pair, 32 px, in a 48 px row level with the list's
  header (the theme test checks it there; the accent fill stays for the setup's primary
  buttons). The footer's config button is a labelled row, "Preferences", like the folders'.

### Self-test commands added (part 2)

`assert-row-part POS PART EDGE PX`, `assert-rows-aligned PART`, `assert-row-heights PX`,
`assert-quick-rows POS,…|none`, `assert-row-dot POS BOOL`, `assert-edge NAME left|right PX
[icon]` and `text-direction ltr|rtl` (app/src/selftest/layout.rs). From the review's round:
`click-row-held POS MS [mods]` (a person's click: the press, MS ms of frames and idles, the
release at the same place; `broadway::press`/`release`, where `click` sends both at once and
can't catch what runs between them), `click-row-twice POS GAP_MS` and `assert-reader-presses N`
(a person's double click, counting the presses that reach the reader pane), and
`conversation-open-at POS MESSAGE` (a notification's click without the notification).

### Tests (part 2)

- New scripts: `list-polish` (the app's defaults: geometry with CJK and emoji at 1440, 710 and
  480 px, the header's lines, quick actions on the pointer only, a click that opens and Escape
  back, Ctrl/Shift+click and the select mode's bar with Clear and Done, the dots in the select
  mode, RTL, the search in the header) and `list-polish-split` (Split: a click selects, the
  date headers stay, a search typed with no results, results and none again keeps the list at
  380 px with the reader blank, Undo from Inbox Zero selects and shows the conversation and
  focuses its row).
- Updated on purpose: `list-sections` (yesterday's row says "OCT 6"), `theme` (the quiet
  compose: the chip pair, 32 px; the accent fill checked on Add an account),
  `search-suggestions` (a click on the field, not on the title the field now hides, gives
  Broadway's keyboard focus back to the window).
- Already covered by part 1's scripts: the Back click after j/k (`conversations-focused`),
  Escape from a search on an empty list (`focused`), Undo from Inbox Zero in Focused
  (`focused`).
- Unit tests: the date forms and the column's samples, the locale's numeric order, the sender
  column's share, the select mode's words and icons.
- The review's round: `list-polish` (held clicks with the cursor's row scrolled away, from the
  list and from the search field; a person's double click; rows chosen during a search),
  `list-split` (new, Split: held clicks from the list and from the reader; Undo of 26
  conversations from Inbox Zero chooses all 26, nothing shown or read; a reply after Inbox Zero
  comes in unselected, unshown, unread), `conversations-focused` (a notification's click on the
  row the reader has already), `list-polish-split` (the blank reader hides its words); the
  CJK orders in `older_dates_follow_the_locales_order`. `list-polish` and
  `conversations-focused` open the Archive before the Inbox, so the Inbox's first fill holds
  every sample: the window may show the Inbox already as samples arrive (the known open item
  on its first pick), and newer ones then pushed the cursor down (a flake seen once at load
  32). Each new check was seen failing against the code before the fix (the old follow, the
  key-only `returning`, no swallow, the reported message first): 8 steps in `list-polish`, 9 in
  `list-split`, 1 in `conversations-focused`. The reviewer's own external slow-click probes
  (Focused A, Split D) pass. Shots and crops of this round: `scratchpad/shots/b1/round3/`.
- Full CI selection (95 scripts) on the final build: 94 pass; `a11y-window` step 175 (narrow
  window: `focus reader`, Escape, `assert-pane list`) fails under load, the focus left in a
  WebView; the phase A binary fails the same two steps side by side (2 of 2 runs each at load
  ~20), so it isn't this change. `rules-dialog` failed once at load ~40 (its own idle moving the
  focus after rebuilding rows) and passed 4 of 4 again.

### Measured (part 2)

`perf-list` with one row a conversation (100,000 messages, 41,336 rows; Split's 379 px list,
two-line rows), release builds, headless Broadway, part 1's build (the rows before this part)
against this part's:

- **Side by side** (each build on its own display and its own copy of the store, at the same
  time, so both see the same load; three rounds rotating them; load 28–38 from the other
  tracks' builds): jump to the end p50 166 / 141, 137 / 129, 135 / 144 ms (part 1 / part 2);
  folder open 160 / 146, 140 / 150, 146 / 153 ms; GTK layout per bound row 0.19 / 0.17, 0.18 /
  0.17, 0.20 / 0.14 ms. A third build without the RTL rules measured the same (0.17, 0.17,
  0.15 ms a row): the new CSS costs nothing measurable. **No difference within the noise.**
- **One after the other** at a lower load (3–15): jump to the end p50 45–48 ms (part 1) and
  46–69 ms (part 2), folder open 53–58 and 56–85 ms, both under S3's 100 ms; but the same build
  varied up to 2× between runs as the other tracks' builds came and went, which the side by
  side runs remove. A first version with an empty ruler widget in every row was replaced by one
  ruler for the list, which side by side measured no slower than part 1.

## Left for later

- To consider with the owner: past three people the column names the first and the two who
  *joined* last (the store's order of first writing), so in a conversation where an early
  writer wrote the newest message, that person sits inside the "…" ("Ingrid … me, Sana" while
  Tomas wrote last). Naming the last writers instead needs each participant's latest date from
  the store.
- The search results' count shows nowhere on screen now (the field says it to a screen
  reader): Superhuman shows none; a quiet count after the scope would fit a wide header but not
  Split's 380 px one.
- Headless perf only (see Measured); re-check S3's bars on the live session. The review's round
  adds per focus change in the list an idle and an after-paint check of the bound rows (about
  30 rows' bounds) and nothing per row; not measured again.
- `--sw-size-list-date` (domain's `COMPONENT_SIZES`), no longer used by style.css, was removed
  when phase B was merged (with the theme snapshots it rewrote).
- With nothing selected (Split), `j` selects the first row, not the row the keyboard's bar is
  on (as before; the bar is new).
- The sender column narrows to 134 px in the 710 px half tile (BRIEF.md's column is a fixed
  ~180 px): the owner's call.

## Phase C, track L step 1: the list's leftovers (2026-10-29)

BRIEF-C's L1 list and the phase A+B visual QA's L1 issues. Shots: `scratchpad/shots/l/`.

### The visual QA

- **A filter narrows a search, and says so** (blocker). The filter applies to the search (one
  filter for whatever the list shows), and its pill now follows the search's scope in the header
  ("contract · Inbox · All · Unread ×", the scope compact beside it since the review's second
  round): `Window::apply_chrome` moves the pill between the
  view's name and the search slot (centred there, on the name's baseline beside it). An empty
  search says what narrowed it: "No unread conversations in Inbox match “contract”."
  (`no_results`; the query cut at 40 characters). The field may shrink to 4 characters
  (`SEARCH_CHARS`), so Split's 380 px header holds field, scope and pill without the pane
  growing (it grew 4 px). Chosen over "the search ignores the filter" so Shift+U during a search
  narrows the results visibly, and the empty state can name the cause; Search still ignores
  nothing else (track S makes splits ignored by search themselves).
- **Names never cut** (blocker) and **Draft in the count's place**: rows.md's phase C section.
- **Full-strength Read and Star** in the select mode's bar and the quick Star (major): rows.md.
- **Inbox Zero**: no "0" over it ("Inbox · Home": `heading_counted` counts nothing at zero), and
  no seam at y 48: the tint now reaches under the header's transparent border
  (`background-origin: border-box`), which let the untinted window through.
- **The sidebar button's focus ring** sits inside the button, on its fill's edge (`outline-offset`
  of minus the button's padding and 2 px), 4 px clear of the name: it touched the "I" of
  "Inbox" (2 px clear round the fill after step 1, the review's second round).
- **Split right after Focused**: a list at its top stays at its top when the date headers come
  back (`set_focused`: `scroll_after_layout(0)`), "TODAY" in view; GTK kept the first row where
  it was, the header above the view (with the keyboard focus on that row).

### BRIEF-C's L1 items

- **"attempt to allocate GtkLabel … height -12"**: "Loading older messages…" was shown from the
  scroller's adjustment `changed`, inside the pane's own layout, and the pane's box laid it out
  by the count of children it took before (with the ruler after it, it got the ruler's slot, 0
  px). It shows in an idle now. The new `assert-gtk-warnings N` (GTK's own warnings and criticals,
  counted through the process's GLib log writer from the first list command) holds list-older,
  notify-content and list-names at 0.
- **Split with nothing selected**: a keyboard focus coming into the list goes to the first row in
  view, not where GtkListView's own focus row drifted as mail came in above it
  (`focus_follows_selection`); `j`/`k` start from the row the focus is on (the bar's row), else the
  first.
- **`select_carried`** sorts and dedups: two message rows of one conversation are its one row,
  selected alone, after Group by Conversation (not one row ticked in the select mode).
- **RTL**: the select mode's bar mirrors its count's gap and Clear's (`:dir(rtl)` twins).
- **One pass by thread**: `triage::conversation_targets` builds its groups only for Mark as
  Unread and Star, by `HashMap<ThreadId, usize>` (60,000 conversations: 0.07 s in a debug build,
  where the search per member was minutes); notify's `by_conversation` too.
- **`swallow_double_clicks`**: its state is `DoubleClicks` (pure, unit-tested); every press starts
  afresh, so a swallowed press whose release never came eats nothing more.
- Doc nits fixed (`rows_of_keys`, `position_of_key`, the long line above). `--sw-size-list-date`
  was already gone.
- **rose-pine's selected-row bar** reaches 3:1: rows.md.

### The first view and scheduled drafts (the review's second round)

- **Deterministic first view** (BRIEF-C). The window opens the first account's Inbox, All
  Inboxes when there are several accounts, however the reads come: the account and its folders
  in one read or two (a real setup lists the folders a server round trip after the account,
  where the window stayed on All Inboxes; in one read it opened the Inbox). Until the first
  account has its folders the window's view is a stand-in (`Inner::provisional`: nothing, or All
  Inboxes while the one account has none), and its own pick follows
  `SidebarModel::default_selection` (one account: its Inbox; several: All Inboxes) until a view
  is picked or the stand-in is done; the view remembered from the stand-in itself is passed over
  (`SidebarModel::own_selection`). An account added later doesn't move the view; a new first
  account (after the last was removed) opens its Inbox again. The store's read-ahead of the first
  view follows the same rule.
- Scripts that pinned the old timing, changed on purpose: with an engine attached, a new first
  account's Inbox sends `FolderOpened`, and its first rows a `LoadOlder` (the list has its end in
  view): 20 scripts expect them where they come (`engine-expect folder-opened`, `load-older`:
  a11y-dialogs, a11y-setup, files, layout-toggles, list-older, reader-bodies, reader-late, setup,
  setup-bridge, setup-detect, setup-edit, setup-microsoft, setup-microsoft-banner,
  setup-microsoft-failures, setup-pop3-folders, setup-remove, status-banners, status-offline,
  status-password); list-sections, window and theme expect the Inbox (two accounts: All Inboxes).
  The harness keeps filing an account before its folders (`mail::account_read`), as a real setup
  does.
- **The "open the Archive first" lines stay** in list-polish and conversations-focused, for a
  reason the first view makes certain: the window shows the Inbox as soon as the account has its
  folders, so the samples (filed after) come into it one at a time, and the cursor stays on the
  first to come, which later ones push down (a cursor never moves under the user). Taken out
  (tried), the cursor sits on row 4 (list-polish, `assert-list-selected 0` at step 37) and row 2
  (conversations-focused, steps 12 and 19). Both scripts check how a view opens filled, so they
  open the Inbox again once every sample is there; their comments say so. list-names (new in
  phase C) does the same and now says why (the review's third round): taken out, the cursor
  stays on the trip, the first sample to come, and once the rows are messages the list keeps
  that row in view (row 7), row 0 above it out of view, its names not laid out (`names-whole`
  false at its last check).
- **Scheduled drafts say "Sending tomorrow 08:00"** in their copy's row in Drafts (message and
  conversation rows) and in Scheduled Messages: writing.md's phase C section.

### More from the review

- **"Marius +7 10"**: the shortest names ("Marius +7", how many others wrote) no longer have the
  message count after them, two numbers of a kind side by side; the count makes way by opacity, so
  the room the names were picked for stays (`show_names`). `list-names` checks it at 710 px.
- **The search's scope compact beside the filter's pill**: "Inbox · All · Unread ×" ("All
  folders" while no filter shows; a screen reader hears "All folders" either way,
  `accessibility::name_button`), so Split's 380 px header leaves the field its room ("workshop"
  showed cut as "ɔrkshop"). `list-polish-split` checks the label and the name.
- **The sidebar button's focus ring** sits on its fill's edge, 4 px clear of the "I" of "Inbox"
  (2 px round the fill): `list-polish` checks 4 px blank at the button's edges.
- **rose-pine's selected fill**: rows.md.
- Draft still takes the count's place (`row.rs` `show_fields`): a conversation holding a draft
  doesn't say its size on the row (its accessible label does). The QA asked for the count after
  the names and Draft in its own slot; at 1440 px both beside the names left them too little
  room. The owner's call.

### A reply that comes in under the cursor (the review's third round)

- **It opens where it starts now** (pre-existing since phase B; threading). A conversation that
  gains a message stays reported as it was, so the reader follows it in place without
  reloading (`MessageList::report`); kept behind a Focused list (or beside Split's), Enter then
  opened it at the old anchor, the new unread reply a collapsed row that mark-read-on-open left
  unread. `MessageList::open_selected` reports the conversation again first when its anchor
  moved since it was reported to an unread message (`report_new_unread`: the row's anchor as
  reported is kept, `reported_anchor`), so it opens at its first unread message, as a
  conversation selected afresh does; the reader shows it there in place (`retarget`) and marks
  it read once on screen. Also after an earlier notification's click opened the row at another
  message (`open_at`, cleared then). An anchor that moved because the conversation was read (to
  its newest message) isn't followed; `j`/`k` don't re-report. `r`, `a` and Enter from a Focused
  list open it the same way (`compose_actions::reply_inline` reads the selection after opening).
  `conversations-focused` checks it (seen failing without it).

### Self-test commands added

`assert-gtk-warnings N` (mail.rs), `assert-list-row POS names-whole BOOL`, `assert-list-empty
TITLE [LINE]`, `assert-glyph NAME star-outline|mail-read|theme`, `assert-seamless NAME PX`,
`assert-drawn-inside NAME PX` (layout.rs; the last two draw the widget alone, `probe::Drawing`).
The review's second round: `outbox-copy NAME` (outbox.rs), `assert-list-row POS
scheduled|label-ends`; `assert-list-row POS count` says nothing for a count that made way.

### Tests

- New script `list-names` (names whole at 1440, 710 and in Split, the count after them, Draft in
  the count's place, the latest writers, Split at its top after Focused, no GTK warning).
- Extended: `list-polish` (glyphs, the focus ring, the bar's RTL edges), `list-polish-split` (a
  search under a filter: the pill, the empty line, 380 px), `list-split` (nothing selected: the
  focus and `j`), `conversations` (the latest writers; the carried selection deduped), `focused`
  (Inbox Zero: no "0", no seam), `list-older` and `notify-content` (no GTK warning).
- Unit tests: `participants` (latest writers, the shorter texts, the short text's emphasis),
  `conversations` (store: the latest writers kept), `glyph`, `triage` (one pass), list
  (`DoubleClicks`, `no_results`), sidebar (`heading_counted`), theme (`selected_fill`; the theme
  tests' exception gone).
- Each new check was seen failing with its fix taken out (mutation runs): the names fitting (6
  steps of `list-names`), the older row (`assert-gtk-warnings`), the seam, the focus ring, the
  Split top, the dedup, the RTL twins, the focus and `j` in Split.
- The review's second round: `send-later` (a scheduled draft's copy in Drafts, message and
  conversation rows, and after Send Now), `list-names` (no count after "Marius +7"),
  `list-polish-split` (the compact scope), `list-polish` (the ring 4 px clear), the 20 scripts
  above and `list-sections`; store test `draft_copies` (`compose_tests`), unit test
  `scheduled::a_row_says_when_and_to_whom`.
