//! Workspace-confined file access for provider-selected read and search paths.
//!
//! A canonical target must be beneath the canonical workspace. On Unix,
//! opening each canonical component relative to an already-open directory
//! with NOFOLLOW prevents a later symlink swap from escaping the workspace.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::path::Component;

/// An untrusted path is either outside the workspace or failed to resolve/open.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("outside the workspace")]
    Outside,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Resolves an existing target and proves canonical workspace containment.
pub(crate) fn resolve(workspace: &Path, requested: &Path) -> Result<PathBuf, Error> {
    let root = fs::canonicalize(workspace)?;
    if requested.is_absolute()
        && !requested.starts_with(&root)
        && !requested.starts_with(workspace)
    {
        return Err(Error::Outside);
    }
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let canonical = fs::canonicalize(candidate)?;
    if !canonical.starts_with(&root) {
        return Err(Error::Outside);
    }
    Ok(canonical)
}

/// Opens the confined target. Unix traverses by file descriptor with NOFOLLOW.
pub(crate) fn open(workspace: &Path, requested: &Path) -> Result<File, Error> {
    let canonical = resolve(workspace, requested)?;
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, openat};

        let root = fs::canonicalize(workspace)?;
        let relative = canonical.strip_prefix(&root).map_err(|_| Error::Outside)?;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut current = File::from(
            openat(rustix::fs::CWD, &root, flags, Mode::empty())
                .map_err(io::Error::from)?,
        );
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(Error::Outside);
            };
            let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            if components.peek().is_some() {
                flags |= OFlags::DIRECTORY;
            }
            current = File::from(
                openat(&current, name, flags, Mode::empty())
                    .map_err(io::Error::from)?,
            );
        }
        Ok(current)
    }
    #[cfg(not(unix))]
    {
        // Canonicalization rejects external targets on non-Unix platforms.
        Ok(File::open(canonical)?)
    }
}

/// Reads at most `max + 1` bytes from a confined file so callers detect oversize files.
pub(crate) fn read_limited(
    workspace: &Path,
    requested: &Path,
    max: u64,
) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    open(workspace, requested)?
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_only_workspace_descendants() {
        let base = tempfile::tempdir().unwrap();
        let workspace = base.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let inside = workspace.join("inside.txt");
        let outside = base.path().join("outside.txt");
        fs::write(&inside, b"inside").unwrap();
        fs::write(&outside, b"private").unwrap();

        let canonical_inside = fs::canonicalize(&inside).unwrap();
        assert_eq!(resolve(&workspace, Path::new("inside.txt")).unwrap(), canonical_inside);
        assert_eq!(resolve(&workspace, &inside).unwrap(), canonical_inside);
        assert!(matches!(resolve(&workspace, &outside), Err(Error::Outside)));
        assert!(matches!(
            resolve(&workspace, Path::new("../outside.txt")),
            Err(Error::Outside)
        ));
        assert!(matches!(open(&workspace, &outside), Err(Error::Outside)));
        assert!(matches!(
            read_limited(&workspace, Path::new("../outside.txt"), 16),
            Err(Error::Outside)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_outside_symlinks_and_keeps_internal_links() {
        let base = tempfile::tempdir().unwrap();
        let workspace = base.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let inside = workspace.join("inside.txt");
        let outside = base.path().join("outside.txt");
        fs::write(&inside, b"inside").unwrap();
        fs::write(&outside, b"private").unwrap();
        std::os::unix::fs::symlink(&inside, workspace.join("safe")).unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("escape")).unwrap();

        assert_eq!(read_limited(&workspace, Path::new("safe"), 16).unwrap(), b"inside");
        assert!(matches!(
            read_limited(&workspace, Path::new("escape"), 16),
            Err(Error::Outside)
        ));
    }
}
