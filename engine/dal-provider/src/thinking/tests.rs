use super::*;
use dal_core::{Family, ThinkingLevel};
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex, PoisonError};

use super::level::{LADDER, rank};
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

const EFFORTS: [Effort; 5] = [
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// Every capability row a catalog can produce: all subsets of accepted
/// levels or efforts, with and without the disabled mode.
fn every_support_row() -> Vec<ThinkingSupport> {
    let mut rows = vec![
        ThinkingSupport::UnknownAdaptive,
        ThinkingSupport::Budget { can_disable: false },
        ThinkingSupport::Budget { can_disable: true },
    ];
    for mask in 0_u32..(1 << LADDER.len()) {
        let accepted: Vec<ThinkingLevel> = LADDER
            .iter()
            .enumerate()
            .filter(|(bit, _)| (mask >> bit) & 1 == 1)
            .map(|(_, level)| *level)
            .collect();
        for none_supported in [false, true] {
            rows.push(ThinkingSupport::OpenAi {
                accepted: accepted.clone(),
                none_supported,
            });
        }
    }
    for mask in 0_u32..(1 << EFFORTS.len()) {
        let accepted: Vec<Effort> = EFFORTS
            .iter()
            .enumerate()
            .filter(|(bit, _)| (mask >> bit) & 1 == 1)
            .map(|(_, effort)| *effort)
            .collect();
        for can_disable in [false, true] {
            rows.push(ThinkingSupport::Adaptive {
                can_disable,
                accepted: accepted.clone(),
            });
        }
    }
    rows
}

fn budget_plan(level: ThinkingLevel, max_tokens: u32, max_output: u32) -> ThinkingPlan {
    bare(
        level,
        &ThinkingSupport::Budget { can_disable: true },
        RequestLimits {
            max_tokens,
            max_output,
            temperature_allowed: false,
        },
    )
}

/// The budget `level` gets when the output ceiling is no constraint.
fn full_budget(level: ThinkingLevel) -> u32 {
    match budget_plan(level, 32_000, 64_000).wire {
        WireThinking::Anthropic {
            thinking: Enabled { budget_tokens, .. },
            ..
        } => budget_tokens,
        other => panic!("{level:?} must be enabled under a wide ceiling, got {other:?}"),
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
            notice,
            (got != requested).then_some(CLAMP_NOTICE),
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
        assert_eq!(got.notices, []);
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
    assert_eq!(unknown.notices, []);
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
    assert_eq!(
        got.notices,
        [ThinkingNotice::Clamp {
            requested: Minimal,
            effective: Off
        }]
    );
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
fn parse_level_rejects_invalid_spellings_with_original_text() {
    for bad in [
        "ultra", "Ultra", " low", "none", "", "Low", "HIGH", "Xhigh", "MAX", "low ", "low\n",
        "\tlow", "hig", "maximum", "high,low", "xhigh\0",
    ] {
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

struct Step {
    name: &'static str,
    ran: Arc<Mutex<Vec<String>>>,
    outcome: Result<BeforeRequestPatch, &'static str>,
}

impl BeforeRequest for Step {
    fn before_request<'a>(
        &'a self,
        input: BeforeRequestInput<'a>,
    ) -> BoxFuture<'a, Result<BeforeRequestPatch, ProviderError>> {
        Box::pin(async move {
            self.ran
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("{}:{}:{:?}", self.name, input.model, input.level));
            self.outcome
                .clone()
                .map_err(|detail| ProviderError::Protocol {
                    family: Family::Anthropic,
                    detail: detail.into(),
                })
        })
    }
}

#[test]
fn compose_stops_at_the_first_failing_hook_and_gives_every_hook_the_same_input() {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let step = |name, outcome| -> Arc<dyn BeforeRequest> {
        Arc::new(Step {
            name,
            ran: Arc::clone(&ran),
            outcome,
        })
    };
    let patch = |effort, temperature| -> Result<BeforeRequestPatch, &'static str> {
        Ok(BeforeRequestPatch {
            effort,
            temperature,
        })
    };
    let hooks = vec![
        step("a", patch(Some(Effort::Low), Some(0.3))),
        step("b", Err("first failure")),
        step("c", Err("second failure")),
        step("d", patch(Some(Effort::Max), None)),
    ];
    let failed = futures::executor::block_on(compose_hooks(&hooks, "m", High));
    assert!(
        matches!(&failed, Err(ProviderError::Protocol { detail, .. }) if detail == "first failure"),
        "{failed:?}"
    );
    let seen = ran.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(seen, ["a:m:High", "b:m:High"]);

    let empty =
        futures::executor::block_on(compose_hooks(&[], "m", High)).expect("no hook, no failure");
    assert_eq!((empty.effort, empty.temperature), (None, None));
}

#[test]
fn clamp_notices_are_deduplicated_by_model_and_requested_level_only() {
    let clamp_notice = |requested, effective| ThinkingNotice::Clamp {
        requested,
        effective,
    };
    let mut seen = SessionNotices::default();
    assert!(seen.first("m1", clamp_notice(Xhigh, High)));
    assert!(!seen.first("m1", clamp_notice(Xhigh, High)));
    assert!(
        !seen.first("m1", clamp_notice(Xhigh, Medium)),
        "the effective level is not part of the key"
    );
    assert!(
        seen.first("m1", clamp_notice(Max, High)),
        "another requested level on the same model shows again"
    );
    assert!(
        seen.first("m2", clamp_notice(Xhigh, High)),
        "another model shows again"
    );
    assert!(!seen.first("m2", clamp_notice(Xhigh, High)));
    assert!(!seen.first("m1", clamp_notice(Max, High)));
}

#[test]
fn levels_for_lists_exactly_the_levels_clamp_leaves_alone() {
    for caps in every_support_row() {
        let listed = levels_for(&caps);
        assert!(
            listed.windows(2).all(|pair| rank(pair[0]) < rank(pair[1])),
            "{caps:?} lists {listed:?} out of ladder order"
        );
        for level in LADDER {
            if level == Off {
                let expected = match &caps {
                    ThinkingSupport::OpenAi {
                        accepted,
                        none_supported,
                    } => *none_supported || accepted.contains(&Off),
                    ThinkingSupport::Adaptive { can_disable, .. }
                    | ThinkingSupport::Budget { can_disable } => *can_disable,
                    ThinkingSupport::UnknownAdaptive => true,
                };
                assert_eq!(listed.contains(&Off), expected, "{caps:?}");
            } else {
                let (kept, _) = clamp(level, &caps);
                assert_eq!(
                    listed.contains(&level),
                    kept == level,
                    "{level:?} on {caps:?} lists {listed:?}"
                );
            }
        }
    }
}

#[test]
fn clamp_is_idempotent_monotone_and_never_skips_a_closer_supported_level() {
    for caps in every_support_row() {
        let supported: Vec<ThinkingLevel> = LADDER
            .into_iter()
            .filter(|level| caps.supports(*level))
            .collect();
        let mut previous = 0;
        for requested in LADDER {
            let (kept, notice) = clamp(requested, &caps);
            let context = format!("{requested:?} -> {kept:?} on {caps:?}");
            assert_eq!(
                notice,
                (kept != requested).then_some(CLAMP_NOTICE),
                "{context}"
            );
            assert!(kept == Off || caps.supports(kept), "{context}");
            assert_eq!(clamp(kept, &caps), (kept, None), "{context}");
            assert!(rank(kept) >= previous, "not monotone: {context}");
            previous = rank(kept);
            if requested == Off || supported.contains(&requested) {
                assert_eq!(kept, requested, "{context}");
            } else if rank(kept) < rank(requested) {
                assert!(
                    !supported
                        .iter()
                        .any(|level| rank(*level) > rank(kept) && rank(*level) < rank(requested)),
                    "skipped a closer supported level: {context}"
                );
            } else {
                assert!(
                    !supported
                        .iter()
                        .any(|level| rank(*level) <= rank(requested) || rank(*level) < rank(kept)),
                    "moved up although a level at or below the request is supported: {context}"
                );
            }
        }
    }
}

#[test]
fn budget_mode_never_exceeds_the_ceiling_and_always_leaves_answer_room() {
    for level in [Minimal, Low, Medium, High, Xhigh, Max] {
        let full = full_budget(level);
        let need = full + 1024;
        let mut ceilings = vec![
            0,
            1,
            1023,
            1024,
            2047,
            2048,
            2049,
            need - 1,
            need,
            need + 1,
            2 * need,
            u32::MAX,
        ];
        ceilings.sort_unstable();
        ceilings.dedup();
        for requested_max in [0, 1024, 4096, u32::MAX] {
            let mut previous_budget = 0;
            let mut was_enabled = false;
            for &max_output in &ceilings {
                let context =
                    format!("{level:?} max_tokens {requested_max} max_output {max_output}");
                let got = budget_plan(level, requested_max, max_output);
                match got.wire {
                    WireThinking::Anthropic {
                        thinking:
                            Enabled {
                                budget_tokens,
                                max_tokens,
                            },
                        effort: None,
                    } => {
                        assert_eq!(got.level, level, "{context}");
                        assert!(got.notices.is_empty(), "{context}");
                        assert!(budget_tokens >= 1024, "{context}: budget {budget_tokens}");
                        assert!(budget_tokens <= full, "{context}: budget {budget_tokens}");
                        assert!(max_tokens <= max_output, "{context}: sent {max_tokens}");
                        assert!(
                            max_tokens >= budget_tokens + 1024,
                            "{context}: no answer room, budget {budget_tokens} sent {max_tokens}"
                        );
                        assert!(
                            max_tokens >= requested_max.min(max_output),
                            "{context}: sent {max_tokens} is below the requested limit"
                        );
                        if max_output >= need {
                            assert_eq!(budget_tokens, full, "{context}");
                        } else {
                            assert_eq!(budget_tokens + 1024, max_output, "{context}");
                        }
                        assert!(budget_tokens >= previous_budget, "{context}: not monotone");
                        previous_budget = budget_tokens;
                        was_enabled = true;
                    }
                    other => {
                        assert_eq!(other, an(Disabled, None), "{context}");
                        assert!(max_output < 2048, "{context}: fell back to off");
                        assert!(!was_enabled, "{context}: thinking switched off again");
                        assert_eq!(got.level, Off, "{context}");
                        assert_eq!(
                            got.notices,
                            [ThinkingNotice::Clamp {
                                requested: level,
                                effective: Off
                            }],
                            "{context}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn budget_mode_falls_back_to_off_exactly_below_two_thousand_forty_eight_tokens() {
    for level in [Minimal, Low, Medium, High, Xhigh, Max] {
        let full = full_budget(level);
        let below = budget_plan(level, 1024, 2047);
        assert_eq!(below.level, Off, "{level:?}");
        assert_eq!(below.wire, an(Disabled, None), "{level:?}");
        for (max_output, budget_tokens) in [(2048, 1024), (2049, full.min(1025))] {
            let got = budget_plan(level, 1024, max_output);
            assert_eq!(got.level, level, "{level:?} at {max_output}");
            assert!(
                matches!(got.wire, WireThinking::Anthropic {
                    thinking: Enabled { budget_tokens: got_budget, .. },
                    ..
                } if got_budget == budget_tokens),
                "{level:?} at {max_output}: {:?}",
                got.wire
            );
        }
    }
}

#[test]
fn level_spellings_agree_across_the_parser_the_core_name_and_the_error_list() {
    let spellings = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
    assert_eq!(LADDER.map(level_name), spellings);
    for (level, spelling) in LADDER.into_iter().zip(spellings) {
        assert_eq!(parse_level(spelling).ok(), Some(level), "{spelling}");
        assert_eq!(
            level.name(),
            spelling,
            "dal-core spells {level:?} differently"
        );
    }
    assert_eq!(
        crate::error::THINKING_LEVEL_NAMES
            .split(", ")
            .collect::<Vec<_>>(),
        spellings
    );
}

#[test]
fn notice_templates_and_rendered_notices_never_drift() {
    for (requested, effective) in [(Max, High), (Low, Off), (Medium, Minimal)] {
        assert_eq!(
            ThinkingNotice::Clamp {
                requested,
                effective
            }
            .render("claude-sonnet-5"),
            CLAMP_NOTICE
                .replace("<requested>", requested.name())
                .replace("<model>", "claude-sonnet-5")
                .replace("<effective>", effective.name())
        );
    }
    assert_eq!(
        ThinkingNotice::Temperature { level: Max }.render("claude-opus-5"),
        TEMPERATURE_NOTICE
            .replace("<model>", "claude-opus-5")
            .replace("<level>", "max")
    );
}
