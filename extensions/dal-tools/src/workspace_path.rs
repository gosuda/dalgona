//! Canonical workspace confinement for automatically approved file tools.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// An existing path proven to resolve within the selected workspace.
pub(crate) struct Resolved {
    pub(crate) absolute: PathBuf,
    pub(crate) relative: PathBuf,
}

/// Why a model-supplied filesystem path cannot be resolved.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveError {
    #[error("path must stay within the workspace")]
    OutsideWorkspace,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Resolves an existing workspace-relative path without following a link outside.
///
/// The returned absolute path is canonical and must be used for subsequent I/O:
/// reopening the original spelling would permit a symlink to change targets.
///
/// # Errors
/// Rejects absolute paths, parent components, paths resolving outside the
/// workspace, and filesystem resolution failures.
pub(crate) fn resolve(workspace: &Path, path: &Path) -> Result<Resolved, ResolveError> {
    if path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return Err(ResolveError::OutsideWorkspace);
    }
    let root = fs::canonicalize(workspace)?;
    let absolute = fs::canonicalize(root.join(path))?;
    let relative = absolute
        .strip_prefix(&root)
        .map_err(|_| ResolveError::OutsideWorkspace)?
        .to_path_buf();
    Ok(Resolved { absolute, relative })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_descendants_and_rejects_absolute_or_parent_paths() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("file.txt"), "inside").unwrap();

        let found = resolve(root.path(), Path::new("nested/./file.txt")).unwrap();
        assert_eq!(found.absolute, fs::canonicalize(nested.join("file.txt")).unwrap());
        assert_eq!(found.relative, Path::new("nested/file.txt"));

        for path in [
            nested.join("file.txt"),
            PathBuf::from("../file.txt"),
            PathBuf::from("nested/../nested/file.txt"),
        ] {
            assert!(matches!(
                resolve(root.path(), &path),
                Err(ResolveError::OutsideWorkspace)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_escaping_symlinks_and_accepts_internal_symlinks() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("inside"), "inside").unwrap();
        fs::write(parent.path().join("secret"), "secret").unwrap();
        symlink(parent.path().join("secret"), root.join("escape")).unwrap();
        symlink(root.join("inside"), root.join("alias")).unwrap();

        assert!(matches!(
            resolve(&root, Path::new("escape")),
            Err(ResolveError::OutsideWorkspace)
        ));
        let found = resolve(&root, Path::new("alias")).unwrap();
        assert_eq!(found.relative, Path::new("inside"));
    }
}
