//! `config.toml` (ADR-009): Pronto's settings, in `$XDG_CONFIG_HOME/pronto/`. The file is
//! TOML read with positions, so every mistake is reported with its line and column, and a
//! file with mistakes is not applied at all. Missing settings keep their defaults; a missing
//! file is the defaults. `keys.toml` shares the error type and helpers (`crate::keymap`).

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

pub const CONFIG_FILE: &str = "config.toml";
pub const KEYS_FILE: &str = "keys.toml";

/// A mistake in a settings file, at a 1-based line and column (counted in characters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl FileError {
    /// The error at byte `byte` of `text`.
    pub fn at(text: &str, byte: usize, message: impl Into<String>) -> Self {
        let mut byte = byte.min(text.len());
        while !text.is_char_boundary(byte) {
            byte -= 1;
        }
        let line_start = text[..byte].rfind('\n').map_or(0, |at| at + 1);
        Self {
            line: text[..line_start].matches('\n').count() + 1,
            column: text[line_start..byte].chars().count() + 1,
            message: message.into(),
        }
    }

    pub(crate) fn spanned(text: &str, span: Range<usize>, message: impl Into<String>) -> Self {
        Self::at(text, span.start, message)
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for FileError {}

/// Parses TOML with positions; a syntax error becomes a [`FileError`].
pub(crate) fn parse_toml(text: &str) -> Result<Spanned<DeTable<'_>>, FileError> {
    DeTable::parse(text).map_err(|error| {
        let message = error.message().lines().next().unwrap_or("invalid TOML");
        FileError::at(text, error.span().map_or(0, |span| span.start), message)
    })
}

pub(crate) type Entry<'a, 'i> = (&'a Spanned<Cow<'i, str>>, &'a Spanned<DeValue<'i>>);

/// A table's entries in the order the file lists them.
pub(crate) fn entries<'a, 'i>(table: &'a DeTable<'i>) -> Vec<Entry<'a, 'i>> {
    let mut entries: Vec<_> = table.iter().collect();
    entries.sort_by_key(|(key, _)| key.span().start);
    entries
}

/// The candidate closest to `word`, when it is a likely typo of it.
pub(crate) fn closest<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).clamp(1, 3);
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(word, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// `; did you mean …?` for a likely typo, or nothing.
pub(crate) fn did_you_mean<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> String {
    closest(word, candidates)
        .map(|near| format!("; did you mean {near}?"))
        .unwrap_or_default()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(above + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// `a string`, `an integer`.
pub(crate) fn article(kind: &str) -> String {
    match kind.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => format!("an {kind}"),
        _ => format!("a {kind}"),
    }
}

pub use crate::theme::{
    ChromeFont, CornerRadius, MAX_RADIUS as MAX_CORNER_RADIUS, PaletteChoice, ReadingFont,
    ThemeOptions as Appearance,
};

/// An OAuth client registration that overrides the build's (ADR-008, OAUTH-SETUP.md). Microsoft's
/// client is public, so `[oauth.microsoft]` takes only the client ID. `[oauth.google]` also takes
/// the `client_secret` Google issues to a Desktop app client, which Google says isn't
/// confidential (ADR-023); its debug form never shows it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct OAuthClient {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

impl std::fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient")
            .field("client_id", &self.client_id.is_some())
            .field("client_secret", &self.client_secret.is_some())
            .finish()
    }
}

/// `[oauth.microsoft]` and `[oauth.google]` (ADR-023: Sign in with Google, 2026-10-22).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OAuth {
    pub microsoft: OAuthClient,
    pub google: OAuthClient,
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// The disk downloaded messages may use by default (a proposal, ML-044).
pub const DEFAULT_CACHE_SIZE: u64 = 2 * GIB;
/// The smallest `cache_size`: a few large messages must fit.
pub const MIN_CACHE_SIZE: u64 = 100 * MIB;
/// Days of Inbox mail downloaded in the background by default (a proposal, ML-044). Since the
/// fork's Superhuman pass any value but 0 means the whole Inbox (see [`Storage::prefetch_days`]).
pub const DEFAULT_PREFETCH_DAYS: u32 = 14;
pub const MAX_PREFETCH_DAYS: u32 = 365;

/// `[storage]`: how much of an account's mail is kept on this computer (ML-044).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Storage {
    /// Bytes the downloaded messages may take; `None` for no limit. Past it, the messages
    /// opened or fetched longest ago go back to their headers and preview.
    pub cache_size: Option<u64>,
    /// 0 turns the background download off. Since the fork's Superhuman pass any other value
    /// turns it on for the whole Inbox and the rest of its conversations, whatever their age
    /// (upstream: Inbox messages from the last this many days); the key keeps its name and
    /// range so existing files stay valid.
    pub prefetch_days: u32,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            cache_size: Some(DEFAULT_CACHE_SIZE),
            prefetch_days: DEFAULT_PREFETCH_DAYS,
        }
    }
}

/// What a new-mail notification shows (ADR-013 amendment, M7 start).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NotificationContent {
    /// The sender and the subject: the user's choice at M7's start.
    #[default]
    SenderAndSubject,
    /// Only that new mail came, with a count: nothing readable on a shared screen.
    Count,
}

/// `[notifications]`: new-mail notifications (ML-083).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notifications {
    pub enabled: bool,
    pub content: NotificationContent,
    /// Mail the Split Inbox puts in Other notifies too (the fork's phase C: off by default, as
    /// in Superhuman; mail in Important notifies as before, and with the splits off, all of it).
    pub other: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            content: NotificationContent::default(),
            other: false,
        }
    }
}

/// `[inbox]`: the Inbox and All Inboxes (the fork's phase C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxSettings {
    /// The Split Inbox: two tabs, Important (mail from people) and Other (newsletters,
    /// notifications, receipts); false shows one Inbox. Split Inbox in the palette and the View
    /// menu switches it too, which `layout.toml` remembers until this changes.
    pub split: bool,
}

impl Default for InboxSettings {
    fn default() -> Self {
        Self { split: true }
    }
}

/// `[background]`: closing the window keeps Pronto running, so sync and notifications go on
/// (ML-084; on by default, ADR-013 amendment, M7 start). Quit stops it either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Background {
    pub enabled: bool,
}

impl Default for Background {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[reader]`: how the reader shows a conversation (issue #3) and whom Enter's reply answers
/// (issue #8; the fork's phase C).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReaderSettings {
    pub conversation_order: ConversationOrder,
    pub reply: DefaultReply,
}

/// The order of a conversation's messages in the reader.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConversationOrder {
    #[default]
    OldestFirst,
    NewestFirst,
}

/// Whom the reply under a conversation answers when the user doesn't say: Enter in a
/// conversation (Quick Reply), a click in its box. Superhuman's Enter is Reply All, the fork's
/// default since phase C; `r` (Reply) always answers the sender and `a` (Reply All) everyone.
/// (Upstream's issue #8 made "all" turn Reply itself into Reply All.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DefaultReply {
    /// The sender (or Reply-To) alone, as Reply does.
    Sender,
    /// Everyone the message went to, as Reply All does (a message to the user alone is
    /// answered as Reply does).
    #[default]
    All,
}

/// How long a sent message waits so Undo can take it back by default, in seconds: Superhuman's
/// ten (the fork's phase B; upstream waited 7).
pub const DEFAULT_UNDO_SEND_SECONDS: u32 = 10;
/// The longest undo window `undo_send_seconds` takes, as Gmail's.
pub const MAX_UNDO_SEND_SECONDS: u32 = 30;

/// `[compose]`: writing and sending (the fork's phase B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComposeSettings {
    /// Seconds a sent message waits before it goes, while Undo can take it back; 0 sends at
    /// once, with no Undo.
    pub undo_send_seconds: u32,
}

impl Default for ComposeSettings {
    fn default() -> Self {
        Self {
            undo_send_seconds: DEFAULT_UNDO_SEND_SECONDS,
        }
    }
}

/// The settings in `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub appearance: Appearance,
    pub oauth: OAuth,
    pub storage: Storage,
    pub notifications: Notifications,
    pub background: Background,
    pub reader: ReaderSettings,
    pub compose: ComposeSettings,
    pub inbox: InboxSettings,
}

const APPEARANCE_SETTINGS: [&str; 4] = ["palette", "corner_radius", "chrome_font", "reading_font"];
const STORAGE_SETTINGS: [&str; 2] = ["cache_size", "prefetch_days"];
const NOTIFICATION_SETTINGS: [&str; 3] = ["enabled", "show", "other"];
const BACKGROUND_SETTINGS: [&str; 1] = ["enabled"];
const READER_SETTINGS: [&str; 2] = ["conversation_order", "reply"];
const COMPOSE_SETTINGS: [&str; 1] = ["undo_send_seconds"];
const INBOX_SETTINGS: [&str; 1] = ["split"];
const SECTIONS: [&str; 8] = [
    "appearance",
    "oauth",
    "storage",
    "notifications",
    "background",
    "reader",
    "compose",
    "inbox",
];
const PROVIDERS: [&str; 2] = ["microsoft", "google"];

/// Where a setting written at the top level belongs.
fn section_of(name: &str) -> Option<&'static str> {
    if APPEARANCE_SETTINGS.contains(&name) {
        Some("[appearance]")
    } else if name == "client_id" {
        Some("[oauth.microsoft] or [oauth.google]")
    } else if name == "client_secret" {
        Some("[oauth.google]")
    } else if STORAGE_SETTINGS.contains(&name) {
        Some("[storage]")
    } else if name == "enabled" {
        Some("[notifications] or [background]")
    } else if NOTIFICATION_SETTINGS.contains(&name) {
        Some("[notifications]")
    } else if READER_SETTINGS.contains(&name) {
        Some("[reader]")
    } else if COMPOSE_SETTINGS.contains(&name) {
        Some("[compose]")
    } else if INBOX_SETTINGS.contains(&name) {
        Some("[inbox]")
    } else {
        None
    }
}

/// A size such as `2 GiB`, `500 MB` or `1.5GB`: decimal units are powers of 1000, binary ones
/// of 1024.
fn size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let scale: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "b" => 1,
        "kb" => 1000,
        "mb" => 1000 * 1000,
        "gb" => 1000 * 1000 * 1000,
        "tb" => 1000 * 1000 * 1000 * 1000,
        "kib" => 1024,
        "mib" => MIB,
        "gib" => GIB,
        "tib" => 1024 * GIB,
        _ => return None,
    };
    let bytes = number * scale as f64;
    (bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64).then_some(bytes as u64)
}

/// Collects the mistakes of one `config.toml` read.
struct Reader<'t> {
    text: &'t str,
    errors: Vec<FileError>,
}

impl Reader<'_> {
    fn error(&mut self, span: Range<usize>, message: impl Into<String>) {
        self.errors
            .push(FileError::spanned(self.text, span, message));
    }

    /// The table in `value`, or a mistake that says what `name` should look like.
    fn table<'a, 'i>(
        &mut self,
        value: &'a Spanned<DeValue<'i>>,
        expected: &str,
    ) -> Option<&'a DeTable<'i>> {
        let table = value.get_ref().as_table();
        if table.is_none() {
            self.error(
                value.span(),
                format!("{expected} is a table, like [{expected}]"),
            );
        }
        table
    }

    fn appearance(&mut self, table: &DeTable<'_>, appearance: &mut Appearance) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            let span = value.span();
            match name {
                "palette" => match value.get_ref().as_str() {
                    Some("adwaita") => appearance.palette = PaletteChoice::Adwaita,
                    Some("tokyo-night") => appearance.palette = PaletteChoice::TokyoNight,
                    Some("omarchy") => appearance.palette = PaletteChoice::Omarchy,
                    _ => self.error(
                        span,
                        format!(
                            "palette is \"adwaita\", \"tokyo-night\" or \"omarchy\", not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                "corner_radius" => match corner_radius(value.get_ref()) {
                    Some(radius) => appearance.corner_radius = radius,
                    None => self.error(
                        span,
                        format!(
                            "corner_radius is \"auto\" or a whole number of pixels from 0 to \
                             {MAX_CORNER_RADIUS}, not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                "chrome_font" => match value.get_ref().as_str() {
                    Some("adwaita") => appearance.chrome_font = ChromeFont::Adwaita,
                    Some("omarchy") => appearance.chrome_font = ChromeFont::Omarchy,
                    Some("sans") => appearance.chrome_font = ChromeFont::Sans,
                    _ => self.error(
                        span,
                        format!(
                            "chrome_font is \"adwaita\", \"omarchy\" or \"sans\", not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                "reading_font" => match value.get_ref().as_str() {
                    Some("adwaita") => appearance.reading_font = ReadingFont::Adwaita,
                    Some("sans") => appearance.reading_font = ReadingFont::Sans,
                    Some("mono") => appearance.reading_font = ReadingFont::Mono,
                    _ => self.error(
                        span,
                        format!(
                            "reading_font is \"adwaita\", \"sans\" or \"mono\", not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [appearance]{}",
                        did_you_mean(name, APPEARANCE_SETTINGS)
                    ),
                ),
            }
        }
    }

    fn oauth(&mut self, table: &DeTable<'_>, oauth: &mut OAuth) {
        for (key, value) in entries(table) {
            let provider = key.get_ref().as_ref();
            let (client, settings): (&mut OAuthClient, &[&str]) = match provider {
                "microsoft" => (&mut oauth.microsoft, &["client_id"]),
                "google" => (&mut oauth.google, &["client_id", "client_secret"]),
                _ => {
                    self.error(
                        key.span(),
                        format!(
                            "unknown provider oauth.{provider}{}; Pronto signs in with \
                             microsoft or google",
                            did_you_mean(provider, PROVIDERS)
                        ),
                    );
                    continue;
                }
            };
            let section = format!("oauth.{provider}");
            let Some(table) = self.table(value, &section) else {
                continue;
            };
            for (key, value) in entries(table) {
                let name = key.get_ref().as_ref();
                if !settings.contains(&name) {
                    let hint = if name == "client_secret" {
                        "; Microsoft's client is public and has none, and no settings file \
                         holds a secret"
                            .to_owned()
                    } else {
                        did_you_mean(name, settings.iter().copied())
                    };
                    self.error(
                        key.span(),
                        format!("unknown setting {name} in [{section}]{hint}"),
                    );
                    continue;
                }
                let Some(text) = value.get_ref().as_str() else {
                    self.error(
                        value.span(),
                        format!(
                            "{name} takes a string in quotes, not {}",
                            article(value.get_ref().type_str())
                        ),
                    );
                    continue;
                };
                let text = Some(text.trim().to_owned()).filter(|text| !text.is_empty());
                match name {
                    "client_secret" => client.client_secret = text,
                    _ => client.client_id = text,
                }
            }
        }
    }

    fn storage(&mut self, table: &DeTable<'_>, storage: &mut Storage) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            let span = value.span();
            match name {
                "cache_size" => match value.get_ref() {
                    DeValue::String(text) if text.trim().eq_ignore_ascii_case("unlimited") => {
                        storage.cache_size = None;
                    }
                    DeValue::String(text) => match size(text) {
                        Some(bytes) if bytes >= MIN_CACHE_SIZE => {
                            storage.cache_size = Some(bytes);
                        }
                        Some(_) => self.error(
                            span,
                            format!(
                                "cache_size is at least 100 MiB, not {}",
                                shown(value.get_ref())
                            ),
                        ),
                        None => self.error(
                            span,
                            format!(
                                "cache_size is a size such as \"2 GiB\" or \"500 MB\", or \
                                 \"unlimited\", not {}",
                                shown(value.get_ref())
                            ),
                        ),
                    },
                    other => self.error(
                        span,
                        format!(
                            "cache_size is a size in quotes such as \"2 GiB\", not {}",
                            shown(other)
                        ),
                    ),
                },
                "prefetch_days" => match whole_number(value.get_ref())
                    .and_then(|days| u32::try_from(days).ok())
                    .filter(|days| *days <= MAX_PREFETCH_DAYS)
                {
                    Some(days) => storage.prefetch_days = days,
                    None => self.error(
                        span,
                        format!(
                            "prefetch_days is a whole number of days from 0 to \
                             {MAX_PREFETCH_DAYS}, not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [storage]{}",
                        did_you_mean(name, STORAGE_SETTINGS)
                    ),
                ),
            }
        }
    }
}

impl Reader<'_> {
    /// `true` or `false`, or a mistake.
    fn boolean(&mut self, name: &str, value: &Spanned<DeValue<'_>>) -> Option<bool> {
        match value.get_ref() {
            DeValue::Boolean(on) => Some(*on),
            other => {
                self.error(
                    value.span(),
                    format!("{name} is true or false, not {}", shown(other)),
                );
                None
            }
        }
    }

    fn notifications(&mut self, table: &DeTable<'_>, notifications: &mut Notifications) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            match name {
                "enabled" => {
                    if let Some(on) = self.boolean(name, value) {
                        notifications.enabled = on;
                    }
                }
                "show" => match value.get_ref().as_str() {
                    Some("sender-and-subject") => {
                        notifications.content = NotificationContent::SenderAndSubject;
                    }
                    Some("count") => notifications.content = NotificationContent::Count,
                    _ => self.error(
                        value.span(),
                        format!(
                            "show is \"sender-and-subject\" or \"count\", not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                "other" => {
                    if let Some(on) = self.boolean(name, value) {
                        notifications.other = on;
                    }
                }
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [notifications]{}",
                        did_you_mean(name, NOTIFICATION_SETTINGS)
                    ),
                ),
            }
        }
    }

    fn background(&mut self, table: &DeTable<'_>, background: &mut Background) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            match name {
                "enabled" => {
                    if let Some(on) = self.boolean(name, value) {
                        background.enabled = on;
                    }
                }
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [background]{}",
                        did_you_mean(name, BACKGROUND_SETTINGS)
                    ),
                ),
            }
        }
    }

    fn reader(&mut self, table: &DeTable<'_>, reader: &mut ReaderSettings) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            match name {
                "conversation_order" => match value.get_ref().as_str() {
                    Some("oldest-first") => {
                        reader.conversation_order = ConversationOrder::OldestFirst;
                    }
                    Some("newest-first") => {
                        reader.conversation_order = ConversationOrder::NewestFirst;
                    }
                    _ => self.error(
                        value.span(),
                        format!(
                            "conversation_order is \"oldest-first\" or \"newest-first\", not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                "reply" => match value.get_ref().as_str() {
                    Some("sender") => reader.reply = DefaultReply::Sender,
                    Some("all") => reader.reply = DefaultReply::All,
                    _ => self.error(
                        value.span(),
                        format!(
                            "reply is \"sender\" or \"all\", not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [reader]{}",
                        did_you_mean(name, READER_SETTINGS)
                    ),
                ),
            }
        }
    }

    fn inbox(&mut self, table: &DeTable<'_>, inbox: &mut InboxSettings) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            match name {
                "split" => {
                    if let Some(on) = self.boolean(name, value) {
                        inbox.split = on;
                    }
                }
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [inbox]{}",
                        did_you_mean(name, INBOX_SETTINGS)
                    ),
                ),
            }
        }
    }

    fn compose(&mut self, table: &DeTable<'_>, compose: &mut ComposeSettings) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            match name {
                "undo_send_seconds" => match whole_number(value.get_ref())
                    .and_then(|seconds| u32::try_from(seconds).ok())
                    .filter(|seconds| *seconds <= MAX_UNDO_SEND_SECONDS)
                {
                    Some(seconds) => compose.undo_send_seconds = seconds,
                    None => self.error(
                        value.span(),
                        format!(
                            "undo_send_seconds is a whole number of seconds from 0 to \
                             {MAX_UNDO_SEND_SECONDS}, not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [compose]{}",
                        did_you_mean(name, COMPOSE_SETTINGS)
                    ),
                ),
            }
        }
    }
}

fn whole_number(value: &DeValue<'_>) -> Option<i64> {
    match value {
        DeValue::Integer(integer) => i64::from_str_radix(integer.as_str(), integer.radix()).ok(),
        _ => None,
    }
}

fn corner_radius(value: &DeValue<'_>) -> Option<CornerRadius> {
    match value {
        DeValue::String(text) if text == "auto" => Some(CornerRadius::Auto),
        DeValue::Integer(integer) => i64::from_str_radix(integer.as_str(), integer.radix())
            .ok()
            .and_then(|pixels| u32::try_from(pixels).ok())
            .filter(|pixels| *pixels <= MAX_CORNER_RADIUS)
            .map(CornerRadius::Px),
        _ => None,
    }
}

/// A value as a mistake quotes it: a short string in quotes, or its type.
fn shown(value: &DeValue<'_>) -> String {
    match value {
        DeValue::String(text) if text.chars().count() <= 24 => format!("\"{text}\""),
        DeValue::Integer(integer) => integer.as_str().to_owned(),
        other => article(other.type_str()),
    }
}

impl Config {
    /// Reads `config.toml`. Every mistake is listed, in the order of the file.
    pub fn parse(text: &str) -> Result<Self, Vec<FileError>> {
        let root = parse_toml(text).map_err(|error| vec![error])?;
        let mut config = Self::default();
        let mut reader = Reader {
            text,
            errors: Vec::new(),
        };
        for (key, value) in entries(root.get_ref()) {
            let name = key.get_ref().as_ref();
            match name {
                "appearance" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.appearance(table, &mut config.appearance);
                    }
                }
                "oauth" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.oauth(table, &mut config.oauth);
                    }
                }
                "storage" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.storage(table, &mut config.storage);
                    }
                }
                "notifications" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.notifications(table, &mut config.notifications);
                    }
                }
                "background" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.background(table, &mut config.background);
                    }
                }
                "reader" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.reader(table, &mut config.reader);
                    }
                }
                "compose" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.compose(table, &mut config.compose);
                    }
                }
                "inbox" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.inbox(table, &mut config.inbox);
                    }
                }
                _ => match section_of(name) {
                    Some(section) => reader.error(
                        key.span(),
                        format!("{name} belongs in {section}; put it under that line"),
                    ),
                    None => {
                        let kind = if value.get_ref().as_table().is_some() {
                            "section"
                        } else {
                            "setting"
                        };
                        reader.error(
                            key.span(),
                            format!("unknown {kind} {name}{}", did_you_mean(name, SECTIONS)),
                        );
                    }
                },
            }
        }
        if reader.errors.is_empty() {
            Ok(config)
        } else {
            Err(reader.errors)
        }
    }
}

/// The `config.toml` that Preferences creates: every setting commented out with its default,
/// so the file changes nothing until a line is uncommented.
pub fn default_config_file() -> String {
    format!(
        "# Pronto settings (config.toml). Uncomment a line and change it; Pronto applies the\n\
         # file when you save it. A mistake is shown with its line and column, and the previous\n\
         # settings stay until the file is fixed.\n\
         \n\
         [appearance]\n\
         # Colours: \"adwaita\" (GNOME's look, light or dark as the desktop prefers),\n\
         # \"tokyo-night\", or \"omarchy\" to follow the live Omarchy theme.\n\
         # palette = \"adwaita\"\n\
         # Corners of rows, chips, cards and buttons: \"auto\" follows Hyprland's rounding (0\n\
         # outside Hyprland), or a number of pixels from 0 to {MAX_CORNER_RADIUS}.\n\
         # corner_radius = \"auto\"\n\
         # The sidebar, list and toolbars: \"adwaita\" (Adwaita Sans), \"omarchy\" (the\n\
         # Omarchy font) or \"sans\" (the desktop's sans-serif).\n\
         # chrome_font = \"adwaita\"\n\
         # Message text: \"adwaita\" (Adwaita Sans), \"sans\" or \"mono\".\n\
         # reading_font = \"adwaita\"\n\
         \n\
         # Your own OAuth client for Sign in with Microsoft, in place of the one Pronto was\n\
         # built with (docs/OAUTH-SETUP.md).\n\
         [oauth.microsoft]\n\
         # client_id = \"00000000-0000-0000-0000-000000000000\"\n\
         \n\
         # Your OAuth client for Sign in with Google (Gmail): a Desktop app client from the\n\
         # Google Cloud console (docs/GOOGLE-OAUTH-SETUP.md). Without one, Gmail uses an app\n\
         # password.\n\
         [oauth.google]\n\
         # client_id = \"0000000000-xxxxxxxx.apps.googleusercontent.com\"\n\
         # client_secret = \"GOCSPX-xxxxxxxx\"\n\
         \n\
         [storage]\n\
         # Disk space for messages downloaded from your accounts: a size such as \"2 GiB\" or\n\
         # \"500 MB\" (at least 100 MiB), or \"unlimited\". When it is full, the messages opened\n\
         # longest ago keep only their headers and preview, and download again when opened.\n\
         # cache_size = \"2 GiB\"\n\
         # Inbox mail and the rest of its conversations download in the background,\n\
         # whatever their age, so they open without waiting; 0 turns it off (any other\n\
         # number, up to 365, turns it on).\n\
         # prefetch_days = {DEFAULT_PREFETCH_DAYS}\n\
         \n\
         [notifications]\n\
         # A notification when new mail comes to an inbox: true or false.\n\
         # enabled = true\n\
         # What it shows: \"sender-and-subject\", or \"count\" for only how many new\n\
         # messages came, so nothing can be read on a shared screen.\n\
         # show = \"sender-and-subject\"\n\
         # Mail the Split Inbox puts in Other (newsletters, notifications, receipts) comes\n\
         # quietly; true notifies for it too.\n\
         # other = false\n\
         \n\
         [background]\n\
         # Closing the window keeps Pronto running, so mail still syncs and notifications\n\
         # still come; Quit (Ctrl+Q) stops it. false makes closing the window quit.\n\
         # enabled = true\n\
         \n\
         [reader]\n\
         # The order of a conversation's messages: \"oldest-first\", or \"newest-first\" to show\n\
         # the latest reply at the top.\n\
         # conversation_order = \"oldest-first\"\n\
         # Whom Enter in a conversation (and a click in its reply box) answers: \"all\",\n\
         # everyone the message went to, as Reply All (a) does, or \"sender\", as Reply (r)\n\
         # does.\n\
         # reply = \"all\"\n\
         \n\
         [compose]\n\
         # Seconds a message you send waits before it goes, while Undo (z, Ctrl+Z, the toast)\n\
         # can take it back: 0 sends at once, at most {MAX_UNDO_SEND_SECONDS}.\n\
         # undo_send_seconds = {DEFAULT_UNDO_SEND_SECONDS}\n\
         \n\
         [inbox]\n\
         # The Split Inbox: the Inbox (and All Inboxes) on two tabs, Important for mail from\n\
         # people and Other for newsletters, notifications and receipts; Tab switches them.\n\
         # false shows one Inbox. Split Inbox in the command palette switches it too.\n\
         # split = true\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(text: &str) -> Vec<String> {
        Config::parse(text)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect()
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(Config::parse(""), Ok(Config::default()));
        assert_eq!(Config::parse("# only comments\n"), Ok(Config::default()));
        let defaults = Config::default();
        assert_eq!(defaults.appearance.corner_radius, CornerRadius::Auto);
        assert_eq!(defaults.appearance.palette, PaletteChoice::Adwaita);
        assert_eq!(defaults.appearance.chrome_font, ChromeFont::Adwaita);
        assert_eq!(defaults.appearance.reading_font, ReadingFont::Adwaita);
        assert_eq!(defaults.oauth, OAuth::default());
    }

    #[test]
    fn reads_every_setting() {
        let text = "[appearance]\npalette = \"tokyo-night\"\ncorner_radius = 8\n\
                    chrome_font = \"sans\"\nreading_font = \"mono\"\n\n[oauth.microsoft]\n\
                    client_id = \" id.example \"\n";
        let config = Config::parse(text).unwrap();
        assert_eq!(
            config.appearance,
            Appearance {
                palette: PaletteChoice::TokyoNight,
                corner_radius: CornerRadius::Px(8),
                chrome_font: ChromeFont::Sans,
                reading_font: ReadingFont::Mono,
            }
        );
        assert_eq!(
            config.oauth.microsoft.client_id.as_deref(),
            Some("id.example")
        );
        let config = Config::parse("[oauth.microsoft]\nclient_id = \"\"\n").unwrap();
        assert_eq!(config.oauth.microsoft, OAuthClient::default());
        let config = Config::parse("appearance.corner_radius = \"auto\"\n").unwrap();
        assert_eq!(config.appearance.corner_radius, CornerRadius::Auto);
        let config = Config::parse("[appearance]\ncorner_radius = 0x40\n").unwrap();
        assert_eq!(config.appearance.corner_radius, CornerRadius::Px(64));
    }

    #[test]
    fn mistakes_say_where_they_are() {
        assert_eq!(
            errors("[appearance]\ncorner_radius = 65\n"),
            [
                "line 2, column 17: corner_radius is \"auto\" or a whole number of pixels from 0 \
                 to 64, not 65"
            ]
        );
        assert_eq!(
            errors("[appearance]\ncorner_radius = \"round\"\n"),
            [
                "line 2, column 17: corner_radius is \"auto\" or a whole number of pixels from 0 \
                 to 64, not \"round\""
            ]
        );
        assert_eq!(
            errors("[appearance]\ncorner_radius = -1\n"),
            [
                "line 2, column 17: corner_radius is \"auto\" or a whole number of pixels from 0 \
                 to 64, not -1"
            ]
        );
        assert_eq!(
            errors("[appearance]\nchrome_font = \"serif\"\n"),
            [
                "line 2, column 15: chrome_font is \"adwaita\", \"omarchy\" or \"sans\", not \
                 \"serif\""
            ]
        );
        assert_eq!(
            errors("[appearance]\nreading_font = true\n"),
            [
                "line 2, column 16: reading_font is \"adwaita\", \"sans\" or \"mono\", not a \
                 boolean"
            ]
        );
        assert_eq!(
            errors("[appearance]\npalette = \"nord\"\n"),
            [
                "line 2, column 11: palette is \"adwaita\", \"tokyo-night\" or \"omarchy\", not \
                 \"nord\""
            ]
        );
        assert_eq!(
            errors("[appearance]\ncorner_radios = 4\n"),
            [
                "line 2, column 1: unknown setting corner_radios in [appearance]; did you mean \
                 corner_radius?"
            ]
        );
        assert_eq!(
            errors("[apperance]\n"),
            ["line 1, column 2: unknown section apperance; did you mean appearance?"]
        );
        assert_eq!(
            errors("corner_radius = 4\n"),
            ["line 1, column 1: corner_radius belongs in [appearance]; put it under that line"]
        );
        assert_eq!(
            errors("appearance = 3\n"),
            ["line 1, column 14: appearance is a table, like [appearance]"]
        );
        assert_eq!(
            errors("[oauth.yahoo]\n"),
            [
                "line 1, column 8: unknown provider oauth.yahoo; Pronto signs in with microsoft \
                 or google"
            ]
        );
        assert_eq!(
            errors("[oauth.microsoft]\nclient_secret = \"x\"\n"),
            [
                "line 2, column 1: unknown setting client_secret in [oauth.microsoft]; \
                 Microsoft's client is public and has none, and no settings file holds a secret"
            ]
        );
        assert_eq!(
            errors("[oauth.microsoft]\nclient_id = 7\n"),
            ["line 2, column 13: client_id takes a string in quotes, not an integer"]
        );
    }

    #[test]
    fn every_mistake_is_listed_in_file_order() {
        let found = errors("zzz = 1\n[appearance]\ncorner_radius = 900\nchrome_font = \"x\"\n");
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("line 1, column 1: unknown setting zzz"));
        assert!(found[1].starts_with("line 3,"));
        assert!(found[2].starts_with("line 4,"));
    }

    #[test]
    fn syntax_errors_have_positions() {
        let found = errors("[appearance]\ncorner_radius = \n");
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("line 2, column 17:"), "{found:?}");
        let found = errors("[appearance]\nchrome_font = \"unterminated\n");
        assert!(found[0].starts_with("line 2,"), "{found:?}");
        let found = errors("[appearance]\nchrome_font = \"sans\"\nchrome_font = \"sans\"\n");
        assert!(found[0].starts_with("line 3,"), "{found:?}");
    }

    #[test]
    fn positions_count_characters() {
        let error = FileError::at("ø = 1\nab", 8, "x");
        assert_eq!((error.line, error.column), (2, 2));
        let error = FileError::at("ø", 1, "inside a character");
        assert_eq!((error.line, error.column), (1, 1));
    }

    #[test]
    fn the_default_file_changes_nothing_and_every_line_is_valid() {
        let file = default_config_file();
        assert_eq!(Config::parse(&file), Ok(Config::default()));
        let uncommented: String = file
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(setting)
                    if setting
                        .split_once(" = ")
                        .is_some_and(|(key, _)| !key.contains(' ')) =>
                {
                    setting
                }
                _ => line,
            })
            .map(|line| format!("{line}\n"))
            .collect();
        let config = Config::parse(&uncommented).unwrap();
        assert_eq!(config.appearance, Appearance::default());
        assert!(config.oauth.microsoft.client_id.is_some());
        assert!(config.oauth.google.client_id.is_some());
        assert!(config.oauth.google.client_secret.is_some());
    }

    #[test]
    fn google_takes_a_client_id_and_secret_and_never_shows_them() {
        let config = Config::parse(
            "[oauth.google]\nclient_id = \" 123-abc.apps.googleusercontent.com \"\n\
             client_secret = \" GOCSPX-shh \"\n",
        )
        .unwrap();
        assert_eq!(
            config.oauth.google.client_id.as_deref(),
            Some("123-abc.apps.googleusercontent.com")
        );
        assert_eq!(
            config.oauth.google.client_secret.as_deref(),
            Some("GOCSPX-shh")
        );
        assert_eq!(config.oauth.microsoft, OAuthClient::default());
        let shown = format!("{config:?}");
        assert!(!shown.contains("GOCSPX"), "{shown}");
        assert!(!shown.contains("googleusercontent"), "{shown}");
        let config = Config::parse("[oauth.google]\nclient_secret = \"\"\n").unwrap();
        assert_eq!(config.oauth.google, OAuthClient::default());
        assert_eq!(
            errors("client_secret = \"x\"\n"),
            ["line 1, column 1: client_secret belongs in [oauth.google]; put it under that line"]
        );
    }

    #[test]
    fn storage_takes_sizes_with_units_and_days() {
        assert_eq!(Config::default().storage, Storage::default());
        assert_eq!(Storage::default().cache_size, Some(2 * 1024 * 1024 * 1024));
        assert_eq!(Storage::default().prefetch_days, 14);
        let storage = |text: &str| Config::parse(text).unwrap().storage;
        assert_eq!(
            storage("[storage]\ncache_size = \"500 MB\"\nprefetch_days = 0\n"),
            Storage {
                cache_size: Some(500_000_000),
                prefetch_days: 0,
            }
        );
        assert_eq!(
            storage("[storage]\ncache_size = \"1.5GiB\"\n").cache_size,
            Some(1_610_612_736)
        );
        assert_eq!(
            storage("[storage]\ncache_size = \" Unlimited \"\n").cache_size,
            None
        );
        assert_eq!(storage("storage.prefetch_days = 365\n").prefetch_days, 365);
        assert_eq!(
            errors("[storage]\ncache_size = \"50 MiB\"\n"),
            ["line 2, column 14: cache_size is at least 100 MiB, not \"50 MiB\""]
        );
        assert_eq!(
            errors("[storage]\ncache_size = \"lots\"\n"),
            [
                "line 2, column 14: cache_size is a size such as \"2 GiB\" or \"500 MB\", or \
                 \"unlimited\", not \"lots\""
            ]
        );
        assert_eq!(
            errors("[storage]\ncache_size = 2000\n"),
            ["line 2, column 14: cache_size is a size in quotes such as \"2 GiB\", not 2000"]
        );
        assert_eq!(
            errors("[storage]\nprefetch_days = 400\n"),
            ["line 2, column 17: prefetch_days is a whole number of days from 0 to 365, not 400"]
        );
        assert_eq!(
            errors("[storage]\nprefetch_days = -1\n"),
            ["line 2, column 17: prefetch_days is a whole number of days from 0 to 365, not -1"]
        );
        assert_eq!(
            errors("[storage]\nprefech_days = 3\n"),
            [
                "line 2, column 1: unknown setting prefech_days in [storage]; did you mean \
                 prefetch_days?"
            ]
        );
        assert_eq!(
            errors("cache_size = \"1 GiB\"\n"),
            ["line 1, column 1: cache_size belongs in [storage]; put it under that line"]
        );
    }

    #[test]
    fn the_conversation_order() {
        assert_eq!(
            Config::default().reader.conversation_order,
            ConversationOrder::OldestFirst
        );
        let config = Config::parse("[reader]\nconversation_order = \"newest-first\"\n").unwrap();
        assert_eq!(
            config.reader.conversation_order,
            ConversationOrder::NewestFirst
        );
        assert_eq!(
            errors("[reader]\nconversation_order = \"newest\"\n"),
            [
                "line 2, column 22: conversation_order is \"oldest-first\" or \"newest-first\", \
                 not \"newest\""
            ]
        );
        assert_eq!(
            errors("conversation_order = \"newest-first\"\n"),
            ["line 1, column 1: conversation_order belongs in [reader]; put it under that line"]
        );
        assert_eq!(
            errors("[reader]\nconversation_ordr = \"newest-first\"\n"),
            [
                "line 2, column 1: unknown setting conversation_ordr in [reader]; did you mean \
                 conversation_order?"
            ]
        );
    }

    #[test]
    fn whom_reply_answers() {
        // Superhuman's Enter, the fork's default since phase C (it was the sender).
        assert_eq!(Config::default().reader.reply, DefaultReply::All);
        assert_eq!(
            Config::parse("").unwrap().reader.reply,
            DefaultReply::All,
            "no config.toml"
        );
        let config = Config::parse("[reader]\nreply = \"all\"\n").unwrap();
        assert_eq!(config.reader.reply, DefaultReply::All);
        assert_eq!(
            config.reader.conversation_order,
            ConversationOrder::OldestFirst
        );
        let config = Config::parse("[reader]\nreply = \"sender\"\n").unwrap();
        assert_eq!(config.reader.reply, DefaultReply::Sender);
        assert_eq!(
            errors("[reader]\nreply = \"everyone\"\n"),
            ["line 2, column 9: reply is \"sender\" or \"all\", not \"everyone\""]
        );
        assert_eq!(
            errors("reply = \"all\"\n"),
            ["line 1, column 1: reply belongs in [reader]; put it under that line"]
        );
    }

    #[test]
    fn the_undo_window_of_a_send() {
        assert_eq!(Config::default().compose.undo_send_seconds, 10);
        let seconds = |text: &str| Config::parse(text).unwrap().compose.undo_send_seconds;
        assert_eq!(seconds("[compose]\nundo_send_seconds = 0\n"), 0);
        assert_eq!(seconds("[compose]\nundo_send_seconds = 30\n"), 30);
        assert_eq!(
            errors("[compose]\nundo_send_seconds = 31\n"),
            [
                "line 2, column 21: undo_send_seconds is a whole number of seconds from 0 to 30, \
                 not 31"
            ]
        );
        assert_eq!(
            errors("[compose]\nundo_send_seconds = \"10\"\n"),
            [
                "line 2, column 21: undo_send_seconds is a whole number of seconds from 0 to 30, \
                 not \"10\""
            ]
        );
        assert_eq!(
            errors("undo_send_seconds = 5\n"),
            ["line 1, column 1: undo_send_seconds belongs in [compose]; put it under that line"]
        );
        assert_eq!(
            errors("[compose]\nundo_send_second = 5\n"),
            [
                "line 2, column 1: unknown setting undo_send_second in [compose]; did you mean \
                 undo_send_seconds?"
            ]
        );
        let file = default_config_file();
        assert!(file.contains("[compose]\n"), "{file}");
        assert!(file.contains("# undo_send_seconds = 10\n"), "{file}");
    }

    #[test]
    fn notifications_and_background_mode() {
        let defaults = Config::default();
        assert!(defaults.notifications.enabled);
        assert_eq!(
            defaults.notifications.content,
            NotificationContent::SenderAndSubject
        );
        assert!(defaults.background.enabled);
        let config = Config::parse(
            "[notifications]\nenabled = false\nshow = \"count\"\n\n[background]\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(
            config.notifications,
            Notifications {
                enabled: false,
                content: NotificationContent::Count,
                other: false,
            }
        );
        assert_eq!(config.background, Background { enabled: false });
        assert_eq!(
            Config::parse("notifications.show = \"sender-and-subject\"\n")
                .unwrap()
                .notifications
                .content,
            NotificationContent::SenderAndSubject
        );
        assert_eq!(
            errors("[notifications]\nshow = \"all\"\n"),
            ["line 2, column 8: show is \"sender-and-subject\" or \"count\", not \"all\""]
        );
        assert_eq!(
            errors("[background]\nenabled = \"yes\"\n"),
            ["line 2, column 11: enabled is true or false, not \"yes\""]
        );
        assert_eq!(
            errors("[background]\nenable = true\n"),
            [
                "line 2, column 1: unknown setting enable in [background]; did you mean \
                 enabled?"
            ]
        );
        assert_eq!(
            errors("[notifications]\nshw = \"count\"\n"),
            ["line 2, column 1: unknown setting shw in [notifications]; did you mean show?"]
        );
        assert_eq!(
            errors("enabled = true\n"),
            [
                "line 1, column 1: enabled belongs in [notifications] or [background]; put it \
                 under that line"
            ]
        );
        assert_eq!(
            errors("[notification]\n"),
            ["line 1, column 2: unknown section notification; did you mean notifications?"]
        );
    }

    #[test]
    fn the_split_inbox_and_its_quiet_other_side() {
        let defaults = Config::default();
        assert!(defaults.inbox.split, "on by default");
        assert!(!defaults.notifications.other, "Other is quiet by default");
        let config =
            Config::parse("[inbox]\nsplit = false\n\n[notifications]\nother = true\n").unwrap();
        assert!(!config.inbox.split);
        assert!(config.notifications.other);
        assert!(config.notifications.enabled);
        assert_eq!(
            errors("[inbox]\nsplit = \"yes\"\n"),
            ["line 2, column 9: split is true or false, not \"yes\""]
        );
        assert_eq!(
            errors("[inbox]\nsplits = true\n"),
            ["line 2, column 1: unknown setting splits in [inbox]; did you mean split?"]
        );
        assert_eq!(
            errors("split = false\n"),
            ["line 1, column 1: split belongs in [inbox]; put it under that line"]
        );
        let file = default_config_file();
        assert!(file.contains("[inbox]\n"), "{file}");
        assert!(file.contains("# split = true\n"), "{file}");
        assert!(file.contains("# other = false\n"), "{file}");
    }

    #[test]
    fn sizes_read_decimal_and_binary_units() {
        assert_eq!(size("2 GiB"), Some(2 * GIB));
        assert_eq!(size("2GB"), Some(2_000_000_000));
        assert_eq!(size("100 mib"), Some(100 * MIB));
        assert_eq!(size("0.5 KB"), Some(500));
        assert_eq!(size("12 B"), Some(12));
        assert_eq!(size("1 TiB"), Some(1024 * GIB));
        for bad in ["", "GiB", "2", "2 GiBs", "1e9 B", "-1 MB", "1..2 MB"] {
            assert_eq!(size(bad), None, "{bad}");
        }
    }

    #[test]
    fn typos_get_suggestions() {
        assert_eq!(
            closest("readingfont", APPEARANCE_SETTINGS),
            Some("reading_font")
        );
        assert_eq!(closest("zzz", APPEARANCE_SETTINGS), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
