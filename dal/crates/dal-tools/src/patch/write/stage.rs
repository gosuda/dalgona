//! Staging: guards, proofs, and before/after image construction.

use std::path::{Path, PathBuf};

use super::super::{
    ir::{
        Action, DiffHunk, DiffLine, DiffLineKind, Edit, EngineError, ErrorClass, Guard, Locator,
        Operation, StagedFileOwned,
    },
    resolve::{decode, line_count},
};

use super::PatchSession;

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

#[expect(
    clippy::too_many_lines,
    reason = "staging is one ordered pipeline: resolve, guard, verify, capture; seams would thread nine locals"
)]
pub(crate) async fn stage_file(
    session: &PatchSession,
    style: super::super::ir::DialectId,
    display: &Path,
    canonical: &Path,
    edits: Vec<Edit>,
) -> Result<StagedFileOwned, EngineError> {
    // Rename handling: exactly one rename, last, with optional prior content edits.
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
                        display,
                        &super::super::ir::Locator::Gap { before_line: 0 },
                        0,
                        &reference,
                    )?;
                }
                rename_to = Some(to);
            }
            Edit::Create { path, body, .. } => {
                if tokio::fs::metadata(canonical).await.is_ok() {
                    return Err(EngineError::new(
                        ErrorClass::File,
                        format!("patch: {} already exists.", display.display()),
                    ));
                }
                let _ = (style, path);
                return Ok(StagedFileOwned {
                    path: display.to_path_buf(),
                    absolute_path: canonical.to_path_buf(),
                    before: None,
                    after: Some(body.into_bytes().into_boxed_slice()),
                    op: Operation::Create,
                    renamed_to: None,
                    hunks: Vec::new(),
                });
            }
            Edit::Delete { reference, .. } => {
                if let Some(reference) = reference {
                    prove_reference(
                        session,
                        style,
                        display,
                        &super::super::ir::Locator::Gap { before_line: 0 },
                        0,
                        &reference,
                    )?;
                }
                let before = read_target(canonical, display).await?;
                return Ok(StagedFileOwned {
                    path: display.to_path_buf(),
                    absolute_path: canonical.to_path_buf(),
                    before: Some(before.into_boxed_slice()),
                    after: None,
                    op: Operation::Delete,
                    renamed_to: rename_to.clone(),
                    hunks: Vec::new(),
                });
            }
            Edit::Change { .. } => content_edits.push(edit),
        }
    }
    let before = read_target(canonical, display).await?;
    let text = decode(display, &before)?;
    let mut after = before.clone();
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
            prove_guard(
                session, style, display, canonical, &before, &text, guard, locator, *index,
            )
            .await?;
            match locator {
                Locator::Lines { first, last } => {
                    if *first == 0 || *last == 0 || first > last {
                        return Err(resolve_error(style, display, *index, "invalid range"));
                    }
                    let count = line_count(&text);
                    if *last > count {
                        return Err(EngineError::new(
                            ErrorClass::Resolve,
                            format!("patch: line {last} does not exist (file has {count} lines)"),
                        ));
                    }
                    let (start, end) = line_byte_range(&text, *first, *last);
                    let replacement = render_body(body, &text);
                    match action {
                        Action::Replace => replacements.push((start, end, replacement)),
                        Action::InsertBefore => replacements.push((start, start, replacement)),
                        Action::InsertAfter => replacements.push((end, end, replacement)),
                    }
                }
                Locator::Gap { before_line } => {
                    let count = line_count(&text);
                    if *before_line != usize::MAX && *before_line > count.saturating_add(1) {
                        return Err(EngineError::new(
                            ErrorClass::Resolve,
                            format!(
                                "patch: line {before_line} does not exist (file has {count} lines)"
                            ),
                        ));
                    }
                    let offset = if *before_line == usize::MAX {
                        after.len()
                    } else if *before_line == 0 {
                        0
                    } else {
                        line_byte_range(&text, *before_line, *before_line)
                            .0
                            .min(after.len())
                    };
                    let replacement = render_body(body, &text);
                    replacements.push((offset, offset, replacement));
                }
                Locator::Whole => {
                    let replacement = render_body(body, &text);
                    replacements.push((0, after.len(), replacement));
                }
                Locator::Text {
                    old,
                    line_hint,
                    all,
                    ..
                } => {
                    let view = String::from_utf8_lossy(&text.view).into_owned();
                    let matches = find_text_matches(&view, old);
                    if matches.is_empty() {
                        return Err(EngineError::new(
                            ErrorClass::Resolve,
                            format!(
                                "patch: changes[{index}]: old is not in {}. Copy it again from the lines below, or read {}.",
                                display.display(),
                                display.display()
                            ),
                        ));
                    }
                    if *all {
                        // Replace-all requires whole-tag or Seen coverage of every line.
                        if matches!(guard, Guard::Seen) {
                            let count = super::super::resolve::line_count(&text);
                            let digest = *blake3::hash(&before).as_bytes();
                            let path_str = display.to_string_lossy().replace('\\', "/");
                            if !session.seen.covers(
                                session.session,
                                &path_str,
                                digest,
                                1,
                                count as u64,
                            ) {
                                return Err(EngineError::new(
                                    ErrorClass::Proof,
                                    format!(
                                        "patch: changes[{index}]: all needs tag. Read the whole file with read and copy the tag from its last line."
                                    ),
                                ));
                            }
                        }
                        for (start, end) in matches.iter().rev() {
                            let map = |offset: usize| {
                                let removed = text
                                    .removed_cr_offsets
                                    .iter()
                                    .filter(|pos| **pos < offset)
                                    .count();
                                offset + removed + if text.bom { 3 } else { 0 }
                            };
                            let replacement = render_body(body, &text);
                            replacements.push((map(*start), map(*end), replacement));
                        }
                    } else if let Some(hint) = line_hint {
                        let selected = matches
                            .iter()
                            .find(|(start, _)| view[..*start].matches('\n').count() + 1 == *hint);
                        match selected {
                            Some((start, end)) => {
                                let replacement = render_body(body, &text);
                                replacements.push((*start, *end, replacement));
                            }
                            None => {
                                return Err(EngineError::new(
                                    ErrorClass::Resolve,
                                    format!(
                                        "patch: changes[{index}]: old occurs {} times in {}, but none starts at line {hint}.",
                                        matches.len(),
                                        display.display()
                                    ),
                                ));
                            }
                        }
                    } else if matches.len() > 1 {
                        return Err(EngineError::new(
                            ErrorClass::Resolve,
                            format!(
                                "patch: changes[{index}]: old occurs {} times in {}. Resend with line set to one of these start lines.",
                                matches.len(),
                                display.display()
                            ),
                        ));
                    } else {
                        let (start, end) = matches[0];
                        // Map view offsets to raw offsets for splicing.
                        let map = |offset: usize| {
                            let removed = text
                                .removed_cr_offsets
                                .iter()
                                .filter(|pos| **pos < offset)
                                .count();
                            offset + removed + if text.bom { 3 } else { 0 }
                        };
                        let replacement = render_body(body, &text);
                        replacements.push((map(start), map(end), replacement));
                    }
                }
                Locator::Span {
                    first,
                    last,
                    quoted,
                } => {
                    let count = super::super::resolve::line_count(&text);
                    if *first == 0 || *last == 0 || *first > *last || *last > count {
                        return Err(EngineError::new(
                            ErrorClass::Resolve,
                            format!("patch: line {last} does not exist (file has {count} lines)"),
                        ));
                    }
                    // Span replacement requires Seen coverage of its entire range.
                    {
                        let digest = *blake3::hash(&before).as_bytes();
                        let path_str = display.to_string_lossy().replace('\\', "/");
                        if !session.seen.covers(
                            session.session,
                            &path_str,
                            digest,
                            *first as u64,
                            *last as u64,
                        ) {
                            return Err(EngineError::new(
                                ErrorClass::Proof,
                                format!(
                                    "patch: changes[{index}]: lines {first}-{last} of {} were not displayed in this session.",
                                    display.display()
                                ),
                            ));
                        }
                    }
                    let _ = quoted;
                    let (start, end) = line_byte_range(&text, *first, *last);
                    let replacement = render_body(body, &text);
                    match action {
                        Action::Replace => replacements.push((start, end, replacement)),
                        Action::InsertBefore => replacements.push((start, start, replacement)),
                        Action::InsertAfter => replacements.push((end, end, replacement)),
                    }
                }
                Locator::Node { first_line } => {
                    #[cfg(feature = "symbols")]
                    {
                        let (byte_start, byte_end, node_first, node_last) =
                            super::super::ast::node_span(
                                canonical,
                                &before,
                                u32::try_from(*first_line).unwrap_or(u32::MAX),
                            )
                            .await?;
                        // Map the entire source node footprint; Enhanced requires
                        // coverage of the full effective span (C09).
                        let _ = (byte_start, byte_end);
                        let (start, end) =
                            line_byte_range(&text, node_first as usize, node_last as usize);
                        let replacement = render_body(body, &text);
                        match action {
                            Action::Replace => replacements.push((start, end, replacement)),
                            Action::InsertBefore => {
                                replacements.push((start, start, replacement));
                            }
                            Action::InsertAfter => replacements.push((end, end, replacement)),
                        }
                        // Record footprint for Enhanced coverage below via guard check.
                        // Fall through to coverage verification using mapped span.
                        {
                            // Enhanced Reference guards prove coverage against
                            // the bound snapshot ledger with the frozen cutoff.
                            // Light is observation-optional (OBS-01): reference
                            // identity + digest continuity only, no cutoff.
                            // Legacy Node guards stay Seen-backed.
                            let enhanced_reference =
                                if style == super::super::ir::DialectId::HashlineEnhanced {
                                    match guard {
                                        Guard::Reference(token) => Some(token),
                                        _ => None,
                                    }
                                } else {
                                    None
                                };
                            if let Some(token) = enhanced_reference {
                                let Some(reference) = super::super::snapshot::ReadRef::parse(token)
                                else {
                                    return Err(EngineError::new(
                                        ErrorClass::Proof,
                                        format!(
                                            "patch: unknown reference {token} for {}. Read again.",
                                            display.display()
                                        ),
                                    ));
                                };
                                let Some(cutoff) = session.cutoff else {
                                    return Err(EngineError::new(
                                        ErrorClass::Blocked,
                                        "patch: observation_unavailable: Enhanced needs a reliable delivery boundary.".to_owned(),
                                    ));
                                };
                                if !session.snapshots.covers(
                                    session.session,
                                    session.consumer,
                                    reference,
                                    u64::from(node_first),
                                    u64::from(node_last),
                                    cutoff,
                                ) {
                                    return Err(EngineError::new(
                                        ErrorClass::Proof,
                                        format!(
                                            "patch: changes[{index}]: lines {node_first}-{node_last} of {} need prior observation. Read them, then resend.",
                                            display.display()
                                        ),
                                    ));
                                }
                            }
                            let digest = *blake3::hash(&before).as_bytes();
                            let path_str = display.to_string_lossy().replace('\\', "/");
                            if matches!(guard, Guard::Version(_) | Guard::Seen)
                                && !session.seen.covers(
                                    session.session,
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
                                        display.display()
                                    ),
                                ));
                            }
                        }
                    }
                    #[cfg(not(feature = "symbols"))]
                    {
                        let _ = first_line;
                        return Err(EngineError::new(
                            ErrorClass::Resolve,
                            format!(
                                "patch: changes[{index}]: symbol support is not enabled for this file."
                            ),
                        ));
                    }
                }
                Locator::Symbol { name, ordinal, old } => {
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
                        let (def_start, def_end, _) =
                            super::super::ast::symbol_span(canonical, &before, name, *ordinal)
                                .await?;
                        let def_end = def_end.min(before.len());
                        let replacement = render_body(body, &text);
                        match action {
                            Action::Replace => {
                                if let Some(old) = old {
                                    let span = &before[def_start..def_end];
                                    let needle = old.as_bytes();
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
                                    if positions.is_empty() {
                                        return Err(EngineError::new(
                                            ErrorClass::Resolve,
                                            format!(
                                                "patch: changes[{index}]: old is not in {name} in {}. Copy it again from the lines below, or read {}.",
                                                display.display(),
                                                display.display()
                                            ),
                                        ));
                                    }
                                    if positions.len() > 1 {
                                        return Err(EngineError::new(
                                            ErrorClass::Resolve,
                                            format!(
                                                "patch: changes[{index}]: old occurs {} times in {name} in {}. Resend with a longer unique fragment.",
                                                positions.len(),
                                                display.display()
                                            ),
                                        ));
                                    }
                                    let at = def_start + positions[0];
                                    replacements.push((at, at + needle.len(), replacement));
                                } else {
                                    replacements.push((def_start, def_end, replacement));
                                }
                            }
                            Action::InsertBefore => {
                                replacements.push((def_start, def_start, replacement));
                            }
                            Action::InsertAfter => {
                                replacements.push((def_end, def_end, replacement));
                            }
                        }
                    }
                    #[cfg(not(feature = "symbols"))]
                    {
                        let _ = (name, ordinal, old);
                        return Err(EngineError::new(
                            ErrorClass::Resolve,
                            format!(
                                "patch: changes[{index}]: symbol support is not enabled for this file."
                            ),
                        ));
                    }
                }
            }
        }
    }
    replacements.sort_by_key(|left| std::cmp::Reverse(left.0));
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
    reason = "proof needs the session, dialect, paths, buffers, guard, locator, and edit index together"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one guard proof per Guard variant; arms share the stale-tag wording"
)]
async fn prove_guard(
    session: &PatchSession,
    style: super::super::ir::DialectId,
    display: &Path,
    #[cfg(feature = "symbols")] canonical: &Path,
    #[cfg(not(feature = "symbols"))] _canonical: &Path,
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
        Guard::Reference(token) => prove_reference(session, style, display, locator, index, token),
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

fn prove_reference(
    session: &PatchSession,
    style: super::super::ir::DialectId,
    display: &Path,
    locator: &Locator,
    index: usize,
    token: &str,
) -> Result<(), EngineError> {
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
    let current = std::fs::read(session.workspace.join(display)).unwrap_or_default();
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
            .iter()
            .filter(|pos| **pos < offset)
            .count();
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

fn find_text_matches(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    let mut start = 0;
    while let Some(index) = haystack[start..].find(needle) {
        let absolute = start + index;
        matches.push((absolute, absolute + needle.len()));
        start = absolute + 1;
        if start >= haystack.len() {
            break;
        }
    }
    matches
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
