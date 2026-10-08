//! The typed-service dispatch for scripted native operations (R03 R04).
//!
//! The catalog advertises the operations [`wired`] names because a host
//! service owns each backend. [`call`] decodes the request's raw arguments
//! into the operation's typed wire shape, invokes the service under the
//! caller's declared inject set and grants, and encodes the reply as the
//! operation's raw JSON value. Operations with no landed owner — `state.*` —
//! stay absent and keep the `unavailable` outcome.

use std::sync::Arc;
use std::time::Duration;

use dal_core::ext::NativeOp;
use dal_core::{
    AgentStart, AgentsOp, Answer, CallId, Choice, FetchRequest, JobId, JobsOp, McpRequest, Name,
    Preview, Question, RawJson, SessionId, TurnOp, Workspace,
};
use serde::Deserialize;

use crate::error::ServiceError;
use crate::ext::{Caller, Services};

/// How a service operation failed: strict argument decode, or the service.
pub(super) enum CallError {
    /// The arguments failed the operation's declared shape.
    Args(Box<str>),
    /// The typed service returned its own error.
    Service(ServiceError),
}

/// Reports whether `op` dispatches through the typed service seam.
///
/// The table is exhaustive on purpose: every native operation must name its
/// dispatch path — the tool runtime, a service call, or `unavailable`. A new
/// operation without a backend fails closed here.
pub(super) fn wired(op: NativeOp) -> bool {
    match op {
        NativeOp::EnvRead
        | NativeOp::NetFetch
        | NativeOp::AskConfirm
        | NativeOp::AskSelect
        | NativeOp::AskText
        | NativeOp::AgentsStart
        | NativeOp::AgentsWait
        | NativeOp::AgentsCancel
        | NativeOp::AgentsList
        | NativeOp::JobsStart
        | NativeOp::JobsWait
        | NativeOp::JobsCancel
        | NativeOp::JobsList
        | NativeOp::JobsText
        | NativeOp::TurnCancel
        | NativeOp::TurnSteer
        | NativeOp::TurnWake
        | NativeOp::TurnIsIdle
        | NativeOp::McpCall => true,
        NativeOp::ToolsRead
        | NativeOp::ToolsSearch
        | NativeOp::ToolsPatch
        | NativeOp::ToolsExec
        | NativeOp::ModelsInfer
        | NativeOp::ModelsForward
        | NativeOp::StateRead
        | NativeOp::StateWrite
        | NativeOp::StateDelete => false,
    }
}

/// Runs `op` through the session services.
///
/// `who` is the caller minted for the invocation; `session` and `call` fill
/// the host-owned request fields the script never supplies.
///
/// # Errors
///
/// Returns [`CallError::Args`] when the arguments fail the operation's
/// strict shape and [`CallError::Service`] for the service's own inject,
/// grant, availability, or backend failure.
#[expect(
    clippy::too_many_lines,
    reason = "one match arm per wired operation keeps the wire table readable"
)]
pub(super) async fn call(
    services: &Arc<dyn Services>,
    who: &Caller,
    session: SessionId,
    call: &CallId,
    op: NativeOp,
    args: &RawJson,
) -> Result<RawJson, CallError> {
    match op {
        NativeOp::EnvRead => {
            let args: KeyArgs = decode(args)?;
            encode(
                &services
                    .env(who, &args.key)
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::NetFetch => {
            let request: FetchRequest = decode(args)?;
            encode(
                &services
                    .net(who, request)
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::AskConfirm => {
            let args: AskConfirmArgs = decode(args)?;
            answer(
                services
                    .ask(who, Question::Confirm { text: args.prompt })
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::AskSelect => {
            let args: AskSelectArgs = decode(args)?;
            answer(
                services
                    .ask(
                        who,
                        Question::Select {
                            prompt: args.prompt,
                            options: args.options,
                            multi: args.multi,
                            preview: args.preview,
                        },
                    )
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::AskText => {
            let args: AskTextArgs = decode(args)?;
            answer(
                services
                    .ask(
                        who,
                        Question::Text {
                            prompt: args.prompt,
                            placeholder: args.placeholder,
                        },
                    )
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::AgentsStart => {
            let args: AgentStartArgs = decode(args)?;
            let start = AgentStart {
                call: call.clone(),
                name: args.name.unwrap_or_else(|| who.ext().as_str().into()),
                prompt: args.prompt,
                model: args.model,
                role: args.role,
                system: args.system,
                tools: args.tools.map(Vec::into_boxed_slice),
                workspace: args.workspace,
            };
            start
                .validate()
                .map_err(|error| CallError::Args(error.to_string().into()))?;
            encode(
                &services
                    .agents(who, AgentsOp::Start(start))
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::AgentsWait => {
            let args: IdTimeout<SessionId> = decode(args)?;
            encode(
                &services
                    .agents(
                        who,
                        AgentsOp::Await {
                            id: args.id,
                            timeout: args.timeout,
                        },
                    )
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::AgentsCancel => {
            let args: Id<SessionId> = decode(args)?;
            encode(
                &services
                    .agents(who, AgentsOp::Cancel { id: args.id })
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::AgentsList => {
            decode::<Empty>(args)?;
            encode(
                &services
                    .agents(who, AgentsOp::List)
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::JobsStart => {
            let args: SpawnJobArgs = decode(args)?;
            encode(
                &services
                    .jobs(
                        who,
                        JobsOp::Spawn {
                            name: args.name,
                            payload: args.payload,
                            parent: args.parent,
                        },
                    )
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::JobsWait => {
            let args: IdTimeout<JobId> = decode(args)?;
            encode(
                &services
                    .jobs(
                        who,
                        JobsOp::Wait {
                            id: args.id,
                            timeout: args.timeout,
                        },
                    )
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::JobsCancel => {
            let args: Id<JobId> = decode(args)?;
            encode(
                &services
                    .jobs(who, JobsOp::Cancel { id: args.id })
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::JobsList => {
            decode::<Empty>(args)?;
            encode(
                &services
                    .jobs(who, JobsOp::List)
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::JobsText => {
            let args: Id<JobId> = decode(args)?;
            encode(
                &services
                    .jobs(who, JobsOp::Text { id: args.id })
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::TurnCancel => {
            decode::<Empty>(args)?;
            encode(
                &services
                    .turn(who, TurnOp::Cancel)
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::TurnSteer => {
            let args: TextArgs = decode(args)?;
            encode(
                &services
                    .turn(who, TurnOp::Steer { text: args.text })
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::TurnWake => {
            let args: WakeArgs = decode(args)?;
            encode(
                &services
                    .turn(
                        who,
                        TurnOp::Wake {
                            text: args.text,
                            sources: args.sources,
                            job_ids: args.job_ids,
                        },
                    )
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::TurnIsIdle => {
            decode::<Empty>(args)?;
            encode(
                &services
                    .turn(who, TurnOp::IsIdle)
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        NativeOp::McpCall => {
            let args: McpCallArgs = decode(args)?;
            encode(
                &services
                    .mcp(
                        who,
                        McpRequest {
                            session,
                            server: args.server,
                            tool: args.tool,
                            arguments: args.arguments,
                        },
                    )
                    .await
                    .map_err(CallError::Service)?,
            )
        }
        other => Err(CallError::Args(
            format!("operation {other} has no service dispatch")
                .as_str()
                .into(),
        )),
    }
}

/// Decodes the operation arguments under their strict declared shape.
fn decode<T: serde::de::DeserializeOwned>(args: &RawJson) -> Result<T, CallError> {
    args.decode_as::<T>()
        .map_err(|error| CallError::Args(error.to_string().into()))
}

/// Encodes a typed reply as the operation's raw JSON value.
fn encode<T: serde::Serialize>(value: &T) -> Result<RawJson, CallError> {
    let text = sonic_rs::to_string(value)
        .map_err(|error| CallError::Service(ServiceError::failed(None, error.to_string())))?;
    RawJson::parse(&text)
        .map_err(|error| CallError::Service(ServiceError::failed(None, error.to_string())))
}

/// Projects an `ask` answer: a value answer keeps its raw payload, any other
/// answer keeps its tagged wire shape, and an unanswered question is `null`.
fn answer(answer: Option<Answer>) -> Result<RawJson, CallError> {
    match answer {
        Some(Answer::Value(raw)) => Ok(raw),
        other => encode(&other),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    key: Box<str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextArgs {
    text: Box<str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id<I> {
    id: I,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdTimeout<I> {
    id: I,
    #[serde(default)]
    timeout: Option<Duration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskConfirmArgs {
    prompt: Box<str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskSelectArgs {
    prompt: Box<str>,
    options: Vec<Choice>,
    #[serde(default)]
    multi: bool,
    #[serde(default)]
    preview: Option<Preview>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskTextArgs {
    prompt: Box<str>,
    #[serde(default)]
    placeholder: Option<Box<str>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStartArgs {
    prompt: Box<str>,
    #[serde(default)]
    name: Option<Box<str>>,
    #[serde(default)]
    model: Option<Box<str>>,
    #[serde(default)]
    role: Option<Box<str>>,
    #[serde(default)]
    system: Option<Box<str>>,
    #[serde(default)]
    tools: Option<Vec<Name>>,
    #[serde(default)]
    workspace: Option<Workspace>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnJobArgs {
    name: Name,
    payload: RawJson,
    #[serde(default)]
    parent: Option<JobId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WakeArgs {
    text: Box<str>,
    #[serde(default)]
    sources: Vec<Box<str>>,
    #[serde(default)]
    job_ids: Vec<JobId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpCallArgs {
    server: Box<str>,
    tool: Box<str>,
    arguments: RawJson,
}
