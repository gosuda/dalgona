// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Delivery tests: ordered counts, bounded notices, one honesty suffix.

use super::*;
use crate::orchestration::agents_tool::{ReportCell, ReportStatus, submit};
use crate::orchestration::pool::TaskState;
