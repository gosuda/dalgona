// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pool index-ordering tests: the last item may finish first.

use super::*;

fn result(state: TaskState) -> TaskResult {
    TaskResult {
        id: JobId::new_v7(),
        state,
        changed: Vec::new(),
    }
}
