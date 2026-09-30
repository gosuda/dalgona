use super::*;
use dal_core::{Family, ThinkingLevel};
use futures::future::BoxFuture;
use std::sync::Arc;

use super::level::LADDER;
use crate::error::ProviderError;

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
        &[
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ],
        true,
    );
    let budget = ThinkingSupport::Budget { can_disable: true };
    let rows = [
        (
            Off,
            oa(Some("none")),
            oa(None),
            an(Disabled, None),
            an(Disabled, None),
        ),
        (
            Minimal,
            oa(Some("minimal")),
            oa(Some("minimal")),
            an(Ad, Some(Effort::Low)),
            an(enabled(1024), None),
        ),
        (
            Low,
            oa(Some("low")),
            oa(Some("low")),
            an(Ad, Some(Effort::Low)),
            an(enabled(2048), None),
        ),
        (
            Medium,
            oa(Some("medium")),
            oa(Some("medium")),
            an(Ad, Some(Effort::Medium)),
            an(enabled(8192), None),
        ),
        (
            High,
            oa(Some("high")),
            oa(Some("high")),
            an(Ad, Some(Effort::High)),
            an(enabled(16384), None),
        ),
        (
            Xhigh,
            oa(Some("xhigh")),
            oa(Some("xhigh")),
            an(Ad, Some(Effort::Xhigh)),
            an(enabled(16384), None),
        ),
        (
            Max,
            oa(Some("max")),
            oa(Some("max")),
            an(Ad, Some(Effort::Max)),
            an(enabled(16384), None),
        ),
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
        assert_eq!(
            notice.is_some(),
            got != requested,
            "{requested:?} on {caps:?}"
        );
        assert!(
            caps.supports(got) || got == Off,
            "{requested:?} on {caps:?}"
        );
    }
}

#[test]
fn adaptive_effort_capabilities_are_independent() {
    let low_medium = adaptive_support(&[Effort::Low, Effort::Medium], true);
    let max_without_xhigh = adaptive_support(
        &[Effort::Low, Effort::Medium, Effort::High, Effort::Max],
        true,
    );
    let xhigh_without_max = adaptive_support(
        &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh],
        true,
    );
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
        assert_eq!(
            notice.is_some(),
            requested != expected,
            "{requested:?} on {caps:?}"
        );
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
    assert_eq!(
        got.notices,
        [ThinkingNotice::Clamp {
            requested: Max,
            effective: Medium
        }]
    );

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
    assert!(
        got.notices
            .contains(&ThinkingNotice::Temperature { level: Medium })
    );
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
    let caps = adaptive_support(
        &[Effort::Low, Effort::Medium, Effort::High, Effort::Max],
        true,
    );
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
    let omit = bare(
        Medium,
        &ThinkingSupport::Budget { can_disable: false },
        tight(1500),
    );
    assert_eq!(omit.wire, an(Omit, None));
}

#[test]
fn hook_effort_temperature() {
    let patch = BeforeRequestPatch {
        effort: Some(Effort::Max),
        temperature: Some(0.2),
    };
    let caps = adaptive_support(
        &[
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ],
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
        [
            "Temperature is not available for claude-opus-5 with thinking max; dalgon does not send it."
        ]
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
    assert!(
        openai_plan
            .notices
            .contains(&ThinkingNotice::Temperature { level: High })
    );
    let anthropic = adaptive_support(&[Effort::Low, Effort::High], true);
    let anthropic_plan = plan(Off, &patch, &anthropic, WIDE);
    assert_eq!(anthropic_plan.level, High);
    assert_eq!(anthropic_plan.wire, an(Ad, Some(Effort::High)));
    assert_eq!(anthropic_plan.temperature, None);
}

fn off_case(
    level: ThinkingLevel,
    patch: &BeforeRequestPatch,
    caps: &ThinkingSupport,
    limits: RequestLimits,
    wire: WireThinking,
    temperature: Option<f32>,
) -> (
    ThinkingLevel,
    BeforeRequestPatch,
    ThinkingSupport,
    RequestLimits,
    WireThinking,
    Option<f32>,
) {
    (
        level,
        patch.clone(),
        caps.clone(),
        limits,
        wire,
        temperature,
    )
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
    let full_t = openai(&full, true);
    let full_f = openai(&full, false);
    let empty = openai(&[], false);
    let p_high = hook(Some(Effort::High));
    let p_none = hook(None);
    let p_low = hook(Some(Effort::Low));
    let low_med_high = adaptive_support(&[Effort::Low, Effort::Medium, Effort::High], true);
    let low_high = adaptive_support(&[Effort::Low, Effort::High], true);
    let low_only = adaptive_support(&[Effort::Low], false);
    let med_only = adaptive_support(&[Effort::Medium], false);
    let budget = ThinkingSupport::Budget { can_disable: true };
    let unknown = ThinkingSupport::UnknownAdaptive;
    let big = an(enabled(16384), None);
    let hi_ad = an(Ad, Some(Effort::High));
    let omit_low = an(Omit, Some(Effort::Low));
    let omit_high = an(Omit, Some(Effort::High));
    let min_med = openai(&[Minimal, Medium], false);
    // (level, patch, caps, limits, wire, temperature)
    let cases = [
        off_case(Off, &p_high, &full_t, WIDE, oa(Some("high")), None),
        off_case(Off, &p_high, &full_f, WIDE, oa(Some("high")), None),
        off_case(Off, &p_high, &empty, WIDE, oa(None), Some(0.5)),
        off_case(Off, &p_none, &full_t, WIDE, oa(Some("none")), Some(0.5)),
        off_case(Off, &p_none, &full_t, no_temp, oa(Some("none")), None),
        off_case(Medium, &p_none, &full_t, WIDE, oa(Some("medium")), None),
        off_case(
            Medium,
            &hook(Some(Effort::Xhigh)),
            &full_t,
            WIDE,
            oa(Some("high")),
            None,
        ),
        off_case(Minimal, &p_low, &min_med, WIDE, oa(Some("minimal")), None),
        off_case(Off, &p_high, &low_med_high, WIDE, hi_ad, None),
        off_case(Off, &p_none, &low_high, WIDE, an(Disabled, None), Some(0.5)),
        off_case(Off, &p_none, &budget, WIDE, an(Disabled, None), Some(0.5)),
        off_case(Off, &p_none, &low_only, WIDE, omit_low, Some(0.5)),
        off_case(Off, &p_none, &med_only, WIDE, an(Omit, None), Some(0.5)),
        off_case(Off, &p_high, &unknown, WIDE, omit_high, None),
        off_case(Off, &p_none, &unknown, WIDE, omit_low, Some(0.5)),
        off_case(High, &p_low, &budget, WIDE, big, None),
        off_case(Off, &p_low, &budget, WIDE, an(Disabled, None), Some(0.5)),
        off_case(Off, &p_none, &budget, WIDE, an(Disabled, None), Some(0.5)),
    ];
    for (level, patch, caps, limits, wire, temperature) in cases {
        let got = plan(level, &patch, &caps, limits);
        assert_eq!(got.wire, wire, "{level:?} {patch:?} on {caps:?}");
        assert_eq!(
            got.temperature, temperature,
            "{level:?} {patch:?} on {caps:?}"
        );
        let dropped = got
            .notices
            .contains(&ThinkingNotice::Temperature { level: got.level });
        assert_eq!(
            dropped,
            temperature.is_none(),
            "{level:?} {patch:?} on {caps:?}"
        );
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
    assert!(
        matches!(failed, Err(ProviderError::Protocol { detail, .. }) if detail == "hook refused")
    );
    Ok(())
}
