//! Workspace-scoped session paths and journal record locations.

use std::path::{Path, PathBuf};

use dal_core::{EntryId, SessionId};

/// Identifies one entry's byte range in a workspace session journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Locator {
    /// The workspace directory key containing the session.
    pub workspace_key: Box<str>,
    /// The session identifier.
    pub session: SessionId,
    /// The journal entry identifier.
    pub entry: EntryId,
    /// The entry's starting byte offset in `journal.jsonl`.
    pub offset: u64,
    /// The entry's encoded byte length.
    pub len: u64,
}

#[derive(Debug)]
pub(crate) struct SessionPaths {
    directory: PathBuf,
}

impl SessionPaths {
    #[must_use]
    pub(crate) fn new(data_root: &Path, workspace_key: &str, session: SessionId) -> Self {
        let directory = data_root
            .join("sessions")
            .join(workspace_key)
            .join(session.to_string());
        Self { directory }
    }

    #[must_use]
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub(crate) fn journal(&self) -> PathBuf {
        self.directory.join("journal.jsonl")
    }

    #[must_use]
    pub(crate) fn lock(&self) -> PathBuf {
        self.directory.join("lock")
    }

    #[must_use]
    pub(crate) fn info(&self) -> PathBuf {
        self.directory.join("info.json")
    }

    #[must_use]
    pub(crate) fn jobs(&self) -> PathBuf {
        self.directory.join("jobs")
    }

    #[must_use]
    pub(crate) fn sidecar(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    #[must_use]
    pub(crate) fn sidecar_dir(&self) -> PathBuf {
        self.directory.join("sidecar")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_id() -> SessionId {
        SessionId::parse("0192aa00-0000-7000-8000-000000000001").expect("valid UUIDv7")
    }

    #[test]
    fn session_paths_are_workspace_scoped() {
        let session = session_id();
        let paths = SessionPaths::new(Path::new("/data"), "workspace-key", session);
        let expected = Path::new("/data")
            .join("sessions")
            .join("workspace-key")
            .join(session.to_string());

        assert_eq!(paths.directory(), expected);
        assert_eq!(paths.journal(), expected.join("journal.jsonl"));
        assert_eq!(paths.lock(), expected.join("lock"));
        assert_eq!(paths.info(), expected.join("info.json"));
        assert_eq!(paths.jobs(), expected.join("jobs"));
        assert_eq!(paths.sidecar("serve.token"), expected.join("serve.token"));
    }
}
