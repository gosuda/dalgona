//! The fixed command-tail lexer (P04).

use super::{Deserialize, Serialize};

/// Splits a command tail into tokens with the fixed P04 grammar.
///
/// Only ASCII space and tab separate tokens. A single-quoted span keeps
/// every character literally. A double-quoted span groups text, and a
/// backslash inside it escapes the next character. Outside single quotes a
/// backslash escapes the next character, so `\ ` is a literal space. Quoted
/// and unquoted spans next to each other join into one token, and an empty
/// quoted span is an empty token. Every other character, including
/// non-ASCII whitespace and line breaks, is token text.
///
/// # Errors
/// Returns [`LexError::UnclosedQuote`] when a quote never closes and
/// [`LexError::TrailingEscape`] when the tail ends with an unquoted
/// backslash.
pub fn tokens(raw: &str) -> Result<Vec<Box<str>>, LexError> {
    let mut split = Vec::new();
    let mut current = String::new();
    let mut open = false;
    let mut chars = raw.char_indices();
    while let Some((at, ch)) = chars.next() {
        if matches!(ch, ' ' | '\t') {
            if open {
                split.push(Box::<str>::from(current.as_str()));
                current.clear();
                open = false;
            }
            continue;
        }
        open = true;
        match ch {
            '\'' => single_quoted(&mut chars, &mut current, at)?,
            '"' => double_quoted(&mut chars, &mut current, at)?,
            '\\' => current.push(chars.next().ok_or(LexError::TrailingEscape { at })?.1),
            other => current.push(other),
        }
    }
    if open {
        split.push(Box::<str>::from(current.as_str()));
    }
    Ok(split)
}

fn single_quoted(
    chars: &mut std::str::CharIndices<'_>,
    current: &mut String,
    at: usize,
) -> Result<(), LexError> {
    for (_, ch) in chars.by_ref() {
        if ch == '\'' {
            return Ok(());
        }
        current.push(ch);
    }
    Err(LexError::UnclosedQuote { quote: '\'', at })
}

fn double_quoted(
    chars: &mut std::str::CharIndices<'_>,
    current: &mut String,
    at: usize,
) -> Result<(), LexError> {
    while let Some((_, ch)) = chars.next() {
        match ch {
            '"' => return Ok(()),
            '\\' => current.extend(chars.next().map(|(_, escaped)| escaped)),
            other => current.push(other),
        }
    }
    Err(LexError::UnclosedQuote { quote: '"', at })
}

/// A command tail the P04 lexer rejects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LexError {
    /// A quote opened at byte `at` never closes.
    #[error("unclosed {quote} quote at byte {at}")]
    UnclosedQuote {
        /// The opening quote character.
        quote: char,
        /// The byte offset of the opening quote.
        at: usize,
    },
    /// The tail ends with a backslash that escapes nothing.
    #[error("trailing backslash at byte {at}")]
    TrailingEscape {
        /// The byte offset of the backslash.
        at: usize,
    },
}
impl LexError {
    /// Returns the opening quote for an unclosed-quote rejection.
    #[must_use]
    pub const fn quote(self) -> Option<char> {
        match self {
            Self::UnclosedQuote { quote, .. } => Some(quote),
            Self::TrailingEscape { .. } => None,
        }
    }

    /// Returns the byte offset of the opening quote or trailing backslash.
    #[must_use]
    pub const fn at(self) -> usize {
        match self {
            Self::UnclosedQuote { at, .. } | Self::TrailingEscape { at } => at,
        }
    }
}

/// Whether a message line invokes a command or is plain text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Classify {
    /// A `/name args` invocation.
    Command {
        /// The command name as written.
        name: Box<str>,
        /// The trimmed tail after the name.
        args: Box<str>,
    },
    /// Plain message text.
    Text,
}

/// Classifies a message line without failing.
///
/// A line starting with `/` followed by a nonempty name is a command; a
/// missing slash, a bare `/`, and `/skill:` with an empty body are text.
/// Qualified plugin names such as `/quality:todos` classify as commands.
#[must_use]
pub fn classify(line: &str) -> Classify {
    let Some(rest) = line.strip_prefix('/') else {
        return Classify::Text;
    };
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() || name == "skill:" {
        return Classify::Text;
    }
    Classify::Command {
        name: Box::<str>::from(name),
        args: Box::<str>::from(rest[end..].trim()),
    }
}

#[cfg(test)]
mod tests;
