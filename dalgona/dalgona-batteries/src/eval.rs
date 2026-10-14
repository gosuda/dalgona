// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Eval packs: Starlark case sets and the runner behind `dalgona eval`.
//!
//! A pack is pure Starlark data and functions ([`pack`]); the runner
//! ([`run`]) owns the workspace, the clock, and the scoring over an
//! [`run::Executor`] that the dalgona binary backs with a real host. The
//! [`probe`] extension is added to the hosts a run starts.

mod docs;
pub mod pack;
pub mod probe;
pub mod report;
pub mod resolve;
pub mod run;

pub use docs::EVAL_DOC;
pub use tokio_util::sync::CancellationToken;

/// The `dalgona eval --help` usage line.
pub const USAGE: &str =
    "dalgona eval <pack> [--case <id>] [--repeats <n>] [--model <id>] [--against <dal.toml>]";

/// When a run started and the directory name that identifies it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stamp {
    /// RFC 3339 UTC with milliseconds, the report's `started_at`.
    pub started_at: String,
    /// A sortable, unique run directory name.
    pub run_id: String,
}

impl Stamp {
    /// Stamps the current time.
    #[must_use]
    pub fn now() -> Self {
        let now = jiff::Timestamp::now();
        let unique = uuid::Uuid::new_v4().simple().to_string();
        Self {
            started_at: now.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            run_id: format!("{}-{}", now.strftime("%Y%m%dT%H%M%SZ"), &unique[..8]),
        }
    }
}

#[cfg(test)]
mod tests;
