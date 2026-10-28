//! Provider identity survives journal decoding and encoding.
//!
//! An assistant record may carry an optional top-level `provider` member.
//! Absence means legacy, unattributed provenance; a present non-empty string
//! is preserved exactly; `null`, an empty string, or a wrong type is refused;
//! unrelated top-level members stay forward-compatible. Both decode dialects
//! are checked: the manual journal-line codec (`decode`/`encode`) and the
//! serde carrier (`sonic_rs` over `EntryKind`). Fixtures inject the member as
//! a JSON literal, so no Rust `provider` field is needed.

use std::num::NonZeroU64;

use dal_core::{
    AssistantRoute, AssistantStop, DecodeError, EntryId, EntryKind, Family, Record, ReplaySource,
    Usage, decode, encode,
};

const LEGACY_ASSISTANT_LINE: &str = "{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\"}\n";
const SYNTHETIC_ASSISTANT_LINE: &str = "{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"probe/model\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\",\"route\":\"synthetic\"}\n";
const HARNESS_ASSISTANT_LINE: &str = "{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"dalgon/normal\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\",\"route\":\"harness\"}\n";

fn test_usage() -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
        premium_requests: None,
    }
}

fn legacy_kind() -> EntryKind {
    EntryKind::Assistant {
        api: Family::Anthropic,
        model: "m".into(),
        provider: None,
        content: Vec::new(),
        usage: test_usage(),
        stop: AssistantStop::Done,
        route: None,
        replay: None,
    }
}

fn with_top_level_member(
    line: &str,
    anchor: &str,
    member: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if !line.contains(anchor) {
        return Err("fixture anchor missing for provider injection".into());
    }
    Ok(line.replacen(anchor, &format!("{anchor},{member}"), 1))
}

fn serde_with_member(base: &str, member: &str) -> Result<String, Box<dyn std::error::Error>> {
    if !base.ends_with('}') {
        return Err("serde base is not a JSON object".into());
    }
    Ok(format!("{},{}", &base[..base.len() - 1], member) + "}")
}

fn assistant_route_of(
    record: &Record,
) -> Result<Option<AssistantRoute>, Box<dyn std::error::Error>> {
    match record {
        Record::Assistant(entry) => match &entry.kind {
            EntryKind::Assistant { route, .. } => Ok(*route),
            _ => Err("expected an assistant kind".into()),
        },
        _ => Err("expected an assistant record".into()),
    }
}

#[test]
fn manual_legacy_absent_assistant_stays_readable_and_byte_stable()
-> Result<(), Box<dyn std::error::Error>> {
    let decoded = decode(LEGACY_ASSISTANT_LINE.as_bytes())?.record;
    assert_eq!(assistant_route_of(&decoded)?, None);
    let again = encode(&decoded)?;
    assert_eq!(again.as_slice(), LEGACY_ASSISTANT_LINE.as_bytes());
    Ok(())
}

#[test]
fn serde_legacy_absent_assistant_kind_round_trips_without_provider()
-> Result<(), Box<dyn std::error::Error>> {
    let kind = legacy_kind();
    let text = sonic_rs::to_string(&kind)?;
    assert!(
        !text.contains("\"provider\""),
        "legacy serde kind must not name a provider: {text}"
    );
    assert_eq!(sonic_rs::from_str::<EntryKind>(&text)?, kind);
    Ok(())
}

#[test]
fn manual_present_provider_is_preserved_verbatim() -> Result<(), Box<dyn std::error::Error>> {
    let line = with_top_level_member(
        LEGACY_ASSISTANT_LINE,
        "\"model\":\"m\"",
        "\"provider\":\"acme-proxy\"",
    )?;
    assert!(
        line.contains("\"provider\":\"acme-proxy\""),
        "injected fixture must carry the provider member: {line}"
    );
    let decoded = decode(line.as_bytes())?.record;
    let again = encode(&decoded)?;
    let text = String::from_utf8(again)?;
    assert!(
        text.contains("\"provider\":\"acme-proxy\""),
        "manual round trip dropped the non-empty provider id: {text}"
    );
    assert!(
        !text.contains("\"model\":\"acme-proxy\""),
        "provider must not leak into the model id: {text}"
    );
    Ok(())
}

#[test]
fn serde_present_provider_is_preserved_verbatim() -> Result<(), Box<dyn std::error::Error>> {
    let base = sonic_rs::to_string(&legacy_kind())?;
    let injected = serde_with_member(&base, "\"provider\":\"acme-proxy\"")?;
    assert!(
        injected.contains("\"provider\":\"acme-proxy\""),
        "injected serde fixture must carry the provider member: {injected}"
    );
    let decoded = sonic_rs::from_str::<EntryKind>(&injected)?;
    let text = sonic_rs::to_string(&decoded)?;
    assert!(
        text.contains("\"provider\":\"acme-proxy\""),
        "serde round trip dropped the non-empty provider id: {text}"
    );
    assert!(
        !text.contains("\"model\":\"acme-proxy\""),
        "provider must not leak into the model id: {text}"
    );
    Ok(())
}

#[test]
fn manual_null_provider_is_rejected_not_dropped() -> Result<(), Box<dyn std::error::Error>> {
    let line = with_top_level_member(
        LEGACY_ASSISTANT_LINE,
        "\"model\":\"m\"",
        "\"provider\":null",
    )?;
    match decode(line.as_bytes()) {
        Err(error @ DecodeError::Invalid { .. }) => {
            assert!(
                error.to_string().contains("provider"),
                "manual null rejection must name the provider member: {error}"
            );
            Ok(())
        }
        Err(error) => {
            Err(format!("manual null provider failed in the wrong category: {error:?}").into())
        }
        Ok(decoded) => {
            let text = String::from_utf8(encode(&decoded.record)?)?;
            Err(format!("manual path dropped an explicit null provider: {text}").into())
        }
    }
}

#[test]
fn serde_null_provider_is_rejected_not_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let base = sonic_rs::to_string(&legacy_kind())?;
    let injected = serde_with_member(&base, "\"provider\":null")?;
    match sonic_rs::from_str::<EntryKind>(&injected) {
        Err(error) => {
            assert!(
                error.to_string().contains("provider"),
                "serde null rejection must name the provider member, not an unknown field: {error}"
            );
            Ok(())
        }
        Ok(kind) => {
            let text = sonic_rs::to_string(&kind)?;
            Err(format!("serde path ignored an explicit null provider: {text}").into())
        }
    }
}

#[test]
fn manual_empty_provider_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let line = with_top_level_member(
        LEGACY_ASSISTANT_LINE,
        "\"model\":\"m\"",
        "\"provider\":\"\"",
    )?;
    match decode(line.as_bytes()) {
        Err(error @ DecodeError::Invalid { .. }) => {
            assert!(
                error.to_string().contains("provider"),
                "manual empty rejection must name the provider member: {error}"
            );
            Ok(())
        }
        Err(error) => {
            Err(format!("manual empty provider failed in the wrong category: {error:?}").into())
        }
        Ok(decoded) => {
            let text = String::from_utf8(encode(&decoded.record)?)?;
            Err(format!("manual path dropped an empty provider: {text}").into())
        }
    }
}

#[test]
fn serde_empty_provider_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let base = sonic_rs::to_string(&legacy_kind())?;
    let injected = serde_with_member(&base, "\"provider\":\"\"")?;
    match sonic_rs::from_str::<EntryKind>(&injected) {
        Err(error) => {
            assert!(
                error.to_string().contains("provider"),
                "serde empty rejection must name the provider member: {error}"
            );
            Ok(())
        }
        Ok(kind) => {
            let text = sonic_rs::to_string(&kind)?;
            Err(format!("serde path ignored an empty provider: {text}").into())
        }
    }
}

#[test]
fn manual_wrong_type_provider_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    for member in [
        "\"provider\":42",
        "\"provider\":true",
        "\"provider\":{\"id\":\"acme-proxy\"}",
    ] {
        let line = with_top_level_member(LEGACY_ASSISTANT_LINE, "\"model\":\"m\"", member)?;
        match decode(line.as_bytes()) {
            Err(error @ DecodeError::Invalid { .. }) => {
                assert!(
                    error.to_string().contains("provider"),
                    "manual {member} rejection must name the provider member: {error}"
                );
            }
            Err(error) => {
                return Err(
                    format!("manual {member} failed in the wrong category: {error:?}").into(),
                );
            }
            Ok(decoded) => {
                let text = String::from_utf8(encode(&decoded.record)?)?;
                return Err(
                    format!("manual path dropped a wrong-type provider {member}: {text}").into(),
                );
            }
        }
    }
    Ok(())
}

#[test]
fn serde_wrong_type_provider_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    for member in [
        "\"provider\":42",
        "\"provider\":true",
        "\"provider\":{\"id\":\"acme-proxy\"}",
    ] {
        let base = sonic_rs::to_string(&legacy_kind())?;
        let injected = serde_with_member(&base, member)?;
        match sonic_rs::from_str::<EntryKind>(&injected) {
            Err(error) => {
                assert!(
                    error.to_string().contains("provider"),
                    "serde {member} rejection must name the provider member: {error}"
                );
            }
            Ok(kind) => {
                let text = sonic_rs::to_string(&kind)?;
                return Err(
                    format!("serde path ignored a wrong-type provider {member}: {text}").into(),
                );
            }
        }
    }
    Ok(())
}

#[test]
fn manual_synthetic_and_harness_routes_with_provider_are_rejected_with_good_controls()
-> Result<(), Box<dyn std::error::Error>> {
    for (line, name) in [
        (SYNTHETIC_ASSISTANT_LINE, "synthetic"),
        (HARNESS_ASSISTANT_LINE, "harness"),
    ] {
        let decoded = decode(line.as_bytes())?.record;
        let again = encode(&decoded)?;
        assert_eq!(
            again.as_slice(),
            line.as_bytes(),
            "{name} good control diverged"
        );
    }
    for (line, anchor, name) in [
        (
            SYNTHETIC_ASSISTANT_LINE,
            "\"model\":\"probe/model\"",
            "synthetic",
        ),
        (
            HARNESS_ASSISTANT_LINE,
            "\"model\":\"dalgon/normal\"",
            "harness",
        ),
    ] {
        let injected = with_top_level_member(line, anchor, "\"provider\":\"acme-proxy\"")?;
        match decode(injected.as_bytes()) {
            Err(error @ DecodeError::Invalid { .. }) => {
                assert!(
                    error.to_string().contains("provider"),
                    "manual {name}+provider rejection must name the provider member: {error}"
                );
            }
            Err(error) => {
                return Err(format!(
                    "manual {name}+provider failed in the wrong category: {error:?}"
                )
                .into());
            }
            Ok(decoded) => {
                let text = String::from_utf8(encode(&decoded.record)?)?;
                return Err(
                    format!("manual path kept a provider beside a {name} route: {text}").into(),
                );
            }
        }
    }
    Ok(())
}

#[test]
fn serde_synthetic_and_harness_routes_with_provider_are_rejected_with_good_controls()
-> Result<(), Box<dyn std::error::Error>> {
    let id = NonZeroU64::new(5).ok_or("fixture id is nonzero")?;
    let at: jiff::Timestamp = "2026-09-25T10:15:35.410Z".parse()?;
    for (model, route, name) in [
        ("probe/model", AssistantRoute::Synthetic, "synthetic"),
        ("dalgon/normal", AssistantRoute::Harness, "harness"),
    ] {
        let kind = EntryKind::Assistant {
            api: Family::Anthropic,
            model: model.into(),
            provider: None,
            content: Vec::new(),
            usage: test_usage(),
            stop: AssistantStop::Done,
            route: Some(route),
            replay: None,
        };
        let _ = (EntryId::new(id), at);
        let base = sonic_rs::to_string(&kind)?;
        assert_eq!(
            sonic_rs::from_str::<EntryKind>(&base)?,
            kind,
            "{name} good control diverged"
        );
        let injected = serde_with_member(&base, "\"provider\":\"acme-proxy\"")?;
        match sonic_rs::from_str::<EntryKind>(&injected) {
            Err(error) => {
                assert!(
                    error.to_string().contains("provider"),
                    "serde {name}+provider rejection must name the provider member: {error}"
                );
            }
            Ok(next) => {
                let text = sonic_rs::to_string(&next)?;
                return Err(
                    format!("serde path kept a provider beside a {name} route: {text}").into(),
                );
            }
        }
    }
    Ok(())
}

#[test]
fn manual_unrelated_top_level_member_stays_forward_compatible()
-> Result<(), Box<dyn std::error::Error>> {
    let line = with_top_level_member(
        LEGACY_ASSISTANT_LINE,
        "\"model\":\"m\"",
        "\"zzz_probe\":\"keep\"",
    )?;
    let decoded = decode(line.as_bytes())?.record;
    let text = String::from_utf8(encode(&decoded)?)?;
    assert!(
        !text.contains("zzz_probe"),
        "re-encode must not invent the unknown member: {text}"
    );
    Ok(())
}

#[test]
fn serde_unrelated_top_level_member_stays_forward_compatible()
-> Result<(), Box<dyn std::error::Error>> {
    let base = sonic_rs::to_string(&legacy_kind())?;
    let injected = serde_with_member(&base, "\"zzz_probe\":\"keep\"")?;
    let decoded = sonic_rs::from_str::<EntryKind>(&injected)?;
    assert_eq!(decoded, legacy_kind());
    Ok(())
}

#[test]
fn serde_nested_replay_source_empty_model_stays_rejected() -> Result<(), Box<dyn std::error::Error>>
{
    let good = sonic_rs::from_str::<ReplaySource>("{\"family\":\"anthropic\",\"model\":\"m\"}")?;
    assert_eq!(good.model.as_ref(), "m");
    match sonic_rs::from_str::<ReplaySource>("{\"family\":\"anthropic\",\"model\":\"\"}") {
        Err(error) => {
            assert!(
                error.to_string().contains("model"),
                "nested replay-source rejection must name the model member: {error}"
            );
            Ok(())
        }
        Ok(source) => Err(format!(
            "nested replay source accepted an empty model: {}",
            source.model.as_ref()
        )
        .into()),
    }
}
