# Superhuman pass: layout and chrome

What changed in the window's layout, the sidebar, the list's header and the empty states, and
why. Product decisions are the brief's (2026-10-28); this note records how they were built.

## One thing at a time: Focused and Split

- **Focused** is the default at every width. The sidebar is hidden and the list fills the window.
  Enter, `o`, `l` or a double click opens the selected conversation in the list's place. Escape
  (Back to List) comes back with the list's focus on the row that was left. In an open
  conversation, `j` and `k` move the list's selection and the reader follows. Triage
  auto-advances through the list's existing `advance_after`. When nothing is left (the selection
  clears), the window goes back to the list, which shows Inbox Zero.
- **Split** is today's list beside the reader. The **Reading Pane** toggle switches to it
  (`win.toggle-reading-pane`, View menu and palette, no default key under ADR-017).
- Below 900 px both modes show one pane at a time, as before.
- The list and the reader live in `window::mail_panes::MailPanes`, a small container widget,
  instead of an `AdwNavigationSplitView`. The navigation view slides its pages and can't be told
  not to. MailPanes switches by changing which child it allocates and draws, so the switch lands
  in the next frame.
- **The reader stays warm behind a Focused list.** It is mapped and laid out at the area's size
  and drawn into an empty clip (MailPanes' `snapshot`, as the reader's stage draws the pages it
  lays out ahead: WebKit hands a view its next frame only once the last one was drawn, so a reader
  never drawn left its views at their first frame and its neighbours never got ready; found when
  phase A was integrated, guarded by `focused-ahead.sealwax-test`). It is not targetable, can't
  take the focus, and is `Hidden` to screen readers. So moving the cursor lays the conversation
  out unseen, and Enter is a visibility flip.
- In Split's one-pane fallback (a narrow window) the reader is unmapped behind the list as
  before, so ADR-011's release of hidden views after 3 s still applies there.
- **Mark-read-on-open** now follows an explicit `Window::reader_on_screen()`
  (`connect_reader_on_screen`) instead of "the reader widget is mapped". That flag is true when
  the reader area is mapped and shown (Split's two panes, or the reader in front). Moving the
  cursor in a Focused list never marks mail read. Opening does, at once.
- **Focus after opening** goes into the conversation through `Reader::focus_message` (the selected
  card's body, else its header or row). This is the reader's own focus target. If the
  conversation is still being laid out, the reader bar's Back button holds the focus until
  `Shown` arrives. It moves into the conversation then only if it still waits where opening left
  it (nowhere, Back, or the list's rows): `/` or a click elsewhere meanwhile keeps it. The Reader
  scope's shortcuts are also attached to the whole reader pane, so a focus resting on Back never
  swallows `j`/`k`/`e`/Escape (the verified bug). Back has `focus_on_click(false)`.
- Switching Split to Focused keeps the reader in front only if it has the keyboard focus.
- Search (`/`, Ctrl+F) from a conversation open in the list's place shows the list again with the
  field. Go-to keys and picking a folder also come back to the list.

## The focus across width changes

Crossing 1280 px or 900 px keeps the keyboard focus where it was: on the list's row, in an open
conversation, in the sidebar. Before this fix, every crossing (even 1281 to 1279 px) moved it,
from a row to the list bar's search button, so `j` stopped moving the selection on the owner's
Hyprland half tile (1440 to 710 px) and Enter pressed Search.

The cause is in libadwaita 1.10 and GTK 4.24, not in the layout code:

1. On a breakpoint switch, `AdwBreakpointBin` (inside `AdwApplicationWindow`) records the focus.
   It then hides its child for one frame (`gtk_widget_set_child_visible(FALSE)`) to cross-fade
   from a snapshot.
2. Hiding the content flags GTK's `move_focus`. At the frame's end (after-paint), GTK walks up from
   the hidden content and grabs the bin.
3. With its switch pending, the bin then drops the focus it recorded. Instead it queues "grab
   the first child, then Tab forward" for the next frame, which lands on the list bar's second
   button.

HEAD escaped this only at the 900 px crossing, because `AdwNavigationSplitView` grabs the focus
again when it collapses. HEAD's 1280 px (rail) crossing lost the focus the same way.

The fix is in `Window::switch_class`, run from the breakpoint notification:

- The bin's one-frame hide is read on whatever holds the window's toasts up to the window
  (`Window::content_hidden`): the command palette makes its own overlay the window's content and
  puts the toasts inside it, so the bin hides that overlay, not the toasts.
- The focus is set again with `gtk_root_set_focus` on the same widget. GTK then clears its
  pending move, and the bin restores the focus after its frame. A focus on a control the new
  layout hides (Back as two panes show) waits on the list bar's first button instead.
- Focus moves the layout asks for during the switch (`focus_front`) are made once the content
  shows. So are moves away from what the new layout no longer shows: the sidebar it closed, the
  pane one pane at a time leaves out, a hidden control. This is `after_switch`, in an idle after
  the content maps again.
- `reader_on_screen` ignores the bin's one-frame hide. The reader no longer flips off and on
  screen on every switch, so a switch doesn't re-run mark-read or move the focus into it.
- MailPanes asks only for the list's width as its minimum beside the reader. A squeezed reader
  is cut at the area's edge, as before. In its switch frame, libadwaita lays out the old width
  class at the new width (Split at 1440 to 800 px). Counting the reader's minimum then made the
  split view give the hidden sidebar a negative width, with GTK warnings and a critical for every
  widget in it.

`nav.pane`, `show_pane()`, `pane()` and `layout()` keep their meaning. `Pane::Folders` is now
"the sidebar over the list". `Layout` is `wide` (≥ 1280), `medium` (900–1279; it was "rail")
and `narrow`.

## Sidebar

- The outer split is an `AdwOverlaySplitView` (pinned; the window sets `show-sidebar` itself).
  The sidebar is **hidden by default**. In a wide window (≥ 1280 px) **Toggle Sidebar** puts it
  beside the list, and that choice is remembered. Below 1280 px it lies over the list: picking a
  folder, Escape or a click beside it closes it, the focus goes back to the list, and nothing is
  remembered there.
- Keys: Ctrl+Shift+S. F9 was dropped because Omarchy's voxtype dictation grabs it (adding F9 to
  `GRABBED` is the keys area's). The list bar's sidebar button is a toggle bound to
  `win.toggle-sidebar`.
- **Attention dot:** while the sidebar is hidden and an account needs the user (offline, sign-in,
  certificate, mailbox full), the sidebar button carries a status dot in that state's colour.
  The words are in the button's tooltip, on a line over "Toggle Sidebar (Ctrl+Shift+S)", and in
  its accessible description: "Work: offline, and 1 other account needs attention". So the dot
  never relies on colour alone (`Tooltips::set_note`, `SidebarModel::attention`).
- Outside Hyprland the sidebar carries the start window controls while it shows, over the list as
  well as beside it, since it covers the list bar's.
- **The 56 px rail is gone** (code, CSS and tests).
- **Quiet sidebar:**
  - The footer's summary ("2 accounts · offline") shows only while an account needs attention.
    The `config.toml` button stays, because it is the folder list's Tab neighbour (a hidden one
    trapped Tab).
  - An account heading's status word shows only for problem states. The dot and the accessible
    label ("Personal, synced") always say the state.
  - `Sidebar::set_model` returns at once when the model is unchanged. When only counts changed
    (same shape), it updates the count labels and accessible labels in place instead of
    rebuilding every row and its controllers. The store sends a model on every count change.

## The list's header

- **One quiet header row** (48 px, the pane bar): the sidebar button, the view's name (bold, no
  longer heavy) with its quiet count, the filter pill, then Search, Compose and the main menu,
  all icon buttons with key tooltips.
- Chrome above the first row went from 128 px (bar 48 + search 32 + chips 26 + gaps) to 48 px.
- **Search on demand:** the field shows on `/`, Ctrl+F or the Search button, and while a query
  runs. Escape (stop-search) ends the search and hides it. It also hides when it loses the focus
  while empty. Suggestions, the scope row and the "Search · N results" heading are unchanged.
- **The filter chips row is gone.** `MessageList::set_filter` still works: Go to Starred and the
  self-test's `chip` use it. A non-All filter shows as a removable pill (Unread ×) after the
  count, and a click on the pill shows everything again (`connect_filter_changed` /
  `Window::connect_filter_cleared`). Unread and Attachments are reachable from search operators
  (`is:unread`, `has:attachment`). Filter commands for keys and the palette are the keys area's.
- In one-pane modes the list takes the window's surface (the reader's), so opening a
  conversation changes nothing around it. Its rows then take the row roles made for that surface
  (`--sw-window-*`, docs/superhuman/rows.md), not the sidebar's fills (the integration's review,
  2026-10-29).

## Empty states

- **Inbox Zero:** an Inbox or All Inboxes with filter All and no search, when empty, shows a calm
  full-area state: a soft flat tint of the accent, a quiet sun mark, a large light "Inbox Zero",
  the day by the list's clock, and "Press [c] to write", with the Compose key in use drawn as a
  keycap.
  - The tint is flat. The first version's faint radial gradient showed concentric bands on 8-bit
    output, which GTK doesn't dither.
  - The day turns at midnight with the list's day check.
- **The list's empty states hold the keyboard focus** (Inbox Zero and "No messages" alike: role
  `status`, their whole text as their label). When a reset takes away the row that had the
  focus, the focus stays with the list, so its keys keep working with no row (`c` writes, `z`
  undoes, `g i` goes on). Before, GTK moved the focus to the window's first control.
- **One empty state only:** a Split window whose list is empty while the reader has nothing
  shows the list alone, so its empty state is never beside "No message selected". This holds
  whatever was open before: the last conversation archived from the reader after Enter resets
  the page to the list.
- In Split, the reader's toolbar hides while nothing is selected. Its bar stays: the header line
  continues across the reader, and outside Hyprland the bar holds the end window controls and
  drags the window.

## layout.toml

New keys, written only once the user chooses, and read leniently like the rest:

```toml
mode = "split"      # or "focused"; absent: Focused
sidebar = true      # the sidebar beside a wide window's list; absent: hidden
```

`layout.toml` writes are now ordered. Two quick changes could land on disk in the wrong order,
for example both widths reset by double clicks: an intermittent `window-layout` failure.

## Self-test harness

- The harness starts every window in Split with the sidebar shown
  (`AppState::set_layout_defaults`), which the existing scripts were written for. A script that
  starts with `layout-defaults app` gets the app's own defaults.
- New commands (`app/src/selftest/layout.rs`): `view-mode split|focused`, `sidebar shown|hidden`
  (beside `sidebar sample|none`), `assert-view-mode`, `assert-sidebar inline|overlay|hidden`,
  `assert-reader-on-screen`, `assert-reader-warm`, `assert-search-shown`, `assert-action-state`,
  `assert-focus-row` and `measure-open COUNT [MS]`.
- `assert-shown` treats a transparent ancestor, and the warm reader behind the list
  (`Window::is_behind_list`), as off screen. `focus search` shows the field first.
  `list sample|empty` tells the window whether that list is empty.
- New scripts:
  - `focused.sealwax-test`: open, back, `j`/`k`, mark-read only on screen, auto-advance, Inbox
    Zero, `c` from it, Undo, and open/back frame timing.
  - `layout-toggles.sealwax-test`: the sidebar inline and over the list, its keys, button,
    Escape and focus; the Reading Pane; both remembered across a new window; the attention dot
    and its tooltip; the filter pill; search on demand.
  - `layout-resize.sealwax-test`: the focus across 1280 and 900 px both ways, two pixels apart,
    and the half tile (1440 to 710 px).
    - Focused and Split, in the list (`j`/`k` after each crossing) and in an open conversation
      (`j`, one Escape).
    - The sidebar with the focus, beside the list and over it.
    - The reader left in front of a narrowing Split window.
    - Split Inbox Zero after archiving from the reader, and its day turning at midnight.
- Updated on purpose:
  - `window`, `window-layout`, `window-controls` and `theme`: no rail; the medium width; the
    sidebar over the list; the one-pane list surface. `window-controls` also covers Split with
    nothing selected, the sidebar over the list, and Focused.
  - `a11y-window`: the Tab chains through the new header row.
  - `a11y-dialogs`: Toggle Sidebar is built.
  - `status-banners`: the attention dot and its tooltip, per state, in place of the rail's dots
    and tooltips.
  - `search-suggestions`: the field shows on demand.
  - `triage-keys`: Enter now moves the focus into the conversation.
  - `list-sections`: an empty Inbox is Inbox Zero.
  - `reader-bodies`: a body fetched again and shown in place is now waited for in the page
    (`wait-until assert-body-text`). WebKit commits that page a little after the reader shows
    it. The check read it once, a frame later. With the layout's quicker frame that read came
    first in about one run in three. At low load the pristine build passed 16 of 16 and this
    build 8 of 16 in a run at the same time; with the wait it passed 16 of 16.
  - The preview scripts: `assert-layout medium` in place of `rail`.

## Measured

- Chrome above the first row: 128 px before (bar, search field, chips), 48 px now.
- Open and Back in Focused (`measure-open 4` in `focused.sealwax-test`), each open on a fresh
  conversation already laid out behind the list. Times are from the pane's change to the first
  frame painted with it, on headless Broadway, which draws in software:
  - The change lands in the next frame.
  - About 7–12 ms when the page was drawn before.
  - Up to about 60 ms on a page drawn for the first time. That is Broadway redrawing the whole
    window, not layout: triage-frames notes 40–60 ms for any reader redraw there.
  - The live GPU path wasn't measured here.
- Moving the cursor in a Focused list does the reader's layout work behind the list, as Split
  always did, and draws it into an empty clip, so nothing of it shows.

## Left for later

- Select-mode batch actions in Focused mode: keys and the row menu still act on the selection,
  but the reader's batch page isn't seen while the list is in front.
- A single click on a row in Focused mode moves the cursor; a double click (or Enter, `o`) opens.
  The brief says "click opens", the assignment said double click: this needs a product call.
- Inbox Zero shows for any empty Inbox, including an account's first sync before mail arrives.
- Split-inbox tabs: the header row keeps room for them after the title.
- DESIGN.md's Layout section and USER_GUIDE.md's "Toggle Sidebar not built" note are upstream
  docs and were left alone. Only USER_GUIDE.md's generated keymap table changed.
