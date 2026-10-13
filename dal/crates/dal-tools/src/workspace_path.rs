//! Validates model-supplied filesystem paths before a tool can read them.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// A path resolved to a descendant of the active workspace.
pub(crate) struct ScopedPath {
    pub(crate) absolute: PathBuf,
    pub(crate) relative: PathBuf,
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
    Ok(ScopedPath { absolute, relative })
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
}
