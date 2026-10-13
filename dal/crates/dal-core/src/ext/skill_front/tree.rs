//! The indentation tree of a skill's `mcp:` front matter subtree.
//!
//! The grammar is the YAML subset the `mcp` object needs: block mappings by
//! space indentation, plain and quoted string scalars, and one-line flow
//! lists of strings. Every scalar is a string; nothing is coerced.

use super::{Fault, Kind, Res, pos_col, to_u32};

/// A 1-based source position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Pos {
    pub(super) line: u32,
    pub(super) col: u32,
}

/// One parsed value.
#[derive(Debug)]
pub(super) enum Value {
    Map(Vec<Entry>),
    Scalar(Box<str>),
    Seq(Vec<Box<str>>),
    Empty,
}

/// One `key: value` line and everything nested below it.
#[derive(Debug)]
pub(super) struct Entry {
    pub(super) key: Box<str>,
    pub(super) key_at: Pos,
    pub(super) value_at: Pos,
    pub(super) value: Value,
}

/// One non-blank, non-comment source line with its indentation.
#[derive(Clone, Copy, Debug)]
pub(super) struct Line<'a> {
    pub(super) no: u32,
    pub(super) indent: usize,
    pub(super) text: &'a str,
}

impl Line<'_> {
    fn at(&self, byte: usize) -> Pos {
        Pos {
            line: self.no,
            col: pos_col(self.text, byte, self.indent),
        }
    }
}

/// Splits `lines` into content lines, rejecting tab indentation.
pub(super) fn content_lines<'a>(lines: impl Iterator<Item = (u32, &'a str)>) -> Res<Vec<Line<'a>>> {
    let mut out = Vec::new();
    for (no, raw) in lines {
        let text = raw.trim_start_matches(' ');
        let indent = raw.len() - text.len();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if text.starts_with('\t') {
            return Err(fault(no, indent, Kind::Tab));
        }
        out.push(Line {
            no,
            indent,
            text: text.trim_end(),
        });
    }
    Ok(out)
}

fn fault(line: u32, indent: usize, kind: Kind) -> Fault {
    Fault {
        line,
        col: to_u32(indent + 1),
        kind,
    }
}

/// Parses `lines` as one block mapping whose keys sit at the first line's
/// indentation.
pub(super) fn parse_map(lines: &[Line<'_>]) -> Res<Vec<Entry>> {
    let mut next = 0;
    let entries = block(lines, &mut next)?;
    match lines.get(next) {
        Some(line) => Err(fault(line.no, line.indent, Kind::Indent)),
        None => Ok(entries),
    }
}

fn block(lines: &[Line<'_>], next: &mut usize) -> Res<Vec<Entry>> {
    let Some(first) = lines.get(*next) else {
        return Ok(Vec::new());
    };
    let indent = first.indent;
    let mut entries = Vec::new();
    while let Some(line) = lines.get(*next) {
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(fault(line.no, line.indent, Kind::Indent));
        }
        *next += 1;
        entries.push(entry(line, lines, next)?);
    }
    Ok(entries)
}

fn entry(line: &Line<'_>, lines: &[Line<'_>], next: &mut usize) -> Res<Entry> {
    let (key, rest_at) = split_key(line)?;
    let rest = &line.text[rest_at..];
    let body = rest.trim_start_matches(' ');
    let value_byte = rest_at + (rest.len() - body.len());
    let value_at = line.at(value_byte);
    let key_at = line.at(0);
    let value = if body.is_empty() || body.starts_with('#') {
        nested(line, lines, next)?
    } else {
        inline(line, value_byte)?
    };
    Ok(Entry {
        key: key.into(),
        key_at,
        value_at,
        value,
    })
}

fn nested(line: &Line<'_>, lines: &[Line<'_>], next: &mut usize) -> Res<Value> {
    match lines.get(*next) {
        Some(child) if child.indent > line.indent => Ok(Value::Map(block(lines, next)?)),
        _ => Ok(Value::Empty),
    }
}

fn split_key<'a>(line: &Line<'a>) -> Res<(&'a str, usize)> {
    let text = line.text;
    let mut search = 0;
    while let Some(found) = text[search..].find(':') {
        let at = search + found;
        let after = &text[at + 1..];
        if after.is_empty() || after.starts_with(' ') {
            let key = text[..at].trim_end();
            if key.is_empty() {
                break;
            }
            return Ok((key, at + 1));
        }
        search = at + 1;
    }
    Err(fault(line.no, line.indent, Kind::NotAKey))
}

fn inline(line: &Line<'_>, byte: usize) -> Res<Value> {
    let text = &line.text[byte..];
    let bad = |offset: usize, kind: Kind| Fault {
        line: line.no,
        col: line.at(byte + offset).col,
        kind,
    };
    if let Some(list) = text.strip_prefix('[') {
        let (items, used) = flow_list(list).map_err(|(off, kind)| bad(1 + off, kind))?;
        tail(&text[1 + used..]).map_err(|off| bad(1 + used + off, Kind::Trailing))?;
        return Ok(Value::Seq(items));
    }
    let (value, used) = scalar(text).map_err(|(off, kind)| bad(off, kind))?;
    tail(&text[used..]).map_err(|off| bad(used + off, Kind::Trailing))?;
    Ok(Value::Scalar(value.into()))
}

fn tail(rest: &str) -> Result<(), usize> {
    let body = rest.trim_start_matches(' ');
    if body.is_empty() || body.starts_with('#') {
        Ok(())
    } else {
        Err(rest.len() - body.len())
    }
}

type ScalarFault = (usize, Kind);

fn scalar(text: &str) -> Result<(String, usize), ScalarFault> {
    match text.chars().next() {
        Some('"' | '\'') => quoted(text),
        _ => Ok(plain(text, &[])),
    }
}

fn plain(text: &str, stops: &[char]) -> (String, usize) {
    let mut end = text.len();
    let mut previous_space = true;
    for (at, c) in text.char_indices() {
        if stops.contains(&c) || (c == '#' && previous_space) {
            end = at;
            break;
        }
        previous_space = c == ' ';
    }
    (text[..end].trim_end().to_owned(), end)
}

fn quoted(text: &str) -> Result<(String, usize), ScalarFault> {
    match text.chars().next() {
        Some('"') => double_quoted(text),
        Some('\'') => single_quoted(text),
        _ => Ok((String::new(), 0)),
    }
}

fn double_quoted(text: &str) -> Result<(String, usize), ScalarFault> {
    let mut out = String::new();
    let mut chars = text.char_indices().skip(1);
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return Ok((out, at + 1)),
            '\\' => {
                let Some((esc_at, esc)) = chars.next() else {
                    break;
                };
                let Some(decoded) = unescape(esc) else {
                    return Err((esc_at, Kind::BadEscape));
                };
                out.push(decoded);
            }
            other => out.push(other),
        }
    }
    Err((0, Kind::UnterminatedQuote))
}

fn unescape(c: char) -> Option<char> {
    match c {
        '\\' | '"' | '/' => Some(c),
        'n' => Some('\n'),
        't' => Some('\t'),
        _ => None,
    }
}

fn single_quoted(text: &str) -> Result<(String, usize), ScalarFault> {
    let mut out = String::new();
    let mut chars = text.char_indices().skip(1).peekable();
    while let Some((at, c)) = chars.next() {
        if c != '\'' {
            out.push(c);
            continue;
        }
        if chars.next_if(|(_, next)| *next == '\'').is_some() {
            out.push('\'');
            continue;
        }
        return Ok((out, at + 1));
    }
    Err((0, Kind::UnterminatedQuote))
}

fn flow_list(text: &str) -> Result<(Vec<Box<str>>, usize), ScalarFault> {
    let mut items = Vec::new();
    let mut at = 0;
    loop {
        at += skip_spaces(&text[at..]);
        let rest = &text[at..];
        if rest.starts_with(']') {
            return Ok((items, at + 1));
        }
        let (item, used) = flow_item(rest).map_err(|(off, kind)| (at + off, kind))?;
        items.push(item.into());
        at += used;
        at += skip_spaces(&text[at..]);
        match text[at..].chars().next() {
            Some(',') => at += 1,
            Some(']') => {}
            _ => return Err((at, Kind::BadList)),
        }
    }
}

fn flow_item(rest: &str) -> Result<(String, usize), ScalarFault> {
    if let Some('"' | '\'') = rest.chars().next() {
        quoted(rest)
    } else {
        let (item, used) = plain(rest, &[',', ']']);
        if item.is_empty() {
            Err((0, Kind::BadList))
        } else {
            Ok((item, used))
        }
    }
}

fn skip_spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}
