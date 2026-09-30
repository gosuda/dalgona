//! The thinking ladder, capability clamping, and the request-parameter seam.
//!
//! Levels form one closed ordered set, `off < minimal < low < medium < high <
//! xhigh < max`, with `medium` as the default. [`clamp`] searches downward and
//! falls back to the lowest supported level when no lower level is available.
//! [`compose_hooks`] folds the `before_request` hooks in registration order,
//! and [`plan`] is the single pure mapping from the session level, the
//! merged hook patch, and the model's capabilities to the typed wire fragment
//! a family body writes. Nothing here reads configuration, touches the network,
//! or exposes a raw payload; notices are returned as values, and
//! [`SessionNotices`] keeps the once-per-session clamp rule.

mod hooks;
mod level;
mod support;
mod wire;

pub use hooks::{BeforeRequest, BeforeRequestInput, BeforeRequestPatch, compose_hooks};
pub use level::{DEFAULT_LEVEL, Effort, level_name, parse_level};
pub use support::{CLAMP_NOTICE, TEMPERATURE_NOTICE, ThinkingSupport, clamp, levels_for};
pub use wire::{
    AnthropicThinking, RequestLimits, SessionNotices, ThinkingNotice, ThinkingPlan, WireThinking,
    plan,
};

#[cfg(test)]
mod tests;
