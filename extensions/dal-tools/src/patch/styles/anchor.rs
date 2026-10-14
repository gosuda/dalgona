//! Bounded parser for the anchor-style patch grammar.

use std::path::{Path, PathBuf};

use super::super::{
    ir::{Action, Edit, Guard, Locator, ParseError, Window},
    style::{strip_anchor_envelope, strip_outer_fence},
};

#[derive(Clone, Copy)]
struct Line<'a> {
    raw: &'a str,
    text: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FindSelector {
    Text,
    Line(usize),
    Span(usize, usize),
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionKind {
    Replace,
    InsertBefore,
    InsertAfter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Marker<'a> {
    File(&'a str),
    NewFile(&'a str),
    DeleteFile(&'a str),
    Move(&'a str),
    Find(&'a str),
    Action(ActionKind),
    ReplaceFile,
}

/// Parses anchor sections into the shared edit IR.
///
/// # Errors
/// Returns a dialect parse error with the exact payload line and catalog text.
pub(crate) fn parse(input: &str, _symbols: bool) -> Result<Vec<Edit>, ParseError> {
    let input = strip_anchor_envelope(strip_outer_fence(input));
    let lines = split_lines(input);
    if lines.first().is_none_or(|line| marker(line.text).is_none()) {
        return Err(ParseError::new(
            "patch: input must start with *** File:, *** New File:, *** Delete File:, or *** Move:.",
        ));
    }
    let mut edits = Vec::new();
    let mut cursor = 0;
    let mut current_path: Option<PathBuf> = None;
    let mut current_tag: Option<String> = None;

    while cursor < lines.len() {
        let number = cursor + 1;
        let line = lines[cursor].text;
        match marker(line) {
            Some(Marker::File(header)) => {
                let (path, tag) = parse_file_header(header, number)?;
                if let Some(path) = path {
                    current_path = Some(path);
                    current_tag = tag;
                } else if current_path.is_none() {
                    return Err(ParseError::at_line(
                        number,
                        format!(
                            "patch: line {number}: a bare *** File: needs an earlier *** File: path."
                        ),
                    ));
                } else if let Some(tag) = tag {
                    current_tag = Some(tag);
                }
                cursor += 1;
                let previous_count = edits.len();
                cursor = parse_file_body(
                    &lines,
                    cursor,
                    number,
                    current_path.as_ref().ok_or_else(|| {
                        ParseError::at_line(
                            number,
                            format!("patch: line {number}: a bare *** File: needs an earlier *** File: path."),
                        )
                    })?,
                    current_tag.as_deref(),
                    &mut edits,
                )?;
                if edits.len() == previous_count {
                    return Err(expected_header(number, ""));
                }
            }
            Some(Marker::NewFile(path)) => {
                let path = nonempty_path(path, number)?;
                let (body, next) = collect_body(&lines, cursor + 1);
                edits.push(Edit::Create {
                    index: edits.len(),
                    path,
                    body,
                });
                cursor = next;
            }
            Some(Marker::DeleteFile(path)) => {
                edits.push(Edit::Delete {
                    index: edits.len(),
                    path: nonempty_path(path, number)?,
                    reference: None,
                });
                cursor += 1;
            }
            Some(Marker::Move(paths)) => {
                let (from, to) = parse_move(paths, number)?;
                edits.push(Edit::Rename {
                    index: edits.len(),
                    from,
                    to,
                    reference: None,
                });
                cursor += 1;
            }
            _ => return Err(expected_header(number, line)),
        }
    }

    if edits.is_empty() {
        return Err(ParseError::new(
            "patch: input must start with *** File:, *** New File:, *** Delete File:, or *** Move:.",
        ));
    }
    Ok(edits)
}

fn split_lines(input: &str) -> Vec<Line<'_>> {
    input
        .split_inclusive('\n')
        .map(|raw| Line {
            raw,
            text: raw.strip_suffix('\n').unwrap_or(raw),
        })
        .collect()
}

fn marker(line: &str) -> Option<Marker<'_>> {
    let line = line.trim_end_matches([' ', '\t']);
    for (prefix, kind) in [
        ("*** File:", 0),
        ("*** New File:", 1),
        ("*** Delete File:", 2),
        ("*** Move:", 3),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return match kind {
                0 => Some(Marker::File(rest)),
                1 => Some(Marker::NewFile(rest)),
                2 => Some(Marker::DeleteFile(rest)),
                3 => Some(Marker::Move(rest)),
                _ => None,
            };
        }
    }
    if let Some(rest) = line.strip_prefix("*** Find") {
        let selector = rest.trim_matches([' ', '\t']);
        let valid = selector.is_empty()
            || selector == "all"
            || selector.strip_prefix('@').and_then(parse_line).is_some()
            || selector.split_once('-').is_some_and(|(first, last)| {
                parse_line(first).is_some() && parse_line(last).is_some()
            });
        if valid {
            return Some(Marker::Find(rest));
        }
    }
    match line {
        "*** Replace" => Some(Marker::Action(ActionKind::Replace)),
        "*** Insert Before" => Some(Marker::Action(ActionKind::InsertBefore)),
        "*** Insert After" => Some(Marker::Action(ActionKind::InsertAfter)),
        "*** Replace File" => Some(Marker::ReplaceFile),
        _ => None,
    }
}

fn parse_file_header(
    header: &str,
    line: usize,
) -> Result<(Option<PathBuf>, Option<String>), ParseError> {
    let header = header.trim_matches([' ', '\t']);
    let (path, tag) = match header.rsplit_once(" #") {
        Some((path, tag)) => {
            if tag.len() != 8
                || !tag
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())
            {
                return Err(expected_header(line, &format!("*** File:{header}")));
            }
            (path.trim_end_matches([' ', '\t']), Some(tag.to_owned()))
        }
        None => (header, None),
    };
    if path.contains('#') {
        return Err(expected_header(line, &format!("*** File:{header}")));
    }
    if path.is_empty() {
        return Ok((None, tag));
    }
    Ok((Some(PathBuf::from(path)), tag))
}

fn nonempty_path(value: &str, line: usize) -> Result<PathBuf, ParseError> {
    let path = value.trim_matches([' ', '\t']);
    if path.is_empty() {
        return Err(expected_header(line, &format!("*** {value}")));
    }
    Ok(PathBuf::from(path))
}

fn parse_move(value: &str, line: usize) -> Result<(PathBuf, PathBuf), ParseError> {
    let Some((from, to)) = value.split_once("->") else {
        return Err(expected_header(line, &format!("*** Move:{value}")));
    };
    let from = from.trim_matches([' ', '\t']);
    let to = to.trim_matches([' ', '\t']);
    if from.is_empty() || to.is_empty() {
        return Err(expected_header(line, &format!("*** Move:{value}")));
    }
    Ok((PathBuf::from(from), PathBuf::from(to)))
}

fn parse_file_body(
    lines: &[Line<'_>],
    mut cursor: usize,
    section_line: usize,
    path: &Path,
    tag: Option<&str>,
    edits: &mut Vec<Edit>,
) -> Result<usize, ParseError> {
    let section_start = edits.len();
    while cursor < lines.len() {
        let number = cursor + 1;
        match marker(lines[cursor].text) {
            Some(
                Marker::File(_) | Marker::NewFile(_) | Marker::DeleteFile(_) | Marker::Move(_),
            ) => break,
            Some(Marker::ReplaceFile) => {
                if edits.len() != section_start {
                    return Err(ParseError::at_line(
                        number,
                        format!(
                            "patch: line {number}: expected a header, got {:?}.",
                            lines[number - 1].text
                        ),
                    ));
                }
                let guard = tag.map_or(Guard::Seen, |tag| Guard::WholeTag(tag.to_owned()));
                let (body, next) = collect_body(lines, cursor + 1);
                edits.push(Edit::Change {
                    index: edits.len(),
                    path: path.to_path_buf(),
                    locator: Locator::Whole,
                    action: Action::Replace,
                    guard,
                    body,
                    window: Window::BeforePayload,
                });
                return Ok(next);
            }
            Some(Marker::Find(selector)) => {
                let selector = parse_find_selector(selector, number)?;
                let (quoted, action_line) = collect_find_body(lines, cursor + 1);
                if quoted.is_empty() {
                    return Err(ParseError::new(format!(
                        "patch: changes[{}]: *** Find needs a non-empty body.",
                        edits.len()
                    )));
                }
                if action_line >= lines.len() {
                    return Err(find_needs_action(edits.len()));
                }
                let Some(Marker::Action(action)) = marker(lines[action_line].text) else {
                    return Err(find_needs_action(edits.len()));
                };
                let (body, next) = collect_body(lines, action_line + 1);
                let guard = find_guard(selector, action, tag);
                let locator = find_locator(selector, quoted, edits.len())?;
                edits.push(Edit::Change {
                    index: edits.len(),
                    path: path.to_path_buf(),
                    locator,
                    action: action.into(),
                    guard,
                    body,
                    window: Window::BeforePayload,
                });
                cursor = next;
            }
            _ => {
                return Err(expected_header(number, lines[number - 1].text));
            }
        }
    }
    if edits.len() == section_start {
        return Err(expected_header(section_line, ""));
    }
    Ok(cursor)
}

fn find_guard(selector: FindSelector, action: ActionKind, tag: Option<&str>) -> Guard {
    match selector {
        FindSelector::All => tag.map_or(Guard::Seen, |tag| Guard::WholeTag(tag.to_owned())),
        FindSelector::Span(_, _) if action == ActionKind::Replace => Guard::Seen,
        _ => Guard::Quoted,
    }
}

fn find_locator(
    selector: FindSelector,
    quoted: String,
    index: usize,
) -> Result<Locator, ParseError> {
    Ok(match selector {
        FindSelector::Text => text_locator(quoted, None, false),
        FindSelector::Line(line) => text_locator(quoted, Some(line), false),
        FindSelector::All => text_locator(quoted, None, true),
        FindSelector::Span(first, last) => {
            let quoted = quote_lines(&quoted);
            let span = last.saturating_sub(first).saturating_add(1);
            if quoted.len() != 2 && quoted.len() != span {
                return Err(ParseError::new(format!(
                    "patch: changes[{index}]: *** Find {first}-{last} quotes {} lines; quote 2 lines (first and last) or all {span} lines.",
                    quoted.len(),
                )));
            }
            Locator::Span {
                first,
                last,
                quoted,
            }
        }
    })
}

fn text_locator(old: String, line_hint: Option<usize>, all: bool) -> Locator {
    Locator::Text {
        old,
        line_hint,
        all,
        window: Window::BeforePayload,
        context: None,
        at_eof: false,
    }
}

fn collect_body(lines: &[Line<'_>], mut cursor: usize) -> (String, usize) {
    let mut body = String::new();
    while cursor < lines.len() && marker(lines[cursor].text).is_none() {
        body.push_str(lines[cursor].raw);
        cursor += 1;
    }
    (body, cursor)
}

fn collect_find_body(lines: &[Line<'_>], mut cursor: usize) -> (String, usize) {
    let mut body = String::new();
    while cursor < lines.len() {
        if matches!(marker(lines[cursor].text), Some(Marker::Action(_))) {
            break;
        }
        if matches!(
            marker(lines[cursor].text),
            Some(
                Marker::File(_)
                    | Marker::NewFile(_)
                    | Marker::DeleteFile(_)
                    | Marker::Move(_)
                    | Marker::Find(_)
                    | Marker::ReplaceFile
            )
        ) {
            break;
        }
        body.push_str(lines[cursor].raw);
        cursor += 1;
    }
    (body, cursor)
}

fn parse_find_selector(value: &str, line: usize) -> Result<FindSelector, ParseError> {
    let selector = value.trim_matches([' ', '\t']);
    if selector.is_empty() {
        return Ok(FindSelector::Text);
    }
    if selector == "all" {
        return Ok(FindSelector::All);
    }
    if let Some(value) = selector.strip_prefix('@') {
        return parse_line(value)
            .map(FindSelector::Line)
            .ok_or_else(|| expected_header(line, selector));
    }
    let Some((first, last)) = selector.split_once('-') else {
        return Err(expected_header(line, selector));
    };
    let first = parse_line(first).ok_or_else(|| expected_header(line, selector))?;
    let last = parse_line(last).ok_or_else(|| expected_header(line, selector))?;
    Ok(FindSelector::Span(first, last))
}

fn parse_line(value: &str) -> Option<usize> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parsed = value.parse::<usize>().ok()?;
    (parsed > 0).then_some(parsed)
}

fn quote_lines(value: &str) -> Vec<String> {
    let mut lines: Vec<String> = value.split('\n').map(str::to_owned).collect();
    if value.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn find_needs_action(index: usize) -> ParseError {
    ParseError::new(format!(
        "patch: changes[{index}]: *** Find needs an action: *** Replace, *** Insert Before, or *** Insert After."
    ))
}

fn expected_header(line: usize, text: &str) -> ParseError {
    ParseError::at_line(
        line,
        format!("patch: line {line}: expected a header, got {text:?}."),
    )
}

impl From<ActionKind> for Action {
    fn from(action: ActionKind) -> Self {
        match action {
            ActionKind::Replace => Self::Replace,
            ActionKind::InsertBefore => Self::InsertBefore,
            ActionKind::InsertAfter => Self::InsertAfter,
        }
    }
}
