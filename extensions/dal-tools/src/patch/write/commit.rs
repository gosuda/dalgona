//! Atomic commit: locks, temps, ordered operations, and truthful outcomes.

use std::{path::PathBuf, sync::Arc};

use super::super::ir::{
    Diff, DiffFile, EngineError, ErrorClass, FileChange, FindingSeverity, Output, Plan,
    StagedFileOwned,
};

use super::super::resolve::revalidate_path;
use super::{PatchSession, ensure_regular_target, non_regular_target};

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


#[derive(Clone)]
struct LocatedPath {
    // The directory handle, rather than an absolute path, carries write authority.
    parent: Arc<cap_std::fs::Dir>,
    name: std::ffi::OsString,
    absolute: PathBuf,
}

impl LocatedPath {
    fn bind(
        workspace_dir: &cap_std::fs::Dir,
        workspace: &std::path::Path,
        absolute: &std::path::Path,
        create_parent: bool,
    ) -> std::io::Result<Self> {
        let relative = absolute.strip_prefix(workspace).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "patch target is outside the workspace",
            )
        })?;
        let name = relative.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "patch target has no file name",
            )
        })?;
        let parent_path = relative.parent().unwrap_or(std::path::Path::new(""));
        if create_parent && !parent_path.as_os_str().is_empty() {
            workspace_dir.create_dir_all(parent_path)?;
        }
        let parent = if parent_path.as_os_str().is_empty() {
            workspace_dir.try_clone()?
        } else {
            workspace_dir.open_dir(parent_path)?
        };
        Ok(Self {
            parent: Arc::new(parent),
            name: name.to_os_string(),
            absolute: absolute.to_path_buf(),
        })
    }

    fn sibling(&self, name: std::ffi::OsString) -> Self {
        Self {
            parent: Arc::clone(&self.parent),
            absolute: self.absolute.with_file_name(&name),
            name,
        }
    }

    fn rename_to(&self, target: &Self) -> std::io::Result<()> {
        self.parent.rename(
            std::path::Path::new(&self.name),
            &target.parent,
            std::path::Path::new(&target.name),
        )
    }

    fn remove(&self) -> std::io::Result<()> {
        self.parent.remove_file(std::path::Path::new(&self.name))
    }
}

struct StagedTemp<'a> {
    file: &'a StagedFileOwned,
    temp: LocatedPath,
    target: LocatedPath,
}

fn file_error(path: &std::path::Path, error: std::io::Error) -> EngineError {
    EngineError::new(
        ErrorClass::Io,
        format!(
            "patch: cannot write {}: {error}. Nothing was written.",
            path.display()
        ),
    )
}

pub(crate) async fn apply_files(
    session: &PatchSession,
    plan: &Plan,
) -> Result<Output, EngineError> {
    let _guards = prepare_write_set(session, plan).await?;
    let workspace = std::fs::canonicalize(&session.workspace)
        .map_err(|error| file_error(&session.workspace, error))?;
    let root = cap_std::fs::Dir::open_ambient_dir(&workspace, cap_std::ambient_authority())
        .map_err(|error| file_error(&workspace, error))?;

    // Pin each source before staging writes, and check its before-image
    // through that handle. An external directory swap cannot redirect reads.
    let mut sources = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        if file.op == super::super::ir::Operation::Create {
            sources.push(None);
            continue;
        }
        let source = LocatedPath::bind(&root, &workspace, &file.absolute_path, false)
            .map_err(|error| file_error(&file.path, error))?;
        if let Some(before) = file.before.as_deref() {
            let metadata = source
                .parent
                .metadata(std::path::Path::new(&source.name))
                .map_err(|error| file_error(&file.path, error))?;
            if !metadata.is_file() {
                return Err(non_regular_target(&file.path));
            }
            let current = source
                .parent
                .read(std::path::Path::new(&source.name))
                .map_err(|error| file_error(&file.path, error))?;
            if current != before {
                return Err(EngineError::new(
                    ErrorClass::Stale,
                    format!(
                        "patch: {} changed while you were approving. Nothing was written. Try again.",
                        file.path.display()
                    ),
                ));
            }
        }
        sources.push(Some(source));
    }

    // Pin destinations as well. Never return to their ambient pathnames.
    let mut targets = Vec::with_capacity(plan.files.len());
    for (file, source) in plan.files.iter().zip(&sources) {
        if file.op == super::super::ir::Operation::Delete {
            targets.push(None);
            continue;
        }
        let target = match (file.op, source) {
            (super::super::ir::Operation::Create | super::super::ir::Operation::Rename, _) => {
                LocatedPath::bind(&root, &workspace, write_target(file), true)
                    .map_err(|error| file_error(&file.path, error))?
            }
            (_, Some(source)) => source.clone(),
            (_, None) => {
                return Err(EngineError::new(
                    ErrorClass::Resolve,
                    format!("patch: {} has no pinned source.", file.path.display()),
                ));
            }
        };
        if matches!(
            file.op,
            super::super::ir::Operation::Create | super::super::ir::Operation::Rename
        ) {
            match target
                .parent
                .symlink_metadata(std::path::Path::new(&target.name))
            {
                Ok(metadata) => {
                    let display = file
                        .renamed_to
                        .as_ref()
                        .filter(|_| file.op == super::super::ir::Operation::Rename)
                        .map_or(file.path.as_path(), |destination| destination.path.as_path());
                    if !metadata.is_file() {
                        return Err(non_regular_target(display));
                    }
                    return Err(EngineError::new(
                        ErrorClass::File,
                        format!("patch: {} already exists.", display.display()),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(file_error(&file.path, error)),
            }
        }
        targets.push(Some(target));
    }

    let mut temps = Vec::new();
    for (file, target) in plan.files.iter().zip(&targets) {
        if let Some(target) = target
            && let Err(error) = stage_temp(file, target, &mut temps).await
        {
            cleanup_temps(&temps);
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
    for (file, source) in plan.files.iter().zip(&sources) {
        if file.op == super::super::ir::Operation::Delete
            && let Some(source) = source
        {
            trash_source(&file.path, source, &temps, &mut undo)?;
        }
    }
    for staged in &temps {
        if let Err(error) = staged.temp.rename_to(&staged.target) {
            return Err(rollback_error(&staged.target.absolute, error, &temps, &undo));
        }
        undo.push(match (staged.file.op, staged.file.before.as_deref()) {
            (super::super::ir::Operation::Rename, _) | (_, None) => {
                Undo::RemoveInstalled(staged.target.clone())
            }
            (_, Some(before)) => Undo::RestoreBytes(staged.target.clone(), before),
        });
    }
    for (file, source) in plan.files.iter().zip(&sources) {
        if file.op == super::super::ir::Operation::Rename
            && let Some(source) = source
        {
            trash_source(&file.path, source, &temps, &mut undo)?;
        }
    }

    #[cfg(unix)]
    for target in targets.iter().flatten().chain(sources.iter().flatten()) {
        let _ = target
            .parent
            .try_clone()
            .and_then(|dir| dir.into_std_file().sync_all());
    }
    for action in undo {
        if let Undo::RestoreMoved { backup, .. } = action {
            let _ = backup.remove();
        }
    }
    cleanup_temps(&temps);
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

/// Phase A: create and sync a temp through the pinned parent directory.
async fn stage_temp<'a>(
    file: &'a StagedFileOwned,
    target: &LocatedPath,
    temps: &mut Vec<StagedTemp<'a>>,
) -> Result<(), EngineError> {
    let Some(after) = file.after.as_deref() else {
        return Ok(());
    };
    let temp = target.sibling(
        format!(
            ".dalgon-patch-{}-{:032x}.tmp",
            std::process::id(),
            nonce_u128()
        )
        .into(),
    );
    let write = async {
        use tokio::io::AsyncWriteExt as _;
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let file = temp
            .parent
            .open_with(std::path::Path::new(&temp.name), &options)?;
        let mut handle = tokio::fs::File::from_std(file.into_std());
        handle.write_all(after).await?;
        handle.sync_all().await
    }
    .await;
    if let Err(error) = write {
        let _ = temp.remove();
        return Err(file_error(&file.path, error));
    }
    temps.push(StagedTemp {
        file,
        temp,
        target: target.clone(),
    });
    Ok(())
}

enum Undo<'a> {
    RestoreBytes(LocatedPath, &'a [u8]),
    RemoveInstalled(LocatedPath),
    RestoreMoved {
        source: LocatedPath,
        backup: LocatedPath,
    },
}

impl Undo<'_> {
    fn restore(&self) -> std::io::Result<()> {
        match self {
            Self::RestoreBytes(path, before) => {
                path.parent.write(std::path::Path::new(&path.name), before)
            }
            Self::RemoveInstalled(path) => match path.remove() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
            Self::RestoreMoved { source, backup } => backup.rename_to(source),
        }
    }
}

fn rollback_error(
    path: &std::path::Path,
    error: std::io::Error,
    temps: &[StagedTemp<'_>],
    undo: &[Undo<'_>],
) -> EngineError {
    use std::fmt::Write as _;

    let mut failures = Vec::new();
    for action in undo.iter().rev() {
        if let Err(error) = action.restore() {
            failures.push((action, error));
        }
    }
    cleanup_temps(temps);
    let mut message = format!("patch: cannot write {}: {error}.", path.display());
    if failures.is_empty() {
        message.push_str(" Nothing was written.");
    } else {
        message.push_str(" Rollback was incomplete. Inspect these paths before retrying:");
        for (action, error) in failures {
            match action {
                Undo::RestoreBytes(path, _) | Undo::RemoveInstalled(path) => {
                    let _ = write!(message, "\n{}: {error}.", path.absolute.display());
                }
                Undo::RestoreMoved { source, backup } => {
                    let _ = write!(
                        message,
                        "\n{}: {error}. Backup retained at {}.",
                        source.absolute.display(),
                        backup.absolute.display()
                    );
                }
            }
        }
    }
    EngineError::new(ErrorClass::Io, message)
}

fn trash_source<'a>(
    display: &std::path::Path,
    source: &LocatedPath,
    temps: &[StagedTemp<'_>],
    undo: &mut Vec<Undo<'a>>,
) -> Result<(), EngineError> {
    let backup = source.sibling(
        format!(
            ".dalgon-trash-{}-{:032x}.tmp",
            std::process::id(),
            nonce_u128()
        )
        .into(),
    );
    if let Err(error) = source.rename_to(&backup) {
        return Err(rollback_error(display, error, temps, undo));
    }
    undo.push(Undo::RestoreMoved {
        source: source.clone(),
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

fn cleanup_temps(temps: &[StagedTemp<'_>]) {
    for staged in temps {
        let _ = staged.temp.remove();
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
    async fn invalid_later_target_prevents_earlier_create() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = std::fs::canonicalize(directory.path()).expect("workspace");
        let source = workspace.join("source.txt");
        let created = workspace.join("created");
        // The rename destination must fail. unix refuses a file target
        // under a missing directory at Phase-B rename; Windows accepts
        // that spelling, so on Windows the destination sits under a
        // regular file and prepare refuses it instead.
        #[cfg(unix)]
        let destination = {
            let blocker = workspace.join("other");
            tokio::fs::create_dir(&blocker)
                .await
                .expect("seed the directory that blocks the destination");
            blocker
        };
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
                            PathBuf::from("other")
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
            .expect_err("later rename destination must fail");

        // The refused destination can surface as a Phase-B rename error
        // (Io) or an earlier prepare refusal (Stale) depending on which
        // validator reaches it first on each platform.
        assert!(
            matches!(error.class, ErrorClass::Io | ErrorClass::Stale | ErrorClass::File),
            "{error:?}"
        );
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
        let root = cap_std::fs::Dir::open_ambient_dir(
            &workspace,
            cap_std::ambient_authority(),
        )
        .expect("workspace handle");
        let location = |path: &std::path::Path| {
            super::LocatedPath::bind(&root, &workspace, path, false)
                .expect("pinned path")
        };
        let missing = location(&workspace.join("missing-source"));
        let mut undo = vec![
            super::Undo::RestoreBytes(location(update), b"before\n"),
            super::Undo::RemoveInstalled(location(create)),
            super::Undo::RestoreBytes(location(restored), b"before\n"),
            super::Undo::RemoveInstalled(location(destination)),
            super::Undo::RestoreMoved {
                source: location(earlier),
                backup: location(backup),
            },
        ];
        let error = super::trash_source(&PathBuf::from("failed-source"), &missing, &[], &mut undo)
            .expect_err("missing source cannot move");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn swapped_parent_does_not_redirect_install_delete_or_rollback() {
        use std::io::Write as _;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let parent = workspace.path().join("parent");
        std::fs::create_dir(&parent).expect("parent");
        std::fs::write(parent.join("source.txt"), b"original\n").expect("source");

        let root = cap_std::fs::Dir::open_ambient_dir(
            workspace.path(),
            cap_std::ambient_authority(),
        )
        .expect("workspace handle");
        let installed = super::LocatedPath::bind(
            &root,
            workspace.path(),
            &parent.join("installed.txt"),
            false,
        )
        .expect("pinned install target");
        let source = super::LocatedPath::bind(
            &root,
            workspace.path(),
            &parent.join("source.txt"),
            false,
        )
        .expect("pinned source");
        let temp = installed.sibling(".dalgon-patch-test.tmp".into());
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = temp
            .parent
            .open_with(std::path::Path::new(&temp.name), &options)
            .expect("staged temp")
            .into_std();
        file.write_all(b"after\n").expect("stage bytes");
        file.sync_all().expect("sync bytes");
        drop(file);

        std::fs::rename(&parent, workspace.path().join("moved")).expect("swap directory");
        std::os::unix::fs::symlink(outside.path(), &parent)
            .expect("point workspace parent outside");

        temp.rename_to(&installed).expect("install under pinned parent");
        assert_eq!(
            std::fs::read(workspace.path().join("moved/installed.txt")).expect("installed"),
            b"after\n"
        );
        assert!(!outside.path().join("installed.txt").exists());
        assert!(!outside.path().join(".dalgon-patch-test.tmp").exists());

        let mut undo = Vec::new();
        super::trash_source(&PathBuf::from("parent/source.txt"), &source, &[], &mut undo)
            .expect("delete under pinned parent");
        assert!(!workspace.path().join("moved/source.txt").exists());
        assert!(!outside.path().join("source.txt").exists());
        for action in undo.iter().rev() {
            action.restore().expect("restore deleted file");
        }
        assert_eq!(
            std::fs::read(workspace.path().join("moved/source.txt")).expect("restored"),
            b"original\n"
        );
        super::Undo::RemoveInstalled(installed)
            .restore()
            .expect("remove installed file under pinned parent");
        assert!(!workspace.path().join("moved/installed.txt").exists());
        assert!(!outside.path().join("installed.txt").exists());
    }

}
