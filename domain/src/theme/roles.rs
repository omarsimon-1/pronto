use super::color::{Rgb, distinct_pairs, readable_on};
use super::palette::{Mode, Palette};
use super::shell::ControlStates;
use crate::account::AccountColor;

/// The hues the status colours turn to when a theme can't tell its `red`, `yellow` and
/// `green` apart ([`distinct_pairs`]): error, warning, success. Matte-black's are two reds
/// and an amber.
pub const STATUS_HUES: [f64; 3] = [0.0, 40.0, 130.0];
/// How far apart, in degrees of hue, the theme's own status colours must be to be kept.
pub const STATUS_MIN_GAP: f64 = 20.0;

/// How far borders and separators mix `lighter_background` toward `foreground`, in percent:
/// dark themes, then light ones (the approved mock; on matte-black `lighter_background`
/// alone is nearly invisible against the window).
pub const BORDER_MIX: [u8; 2] = [8, 14];

/// The contrast a white Send label keeps on its fill (WCAG AA for text; the fork's phase B).
pub const PRIMARY_CONTRAST: f64 = 4.5;
/// How far Send's fill may deepen the accent toward black for a white label, in percent: far
/// enough for Adwaita's blue (8 %), not so far that a pastel accent turns muddy.
pub const PRIMARY_DEEPEN_MAX: u8 = 12;

/// libadwaita's roles, mapped as in DESIGN.md's token table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiRoles {
    /// Sidebar, list and header bar: `dark_background`.
    pub sidebar: Rgb,
    /// Popovers and dialogs: `darker_background`.
    pub popover: Rgb,
    /// The command palette's and the pickers' card over the dimmed window: [`raised_surface`].
    pub raised: Rgb,
    /// Cards, hovered rows and attachment cards: `lighter_background`.
    pub card: Rgb,
    /// Borders and separators: `lighter_background` mixed toward `foreground` by
    /// [`BORDER_MIX`].
    pub border: Rgb,
    /// Text on the accent: `readable_on(accent)`.
    pub accent_text: Rgb,
    pub error: Rgb,
    pub warning: Rgb,
    pub success: Rgb,
    pub error_text: Rgb,
    pub warning_text: Rgb,
    pub success_text: Rgb,
    /// Banner backgrounds: the window background tinted toward the status colour.
    pub error_soft: Rgb,
    pub warning_soft: Rgb,
    /// The composer card's surface (the fork's phase B): raised above the window by a lighter
    /// surface in a dark theme (`lighter_background`, as Superhuman's dark mode does), the
    /// window's own in a light one, where the card's shadow lifts it. Never above
    /// [`UiRoles::raised`]: the palette and the pickers open over the card.
    pub composer: Rgb,
    /// Send's fill and label (the fork's phase B): a white label on the accent deepened toward
    /// black until it reaches [`PRIMARY_CONTRAST`], at most [`PRIMARY_DEEPEN_MAX`], so Send
    /// reads alike in a theme's dark and light modes (`accent_text` turned Adwaita dark's
    /// label near-black); a pastel accent keeps its fill and `accent_text`.
    pub primary: Rgb,
    pub primary_text: Rgb,
}

/// The composer card's surface: in a dark theme the window mixed toward `lighter_background`
/// as far as secondary text still reads on it ([`TEXT_CONTRAST`], in 10 % steps; most themes
/// take the whole step), but never above the raised surface ([`raised_surface`]): the snippet
/// and time pickers and the palette open over the card, and one elevation order holds in every
/// theme, window ≤ composer card ≤ raised (the phase B merge's visual review: Tokyo Night's
/// pickers sank into the card under them). In a light one the window.
fn composer_surface(p: &Palette) -> Rgb {
    if p.mode == Mode::Light {
        return p.background;
    }
    let card = card_step(p);
    let raised = raised_surface(p);
    if card.relative_luminance() > raised.relative_luminance() {
        raised
    } else {
        card
    }
}

/// A dark theme's composer card on its own: the window mixed toward `lighter_background` as far
/// as text and secondary text read on it, in 10 % steps.
fn card_step(p: &Palette) -> Rgb {
    let secondary = secondary_text(p);
    (0..=10)
        .rev()
        .map(|step| p.background.mix(p.lighter_background, step * 10))
        .find(|surface| {
            secondary.contrast(*surface) >= TEXT_CONTRAST
                && p.foreground.contrast(*surface) >= TEXT_CONTRAST
        })
        .unwrap_or(p.background)
}

/// How far a link may move from the accent toward the text colour to read ([`link_on`]), in
/// percent: past that it no longer looks like the accent.
pub const LINK_MIX_MAX: u8 = 50;

/// Links on `surface` (the fork's phase B: the composer card's body, on [`UiRoles::composer`]):
/// the accent mixed toward `foreground` in 5 % steps (lighter in a dark theme, darker in a light
/// one, as libadwaita's own accent colour for text is) until it reaches [`TEXT_CONTRAST`]
/// there, at most [`LINK_MIX_MAX`]. Adwaita dark's #3584e4 reads at 3.5:1 on its composer card.
pub fn link_on(palette: &Palette, surface: Rgb) -> Rgb {
    (0..=LINK_MIX_MAX)
        .step_by(5)
        .map(|percent| palette.accent.mix(palette.foreground, percent))
        .find(|link| link.contrast(surface) >= TEXT_CONTRAST)
        .unwrap_or_else(|| palette.accent.mix(palette.foreground, LINK_MIX_MAX))
}

/// `color` deepened toward black in 2 % steps, at most `most` percent, until `text` reaches
/// `floor` on it; `None` when it doesn't by then.
pub fn deepened(color: Rgb, text: Rgb, floor: f64, most: u8) -> Option<Rgb> {
    (0..=most)
        .step_by(2)
        .map(|percent| color.mix(Rgb::BLACK, percent))
        .find(|fill| fill.contrast(text) >= floor)
}

impl UiRoles {
    pub fn new(palette: &Palette) -> Self {
        let p = palette;
        let on = |color: Rgb| readable_on(color, p.background, p.foreground);
        let [error, warning, success] =
            distinct_pairs([p.red, p.yellow, p.green], STATUS_HUES, STATUS_MIN_GAP);
        let (soft, border) = match p.mode {
            Mode::Dark => (16, BORDER_MIX[0]),
            Mode::Light => (10, BORDER_MIX[1]),
        };
        let accent_text = on(p.accent);
        let (primary, primary_text) =
            match deepened(p.accent, Rgb::WHITE, PRIMARY_CONTRAST, PRIMARY_DEEPEN_MAX) {
                Some(fill) => (fill, Rgb::WHITE),
                None => (p.accent, accent_text),
            };
        Self {
            sidebar: p.dark_background,
            popover: p.darker_background,
            raised: raised_surface(p),
            card: p.lighter_background,
            border: p.lighter_background.mix(p.foreground, border),
            accent_text,
            error,
            warning,
            success,
            error_text: on(error),
            warning_text: on(warning),
            success_text: on(success),
            error_soft: p.background.mix(error, soft),
            warning_soft: p.background.mix(warning, soft),
            composer: composer_surface(p),
            primary,
            primary_text,
        }
    }
}

/// The keycaps' fill on a raised surface, `foreground` in percent (omarchy-shell's normal
/// control fill): the darkest place secondary text sits on it.
const RAISED_KEYCAP_FILL: u8 = 4;

/// A surface raised over the window, for the command palette's card and the snippet and time
/// pickers (the Superhuman pass's visual review, 2026-10-29): the popover colour where it is at
/// least as light as the window and the composer card the pickers open over (libadwaita's) and
/// secondary text reaches [`TEXT_CONTRAST`] on it and on a keycap's fill. Omarchy's
/// `darker_background` is darker: over the dimmed window it read as a hole, and a light theme's
/// left secondary text under the floor (Catppuccin Latte). Otherwise a light theme's card is
/// the window's colour; a dark theme's is the window stepped toward `lighter_background` as far
/// as secondary text still reads there, keycaps included. Where that stops short of the
/// composer card's own step, the card comes down to it ([`UiRoles::composer`]).
pub fn raised_surface(palette: &Palette) -> Rgb {
    let p = palette;
    let window = p.background.relative_luminance();
    let text = secondary_text(p);
    let reads = |surface: Rgb| {
        text.contrast(surface) >= TEXT_CONTRAST
            && text.contrast(surface.mix(p.foreground, RAISED_KEYCAP_FILL)) >= TEXT_CONTRAST
    };
    let popover = p.darker_background;
    let under = match p.mode {
        Mode::Dark => window.max(card_step(p).relative_luminance()),
        Mode::Light => window,
    };
    if popover.relative_luminance() >= under && reads(popover) {
        return popover;
    }
    if p.mode == Mode::Light {
        return p.background;
    }
    (0..=100)
        .rev()
        .map(|percent| p.background.mix(p.lighter_background, percent))
        .find(|surface| surface.relative_luminance() >= window && reads(*surface))
        .unwrap_or(p.background)
}

/// Secondary text in the palette's and the pickers' rows (phase C): [`secondary_text_on`] the
/// raised card, a hovered row and the selected row there (`controls`' fills over `raised`),
/// each with a keycap's fill too. The app's secondary text fell to 4.16:1 on a hovered row in
/// Tokyo Night and 4.13:1 in Gruvbox, and the lighter card took Catppuccin's from 5.02 to 4.38.
pub fn raised_secondary_text(palette: &Palette, raised: Rgb, controls: &ControlStates) -> Rgb {
    let hovered = controls.hover.color.over(raised, controls.hover.fill);
    let selected = controls.selected.color.over(raised, controls.selected.fill);
    let keycap = |surface: Rgb| surface.mix(palette.foreground, RAISED_KEYCAP_FILL);
    secondary_text_on(
        palette,
        &[
            raised,
            hovered,
            selected,
            keycap(raised),
            keycap(hovered),
            keycap(selected),
        ],
    )
}

/// How many avatar tints there are.
pub const AVATAR_TINTS: usize = 7;
/// The hue step between neighbouring tints, from the accent's hue (DESIGN.md).
pub const AVATAR_HUE_STEP: f64 = 51.0;
/// The tints' fixed saturation and their lightness per mode, as in the approved mock.
pub const AVATAR_SATURATION: f64 = 0.42;
pub const AVATAR_LIGHTNESS_DARK: f64 = 0.32;
pub const AVATAR_LIGHTNESS_LIGHT: f64 = 0.80;
/// The two colours the initials choose between with `readable_on`, as in the approved mock.
pub const INITIALS_LIGHT: Rgb = Rgb::WHITE;
pub const INITIALS_DARK: Rgb = Rgb::from_hex(0x1b1b1f);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvatarTint {
    pub background: Rgb,
    /// The initials: whichever of [`INITIALS_LIGHT`] and [`INITIALS_DARK`] reads better.
    pub foreground: Rgb,
}

/// The seven tints: hues rotated from `accent`'s in 51° steps at a fixed saturation and a
/// mode-dependent lightness. Palette hues are often too alike (matte-black's are reds and
/// ambers), so the tints never come from the palette's colours.
pub fn avatar_tints(accent: Rgb, mode: Mode) -> [AvatarTint; AVATAR_TINTS] {
    let (hue, _, _) = accent.to_hsl();
    let lightness = match mode {
        Mode::Dark => AVATAR_LIGHTNESS_DARK,
        Mode::Light => AVATAR_LIGHTNESS_LIGHT,
    };
    std::array::from_fn(|index| {
        let background = Rgb::from_hsl(
            hue + AVATAR_HUE_STEP * index as f64,
            AVATAR_SATURATION,
            lightness,
        );
        AvatarTint {
            background,
            foreground: readable_on(background, INITIALS_LIGHT, INITIALS_DARK),
        }
    })
}

/// Which tint an address gets: FNV-1a over the trimmed, lower-cased address, so the same
/// sender keeps its colour across runs, machines and themes.
pub fn avatar_index(address: &str) -> usize {
    let hash = address
        .trim()
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    (hash % AVATAR_TINTS as u64) as usize
}

/// Up to two initials: the first letters of the display name's first and last words, or else
/// of the address's local part split at `.`, `_`, `-` and `+`; `?` when there is neither.
/// Words that don't start with a letter or digit, such as `(Work)`, are skipped.
pub fn initials(display_name: &str, address: &str) -> String {
    let name = display_name.trim().trim_matches(['"', '\'']);
    let from_name = first_letters(name.split_whitespace());
    if !from_name.is_empty() {
        return from_name;
    }
    let local = address.trim().split('@').next().unwrap_or_default();
    let from_address = first_letters(local.split(['.', '_', '-', '+']));
    if from_address.is_empty() {
        "?".to_owned()
    } else {
        from_address
    }
}

fn first_letters<'a>(words: impl Iterator<Item = &'a str>) -> String {
    let letters: Vec<char> = words
        .filter_map(|word| word.chars().next())
        .filter(|letter| letter.is_alphanumeric())
        .collect();
    let (first, last) = match letters.as_slice() {
        [] => return String::new(),
        [only] => (*only, None),
        [first, .., last] => (*first, Some(*last)),
    };
    std::iter::once(first)
        .chain(last)
        .flat_map(char::to_uppercase)
        .collect()
}

/// The account colours' OKLCH lightness and chroma to start from, per mode: muted pastels on
/// dark themes, deeper tones on light ones (the user's UI tweak, 2026-10-08).
pub const ACCOUNT_LIGHTNESS_DARK: f64 = 0.74;
pub const ACCOUNT_LIGHTNESS_LIGHT: f64 = 0.52;
pub const ACCOUNT_CHROMA_DARK: f64 = 0.065;
pub const ACCOUNT_CHROMA_LIGHT: f64 = 0.075;

/// Each [`AccountColor`] for `palette`, in [`AccountColor::ALL`]'s order: its hue at the mode's
/// lightness and chroma, the chroma lowered where sRGB can't show it, the lightness moved away
/// from the background, a hundredth at a time, until the colour reads as text at
/// [`TEXT_CONTRAST`] on the window and the sidebar backgrounds.
pub fn account_colors(palette: &Palette) -> [Rgb; AccountColor::ALL.len()] {
    let (start, chroma, step) = match palette.mode {
        Mode::Dark => (ACCOUNT_LIGHTNESS_DARK, ACCOUNT_CHROMA_DARK, 0.01),
        Mode::Light => (ACCOUNT_LIGHTNESS_LIGHT, ACCOUNT_CHROMA_LIGHT, -0.01),
    };
    let surfaces = [palette.background, palette.dark_background];
    AccountColor::ALL.map(|color| {
        let at = |lightness: f64| {
            let mut chroma = chroma;
            loop {
                if let Some(rgb) = Rgb::from_oklch(lightness, chroma, color.hue()) {
                    return rgb;
                }
                chroma -= 0.005;
            }
        };
        (0..=100)
            .map(|steps| (start + step * f64::from(steps)).clamp(0.0, 1.0))
            .map(at)
            .find(|rgb| {
                surfaces
                    .iter()
                    .all(|surface| rgb.contrast(*surface) >= TEXT_CONTRAST)
            })
            .unwrap_or_else(|| readable_on(palette.dark_background, Rgb::WHITE, Rgb::BLACK))
    })
}

/// Secondary text starts as `foreground` mixed this far toward `background`, in percent
/// (DESIGN.md; 35 until the visual polish of 2026-10-22 brightened it for HiDPI panels).
pub const SECONDARY_MIX: u8 = 25;
/// The contrast secondary text keeps against the window background and the list's
/// `dark_background`, and the selected chip's text against its background (WCAG AA).
pub const TEXT_CONTRAST: f64 = 4.5;

/// Secondary text: `foreground` mixed [`SECONDARY_MIX`] % toward `background`, or less, one
/// percent at a time, until it reaches [`TEXT_CONTRAST`] against both the window background
/// and `dark_background`; plain `foreground` when nothing does.
pub fn secondary_text(palette: &Palette) -> Rgb {
    secondary_text_on(palette, &[palette.background, palette.dark_background])
}

/// [`secondary_text`] mixed only as far as it still reaches [`TEXT_CONTRAST`] on every one of
/// `surfaces`; plain `foreground` when nothing does.
pub fn secondary_text_on(palette: &Palette, surfaces: &[Rgb]) -> Rgb {
    muted_text_on(palette, SECONDARY_MIX, surfaces)
}

/// `foreground` mixed `percent` toward `background`, or less, one percent at a time, until it
/// reaches [`TEXT_CONTRAST`] on every one of `surfaces`; plain `foreground` when nothing does.
pub fn muted_text_on(palette: &Palette, percent: u8, surfaces: &[Rgb]) -> Rgb {
    let p = palette;
    (0..=percent)
        .rev()
        .map(|percent| p.foreground.mix(p.background, percent))
        .find(|color| reads_on_all(*color, surfaces))
        .unwrap_or(p.foreground)
}

/// How far `color` is mixed from `foreground` toward `background`, in percent ([`SECONDARY_MIX`]
/// when it is no such mix). Read text is never mixed further than an unread row's snippet (the
/// secondary text), so it stays the louder of the two (phase C: rose-pine's one-pane list,
/// once its selected row kept the list's luminance, had read text over the snippet's).
fn mix_of(palette: &Palette, color: Rgb) -> u8 {
    (0..=SECONDARY_MIX)
        .rev()
        .find(|percent| palette.foreground.mix(palette.background, *percent) == color)
        .unwrap_or(SECONDARY_MIX)
}

fn reads_on_all(color: Rgb, surfaces: &[Rgb]) -> bool {
    surfaces
        .iter()
        .all(|surface| color.contrast(*surface) >= TEXT_CONTRAST)
}

/// How far a hovered message row steps from the list, as a share of the selected row's step
/// (both as [`Rgb::distance`]): half, so the pointer's row never reads as a second selection
/// (the Superhuman pass's review, 2026-10-29).
pub const ROW_HOVER_SHARE: f64 = 0.5;
/// The smallest step a hovered row takes from the list ([`Rgb::distance`]), so it shows in every
/// theme: a little over OKLab's just-noticeable 0.02.
pub const ROW_HOVER_MIN_STEP: f64 = 0.025;
/// The largest share of the selected row's step a hovered row takes, [`ROW_HOVER_MIN_STEP`]
/// included: the selection stays the stronger signal.
pub const ROW_HOVER_MAX_SHARE: f64 = 0.75;
/// How far the selected message row tints the list toward the accent, in percent: dark themes,
/// then light ones. The tint and an accent bar at the row's start are the selection's one
/// signal (the Superhuman pass); omarchy-shell's grey selected fill stays on other controls.
pub const ROW_SELECTED_TINT: [u8; 2] = [16, 12];
/// WCAG's floor for graphics: the selected row's accent bar keeps it on the row's tint wherever
/// the accent has it on the list (phase C).
pub const NON_TEXT_CONTRAST: f64 = 3.0;
/// A read row's sender and subject start as `foreground` mixed this far toward `background`,
/// in percent: a step quieter than an unread row's, which keep `foreground` and bold.
pub const READ_MIX: u8 = 15;
/// A read row's snippet and a row's message count start this far toward `background`, in
/// percent: quieter than [`SECONDARY_MIX`], the unread row's snippet.
pub const TERTIARY_MIX: u8 = 40;

/// The message list's rows on one surface (DESIGN.md contrast floors): the hovered and selected
/// fills over the list background, and text that still reads at [`TEXT_CONTRAST`] on them. The
/// theme makes them twice: for the list on the sidebar's colour, beside the reader, and alone
/// on the window's (one pane at a time), since fills mixed from one surface look wrong on the
/// other. Read text and the Draft marker read on every one of the window, the list, the
/// hovered and the selected row.
/// Secondary and tertiary text are as quiet as each surface allows, as the rest of the app
/// uses secondary text: a colour for the window and the list, one for a hovered row and one
/// for the selected row. One colour for every surface would make a read row's snippet brighter
/// than an unread row's on the themes whose fills pin it (catppuccin-latte, rose-pine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowRoles {
    /// omarchy-shell's hover colour over the list, half as far from it as the selected row
    /// ([`ROW_HOVER_SHARE`]).
    pub hover_background: Rgb,
    pub hover_secondary_text: Rgb,
    /// The list tinted [`ROW_SELECTED_TINT`] toward the accent.
    pub selected_background: Rgb,
    pub selected_secondary_text: Rgb,
    /// A read row's sender and subject: [`READ_MIX`], or less until it reads.
    pub read_text: Rgb,
    /// A read row's snippet and the quiet message count: [`TERTIARY_MIX`], or less until it
    /// reads on the window and the list.
    pub tertiary_text: Rgb,
    pub hover_tertiary_text: Rgb,
    pub selected_tertiary_text: Rgb,
    /// The "Draft" marker: the error colour, mixed toward `foreground` until it reads.
    pub draft: Rgb,
    /// The clock mark of a conversation back from Reminders (the fork's phase C): Superhuman's
    /// purple, the palette's magenta, mixed toward `foreground` until it reaches
    /// [`MARK_CONTRAST`] on every row surface, else the accent the same way ([`reminder_mark`]).
    pub reminder: Rgb,
}

impl RowRoles {
    /// `list` is the list's background and `error` the theme's error colour ([`UiRoles`]).
    pub fn new(palette: &Palette, list: Rgb, error: Rgb, controls: &ControlStates) -> Self {
        let p = palette;
        let reads_on = |surface: Rgb| secondary_text_on(p, &[p.background, list, surface]);
        let quietest_on =
            |surface: Rgb| muted_text_on(p, TERTIARY_MIX, &[p.background, list, surface]);
        let tint = match p.mode {
            Mode::Dark => ROW_SELECTED_TINT[0],
            Mode::Light => ROW_SELECTED_TINT[1],
        };
        let selected_background = selected_fill(list, p.accent, tint);
        let hover_background = hover_fill(
            controls.hover.color,
            p.foreground,
            list,
            selected_background,
        );
        let surfaces = [p.background, list, hover_background, selected_background];
        let draft = (0..=100)
            .map(|percent| error.mix(p.foreground, percent))
            .find(|color| reads_on_all(*color, &surfaces))
            .unwrap_or(p.foreground);
        let reminder = reminder_mark(p, &surfaces);
        Self {
            hover_background,
            hover_secondary_text: reads_on(hover_background),
            selected_background,
            selected_secondary_text: reads_on(selected_background),
            read_text: muted_text_on(p, READ_MIX.min(mix_of(p, secondary_text(p))), &surfaces),
            tertiary_text: muted_text_on(p, TERTIARY_MIX, &surfaces[..2]),
            hover_tertiary_text: quietest_on(hover_background),
            selected_tertiary_text: quietest_on(selected_background),
            draft,
            reminder,
        }
    }

    /// The surfaces a row's text must read on: the window, the list and both row fills.
    pub fn surfaces(&self, palette: &Palette, list: Rgb) -> [Rgb; 4] {
        [
            palette.background,
            list,
            self.hover_background,
            self.selected_background,
        ]
    }
}

/// The contrast a row's marks keep on its surfaces: WCAG's 3:1 for graphics that carry meaning
/// (the reminder's clock; text keeps [`TEXT_CONTRAST`]).
pub const MARK_CONTRAST: f64 = 3.0;
/// How far a mark's colour may move toward `foreground` to reach [`MARK_CONTRAST`], in percent:
/// past that it no longer reads as its hue.
pub const MARK_MIX_MAX: u8 = 60;

/// The clock mark of a conversation back from Reminders (phase C): the palette's magenta
/// (Superhuman marks a returned reminder purple), mixed toward `foreground` in 1 % steps until
/// it reaches [`MARK_CONTRAST`] on every one of `surfaces`, at most [`MARK_MIX_MAX`]; where the
/// magenta can't, the accent the same way; else `foreground`.
pub fn reminder_mark(palette: &Palette, surfaces: &[Rgb]) -> Rgb {
    let reads = |color: Rgb| {
        surfaces
            .iter()
            .all(|surface| color.contrast(*surface) >= MARK_CONTRAST)
    };
    [palette.magenta, palette.accent]
        .into_iter()
        .find_map(|start| {
            (0..=MARK_MIX_MAX)
                .map(|percent| start.mix(palette.foreground, percent))
                .find(|color| reads(*color))
        })
        .unwrap_or(palette.foreground)
}

/// The selected row's fill: `list` mixed `tint` % toward `accent`, or, where that mix takes the
/// accent's bar under [`NON_TEXT_CONTRAST`] though the accent has it on the list, a tint of
/// hue alone: the list moved toward the accent's hue (OKLab), at the list's own luminance, so
/// the bar keeps its contrast with the list (phase C: rose-pine's accent, 3.14:1 on its window,
/// fell to 2.78:1 on the mix and was the theme tests' one exception). It moves as far as the mix
/// does while it is no more colourful than the list or the mix, whichever is more, but at least
/// as far as a hovered row needs to stay the quieter of the two ([`hover_fill`]; phase C's
/// review: the mix's whole step in hue made rose-pine's warm list an icy cyan, `#dafbff`, the
/// loudest tint of any theme; now `#e0fafb`); less of the hue where the bar wants it.
fn selected_fill(list: Rgb, accent: Rgb, tint: u8) -> Rgb {
    let mixed = list.mix(accent, tint);
    if accent.contrast(mixed) >= NON_TEXT_CONTRAST || accent.contrast(list) < NON_TEXT_CONTRAST {
        return mixed;
    }
    let [lightness, list_a, list_b] = list.to_oklab();
    let [_, accent_a, accent_b] = accent.to_oklab();
    let [_, mixed_a, mixed_b] = mixed.to_oklab();
    let colourful = list_a.hypot(list_b).max(mixed_a.hypot(mixed_b));
    let (toward_a, toward_b) = (accent_a - list_a, accent_b - list_b);
    let length = toward_a.hypot(toward_b);
    if length < 1e-6 {
        // A grey accent has no hue to tint with.
        return mixed;
    }
    let step = mixed.distance(list);
    let luminance = list.relative_luminance();
    let at = |shift: f64| {
        (
            list_a + toward_a / length * shift,
            list_b + toward_b / length * shift,
        )
    };
    let within = (0..=20)
        .rev()
        .map(|twentieths| step * f64::from(twentieths) / 20.0)
        .find(|shift| {
            let (a, b) = at(*shift);
            a.hypot(b) <= colourful
        })
        .unwrap_or(0.0);
    // A hovered row steps at least `ROW_HOVER_MIN_STEP` and at most `ROW_HOVER_MAX_SHARE` of
    // this one's step; a little over, for 8-bit channels.
    let least = (ROW_HOVER_MIN_STEP / ROW_HOVER_MAX_SHARE + 0.002).min(step);
    let shift = within.max(least);
    (1..=20)
        .rev()
        .find_map(|twentieths| {
            let (a, b) = at(shift * f64::from(twentieths) / 20.0);
            let (chroma, hue) = (a.hypot(b), b.atan2(a).to_degrees());
            // The lightness near the list's whose colour is closest to its luminance and keeps
            // the bar (8-bit channels round the luminance a little either way).
            (-40..=40)
                .filter_map(|offset| {
                    Rgb::from_oklch(lightness + f64::from(offset) * 0.001, chroma, hue)
                })
                .filter(|fill| accent.contrast(*fill) >= NON_TEXT_CONTRAST)
                .min_by(|x, y| {
                    let off = |fill: &Rgb| (fill.relative_luminance() - luminance).abs();
                    off(x).total_cmp(&off(y))
                })
        })
        .unwrap_or(mixed)
}

/// omarchy-shell's hover colour (else `fallback`, when that colour can't step from the list)
/// as far over `list` as a hovered row steps: [`ROW_HOVER_SHARE`] of `selected`'s step, at
/// least [`ROW_HOVER_MIN_STEP`] and at most [`ROW_HOVER_MAX_SHARE`] of it.
fn hover_fill(hover: Rgb, fallback: Rgb, list: Rgb, selected: Rgb) -> Rgb {
    let step = selected.distance(list);
    let wanted = (step * ROW_HOVER_SHARE)
        .max(ROW_HOVER_MIN_STEP)
        .min(step * ROW_HOVER_MAX_SHARE);
    // In steps of half a percent, the first fill that steps far enough; 8-bit channels move a
    // dark grey by about 0.006 per level.
    let fills = |color: Rgb| (1..=200).map(move |n| color.over(list, f64::from(n) / 200.0));
    fills(hover)
        .chain(fills(fallback))
        .find(|fill| fill.distance(list) >= wanted)
        .unwrap_or(fallback)
}

/// The roles Pronto adds to the palette (DESIGN.md "Theme tokens").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailRoles {
    /// Snippets, times, addresses and quotes: [`secondary_text`].
    pub secondary_text: Rgb,
    pub star: Rgb,
    pub unread: Rgb,
    /// The selected filter chip's text: the accent where it reaches [`TEXT_CONTRAST`] on the
    /// chip background, otherwise whichever of the accent and `foreground` reads better there.
    pub selected_chip: Rgb,
    /// The background tinted 18 % (dark) or 14 % (light) toward the accent.
    pub selected_chip_background: Rgb,
    pub avatars: [AvatarTint; AVATAR_TINTS],
    /// [`account_colors`].
    pub accounts: [Rgb; AccountColor::ALL.len()],
}

impl MailRoles {
    pub fn new(palette: &Palette) -> Self {
        let p = palette;
        let chip_tint = match p.mode {
            Mode::Dark => 18,
            Mode::Light => 14,
        };
        let chip_background = p.background.mix(p.accent, chip_tint);
        let selected_chip = if p.accent.contrast(chip_background) >= TEXT_CONTRAST {
            p.accent
        } else {
            readable_on(chip_background, p.accent, p.foreground)
        };
        Self {
            secondary_text: secondary_text(p),
            star: p.accent,
            unread: p.accent,
            selected_chip,
            selected_chip_background: chip_background,
            avatars: avatar_tints(p.accent, p.mode),
            accounts: account_colors(p),
        }
    }

    /// The tint for a sender's address.
    pub fn avatar(&self, address: &str) -> AvatarTint {
        self.avatars[avatar_index(address)]
    }
}

#[cfg(test)]
mod tests {
    use super::super::color::hue_distance;
    use super::*;

    #[test]
    fn avatar_index_is_stable_and_ignores_case_and_spaces() {
        let index = avatar_index("ingrid@fjordpost.example");
        assert_eq!(index, avatar_index(" Ingrid@FjordPost.example "));
        assert!(index < AVATAR_TINTS);
        assert_eq!(
            avatar_index(""),
            0xcbf2_9ce4_8422_2325_u64 as usize % AVATAR_TINTS
        );
        let spread: std::collections::BTreeSet<usize> = (0..200)
            .map(|n| avatar_index(&format!("user{n}@example.test")))
            .collect();
        assert_eq!(spread.len(), AVATAR_TINTS);
    }

    #[test]
    fn avatar_index_values_never_change() {
        // FNV-1a mod 7 for fixed synthetic addresses; a different value means every sender
        // would change colour after an update.
        let indexes: Vec<usize> = [
            "ingrid@fjordpost.example",
            "news@northwind.example",
            "marius@stavtech.example",
            "a@b.test",
        ]
        .iter()
        .map(|address| avatar_index(address))
        .collect();
        assert_eq!(indexes, [2, 4, 5, 0]);
    }

    #[test]
    fn tints_rotate_from_the_accent_hue() {
        let accent = Rgb::from_hex(0xe68e0d);
        let (accent_hue, _, _) = accent.to_hsl();
        let tints = avatar_tints(accent, Mode::Dark);
        for (index, tint) in tints.iter().enumerate() {
            let (hue, saturation, lightness) = tint.background.to_hsl();
            let expected = accent_hue + AVATAR_HUE_STEP * index as f64;
            assert!(hue_distance(hue, expected) < 2.0, "{index}: {hue}");
            assert!((saturation - AVATAR_SATURATION).abs() < 0.03, "{index}");
            assert!((lightness - AVATAR_LIGHTNESS_DARK).abs() < 0.01, "{index}");
        }
        let light = avatar_tints(accent, Mode::Light);
        assert!(light.iter().all(|tint| tint.foreground == INITIALS_DARK));
        assert!(tints.iter().all(|tint| tint.foreground == INITIALS_LIGHT));
    }

    #[test]
    fn initials_come_from_the_name_then_the_address() {
        assert_eq!(initials("Ingrid Solberg", "ingrid@fjordpost.example"), "IS");
        assert_eq!(
            initials("\"Northwind Weekly\"", "news@northwind.example"),
            "NW"
        );
        assert_eq!(initials("Marius Dahl (Work)", ""), "MD");
        assert_eq!(initials("Cher", ""), "C");
        assert_eq!(initials("  ", "tomas.berg@example.net"), "TB");
        assert_eq!(initials("", "news@northwind.example"), "N");
        assert_eq!(initials("øyvind ås", ""), "ØÅ");
        assert_eq!(initials("", ""), "?");
        assert_eq!(initials("(none)", "@example.test"), "?");
    }

    #[test]
    fn mail_roles_follow_the_design_table() {
        let palette = Palette::DARK;
        let roles = MailRoles::new(&palette);
        assert_eq!(
            roles.secondary_text,
            palette.foreground.mix(palette.background, SECONDARY_MIX)
        );
        assert_eq!(roles.star, palette.accent);
        assert_eq!(roles.unread, palette.accent);
        assert_eq!(
            roles.selected_chip_background,
            palette.background.mix(palette.accent, 18)
        );
        assert_eq!(
            roles.avatar("ingrid@fjordpost.example"),
            roles.avatars[avatar_index("ingrid@fjordpost.example")]
        );
    }

    #[test]
    fn row_roles_follow_the_design_table() {
        let palette = Palette::DARK;
        let ui = UiRoles::new(&palette);
        let controls = ControlStates::from_shell(&Default::default(), &palette);
        let rows = RowRoles::new(&palette, ui.sidebar, ui.error, &controls);
        assert_eq!(
            rows.selected_background,
            ui.sidebar.mix(palette.accent, ROW_SELECTED_TINT[0])
        );
        // The hover: the foreground over the list, half the selected row's step from it.
        let step = |fill: Rgb| fill.distance(ui.sidebar);
        let hover = step(rows.hover_background);
        let half = step(rows.selected_background) * ROW_HOVER_SHARE;
        assert!((half..half + 0.01).contains(&hover), "{hover} for {half}");
        let alpha = (1..=200)
            .map(|n| f64::from(n) / 200.0)
            .find(|alpha| palette.foreground.over(ui.sidebar, *alpha) == rows.hover_background)
            .unwrap();
        assert!(step(palette.foreground.over(ui.sidebar, alpha - 0.005)) < half);
        // Adwaita's dark palette reads at the design's mixes, and its error colour as it is.
        let mix = |percent| palette.foreground.mix(palette.background, percent);
        assert_eq!(rows.read_text, mix(READ_MIX));
        assert_eq!(rows.tertiary_text, mix(TERTIARY_MIX));
        assert_eq!(rows.hover_tertiary_text, mix(TERTIARY_MIX));
        assert_eq!(rows.selected_tertiary_text, mix(TERTIARY_MIX));
        assert_eq!(rows.draft, ui.error);
        // Adwaita dark's magenta reads as the reminder's mark as it is.
        assert_eq!(rows.reminder, palette.magenta);
        for surface in rows.surfaces(&palette, ui.sidebar) {
            assert!(rows.reminder.contrast(surface) >= MARK_CONTRAST);
            for color in [rows.read_text, rows.draft] {
                assert!(
                    color.contrast(surface) >= TEXT_CONTRAST,
                    "{color} on {surface}"
                );
            }
        }
        let light = Palette::LIGHT;
        let ui = UiRoles::new(&light);
        let rows = RowRoles::new(&light, ui.sidebar, ui.error, &controls);
        assert_eq!(
            rows.selected_background,
            ui.sidebar.mix(light.accent, ROW_SELECTED_TINT[1])
        );
    }

    /// rose-pine's pine accent on its base: 3.38:1 on the list, 2.92:1 on the 16 % mix. The fill
    /// keeps the bar at 3:1 and is still a tint of the accent's hue, no further from the list
    /// than a hovered row needs (phase C).
    #[test]
    fn the_selected_fill_keeps_the_accents_bar() {
        let (list, accent) = (Rgb::from_hex(0x19_17_24), Rgb::from_hex(0x31_74_8f));
        assert!(accent.contrast(list) >= NON_TEXT_CONTRAST);
        assert!(accent.contrast(list.mix(accent, 16)) < NON_TEXT_CONTRAST);
        let fill = selected_fill(list, accent, 16);
        assert!(
            accent.contrast(fill) >= NON_TEXT_CONTRAST,
            "{:.2}",
            accent.contrast(fill)
        );
        assert!(fill.distance(list) > 0.01, "{fill} is no tint of {list}");
        // Toward the accent's hue, at the list's luminance, as far as the mix steps.
        let toward = |color: Rgb| {
            let ([_, a, b], [_, a0, b0]) = (color.to_oklab(), list.to_oklab());
            (b - b0).atan2(a - a0).to_degrees()
        };
        assert!((toward(fill) - toward(accent)).abs() < 10.0, "{fill}");
        assert!(
            fill.contrast(list) < 1.15,
            "{fill} keeps about the list's luminance"
        );
        // No further than the mix, nor than a hovered row needs (it would be no more
        // colourful than the list or the mix only nearer the list).
        let least = ROW_HOVER_MIN_STEP / ROW_HOVER_MAX_SHARE;
        let distance = fill.distance(list);
        assert!(distance <= list.mix(accent, 16).distance(list) + 0.005);
        assert!(
            (least..least + 0.006).contains(&distance),
            "{fill}: {distance:.4}"
        );
        // Where the mix keeps 3:1 it stays as it was, and where the accent hasn't 3:1 on the
        // list (no tint can give it) too.
        let blue = Rgb::from_hex(0x35_84_e4);
        let dark = Rgb::from_hex(0x28_28_2c);
        assert_eq!(selected_fill(dark, blue, 16), dark.mix(blue, 16));
        let faint = Rgb::from_hex(0x52_4f_67);
        assert_eq!(selected_fill(list, faint, 16), list.mix(faint, 16));
    }

    #[test]
    fn muted_text_mixes_only_as_far_as_it_reads() {
        let palette = Palette::DARK;
        let surfaces = [palette.background, Rgb::from_hex(0x6a6a6a)];
        let muted = muted_text_on(&palette, 60, &surfaces);
        let percent = (0..=60)
            .rev()
            .find(|percent| palette.foreground.mix(palette.background, *percent) == muted)
            .unwrap();
        assert!(percent < 60);
        assert!(reads_on_all(muted, &surfaces));
        assert!(!reads_on_all(
            palette.foreground.mix(palette.background, percent + 1),
            &surfaces
        ));
        assert_eq!(
            muted_text_on(&palette, 10, &[palette.foreground]),
            palette.foreground
        );
    }

    #[test]
    fn secondary_text_mixes_less_until_it_reads() {
        let faint = Palette {
            foreground: Rgb::from_hex(0x575279),
            background: Rgb::from_hex(0xfaf4ed),
            dark_background: Rgb::from_hex(0xf2e9e1),
            ..Palette::LIGHT
        };
        let secondary = secondary_text(&faint);
        let percent = (0..=SECONDARY_MIX)
            .rev()
            .find(|percent| faint.foreground.mix(faint.background, *percent) == secondary)
            .unwrap();
        assert!(percent < SECONDARY_MIX, "{percent}");
        for background in [faint.background, faint.dark_background] {
            assert!(secondary.contrast(background) >= TEXT_CONTRAST);
        }
        let more = faint.foreground.mix(faint.background, percent + 1);
        assert!(
            more.contrast(faint.background) < TEXT_CONTRAST
                || more.contrast(faint.dark_background) < TEXT_CONTRAST
        );

        let hopeless = Palette {
            foreground: Rgb::from_hex(0x888888),
            background: Rgb::from_hex(0x777777),
            dark_background: Rgb::from_hex(0x666666),
            ..Palette::DARK
        };
        assert_eq!(secondary_text(&hopeless), hopeless.foreground);
    }

    #[test]
    fn the_selected_chip_keeps_the_accent_only_where_it_reads() {
        let readable = Palette {
            accent: Rgb::from_hex(0xe68e0d),
            background: Rgb::from_hex(0x121212),
            ..Palette::DARK
        };
        assert_eq!(MailRoles::new(&readable).selected_chip, readable.accent);

        let roles = MailRoles::new(&Palette::DARK);
        let chip = roles.selected_chip_background;
        assert!(Palette::DARK.accent.contrast(chip) < TEXT_CONTRAST);
        assert_eq!(roles.selected_chip, Palette::DARK.foreground);
        assert!(roles.selected_chip.contrast(chip) >= TEXT_CONTRAST);
    }

    #[test]
    fn borders_mix_toward_the_foreground() {
        for palette in [Palette::DARK, Palette::LIGHT] {
            let percent = match palette.mode {
                Mode::Dark => 8,
                Mode::Light => 14,
            };
            assert_eq!(
                UiRoles::new(&palette).border,
                palette.lighter_background.mix(palette.foreground, percent)
            );
        }
        let matte = Palette {
            lighter_background: Rgb::from_hex(0x1e1e1e),
            foreground: Rgb::from_hex(0xbebebe),
            ..Palette::DARK
        };
        assert_eq!(UiRoles::new(&matte).border, Rgb::from_hex(0x2b2b2b));
    }

    #[test]
    fn status_colours_are_turned_apart_on_matte_black() {
        let palette = Palette {
            red: Rgb::from_hex(0xd35f5f),
            yellow: Rgb::from_hex(0xb91c1c),
            green: Rgb::from_hex(0xffc107),
            ..Palette::DARK
        };
        let roles = UiRoles::new(&palette);
        let hues = [roles.error, roles.warning, roles.success].map(|color| color.to_hsl().0);
        for (index, hue) in hues.iter().enumerate() {
            assert!(
                hue_distance(*hue, STATUS_HUES[index]) < 2.0,
                "{index}: {hue}"
            );
        }
    }
}
