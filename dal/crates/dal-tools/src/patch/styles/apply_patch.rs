//! Parser for the pinned Codex `apply_patch` envelope.

use std::path::{Path, PathBuf};

use super::super::{
    ir::{Action, Edit, Guard, Locator, ParseError, Window},
    style::strip_outer_fence,
};

#[derive(Clone, Copy)]
struct Line<'a> {
    text: &'a str,
    terminated: bool,
}

struct Chunk {
    context: Option<String>,
    old: String,
    new: String,
    at_eof: bool,
}

/// Parses a complete Codex patch into the shared edit IR.
///
/// # Errors
/// Returns Codex-compatible parse text for malformed envelopes and hunks.
pub(crate) fn parse(input: &str, _symbols: bool) -> Result<Vec<Edit>, ParseError> {
    let input = strip_outer_fence(input);
    let lines = split_lines(input);
    if lines
        .first()
        .is_none_or(|line| line.text != "*** Begin Patch" || !line.terminated)
    {
        return Err(ParseError::new(
            "invalid patch: The first line of the patch must be '*** Begin Patch'",
        ));
    }
    let end_index = lines.len().checked_sub(1).ok_or_else(|| {
        ParseError::new("invalid patch: The last line of the patch must be '*** End Patch'")
    })?;
    if lines[end_index].text != "*** End Patch" {
        return Err(ParseError::new(
            "invalid patch: The last line of the patch must be '*** End Patch'",
        ));
    }

    let mut cursor = 1;
    let mut edits = Vec::new();
    let mut targets: Vec<PathBuf> = Vec::new();
    while cursor < end_index {
        let line_no = cursor + 1;
        let text = lines[cursor].text;
        if let Some(path) = text.strip_prefix("*** Add File: ") {
            let path = filename(path, line_no)?;
            reject_duplicate(&targets, &path)?;
            targets.push(path.clone());
            let (body, next) = parse_add_body(&lines, cursor + 1, end_index)?;
            edits.push(Edit::Create {
                index: edits.len(),
                path,
                body,
            });
            cursor = next;
            continue;
        }
        if let Some(path) = text.strip_prefix("*** Delete File: ") {
            let path = filename(path, line_no)?;
            reject_duplicate(&targets, &path)?;
            targets.push(path.clone());
            edits.push(Edit::Delete {
                index: edits.len(),
                path,
                reference: None,
            });
            cursor += 1;
            continue;
        }
        if let Some(path) = text.strip_prefix("*** Update File: ") {
            let path = filename(path, line_no)?;
            reject_duplicate(&targets, &path)?;
            targets.push(path.clone());
            let (rename_to, chunks, next) = parse_update(&lines, cursor + 1, end_index, &path)?;
            for chunk in chunks {
                edits.push(chunk.into_edit(edits.len(), &path));
            }
            if let Some(to) = rename_to {
                edits.push(Edit::Rename {
                    index: edits.len(),
                    from: path,
                    to,
                    reference: None,
                });
            }
            cursor = next;
            continue;
        }
        return Err(invalid_hunk(
            line_no,
            format!(
                "'{text}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
            ),
        ));
    }
    Ok(edits)
}

impl Chunk {
    /// A pure addition appends at the tail; anything else replaces the
    /// quoted old text after the previous chunk.
    fn into_edit(self, index: usize, path: &Path) -> Edit {
        let (locator, action, guard) = if self.old.is_empty() && !self.new.is_empty() {
            (
                Locator::Gap {
                    before_line: usize::MAX,
                },
                Action::InsertAfter,
                Guard::Exists,
            )
        } else {
            (
                Locator::Text {
                    old: self.old,
                    line_hint: None,
                    all: false,
                    window: Window::AfterPrevious,
                    context: self.context,
                    at_eof: self.at_eof,
                },
                Action::Replace,
                Guard::Quoted,
            )
        };
        Edit::Change {
            index,
            path: path.to_path_buf(),
            locator,
            action,
            guard,
            body: self.new,
            window: Window::AfterPrevious,
        }
    }
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

fn filename(value: &str, line: usize) -> Result<PathBuf, ParseError> {
    if value.is_empty() || value.contains(['\r', '\n']) {
        return Err(invalid_hunk(line, format!("invalid file path '{value}'")));
    }
    Ok(PathBuf::from(value))
}

fn reject_duplicate(paths: &[PathBuf], path: &PathBuf) -> Result<(), ParseError> {
    if paths.iter().any(|existing| existing == path) {
        return Err(ParseError::new(format!(
            "multiple operations target {}",
            path.display()
        )));
    }
    Ok(())
}

fn parse_add_body(
    lines: &[Line<'_>],
    mut cursor: usize,
    end_index: usize,
) -> Result<(String, usize), ParseError> {
    let start = cursor;
    let mut body = String::new();
    while cursor < end_index && lines[cursor].text.starts_with('+') {
        if !lines[cursor].terminated {
            return Err(invalid_hunk(cursor + 1, "add line is missing LF"));
        }
        body.push_str(&lines[cursor].text[1..]);
        body.push('\n');
        cursor += 1;
    }
    if cursor == start {
        return Err(invalid_hunk(cursor + 1, "add file needs at least one line"));
    }
    Ok((body, cursor))
}

fn parse_update(
    lines: &[Line<'_>],
    mut cursor: usize,
    end_index: usize,
    path: &Path,
) -> Result<(Option<PathBuf>, Vec<Chunk>, usize), ParseError> {
    let mut rename_to = None;
    if cursor < end_index
        && let Some(destination) = lines[cursor].text.strip_prefix("*** Move to: ")
    {
        rename_to = Some(filename(destination, cursor + 1)?);
        cursor += 1;
    }
    let mut chunks = Vec::new();
    while cursor < end_index && !is_file_operation(lines[cursor].text) {
        if !lines[cursor].text.starts_with("@@") {
            return Err(invalid_hunk(
                cursor + 1,
                format!(
                    "Expected update hunk to start with a @@ context marker, got: '{}'",
                    lines[cursor].text
                ),
            ));
        }
        let (chunk, next) = parse_chunk(lines, cursor, end_index, path)?;
        chunks.push(chunk);
        cursor = next;
    }
    if chunks.is_empty() && rename_to.is_none() {
        return Err(invalid_hunk(
            cursor + 1,
            format!("Update file hunk for path '{}' is empty", path.display()),
        ));
    }
    Ok((rename_to, chunks, cursor))
}

fn is_file_operation(line: &str) -> bool {
    line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
}

fn parse_chunk(
    lines: &[Line<'_>],
    start: usize,
    end_index: usize,
    path: &Path,
) -> Result<(Chunk, usize), ParseError> {
    let marker = lines[start].text;
    let context = marker
        .strip_prefix("@@")
        .map(str::trim)
        .filter(|context| !context.is_empty())
        .map(str::to_owned);
    let mut old = String::new();
    let mut new = String::new();
    let mut cursor = start + 1;
    let mut any_change = false;
    let mut at_eof = false;

    while cursor < end_index {
        let line = lines[cursor].text;
        if line.starts_with("@@") || is_file_operation(line) {
            break;
        }
        if line == "*** End of File" {
            at_eof = true;
            cursor += 1;
            if cursor != end_index && !is_file_operation(lines[cursor].text) {
                return Err(invalid_hunk(
                    cursor + 1,
                    "*** End of File is not at end of update",
                ));
            }
            break;
        }
        let Some((prefix, body)) = line.split_at_checked(1) else {
            return Err(invalid_hunk(
                cursor + 1,
                format!(
                    "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                ),
            ));
        };
        match prefix {
            " " => {
                append_line(&mut old, body);
                append_line(&mut new, body);
                any_change = true;
            }
            "+" => {
                append_line(&mut new, body);
                any_change = true;
            }
            "-" => {
                append_line(&mut old, body);
                any_change = true;
            }
            _ => {
                return Err(invalid_hunk(
                    cursor + 1,
                    format!(
                        "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                    ),
                ));
            }
        }
        cursor += 1;
    }
    if !any_change {
        return Err(invalid_hunk(
            start + 1,
            "Update hunk does not contain any lines",
        ));
    }
    if old.is_empty() && new.is_empty() {
        return Err(invalid_hunk(
            start + 1,
            format!("Update file hunk for path '{}' is empty", path.display()),
        ));
    }
    Ok((
        Chunk {
            context,
            old,
            new,
            at_eof,
        },
        cursor,
    ))
}

fn append_line(target: &mut String, line: &str) {
    target.push_str(line);
    target.push('\n');
}

fn invalid_hunk(line: usize, message: impl AsRef<str>) -> ParseError {
    ParseError::at_line(
        line,
        format!("invalid hunk at line {line}, {}", message.as_ref()),
    )
}
