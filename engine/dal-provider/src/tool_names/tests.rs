use std::{borrow::Cow, collections::BTreeSet, error::Error, sync::Arc};

use dal_core::{
    Family, ModelRequest, ModelRoute, ModelToolSpec, Purpose, RawJson, RequestParams, ThinkingLevel,
};

use super::{ANTHROPIC_OAUTH_NAME_MAX, ToolNames, WIRE_NAME_MAX, wire_name};
use crate::{
    claude_fingerprint::CLAUDE_TOOL_PREFIX,
    stream::{StreamEvent, ToolArgs, ToolCall},
};

type TestResult = Result<(), Box<dyn Error>>;

fn request(names: &[&str]) -> Result<ModelRequest, Box<dyn Error>> {
    let parameters = RawJson::parse(r#"{"type":"object","properties":{}}"#)?;
    let tools = names
        .iter()
        .map(|name| ModelToolSpec {
            name: (*name).into(),
            description: "tool".into(),
            parameters: parameters.clone(),
            grammar: None,
            defer_loading: false,
        })
        .collect::<Vec<_>>();
    Ok(ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Chat,
            model: "test-model".into(),
            provider: None,
        },
        system: Arc::from(""),
        tools: tools.into(),
        context: Arc::from([]),
        params: RequestParams {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    })
}

#[test]
fn mapped_tool_name_has_stable_safe_wire_spelling() {
    assert_eq!(
        wire_name("deploy.web-x.list", 64),
        "deploy_web-x_list_41588e56"
    );
}

#[test]
fn long_mapped_tool_name_stays_within_wire_limit() {
    let name = format!("deploy.{}", "x".repeat(220));
    let wire = wire_name(&name, 64);
    assert_eq!(wire.len(), 64);
    assert!(
        wire.bytes()
            .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') })
    );
}

#[test]
fn non_ascii_tool_names_fold_without_exceeding_the_byte_limit() {
    let internal = format!("deploy.{}", "é".repeat(80));
    let wire = wire_name(&internal, 64);
    assert_eq!(wire.len(), 64);
    assert!(
        wire.bytes()
            .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') })
    );
}

#[test]
fn advertised_names_mapping_detects_fold_collisions() -> TestResult {
    let internal = "deploy.web-x.list";
    let wire = wire_name(internal, 64).into_owned();
    let request = request(&[&wire, internal])?;
    let error = ToolNames::for_request(&request, 64)
        .err()
        .ok_or("collision accepted")?;
    assert!(matches!(
        &error,
        crate::ProviderError::ToolNameCollision { wire: found, first, second }
            if found == &wire && first == &wire && second == internal
    ));
    assert!(error.to_string().contains(&wire));
    assert!(error.to_string().contains(internal));
    assert!(!error.retryable_by_loop());
    Ok(())
}

#[test]
fn streamed_wire_tool_call_name_restores_to_internal_name() -> TestResult {
    let internal = "deploy.web-x.list";
    let request = request(&[internal])?;
    let names = ToolNames::for_request(&request, 64)?;
    let wire = wire_name(internal, 64).into_owned();
    let event = StreamEvent::ToolCallsDone {
        calls: vec![ToolCall {
            id: String::from("call-1"),
            name: wire,
            args: ToolArgs::Parsed(RawJson::parse("{}")?),
        }],
    };
    assert!(matches!(
        names.restore_event(event),
        StreamEvent::ToolCallsDone { calls }
            if calls.len() == 1 && calls[0].name == internal
    ));
    Ok(())
}

#[test]
fn streamed_started_tool_name_restores_to_internal_name() -> TestResult {
    let internal = "deploy.web-x.list";
    let request = request(&[internal])?;
    let names = ToolNames::for_request(&request, 64)?;
    let wire = wire_name(internal, 64).into_owned();
    assert!(matches!(
        names.restore_event(StreamEvent::ToolCallStarted {
            id: String::from("call-1"),
            name: wire,
        }),
        StreamEvent::ToolCallStarted { name, .. } if name == internal
    ));
    Ok(())
}

#[test]
fn the_limit_is_inclusive_and_one_byte_over_is_hashed_to_exactly_the_limit() {
    // `_` plus eight hex digits is the suffix of every mapped name.
    const SUFFIX: usize = 9;
    for max in [WIRE_NAME_MAX, ANTHROPIC_OAUTH_NAME_MAX] {
        let below = "a".repeat(max - 1);
        let at = "b".repeat(max);
        let over = "c".repeat(max + 1);
        assert!(
            matches!(wire_name(&below, max), Cow::Borrowed(kept) if kept == below),
            "{max}: a name one byte under the limit is kept"
        );
        assert!(
            matches!(wire_name(&at, max), Cow::Borrowed(kept) if kept == at),
            "{max}: a name exactly at the limit is kept"
        );
        let hashed = wire_name(&over, max);
        assert!(
            matches!(hashed, Cow::Owned(_)),
            "{max}: a name one byte over the limit is mapped"
        );
        assert_eq!(hashed.len(), max, "{max}: the mapped name fills the limit");
        assert_eq!(
            &hashed[..max - SUFFIX],
            "c".repeat(max - SUFFIX),
            "{max}: the kept prefix"
        );
        assert_eq!(hashed.as_bytes()[max - SUFFIX], b'_', "{max}");
        assert!(
            hashed[max - SUFFIX + 1..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "{max}: the suffix is lowercase hex: {hashed}"
        );
    }
}

#[test]
fn the_anthropic_oauth_prefix_never_pushes_a_mapped_name_past_the_wire_limit() {
    let over = "d".repeat(WIRE_NAME_MAX * 2);
    let wire = wire_name(&over, ANTHROPIC_OAUTH_NAME_MAX);
    assert_eq!(CLAUDE_TOOL_PREFIX.len() + wire.len(), WIRE_NAME_MAX);
}

#[test]
fn an_empty_name_maps_to_a_non_empty_hashed_wire_name_that_restores() -> TestResult {
    // SHA-256 of the empty string starts `e3b0c442`.
    assert_eq!(wire_name("", WIRE_NAME_MAX), "_e3b0c442");
    let request = request(&[""])?;
    let names = ToolNames::for_request(&request, WIRE_NAME_MAX)?;
    assert!(!names.is_empty());
    assert!(matches!(
        names.restore_event(StreamEvent::ToolCallStarted {
            id: String::from("call-1"),
            name: String::from("_e3b0c442"),
        }),
        StreamEvent::ToolCallStarted { name, .. } if name.is_empty()
    ));
    Ok(())
}

#[test]
fn distinct_internal_names_never_share_a_wire_name_and_each_restores_to_itself() -> TestResult {
    let long = "p".repeat(WIRE_NAME_MAX + 6);
    let internal_names = [
        // Differ only by the character that sanitising folds to `_`.
        String::from("a.b"),
        String::from("a b"),
        String::from("a/b"),
        String::from("a:b"),
        // The sanitised spellings are themselves valid names.
        String::from("a_b"),
        String::from("a-b"),
        String::new(),
        String::from("é"),
        String::from("è"),
        // Differ only after the cut that keeps the wire name within the limit.
        format!("{long}x"),
        format!("{long}y"),
        format!("{long}.1"),
        format!("{long}.2"),
        "z".repeat(WIRE_NAME_MAX),
        "z".repeat(WIRE_NAME_MAX + 1),
    ];
    let refs = internal_names
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let request = request(&refs)?;
    for max in [WIRE_NAME_MAX, ANTHROPIC_OAUTH_NAME_MAX] {
        let names = ToolNames::for_request(&request, max)?;
        let wires = internal_names
            .iter()
            .map(|internal| wire_name(internal, max).into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            wires.iter().collect::<BTreeSet<_>>().len(),
            internal_names.len(),
            "{max}: two names share a wire name: {wires:?}"
        );
        for (internal, wire) in internal_names.iter().zip(wires) {
            assert!(
                matches!(
                    names.restore_event(StreamEvent::ToolCallStarted {
                        id: String::from("call-1"),
                        name: wire,
                    }),
                    StreamEvent::ToolCallStarted { name, .. } if name == *internal
                ),
                "{max}: {internal:?} does not restore to itself"
            );
        }
    }
    Ok(())
}

#[test]
fn advertising_the_same_tool_twice_is_not_a_collision() -> TestResult {
    let internal = "deploy.web-x.list";
    let request = request(&[internal, "bash", internal])?;
    let names = ToolNames::for_request(&request, WIRE_NAME_MAX)?;
    assert!(matches!(
        names.restore_event(StreamEvent::ToolCallStarted {
            id: String::from("call-1"),
            name: wire_name(internal, WIRE_NAME_MAX).into_owned(),
        }),
        StreamEvent::ToolCallStarted { name, .. } if name == internal
    ));
    Ok(())
}

#[test]
fn restoring_rewrites_every_call_of_a_batch_and_only_advertised_wire_names() -> TestResult {
    let (deploy, search) = ("deploy.web-x.list", "mcp.search.run");
    let request = request(&[deploy, "bash", search])?;
    let names = ToolNames::for_request(&request, WIRE_NAME_MAX)?;
    let wire_deploy = wire_name(deploy, WIRE_NAME_MAX).into_owned();
    let wire_search = wire_name(search, WIRE_NAME_MAX).into_owned();
    // A wire-shaped name the request never advertised.
    let foreign = wire_name("other.server.tool", WIRE_NAME_MAX).into_owned();
    let args = ToolArgs::Parsed(RawJson::parse(r#"{"k":1}"#)?);
    let call = |id: &str, name: &str| ToolCall {
        id: id.into(),
        name: name.into(),
        args: args.clone(),
    };
    let sent = vec![
        call("c1", &wire_deploy),
        call("c2", "bash"),
        call("c3", &wire_search),
        call("c4", &foreign),
        call("c5", &wire_deploy),
    ];
    let StreamEvent::ToolCallsDone { calls } =
        names.restore_event(StreamEvent::ToolCallsDone { calls: sent })
    else {
        return Err("the batch event changed kind".into());
    };
    assert_eq!(
        calls,
        vec![
            call("c1", deploy),
            call("c2", "bash"),
            call("c3", search),
            call("c4", &foreign),
            call("c5", deploy),
        ]
    );
    // Assistant text that spells a wire name is text, never a tool name.
    assert!(matches!(
        names.restore_event(StreamEvent::TextDelta { text: wire_deploy.clone() }),
        StreamEvent::TextDelta { text } if text == wire_deploy
    ));
    Ok(())
}

#[test]
fn a_table_is_empty_only_when_every_advertised_name_is_already_valid() -> TestResult {
    let plain = ToolNames::for_request(&request(&["bash", "fs_read-2"])?, WIRE_NAME_MAX)?;
    assert!(plain.is_empty());
    let none = ToolNames::for_request(&request(&[])?, WIRE_NAME_MAX)?;
    assert!(none.is_empty());
    let mapped = ToolNames::for_request(&request(&["bash", "a.b"])?, WIRE_NAME_MAX)?;
    assert!(!mapped.is_empty());
    Ok(())
}
