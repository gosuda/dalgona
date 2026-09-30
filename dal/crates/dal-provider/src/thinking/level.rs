//! The closed thinking-level ladder and its spellings.
//!
//! Levels form one ordered set, `off < minimal < low < medium < high <
//! xhigh < max`, with `medium` as the default.

use dal_core::ThinkingLevel;

use crate::error::ProviderError;

/// Every level in ladder order, lowest first.
pub(crate) const LADDER: [ThinkingLevel; 7] = [
    ThinkingLevel::Off,
    ThinkingLevel::Minimal,
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::Xhigh,
    ThinkingLevel::Max,
];

/// The level used when neither the session nor the configuration names one.
pub const DEFAULT_LEVEL: ThinkingLevel = ThinkingLevel::Medium;

/// The smallest Anthropic thinking budget; a smaller budget turns thinking off.
pub(crate) const MIN_BUDGET: u32 = 1024;

/// An effort a hook may request in place of the level-derived effort.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effort {
    /// Low effort.
    Low,
    /// Medium effort.
    Medium,
    /// High effort.
    High,
    /// Extra-high effort.
    Xhigh,
    /// Maximum effort.
    Max,
}

impl Effort {
    /// The wire spelling shared by every family that carries an effort field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        level_name(self.level())
    }

    pub(crate) fn level(self) -> ThinkingLevel {
        match self {
            Self::Low => ThinkingLevel::Low,
            Self::Medium => ThinkingLevel::Medium,
            Self::High => ThinkingLevel::High,
            Self::Xhigh => ThinkingLevel::Xhigh,
            Self::Max => ThinkingLevel::Max,
        }
    }

    /// The Anthropic adaptive effort for a thinking level; `off` has none.
    pub(crate) fn adaptive(level: ThinkingLevel) -> Option<Self> {
        match level {
            ThinkingLevel::Off => None,
            ThinkingLevel::Minimal | ThinkingLevel::Low => Some(Self::Low),
            ThinkingLevel::Medium => Some(Self::Medium),
            ThinkingLevel::High => Some(Self::High),
            ThinkingLevel::Xhigh => Some(Self::Xhigh),
            ThinkingLevel::Max => Some(Self::Max),
        }
    }
}

/// The configuration and CLI spelling of a level.
#[must_use]
pub fn level_name(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

pub(crate) fn rank(level: ThinkingLevel) -> usize {
    LADDER.iter().position(|l| *l == level).unwrap_or(0)
}

/// Parses one of `off, minimal, low, medium, high, xhigh, max`, exactly.
///
/// # Errors
///
/// Returns [`ProviderError::InvalidThinkingLevel`] when `s` is not a supported
/// spelling. The error retains the rejected spelling and displays the accepted
/// values.
pub fn parse_level(s: &str) -> Result<ThinkingLevel, ProviderError> {
    LADDER
        .into_iter()
        .find(|level| level_name(*level) == s)
        .ok_or_else(|| ProviderError::InvalidThinkingLevel {
            spelling: s.to_owned(),
        })
}
