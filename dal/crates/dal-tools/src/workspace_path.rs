//! Canonical workspace boundaries for model-facing filesystem paths.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// A path verified to resolve inside the current workspace.
pub(crate) struct WorkspacePath {
    pub(crate) abs: PathBuf,
    pub(crate) ws_rel: PathBuf,
}

/// Resolve an existing path, rejecting absolute paths outside the workspace,
/// parent traversal, and symlinks that escape its canonical root.
///
/// The caller must use the returned path, not the original provider input.
pub(crate) fn resolve(workspace: &Path, path: &Path) -> io::Result<Option<WorkspacePath>> {
    let root = fs::canonicalize(workspace)?;
    let relative = if path.is_absolute() {
        let Ok(relative) = path.strip_prefix(&root).or_else(|_| path.strip_prefix(workspace))
        else {
            return Ok(None);
        };
        relative
    } else {
        path
    };
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Ok(None);
    }

    let abs = fs::canonicalize(root.join(relative))?;
    let Ok(ws_rel) = abs.strip_prefix(&root) else {
        return Ok(None);
    };
    let ws_rel = ws_rel.to_path_buf();
    Ok(Some(WorkspacePath { abs, ws_rel }))
}
