//! The theme over Omarchy's 22 stock palettes, resolved by `omarchy-theme-color --all` and
//! stored in `tests/theme/` (regenerate with `bash tests/theme/regenerate.sh`), so nothing
//! here reads the system.

use proptest::prelude::*;
use sealwax_domain::account::AccountColor;
use sealwax_domain::theme::{
    AvatarTint, BORDER_MIX, BodyStyle, FontFamilies, MEASURE_CSS, Mode, PRIMARY_CONTRAST,
    PRIMARY_DEEPEN_MAX, Palette, READ_MIX, ROW_HOVER_MAX_SHARE, ROW_HOVER_MIN_STEP,
    ROW_HOVER_SHARE, ROW_SELECTED_TINT, Rgb, RowRoles, SECONDARY_MIX, ShellValues, TERTIARY_MIX,
    Theme, ThemeInputs, ThemeOptions, ThemeSource, avatar_index, avatar_tints, deepened,
    hue_distance, link_on, raised_secondary_text,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

/// WCAG AA for normal text: the avatar initials must reach it on every tint, secondary text
/// on the window and the list, and the selected chip's text on its background.
const INITIALS_CONTRAST: f64 = 4.5;
const TEXT_CONTRAST: f64 = 4.5;
/// WCAG's floor for large text and controls: text on the accent fill.
const ACCENT_TEXT_CONTRAST: f64 = 3.0;
/// WCAG's floor for graphics that carry meaning (1.4.11): the selected row's bar, the unread dot.
const NON_TEXT_CONTRAST: f64 = 3.0;
/// Tints are 51° apart by design; quantizing to 8-bit channels moves a hue by about 1°.
const TINT_HUE_GAP: f64 = 49.0;
/// The smallest RGB distance between two tints over every accent hue and both modes was
/// 24.8 when this was written (the test below samples every degree).
const TINT_RGB_GAP: f64 = 20.0;

const SNAPSHOTTED: [&str; 5] = [
    "matte-black",
    "tokyo-night",
    "catppuccin-latte",
    "flexoki-light",
    "gruvbox",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/theme")
}

fn theme_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn resolved(name: &str) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(fixtures_dir().join(name).join("resolved.tsv")).unwrap();
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn theme(name: &str) -> Theme {
    let inputs = ThemeInputs {
        source: ThemeSource::Omarchy(name.to_owned()),
        palette: Palette::from_resolved(&resolved(name)),
        shell: ShellValues::new(),
        fonts: FontFamilies {
            monospace: "JetBrainsMono Nerd Font".to_owned(),
            sans: "Liberation Sans".to_owned(),
        },
        hyprland_rounding: Some(8),
    };
    Theme::new(&inputs, &ThemeOptions::default())
}

fn rgb_distance(a: Rgb, b: Rgb) -> f64 {
    [(a.r, b.r), (a.g, b.g), (a.b, b.b)]
        .iter()
        .map(|&(x, y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum::<f64>()
        .sqrt()
}

fn assert_tints_apart(tints: &[AvatarTint], context: &str) {
    for (index, a) in tints.iter().enumerate() {
        for (offset, b) in tints[index + 1..].iter().enumerate() {
            let (x, y) = (a.background, b.background);
            let pair = format!("{context}: tints {index} and {}", index + offset + 1);
            assert!(
                hue_distance(x.to_hsl().0, y.to_hsl().0) >= TINT_HUE_GAP,
                "{pair}: {x} {y}"
            );
            assert!(rgb_distance(x, y) >= TINT_RGB_GAP, "{pair}: {x} {y}");
        }
    }
}

/// The mail roles and the status colours, one per line, for snapshots.
fn roles(theme: &Theme) -> String {
    let mut text = String::new();
    let m = &theme.mail;
    let ui = &theme.ui;
    let _ = writeln!(text, "mode {}", theme.mode().as_str());
    let _ = writeln!(text, "secondary_text {}", m.secondary_text);
    for (surface, rows) in [("", &theme.rows), ("window ", &theme.window_rows)] {
        let _ = writeln!(
            text,
            "{surface}hovered row: secondary_text {} on {}",
            rows.hover_secondary_text, rows.hover_background
        );
        let _ = writeln!(
            text,
            "{surface}selected row: secondary_text {} on {}",
            rows.selected_secondary_text, rows.selected_background
        );
        let _ = writeln!(
            text,
            "{surface}rows: read_text {} draft {} reminder {}",
            rows.read_text, rows.draft, rows.reminder
        );
        let _ = writeln!(
            text,
            "{surface}tertiary_text {} hovered {} selected {}",
            rows.tertiary_text, rows.hover_tertiary_text, rows.selected_tertiary_text
        );
    }
    let _ = writeln!(text, "star {}", m.star);
    let _ = writeln!(text, "unread {}", m.unread);
    let _ = writeln!(
        text,
        "selected_chip {} on {}",
        m.selected_chip, m.selected_chip_background
    );
    let _ = writeln!(text, "accent_text {}", ui.accent_text);
    let _ = writeln!(
        text,
        "primary {} on {}; composer {}",
        ui.primary_text, ui.primary, ui.composer
    );
    let _ = writeln!(text, "border {}", ui.border);
    let _ = writeln!(
        text,
        "error {} warning {} success {}",
        ui.error, ui.warning, ui.success
    );
    for (index, tint) in m.avatars.iter().enumerate() {
        let _ = writeln!(
            text,
            "avatar {index} {} on {}",
            tint.foreground, tint.background
        );
    }
    for (color, value) in AccountColor::ALL.iter().zip(m.accounts) {
        let _ = writeln!(text, "account {} {value}", color.name());
    }
    text
}

#[test]
fn all_stock_themes_are_present() {
    let names = theme_names();
    assert_eq!(names.len(), 22, "{names:?}");
    for name in SNAPSHOTTED {
        assert!(names.iter().any(|known| known == name), "{name}");
    }
}

#[test]
fn modes_match_the_files() {
    let light: Vec<String> = theme_names()
        .into_iter()
        .filter(|name| theme(name).mode() == Mode::Light)
        .collect();
    assert_eq!(
        light,
        [
            "catppuccin-latte",
            "flexoki-light",
            "lupine",
            "rose-pine",
            "white"
        ]
    );
    for name in theme_names() {
        assert_eq!(
            theme(&name).mode().as_str(),
            resolved(&name)["mode"],
            "{name}"
        );
    }
}

#[test]
fn snapshots() {
    for name in SNAPSHOTTED {
        let theme = theme(name);
        insta::assert_snapshot!(format!("{name}_css"), theme.css());
        insta::assert_snapshot!(format!("{name}_roles"), roles(&theme));
        insta::assert_snapshot!(
            format!("{name}_webview_themed"),
            theme.webview_css(BodyStyle::Themed)
        );
    }
    insta::assert_snapshot!(
        "webview_paper",
        theme("matte-black").webview_css(BodyStyle::Paper)
    );
}

#[test]
fn tints_stay_distinct_on_single_hue_themes() {
    let matte = theme("matte-black");
    let p = &matte.palette;
    let own = [p.red, p.yellow, p.green, p.blue, p.magenta].map(|color| color.to_hsl().0);
    let spread_from_accent = own
        .iter()
        .map(|hue| hue_distance(*hue, p.accent.to_hsl().0))
        .fold(0.0, f64::max);
    assert!(
        spread_from_accent < 45.0,
        "matte-black is no longer single-hue: {own:?}"
    );
    assert_tints_apart(&matte.mail.avatars, "matte-black");
    for name in theme_names() {
        assert_tints_apart(&theme(&name).mail.avatars, &name);
    }
}

#[test]
fn tints_stay_distinct_for_every_accent_hue() {
    for mode in [Mode::Dark, Mode::Light] {
        for hue in 0..360 {
            let accent = Rgb::from_hsl(f64::from(hue), 0.7, 0.5);
            let context = format!("{} accent hue {hue}", mode.as_str());
            assert_tints_apart(&avatar_tints(accent, mode), &context);
        }
    }
    assert_tints_apart(
        &avatar_tints(Rgb::from_hex(0x6e6e6e), Mode::Light),
        "grey accent",
    );
}

#[test]
fn initials_clear_aa_contrast_on_every_tint() {
    for name in theme_names() {
        for tint in theme(&name).mail.avatars {
            let contrast = tint.background.contrast(tint.foreground);
            assert!(
                contrast >= INITIALS_CONTRAST,
                "{name}: {} on {} is {contrast:.2}",
                tint.foreground,
                tint.background
            );
        }
    }
}

#[test]
fn text_on_the_accent_stays_readable() {
    for name in theme_names() {
        let theme = theme(&name);
        let contrast = theme.ui.accent_text.contrast(theme.palette.accent);
        assert!(contrast >= ACCENT_TEXT_CONTRAST, "{name}: {contrast:.2}");
    }
}

/// Send's label: white on the accent deepened a little where that reaches AA (Adwaita's blue in
/// both modes: the fork's phase B, its dark label on Adwaita dark looked broken beside light's
/// white), else the accent with its own label at the accent fill's floor; the hue stays.
#[test]
fn send_reads_alike_in_both_modes() {
    for (name, theme) in all_themes() {
        let ui = &theme.ui;
        let accent = theme.palette.accent;
        let contrast = ui.primary_text.contrast(ui.primary);
        match deepened(accent, Rgb::WHITE, PRIMARY_CONTRAST, PRIMARY_DEEPEN_MAX) {
            Some(fill) => {
                assert_eq!((ui.primary, ui.primary_text), (fill, Rgb::WHITE), "{name}");
                assert!(contrast >= PRIMARY_CONTRAST, "{name}: {contrast:.2}");
            }
            None => {
                assert_eq!((ui.primary, ui.primary_text), (accent, ui.accent_text));
                assert!(contrast >= ACCENT_TEXT_CONTRAST, "{name}: {contrast:.2}");
            }
        }
        if accent.to_hsl().1 > 0.2 {
            let gap = hue_distance(accent.to_hsl().0, ui.primary.to_hsl().0);
            assert!(
                gap < 4.0,
                "{name}: {accent} became {} ({gap:.1}°)",
                ui.primary
            );
        }
    }
    for mode in [Mode::Dark, Mode::Light] {
        let adwaita = Theme::new(&ThemeInputs::builtin(mode), &ThemeOptions::default());
        assert_eq!(adwaita.ui.primary_text, Rgb::WHITE, "{mode:?}");
    }
    let dark = Theme::new(&ThemeInputs::builtin(Mode::Dark), &ThemeOptions::default());
    assert_ne!(
        dark.ui.primary, dark.palette.accent,
        "Adwaita dark's accent deepens"
    );
    assert_eq!(
        dark.ui.composer, dark.palette.lighter_background,
        "Adwaita dark's card takes its lighter surface"
    );
}

/// The composer card's surface: lighter than the window in a dark theme, the window's in a
/// light one; its text and secondary text read on it at AA.
#[test]
fn the_composer_card_is_raised_and_readable() {
    for (name, theme) in all_themes() {
        let raised = theme.ui.composer;
        let p = &theme.palette;
        match theme.mode() {
            Mode::Dark => assert!(
                raised.relative_luminance() >= p.background.relative_luminance(),
                "{name}: {raised} on {}",
                p.background
            ),
            Mode::Light => assert_eq!(raised, p.background, "{name}"),
        }
        for (role, text) in [
            ("text", p.foreground),
            ("secondary text", theme.mail.secondary_text),
            ("a link", link_on(p, raised)),
        ] {
            let contrast = text.contrast(raised);
            assert!(
                contrast >= TEXT_CONTRAST,
                "{name}: {role} {text} on {raised} is {contrast:.2}"
            );
        }
        // A recipient's chip on the card colour: its name is text, its remove button an icon.
        for (role, color, floor) in [
            ("chip text", p.foreground, TEXT_CONTRAST),
            (
                "chip's remove icon",
                theme.mail.secondary_text,
                NON_TEXT_CONTRAST,
            ),
        ] {
            let contrast = color.contrast(theme.ui.card);
            assert!(
                contrast >= floor,
                "{name}: {role} {color} on {} is {contrast:.2}",
                theme.ui.card
            );
        }
        // The link keeps the accent's look: its hue, unless the accent is grey.
        let link = link_on(p, raised);
        if p.accent.to_hsl().1 > 0.2 {
            let gap = hue_distance(p.accent.to_hsl().0, link.to_hsl().0);
            assert!(gap < 20.0, "{name}: {} became {link} ({gap:.1}°)", p.accent);
        }
    }
    let dark = Theme::new(&ThemeInputs::builtin(Mode::Dark), &ThemeOptions::default());
    assert_ne!(
        link_on(&dark.palette, dark.ui.composer),
        dark.palette.accent,
        "Adwaita dark's accent lightens on its composer card"
    );
}

/// Every stock theme, then the built-in palettes.
fn all_themes() -> Vec<(String, Theme)> {
    let mut themes: Vec<(String, Theme)> = theme_names()
        .into_iter()
        .map(|name| {
            let theme = theme(&name);
            (name, theme)
        })
        .collect();
    for mode in [Mode::Dark, Mode::Light] {
        let builtin = Theme::new(&ThemeInputs::builtin(mode), &ThemeOptions::default());
        themes.push((builtin.name().to_owned(), builtin));
    }
    // `palette = "tokyo-night"` in config.toml: Pronto's own Tokyo Night, not the Omarchy
    // fixture's (the phase B merge's visual review took its pickers' screenshots in it).
    let tokyo = ThemeInputs {
        source: ThemeSource::TokyoNight,
        palette: Palette::TOKYO_NIGHT,
        ..ThemeInputs::builtin(Mode::Dark)
    };
    let tokyo = Theme::new(&tokyo, &ThemeOptions::default());
    themes.push((tokyo.name().to_owned(), tokyo));
    themes
}

/// The list's two surfaces and the rows made for each: the sidebar's colour (Split), and the
/// window's, where the list is alone (one pane at a time).
fn row_surfaces(theme: &Theme) -> [(&'static str, Rgb, &RowRoles); 2] {
    [
        ("list", theme.ui.sidebar, &theme.rows),
        (
            "one-pane list",
            theme.palette.background,
            &theme.window_rows,
        ),
    ]
}

fn reads_on_window_and_list(theme: &Theme, color: Rgb) -> bool {
    color.contrast(theme.palette.background) >= TEXT_CONTRAST
        && color.contrast(theme.palette.dark_background) >= TEXT_CONTRAST
}

#[test]
fn secondary_text_reaches_aa_on_the_window_and_the_list() {
    for (name, theme) in all_themes() {
        let secondary = theme.mail.secondary_text;
        for (surface, background) in [
            ("window", theme.palette.background),
            ("list", theme.palette.dark_background),
        ] {
            let contrast = secondary.contrast(background);
            assert!(
                contrast >= TEXT_CONTRAST,
                "{name}: {secondary} on the {surface} {background} is {contrast:.2}"
            );
        }
    }
}

/// The account colours are sidebar text: AA on the window and the sidebar in every theme, each
/// still near its own hue, and no two alike.
#[test]
fn account_colors_read_as_text_and_stay_apart() {
    for (name, theme) in all_themes() {
        let colors = theme.mail.accounts;
        for (color, value) in AccountColor::ALL.iter().zip(colors) {
            assert!(
                reads_on_window_and_list(&theme, value),
                "{name}: {} {value} doesn't read on {} and {}",
                color.name(),
                theme.palette.background,
                theme.palette.dark_background
            );
        }
        for (index, a) in colors.iter().enumerate() {
            for b in &colors[index + 1..] {
                let distance = rgb_distance(*a, *b);
                assert!(
                    distance >= 20.0,
                    "{name}: {a} and {b} are {distance:.1} apart"
                );
            }
        }
    }
}

/// A hovered or selected row's fill must not take secondary text below AA: its own colour reads
/// on the fill, the window and the list's surface, mixed as far toward the background as it can
/// be. On either surface the selected row is the list tinted toward the accent.
#[test]
fn secondary_text_reaches_aa_on_hovered_and_selected_rows() {
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        for (surface, list, rows) in row_surfaces(&theme) {
            for (state, background, secondary) in [
                ("hovered", rows.hover_background, rows.hover_secondary_text),
                (
                    "selected",
                    rows.selected_background,
                    rows.selected_secondary_text,
                ),
            ] {
                let reads = |color: Rgb| {
                    [p.background, list, background]
                        .iter()
                        .all(|on| color.contrast(*on) >= TEXT_CONTRAST)
                };
                let contrast = secondary.contrast(background);
                assert!(
                    contrast >= TEXT_CONTRAST,
                    "{name}: {secondary} on the {surface}'s {state} row {background} is \
                     {contrast:.2}"
                );
                assert!(reads(secondary), "{name} {surface} {state}");
                let percent = (0..=SECONDARY_MIX)
                    .rev()
                    .find(|percent| p.foreground.mix(p.background, *percent) == secondary)
                    .unwrap_or_else(|| {
                        panic!("{name} {surface} {state}: {secondary} is not a mix")
                    });
                if percent < SECONDARY_MIX {
                    let more = p.foreground.mix(p.background, percent + 1);
                    assert!(
                        !reads(more),
                        "{name} {surface} {state}: {percent} % is not the largest mix that reads"
                    );
                }
            }
            let tint = match p.mode {
                Mode::Dark => ROW_SELECTED_TINT[0],
                Mode::Light => ROW_SELECTED_TINT[1],
            };
            let mixed = list.mix(p.accent, tint);
            if p.accent.contrast(mixed) >= NON_TEXT_CONTRAST
                || p.accent.contrast(list) < NON_TEXT_CONTRAST
            {
                assert_eq!(rows.selected_background, mixed, "{name} {surface}");
            } else {
                // The accent's bar would fall under 3:1 on the mix: a tint of hue alone, at
                // the list's luminance, no further from it than the mix, and as far as a
                // hovered row needs to stay the quieter (phase C; its review: no further, where
                // the whole step made rose-pine's list an icy cyan).
                let fill = rows.selected_background;
                assert!(fill.contrast(list) < 1.1, "{name} {surface}: {fill}");
                assert!(
                    fill.distance(list) <= mixed.distance(list) + 0.005,
                    "{name} {surface}: {fill} steps further than {mixed}"
                );
                assert!(
                    fill.distance(list) >= ROW_HOVER_MIN_STEP / ROW_HOVER_MAX_SHARE,
                    "{name} {surface}: {fill} too near {list} for a quieter hover"
                );
            }
        }
    }
}

/// A hovered row is half as far from its list as the selected row (OKLab distance), on the
/// sidebar's colour and on the window's alike, so the pointer's row never reads as a second
/// selection (the Superhuman pass's review, 2026-10-29: on the window's surface Adwaita's hover
/// was as loud as the selection and Tokyo Night's invisible). It shows in every theme, and the
/// selection stays the stronger signal.
#[test]
fn a_hovered_row_is_half_as_loud_as_the_selected_one() {
    for (name, theme) in all_themes() {
        for (surface, list, rows) in row_surfaces(&theme) {
            let hover = rows.hover_background.distance(list);
            let selected = rows.selected_background.distance(list);
            let wanted = (selected * ROW_HOVER_SHARE)
                .max(ROW_HOVER_MIN_STEP)
                .min(selected * ROW_HOVER_MAX_SHARE);
            let context = format!(
                "{name}, the {surface} {list}: the hover {} is {hover:.4} from it, the selection \
                 {} {selected:.4}",
                rows.hover_background, rows.selected_background
            );
            assert!(hover >= ROW_HOVER_MIN_STEP, "{context}");
            // The first fill that steps far enough: an 8-bit level or two past it at most.
            assert!((wanted..wanted + 0.015).contains(&hover), "{context}");
            assert!(hover < selected, "{context}");
        }
    }
}

/// A row's own text (the Superhuman pass): an unread row's foreground, a read row's quieter
/// sender and subject, and the Draft marker read at AA on the window, the list, a hovered row
/// and the selected row, whatever the state; tertiary text (a read row's snippet, the message
/// count) on the surfaces it is drawn on. On both of the list's surfaces.
#[test]
fn row_text_reaches_aa_on_every_row_surface() {
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        for (surface, list, rows) in row_surfaces(&theme) {
            let window = ("window", p.background);
            let list = (surface, list);
            let hovered = ("hovered row", rows.hover_background);
            let selected = ("selected row", rows.selected_background);
            let every = [window, list, hovered, selected];
            for (role, color, surfaces) in [
                ("foreground", p.foreground, &every[..]),
                ("read_text", rows.read_text, &every[..]),
                ("draft", rows.draft, &every[..]),
                ("tertiary_text", rows.tertiary_text, &every[..2]),
                (
                    "hover_tertiary_text",
                    rows.hover_tertiary_text,
                    &[window, list, hovered][..],
                ),
                (
                    "selected_tertiary_text",
                    rows.selected_tertiary_text,
                    &[window, list, selected][..],
                ),
            ] {
                for (on, background) in surfaces {
                    let contrast = color.contrast(*background);
                    assert!(
                        contrast >= TEXT_CONTRAST,
                        "{name}: {role} {color} on the {surface}'s {on} {background} is \
                         {contrast:.2}"
                    );
                }
            }
        }
    }
}

/// The clock mark of a conversation back from Reminders (the fork's phase C) reaches 3:1 on
/// the window, the list, a hovered row and the selected row in every theme, on both of the
/// list's surfaces, and keeps a hue of its own: the palette's magenta (Superhuman's purple) or
/// the accent, mixed toward the text colour no further than it must.
#[test]
fn the_reminder_mark_reads_on_every_row_surface() {
    use sealwax_domain::theme::{MARK_CONTRAST, MARK_MIX_MAX};
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        for (surface, list, rows) in row_surfaces(&theme) {
            for background in rows.surfaces(p, list) {
                let contrast = rows.reminder.contrast(background);
                assert!(
                    contrast >= MARK_CONTRAST,
                    "{name}: the reminder mark {} on the {surface}'s {background} is \
                     {contrast:.2}",
                    rows.reminder
                );
            }
            let mixed = [p.magenta, p.accent].iter().any(|start| {
                (0..=MARK_MIX_MAX).any(|percent| start.mix(p.foreground, percent) == rows.reminder)
            });
            assert!(
                mixed,
                "{name}: {} is neither magenta nor accent",
                rows.reminder
            );
        }
    }
}

/// Read and tertiary text mix as far toward the background as they still read on their
/// surfaces, from their design mixes; read rows never get brighter than unread ones. On both of
/// the list's surfaces.
#[test]
fn row_text_mixes_as_far_as_it_reads() {
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        for (surface, list, rows) in row_surfaces(&theme) {
            let every = rows.surfaces(p, list);
            let hovered = [p.background, list, rows.hover_background];
            let selected = [p.background, list, rows.selected_background];
            // Read text never mixes further than the secondary text, an unread row's snippet,
            // so it stays the louder of the two (phase C).
            let secondary = (0..=SECONDARY_MIX)
                .rev()
                .find(|percent| {
                    p.foreground.mix(p.background, *percent) == theme.mail.secondary_text
                })
                .unwrap_or(SECONDARY_MIX);
            for (role, color, design, surfaces) in [
                (
                    "read_text",
                    rows.read_text,
                    READ_MIX.min(secondary),
                    &every[..],
                ),
                (
                    "tertiary_text",
                    rows.tertiary_text,
                    TERTIARY_MIX,
                    &every[..2],
                ),
                (
                    "hover_tertiary_text",
                    rows.hover_tertiary_text,
                    TERTIARY_MIX,
                    &hovered[..],
                ),
                (
                    "selected_tertiary_text",
                    rows.selected_tertiary_text,
                    TERTIARY_MIX,
                    &selected[..],
                ),
            ] {
                let percent = (0..=design)
                    .rev()
                    .find(|percent| p.foreground.mix(p.background, *percent) == color)
                    .unwrap_or_else(|| panic!("{name} {surface}: {role} {color} is not a mix"));
                if percent < design {
                    let more = p.foreground.mix(p.background, percent + 1);
                    assert!(
                        surfaces
                            .iter()
                            .any(|surface| more.contrast(*surface) < TEXT_CONTRAST),
                        "{name} {surface}: {role} at {percent} % is not the largest mix that reads"
                    );
                }
            }
            // On every surface, unread text is the brightest, then read text, then an unread
            // row's snippet, then a read row's.
            for (background, secondary, tertiary) in [
                (list, theme.mail.secondary_text, rows.tertiary_text),
                (
                    rows.hover_background,
                    rows.hover_secondary_text,
                    rows.hover_tertiary_text,
                ),
                (
                    rows.selected_background,
                    rows.selected_secondary_text,
                    rows.selected_tertiary_text,
                ),
            ] {
                let contrast = |color: Rgb| color.contrast(background);
                assert!(
                    contrast(p.foreground) >= contrast(rows.read_text)
                        && contrast(rows.read_text) >= contrast(secondary)
                        && contrast(secondary) >= contrast(tertiary),
                    "{name}: the row's text on {background} ({surface}) isn't ordered from \
                     unread to read"
                );
            }
        }
    }
}

/// The accent marks the selected row (its bar) and unread rows (the dot). Where a theme's accent
/// keeps WCAG's 3:1 for graphics on the list, the selected row's tint doesn't take it below
/// (rose-pine's accent is 2.8:1 on its own list; unread rows are bold as well). On both of the
/// list's surfaces, every theme: where the accent clears 3:1 by too little for its mix, the
/// tint is a hue at the list's lightness (phase C: rose-pine's one-pane list, 3.14:1 on its
/// window, fell to 2.78:1 on the mix and was this test's one exception).
#[test]
fn the_accent_marks_read_on_the_selected_row() {
    for (name, theme) in all_themes() {
        let accent = theme.palette.accent;
        for (surface, list, rows) in row_surfaces(&theme) {
            if accent.contrast(list) < NON_TEXT_CONTRAST {
                continue;
            }
            let background = rows.selected_background;
            let contrast = accent.contrast(background);
            assert!(
                contrast >= NON_TEXT_CONTRAST,
                "{name}: the accent {accent} on the {surface}'s selected row {background} is \
                 {contrast:.2}"
            );
            assert!(
                background.distance(list) >= 0.01,
                "{name}: the {surface}'s selected row {background} is no tint of {list}"
            );
        }
    }
}

/// Themes that read at the design's 35 % keep it; the others take the largest smaller mix
/// that reads.
#[test]
fn secondary_text_mixes_as_far_as_it_reads() {
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        let secondary = theme.mail.secondary_text;
        let design = p.foreground.mix(p.background, SECONDARY_MIX);
        if reads_on_window_and_list(&theme, design) {
            assert_eq!(
                secondary, design,
                "{name} reads at 35 % and must not change"
            );
            continue;
        }
        let percent = (0..SECONDARY_MIX)
            .rev()
            .find(|percent| p.foreground.mix(p.background, *percent) == secondary)
            .unwrap_or_else(|| panic!("{name}: {secondary} is not a smaller mix"));
        let more = p.foreground.mix(p.background, percent + 1);
        assert!(
            !reads_on_window_and_list(&theme, more),
            "{name}: {percent} % is not the largest mix that reads"
        );
    }
}

#[test]
fn the_selected_chip_reaches_aa() {
    for (name, theme) in all_themes() {
        let m = &theme.mail;
        let (accent, foreground) = (theme.palette.accent, theme.palette.foreground);
        let background = m.selected_chip_background;
        let contrast = m.selected_chip.contrast(background);
        assert!(contrast >= TEXT_CONTRAST, "{name}: {contrast:.2}");
        if accent.contrast(background) >= TEXT_CONTRAST {
            assert_eq!(m.selected_chip, accent, "{name} keeps the accent");
        } else {
            assert!(
                m.selected_chip == foreground
                    && foreground.contrast(background) > accent.contrast(background),
                "{name}: the more readable of accent and foreground"
            );
        }
    }
}

/// The command palette's card is raised over the window in every theme (Tokyo Night's popover
/// colour, #0e0e14, read as a hole in the dimmed window), its text reads there and on a keycap
/// (never worse than on the window's own keycap: Everforest's secondary text is 4.49:1 there),
/// and a theme whose popover already is raised above the window and the composer card and
/// reads keeps it.
#[test]
fn the_palette_card_is_raised_and_reads() {
    let mut stepped = Vec::new();
    for (name, theme) in all_themes() {
        let (p, ui) = (&theme.palette, &theme.ui);
        let raised = ui.raised;
        let keycap = |surface: Rgb| surface.mix(p.foreground, 4);
        for (what, color) in [
            ("text", p.foreground),
            ("secondary text", theme.mail.secondary_text),
        ] {
            let window_keycap = color.contrast(keycap(p.background));
            for (surface, background, floor) in [
                ("card", raised, TEXT_CONTRAST),
                ("keycap", keycap(raised), TEXT_CONTRAST.min(window_keycap)),
            ] {
                let contrast = color.contrast(background);
                assert!(
                    contrast >= floor,
                    "{name}: {what} {color} on the palette's {surface} {background} is \
                     {contrast:.2}"
                );
            }
        }
        assert!(
            raised.relative_luminance() >= p.background.relative_luminance(),
            "{name}: the card {raised} is darker than the window {}",
            p.background
        );
        let popover_reads = [ui.popover, ui.popover.mix(p.foreground, 4)]
            .iter()
            .all(|surface| theme.mail.secondary_text.contrast(*surface) >= TEXT_CONTRAST);
        let popover_raised = ui.popover.relative_luminance()
            >= p.background
                .relative_luminance()
                .max(ui.composer.relative_luminance());
        if popover_raised && popover_reads {
            assert_eq!(raised, ui.popover, "{name} keeps its popover colour");
        } else {
            stepped.push(name);
        }
    }
    for name in ["tokyo-night", "catppuccin-latte"] {
        assert!(stepped.iter().any(|stepped| stepped == name), "{stepped:?}");
    }
    let tokyo = theme("tokyo-night");
    assert!(
        tokyo.ui.raised.relative_luminance() > tokyo.palette.background.relative_luminance(),
        "Tokyo Night's card {} is lighter than its window",
        tokyo.ui.raised
    );
}

/// Secondary text in the palette's and the pickers' rows reads on the raised card, a hovered
/// row and the selected row, each with a keycap's fill (phase C: the app's secondary text fell
/// to 4.16:1 on a hovered row in Tokyo Night, 4.13:1 in Gruvbox, 4.38:1 in Catppuccin), or is
/// the foreground where nothing quieter does.
#[test]
fn the_palette_rows_secondary_text_reads_hovered_and_selected() {
    for (name, theme) in all_themes() {
        let (p, c) = (&theme.palette, &theme.controls);
        let raised = theme.ui.raised;
        let text = raised_secondary_text(p, raised, c);
        let hovered = c.hover.color.over(raised, c.hover.fill);
        let selected = c.selected.color.over(raised, c.selected.fill);
        for (what, surface) in [
            ("card", raised),
            ("hovered row", hovered),
            ("selected row", selected),
            ("keycap on a hovered row", hovered.mix(p.foreground, 4)),
        ] {
            let floor = TEXT_CONTRAST.min(p.foreground.contrast(surface));
            let contrast = text.contrast(surface);
            assert!(
                contrast >= floor,
                "{name}: {text} on the palette's {what} {surface} is {contrast:.2}"
            );
        }
        assert!(
            text.contrast(raised) <= p.foreground.contrast(raised),
            "{name}: never louder than the text"
        );
    }
    // The case the review measured: Tokyo Night's hovered row, 4.16:1 with the app's own.
    let tokyo = theme("tokyo-night");
    let (p, c) = (&tokyo.palette, &tokyo.controls);
    let hovered = c.hover.color.over(tokyo.ui.raised, c.hover.fill);
    assert!(tokyo.mail.secondary_text.contrast(hovered) < TEXT_CONTRAST);
    assert!(raised_secondary_text(p, tokyo.ui.raised, c).contrast(hovered) >= TEXT_CONTRAST);
}

/// One elevation order in every theme: window ≤ composer card ≤ the palette's and pickers'
/// raised card, which open over the composer card (the phase B merge's visual review: Tokyo
/// Night's snippet and time pickers, #1f2231, sank into the card under them, #24283b). Adwaita
/// dark keeps its popover colour a step above its card; Omarchy themes whose raised step reads
/// as far as the card's keep the card's step, the others bring the card down to theirs.
#[test]
fn surfaces_rise_in_one_order() {
    for (name, theme) in all_themes() {
        let (window, ui) = (theme.palette.background, &theme.ui);
        let lum = Rgb::relative_luminance;
        assert!(
            lum(window) <= lum(ui.composer) && lum(ui.composer) <= lum(ui.raised),
            "{name}: window {window}, composer card {}, raised card {}",
            ui.composer,
            ui.raised
        );
    }
    let dark = Theme::new(&ThemeInputs::builtin(Mode::Dark), &ThemeOptions::default());
    assert!(
        dark.ui.raised.relative_luminance() > dark.ui.composer.relative_luminance(),
        "Adwaita dark's pickers stand above its composer card"
    );
    let matte = theme("matte-black");
    assert_eq!(
        (matte.ui.composer, matte.ui.raised),
        (
            matte.palette.lighter_background,
            matte.palette.lighter_background
        ),
        "matte-black's card keeps its whole step and the palette rises to it"
    );
    let tokyo = theme("tokyo-night");
    assert_eq!(
        tokyo.ui.composer, tokyo.ui.raised,
        "Omarchy's Tokyo Night: the card comes down to the raised step its keycaps allow"
    );
    let (_, own) = all_themes()
        .into_iter()
        .find(|(name, _)| name == "builtin-tokyo-night")
        .expect("Pronto's own Tokyo Night");
    assert_eq!(
        (own.ui.composer, own.ui.raised),
        (
            own.palette.lighter_background,
            own.palette.lighter_background
        ),
        "Pronto's own Tokyo Night (the review's): the pickers rise to the card's #24283b"
    );
}

/// The mock's arithmetic: `Math.round(x + (y - x) * t)` per channel.
fn mock_mix(from: Rgb, toward: Rgb, amount: f64) -> Rgb {
    let channel =
        |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * amount).round() as u8;
    Rgb {
        r: channel(from.r, toward.r),
        g: channel(from.g, toward.g),
        b: channel(from.b, toward.b),
    }
}

#[test]
fn borders_follow_the_mock() {
    for (name, theme) in all_themes() {
        let p = &theme.palette;
        let percent = match p.mode {
            Mode::Dark => BORDER_MIX[0],
            Mode::Light => BORDER_MIX[1],
        };
        let expected = mock_mix(
            p.lighter_background,
            p.foreground,
            f64::from(percent) / 100.0,
        );
        assert_eq!(theme.ui.border, expected, "{name}");
        assert!(
            theme
                .css()
                .contains(&format!("--border-color: {expected};")),
            "{name}"
        );
    }
}

#[test]
fn the_same_sender_keeps_its_tint_across_themes() {
    let address = "ingrid@fjordpost.example";
    let index = avatar_index(address);
    for name in theme_names() {
        let theme = theme(&name);
        assert_eq!(theme.avatar(address), theme.mail.avatars[index], "{name}");
    }
}

#[test]
fn every_theme_yields_a_complete_stylesheet() {
    for name in theme_names() {
        let theme = theme(&name);
        let css = theme.css();
        assert!(css.contains("--accent-bg-color: #"), "{name}");
        assert!(css.contains("--sw-avatar-6-fg-color: #"), "{name}");
        assert!(css.contains("--window-radius: 8px;"), "{name}");
        for style in [BodyStyle::Themed, BodyStyle::Paper] {
            assert!(theme.webview_css(style).starts_with(MEASURE_CSS), "{name}");
        }
        let themed = theme.webview_css(BodyStyle::Themed);
        let quote = format!(
            "border-left:3px solid {};color:{}}}",
            theme.ui.border, theme.mail.secondary_text
        );
        assert!(
            themed.contains(&quote),
            "{name}: quotes take the muted text"
        );
        let rule = format!("border-top:1px solid {}", theme.ui.border);
        assert!(themed.contains(&rule), "{name}: separators take the border");
    }
}

proptest! {
    #[test]
    fn initials_clear_aa_contrast_for_any_accent(r: u8, g: u8, b: u8, dark: bool) {
        let mode = Mode::from_dark(dark);
        for tint in avatar_tints(Rgb { r, g, b }, mode) {
            prop_assert!(tint.background.contrast(tint.foreground) >= INITIALS_CONTRAST);
        }
    }

    #[test]
    fn avatar_index_is_in_range(address in "\\PC{0,40}") {
        prop_assert!(avatar_index(&address) < 7);
        prop_assert_eq!(avatar_index(&address), avatar_index(&address.to_lowercase()));
    }

    #[test]
    fn any_palette_yields_a_complete_theme(
        raw in proptest::collection::btree_map(
            proptest::sample::select(vec![
                "mode", "accent", "background", "foreground", "red", "yellow", "green",
                "blue", "lighter_background", "dark_background", "color0", "color7",
            ]).prop_map(str::to_owned),
            prop_oneof!["#[0-9a-fA-F]{6}", "(light|dark)", "[ -~]{0,8}"],
            0..12,
        )
    ) {
        let inputs = ThemeInputs {
            palette: Palette::from_colors(&raw),
            ..ThemeInputs::builtin(Mode::Dark)
        };
        let theme = Theme::new(&inputs, &ThemeOptions::default());
        let css = theme.css();
        prop_assert!(css.contains("--window-bg-color: #"));
        prop_assert!(css.contains("--sw-secondary-fg-color: #"));
        prop_assert_eq!(css.matches('{').count(), 1);
        prop_assert!(theme.webview_css(BodyStyle::Themed).starts_with(MEASURE_CSS));
    }
}
