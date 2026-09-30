//! Closed configuration policy values shared by dal's command and runtime layers.

use std::path::PathBuf;
mod guard;
pub use guard::{GuardBands, GuardCheckMode, GuardPolicies, GuardSection};

mod edit_style;
pub use edit_style::EditStyleInput;

mod limits;
pub use limits::PluginLimits;

mod agents;
pub use agents::AgentsConfig;
mod error;
pub use error::ConfigError;
use error::{invalid_value, value_text};

#[cfg(test)]
mod tests;

mod eval;
mod layer;
mod parse;
mod partial;
mod prices;
mod types;
mod values;

pub use types::{
    ApprovalMode, Config, ConfigOverrides, ConfigProduct, JudgeMode, Mode, ModeProjection,
    PROMPT_DIAGRAMS, RulesConfig, Screen, ServeConfig, TuiConfig,
};
