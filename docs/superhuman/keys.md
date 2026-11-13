# Superhuman pass: keys and the command palette

What the keys area changed, and why. Superhuman's keyboard as the owner knows it, inside
ADR-017's rules (one registry, `keys.toml`, the audit); this file stands in for ADR-017's
amendment until DECISIONS.md and DESIGN.md's Keyboard table catch up.

## Ctrl+K: a third place for a key, "yielding"

Superhuman's Cmd+K opens its palette from everywhere, and in the composer's editor it inserts a
link. GTK had no place for that: a Ctrl key of a window command was an application accelerator
(the window's capture phase, before the focused widget), and Ctrl+K is the composer's Insert
Link (`TEXT_KEYS`), so the audit refused it.

- `keymap::placement` decides where a key works: `Accelerator`, `Scoped`, and now `Yielding`. A
  key of a global action that is one of `TEXT_KEYS` yields when the action says so
  (`ActionSpec::yields`, only Command Palette): it works anywhere in the window **after** the
  focused widget, which keeps it where it uses it. Nothing else changes placement; Compose on
  Ctrl+Z is still refused as a text key.
- The window's yielding keys are a `ScopedShortcuts` of `Scope::Window` attached to the window in
  the bubble phase (`AppState::install_palette`; `keys.toml` changes update it).
- The audit: an accelerator of the same key runs first everywhere and a List or Reader key takes
  it there first, so both are conflicts (`Taken`, reported on the key `keys.toml` gives); the
  Composer scope may keep it, which is the point.
- Result: Ctrl+K opens the palette from the list, the reader (a focused WebView body hands it
  back), the search field, the sidebar and the quick reply (its guard stops Reader keys only); in
  a composer card it stays Insert Link (and reaches the palette when Insert Link can't run).
- Its key again closes it (Ctrl+K, F1 or Ctrl+Shift+P): never two palettes. It stays shut while
  a dialog is open, as Superhuman's does in its settings.
- Menus show Ctrl+K (`menu.rs` sets the item's `accel`, since it is no accelerator); the welcome
  page's hint names the palette's first key ("or press Ctrl+K for every command").

## The palette

`app/src/palette.rs`, rewritten. An overlay layer over the whole window (the window's content is
moved into an overlay at install), not an `AdwDialog`, which libadwaita centres.

- **Look**: 680 px wide (window width minus 32 px when narrower), its top 20 % down, a backdrop
  of `--shade-color` × 2.4 (60 % black in dark Adwaita), the query 20 px in the theme's
  monospace face, rows 40 px with the title and right-aligned keycap pills of the first key, the
  list at most 396 px (about nine rows) with the last one fading into the edge; quiet uppercase
  headings only on the empty query; no close button. Fades in over 100 ms, out over 80 ms
  (instant where the desktop asks for less motion). CSS: the `superhuman: palette` block.
- **Instant**: built once per window and hidden between uses; rows are a pool whose labels change
  in place, new rows only when more are shown than ever before; no per-key rebuild.
- **A dialog without AdwDialog**: the dialog role and modal property, Tab and Shift+Tab stay in
  the field, Escape or a click on the backdrop closes, the focus goes back where it was (else to
  the list), focus moving elsewhere in the window closes it.
- **Ranking** (`domain::palette::find`): Stet's matcher, plus
  - aliases from the registry (`ActionSpec::aliases`): done → Archive, trash/bin → Delete,
    spam → Move to Junk, label/folder → Move to…, flag → Star, snooze/later → Remind Me…,
    settings → Preferences, sync/fetch → Refresh, filter → the four filters, … A row found by an
    alias reads "Archive (done)", but only where the alias matches in a better class than the
    title (from its start, a word's start, inside a word, scattered): "inb" is Go to All
    Inboxes, not "Go to All Inboxes (unified inbox)";
  - a query with two neighbouring letters swapped ("arhcive") still finds it, where the swap
    makes a whole substring (also when the letters as typed are scattered through the title);
  - scattered one-letter matches of a query of three letters or more need 3 points a letter
    (mostly word starts and runs), so "arch" no longer lists half the registry;
  - ranking: by how a row was found first (a substring as typed, then a swap, then scattered
    letters), then by score plus boosts, so a boost reorders rows found the same way only and a
    typo never passes a real substring. Boosts: the commands run last (360 points for the last,
    30 fewer for each older one) and the suggested ones (150).
- **Empty query** (`domain::palette::browse`): Suggested first: on a selection Archive, Reply
  (Reply All when that is the default), Star, Move to…, Mark as Read or Unread (Remind Me once it
  exists); in a composer card Send, Attach File…, Add Cc and Bcc, Discard Draft; otherwise
  Compose, Search, Go to Folder…, Go to Inbox. Then up to five Recent, then the rest by category.
- **Recency**: `$XDG_STATE_HOME/pronto/palette.toml`, `recent = ["archive", "folder-12",
  …]`, newest first, at most 12, written when the palette runs something, read leniently. Its own
  file, so `layout.toml` (the layout area's) is untouched.
- **Places**: every folder that holds mail is "Go to <name>" once something is typed (role names,
  else the path: "Inbox / Receipts"; with several accounts "Go to Sent — Work"). A folder gone to
  lately is listed under Recent.
- Go to Account 1 to 9 exist only for the accounts there are, titled by the account's name.
- Mark as Read or Unread shows what it would do, Mark as Read or Mark as Unread, and stands
  for the command of that name (one row, with `u`). With one account, Go to Inbox, Sent, Drafts
  and Archive stand for their folders' places (`go_to::command_places`). Nothing else is
  merged by title: two accounts of the same name keep both Go to rows (`g 1`, `g 2`).
- Moving up to a group's first row scrolls its heading into view too.

## Keymap changes

| Command | Before | Now | Why |
| --- | --- | --- | --- |
| Command Palette | Ctrl+Shift+P, F1 | Ctrl+K, Ctrl+Shift+P, F1 | Superhuman's hub |
| Mark as Read or Unread (`toggle-read`, new) | — | u | Superhuman's U; as Star's toggle: read unless all are, then unread |
| Back to List | Escape, u | Escape | u is the toggle |
| Mark as Unread | Shift+U | — (palette, u) | Shift+U is a filter |
| Remind Me… (`remind-me`, new, not built) | — | h | Superhuman's H, held until snooze exists ("Not available yet") |
| Keyboard Shortcuts | ?, h, Ctrl+? | ?, Ctrl+?, Ctrl+H | h went to Remind Me; Ctrl+H is the unshifted key the audit wants |
| Show Unread Only (`show-unread-only`, new) | chip | Shift+U (list) | Superhuman's filter; again shows everything |
| Show Starred Only (`show-starred-only`, new) | chip | Shift+S (list) | the same |
| Show Attachments Only, Show All Messages (new) | chips | palette | the chips row goes (layout area) |
| Go to Archive (`go-to-archive`, new) | — | g then e | Superhuman's Done: the Archive, else Gmail's All Mail |
| Go to Folder… (`go-to-folder`, new) | — | g then l | Superhuman's label picker: Move to…'s picker over every folder |
| Select All (`select-all`, new) | swallowed | Ctrl+A (list) | rows load first; the list lets through only a key a List command has, so GtkListView's Ctrl+Shift+A (unselect all) stays swallowed |
| Toggle Sidebar | Ctrl+Shift+S, F9 | Ctrl+Shift+S | F9 is Omarchy's voxtype push-to-talk, now in `GRABBED` |

The audit passes on the defaults; `docs/USER_GUIDE.md`'s generated table is regenerated. The
filters re-focus the list (an empty view used to leave the focus nowhere).

## Speed: refreshing the commands

- `refresh_commands` ran synchronously on every store change event; now store events queue one
  run on the window's next frame (a tick callback; an idle without a window on screen). Selection
  changes still refresh at once, so a key pressed right after sees the right commands, and the
  palette runs a queued refresh before it opens. A filter change (a key, the palette, the
  filter pill) refreshes at once too, from the list's filter-changed handler, so Show All
  Messages applies from the key on (triage-keys checks it; wired when phase A was integrated).
- One `Mailbox::folders()` clone per refresh instead of three; `triage::availability` plans once
  per folder instead of once per selected message.

## Measured

Debug build, Broadway (software rendering), fixture store, 1440 × 900:

- first opening: 45 ms from the key to the first painted frame, 17 ms of it the palette's own
  (filling a new row pool); later openings 18.5 ms (2.7 ms own); a typed query ranks and fills
  its rows in about 1 ms.

## Tests

- domain: `the_design_keymap`, `aliases_are_lower_case_words_of_their_own`,
  `the_palettes_ctrl_k_yields_to_the_focused_widget`, `keys_that_yield_never_lose_to_another_key`,
  `the_superhuman_keys_resolve_where_they_work`, the palette's ranking, browsing and recent tests
  (with `a_typo_never_passes_a_substring_whatever_its_boosts` and the property test
  `substrings_come_first_whatever_the_boosts`); the user guide's table test.
- app: dispatch tests (window keys, `u`, filters, `g e`/`g l`, `the_list_knows_which_keys_are_its_own`),
  `u_marks_read_unless_all_are_read_then_unread`,
  `a_batch_in_one_folder_is_planned_once_and_says_why_not`, Go to Archive, folder names and
  `with_one_account_the_go_to_commands_stand_for_their_folders`, menu and shortcuts subtitles,
  the welcome hint.
- self-tests: new `palette-keys` (every focus, toggle, dialog guard, composer, aliases, places,
  recency, Tab trap, timings); `triage-keys` extended (u, h, Ctrl+H, Ctrl+K, filters, Ctrl+A,
  g e, g l, absent account); `triage-select` (Ctrl+A selects every row in the select mode,
  Ctrl+Shift+A changes nothing); updated a11y-dialogs, a11y-window, theme, triage-actions,
  triage-read, triage-undo, triage-frames, testbed-triage. The harness's `assert-dialog` and
  `assert-accessible dialog` count the open palette as a dialog; new `assert-palette`,
  `palette-shown-in`, `assert-list-filter`.
- examples: `palette` (overlay, toggle, Ctrl+K, aliases and when they show; it also expected
  three menu sections, stale since Check for Updates made four), `reader_keys` (u, and Ctrl+K
  from a focused WebView).

## Not done

- Multi-level undo (z z z): Superhuman's Z undoes the last action or send only, the triage-undo
  self-test and ADR-015 pin single-level Undo throughout, and the store area rewrites triage's
  targets in the same file; left for later.
- Remind Me itself (store schema, engine timer); its key and words are held. (Built in phase C:
  docs/superhuman/remind.md, with `g h` and Shift+E.)
- USER_GUIDE.md's prose still names F1, `h`, Shift+U and F9 in places (only the generated table is
  updated, by the brief's rule); DESIGN.md's Keyboard table and an ADR-017 amendment should follow
  this file.
- The reader's toolbar and the message menu still offer Mark as Read and Mark as Unread
  separately, and Mark as Unread's tooltip names no key now (Shift+U went to the filter); they
  could show the toggle, with `u` (reader and list areas).
- `g e` on a Gmail account opens All Mail, which may never have been synced: opening it starts
  that sync, as picking it in the sidebar does (the engine and store areas keep Archive itself
  from first-syncing it).
- Ctrl+H (Keyboard Shortcuts) is an application accelerator: it would shadow GtkFileChooser's
  Ctrl+H (hidden files) only in an in-process chooser; Sealwax's file dialogs go through the
  portal.

## Phase C (track L step 2)

- **Titles for conversations**: the list's rows are conversations, so `j` and `k` are titled
  **Next Conversation** and **Previous Conversation** (Superhuman's words), and `n` and `p`,
  within the conversation, **Next Message** and **Previous Message** (they were "Next Message"
  and "Next Message in Conversation": the palette's suggestions called `j` a message's key). Only
  the titles changed: the `keys.toml` names (`next-message`, `next-in-conversation`, …) and the
  keys stay. The reader toolbar's arrows say "Next conversation" and "Previous conversation".
  USER_GUIDE.md's table regenerated; the keymap's message for a list key names the new title.
- **Enter in a conversation is Reply All** by default (`[reader] reply = "all"`); `r` stays Reply,
  `a` Reply All (writing.md).
- **More shows `u`**, not GTK's "U", beside the read toggle (polish.md).
