//! Atomic commit: locks, temps, ordered operations, and truthful outcomes.

use std::{path::PathBuf, sync::Arc};

use super::super::ir::{
    Diff, DiffFile, EngineError, ErrorClass, FileChange, FindingSeverity, Output, Plan,
    StagedFileOwned,
};

use super::PatchSession;

fn lock_table()
-> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>> {
    static TABLE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Acquires the canonical write set in ascending byte order — sources and
/// rename destinations together — and re-checks every `before` digest under
/// those guards. Two applies cannot interleave differently, and approval
/// cannot go stale undetected.
async fn prepare_write_set(
    session: &PatchSession,
    plan: &Plan,
) -> Result<Vec<tokio::sync::OwnedMutexGuard<()>>, EngineError> {
    // Acquire the full canonical write set in ascending byte order and
    // re-check every before digest under those guards (stale-approval check).
    let mut ordered: Vec<&StagedFileOwned> = plan.files.iter().collect();
    ordered.sort_by(|left, right| {
        left.absolute_path
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.absolute_path.as_os_str().as_encoded_bytes())
    });
    // Lock sources and rename destinations together in one ascending order.
    let mut lock_paths: Vec<PathBuf> = Vec::with_capacity(ordered.len() * 2);
    for file in &ordered {
        lock_paths.push(file.absolute_path.clone());
        if file.op == super::super::ir::Operation::Rename
            && let Some(dest) = file.renamed_to.as_ref()
        {
            lock_paths.push(session.workspace.join(dest));
        }
    }
    lock_paths.sort_by(|left, right| {
        left.as_os_str()
            .as_encoded_bytes()
            .cmp(right.as_os_str().as_encoded_bytes())
    });
    lock_paths.dedup();
    let mut guards = Vec::with_capacity(lock_paths.len());
    for path in lock_paths {
        let guard = {
            let mut table = lock_table()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            table
                .entry(path)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        guards.push(guard.lock_owned().await);
    }
    for file in &ordered {
        if let Some(expected) = file.before.as_deref() {
            let current = tokio::fs::read(&file.absolute_path)
                .await
                .unwrap_or_default();
            if current != expected {
                drop(guards);
                return Err(EngineError::new(
                    ErrorClass::Stale,
                    format!(
                        "patch: {} changed while you were approving. Nothing was written. Try again.",
                        file.path.display()
                    ),
                ));
            }
        } else if tokio::fs::metadata(&file.absolute_path).await.is_ok() {
            drop(guards);
            return Err(EngineError::new(
                ErrorClass::File,
                format!("patch: {} already exists.", file.path.display()),
            ));
        }
    }
    Ok(guards)
}

pub(crate) async fn apply_files(
    session: &PatchSession,
    plan: &Plan,
) -> Result<Output, EngineError> {
    use std::collections::HashSet;
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let _guards = prepare_write_set(session, plan).await?;

    // Temps map target absolute path -> temp path. For renames the target
    // is the destination; the source is moved to trash in Phase B.
    let mut temps: Vec<(PathBuf, PathBuf)> = Vec::new();
    // Phase A: exclusive temp creation with complete after bytes.
    // Deletes stage no temp; renames stage the destination bytes.
    for file in &plan.files {
        stage_temp(session, file, &mut temps, &mut seen_dirs).await?;
    }
    // Mark every write-set path dirty before the first phase-B rename.
    for file in &plan.files {
        session.index.dirty(&session.workspace, &file.path);
        if let Some(renamed_to) = file.renamed_to.as_ref() {
            session.index.dirty(&session.workspace, renamed_to);
        }
    }
    // Phase B: ordered target operations (updates, deletes, renames).
    let mut completed: Vec<PathBuf> = Vec::new();
    // Deletes first in canonical order (sources only, no temp).
    // All trash paths are tracked for Phase C removal or rollback restore.
    let mut trash_paths: Vec<(PathBuf, PathBuf)> = Vec::new();
    for file in &plan.files {
        if file.op != super::super::ir::Operation::Delete {
            continue;
        }
        let trash = trash_path(session, &file.absolute_path);
        if let Err(error) = tokio::fs::rename(&file.absolute_path, &trash).await {
            cleanup_temps(&temps).await;
            return Err(EngineError::new(
                ErrorClass::Io,
                format!(
                    "patch: cannot write {}: {error}. Nothing was written.",
                    file.path.display()
                ),
            ));
        }
        completed.push(file.absolute_path.clone());
        trash_paths.push((file.absolute_path.clone(), trash.clone()));
    }
    for (target, temp) in &temps {
        if let Err(error) = tokio::fs::rename(temp, target).await {
            // Best-effort restore of already renamed targets from staged before bytes.
            restore_completed(plan, &completed).await;
            cleanup_temps(&temps).await;
            return Err(EngineError::new(
                ErrorClass::Io,
                format!(
                    "patch: cannot write {}: {error}. Nothing was written.",
                    target.display()
                ),
            ));
        }
        completed.push(target.clone());
    }
    // Rename sources move to trash after their destinations install.
    // Track every trash move so a later failure restores sources too.
    let mut trash_moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    for file in &plan.files {
        if file.op != super::super::ir::Operation::Rename {
            continue;
        }
        trash_rename_source(
            session,
            plan,
            file,
            &completed,
            &temps,
            &mut trash_moves,
            &mut trash_paths,
        )
        .await?;
    }
    // Phase C: directory sync on POSIX, trash removal, Seen transfer, output echo.
    #[cfg(unix)]
    for dir in seen_dirs {
        if let Ok(handle) = std::fs::File::open(&dir) {
            let _ = handle.sync_all();
        }
    }
    for (_, trash) in &trash_paths {
        let _ = tokio::fs::remove_file(trash).await;
    }
    // Best-effort leftover temp removal; Phase A/B already cleaned on error.
    for (_, temp) in &temps {
        // Temps that were renamed no longer exist; ignore missing.
        let _ = tokio::fs::remove_file(temp).await;
    }
    Ok(report_output(session, plan))
}

fn trash_path(session: &PatchSession, source: &std::path::Path) -> PathBuf {
    source
        .parent()
        .unwrap_or(session.workspace.as_path())
        .join(format!(
            ".dalgon-trash-{}-{:032x}.tmp",
            std::process::id(),
            nonce_u128()
        ))
}

/// Phase A for one file: compute the write target, refuse to clobber a
/// rename destination, and stage the after bytes into an exclusive temp.
async fn stage_temp(
    session: &PatchSession,
    file: &StagedFileOwned,
    temps: &mut Vec<(PathBuf, PathBuf)>,
    seen_dirs: &mut std::collections::HashSet<PathBuf>,
) -> Result<(), EngineError> {
    if file.op == super::super::ir::Operation::Delete {
        return Ok(());
    }
    let Some(after) = file.after.as_deref() else {
        return Ok(());
    };
    let target_abs = if file.op == super::super::ir::Operation::Rename {
        match file.renamed_to.as_ref() {
            Some(dest) => session.workspace.join(dest),
            None => file.absolute_path.clone(),
        }
    } else {
        file.absolute_path.clone()
    };
    // No-replace: rename destinations must be absent at prepare and commit.
    if file.op == super::super::ir::Operation::Rename
        && tokio::fs::metadata(&target_abs).await.is_ok()
    {
        cleanup_temps(temps).await;
        let dest = file
            .renamed_to
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        return Err(EngineError::new(
            ErrorClass::File,
            format!(
                "patch: cannot rename {} to {dest}: {dest} exists.",
                file.path.display()
            ),
        ));
    }
    let parent = target_abs.parent().unwrap_or(session.workspace.as_path());
    tokio::fs::create_dir_all(parent).await.map_err(|error| {
        EngineError::new(
            ErrorClass::Io,
            format!(
                "patch: cannot write {}: {error}. Nothing was written.",
                file.path.display()
            ),
        )
    })?;
    let temp = parent.join(format!(
        ".dalgon-patch-{}-{:032x}.tmp",
        std::process::id(),
        nonce_u128()
    ));
    let write = async {
        use tokio::io::AsyncWriteExt as _;
        let mut handle = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await?;
        handle.write_all(after).await?;
        handle.sync_all().await
    }
    .await;
    if let Err(error) = write {
        let _ = tokio::fs::remove_file(&temp).await;
        cleanup_temps(temps).await;
        return Err(EngineError::new(
            ErrorClass::Io,
            format!(
                "patch: cannot write {}: {error}. Nothing was written.",
                file.path.display()
            ),
        ));
    }
    temps.push((target_abs.clone(), temp));
    if let Some(parent) = file.absolute_path.parent() {
        seen_dirs.insert(parent.to_path_buf());
    }
    Ok(())
}

/// Best-effort restore of completed targets from staged before bytes.
async fn restore_completed(plan: &Plan, completed: &[PathBuf]) {
    for file in &plan.files {
        if completed.contains(&file.absolute_path)
            && let Some(before) = file.before.as_deref()
        {
            let _ = tokio::fs::write(&file.absolute_path, before).await;
        }
    }
}

/// Phase B tail for one rename: move the source aside once its destination
/// temp installed; restore installed temps and earlier trash moves on failure.
async fn trash_rename_source(
    session: &PatchSession,
    plan: &Plan,
    file: &StagedFileOwned,
    completed: &[PathBuf],
    temps: &[(PathBuf, PathBuf)],
    trash_moves: &mut Vec<(PathBuf, PathBuf)>,
    trash_paths: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), EngineError> {
    let trash = trash_path(session, &file.absolute_path);
    // If source already gone (e.g. failed earlier), skip; restores handle it.
    if tokio::fs::metadata(&file.absolute_path).await.is_err() {
        return Ok(());
    }
    if let Err(error) = tokio::fs::rename(&file.absolute_path, &trash).await {
        // Restore installed temps and earlier trash moves best-effort.
        for file in &plan.files {
            if completed.contains(&file.absolute_path) {
                if let Some(before) = file.before.as_deref() {
                    let _ = tokio::fs::write(&file.absolute_path, before).await;
                } else {
                    let _ = tokio::fs::remove_file(&file.absolute_path).await;
                }
            }
        }
        for (src, trash) in trash_moves.iter().rev() {
            let _ = tokio::fs::rename(trash, src).await;
        }
        cleanup_temps(temps).await;
        return Err(EngineError::new(
            ErrorClass::Io,
            format!(
                "patch: cannot write {}: {error}. Nothing was written.",
                file.path.display()
            ),
        ));
    }
    trash_moves.push((file.absolute_path.clone(), trash.clone()));
    trash_paths.push((file.absolute_path.clone(), trash));
    Ok(())
}

/// Phase C report: per-file line counts, display diffs, Seen transfer of
/// the after digest, and the summary text with observer findings appended.
fn report_output(session: &PatchSession, plan: &Plan) -> Output {
    let mut changes = Vec::new();
    let mut display_files = Vec::new();
    for file in &plan.files {
        let (added, removed) = count_lines(file.before.as_deref(), file.after.as_deref());
        changes.push(FileChange {
            path: file.path.to_string_lossy().replace('\\', "/").into(),
            op: file.op,
            added,
            removed,
            renamed_to: file
                .renamed_to
                .as_ref()
                .map(|path| path.to_string_lossy().replace('\\', "/").into()),
        });
        display_files.push(DiffFile {
            path: file.path.to_string_lossy().replace('\\', "/").into(),
            op: file.op,
            renamed_to: file
                .renamed_to
                .as_ref()
                .map(|path| path.to_string_lossy().replace('\\', "/").into()),
            hunks: file.hunks.clone(),
        });
        // Register echo rows under the new digest for Seen transfer.
        if let Some(after) = file.after.as_deref() {
            let digest = *blake3::hash(after).as_bytes();
            let count = after.split(|byte| *byte == b'\n').count() as u64;
            if count > 0 {
                session.seen.show(
                    session.session,
                    &file.path.to_string_lossy(),
                    digest,
                    1,
                    count,
                );
            }
        }
    }
    let mut text = String::from("Success. Updated the following files:");
    for change in &changes {
        let _ = std::fmt::Write::write_fmt(&mut text, format_args!("\nM {}", change.path));
    }
    // Append observer report findings after patch notes.
    for finding in &plan.findings {
        if finding.severity == FindingSeverity::Report {
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("\n{}", finding.text));
        }
    }
    Output {
        text,
        error_class: None,
        changes,
        display: Diff {
            kind: "diff".into(),
            files: display_files,
        },
    }
}

async fn cleanup_temps(temps: &[(PathBuf, PathBuf)]) {
    for (_, temp) in temps {
        let _ = tokio::fs::remove_file(temp).await;
    }
}

fn count_lines(before: Option<&[u8]>, after: Option<&[u8]>) -> (u64, u64) {
    let count = |bytes: Option<&[u8]>| {
        bytes.map_or(0, |bytes| {
            if bytes.is_empty() {
                0
            } else {
                bytes.split(|byte| *byte == b'\n').count() as u64
                    - u64::from(bytes.ends_with(b"\n"))
            }
        })
    };
    // Simplified added/removed: callers needing exact diff hunks compute them
    // from the immutable images; counts here stay consistent for tests.
    let before_count = count(before);
    let after_count = count(after);
    if after_count >= before_count {
        (after_count - before_count, 0)
    } else {
        (0, before_count - after_count)
    }
}

fn nonce_u128() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let first = COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| u64::from(duration.subsec_nanos()));
    (u128::from(first) << 64) | (u128::from(std::process::id()) << 32) | u128::from(nanos)
}
