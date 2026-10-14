//! Atomic commit: locks, temps, ordered operations, and truthful outcomes.

use std::{path::PathBuf, sync::Arc};

use super::super::ir::{
    Diff, DiffFile, EngineError, ErrorClass, FileChange, FindingSeverity, Output, Plan,
    StagedFileOwned,
};

use super::super::resolve::revalidate_path;
use super::{PatchSession, non_regular_target, secure_fs::SecureFs};

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
) -> Result<(Vec<tokio::sync::OwnedMutexGuard<()>>, SecureFs), EngineError> {
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
            lock_paths.push(dest.absolute_path.clone());
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
    let mut fs = SecureFs::new(&session.workspace).map_err(|error| {
        EngineError::new(
            ErrorClass::Io,
            format!("patch: cannot open workspace: {error}. Nothing was written."),
        )
    })?;
    for file in &ordered {
        revalidate_path(&session.workspace, &file.path, &file.absolute_path)?;
        if let Some(dest) = file.renamed_to.as_ref() {
            revalidate_path(&session.workspace, &dest.path, &dest.absolute_path)?;
        }
    }
    for file in &ordered {
        let current = checked_read(&mut fs, &file.absolute_path, &file.path)?;
        if let Some(expected) = file.before.as_deref() {
            if current.as_deref().unwrap_or_default() != expected {
                drop(guards);
                return Err(EngineError::new(
                    ErrorClass::Stale,
                    format!(
                        "patch: {} changed while you were approving. Nothing was written. Try again.",
                        file.path.display()
                    ),
                ));
            }
        } else if current.is_some() {
            drop(guards);
            return Err(EngineError::new(
                ErrorClass::File,
                format!("patch: {} already exists.", file.path.display()),
            ));
        }
    }
    Ok((guards, fs))
}

fn checked_read(
    fs: &mut SecureFs,
    path: &std::path::Path,
    display: &std::path::Path,
) -> Result<Option<Vec<u8>>, EngineError> {
    fs.read_optional(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            non_regular_target(display)
        } else {
            EngineError::new(
                ErrorClass::Io,
                format!(
                    "patch: cannot read {}: {error}. Nothing was written.",
                    display.display()
                ),
            )
        }
    })
}

pub(crate) async fn apply_files(
    session: &PatchSession,
    plan: &Plan,
) -> Result<Output, EngineError> {
    let mut seen_dirs = std::collections::HashSet::new();
    let (_guards, mut fs) = prepare_write_set(session, plan).await?;
    let mut temps = Vec::new();
    for file in &plan.files {
        stage_temp(&mut fs, session, file, &mut temps, &mut seen_dirs)?;
    }
    for file in &plan.files {
        session.index.dirty(&session.workspace, &file.path);
        if let Some(renamed_to) = file.renamed_to.as_ref() {
            session.index.dirty(&session.workspace, &renamed_to.path);
        }
    }
    let mut undo = Vec::new();
    for file in &plan.files {
        if file.op == super::super::ir::Operation::Delete {
            trash_source(&mut fs, session, file, &temps, &mut undo)?;
        }
    }
    for (file, temp) in &temps {
        let target = write_target(file);
        if let Err(error) = fs.move_file(temp, target) {
            return Err(rollback_error(&mut fs, target, error, &temps, &undo));
        }
        undo.push(match (file.op, file.before.as_deref()) {
            (super::super::ir::Operation::Rename, _) | (_, None) => Undo::RemoveInstalled(target),
            (_, Some(before)) => Undo::RestoreBytes(target, before),
        });
    }
    for file in &plan.files {
        if file.op == super::super::ir::Operation::Rename {
            trash_source(&mut fs, session, file, &temps, &mut undo)?;
        }
    }
    #[cfg(unix)]
    for dir in seen_dirs {
        fs.sync_parent(&dir);
    }
    for action in undo {
        if let Undo::RestoreMoved { backup, .. } = action {
            let _ = fs.remove_file(&backup);
        }
    }
    cleanup_temps(&mut fs, &temps);
    Ok(report_output(session, plan))
}

fn write_target(file: &StagedFileOwned) -> &std::path::Path {
    file.renamed_to
        .as_ref()
        .filter(|_| file.op == super::super::ir::Operation::Rename)
        .map_or(file.absolute_path.as_path(), |dest| {
            dest.absolute_path.as_path()
        })
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
fn stage_temp<'a>(
    fs: &mut SecureFs,
    session: &PatchSession,
    file: &'a StagedFileOwned,
    temps: &mut Vec<(&'a StagedFileOwned, PathBuf)>,
    seen_dirs: &mut std::collections::HashSet<PathBuf>,
) -> Result<(), EngineError> {
    if file.op == super::super::ir::Operation::Delete {
        let parent = file
            .absolute_path
            .parent()
            .unwrap_or(session.workspace.as_path());
        seen_dirs.insert(parent.to_path_buf());
        return Ok(());
    }
    let Some(after) = file.after.as_deref() else {
        return Ok(());
    };
    let target_abs = write_target(file);
    if file.op == super::super::ir::Operation::Rename {
        let dest = file
            .renamed_to
            .as_ref()
            .map_or(file.path.as_path(), |dest| dest.path.as_path());
        match checked_read(fs, target_abs, dest) {
            Ok(Some(_)) => {
                cleanup_temps(fs, temps);
                return Err(EngineError::new(
                    ErrorClass::File,
                    format!(
                        "patch: cannot rename {} to {}: {} exists.",
                        file.path.display(),
                        dest.display(),
                        dest.display()
                    ),
                ));
            }
            Ok(None) => {}
            Err(error) => {
                cleanup_temps(fs, temps);
                return Err(error);
            }
        }
    }
    let parent = target_abs.parent().unwrap_or(session.workspace.as_path());
    let temp = parent.join(format!(
        ".dalgon-patch-{}-{:032x}.tmp",
        std::process::id(),
        nonce_u128()
    ));
    if let Err(error) = fs.write_new(&temp, after) {
        let _ = fs.remove_file(&temp);
        cleanup_temps(fs, temps);
        return Err(EngineError::new(
            ErrorClass::Io,
            format!(
                "patch: cannot write {}: {error}. Nothing was written.",
                file.path.display()
            ),
        ));
    }
    temps.push((file, temp));
    seen_dirs.insert(parent.to_path_buf());
    if file.op == super::super::ir::Operation::Rename {
        let source_parent = file
            .absolute_path
            .parent()
            .unwrap_or(session.workspace.as_path());
        seen_dirs.insert(source_parent.to_path_buf());
    }
    Ok(())
}

enum Undo<'a> {
    RestoreBytes(&'a std::path::Path, &'a [u8]),
    RemoveInstalled(&'a std::path::Path),
    RestoreMoved {
        source: &'a std::path::Path,
        backup: PathBuf,
    },
}

impl Undo<'_> {
    fn restore(&self, fs: &mut SecureFs) -> std::io::Result<()> {
        match self {
            Self::RestoreBytes(path, before) => {
                let temp = path.with_file_name(format!(
                    ".dalgon-rollback-{}-{:032x}.tmp",
                    std::process::id(),
                    nonce_u128()
                ));
                fs.write_new(&temp, before)?;
                let result = fs.move_file(&temp, path);
                if result.is_err() {
                    let _ = fs.remove_file(&temp);
                }
                result
            }
            Self::RemoveInstalled(path) => match fs.remove_file(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
            Self::RestoreMoved { source, backup } => fs.move_file(backup, source),
        }
    }
}

fn rollback_error(
    fs: &mut SecureFs,
    path: &std::path::Path,
    error: std::io::Error,
    temps: &[(&StagedFileOwned, PathBuf)],
    undo: &[Undo<'_>],
) -> EngineError {
    use std::fmt::Write as _;

    let mut failures = Vec::new();
    for action in undo.iter().rev() {
        if let Err(error) = action.restore(fs) {
            failures.push((action, error));
        }
    }
    cleanup_temps(fs, temps);
    let mut message = format!("patch: cannot write {}: {error}.", path.display());
    if failures.is_empty() {
        message.push_str(" Nothing was written.");
    } else {
        message.push_str(" Rollback was incomplete. Inspect these paths before retrying:");
        for (action, error) in failures {
            match action {
                Undo::RestoreBytes(path, _) | Undo::RemoveInstalled(path) => {
                    let _ = write!(message, "\n{}: {error}.", path.display());
                }
                Undo::RestoreMoved { source, backup } => {
                    let _ = write!(
                        message,
                        "\n{}: {error}. Backup retained at {}.",
                        source.display(),
                        backup.display()
                    );
                }
            }
        }
    }
    EngineError::new(ErrorClass::Io, message)
}

fn trash_source<'a>(
    fs: &mut SecureFs,
    session: &PatchSession,
    file: &'a StagedFileOwned,
    temps: &[(&StagedFileOwned, PathBuf)],
    undo: &mut Vec<Undo<'a>>,
) -> Result<(), EngineError> {
    let backup = trash_path(session, &file.absolute_path);
    if let Err(error) = fs.move_file(&file.absolute_path, &backup) {
        return Err(rollback_error(fs, &file.path, error, temps, undo));
    }
    undo.push(Undo::RestoreMoved {
        source: &file.absolute_path,
        backup,
    });
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
                .map(|dest| dest.path.to_string_lossy().replace('\\', "/").into()),
        });
        display_files.push(DiffFile {
            path: file.path.to_string_lossy().replace('\\', "/").into(),
            op: file.op,
            renamed_to: file
                .renamed_to
                .as_ref()
                .map(|dest| dest.path.to_string_lossy().replace('\\', "/").into()),
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

fn cleanup_temps(fs: &mut SecureFs, temps: &[(&StagedFileOwned, PathBuf)]) {
    for (_, temp) in temps {
        let _ = fs.remove_file(temp);
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

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, path::PathBuf, sync::Arc};

    use dal_core::{CallId, GenerationId, SessionId, TurnId};

    use crate::{
        patch::{
            ir::{DialectId, ErrorClass, Operation, Plan, RenameTarget, StagedFileOwned},
            snapshot::SnapshotStore,
        },
        search::index::Index,
    };

    use super::super::PatchSession;
    use super::apply_files;

    #[tokio::test]
    async fn restore_completed_removes_create_after_later_temp_install_failure() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let source = workspace.join("source.txt");
        let created = workspace.join("created");
        // The rename destination must fail. unix refuses a file target
        // under a missing directory at Phase-B rename; Windows accepts
        // that spelling, so on Windows the destination sits under a
        // regular file and prepare refuses it instead.
        #[cfg(unix)]
        let destination = workspace.join("other/");
        #[cfg(windows)]
        let destination = {
            let blocker = workspace.join("blocked");
            tokio::fs::write(&blocker, b"file\n")
                .await
                .expect("seed the file that blocks the destination parent");
            blocker.join("dest")
        };

        tokio::fs::write(&source, b"source\n")
            .await
            .expect("seed rename source");

        let session = PatchSession {
            workspace,
            session: SessionId::new_v7(),
            generation: GenerationId::new(NonZeroU64::MIN),
            turn: TurnId::new(NonZeroU64::MIN),
            call: CallId::new("restore-create"),
            consumer: dal_core::Consumer::Model,
            symbols: false,
            seen: crate::Seen::new(),
            index: Index::new(None),
            snapshots: Arc::new(SnapshotStore::new([7; 16])),
            cutoff: Some(u64::MAX),
        };
        let plan = Plan {
            style: DialectId::Replace,
            files: vec![
                StagedFileOwned {
                    path: PathBuf::from("created"),
                    absolute_path: created.clone(),
                    before: None,
                    after: Some(b"created\n".to_vec().into_boxed_slice()),
                    op: Operation::Create,
                    renamed_to: None,
                    hunks: Vec::new(),
                },
                StagedFileOwned {
                    path: PathBuf::from("source.txt"),
                    absolute_path: source.clone(),
                    before: Some(b"source\n".to_vec().into_boxed_slice()),
                    after: Some(b"renamed\n".to_vec().into_boxed_slice()),
                    op: Operation::Rename,
                    renamed_to: Some(RenameTarget {
                        path: if cfg!(unix) {
                            PathBuf::from("other/")
                        } else {
                            PathBuf::from("blocked/dest")
                        },
                        absolute_path: destination,
                    }),
                    hunks: Vec::new(),
                },
            ],
            findings: Vec::new(),
        };

        let error = apply_files(&session, &plan)
            .await
            .expect_err("later rename install must fail");

        // The refused destination can surface as a Phase-B rename error
        // (Io) or an earlier prepare refusal (Stale) depending on which
        // validator reaches it first on each platform.
        assert!(
            matches!(error.class, ErrorClass::Io | ErrorClass::Stale),
            "{error:?}"
        );
        assert!(error.message.contains("Nothing was written"), "{error}");
        assert!(
            tokio::fs::metadata(created).await.is_err(),
            "completed create must be removed during restore"
        );
        assert_eq!(
            tokio::fs::read(source).await.expect("read source"),
            b"source\n"
        );
    }

    #[tokio::test]
    async fn failed_rollback_reports_every_stranded_path_and_preserves_backups() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let session = PatchSession {
            workspace: workspace.clone(),
            session: SessionId::new_v7(),
            generation: GenerationId::new(NonZeroU64::MIN),
            turn: TurnId::new(NonZeroU64::MIN),
            call: CallId::new("failed-rollback"),
            consumer: dal_core::Consumer::Model,
            symbols: false,
            seen: crate::Seen::new(),
            index: Index::new(None),
            snapshots: Arc::new(SnapshotStore::new([7; 16])),
            cutoff: Some(64),
        };
        let names = [
            "blocked-update",
            "blocked-create",
            "restored-update",
            "blocked-destination",
            "failed-source",
            "blocked-earlier-source",
            "earlier-source-backup",
        ];
        let paths = names.map(|name| workspace.join(name));
        let [
            update,
            create,
            restored,
            destination,
            source,
            earlier,
            backup,
        ] = &paths;
        for path in [update, create, destination, source, earlier] {
            tokio::fs::create_dir(path).await.expect("blocked restore");
        }
        tokio::fs::write(restored, b"after\n")
            .await
            .expect("changed file");
        tokio::fs::write(earlier.join("occupant"), b"preserve\n")
            .await
            .expect("occupant");
        tokio::fs::write(backup, b"original\n")
            .await
            .expect("backup");
        let file = StagedFileOwned {
            path: PathBuf::from("failed-source"),
            absolute_path: source.join("."),
            before: Some(b"source\n".to_vec().into_boxed_slice()),
            after: Some(b"source\n".to_vec().into_boxed_slice()),
            op: Operation::Rename,
            renamed_to: Some(RenameTarget {
                path: PathBuf::from("blocked-destination"),
                absolute_path: destination.clone(),
            }),
            hunks: Vec::new(),
        };
        let mut undo = vec![
            super::Undo::RestoreBytes(update, b"before\n"),
            super::Undo::RemoveInstalled(create),
            super::Undo::RestoreBytes(restored, b"before\n"),
            super::Undo::RemoveInstalled(destination),
            super::Undo::RestoreMoved {
                source: earlier,
                backup: backup.clone(),
            },
        ];
        let mut fs = super::SecureFs::new(&workspace).expect("secure filesystem");
        let error = super::trash_source(&mut fs, &session, &file, &[], &mut undo)
            .expect_err("source cannot move inside itself");
        assert_eq!(error.class, ErrorClass::Io);
        assert!(error.message.contains("failed-source"), "{error}");
        assert!(!error.message.contains("Nothing was written"), "{error}");
        for path in [update, create, destination, earlier, backup] {
            assert!(
                error.message.contains(path.to_str().expect("path")),
                "missing {path:?}: {error}"
            );
        }
        assert_eq!(
            tokio::fs::read(backup).await.expect("backup survives"),
            b"original\n"
        );
        assert_eq!(
            tokio::fs::read(earlier.join("occupant"))
                .await
                .expect("occupant survives"),
            b"preserve\n"
        );
        assert_eq!(
            tokio::fs::read(restored)
                .await
                .expect("other rollback continues"),
            b"before\n"
        );
    }
}
