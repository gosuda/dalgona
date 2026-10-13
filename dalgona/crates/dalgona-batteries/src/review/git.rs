// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Read-only workspace collection: fixed git argv and bounded capture.

use std::ffi::OsString;

use dal_core::{RunRequest, Workspace};

use super::{DIFF_TRUNCATION_MARKER, GIT_STDOUT_PREFIX_LIMIT, MAX_DIFF_BYTES, utf8_prefix};

pub(crate) fn diff_argv(base: &str) -> Vec<OsString> {
    let base = if base.is_empty() { "HEAD" } else { base };
    vec![
        OsString::from("git"),
        OsString::from("diff"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from(base),
    ]
}

pub(crate) fn status_argv() -> Vec<OsString> {
    vec![
        OsString::from("git"),
        OsString::from("status"),
        OsString::from("--short"),
    ]
}

pub(crate) fn git_run_request(workspace: &Workspace, argv: Vec<OsString>) -> RunRequest {
    RunRequest {
        argv,
        cwd: Some(workspace.as_path().to_path_buf()),
        stdin: None,
        timeout: None,
        env: vec![(Box::from("GIT_OPTIONAL_LOCKS"), Box::from("0"))],
        stdout_prefix_limit: GIT_STDOUT_PREFIX_LIMIT,
    }
}
pub(crate) fn cap_diff(diff: &str, overflowed: bool) -> (String, bool) {
    let prefix = utf8_prefix(diff, MAX_DIFF_BYTES);
    let truncated = overflowed || prefix.len() != diff.len();
    if !truncated {
        return (prefix.to_owned(), false);
    }
    let mut capped = String::with_capacity(prefix.len() + DIFF_TRUNCATION_MARKER.len());
    capped.push_str(prefix);
    capped.push_str(DIFF_TRUNCATION_MARKER);
    (capped, true)
}

pub(crate) fn status_exceeds_limit(prefix: &[u8], overflowed: bool) -> bool {
    overflowed || prefix.len() > MAX_DIFF_BYTES
}
