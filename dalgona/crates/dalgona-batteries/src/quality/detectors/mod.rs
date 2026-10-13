// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Report-only stream and turn detector lanes.

/// Repetition-collapse detector.
pub mod collapse;
/// Control-token run detector.
pub mod control_leak;
/// Fabricated unavailable-tool-call rule.
pub mod fabricated_call;
/// Repetitive-turn streak detector.
pub mod repetitive_turns;
