//! Canonical workspace boundary for model-facing filesystem paths.
//!
//! Neither read nor search may grant its model caller ambient access to the
//! process filesystem. Every explicit scope must resolve below the workspace.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Why a model-supplied filesystem path cannot be resolved.
#[derive(Debug)]
pub(crate) enum ResolveError {
    /// The trusted workspace root was unavailable.
    Workspace(io::Error),
    /// The requested path did not resolve to an existing target.
    Target(io::Error),
    /// The path traverses parents or resolves outside the workspace.
    Outside,
}

/// A canonical target whose relative path proves workspace containment.
pub(crate) struct WorkspacePath {
    pub(crate) absolute: PathBuf,
    pub(crate) relative: PathBuf,
}

/// Resolve an existing path without exposing content outside the workspace.
///
/// Absolute paths inside the workspace remain valid. Parent traversal and
/// symlinks resolving outside the workspace are rejected before any file
/// contents or directory entries are read.
pub(crate) fn resolve(workspace: &Path, path: &Path) -> Result<WorkspacePath, ResolveError> {
    let absolute = path.is_absolute();
    if path.components().any(|component| {
        matches!(component, Component::ParentDir)
            || (!absolute && matches!(component, Component::Prefix(_)))
    }) {
        return Err(ResolveError::Outside);
    }

    let root = fs::canonicalize(workspace).map_err(ResolveError::Workspace)?;
    let requested = if absolute {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let target = fs::canonicalize(requested).map_err(ResolveError::Target)?;
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| ResolveError::Outside)?
        .to_path_buf();
    Ok(WorkspacePath {
        absolute: target,
        relative,
    })
}
