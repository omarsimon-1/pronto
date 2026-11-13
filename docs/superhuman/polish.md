# Superhuman pass, phase B: polish of phase A (track P)

What track P changed outside the message list, why, and how each change is tested. The list
(`app/src/list/**`) and the composer are other tracks'.

## Command palette

- **Several words are one substring.** The query's whitespace used to be folded away, so
  "mark as read" could only match scattered letters: after one run of Mark Folder as Read,
  its recency boost put it above Mark as Read, and Enter marked the folder. A query of several
  words is now tried first with its words one space apart (`domain/src/palette.rs`
  `spaced_query`), as a substring of the title or an alias; then as before. `score()` (the
  folder pickers' ranking) does the same.
- **A title before an alias that matches as well.** At equal kind and score, a row found by its
  title comes before one found by another word: "mark" lists the read toggle before "Archive
  (mark done)".
- **Down and Page Down keep the selected row clear of the bottom fade.** While rows remain below,
  the next one scrolls into view too (a row, 40 px, is taller than the 36 px fade); the last row
  scrolls to the list's end, so nothing is left below to fade. The palette's pooled rows past the
  last one shown are hidden and don't count.
- **The fade itself** went to `transparent` (transparent black), which drew a grey band halfway
  (#C0C0C0 on white). It now fades to the card's own colour at no alpha.
- **The card is a raised surface** (`--sw-raised-bg-color`, `UiRoles::raised`,
  `theme::raised_surface`). Omarchy's popover colour is `darker_background`, darker than the
  window: Tokyo Night's #0E0E14 read as a hole in the dimmed window. The card keeps the popover
  colour where it is at least as light as the window and secondary text reads on it (Adwaita);
  otherwise a dark theme's card is the window stepped toward `lighter_background` as far as
  secondary text keeps 4.5:1, a keycap's fill included (Tokyo Night #1E202F), and a light
  theme's is the window's colour (Catppuccin Latte's popover left its secondary text at
  4.08:1). After the phase B merge the snippet and time pickers share this card and open over
  the composer card (`ui.composer`), so one elevation order holds in every theme: window ≤
  composer card ≤ raised card (`theme::surfaces_rise_in_one_order`). Adwaita dark keeps its
  popover colour (#36363A over the card's #2F2F33); the popover counts only where it is at least
  as light as the composer card's own step too (Vantablack's #070707 no longer does). The
  first cut stopped at 50 % (`RAISED_MIX`, gone): in Sealwax's own Tokyo Night palette (the
  review's shots: picker #1F2231 against the dimmed card's #1E2132, 1.01:1) and in 15 of the 17
  Omarchy dark themes the composer card stood above the pickers that open over it. Now the card
  steps as far as the keycaps read (Sealwax's Tokyo Night, matte-black, Catppuccin, Kanagawa…
  reach the composer card's `lighter_background`: Tokyo Night's pickers are #24283B, 1.10:1
  against the dimmed card), and where they stop short the composer card comes down to the same
  step (Omarchy's tokyo-night #1E202F, Gruvbox, Everforest, Nord, Osaka Jade). The theme tests
  now cover Sealwax's own Tokyo Night palette too (`all_themes`). Secondary text on a *hovered* palette row (8 % foreground over the card) isn't
  held to 4.5:1 (P's card wasn't either: Tokyo Night 4.16, Gruvbox 4.13); the lighter card
  lowers it in some themes (Catppuccin 5.02 → 4.38).
- **Title and alias sizes** apply inside `.sw-palette-card` only: the Move to… and Go to Folder…
  pickers' rows are body text again.
- **Role folders show no raw IMAP name** in the pickers: the server's word only when it is
  another word than the role's, by its last level ("Sent Mail" beside Sent on Gmail, "Sendt"),
  never "INBOX" beside Inbox.
- **palette.toml outlives the window**: `tests/selftest/palette-recent.sealwax-test` runs a
  command, closes the window (background mode), builds a new one and finds it under Recent.

## Reader

- **The toolbar keeps to the thread's column.** Back and the toolbar sit in a clamp as wide as
  the conversation (`Window::follow_reader_column`, `reader::column_width`, following the reading
  size): at 1440 px Back starts at x=360 and Next ends at x=1080, where the conversation does
  (they sat about 330 px out on each side). The heading takes the rows' inset (x=366). Both
  clamps are in px, as the theme's text sizes are: the desktop's text scale doesn't change the
  message text, and the conversation's clamp, in libadwaita's default unit (sp), narrowed to
  653 px at the owner's 0.9091 (`gtk-xft-dpi` 89367) while the bar kept 720 (the review).
- **Star** is the outline until the selection is starred, then the filled star in the star
  colour (`Toolbar::set_starred`, from the selection's state, not from its title).
- **The read toggle in More** says what `u` would do (Mark as Read or Mark as Unread) and shows
  `u` beside it (the menu's `accel` attribute: Junk shows `!`, Print Ctrl+P); it replaces Mark
  as Unread, which had no key since `u` became the toggle.
- **Dates read as the list's**: rows "09:30", "Yesterday", "Oct 15", "Oct 15 2025" in the list's
  small spaced capitals; card headers "09:30 (2 h ago)", "Yesterday 21:41", "Oct 22, 02:33";
  details "Sunday, October 4, 2026, 09:30 (+02:00)". The weekday forms are gone, so a row and
  its card say the same day.
- **Emphasis**: a read row's sender is regular weight, the expanded card's sender bold at the
  title size (a row's name outweighed it).
- **The heading drops "Re:"** and its twins (`message::base_subject`, as the list's rows).
- **Folds** show their count in a quiet round badge, "147 more messages" (the stacked rules read
  as a menu).
- **Heights measured with the quotes opened aren't kept.** The height cache keys say "quotes
  collapsed"; a height measured while the user had "•••" open would have been the next fresh
  open's starting height. Each body now tracks `quotes_open` from its view's `Quotes` events.
- **A page built again for the same conversation never stays laid out ahead.** `show_again`
  (the order or Reply's setting changed) and a folded message's reload built a new page and kept
  the old one warm, stale, ready to be swapped back in. `present` keeps the replaced page only
  for another conversation.
- **Dates are said afresh when a page comes on screen**: a page laid out ahead when it is
  swapped in (`Page::redate`), and the shown page when the reader comes on screen
  (`Reader::redate_shown`, from the window's reader-on-screen handler). Opened from a Focused
  list, the page laid out behind it when the cursor came (or left there by Escape) said "(30 min
  ago)" hours later (the review); "25 min ago", Today and Yesterday no longer date from when the
  page was built.
- **Pages laid out unseen stop being drawn once settled (battery).** WebKit makes a view's next
  frame only once the last was drawn, so the Stage drew its staged pages into an empty clip every
  frame, and an animated GIF or CSS animation in a neighbour kept the web process and the frame
  clock busy while the user read another conversation. A staged page is left undrawn once it is
  ready and has settled (`Inner::settled`): every view loaded (its `load` event: images, `cid:`
  parts, fonts in), painted, measured at its width, and unchanged for `live::SETTLE` (1 s; a
  load, a new height or width is a change). It is drawn once more (the frame WebKit made
  meanwhile is taken) and then left: its views stop. Shown, given another width, or changed, it
  is drawn again and WebKit goes on; a view's change reaches a page left undrawn through
  `widgets::redraw_up` (GTK passes a draw up only to the first widget already waiting for one,
  and a page its parent skipped goes on waiting). `MailPanes` does the same for the reader kept
  warm behind a Focused list (`Reader::is_quiet`: nothing being read or laid out, the shown page
  and every page ahead settled).
- **Why settled and not painted** (the review): a page left as soon as it had painted at its width
  stalled WebKit, and late layout was no longer measured while it waited: a slow inline image, a
  remote image from an always-allowed sender or a web font came into a view that made no more
  frames, and a transition that played late stopped one frame in (a 10→400 px block measured 409
  of 487 px). The page then jumped, or painted its image late, when shown. Now an animated page
  whose map comes 1.5 s late is drawn until the map is in and measured with it (92 → 252 px), and
  a page that settled and then grows is drawn again until the growth is measured (97 → 487 px);
  the reviewer's script shows its staged view at 706×487 before `k`. Taking out the settle leaves
  the growth at 103 px; taking out both the load rule and the redraw leaves the image unmeasured.
- **Left**: a page that animates (so it was left with a frame not taken) and also changes its
  size later with no load event (a CSS animation of its height starting seconds after it opened)
  isn't measured until shown: WebKit runs no layout in a view that waits for its frame to be
  drawn, so nothing reports the change. Mail like that is rare; drawing quiet pages now and then
  would cover it at a cost in battery.
- **Prepared bodies are shared, not copied.** A prepared-cache hit cloned the whole HTML string on
  the GTK thread (inline images were already shared `glib::Bytes`); `Body::Html.html` is now an
  `Arc<str>` the cache and the card share. A body the cache keeps is copied once onto the GTK
  thread's heap, where it doesn't sit long-lived in the prepare worker's malloc arena among
  short-lived allocations; one the cache doesn't keep (no stored message) isn't copied. The
  difference this makes is within the noise of headless `reader_memory` runs on a shared machine
  (see Measured).
- Nits: `update_position`'s doc comment is back on it, `assert-reader` and `assert-reader-more`
  have their own docs, and the reading measure is one constant (`theme::READING_MEASURE`, 70).

## Engine

- **The prefetch fetches the copy the reader shows.** `body_candidates` kept the lowest-id copy
  outside the Inbox and dropped a copy when any other had a body; the conversation shows the best
  copy by `conversation::rank` (Inbox, Archive, All Mail, Sent, other folders, Drafts). The
  prefetch now ranks copies the same way in SQL (`rank_sql`, tested equal to `rank`) and takes
  the best copy unless it has its body: a reply archived from Sent prefetches its Archive copy.
  The cost: on Gmail a message whose best copy has no body is downloaded even when another copy
  (a label's) already has its body, counting toward Gmail's daily IMAP download allowance; the
  old rule saved that download and made the reader fetch the shown copy when opened.
- **The IDLE connection's inline body fetch has its own limit**
  (`SyncConfig::inline_bodies`, 8 s; 2 MiB at most go that way), on its FETCH alone
  (`bodies::fetch_arrived`'s `limit`): the store's reads and the cache's cap around it aren't
  the connection's, and a slow disk mustn't drop a healthy IDLE connection. Past it the
  connection is in no known state, so it connects again, and the fetch's claim hands what it
  had left to the body connection: a reader who opened the new message waited for the read
  timeout (a minute).
- **Losing a healthy IDLE connection starts its backoff afresh** (`Kept::Lost { passed }`): one
  that had a pass go through reconnects at once, as the command connection does after a good
  pass; only failures to connect and pass add up. Each loss in a long session (a server's
  restart, a stalled inline fetch) used to add to a backoff that doubled up to `backoff_max`
  (five minutes) for the rest of the session.
- **No Place pass into an `\All` folder whose first sync isn't done** (`Facts::first_synced`,
  `SyncState::Ready`). An All Mail opened once and left mid-first-sync got the Place pass on
  every Archive, which went on with that first sync on the command connection.

## Store

- **A merged-away conversation's id never comes back in its own commit.** SQLite gives a new row
  the highest id plus one; a merge deleted the merged thread at once, so a new conversation in
  the same commit took its id, and `merged_threads` (`from` 88 into 19) then moved the new
  message's rows too (the replay test's seeds 1051, 1093, 1101). The merged thread's row now
  stays until the commit's end (`writer::retire_merged`, in the three write paths), so a
  `from` always names a conversation that is gone.

## User guide

`docs/USER_GUIDE.md` now describes Focused and Split (Reading Pane), the sidebar toggle, the one
header row with search on demand and the filter pill, the width table without the rail, Ctrl+K,
the palette's suggestions, other words and places, Keyboard Shortcuts' keys (`h` is Remind Me),
`u` for read or unread, the folds (how long a run of read messages folds: a fold holds three at
least, the first message and the one before each card stay), mark-read-on-open in Focused mode,
the toolbar and More, the prefetch of the Inbox and its conversations (also the `prefetch_days`
row), Remind Me as the command not built yet, and F9 left to Omarchy.

## Tests

Each change has a test that fails on the code before it (checked by putting the old code back):

| change | test | before |
| --- | --- | --- |
| several words, title before alias | `palette::tests::several_words_are_found_as_typed_whatever_the_boosts`, `a_title_comes_before_an_alias_that_matches_as_well`; `palette-keys` "mark" | Mark Folder as Read first; Archive first |
| fade clearance | `palette-keys` `assert-palette clear` after Page Down and Down | row 8 ended at 370 of 396 px (fade 36) |
| raised card | `theme::the_palette_card_is_raised_and_reads` (all 22 stock themes) | Tokyo Night's card darker than its window |
| role folder names | `move_to::tests::role_folders_show_no_raw_imap_name` | "INBOX" beside Inbox |
| palette.toml | `palette-recent.sealwax-test` | new |
| toolbar column | `focused` `assert-aligned reader-back/reader-next reader-page` | 20 and 1420 vs 360 and 1080 |
| at a text scale | `focused` `text-scale 0.9091`, the same two checks | the column at 393 and 1046 |
| Star, read toggle | `toolbar` unit tests; `triage-actions` `assert-icon reader-star`, `assert-reader-more toggle-read … u` | filled star; Mark as Unread without a key |
| heading, dates | `conversation`/`format` unit tests; `reader-conversation` heading | "Re: Cabin trip…", "15 Oct" |
| quotes height | `reader` `assert-reader height-kept shown/other` | the opened height kept |
| show_again | `reader-conversation-order` `ahead-has trip-6 false` | the stale page kept |
| redate | `reader-ahead` `conversation-clock 2`, `date-lacks "min ago"` | "(40 min ago)" two hours later |
| redate on screen | `reader-ahead`'s Focused part: Enter two hours later, and again after Escape | "(30 min ago)" |
| battery | `reader-quiet.sealwax-test` `ahead-drawn`/`warm-drawn 1000 1` once `ahead-settled`/`quiet`; `ahead-drawn 300 5+` while settling | 62 and 124 draws a second with an animated neighbour, 62 behind the list; now 0 |
| late layout | `reader-late.sealwax-test`: a late `cid:` map (`files-late-image`), a block that grows 3 s after it settled, in Split and behind a Focused list; `ahead-measured` | 92 px without the load rule and the redraw; 103 without the settle |
| IDLE backoff | `actor::a_healthy_idle_connection_lost_connects_again_at_once` | the second loss waited `backoff_min` |
| Arc bodies | `cache::tests::a_kept_message_shares_its_html_rather_than_copying_it` | a copy per hit |
| prefetch copy | `bodies::tests::the_prefetch_takes_one_copy_of_a_message` (Archive copy), `the_prefetch_ranks_copies_as_the_conversation_does` | the older Sent copy |
| inline limit | `actor::a_stalled_inline_body_fetch_hands_the_body_on_in_time` (fake `hold`) | unanswered after 20 s |
| All Mail place | `sync::tests::a_move_never_first_syncs_all_mail`, `gmail::an_archive_doesnt_go_on_with_all_mails_unfinished_first_sync` | headers fetched |
| merged ids | `thread_tests::a_merged_conversations_id_stays_gone_in_its_commit`; the replay with seeds 1051, 1093, 1101 | the new conversation took id 88 |

`a11y-window.sealwax-test`'s Escape from the message's WebView now waits for the list (`wait-until`):
the WebView hands a key it doesn't use back to the window only once its web process answers,
so the immediate check raced it under load. Phase A's binary failed it 1 run in 9 at load
about 20, as this build did (the stage always drawing: 3 in 9); at load 25–29 this build failed
2 runs of 3 and phase A's none of 3. It still fails if Escape doesn't go back; with the wait,
3 of 3 pass.

New self-test commands: `assert-palette clear`, `assert-icon`, `assert-aligned`,
`assert-reader height-kept`, `assert-reader-more ACTION TITLE KEY`, `conversation-clock HOURS`,
`text-scale FACTOR` (GTK's `gtk-xft-dpi`, as GNOME's text-scaling-factor sets it),
`assert-conversation ahead-has|date-lacks|ahead-drawn|warm-drawn|ahead-settled|ahead-measured|quiet`
(`ahead-drawn MS N+`: at least N draws). A files sample, `files-late-image`, animates and has an
inline image without a size of its own, stored as a large message's part.

## Measured

`reader_memory` (release, headless, 1440×900, as docs/superhuman/reader.md runs it; the
"before" binary is the integrated phase A tree built the same way), one run at a time on
display 71, alternating. The machine was shared with three other tracks (load 13–27, 10 GB
swapped by their builds, their WebKit processes sharing our libraries' pages):

| | tree growth, MiB (bar 19.1) | the app's own growth, MiB | tree Pss open, MiB |
| --- | --- | --- | --- |
| before, 12 runs | 13.3–53.9 (median about 21) | 3.8–26.2 (median 6.9) | 201–313 |
| after (final code), 4 runs | −37.6, 16.0, 32.0, 33.7 | −5.5, 4.5, 6.1, 19.4 | 214–272 |

- The bar can't be judged here: the unchanged binary itself ranged 13.3–53.9 MiB, and runs swing
  by tens of MiB in file Pss (pages shared with the other tracks' processes) and in the web
  process's anon memory (+5 to +17 MiB in either build, WebKit's own). Swapped Pss of the runs'
  processes was 0, so the kernel's swapping wasn't it.
- What the reader does about growth: a cache hit copies nothing, the cached copy lives on the
  GTK thread's heap, and pages laid out ahead stop rendering once settled. The app's own growth
  is back in the old binary's range once the copy moved off the worker's arena (that first
  version measured 6.4–17.9 MiB in five runs).
- The review's own runs (release, 4 + 4 alternating, display 78, load 5–10) found no difference
  between the builds: tree growth after 33.1, 16.3, 21.7, 28.9 MiB against 35.6, 25.1, 11.0,
  38.9 before; the app's own growth about 5 or about 19 MiB in both (bimodal). The memory item
  stays open until the owner's `--live --judge` run.
- Also tried, measured, and left out: a 4 MiB prepared-body cap, and returning freed heap with
  `malloc_trim` once the reader rests: neither moved the numbers beyond the noise.
- No leak: over 800 opens both level off (app anon after 200, 400, 600 and 800 opens, a run
  with a larger store: before 202, 206, 214, 215 MiB; the first after version, its copy still
  on the worker's arena, 234, 236, 242, 243 MiB).
- Battery: with an animated message laid out ahead, the stage drew its pages 62 times a second
  (124 with two pages), and the reader warm behind a Focused list 62 times a second; now 0 once
  settled, a second after the page last changed (`reader-quiet.sealwax-test`, checked against
  the old drawing; 17–19 draws in the 300 ms while it settles).

## Left

- Rose-pine Dawn's accent bar on the one-pane list's selected row stays 2.78:1, the listed
  exception (`ACCENT_UNDER_THE_TINT`). Keeping the list's lightness under the tint leaves the
  selection invisible there (0.0095 from the list in OKLab), so the fix is a bar colour of its
  own (the accent stepped toward the foreground until 3:1) in the rows' CSS, which is the list's.
- The first-account view (All Inboxes or the Inbox, by timing) is unchanged: B1's list wiring.
- `reader_memory`'s growth bar needs a quiet machine to judge (the owner's laptop, with
  `--live --judge`); the web process's growth is WebKit's.
- A page laid out unseen that animates and also changes its size later with no load event is
  measured only when shown (Reader, "Left").

## Phase C, track L step 2: the review's remaining notes (2026-10-29)

Each nonblocking note of this track's final review, with what became of it:

| note | now | test |
| --- | --- | --- |
| the HTML copied before the cache's size check (5 MiB: about 1 ms, then not kept) | `cache::keeps` first; a body over `PREPARED_LARGEST` isn't copied | `cache` unit test |
| `Page::redate` left a carded message's hidden row as built | `collapse_item` says the row's date as of then | `reader-ahead`: a day later, the collapsed row has no time, nor "Yesterday" |
| More's "U" (GTK's capital: Shift+U is Show Unread Only) | the rows' letters written as typed, "u" (`toolbar::write_keys`) | `triage-actions` `assert-reader-more … u u` |
| the toolbar 10 px past the column in Split at 1100 px | the strip's margin is the column's (`.sw-reader-strip`) | `reader-bar` in Split at 1100 |
| `set_starred` set the icon on every refresh | compares first | — (one icon set per change) |
| the inline body fetch's fixed 8 s for up to 2 MiB | 8 s plus the claimed bytes at 128 KiB/s (`bodies::arrived_limit`; 24 s for 2 MiB) | `bodies` unit test |
| USER_GUIDE's "to me, Tomas and 2 others" (it can't occur) | "to me, Tomas, Leah and 2 others" | — |
| the arrows after More with several selected (pre-existing) | a spacer of their own | `reader-bar` in select mode |

**The palette's secondary text on a hovered row** (the phase B merge's open note, this track's
palette): the rows' secondary text, their aliases, the pickers' details and the keycaps take
`--sw-raised-secondary-fg-color`, `theme::raised_secondary_text`: secondary text that reads at
4.5:1 on the raised card, a hovered row and the selected row there, each with a keycap's fill
(the app's own fell to 4.16:1 on a hovered row in Tokyo Night, 4.13:1 in Gruvbox, 4.38:1 in
Catppuccin; Tokyo Night's is now #a2aacd). `theme::the_palette_rows_secondary_text_reads_hovered_and_selected`
(every theme; Tokyo Night's measured case); the five CSS snapshots gain the variable.

Also this step, from the visual QA: Star's outline at full strength (`window::glyph`), the
toolbar's buttons centred at every width, the reply's target, drafts, the heading's subject and
the dates (reader.md's phase C section); `j`/`k`/`n`/`p`'s titles (keys.md); pickers, Enter =
Reply All and the composer's quotes (writing.md).

**Tools.** `tools/headless.sh` waits for gtk4-broadwayd until it says where it listens and that
socket is there, up to `SEALWAX_BROADWAYD_WAIT` (30 s): it gave up after 5 s, which a busy
machine passed (notify-click's "did not become ready" in the phase B sweep, at a load of 30); a
daemon that exits still fails at once. Checked with a daemon that takes 7 s to start (the old
script fails, the new one runs) and one that exits (fails at once, its log shown).

Still left: `reader_memory`'s growth bar needs the owner's `--live --judge` run; outside Hyprland
the reader bar's end window controls sit beside the toolbar's strip, which then centres half
their width left of the column.
