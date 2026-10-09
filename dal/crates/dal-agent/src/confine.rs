//! The one containment rule for workspace-relative paths that a script, a
//! model, or an extension supplies.
//!
//! A path is refused before the filesystem is touched when it is empty,
//! absolute, carries a platform prefix, or climbs with `..`. The rest is
//! resolved through the nearest existing ancestor, so a link in an existing
//! prefix cannot hide an escape behind directories that do not exist yet. A
//! caller that creates directories between the check and the use calls
//! [`Confined::recheck`] before it trusts the result.

use std::path::{Component, Path, PathBuf};

/// A path proved to lie inside one workspace root.
#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Confined {
    relative: PathBuf,
    resolved: PathBuf,
}

impl Confined {
    /// The path below the root, made of normal components only.
    #[must_use]
    pub fn relative(&self) -> &Path {
        &self.relative
    }

    /// The canonical location below the canonical root. Components that do
    /// not exist yet are appended as written.
    #[must_use]
    pub fn resolved(&self) -> &Path {
        &self.resolved
    }

    /// Splits into the relative path and the resolved location.
    #[must_use]
    pub fn into_parts(self) -> (PathBuf, PathBuf) {
        (self.relative, self.resolved)
    }

    /// Resolves the same relative path again and proves it still names the
    /// same location inside the root.
    ///
    /// # Errors
    ///
    /// Returns the refusal of [`confine`] when the path now leaves the root,
    /// and [`ConfineError::Changed`] when it now resolves elsewhere inside
    /// the root, such as through a link created since the first resolution.
    pub fn recheck(&self, root: &Path) -> Result<(), ConfineError> {
        if confine(root, &self.relative)?.resolved == self.resolved {
            Ok(())
        } else {
            Err(ConfineError::Changed)
        }
    }
}

/// Why a path is not contained in a workspace root.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfineError {
    /// The path is empty.
    #[error("the path is empty")]
    Empty,
    /// The path is absolute or carries a platform prefix.
    #[error("the path is absolute")]
    Absolute,
    /// The path climbs with a `..` component.
    #[error("the path climbs out of its directory with `..`")]
    ParentDir,
    /// The path resolves outside the root.
    #[error("the path resolves outside the workspace")]
    Outside,
    /// The root or a component of the path exists but cannot be resolved,
    /// such as a dangling link.
    #[error("the path cannot be resolved")]
    Unresolvable,
    /// The path resolves to a different location than it did before.
    #[error("the path now resolves to a different location")]
    Changed,
}

/// Proves that `raw` names a location inside `root`.
///
/// # Errors
///
/// Returns [`ConfineError`] when `raw` is empty, absolute, carries a
/// platform prefix, climbs with `..`, resolves outside the root, or runs
/// through a component that exists but cannot be resolved.
pub fn confine(root: &Path, raw: &Path) -> Result<Confined, ConfineError> {
    if raw.as_os_str().is_empty() {
        return Err(ConfineError::Empty);
    }
    let mut relative = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => return Err(ConfineError::Absolute),
            Component::ParentDir => return Err(ConfineError::ParentDir),
            Component::CurDir => {}
            Component::Normal(part) => relative.push(part),
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(ConfineError::Empty);
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|_| ConfineError::Unresolvable)?;
    let resolved = canonicalize_existing_prefix(&canonical_root.join(&relative))
        .ok_or(ConfineError::Unresolvable)?;
    if !resolved.starts_with(&canonical_root) {
        return Err(ConfineError::Outside);
    }
    Ok(Confined { relative, resolved })
}

/// Canonicalizes the deepest existing ancestor and re-appends the missing
/// tail. Returns `None` when a component exists but cannot be resolved.
#[must_use]
pub fn canonicalize_existing_prefix(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut resolved) => {
                resolved.extend(tail.iter().rev());
                return Some(resolved);
            }
            Err(_) if std::fs::symlink_metadata(current).is_ok() => return None,
            Err(_) => {
                tail.push(current.file_name()?);
                current = current.parent()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root)?;
        std::fs::create_dir(&outside)?;
        Ok((temp, root, outside))
    }

    #[test]
    fn lexical_refusals_precede_any_filesystem_access() {
        let missing = Path::new("/nonexistent-dal-confine-root");
        for (raw, expected) in [
            ("", ConfineError::Empty),
            ("/etc/passwd", ConfineError::Absolute),
            ("../x", ConfineError::ParentDir),
            ("a/../b", ConfineError::ParentDir),
            ("new/../../outside/file", ConfineError::ParentDir),
        ] {
            assert_eq!(confine(missing, Path::new(raw)), Err(expected), "{raw}");
        }
    }

    #[test]
    fn a_missing_tail_resolves_below_the_canonical_root() -> TestResult {
        let (_temp, root, _outside) = fixture()?;
        let confined = confine(&root, Path::new("./new/deep/file.txt"))?;
        assert_eq!(confined.relative(), Path::new("new/deep/file.txt"));
        assert_eq!(
            confined.resolved(),
            std::fs::canonicalize(&root)?.join("new/deep/file.txt")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_the_root_are_refused_through_missing_tails() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        std::os::unix::fs::symlink(&outside, root.join("link"))?;
        assert_eq!(
            confine(&root, Path::new("link/new/file")),
            Err(ConfineError::Outside)
        );
        assert_eq!(
            confine(&root, Path::new("link")),
            Err(ConfineError::Outside)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_is_unresolvable_not_trusted() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        std::os::unix::fs::symlink(outside.join("absent"), root.join("dangling"))?;
        assert_eq!(
            confine(&root, Path::new("dangling")),
            Err(ConfineError::Unresolvable)
        );
        assert_eq!(
            confine(&root, Path::new("dangling/file")),
            Err(ConfineError::Unresolvable)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_root_is_followed_and_rechecked() -> TestResult {
        let (_temp, root, _outside) = fixture()?;
        std::fs::create_dir(root.join("real"))?;
        std::os::unix::fs::symlink(root.join("real"), root.join("alias"))?;
        let confined = confine(&root, Path::new("alias/file"))?;
        assert_eq!(
            confined.resolved(),
            std::fs::canonicalize(&root)?.join("real/file")
        );
        confined.recheck(&root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recheck_catches_a_link_swapped_in_after_resolution() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        let confined = confine(&root, Path::new("staged/file"))?;
        std::os::unix::fs::symlink(&outside, root.join("staged"))?;
        assert_eq!(confined.recheck(&root), Err(ConfineError::Outside));
        std::fs::remove_file(root.join("staged"))?;
        std::fs::create_dir(root.join("elsewhere"))?;
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("staged"))?;
        assert_eq!(confined.recheck(&root), Err(ConfineError::Changed));
        Ok(())
    }
}
