//! Atomic commit: locks, temps, ordered operations, and truthful outcomes.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::super::ir::{
    Diff, DiffFile, EngineError, ErrorClass, FileChange, FindingSeverity, Output, Plan,
    StagedFileOwned,
};

use super::super::resolve::revalidate_path;
use super::{PatchSession, ensure_regular_target, non_regular_target, rename_temp::RenameTemp};

/// One staged after-image and its publication authority.
struct PendingTemp<'a> {
    file: &'a StagedFileOwned,
    destination: PendingDestination,
}

enum PendingDestination {
    Ordinary { target: PathBuf, temp: PathBuf },
    Rename(Arc<RenameTemp>),
}

impl PendingTemp<'_> {
    fn target(&self) -> &Path {
        match &self.destination {
            PendingDestination::Ordinary { target, .. } => target,
            PendingDestination::Rename(rename) => rename.target(),
        }
    }

    async fn publish(&self) -> io::Result<()> {
        match &self.destination {
            PendingDestination::Ordinary { target, temp } => {
                tokio::fs::rename(temp, target).await
            }
            PendingDestination::Rename(rename) => rename.publish(),
        }
    }

    async fn cleanup(&self) {
        match &self.destination {
            PendingDestination::Ordinary { temp, .. } => {
                let _ = tokio::fs::remove_file(temp).await;
            }
            PendingDestination::Rename(rename) => rename.cleanup(),
        }
    }

    fn sync_rename_parent(&self) {
        if let PendingDestination::Rename(rename) = &self.destination {
            rename.sync_dir();
        }
    }
}

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
    for file in &ordered {
        revalidate_path(&session.workspace, &file.path, &file.absolute_path)?;
        if let Some(dest) = file.renamed_to.as_ref() {
            revalidate_path(&session.workspace, &dest.path, &dest.absolute_path)?;
        }
    }
    for file in &ordered {
        if let Some(expected) = file.before.as_deref() {
            ensure_regular_target(&file.absolute_path, &file.path).await?;
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
        } else if let Ok(metadata) = tokio::fs::metadata(&file.absolute_path).await {
            if !metadata.is_file() {
                return Err(non_regular_target(&file.path));
            }
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
    let mut seen_dirs = std::collections::HashSet::new();
    let _guards = prepare_write_set(session, plan).await?;
    let mut temps = Vec::new();
    for file in &plan.files {
        if let Err(error) = stage_temp(session, file, &mut temps, &mut seen_dirs).await {
            cleanup_temps(&temps).await;
            return Err(error);
        }
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
            trash_source(session, file, &temps, &mut undo).await?;
        }
    }
    for pending in &temps {
        let target = pending.target();
        if let Err(error) = pending.publish().await {
            return Err(rollback_error(target, error, &temps, &undo).await);
        }
        undo.push(match (&pending.destination, pending.file.before.as_deref()) {
            (PendingDestination::Rename(rename), _) => Undo::RemoveRenamed(Arc::clone(rename)),
            (PendingDestination::Ordinary { target, .. }, None) => {
                Undo::RemoveInstalled(target.clone())
            }
            (PendingDestination::Ordinary { target, .. }, Some(before)) => {
                Undo::RestoreBytes(target.clone(), before)
            }
        });
    }
    for file in &plan.files {
        if file.op == super::super::ir::Operation::Rename {
            trash_source(session, file, &temps, &mut undo).await?;
        }
    }
    for pending in &temps {
        pending.sync_rename_parent();
    }
    #[cfg(unix)]
    for dir in seen_dirs {
        if let Ok(handle) = std::fs::File::open(&dir) {
            let _ = handle.sync_all();
        }
    }
    for action in undo {
        if let Undo::RestoreMoved { backup, .. } = action {
            let _ = tokio::fs::remove_file(backup).await;
        }
    }
    cleanup_temps(&temps).await;
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
async fn stage_temp<'a>(
    session: &PatchSession,
    file: &'a StagedFileOwned,
    temps: &mut Vec<PendingTemp<'a>>,
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
        let dest = file.renamed_to.as_ref().ok_or_else(|| {
            EngineError::new(ErrorClass::Resolve, "patch: rename has no destination.")
        })?;
        if let Ok(metadata) = tokio::fs::symlink_metadata(target_abs).await {
            if !metadata.is_file() {
                return Err(non_regular_target(&dest.path));
            }
            return Err(EngineError::new(
                ErrorClass::File,
                format!(
                    "patch: cannot rename {} to {}: {} exists.",
                    file.path.display(),
                    dest.path.display(),
                    dest.path.display()
                ),
            ));
        }
        // Stage through pinned directory handles, never through a pathname
        // that can be redirected between validation and publication.
        let (rename, handle) =
            RenameTemp::create(&session.workspace, &dest.path).map_err(|error| {
                EngineError::new(
                    ErrorClass::Io,
                    format!(
                        "patch: cannot write {}: {error}. Nothing was written.",
                        dest.path.display()
                    ),
                )
            })?;
        let write = async {
            use tokio::io::AsyncWriteExt as _;
            let mut handle = tokio::fs::File::from_std(handle);
            handle.write_all(after).await?;
            handle.sync_all().await
        }
        .await;
        if let Err(error) = write {
            rename.cleanup();
            return Err(EngineError::new(
                ErrorClass::Io,
                format!(
                    "patch: cannot write {}: {error}. Nothing was written.",
                    dest.path.display()
                ),
            ));
        }
        temps.push(PendingTemp {
            file,
            destination: PendingDestination::Rename(Arc::new(rename)),
        });
        let source_parent = file
            .absolute_path
            .parent()
            .unwrap_or(session.workspace.as_path());
        seen_dirs.insert(source_parent.to_path_buf());
        return Ok(());
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
        return Err(EngineError::new(
            ErrorClass::Io,
            format!(
                "patch: cannot write {}: {error}. Nothing was written.",
                file.path.display()
            ),
        ));
    }
    temps.push(PendingTemp {
        file,
        destination: PendingDestination::Ordinary {
            target: target_abs.to_path_buf(),
            temp,
        },
    });
    seen_dirs.insert(parent.to_path_buf());
    Ok(())
}

enum Undo<'a> {
    RestoreBytes(PathBuf, &'a [u8]),
    RemoveInstalled(PathBuf),
    RemoveRenamed(Arc<RenameTemp>),
    RestoreMoved {
        source: PathBuf,
        backup: PathBuf,
    },
}

impl Undo<'_> {
    async fn restore(&self) -> std::io::Result<()> {
        match self {
            Self::RestoreBytes(path, before) => tokio::fs::write(path, before).await,
            Self::RemoveInstalled(path) => match tokio::fs::remove_file(path).await {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
            Self::RemoveRenamed(rename) => rename.rollback(),
            Self::RestoreMoved { source, backup } => tokio::fs::rename(backup, source).await,
        }
    }
}

async fn rollback_error(
    path: &std::path::Path,
    error: std::io::Error,
    temps: &[PendingTemp<'_>],
    undo: &[Undo<'_>],
) -> EngineError {
    use std::fmt::Write as _;

    let mut failures = Vec::new();
    for action in undo.iter().rev() {
        if let Err(error) = action.restore().await {
            failures.push((action, error));
        }
    }
    cleanup_temps(temps).await;
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
                Undo::RemoveRenamed(rename) => {
                    let _ = write!(message, "\n{}: {error}.", rename.target().display());
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

async fn trash_source<'a>(
    session: &PatchSession,
    file: &'a StagedFileOwned,
    temps: &[PendingTemp<'_>],
    undo: &mut Vec<Undo<'a>>,
) -> Result<(), EngineError> {
    let backup = trash_path(session, &file.absolute_path);
    if let Err(error) = tokio::fs::rename(&file.absolute_path, &backup).await {
        return Err(rollback_error(&file.path, error, temps, undo).await);
    }
    undo.push(Undo::RestoreMoved {
        source: file.absolute_path.clone(),
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

async fn cleanup_temps(temps: &[PendingTemp<'_>]) {
    for temp in temps {
        temp.cleanup().await;
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

pub(super) fn nonce_u128() -> u128 {
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
    async fn failed_rename_staging_removes_preceding_create_temp() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let source = workspace.join("source.txt");
        let created = workspace.join("created");
        // The second rename stage must fail because its parent is a file.
        // The earlier create must not survive a failed staging phase.
        let blocker = workspace.join("blocked");
        tokio::fs::write(&blocker, b"file\n")
            .await
            .expect("seed the file that blocks the destination parent");
        let destination = blocker.join("dest");

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
                        path: PathBuf::from("blocked/dest"),
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
            super::Undo::RestoreBytes(update.clone(), b"before\n"),
            super::Undo::RemoveInstalled(create.clone()),
            super::Undo::RestoreBytes(restored.clone(), b"before\n"),
            super::Undo::RemoveInstalled(destination.clone()),
            super::Undo::RestoreMoved {
                source: earlier.clone(),
                backup: backup.clone(),
            },
        ];
        let error = super::trash_source(&session, &file, &[], &mut undo)
            .await
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
