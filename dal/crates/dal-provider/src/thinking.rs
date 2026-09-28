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

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use dal_core::ThinkingLevel;
use futures::future::BoxFuture;

use crate::error::ProviderError;

/// Every level in ladder order, lowest first.
const LADDER: [ThinkingLevel; 7] = [
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

/// The clamp notice template; see [`ThinkingNotice::render`].
pub const CLAMP_NOTICE: &str =
    "Thinking level <requested> is not available for <model>; using <effective>.";

/// The temperature notice template; see [`ThinkingNotice::render`].
pub const TEMPERATURE_NOTICE: &str =
    "Temperature is not available for <model> with thinking <level>; dalgon does not send it.";

/// The smallest Anthropic thinking budget; a smaller budget turns thinking off.
const MIN_BUDGET: u32 = 1024;

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

    fn level(self) -> ThinkingLevel {
        match self {
            Self::Low => ThinkingLevel::Low,
            Self::Medium => ThinkingLevel::Medium,
            Self::High => ThinkingLevel::High,
            Self::Xhigh => ThinkingLevel::Xhigh,
            Self::Max => ThinkingLevel::Max,
        }
    }

    /// The Anthropic adaptive effort for a thinking level; `off` has none.
    fn adaptive(level: ThinkingLevel) -> Option<Self> {
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

fn rank(level: ThinkingLevel) -> usize {
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

/// What thinking a model accepts, as read from its catalog row.
#[derive(Clone, Debug)]
pub enum ThinkingSupport {
    /// OpenAI Chat, Responses, and Codex: the accepted effort levels.
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
    fn supports(&self, level: ThinkingLevel) -> bool {
        match self {
            Self::OpenAi {
                accepted,
                none_supported,
            } => {
                accepted.contains(&level) || (level == ThinkingLevel::Off && *none_supported)
            }
            Self::Adaptive { accepted, .. } => Effort::adaptive(level)
                .is_some_and(|effort| accepted.contains(&effort)),
            Self::Budget { .. } | Self::UnknownAdaptive => true,
        }
    }

    /// Moves `level` down to a supported level no lower than `floor`, else
    /// up to the lowest such level; `off` when nothing qualifies.
    fn ladder_down(&self, level: ThinkingLevel, floor: ThinkingLevel) -> ThinkingLevel {
        let ok = |l: &&ThinkingLevel| rank(**l) >= rank(floor) && self.supports(**l);
        if ok(&&level) {
            return level;
        }
        LADDER[..rank(level)]
            .iter()
            .rev()
            .find(ok)
            .or_else(|| LADDER.iter().find(ok))
            .copied()
            .unwrap_or(ThinkingLevel::Off)
    }
}

/// Clamps a requested level to the model's capabilities.
///
/// A missing level becomes the nearest lower supported level, else the lowest
/// supported level, else `off`. `off` itself never clamps: every family can
/// express it by omitting or disabling thinking. The second value is
/// [`CLAMP_NOTICE`] exactly when the level changed.
#[must_use]
pub fn clamp(level: ThinkingLevel, caps: &ThinkingSupport) -> (ThinkingLevel, Option<&'static str>) {
    if level == ThinkingLevel::Off {
        return (level, None);
    }
    let effective = caps.ladder_down(level, ThinkingLevel::Off);
    (effective, (effective != level).then_some(CLAMP_NOTICE))
}

/// What a `before_request` hook sees.
#[derive(Clone, Copy, Debug)]
pub struct BeforeRequestInput<'a> {
    /// The model id of the request.
    pub model: &'a str,
    /// The session level, before clamping.
    pub level: ThinkingLevel,
}

/// The typed parameters a `before_request` hook may set; `None` keeps the
/// value composed so far.
#[derive(Clone, Debug, Default)]
pub struct BeforeRequestPatch {
    /// Replaces the level-derived effort on families with an effort field.
    pub effort: Option<Effort>,
    /// A sampling temperature, sent only when [`plan`] allows it.
    pub temperature: Option<f32>,
}

/// A Rust or Starlark hook run before each provider request.
pub trait BeforeRequest: Send + Sync {
    /// Returns the parameters this hook sets for one request.
    fn before_request<'a>(
        &'a self,
        input: BeforeRequestInput<'a>,
    ) -> BoxFuture<'a, Result<BeforeRequestPatch, ProviderError>>;
}

/// Runs `hooks` in registration order; a later hook's `Some` field replaces
/// an earlier one.
///
/// # Errors
///
/// The first hook error, unchanged; later hooks do not run.
pub async fn compose_hooks(
    hooks: &[Arc<dyn BeforeRequest>],
    model: &str,
    level: ThinkingLevel,
) -> Result<BeforeRequestPatch, ProviderError> {
    let mut merged = BeforeRequestPatch::default();
    for hook in hooks {
        let patch = hook.before_request(BeforeRequestInput { model, level }).await?;
        merged.effort = patch.effort.or(merged.effort);
        merged.temperature = patch.temperature.or(merged.temperature);
    }
    Ok(merged)
}

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
fn known_adaptive(
    level: ThinkingLevel,
    can_disable: bool,
    accepted: &[Effort],
) -> WireThinking {
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
fn replace_effort(
    wire: &mut WireThinking,
    target: ThinkingLevel,
    caps: &ThinkingSupport,
) -> bool {
    let Some(new_effort) = Effort::adaptive(target) else {
        return false;
    };
    match (wire, caps) {
        (WireThinking::OpenAi { effort }, _) => {
            *effort = Some(level_name(target));
            true
        }
        (WireThinking::Anthropic { effort: Some(e), .. }, _) => {
            *e = new_effort;
            true
        }
        (
            WireThinking::Anthropic {
                thinking,
                effort,
            },
            ThinkingSupport::Adaptive { .. },
        ) if *thinking == AnthropicThinking::Disabled => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use dal_core::Family;

    use AnthropicThinking::{Adaptive as Ad, Disabled, Enabled, Omit};
    use ThinkingLevel::{High, Low, Max, Medium, Minimal, Off, Xhigh};

    const WIDE: RequestLimits = RequestLimits {
        max_tokens: 32_000,
        max_output: 64_000,
        temperature_allowed: true,
    };

    fn openai(accepted: &[ThinkingLevel], none_supported: bool) -> ThinkingSupport {
        ThinkingSupport::OpenAi {
            accepted: accepted.to_vec(),
            none_supported,
        }
    }

    fn adaptive_support(accepted: &[Effort], can_disable: bool) -> ThinkingSupport {
        ThinkingSupport::Adaptive {
            can_disable,
            accepted: accepted.to_vec(),
        }
    }

    fn oa(effort: Option<&'static str>) -> WireThinking {
        WireThinking::OpenAi { effort }
    }

    fn an(thinking: AnthropicThinking, effort: Option<Effort>) -> WireThinking {
        WireThinking::Anthropic { thinking, effort }
    }

    fn bare(level: ThinkingLevel, caps: &ThinkingSupport, limits: RequestLimits) -> ThinkingPlan {
        plan(level, &BeforeRequestPatch::default(), caps, limits)
    }

    fn enabled(budget_tokens: u32) -> AnthropicThinking {
        Enabled {
            budget_tokens,
            max_tokens: 32_000,
        }
    }

    #[test]
    fn thinking_table_mappings() {
        let full = [Minimal, Low, Medium, High, Xhigh, Max];
        let with_none = openai(&full, true);
        let without_none = openai(&full, false);
        let adaptive = adaptive_support(
            &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max],
            true,
        );
        let budget = ThinkingSupport::Budget { can_disable: true };
        let rows = [
            (Off, oa(Some("none")), oa(None), an(Disabled, None), an(Disabled, None)),
            (Minimal, oa(Some("minimal")), oa(Some("minimal")), an(Ad, Some(Effort::Low)), an(enabled(1024), None)),
            (Low, oa(Some("low")), oa(Some("low")), an(Ad, Some(Effort::Low)), an(enabled(2048), None)),
            (Medium, oa(Some("medium")), oa(Some("medium")), an(Ad, Some(Effort::Medium)), an(enabled(8192), None)),
            (High, oa(Some("high")), oa(Some("high")), an(Ad, Some(Effort::High)), an(enabled(16384), None)),
            (Xhigh, oa(Some("xhigh")), oa(Some("xhigh")), an(Ad, Some(Effort::Xhigh)), an(enabled(16384), None)),
            (Max, oa(Some("max")), oa(Some("max")), an(Ad, Some(Effort::Max)), an(enabled(16384), None)),
        ];
        for (level, chat, responses, adapt, budg) in rows {
            for (caps, want) in [
                (&with_none, chat),
                (&without_none, responses),
                (&adaptive, adapt),
                (&budget, budg),
            ] {
                let got = bare(level, caps, WIDE);
                assert_eq!(got.wire, want, "{level:?} on {caps:?}");
                assert_eq!(got.level, level, "{level:?} on {caps:?}");
                assert!(got.notices.is_empty(), "{level:?} on {caps:?}");
            }
        }
    }

    #[test]
    fn clamp_fallback_table() {
        let no_xhigh = adaptive_support(
            &[Effort::Low, Effort::Medium, Effort::High, Effort::Max],
            true,
        );
        let cases = [
            (Xhigh, no_xhigh.clone(), High),
            (Max, no_xhigh, Max),
            (Minimal, openai(&[Low], true), Off),
            (Minimal, openai(&[Low], false), Low),
            (Max, openai(&[Low, Medium], false), Medium),
            (Low, openai(&[Medium, High], false), Medium),
            (Medium, openai(&[], false), Off),
            (Off, openai(&[], false), Off),
            (Off, ThinkingSupport::UnknownAdaptive, Off),
            (Max, ThinkingSupport::UnknownAdaptive, Max),
        ];
        for (requested, caps, want) in cases {
            let (got, notice) = clamp(requested, &caps);
            assert_eq!(got, want, "{requested:?} on {caps:?}");
            assert_eq!(notice.is_some(), got != requested, "{requested:?} on {caps:?}");
            assert!(caps.supports(got) || got == Off, "{requested:?} on {caps:?}");
        }
    }

    #[test]
    fn adaptive_effort_capabilities_are_independent() {
        let low_medium = adaptive_support(&[Effort::Low, Effort::Medium], true);
        let max_without_xhigh =
            adaptive_support(&[Effort::Low, Effort::Medium, Effort::High, Effort::Max], true);
        let xhigh_without_max =
            adaptive_support(&[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh], true);
        let no_effort = adaptive_support(&[], true);
        let cases = [
            (High, low_medium.clone(), Medium),
            (Xhigh, max_without_xhigh.clone(), High),
            (Max, max_without_xhigh, Max),
            (Xhigh, xhigh_without_max.clone(), Xhigh),
            (Max, xhigh_without_max, Xhigh),
            (Medium, no_effort, Off),
        ];
        for (requested, caps, expected) in cases {
            let (effective, notice) = clamp(requested, &caps);
            assert_eq!(effective, expected, "{requested:?} on {caps:?}");
            assert_eq!(notice.is_some(), requested != expected, "{requested:?} on {caps:?}");
            if expected != Off {
                assert!(caps.supports(expected), "{expected:?} on {caps:?}");
            }
        }

        let patch = BeforeRequestPatch {
            effort: Some(Effort::Max),
            temperature: None,
        };
        let got = plan(Low, &patch, &low_medium, WIDE);
        assert_eq!(got.wire, an(Ad, Some(Effort::Medium)));
        assert_eq!(got.notices, [ThinkingNotice::Clamp { requested: Max, effective: Medium }]);

        let no_levels = adaptive_support(&[], true);
        let got = bare(Medium, &no_levels, WIDE);
        assert_eq!(got.level, Off);
        assert_eq!(got.wire, an(Disabled, None));
        assert_eq!(
            got.notices,
            [ThinkingNotice::Clamp {
                requested: Medium,
                effective: Off
            }]
        );
    }

    #[test]
    fn known_adaptive_off_never_sends_unsupported_low() {
        let no_low = adaptive_support(&[Effort::Medium], false);
        let no_effort = adaptive_support(&[], false);
        for caps in [no_low.clone(), no_effort.clone()] {
            let got = bare(Off, &caps, WIDE);
            assert_eq!(got.level, Off);
            assert_eq!(got.wire, an(Omit, None));
            assert!(got.notices.is_empty());
        }
        let patch = BeforeRequestPatch {
            effort: Some(Effort::Medium),
            temperature: Some(0.2),
        };
        let got = plan(Off, &patch, &no_low, WIDE);
        assert_eq!(got.level, Medium);
        assert_eq!(got.wire, an(Omit, Some(Effort::Medium)));
        assert_eq!(got.temperature, None);
        assert!(got.notices.contains(&ThinkingNotice::Temperature { level: Medium }));
        let unsupported = BeforeRequestPatch {
            effort: Some(Effort::Max),
            temperature: None,
        };
        let got = plan(Off, &unsupported, &no_effort, WIDE);
        assert_eq!(got.wire, an(Omit, None));
        assert_eq!(got.level, Off);
        assert_eq!(
            got.notices,
            [ThinkingNotice::Clamp {
                requested: Max,
                effective: Off
            }]
        );
        let unknown = bare(Off, &ThinkingSupport::UnknownAdaptive, WIDE);
        assert_eq!(unknown.wire, an(Omit, Some(Effort::Low)));
        assert!(unknown.notices.is_empty());
    }

    #[test]
    fn clamp_down_notice() {
        let caps = adaptive_support(&[Effort::Low, Effort::Medium, Effort::High, Effort::Max], true);
        let got = bare(Xhigh, &caps, WIDE);
        assert_eq!(got.level, High);
        assert_eq!(got.wire, an(Ad, Some(Effort::High)));
        assert_eq!(
            got.notices
                .iter()
                .map(|n| n.render("claude-sonnet-5"))
                .collect::<Vec<_>>(),
            ["Thinking level xhigh is not available for claude-sonnet-5; using high."]
        );
        let mut seen = SessionNotices::default();
        let shown = |seen: &mut SessionNotices| {
            bare(Xhigh, &caps, WIDE)
                .notices
                .into_iter()
                .filter(|n| seen.first("claude-sonnet-5", *n))
                .count()
        };
        assert_eq!(shown(&mut seen), 1);
        assert_eq!(shown(&mut seen), 0);
        assert!(seen.first(
            "claude-opus-5",
            ThinkingNotice::Clamp {
                requested: Xhigh,
                effective: High
            }
        ));
        let temperature = ThinkingNotice::Temperature { level: High };
        assert!(seen.first("claude-sonnet-5", temperature));
        assert!(seen.first("claude-sonnet-5", temperature));
    }

    #[test]
    fn clamp_to_off() {
        let got = bare(Minimal, &openai(&[Low], true), WIDE);
        assert_eq!(got.level, Off);
        assert_eq!(got.wire, oa(Some("none")));
    }

    #[test]
    fn cant_disable_model() {
        for caps in [
            adaptive_support(&[Effort::Low], false),
            ThinkingSupport::UnknownAdaptive,
        ] {
            let got = bare(Off, &caps, WIDE);
            assert_eq!(got.wire, an(Omit, Some(Effort::Low)), "{caps:?}");
            assert!(got.notices.is_empty(), "{caps:?}");
        }
    }

    #[test]
    fn budget_mode() {
        let caps = ThinkingSupport::Budget { can_disable: true };
        let tight = |max_output| RequestLimits {
            max_tokens: 1024,
            max_output,
            temperature_allowed: false,
        };
        let got = bare(Medium, &caps, tight(4096));
        assert_eq!(
            got.wire,
            an(
                Enabled {
                    budget_tokens: 3072,
                    max_tokens: 4096
                },
                None
            )
        );
        let got = bare(Medium, &caps, tight(1500));
        assert_eq!(got.level, Off);
        assert_eq!(got.wire, an(Disabled, None));
        assert_eq!(
            got.notices
                .iter()
                .map(|n| n.render("claude-haiku"))
                .collect::<Vec<_>>(),
            ["Thinking level medium is not available for claude-haiku; using off."]
        );
        let omit = bare(Medium, &ThinkingSupport::Budget { can_disable: false }, tight(1500));
        assert_eq!(omit.wire, an(Omit, None));
    }

    #[test]
    fn hook_effort_temperature() {
        let patch = BeforeRequestPatch {
            effort: Some(Effort::Max),
            temperature: Some(0.2),
        };
        let caps = adaptive_support(
            &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max],
            true,
        );
        let got = plan(Low, &patch, &caps, WIDE);
        assert_eq!(got.level, Max);
        assert_eq!(got.wire, an(Ad, Some(Effort::Max)));
        assert_eq!(got.temperature, None);
        assert_eq!(
            got.notices
                .iter()
                .map(|n| n.render("claude-opus-5"))
                .collect::<Vec<_>>(),
            ["Temperature is not available for claude-opus-5 with thinking max; dalgon does not send it."]
        );
        let capped = adaptive_support(&[Effort::Low, Effort::Medium, Effort::High], true);
        let got = plan(Low, &patch, &capped, WIDE);
        assert_eq!(got.level, High);
        assert_eq!(got.wire, an(Ad, Some(Effort::High)));
        assert!(got.notices.contains(&ThinkingNotice::Clamp {
            requested: Max,
            effective: High
        }));
    }

    #[test]
    fn hook_effort_at_off_drives_temperature_precedence() {
        let patch = BeforeRequestPatch {
            effort: Some(Effort::High),
            temperature: Some(0.5),
        };
        let openai_caps = openai(&[Low, High], true);
        let openai_plan = plan(Off, &patch, &openai_caps, WIDE);
        assert_eq!(openai_plan.level, High);
        assert_eq!(openai_plan.wire, oa(Some("high")));
        assert_eq!(openai_plan.temperature, None);
        assert!(openai_plan
            .notices
            .contains(&ThinkingNotice::Temperature { level: High }));
        let anthropic = adaptive_support(&[Effort::Low, Effort::High], true);
        let anthropic_plan = plan(Off, &patch, &anthropic, WIDE);
        assert_eq!(anthropic_plan.level, High);
        assert_eq!(anthropic_plan.wire, an(Ad, Some(Effort::High)));
        assert_eq!(anthropic_plan.temperature, None);
    }

    #[test]
    fn off_and_temperature_precedence() {
        let hook = |effort| BeforeRequestPatch {
            effort,
            temperature: Some(0.5),
        };
        let no_temp = RequestLimits {
            temperature_allowed: false,
            ..WIDE
        };
        let full = [Minimal, Low, Medium, High];
        // (level, patch, caps, limits, wire, temperature)
        let cases = [
            (Off, hook(Some(Effort::High)), openai(&full, true), WIDE, oa(Some("high")), None),
            (Off, hook(Some(Effort::High)), openai(&full, false), WIDE, oa(Some("high")), None),
            (Off, hook(Some(Effort::High)), openai(&[], false), WIDE, oa(None), Some(0.5)),
            (Off, hook(None), openai(&full, true), WIDE, oa(Some("none")), Some(0.5)),
            (Off, hook(None), openai(&full, true), no_temp, oa(Some("none")), None),
            (Medium, hook(None), openai(&full, true), WIDE, oa(Some("medium")), None),
            (Medium, hook(Some(Effort::Xhigh)), openai(&full, true), WIDE, oa(Some("high")), None),
            (Minimal, hook(Some(Effort::Low)), openai(&[Minimal, Medium], false), WIDE, oa(Some("minimal")), None),
            (
                Off,
                hook(Some(Effort::High)),
                adaptive_support(&[Effort::Low, Effort::Medium, Effort::High], true),
                WIDE,
                an(Ad, Some(Effort::High)),
                None,
            ),
            (
                Off,
                hook(None),
                adaptive_support(&[Effort::Low, Effort::High], true),
                WIDE,
                an(Disabled, None),
                Some(0.5),
            ),
            (Off, hook(None), ThinkingSupport::Budget { can_disable: true }, WIDE, an(Disabled, None), Some(0.5)),
            (
                Off,
                hook(None),
                adaptive_support(&[Effort::Low], false),
                WIDE,
                an(Omit, Some(Effort::Low)),
                Some(0.5),
            ),
            (
                Off,
                hook(None),
                adaptive_support(&[Effort::Medium], false),
                WIDE,
                an(Omit, None),
                Some(0.5),
            ),
            (Off, hook(Some(Effort::High)), ThinkingSupport::UnknownAdaptive, WIDE, an(Omit, Some(Effort::High)), None),
            (Off, hook(None), ThinkingSupport::UnknownAdaptive, WIDE, an(Omit, Some(Effort::Low)), Some(0.5)),
            (High, hook(Some(Effort::Low)), ThinkingSupport::Budget { can_disable: true }, WIDE, an(enabled(16384), None), None),
            (Off, hook(Some(Effort::Low)), ThinkingSupport::Budget { can_disable: true }, WIDE, an(Disabled, None), Some(0.5)),
            (Off, hook(None), ThinkingSupport::Budget { can_disable: true }, WIDE, an(Disabled, None), Some(0.5)),
        ];
        for (level, patch, caps, limits, wire, temperature) in cases {
            let got = plan(level, &patch, &caps, limits);
            assert_eq!(got.wire, wire, "{level:?} {patch:?} on {caps:?}");
            assert_eq!(got.temperature, temperature, "{level:?} {patch:?} on {caps:?}");
            let dropped = got
                .notices
                .contains(&ThinkingNotice::Temperature { level: got.level });
            assert_eq!(dropped, temperature.is_none(), "{level:?} {patch:?} on {caps:?}");
        }
    }

    #[test]
    fn parse_level_accepts_all_ladder_spellings() {
        for level in LADDER {
            assert!(matches!(
                parse_level(level_name(level)),
                Ok(parsed) if parsed == level
            ));
        }
    }

    #[test]
    fn parse_level_rejects_invalid_spellings_with_original_text() {
        for bad in ["ultra", "Ultra", " low", "none", ""] {
            let error = parse_level(bad);
            assert!(matches!(
                &error,
                Err(ProviderError::InvalidThinkingLevel { spelling }) if spelling == bad
            ));
            let display = match error {
                Err(error) => error.to_string(),
                Ok(_) => String::new(),
            };
            assert_eq!(
                display,
                format!(
                    "unknown thinking level \"{bad}\"; use one of off, minimal, low, medium, high, xhigh, max."
                )
            );
        }
    }


    struct Fixed(BeforeRequestPatch);

    impl BeforeRequest for Fixed {
        fn before_request<'a>(
            &'a self,
            input: BeforeRequestInput<'a>,
        ) -> BoxFuture<'a, Result<BeforeRequestPatch, ProviderError>> {
            Box::pin(async move {
                if input.model.is_empty() {
                    return Err(ProviderError::Protocol {
                        family: Family::Anthropic,
                        detail: "hook refused".into(),
                    });
                }
                Ok(self.0.clone())
            })
        }
    }

    #[test]
    fn hooks_compose_in_registration_order() -> Result<(), ProviderError> {
        let hooks: Vec<Arc<dyn BeforeRequest>> = vec![
            Arc::new(Fixed(BeforeRequestPatch {
                effort: Some(Effort::Low),
                temperature: Some(0.3),
            })),
            Arc::new(Fixed(BeforeRequestPatch {
                effort: Some(Effort::Max),
                temperature: None,
            })),
        ];
        let merged = futures::executor::block_on(compose_hooks(&hooks, "m", Medium))?;
        assert_eq!(merged.effort, Some(Effort::Max));
        assert_eq!(merged.temperature, Some(0.3));
        let failed = futures::executor::block_on(compose_hooks(&hooks, "", Medium));
        assert!(matches!(failed, Err(ProviderError::Protocol { detail, .. }) if detail == "hook refused"));
        Ok(())
    }
}
