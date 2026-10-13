//! Bounded line parser for hashline snapshot-tagged operations.

use std::path::{Path, PathBuf};

use super::super::{
    ir::{Action, Edit, Guard, Locator, ParseError, Window},
    style::strip_outer_fence,
};

struct FileSection {
    path: PathBuf,
    tag: String,
    is_new: bool,
    edits: Vec<Edit>,
    rem: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PutTarget {
    Replace(LocatorTag),
    InsertBefore(usize),
    InsertAfter(usize),
    InsertAfterBlock(usize),
    Tail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocatorTag {
    Lines(usize, usize),
    Node(usize),
}

/// Parses the hashline grammar without reading files or proving tags.
///
/// # Errors
/// Returns a line-addressed dialect error for malformed headers or operations.
pub(crate) fn parse(input: &str, symbols: bool) -> Result<Vec<Edit>, ParseError> {
    let input = strip_hashline_envelope(strip_outer_fence(input));
    if input.is_empty() {
        return Err(expected(1, "a [path#TAG] header", ""));
    }
    let lines = split_lines(input);
    let mut cursor = 0;
    let mut edits = Vec::new();
    let mut seen_headers: Vec<PathBuf> = Vec::new();
    while cursor < lines.len() {
        let mut section = open_section(&lines, cursor, &mut seen_headers)?;
        cursor += 1;
        while cursor < lines.len() && !is_header(lines[cursor].text) {
            cursor = parse_operation(&lines, cursor, symbols, edits.len(), &mut section)?;
        }
        finish_section(section, cursor, &mut edits)?;
    }
    if edits.is_empty() {
        return Err(expected(1, "a [path#TAG] header", ""));
    }
    Ok(edits)
}

fn open_section(
    lines: &[Line<'_>],
    cursor: usize,
    seen_headers: &mut Vec<PathBuf>,
) -> Result<FileSection, ParseError> {
    let header_line = cursor + 1;
    if !lines[cursor].terminated {
        return Err(expected(
            header_line,
            "a [path#TAG] header",
            lines[cursor].text,
        ));
    }
    let (path, tag, is_new) = parse_header(lines[cursor].text, header_line)?;
    if seen_headers.iter().any(|existing| existing == &path) {
        return Err(ParseError::new(format!(
            "patch: line {header_line}: duplicate file section for {}.",
            path.display()
        )));
    }
    seen_headers.push(path.clone());
    Ok(FileSection {
        path,
        tag,
        is_new,
        edits: Vec::new(),
        rem: false,
    })
}

fn parse_operation(
    lines: &[Line<'_>],
    cursor: usize,
    symbols: bool,
    before: usize,
    section: &mut FileSection,
) -> Result<usize, ParseError> {
    let operation_line = cursor + 1;
    let text = lines[cursor].text;
    if !lines[cursor].terminated {
        return Err(expected(operation_line, "PUT, CUT, REM, or MV", text));
    }
    let index = before + section.edits.len();
    if let Some(locator) = text.strip_prefix("PUT ") {
        let (target, body_start) = parse_put(locator, operation_line, symbols)?;
        let (body, next) = body_rows(lines, body_start, operation_line)?;
        let mut body = body.join("\n");
        if next > body_start && lines[next - 1].terminated {
            body.push('\n');
        }
        let (locator, action) = put_locator(target);
        section
            .edits
            .push(section.change(index, locator, action, body));
        return Ok(next);
    }
    if let Some(locator) = text.strip_prefix("CUT ") {
        let locator = located(parse_cut(locator, operation_line, symbols)?);
        section.reject_new()?;
        section
            .edits
            .push(section.change(index, locator, Action::Replace, String::new()));
        return Ok(cursor + 1);
    }
    if text == "REM" {
        section.reject_new()?;
        section.rem = true;
        section.edits.push(Edit::Delete {
            index,
            path: section.path.clone(),
            reference: None,
        });
        return Ok(cursor + 1);
    }
    if let Some(destination) = text.strip_prefix("MV ") {
        section.reject_new()?;
        let destination = nonempty_filename(destination, operation_line)?;
        section.edits.push(Edit::Rename {
            index,
            from: section.path.clone(),
            to: destination,
            reference: None,
        });
        return Ok(cursor + 1);
    }
    Err(expected(operation_line, "PUT, CUT, REM, or MV", text))
}

fn finish_section(
    mut section: FileSection,
    cursor: usize,
    edits: &mut Vec<Edit>,
) -> Result<(), ParseError> {
    if section.edits.is_empty() {
        return Err(expected(cursor + 1, "PUT, CUT, REM, or MV", ""));
    }
    if section.rem && section.edits.len() > 1 {
        return Err(ParseError::new(format!(
            "patch: [{}#{}]: REM cannot be combined with other ops on the same file.",
            section.path.display(),
            section.tag
        )));
    }
    if !section.is_new {
        edits.extend(section.edits);
        return Ok(());
    }
    if section.edits.len() != 1 {
        return Err(new_file_operation(&section.path));
    }
    let only = section
        .edits
        .pop()
        .ok_or_else(|| new_file_operation(&section.path))?;
    let Edit::Change {
        locator: Locator::Gap {
            before_line: usize::MAX,
        },
        action: Action::InsertAfter,
        body,
        ..
    } = only
    else {
        return Err(new_file_operation(&section.path));
    };
    edits.push(Edit::Create {
        index: edits.len(),
        path: section.path,
        body,
    });
    Ok(())
}

impl FileSection {
    fn change(&self, index: usize, locator: Locator, action: Action, body: String) -> Edit {
        Edit::Change {
            index,
            path: self.path.clone(),
            locator,
            action,
            guard: Guard::Version(self.tag.clone()),
            body,
            window: Window::BeforePayload,
        }
    }

    fn reject_new(&self) -> Result<(), ParseError> {
        if self.is_new {
            return Err(new_file_operation(&self.path));
        }
        Ok(())
    }
}

fn put_locator(target: PutTarget) -> (Locator, Action) {
    match target {
        PutTarget::Tail => (
            Locator::Gap {
                before_line: usize::MAX,
            },
            Action::InsertAfter,
        ),
        PutTarget::InsertBefore(line) => (Locator::Gap { before_line: line }, Action::InsertBefore),
        PutTarget::InsertAfter(line) => (
            Locator::Gap {
                before_line: line.saturating_add(1),
            },
            Action::InsertAfter,
        ),
        PutTarget::InsertAfterBlock(line) => {
            (Locator::Node { first_line: line }, Action::InsertAfter)
        }
        PutTarget::Replace(tag) => (located(tag), Action::Replace),
    }
}

fn located(tag: LocatorTag) -> Locator {
    match tag {
        LocatorTag::Lines(first, last) => Locator::Lines { first, last },
        LocatorTag::Node(first_line) => Locator::Node { first_line },
    }
}

fn strip_hashline_envelope(input: &str) -> &str {
    let input = input.strip_prefix("*** Begin Patch\n").unwrap_or(input);
    if let Some(body) = input.strip_suffix("*** End Patch\n") {
        body.strip_suffix('\n').unwrap_or(body)
    } else {
        input.strip_suffix("*** End Patch").unwrap_or(input)
    }
}

#[derive(Clone, Copy)]
struct Line<'a> {
    text: &'a str,
    terminated: bool,
}

fn split_lines(input: &str) -> Vec<Line<'_>> {
    input
        .split_inclusive('\n')
        .map(|raw| {
            let terminated = raw.ends_with('\n');
            Line {
                text: raw.strip_suffix('\n').unwrap_or(raw),
                terminated,
            }
        })
        .collect()
}

fn is_header(line: &str) -> bool {
    line.starts_with('[') && line.ends_with(']')
}

fn parse_header(line: &str, number: usize) -> Result<(PathBuf, String, bool), ParseError> {
    let Some(header) = line
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Err(expected(number, "a [path#TAG] header", line));
    };
    let Some((path, tag)) = header.rsplit_once('#') else {
        return Err(expected(number, "a [path#TAG] header", line));
    };
    if path.is_empty() || path.contains(['\r', '\n', '#']) {
        return Err(expected(number, "a [path#TAG] header", line));
    }
    let is_new = tag == "NEW";
    let valid_tag = is_new
        || (tag.len() == 4
            && tag
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase()));
    if !valid_tag {
        return Err(expected(number, "a [path#TAG] header", line));
    }
    Ok((PathBuf::from(path), tag.to_owned(), is_new))
}

fn parse_put(locator: &str, line: usize, symbols: bool) -> Result<(PutTarget, usize), ParseError> {
    let Some(locator) = locator.strip_suffix(':') else {
        return Err(expected(
            line,
            "PUT with a trailing colon",
            &format!("PUT {locator}"),
        ));
    };
    if locator.contains('@') {
        return Err(ParseError::at_line(
            line,
            "patch: line 1: registers (@name) are not supported; write the lines with PUT.",
        ));
    }
    let target = if locator == ">$" {
        PutTarget::Tail
    } else if let Some(rest) = locator.strip_prefix('<') {
        PutTarget::InsertBefore(parse_id(rest, line, locator)?)
    } else if let Some(rest) = locator.strip_prefix('>') {
        if let Some(block) = rest.strip_suffix('*') {
            if !symbols {
                return Err(block_ops_disabled(line));
            }
            PutTarget::InsertAfterBlock(parse_id(block, line, locator)?)
        } else {
            PutTarget::InsertAfter(parse_id(rest, line, locator)?)
        }
    } else if let Some(block) = locator.strip_suffix('*') {
        if !symbols {
            return Err(block_ops_disabled(line));
        }
        PutTarget::Replace(LocatorTag::Node(parse_id(block, line, locator)?))
    } else {
        PutTarget::Replace(parse_range(locator, line)?)
    };
    Ok((target, line))
}

fn parse_cut(locator: &str, line: usize, symbols: bool) -> Result<LocatorTag, ParseError> {
    if locator.contains('@') {
        return Err(ParseError::at_line(
            line,
            "patch: line 1: registers (@name) are not supported; write the lines with PUT.",
        ));
    }
    if let Some(block) = locator.strip_suffix('*') {
        if !symbols {
            return Err(block_ops_disabled(line));
        }
        return Ok(LocatorTag::Node(parse_id(block, line, locator)?));
    }
    parse_range(locator, line)
}

fn parse_range(value: &str, line: usize) -> Result<LocatorTag, ParseError> {
    let range = value.split_once(".=").or_else(|| value.split_once('-'));
    let Some((first, last)) = range else {
        let id = parse_id(value, line, value)?;
        return Ok(LocatorTag::Lines(id, id));
    };
    let first = parse_id(first, line, value)?;
    let last = parse_id(last, line, value)?;
    Ok(LocatorTag::Lines(first, last))
}

fn parse_id(value: &str, line: usize, token: &str) -> Result<usize, ParseError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(expected(line, "a positive line number", token));
    }
    let id = value
        .parse::<usize>()
        .map_err(|_| expected(line, "a positive line number", token))?;
    if id == 0 {
        return Err(expected(line, "a positive line number", token));
    }
    Ok(id)
}

fn nonempty_filename(value: &str, line: usize) -> Result<PathBuf, ParseError> {
    let value = value.trim_matches([' ', '\t']);
    if value.is_empty() || value.contains(['\r', '\n', '#']) {
        return Err(expected(line, "a destination filename", value));
    }
    Ok(PathBuf::from(value))
}

fn body_rows(
    lines: &[Line<'_>],
    mut cursor: usize,
    line: usize,
) -> Result<(Vec<String>, usize), ParseError> {
    let mut rows = Vec::new();
    while cursor < lines.len() && lines[cursor].text.starts_with('+') {
        if !lines[cursor].terminated {
            return Err(expected(cursor + 1, "a + body row", lines[cursor].text));
        }
        rows.push(lines[cursor].text[1..].to_owned());
        cursor += 1;
    }
    if rows.is_empty() {
        return Err(expected(line, "a + body row", ""));
    }
    Ok((rows, cursor))
}

fn block_ops_disabled(line: usize) -> ParseError {
    ParseError::at_line(
        line,
        format!("patch: line {line}: block ops (N*) need search_symbols = true in config.toml."),
    )
}

fn new_file_operation(path: &Path) -> ParseError {
    ParseError::new(format!(
        "patch: [{}#NEW]: a new file takes only PUT >$: rows.",
        path.display()
    ))
}

fn expected(line: usize, what: &str, text: &str) -> ParseError {
    ParseError::at_line(
        line,
        format!("patch: line {line}: expected {what}, got {text:?}."),
    )
}
