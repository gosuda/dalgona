//! The pure request-thinking planner and its wire fragments.
//!
//! [`plan`] is the single mapping from session level, merged hook patch, and
//! model capabilities to the typed wire fragment a family body writes.

use std::collections::{HashMap, HashSet};

use dal_core::ThinkingLevel;

use super::{
    hooks::BeforeRequestPatch,
    level::{Effort, MIN_BUDGET, level_name},
    support::{ThinkingSupport, clamp},
};

/// The request facts [`plan`] needs besides the capabilities.
#[derive(Clone, Copy, Debug)]
pub struct RequestLimits {
    /// The `max_tokens` the body would carry without thinking.
    pub max_tokens: u32,
    /// The model's output ceiling.
    pub max_output: u32,
    /// Whether the model row allows a temperature at all.
    pub temperature_allowed: bool,
}

/// The Anthropic `thinking` member.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnthropicThinking {
    /// No `thinking` member.
    Omit,
    /// `{"type":"disabled"}`.
    Disabled,
    /// `{"type":"adaptive"}`.
    Adaptive,
    /// `{"type":"enabled","budget_tokens":<b>}` with the body's `max_tokens`
    /// raised to `max_tokens`.
    Enabled {
        /// The thinking budget.
        budget_tokens: u32,
        /// The body's `max_tokens`.
        max_tokens: u32,
    },
}

/// The thinking fragment of one request body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireThinking {
    /// Chat `reasoning_effort`, or Responses and Codex `reasoning.effort`;
    /// `None` omits the member.
    OpenAi {
        /// The effort string, `"none"` included.
        effort: Option<&'static str>,
    },
    /// The Anthropic `thinking` member and `output_config.effort`.
    Anthropic {
        /// The `thinking` member.
        thinking: AnthropicThinking,
        /// `output_config.effort`; `None` omits it.
        effort: Option<Effort>,
    },
}

/// A notice produced while planning; the caller shows each clamp notice at
/// most once per session for each `(model, requested)` pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThinkingNotice {
    /// A level or hook effort moved down the ladder.
    Clamp {
        /// The level asked for.
        requested: ThinkingLevel,
        /// The level used.
        effective: ThinkingLevel,
    },
    /// A temperature was dropped.
    Temperature {
        /// The effective level of the request.
        level: ThinkingLevel,
    },
}

impl ThinkingNotice {
    /// The notice text for `model`.
    #[must_use]
    pub fn render(self, model: &str) -> String {
        match self {
            Self::Clamp {
                requested,
                effective,
            } => format!(
                "Thinking level {} is not available for {model}; using {}.",
                level_name(requested),
                level_name(effective)
            ),
            Self::Temperature { level } => format!(
                "Temperature is not available for {model} with thinking {}; dalgon does not send it.",
                level_name(level)
            ),
        }
    }
}

/// The clamp notices already shown in one session.
///
/// A clamp notice is shown at most once for each `(model, requested)` pair;
/// temperature notices are shown every time.
#[derive(Debug, Default)]
pub struct SessionNotices {
    clamps: HashMap<Box<str>, HashSet<ThinkingLevel>>,
}

impl SessionNotices {
    /// Records `notice` for `model` and reports whether to show it now.
    pub fn first(&mut self, model: &str, notice: ThinkingNotice) -> bool {
        match notice {
            ThinkingNotice::Clamp { requested, .. } => {
                if let Some(levels) = self.clamps.get_mut(model) {
                    return levels.insert(requested);
                }
                let mut levels = HashSet::new();
                levels.insert(requested);
                self.clamps.insert(model.into(), levels);
                true
            }
            ThinkingNotice::Temperature { .. } => true,
        }
    }
}

/// The composed thinking parameters of one request.
#[derive(Clone, Debug, PartialEq)]
pub struct ThinkingPlan {
    /// The effective level after capability clamps, budget fallback, and hook effort.
    pub level: ThinkingLevel,
    /// The thinking fragment to write.
    pub wire: WireThinking,
    /// The temperature to send, if any.
    pub temperature: Option<f32>,
    /// Notices in the order they arose.
    pub notices: Vec<ThinkingNotice>,
}

fn budget_for(level: ThinkingLevel) -> u32 {
    match level {
        ThinkingLevel::Off => 0,
        ThinkingLevel::Minimal => 1024,
        ThinkingLevel::Low => 2048,
        ThinkingLevel::Medium => 8192,
        ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 16384,
    }
}

fn anthropic_off(can_disable: bool) -> WireThinking {
    WireThinking::Anthropic {
        thinking: if can_disable {
            AnthropicThinking::Disabled
        } else {
            AnthropicThinking::Omit
        },
        effort: None,
    }
}

fn adaptive(level: ThinkingLevel, can_disable: bool) -> WireThinking {
    match Effort::adaptive(level) {
        Some(effort) => WireThinking::Anthropic {
            thinking: AnthropicThinking::Adaptive,
            effort: Some(effort),
        },
        None if can_disable => anthropic_off(true),
        None => WireThinking::Anthropic {
            thinking: AnthropicThinking::Omit,
            effort: Some(Effort::Low),
        },
    }
}

/// Uses known adaptive effort capabilities; only sends the required off-mode
/// low fallback when the catalog confirms that low effort is supported.
fn known_adaptive(level: ThinkingLevel, can_disable: bool, accepted: &[Effort]) -> WireThinking {
    if level != ThinkingLevel::Off {
        return adaptive(level, can_disable);
    }
    if can_disable {
        return anthropic_off(true);
    }
    if accepted.contains(&Effort::Low) {
        return adaptive(level, false);
    }
    WireThinking::Anthropic {
        thinking: AnthropicThinking::Omit,
        effort: None,
    }
}

/// The budget-mode `thinking` member for an `on` level, or `None` when the
/// output ceiling leaves less than the minimum budget.
fn budget_thinking(level: ThinkingLevel, limits: RequestLimits) -> Option<AnthropicThinking> {
    let wanted = budget_for(level);
    let need = wanted + MIN_BUDGET;
    let budget = if limits.max_output < need {
        limits.max_output.saturating_sub(MIN_BUDGET)
    } else {
        wanted
    };
    (budget >= MIN_BUDGET).then(|| AnthropicThinking::Enabled {
        budget_tokens: budget,
        max_tokens: limits.max_tokens.max(need).min(limits.max_output),
    })
}
/// The wire fragment for a clamped level, with the level it actually
/// expresses: budget mode falls back to `off` when the budget cannot fit.
fn fragment(
    level: ThinkingLevel,
    caps: &ThinkingSupport,
    limits: RequestLimits,
) -> (ThinkingLevel, WireThinking) {
    let wire = match caps {
        ThinkingSupport::OpenAi { .. } => WireThinking::OpenAi {
            effort: match level {
                ThinkingLevel::Off => caps.supports(ThinkingLevel::Off).then_some("none"),
                on => Some(level_name(on)),
            },
        },
        ThinkingSupport::Adaptive {
            can_disable,
            accepted,
        } => known_adaptive(level, *can_disable, accepted),
        ThinkingSupport::UnknownAdaptive => adaptive(level, false),
        ThinkingSupport::Budget { can_disable } => {
            let enabled = match level {
                ThinkingLevel::Off => None,
                on => budget_thinking(on, limits),
            };
            let Some(thinking) = enabled else {
                return (ThinkingLevel::Off, anthropic_off(*can_disable));
            };
            WireThinking::Anthropic {
                thinking,
                effort: None,
            }
        }
    };
    (level, wire)
}

/// Applies a supported hook effort to a family effort field.
fn replace_effort(wire: &mut WireThinking, target: ThinkingLevel, caps: &ThinkingSupport) -> bool {
    let Some(new_effort) = Effort::adaptive(target) else {
        return false;
    };
    match (wire, caps) {
        (WireThinking::OpenAi { effort }, _) => {
            *effort = Some(level_name(target));
            true
        }
        (
            WireThinking::Anthropic {
                effort: Some(e), ..
            },
            _,
        ) => {
            *e = new_effort;
            true
        }
        (WireThinking::Anthropic { thinking, effort }, ThinkingSupport::Adaptive { .. })
            if *thinking == AnthropicThinking::Disabled =>
        {
            *thinking = AnthropicThinking::Adaptive;
            *effort = Some(new_effort);
            true
        }
        (
            WireThinking::Anthropic {
                thinking: AnthropicThinking::Omit,
                effort,
            },
            ThinkingSupport::Adaptive { .. },
        ) => {
            *effort = Some(new_effort);
            true
        }
        _ => false,
    }
}

/// Composes one request's thinking parameters in this order: session `level`,
/// merged hook patch, model-capability clamp, then the wire fragment.
///
/// A hook effort overrides the level-derived provider effort and can enable an
/// off-mode effort only when the family has a supported effort field. Budget
/// mode carries no effort field. Temperature is kept only when the model row
/// allows it and the post-hook effective level is `off`.
#[must_use]
pub fn plan(
    level: ThinkingLevel,
    patch: &BeforeRequestPatch,
    caps: &ThinkingSupport,
    limits: RequestLimits,
) -> ThinkingPlan {
    let mut notices = Vec::new();
    let (effective, clamped) = clamp(level, caps);
    if clamped.is_some() {
        notices.push(ThinkingNotice::Clamp {
            requested: level,
            effective,
        });
    }

    let (fallback, mut wire) = fragment(effective, caps, limits);
    if fallback != effective {
        notices.push(ThinkingNotice::Clamp {
            requested: level,
            effective: fallback,
        });
    }
    let mut effective = fallback;

    if let Some(asked) = patch.effort {
        let target = caps.ladder_down(asked.level(), ThinkingLevel::Minimal);
        let effort_family = matches!(
            caps,
            ThinkingSupport::OpenAi { .. }
                | ThinkingSupport::Adaptive { .. }
                | ThinkingSupport::UnknownAdaptive
        );
        let replaced = target != ThinkingLevel::Off && replace_effort(&mut wire, target, caps);
        if replaced {
            effective = target;
        }
        if effort_family && target != asked.level() {
            notices.push(ThinkingNotice::Clamp {
                requested: asked.level(),
                effective: target,
            });
        }
    }

    let temperature = patch.temperature.and_then(|t| {
        let keep = limits.temperature_allowed && effective == ThinkingLevel::Off;
        if !keep {
            notices.push(ThinkingNotice::Temperature { level: effective });
        }
        keep.then_some(t)
    });

    ThinkingPlan {
        level: effective,
        wire,
        temperature,
        notices,
    }
}
