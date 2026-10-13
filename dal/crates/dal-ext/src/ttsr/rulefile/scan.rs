//! Front matter scanning: fence splitting, the header walk, and scalars.
//!
//! [`split`] reads only the block; validation of the keys against the rule
//! shape happens in the parent module when it builds the rule.

use super::{
    EXPECTED_KEY, Entry, FENCE, FrontError, FrontErrorKind, FrontMatter, HEADER_MAX_LINES,
    KNOWN_KEYS, MISSING_CLOSE, TEXT_AFTER_LIST, TEXT_AFTER_QUOTE, TOO_LONG, UNSUPPORTED_KEYS,
    UNTERMINATED_LIST, UNTERMINATED_QUOTE, Value,
};

/// Splits rule file text into its front matter block, its body, and the
/// errors and key notes of the block.
///
/// A leading UTF-8 BOM is removed and every CR is stripped first, so LF,
/// CRLF, and BOM text parse equal. Without an opening `---` line the whole
/// text is the body. The body is the text after the closing line, trimmed,
/// and otherwise kept byte for byte. A missing closing line or an over-long
/// header yields one error at line 1, an empty block, and an empty body.
#[must_use]
pub fn split(text: &str) -> (FrontMatter, String, Vec<FrontError>) {
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace('\r', "");
    let mut lines = text.split('\n');
    if lines.next() != Some(FENCE) {
        return (FrontMatter::default(), text.trim().to_owned(), Vec::new());
    }
    let mut header = Vec::new();
    let mut offset = FENCE.len() + 1;
    let mut body = None;
    for line in lines {
        offset += line.len() + 1;
        if line == FENCE {
            body = Some(text.get(offset..).unwrap_or_default().trim().to_owned());
            break;
        }
        header.push(line);
    }
    let Some(body) = body else {
        let error = FrontError::new(1, FrontErrorKind::Line, MISSING_CLOSE);
        return (FrontMatter::default(), String::new(), vec![error]);
    };
    if header.len() > HEADER_MAX_LINES {
        let error = FrontError::new(1, FrontErrorKind::Line, TOO_LONG);
        return (FrontMatter::default(), String::new(), vec![error]);
    }
    let mut parser = Header {
        lines: header,
        next: 0,
        seen: Vec::new(),
        front: FrontMatter::default(),
        errors: Vec::new(),
    };
    parser.run();
    (parser.front, body, parser.errors)
}

/// The header walk: one pass over the lines between the fences.
struct Header<'a> {
    lines: Vec<&'a str>,
    next: usize,
    seen: Vec<String>,
    front: FrontMatter,
    errors: Vec<FrontError>,
}

impl<'a> Header<'a> {
    /// The file line of header index `index`; the opening fence is line 1.
    const fn line_of(index: usize) -> usize {
        index + 2
    }

    fn take(&mut self) -> Option<(usize, &'a str)> {
        let line = *self.lines.get(self.next)?;
        let number = Self::line_of(self.next);
        self.next += 1;
        Some((number, line))
    }

    fn run(&mut self) {
        while let Some((number, line)) = self.take() {
            let rest = line.trim_start_matches(' ');
            if rest.is_empty() || rest.starts_with('#') {
                continue;
            }
            let Some((raw_key, after)) = split_key(line) else {
                self.errors
                    .push(FrontError::new(number, FrontErrorKind::Line, EXPECTED_KEY));
                continue;
            };
            let key = camel_case(raw_key);
            let value = self.value(&key, number, after);
            if self.seen.contains(&key) {
                self.errors.push(FrontError::new(
                    number,
                    FrontErrorKind::Duplicate,
                    format!("\"{key}\" appears twice"),
                ));
                continue;
            }
            self.seen.push(key.clone());
            if let Some(reason) = key_note(&key) {
                self.errors
                    .push(FrontError::new(number, FrontErrorKind::Note, reason));
            }
            match value {
                Ok(value) => self.front.entries.push(Entry {
                    key,
                    line: number,
                    value,
                }),
                Err(error) => self.errors.push(error),
            }
        }
    }

    /// Parses the value after `key:` at `line`, consuming continuation lines.
    fn value(&mut self, key: &str, line: usize, after: &str) -> Result<Value, FrontError> {
        let text = after.trim_start_matches(' ');
        let comment_only = text.starts_with('#') && text.len() < after.len();
        if text.is_empty() || comment_only {
            return self.dash_list(key, line);
        }
        let value_error = |reason: String| FrontError::new(line, FrontErrorKind::Value, reason);
        if text.starts_with(['"', '\'']) {
            let (string, rest) = quoted(text).map_err(value_error)?;
            only_comment(rest, TEXT_AFTER_QUOTE).map_err(value_error)?;
            return Ok(Value::Str(string));
        }
        if text.starts_with('[') {
            return flow_list(text).map(Value::List).map_err(value_error);
        }
        let plain = plain(text);
        Ok(match plain {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            ">" | ">-" | "|" | "|-" => Value::Str(self.block(plain)),
            _ if (1..=9).contains(&plain.len()) && plain.bytes().all(|b| b.is_ascii_digit()) => {
                Value::Int(
                    plain
                        .bytes()
                        .fold(0, |acc, digit| acc * 10 + u32::from(digit - b'0')),
                )
            }
            _ => Value::Str(plain.to_owned()),
        })
    }

    /// Reads the `- ` item lines after an empty value.
    fn dash_list(&mut self, key: &str, line: usize) -> Result<Value, FrontError> {
        let mut items = Vec::new();
        let mut first_error = None;
        while let Some(item) = self.lines.get(self.next).copied().and_then(dash_item) {
            let item_line = Self::line_of(self.next);
            self.next += 1;
            if plain(item).is_empty() {
                continue;
            }
            match item_scalar(item) {
                Ok(item) => items.push(item),
                Err(reason) => {
                    first_error.get_or_insert_with(|| {
                        FrontError::new(item_line, FrontErrorKind::Value, reason)
                    });
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        if items.is_empty() {
            return Err(FrontError::new(
                line,
                FrontErrorKind::Line,
                format!("\"{key}\" has no value"),
            ));
        }
        Ok(Value::List(items))
    }

    /// Reads a block scalar in `style` (`>`, `>-`, `|`, or `|-`).
    fn block(&mut self, style: &str) -> String {
        let start = self.next;
        while self
            .lines
            .get(self.next)
            .is_some_and(|line| line.is_empty() || line.starts_with(' '))
        {
            self.next += 1;
        }
        let mut rows = self.lines.get(start..self.next).unwrap_or_default();
        while let Some((last, rest)) = rows.split_last()
            && is_blank(last)
        {
            rows = rest;
        }
        let indent = rows
            .iter()
            .filter(|row| !is_blank(row))
            .map(|row| row.len() - row.trim_start_matches(' ').len())
            .min()
            .unwrap_or(0);
        let rows = rows.iter().map(|row| {
            if is_blank(row) {
                ""
            } else {
                row.get(indent..).unwrap_or_default()
            }
        });
        let mut out = String::new();
        if style.starts_with('|') {
            for (index, row) in rows.enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str(row);
            }
        } else {
            let mut joins = false;
            for row in rows {
                if row.is_empty() {
                    out.push('\n');
                    joins = false;
                } else {
                    if joins {
                        out.push(' ');
                    }
                    out.push_str(row);
                    joins = true;
                }
            }
        }
        if !out.is_empty() && !style.ends_with('-') {
            out.push('\n');
        }
        out
    }
}

fn is_blank(line: &str) -> bool {
    line.trim_start_matches(' ').is_empty()
}

/// Splits `key:` at column 0, the key matching `[A-Za-z_][A-Za-z0-9_-]*`.
fn split_key(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let end = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
        .unwrap_or(bytes.len());
    let (key, rest) = line.split_at(end);
    rest.strip_prefix(':').map(|after| (key, after))
}

/// Normalizes kebab-case to camelCase: each `-` is dropped and the next
/// character is upper-cased.
fn camel_case(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut upper = false;
    for ch in key.chars() {
        if ch == '-' {
            upper = true;
        } else if upper {
            out.push(ch.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Returns the note for a key the file parser does not consume.
fn key_note(key: &str) -> Option<String> {
    if KNOWN_KEYS.contains(&key) {
        None
    } else if UNSUPPORTED_KEYS.contains(&key) {
        Some(format!("\"{key}\" is not supported"))
    } else {
        Some(format!("unknown key \"{key}\""))
    }
}

/// Returns a plain scalar: the text up to the first ` #`, trimmed.
fn plain(text: &str) -> &str {
    text.find(" #").map_or(text, |at| &text[..at]).trim()
}

/// Accepts only spaces or a ` #` comment after a closing quote or bracket.
fn only_comment(rest: &str, reason: &str) -> Result<(), String> {
    let text = rest.trim_start_matches(' ');
    if text.is_empty() || (text.starts_with('#') && text.len() < rest.len()) {
        Ok(())
    } else {
        Err(reason.to_owned())
    }
}

/// Reads one quoted scalar at the start of `text`; returns it and the rest
/// after the closing quote.
///
/// Double quotes take the escapes `\"` `\\` `\/` `\n` `\t` `\uXXXX`; single
/// quotes take `''` as a quote and no escapes.
fn quoted(text: &str) -> Result<(String, &str), String> {
    let mut chars = text.char_indices();
    let Some((_, quote)) = chars.next() else {
        return Err(UNTERMINATED_QUOTE.to_owned());
    };
    let mut out = String::new();
    while let Some((at, ch)) = chars.next() {
        if ch == quote {
            let rest = &text[at + 1..];
            if quote == '\'' && rest.starts_with('\'') {
                chars.next();
                out.push('\'');
                continue;
            }
            return Ok((out, rest));
        }
        if ch != '\\' || quote == '\'' {
            out.push(ch);
            continue;
        }
        let Some((_, escape)) = chars.next() else {
            return Err(UNTERMINATED_QUOTE.to_owned());
        };
        out.push(match escape {
            '"' | '\\' | '/' => escape,
            'n' => '\n',
            't' => '\t',
            'u' => {
                let mut hex = String::with_capacity(4);
                while hex.len() < 4
                    && let Some((_, digit)) = chars.clone().next()
                    && digit.is_ascii_hexdigit()
                {
                    hex.push(digit);
                    chars.next();
                }
                let decoded = (hex.len() == 4)
                    .then(|| u32::from_str_radix(&hex, 16).ok())
                    .flatten()
                    .and_then(char::from_u32);
                decoded.ok_or_else(|| format!("bad escape \"\\u{hex}\""))?
            }
            other => return Err(format!("bad escape \"\\{other}\"")),
        });
    }
    Err(UNTERMINATED_QUOTE.to_owned())
}

/// Reads a one-line flow list `[a, "b,c", 'd']`; commas inside quotes do
/// not split, and an empty plain item (`[a,]`, `[a,,b]`) drops, so no empty
/// condition source can reach a rule. A quoted empty item is kept.
fn flow_list(text: &str) -> Result<Vec<String>, String> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut start = 1;
    let mut quote = None;
    let mut close = None;
    let mut at = 1;
    while let Some(&byte) = bytes.get(at) {
        match quote {
            Some(b'"') if byte == b'\\' => at += 1,
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b',' => {
                    parts.push(&text[start..at]);
                    start = at + 1;
                }
                b']' => {
                    parts.push(&text[start..at]);
                    close = Some(at);
                    break;
                }
                _ => {}
            },
        }
        at += 1;
    }
    let Some(close) = close else {
        return Err(UNTERMINATED_LIST.to_owned());
    };
    only_comment(&text[close + 1..], TEXT_AFTER_LIST)?;
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .map(|part| item_scalar(part.trim()))
        .collect()
}

/// Returns the text after `- ` of a line matching `^\s*- `, keeping the
/// space so a following `#` reads as a comment.
fn dash_item(line: &str) -> Option<&str> {
    line.trim_start_matches([' ', '\t'])
        .strip_prefix('-')
        .filter(|rest| rest.starts_with(' '))
}

/// Reads a `- ` list item: a quoted or a plain scalar.
fn item_scalar(item: &str) -> Result<String, String> {
    let text = item.trim_start_matches(' ');
    if text.starts_with(['"', '\'']) {
        let (value, rest) = quoted(text)?;
        only_comment(rest, TEXT_AFTER_QUOTE)?;
        Ok(value)
    } else {
        Ok(plain(item).to_owned())
    }
}
