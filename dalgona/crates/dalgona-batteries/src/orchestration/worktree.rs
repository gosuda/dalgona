// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Git worktree isolation policy: preflight refusals, outcome records, and
//! the exact git argv run through the granted run service. No process is
//! spawned here; execution wiring follows once the run service seam lands.

use std::path::PathBuf;

/// How one task's delta resolved against the run base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IsolationOutcome {
    /// The task left no delta.
    Clean,
    /// The patch applied onto the workspace checkout.
    Merged,
    /// The patch did not apply; the tree is retained for hand merging.
    Retained {
        worktree: PathBuf,
        patch: PathBuf,
        reason: String,
    },
    /// The task did not end done; nothing merged into the parent.
    Kept {
        patch: Option<PathBuf>,
        reason: Option<String>,
    },
}

/// The base commit every task worktree of one run starts from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Base {
    /// Repository top-level directory.
    pub top: PathBuf,
    /// Forty hex characters of the base commit.
    pub commit: String,
    /// Workspace path relative to `top`.
    pub relative_workspace: PathBuf,
}

#[cfg(test)]
mod tests;
