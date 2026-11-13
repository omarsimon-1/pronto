//! The theme as Pronto draws it (ADR-004, ADR-019; DESIGN.md "Theme tokens", "Typography" and
//! "HTML mail inside the theme"): the palette (Adwaita by default, Tokyo Night, or the live
//! Omarchy theme, as `config.toml` chooses), the mail roles, the
//! fonts and type scale, omarchy-shell's control states and spacing, the corner radius, and
//! the stylesheets made from them. Pure: callers read `colors.toml`, `shell.toml`,
//! fontconfig and Hyprland and pass the values in.

mod color;
mod css;
mod palette;
mod roles;
mod shell;
mod typography;
mod webview;

pub use color::{Rgb, distinct, distinct_pairs, hue_distance, readable_on};
pub use css::{COMPONENT_SIZES, MOTION_MS, avatar_class};
pub use palette::{Mode, Palette, resolve_colors};
pub use roles::{
    ACCOUNT_CHROMA_DARK, ACCOUNT_CHROMA_LIGHT, ACCOUNT_LIGHTNESS_DARK, ACCOUNT_LIGHTNESS_LIGHT,
    AVATAR_HUE_STEP, AVATAR_LIGHTNESS_DARK, AVATAR_LIGHTNESS_LIGHT, AVATAR_SATURATION,
    AVATAR_TINTS, AvatarTint, BORDER_MIX, INITIALS_DARK, INITIALS_LIGHT, LINK_MIX_MAX,
    MARK_CONTRAST, MARK_MIX_MAX, MailRoles, PRIMARY_CONTRAST, PRIMARY_DEEPEN_MAX, READ_MIX,
    ROW_HOVER_MAX_SHARE, ROW_HOVER_MIN_STEP, ROW_HOVER_SHARE, ROW_SELECTED_TINT, RowRoles,
    SECONDARY_MIX, STATUS_HUES, STATUS_MIN_GAP, TERTIARY_MIX, TEXT_CONTRAST, UiRoles,
    account_colors, avatar_index, avatar_tints, deepened, initials, link_on, muted_text_on,
    raised_secondary_text, raised_surface, reminder_mark, secondary_text, secondary_text_on,
};
pub use shell::{ControlState, ControlStates, SPACING_TOKENS, ShellValues, Spacing};
pub use typography::{
    ADWAITA_SANS, ChromeFont, DEFAULT_BASE_SIZE, FONT_WEIGHTS, FontFamilies, Fonts, MAX_BASE_SIZE,
    READING_LINE_HEIGHT, READING_SIZE, ReadingFont, TypeScale, css_family,
};
pub use webview::{
    BodyStyle, MARGINS_COLOR, MEASURE_CSS, PAPER, PAPER_TEXT, QUOTE_TOGGLE_FILL, QUOTE_TOGGLE_TEXT,
    READING_MEASURE, WebViewFonts,
};

use std::str::FromStr;

/// The largest corner radius applied, in px.
pub const MAX_RADIUS: u32 = 64;

/// `config.toml`'s `corner_radius` (ADR-019): Hyprland's `decoration:rounding`, or px.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CornerRadius {
    #[default]
    Auto,
    Px(u32),
}

impl CornerRadius {
    /// The radius in px: Hyprland's rounding for `Auto`, square (0) when that is unknown.
    pub fn resolve(self, hyprland: Option<u32>) -> u32 {
        match self {
            Self::Auto => hyprland.unwrap_or(0),
            Self::Px(px) => px,
        }
        .min(MAX_RADIUS)
    }
}

impl FromStr for CornerRadius {
    type Err = String;

    /// `auto`, or a whole number of px with an optional `px`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        text.strip_suffix("px")
            .unwrap_or(text)
            .trim()
            .parse()
            .map(Self::Px)
            .map_err(|_| format!("expected \"auto\" or a number of px, not {text:?}"))
    }
}

/// `config.toml`'s `palette`: where the colours come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PaletteChoice {
    /// libadwaita's own look ([`Palette::DARK`], [`Palette::LIGHT`]), light or dark as the
    /// desktop prefers.
    #[default]
    Adwaita,
    /// [`Palette::TOKYO_NIGHT`], always dark.
    TokyoNight,
    /// The live Omarchy theme, followed as it changes; Adwaita's colours outside Omarchy.
    Omarchy,
}

impl PaletteChoice {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adwaita => "adwaita",
            Self::TokyoNight => "tokyo-night",
            Self::Omarchy => "omarchy",
        }
    }
}

impl FromStr for PaletteChoice {
    type Err = String;

    /// `adwaita`, `tokyo-night` or `omarchy`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().as_str() {
            "adwaita" => Ok(Self::Adwaita),
            "tokyo-night" => Ok(Self::TokyoNight),
            "omarchy" => Ok(Self::Omarchy),
            _ => Err(format!(
                "expected \"adwaita\", \"tokyo-night\" or \"omarchy\", not {text:?}"
            )),
        }
    }
}

/// The appearance choices `config.toml` makes; the app fills them from its settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ThemeOptions {
    pub palette: PaletteChoice,
    pub corner_radius: CornerRadius,
    pub chrome_font: ChromeFont,
    pub reading_font: ReadingFont,
}

/// Where the palette came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThemeSource {
    /// An Omarchy theme, by its `theme.name`.
    Omarchy(String),
    /// Adwaita's palette: chosen, or no Omarchy theme was found.
    Builtin,
    /// The built-in Tokyo Night palette (`palette = "tokyo-night"`).
    TokyoNight,
}

/// Everything read from the system that the theme depends on.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeInputs {
    pub source: ThemeSource,
    pub palette: Palette,
    pub shell: ShellValues,
    pub fonts: FontFamilies,
    /// Hyprland's `decoration:rounding`, when Hyprland answered.
    pub hyprland_rounding: Option<u32>,
}

impl ThemeInputs {
    /// A built-in palette and every other input at its default, for running outside Omarchy.
    pub fn builtin(mode: Mode) -> Self {
        Self {
            source: ThemeSource::Builtin,
            palette: Palette::builtin(mode),
            shell: ShellValues::new(),
            fonts: FontFamilies::default(),
            hyprland_rounding: None,
        }
    }
}

/// The resolved theme: every colour, size and font the app draws with.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub source: ThemeSource,
    pub palette: Palette,
    pub ui: UiRoles,
    pub mail: MailRoles,
    pub controls: ControlStates,
    /// The message list's rows on the sidebar's colour, the list's own beside the reader
    /// (Split): their fills and text.
    pub rows: RowRoles,
    /// The rows on the window's colour, where the list is alone (one pane at a time: Focused,
    /// or a narrow window).
    pub window_rows: RowRoles,
    pub fonts: Fonts,
    pub scale: TypeScale,
    pub spacing: Spacing,
    /// Rows, cards and the compose button.
    pub radius: u32,
    /// Chips, folder rows and icon buttons.
    pub radius_small: u32,
}

impl Theme {
    pub fn new(inputs: &ThemeInputs, options: &ThemeOptions) -> Self {
        let palette = &inputs.palette;
        let scale = TypeScale::from_shell(&inputs.shell);
        let radius = options.corner_radius.resolve(inputs.hyprland_rounding);
        let ui = UiRoles::new(palette);
        let controls = ControlStates::from_shell(&inputs.shell, palette);
        Self {
            source: inputs.source.clone(),
            palette: palette.clone(),
            rows: RowRoles::new(palette, ui.sidebar, ui.error, &controls),
            window_rows: RowRoles::new(palette, palette.background, ui.error, &controls),
            ui,
            mail: MailRoles::new(palette),
            controls,
            fonts: Fonts::new(&inputs.fonts, options.chrome_font, options.reading_font),
            spacing: Spacing::from_shell(&inputs.shell, scale.base),
            scale,
            radius,
            radius_small: (radius * 3 + 2) / 4,
        }
    }

    pub fn mode(&self) -> Mode {
        self.palette.mode
    }

    /// The Omarchy theme's name, `builtin-dark` / `builtin-light` for Adwaita's palette, or
    /// `builtin-tokyo-night`.
    pub fn name(&self) -> &str {
        match &self.source {
            ThemeSource::Omarchy(name) => name,
            ThemeSource::Builtin => match self.mode() {
                Mode::Dark => "builtin-dark",
                Mode::Light => "builtin-light",
            },
            ThemeSource::TokyoNight => "builtin-tokyo-night",
        }
    }

    /// The avatar tint for a sender's address.
    pub fn avatar(&self, address: &str) -> AvatarTint {
        self.mail.avatar(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_radius_follows_hyprland_or_the_setting() {
        assert_eq!(CornerRadius::Auto.resolve(Some(8)), 8);
        assert_eq!(CornerRadius::Auto.resolve(None), 0);
        assert_eq!(CornerRadius::Px(4).resolve(Some(8)), 4);
        assert_eq!(CornerRadius::Px(500).resolve(None), MAX_RADIUS);
        assert_eq!("auto".parse(), Ok(CornerRadius::Auto));
        assert_eq!(" 12px".parse(), Ok(CornerRadius::Px(12)));
        assert_eq!("0".parse(), Ok(CornerRadius::Px(0)));
        assert!("round".parse::<CornerRadius>().is_err());
        assert!("-2".parse::<CornerRadius>().is_err());
    }

    #[test]
    fn palette_choice_parses_its_names() {
        assert_eq!(PaletteChoice::default(), PaletteChoice::Adwaita);
        for choice in [
            PaletteChoice::Adwaita,
            PaletteChoice::TokyoNight,
            PaletteChoice::Omarchy,
        ] {
            assert_eq!(choice.as_str().parse(), Ok(choice));
        }
        assert_eq!(" Tokyo-Night ".parse(), Ok(PaletteChoice::TokyoNight));
        assert!("tokyonight".parse::<PaletteChoice>().is_err());
    }

    #[test]
    fn small_radius_is_three_quarters() {
        let inputs = |rounding| ThemeInputs {
            hyprland_rounding: Some(rounding),
            ..ThemeInputs::builtin(Mode::Dark)
        };
        let radii = |rounding| {
            let theme = Theme::new(&inputs(rounding), &ThemeOptions::default());
            (theme.radius, theme.radius_small)
        };
        assert_eq!(radii(8), (8, 6));
        assert_eq!(radii(0), (0, 0));
        assert_eq!(radii(1), (1, 1));
        assert_eq!(radii(12), (12, 9));
    }

    #[test]
    fn builtin_themes_are_named_by_mode() {
        let dark = Theme::new(&ThemeInputs::builtin(Mode::Dark), &ThemeOptions::default());
        let light = Theme::new(&ThemeInputs::builtin(Mode::Light), &ThemeOptions::default());
        assert_eq!((dark.name(), dark.mode()), ("builtin-dark", Mode::Dark));
        assert_eq!((light.name(), light.mode()), ("builtin-light", Mode::Light));
        let named = ThemeInputs {
            source: ThemeSource::Omarchy("tokyo-night".into()),
            ..ThemeInputs::builtin(Mode::Dark)
        };
        assert_eq!(
            Theme::new(&named, &ThemeOptions::default()).name(),
            "tokyo-night"
        );
        let tokyo = ThemeInputs {
            source: ThemeSource::TokyoNight,
            palette: Palette::TOKYO_NIGHT,
            ..ThemeInputs::builtin(Mode::Dark)
        };
        let theme = Theme::new(&tokyo, &ThemeOptions::default());
        assert_eq!(
            (theme.name(), theme.mode()),
            ("builtin-tokyo-night", Mode::Dark)
        );
    }
}
