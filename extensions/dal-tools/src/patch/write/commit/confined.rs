//! Handle-relative Unix patch writes. No ancestor is followed after approval.
//!
//! The resolved paths identify targets for authorization, while opened
//! directories are the authority used for every mutation. A concurrent
//! symlink substitution cannot redirect a later mkdir, temp, rename or undo.

use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    os::{fd::OwnedFd, unix::ffi::OsStrExt as _},
    path::{Component, Path, PathBuf},
};

use rustix::{
    fs::{self, AtFlags, FileType, Mode, OFlags},
    io::Errno,
};

use super::{
    nonce_u128, report_output, trash_path, write_target, PatchSession,
};
use crate::patch::ir::{EngineError, ErrorClass, Operation, Output, Plan, StagedFileOwned};

const DIRECTORY_FLAGS: OFlags =
    OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
const FILE_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::RGRP)
    .union(Mode::WGRP)
    .union(Mode::ROTH)
    .union(Mode::WOTH);
const DIRECTORY_MODE: Mode = Mode::RWXU.union(Mode::RWXG).union(Mode::RWXO);

struct Root {
    canonical: PathBuf,
    handle: OwnedFd,
}

impl Root {
    fn open(workspace: &Path) -> io::Result<Self> {
        let canonical = std::fs::canonicalize(workspace)?;
        let handle = fs::open(&canonical, DIRECTORY_FLAGS, Mode::empty())?;
        Ok(Self { canonical, handle })
    }

    fn directory(&self, relative: &Path, create: bool) -> io::Result<OwnedFd> {
        let mut dir = fs::openat(&self.handle, ".", DIRECTORY_FLAGS, Mode::empty())?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(invalid_path());
            };
            let next = match fs::openat(&dir, name, DIRECTORY_FLAGS, Mode::empty()) {
                Ok(next) => next,
                Err(Errno::NOENT) if create => {
                    match fs::mkdirat(&dir, name, DIRECTORY_MODE) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => return Err(error.into()),
                    }
                    fs::openat(&dir, name, DIRECTORY_FLAGS, Mode::empty())?
                }
                Err(error) => return Err(error.into()),
            };
            dir = next;
        }
        Ok(dir)
    }

    fn parent(&self, path: &Path, create: bool) -> io::Result<(OwnedFd, OsString)> {
        // A directory-only spelling cannot be the name of a regular file.
        if path.as_os_str().as_bytes().last() == Some(&b'/') {
            return Err(invalid_path());
        }
        let relative = path.strip_prefix(&self.canonical).map_err(|_| invalid_path())?;
        let leaf = relative.file_name().ok_or_else(invalid_path)?;
        if !matches!(relative.components().last(), Some(Component::Normal(_))) {
            return Err(invalid_path());
        }
        let parent = relative.parent().ok_or_else(invalid_path)?;
        Ok((self.directory(parent, create)?, leaf.to_os_string()))
    }

    fn kind(&self, path: &Path) -> io::Result<Option<FileType>> {
        let (dir, name) = match self.parent(path, false) {
            Ok(parts) => parts,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match fs::statat(&dir, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(FileType::from_raw_mode(stat.st_mode))),
            Err(Errno::NOENT) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn create_temp(&self, dir: &OwnedFd, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        let handle = fs::openat(
            dir,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            FILE_MODE,
        )?;
        let mut file = File::from(handle);
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = fs::unlinkat(dir, name, AtFlags::empty());
            return Err(error);
        }
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (source_dir, source_name) = self.parent(from, false)?;
        let (target_dir, target_name) = self.parent(to, false)?;
        fs::renameat(&source_dir, &source_name, &target_dir, &target_name)?;
        Ok(())
    }

    fn install(&self, temp: &Temp<'_>) -> io::Result<()> {
        let (target_dir, target_name) = self.parent(write_target(temp.file), false)?;
        fs::renameat(&temp.dir, &temp.name, &target_dir, &target_name)?;
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        let (dir, name) = self.parent(path, false)?;
        fs::unlinkat(&dir, &name, AtFlags::empty())?;
        Ok(())
    }

    fn restore_bytes(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let (dir, name) = self.parent(path, false)?;
        let handle = fs::openat(
            &dir,
            &name,
            OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let mut file = File::from(handle);
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a regular file"));
        }
        file.set_len(0)?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    fn sync_directory(&self, absolute: &Path) {
        if let Ok(relative) = absolute.strip_prefix(&self.canonical)
            && let Ok(dir) = self.directory(relative, false)
        {
            let _ = fs::fsync(&dir);
        }
    }
}

fn invalid_path() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "path is not a workspace file")
}

struct Temp<'a> {
    file: &'a StagedFileOwned,
    dir: OwnedFd,
    name: OsString,
}

enum Undo<'a> {
    RestoreBytes(&'a Path, &'a [u8]),
    RemoveInstalled(&'a Path),
    RestoreMoved { source: &'a Path, backup: PathBuf },
}

impl Undo<'_> {
    fn restore(&self, root: &Root) -> io::Result<()> {
        match self {
            Self::RestoreBytes(path, bytes) => root.restore_bytes(path, bytes),
            Self::RemoveInstalled(path) => match root.remove(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
            Self::RestoreMoved { source, backup } => root.rename(backup, source),
        }
    }
}

fn cleanup_temps(temps: &[Temp<'_>]) {
    for temp in temps {
        let _ = fs::unlinkat(&temp.dir, &temp.name, AtFlags::empty());
    }
}

fn cannot_write(path: &Path, error: impl std::fmt::Display) -> EngineError {
    EngineError::new(
        ErrorClass::Io,
        format!("patch: cannot write {}: {error}. Nothing was written.", path.display()),
    )
}

fn rollback_error(
    root: &Root,
    path: &Path,
    error: io::Error,
    temps: &[Temp<'_>],
    undo: &[Undo<'_>],
) -> EngineError {
    use std::fmt::Write as _;

    let mut failures = Vec::new();
    for action in undo.iter().rev() {
        if let Err(error) = action.restore(root) {
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
    root: &Root,
    session: &PatchSession,
    file: &'a StagedFileOwned,
    temps: &[Temp<'_>],
    undo: &mut Vec<Undo<'a>>,
) -> Result<(), EngineError> {
    let backup = trash_path(session, &file.absolute_path);
    if let Err(error) = root.rename(&file.absolute_path, &backup) {
        return Err(rollback_error(root, &file.path, error, temps, undo));
    }
    undo.push(Undo::RestoreMoved {
        source: &file.absolute_path,
        backup,
    });
    Ok(())
}

pub(super) fn apply_files(session: &PatchSession, plan: &Plan) -> Result<Output, EngineError> {
    let root = Root::open(&session.workspace)
        .map_err(|error| cannot_write(&session.workspace, error))?;
    let mut seen_dirs = HashSet::new();
    let mut temps = Vec::new();

    // Phase A: keep open handles for all temps so cleanup never traverses
    // a changed ancestor path, even when a later staging step fails.
    for file in &plan.files {
        if file.op == Operation::Delete {
            if let Some(parent) = file.absolute_path.parent() {
                seen_dirs.insert(parent.to_path_buf());
            }
            continue;
        }
        let Some(after) = file.after.as_deref() else {
            continue;
        };
        let target = write_target(file);
        if file.op == Operation::Rename {
            match root.kind(target) {
                Ok(Some(kind)) => {
                    cleanup_temps(&temps);
                    let dest = file
                        .renamed_to
                        .as_ref()
                        .map_or(file.path.as_path(), |dest| dest.path.as_path());
                    if !kind.is_file() {
                        return Err(super::non_regular_target(dest));
                    }
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
                    cleanup_temps(&temps);
                    return Err(cannot_write(target, error));
                }
            }
        }
        let (dir, _) = match root.parent(target, true) {
            Ok(parent) => parent,
            Err(error) => {
                cleanup_temps(&temps);
                return Err(cannot_write(target, error));
            }
        };
        let name = OsString::from(format!(
            ".dalgon-patch-{}-{:032x}.tmp",
            std::process::id(),
            nonce_u128()
        ));
        if let Err(error) = root.create_temp(&dir, &name, after) {
            cleanup_temps(&temps);
            return Err(cannot_write(target, error));
        }
        temps.push(Temp { file, dir, name });
        if let Some(parent) = target.parent() {
            seen_dirs.insert(parent.to_path_buf());
        }
        if file.op == Operation::Rename
            && let Some(parent) = file.absolute_path.parent()
        {
            seen_dirs.insert(parent.to_path_buf());
        }
    }

    for file in &plan.files {
        session.index.dirty(&session.workspace, &file.path);
        if let Some(dest) = file.renamed_to.as_ref() {
            session.index.dirty(&session.workspace, &dest.path);
        }
    }

    let mut undo = Vec::new();
    for file in &plan.files {
        if file.op == Operation::Delete {
            trash_source(&root, session, file, &temps, &mut undo)?;
        }
    }
    for temp in &temps {
        let target = write_target(temp.file);
        if let Err(error) = root.install(temp) {
            return Err(rollback_error(&root, target, error, &temps, &undo));
        }
        undo.push(match (temp.file.op, temp.file.before.as_deref()) {
            (Operation::Rename, _) | (_, None) => Undo::RemoveInstalled(target),
            (_, Some(before)) => Undo::RestoreBytes(target, before),
        });
    }
    for file in &plan.files {
        if file.op == Operation::Rename {
            trash_source(&root, session, file, &temps, &mut undo)?;
        }
    }
    for dir in seen_dirs {
        root.sync_directory(&dir);
    }
    for action in undo {
        if let Undo::RestoreMoved { backup, .. } = action {
            let _ = root.remove(&backup);
        }
    }
    cleanup_temps(&temps);
    Ok(report_output(session, plan))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_directories_are_created_beneath_opened_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = Root::open(workspace.path()).expect("open workspace");
        let path = root.canonical.join("a/b/file.txt");
        let (dir, name) = root.parent(&path, true).expect("nested directories");
        root.create_temp(&dir, &name, b"inside\n")
            .expect("create file through parent handle");
        assert_eq!(std::fs::read(path).expect("inside file"), b"inside\n");
    }

    #[test]
    fn swapped_symlink_ancestor_cannot_redirect_missing_directory_creation() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let root = Root::open(workspace.path()).expect("open workspace");
        let sub = root.canonical.join("sub");
        std::fs::create_dir(&sub).expect("initial directory");
        let target = sub.join("a/b/new.txt");

        // Equivalent to an ancestor changing after plan and revalidation.
        std::fs::remove_dir(&sub).expect("remove initial directory");
        std::os::unix::fs::symlink(outside.path(), &sub).expect("swap to outside link");

        root.parent(&target, true)
            .expect_err("NOFOLLOW must reject the substituted ancestor");
        assert!(!outside.path().join("a").exists());
    }

    #[test]
    fn symlink_inserted_after_temp_staging_does_not_escape_during_install() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let root = Root::open(workspace.path()).expect("open workspace");
        let sub = root.canonical.join("sub");
        std::fs::create_dir(&sub).expect("initial parent");
        let target = sub.join("target.txt");
        let (dir, _) = root.parent(&target, false).expect("open parent");
        let temp = Temp {
            file: &StagedFileOwned {
                path: PathBuf::from("sub/target.txt"),
                absolute_path: target.clone(),
                before: None,
                after: Some(b"inside\n".to_vec().into_boxed_slice()),
                op: Operation::Create,
                renamed_to: None,
                hunks: Vec::new(),
            },
            dir,
            name: OsString::from("temp.txt"),
        };
        root.create_temp(&temp.dir, &temp.name, b"inside\n")
            .expect("stage temp");

        std::fs::rename(&sub, root.canonical.join("old-sub")).expect("move staged directory");
        std::os::unix::fs::symlink(outside.path(), &sub).expect("substitute outside link");

        root.install(&temp).expect_err("install must reject symlink parent");
        assert!(!outside.path().join("target.txt").exists());
        cleanup_temps(&[temp]);
    }
}
