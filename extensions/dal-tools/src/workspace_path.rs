//! Canonical workspace boundary for automatically approved filesystem reads.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// A resolved, existing path within the selected workspace.
pub(crate) struct ResolvedPath {
    pub(crate) absolute: PathBuf,
    pub(crate) relative: PathBuf,
}

/// Filesystem and confinement errors must stay distinct at tool boundaries.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkspacePathError {
    #[error("path is outside the workspace")]
    Outside,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Resolves an existing path before any content or directory access.
///
/// The workspace is the authority boundary. Parent components are never
/// accepted, and even an absolute path must be under the selected workspace.
/// Canonicalizing both ends rejects symlinks that lead outside that boundary.
pub(crate) fn resolve(
    workspace: &Path,
    requested: &Path,
) -> Result<ResolvedPath, WorkspacePathError> {
    if requested.components().any(|component| {
        matches!(component, Component::ParentDir)
            || (matches!(component, Component::Prefix(_)) && !requested.is_absolute())
    }) {
        return Err(WorkspacePathError::Outside);
    }

    let root = fs::canonicalize(workspace)?;
    if requested.is_absolute()
        && !requested.starts_with(workspace)
        && !requested.starts_with(&root)
    {
        return Err(WorkspacePathError::Outside);
    }

    let absolute = fs::canonicalize(workspace.join(requested))?;
    let relative = absolute
        .strip_prefix(&root)
        .map_err(|_| WorkspacePathError::Outside)?
        .to_path_buf();
    Ok(ResolvedPath { absolute, relative })
}
