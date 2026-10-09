// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Git worktree isolation policy: preflight refusals, outcome records, and
//! the exact git argv run through the granted run service. No process is
//! spawned here; execution wiring follows once the run service seam lands.

use std::path::PathBuf;

#[cfg(test)]
mod tests;

/// Minimum git version that supports detached worktrees for this battery.
#[expect(
    dead_code,
    reason = "the worktree isolation contract fixes the minimum git version"
)]
pub(crate) const MIN_GIT_VERSION: (u32, u32) = (2, 17);

/// Suffix appended to every isolation refusal naming the shared escape.
pub(crate) const SHARED_SUFFIX: &str =
    " Set isolation = \"shared\" on step <a> to let it write the real checkout.";

fn shared_suffix(step: &str) -> String {
    SHARED_SUFFIX.replace("<a>", step)
}

/// How one task's delta resolved against the run base.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    dead_code,
    reason = "the worktree isolation contract fixes the outcome vocabulary"
)]
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
#[expect(
    dead_code,
    reason = "the worktree isolation contract fixes the base commit record"
)]
pub(crate) struct Base {
    /// Repository top-level directory.
    pub top: PathBuf,
    /// Forty hex characters of the base commit.
    pub commit: String,
    /// Workspace path relative to `top`.
    pub relative_workspace: PathBuf,
}

/// A preflight refusal raised before any job starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IsolationRefusal {
    NotARepository,
    NoGit,
    TooOld { version: String },
    NoHead,
    Denied { reason: String },
}

impl IsolationRefusal {
    /// Renders the exact model-facing refusal for one step.
    pub(crate) fn text(&self, step: &str, workspace: &str) -> String {
        let base = match self {
            IsolationRefusal::NotARepository => format!(
                "agents: step {step} needs a git worktree, but {workspace} is not inside a git repository."
            ),
            IsolationRefusal::NoGit => {
                format!("agents: step {step} needs a git worktree, but git was not found on PATH.")
            }
            IsolationRefusal::TooOld { version } => format!(
                "agents: step {step} needs git 2.17 or newer for worktrees; found {version}."
            ),
            IsolationRefusal::NoHead => format!(
                "agents: step {step} needs a git worktree, but the repository has no commit yet."
            ),
            IsolationRefusal::Denied { reason } => format!(
                "agents: step {step} needs a git worktree, but running git was denied: {reason}."
            ),
        };
        format!("{base}{}", shared_suffix(step))
    }
}

/// The exact git argv forms, each run with an explicit cwd inside the one
/// run job's scoped grant. Dynamic segments are caller-supplied paths.
pub(crate) fn argv_version() -> [&'static str; 2] {
    ["git", "--version"]
}

pub(crate) fn argv_toplevel(workspace: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        workspace.to_owned(),
        "rev-parse".to_owned(),
        "--show-toplevel".to_owned(),
    ]
}

pub(crate) fn argv_verify_head(top: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "rev-parse".to_owned(),
        "--verify".to_owned(),
        "HEAD".to_owned(),
    ]
}

pub(crate) fn argv_stash_create(top: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "stash".to_owned(),
        "create".to_owned(),
    ]
}

pub(crate) fn argv_worktree_add(top: &str, dir: &str, base: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "worktree".to_owned(),
        "add".to_owned(),
        "--detach".to_owned(),
        dir.to_owned(),
        base.to_owned(),
    ]
}

pub(crate) fn argv_add_all(dir: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        dir.to_owned(),
        "add".to_owned(),
        "-A".to_owned(),
    ]
}

pub(crate) fn argv_cached_diff(dir: &str, base: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        dir.to_owned(),
        "diff".to_owned(),
        "--cached".to_owned(),
        "--binary".to_owned(),
        base.to_owned(),
    ]
}

pub(crate) fn argv_cached_names(dir: &str, base: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        dir.to_owned(),
        "diff".to_owned(),
        "--cached".to_owned(),
        "--name-only".to_owned(),
        base.to_owned(),
    ]
}

pub(crate) fn argv_apply_check(top: &str, patch: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "apply".to_owned(),
        "--check".to_owned(),
        patch.to_owned(),
    ]
}

pub(crate) fn argv_apply(top: &str, patch: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "apply".to_owned(),
        patch.to_owned(),
    ]
}

pub(crate) fn argv_worktree_remove(top: &str, dir: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "worktree".to_owned(),
        "remove".to_owned(),
        "--force".to_owned(),
        dir.to_owned(),
    ]
}

pub(crate) fn argv_worktree_move(top: &str, dir: &str, retained: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "worktree".to_owned(),
        "move".to_owned(),
        dir.to_owned(),
        retained.to_owned(),
    ]
}

pub(crate) fn argv_worktree_prune(top: &str) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        top.to_owned(),
        "worktree".to_owned(),
        "prune".to_owned(),
    ]
}

/// Builds the retained-worktree notice appended to a task line.
pub(crate) fn retained_notice(
    dir: &str,
    stderr_line: &str,
    workspace: &str,
    patch: &str,
) -> String {
    format!(
        "isolation: retained at {dir}; the changes did not apply cleanly ({stderr_line}). Apply them by hand: git -C {workspace} apply --3way {patch}"
    )
}

/// Builds the exact `retained.json` body, terminated by one LF.
pub(crate) fn retained_body(reason: &str, base: &str, worktree: &str, at: &str) -> String {
    format!(
        "{{\"v\":1,\"reason\":\"{reason}\",\"base\":\"{base}\",\"worktree\":\"{worktree}\",\"at\":\"{at}\"}}\n"
    )
}
