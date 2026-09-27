use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, Serializer};

/// An absolute workspace path, with its lexical spelling preserved.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "PathBuf")]
pub struct Workspace(PathBuf);

/// A workspace path violates the absolute-path requirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WorkspaceError {
    /// The path is relative to an unspecified working directory.
    #[error("workspace path must be absolute")]
    Relative,
}

impl Workspace {
    /// Retains an absolute path without accessing the filesystem.
    ///
    /// # Errors
    /// Returns `WorkspaceError::Relative` when the path is not absolute.
    pub fn new(path: PathBuf) -> Result<Self, WorkspaceError> {
        if !path.is_absolute() {
            return Err(WorkspaceError::Relative);
        }
        Ok(Self(path))
    }

    /// Borrows the original absolute path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Returns the owned path without normalization.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl TryFrom<PathBuf> for Workspace {
    type Error = WorkspaceError;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        Self::new(path)
    }
}

impl From<Workspace> for PathBuf {
    fn from(workspace: Workspace) -> Self {
        workspace.0
    }
}

impl Serialize for Workspace {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_and_decoding_reject_relative_paths() {
        assert_eq!(
            Workspace::new(PathBuf::from("relative/child")),
            Err(WorkspaceError::Relative)
        );
        assert!(sonic_rs::from_str::<Workspace>("\"relative/child\"").is_err());
    }

    #[test]
    fn absolute_lexical_path_is_not_resolved() -> Result<(), WorkspaceError> {
        #[cfg(unix)]
        let path = PathBuf::from("/nonexistent-dal-workspace/link/../child");
        #[cfg(windows)]
        let path = PathBuf::from(r"C:\nonexistent-dal-workspace\link\..\child");
        let workspace = Workspace::new(path.clone())?;
        assert_eq!(workspace.as_path(), path);
        Ok(())
    }
}
