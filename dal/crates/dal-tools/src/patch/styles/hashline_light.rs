//! Hashline Light profile: snapshot-bound, observation-optional.

use std::path::PathBuf;

use super::super::{
    ir::{Action, Edit, Guard, Locator, ParseError, Window},
    snapshot::ReadRef,
    style::strip_outer_fence,
};

/// Parses Light payloads with `[PATH@ReadRef]` headers.
///
/// Legacy four-hex `[PATH#TAG]` is rejected; tagless headers are rejected.
pub(crate) fn parse(input: &str, symbols: bool) -> Result<Vec<Edit>, ParseError> {
    parse_profile(input, symbols)
}

pub(crate) fn parse_profile(input: &str, symbols: bool) -> Result<Vec<Edit>, ParseError> {
    let input = strip_light_envelope(strip_outer_fence(input));
    if input.is_empty() {
        return Err(ParseError::at_line(
            1,
            "patch: line 1: expected a [path@reference] header, got \"\".",
        ));
    }
    let (texts, framed) = split_framed(input);
    let mut cursor = 0;
    let mut edits = Vec::new();
    while cursor < texts.len() {
        let (path, reference, is_new) = parse_header(&texts[cursor], cursor + 1)?;
        let header = Header {
            path,
            reference,
            is_new,
        };
        cursor += 1;
        let first = edits.len();
        while cursor < texts.len() && !texts[cursor].starts_with('[') {
            cursor = parse_operation(&texts, &framed, cursor, symbols, &header, &mut edits)?;
        }
        if edits.len() == first {
            return Err(ParseError::at_line(
                cursor + 1,
                "patch: line 1: expected PUT, CUT, REM, or MV, got \"\".",
            ));
        }
    }
    if edits.is_empty() {
        return Err(ParseError::new(
            "patch: input must start with [PATH@reference], [PATH#NEW], *** Add File:, or similar header.",
        ));
    }
    Ok(edits)
}

/// One parsed `[path@reference]` or `[path#NEW]` section header.
struct Header {
    path: PathBuf,
    reference: String,
    is_new: bool,
}

impl Header {
    fn edit(&self, index: usize, locator: Locator, action: Action, body: String) -> Edit {
        if self.is_new {
            return Edit::Create {
                index,
                path: self.path.clone(),
                body,
            };
        }
        Edit::Change {
            index,
            path: self.path.clone(),
            locator,
            action,
            guard: Guard::Reference(self.reference.clone()),
            body,
            window: Window::BeforePayload,
        }
    }

    fn reject_new(&self) -> Result<(), ParseError> {
        if self.is_new {
            return Err(new_file_only());
        }
        Ok(())
    }
}

fn new_file_only() -> ParseError {
    ParseError::new("patch: a new file takes only PUT >$: rows.")
}

/// Splits framing lines, accepting LF or CRLF and an omitted final line
/// break; each flag records whether its line had its own LF.
fn split_framed(input: &str) -> (Vec<String>, Vec<bool>) {
    input
        .split_inclusive('\n')
        .map(|raw| {
            let no_lf = raw.strip_suffix('\n').unwrap_or(raw);
            let text = no_lf.strip_suffix('\r').unwrap_or(no_lf).to_owned();
            (text, raw.ends_with('\n'))
        })
        .unzip()
}

fn parse_operation(
    texts: &[String],
    framed: &[bool],
    cursor: usize,
    symbols: bool,
    header: &Header,
    edits: &mut Vec<Edit>,
) -> Result<usize, ParseError> {
    let line = cursor + 1;
    let line_text = texts[cursor].as_str();
    let index = edits.len();
    if let Some(rest) = line_text.strip_prefix("PUT ") {
        if header.is_new && rest != ">$:" {
            return Err(new_file_only());
        }
        let (locator, action) = parse_put(rest, line, symbols)?;
        let at_tail = matches!(
            locator,
            Locator::Gap {
                before_line: usize::MAX
            }
        );
        if header.is_new && !at_tail {
            return Err(new_file_only());
        }
        let (body, next) = body_rows(texts, framed, line)?;
        edits.push(header.edit(index, locator, action, body));
        return Ok(next);
    }
    if let Some(rest) = line_text.strip_prefix("CUT ") {
        header.reject_new()?;
        let locator = parse_range(rest, line)?;
        edits.push(header.edit(index, locator, Action::Replace, String::new()));
        return Ok(line);
    }
    if line_text == "REM" {
        header.reject_new()?;
        edits.push(Edit::Delete {
            index,
            path: header.path.clone(),
            reference: Some(header.reference.clone()),
        });
        return Ok(line);
    }
    if let Some(dest) = line_text.strip_prefix("MV ") {
        header.reject_new()?;
        let dest = dest.trim();
        if dest.is_empty() {
            return Err(ParseError::at_line(
                line,
                format!("patch: line {line}: expected a destination filename, got {line_text:?}."),
            ));
        }
        edits.push(Edit::Rename {
            index,
            from: header.path.clone(),
            to: PathBuf::from(decode_mv_destination(dest, line)?),
            reference: Some(header.reference.clone()),
        });
        return Ok(line);
    }
    Err(ParseError::at_line(
        line,
        format!("patch: line {line}: expected PUT, CUT, REM, or MV, got {line_text:?}."),
    ))
}

fn parse_header(line: &str, number: usize) -> Result<(PathBuf, String, bool), ParseError> {
    let inner = line
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .ok_or_else(|| {
            ParseError::at_line(
                number,
                format!("patch: line {number}: expected a [path@reference] header, got {line:?}."),
            )
        })?;
    let (path, suffix) = split_header(inner).ok_or_else(|| {
        ParseError::at_line(
            number,
            format!("patch: line {number}: expected a [path@reference] header, got {line:?}."),
        )
    })?;
    if suffix == "#NEW" {
        return Ok((path, String::new(), true));
    }
    if let Some(reference) = suffix.strip_prefix('@') {
        if ReadRef::parse(reference).is_none() {
            return Err(ParseError::at_line(
                number,
                format!("patch: line {number}: expected a [path@reference] header, got {line:?}."),
            ));
        }
        return Ok((path, reference.to_owned(), false));
    }
    if suffix.starts_with('#') {
        return Err(ParseError::new(
            "patch: Light and Enhanced profiles do not accept [PATH#TAG]; copy the [path@reference] header from read.",
        ));
    }
    Err(ParseError::at_line(
        number,
        format!("patch: line {number}: expected a [path@reference] header, got {line:?}."),
    ))
}

/// Splits a header body into its decoded path and `@ref`/`#NEW` suffix.
fn split_header(inner: &str) -> Option<(PathBuf, String)> {
    if inner.bytes().any(|b| b == 0) {
        return None;
    }
    if let Some(rest) = inner.strip_prefix('"') {
        let mut escaped = false;
        let mut end = None;
        for (index, byte) in rest.bytes().enumerate() {
            if escaped {
                escaped = false;
                continue;
            }
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                end = Some(index);
                break;
            }
        }
        let end = end?;
        let literal = &inner[..end + 2];
        let path_text: String = sonic_rs::from_str(literal).ok()?;
        if path_text.is_empty() || path_text.bytes().any(|b| b == 0) {
            return None;
        }
        let suffix = &inner[end + 2..];
        if suffix.is_empty() {
            return None;
        }
        return Some((PathBuf::from(path_text), suffix.to_owned()));
    }
    if inner.is_empty() {
        return None;
    }
    let at = inner.rfind('@');
    let hash = inner.rfind('#');
    let split_at = match (at, hash) {
        (Some(at), Some(hash)) if hash > at => hash,
        (Some(at), _) => at,
        (None, Some(hash)) => hash,
        (None, None) => return None,
    };
    let (path, suffix) = inner.split_at(split_at);
    if path.is_empty()
        || path
            .bytes()
            .any(|b| matches!(b, b'@' | b'#' | b']' | b'\r' | b'\n' | 0))
        || path.starts_with([' ', '\t'])
        || path.ends_with([' ', '\t'])
        || path.bytes().any(|b| b < 0x20 || b == 0x7f)
    {
        return None;
    }
    Some((PathBuf::from(path), suffix.to_owned()))
}

fn decode_mv_destination(dest: &str, number: usize) -> Result<String, ParseError> {
    if dest.bytes().any(|b| b == 0) {
        return Err(ParseError::at_line(
            number,
            format!("patch: line {number}: expected a destination filename, got {dest:?}."),
        ));
    }
    if let Some(stripped) = dest.strip_prefix('"') {
        let literal = format!("\"{stripped}");
        // The destination uses the same JSON-quoted decoder as headers.
        if !literal.ends_with('"') {
            return Err(ParseError::at_line(
                number,
                format!("patch: line {number}: expected a destination filename, got {dest:?}."),
            ));
        }
        let decoded: String = sonic_rs::from_str(&literal).map_err(|_| {
            ParseError::at_line(
                number,
                format!("patch: line {number}: expected a destination filename, got {dest:?}."),
            )
        })?;
        if decoded.is_empty() {
            return Err(ParseError::at_line(
                number,
                format!("patch: line {number}: expected a destination filename, got {dest:?}."),
            ));
        }
        return Ok(decoded);
    }
    Ok(dest.to_owned())
}

fn parse_put(rest: &str, number: usize, symbols: bool) -> Result<(Locator, Action), ParseError> {
    let locator_text = rest.strip_suffix(':').ok_or_else(|| {
        ParseError::at_line(
            number,
            format!("patch: line {number}: expected PUT with a trailing colon, got {rest:?}."),
        )
    })?;
    if locator_text == ">$" {
        return Ok((
            Locator::Gap {
                before_line: usize::MAX,
            },
            Action::InsertAfter,
        ));
    }
    if let Some(after) = locator_text.strip_prefix('<') {
        let id = positive(after, number)?;
        return Ok((Locator::Gap { before_line: id }, Action::InsertBefore));
    }
    if let Some(after) = locator_text.strip_prefix('>') {
        let id = positive(after.trim_end_matches('*'), number)?;
        if after.ends_with('*') {
            if !symbols {
                return Err(ParseError::at_line(
                    number,
                    format!(
                        "patch: line {number}: block ops (N*) need search_symbols = true in config.toml."
                    ),
                ));
            }
            return Ok((Locator::Node { first_line: id }, Action::InsertAfter));
        }
        return Ok((
            Locator::Gap {
                before_line: id.saturating_add(1),
            },
            Action::InsertAfter,
        ));
    }
    if let Some(node) = locator_text.strip_suffix('*') {
        if !symbols {
            return Err(ParseError::at_line(
                number,
                format!(
                    "patch: line {number}: block ops (N*) need search_symbols = true in config.toml."
                ),
            ));
        }
        let id = positive(node, number)?;
        return Ok((Locator::Node { first_line: id }, Action::Replace));
    }
    let locator = parse_range(locator_text, number)?;
    Ok((locator, Action::Replace))
}

fn parse_range(text: &str, number: usize) -> Result<Locator, ParseError> {
    let (first_text, last_text) = text
        .split_once(".=")
        .or_else(|| text.split_once('-'))
        .map_or((text, text), |(first, last)| (first, last));
    let first = positive(first_text, number)?;
    let last = positive(last_text, number)?;
    if first > last {
        return Err(ParseError::at_line(
            number,
            format!(
                "patch: line {number}: invalid range {first}-{last}; start must not exceed end."
            ),
        ));
    }
    Ok(Locator::Lines { first, last })
}

fn positive(text: &str, number: usize) -> Result<usize, ParseError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError::at_line(
            number,
            format!("patch: line {number}: expected a positive line number, got {text:?}."),
        ));
    }
    let value: usize = text.parse().map_err(|_| {
        ParseError::at_line(
            number,
            format!("patch: line {number}: expected a positive line number, got {text:?}."),
        )
    })?;
    if value == 0 {
        return Err(ParseError::at_line(
            number,
            format!("patch: line {number}: expected a positive line number, got {text:?}."),
        ));
    }
    Ok(value)
}

fn body_rows(
    lines: &[String],
    framed: &[bool],
    mut cursor: usize,
) -> Result<(String, usize), ParseError> {
    let mut rows = Vec::new();
    let mut last_terminated = true;
    let start = cursor.saturating_sub(1);
    while cursor < lines.len() {
        let text = lines[cursor].as_str();
        if !text.starts_with('+') {
            break;
        }
        rows.push(text[1..].to_owned());
        last_terminated = framed.get(cursor).copied().unwrap_or(false);
        cursor += 1;
    }
    if rows.is_empty() {
        return Err(ParseError::at_line(
            start + 1,
            format!(
                "patch: line {}: expected a + body row, got \"\".",
                start + 1
            ),
        ));
    }
    let mut body = rows.join("\n");
    if last_terminated {
        body.push('\n');
    }
    Ok((body, cursor))
}

fn strip_light_envelope(input: &str) -> &str {
    let has_begin = input.starts_with("*** Begin Patch\n");
    let has_end = input.ends_with("*** End Patch\n") || input.ends_with("*** End Patch");
    if has_begin != has_end {
        return "";
    }
    let input = input.strip_prefix("*** Begin Patch\n").unwrap_or(input);
    if let Some(body) = input.strip_suffix("*** End Patch\n") {
        body
    } else {
        input.strip_suffix("*** End Patch").unwrap_or(input)
    }
}
