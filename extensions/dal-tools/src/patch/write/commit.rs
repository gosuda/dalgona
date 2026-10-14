//! Atomic commit: locks, temps, ordered operations, and truthful outcomes.

use std::{path::PathBuf, sync::Arc};

use super::super::ir::{
    Diff, DiffFile, EngineError, ErrorClass, FileChange, FileId, FindingSeverity, Output, Plan,
    StagedFileOwned,
};

use super::super::resolve::revalidate_path;
use super::{PatchSession, non_regular_target};

fn lock_table()
-> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>> {
    static TABLE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Acquires the canonical write set in ascending byte order — sources and
/// rename destinations together. The caller verifies identities and
/// before-images under these guards using pinned directory handles, so
/// approval cannot go stale.
async fn prepare_write_set(
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
    /// Splits an approved canonical path into its workspace-relative parent
    /// and final name. Pure path math: it performs no filesystem access and
    /// creates nothing.
    fn locate(
        workspace: &std::path::Path,
        absolute: &std::path::Path,
    ) -> std::io::Result<(PathBuf, std::ffi::OsString)> {
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
        Ok((
            relative
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .to_path_buf(),
            name.to_os_string(),
        ))
    }

    /// Opens the parent's directory handle for a located path.
    fn open(
        root: &cap_std::fs::Dir,
        parent: &std::path::Path,
        name: std::ffi::OsString,
        absolute: &std::path::Path,
    ) -> std::io::Result<Self> {
        Ok(Self {
            parent: Arc::new(open_parent(root, parent)?),
            name,
            absolute: absolute.to_path_buf(),
        })
    }

    /// Locate and open in one step, creating the parent when asked.
    fn bind(
        workspace_dir: &cap_std::fs::Dir,
        workspace: &std::path::Path,
        absolute: &std::path::Path,
        create_parent: bool,
    ) -> std::io::Result<Self> {
        let (parent_path, name) = Self::locate(workspace, absolute)?;
        if create_parent && !parent_path.as_os_str().is_empty() {
            workspace_dir.create_dir_all(&parent_path)?;
        }
        Self::open(workspace_dir, &parent_path, name, absolute)
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

/// Opens a located parent directory through the pinned root.
fn open_parent(
    root: &cap_std::fs::Dir,
    parent: &std::path::Path,
) -> std::io::Result<cap_std::fs::Dir> {
    if parent.as_os_str().is_empty() {
        root.try_clone()
    } else {
        root.open_dir(parent)
    }
}

/// One resolved create/rename destination.
enum Target {
    /// Parent was already open and the destination proved absent.
    Bound(LocatedPath),
    /// The parent was missing at validation; it is created only after
    /// every destination has passed its checks.
    Pending {
        parent: PathBuf,
        name: std::ffi::OsString,
        absolute: PathBuf,
    },
}

struct StagedTemp<'a> {
    file: &'a StagedFileOwned,
    temp: LocatedPath,
    target: LocatedPath,
}

fn file_error(path: &std::path::Path, error: &std::io::Error) -> EngineError {
    EngineError::new(
        ErrorClass::Io,
        format!(
            "patch: cannot write {}: {error}. Nothing was written.",
            path.display()
        ),
    )
}

/// A source or its parent vanished or moved after approval: the staged
/// before-image no longer describes this call, which is a stale plan.
fn source_error(file: &StagedFileOwned, error: &std::io::Error) -> EngineError {
    if error.kind() == std::io::ErrorKind::NotFound && file.before.is_some() {
        stale_changed(&file.path)
    } else {
        file_error(&file.path, error)
    }
}

fn stale_changed(display: &std::path::Path) -> EngineError {
    EngineError::new(
        ErrorClass::Stale,
        format!(
            "patch: {} changed while you were approving. Nothing was written. Try again.",
            display.display()
        ),
    )
}

fn stale_moved(display: &std::path::Path) -> EngineError {
    EngineError::new(
        ErrorClass::Stale,
        format!(
            "patch: {} resolves to a different file than when it was staged. Nothing was written. Try again.",
            display.display()
        ),
    )
}

/// Identity of a capability metadata result, matching [`FileId::of`].
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "cap-std unix metadata always reports device and inode"
)]
fn cap_file_id(metadata: &cap_std::fs::Metadata) -> Option<FileId> {
    use cap_std::fs::MetadataExt as _;
    Some(FileId {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

/// Identity of a capability metadata result, matching [`FileId::of`].
#[cfg(windows)]
fn cap_file_id(metadata: &cap_std::fs::Metadata) -> Option<FileId> {
    use cap_std::fs::MetadataExt as _;
    Some(FileId {
        dev: u64::from(metadata.volume_serial_number()?),
        ino: metadata.file_index()?,
    })
}

/// Identity of a capability metadata result, matching [`FileId::of`].
#[cfg(not(any(unix, windows)))]
fn cap_file_id(_metadata: &cap_std::fs::Metadata) -> Option<FileId> {
    None
}

/// Identity of an open directory handle.
fn cap_dir_id(dir: &cap_std::fs::Dir) -> Option<FileId> {
    dir.dir_metadata()
        .ok()
        .and_then(|metadata| cap_file_id(&metadata))
}

/// A recorded identity must still hold for the pinned object. `None`
/// recorded means nothing was approved to compare against; `None` actual
/// means the filesystem cannot prove the object, which fails closed.
fn check_identity(
    recorded: Option<FileId>,
    actual: Option<FileId>,
    display: &std::path::Path,
) -> Result<(), EngineError> {
    let Some(recorded) = recorded else {
        return Ok(());
    };
    if actual == Some(recorded) {
        Ok(())
    } else {
        Err(stale_moved(display))
    }
}

/// Display path for a create/rename destination.
fn dest_display(file: &StagedFileOwned) -> &std::path::Path {
    file.renamed_to
        .as_ref()
        .filter(|_| file.op == super::super::ir::Operation::Rename)
        .map_or(file.path.as_path(), |destination| {
            destination.path.as_path()
        })
}

/// A create/rename destination must be absent at validation time.
fn check_dest_absent(
    file: &StagedFileOwned,
    dir: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
) -> Result<(), EngineError> {
    match dir.symlink_metadata(std::path::Path::new(name)) {
        Ok(metadata) => {
            let display = dest_display(file);
            if !metadata.is_file() {
                return Err(non_regular_target(display));
            }
            Err(EngineError::new(
                ErrorClass::File,
                format!("patch: {} already exists.", display.display()),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(file_error(&file.path, &error)),
    }
}

/// Atomically installs the staged file without replacing an existing
/// destination: a hard link of the synced temp is atomic and fails with
/// `AlreadyExists` when a concurrent writer claimed the name first.
fn install_new(temp: &LocatedPath, target: &LocatedPath) -> std::io::Result<()> {
    temp.parent.hard_link(
        std::path::Path::new(&temp.name),
        &target.parent,
        std::path::Path::new(&target.name),
    )?;
    let _ = temp.remove();
    Ok(())
}

/// Removes directories the commit created for pending targets, deepest
/// first. `remove_dir` only deletes empty directories, so anything another
/// writer reused or populated is left alone, and directories that already
/// existed are never touched.
fn cleanup_dirs(root: &cap_std::fs::Dir, created: &[PathBuf]) {
    for dir in created.iter().rev() {
        let _ = root.remove_dir(dir);
    }
}

pub(crate) async fn apply_files(session: &PatchSession, plan: Plan) -> Result<Output, EngineError> {
    let guards = prepare_write_set(&plan).await?;
    let session = session.clone();
    // Every pinned-handle filesystem operation is synchronous and can read
    // up to the staged 64 MiB bound; keep all of it off the async executor.
    tokio::task::spawn_blocking(move || apply_files_inner(&session, &plan, guards))
        .await
        .map_err(|error| {
            EngineError::new(
                ErrorClass::Io,
                format!("patch: the commit worker failed to run: {error}. Nothing was written."),
            )
        })?
}

#[expect(
    clippy::too_many_lines,
    reason = "the commit walks one ordered sequence of guarded phases"
)]
fn apply_files_inner(
    session: &PatchSession,
    plan: &Plan,
    _guards: Vec<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<Output, EngineError> {
    for file in &plan.files {
        revalidate_path(&session.workspace, &file.path, &file.absolute_path)?;
        if let Some(dest) = file.renamed_to.as_ref() {
            revalidate_path(&session.workspace, &dest.path, &dest.absolute_path)?;
        }
    }
    let workspace = std::fs::canonicalize(&session.workspace)
        .map_err(|error| file_error(&session.workspace, &error))?;
    let root = cap_std::fs::Dir::open_ambient_dir(&workspace, cap_std::ambient_authority())
        .map_err(|error| file_error(&workspace, &error))?;
    // The pinned root must be the directory the plan was staged on. A
    // swapped or over-mounted workspace root cannot satisfy the recorded
    // identity no matter what the pathnames currently resolve to.
    check_identity(
        Some(plan.workspace_id.ok_or_else(|| {
            EngineError::new(
                ErrorClass::Resolve,
                "patch: this plan carries no workspace identity. Re-stage and try again."
                    .to_owned(),
            )
        })?),
        cap_dir_id(&root),
        &session.workspace,
    )?;

    // Pin each source before staging writes, prove the parent handle is the
    // approved directory and the file is the approved object, and check the
    // before-image through that handle. An external directory swap cannot
    // redirect reads, and a same-content replacement still fails identity.
    let mut sources = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        if file.op == super::super::ir::Operation::Create {
            sources.push(None);
            continue;
        }
        let source = LocatedPath::bind(&root, &workspace, &file.absolute_path, false)
            .map_err(|error| source_error(file, &error))?;
        check_identity(file.dir_id, cap_dir_id(&source.parent), &file.path)?;
        let metadata = source
            .parent
            .metadata(std::path::Path::new(&source.name))
            .map_err(|error| source_error(file, &error))?;
        if !metadata.is_file() {
            return Err(non_regular_target(&file.path));
        }
        check_identity(file.file_id, cap_file_id(&metadata), &file.path)?;
        if let Some(before) = file.before.as_deref() {
            let current = source
                .parent
                .read(std::path::Path::new(&source.name))
                .map_err(|error| source_error(file, &error))?;
            if current != before {
                return Err(stale_changed(&file.path));
            }
        }
        sources.push(Some(source));
    }

    // Pass 1 is pure validation: locate every create/rename destination and
    // prove it absent. A missing parent validates as absent without being
    // created, so a failing target leaves nothing written behind.
    let mut targets: Vec<Option<Target>> = Vec::with_capacity(plan.files.len());
    for (file, source) in plan.files.iter().zip(&sources) {
        targets.push(match file.op {
            super::super::ir::Operation::Delete => None,
            super::super::ir::Operation::Create | super::super::ir::Operation::Rename => {
                let absolute = write_target(file);
                let (parent, name) = LocatedPath::locate(&workspace, absolute)
                    .map_err(|error| file_error(&file.path, &error))?;
                let recorded = if file.op == super::super::ir::Operation::Create {
                    file.dir_id
                } else {
                    file.renamed_to.as_ref().and_then(|dest| dest.dir_id)
                };
                match open_parent(&root, &parent) {
                    Ok(dir) => {
                        check_dest_absent(file, &dir, &name)?;
                        check_identity(recorded, cap_dir_id(&dir), dest_display(file))?;
                        Some(Target::Bound(LocatedPath {
                            parent: Arc::new(dir),
                            name,
                            absolute: absolute.to_path_buf(),
                        }))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        // The parent existed at stage time but is gone now:
                        // recreating it would write through a different
                        // directory than the one that was approved.
                        if recorded.is_some() {
                            return Err(stale_moved(dest_display(file)));
                        }
                        Some(Target::Pending {
                            parent,
                            name,
                            absolute: absolute.to_path_buf(),
                        })
                    }
                    Err(error) => return Err(file_error(&file.path, &error)),
                }
            }
            super::super::ir::Operation::Update => match source {
                Some(source) => Some(Target::Bound(source.clone())),
                None => {
                    return Err(EngineError::new(
                        ErrorClass::Resolve,
                        format!("patch: {} has no pinned source.", file.path.display()),
                    ));
                }
            },
        });
    }

    // Pass 2 is the first mutation: create exactly the parents that were
    // absent during validation, tracking each so a later failure can remove
    // them again.
    let mut created = Vec::new();
    for target in &mut targets {
        let Some(Target::Pending {
            parent,
            name,
            absolute,
        }) = target
        else {
            continue;
        };
        if let Err(error) = create_parent_dirs(&root, parent, &mut created) {
            cleanup_dirs(&root, &created);
            return Err(file_error(absolute, &error));
        }
        match open_parent(&root, parent) {
            Ok(dir) => {
                *target = Some(Target::Bound(LocatedPath {
                    parent: Arc::new(dir),
                    name: name.clone(),
                    absolute: absolute.clone(),
                }));
            }
            Err(error) => {
                cleanup_dirs(&root, &created);
                return Err(file_error(absolute, &error));
            }
        }
    }

    let mut temps = Vec::new();
    for (file, target) in plan.files.iter().zip(&targets) {
        if let Some(Target::Bound(target)) = target
            && let Err(error) = stage_temp(file, target, &mut temps)
        {
            cleanup_temps(&temps);
            cleanup_dirs(&root, &created);
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
            trash_source(&file.path, source, &temps, &mut undo, &root, &created)?;
        }
    }
    for staged in &temps {
        let installed = match staged.file.op {
            super::super::ir::Operation::Create | super::super::ir::Operation::Rename => {
                install_new(&staged.temp, &staged.target)
            }
            _ => staged.temp.rename_to(&staged.target),
        };
        if let Err(error) = installed {
            let (class, summary) = if error.kind() == std::io::ErrorKind::AlreadyExists {
                (
                    ErrorClass::File,
                    format!(
                        "patch: {} already exists.",
                        dest_display(staged.file).display()
                    ),
                )
            } else {
                (
                    ErrorClass::Io,
                    format!(
                        "patch: cannot write {}: {error}.",
                        staged.target.absolute.display()
                    ),
                )
            };
            return Err(rollback_error(
                class, summary, &temps, &undo, &root, &created,
            ));
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
            trash_source(&file.path, source, &temps, &mut undo, &root, &created)?;
        }
    }

    #[cfg(unix)]
    for target in targets
        .iter()
        .filter_map(|target| match target {
            Some(Target::Bound(target)) => Some(&target.parent),
            _ => None,
        })
        .chain(sources.iter().flatten().map(|source| &source.parent))
    {
        let _ = target
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

/// Creates a missing target parent component by component, recording
/// exactly the directories this call created so they can be removed on
/// failure. Components another writer made first are reused, not claimed.
fn create_parent_dirs(
    root: &cap_std::fs::Dir,
    parent: &std::path::Path,
    created: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let mut missing = Vec::new();
    let mut probe = parent;
    while !probe.as_os_str().is_empty() {
        match root.symlink_metadata(probe) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(probe.to_path_buf());
            }
            _ => break,
        }
        match probe.parent() {
            Some(next) => probe = next,
            None => break,
        }
    }
    for dir in missing.iter().rev() {
        match root.create_dir(dir) {
            Ok(()) => created.push(dir.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
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
fn stage_temp<'a>(
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
    let write = (|| {
        use std::io::Write as _;
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut handle = temp
            .parent
            .open_with(std::path::Path::new(&temp.name), &options)?
            .into_std();
        handle.write_all(after)?;
        handle.sync_all()
    })();
    if let Err(error) = write {
        let _ = temp.remove();
        return Err(file_error(&file.path, &error));
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
    class: ErrorClass,
    summary: String,
    temps: &[StagedTemp<'_>],
    undo: &[Undo<'_>],
    root: &cap_std::fs::Dir,
    created: &[PathBuf],
) -> EngineError {
    use std::fmt::Write as _;

    let mut failures = Vec::new();
    for action in undo.iter().rev() {
        if let Err(error) = action.restore() {
            failures.push((action, error));
        }
    }
    cleanup_temps(temps);
    cleanup_dirs(root, created);
    let mut message = summary;
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
    EngineError::new(class, message)
}

fn trash_source(
    display: &std::path::Path,
    source: &LocatedPath,
    temps: &[StagedTemp<'_>],
    undo: &mut Vec<Undo<'_>>,
    root: &cap_std::fs::Dir,
    created: &[PathBuf],
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
        let (class, summary) = if error.kind() == std::io::ErrorKind::NotFound {
            (
                ErrorClass::Stale,
                format!(
                    "patch: {} changed while you were approving.",
                    display.display()
                ),
            )
        } else {
            (
                ErrorClass::Io,
                format!("patch: cannot write {}: {error}.", display.display()),
            )
        };
        return Err(rollback_error(class, summary, temps, undo, root, created));
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
        // A directory blocks the rename on Unix. Windows refuses the
        // destination because its parent is a regular file.
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
            workspace: workspace.clone(),
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
                    dir_id: super::super::super::resolve::file_id(&workspace),
                    file_id: None,
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
                        absolute_path: destination.clone(),
                        dir_id: destination
                            .parent()
                            .and_then(super::super::super::resolve::file_id),
                    }),
                    hunks: Vec::new(),
                    dir_id: super::super::super::resolve::file_id(&workspace),
                    file_id: super::super::super::resolve::file_id(&source),
                },
            ],
            findings: Vec::new(),
            workspace_id: super::super::workspace_file_id(&workspace),
        };

        let error = apply_files(&session, plan)
            .await
            .expect_err("later rename destination must fail");

        // A failed destination must not leave a created file or move the source.
        assert!(
            matches!(
                error.class,
                ErrorClass::Io | ErrorClass::Stale | ErrorClass::File
            ),
            "{error:?}"
        );
        assert!(
            tokio::fs::metadata(created).await.is_err(),
            "invalid destination must prevent the earlier create"
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
        let root = cap_std::fs::Dir::open_ambient_dir(&workspace, cap_std::ambient_authority())
            .expect("workspace handle");
        let location = |path: &std::path::Path| {
            super::LocatedPath::bind(&root, &workspace, path, false).expect("pinned path")
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
        let error = super::trash_source(
            &PathBuf::from("failed-source"),
            &missing,
            &[],
            &mut undo,
            &root,
            &[],
        )
        .expect_err("missing source cannot move");
        assert_eq!(error.class, ErrorClass::Stale);
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

        let root =
            cap_std::fs::Dir::open_ambient_dir(workspace.path(), cap_std::ambient_authority())
                .expect("workspace handle");
        let installed = super::LocatedPath::bind(
            &root,
            workspace.path(),
            &parent.join("installed.txt"),
            false,
        )
        .expect("pinned install target");
        let source =
            super::LocatedPath::bind(&root, workspace.path(), &parent.join("source.txt"), false)
                .expect("pinned source");
        std::fs::rename(&parent, workspace.path().join("moved")).expect("swap directory");
        std::os::unix::fs::symlink(outside.path(), &parent)
            .expect("point workspace parent outside");

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

        temp.rename_to(&installed)
            .expect("install under pinned parent");
        assert_eq!(
            std::fs::read(workspace.path().join("moved/installed.txt")).expect("installed"),
            b"after\n"
        );
        assert!(!outside.path().join("installed.txt").exists());
        assert!(!outside.path().join(".dalgon-patch-test.tmp").exists());

        let mut undo = Vec::new();
        super::trash_source(
            &PathBuf::from("parent/source.txt"),
            &source,
            &[],
            &mut undo,
            &root,
            &[],
        )
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

    use super::super::super::resolve::file_id;

    fn test_session(workspace: &std::path::Path) -> PatchSession {
        PatchSession {
            workspace: workspace.to_path_buf(),
            session: SessionId::new_v7(),
            generation: GenerationId::new(NonZeroU64::MIN),
            turn: TurnId::new(NonZeroU64::MIN),
            call: CallId::new("commit-test"),
            consumer: dal_core::Consumer::Model,
            symbols: false,
            seen: crate::Seen::new(),
            index: Index::new(None),
            snapshots: Arc::new(SnapshotStore::new([7; 16])),
            cutoff: Some(u64::MAX),
        }
    }

    /// A modify plan over `relative`, staged the way `stage_file` records it.
    fn modify_plan(
        workspace: &std::path::Path,
        relative: &str,
        before: &[u8],
        after: &[u8],
    ) -> super::super::super::ir::Plan {
        let absolute = workspace.join(relative);
        super::super::super::ir::Plan {
            style: DialectId::Replace,
            files: vec![super::super::super::ir::StagedFileOwned {
                path: PathBuf::from(relative),
                absolute_path: absolute.clone(),
                before: Some(before.to_vec().into_boxed_slice()),
                after: Some(after.to_vec().into_boxed_slice()),
                op: Operation::Update,
                renamed_to: None,
                hunks: Vec::new(),
                dir_id: absolute.parent().and_then(file_id),
                file_id: file_id(&absolute),
            }],
            findings: Vec::new(),
            workspace_id: super::super::workspace_file_id(workspace),
        }
    }

    #[tokio::test]
    async fn missing_source_is_stale_not_io() {
        let directory = tempfile::tempdir().expect("workspace");
        let workspace = std::fs::canonicalize(directory.path()).expect("canonical");
        tokio::fs::write(workspace.join("gone.txt"), b"before\n")
            .await
            .expect("source");
        let session = test_session(&workspace);
        let plan = modify_plan(&workspace, "gone.txt", b"before\n", b"after\n");
        tokio::fs::remove_file(workspace.join("gone.txt"))
            .await
            .expect("delete source");

        let error = super::apply_files(&session, plan)
            .await
            .expect_err("missing source must fail");
        assert!(
            matches!(error.class, ErrorClass::Stale),
            "deleted source must classify Stale for replanning, got {error:?}"
        );
    }

    #[tokio::test]
    async fn validation_failure_creates_no_directories() {
        let directory = tempfile::tempdir().expect("workspace");
        let workspace = std::fs::canonicalize(directory.path()).expect("canonical");
        tokio::fs::write(workspace.join("source.txt"), b"source\n")
            .await
            .expect("rename source");
        tokio::fs::create_dir(workspace.join("blocked"))
            .await
            .expect("blocked destination");
        let session = test_session(&workspace);
        let pending_target = workspace.join("pending/created.txt");
        let blocked_target = workspace.join("blocked");
        let plan = super::super::super::ir::Plan {
            style: DialectId::Replace,
            files: vec![
                super::super::super::ir::StagedFileOwned {
                    path: PathBuf::from("pending/created.txt"),
                    absolute_path: pending_target.clone(),
                    before: None,
                    after: Some(b"created\n".to_vec().into_boxed_slice()),
                    op: Operation::Create,
                    renamed_to: None,
                    hunks: Vec::new(),
                    dir_id: None,
                    file_id: None,
                },
                super::super::super::ir::StagedFileOwned {
                    path: PathBuf::from("source.txt"),
                    absolute_path: workspace.join("source.txt"),
                    before: Some(b"source\n".to_vec().into_boxed_slice()),
                    after: Some(b"renamed\n".to_vec().into_boxed_slice()),
                    op: Operation::Rename,
                    renamed_to: Some(RenameTarget {
                        path: PathBuf::from("blocked"),
                        absolute_path: blocked_target.clone(),
                        dir_id: blocked_target.parent().and_then(file_id),
                    }),
                    hunks: Vec::new(),
                    dir_id: file_id(&workspace),
                    file_id: file_id(&workspace.join("source.txt")),
                },
            ],
            findings: Vec::new(),
            workspace_id: super::super::workspace_file_id(&workspace),
        };

        let error = super::apply_files(&session, plan)
            .await
            .expect_err("blocked destination must fail validation");
        assert!(matches!(error.class, ErrorClass::File | ErrorClass::Stale));
        assert!(
            !workspace.join("pending").exists(),
            "validation must not create directories before every target passes"
        );
        assert!(!pending_target.exists());
        assert_eq!(
            tokio::fs::read(workspace.join("source.txt"))
                .await
                .expect("source intact"),
            b"source\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn swapped_workspace_root_is_stale() {
        let directory = tempfile::tempdir().expect("workspace");
        let workspace = std::fs::canonicalize(directory.path()).expect("canonical");
        tokio::fs::write(workspace.join("kept.txt"), b"before\n")
            .await
            .expect("source");
        let session = test_session(&workspace);
        let plan = modify_plan(&workspace, "kept.txt", b"before\n", b"after\n");

        // Replace the workspace directory itself: the canonical pathname
        // still resolves, but it is no longer the directory the plan was
        // staged on.
        let moved = workspace.with_file_name(format!(
            "{}-moved",
            workspace
                .file_name()
                .expect("workspace name")
                .to_string_lossy()
        ));
        std::fs::rename(&workspace, &moved).expect("move workspace aside");
        std::fs::create_dir(&workspace).expect("replacement workspace");
        std::fs::write(workspace.join("kept.txt"), b"before\n").expect("identical bait");

        let error = super::apply_files(&session, plan)
            .await
            .expect_err("swapped workspace root must fail");
        assert!(
            matches!(error.class, ErrorClass::Stale),
            "workspace identity mismatch must be Stale, got {error:?}"
        );
        std::fs::remove_dir_all(&workspace).expect("remove bait workspace");
        std::fs::rename(&moved, &workspace).expect("restore workspace");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn swapped_source_parent_is_stale() {
        let directory = tempfile::tempdir().expect("workspace");
        let workspace = std::fs::canonicalize(directory.path()).expect("canonical");
        let parent = workspace.join("parent");
        std::fs::create_dir(&parent).expect("parent");
        std::fs::write(parent.join("edit.txt"), b"before\n").expect("source");
        let session = test_session(&workspace);
        let plan = modify_plan(&workspace, "parent/edit.txt", b"before\n", b"after\n");

        // Same pathname, different directory object: canonicalization still
        // yields `parent/edit.txt`, so only the recorded identity notices.
        // Identical bytes at the bait path close the before-image check too.
        std::fs::rename(&parent, workspace.join("moved")).expect("swap parent");
        std::fs::create_dir(&parent).expect("bait parent");
        std::fs::write(parent.join("edit.txt"), b"before\n").expect("bait");

        let error = super::apply_files(&session, plan)
            .await
            .expect_err("swapped parent must fail");
        assert!(
            matches!(error.class, ErrorClass::Stale),
            "parent identity mismatch must be Stale, got {error:?}"
        );
        assert_eq!(
            std::fs::read(workspace.join("parent/edit.txt")).expect("bait unread"),
            b"before\n"
        );
        assert_eq!(
            std::fs::read(workspace.join("moved/edit.txt")).expect("real unread"),
            b"before\n"
        );
    }

    #[tokio::test]
    async fn install_new_refuses_to_replace() {
        let directory = tempfile::tempdir().expect("workspace");
        let workspace = std::fs::canonicalize(directory.path()).expect("canonical");
        std::fs::write(workspace.join("claimed.txt"), b"winner\n").expect("claimed");
        let root = cap_std::fs::Dir::open_ambient_dir(&workspace, cap_std::ambient_authority())
            .expect("root");
        let target =
            super::LocatedPath::bind(&root, &workspace, &workspace.join("claimed.txt"), false)
                .expect("target");
        let temp = target.sibling(".dalgon-patch-race.tmp".into());
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        {
            use std::io::Write as _;
            let mut file = temp
                .parent
                .open_with(std::path::Path::new(&temp.name), &options)
                .expect("temp")
                .into_std();
            file.write_all(b"challenger\n").expect("bytes");
            file.sync_all().expect("sync");
        }

        let error = super::install_new(&temp, &target)
            .expect_err("an occupied destination must not be replaced");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(workspace.join("claimed.txt")).expect("claimed read"),
            b"winner\n"
        );
        let _ = temp.remove();
    }
}
