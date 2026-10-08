//! Staging: guards, proofs, and before/after image construction.

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use super::super::{
    ir::{
        Action, DiffHunk, DiffLine, DiffLineKind, Edit, EngineError, ErrorClass, Guard, Locator,
        Operation, StagedFileOwned,
    },
    resolve::{decode, line_count},
};

use super::{PatchSession, ensure_regular_target, non_regular_target};

pub(crate) async fn stage_replacement(
    display: &Path,
    canonical: &Path,
    before_expected: &[u8],
    after_bytes: &[u8],
    line: u32,
) -> Result<StagedFileOwned, EngineError> {
    if line == 0 || before_expected.is_empty() {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!(
                "patch: {}:{line}: before must be non-empty and line must be at least 1.",
                display.display()
            ),
        ));
    }
    let before = read_target(canonical, display).await?;
    let _ = decode(display, &before)?;
    let target_line = usize::try_from(line).unwrap_or(usize::MAX);
    let mut line_start = 0_usize;
    let mut line_exists = !before.is_empty();
    for _ in 1..target_line {
        let Some(relative) = before[line_start..].iter().position(|byte| *byte == b'\n') else {
            line_exists = false;
            break;
        };
        line_start = line_start.saturating_add(relative + 1);
        if line_start >= before.len() {
            line_exists = false;
            break;
        }
    }
    let line_end = before[line_start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(before.len(), |relative| line_start + relative);
    let mut found = None;
    if line_exists && line_start < before.len() && before_expected.len() <= before.len() {
        let last_start = line_end.min(before.len() - before_expected.len());
        for start in line_start..=last_start {
            if before[start] == before_expected[0]
                && before[start..start + before_expected.len()] == *before_expected
            {
                found = Some((start, start + before_expected.len()));
                break;
            }
        }
    }
    let Some((start, end)) = found else {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!(
                "patch: {}:{line}: before does not match the current file bytes.",
                display.display()
            ),
        ));
    };
    let mut after = Vec::with_capacity(
        before
            .len()
            .saturating_sub(end.saturating_sub(start))
            .saturating_add(after_bytes.len()),
    );
    after.extend_from_slice(&before[..start]);
    after.extend_from_slice(after_bytes);
    after.extend_from_slice(&before[end..]);
    std::str::from_utf8(&after).map_err(|_| {
        EngineError::new(
            ErrorClass::File,
            format!(
                "patch: {} replacement result is not valid UTF-8.",
                display.display()
            ),
        )
    })?;
    let hunks = diff_hunks(&before, &after);
    Ok(StagedFileOwned {
        path: display.to_path_buf(),
        absolute_path: canonical.to_path_buf(),
        before: Some(before.into_boxed_slice()),
        after: Some(after.into_boxed_slice()),
        op: Operation::Update,
        renamed_to: None,
        hunks,
    })
}

/// Inputs shared by every change-locator planner: the session's proof
/// ledgers, the dialect, the staged file's paths, and its decoded
/// before-image.
struct ChangeCx<'a> {
    session: &'a PatchSession,
    style: super::super::ir::DialectId,
    display: &'a Path,
    canonical: &'a Path,
    before: &'a [u8],
    text: &'a super::super::resolve::Text,
}

/// How one `Edit` sequence stages: create and delete produce the staged
/// file directly; content edits plan byte replacements per locator.
enum StagePlan {
    Ready(StagedFileOwned),
    Changes {
        content_edits: Vec<Edit>,
        rename_to: Option<PathBuf>,
    },
}
pub(crate) async fn stage_file(
    session: &PatchSession,
    style: super::super::ir::DialectId,
    display: &Path,
    canonical: &Path,
    edits: Vec<Edit>,
) -> Result<StagedFileOwned, EngineError> {
    let (content_edits, rename_to) =
        match classify_edits(session, style, display, canonical, edits).await? {
            StagePlan::Ready(staged) => return Ok(staged),
            StagePlan::Changes {
                content_edits,
                rename_to,
            } => (content_edits, rename_to),
        };
    let before = read_target(canonical, display).await?;
    let text = decode(display, &before)?;
    let mut after = before.clone();
    let cx = ChangeCx {
        session,
        style,
        display,
        canonical,
        before: &before,
        text: &text,
    };
    // Apply content edits in descending view-offset order; here lines only.
    // Collect line replacements/insertions first, then splice once.
    let mut replacements: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    for edit in &content_edits {
        if let Edit::Change {
            locator,
            action,
            guard,
            body,
            index,
            ..
        } = edit
        {
            replacements
                .extend(plan_change_edit(&cx, locator, *action, guard, body, *index).await?);
        }
    }
    replacements.sort_by_key(|replacement| std::cmp::Reverse(replacement.0));
    for (start, end, replacement) in replacements {
        after.splice(start..end, replacement);
    }
    let op = if rename_to.is_some() {
        Operation::Rename
    } else {
        Operation::Update
    };
    let hunks = if style == super::super::ir::DialectId::Replace {
        diff_hunks(&before, &after)
    } else {
        Vec::new()
    };
    Ok(StagedFileOwned {
        path: display.to_path_buf(),
        absolute_path: canonical.to_path_buf(),
        before: Some(before.into_boxed_slice()),
        after: Some(after.into_boxed_slice()),
        op,
        renamed_to: rename_to,
        hunks,
    })
}

/// Classifies the edit list: exactly one rename, last, with optional prior
/// content edits; create and delete stage the file outright.
async fn classify_edits(
    session: &PatchSession,
    style: super::super::ir::DialectId,
    display: &Path,
    canonical: &Path,
    edits: Vec<Edit>,
) -> Result<StagePlan, EngineError> {
    let mut rename_to: Option<PathBuf> = None;
    let mut content_edits = Vec::new();
    for edit in edits {
        match edit {
            Edit::Rename { to, reference, .. } => {
                if rename_to.is_some() {
                    return Err(EngineError::new(
                        ErrorClass::Resolve,
                        "patch: overlapping rename in one file.".to_owned(),
                    ));
                }
                if let Some(reference) = reference {
                    prove_reference(
                        session,
                        style,
                        canonical,
                        display,
                        &super::super::ir::Locator::Gap { before_line: 0 },
                        0,
                        &reference,
                    )
                    .await?;
                }
                rename_to = Some(to);
            }
            Edit::Create { path, body, .. } => {
                if let Ok(metadata) = tokio::fs::metadata(canonical).await {
                    if !metadata.is_file() {
                        return Err(non_regular_target(display));
                    }
                    return Err(EngineError::new(
                        ErrorClass::File,
                        format!("patch: {} already exists.", display.display()),
                    ));
                }
                let _ = (style, path);
                return Ok(StagePlan::Ready(StagedFileOwned {
                    path: display.to_path_buf(),
                    absolute_path: canonical.to_path_buf(),
                    before: None,
                    after: Some(body.into_bytes().into_boxed_slice()),
                    op: Operation::Create,
                    renamed_to: None,
                    hunks: Vec::new(),
                }));
            }
            Edit::Delete { reference, .. } => {
                if let Some(reference) = reference {
                    prove_reference(
                        session,
                        style,
                        canonical,
                        display,
                        &super::super::ir::Locator::Gap { before_line: 0 },
                        0,
                        &reference,
                    )
                    .await?;
                }
                let before = read_target(canonical, display).await?;
                return Ok(StagePlan::Ready(StagedFileOwned {
                    path: display.to_path_buf(),
                    absolute_path: canonical.to_path_buf(),
                    before: Some(before.into_boxed_slice()),
                    after: None,
                    op: Operation::Delete,
                    renamed_to: rename_to.clone(),
                    hunks: Vec::new(),
                }));
            }
            Edit::Change { .. } => content_edits.push(edit),
        }
    }
    Ok(StagePlan::Changes {
        content_edits,
        rename_to,
    })
}

/// Maps one `Change` edit's locator to raw byte-span replacements against
/// the decoded before-image; callers splice in descending offset order.
async fn plan_change_edit(
    cx: &ChangeCx<'_>,
    locator: &Locator,
    action: Action,
    guard: &Guard,
    body: &str,
    index: usize,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    prove_guard(
        cx.session,
        cx.style,
        cx.display,
        cx.canonical,
        cx.before,
        cx.text,
        guard,
        locator,
        index,
    )
    .await?;
    match locator {
        Locator::Lines { first, last } => lines_replacement(cx, *first, *last, action, index, body),
        Locator::Gap { before_line } => gap_replacement(cx, *before_line, index, body),
        Locator::Whole => Ok(vec![(0, cx.before.len(), render_body(body, cx.text))]),
        Locator::Text {
            old,
            line_hint,
            all,
            ..
        } => text_replacements(cx, (old, *line_hint, *all), action, guard, index, body),
        Locator::Span {
            first,
            last,
            quoted: _,
        } => span_replacements(cx, *first, *last, action, index, body),
        Locator::Node { first_line } => {
            node_replacements(cx, *first_line, action, guard, index, body).await
        }
        Locator::Symbol { name, ordinal, old } => {
            symbol_replacements(cx, name, *ordinal, old.as_ref(), action, index, body).await
        }
    }
}

/// `Locator::Lines`: a line range must exist inside the file and becomes
/// one replacement, insertion, or append span.
fn lines_replacement(
    cx: &ChangeCx<'_>,
    first: usize,
    last: usize,
    action: Action,
    index: usize,
    body: &str,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    if first == 0 || last == 0 || first > last {
        return Err(resolve_error(cx.style, cx.display, index, "invalid range"));
    }
    let count = line_count(cx.text);
    if last > count {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!("patch: line {last} does not exist (file has {count} lines)"),
        ));
    }
    let (start, end) = line_byte_range(cx.text, first, last);
    let replacement = render_body(body, cx.text);
    Ok(vec![match action {
        Action::Replace => (start, end, replacement),
        Action::InsertBefore => (start, start, replacement),
        Action::InsertAfter => (end, end, replacement),
    }])
}

/// `Locator::Gap`: a position boundary becomes a zero-width insertion.
fn gap_replacement(
    cx: &ChangeCx<'_>,
    before_line: usize,
    _index: usize,
    body: &str,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    let count = line_count(cx.text);
    if before_line != usize::MAX && before_line > count.saturating_add(1) {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!("patch: line {before_line} does not exist (file has {count} lines)"),
        ));
    }
    let offset = if before_line == usize::MAX {
        cx.before.len()
    } else if before_line == 0 {
        0
    } else {
        line_byte_range(cx.text, before_line, before_line)
            .0
            .min(cx.before.len())
    };
    Ok(vec![(offset, offset, render_body(body, cx.text))])
}

/// Maps a view offset back to raw bytes: removed CR columns and the BOM.
fn raw_offset(text: &super::super::resolve::Text, offset: usize) -> usize {
    let removed = text
        .removed_cr_offsets
        .partition_point(|position| *position < offset);
    offset + removed + if text.bom { 3 } else { 0 }
}

/// Replace-all needs a whole-tag guard or Seen coverage of every line.
fn require_whole_file_coverage(
    cx: &ChangeCx<'_>,
    guard: &Guard,
    index: usize,
) -> Result<(), EngineError> {
    if !matches!(guard, Guard::Seen) {
        return Ok(());
    }
    let count = super::super::resolve::line_count(cx.text);
    let digest = *blake3::hash(cx.before).as_bytes();
    let path_str = cx.display.to_string_lossy().replace('\\', "/");
    if cx
        .session
        .seen
        .covers(cx.session.session, &path_str, digest, 1, count as u64)
    {
        return Ok(());
    }
    Err(EngineError::new(
        ErrorClass::Proof,
        format!(
            "patch: changes[{index}]: all needs tag. Read the whole file with read and copy the tag from its last line."
        ),
    ))
}

/// `Locator::Text`: match `old` against the view; unique, hinted by
/// `line_hint`, or every match under `all` with Seen coverage.
fn text_replacements(
    cx: &ChangeCx<'_>,
    needle: (&str, Option<usize>, bool),
    action: Action,
    guard: &Guard,
    index: usize,
    body: &str,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    let (old, line_hint, all) = needle;
    let view = String::from_utf8_lossy(&cx.text.view).into_owned();
    let found = find_text_match_set(&view, old);
    let matches = &found.spans;
    if matches.is_empty() {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!(
                "patch: changes[{index}]: old is not in {}. Copy it again from the lines below, or read {}.",
                cx.display.display(),
                cx.display.display()
            ),
        ));
    }
    let stripped_body = found.prefix_start.and_then(|start| {
        let normalized_body = normalize_crlf(body);
        strip_numbered_prefixes(&normalized_body)
            .filter(|(body_start, _)| *body_start == start)
            .map(|(_, body)| body)
    });
    let replacement = render_body(stripped_body.as_deref().unwrap_or(body), cx.text);
    let planned = |start: usize, end: usize| match action {
        Action::Replace => (raw_offset(cx.text, start), raw_offset(cx.text, end)),
        Action::InsertBefore => {
            let at = raw_offset(cx.text, start);
            (at, at)
        }
        Action::InsertAfter => {
            let at = raw_offset(cx.text, end);
            (at, at)
        }
    };
    if all {
        require_whole_file_coverage(cx, guard, index)?;
        return Ok(matches
            .iter()
            .rev()
            .map(|(start, end)| {
                let (start, end) = planned(*start, *end);
                (start, end, replacement.clone())
            })
            .collect());
    }
    if let Some(hint) = line_hint {
        let selected = hint
            .checked_sub(1)
            .and_then(|line| cx.text.line_starts.get(line).copied())
            .and_then(|line_start| {
                let line_end = cx
                    .text
                    .line_starts
                    .get(hint)
                    .copied()
                    .unwrap_or(cx.text.view.len());
                matches
                    .iter()
                    .find(|(start, _)| *start >= line_start && *start < line_end)
            });
        return match selected {
            Some((start, end)) => {
                let (start, end) = planned(*start, *end);
                Ok(vec![(start, end, replacement)])
            }
            None => Err(EngineError::new(
                ErrorClass::Resolve,
                format!(
                    "patch: changes[{index}]: old occurs {} times in {}, but none starts at line {hint}.",
                    matches.len(),
                    cx.display.display()
                ),
            )),
        };
    }
    if matches.len() > 1 {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!(
                "patch: changes[{index}]: old occurs {} times in {}. Resend with line set to one of these start lines.",
                matches.len(),
                cx.display.display()
            ),
        ));
    }
    let (start, end) = matches[0];
    let (start, end) = planned(start, end);
    Ok(vec![(start, end, replacement)])
}

/// `Locator::Span`: an explicit line range needs Seen coverage of its
/// whole span before it becomes one replacement span.
fn span_replacements(
    cx: &ChangeCx<'_>,
    first: usize,
    last: usize,
    action: Action,
    index: usize,
    body: &str,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    let count = super::super::resolve::line_count(cx.text);
    if first == 0 || last == 0 || first > last || last > count {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!("patch: line {last} does not exist (file has {count} lines)"),
        ));
    }
    let digest = *blake3::hash(cx.before).as_bytes();
    let path_str = cx.display.to_string_lossy().replace('\\', "/");
    if !cx.session.seen.covers(
        cx.session.session,
        &path_str,
        digest,
        first as u64,
        last as u64,
    ) {
        return Err(EngineError::new(
            ErrorClass::Proof,
            format!(
                "patch: changes[{index}]: lines {first}-{last} of {} were not displayed in this session.",
                cx.display.display()
            ),
        ));
    }
    let (start, end) = line_byte_range(cx.text, first, last);
    let replacement = render_body(body, cx.text);
    Ok(vec![match action {
        Action::Replace => (start, end, replacement),
        Action::InsertBefore => (start, start, replacement),
        Action::InsertAfter => (end, end, replacement),
    }])
}

#[cfg(not(feature = "symbols"))]
fn symbols_disabled(display: &Path, index: usize) -> EngineError {
    let _ = display;
    EngineError::new(
        ErrorClass::Resolve,
        format!("patch: changes[{index}]: symbol support is not enabled for this file."),
    )
}

/// `Locator::Node`: the AST node footprint maps to one replacement span,
/// proven by Enhanced snapshot coverage or the Seen ledger.
#[cfg(feature = "symbols")]
async fn node_replacements(
    cx: &ChangeCx<'_>,
    first_line: usize,
    action: Action,
    guard: &Guard,
    index: usize,
    body: &str,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    if !cx.session.symbols {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!("patch: changes[{index}]: symbol support is not enabled for this file."),
        ));
    }
    let (_byte_start, _byte_end, node_first, node_last) = super::super::ast::node_span(
        cx.canonical,
        cx.before,
        u32::try_from(first_line).unwrap_or(u32::MAX),
    )
    .await?;
    let (start, end) = line_byte_range(cx.text, node_first as usize, node_last as usize);
    let replacement = render_body(body, cx.text);
    node_coverage(cx, guard, index, node_first, node_last)?;
    Ok(vec![match action {
        Action::Replace => (start, end, replacement),
        Action::InsertBefore => (start, start, replacement),
        Action::InsertAfter => (end, end, replacement),
    }])
}

#[cfg(not(feature = "symbols"))]
async fn node_replacements(
    cx: &ChangeCx<'_>,
    _first_line: usize,
    _action: Action,
    _guard: &Guard,
    index: usize,
    _body: &str,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    Err(symbols_disabled(cx.display, index))
}

/// The Node arm's coverage proof: Enhanced Reference guards prove against
/// the bound snapshot ledger with the frozen cutoff; Light is
/// observation-optional (reference identity + digest continuity, no
/// cutoff); legacy Node guards stay Seen-backed.
#[cfg(feature = "symbols")]
fn node_coverage(
    cx: &ChangeCx<'_>,
    guard: &Guard,
    index: usize,
    node_first: u32,
    node_last: u32,
) -> Result<(), EngineError> {
    let enhanced_reference = if cx.style == super::super::ir::DialectId::HashlineEnhanced {
        match guard {
            Guard::Reference(token) => Some(token),
            _ => None,
        }
    } else {
        None
    };
    if let Some(token) = enhanced_reference {
        let Some(reference) = super::super::snapshot::ReadRef::parse(token) else {
            return Err(EngineError::new(
                ErrorClass::Proof,
                format!(
                    "patch: unknown reference {token} for {}. Read again.",
                    cx.display.display()
                ),
            ));
        };
        let Some(cutoff) = cx.session.cutoff else {
            return Err(EngineError::new(
                ErrorClass::Blocked,
                "patch: observation_unavailable: Enhanced needs a reliable delivery boundary."
                    .to_owned(),
            ));
        };
        if !cx.session.snapshots.covers(
            cx.session.session,
            cx.session.consumer,
            reference,
            u64::from(node_first),
            u64::from(node_last),
            cutoff,
        ) {
            return Err(EngineError::new(
                ErrorClass::Proof,
                format!(
                    "patch: changes[{index}]: lines {node_first}-{node_last} of {} need prior observation. Read them, then resend.",
                    cx.display.display()
                ),
            ));
        }
    }
    let digest = *blake3::hash(cx.before).as_bytes();
    let path_str = cx.display.to_string_lossy().replace('\\', "/");
    if matches!(guard, Guard::Version(_) | Guard::Seen)
        && !cx.session.seen.covers(
            cx.session.session,
            &path_str,
            digest,
            u64::from(node_first),
            u64::from(node_last),
        )
    {
        return Err(EngineError::new(
            ErrorClass::Proof,
            format!(
                "patch: changes[{index}]: lines {node_first}-{node_last} of {} were not displayed in this session.",
                cx.display.display()
            ),
        ));
    }
    Ok(())
}

/// `Locator::Symbol`: the named definition maps to one replacement span;
/// `old` inside it narrows to a unique byte match.
#[cfg(feature = "symbols")]
async fn symbol_replacements(
    cx: &ChangeCx<'_>,
    name: &str,
    ordinal: Option<usize>,
    old: Option<&String>,
    action: Action,
    index: usize,
    body: &str,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    if !cx.session.symbols {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!("patch: changes[{index}]: symbol support is not enabled for this file."),
        ));
    }
    let (def_start, def_end, _) =
        super::super::ast::symbol_span(cx.canonical, cx.before, name, ordinal).await?;
    let def_end = def_end.min(cx.before.len());
    let replacement = render_body(body, cx.text);
    Ok(vec![match action {
        Action::Replace => {
            if let Some(old) = old {
                let span = &cx.before[def_start..def_end];
                let positions = needle_positions(span, old.as_bytes());
                if positions.is_empty() {
                    return Err(EngineError::new(
                        ErrorClass::Resolve,
                        format!(
                            "patch: changes[{index}]: old is not in {name} in {}. Copy it again from the lines below, or read {}.",
                            cx.display.display(),
                            cx.display.display()
                        ),
                    ));
                }
                if positions.len() > 1 {
                    return Err(EngineError::new(
                        ErrorClass::Resolve,
                        format!(
                            "patch: changes[{index}]: old occurs {} times in {name} in {}. Resend with a longer unique fragment.",
                            positions.len(),
                            cx.display.display()
                        ),
                    ));
                }
                let at = def_start + positions[0];
                (at, at + old.len(), replacement)
            } else {
                (def_start, def_end, replacement)
            }
        }
        Action::InsertBefore => (def_start, def_start, replacement),
        Action::InsertAfter => (def_end, def_end, replacement),
    }])
}

#[cfg(not(feature = "symbols"))]
async fn symbol_replacements(
    cx: &ChangeCx<'_>,
    _name: &str,
    _ordinal: Option<usize>,
    _old: Option<&String>,
    _action: Action,
    index: usize,
    _body: &str,
) -> Result<Vec<(usize, usize, Vec<u8>)>, EngineError> {
    Err(symbols_disabled(cx.display, index))
}

/// All non-overlapping positions of `needle` inside `span`.
#[cfg(feature = "symbols")]
fn needle_positions(span: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut cursor = 0;
    while cursor + needle.len() <= span.len() {
        match span[cursor..]
            .windows(needle.len())
            .position(|window| window == needle)
        {
            Some(offset) => {
                positions.push(cursor + offset);
                cursor += offset + needle.len().max(1);
            }
            None => break,
        }
    }
    positions
}

fn diff_hunks(before: &[u8], after: &[u8]) -> Vec<DiffHunk> {
    let (Ok(before), Ok(after)) = (std::str::from_utf8(before), std::str::from_utf8(after)) else {
        return Vec::new();
    };
    let diff = similar::TextDiff::from_lines(before, after);
    diff.grouped_ops(3)
        .into_iter()
        .filter_map(|ops| {
            let first = ops.first()?;
            let last = ops.last()?;
            let old_start = first.old_range().start;
            let old_end = last.old_range().end;
            let new_start = first.new_range().start;
            let new_end = last.new_range().end;
            let lines = ops
                .iter()
                .flat_map(|op| diff.iter_changes(op))
                .map(|change| {
                    let kind = match change.tag() {
                        similar::ChangeTag::Equal => DiffLineKind::Context,
                        similar::ChangeTag::Delete => DiffLineKind::Removed,
                        similar::ChangeTag::Insert => DiffLineKind::Added,
                    };
                    let text = change.value().trim_end_matches(['\r', '\n']);
                    DiffLine {
                        kind,
                        text: text.into(),
                    }
                })
                .collect();
            Some(DiffHunk {
                old_start: old_start.saturating_add(1),
                old_lines: old_end.saturating_sub(old_start),
                new_start: new_start.saturating_add(1),
                new_lines: new_end.saturating_sub(new_start),
                lines,
            })
        })
        .collect()
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one proof carries the session, path pair, and locator inputs"
)]
async fn prove_guard(
    session: &PatchSession,
    style: super::super::ir::DialectId,
    display: &Path,
    canonical: &Path,
    before: &[u8],
    text: &super::super::resolve::Text,
    guard: &Guard,
    locator: &Locator,
    index: usize,
) -> Result<(), EngineError> {
    match guard {
        Guard::Quoted => {
            // Quoted bytes must be present; Text locators verify at stage time.
            // Whole/span locators with Quoted are validated by their own proof.
            Ok(())
        }
        Guard::Seen => {
            // Seen coverage is checked per-locator at stage time for Whole/all.
            // Gap insertions remove nothing and need no coverage here.
            if matches!(locator, Locator::Gap { .. }) {
                return Ok(());
            }
            if matches!(locator, Locator::Whole) {
                let count = super::super::resolve::line_count(text);
                if count > 0 {
                    let digest = *blake3::hash(before).as_bytes();
                    let path_str = display.to_string_lossy().replace('\\', "/");
                    if !session
                        .seen
                        .covers(session.session, &path_str, digest, 1, count as u64)
                    {
                        return Err(EngineError::new(
                            ErrorClass::Proof,
                            format!(
                                "patch: changes[{index}]: replacing the whole file needs tag. Read the whole file with read and copy the tag from its last line."
                            ),
                        ));
                    }
                }
            }
            Ok(())
        }
        Guard::WholeTag(expected) | Guard::Version(expected) => {
            // Legacy tags: Whole is 8 hex, Version is 4 hex.
            let whole = crate::tag8("whole", before);
            let version = format!("{:.4}", crate::tag8("version", before));
            // New-profile references are `r<boot>.<seq>`; they never match a hex tag.
            if expected == &whole || expected == &version {
                return Ok(());
            }
            // Stale-write rejection names the current digest/tag.
            let current = if expected.len() == 4 { version } else { whole };
            let _ = (session, style, index);
            Err(EngineError::new(
                ErrorClass::Stale,
                format!(
                    "patch: changes[{index}]: tag {expected} does not match {} now (tag {current}). The file changed since you read it; read it again.",
                    display.display()
                ),
            ))
        }
        Guard::Reference(token) => {
            prove_reference(session, style, canonical, display, locator, index, token).await
        }
        Guard::DefTag(expected) => {
            // Definition-tag proof resolves the named definition through the
            // single parser service and compares its current tag.
            #[cfg(feature = "symbols")]
            {
                if !session.symbols {
                    return Err(EngineError::new(
                        ErrorClass::Resolve,
                        format!(
                            "patch: changes[{index}]: symbol support is not enabled for this file."
                        ),
                    ));
                }
                match locator {
                    Locator::Symbol { name, ordinal, .. } => {
                        let (_, _, current) =
                            super::super::ast::symbol_span(canonical, before, name, *ordinal)
                                .await?;
                        if expected.is_empty() || expected == &current {
                            return Ok(());
                        }
                        Err(EngineError::new(
                            ErrorClass::Stale,
                            format!(
                                "patch: changes[{index}]: tag {expected} does not match {name} now (tag {current}). The file changed since you read it; read it again."
                            ),
                        ))
                    }
                    _ => Err(EngineError::new(
                        ErrorClass::Resolve,
                        format!("patch: changes[{index}]: definition tag needs a symbol selector."),
                    )),
                }
            }
            #[cfg(not(feature = "symbols"))]
            {
                let _ = expected;
                return Err(EngineError::new(
                    ErrorClass::Resolve,
                    format!(
                        "patch: changes[{index}]: symbol support is not enabled for this file."
                    ),
                ));
            }
        }
        Guard::Absent => {
            if std::fs::metadata(session.workspace.join(display)).is_ok() {
                return Err(EngineError::new(
                    ErrorClass::File,
                    format!("patch: {} already exists.", display.display()),
                ));
            }
            Ok(())
        }
        Guard::Exists => {
            if std::fs::metadata(session.workspace.join(display)).is_err() {
                return Err(EngineError::new(
                    ErrorClass::File,
                    format!("patch: {} does not exist.", display.display()),
                ));
            }
            Ok(())
        }
    }
}

async fn prove_reference(
    session: &PatchSession,
    style: super::super::ir::DialectId,
    canonical: &Path,
    display: &Path,
    locator: &Locator,
    index: usize,
    token: &str,
) -> Result<(), EngineError> {
    ensure_regular_target(canonical, display).await?;
    let reference = super::super::snapshot::ReadRef::parse(token).ok_or_else(|| {
        EngineError::new(
            ErrorClass::Proof,
            format!(
                "patch: unknown reference {token} for {}. Read again.",
                display.display()
            ),
        )
    })?;
    let snapshot = session
        .snapshots
        .lookup(session.session, session.generation, session.consumer, reference, display)
        .ok_or_else(|| {
            EngineError::new(
                ErrorClass::Stale,
                format!(
                    "patch: reference {} for {} is expired or bound to another session. Read again.",
                    token,
                    display.display()
                ),
            )
        })?;
    // Digest continuity: current workspace bytes must equal the captured snapshot.
    // Snapshot.path is workspace-relative; always resolve against the session workspace.
    let current = tokio::fs::read(canonical).await.unwrap_or_default();
    if blake3::hash(&current).as_bytes() != &snapshot.digest {
        return Err(EngineError::new(
            ErrorClass::Stale,
            format!(
                "patch: {} changed since reference {}. Read again.",
                display.display(),
                token
            ),
        ));
    }
    // Profile-specific coverage: Light waives removed-line coverage;
    // Enhanced requires every cell in the destructive footprint delivered
    // at or before the frozen request cutoff (E-04, INV-CUTOFF).
    if style == super::super::ir::DialectId::HashlineEnhanced {
        let footprint = destructive_footprint(locator);
        if let Some((first, last)) = footprint {
            // Cutoff is u64::MAX until host cutoffs land; deliveries are still
            // consumer-scoped and reference-bound, so future rows cannot leak
            // across references. Host cutoff wiring is an open seam.
            let Some(cutoff) = session.cutoff else {
                return Err(EngineError::new(
                    ErrorClass::Blocked,
                    "patch: observation_unavailable: Enhanced needs a reliable delivery boundary."
                        .to_owned(),
                ));
            };
            if !session.snapshots.covers(
                session.session,
                session.consumer,
                reference,
                first as u64,
                last as u64,
                cutoff,
            ) {
                return Err(EngineError::new(
                    ErrorClass::Proof,
                    format!(
                        "patch: changes[{index}]: lines {first}-{last} of {} need prior observation. Read them, then resend.",
                        display.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn destructive_footprint(locator: &Locator) -> Option<(usize, usize)> {
    // Returns one-based inclusive footprint for replacements/deletions;
    // pure-gap insertions remove no cells and need no coverage.
    // Node spans resolve at stage time: the `Locator::Node` arm maps the
    // full source-node footprint via the parser and enforces Enhanced
    // snapshot coverage there, so no static footprint is returned here.
    match locator {
        Locator::Lines { first, last } => Some((*first, *last)),
        Locator::Node { .. }
        | Locator::Text { .. }
        | Locator::Span { .. }
        | Locator::Whole
        | Locator::Gap { .. }
        | Locator::Symbol { .. } => None,
    }
}

async fn read_target(canonical: &Path, display: &Path) -> Result<Vec<u8>, EngineError> {
    ensure_regular_target(canonical, display).await?;
    tokio::fs::read(canonical).await.map_err(|error| {
        EngineError::new(
            ErrorClass::File,
            format!("patch: {} could not be read: {error}.", display.display()),
        )
    })
}

fn line_byte_range(
    text: &super::super::resolve::Text,
    first: usize,
    last: usize,
) -> (usize, usize) {
    let starts = &text.line_starts;
    if starts.is_empty() {
        return (0, 0);
    }
    let start = starts
        .get(first.saturating_sub(1))
        .copied()
        .unwrap_or(text.view.len());
    let end = if last < starts.len() {
        starts[last]
    } else {
        text.view.len()
    };
    // Map view offsets back through removed CRs (view lacks CR before LF).
    let map = |offset: usize| {
        let removed = text
            .removed_cr_offsets
            .partition_point(|position| *position < offset);
        offset + removed + if text.bom { 3 } else { 0 }
    };
    (map(start), map(end))
}

fn render_body(body: &str, text: &super::super::resolve::Text) -> Vec<u8> {
    // Convert inserted LF to the file's dominant ending; preserve BOM state
    // in the surrounding bytes (untouched bytes are never rewritten here).
    if text.dominant_crlf {
        body.replace('\n', "\r\n").into_bytes()
    } else {
        body.as_bytes().to_vec()
    }
}

#[derive(Debug)]
struct TextMatchSet {
    spans: Vec<(usize, usize)>,
    prefix_start: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct LineSpan {
    start: usize,
    content_end: usize,
    end: usize,
}

#[cfg(test)]
static MATCH_COMPARISONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[inline]
fn note_match_comparison() {
    #[cfg(test)]
    MATCH_COMPARISONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn find_text_match_set(haystack: &str, needle: &str) -> TextMatchSet {
    if needle.is_empty() {
        return TextMatchSet {
            spans: Vec::new(),
            prefix_start: None,
        };
    }
    let normalized = normalize_crlf(needle);
    let exact = byte_match_spans(haystack, &normalized);
    if !exact.is_empty() {
        return TextMatchSet {
            spans: exact,
            prefix_start: None,
        };
    }
    if line_spans(&normalized).len() > 1
        && let Some((prefix_start, stripped)) = strip_numbered_prefixes(&normalized)
    {
        let healed = byte_match_spans(haystack, &stripped);
        if !healed.is_empty() {
            return TextMatchSet {
                spans: healed,
                prefix_start: Some(prefix_start),
            };
        }
        let canonical = canonical_line_matches(haystack, &stripped);
        if !canonical.is_empty() {
            return TextMatchSet {
                spans: canonical,
                prefix_start: Some(prefix_start),
            };
        }
    }
    TextMatchSet {
        spans: canonical_line_matches(haystack, &normalized),
        prefix_start: None,
    }
}

fn byte_match_spans(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let starts = kmp_positions(haystack.as_bytes(), needle.as_bytes());
    starts
        .into_iter()
        .map(|start| (start, start + needle.len()))
        .collect()
}

fn kmp_positions<T: Eq>(haystack: &[T], needle: &[T]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let prefix = kmp_prefix(needle);
    let mut positions = Vec::new();
    let mut matched = 0_usize;
    for (index, value) in haystack.iter().enumerate() {
        loop {
            note_match_comparison();
            if *value == needle[matched] {
                matched += 1;
                break;
            }
            if matched == 0 {
                break;
            }
            matched = prefix[matched - 1];
        }
        if matched == needle.len() {
            positions.push(index + 1 - needle.len());
            matched = prefix[matched - 1];
        }
    }
    positions
}

fn kmp_prefix<T: Eq>(needle: &[T]) -> Vec<usize> {
    let mut prefix = vec![0; needle.len()];
    let mut matched = 0_usize;
    for index in 1..needle.len() {
        loop {
            note_match_comparison();
            if needle[index] == needle[matched] {
                matched += 1;
                break;
            }
            if matched == 0 {
                break;
            }
            matched = prefix[matched - 1];
        }
        prefix[index] = matched;
    }
    prefix
}

fn line_spans(value: &str) -> Vec<LineSpan> {
    let mut spans = Vec::new();
    let mut start = 0_usize;
    for (offset, byte) in value.as_bytes().iter().enumerate() {
        if *byte == b'\n' {
            spans.push(LineSpan {
                start,
                content_end: offset,
                end: offset + 1,
            });
            start = offset + 1;
        }
    }
    if start < value.len() {
        spans.push(LineSpan {
            start,
            content_end: value.len(),
            end: value.len(),
        });
    }
    spans
}

fn canonical_line_matches(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let haystack_lines = line_spans(haystack);
    let needle_lines = line_spans(needle);
    if needle_lines.is_empty() || needle_lines.len() > haystack_lines.len() {
        return Vec::new();
    }
    let haystack_keys: Vec<String> = haystack_lines
        .iter()
        .map(|span| canonical_line(&haystack[span.start..span.content_end]))
        .collect();
    let needle_keys: Vec<String> = needle_lines
        .iter()
        .map(|span| canonical_line(&needle[span.start..span.content_end]))
        .collect();
    let needle_has_final_newline = needle_lines
        .last()
        .is_some_and(|line| line.end > line.content_end);
    kmp_positions(&haystack_keys, &needle_keys)
        .into_iter()
        .filter_map(|start| {
            let first = haystack_lines[start];
            let last = haystack_lines[start + needle_lines.len() - 1];
            if needle_has_final_newline && last.end == last.content_end {
                return None;
            }
            let end = if needle_has_final_newline {
                last.end
            } else {
                last.content_end
            };
            Some((first.start, end))
        })
        .collect()
}

fn canonical_line(line: &str) -> String {
    let mut canonical = String::with_capacity(line.len());
    for character in line.chars() {
        let replacement = match character {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' => "'",
            '\u{201C}' | '\u{201D}' | '\u{201F}' | '\u{2033}' => "\"",
            '\u{2010}'..='\u{2015}' | '\u{2212}' => "-",
            '\u{00A0}' | '\u{2007}' | '\u{202F}' => " ",
            '\u{2026}' => "...",
            _ => {
                canonical.push(character);
                continue;
            }
        };
        canonical.push_str(replacement);
    }
    canonical.trim_end_matches([' ', '\t', '\r']).to_owned()
}

fn normalize_crlf(value: &str) -> Cow<'_, str> {
    if !value.as_bytes().contains(&b'\r') {
        return Cow::Borrowed(value);
    }
    Cow::Owned(value.replace("\r\n", "\n"))
}

fn strip_numbered_prefixes(value: &str) -> Option<(usize, String)> {
    let spans = line_spans(value);
    if spans.len() < 2 {
        return None;
    }
    let mut first = None;
    let mut expected = None;
    let mut stripped = String::with_capacity(value.len());
    for span in spans {
        let line = &value[span.start..span.content_end];
        let colon = line.as_bytes().iter().position(|byte| *byte == b':')?;
        if colon == 0 || !line.as_bytes()[..colon].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let number = line[..colon].parse::<usize>().ok()?;
        if let Some(next) = expected {
            if number != next {
                return None;
            }
        } else if first.is_some() {
            return None;
        } else {
            first = Some(number);
        }
        expected = number.checked_add(1);
        stripped.push_str(&line[colon + 1..]);
        if span.end > span.content_end {
            stripped.push('\n');
        }
    }
    let start = first?;
    Some((start, stripped))
}

#[cfg(test)]
fn find_text_matches(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    find_text_match_set(haystack, needle).spans
}
#[cfg(test)]
pub(crate) fn find_text_matches_linear_probe(haystack: &str, needle: &str) -> usize {
    MATCH_COMPARISONS.store(0, std::sync::atomic::Ordering::Relaxed);
    let _ = find_text_matches(haystack, needle);
    MATCH_COMPARISONS.load(std::sync::atomic::Ordering::Relaxed)
}

fn resolve_error(
    _style: super::super::ir::DialectId,
    display: &Path,
    index: usize,
    message: &str,
) -> EngineError {
    EngineError::new(
        ErrorClass::Resolve,
        format!(
            "patch: changes[{index}]: {message} in {}.",
            display.display()
        ),
    )
}
