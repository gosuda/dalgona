// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pure job-output monitor, status, quiet, and abort reducers.
//!
//! [`state`] owns monitor lifecycle and the `monitor` tool contract,
//! [`delivery`] owns clock-injected batch delivery, and [`status`] owns
//! inflight counts, the status payload, quiet polling, and `/abort`.

pub(crate) mod state;
pub(crate) mod status;

pub(crate) use status::{GoalPreview, InflightCounts};

#[cfg(test)]
mod tests;
