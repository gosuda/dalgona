//! Model thinking capabilities and level clamping.
//!
//! [`clamp`] searches downward and falls back to the lowest supported level
//! when no lower level is available.

use dal_core::ThinkingLevel;

use super::level::{Effort, rank};

/// The clamp notice template; see [`super::wire::ThinkingNotice::render`].
pub const CLAMP_NOTICE: &str =
    "Thinking level <requested> is not available for <model>; using <effective>.";

/// The temperature notice template; see [`super::wire::ThinkingNotice::render`].
pub const TEMPERATURE_NOTICE: &str =
    "Temperature is not available for <model> with thinking <level>; dalgon does not send it.";

/// What thinking a model accepts, as read from its catalog row.
#[derive(Clone, Debug)]
pub enum ThinkingSupport {
    /// `OpenAI` Chat, Responses, and Codex: the accepted effort levels.
    OpenAi {
        /// The accepted levels; `Off` here means the same as `none_supported`.
        accepted: Vec<ThinkingLevel>,
        /// Whether the effort `none` is accepted.
        none_supported: bool,
    },
    /// Anthropic with `capabilities.thinking.types.adaptive.supported`.
    Adaptive {
        /// Whether `{"type":"disabled"}` is accepted.
        can_disable: bool,
        /// Accepted effort levels from `capabilities.effort`.
        accepted: Vec<Effort>,
    },
    /// Anthropic with only `types.enabled.supported`.
    Budget {
        /// Whether `{"type":"disabled"}` is accepted.
        can_disable: bool,
    },
    /// Anthropic whose capabilities failed to fetch: adaptive, and `off`
    /// omits thinking and sends effort `low`.
    UnknownAdaptive,
}

impl ThinkingSupport {
    pub(crate) fn supports(&self, level: ThinkingLevel) -> bool {
        match self {
            Self::OpenAi {
                accepted,
                none_supported,
            } => accepted.contains(&level) || (level == ThinkingLevel::Off && *none_supported),
            Self::Adaptive { accepted, .. } => {
                Effort::adaptive(level).is_some_and(|effort| accepted.contains(&effort))
            }
            Self::Budget { .. } | Self::UnknownAdaptive => true,
        }
    }

    /// Moves `level` down to a supported level no lower than `floor`, else
    /// up to the lowest such level; `off` when nothing qualifies.
    pub(crate) fn ladder_down(&self, level: ThinkingLevel, floor: ThinkingLevel) -> ThinkingLevel {
        let ok = |l: &&ThinkingLevel| rank(**l) >= rank(floor) && self.supports(**l);
        if ok(&&level) {
            return level;
        }
        super::level::LADDER[..rank(level)]
            .iter()
            .rev()
            .find(ok)
            .or_else(|| super::level::LADDER.iter().find(ok))
            .copied()
            .unwrap_or(ThinkingLevel::Off)
    }
}

/// Returns the supported ladder levels for one model capability row.
///
/// When several levels map to the same provider effort, each remains
/// selectable. `off` is included only when the family can express disabled
/// thinking; unknown adaptive capabilities preserve the full fallback ladder.
#[must_use]
pub fn levels_for(support: &ThinkingSupport) -> Box<[ThinkingLevel]> {
    use ThinkingLevel::{High, Low, Max, Medium, Minimal, Off, Xhigh};
    let levels = [Off, Minimal, Low, Medium, High, Xhigh, Max];
    levels
        .into_iter()
        .filter(|level| match support {
            ThinkingSupport::OpenAi {
                accepted,
                none_supported,
            } => accepted.contains(level) || (*level == Off && *none_supported),
            ThinkingSupport::Adaptive {
                accepted,
                can_disable,
            } => {
                if *level == Off {
                    *can_disable
                } else {
                    let effort = match level {
                        Minimal | Low => Effort::Low,
                        Medium => Effort::Medium,
                        High => Effort::High,
                        Xhigh => Effort::Xhigh,
                        Max => Effort::Max,
                        Off => return *can_disable,
                    };
                    accepted.contains(&effort)
                }
            }
            ThinkingSupport::Budget { can_disable } => *level != Off || *can_disable,
            ThinkingSupport::UnknownAdaptive => true,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Clamps a requested level to the model's capabilities.
///
/// A missing level becomes the nearest lower supported level, else the lowest
/// supported level, else `off`. `off` itself never clamps: every family can
/// express it by omitting or disabling thinking. The second value is
/// [`CLAMP_NOTICE`] exactly when the level changed.
#[must_use]
pub fn clamp(
    level: ThinkingLevel,
    caps: &ThinkingSupport,
) -> (ThinkingLevel, Option<&'static str>) {
    if level == ThinkingLevel::Off {
        return (level, None);
    }
    let effective = caps.ladder_down(level, ThinkingLevel::Off);
    (effective, (effective != level).then_some(CLAMP_NOTICE))
}
