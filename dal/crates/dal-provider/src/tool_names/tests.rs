use std::{error::Error, sync::Arc};

use dal_core::{
    Family, ModelRequest, ModelRoute, ModelToolSpec, Purpose, RawJson, RequestParams, ThinkingLevel,
};

use super::{ToolNames, wire_name};
use crate::stream::{StreamEvent, ToolArgs, ToolCall};

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
        })
        .collect::<Vec<_>>();
    Ok(ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Chat,
            model: "test-model".into(),
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
