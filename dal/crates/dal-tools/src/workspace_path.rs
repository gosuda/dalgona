//! Validates model-supplied filesystem paths before a tool can read them.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// A path resolved to a descendant of the active workspace.
pub(crate) struct ScopedPath {
    pub(crate) absolute: PathBuf,
    pub(crate) relative: PathBuf,
    root: PathBuf,
}

/// Failure to resolve a model-controlled path under the workspace.
#[derive(Debug)]
pub(crate) enum ScopeError {
    OutsideWorkspace,
    Io(io::Error),
}

/// Resolve an existing workspace-relative path and reject lexical or symlink escapes.
///
/// This is the single admission boundary for filesystem scopes exposed to the model.
pub(crate) fn resolve(workspace: &Path, path: &Path) -> Result<ScopedPath, ScopeError> {
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(ScopeError::OutsideWorkspace);
    }
    let root = fs::canonicalize(workspace).map_err(ScopeError::Io)?;
    let absolute = fs::canonicalize(root.join(path)).map_err(ScopeError::Io)?;
    let relative = absolute
        .strip_prefix(&root)
        .map_err(|_| ScopeError::OutsideWorkspace)?
        .to_path_buf();
    Ok(ScopedPath {
        absolute,
        relative,
        root,
    })
}

/// Open a validated path relative to its workspace handle.
///
/// The Unix implementation does not follow symlinks when opening path components.
/// Internal symlinks are resolved during validation, so an intervening
/// symlink replacement fails closed.
pub(crate) fn open(scoped: &ScopedPath) -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, open as open_dir, openat};

        let mut current = open_dir(
            &scoped.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let mut parts = scoped.relative.components().peekable();
        while let Some(component) = parts.next() {
            let Component::Normal(name) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "invalid workspace-relative path",
                ));
            };
            let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
            if parts.peek().is_some() {
                flags |= OFlags::DIRECTORY;
            }
            current = openat(&current, name, flags, Mode::empty())?;
        }
        Ok(fs::File::from(current))
    }
    #[cfg(not(unix))]
    {
        fs::File::open(&scoped.absolute)
    }
}

/// Reads at most `max_bytes` from a workspace file.
pub(crate) fn read_limited(
    workspace: &Path,
    relative: &Path,
    max_bytes: u64,
) -> io::Result<Option<Vec<u8>>> {
    use io::Read as _;

    let scoped = resolve(workspace, relative).map_err(|error| match error {
        ScopeError::OutsideWorkspace => io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path is outside the workspace",
        ),
        ScopeError::Io(error) => error,
    })?;
    let mut file = open(&scoped)?;
    if file.metadata()?.len() > max_bytes {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(u64::try_from(bytes.len())
        .is_ok_and(|len| len <= max_bytes)
        .then_some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_path_allows_files_and_directories_but_denies_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("allowed"), b"public").unwrap();
        fs::write(dir.path().join("private"), b"secret").unwrap();

        assert_eq!(
            resolve(&workspace, Path::new(".")).unwrap().relative,
            Path::new("")
        );
        assert_eq!(
            resolve(&workspace, Path::new("allowed")).unwrap().relative,
            Path::new("allowed")
        );
        for path in ["../private", "child/../../private"] {
            assert!(matches!(
                resolve(&workspace, Path::new(path)),
                Err(ScopeError::OutsideWorkspace)
            ));
        }
        assert!(matches!(
            resolve(&workspace, &workspace.join("allowed")),
            Err(ScopeError::OutsideWorkspace)
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            symlink("allowed", workspace.join("inside")).unwrap();
            assert_eq!(
                resolve(&workspace, Path::new("inside")).unwrap().relative,
                Path::new("allowed")
            );
            symlink(dir.path().join("private"), workspace.join("outside")).unwrap();
            assert!(matches!(
                resolve(&workspace, Path::new("outside")),
                Err(ScopeError::OutsideWorkspace)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_swap_after_validation_cannot_redirect_the_open() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let inside = workspace.join("inner");
        fs::create_dir_all(&inside).unwrap();
        fs::write(inside.join("file"), b"allowed").unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("file"), b"secret").unwrap();

        let scoped = resolve(&workspace, Path::new("inner/file")).unwrap();
        fs::rename(&inside, workspace.join("renamed")).unwrap();
        symlink(&outside, &inside).unwrap();
        assert!(open(&scoped).is_err());
    }
}
