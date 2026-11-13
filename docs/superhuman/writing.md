# Superhuman pass: writing (phase B2)

What the writing track changed, and why: Superhuman's reply flow, a calm composer card, undo
send, and a composer that doesn't make the user wait (part 1); Send Later with its time parser
and picker, Scheduled Messages, and snippets (part 2, at the end).

## The inline reply

The quick reply under a conversation (`app/src/reader/reply.rs`, ML-068) is now the reply: `r`,
`a`, Ctrl+R, Ctrl+Shift+R, Reply to Sender, the cards' Reply and Reply all buttons and, in an
open conversation, Enter all open it with the keyboard focus in it, instead of a composer card.

- **Keys.** `QuickReply` gets Enter (Reader scope: in the list Enter still opens). Reply, Reply
  All and Reply to Sender route through `compose_actions::reply_inline`: the reader's
  `reply_inline(selected, message, kind)` takes them when it shows (or is reading) the selected
  message's conversation; a Focused list (or a narrow window's) opens the conversation first
  (`list.open_selected()`, the same path as Enter). Without an inline reply to take it (no
  reader, only drafts) a card opens as before. Forward always opens a card; its caret is in To.
- **Whom it answers.** The message the user moved to (`n`/`p`, a press in it, or Tab from
  another message: `Reader::chosen_message`; since phase C only the user's own moves count, not
  where the focus goes by itself, see reader.md), else its own target: the latest message that
  isn't the user's own (`reply_target`, unchanged). A box holding text moves its text along when the user replies to
  another message; a draft is never answered. `n`/`p` carry a keyboard focus that is in the
  conversation along to the message they reach (`cards.rs` `focus_reached`): a Focused
  conversation opens with the focus in the selected message, so before, `p` then `r` still
  answered the selected one. The focus goes into the reached message's body; when that card is
  still being read or its body loading (a collapsed row that `n`/`p` expands), its row or
  header holds it only until the body is there (`focus_body`, `focus_loaded_body`, from the
  card's `install` and the live views' `attach`), so no focus ring is left on the header (the
  round 3 review's 03-after-pp). `reply-step-focus` checks it (`assert-conversation focus`).
- **How.** `ReplyKind` per box: Enter keeps the box's kind when it holds text, else the default
  (`[reader] reply`, issue #8); `r`, `a` and the buttons set theirs (the recipients follow unless
  the user chose them).
- **Recipients.** The first line of the open box is a button ("Reply to Tomas Berg and 2 others")
  naming whom it goes to: the compose rules' plan (`reply::planned`, `compose::reply::recipients`)
  from the target's headers (now kept on each card: `Card::headers`; read from the store for a
  target without a card). A click, or Edit To / Edit Cc / Edit Bcc (Ctrl+Shift+O, C, B; new
  Composer-scope actions), shows To, Cc and Bcc fields: the composer's `RecipientField`, made at
  the first edit. Once edited they are the reply's (`QuickDraft::recipients`), carried to the
  send, the draft keeper, Pop Out and Undo.
- **Quoted history** isn't shown; the compose rules add it when the reply is sent or popped out
  (as before).
- **Its draft** (`compose_actions::QuickDrafts`, 2 s after the typing rests, at most 5 s
  behind) is made as the kind the box answers as: when the box answers another way than its
  saved draft (Reply, saved, then `a`; or Reply All narrowed with `r`), the draft is prepared
  again for the new kind (`keep`, `answers_as`): its recipients are the compose rules' for it
  (or the user's choice) and its `source.kind` says how it answers; the old draft and its
  Drafts-folder copy go once the new one is saved. Before, only the body was replaced: after a
  restart a Reply All came back as Reply, and a Reply All narrowed to Reply came back as Reply
  All, its Send going to the person `r` had taken out (the review's repro). A prepared draft
  whose save failed is discarded, so no open draft reopens in a card at the next start.
- **After a restart** the box comes back as its draft left it (`QuickDrafts::find`, the reader's
  `restore_recipients`): its text, how it answered, and whom it goes to when that was the user's
  choice: a draft whose addresses aren't the compose rules' plan for the kind it was made as
  (`same_people`: To, Cc and Bcc, names and order aside) keeps them chosen, so a recipient taken
  out of a Reply All doesn't come back; one that is the plan stays planned (Reply All then adds
  the others). The comparison waits for the headers when they aren't read yet, and uses the
  draft's own kind even if the user answers another way meanwhile; once it decides they were
  chosen, that is a change the reader keeps. The store's `reply_draft` finds Reply All drafts
  too (it found Reply's only, so a Reply All draft didn't come back at all).
- **In the same session** the reader keeps what each box holds per message it answers
  (`Quick::drafts`: the whole `QuickDraft`, text, kind and chosen recipients; it kept the text
  alone before), updated by every change (`Inner::hold`, which also tells the keeper). A
  conversation laid out again (fallen out of the warm set) gets its reply back as it was, also
  when it answered an older message of the conversation; before, it came back as the default
  kind with the planned recipients.
- **Closing the window, quitting**: the window's close first saves what the inline replies hold
  (as its cards' drafts are), and Quit waits for them too (`save_quick_drafts`, at most
  `SAVE_ON_QUIT`, 3 s): the keeper went with the reader, and typing less than 2 s old was lost.
  The close waits once, at most `SAVE_ON_QUIT`, then the window goes, saved or not (its cards
  may hold it a moment longer). Quit saves the cards' drafts and the inline replies' side by
  side, at most 3 s in all (it was one after the other, up to 6 s).
- **Other writers** (the round 3 review's blocker): the inline reply's draft has a Drafts-folder
  copy, which Edit Draft (or Enter on its Drafts row) opens in a card as the same draft. A card
  that opens a draft written before tells the keeper (`composer::on_draft_opened`, from the
  card's `start`), which lets it go (`QuickDrafts::give`): never saved nor discarded by the box
  again, and the box that holds just its text empties (`Reader::quick_reply_taken`), as after
  Pop Out; a box holding more (typing not saved yet, or a save under way) keeps it for a new
  draft. Before, both wrote the draft; once the card sent or discarded it, every save failed,
  the retries ran in a tight loop once 5 s had passed (90,000 warnings in 11 s), the window's
  close waited for them for good (it re-ran its own handler every 3 s), anything typed after was
  never kept, and emptying the box could discard a send the card had queued.
- **Failures**: the backend's `save` and `discard` now say why (`backend::Unsaved`): `Gone` (being
  sent, sent, or discarded elsewhere: trying again can't help) or `Failed` (the store isn't open
  or failed). A draft that is `Gone` is let go and the text goes into a new draft at once (as
  for a kind change); emptying the box then discards nothing. A failed save is tried again after
  2 s, 4 s, 8 s… at most a minute (`retry_after`), also while the typing goes on, never in a loop;
  a window closing or Quit tries each once more (`save_now`).
- **Escape.** With the fields open it closes them; in an empty box it closes the box, resets it
  (default target and kind, planned recipients) and gives the focus back where the reply started
  (`return_focus`: the list in Split, the conversation in Focused, where a second Escape goes back
  to the list); a box with text stays open, a draft, and the focus goes to the conversation.
  Closing also ends the box's 400 ms scroll-into-view (`Inner::end_reveal`), which otherwise
  fought a scroll right after it (found by reply-commands).
- **Send** (Ctrl+Enter) empties the box at once and gives the focus to the conversation
  (Superhuman: a send never waits on screen); a refusal puts the draft back. The seam is now
  `QuickSend = Fn(QuickDraft, SendMode)`.
- **Look.** Send in the new primary fill with its label alone; its keys sit beside it, small and
  quiet; Pop Out is an icon at the far end (widget name `quick-reply-expand` kept); the hint
  sentence "Expand to the full composer with Ctrl+Shift+E" is gone (phase A's QA: chatty, and the
  keys inside the button read at 1.9:1).

## Send and Archive (Superhuman's Send + Mark Done)

`SendAndArchive` (`send-and-archive`, Ctrl+Shift+Enter, Composer scope), in the inline reply and
in a reply card.

- The rows are taken when the key is pressed (`compose_actions::done_rows`): the list's
  selected rows when the reader shows the reply's conversation as the selected one, the same
  rows `e` takes; else (a card whose conversation isn't selected) its Inbox messages, read
  from the store once the send is queued.
- Once the backend has queued the send, `AppState::archive_after_send` archives as `e` does
  (merged with track B1): a conversation row stands for its members in the view's folders
  (`expand_rows`, `triage::conversation_targets`), `list.advance_after` takes the rows' keys,
  and the list moves on to the next conversation (Focused: it opens, the focus stays in the
  reader).
- One Undo for both: the send is queued **held** (`outbox::hold`: no toast, not the last action);
  the archive is queued with `Controller::queue_with_shown`, its toast "Archived 1 message.
  Sending…" ("Archived 1 conversation. Sending…" on a conversation row, as `e` counts it), its
  Undo also running `outbox::undo_send`. The archive's "Undone" is then the one
  toast. The toast shows while either can be taken back: the longer of the triage window (7 s)
  and the send's (`undo_send_seconds`, 10 s by default), so the send is never undoable without a
  toast saying so. With `undo_send_seconds = 0` the send went at once: the toast still says
  "Sending…", its Undo takes back the archive alone. With nothing to archive (not archivable
  there, or no targets) the send gets its own toast (`outbox::announce`).
- The stand-in backend queues nothing: there it is a plain archive after the send.

## Undo send

- `[compose] undo_send_seconds` in `config.toml` (`domain::config::ComposeSettings`): default 10
  (Superhuman's; the M6 gate's 7 before), 0 to 30, 0 sends at once without Undo. The store
  backend queues with `not_before = now + outbox::send_window(state)`; the self-tests'
  `triage-undo-window` sets it too (`outbox::set_send_window`), as the send followed the triage
  window before.
- One toast, "Sending…" (was "Message queued"), with Undo for the window and timeout 0: at the
  window's end its Undo goes and it waits for the server, at most `SENDING_GRACE` (8 s); when the
  server takes it the same toast says "Message sent" for 2 s. A toast of its own says "Message
  sent" only when the sending toast was gone (replaced, dismissed, or the send waited).
- Offline, the banner's "N waiting" counts every send queued or being sent from the moment it is
  queued, in its undo window too (as before the fork's phase B), and a scheduled one (Send
  Later) from a minute before its time (`outbox::is_scheduled`, `SCHEDULED_AFTER`): a one-shot
  timer (`wake_when_due`) wakes then, since nothing else would refresh the banner (the outbox is
  read on store changes, and the engine sends only while connected). A send waiting to be tried
  again is never "scheduled".
- Undo reopens the draft where it was written: the inline reply's `QuickDraft` (text, kind,
  recipients) goes back under its conversation, focused when it shows, into the box of a
  conversation laid out unseen, or kept for when it is laid out; a card's draft reopens in a
  card (unchanged). When that box holds another draft by then, the undone reply opens in a card
  (the toast says "Sending undone"); before, it was dropped.

## Pop Out and Pop In

`PopOutDraft` (`pop-out-draft`, "Pop Draft Out or In", Ctrl+Shift+P, Composer scope).

- From the inline reply (and Ctrl+Shift+E, Open Reply in Composer): a `ComposeRequest::QuickReply`
  with the text, the kind and the chosen recipients (`Recipients`, new field); the card's caret
  goes after the popped-out text (`helper.js` `caretAfterTyped`).
- From a reply card: `helper.js` `typed()` reads what is above the signature and the quote; when it
  is plain lines, nothing is typed below the quote, the card has no attachment or image, and the
  reader shows its conversation with an empty inline reply (`Reader::pop_in` → `PopIn`), the text,
  kind and recipients (only when they aren't the plan's people, names and order aside, as after
  a restart) go back under the conversation, focused; the card's draft is discarded and the card
  goes. Otherwise a toast in the card says why it stays.
- The palette's Ctrl+Shift+P: `keymap::TEXT_KEYS` gains Ctrl+Shift+P, so the Command Palette's
  Ctrl+Shift+P now **yields** like its Ctrl+K: in a draft it is Pop Out, elsewhere the palette.
  Menus show Ctrl+K (they already did); the F-key test now accepts a yielding first key.

## The composer card

`app/src/composer/card.rs`, `window/dock.rs`, the composer block of `style.css`.

- **Flush with the window's edges** (phase A's QA blocker): `dock::EDGE` is 0, so the first card
  meets the window's right and bottom edges; the dock marks it `sw-dock-edge` (and every card
  `sw-dock-full` while one pane fills the window), whose outer border and corners go. No strip of
  the list's dates and scrollbar shows beside it any more.
- **Raised**: the theme's new `ui.composer` / `--sw-composer-bg-color` (in a dark theme the
  window mixed toward `lighter_background` as far as secondary text still reads at 4.5:1; Adwaita
  dark takes the whole step, `#2f2f33`; a light theme keeps the window's), a neutral hairline (no
  accent outline when focused) and an elevation shadow. The editor's body takes the same surface.
  (Track P's command palette card is `ui.raised` / `--sw-raised-bg-color`; both tracks first
  named theirs `raised`, and the merge gave the composer's its own name. The snippet and time
  pickers open over the card on the palette's surface, so the card never steps above it: where
  the palette's keycaps stop secondary text short of the whole step, the card takes the
  palette's step, e.g. Omarchy's tokyo-night #1E202F. See polish.md.)
- **Calm**: the header is the title, the card's menu (⋮: Formatting Toolbar, Add Cc and Bcc, Pop
  Draft Out or In, Attach File…, Discard Draft), Minimize, Expand, Close; From is a plain line
  ("From" and the sender as text) shown only when there is more than one sender; rows are
  compact; the footer is Send, its keys, "Draft saved", the attachments, Attach and Discard.
- **Formatting toolbar hidden** by default; `ShowFormatting` ("Formatting Toolbar", a toggle, no
  default key) shows it in every card, and `layout.toml` remembers it (`Layout::formatting`). Its
  keys (Ctrl+B, Ctrl+I, …) work either way.
- **Send**: the theme's new `ui.primary` / `primary_text` (`--sw-primary-*`): a white label on the
  accent deepened toward black until it reads at 4.5:1, at most 12 % (`PRIMARY_DEEPEN_MAX`), else
  the accent and its own label. Adwaita gets a white label in both modes (phase A's QA: near-black
  on `#3584e4` in dark); pastel accents (Tokyo Night, Catppuccin…) keep their look.
- **Links in the body** take `theme::link_on(palette, ui.composer)`: the accent mixed toward the
  text colour in 5 % steps until it reads at 4.5:1 on the card, at most 50 % (as libadwaita's
  own accent colour for text is lighter in dark mode). Adwaita dark: `#3584e4` read at 3.5:1 on
  `#2f2f33`, now `#5d9de9` at 4.74:1. The theme test checks every theme's links on the card, and
  a recipient chip's name (4.5:1) and remove icon (3:1) on the chip colour.
- **Toasts clear of the cards** (`Dock::keep_toasts_clear`): libadwaita centres a toast at the
  window's bottom, over a card's Send at 1440 px ("Sending…" stays up to window + 8 s). While
  cards are docked at the bottom right and the room left of them holds the toast with its
  sides, the toast gets a margin on its right (the cards' side in either text direction) as wide
  as the cards, so it is centred in that room; else (one pane, an expanded card, a narrow room)
  on the window as before. The toast overlay's children are watched (`observe_children`: a
  toast's widget is made when it shows); the dock places them again when its cards change or
  the window's width does.

## Speed

- **One web process for every card**: `Editor::new` builds every view after the first related to
  it (`related-view`, as the reader's views are); the engine keeps that root view while the
  process runs and blanks its page when its card goes. Measured with `assert-web-sandbox`: 3
  cards open, phase A had 4 web processes (the reader's + one per card), now 2 in all.
- **A spare editor**: `composer::prewarm` makes one (its page loaded) 1.5 s after the reader is
  installed, at `Priority::LOW`, and again each time a card takes it; `Composer::open` adopts it
  (`EditorEngine::take_spare`, restyled for the theme in use).
- **The editor page loads while the draft is prepared** (`Inner::start`: `prepare` is started
  first, the page loads meanwhile; `ready()` runs once both are done, whichever comes last).
- **Measured** (headless Broadway, debug build, Intel Core Ultra; `measure-compose`, from the
  command to a body ready to type in):

  | case | phase A | now |
  | --- | --- | --- |
  | first card after startup (web process cold) | sequential; ≈ 75 ms like "no spare" below | 14 ms (spare) |
  | first card, `composer-prewarm false` | — | 75 ms |
  | later cards, Compose | 25–45 ms (5 ms polling) | 9–13 ms (median 9–12) |
  | later cards, Forward | ≈ 25 ms (5 ms polling) | 11–12 ms |
  | web processes, 3 cards open | 4 | 2 |

  On this machine the spare matters most for the first card (the web process start); later
  cards were already quick, the shared process saves memory. Slower hardware gains more.
- **Background mode**: the spare and the composer's web process cost about 37 MiB PSS at idle
  (the review's probe: 276 against 239 MiB). When the main window closes into background mode
  (`AppState::main_window_closed`, at idle once its cards have gone) the spare is released and,
  with no card open, the engine's root view too (`EditorEngine::rest`), so that web process
  ends; the next window makes a spare again 1.5 s after its reader. `composer-prewarm` checks
  it (two web processes, one after the close, two and a spare after a new window).
- **Inline reply**: `measure-reply` times from the key reaching the window to the frame painted
  with the reply open and focused: about 8 ms (7–15) for Enter and `r`, or about 45 ms when
  Broadway paces that frame (the layout track saw the same bimodality); phase A's
  `win.quick-reply` focus was under 5 ms (polling). The script's bar is 100 ms so a loaded machine
  doesn't fail it, and a round over it is timed once more before it counts (the phase B sweep, 5
  shards at load 30–37 on 16 CPUs: one round of seven took 104.7 ms; alone the worst was 13.9).

## Tests

- Unit: `config::the_undo_window_of_a_send`; theme `send_reads_alike_in_both_modes`,
  `the_composer_card_is_raised_and_readable` (+ snapshots: three new CSS variables and a
  `primary … composer …` roles line, `raised` before the merge); actions/keymap/shortcuts/menu
  tests for the new keys and the yielding Ctrl+Shift+P; `reply` unit tests (`planned`,
  `recipients_label`, `pop_out_tooltip`, a `QuickDraft` prints no text).
- New self-tests: `reply-inline` (Enter/`r`/`a` from the conversation and the Focused list,
  recipients, Escape, `f`, Pop Out and Pop In, a card with formatting staying, timings),
  `reply-send-done` (store backend + fake engine: the sending toast's lifecycle, Undo back into
  the inline reply, Send and Archive with auto-advance and its combined Undo,
  `undo_send_seconds = 0`), `composer-prewarm` (spare made after startup, Compose and Forward
  timed with and without it, two web processes).
- Changed on purpose: `reply-commands`, `reply-default` (Reply and Reply All answer inline),
  `reply-quick` (the focus leaves after Send; Pop Out), `reply-outbox` and `reply-quick-drafts`
  ("Sending…"), `composer-dock` (flush bounds, 43 px header), `composer-editing` and
  `a11y-composer` (the toolbar hidden until shown; the new Tab order), `composer-safety` (the
  body's raised surface).
- Round two (the review's): `picker-palette` (new: the palette over the time picker and the
  snippet picker and the reverse, in the inline reply and a card), `reply-restore` (new: kind
  and chosen recipients after a restart), `reply-outbox` (a send in its undo window and a
  scheduled one near its time counted offline), `reply-send-done` (the toast as long as the
  longer window, its Undo taking both back; without a window the archive alone; a toast clear
  of a card), `send-later` (a past or near time sends as Send does; Scheduled Messages keeps the
  focus and names its buttons), `snippets` (the caret after an emoji with its skin tone),
  `composer-send-keys` (Close keeps every key), `composer-prewarm` (background mode),
  `a11y-window` (waits for the reader's Escape: a web view hands keys back a moment later). Unit:
  `snippets::an_unknown_name_leaves_no_gap`, `when::a_nights_midnight_is_its_end` and more
  `when` cases, `reply::a_draft_to_whom_the_rules_plan_is_no_choice`, the theme's links and
  chips on the card, the store's `a_quick_reply_finds_its_closed_reply_draft_again` (Reply All).
  New DSL: `send-window`, `assert-toast-clear`, `quick-send-at`, `outbox-queue NAME [MS]`,
  `quick-store-draft … [KIND] [TO]`, `assert-quick-send-last … [TO]`, `assert-stand-in
  last-saved`.
- Round three (the review's): `reply-restore-kind` (new, store backend: Reply then Reply All,
  and Reply All then Reply, each after the first save, across a restart, with the stored
  draft's kind and recipients checked and Send going to the sender alone; chosen recipients
  through a kind change and a restart; a conversation laid out again in the same session, also
  answering an older message after `p`; Undo of a send whose box holds another draft; the
  window closed right after typing). Unit: `a_draft_made_for_another_kind_is_made_again`,
  `when::a_nights_small_hours_are_the_next_days`, `when::midnight_is_a_days_end`,
  `snippets::an_unknown_name_alone_at_a_lines_start_takes_its_punctuation` (and the cursor
  cases in `an_unknown_name_leaves_no_gap`, whose old caret position, after the comma, was the
  bug). New DSL: `reply-relayout`, `assert-reply-draft MESSAGE none|KIND [TO] [CC]`.
- Round four (the review's blocker and its notes): `reply-draft-elsewhere` (new, store backend:
  a card that opens the inline reply's draft takes it and the box empties; discarded there,
  nothing tries it again; discarded or queued behind the keeper's back, the next change goes
  into a new draft in one try, never in a loop, and emptying the box leaves the queued send
  alone; a box holding more than the draft keeps it; a store that fails is tried 3 times in 9 s;
  the window closes at once each time, and the text comes back after a restart),
  `reply-step-focus` (new: `n`/`p` leave the focus in the reached message's body, also one that
  loads after the key), `composer-send-keys` (waits for the inline reply's draft before the
  cards: its last-saved check raced that first save, 2 s after the typing). Unit: `a_failed_save_waits_longer_each_time`,
  `when::in_the_small_hours_the_night_still_on_is_meant`, "evening 1" in
  `a_nights_small_hours_are_the_next_days`, the capital past `{cursor}` and blank lines in
  `an_unknown_name_alone_at_a_lines_start_takes_its_punctuation`. New DSL: `reply-draft-name NAME
  MESSAGE`, `outbox-discard NAME`, `composer-fail-saves BOOL`, `quick-tries-mark`,
  `assert-quick-tries MIN MAX`, `assert-conversation focus MESSAGE body|header|row|card|none`.
- New DSL: `measure-reply`, `measure-compose`, `composer-prewarm`, `assert-composer spare`,
  `outbox-name`, `assert-web-sandbox [N]`, `assert-quick-reply kind|open|editing|chosen|to|cc|bcc`,
  `assert-compose-last quick-reply … [TO]`, `assert-quick-send-last … [KIND]`.

## Part 2: Send Later, the time picker and snippets

### The time parser (`domain/src/when.rs`)

Pure, on the wall clock (`Civil`, to the minute); the caller gives "now", so the tests run on a
fixed now (Thursday 29 October 2026, 14:20) and a property test runs it from any now. The app
turns its answers into instants in the local zone with GLib (`time_picker::instant`), which gets
summer time right; a span ("2h") is added to the instant, not the wall clock.

`parse` reads "tomorrow", "tomorrow 9am", "tonight", "this weekend", "next weekend", "next week",
"mon"/"monday"/"tues", "monday 8:30", "in 3 days", "3d", "2w", "1mo", "2h", "30m", "1h30m",
"in an hour", "half an hour", "nov 5", "5 nov", "november 5th 3pm", "jan 2 2027", "21:30", "9:30pm",
"noon", "midnight", "this evening", "tomorrow afternoon", "later today"; unknown words are refused
rather than ignored ("tomorrow banana"). The choices it documents:

- A day without a time is at **08:00** (Superhuman's default); morning 08:00, afternoon 13:00,
  evening 18:00, night and tonight 20:00.
- Times are **24-hour** unless am/pm says otherwise ("5" is 05:00, as Sealwax shows times); after
  afternoon/evening/night/tonight an hour before 12 is the afternoon's ("tonight 9" is 21:00).
  Contradictions ("tonight 7am", "morning 14") are refused.
- **Past times roll forward**: a time alone that has passed is tomorrow's, a weekday is always
  the next one after today ("mon" on a Monday is next week's), a date without a year that has
  passed is next year's. "today 9am" or "tonight 7pm" once passed is refused ("That time has
  passed"): the words say today. "tonight" alone once 20:00 is near is the next whole hour at
  least 30 minutes away, while that is still today.
- **Spans**: hours and minutes are exact (`Target::In`); days, weeks and months move the date
  and keep 08:00 or the time given ("in 3 days 5pm").
- A bare number is an hour ("10" is 10:00), not Superhuman's 10 October. Numeric dates (5/11,
  5.11) aren't read as dates: day/month order is ambiguous, and a dotted number is a time ("5.11"
  is 05:11, Norwegian's dotted date read as the dotted time "9.30" is); the picker shows the date
  beside what it read, and "5 nov" is the date.
- "next" with a weekday is that day of next week, weeks starting on Monday ("next fri" on a
  Thursday is 8 days ahead, "fri" and "this fri" tomorrow), as "fri next week" says (refused
  before).
- 12 or midnight after "tonight", "evening" or "night" is the night's end, the next day's 00:00
  ("tonight 12", "tonight at midnight", "friday night 12"; it was noon); "tonight 12pm" and
  "tonight noon" are refused.
- A span of nothing ("0h", "0m", "in 0 days") is refused; it read as today 08:00 before 08:00.
- A night runs past its midnight: 1 to 4 after "tonight" or "night" (or 1am to 4am) are its
  small hours, the next day's ("tonight 1" is 01:00 tomorrow; it was 13:00); 5 to 11 are the
  evening's ("tonight 5" is 17:00). "evening 2am" and "tonight 5am" are refused. In the small
  hours themselves the night still on is meant while its time is ahead (`tonight_or_on`):
  "tonight 1" or "tonight 0:45" at 00:30 are in less than an hour (they were a day later);
  "tonight 12" then is the coming midnight, and "tonight 9" this evening. "evening 1" to
  "evening 4" are refused (they read 13:00 to 16:00).
- "midnight" is a day's end, the next day's 00:00, as "tonight 12" is: alone it is tonight's,
  "tomorrow midnight" is tomorrow night's (31 October on the 29th; it was the same instant as
  "tonight 12"), "fri midnight" Friday night's, as "due Friday at midnight" says.
- A weekday beside a date must agree ("thu nov 5"), else it is refused.

**Presets** (`Preset::ALL`, Superhuman's, each shown with its time):

| Preset | Resolves to |
| --- | --- |
| Later today | now + 3 h, rounded up to the next half hour; hidden once that would be tomorrow (after about 20:30). 18:00 is "this evening" when typed |
| Tomorrow morning | tomorrow 08:00 |
| This weekend | the coming Saturday 08:00; on a Saturday once 08:00 has passed, Sunday 08:00; on a Sunday once it has passed, the next Saturday |
| Next week | the next Monday 08:00 (tomorrow on a Sunday, a week on a Monday) |

`label` says a time back: "today 18:00", "tomorrow 08:00", "Tue 08:00" (within the week),
"Thu, Nov 5, 08:00", "Thu, Nov 5 2027, 08:00" (24-hour, as the app's clock; month first since
phase C, as the list writes a date).

### The pickers (`app/src/picker.rs`, `app/src/time_picker.rs`)

`picker.rs` is a palette-style overlay for choices that aren't commands: built once per window,
hidden between uses, a quiet heading over the field, rows with a muted detail at the end, Up/Down,
Enter or a click, Escape/backdrop/focus-leaving closes, the focus going back before the choice
runs (so a snippet lands at the caret).

The command palette and a picker take each other's place (`picker::close_in`,
`palette::close_in`): Ctrl+K or F1 over a picker closes the picker first (its focus back in the
draft) before the palette sees where the focus is, so it lists the card's commands and Escape
goes back to the draft; Ctrl+; over the palette (or over the time picker) closes it first, so the
snippet goes to the draft. Neither records a place inside a layer over the window as where the
focus goes back (`palette::holds`). Before, the palette recorded the closing picker's field: on
its own close the focus fell back to the list hidden behind the open conversation, whose
single-letter commands then ran on what was typed (`a` turned a reply into Reply All).
`picker-palette` covers it. It reuses the palette's classes and its layer class, so
`palette::holds` treats it as the palette (the search field stays open while it borrows the
focus); its own CSS is the `superhuman: writing` block. A self-test sees it as the window's dialog
(`assert-dialog`, `a11y` scopes).

**The time picker API** (Remind Me reuses it):

```rust
time_picker::pick(&window, "Remind me", &Preset::ALL, |chosen: Chosen| {
    // chosen.at: Unix ms; chosen.label: "tomorrow 08:00"
});
```

Its rows: the presets with their times; once something is typed, what it reads comes first
(chosen), with its date ("Tomorrow 09:00 · Fri, Oct 30"; not repeated after "Thu, Nov 12, 09:00"), and presets whose names match stay below (one the typed time already is shows once);
a hint row when it isn't a time ("Not a time Sealwax reads: try tomorrow 9am, mon 8:30, in 3
days or nov 5") or has passed. `time_picker::describe(at)` says any instant the same way.

### Send Later

- `SendLater` (`send-later`, "Send Later…", Ctrl+Shift+L, Composer scope; aliases "schedule
  send", "send at"), in a card and in the inline reply (with the sends: enabled while it holds
  text). A clock icon beside the card's Send keys (`composer-send-later`).
- A card checks the draft first (`Inner::sendable`: valid recipients, at least one, attachments
  added), then asks; the inline reply asks through a seam (`Reader::set_quick_later`), and its
  `SendMode::Later(at)` goes the inline reply's send path (the box empties at once).
- `Backend::send(id, draft, at)` (new `at: Option<i64>`): the store backend queues with
  `not_before = at` (else now + the undo window): the store's outbox and the engine's timer
  already scheduled by `not_before`, so nothing changed in the store or the engine. The stand-in
  records it (`StandIn::sent_at`). A time that has passed meanwhile (the picker's rows are worked
  out as the user types, so one left open past "11:00" chose a past time) or is less than a
  minute away goes as Send does: after the undo window, with the "Sending…" toast and its Undo,
  never at once.
- `outbox::scheduled`: one toast, "Sending tomorrow 09:00", with Undo for 10 s (its Undo works
  whatever came after it; `z` too while it is the last action); then the outbox keeps it. When it
  goes, "Message sent". Sealwax must be running then; otherwise it goes at the next start.
- The offline banner's "N waiting" counts a scheduled send only from a minute before its time
  (`outbox::waiting_sends`, see Undo send).
- **Scheduled Messages** (`ShowScheduled`, `show-scheduled`, Window scope, palette only; aliases
  "outbox", "scheduled sends", "send later"; `app/src/scheduled.rs`): the outbox the window shows,
  read from the store when it opens and on every outbox change: each send's subject, "tomorrow
  08:00 · to Tomas Berg and 2 others", **Edit** (`outbox::unschedule`: back to a draft, opened in
  a card, or under its conversation when it was written there this session) and **Send Now**
  (`outbox::send_now`: undo, then queued again after the undo window, with the sending toast).
  The rows are rebuilt only when the sends listed change, and the keyboard focus goes back to
  the same button of the same send (else of the send now in its place); the buttons are named
  for their send ("Edit “Notes”", "Send “Notes” now": the label property, in place of the
  relation GTK gives a button to its label).
- Opening a scheduled draft's Drafts-folder copy (Edit Draft) says when it is scheduled and that
  Scheduled Messages cancels it, instead of "This message is being sent".

### Snippets

- `domain/src/snippets.rs` reads `snippets.toml` with `config.rs`'s positioned helpers: `[[snippet]]`
  with `name` (required, one line, at most 80 characters, unique ignoring case), `body` (required,
  at most 20 000 characters; leading blank lines and trailing white space dropped), `subject`,
  `to`, `cc`, `bcc` (a string of addresses or a list; each must be an address Sealwax can send
  to). Variables `{first_name}`, `{last_name}`, `{name}`, `{my_name}`, `{my_first_name}`, `{date}`,
  `{cursor}` (once, body only); `{{`/`}}` are braces; an unknown variable is a mistake with a
  "did you mean". Every mistake has its line and column; a file with one isn't applied (the
  previous snippets stay), as for the other settings files. `default_snippets_file` is the file
  Edit Snippets creates: how it works and two commented examples.
- `Vars::new(recipient, me, date)`: names from the display name ("Tomas Berg", "Berg, Tomas", a
  trailing "(Acme)" and quotes dropped); without one from an address like tomas.berg@… or
  ola_nordmann@… (two or three words of letters); nothing from info@… or kari@… (one word is a
  role as likely as a name, jsmith@… would read "Jsmith", and a wrong guess goes to the
  recipient), where `{name}` is the address. `{date}` is "29 October 2026".
- A name that is unknown leaves no gap (`expand`): before punctuation or a line end the blanks in
  front of it go ("Hi {first_name}," is "Hi,", not "Hi ,"), before more text one blank stays
  ("Hi {first_name} and welcome" is "Hi and welcome"). Alone at a line's start its punctuation
  goes too and the next word takes a capital ("{first_name}, thanks" is "Thanks", was ", thanks"),
  also past a `{cursor}` or blank lines between ("{first_name}, {cursor}thanks" and
  "{first_name},\n\nthanks" are "Thanks"; a value filled in is left as it is); a line left
  empty goes, and blank lines left at the start. `{cursor}` stays where the text
  closed up ("Hi {first_name}{cursor}, ok" puts the caret after "Hi"; it was after the comma).
- `Preferences` reads a fourth file (`SettingsFile::Snippets`, `Update::snippets_changed`,
  `Preferences::snippets`), watched and reloaded like the others; `SealwaxDirs::snippets_file`.
- **Use Snippet…** (`UseSnippet`, `use-snippet`, Window scope, Ctrl+; and Ctrl+'; aliases "insert
  snippet", "template", "canned response"; `app/src/snippets.rs`; its key again closes it): where
  it goes is decided when it opens (`snippets::destination`): the focused card, else the inline reply when it holds the focus
  (text or fields), else a new message from the account the list shows. The picker finds by name
  (the palette's matching), then by body; rows show a one-line preview. In the inline reply:
  inserted at the caret over a selection, the caret at `{cursor}`, the snippet's recipients joined
  to whom it goes (the user's choice from then on), `{first_name}` from its first To, `{my_name}`
  from the account's sender name. In a card (`Composer::insert_snippet`, after `wait_ready`): the
  recipients joined (Cc and Bcc show), the subject filled when empty, the body at its caret
  (`helper.js` `insertSnippet`: typed in, the caret moved back to `{cursor}` over the text after
  it, counted in graphemes as `selection.modify` steps (`Intl.Segmenter`): "A{cursor}B 👍🏽 C"
  put the caret one step early when it was counted in code points; a new card with no caret yet
  gets the body's start). With no snippets the picker offers Edit Snippets.
- **Edit Snippets (snippets.toml)** (`EditSnippets`, App scope, palette, no key) opens the file in
  the editor, created with its defaults, like Preferences.

### Keys (ADR-017)

Four actions appended in one block after part 1's (`ActionId::ALL` is 109): `SendLater`,
`UseSnippet`, `EditSnippets`, `ShowScheduled`. Only Send Later and Use Snippet have keys.

- **Ctrl+;** is GTK's and WebKit's second emoji-chooser key, so it was in `keymap::TEXT_KEYS`
  and the audit refuses it for a global command. Use Snippet must work in the composer's editor
  and the inline reply too (Superhuman's Cmd+; inserts into a draft), where a yielding key would
  lose to the emoji chooser. So Ctrl+; leaves `TEXT_KEYS` and is an application accelerator: it
  runs before the focused widget everywhere; the emoji chooser keeps Ctrl+. (and its context menu).
- **Norwegian layout**: `;` is Shift+`,`, so Ctrl+; alone fails the audit's "needs Shift
  everywhere" rule (`Fault::NoUnshifted`). **Ctrl+'** is the fallback: `'` is unshifted on both
  layouts, next to `;` on US and at the end of the home row on Norwegian; no text field uses it.
- **Ctrl+Shift+L** is free in every scope (Ctrl+L is Forward's, scoped to the list and reader).
- The palette now sorts its commands by category (`palette.rs` `fill`, a stable sort): additions
  at the registry's end (every track's, by the brief) would otherwise make a second "Compose"
  heading on the empty query (palette-keys found it).
- `keys.toml`'s default file is generated (grouped by category); `docs/USER_GUIDE.md`'s table is
  regenerated (registry order; the test checks it).

### A fix in the composer: Send kept every key

Found by `send-later`, present in phase A too (its binary loses the same characters): WebKit
hands an editor's keys to its page one at a time, and Send serialized the body at once, so the
end of text typed just before Ctrl+Enter could still be on its way: it was left out of the sent
message, and once the card went WebKit handed those keys to whatever had the focus (the inline
reply got "or mandag." of "Agenda for mandag."). Broadway types at machine speed, and a refused
send before it made the page lag behind; a fast typist on a busy web process could hit it too.

- The card counts the key presses its view hands WebKit (a capture controller added before the
  card's shortcuts, since GTK runs a widget's later controllers first, so keys the card takes
  aren't counted); `helper.js` counts the `keydown`s its page sees and every message it posts
  says the count (`Editor::keys_seen`).
- Send (and Send and Archive, Send Later), Close (Ctrl+W) and Pop In wait until the page has
  seen as many (`Inner::keys_caught_up`), asking the page only when its last message is behind,
  400 ms at most (an input method that keeps keys from the page runs the wait out). The Ctrl of
  Ctrl+Enter or Ctrl+W counts as a key the page saw, but makes it post no message, so these
  usually cost one round trip to the page (a few ms). Close hides the card only after that wait:
  before, the saved draft lost the last characters typed (17 to 20 bytes of 20 in
  `composer-send-keys`) and WebKit handed them to whatever had the focus next.
- Regression self-test: `composer-send-keys` (a refused send, typing at machine speed, Ctrl+Enter;
  then the same with Send Later): the message holds every character, the inline reply none.


### Measured

Headless Broadway, debug build, Intel Core Ultra; the picker's own timing, from the command to the
first painted frame (`picker-shown-in`, five openings each): the time picker 7.7–9.4 ms, the
snippet picker 5.7–7.4 ms. The first opening of a run measured 8–54 ms, and a later one now and
then 45–46 ms too (four runs of ten openings, load average 9–21): Broadway pacing the frame, as
the layout track saw, not a first-open cost. Laying the hidden card out once after install
(a measure with rows of text, at low priority) didn't change it, so it isn't done. Rows
for a query are filled in place (no rebuild). Parsing a typed time is microseconds. Send's key check costs no round trip when the
page's last message is current (`composer-attachments` times its toast right after Send).

### Tests

- Unit: `when` (13 tests with a property test: clock times, weekdays and presets always lie
  ahead, from any now), `snippets` (6), `preferences::snippets_are_read_and_a_faulty_file_keeps_them`,
  `time_picker` (presets with their times, typed time first, hints, local instants),
  `snippets::the_picker_lists_snippets_with_a_preview`, `scheduled::a_row_says_when_and_to_whom`,
  `picker`; the registry/keymap/shortcut tests extended (`the_design_keymap`, the composer's keys).
- Self-tests: `send-later` (the time picker's presets, typed times, hints, Escape; the inline
  reply queued with `not_before` and the toast's time; Undo; the offline banner; a preset;
  Scheduled Messages' Edit, back under the conversation and back in a card, and Send Now; a
  card's checks and its scheduled send; a scheduled draft opened from Drafts says when; the
  stand-in's time), `composer-send-keys` (above), `snippets`
  (`snippets.toml` read live; the picker by name and body; the inline reply's text, variables,
  caret and recipients; Ctrl+' and Ctrl+; again closing it; a new message from the list with
  subject, recipients and caret; a card; the palette's Use Snippet…; a faulty file's toast; the
  empty picker).
- New DSL (`selftest/writing.rs`): `picker-type`, `picker-move`, `picker-choose [TITLE]`,
  `picker-close`, `picker-shown-in`, `open-draft NAME`, `assert-picker`, `assert-snippets`,
  `assert-quick-caret`, `assert-sent-at`, `assert-scheduled`, `assert-scheduled-sends`;
  `assert-outbox-at NAME WHEN` (`selftest/outbox.rs`); `assert-dialog` and the a11y `dialog`
  scope know the picker; the card's problem label is `composer-problem`.

## Phase C, track L step 2 (2026-10-29)

- **Enter is Reply All** (BRIEF-C: Superhuman's Enter; `r` stays Reply, `a` Reply All).
  `[reader] reply` now says what Enter in a conversation (Quick Reply) and the reply box's own
  line answer, and defaults to `"all"` (`DefaultReply::All`; the default file says so).
  Upstream's issue #8 made `"all"` turn Reply itself into Reply All (`r`, Ctrl+R, the cards' first
  button "Reply all", the second Reply to Sender, the menu and the palette's titles): that is gone,
  since `r` must stay Reply; the cards always show Reply, Reply all and Forward, the menu Reply,
  Reply All and Forward. Reply to Sender stays a command (palette). A message to the user alone is
  answered as Reply does either way. Upstream's DESIGN.md, DECISIONS.md (issue #8's ADR) and
  TESTING.md still describe the old `"all"`; USER_GUIDE.md says the fork's. Pinned the old default and changed on purpose:
  `reply-default` (rewritten: Enter and the box answer everyone, `r` the sender, `"sender"`
  turns Enter back), `reply-inline`, `reply-quick`, `reply-quick-drafts` (Enter, a box's send and
  Pop Out with nothing typed answer everyone), the cards' unit test.
- **Whom a reply answers** (the visual QA's major): reader.md's phase C section. `r`, `a` and Enter
  answer the latest message unless the user moved; the closed box names that person first.
- **A reply's quoted history is folded** behind the reader's "•••" pill (BRIEF-C), as the reader
  folds it: only in a card whose draft answers a message (`card::answers`: a reply or reply all
  made here, a reply's draft reopened, a draft from elsewhere with In-Reply-To; never a new
  message or a forward, whose quotes are what they send), only the trailing quote (the last
  outermost cite quote that nothing but blank lines or the signature follows), and only with the
  attribution line just before it ("On …, Grace wrote:", `domain::quotes`' rule ported to
  `helper.js`), which becomes the one pill. The review's third round found the first version
  folding every top-level cite quote of any body (new messages, forwards, every reopened draft)
  and drawing the line before each as a pill: a draft answered inline hid the user's answers,
  one pill didn't open, and a new message's "Here is what the client asked for:" vanished.
  `helper.js` marks the two nodes (`data-sw-fold="pill"`/`"quote"`; the cleaner keeps no such
  attribute, so the draft never holds them) and `<html>` `sw-quote-folded`; the editor's
  stylesheet hides the quote and draws the pill as the attribution's `::before`, the
  attribution's box as wide as the pill. It opens for the rest of the card's life on a click on
  the pill, the caret going onto it (Down from the line above, Ctrl+End, Select All:
  `selectionchange`), or an edit that leaves the quote no longer trailing or its attribution
  gone (a `MutationObserver`); a press beside or under the pill puts the caret at the end of the
  line before the fold instead of opening it (people click under their text to type), and an
  image or file dropped there goes in at that place too, the quote still folded. The
  body's text (`text`, `isEmpty`) counts the folded quote.
- **Only attribution lines read small and muted**: the stylesheet's `:has(+ blockquote)` rule (the
  approved mock's, upstream's) took any line before a quote, so a user's answer between a
  quote's parts, or "Here is what the client asked for:", read 11 px and muted. `helper.js` marks
  the lines that read as attributions (`data-sw-attribution`) when content is set or pasted, and
  a marked line loses its mark once it no longer reads as one (typed into; the empty line Enter
  splits off it). `composer-drafts` checks the fold (one pill, the marks kept out of the draft, a
  press under the pill, Down, Ctrl+End, a click), a reply's draft reopened folding again, a draft
  answered inline (reopened: nothing folded, the answers at the body's size, a last quote headed
  by the user's line open too), typing after a trailing quote opening it, a forward, and a new
  message with quotes (reopened too). With the fold put back to any cite quote in any card (a
  mutation run), the forward, the inline-answered draft and the new message fail.
- **"Send undone"** after Undo send, wherever the draft went back (it was "Sending undone: the
  reply is back under its conversation", which ran across the restored reply's footer); the toast
  keeps off the open reply (reader.md). `reply-send-done` checks it (`assert-toast-clear`).
- **Scheduled sends say when on their drafts' rows** (BRIEF-C): the copy of a scheduled draft in
  its Drafts folder says "Sending tomorrow 08:00" (`time_picker::describe`, as the toast) in its
  row's date place, a phrase the subject and snippet make room for, and to a screen reader;
  message and conversation rows (a Drafts view's conversation row: its latest message is that
  draft). The outbox reads the scheduled drafts' copies (`Store::draft_copies`: by Message-ID, as
  `draft_for_message` matches them) after every outbox read, when mail comes into or leaves a
  folder while a send is scheduled, and when one stops being scheduled (`outbox::mark_copies`), and
  hands the list `MessageId → when` (`MessageList::set_scheduled`; the rows bound then show it at
  once). A draft never uploaded has no copy to mark. Scheduled Messages' rows say it the same way
  ("Sending tomorrow 08:00 · to Kari Nordmann"). `send-later` checks a message row, a
  conversation row and the row after Send Now (`outbox-copy NAME` files the copy as the engine's
  upload and the next sync would; `assert-list-row POS scheduled|label-ends`).
- **Pickers** (the visual QA's minor): the field reads in the rows' face and size (the palette's
  large monospace query read like a terminal) with a glyph of what it reads (`Spec::icon`: the
  composer's Send Later clock for a time, `appointment-soon` carries an orange badge; the
  magnifier for snippets); snippet previews read as the snippet would go in, filled in for the
  draft it goes to (`snippets::Context`: its first recipient, else the snippet's own To, and the
  sender), `{cursor}` gone, left-aligned in one column after the names (`Details::Column`); the
  time picker's rows start with a capital ("Tomorrow 08:00" beside "Tomorrow 09:00"; a sentence
  still says "Sending tomorrow 08:00"), dates month first as the list and the reader write them
  (`when::label` "Thu, Nov 5, 08:00", `when::day` "Fri, Oct 30"), a typed date that names its day
  doesn't repeat it. `send-later` and `snippets` check the glyph and the column
  (`assert-picker icon|details`).

## Still not done

- Enter inside a quote stays in it (WebKit's default; Thunderbird and Apple Mail split a cite
  quote there): an answer typed that way is part of the quote, reads as quoted to the
  recipients, and folds with it when the reply's draft reopens. WebKit's
  `InsertNewlineInQuotedContent` editing command breaks a cite quote and is reachable through
  `execute_editing_command`; wiring Enter to it in a quote is the fix.
- Warming the submission limit when a reply starts.
- A copy uploaded to the Drafts folder before the draft was scheduled stays there until the
  message is sent, as for any queued send (its row says when it goes since phase C): sending it
  from another device would send it twice.
- Snippets typed with `;` inside the body (Superhuman's inline menu) and placeholders the user
  fills in; snippets with attachments.
- Send Later from a card with Send and Archive (Superhuman's Send Later + Done) isn't offered.
