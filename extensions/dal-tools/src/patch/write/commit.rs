//! Atomic commit: locks, temps, ordered operations, and truthful outcomes.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use dal_agent::ext::BoxFuture;

use super::super::ir::{
    Diff, DiffFile, EngineError, ErrorClass, FileChange, FindingSeverity, Operation, Output, Plan,
    StagedFileOwned,
};

use super::super::resolve::revalidate_path;
use super::{PatchSession, ensure_regular_target, non_regular_target, rename_temp::RenameTemp};

/// An opened directory is the authority for rename publication.
enum PendingTemp {
    /// Mode bits the temp is born with and then stamped to exactly; `None`
    /// keeps the 0644 create mode of a new file.
    Ordinary {
        target: PathBuf,
        temp: PathBuf,
        mode: Option<u32>,
    },
    Rename(RenameTemp),
}

impl PendingTemp {
    fn target(&self) -> &Path {
        match self {
            Self::Ordinary { target, .. } => target,
            Self::Rename(rename) => rename.target(),
        }
    }

    async fn publish(&self) -> io::Result<()> {
        match self {
            Self::Ordinary { target, temp, .. } => tokio::fs::rename(temp, target).await,
            Self::Rename(rename) => rename.publish(),
        }
    }

    async fn cleanup(&self) {
        match self {
            Self::Ordinary { temp, .. } => {
                let _ = tokio::fs::remove_file(temp).await;
            }
            Self::Rename(rename) => rename.cleanup(),
        }
    }

    fn sync_rename_dir(&self) {
        if let Self::Rename(rename) = self {
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
        check_unchanged(file).await?;
    }
    Ok(guards)
}

fn changed_while_approving(path: &Path) -> EngineError {
    EngineError::new(
        ErrorClass::Stale,
        format!(
            "patch: {} changed while you were approving. Nothing was written. Try again.",
            path.display()
        ),
    )
}

/// Proves one staged file still has the bytes it was staged from, or still
/// has no occupant when it was staged as new. A file that vanished is
/// stale, and a read that fails for any other reason is an I/O failure:
/// neither may pass as an empty file.
async fn check_unchanged(file: &StagedFileOwned) -> Result<(), EngineError> {
    let Some(expected) = file.before.as_deref() else {
        // A symlink that resolves nowhere still occupies the name, and
        // publishing over it would silently destroy the link.
        return match tokio::fs::symlink_metadata(&file.absolute_path).await {
            Ok(metadata) if metadata.is_file() => Err(EngineError::new(
                ErrorClass::File,
                format!("patch: {} already exists.", file.path.display()),
            )),
            Ok(_) => Err(non_regular_target(&file.path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(cannot_read(&file.path, &error)),
        };
    };
    ensure_regular_target(&file.absolute_path, &file.path).await?;
    match tokio::fs::read(&file.absolute_path).await {
        Ok(current) if current == expected => Ok(()),
        Ok(_) => Err(changed_while_approving(&file.path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(changed_while_approving(&file.path))
        }
        Err(error) => Err(cannot_read(&file.path, &error)),
    }
}

/// A target whose state cannot be read is never assumed vacant or empty.
fn cannot_read(path: &Path, error: &io::Error) -> EngineError {
    EngineError::new(
        ErrorClass::Io,
        format!(
            "patch: cannot read {}: {error}. Nothing was written.",
            path.display()
        ),
    )
}

/// The text shown for a commit cancelled before any target changed.
pub(crate) const CANCELLED: &str = "patch: cancelled before any file changed. Nothing was written.";

/// How a commit that was not refused or failed ended.
#[derive(Debug)]
pub(crate) enum Settled {
    /// Every change is published.
    Done(Output),
    /// Cancellation stopped the commit before any target changed, so nothing
    /// was written and no temp remains.
    Cancelled,
}

/// Why the write phases stopped before publishing everything.
enum Stop {
    Cancelled,
    Failed(EngineError),
}

impl From<EngineError> for Stop {
    fn from(error: EngineError) -> Self {
        Self::Failed(error)
    }
}

/// The decision points of the commit pipeline. Production runs unpaced;
/// a test pacer interleaves one concurrent actor at an exact point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    /// Every temp is staged and the index is invalidated; no target has
    /// changed, and cancellation is still honored.
    Staged,
    /// The first target is published; cancellation is ignored from here to
    /// the end.
    Published,
}

trait Pace: Send + Sync {
    fn at(&self, phase: Phase) -> BoxFuture<'_, ()>;
}

struct Unpaced;

impl Pace for Unpaced {
    fn at(&self, _phase: Phase) -> BoxFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// Commits `plan` to completion even when the caller stops polling it.
///
/// Dropping the returned future before the first target is published cancels
/// the commit and removes its temps. From the first publication on, the
/// commit runs to the end on its own task, so a caller that goes away can
/// never leave a multi-file plan half-published.
pub(crate) async fn apply_detached(
    session: &PatchSession,
    plan: Plan,
) -> Result<Settled, EngineError> {
    detached(session, plan, Arc::new(Unpaced)).await
}

async fn detached(
    session: &PatchSession,
    plan: Plan,
    pace: Arc<dyn Pace>,
) -> Result<Settled, EngineError> {
    let token = session.cancel.child_token();
    let abandoned = token.clone().drop_guard();
    let mut owned = session.clone();
    owned.cancel = token;
    #[expect(
        clippy::disallowed_methods,
        reason = "a started publication must finish after the caller drops this future; the task is joined below and holds only owned state"
    )]
    let task = tokio::spawn(async move { apply_with(&owned, &plan, pace.as_ref()).await });
    let joined = task.await;
    drop(abandoned.disarm());
    joined.unwrap_or_else(|error| {
        Err(EngineError::new(
            ErrorClass::Io,
            format!("patch: the write task ended abnormally: {error}. Inspect the files before retrying."),
        ))
    })
}

async fn apply_with(
    session: &PatchSession,
    plan: &Plan,
    pace: &dyn Pace,
) -> Result<Settled, EngineError> {
    let _guards = prepare_write_set(session, plan).await?;
    // The index hears about the write before the first mutation and again
    // after the last. A build that starts between the two reads pre-write
    // bytes under the generation the first call produced, and only the second
    // call keeps that snapshot from being served as fresh.
    mark_dirty(session, plan);
    let written = write_set(session, plan, pace).await;
    mark_dirty(session, plan);
    match written {
        Ok(()) => Ok(Settled::Done(report_output(session, plan))),
        Err(Stop::Cancelled) => Ok(Settled::Cancelled),
        Err(Stop::Failed(error)) => Err(error),
    }
}

fn mark_dirty(session: &PatchSession, plan: &Plan) {
    for file in &plan.files {
        session.index.dirty(&session.workspace, &file.path);
        if let Some(renamed_to) = file.renamed_to.as_ref() {
            session.index.dirty(&session.workspace, &renamed_to.path);
        }
    }
}

/// Phases A to C for a plan whose locks are held and whose digests were
/// re-proved: stage temps, publish in order, and undo everything on failure.
async fn write_set(session: &PatchSession, plan: &Plan, pace: &dyn Pace) -> Result<(), Stop> {
    let mut seen_dirs = std::collections::HashSet::new();
    let mut temps: Vec<(&StagedFileOwned, PendingTemp)> = Vec::new();
    for file in &plan.files {
        if session.cancel.is_cancelled() {
            cleanup_temps(&temps).await;
            return Err(Stop::Cancelled);
        }
        match stage_temp(session, file, &mut seen_dirs).await {
            Ok(Some(temp)) => temps.push((file, temp)),
            Ok(None) => {}
            Err(error) => {
                cleanup_temps(&temps).await;
                return Err(error.into());
            }
        }
    }
    pace.at(Phase::Staged).await;
    if session.cancel.is_cancelled() {
        cleanup_temps(&temps).await;
        return Err(Stop::Cancelled);
    }
    let mut undo = Vec::new();
    for file in &plan.files {
        if file.op == super::super::ir::Operation::Delete {
            trash_source(session, file, &temps, &mut undo).await?;
        }
    }
    for (position, (file, pending)) in temps.iter().enumerate() {
        let target = pending.target();
        if let Err(error) = pending.publish().await {
            return Err(rollback_error(target, error, &temps, &undo).await.into());
        }
        undo.push(match (pending, file.before.as_deref()) {
            (PendingTemp::Rename(rename), _) => Undo::RemoveRenamed(rename),
            (PendingTemp::Ordinary { target, .. }, None) => Undo::RemoveInstalled(target),
            (PendingTemp::Ordinary { target, mode, .. }, Some(before)) => Undo::RestoreBytes {
                path: target,
                bytes: before,
                mode: *mode,
            },
        });
        if position == 0 {
            pace.at(Phase::Published).await;
        }
    }
    for file in &plan.files {
        if file.op == super::super::ir::Operation::Rename {
            trash_source(session, file, &temps, &mut undo).await?;
        }
    }
    for (_, pending) in &temps {
        pending.sync_rename_dir();
    }
    #[cfg(unix)]
    sync_dirs(seen_dirs).await;
    for action in undo {
        if let Undo::RestoreMoved { backup, .. } = action {
            let _ = tokio::fs::remove_file(backup).await;
        }
    }
    cleanup_temps(&temps).await;
    Ok(())
}

/// Syncs every touched directory. An fsync waits for the device, so it runs on
/// the blocking pool, never on the executor thread.
#[cfg(unix)]
async fn sync_dirs(dirs: std::collections::HashSet<PathBuf>) {
    let _ = tokio::task::spawn_blocking(move || {
        for dir in dirs {
            if let Ok(handle) = std::fs::File::open(&dir) {
                let _ = handle.sync_all();
            }
        }
    })
    .await;
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

/// Open the rename destination beneath the workspace without following
/// symlinked parents.
fn stage_rename_temp(
    session: &PatchSession,
    file: &StagedFileOwned,
    mode: Option<u32>,
) -> Result<(PendingTemp, tokio::fs::File), EngineError> {
    let dest = file.renamed_to.as_ref().ok_or_else(|| {
        EngineError::new(
            ErrorClass::Resolve,
            "patch: rename is missing its validated destination.",
        )
    })?;
    let created = RenameTemp::create(&session.workspace, &dest.path, mode);
    let (rename, handle) = created.map_err(|error| {
        EngineError::new(
            ErrorClass::Io,
            format!(
                "patch: cannot stage rename destination {}: {error}. Nothing was written.",
                dest.path.display()
            ),
        )
    })?;
    Ok((
        PendingTemp::Rename(rename),
        tokio::fs::File::from_std(handle),
    ))
}

/// Phase A: stage after-images for publication. Rename destinations are
/// opened under the workspace without following symlinked parents.
async fn stage_temp(
    session: &PatchSession,
    file: &StagedFileOwned,
    seen_dirs: &mut std::collections::HashSet<PathBuf>,
) -> Result<Option<PendingTemp>, EngineError> {
    use tokio::io::AsyncWriteExt as _;

    if file.op == Operation::Delete {
        let parent = file
            .absolute_path
            .parent()
            .unwrap_or(session.workspace.as_path());
        seen_dirs.insert(parent.to_path_buf());
        return Ok(None);
    }
    let Some(after) = file.after.as_deref() else {
        return Ok(None);
    };
    let target_abs = write_target(file);
    if file.op == super::super::ir::Operation::Rename
        && tokio::fs::symlink_metadata(target_abs).await.is_ok()
    {
        let dest = file
            .renamed_to
            .as_ref()
            .map_or(file.path.as_path(), |dest| dest.path.as_path());
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

    // The published mode is decided before staging: a new file keeps the
    // create-time 0644, an update keeps the target's mode, and a rename
    // keeps the source's mode. The temp is born with that mode, so no other
    // user can open it wider than its final mode and keep the descriptor.
    // A mode that cannot be read refuses the stage while nothing exists to
    // clean up.
    let mode_bits = staged_mode_bits(file, target_abs)
        .await
        .map_err(|error| cannot_write(file.path.as_path(), &error))?;
    let (pending, mut handle) = if file.op == super::super::ir::Operation::Rename {
        stage_rename_temp(session, file, mode_bits)?
    } else {
        let parent = target_abs.parent().unwrap_or(session.workspace.as_path());
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| cannot_write(file.path.as_path(), &error))?;
        let (temp, handle) = create_sibling_temp(parent, mode_bits)
            .await
            .map_err(|error| cannot_write(file.path.as_path(), &error))?;
        seen_dirs.insert(parent.to_path_buf());
        (
            PendingTemp::Ordinary {
                target: file.absolute_path.clone(),
                temp,
                mode: mode_bits,
            },
            handle,
        )
    };
    // The mode is stamped on the open handle before any byte is written and
    // before publication, so an interruption never publishes a mis-moded
    // file and a failed stamp removes the temp instead.
    let result = async {
        #[cfg(unix)]
        stamp_saved_mode(&handle, mode_bits).await?;
        handle.write_all(after).await?;
        handle.sync_all().await
    }
    .await;
    drop(handle);
    if let Err(error) = result {
        pending.cleanup().await;
        return Err(cannot_write(file.path.as_path(), &error));
    }
    if file.op == super::super::ir::Operation::Rename {
        let source_parent = file
            .absolute_path
            .parent()
            .unwrap_or(session.workspace.as_path());
        seen_dirs.insert(source_parent.to_path_buf());
    }
    Ok(Some(pending))
}

/// Creates a temp beside its target, born with `mode` — the saved bits of an
/// update or rename, or 0644 for a new file — so a private target's bytes
/// never sit in a temp wider than their final mode. The umask only narrows
/// the create mode; callers stamp the exact bits on the handle afterwards.
async fn create_sibling_temp(
    parent: &Path,
    mode: Option<u32>,
) -> io::Result<(PathBuf, tokio::fs::File)> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(mode.unwrap_or(0o644));
    #[cfg(not(unix))]
    let _ = mode;
    // An exclusive-create collision draws a fresh nonce and tries again.
    for _ in 0..16 {
        let temp = parent.join(format!(
            ".dalgon-patch-{}-{:032x}.tmp",
            std::process::id(),
            nonce_u128()
        ));
        match options.open(&temp).await {
            Ok(handle) => return Ok((temp, handle)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate a unique temporary file",
    ))
}

/// One failure text for every refusal in `stage_temp`: the plan is dead,
/// the temp is gone, and nothing was published.
fn cannot_write(path: &Path, error: &std::io::Error) -> EngineError {
    EngineError::new(
        ErrorClass::Io,
        format!(
            "patch: cannot write {}: {error}. Nothing was written.",
            path.display()
        ),
    )
}

/// Decides the published mode for one staged file. A new file keeps the
/// create-time 0644; an update keeps the target's mode; a rename keeps the
/// source's mode. Renames read the source because publication is no-replace:
/// the destination never carries an earlier mode to preserve.
#[cfg(unix)]
async fn staged_mode_bits(
    file: &StagedFileOwned,
    target_abs: &Path,
) -> std::io::Result<Option<u32>> {
    if file.op == Operation::Create {
        return Ok(None);
    }
    let mode_path = match file.op {
        Operation::Rename => file.absolute_path.as_path(),
        _ => target_abs,
    };
    let metadata = tokio::fs::metadata(mode_path).await?;
    Ok(Some(
        std::os::unix::fs::MetadataExt::mode(&metadata) & 0o777,
    ))
}

#[cfg(not(unix))]
async fn staged_mode_bits(
    _file: &StagedFileOwned,
    _target_abs: &Path,
) -> std::io::Result<Option<u32>> {
    Ok(None)
}

/// Fchmods an open handle to `bits`; no-op off Unix.
#[cfg(unix)]
async fn stamp_saved_mode(handle: &tokio::fs::File, bits: Option<u32>) -> std::io::Result<()> {
    let Some(bits) = bits else {
        return Ok(());
    };
    handle
        .set_permissions(std::fs::Permissions::from_mode(bits))
        .await
}

#[cfg(not(unix))]
async fn stamp_saved_mode(_handle: &tokio::fs::File, _bits: Option<u32>) -> std::io::Result<()> {
    Ok(())
}

enum Undo<'a> {
    RestoreBytes {
        path: &'a std::path::Path,
        bytes: &'a [u8],
        mode: Option<u32>,
    },
    RemoveInstalled(&'a std::path::Path),
    RemoveRenamed(&'a RenameTemp),
    RestoreMoved {
        source: &'a std::path::Path,
        backup: PathBuf,
    },
}

impl Undo<'_> {
    async fn restore(&self) -> std::io::Result<()> {
        match self {
            Self::RestoreBytes { path, bytes, mode } => restore_bytes(path, bytes, *mode).await,
            Self::RemoveInstalled(path) => match tokio::fs::remove_file(path).await {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
            Self::RemoveRenamed(rename) => rename.rollback(),
            Self::RestoreMoved { source, backup } => tokio::fs::rename(backup, source).await,
        }
    }
}

/// Puts a pre-image back the way an update publishes: a sibling temp born
/// with the saved mode, written and synced, then renamed over the target.
/// The published target can be read-only, so it is never opened for writing.
/// A temp that fails before the rename holds no trusted bytes and goes away.
/// One that fails at the rename holds the only copy of the original bytes,
/// so it stays on disk and the error names it.
async fn restore_bytes(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "restore target has no parent directory",
        )
    })?;
    let (temp, mut handle) = create_sibling_temp(parent, mode).await?;
    let staged = async {
        stamp_saved_mode(&handle, mode).await?;
        handle.write_all(bytes).await?;
        handle.sync_all().await
    }
    .await;
    drop(handle);
    if let Err(error) = staged {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error);
    }
    tokio::fs::rename(&temp, path).await.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("{error}. Pre-image retained at {}", temp.display()),
        )
    })
}

async fn rollback_error(
    path: &std::path::Path,
    error: std::io::Error,
    temps: &[(&StagedFileOwned, PendingTemp)],
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
                Undo::RestoreBytes { path, .. } | Undo::RemoveInstalled(path) => {
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
    temps: &[(&StagedFileOwned, PendingTemp)],
    undo: &mut Vec<Undo<'a>>,
) -> Result<(), EngineError> {
    let backup = trash_path(session, &file.absolute_path);
    if let Err(error) = tokio::fs::rename(&file.absolute_path, &backup).await {
        return Err(rollback_error(&file.path, error, temps, undo).await);
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

async fn cleanup_temps(temps: &[(&StagedFileOwned, PendingTemp)]) {
    for (_, temp) in temps {
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
    use std::{num::NonZeroU64, path::PathBuf, process::Command, sync::Arc, time::Duration};

    use dal_agent::ext::BoxFuture;
    use dal_core::{CallId, GenerationId, SessionId, TurnId};
    use tokio_util::sync::CancellationToken;

    use crate::{
        patch::{
            ir::{
                DialectId, EngineError, ErrorClass, Operation, Output, Plan, RenameTarget,
                StagedFileOwned,
            },
            snapshot::SnapshotStore,
        },
        search::index::Index,
    };

    use super::super::PatchSession;
    use super::{Pace, Phase, Settled, Unpaced, apply_with, detached, prepare_write_set};

    async fn apply_files(session: &PatchSession, plan: &Plan) -> Result<Output, EngineError> {
        match apply_with(session, plan, &Unpaced).await? {
            Settled::Done(output) => Ok(output),
            Settled::Cancelled => panic!("the commit was cancelled"),
        }
    }

    #[tokio::test]
    async fn rejected_rename_parent_cleans_up_earlier_staged_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let source = workspace.join("source.txt");
        let created = workspace.join("created");
        // A file cannot serve as the parent of the rename destination.
        // Staging this rename fails after staging the earlier create.
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
            cancel: CancellationToken::new(),
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

        // Refusal happens before publication and must clean up every temp.
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

    fn failed_rollback_session(workspace: &std::path::Path) -> PatchSession {
        PatchSession {
            workspace: workspace.to_path_buf(),
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
            cancel: CancellationToken::new(),
        }
    }

    #[derive(Clone, Copy)]
    struct FailedRollbackPaths<'a> {
        update: &'a std::path::Path,
        create: &'a std::path::Path,
        restored: &'a std::path::Path,
        destination: &'a std::path::Path,
        source: &'a std::path::Path,
        earlier: &'a std::path::Path,
        backup: &'a std::path::Path,
    }

    async fn seed_failed_rollback(paths: FailedRollbackPaths<'_>) {
        let FailedRollbackPaths {
            update,
            create,
            restored,
            destination,
            source,
            earlier,
            backup,
        } = paths;
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
    }

    fn failed_rollback_file(
        source: &std::path::Path,
        destination: &std::path::Path,
    ) -> StagedFileOwned {
        StagedFileOwned {
            path: PathBuf::from("failed-source"),
            absolute_path: source.join("."),
            before: Some(b"source\n".to_vec().into_boxed_slice()),
            after: Some(b"source\n".to_vec().into_boxed_slice()),
            op: Operation::Rename,
            renamed_to: Some(RenameTarget {
                path: PathBuf::from("blocked-destination"),
                absolute_path: destination.to_path_buf(),
            }),
            hunks: Vec::new(),
        }
    }

    fn failed_rollback_undo(paths: FailedRollbackPaths<'_>) -> Vec<super::Undo<'_>> {
        let FailedRollbackPaths {
            update,
            create,
            restored,
            destination,
            earlier,
            backup,
            ..
        } = paths;
        vec![
            super::Undo::RestoreBytes {
                path: update,
                bytes: b"before\n",
                mode: None,
            },
            super::Undo::RemoveInstalled(create),
            super::Undo::RestoreBytes {
                path: restored,
                bytes: b"before\n",
                mode: None,
            },
            super::Undo::RemoveInstalled(destination),
            super::Undo::RestoreMoved {
                source: earlier,
                backup: backup.to_path_buf(),
            },
        ]
    }

    #[tokio::test]
    async fn failed_rollback_reports_every_stranded_path_and_preserves_backups() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let session = failed_rollback_session(&workspace);
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
        let fixture = FailedRollbackPaths {
            update: update.as_path(),
            create: create.as_path(),
            restored: restored.as_path(),
            destination: destination.as_path(),
            source: source.as_path(),
            earlier: earlier.as_path(),
            backup: backup.as_path(),
        };
        seed_failed_rollback(fixture).await;
        let file = failed_rollback_file(source, destination);
        let mut undo = failed_rollback_undo(fixture);
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
        // The pre-image whose replacement failed is the only copy of the
        // original bytes: it stays on disk and the error names it.
        let mut retained = Vec::new();
        for entry in std::fs::read_dir(&workspace).expect("read workspace") {
            let name = entry.expect("directory entry").file_name();
            if name.to_string_lossy().starts_with(".dalgon-patch-") {
                retained.push(workspace.join(name));
            }
        }
        assert_eq!(retained.len(), 1, "{retained:?}");
        let pre_image = &retained[0];
        assert_eq!(
            std::fs::read(pre_image).expect("retained pre-image"),
            b"before\n"
        );
        let name = pre_image.to_str().expect("temp path");
        assert!(error.message.contains(name), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "std has no umask API; a child shell scopes the umask to one test process"
    )]
    async fn commit_published_modes_respect_umask_and_preserve_updates() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        const CHILD: &str = "DAL_TOOLS_UMASK_CHILD";
        const TEST: &str = "patch::write::commit::tests::commit_published_modes_respect_umask_and_preserve_updates";

        if std::env::var_os(CHILD).is_none() {
            // Rust's standard library has no umask API. Run the real test body
            // in a child shell, whose POSIX umask is scoped to that process.
            let exe = std::env::current_exe().expect("test executable");
            let status = Command::new("sh")
                .args([
                    "-c",
                    "umask 027; exec \"$1\" --exact \"$2\" --nocapture",
                    "dal-tools-umask",
                    exe.to_str().expect("test executable path"),
                    TEST,
                ])
                .env(CHILD, "1")
                .status()
                .expect("run umask child");
            assert!(status.success(), "umask child exited with {status}");
            return;
        }

        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let new_path = workspace.join("new.txt");
        let existing_path = workspace.join("existing.txt");
        tokio::fs::write(&existing_path, b"before\n")
            .await
            .expect("seed existing file");
        tokio::fs::set_permissions(&existing_path, std::fs::Permissions::from_mode(0o600))
            .await
            .expect("set existing mode");

        let session = failed_rollback_session(&workspace);
        let plan = Plan {
            style: DialectId::Replace,
            files: vec![
                StagedFileOwned {
                    path: PathBuf::from("new.txt"),
                    absolute_path: new_path.clone(),
                    before: None,
                    after: Some(b"new\n".to_vec().into_boxed_slice()),
                    op: Operation::Create,
                    renamed_to: None,
                    hunks: Vec::new(),
                },
                StagedFileOwned {
                    path: PathBuf::from("existing.txt"),
                    absolute_path: existing_path.clone(),
                    before: Some(b"before\n".to_vec().into_boxed_slice()),
                    after: Some(b"after\n".to_vec().into_boxed_slice()),
                    op: Operation::Update,
                    renamed_to: None,
                    hunks: Vec::new(),
                },
            ],
            findings: Vec::new(),
        };

        apply_files(&session, &plan)
            .await
            .expect("publish mode test plan");
        let new_mode = std::fs::metadata(&new_path).expect("new metadata").mode() & 0o777;
        assert_eq!(new_mode, 0o644 & !0o027, "new mode {new_mode:o}");
        let existing_mode = std::fs::metadata(&existing_path)
            .expect("existing metadata")
            .mode()
            & 0o777;
        assert_eq!(existing_mode, 0o600, "existing mode {existing_mode:o}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sibling_temp_is_born_with_the_decided_mode() {
        use std::os::unix::fs::MetadataExt as _;

        let directory = tempfile::tempdir().expect("temporary directory");
        // The umask only removes bits at creation, so a temp born with its
        // decided mode never carries a bit outside it. A constant wider
        // create mode would show as group or other access on a private file.
        for (decided, ceiling) in [(Some(0o600), 0o600), (Some(0o400), 0o400), (None, 0o644)] {
            let (temp, handle) = super::create_sibling_temp(directory.path(), decided)
                .await
                .expect("create staged temp");
            let born = handle.metadata().await.expect("temp metadata").mode() & 0o777;
            assert_eq!(born & !ceiling, 0, "{decided:?} born as {born:o}");
            drop(handle);
            tokio::fs::remove_file(temp).await.expect("remove temp");
        }
    }

    fn update(
        workspace: &std::path::Path,
        name: &str,
        before: &[u8],
        after: &[u8],
    ) -> StagedFileOwned {
        StagedFileOwned {
            path: PathBuf::from(name),
            absolute_path: workspace.join(name),
            before: Some(before.to_vec().into_boxed_slice()),
            after: Some(after.to_vec().into_boxed_slice()),
            op: Operation::Update,
            renamed_to: None,
            hunks: Vec::new(),
        }
    }

    fn plan_of(files: Vec<StagedFileOwned>) -> Plan {
        Plan {
            style: DialectId::Replace,
            files,
            findings: Vec::new(),
        }
    }

    /// Names the temps and trash a commit may leave behind in `workspace`.
    fn leftovers(workspace: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(workspace)
            .expect("read workspace")
            .map(|entry| entry.expect("directory entry").file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".dalgon-"))
            .collect()
    }

    fn two_file_workspace() -> (tempfile::TempDir, PathBuf, Plan) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        std::fs::write(workspace.join("a.txt"), b"one\n").expect("seed a");
        std::fs::write(workspace.join("b.txt"), b"two\n").expect("seed b");
        let plan = plan_of(vec![
            update(&workspace, "a.txt", b"one\n", b"ONE\n"),
            update(&workspace, "b.txt", b"two\n", b"TWO\n"),
        ]);
        (directory, workspace, plan)
    }

    fn assert_files(workspace: &std::path::Path, a: &[u8], b: &[u8]) {
        assert_eq!(std::fs::read(workspace.join("a.txt")).expect("read a"), a);
        assert_eq!(std::fs::read(workspace.join("b.txt")).expect("read b"), b);
        assert!(
            leftovers(workspace).is_empty(),
            "{:?}",
            leftovers(workspace)
        );
    }

    /// Cancels the call when the commit reaches `when`.
    struct CancelAt<'a> {
        when: Phase,
        token: &'a CancellationToken,
    }

    impl Pace for CancelAt<'_> {
        fn at(&self, phase: Phase) -> BoxFuture<'_, ()> {
            if phase == self.when {
                self.token.cancel();
            }
            Box::pin(std::future::ready(()))
        }
    }

    /// Reports the first publication, then holds the commit there until released.
    struct Hold {
        published: tokio::sync::mpsc::Sender<()>,
        release: Arc<tokio::sync::Notify>,
    }

    impl Pace for Hold {
        fn at(&self, phase: Phase) -> BoxFuture<'_, ()> {
            if phase != Phase::Published {
                return Box::pin(std::future::ready(()));
            }
            let _ = self.published.try_send(());
            Box::pin(self.release.notified())
        }
    }

    /// Runs one index query at the instant a concurrent build could capture
    /// the pre-write bytes under the generation the first invalidation made.
    struct BuildInWindow {
        index: Arc<Index>,
        workspace: PathBuf,
    }

    impl Pace for BuildInWindow {
        fn at(&self, phase: Phase) -> BoxFuture<'_, ()> {
            if phase != Phase::Staged {
                return Box::pin(std::future::ready(()));
            }
            Box::pin(async move {
                let found = candidates(&self.index, &self.workspace, "before_marker").await;
                assert_eq!(
                    found,
                    [PathBuf::from("a.txt")],
                    "the window build saw old bytes"
                );
            })
        }
    }

    async fn candidates(index: &Index, workspace: &std::path::Path, text: &str) -> Vec<PathBuf> {
        let clauses = vec![vec![text.as_bytes().to_vec()]];
        index
            .search_candidates(workspace, &clauses, false, None)
            .await
            .expect("query the index")
            .expect("an index root is configured")
    }

    #[tokio::test]
    async fn a_file_that_vanished_while_approving_is_stale_even_when_it_was_empty() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let session = failed_rollback_session(&workspace);
        let plan = plan_of(vec![update(&workspace, "empty.txt", b"", b"filled\n")]);

        let error = apply_files(&session, &plan)
            .await
            .expect_err("a vanished target is stale, not an empty file");

        assert_eq!(error.class, ErrorClass::Stale, "{error}");
        assert!(
            error.message.contains("changed while you were approving"),
            "{error}"
        );
        assert!(
            std::fs::symlink_metadata(workspace.join("empty.txt")).is_err(),
            "nothing is recreated"
        );
        assert_eq!(leftovers(&workspace), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_symlink_where_a_file_was_to_be_created_is_kept() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let target = workspace.join("made.txt");
        std::os::unix::fs::symlink(workspace.join("nowhere.txt"), &target)
            .expect("dangling symlink");
        let session = failed_rollback_session(&workspace);
        let mut file = update(&workspace, "made.txt", b"", b"made\n");
        file.before = None;
        file.op = Operation::Create;

        let error = apply_files(&session, &plan_of(vec![file]))
            .await
            .expect_err("the link occupies the name");

        assert_eq!(error.class, ErrorClass::File, "{error}");
        let metadata = std::fs::symlink_metadata(&target).expect("the link survives");
        assert!(metadata.file_type().is_symlink());
        assert!(!workspace.join("nowhere.txt").exists());
        assert_eq!(leftovers(&workspace), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_create_whose_name_cannot_be_probed_is_refused_not_assumed_vacant() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        std::fs::write(workspace.join("blocker"), b"file\n").expect("seed blocker");
        let session = failed_rollback_session(&workspace);
        let mut file = update(&workspace, "blocker/new.txt", b"", b"made\n");
        file.before = None;
        file.op = Operation::Create;

        let error = apply_files(&session, &plan_of(vec![file]))
            .await
            .expect_err("a name under a file cannot be proved free");

        assert_eq!(error.class, ErrorClass::Io, "{error}");
        assert!(error.message.contains("cannot read"), "{error}");
        assert!(error.message.contains("Nothing was written"), "{error}");
        assert_eq!(
            std::fs::read(workspace.join("blocker")).expect("read blocker"),
            b"file\n"
        );
        assert_eq!(leftovers(&workspace), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_call_cancelled_before_staging_writes_nothing() {
        let (_directory, workspace, plan) = two_file_workspace();
        let session = failed_rollback_session(&workspace);
        session.cancel.cancel();

        let settled = apply_with(&session, &plan, &Unpaced)
            .await
            .expect("a cancelled call is not a failure");

        assert!(matches!(settled, Settled::Cancelled), "{settled:?}");
        assert_files(&workspace, b"one\n", b"two\n");
    }

    #[tokio::test]
    async fn cancellation_before_publication_removes_every_temp_and_changes_nothing() {
        let (_directory, workspace, plan) = two_file_workspace();
        let session = failed_rollback_session(&workspace);
        let pace = CancelAt {
            when: Phase::Staged,
            token: &session.cancel,
        };

        let settled = apply_with(&session, &plan, &pace)
            .await
            .expect("a cancelled call is not a failure");

        assert!(matches!(settled, Settled::Cancelled), "{settled:?}");
        assert_files(&workspace, b"one\n", b"two\n");
    }

    #[tokio::test]
    async fn cancellation_after_the_first_publication_is_ignored_until_the_plan_finishes() {
        let (_directory, workspace, plan) = two_file_workspace();
        let session = failed_rollback_session(&workspace);
        let pace = CancelAt {
            when: Phase::Published,
            token: &session.cancel,
        };

        let settled = apply_with(&session, &plan, &pace)
            .await
            .expect("a started publication runs to the end");

        assert!(matches!(settled, Settled::Done(_)), "{settled:?}");

        assert!(
            session.cancel.is_cancelled(),
            "the call was cancelled mid-publication"
        );
        assert_files(&workspace, b"ONE\n", b"TWO\n");
    }

    #[tokio::test]
    async fn dropping_the_commit_after_the_first_publication_still_publishes_every_file() {
        let (_directory, workspace, plan) = two_file_workspace();
        let session = failed_rollback_session(&workspace);
        let (published, mut reached) = tokio::sync::mpsc::channel(1);
        let release = Arc::new(tokio::sync::Notify::new());
        let hold = Arc::new(Hold {
            published,
            release: Arc::clone(&release),
        });
        let locks = plan.clone();

        tokio::select! {
            result = detached(&session, plan, hold) => {
                panic!("the commit ended before it was held: {result:?}");
            }
            signal = reached.recv() => {
                assert!(signal.is_some(), "the commit reached its first publication");
            }
        }
        // The caller is gone, so the commit's own task is all that is left.
        // It still holds the path locks: taking them again returns only once
        // it has finished, and then reports the plan stale because the files
        // already hold the after images.
        release.notify_one();
        let outcome =
            tokio::time::timeout(Duration::from_secs(30), prepare_write_set(&session, &locks))
                .await
                .expect("the commit releases its locks");

        let error = outcome.expect_err("the files already hold the after images");
        assert_eq!(error.class, ErrorClass::Stale, "{error}");
        assert_files(&workspace, b"ONE\n", b"TWO\n");
    }

    #[tokio::test]
    async fn a_build_inside_the_write_window_never_serves_pre_write_bytes_as_fresh() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let root = tempfile::tempdir().expect("index root");
        std::fs::write(workspace.join("a.txt"), b"before_marker\n").expect("seed");
        let index = Index::new(Some(root.path().to_path_buf()));
        let mut session = failed_rollback_session(&workspace);
        session.index = Arc::clone(&index);
        let plan = plan_of(vec![update(
            &workspace,
            "a.txt",
            b"before_marker\n",
            b"after_marker\n",
        )]);
        let pace = BuildInWindow {
            index: Arc::clone(&index),
            workspace: workspace.clone(),
        };

        apply_with(&session, &plan, &pace)
            .await
            .expect("publish through the window");

        assert_eq!(
            candidates(&index, &workspace, "after_marker").await,
            [PathBuf::from("a.txt")],
            "the invalidation after publication discards the window build"
        );
        assert_eq!(
            candidates(&index, &workspace, "before_marker").await,
            Vec::<PathBuf>::new()
        );
    }
}
