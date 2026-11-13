# Superhuman pass: list rows

*2026-10-28.* The message list's row, rebuilt as Superhuman's single line, with the look only:
another engineer switches the list's data to conversation rows, through the hook below. Code:
`app/src/list/row.rs` (the row, its layout, the hook), `app/src/style.css` (the
`superhuman: rows` blocks), `domain/src/theme/roles.rs` (`RowRoles`), `domain/src/theme/css.rs`
(variables and sizes), `app/src/list/model.rs` (`FIRST_ROWS`, `GROW_STEP`).

## The row

```
• Ingrid, Tomas, me  3   Cabin trip in November?  Hi all, the cabin is booked for…  📎 ★   OCT 26
```

- **One line, `--sw-size-list-row` (38 px at base 12, 35 at the owner's 11).** The unread dot's
  gutter (`--sw-size-list-gutter`, 28), the sender column at a fixed width
  (`--sw-size-list-sender`, 180), the subject, then the snippet in a quieter colour, the paperclip
  and the star (by opacity, so they keep a column), and the date right-aligned (phase B: in a
  column as wide as the widest date label, so dates and the icons before them line up; the
  `--sw-size-list-date` token it replaced is gone).
- **No avatars.** `RowContext::avatars` (default off) brings back a small (24 px) one after the
  dot, for rows created after it is set; its tint class then changes on bind, as before.
- **Two lines in a narrow list.** A row narrower than its gutter plus 2.5 sender columns (a list
  under about 530 px at base 12, such as the split layout's 380 px pane) shows the sender column
  and the date, then the subject and the snippet below: 57 px (one and a half rows), where a
  single line would leave the subject about 60 px; 1.5 times as many rows as the old 85 px ones.
  The shape follows the width alone, so every row of a list has the same one, and a row's height
  never depends on its text: the one-line height is the gutter's CSS height, the second line is
  centred on that height's bottom edge, so CJK and emoji fallback fonts don't change it
  (`dump-layout`: every row 38 px at 640, 57 px at 380, with Chinese and emoji senders and
  subjects).
- **Subject and snippet.** When both fit, the subject takes what it needs and the snippet the
  rest; otherwise the subject takes at most `SUBJECT_SHARE` (70 %) of the room, or more when the
  snippet is short, so a long subject never hides the snippet. Reply and forward prefixes are
  left out (`message::base_subject`): "Re: SV: Plans" shows as "Plans".
- **Dates** (`time_text`): today the time ("10:32"); yesterday `YESTERDAY` (the list may lose its
  section headers in Focused mode, and a bare time would then be ambiguous); this year the
  month and day (`OCT 4`); before that with the year (`OCT 4 2025`). Small, medium weight,
  letter-spaced capitals, tabular figures; the month is the locale's abbreviation.
- **Quick actions** (Done = check mark, Star, Delete; `Quick::ALL`) show on the hovered row, and
  on the selected row while the pointer is on no other row (Superhuman's cursor row), over the
  date and the icons. They are drawn last, on the row's own fill with a short fade into the
  snippet, and the date and icons fade out by opacity, so showing them moves nothing. The star
  is an outline on an unstarred row and full on a starred one (it unstars).
- **Section headers** stay (`SectionModel`, which the date form depends on), as a quiet 26 px
  caption over the sender column; libadwaita's 18/6 px header padding is gone (51 px before).

## Read, unread, selected

- Unread: the dot, the sender's names and the subject bold (Pango attributes, as before) and in
  the foreground. Read: regular weight, sender and subject `--sw-read-fg-color`, the snippet and
  the count `--sw-tertiary-fg-color`. The date is secondary text either way.
- The row's one state class is `sw-unread` on the row, set only when it changes; it replaces the
  avatar tint as the one class that changes on bind, and only colours text.
- The selected row: the list tinted toward the accent (`--sw-row-selected-bg-color`) and a 3 px
  accent bar at its start. No focus ring on it outside the select mode; a focused row that isn't
  selected (the select mode's cursor) keeps libadwaita's ring. Rows run edge to edge, square.
- Hover: omarchy-shell's hover colour over the list, half as far from it as the selected row
  (`--sw-row-hover-bg-color`), instant (the quick actions' backdrop is the same colour). The
  integration's review (2026-10-29) replaced a fixed 5 %: on the window's surface it was as loud
  as the selection on Adwaita dark and invisible on Tokyo Night.
- **Two surfaces.** The theme makes the row roles twice: `rows` for the list on the sidebar's
  colour (Split, beside the reader) and `window_rows` (`--sw-window-*`) for the list alone on
  the window's colour (Focused, a narrow window). While the window has `sw-one-pane`, style.css
  puts the `--sw-window-*` values in place of the row variables on the list.
- The select mode's checkbox takes the dot's place in the gutter; the text doesn't move.

### Theme roles (`RowRoles`, `domain/src/theme/roles.rs`)

| Role | Variable | Rule |
| --- | --- | --- |
| Hovered row | `--sw-row-hover-bg-color` | hover colour over the list, `ROW_HOVER_SHARE` (half) of the selected row's OKLab distance from the list; at least `ROW_HOVER_MIN_STEP` (0.025, a little over OKLab's just-noticeable 0.02), at most `ROW_HOVER_MAX_SHARE` (3/4) of the selection's |
| Selected row | `--sw-row-selected-bg-color` | the list mixed `ROW_SELECTED_TINT` (16 % dark, 12 % light) toward the accent |
| Read sender/subject | `--sw-read-fg-color` | foreground mixed `READ_MIX` (15 %) toward the background, less until 4.5:1 on the window, the list and both row fills |
| Read snippet, count | `--sw-tertiary-fg-color` (+ `--sw-hover-…`, `--sw-selected-…`) | `TERTIARY_MIX` (40 %), less until 4.5:1 on the window and the list (and on each fill for its variant) |
| Draft marker | `--sw-draft-fg-color` | the error colour mixed toward the foreground until 4.5:1 on all four surfaces |

`domain/tests/theme.rs` holds them over the 22 stock themes and the built-in palettes, on both
surfaces: `a_hovered_row_is_half_as_loud_as_the_selected_one`,
`row_text_reaches_aa_on_every_row_surface`, `row_text_mixes_as_far_as_it_reads` (also: on each
surface, unread text ≥ read text ≥ unread snippet ≥ read snippet in contrast), and
`the_accent_marks_read_on_the_selected_row` (the bar and the dot keep 3:1 on the tint wherever
the theme's accent has 3:1 on the list, every theme: phase C gave rose-pine's one-pane list,
3.14:1 on its window and 2.78:1 on the mix, a tint of hue alone, `#e0fafb`, 3.14:1, see below;
its listed exception is gone). Tertiary
text has a colour per surface, as secondary text does: one colour for every surface made a read
row's snippet brighter than an unread one's on catppuccin-latte and rose-pine. On Adwaita dark:
unread `#ffffff`, read `#ddddde`, unread snippet `#c7c7c7`, read snippet `#a5a5a6` on `#28282c`;
hovered `#303033`, selected `#2a3749`. On its window (`#1d1d20`, the Focused list): hovered
`#262629`, selected `#212d3f`. Tokyo Night's window (`#1a1b26`): hovered `#252633`, selected
`#293147`. Adwaita light's (`#ffffff`): hovered `#f5f5f5`, selected `#e4eefa`. The hover is 0.50
to 0.56 of the selection's step in every stock theme but rose-pine (0.62 and 0.72, held up by
the floor).

## The conversation hook

The row draws only from `RowFields` (`row.rs`), through `MessageRow::show_fields`:

```rust
pub struct RowFields<'a> {
    pub participants: &'a [Participant<'a>], // oldest first, each once
    pub address: &'a str,                    // first participant's, for the optional avatar
    pub count: u32,                          // messages; shown quietly when > 1
    pub draft: bool,                         // red "Draft" after the names
    pub subject: &'a str,                    // as stored; prefixes are left out when shown
    pub snippet: &'a str,
    pub date: i64,                           // the row's section picks the date's form
    pub unread: bool,
    pub starred: bool,
    pub attachment: bool,
}
pub struct Participant<'a> { pub name: &'a str, pub me: bool, pub unread: bool }
```

Today `MessageRow::show` fills it with `RowFields::message(summary, &[Participant::sender(summary)])`
(the count is the conversation's size, `thread_count`; the Draft marker the `\Draft` flag). For
conversation rows, `show` only has to build the fields from the conversation:

```rust
let people: Vec<Participant> = conversation.participants.iter().map(|p| Participant {
    name: &p.name,          // display name, else the address
    me: p.is_account,       // the account's own address or identity: shown as "me"
    unread: p.has_unread,   // sent a message that is unread in the view
}).collect();
let fields = RowFields { participants: &people, address: &conversation.first_address,
    count: conversation.messages, draft: conversation.has_draft, subject: &conversation.subject,
    snippet: &conversation.snippet, date: conversation.date, unread: conversation.unread > 0,
    starred: conversation.flagged > 0, attachment: conversation.attachments > 0 };
self.show_fields(&fields, item.section());
```

The sender column then reads "Ingrid Solberg" for one person, "Ingrid, Tomas, me" for several
(first names; "Berg, Tomas" gives "Tomas"), and "Ingrid … Sana, Tomas" past three (first, "…",
the two who wrote last, as Gmail does; phase C: the two who joined last could leave out the
newest writer). In an unread row the unread people are bold (a Pango attribute
list per such row; one shared list when all are); with no `unread` marks all names are bold.
The accessible label says "Unread, Draft, Ingrid, Tomas and me, Cabin trip, OCT 26, 3 messages,
starred, has attachments". Still message-shaped, for the list engineer: `quick_clicked` builds
its `Target` from the item's `MessageSummary`, and `texts()` (the self-test's `assert-list-row`)
returns the shown sender column, subject, snippet and date.

## Speed (S3's rules kept)

Text is cut before it is set (sender column 48 characters, subject and snippet 72 at a 400 px
list, scaled with the width), each text is one label, weight comes from Pango attributes, the
dot, star and paperclip use opacity, nothing is set unless it changed, one class changes on
bind, and the placeholder look is unchanged. `RowLayout` is a small `GtkLayoutManager` that
measures each part once per layout (GTK caches the measures) and places them; hovering or
selecting never relayouts the row (the quick actions lie over the date).

`FIRST_ROWS` and `GROW_STEP` went from 40 to 64: single-line rows are 38 px, so 1600 px of list
shows about 42 and 2160 px about 57, and the first frame still fills any screen without
placeholders; no more, because every one of them is laid out in the first frame of every folder,
chip and search (the owner's 1440×900 list shows about 21). Rows bound after each frame went from
`[40, 80, 120, 160, 200, 205]` to `[64, 128, 192, 205]` (`list-first-frame` asserts 64). Deriving
the count from the viewport height (the brief's alternative) needs `list/mod.rs` to pass its
height to the model; left to the list's owner.

Measured with `tests/selftest/perf-list.sealwax-test` (release, headless Broadway at 2560×1600,
100,000 messages; the list pane is 379 px, so two-line rows), and a copy with the list dragged to
640 px (single-line rows), the pristine and the new binary alternating on one copy of the store.
Five other agents were building at the time (load average 15–30 on 16 threads), so absolute
numbers are well above a quiet machine's (the pristine build measured alone first: jump to start
92 ms, to end 113 ms, folder open 63 ms, 12,000 px/s p99 6 ms); compare the pairs. Medians, new
vs pristine, with `FIRST_ROWS` at 80 (now 64, which binds fewer rows on a folder open):

| | split pane, two lines (4 pairs) | 640 px, one line (2 pairs) |
| --- | --- | --- |
| layout per bound row | 0.084 vs 0.121 ms | 0.089 vs 0.092 ms |
| jump to start → painted | 98 vs 241 ms | 121 vs 224 ms |
| jump to end → painted | 113 vs 282 ms | 150 vs 298 ms |
| scrolling 1,500 px/s, frame interval p99 | 2.9 vs 2.3 ms | 0.9 vs 16.1 ms |
| scrolling 12,000 px/s, frame interval p99 | 73 vs 145 ms | 65 vs 49 ms |
| folder open → first frame with rows | 109 vs 156 ms | 212 vs 117 ms (one run hit a load spike: 358 ms; the other 65 vs 77) |
| new mail → row painted | 7.9 vs 9.7 ms | 12.0 vs 9.8 ms |

Row binding costs less per row (no avatar, no two-line wrapped snippet, single-line labels cut
to what can show). The bars in TESTING.md (p99 ≤ 16.7 ms while scrolling, jump to end ≤ 100 ms)
should be re-checked on a quiet machine; under this load neither binary meets the fling bar.

## Tests changed on purpose

- `list-sections`: yesterday's row reads `YESTERDAY` (was 12:00).
- `list-first-frame`: `assert-list-growth 64` (was 40).
- `list-insert`: scrolls 1200 px instead of 3000 to stay ~25 rows down (3000 px is ~52 rows of
  57 px, where GTK no longer keeps row 0 bound); `list-churn`: asserts the list is still deeper
  than 5000 px after the reload (row 230 is 8,700–13,000 px down now, not ~19,500).
- New unit tests: `row.rs` (sender column, first names, dates, fields, accessible text),
  `roles.rs` (`row_roles_follow_the_design_table`, `muted_text_mixes_only_as_far_as_it_reads`),
  `css.rs` (new variables), and the three contrast tests above; the 10 theme snapshots gained
  the new variables and roles.
- `app/src/selftest/sample.rs`: the theme self-tests' sample list draws the real row
  (`MessageRow::show_unbound`), so their screenshots show what the list shows.

## Phase C (track L, step 1)

- **Whole names for the column's width** (the visual QA's blocker: "Marius … To…", "Ingrid …
  me, …" at the 710 px half tile, the unread latest writer cut to an initial behind the Draft
  marker at 1440 px). `participants::columns` gives the column's texts, longest first, each of
  whole names: every name while three or fewer ("Ingrid, Tomas, Sana"), else the first and the
  two who wrote last ("Ingrid … Sana, Tomas"); then the first and the one the row is about of the
  others ("Ingrid … Tomas": in an unread row the latest to write unread mail, else the latest who
  isn't the user); then the one the row is about of everyone and how many others wrote ("Tomas
  +3", the name bold, never the count). One person keeps their whole name (a long one is
  ellipsized, as before). The row shows the longest that fits (`MessageRow::show_names`): picked
  at bind for the column the list's rows were last laid out in (`RowContext::column`), measured
  with a copy of the names label's own layout (its CSS's font features kept, its weights replaced:
  `text_width`), and picked again in `RowLayout`'s allocation when the room differs (a resize,
  the first frame), before the column is laid out. The room is the column less what the column's
  other parts and margins take (`who`'s natural width less the names'). So the count and the
  Draft marker always sit right after the names.
- **The Draft marker takes the count's place**: "Ingrid, Tomas, Sana Draft" fits the 1440 px
  column whole; both beside the names left them too little. The accessible label still says
  "Draft, …, 3 messages".
- **The store names the latest writers**: `ConversationSummary.participants` keeps the first and
  the three who *wrote* last (each `Participant` has `last`, the date of their newest copy), so
  the row can name the newest writer (trip's Tomas; store.md).
- **Full-strength Star and Read glyphs** (`app/src/window/glyph.rs`): Adwaita's
  `non-starred-symbolic` and `mail-read-symbolic` paint at 35 % and 50 % opacity, which CSS can't
  lift, so the quick Star and the select mode's bar's Star and Read looked disabled (peaks 88 and
  115 against 199 on Adwaita dark). They are drawn here, an outlined star and an open envelope on a
  16 px grid with a 1.5 px round stroke, as `GtkSymbolicPaintable`s that GTK colours with the
  image's CSS colour; `glyph::set_icon(image, names)` picks one for those two names, else the
  theme icon. Measured on the bar: all five glyphs peak at 199 (dark) and bottom at 85 (light),
  where Read and Star were 115/88 and 169/195. The reader's toolbar Star (track L's step 2) can use
  `set_icon` too.
- **rose-pine's selected row**: `selected_fill` keeps the mix where the accent's bar keeps 3:1 on
  it (every other theme and surface); where the accent has 3:1 on the list but not on the mix, the
  fill moves the list toward the accent's hue (OKLab), at the list's own luminance, so the bar
  keeps the list's contrast. Only rose-pine's one-pane list takes it: `#e6e8e4` (2.78:1) became
  `#e0fafb` (3.14:1). It moves as far as the mix does while no more colourful than the list or
  the mix, but at least as far as a hovered row needs to stay the quieter
  (`ROW_HOVER_MIN_STEP / ROW_HOVER_MAX_SHARE`, 0.033; the review's second round: the mix's whole
  step, 0.043, all in hue, made the warm list an icy cyan, `#dafbff`, the loudest tint of any
  theme). Read text there would then have been lighter than an unread row's snippet, so read
  text never mixes further than the secondary text (`mix_of`; no other theme changes).
- **No count after "Marius +7"** (the review's second round): the shortest names already count
  the others; the message count after them made way (conversations.md).
- **What picking names costs** (the review's third round asked for it against S3's bars): an
  A/B of release builds, the integrated phase A+B base (names cut by characters) against this
  track, on the 100,000-message fixture (41,336 conversation rows), `perf-list-conversations`
  (Split, 1440 px, two-line rows) and the same scrolling and jumps in the app's own layout at the
  710 px half tile (Focused, one-line rows, the 122 px column: up to three layouts a row), base
  and new alternating, two runs each, headless Broadway at load 9–12. Work per frame (ms,
  base runs; new runs):

  | | 1,500 px/s p50 / p99 | 12,000 px/s p50 | sweep p50 / p95 | per bound row, frames binding ≥ 50 |
  |---|---|---|---|---|
  | 710, Focused | 0.26 / 1.18, 0.22 / 0.54; 0.25 / 0.66, 0.26 / 0.99 | 33.8, 24.3; 27.6, 32.8 | 4.0 / 5.9, 3.5 / 5.0; 3.4 / 4.4, 4.0 / 6.0 | 0.224, 0.187; 0.186, 0.214 |
  | 1440, Split | 0.33 / 1.17, 0.36 / 2.94; 0.33 / 1.17, 0.29 / 0.97 | 6.8, 9.2; 7.0, 7.2 | 3.5 / 4.8, 4.3 / 6.2; 3.4 / 4.5, 4.0 / 5.2 | 0.584, 0.302; 0.595, 0.324 |

  Jumps (p50, to start / end) 115–125 / 102–113 ms in the base and 68–129 / 58–116 ms in the new
  build. Every figure sits inside the base's own run-to-run spread: no cost shows. The bars
  themselves are the live session's (Broadway paints in software at 30 Hz); re-check them there
  when the machine is quiet. Scripts and logs: `sealwax-wt/_scratch/l-fixes/perf/`.
