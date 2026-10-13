//! Byte decoding, path containment, overlap, and exact range mapping.

use std::path::{Component, Path, PathBuf};

use super::ir::{Edit, EngineError, ErrorClass};

/// Decoded text with BOM, EOL, and final-newline metadata.
#[derive(Clone, Debug)]
pub struct Text {
    /// Original raw bytes.
    pub raw: Vec<u8>,
    /// Whether an `EF BB BF` prefix was present.
    pub bom: bool,
    /// View bytes with BOM and carriage returns before LF removed.
    pub view: Vec<u8>,
    /// View offsets where a CR was removed, in ascending order.
    pub removed_cr_offsets: Vec<usize>,
    /// Whether the raw file ends with LF.
    pub final_newline: bool,
    /// Whether CRLF pairs outnumber bare LF.
    pub dominant_crlf: bool,
    /// One-based line starts in view bytes.
    pub line_starts: Vec<usize>,
}

/// Decodes raw bytes or rejects binary, invalid UTF-8, and oversize files.
///
/// # Errors
///
/// Returns an [`ErrorClass::File`] error when the file is over 16 MiB, has a
/// NUL byte in its first 8 KiB, or is not valid UTF-8.
pub fn decode(path: &Path, bytes: &[u8]) -> Result<Text, EngineError> {
    if bytes.len() > 16 << 20 {
        return Err(EngineError::new(
            ErrorClass::File,
            format!("patch: {} is over 16 MiB. Use exec.", display_path(path)),
        ));
    }
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err(EngineError::new(
            ErrorClass::File,
            format!(
                "patch: {} is not a text file (binary or invalid UTF-8).",
                display_path(path)
            ),
        ));
    }
    let bom = bytes.starts_with(b"\xEF\xBB\xBF");
    let raw_text = if bom { &bytes[3..] } else { bytes };
    let text = std::str::from_utf8(raw_text).map_err(|_| {
        EngineError::new(
            ErrorClass::File,
            format!(
                "patch: {} is not a text file (binary or invalid UTF-8).",
                display_path(path)
            ),
        )
    })?;
    let mut view = Vec::with_capacity(raw_text.len());
    let mut removed = Vec::new();
    let mut crlf = 0_usize;
    let mut lf = 0_usize;
    let mut previous_cr = false;
    for (index, byte) in raw_text.iter().enumerate() {
        if *byte == b'\r' {
            previous_cr = true;
            continue;
        }
        if *byte == b'\n' {
            if previous_cr {
                crlf += 1;
                removed.push(view.len());
            } else {
                lf += 1;
            }
            previous_cr = false;
            view.push(b'\n');
            continue;
        }
        if previous_cr {
            // A lone CR is preserved as source bytes.
            view.push(b'\r');
            previous_cr = false;
            let _ = index;
        }
        view.push(*byte);
    }
    if previous_cr {
        view.push(b'\r');
    }
    let _ = text;
    let final_newline = view.last().is_some_and(|byte| *byte == b'\n');
    let mut line_starts = vec![0];
    for (index, byte) in view.iter().enumerate() {
        if *byte == b'\n' && index + 1 < view.len() {
            line_starts.push(index + 1);
        }
    }
    // An empty file has zero lines; otherwise the last line may be unterminated.
    if view.is_empty() {
        line_starts.clear();
    }
    Ok(Text {
        raw: bytes.to_vec(),
        bom,
        view,
        removed_cr_offsets: removed,
        final_newline,
        dominant_crlf: crlf > lf,
        line_starts,
    })
}

/// Returns the one-based line count of decoded text.
#[must_use]
pub fn line_count(text: &Text) -> usize {
    text.line_starts.len()
}

/// Resolves a workspace-relative patch path against the workspace root.
///
/// # Errors
///
/// Returns an [`ErrorClass::Resolve`] error when the path is empty, is
/// absolute, or resolves outside the workspace.
pub fn resolve_path(workspace: &Path, raw: &Path) -> Result<(PathBuf, PathBuf), EngineError> {
    if raw.as_os_str().is_empty() {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            "patch: path must not be empty.".to_owned(),
        ));
    }
    let mut normalized = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err(outside_workspace(raw, workspace));
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(outside_workspace(raw, workspace));
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    let absolute = workspace.join(&normalized);
    // Canonicalize the parent chain when it exists; never escape the workspace.
    let parent = absolute.parent().unwrap_or(workspace);
    let canonical_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    let canonical_workspace =
        std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    if canonical_parent != canonical_workspace
        && !canonical_parent.starts_with(&canonical_workspace)
    {
        return Err(outside_workspace(raw, workspace));
    }
    // Resolve the target itself when it exists; otherwise join the canonical parent.
    let canonical_target = std::fs::canonicalize(&absolute)
        .unwrap_or_else(|_| canonical_parent.join(absolute.file_name().unwrap_or_default()));
    if canonical_target != canonical_workspace
        && !canonical_target.starts_with(&canonical_workspace)
    {
        return Err(outside_workspace(raw, workspace));
    }
    // Preserve in-workspace symlinks: write through the link itself.
    Ok((normalized.clone(), canonical_target))
}

/// Checks pairwise overlap for edits to one canonical path.
///
/// # Errors
///
/// Returns an [`ErrorClass::Resolve`] error naming the first two edits whose
/// line spans overlap.
pub fn check_overlap(edits: &[Edit], path: &Path) -> Result<(), EngineError> {
    let spans = edit_spans(edits, path);
    for (left_index, (left_start, left_end, _)) in spans.iter().enumerate() {
        for (right_start, right_end, right_edit) in spans.iter().skip(left_index + 1) {
            if left_start.max(right_start) < left_end.min(right_end) {
                let left_edit = spans[left_index].2;
                return Err(EngineError::new(
                    ErrorClass::Resolve,
                    format!(
                        "patch: changes[{left_edit}] and changes[{right_edit}] overlap in {}. Merge them into one change.",
                        display_path(path)
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn edit_spans(edits: &[Edit], path: &Path) -> Vec<(usize, usize, usize)> {
    let mut spans = Vec::new();
    for edit in edits {
        let (target, start, end) = match edit {
            Edit::Change {
                path: edit_path,
                locator,
                index,
                ..
            } if edit_path == path => match locator {
                super::ir::Locator::Lines { first, last }
                | super::ir::Locator::Span { first, last, .. } => {
                    (*index, *first, last.saturating_add(1))
                }
                super::ir::Locator::Gap { before_line } => (*index, *before_line, *before_line),
                super::ir::Locator::Node { first_line } => {
                    (*index, *first_line, first_line.saturating_add(1))
                }
                super::ir::Locator::Whole => (*index, 1, usize::MAX / 2),
                super::ir::Locator::Text { .. } | super::ir::Locator::Symbol { .. } => continue,
            },
            _ => continue,
        };
        spans.push((start, end, target));
    }
    spans
}

fn outside_workspace(raw: &Path, workspace: &Path) -> EngineError {
    EngineError::new(
        ErrorClass::File,
        format!(
            "patch: {} is outside the workspace {}. patch changes files only inside the workspace.",
            display_path(raw),
            workspace.display()
        ),
    )
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
