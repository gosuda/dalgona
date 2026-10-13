//! Harness execution for the HTTP router.
//!
//! Harness models (`dalgon/normal`, `dalgon/eval-first`, `dalgon/eval-only`)
//! run the dal harness with its own tools: client tools, tool choices, and
//! tool history are refused. Sessions continue by header, previous response
//! id, or history digest; anything else opens a new session at the serve
//! working directory. Open questions answer their core defaults at once.

use std::sync::Arc;

use dal_agent::Host;
use dal_core::{ApprovalMode, Command, Expect, PageReq, Part, Save, SessionId, ThinkingLevel};
use sonic_rs::Value;
use tokio::sync::Mutex;

use super::decode::{ChatRequest, MessagesRequest, ResponsesRequest, RouterFail};
use super::sink::EventSink;
use super::stream::TurnIds;

mod history;
mod pump;
mod session;

use super::{DigestTable, RouterOptions};
use crate::router::HarnessMode;
pub(crate) use history::data_url_image;
use history::{
    HistoryText, build_prompt, canon_items, chat_texts, messages_texts, reject_harness_tools,
    responses_texts,
};
use pump::drive_turn;
use session::{active_leaf, check_idle_turn, is_fresh_session, select_session};

/// Shared harness state for one listener.
pub(crate) struct HarnessShared {
    /// The serving host.
    pub host: Host,
    /// The router options captured at startup.
    pub options: RouterOptions,
    /// The history-digest continuation table.
    pub digests: Arc<Mutex<DigestTable>>,
    /// Fires when the listener stops.
    pub shutdown: tokio_util::sync::CancellationToken,
}

/// HTTP request parts needed for harness routing.
#[derive(Clone, Debug, Default)]
pub(crate) struct HttpParts {
    /// The `x-dal-session` header value, when present.
    pub session_header: Option<String>,
}

/// The outcome of one harness turn.
pub(crate) struct HarnessTurn {
    /// The accumulated assistant text.
    pub text: String,
    /// The accumulated reasoning text.
    pub reasoning: String,
    /// The summed usage rows.
    pub usage: UsageSum,
    /// The terminal stop.
    pub stop: HarnessStop,
    /// The prompt turn id.
    pub turn: dal_core::TurnId,
    /// The session id.
    pub session: SessionId,
}

/// One live turn event for stream encoding, from a harness turn or a relay.
#[derive(Clone, Debug)]
pub(crate) enum HarnessEvent {
    /// The turn started; stream encoders learn the wire ids.
    Started(TurnIds),
    /// Assistant text delta.
    Text(String),
    /// Reasoning delta.
    Reasoning(String),
    /// A client tool call began.
    ToolCallStarted {
        /// The provider call id.
        id: String,
        /// The tool name.
        name: String,
    },
    /// Argument text of a started tool call.
    ToolArgs {
        /// The provider call id.
        id: String,
        /// The next argument text slice.
        fragment: String,
    },
    /// A tool call completed with its full arguments.
    ToolCallDone {
        /// The provider call id.
        id: String,
        /// The tool name.
        name: String,
        /// The complete arguments JSON.
        args: String,
    },
    /// A provider item replayed verbatim to a same-family wire.
    Replay(Value),
    /// Updated usage totals.
    Usage(UsageSum),
    /// The terminal stop.
    Stop(HarnessStop),
}

/// Summed token usage for one turn.
#[derive(Clone, Debug, Default)]
pub(crate) struct UsageSum {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache read tokens.
    pub cache_read: u64,
    /// Cache write tokens.
    pub cache_write: u64,
    /// Context tokens.
    pub context_tokens: u64,
    /// Context window size.
    pub context_window: u64,
    /// Cost in USD, when reported.
    pub cost: Option<f64>,
}

/// A harness turn stop in wire literals.
#[derive(Clone, Debug)]
pub(crate) enum HarnessStop {
    /// The turn ended normally.
    EndTurn,
    /// The model hit a length limit.
    MaxTokens,
    /// The turn reached its request limit.
    MaxTurnRequests,
    /// The model refused.
    Refusal,
    /// The model requested client tool calls.
    ToolUse,
    /// The turn was cancelled.
    Cancelled,
    /// The turn failed with a message.
    Failed(String),
}

/// One decoded harness request of any router family.
pub(crate) enum HarnessRequest {
    /// A Chat Completions request.
    Chat(ChatRequest),
    /// A Responses request.
    Responses(ResponsesRequest),
    /// An Anthropic Messages request.
    Messages(MessagesRequest),
}

/// Runs one decoded request through the harness.
pub(crate) async fn run_request(
    shared: &HarnessShared,
    mode: HarnessMode,
    request: &HarnessRequest,
    http: &HttpParts,
    sink: &mut impl EventSink,
) -> Result<HarnessTurn, RouterFail> {
    match request {
        HarnessRequest::Chat(req) => run_chat(shared, mode, req, http, sink).await,
        HarnessRequest::Responses(req) => run_responses(shared, mode, req, http, sink).await,
        HarnessRequest::Messages(req) => run_messages(shared, mode, req, http, sink).await,
    }
}

/// Runs one chat request through the harness.
async fn run_chat(
    shared: &HarnessShared,
    mode: HarnessMode,
    req: &ChatRequest,
    http: &HttpParts,
    sink: &mut impl EventSink,
) -> Result<HarnessTurn, RouterFail> {
    reject_harness_tools(mode, &req.tools, &req.tool_choice, &req.messages)?;
    let level =
        super::decode::effort_level(req.reasoning_effort.as_deref(), ThinkingLevel::Medium)?;
    let texts = chat_texts(&req.messages, mode)?;
    run_harness(shared, mode, texts, None, level, http, sink).await
}

/// Runs one responses request through the harness.
async fn run_responses(
    shared: &HarnessShared,
    mode: HarnessMode,
    req: &ResponsesRequest,
    http: &HttpParts,
    sink: &mut impl EventSink,
) -> Result<HarnessTurn, RouterFail> {
    reject_harness_tools(mode, &req.tools, &req.tool_choice, &[])?;
    let level =
        super::decode::effort_level(req.reasoning_effort.as_deref(), ThinkingLevel::Medium)?;
    let texts = responses_texts(&req.input, &req.instructions, mode)?;
    run_harness(
        shared,
        mode,
        texts,
        req.previous_response_id.clone(),
        level,
        http,
        sink,
    )
    .await
}

/// Runs one messages request through the harness.
async fn run_messages(
    shared: &HarnessShared,
    mode: HarnessMode,
    req: &MessagesRequest,
    http: &HttpParts,
    sink: &mut impl EventSink,
) -> Result<HarnessTurn, RouterFail> {
    reject_harness_tools(mode, &req.tools, &req.tool_choice, &req.messages)?;
    let level = super::decode::budget_level(req.thinking_budget, ThinkingLevel::Medium);
    let texts = messages_texts(&req.messages, &req.system, mode)?;
    run_harness(shared, mode, texts, None, level, http, sink).await
}

/// Runs the shared harness turn pipeline for one normalized history.
async fn run_harness(
    shared: &HarnessShared,
    mode: HarnessMode,
    history: (Vec<HistoryText>, Vec<Part>),
    previous: Option<String>,
    level: ThinkingLevel,
    http: &HttpParts,
    sink: &mut impl EventSink,
) -> Result<HarnessTurn, RouterFail> {
    let (items, images) = history;
    let mut canon = canon_items(&items);
    let last = canon.pop();
    let (continued, agent) = select_session(shared, &canon, previous, http).await?;
    let head = agent
        .view(PageReq::default())
        .map_err(|_| RouterFail::busy("the session closed during the request".to_owned()))?;
    let fresh = is_fresh_session(&head);
    check_idle_turn(&head)?;
    let prompt = build_prompt(&items, fresh);
    let mut parts: Vec<Part> = prompt
        .into_iter()
        .map(|text| Part::Text { text: text.into() })
        .collect();
    parts.extend(images);
    agent
        .submit(Command::SetThinking {
            level,
            save: Save::SessionOnly,
        })
        .await
        .map_err(|_| RouterFail::busy("the session closed during the request".to_owned()))?;
    if fresh {
        let run = Command::Run {
            name: "mode".into(),
            args: mode.mode_arg().into(),
            expected: None,
        };
        agent.submit(run).await?;
        let approval = strictest(head.settings.approval, shared.options.approval);
        agent
            .submit(Command::SetApproval {
                mode: approval,
                save: Save::SessionOnly,
            })
            .await?;
    }
    let pre_leaf = active_leaf(&head);
    let subscription = agent.subscribe(Some((head.r#gen, head.seq)))?;
    let command = Command::Prompt {
        expect: Expect::Idle,
        content: parts,
    };
    let accepted = agent.submit(command).await.map_err(|error| match error {
        dal_agent::AgentError::WrongTurn {
            actual: dal_agent::error::ActualTurn::Turn { turn, .. },
            ..
        } => RouterFail::busy(format!(
            "session {} is running turn {turn}: wait for it to end",
            head.session.id
        )),
        other => RouterFail::from(other),
    })?;
    let dal_core::Reply::Accepted { turn, .. } = accepted else {
        return Err(RouterFail::bad(
            "internal",
            "the prompt did not start a turn".to_owned(),
        ));
    };
    let session = continued;
    let outcome = drive_turn(
        shared,
        &agent,
        (session, subscription),
        turn,
        pre_leaf,
        sink,
    )
    .await?;
    if !matches!(outcome.stop, HarnessStop::Failed(_)) {
        let mut full = canon;
        full.extend(last);
        full.push(sonic_rs::json!({"role": "assistant", "text": outcome.text}));
        let digest = crate::router::digest_history(&full);
        shared
            .digests
            .lock()
            .await
            .insert(digest, session.to_string());
    }
    Ok(outcome)
}

/// Returns the stricter of two approval modes.
pub(crate) fn strictest(current: ApprovalMode, serve: ApprovalMode) -> ApprovalMode {
    let rank = |mode: ApprovalMode| match mode {
        ApprovalMode::Ask => 0,
        ApprovalMode::Edits => 1,
        ApprovalMode::All => 2,
    };
    if rank(current) <= rank(serve) {
        current
    } else {
        serve
    }
}

impl From<dal_agent::AgentError> for RouterFail {
    fn from(error: dal_agent::AgentError) -> Self {
        Self::bad("internal", error.to_string())
    }
}
