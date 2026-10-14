//! Scripted synthetic model handlers.

use std::sync::Arc;

use dal_agent::ext::ModelError;
use dal_agent::ext::script::HostTerminal;
use dal_agent::ext::{BoxFuture, EventStream, ModelCx, ModelHandler};
use dal_core::{
    InferFailure, ModelRequest, ModelRoute, Stop, StreamChannel, StreamEvent as CoreEvent,
};
use dal_provider::{
    EventStream as ProviderStream, ProviderError, ReplayPayload, StopReason, StreamEvent, ToolArgs,
    ToolCall,
};

use crate::engine::CELL_WALL;
use crate::invoke::{self, Arg, Handler, InvokeFailure};
use crate::validate::LoadedPlugin;
use crate::value;

/// One Starlark model registered as a host model handler.
pub(crate) struct ScriptModelHandler {
    plugin: Arc<LoadedPlugin>,
    model: usize,
}

impl ScriptModelHandler {
    /// Builds the handler for `plugin.models[model]`.
    pub(crate) fn new(plugin: Arc<LoadedPlugin>, model: usize) -> Self {
        Self { plugin, model }
    }
}

impl ModelHandler for ScriptModelHandler {
    fn uses(&self) -> dal_core::ext::OpSet {
        self.plugin.models[self.model].uses.clone()
    }

    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let Some(script) = cx.script_cx() else {
                return Ok(failure_stream(fatal(
                    "script context is unavailable for this model",
                )));
            };
            let model = &self.plugin.models[self.model];
            let args = match encode_request(&request) {
                Ok(args) => args,
                Err(message) => return Ok(failure_stream(fatal(message))),
            };
            let handler = Handler {
                plugin: &self.plugin,
                id: &model.id,
                phase: dal_core::ext::Phase::Model,
                run: model.run,
                cap: CELL_WALL,
            };
            let output = match invoke::settle(
                invoke::enter(&script, handler, Arg::Data(args)).await,
                None,
                &model.id,
            ) {
                Ok(output) => output,
                Err(
                    InvokeFailure::Cancelled | InvokeFailure::Terminal(HostTerminal::Cancelled),
                ) => {
                    return Ok(failure_stream(InferFailure::Cancelled));
                }
                Err(failure) => return Ok(failure_stream(fatal(failure.to_string()))),
            };
            let (route, events) = match decode_forward(&output.value) {
                Ok(forward) => forward,
                Err(message) => return Ok(failure_stream(fatal(message))),
            };
            match provider_stream(events, &route) {
                Ok(stream) => Ok(stream),
                Err(failure) => Ok(failure_stream(failure)),
            }
        })
    }
}

/// The transport wrapper a scripted model returns from `ctx.models.infer` or
/// `ctx.models.forward`.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ModelForward {
    pub(crate) model: ModelRoute,
    pub(crate) events: Vec<CoreEvent>,
}

fn encode_request(request: &ModelRequest) -> Result<value::Value, Box<str>> {
    let json = sonic_rs::to_string(request).map_err(|error| error.to_string().into_boxed_str())?;
    value::Value::decode(&json).map_err(|error| error.to_string().into_boxed_str())
}

fn decode_forward(value: &value::Value) -> Result<(ModelRoute, Vec<CoreEvent>), Box<str>> {
    let value::Value::Object(fields) = value else {
        return Err("scripted model must return a model inference record".into());
    };
    for (name, _) in fields {
        if !matches!(name.as_ref(), "model" | "events") {
            return Err(format!("model inference has unknown field `{name}`").into());
        }
    }
    let forward: ModelForward = sonic_rs::from_str(&value.to_json())
        .map_err(|error| format!("invalid model inference: {error}").into_boxed_str())?;
    Ok((forward.model, forward.events))
}

fn provider_stream(
    events: Vec<CoreEvent>,
    model: &ModelRoute,
) -> Result<EventStream, InferFailure> {
    let mut output = Vec::with_capacity(events.len().saturating_add(2));
    let mut calls = Vec::new();
    let mut usage = None;
    let mut stop = None;
    for event in events {
        if stop.is_some() {
            return Err(fatal("model inference has events after its terminal stop"));
        }
        match event {
            CoreEvent::Delta {
                channel: StreamChannel::Text,
                text,
            } => output.push(Ok(StreamEvent::TextDelta {
                text: text.into_string(),
            })),
            CoreEvent::Delta {
                channel: StreamChannel::Thinking,
                text,
            } => output.push(Ok(StreamEvent::ReasoningDelta {
                text: text.into_string(),
            })),
            CoreEvent::Delta {
                channel: StreamChannel::ToolArgs { .. },
                ..
            } => {}
            CoreEvent::ToolCall { call, name, args } => {
                let id = call.to_string();
                let name = name.into_string();
                let argument_bytes = args.as_str().as_bytes().to_vec();
                output.push(Ok(StreamEvent::ToolCallStarted {
                    id: id.clone(),
                    name: name.clone(),
                }));
                output.push(Ok(StreamEvent::ToolArgsDelta {
                    id: id.clone(),
                    fragment: argument_bytes,
                }));
                calls.push(ToolCall {
                    id,
                    name,
                    args: ToolArgs::Parsed(args),
                });
            }
            CoreEvent::ThinkingReplay { payload } => {
                let ModelRoute::Api { family, model: id } = model else {
                    return Err(fatal("model replay data requires an API model route"));
                };
                output.push(Ok(StreamEvent::Replay {
                    payload: ReplayPayload {
                        family: *family,
                        model: id.clone(),
                        item: payload,
                    },
                }));
            }
            CoreEvent::Usage(value) => {
                if usage.replace(value).is_some() {
                    return Err(fatal("model inference contains multiple usage events"));
                }
            }
            CoreEvent::Stop(reason) => {
                if stop.replace(reason).is_some() {
                    return Err(fatal("model inference contains multiple stop events"));
                }
            }
            CoreEvent::Compaction { .. } => {
                return Err(fatal(
                    "scripted model compaction cannot be returned as a provider stream",
                ));
            }
        }
    }
    let usage = usage.ok_or_else(|| fatal("model inference is missing its usage event"))?;
    let stop = stop.ok_or_else(|| fatal("model inference is missing its stop event"))?;
    let has_calls = !calls.is_empty();
    output.push(Ok(StreamEvent::ToolCallsDone { calls }));
    output.push(Ok(StreamEvent::Usage { usage }));
    let reason = match stop {
        Stop::EndTurn if has_calls => StopReason::ToolUse,
        Stop::EndTurn => StopReason::EndTurn,
        Stop::Length => StopReason::MaxTokens,
        Stop::Filter => StopReason::Refusal,
        Stop::MaxSteps => return Err(fatal("scripted model reached its step limit")),
        Stop::Cancelled => return Err(InferFailure::Cancelled),
        Stop::Failed => return Err(fatal("scripted model returned a failed stop")),
    };
    output.push(Ok(StreamEvent::Stop { reason }));
    Ok(ProviderStream::new(futures::stream::iter(output), || {}))
}

fn fatal(message: impl Into<Box<str>>) -> InferFailure {
    InferFailure::Fatal {
        message: message.into(),
        fix: None,
    }
}

fn failure_stream(failure: InferFailure) -> EventStream {
    ProviderStream::new(
        futures::stream::iter([Err(ProviderError::Synthetic(failure))]),
        || {},
    )
}
