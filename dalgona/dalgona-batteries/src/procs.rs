// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The procs battery: one deferred tool over this session's background jobs.

#[cfg(test)]
mod tests;
mod text;
mod tool;

use dal_core::ext::ServiceSet;
use dal_core::{Origin, RegistrationError};

pub use self::tool::PROCS_DESCRIPTION;
use self::tool::ProcsTool;

/// The text of the `dalgona://procs` manual page.
pub const PROCS_DOC: &str = concat!(
    "# procs\n\n",
    "One deferred tool over the background jobs of this session. Other sessions' ",
    "jobs are invisible. A first call by name promotes it, and a promoted tool ",
    "never leaves the tool list mid-session. The battery asks for the `jobs` ",
    "service only: it reads job rows and retained output text through that ",
    "service and opens no file.\n\n",
    "`op` `list` prints one line per job, live jobs oldest first, then at most ten ",
    "terminal jobs newest first: `<id> <status> <age> <label>: <command>`. The ",
    "status is `running`, `done` (exit code 0), `failed` (a nonzero exit, a failure, ",
    "or a lost job), or `cancelled`. The age is the time since the job's last ",
    "activity, in the jobs duration format (`48.0s`, `4m12s`, `1h02m05s`). The ",
    "jobs service keeps one label per job, so the command text is the label, cut ",
    "at 80 characters. `query` keeps only jobs whose label contains it as a ",
    "substring, `status` keeps one phase, and `limit` (1 to 50, default 20) caps the ",
    "rows. A filter that keeps no job answers with the no-match text below.\n\n",
    "`op` `grep` matches `query` as a regex over the retained output text of each ",
    "job, oldest job first and each job's text read once. Each match prints ",
    "`<job id>:<line>: <text>`, the text cut at `match_bytes` bytes (1 to 2048, ",
    "default 256) with control characters escaped. `max_matches` (1 to 200, ",
    "default 50) and a 16384-byte limit on the whole result, tail included, bound ",
    "what is shown; the tail `(<k> more matches elided; narrow query or raise ",
    "max_matches.)` counts every further match in the session's retained output. ",
    "The regex syntax has no look-around, and the error names the reason.\n\n",
    "A number outside its range, an unknown field, or a field of the other op is ",
    "rejected as `procs: invalid input: <reason>.`\n\n",
    "Errors, exact: `procs: no jobs in this session.`, `procs: no match for ",
    "\"<query>\" in <n> job(s).`, `procs: query is not a valid regex: <message>.`, ",
    "and `procs: the jobs service is not granted to this battery: <Denied text>.`\n",
);

/// Builds the procs extension: one deferred tool over the `jobs` service.
/// Registration performs no I/O; effects begin only after the host starts a
/// session.
///
/// # Errors
///
/// Returns [`RegistrationError`] when the fixed identity, service set, or
/// tool schema is rejected.
pub fn procs() -> Result<dal_agent::ext::Extension, RegistrationError> {
    let inject = ServiceSet::from_names(["jobs"])?;
    let tool = ProcsTool::new()?;
    dal_agent::ext::ExtensionBuilder::new("procs", env!("CARGO_PKG_VERSION"), inject)?
        .with_origin(Origin::Bundled, None)
        .tool(std::sync::Arc::new(tool), dal_core::Visibility::Deferred)
        .build()
}
