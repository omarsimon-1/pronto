//! Snippets (the fork's phase B, Superhuman's): text the user keeps in `snippets.toml` in the
//! config folder and puts into a draft with Use Snippet… (Ctrl+;). Not the list's preview line,
//! which is [`crate::snippet`].
//!
//! ```toml
//! [[snippet]]
//! name = "Thanks"
//! subject = "Thanks"            # optional, like to, cc and bcc
//! cc = ["Kari Nordmann <kari@example.com>"]
//! body = """
//! Hi {first_name},
//!
//! {cursor}
//!
//! {my_first_name}
//! """
//! ```
//!
//! The file is read with positions like `config.toml` (`crate::config`'s helpers): every mistake
//! is reported with its line and column, and a file with mistakes isn't applied. In the body and
//! the subject, `{first_name}`, `{last_name}` and `{name}` are the draft's first recipient,
//! `{my_name}` and `{my_first_name}` the sender, `{date}` today, and `{cursor}` (the body only)
//! where the caret goes; `{{` and `}}` are braces. An unknown variable is a mistake, so a typo
//! never reaches a recipient.

use std::fmt;
use std::ops::Range;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::compose::address;
use crate::config::{FileError, article, closest, did_you_mean, entries, parse_toml};
use crate::model::Address;

pub const SNIPPETS_FILE: &str = "snippets.toml";

/// The variables a snippet may hold, as written between braces.
pub const VARIABLES: [&str; 7] = [
    "first_name",
    "last_name",
    "name",
    "my_name",
    "my_first_name",
    "date",
    "cursor",
];

/// The longest name and body Pronto takes, in characters.
pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_BODY_CHARS: usize = 20_000;

const FIELDS: [&str; 6] = ["name", "body", "subject", "to", "cc", "bcc"];

/// One snippet. `Debug` shows its name only: the rest is the user's text.
#[derive(Clone, PartialEq, Eq)]
pub struct Snippet {
    pub name: String,
    /// The text, its variables as written (`Hi {first_name},`).
    pub body: String,
    /// Fills the draft's subject when that is empty.
    pub subject: Option<String>,
    /// Added to the draft's fields.
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
}

impl fmt::Debug for Snippet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snippet")
            .field("name", &self.name)
            .field("body_len", &self.body.len())
            .field("subject", &self.subject.is_some())
            .field(
                "recipients",
                &(self.to.len() + self.cc.len() + self.bcc.len()),
            )
            .finish()
    }
}

/// What the variables stand for where a snippet goes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vars {
    pub first_name: String,
    pub last_name: String,
    pub name: String,
    pub my_name: String,
    pub my_first_name: String,
    pub date: String,
}

impl Vars {
    /// The variables for a draft to `recipient` (its first in To; `None` before there is one)
    /// sent as `me` (the sender's name), on `date` ([`date_text`]). Names come from the
    /// recipient's display name ("Tomas Berg", "Berg, Tomas"); without one, from an address
    /// like tomas.berg@…; else the name variables are empty and `{name}` is the address.
    pub fn new(recipient: Option<&Address>, me: &str, date: &str) -> Self {
        let (first_name, last_name, name) = match recipient {
            Some(recipient) => {
                let display = clean_name(&recipient.name);
                let (first, last) = if display.is_empty() {
                    names_from_address(&recipient.email)
                } else {
                    split_name(&display)
                };
                let name = if display.is_empty() {
                    recipient.email.trim().to_owned()
                } else {
                    display
                };
                (first, last, name)
            }
            None => Default::default(),
        };
        let me = clean_name(me);
        Self {
            first_name,
            last_name,
            name,
            my_first_name: split_name(&me).0,
            my_name: me,
            date: date.to_owned(),
        }
    }

    fn value(&self, variable: &str) -> &str {
        match variable {
            "first_name" => &self.first_name,
            "last_name" => &self.last_name,
            "name" => &self.name,
            "my_name" => &self.my_name,
            "my_first_name" => &self.my_first_name,
            "date" => &self.date,
            _ => "",
        }
    }
}

/// A display name without quotes, a trailing note in parentheses ("Tomas Berg (Acme)") or
/// runs of white space.
fn clean_name(name: &str) -> String {
    let mut name = name.trim().trim_matches(['"', '\'']).trim().to_owned();
    while name.ends_with(')')
        && let Some(open) = name.rfind('(')
    {
        name.truncate(open);
        name = name.trim_end().to_owned();
    }
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First and last names: "Tomas Berg", "Ada King Lovelace" (the first and last words), "Berg,
/// Tomas"; one word is a first name.
fn split_name(name: &str) -> (String, String) {
    if let Some((last, first)) = name.split_once(',') {
        let first = first.split_whitespace().next().unwrap_or_default();
        return (first.to_owned(), last.trim().to_owned());
    }
    let words: Vec<&str> = name.split_whitespace().collect();
    match words.as_slice() {
        [] => (String::new(), String::new()),
        [one] => ((*one).to_owned(), String::new()),
        [first, .., last] => ((*first).to_owned(), (*last).to_owned()),
    }
}

/// Names from an address like tomas.berg@… or tomas_berg@… (two or three words of letters);
/// nothing from info@… or a single word, which is as likely a role as a name.
fn names_from_address(email: &str) -> (String, String) {
    let local = email.trim().split('@').next().unwrap_or_default();
    let words: Vec<&str> = local.split(['.', '_', '-']).collect();
    let named = (2..=3).contains(&words.len())
        && words
            .iter()
            .all(|word| !word.is_empty() && word.chars().all(char::is_alphabetic));
    if !named {
        return (String::new(), String::new());
    }
    let capital = |word: &str| {
        let mut chars = word.chars();
        chars
            .next()
            .map(|first| {
                first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect::<String>()
            })
            .unwrap_or_default()
    };
    (capital(words[0]), capital(words[words.len() - 1]))
}

/// A snippet's text with its variables filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    /// Where `{cursor}` was, in characters from the start; `None` puts the caret after the text.
    pub cursor: Option<usize>,
}

/// One piece of a snippet's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece<'a> {
    Text(&'a str),
    Variable(&'a str),
}

/// The pieces of `template`, or why it is wrong and where (a byte range inside it).
fn pieces(template: &str) -> Result<Vec<Piece<'_>>, (Range<usize>, String)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let bytes = template.as_bytes();
    while at < bytes.len() {
        match bytes[at] {
            b'{' if bytes.get(at + 1) == Some(&b'{') => {
                out.push(Piece::Text(&template[start..=at]));
                at += 2;
                start = at;
            }
            b'}' if bytes.get(at + 1) == Some(&b'}') => {
                out.push(Piece::Text(&template[start..=at]));
                at += 2;
                start = at;
            }
            b'{' => {
                let close = template[at..].find('}').map(|offset| at + offset);
                let name = close.map(|close| &template[at + 1..close]);
                match (close, name) {
                    (Some(close), Some(name)) if VARIABLES.contains(&name) => {
                        out.push(Piece::Text(&template[start..at]));
                        out.push(Piece::Variable(name));
                        at = close + 1;
                        start = at;
                    }
                    (Some(close), Some(name))
                        if !name.is_empty()
                            && name.len() <= 40
                            && name.chars().all(|c| c.is_alphanumeric() || c == '_') =>
                    {
                        return Err((
                            at..close + 1,
                            format!(
                                "unknown variable {{{name}}}{}; the variables are {}",
                                closest(name, VARIABLES)
                                    .map(|near| format!("; did you mean {{{near}}}?"))
                                    .unwrap_or_default(),
                                variable_list()
                            ),
                        ));
                    }
                    _ => {
                        return Err((
                            at..at + 1,
                            "a { starts a variable, like {first_name}; write {{ for a brace"
                                .to_owned(),
                        ));
                    }
                }
            }
            _ => at += 1,
        }
    }
    out.push(Piece::Text(&template[start..]));
    Ok(out)
}

fn variable_list() -> String {
    let names: Vec<String> = VARIABLES.iter().map(|name| format!("{{{name}}}")).collect();
    names.join(", ")
}

/// Fills in `template`'s variables (a snippet's body or subject) with `vars`. A template that
/// [`parse`] took never has a mistake; if one does, it is kept as written.
///
/// A name that is unknown (an address like info@… or kari@…: one word may as well be a role,
/// and a wrong guess would go to the recipient) leaves no gap before the punctuation or the line
/// end after it, and one blank before more text: "Hi {first_name}," is "Hi," then, never
/// "Hi ,". Alone at the start of a line its punctuation goes with it, and the next word starts
/// with a capital, past a `{cursor}` or blank lines between: "{first_name}, thanks" is
/// "Thanks"; a line left empty goes, as do blank lines left at the start. `{cursor}` stays
/// where the text around it closed up.
pub fn expand(template: &str, vars: &Vars) -> Expanded {
    let Ok(pieces) = pieces(template) else {
        return Expanded {
            text: template.to_owned(),
            cursor: None,
        };
    };
    let mut text = String::with_capacity(template.len() + 32);
    // Where `{cursor}` is, in bytes of `text`.
    let mut cursor: Option<usize> = None;
    // Where the text ended before the blanks in front of an empty variable just filled in.
    let mut gap: Option<usize> = None;
    // A dropped name took its line's start and its punctuation: the next word written starts
    // with a capital (a value filled in is left as it is).
    let mut capital = false;
    let last = pieces.len().saturating_sub(1);
    for (index, piece) in pieces.into_iter().enumerate() {
        match piece {
            // Another variable may follow: the gap waits for it.
            Piece::Text("") if index != last => {}
            Piece::Text(mut part) => {
                if let Some(at) = gap.take() {
                    let closes = part.is_empty()
                        || part.starts_with([',', '.', '!', '?', ';', ':', ')', '\n', '\r']);
                    let line_start = at == 0 || text[..at].ends_with('\n');
                    if closes || line_start {
                        text.truncate(at);
                        cursor = cursor.map(|cursor| cursor.min(at));
                    }
                    if closes && line_start {
                        // "{first_name}, thanks": the punctuation goes too, and a line left
                        // empty.
                        part = part
                            .trim_start_matches([',', '.', '!', '?', ';', ':'])
                            .trim_start_matches([' ', '\t']);
                        part = part
                            .strip_prefix("\r\n")
                            .or_else(|| part.strip_prefix('\n'))
                            .unwrap_or(part);
                        capital = true;
                    } else if line_start || text.len() > at {
                        // Blanks on both sides of it: one stays (none at a line's start).
                        part = part.trim_start_matches([' ', '\t']);
                    }
                }
                if !capital {
                    text.push_str(part);
                    continue;
                }
                // The first word, past the blanks and line ends before it; none here, the next
                // piece's.
                let word = part.trim_start();
                text.push_str(&part[..part.len() - word.len()]);
                if let Some(first) = word.chars().next() {
                    capital = false;
                    if first.is_lowercase() {
                        text.extend(first.to_uppercase());
                        text.push_str(&word[first.len_utf8()..]);
                    } else {
                        text.push_str(word);
                    }
                }
            }
            Piece::Variable("cursor") => {
                cursor.get_or_insert(text.len());
            }
            Piece::Variable(name) => {
                let value = vars.value(name);
                if value.is_empty() {
                    let at = text.trim_end_matches([' ', '\t']).len();
                    gap = Some(gap.map_or(at, |gap| gap.min(at)));
                } else {
                    gap = None;
                    capital = false;
                }
                text.push_str(value);
            }
        }
    }
    // Blank lines a dropped line left at the start.
    let lead = text.len() - text.trim_start_matches(['\n', '\r']).len();
    text.drain(..lead);
    let cursor = cursor.map(|cursor| {
        let at = cursor.saturating_sub(lead).min(text.len());
        text[..at].chars().count()
    });
    Expanded { text, cursor }
}

/// "29 October 2026": `{date}`, as Pronto writes dates in a quote's attribution.
pub fn date_text(year: i32, month: u8, day: u8) -> String {
    format!("{day} {} {year}", crate::when::month_name(month))
}

/// The snippets `query` finds, best first: by name (the palette's matching: from its start, a
/// word's start, inside a word, its letters in order), then those whose body holds the query.
/// An empty query lists them all in the file's order.
pub fn find(query: &str, snippets: &[Snippet]) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..snippets.len()).collect();
    }
    let mut found = crate::palette::rank(query, snippets.iter().map(|s| s.name.as_str()));
    let lower = query.to_lowercase();
    for (index, snippet) in snippets.iter().enumerate() {
        if !found.contains(&index) && snippet.body.to_lowercase().contains(&lower) {
            found.push(index);
        }
    }
    found
}

/// The body on one line, its white space collapsed, at most `chars` characters ("…" after):
/// the picker's preview.
pub fn preview(body: &str, chars: usize) -> String {
    let line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= chars {
        return line;
    }
    let mut cut: String = line.chars().take(chars.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

struct Reader<'t> {
    text: &'t str,
    errors: Vec<FileError>,
}

impl Reader<'_> {
    fn error(&mut self, span: Range<usize>, message: impl Into<String>) {
        self.errors
            .push(FileError::at(self.text, span.start, message));
    }

    fn string(&mut self, name: &str, value: &Spanned<DeValue<'_>>) -> Option<String> {
        match value.get_ref().as_str() {
            Some(text) => Some(text.replace("\r\n", "\n")),
            None => {
                self.error(
                    value.span(),
                    format!(
                        "{name} takes a string in quotes, or in \"\"\" for several lines, not {}",
                        article(value.get_ref().type_str())
                    ),
                );
                None
            }
        }
    }

    /// A template's variables are known; a mistake is reported where it is in the file when
    /// the string is written as is (else at the string's start).
    fn template(&mut self, name: &str, text: &str, value: &Spanned<DeValue<'_>>) -> bool {
        match pieces(text) {
            Ok(pieces) => {
                let cursors = pieces
                    .iter()
                    .filter(|piece| **piece == Piece::Variable("cursor"))
                    .count();
                if cursors > 0 && name != "body" {
                    self.error(
                        value.span(),
                        format!("{name} can't hold {{cursor}}: only the body has a caret"),
                    );
                    return false;
                }
                if cursors > 1 {
                    self.error(
                        value.span(),
                        "the body has {cursor} twice; the caret goes to one place",
                    );
                    return false;
                }
                true
            }
            Err((range, message)) => {
                let written = &self.text[value.span()];
                let at = written
                    .find(&text[range.clone()])
                    .map_or(value.span().start, |offset| value.span().start + offset);
                self.error(at..at + 1, message);
                false
            }
        }
    }

    /// `to`, `cc` or `bcc`: a string of addresses separated by commas, or a list of them.
    fn addresses(&mut self, name: &str, value: &Spanned<DeValue<'_>>) -> Option<Vec<Address>> {
        let mut texts: Vec<(String, Range<usize>)> = Vec::new();
        match value.get_ref() {
            DeValue::String(text) => texts.push((text.to_string(), value.span())),
            DeValue::Array(array) => {
                for item in array.iter() {
                    match item.get_ref().as_str() {
                        Some(text) => texts.push((text.to_owned(), item.span())),
                        None => {
                            self.error(
                                item.span(),
                                format!(
                                    "an address is a string in quotes, not {}",
                                    article(item.get_ref().type_str())
                                ),
                            );
                            return None;
                        }
                    }
                }
            }
            other => {
                self.error(
                    value.span(),
                    format!(
                        "{name} takes addresses in quotes, like {name} = [\"Kari Nordmann \
                         <kari@example.com>\"], not {}",
                        article(other.type_str())
                    ),
                );
                return None;
            }
        }
        let mut out = Vec::new();
        for (text, span) in texts {
            let list = address::parse_list(&text);
            let invalid = list
                .addresses
                .iter()
                .find(|address| !address::is_valid(&address.email));
            if let Some(rest) = list.rest.first() {
                self.error(span, format!("“{rest}” isn't an email address"));
                return None;
            }
            if let Some(invalid) = invalid {
                self.error(
                    span,
                    format!("“{}” isn't an address Pronto can send to", invalid.email),
                );
                return None;
            }
            out.extend(list.addresses);
        }
        Some(address::dedupe(out))
    }

    fn snippet(
        &mut self,
        table: &DeTable<'_>,
        header: Range<usize>,
    ) -> Option<(Snippet, Range<usize>)> {
        let errors_before = self.errors.len();
        let mut snippet = Snippet {
            name: String::new(),
            body: String::new(),
            subject: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
        };
        let (mut named, mut bodied) = (None, false);
        for (key, value) in entries(table) {
            let field = key.get_ref().as_ref();
            match field {
                "name" => {
                    let Some(text) = self.string("name", value) else {
                        continue;
                    };
                    let text = text.trim().to_owned();
                    if text.is_empty() || text.chars().any(char::is_control) {
                        self.error(value.span(), "name is the words the picker finds the snippet by: not empty, on one line");
                    } else if text.chars().count() > MAX_NAME_CHARS {
                        self.error(
                            value.span(),
                            format!("name is at most {MAX_NAME_CHARS} characters"),
                        );
                    } else {
                        snippet.name = text;
                        named = Some(value.span());
                    }
                }
                "body" => {
                    let Some(text) = self.string("body", value) else {
                        continue;
                    };
                    let text = text.trim_start_matches('\n').trim_end().to_owned();
                    if text.chars().count() > MAX_BODY_CHARS {
                        self.error(
                            value.span(),
                            format!("body is at most {MAX_BODY_CHARS} characters"),
                        );
                    } else if text
                        .chars()
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
                    {
                        self.error(value.span(), "body can't hold control characters");
                    } else if self.template("body", &text, value) {
                        snippet.body = text;
                        bodied = true;
                    }
                }
                "subject" => {
                    let Some(text) = self.string("subject", value) else {
                        continue;
                    };
                    let text = text.trim().to_owned();
                    if text.chars().any(char::is_control) {
                        self.error(value.span(), "subject is one line");
                    } else if self.template("subject", &text, value) && !text.is_empty() {
                        snippet.subject = Some(text);
                    }
                }
                "to" | "cc" | "bcc" => {
                    if let Some(addresses) = self.addresses(field, value) {
                        match field {
                            "to" => snippet.to = addresses,
                            "cc" => snippet.cc = addresses,
                            _ => snippet.bcc = addresses,
                        }
                    }
                }
                _ => {
                    self.error(
                        key.span(),
                        format!(
                            "unknown setting {field} in [[snippet]]{}; a snippet has {}",
                            did_you_mean(field, FIELDS),
                            FIELDS.join(", ")
                        ),
                    );
                }
            }
        }
        if !table.contains_key("name") {
            self.error(
                header.clone(),
                "this snippet has no name; add one, like name = \"Thanks\"",
            );
        }
        if !table.contains_key("body") {
            self.error(header.clone(), "this snippet has no body, the text it puts in a draft; add one, like body = \"Thanks!\"");
        } else if bodied && snippet.body.trim().is_empty() {
            self.error(header.clone(), "this snippet's body is empty");
        }
        (self.errors.len() == errors_before && named.is_some())
            .then(|| (snippet, named.unwrap_or(header)))
    }
}

/// Reads `snippets.toml`. Every mistake is listed, in the order of the file; an empty file has
/// no snippets. Two snippets may not share a name (case aside).
pub fn parse(text: &str) -> Result<Vec<Snippet>, Vec<FileError>> {
    let root = parse_toml(text).map_err(|error| vec![error])?;
    let mut reader = Reader {
        text,
        errors: Vec::new(),
    };
    let mut snippets: Vec<(Snippet, Range<usize>)> = Vec::new();
    for (key, value) in entries(root.get_ref()) {
        let name = key.get_ref().as_ref();
        if name != "snippet" {
            reader.error(
                key.span(),
                format!(
                    "unknown setting {name}{}; snippets are [[snippet]] tables, each with a name \
                     and a body",
                    did_you_mean(name, ["snippet"])
                ),
            );
            continue;
        }
        let tables = value.get_ref().as_array().filter(|array| {
            array
                .iter()
                .all(|element| text[element.span()].starts_with("[["))
        });
        let Some(tables) = tables else {
            reader.error(
                value.span(),
                "snippets are written as [[snippet]] tables, one per snippet",
            );
            continue;
        };
        for element in tables.iter() {
            if let Some(table) = element.get_ref().as_table()
                && let Some(snippet) = reader.snippet(table, element.span())
            {
                snippets.push(snippet);
            }
        }
    }
    for (index, (snippet, span)) in snippets.iter().enumerate() {
        if snippets[..index]
            .iter()
            .any(|(earlier, _)| earlier.name.to_lowercase() == snippet.name.to_lowercase())
        {
            reader.error(
                span.clone(),
                format!(
                    "two snippets are named “{}”; give each its own name",
                    snippet.name
                ),
            );
        }
    }
    if reader.errors.is_empty() {
        Ok(snippets.into_iter().map(|(snippet, _)| snippet).collect())
    } else {
        reader
            .errors
            .sort_by_key(|error| (error.line, error.column));
        Err(reader.errors)
    }
}

/// The file Edit Snippets creates when there is none: how it works and two examples, commented
/// out.
pub fn default_snippets_file() -> String {
    "# Pronto snippets (snippets.toml): text you put into a draft with Use Snippet… (Ctrl+;).\n\
     #\n\
     # Each [[snippet]] has a name, which the picker finds it by, and a body. A subject, to, cc\n\
     # and bcc are optional: the subject fills an empty one, and to, cc and bcc add their\n\
     # addresses. In the body and the subject these words in braces are filled in:\n\
     #\n\
     #   {first_name} {last_name} {name}  the first recipient: Tomas, Berg, Tomas Berg\n\
     #   {my_name} {my_first_name}        you, as the draft is sent from\n\
     #   {date}                           today, like 29 October 2026\n\
     #   {cursor}                         where the caret goes (the body only)\n\
     #\n\
     # Write {{ and }} for braces. Pronto applies the file when you save it.\n\
     #\n\
     # [[snippet]]\n\
     # name = \"Thanks\"\n\
     # body = \"\"\"\n\
     # Hi {first_name},\n\
     #\n\
     # Thanks for your message. {cursor}\n\
     #\n\
     # Best,\n\
     # {my_first_name}\n\
     # \"\"\"\n\
     #\n\
     # [[snippet]]\n\
     # name = \"Introduction\"\n\
     # subject = \"Meet Kari\"\n\
     # cc = [\"Kari Nordmann <kari@example.com>\"]\n\
     # body = \"Hi {first_name}, meet Kari, who {cursor}\"\n"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(text: &str) -> Vec<Snippet> {
        parse(text).unwrap_or_else(|errors| {
            panic!(
                "{}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
    }

    fn errors(text: &str) -> Vec<String> {
        parse(text)
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    const THANKS: &str = "[[snippet]]\nname = \"Thanks\"\nbody = \"\"\"\nHi {first_name},\n\n\
                          {cursor}\n\nBest,\n{my_first_name}\n\"\"\"\n";

    #[test]
    fn a_file_of_snippets_reads() {
        assert_eq!(parse("").unwrap(), []);
        assert_eq!(parse("# nothing yet\n").unwrap(), []);
        let text = format!(
            "{THANKS}\n[[snippet]]\nname = \"Intro\"\nsubject = \"Meet {{first_name}}\"\n\
             to = \"Tomas Berg <tomas@example.com>\"\ncc = [\"Kari Nordmann <kari@example.com>\", \
             \"ola@example.org\"]\nbcc = []\nbody = \"Hi {{first_name}}, meet Kari.\"\n"
        );
        let snippets = parse_ok(&text);
        assert_eq!(snippets.len(), 2);
        assert_eq!(snippets[0].name, "Thanks");
        assert_eq!(
            snippets[0].body,
            "Hi {first_name},\n\n{cursor}\n\nBest,\n{my_first_name}"
        );
        assert_eq!(snippets[0].subject, None);
        assert!(snippets[0].to.is_empty());
        assert_eq!(snippets[1].subject.as_deref(), Some("Meet {first_name}"));
        assert_eq!(
            snippets[1].to,
            [Address::new("Tomas Berg", "tomas@example.com")]
        );
        assert_eq!(snippets[1].cc.len(), 2);
        assert_eq!(snippets[1].cc[1], Address::new("", "ola@example.org"));
        assert!(snippets[1].bcc.is_empty());
        assert!(!format!("{:?}", snippets[1]).contains("meet Kari"));
    }

    #[test]
    fn the_default_file_has_no_snippets_and_its_examples_read() {
        let text = default_snippets_file();
        assert_eq!(parse(&text).unwrap(), []);
        // The examples, uncommented: everything from the first "# [[snippet]]" on.
        let uncommented: String = text
            .lines()
            .skip_while(|line| *line != "# [[snippet]]")
            .map(|line| {
                line.strip_prefix("# ")
                    .or_else(|| line.strip_prefix('#'))
                    .unwrap_or(line)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let snippets = parse_ok(&uncommented);
        assert_eq!(snippets.len(), 2, "{uncommented}");
        assert_eq!(snippets[1].cc.len(), 1);
    }

    #[test]
    fn mistakes_are_reported_where_they_are() {
        assert_eq!(
            errors("[[snippet]]\nname = \"Hi\"\nbody = \"Hi {frist_name}\"\n"),
            [
                "line 3, column 12: unknown variable {frist_name}; did you mean {first_name}?; the \
              variables are {first_name}, {last_name}, {name}, {my_name}, {my_first_name}, \
              {date}, {cursor}"
            ]
        );
        assert_eq!(
            errors("[[snippet]]\nname = \"Hi\"\nbody = \"a { b\"\n"),
            ["line 3, column 11: a { starts a variable, like {first_name}; write {{ for a brace"]
        );
        assert_eq!(
            errors("[[snippet]]\nname = \"Hi\"\nbody = \"{cursor} {cursor}\"\n"),
            ["line 3, column 8: the body has {cursor} twice; the caret goes to one place"]
        );
        assert_eq!(
            errors("[[snippet]]\nname = \"Hi\"\nsubject = \"{cursor}\"\nbody = \"x\"\n"),
            ["line 3, column 11: subject can't hold {cursor}: only the body has a caret"]
        );
        assert_eq!(
            errors("[[snippet]]\nbody = \"x\"\n"),
            ["line 1, column 1: this snippet has no name; add one, like name = \"Thanks\""]
        );
        assert_eq!(
            errors("[[snippet]]\nname = \"x\"\n"),
            [
                "line 1, column 1: this snippet has no body, the text it puts in a draft; add one, \
              like body = \"Thanks!\""
            ]
        );
        assert_eq!(
            errors("[[snippet]]\nname = \"x\"\nbody = \"  \"\n"),
            ["line 1, column 1: this snippet's body is empty"]
        );
        assert_eq!(
            errors("[[snippet]]\nname = \"x\"\nbody = \"y\"\nbbc = \"a@example.com\"\n"),
            [
                "line 4, column 1: unknown setting bbc in [[snippet]]; did you mean bcc?; a snippet \
              has name, body, subject, to, cc, bcc"
            ]
        );
        assert_eq!(
            errors("[[snippet]]\nname = \"x\"\nbody = \"y\"\nto = \"not an address\"\n"),
            ["line 4, column 6: “not an address” isn't an email address"]
        );
        assert_eq!(
            errors("[[snippet]]\nname = \"x\"\nbody = 3\n"),
            [
                "line 3, column 8: body takes a string in quotes, or in \"\"\" for several lines, \
              not an integer"
            ]
        );
        assert_eq!(
            errors("snippets = 1\n"),
            [
                "line 1, column 1: unknown setting snippets; did you mean snippet?; snippets are \
              [[snippet]] tables, each with a name and a body"
            ]
        );
        assert_eq!(
            errors("[snippet]\nname = \"x\"\nbody = \"y\"\n"),
            ["line 1, column 1: snippets are written as [[snippet]] tables, one per snippet"]
        );
        assert_eq!(
            errors(&format!(
                "{THANKS}[[snippet]]\nname = \"thanks\"\nbody = \"y\"\n"
            )),
            ["line 12, column 8: two snippets are named “thanks”; give each its own name"]
        );
        assert!(errors("[[snippet\n")[0].starts_with("line 1, column"));
        // Every mistake of a file, in its order.
        assert_eq!(
            errors("[[snippet]]\nname = \"\"\nbody = \"{x}\"\n").len(),
            2
        );
    }

    #[test]
    fn variables_are_filled_in() {
        let tomas = Address::new("Tomas Berg", "tomas@example.com");
        let vars = Vars::new(Some(&tomas), "Ada King Lovelace", "29 October 2026");
        let expanded = expand(
            "Hi {first_name} ({name}, {last_name}),\n\n{cursor}\n\n{my_first_name} \
             {{{my_name}}} {date}",
            &vars,
        );
        assert_eq!(
            expanded.text,
            "Hi Tomas (Tomas Berg, Berg),\n\n\n\nAda {Ada King Lovelace} 29 October 2026"
        );
        assert_eq!(
            expanded.cursor,
            Some("Hi Tomas (Tomas Berg, Berg),\n\n".chars().count())
        );
        assert_eq!(expand("No caret", &vars).cursor, None);
        assert_eq!(expand("æøå {cursor}!", &vars).cursor, Some(4));
        assert_eq!(expand("}} {{ }", &vars).text, "} { }");
    }

    #[test]
    fn an_unknown_name_leaves_no_gap() {
        let info = Address::new("", "info@example.com");
        let vars = Vars::new(Some(&info), "", "29 October 2026");
        let text = |template: &str| expand(template, &vars).text;
        assert_eq!(text("Hi {first_name},\n\nThanks!"), "Hi,\n\nThanks!");
        assert_eq!(text("Dear {first_name} {last_name}."), "Dear.");
        assert_eq!(text("Thanks {first_name}!"), "Thanks!");
        assert_eq!(text("Hi {first_name}\nThanks"), "Hi\nThanks");
        assert_eq!(text("Best,\n{my_first_name}"), "Best,\n");
        assert_eq!(text("Hi {first_name}"), "Hi");
        // Before more text one blank stays; a name that is known keeps its place.
        assert_eq!(text("Hi {first_name} and welcome"), "Hi and welcome");
        assert_eq!(text("Hi {first_name} {name}!"), "Hi info@example.com!");
        assert_eq!(text("To {first_name}{name}:"), "To info@example.com:");
        let kari = Address::new("Kari", "kari@example.com");
        let known = Vars::new(Some(&kari), "", "");
        assert_eq!(
            expand("Hi {first_name} {last_name},", &known).text,
            "Hi Kari,"
        );
        // The caret stays where the text closed up, before the punctuation.
        let at = |template: &str| {
            let expanded = expand(template, &vars);
            (expanded.text, expanded.cursor)
        };
        assert_eq!(at("Hi {cursor}{first_name},"), ("Hi,".to_owned(), Some(2)));
        assert_eq!(
            at("Hi {first_name}{cursor}, ok"),
            ("Hi, ok".to_owned(), Some(2))
        );
        assert_eq!(
            at("Hi {first_name} {cursor}and welcome"),
            ("Hi and welcome".to_owned(), Some(3))
        );
    }

    #[test]
    fn an_unknown_name_alone_at_a_lines_start_takes_its_punctuation() {
        let info = Address::new("", "info@example.com");
        let vars = Vars::new(Some(&info), "", "29 October 2026");
        let at = |template: &str| {
            let expanded = expand(template, &vars);
            (expanded.text, expanded.cursor)
        };
        let text = |template: &str| at(template).0;
        assert_eq!(
            text("{first_name}, thanks for the notes."),
            "Thanks for the notes."
        );
        assert_eq!(text("{first_name}! Great news."), "Great news.");
        assert_eq!(
            text("Hello all,\n{first_name}: see below."),
            "Hello all,\nSee below."
        );
        // A line left empty goes, and the blank lines it leaves at the start.
        assert_eq!(text("{first_name},\n\nThanks!"), "Thanks!");
        assert_eq!(text("{first_name}\nThanks"), "Thanks");
        assert_eq!(text("Hei,\n{first_name},\nTakk"), "Hei,\nTakk");
        assert_eq!(
            at("{first_name},\n\n{cursor}Thanks"),
            ("Thanks".to_owned(), Some(0))
        );
        assert_eq!(
            at("{first_name},\n\nThanks{cursor}"),
            ("Thanks".to_owned(), Some(6))
        );
        // The capital goes past a `{cursor}` and blank lines between (the review's cases).
        assert_eq!(
            at("{first_name}, {cursor}thanks for the notes."),
            ("Thanks for the notes.".to_owned(), Some(0))
        );
        assert_eq!(text("{first_name},\n\nthanks"), "Thanks");
        assert_eq!(
            at("{first_name},\n\n{cursor}thanks"),
            ("Thanks".to_owned(), Some(0))
        );
        assert_eq!(text("{first_name}, {last_name} thanks"), "Thanks");
        assert_eq!(text("Hei,\n{first_name},\n\ntakk"), "Hei,\n\nTakk");
        // A value filled in is left as it is.
        let me = Vars::new(Some(&info), "ola nordmann", "29 October 2026");
        assert_eq!(
            expand("{first_name}, {my_first_name} here", &me).text,
            "ola here"
        );
        // Before more text, no blank is left at the start.
        assert_eq!(text("{first_name} and welcome"), "and welcome");
        // A known name keeps everything.
        let kari = Address::new("Kari", "kari@example.com");
        let known = Vars::new(Some(&kari), "", "");
        assert_eq!(expand("{first_name}, thanks", &known).text, "Kari, thanks");
    }

    #[test]
    fn names_come_from_the_display_name_or_the_address() {
        let vars = |name: &str, email: &str| Vars::new(Some(&Address::new(name, email)), "", "");
        let names = |vars: Vars| (vars.first_name, vars.last_name, vars.name);
        assert_eq!(
            names(vars("Berg, Tomas", "t@example.com")),
            (
                "Tomas".to_owned(),
                "Berg".to_owned(),
                "Berg, Tomas".to_owned()
            )
        );
        assert_eq!(
            names(vars("\"Kari Nordmann (Acme)\"", "k@example.com")),
            (
                "Kari".to_owned(),
                "Nordmann".to_owned(),
                "Kari Nordmann".to_owned()
            )
        );
        assert_eq!(
            names(vars("Grace", "g@example.com")),
            ("Grace".to_owned(), String::new(), "Grace".to_owned())
        );
        assert_eq!(
            names(vars("", "tomas.berg@example.com")),
            (
                "Tomas".to_owned(),
                "Berg".to_owned(),
                "tomas.berg@example.com".to_owned()
            )
        );
        assert_eq!(
            names(vars("", "ola_nordmann@example.org")),
            (
                "Ola".to_owned(),
                "Nordmann".to_owned(),
                "ola_nordmann@example.org".to_owned()
            )
        );
        for role in [
            "info@example.com",
            "no-reply2@example.com",
            "a.b.c.d@example.com",
        ] {
            let found = vars("", role);
            assert_eq!(
                (found.first_name.as_str(), found.last_name.as_str()),
                ("", ""),
                "{role}"
            );
            assert_eq!(found.name, role);
        }
        let nobody = Vars::new(None, "Berg, Tomas", "29 October 2026");
        assert_eq!(nobody.first_name, "");
        assert_eq!(nobody.name, "");
        assert_eq!(nobody.my_first_name, "Tomas");
        assert_eq!(date_text(2026, 10, 29), "29 October 2026");
    }

    #[test]
    fn the_picker_finds_by_name_then_body() {
        let snippet = |name: &str, body: &str| Snippet {
            name: name.to_owned(),
            body: body.to_owned(),
            subject: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
        };
        let snippets = [
            snippet("Thanks", "Hi {first_name}, thanks!"),
            snippet("Meeting notes", "Notes from today's meeting"),
            snippet("Intro", "Meet Kari, who thanks you"),
            snippet("Out of office", "I'm away until Monday"),
        ];
        assert_eq!(find("", &snippets), [0, 1, 2, 3]);
        assert_eq!(find("  ", &snippets), [0, 1, 2, 3]);
        assert_eq!(find("thanks", &snippets)[..2], [0, 2]);
        assert_eq!(find("meet", &snippets), [1, 2]);
        assert_eq!(find("monday", &snippets), [3]);
        assert_eq!(find("ooo", &snippets), [3]);
        assert!(find("zzz", &snippets).is_empty());
        assert_eq!(
            preview("Hi {first_name},\n\n  thanks!", 40),
            "Hi {first_name}, thanks!"
        );
        assert_eq!(preview("one two three four", 9), "one two…");
    }
}
