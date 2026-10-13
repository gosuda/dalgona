// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Exec rewrite: clamps a classified sleep-wait's foreground window to five
//! seconds so the approval ladder previews the rewritten value.

use dal_core::RawJson;
use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};

use super::GuardError;
use super::guard::parse_args;
use super::sleep::SleepWait;

/// Rewrites `foreground_s` to JSON number `5` for a classified sleep-wait.
/// This bounds the foreground wait so a sleep cannot park the orchestration
/// owner indefinitely; after detachment, the normal job monitor owns it.
/// Returns `None` (arguments unchanged) for a non-object body, a missing
/// window that cannot be represented, or an existing window below five
/// seconds, which is never lengthened.
pub(crate) fn rewrite_exec_args(
    args: &RawJson,
    _wait: SleepWait,
) -> Result<Option<RawJson>, GuardError> {
    let mut parsed: Value = parse_args(args)?;
    let Some(object) = parsed.as_object_mut() else {
        return Ok(None);
    };
    if let Some(value) = object.get(&"foreground_s") {
        match value.as_f64() {
            Some(seconds) if seconds < 5.0 => return Ok(None),
            Some(_) => {}
            None => return Ok(None),
        }
    }
    object.insert("foreground_s", 5_u64);
    let encoded = sonic_rs::to_string(&parsed).map_err(GuardError::json)?;
    RawJson::parse(&encoded).map(Some).map_err(GuardError::json)
}
