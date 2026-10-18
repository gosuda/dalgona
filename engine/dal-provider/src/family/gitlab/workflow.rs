//! The Duo Agent workflow surface: REST setup plus a stateful workflow socket.
//!
//! A turn runs a server-side workflow: namespace settings, project scope,
//! `direct_access`, workflow create, then a WebSocket carrying a `START`
//! frame whose goal is the flat conversation transcript. The server streams
//! checkpoint snapshots, tool actions, approvals, and a terminal status.
//! Ground truth is the pinned `gitlab-duo-workflow.ts` at `a4dc3302`; every
//! constant below names its source.
//!
//! The socket outlives one turn when the server asks for a tool result: the
//! turn ends `ToolUse`, the action waits in [`WorkflowSessions`], and the
//! next turn answers with `actionResponse` on the same socket. Every other
//! outcome drops the session and stops the workflow server-side, so one
//! terminal event always owns the run and nothing strands.
//!
//! # Reconnection
//!
//! A silent socket (no frame for 90 s) aborts and restarts once on a fresh
//! workflow: same-id reconnect is not recoverable on the inline flow. A
//! step-limit `FAILED` restarts fresh up to four times, the generic
//! processing-error `FAILED` once, and a stalled workflow (a tool boundary
//! whose checkpoint bytes did not advance) twice. An approval status resends
//! an approval `START` on a new socket for the same workflow.
//!
//! # Tool exchange
//!
//! A `runMCPTool` action carries a non-empty `requestID` the client must echo
//! verbatim; a synthesized id is never correct and fails as a protocol
//! error. The action becomes one dal tool call and executes only through dal
//! approval. Tool results return as `plainTextResponse`. A user message that
//! lands after a pending tool result steers: the stale workflow is stopped
//! and a fresh one replays the whole transcript.

use dal_core::{AssistantPart, ContextItem, Family, ModelRequest, Part, RawJson, SessionId, Usage};
use futures::{StreamExt, stream};
use serde::Serialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait as _};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write as _,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        error::Error as WsError,
        http::{HeaderName, HeaderValue, StatusCode},
        protocol::WebSocketConfig,
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    EventStream,
    error::{LimitError, ProviderError},
    family::codex::{redact, redact_provider_error},
    http::{self, Exchange},
    lifecycle::AttemptFailure,
    stream::{StreamEvent, ToolArgs, ToolCall},
    stream_budget::StreamBudget,
    tool_names::{WIRE_NAME_MAX, wire_name},
};

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod support;

/// The workflow definition every dal run uses.
const DEFINITION: &str = "ambient";

/// The `clientVersion` member of every `START` frame.
const CLIENT_VERSION: &str = "1.0";

/// The client type the socket declares.
const CLIENT_TYPE: &str = "node-websocket";

/// The language-server version the socket declares, also the user-agent.
const LANGUAGE_SERVER_VERSION: &str = "8.104.0";

/// Capabilities the client advertises in `START`.
const CAPABILITIES: [&str; 5] = [
    "incremental_streaming",
    "read_file_chunked",
    "shell_command",
    "command_timeout",
    "tool_call_approval",
];

/// The inline flow agent and prompt names; the server builds the flow from
/// this struct, and the names only link the payload's own members.
const AGENT_NAME: &str = "dalgon_agent";
const PROMPT_ID: &str = "dalgon_inline_prompt";

/// The reasoning event the inline flow must opt into for pre-tool-call
/// commentary to stream back as reasoning.
const UI_LOG_EVENTS: [&str; 4] = [
    "on_agent_reasoning",
    "on_agent_final_answer",
    "on_tool_execution_success",
    "on_tool_execution_failed",
];

/// Action names the server dispatches tool calls under.
const ACTION_NAMES: [&str; 2] = ["runMCPTool", "run_mcp_tool"];

/// Approval statuses: the run answers with an approval `START`.
const APPROVAL_STATUSES: [&str; 2] = ["PLAN_APPROVAL_REQUIRED", "TOOL_CALL_APPROVAL_REQUIRED"];

/// Completion statuses: the single terminal success outcome.
const COMPLETION_STATUSES: [&str; 2] = ["INPUT_REQUIRED", "FINISHED"];

/// The stable clause of the server's step-limit `FAILED`.
const STEP_LIMIT_CLAUSE: &str = "reached its maximum step limit";

/// The stable clause of the server's catch-all transient `FAILED`.
const GENERIC_ERROR_CLAUSE: &str = "error processing your request in the duo agent platform";

/// Bounded restarts: twelve sockets, one idle restart, four step-limit
/// restarts, one generic-error retry, two stall restarts.
const MAX_SOCKETS: u32 = 12;
const MAX_STEP_LIMIT_RESTARTS: u32 = 4;
const MAX_GENERIC_ERROR_RETRIES: u32 = 1;
const MAX_STALL_RESTARTS: u32 = 2;

/// The rendered-goal byte zones: below soft sends normally, soft to hard
/// attempts once and relabels a later error as overflow, hard and above
/// fails before spending the request.
const GOAL_SOFT_BYTES: usize = 1_048_576;
const GOAL_HARD_BYTES: usize = 2_000_000;

/// The stall message surfaced when every fresh restart also stalls.
const STALL_MESSAGE: &str = "GitLab Duo Agent stopped making progress (the workflow's visible history did not advance after multiple restarts).";

/// The history note appended to the system prompt of a multi-turn goal: the
/// transcript's markers are a historical record, not a syntax to emit.
/// Paraphrase of the pinned note; transcribe its exact bytes at the join.
const HISTORY_NOTE: &str = "The goal below renders prior turns with <|im_start|>, <|im_end|>, and <ran> markers. Those markers are a historical record of calls that already ran; do not emit them.";

const CHATML_START: &str = "<|im_start|>";
const CHATML_END: &str = "<|im_end|>";

/// Per-hop deadlines. Rationale: the socket has no server keepalive, so the
/// idle deadline bounds a half-open wait; the REST deadline bounds setup.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timeouts {
    /// One REST setup fetch.
    pub(crate) rest: Duration,
    /// One socket frame.
    pub(crate) idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            rest: Duration::from_secs(30),
            idle: Duration::from_secs(90),
        }
    }
}

/// The namespace and project scope of one run. Namespace discovery from the
/// workspace (git remote inspection) needs process spawn, which the provider
/// layer forbids, so the caller supplies the scope from configuration.
#[derive(Clone, Debug)]
pub(crate) struct WorkflowScope {
    /// The root namespace id, digits or `gid://gitlab/Group/<digits>`.
    pub(crate) root_namespace_id: String,
    /// The namespace path, for workflow creation.
    pub(crate) namespace_path: Option<String>,
    /// The numeric project id, for socket routing.
    pub(crate) project_id: Option<String>,
    /// The full project path, resolved to a numeric id when needed.
    pub(crate) project_path: Option<String>,
    /// The workflow definition; defaults to [`DEFINITION`].
    pub(crate) workflow_definition: Option<String>,
}

/// Everything one workflow turn needs.
///
/// It has no `Debug` implementation because it carries the token.
#[derive(Clone, Copy)]
pub(crate) struct WorkflowTurn<'a> {
    /// The neutral request; its route is `Family::GitLab`.
    pub(crate) request: &'a ModelRequest,
    /// The route model id, for error mapping and usage binding.
    pub(crate) model: &'a str,
    /// The GitLab host.
    pub(crate) base_url: &'a str,
    /// The GitLab token, for the REST setup calls.
    pub(crate) token: &'a str,
    /// The provider id owning the session row.
    pub(crate) provider: &'a str,
    /// The dal session owning a resumable socket.
    pub(crate) session: SessionId,
    /// The namespace and project scope.
    pub(crate) scope: &'a WorkflowScope,
    /// A `none` tool choice drops the advertised tools from `START`.
    pub(crate) tool_choice_none: bool,
    /// The per-hop deadlines.
    pub(crate) timeouts: Timeouts,
    /// Cancels the run; teardown still stops the workflow.
    pub(crate) cancel: &'a CancellationToken,
    /// The already-rendered dalgon user-agent.
    pub(crate) user_agent: &'a str,
}

type SessionMap = HashMap<(SessionId, Box<str>), Session>;

/// A live workflow socket table, keyed by session and provider. A turn takes
/// its row, drives it, and returns it only while a tool action is pending.
/// The handle clones so a turn's stream can own it.
#[derive(Clone, Debug, Default)]
pub(crate) struct WorkflowSessions {
    inner: Arc<Mutex<SessionMap>>,
}

impl WorkflowSessions {
    /// Drops every parked workflow row belonging to `session`, closing its
    /// sockets. Parked sockets are solely owned by their rows (a live turn
    /// took its row out of the table), so removing a row drops its transport
    /// and the peer observes the close; nothing is detached while a writer
    /// still holds it. This mirrors `WsSessions::end_session`.
    ///
    /// No workflow stop is sent: stopping is an async REST call and this runs
    /// in the synchronous `ProviderSet::end_session` path. A turn still
    /// driving the session keeps its own socket and stops its workflow
    /// through its driver teardown, and the next turn for the ended session
    /// starts fresh. Rows of other sessions are untouched.
    pub(crate) fn end_session(&self, session: &SessionId) {
        lock(&self.inner).retain(|(owner, _), _| owner != session);
    }
}

struct Session {
    socket: Option<Socket>,
    workflow_id: Box<str>,
    pending: Option<PendingAction>,
    checkpoint: CheckpointState,
    /// The checkpoint byte length of the previous tool-call boundary, when a
    /// checkpoint has been seen: stall detection compares against it. `None`
    /// until a comparable prior reading exists, so a lone boundary is never
    /// flagged as a stall.
    last_action_len: Option<usize>,
    connection: Connection,
    start: StartParts,
    selected: String,
    project_id: Option<String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The socket, connection, and START parts can retain credentials and
        // complete request text, so they never participate in `Debug` output.
        f.debug_struct("Session")
            .field("workflow_id", &self.workflow_id)
            .field("pending", &self.pending)
            .field("checkpoint", &self.checkpoint)
            .field("last_action_len", &self.last_action_len)
            .field("socket_present", &self.socket.is_some())
            .field("selected", &self.selected)
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// One server tool action awaiting its result.
#[derive(Clone, Debug)]
struct PendingAction {
    request_id: String,
}

/// The checkpoint dedupe state: full snapshots replay earlier entries, so a
/// later frame only emits what actually advanced.
#[derive(Debug, Default)]
struct CheckpointState {
    by_key: HashMap<String, String>,
    signatures: HashSet<String>,
}

// --- REST setup ---------------------------------------------------------

/// One workflow connection: the token, the socket base, and extra headers.
/// It has no `Debug` implementation; the token never formats.
#[derive(Clone)]
struct Connection {
    token: String,
    base_url: String,
    headers: Vec<(String, String)>,
    service_endpoint: bool,
}

/// The GraphQL namespace form of a root id.
fn graphql_namespace_id(root: &str) -> String {
    if root.chars().all(|c| c.is_ascii_digit()) && !root.is_empty() {
        return format!("gid://gitlab/Group/{root}");
    }
    String::from(root)
}

/// The REST numeric form of a root id.
fn rest_namespace_id(root: &str) -> String {
    root.strip_prefix("gid://gitlab/Group/")
        .or_else(|| root.strip_prefix("gid://gitlab/Namespace/"))
        .map_or_else(|| String::from(root), String::from)
}

/// One JSON REST call with the workflow timeout.
async fn rest_json(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    method: reqwest::Method,
    url: String,
    body: Option<Vec<u8>>,
) -> Result<(u16, Vec<u8>), ProviderError> {
    let mut request = client
        .request(method, url)
        .header("authorization", format!("Bearer {}", turn.token));
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = match body {
        Some(body) => request.body(body),
        None => request,
    };
    let response = http::send(
        Family::GitLab,
        request,
        turn.user_agent,
        Exchange::Json {
            total: turn.timeouts.rest,
        },
        tokio::time::sleep,
    )
    .await?;
    let status = response.status().as_u16();
    let bytes = http::read_body(Family::GitLab, response).await?;
    Ok((status, bytes))
}

/// The response text of a rejected setup call, with its status.
fn setup_status(what: &str, status: u16, body: &[u8]) -> ProviderError {
    let detail = String::from_utf8_lossy(body);
    ProviderError::Status {
        family: Family::GitLab,
        status,
        message: format!("GitLab Duo Workflow {what} failed with HTTP {status}: {detail}"),
    }
}

/// Best-effort enable of the namespace flags the inline flow needs. Returns
/// whether the attempt was definitive: any HTTP response is, a network error
/// is not, so a later turn retries instead of skipping forever.
async fn ensure_settings(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    rest_namespace: &str,
) -> bool {
    let url = format!(
        "{}/api/v4/groups/{}",
        turn.base_url.trim_end_matches('/'),
        rest_namespace
    );
    let body = br#"{"experiment_features_enabled":true,"ai_settings_attributes":{"duo_agent_platform_enabled":true,"duo_workflow_mcp_enabled":true}}"#.to_vec();
    match rest_json(client, turn, reqwest::Method::PUT, url, Some(body)).await {
        Ok((status, _)) => status < 500,
        Err(_) => false,
    }
}

/// Discovers one project the credential can access: the newest active group
/// project, else any membership project.
async fn discover_project(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    rest_namespace: &str,
) -> Option<(String, String)> {
    let query = "per_page=1&min_access_level=30&order_by=last_activity_at&sort=desc";
    let endpoints = [
        format!(
            "{}/api/v4/groups/{rest_namespace}/projects?include_subgroups=true&{query}",
            turn.base_url.trim_end_matches('/')
        ),
        format!(
            "{}/api/v4/projects?membership=true&{query}",
            turn.base_url.trim_end_matches('/')
        ),
    ];
    for url in endpoints {
        let Ok((status, body)) = rest_json(client, turn, reqwest::Method::GET, url, None).await
        else {
            continue;
        };
        if status != 200 {
            continue;
        }
        if let Some(found) = first_project(&body) {
            return Some(found);
        }
    }
    None
}

/// One id member as text: the API serves numbers, the tests strings.
fn id_text(value: &sonic_rs::Value) -> Option<String> {
    use sonic_rs::JsonValueTrait;
    value
        .as_str()
        .map(String::from)
        .or_else(|| value.as_u64().map(|id| id.to_string()))
}

fn first_project(body: &[u8]) -> Option<(String, String)> {
    use sonic_rs::JsonValueTrait;
    let value: sonic_rs::Value = sonic_rs::from_slice(body).ok()?;
    let first = value.as_array()?.first()?;
    let id = first.get("id").and_then(id_text)?;
    let path = first
        .get("path_with_namespace")?
        .as_str()
        .map(String::from)?;
    Some((id, path))
}

/// Resolves a project path to its numeric id for socket routing.
async fn numeric_project_id(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    path: &str,
) -> Option<String> {
    let url = format!(
        "{}/api/v4/projects/{}",
        turn.base_url.trim_end_matches('/'),
        encode_path(path)
    );
    let (status, body) = rest_json(client, turn, reqwest::Method::GET, url, None)
        .await
        .ok()?;
    if status != 200 {
        return None;
    }
    let value: sonic_rs::Value = sonic_rs::from_slice(&body).ok()?;
    value.get("id").and_then(id_text)
}

/// Percent-encodes a path the way `encodeURIComponent` does.
fn encode_path(path: &str) -> String {
    const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if UNRESERVED.contains(&byte) {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The workflow `direct_access` grant.
async fn workflow_direct_access(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    root_graphql: &str,
    project_id: Option<&str>,
    definition: &str,
) -> Result<Connection, ProviderError> {
    let mut body =
        format!(r#"{{"workflow_definition":"{definition}","root_namespace_id":"{root_graphql}""#);
    if let Some(project) = project_id {
        let _ = write!(body, r#","project_id":"{project}""#);
    }
    body.push('}');
    let url = format!(
        "{}/api/v4/ai/duo_workflows/direct_access",
        turn.base_url.trim_end_matches('/')
    );
    let (status, bytes) = rest_json(
        client,
        turn,
        reqwest::Method::POST,
        url,
        Some(body.into_bytes()),
    )
    .await?;
    if status != 200 {
        return Err(setup_status("direct_access", status, &bytes));
    }
    parse_connection(&bytes)
}

/// Extracts the workflow token by the pinned precedence, plus the service
/// endpoint override.
fn parse_connection(body: &[u8]) -> Result<Connection, ProviderError> {
    use sonic_rs::JsonValueTrait;
    let value: sonic_rs::Value =
        sonic_rs::from_slice(body).map_err(|_| invalid("workflow direct_access is not JSON"))?;
    let token = value
        .get("gitlab_rails")
        .and_then(|rails| rails.get("token"))
        .and_then(|token| token.as_str())
        .or_else(|| {
            value
                .get("duo_workflow_service")
                .and_then(|service| service.get("token"))
                .and_then(|token| token.as_str())
        })
        .or_else(|| {
            value
                .get("duo_workflow_access_token")
                .and_then(|token| token.as_str())
        })
        .or_else(|| value.get("workflow_token").and_then(|token| token.as_str()))
        .or_else(|| value.get("token").and_then(|token| token.as_str()))
        .or_else(|| value.get("access_token").and_then(|token| token.as_str()))
        .or_else(|| value.get("jwt").and_then(|token| token.as_str()))
        .filter(|token| !token.is_empty())
        .ok_or_else(|| invalid("workflow direct_access returned no credentials"))?;
    let service_base = value
        .get("duo_workflow_service")
        .and_then(|service| service.get("base_url"))
        .and_then(|url| url.as_str())
        .filter(|url| !url.is_empty());
    let rails_token = value
        .get("gitlab_rails")
        .and_then(|rails| rails.get("token"))
        .and_then(|token| token.as_str())
        .is_some();
    let service_endpoint = !rails_token && service_base.is_some();
    let headers = value
        .get("duo_workflow_service")
        .and_then(|service| service.get("headers"))
        .and_then(|headers| headers.as_object())
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .as_str()
                        .map(|value| (name.to_owned(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Connection {
        token: token.to_owned(),
        base_url: service_base.map_or_else(String::new, str::to_owned),
        headers,
        service_endpoint,
    })
}

/// Creates one workflow and returns its id.
async fn create_workflow(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    namespace: &str,
    project: Option<&str>,
    definition: &str,
) -> Result<String, ProviderError> {
    let scope = match (namespace.is_empty(), project) {
        (false, _) if project.is_none() => format!(r#","namespace_id":"{namespace}""#),
        (_, Some(project)) => format!(r#","project_id":"{project}""#),
        _ => String::new(),
    };
    let body = format!(
        r#"{{"workflow_definition":"{definition}","environment":"ide","allow_agent_to_request_user":false,"agent_privileges":[6],"pre_approved_agent_privileges":[6],"requires_duo_cli_enabled":false{scope},"goal":""}}"#
    );
    let url = format!(
        "{}/api/v4/ai/duo_workflows/workflows",
        turn.base_url.trim_end_matches('/')
    );
    let (status, bytes) = rest_json(
        client,
        turn,
        reqwest::Method::POST,
        url,
        Some(body.into_bytes()),
    )
    .await?;
    if status != 200 && status != 201 {
        return Err(setup_status("create", status, &bytes));
    }
    let value: sonic_rs::Value =
        sonic_rs::from_slice(&bytes).map_err(|_| invalid("workflow create is not JSON"))?;
    value
        .get("id")
        .or_else(|| value.get("workflow_id"))
        .or_else(|| value.get("workflowId"))
        .and_then(id_text)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| invalid("workflow create returned no id"))
}

/// Stops one workflow; best-effort, with a fresh timeout decoupled from the
/// caller's cancellation so an aborted run still fires its stop.
async fn stop_workflow(client: &reqwest::Client, turn: &WorkflowTurn<'_>, workflow_id: &str) {
    let url = format!(
        "{}/api/v4/ai/duo_workflows/workflows/{}",
        turn.base_url.trim_end_matches('/'),
        workflow_id
    );
    let body = br#"{"status_event":"stop"}"#.to_vec();
    let _ = rest_json(client, turn, reqwest::Method::PATCH, url, Some(body)).await;
}

/// The available-models GraphQL answer: the pinned model takes precedence,
/// else the requested model id rides the socket query.
async fn selected_model(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    root_graphql: &str,
) -> String {
    let query = r"query omp_gitlabDuoWorkflowAvailableModels($rootNamespaceId: GroupID!) { aiChatAvailableModels(rootNamespaceId: $rootNamespaceId) { defaultModel { name ref } selectableModels { name ref } pinnedModel { name ref } } }";
    let body = format!(
        r#"{{"query":{query},"variables":{{"rootNamespaceId":{root}}}}}"#,
        query = quoted(query),
        root = quoted(root_graphql)
    );
    let url = format!("{}/api/graphql", turn.base_url.trim_end_matches('/'));
    let models = rest_json(
        client,
        turn,
        reqwest::Method::POST,
        url,
        Some(body.into_bytes()),
    )
    .await
    .ok()
    .filter(|(status, _)| *status == 200)
    .and_then(|(_, bytes)| {
        use sonic_rs::JsonValueTrait;
        let value: sonic_rs::Value = sonic_rs::from_slice(&bytes).ok()?;
        value
            .get("data")?
            .get("aiChatAvailableModels")?
            .get("pinnedModel")?
            .get("ref")?
            .as_str()
            .map(String::from)
    });
    models.unwrap_or_else(|| String::from(turn.model))
}

fn quoted(text: &str) -> String {
    sonic_rs::to_string(text).unwrap_or_default()
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

// --- Goal rendering -----------------------------------------------------

/// The rendered goal and whether it is a multi-turn transcript. A lone turn
/// rides bare; a real session becomes the flat `ChatML` transcript, ending on
/// the last turn with nothing elevated.
fn render_goal(
    request: &ModelRequest,
    secrets: &[Box<str>],
) -> Result<(String, bool), ProviderError> {
    let mut history: Vec<ReplayTurn> = Vec::new();
    for item in request.context.iter() {
        match item {
            ContextItem::User { parts } => {
                let content = user_text(parts)?;
                if !content.is_empty() {
                    history.push(ReplayTurn::user(content));
                }
            }
            ContextItem::Assistant { parts, .. } => {
                if let Some(turn) = assistant_turn(parts) {
                    history.push(turn);
                }
            }
            ContextItem::ToolResult {
                name,
                parts,
                is_error,
                ..
            } => {
                let content = user_text(parts)?;
                history.push(ReplayTurn::tool(content, name, *is_error));
            }
        }
    }
    if history.len() <= 1 {
        let goal = history.pop().map_or_else(String::new, |turn| turn.content);
        return Ok((redact(&goal, secrets).into_owned(), false));
    }
    let transcript = history
        .iter()
        .map(ReplayTurn::chatml)
        .collect::<Vec<_>>()
        .join("\n");
    Ok((redact(&transcript, secrets).into_owned(), true))
}

struct ReplayTurn {
    role: &'static str,
    content: String,
    calls: Vec<(String, String)>,
    tool_name: Option<String>,
    is_error: bool,
}

impl ReplayTurn {
    fn user(content: String) -> Self {
        Self {
            role: "user",
            content,
            calls: Vec::new(),
            tool_name: None,
            is_error: false,
        }
    }

    fn tool(content: String, name: &str, is_error: bool) -> Self {
        Self {
            role: "tool",
            content,
            calls: Vec::new(),
            tool_name: Some(String::from(name)),
            is_error,
        }
    }

    fn chatml(&self) -> String {
        let mut body = String::new();
        if !self.content.is_empty() {
            body.push_str(&self.content);
            body.push('\n');
        }
        for (name, args) in &self.calls {
            let _ = writeln!(body, "<ran {name}>{args}</ran>");
        }
        if self.role == "tool" {
            let header = self.tool_name.as_ref().map_or_else(
                || String::from("<ran:result>"),
                |_| {
                    if self.is_error {
                        String::from("<ran:result status=error>")
                    } else {
                        String::from("<ran:result>")
                    }
                },
            );
            return format!(
                "{CHATML_START}{}\n{header}\n{}{CHATML_END}",
                self.role, self.content
            );
        }
        format!("{CHATML_START}{}\n{body}{CHATML_END}", self.role)
    }
}

fn assistant_turn(parts: &[AssistantPart]) -> Option<ReplayTurn> {
    let mut content = Vec::new();
    let mut calls = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text, .. } if !text.is_empty() => content.push(text.as_ref()),
            AssistantPart::Thinking { .. } | AssistantPart::Text { .. } => {}
            AssistantPart::ToolCall { name, args, .. } => {
                calls.push((String::from(name.as_ref()), String::from(args.as_str())));
            }
        }
    }
    if content.is_empty() && calls.is_empty() {
        return None;
    }
    Some(ReplayTurn {
        role: "assistant",
        content: content.join("\n"),
        calls,
        tool_name: None,
        is_error: false,
    })
}

fn user_text(parts: &[Part]) -> Result<String, ProviderError> {
    let mut texts: Vec<String> = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => texts.push(String::from(text.as_ref())),
            Part::Image { mime, .. } => texts.push(format!("[{mime} image]")),
            Part::Blob { blob_id, .. } => {
                return Err(ProviderError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok(texts.join("\n"))
}

/// The overflow message of an oversized goal, worded so the shared overflow
/// classifier compacts instead of failing.
fn goal_overflow_message(goal_bytes: usize) -> String {
    format!(
        "prompt is too long: {goal_bytes} bytes exceeds the GitLab Duo Agent goal byte budget (soft {GOAL_SOFT_BYTES}, hard {GOAL_HARD_BYTES})"
    )
}

// --- START frames -------------------------------------------------------

/// The parts of one `START` frame.
#[derive(Clone)]
struct StartParts {
    workflow_id: String,
    definition: String,
    goal: String,
    metadata: String,
    system: String,
    tools_json: String,
    preapproved_json: String,
}

#[derive(Serialize)]
struct StartRequest<'a> {
    #[serde(rename = "workflowID")]
    workflow_id: &'a str,
    #[serde(rename = "clientVersion")]
    client_version: &'static str,
    #[serde(rename = "workflowDefinition")]
    definition: &'a str,
    goal: &'a str,
    #[serde(rename = "workflowMetadata")]
    metadata: &'a str,
    #[serde(rename = "additional_context")]
    additional_context: Vec<RawJson>,
    #[serde(rename = "clientCapabilities")]
    capabilities: &'static [&'static str],
    #[serde(rename = "mcpTools")]
    tools: &'a RawJson,
    #[serde(rename = "preapproved_tools")]
    preapproved: &'a RawJson,
    #[serde(
        rename = "flowConfigSchemaVersion",
        skip_serializing_if = "Option::is_none"
    )]
    flow_version: Option<&'static str>,
    #[serde(rename = "flowConfig", skip_serializing_if = "Option::is_none")]
    flow: Option<FlowConfig<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval: Option<RawJson>,
}
/// The socket envelope of one `START` frame: the server only accepts the
/// payload nested under `startRequest`.
#[derive(Serialize)]
struct StartEnvelope<'a> {
    #[serde(rename = "startRequest")]
    start: &'a StartRequest<'a>,
}

#[derive(Serialize)]
struct FlowConfig<'a> {
    version: &'static str,
    environment: &'static str,
    flow: FlowEntry<'a>,
    components: Vec<FlowComponent<'a>>,
    routers: Vec<FlowRouter<'a>>,
    prompts: Vec<FlowPrompt<'a>>,
}

#[derive(Serialize)]
struct FlowEntry<'a> {
    entry_point: &'a str,
}

#[derive(Serialize)]
struct FlowComponent<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    prompt_id: &'a str,
    toolset: Vec<String>,
    inputs: Vec<FlowInput>,
    ui_log_events: Vec<&'static str>,
}

#[derive(Serialize)]
struct FlowInput {
    from: &'static str,
    #[serde(rename = "as")]
    name: &'static str,
}

#[derive(Serialize)]
struct FlowRouter<'a> {
    from: &'a str,
    to: &'static str,
}

#[derive(Serialize)]
struct FlowPrompt<'a> {
    name: &'a str,
    prompt_id: &'a str,
    unit_primitives: Vec<&'static str>,
    prompt_template: PromptTemplate<'a>,
}

#[derive(Serialize)]
struct PromptTemplate<'a> {
    system: &'a str,
    user: &'static str,
    placeholder: &'static str,
}

/// Renders one `START` frame: the inline ambient flow whose system slot
/// carries dal's authoritative system prompt and whose user slot the server
/// fills with the goal transcript.
fn render_start(parts: &StartParts) -> Result<String, ProviderError> {
    RawJson::parse(&parts.metadata).map_err(|_| invalid("workflow metadata is not JSON"))?;
    let tools =
        RawJson::parse(&parts.tools_json).map_err(|_| invalid("workflow tools are not JSON"))?;
    let preapproved = RawJson::parse(&parts.preapproved_json)
        .map_err(|_| invalid("workflow preapproved tools are not JSON"))?;
    let flow = FlowConfig {
        version: "v1",
        environment: DEFINITION,
        flow: FlowEntry {
            entry_point: AGENT_NAME,
        },
        components: vec![FlowComponent {
            name: AGENT_NAME,
            kind: "AgentComponent",
            prompt_id: PROMPT_ID,
            toolset: Vec::new(),
            inputs: vec![FlowInput {
                from: "context:goal",
                name: "goal",
            }],
            ui_log_events: UI_LOG_EVENTS.to_vec(),
        }],
        routers: vec![FlowRouter {
            from: AGENT_NAME,
            to: "end",
        }],
        prompts: vec![FlowPrompt {
            name: PROMPT_ID,
            prompt_id: PROMPT_ID,
            unit_primitives: vec!["duo_agent_platform"],
            prompt_template: PromptTemplate {
                system: &parts.system,
                user: "{{goal}}",
                placeholder: "history",
            },
        }],
    };
    let frame = StartRequest {
        workflow_id: &parts.workflow_id,
        client_version: CLIENT_VERSION,
        definition: &parts.definition,
        goal: &parts.goal,
        metadata: parts.metadata.as_str(),
        additional_context: Vec::new(),
        capabilities: &CAPABILITIES,
        tools: &tools,
        preapproved: &preapproved,
        flow_version: Some("v1"),
        flow: Some(flow),
        approval: None,
    };
    encode_start(&frame)
}

/// Renders the approval restart of one `START` frame: the goal goes empty
/// and the approval member answers the gate.
fn render_approval_start(parts: &StartParts) -> Result<String, ProviderError> {
    RawJson::parse(&parts.metadata).map_err(|_| invalid("workflow metadata is not JSON"))?;
    let tools =
        RawJson::parse(&parts.tools_json).map_err(|_| invalid("workflow tools are not JSON"))?;
    let preapproved = RawJson::parse(&parts.preapproved_json)
        .map_err(|_| invalid("workflow preapproved tools are not JSON"))?;
    let frame = StartRequest {
        workflow_id: &parts.workflow_id,
        client_version: CLIENT_VERSION,
        definition: &parts.definition,
        goal: "",
        metadata: parts.metadata.as_str(),
        additional_context: Vec::new(),
        capabilities: &CAPABILITIES,
        tools: &tools,
        preapproved: &preapproved,
        flow_version: None,
        flow: None,
        approval: Some(
            RawJson::parse(r#"{"approval":{}}"#)
                .map_err(|_| invalid("approval member is not JSON"))?,
        ),
    };
    encode_start(&frame)
}

fn encode_start(frame: &StartRequest<'_>) -> Result<String, ProviderError> {
    let envelope = StartEnvelope { start: frame };
    let mut bytes =
        sonic_rs::to_vec(&envelope).map_err(|_| invalid("workflow START failed to encode"))?;
    bytes.push(b'\n');
    String::from_utf8(bytes).map_err(|_| invalid("workflow START is not UTF-8"))
}

/// The `mcpTools` array of the advertised tools: bare names, the dalgon
/// server name, and the schemas as JSON strings. A `none` tool choice
/// advertises nothing.
fn render_tools(
    request: &ModelRequest,
    tool_choice_none: bool,
) -> Result<(String, String), ProviderError> {
    if tool_choice_none || request.tools.is_empty() {
        return Ok((String::from("[]"), String::from("[]")));
    }
    let mut tools = String::from("[");
    let mut approved = String::from("[");
    for (index, tool) in request.tools.iter().enumerate() {
        if index > 0 {
            tools.push(',');
            approved.push(',');
        }
        let name = wire_name(&tool.name, WIRE_NAME_MAX);
        let schema = tool.parameters.as_str();
        if sonic_rs::from_str::<sonic_rs::Value>(schema).is_err() {
            return Err(invalid(format!(
                "tool `{}` parameters are not JSON",
                tool.name
            )));
        }
        let _ = write!(
            tools,
            r#"{{"name":"{name}","originalToolName":"{name}","serverName":"dalgon","description":{description},"inputSchema":{schema_json},"isApproved":true}}"#,
            description = quoted(&tool.description),
            schema_json = quoted(schema),
        );
        approved.push_str(&sonic_rs::to_string(name.as_ref()).unwrap_or_default());
    }
    tools.push(']');
    approved.push(']');
    Ok((tools, approved))
}

// --- Socket frames ------------------------------------------------------

/// One tool action the server dispatched.
#[derive(Clone, Debug)]
struct ServerAction {
    request_id: String,
    name: String,
    args: RawJson,
}

/// What one frame (or its absence) means for the run.
enum FrameOutcome {
    /// Keep pumping.
    Continue,
    /// The run completed: exactly one terminal success follows.
    Terminal,
    /// The run failed terminally with this message.
    Failed(String),
    /// One tool call: the turn ends `ToolUse` and the action waits.
    Action(ServerAction),
    /// An approval gate: resend an approval `START`.
    Approval,
    /// The step budget hit: restart fresh.
    StepLimit,
    /// A transient processing failure: retry once fresh.
    Retryable(String),
    /// The workflow stopped advancing: restart fresh.
    Stalled,
}

/// The incremental decoder of one workflow socket: checkpoint snapshots
/// with dedupe, statuses, and tool actions.
struct Decoder {
    budget: StreamBudget,
    checkpoint: CheckpointState,
    /// The checkpoint byte length of the previous tool-call boundary (`None`
    /// until a comparable prior reading exists, so a lone boundary is never
    /// flagged as a stall).
    last_action_len: Option<usize>,
    /// The latest checkpoint frame's byte length, consumed once by the next
    /// action edge. A checkpoint snapshot arms this; the following boundary
    /// compares against it and clears it, so a later bare action with no
    /// fresh checkpoint has nothing comparable and never stalls on a stale
    /// reading.
    pending_checkpoint_len: Option<usize>,
    context_used: Option<u64>,
}

impl Decoder {
    fn new(checkpoint: CheckpointState, last_action_len: Option<usize>) -> Self {
        Self {
            budget: StreamBudget::default(),
            checkpoint,
            last_action_len,
            pending_checkpoint_len: None,
            context_used: None,
        }
    }

    /// Decodes one text frame, appending deltas to `out`.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Protocol`] for an action without its mandatory
    /// `requestID`; [`ProviderError::Limit`] when the frame budget is spent.
    fn feed(
        &mut self,
        data: &str,
        out: &mut Vec<StreamEvent>,
    ) -> Result<FrameOutcome, ProviderError> {
        self.budget.admit_event(data.len())?;
        let Some(frame) = parse_frame(data, &mut self.checkpoint, out)? else {
            return Ok(FrameOutcome::Continue);
        };
        if let Some(usage) = frame.usage {
            self.context_used = Some(usage);
        }
        if frame.checkpoint_len.is_some() {
            self.pending_checkpoint_len = frame.checkpoint_len;
        }
        if let Some(status) = frame.status.as_deref() {
            if APPROVAL_STATUSES.contains(&status) {
                return Ok(FrameOutcome::Approval);
            }
            if COMPLETION_STATUSES.contains(&status) {
                return Ok(FrameOutcome::Terminal);
            }
            if status == "FAILED" || status == "STOPPED" {
                return Ok(failed(&frame.message));
            }
        }
        if let Some(action) = frame.action {
            // Stall compares the current checkpoint reading at this action
            // edge: the frame's own checkpoint length when it carries one,
            // else the latest checkpoint frame consumed once. A bare action
            // with no checkpoint since the last boundary records nothing
            // and never stalls on a stale reading.
            if frame.checkpoint_len.is_some() {
                self.pending_checkpoint_len = None;
            }
            let current = frame
                .checkpoint_len
                .or_else(|| self.pending_checkpoint_len.take());
            if let Some(current) = current {
                if self.last_action_len == Some(current) {
                    return Ok(FrameOutcome::Stalled);
                }
                self.last_action_len = Some(current);
            }
            self.budget.admit_item()?;
            return Ok(FrameOutcome::Action(action));
        }
        Ok(FrameOutcome::Continue)
    }
}

fn failed(message: &str) -> FrameOutcome {
    if message.to_ascii_lowercase().contains(STEP_LIMIT_CLAUSE) {
        return FrameOutcome::StepLimit;
    }
    if message.to_ascii_lowercase().contains(GENERIC_ERROR_CLAUSE) {
        return FrameOutcome::Retryable(String::from(message));
    }
    FrameOutcome::Failed(String::from(message))
}

/// One parsed frame: an optional status, an optional tool action, and the
/// checkpoint length and usage the frame reported. Deltas emit inside
/// [`parse_event`], before the status is read.
struct ParsedFrame {
    status: Option<String>,
    message: String,
    action: Option<ServerAction>,
    checkpoint_len: Option<usize>,
    usage: Option<u64>,
}

/// One checkpoint: direct text or a `ui_chat_log` entry list, with the byte
/// length stall detection compares and the context usage it reported.
struct Checkpoint {
    entries: Vec<CheckpointEntry>,
    len: usize,
    usage: Option<u64>,
}

enum CheckpointEntry {
    Text { key: String, content: String },
    Thinking { key: String, content: String },
    Boundary,
}

fn get_str<'v>(value: &'v sonic_rs::Value, key: &str) -> Option<&'v str> {
    use sonic_rs::JsonValueTrait;
    value.get(key).and_then(|field| field.as_str())
}

fn nested_str(value: &sonic_rs::Value, parent: &str, key: &str) -> Option<String> {
    use sonic_rs::JsonValueTrait;
    value
        .get(parent)
        .and_then(|holder| holder.get(key))
        .and_then(|field| field.as_str())
        .map(String::from)
}

/// Parses one socket text frame into its status, checkpoint, and action.
/// Parse uses the lenient shape: unknown members never fail the run, and a
/// frame that is not a JSON object carries nothing to act on.
///
/// # Errors
///
/// [`ProviderError::Protocol`] for an action without its mandatory
/// `requestID`; a synthesized id would be silently discarded by the server.
fn parse_frame(
    data: &str,
    state: &mut CheckpointState,
    out: &mut Vec<StreamEvent>,
) -> Result<Option<ParsedFrame>, ProviderError> {
    let Ok(event) = sonic_rs::from_str(data) else {
        return Ok(None);
    };
    Ok(Some(parse_event(&event, state, out)?))
}

fn provider_protocol(detail: &str) -> ProviderError {
    ProviderError::Protocol {
        family: Family::GitLab,
        detail: String::from(detail),
    }
}

fn parse_event(
    event: &sonic_rs::Value,
    state: &mut CheckpointState,
    out: &mut Vec<StreamEvent>,
) -> Result<ParsedFrame, ProviderError> {
    let workflow_status = nested_str(event, "workflowStatus", "status");
    let checkpoint_status = nested_str(event, "newCheckpoint", "status");
    let status = get_str(event, "status")
        .or(workflow_status.as_deref())
        .or(checkpoint_status.as_deref())
        .map(String::from);
    let mut checkpoint_len = None;
    let mut usage = None;
    if let Some(found) = extract_checkpoint(event) {
        checkpoint_len = Some(found.len);
        usage = found.usage;
        emit_checkpoint(state, &found, out);
    }
    let message = get_str(event, "error")
        .or_else(|| get_str(event, "message"))
        .unwrap_or_default()
        .to_owned();
    let action = extract_action(event)?;
    Ok(ParsedFrame {
        status,
        message,
        action,
        checkpoint_len,
        usage,
    })
}

/// Extracts the checkpoint of one frame: a direct text member, or the
/// `channel_values.ui_chat_log` entry list of the `checkpoint` JSON member.
fn extract_checkpoint(event: &sonic_rs::Value) -> Option<Checkpoint> {
    use sonic_rs::JsonValueTrait;
    let holder = event
        .get("action")
        .and_then(|action| action.get("newCheckpoint"))
        .or_else(|| event.get("newCheckpoint"))
        .or_else(|| event.get("checkpoint"))?;
    let usage = extract_usage(&[event, holder]);
    let nested_message = nested_str(holder, "checkpoint", "message");
    let nested_text = nested_str(holder, "checkpoint", "text");
    if let Some(text) = get_str(holder, "message")
        .or_else(|| get_str(holder, "text"))
        .or_else(|| get_str(holder, "content"))
        .or(nested_message.as_deref())
        .or(nested_text.as_deref())
    {
        return Some(Checkpoint {
            entries: vec![CheckpointEntry::Text {
                key: String::from("direct:text"),
                content: String::from(text),
            }],
            len: text.len(),
            usage,
        });
    }
    let json = get_str(holder, "checkpoint")?;
    let checkpoint: sonic_rs::Value = sonic_rs::from_str(json).ok()?;
    let log = checkpoint.get("channel_values")?.get("ui_chat_log")?;
    let log = log.as_array()?;
    let mut entries = Vec::new();
    for (index, entry) in log.iter().enumerate() {
        let kind = get_str(entry, "message_type").unwrap_or_default();
        if kind == "agent" {
            let content = get_str(entry, "content").unwrap_or_default();
            if content.is_empty() {
                continue;
            }
            let reasoning = get_str(entry, "message_sub_type") == Some("reasoning");
            let key = get_str(entry, "message_id").map_or_else(
                || format!("{}:{index}", if reasoning { "reasoning" } else { "agent" }),
                |id| format!("agent:{id}"),
            );
            entries.push(if reasoning {
                CheckpointEntry::Thinking {
                    key,
                    content: String::from(content),
                }
            } else {
                CheckpointEntry::Text {
                    key,
                    content: String::from(content),
                }
            });
        } else if kind == "request" || kind == "tool" {
            entries.push(CheckpointEntry::Boundary);
        }
    }
    Some(Checkpoint {
        entries,
        len: json.len(),
        usage,
    })
}

/// Emits the new slices of one checkpoint snapshot. Frames repeat earlier
/// entries verbatim, so only content past the last emission per message key
/// (with a content-signature fallback for renamed keys) becomes a delta.
fn emit_checkpoint(
    state: &mut CheckpointState,
    checkpoint: &Checkpoint,
    out: &mut Vec<StreamEvent>,
) {
    let mut turn = 0_usize;
    for entry in &checkpoint.entries {
        if matches!(entry, CheckpointEntry::Boundary) {
            turn += 1;
            continue;
        }
        let (key, kind, content) = match entry {
            CheckpointEntry::Text { key, content } => (key, "text", content),
            CheckpointEntry::Thinking { key, content } => (key, "thinking", content),
            CheckpointEntry::Boundary => continue,
        };
        let hash = fnv1a(content);
        let signature = format!("{turn}\0{kind}\0{hash}");
        let content_only = format!("{turn}\0content\0{hash}");
        let previous = state.by_key.get(key).cloned();
        let duplicate = previous.is_none()
            && (state.signatures.contains(&signature) || state.signatures.contains(&content_only));
        let delta = if duplicate {
            String::new()
        } else {
            match previous {
                None => {
                    state.by_key.insert(key.clone(), content.clone());
                    state.signatures.insert(signature);
                    state.signatures.insert(content_only);
                    content.clone()
                }
                Some(previous) if previous == *content => String::new(),
                Some(previous) if content.starts_with(previous.as_str()) => {
                    state.by_key.insert(key.clone(), content.clone());
                    state.signatures.insert(signature);
                    state.signatures.insert(content_only);
                    content[previous.len()..].to_owned()
                }
                Some(_) => String::new(),
            }
        };
        if delta.is_empty() {
            continue;
        }
        if kind == "thinking" {
            out.push(StreamEvent::ReasoningDelta { text: delta });
        } else {
            out.push(StreamEvent::TextDelta { text: delta });
        }
    }
}

/// The 64-bit FNV-1a hash of checkpoint text for the dedupe signatures.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The per-agent context occupancy of one frame, preferring the chat agent.
fn extract_usage(sources: &[&sonic_rs::Value]) -> Option<u64> {
    use sonic_rs::JsonValueTrait;
    for source in sources {
        let Some(map) = source
            .get("agent_context_usage")
            .and_then(|map| map.as_object())
        else {
            continue;
        };
        for preferred in ["Chat Agent", "context_builder"] {
            if let Some(used) = map
                .iter()
                .find(|(name, _)| *name == preferred)
                .and_then(|(_, value)| agent_usage(value))
            {
                return Some(used);
            }
        }
        for (_, value) in map {
            if let Some(used) = agent_usage(value) {
                return Some(used);
            }
        }
    }
    None
}

fn agent_usage(value: &sonic_rs::Value) -> Option<u64> {
    use sonic_rs::JsonValueTrait;
    let used = value.get("total_tokens")?.as_u64()?;
    value.get("max_tokens")?.as_u64()?;
    Some(used)
}

/// Extracts one tool action: the wrapped `action` / `workflowAction` /
/// `toolCall` object, or a root `runMCPTool` / `run_mcp_tool` member. A
/// wrapper carrying a checkpoint is not an action. A `runMCPTool` wrapper
/// names the envelope; mapping resolves the dal tool and its arguments.
fn extract_action(event: &sonic_rs::Value) -> Result<Option<ServerAction>, ProviderError> {
    use sonic_rs::JsonValueTrait;
    if let Some(wrapped) = event
        .get("action")
        .or_else(|| event.get("workflowAction"))
        .or_else(|| event.get("toolCall"))
    {
        if wrapped.get("newCheckpoint").is_some() {
            return Ok(None);
        }
        let Some(name) = get_str(wrapped, "name")
            .or_else(|| get_str(wrapped, "action"))
            .or_else(|| get_str(wrapped, "type"))
            .or_else(|| get_str(event, "actionName"))
        else {
            return Ok(None);
        };
        let request_id = get_str(wrapped, "requestID")
            .or_else(|| get_str(wrapped, "requestId"))
            .or_else(|| get_str(wrapped, "id"))
            .or_else(|| get_str(event, "requestID"))
            .or_else(|| get_str(event, "requestId"));
        let Some(request_id) = request_id else {
            return Err(provider_protocol(&format!(
                "workflow action \"{name}\" is missing its requestID"
            )));
        };
        let args_value = wrapped
            .get("args")
            .or_else(|| wrapped.get("arguments"))
            .unwrap_or(wrapped);
        let (tool_name, args) = map_action(name, args_value);
        return Ok(Some(ServerAction {
            request_id: String::from(request_id),
            name: tool_name,
            args,
        }));
    }
    for name in ACTION_NAMES {
        if let Some(args) = event.get(name) {
            let Some(request_id) =
                get_str(event, "requestID").or_else(|| get_str(event, "requestId"))
            else {
                return Err(provider_protocol(&format!(
                    "workflow action \"{name}\" is missing its requestID"
                )));
            };
            let (tool_name, args) = map_action(name, args);
            return Ok(Some(ServerAction {
                request_id: String::from(request_id),
                name: tool_name,
                args,
            }));
        }
    }
    Ok(None)
}

/// Maps one server action to the dal tool call it dispatches. A
/// `runMCPTool`/`run_mcp_tool` wrapper names the MCP envelope; the dal tool
/// is the `toolName`/`tool_name`/`name` member of its args, with the call
/// arguments nested under `args`/`arguments` (a JSON string parses, anything
/// else is no arguments). Any other action keeps its own name and carries
/// the whole args object.
fn map_action(name: &str, args: &sonic_rs::Value) -> (String, RawJson) {
    if !ACTION_NAMES.contains(&name) {
        let args = sonic_rs::to_string(args).unwrap_or_default();
        let args = RawJson::parse(&args).unwrap_or_else(|_| RawJson::null());
        return (String::from(name), args);
    }
    let tool = get_str(args, "toolName")
        .or_else(|| get_str(args, "tool_name"))
        .or_else(|| get_str(args, "name"))
        .unwrap_or_default();
    let args = mcp_arguments(args);
    let args = RawJson::parse(&args).unwrap_or_else(|_| RawJson::null());
    (String::from(tool), args)
}

/// The call arguments nested under a `runMCPTool` wrapper's `args` /
/// `arguments` member: an object rides verbatim, a JSON string parses, and
/// anything else is no arguments.
fn mcp_arguments(args: &sonic_rs::Value) -> String {
    use sonic_rs::JsonValueTrait;
    let Some(inner) = args.get("args").or_else(|| args.get("arguments")) else {
        return String::from("{}");
    };
    if inner.as_object().is_some() {
        return sonic_rs::to_string(inner).unwrap_or_default();
    }
    let Some(text) = inner.as_str() else {
        return String::from("{}");
    };
    sonic_rs::from_str::<sonic_rs::Value>(text)
        .map_or_else(|_| String::from("{}"), |_| String::from(text))
}

/// The `actionResponse` frame answering one tool result on the live socket.
fn action_response(request_id: &str, text: &str, is_error: bool) -> String {
    let body = sonic_rs::to_string(text).unwrap_or_default();
    let response = if is_error {
        format!(r#"{{"error":{body}}}"#)
    } else {
        format!(r#"{{"response":{body}}}"#)
    };
    format!(
        r#"{{"actionResponse":{{"requestID":{id},"plainTextResponse":{response}}}}}"#,
        id = sonic_rs::to_string(request_id).unwrap_or_default()
    )
}

// --- Socket lifecycle ---------------------------------------------------

/// Opens one workflow turn.
///
/// A pending action whose tool result reached the context resumes on its
/// socket; anything else runs the REST setup and a fresh workflow. The
/// returned stream carries exactly one terminal: success, one tool call, or
/// one failure. Cancellation before the first frame returns `None`; a
/// cancelled stream emits no terminal.
///
/// # Errors
///
/// [`AttemptFailure`] for setup failures, an oversized goal, a dead resume,
/// or a transport failure before the first decode.
pub(crate) async fn run_turn(
    client: &reqwest::Client,
    sessions: &WorkflowSessions,
    turn: &WorkflowTurn<'_>,
) -> Result<Option<EventStream>, AttemptFailure> {
    if turn.cancel.is_cancelled() {
        return Ok(None);
    }
    let secrets = [Box::<str>::from(turn.token)];
    let (goal, multiturn) =
        render_goal(turn.request, &secrets).map_err(AttemptFailure::Provider)?;
    if goal.len() >= GOAL_HARD_BYTES {
        return Err(AttemptFailure::Provider(ProviderError::ContextOverflow {
            family: Family::GitLab,
            code: String::from("context_length_exceeded"),
            message: goal_overflow_message(goal.len()),
        }));
    }
    let overflow_label = (goal.len() >= GOAL_SOFT_BYTES).then(|| goal_overflow_message(goal.len()));
    let key = (turn.session, Box::from(turn.provider));
    let resumable = lock(&sessions.inner).remove(&key);
    match resume_if_ready(client, turn, resumable).await? {
        Resume::Live {
            socket,
            decoder,
            workflow_id,
            connection,
            start,
            selected,
            project_id,
        } => {
            let driver = Driver::live(
                client.clone(),
                sessions.clone(),
                turn,
                socket,
                decoder,
                RestParts {
                    workflow_id,
                    connection,
                    start,
                    selected,
                    project_id,
                },
                overflow_label,
                secrets.to_vec(),
            );
            Ok(Some(driver.into_stream()))
        }
        Resume::Fresh => {
            let setup = setup_fresh(client, turn, &goal, multiturn).await?;
            // The setup already created the workflow, so a `START` render
            // or socket-open failure must stop it before surfacing the
            // terminal error; the send arm below already stops the same way.
            let frame = match render_start(&setup.start) {
                Ok(frame) => frame,
                Err(error) => {
                    stop_workflow(client, turn, &setup.workflow_id).await;
                    return Err(AttemptFailure::Provider(error));
                }
            };
            let mut socket = match connect_socket(
                turn,
                &setup.connection,
                &setup.start.definition,
                &setup.selected,
                setup.project_id.as_deref(),
            )
            .await
            {
                Ok(socket) => socket,
                Err(error) => {
                    stop_workflow(client, turn, &setup.workflow_id).await;
                    return Err(error);
                }
            };
            if send_text(&mut socket, &frame, turn.cancel).await.is_err() {
                stop_workflow(client, turn, &setup.workflow_id).await;
                return Err(AttemptFailure::Provider(ProviderError::WsClosed {
                    code: None,
                }));
            }
            let driver = Driver::fresh(
                client.clone(),
                sessions.clone(),
                turn,
                socket,
                setup,
                overflow_label,
                secrets.to_vec(),
            );
            Ok(Some(driver.into_stream()))
        }
    }
}

/// What a removed session row means for this turn.
#[expect(
    clippy::large_enum_variant,
    reason = "Resume::Live retains the connected socket and decoder for the turn"
)]
enum Resume {
    /// No resumable state: run the REST setup and a fresh workflow.
    Fresh,
    /// The tool result answered on the live socket: drive it.
    Live {
        socket: Socket,
        decoder: Decoder,
        workflow_id: String,
        connection: Connection,
        start: StartParts,
        selected: String,
        project_id: Option<String>,
    },
}

/// Either resumes on a live socket or falls back to a fresh workflow.
///
/// A pending action without its tool result yet, or with a user steer past
/// it, abandons the stale workflow and restarts fresh with the rebuilt goal.
/// A pending action whose socket died fails: its result cannot be delivered.
async fn resume_if_ready(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    resumable: Option<Session>,
) -> Result<Resume, AttemptFailure> {
    let Some(session) = resumable else {
        return Ok(Resume::Fresh);
    };
    let Some(pending) = session.pending.clone() else {
        return Ok(Resume::Fresh);
    };
    let abandon = tool_result(turn.request, &pending.request_id).is_none()
        || steered(turn.request, &pending.request_id);
    if abandon {
        stop_workflow(client, turn, &session.workflow_id).await;
        return Ok(Resume::Fresh);
    }
    let Some(mut socket) = session.socket else {
        stop_workflow(client, turn, &session.workflow_id).await;
        return Err(AttemptFailure::Provider(ProviderError::WsClosed {
            code: None,
        }));
    };
    let Some((result, is_error)) = tool_result(turn.request, &pending.request_id) else {
        stop_workflow(client, turn, &session.workflow_id).await;
        return Ok(Resume::Fresh);
    };
    let frame = action_response(&pending.request_id, &result, is_error);
    if send_text(&mut socket, &frame, turn.cancel).await.is_err() {
        stop_workflow(client, turn, &session.workflow_id).await;
        return Err(AttemptFailure::Provider(ProviderError::WsClosed {
            code: None,
        }));
    }
    Ok(Resume::Live {
        socket,
        decoder: Decoder::new(session.checkpoint, session.last_action_len),
        workflow_id: String::from(session.workflow_id),
        connection: session.connection,
        start: session.start,
        selected: session.selected,
        project_id: session.project_id,
    })
}

/// Finds the tool result pairing one pending action, newest first.
fn tool_result(request: &ModelRequest, request_id: &str) -> Option<(String, bool)> {
    for item in request.context.iter().rev() {
        let ContextItem::ToolResult {
            call,
            parts,
            is_error,
            ..
        } = item
        else {
            continue;
        };
        if call.as_str() != request_id {
            continue;
        }
        let text = parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Some((text, *is_error));
    }
    None
}

/// Whether a user message steered past the pending tool result.
fn steered(request: &ModelRequest, request_id: &str) -> bool {
    let mut seen_result = false;
    for item in request.context.iter() {
        match item {
            ContextItem::ToolResult { call, .. } if call.as_str() == request_id => {
                seen_result = true;
            }
            ContextItem::User { .. } if seen_result => return true,
            _ => {}
        }
    }
    false
}

/// The REST setup of a fresh run: settings, scope, access, and workflow.
struct FreshSetup {
    connection: Connection,
    workflow_id: String,
    project_id: Option<String>,
    selected: String,
    start: StartParts,
}

/// Runs the REST setup of a fresh run and returns its connection data. The
/// socket opens after, so a setup failure spends no connection. A
/// `start_parts` failure after the workflow create stops the created
/// workflow before it strands.
async fn setup_fresh(
    client: &reqwest::Client,
    turn: &WorkflowTurn<'_>,
    goal: &str,
    multiturn: bool,
) -> Result<FreshSetup, AttemptFailure> {
    let scope = turn.scope;
    let rest_namespace = rest_namespace_id(&scope.root_namespace_id);
    let root_graphql = graphql_namespace_id(&scope.root_namespace_id);
    let definition = scope.workflow_definition.as_deref().unwrap_or(DEFINITION);
    let _ = ensure_settings(client, turn, &rest_namespace).await;
    let project = match (&scope.project_id, &scope.project_path) {
        (Some(id), _) => Some((id.clone(), String::new())),
        (None, Some(path)) => numeric_project_id(client, turn, path)
            .await
            .map(|id| (id, path.clone())),
        (None, None) => discover_project(client, turn, &rest_namespace).await,
    };
    let (numeric_id, project_path) = project.map_or((None, None), |(id, path)| {
        (Some(id), (!path.is_empty()).then_some(path))
    });
    let rest_project = scope
        .project_path
        .clone()
        .or(scope.project_id.clone())
        .or(project_path);
    let connection = workflow_direct_access(
        client,
        turn,
        &root_graphql,
        rest_project.as_deref(),
        definition,
    )
    .await
    .map_err(AttemptFailure::Provider)?;
    let workflow_id = create_workflow(
        client,
        turn,
        scope.namespace_path.as_deref().unwrap_or(&rest_namespace),
        rest_project.as_deref(),
        definition,
    )
    .await
    .map_err(AttemptFailure::Provider)?;
    let selected = selected_model(client, turn, &root_graphql).await;
    let start = match start_parts(
        turn,
        &workflow_id,
        definition,
        goal,
        multiturn,
        &selected,
        numeric_id.as_deref(),
        &rest_namespace,
    ) {
        Ok(start) => start,
        Err(error) => {
            stop_workflow(client, turn, &workflow_id).await;
            return Err(error);
        }
    };
    Ok(FreshSetup {
        connection,
        workflow_id,
        project_id: numeric_id,
        selected,
        start,
    })
}

/// Assembles one `START`'s parts: metadata, system, and advertised tools.
fn start_parts(
    turn: &WorkflowTurn<'_>,
    workflow_id: &str,
    definition: &str,
    goal: &str,
    multiturn: bool,
    selected: &str,
    project_id: Option<&str>,
    rest_namespace: &str,
) -> Result<StartParts, AttemptFailure> {
    let mut system = turn.request.system.to_string();
    if multiturn {
        system.push_str("\n\n");
        system.push_str(HISTORY_NOTE);
    }
    let (tools_json, preapproved_json) =
        render_tools(turn.request, turn.tool_choice_none).map_err(AttemptFailure::Provider)?;
    Ok(StartParts {
        workflow_id: String::from(workflow_id),
        definition: String::from(definition),
        goal: String::from(goal),
        metadata: render_metadata(
            selected,
            project_id,
            rest_namespace,
            &turn.scope.root_namespace_id,
        ),
        system,
        tools_json,
        preapproved_json,
    })
}

fn render_metadata(
    selected: &str,
    project_id: Option<&str>,
    rest_namespace: &str,
    root_namespace: &str,
) -> String {
    let mut metadata = format!(r#"{{"environment":"ide","client_type":"{CLIENT_TYPE}""#);
    if let Some(project) = project_id {
        let _ = write!(metadata, r#","projectId":"{project}""#);
    }
    let _ = write!(metadata, r#","namespaceId":"{rest_namespace}""#);
    let _ = write!(
        metadata,
        r#","rootNamespaceId":"{}""#,
        rest_namespace_id(root_namespace)
    );
    let _ = write!(
        metadata,
        r#","selectedModelIdentifier":{}"#,
        quoted(selected)
    );
    metadata.push('}');
    metadata
}

/// The socket URL: the service endpoint root, or the instance workflow path,
/// with the routing query. The instance base path is preserved.
fn socket_url(
    turn: &WorkflowTurn<'_>,
    connection: &Connection,
    project_id: Option<&str>,
    selected: &str,
    definition: &str,
) -> Result<String, ProviderError> {
    let scope = turn.scope;
    let rest_namespace = rest_namespace_id(&scope.root_namespace_id);
    let mut query = String::new();
    let mut param = |key: &str, value: &str| {
        if !value.is_empty() {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str(key);
            query.push('=');
            query.push_str(&encode_path(value));
        }
    };
    if let Some(project) = project_id {
        param("project_id", project);
    }
    param("namespace_id", &rest_namespace);
    param("root_namespace_id", &rest_namespace);
    param("user_selected_model_identifier", selected);
    param("workflow_definition", definition);
    if connection.service_endpoint {
        let base = connection.base_url.trim_end_matches('/');
        let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
        let ws = match scheme {
            "http" => "ws",
            _ => "wss",
        };
        return Ok(format!("{ws}://{rest}/?{query}"));
    }
    let base = turn.base_url.trim_end_matches('/');
    let (scheme, rest) = base
        .split_once("://")
        .ok_or_else(|| ProviderError::Transport {
            family: Family::GitLab,
            reason: String::from("GitLab base URL is not absolute"),
        })?;
    let ws = match scheme {
        "http" => "ws",
        "https" => "wss",
        _ => {
            return Err(ProviderError::Transport {
                family: Family::GitLab,
                reason: String::from("GitLab base URL must use http or https"),
            });
        }
    };
    Ok(format!("{ws}://{rest}/api/v4/ai/duo_workflows/ws?{query}"))
}

/// The socket headers: the workflow bearer, the client identity, the origin
/// of the GitLab host, the routing ids, and any service headers.
fn socket_headers(
    turn: &WorkflowTurn<'_>,
    connection: &Connection,
    project_id: Option<&str>,
) -> Vec<(String, String)> {
    let scope = turn.scope;
    let rest_namespace = rest_namespace_id(&scope.root_namespace_id);
    let mut headers = vec![
        (
            String::from("authorization"),
            format!("Bearer {}", connection.token),
        ),
        (
            String::from("x-gitlab-client-type"),
            String::from(CLIENT_TYPE),
        ),
        (
            String::from("x-gitlab-language-server-version"),
            String::from(LANGUAGE_SERVER_VERSION),
        ),
        (
            String::from("user-agent"),
            format!(
                "unknown/unknown unknown/unknown gitlab-language-server/{LANGUAGE_SERVER_VERSION}"
            ),
        ),
        (String::from("origin"), origin_of(turn.base_url)),
    ];
    if let Some(project) = project_id {
        headers.push((String::from("x-gitlab-project-id"), String::from(project)));
    }
    headers.push((
        String::from("x-gitlab-namespace-id"),
        rest_namespace.clone(),
    ));
    headers.push((String::from("x-gitlab-root-namespace-id"), rest_namespace));
    headers.extend(connection.headers.clone());
    headers
}

/// The `scheme://authority` origin of a base URL.
fn origin_of(base: &str) -> String {
    let rest = base.split_once("://").map_or(base, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or(rest);
    let scheme = if base.starts_with("http://") {
        "http"
    } else {
        "https"
    };
    format!("{scheme}://{authority}")
}

/// Opens the workflow socket with its headers and size bounds.
async fn connect_socket(
    turn: &WorkflowTurn<'_>,
    connection: &Connection,
    definition: &str,
    selected: &str,
    project_id: Option<&str>,
) -> Result<Socket, AttemptFailure> {
    let url = socket_url(turn, connection, project_id, selected, definition)
        .map_err(AttemptFailure::Provider)?;
    let mut request = url.into_client_request().map_err(|_| {
        AttemptFailure::Provider(ProviderError::Transport {
            family: Family::GitLab,
            reason: String::from("GitLab workflow WebSocket URL is not valid"),
        })
    })?;
    for (name, value) in socket_headers(turn, connection, project_id) {
        let name = name.parse::<HeaderName>().map_err(|_| {
            AttemptFailure::Provider(ProviderError::Transport {
                family: Family::GitLab,
                reason: String::from("GitLab workflow header name is not valid"),
            })
        })?;
        let value = HeaderValue::from_str(&value).map_err(|_| {
            AttemptFailure::Provider(ProviderError::Transport {
                family: Family::GitLab,
                reason: String::from("GitLab workflow header value is not valid"),
            })
        })?;
        request.headers_mut().insert(name, value);
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(http::WS_MESSAGE_LIMIT))
        .max_frame_size(Some(http::WS_MESSAGE_LIMIT));
    let connected = tokio::select! {
        biased;
        () = turn.cancel.cancelled() => return Err(cancelled()),
        connected = timeout(
            http::RESPONSE_HEADER_TIMEOUT,
            connect_async_with_config(request, Some(config), false),
        ) => connected,
    };
    let Ok(Ok((socket, response))) = connected else {
        return Err(AttemptFailure::Provider(ProviderError::Transport {
            family: Family::GitLab,
            reason: String::from("GitLab workflow WebSocket handshake failed"),
        }));
    };
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        return Err(AttemptFailure::Provider(ProviderError::Transport {
            family: Family::GitLab,
            reason: String::from("GitLab workflow WebSocket handshake was not upgraded"),
        }));
    }
    Ok(socket)
}

fn cancelled() -> AttemptFailure {
    AttemptFailure::Provider(ProviderError::Transport {
        family: Family::GitLab,
        reason: String::from("GitLab workflow turn cancelled before it started"),
    })
}

/// Sends one text frame, or fails when the socket is gone or cancelled.
async fn send_text(
    socket: &mut Socket,
    frame: &str,
    cancel: &CancellationToken,
) -> Result<(), AttemptFailure> {
    use futures::SinkExt;
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(cancelled()),
        result = socket.send(Message::Text(frame.into())) => result.map_err(|_| {
            AttemptFailure::Provider(ProviderError::WsClosed { code: None })
        }),
    }
}

/// Closes one socket without waiting for the peer.
async fn close_socket(socket: &mut Socket) {
    let _ = socket.close(None).await;
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// --- Turn driver ----------------------------------------------------------

/// One frame read off the socket.
enum Read {
    Text(String),
    Binary(Vec<u8>),
    Closed,
    Idle,
    Cancelled,
    Broken,
    Limited,
}

/// The live state of one turn's socket pump. Everything is owned so the
/// stream is `'static`; the session table holds a cloneable handle.
struct Driver {
    client: reqwest::Client,
    sessions: WorkflowSessions,
    request: ModelRequest,
    model: String,
    base_url: String,
    token: String,
    secrets: Vec<Box<str>>,
    provider: Box<str>,
    session: SessionId,
    scope: WorkflowScope,
    tool_choice_none: bool,
    timeouts: Timeouts,
    cancel: CancellationToken,
    socket: Option<Socket>,
    decoder: Decoder,
    queue: VecDeque<Result<StreamEvent, ProviderError>>,
    workflow_id: String,
    connection: Connection,
    start: StartParts,
    selected: String,
    project_id: Option<String>,
    sockets_used: u32,
    step_restarts: u32,
    generic_retries: u32,
    stall_restarts: u32,
    idle_restarted: bool,
    overflow_label: Option<String>,
    retry_message: Option<String>,
    finished: bool,
}

/// The owned connection data one driver pump carries across restarts.
struct RestParts {
    workflow_id: String,
    connection: Connection,
    start: StartParts,
    selected: String,
    project_id: Option<String>,
}

impl Driver {
    /// A driver resuming on a live socket with its action already answered.
    fn live(
        client: reqwest::Client,
        sessions: WorkflowSessions,
        turn: &WorkflowTurn<'_>,
        socket: Socket,
        decoder: Decoder,
        rest: RestParts,
        overflow_label: Option<String>,
        secrets: Vec<Box<str>>,
    ) -> Self {
        Self::new(
            client,
            sessions,
            turn,
            Some(socket),
            decoder,
            rest,
            overflow_label,
            secrets,
        )
    }

    /// A driver on a freshly opened socket with a fresh `START` sent.
    fn fresh(
        client: reqwest::Client,
        sessions: WorkflowSessions,
        turn: &WorkflowTurn<'_>,
        socket: Socket,
        setup: FreshSetup,
        overflow_label: Option<String>,
        secrets: Vec<Box<str>>,
    ) -> Self {
        Self::new(
            client,
            sessions,
            turn,
            Some(socket),
            Decoder::new(CheckpointState::default(), None),
            RestParts {
                workflow_id: setup.workflow_id,
                connection: setup.connection,
                start: setup.start,
                selected: setup.selected,
                project_id: setup.project_id,
            },
            overflow_label,
            secrets,
        )
    }

    fn new(
        client: reqwest::Client,
        sessions: WorkflowSessions,
        turn: &WorkflowTurn<'_>,
        socket: Option<Socket>,
        decoder: Decoder,
        rest: RestParts,
        overflow_label: Option<String>,
        secrets: Vec<Box<str>>,
    ) -> Self {
        Self {
            client,
            sessions,
            request: turn.request.clone(),
            model: String::from(turn.model),
            base_url: String::from(turn.base_url),
            token: String::from(turn.token),
            secrets,
            provider: Box::from(turn.provider),
            session: turn.session,
            scope: turn.scope.clone(),
            tool_choice_none: turn.tool_choice_none,
            timeouts: turn.timeouts,
            cancel: turn.cancel.clone(),
            socket,
            decoder,
            queue: VecDeque::new(),
            workflow_id: rest.workflow_id,
            connection: rest.connection,
            start: rest.start,
            selected: rest.selected,
            project_id: rest.project_id,
            sockets_used: 1,
            step_restarts: 0,
            generic_retries: 0,
            stall_restarts: 0,
            idle_restarted: false,
            overflow_label,
            retry_message: None,
            finished: false,
        }
    }

    /// Borrows the owned setup as a turn for the shared REST helpers.
    fn turn(&self) -> WorkflowTurn<'_> {
        WorkflowTurn {
            request: &self.request,
            model: &self.model,
            base_url: &self.base_url,
            token: &self.token,
            provider: &self.provider,
            session: self.session,
            scope: &self.scope,
            tool_choice_none: self.tool_choice_none,
            timeouts: self.timeouts,
            cancel: &self.cancel,
            user_agent: "",
        }
    }

    fn into_stream(self) -> EventStream {
        let pump = self;
        let source = stream::unfold(pump, |mut driver| async move {
            let item = driver.step().await?;
            Some((item, driver))
        });
        EventStream::new(source, || {})
    }

    async fn step(&mut self) -> Option<Result<StreamEvent, ProviderError>> {
        loop {
            if let Some(item) = self.queue.pop_front() {
                return Some(item);
            }
            if self.finished {
                return None;
            }
            if self.socket.is_none() {
                self.queue
                    .push_back(Err(ProviderError::WsClosed { code: None }));
                self.finished = true;
                continue;
            }
            match self.read().await {
                Read::Text(frame) => self.frame(frame).await,
                Read::Binary(bytes) => {
                    self.close_and_stop().await;
                    if bytes.len() > http::WS_MESSAGE_LIMIT {
                        self.finish_error(ProviderError::Limit(LimitError::WsMessage));
                    } else {
                        self.finish_error(provider_protocol("gitlab workflow sent a binary frame"));
                    }
                }
                Read::Limited => {
                    self.close_and_stop().await;
                    self.finish_error(ProviderError::Limit(LimitError::WsMessage));
                }
                Read::Closed => {
                    self.close_and_stop().await;
                    self.finish_stop();
                }
                Read::Idle => self.idle().await,
                Read::Cancelled => {
                    self.close_and_stop().await;
                    self.finished = true;
                }
                Read::Broken => {
                    self.close_and_stop().await;
                    self.finish_error(ProviderError::WsClosed { code: None });
                }
            }
        }
    }

    /// Reads one frame with the idle deadline and cancellation.
    async fn read(&mut self) -> Read {
        let Some(socket) = self.socket.as_mut() else {
            return Read::Broken;
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Read::Cancelled,
            next = timeout(self.timeouts.idle, socket.next()) => match next {
                Err(_) => Read::Idle,
                Ok(None) => Read::Broken,
                Ok(Some(Err(error))) => match error {
                    WsError::Capacity(_) => Read::Limited,
                    _ => Read::Broken,
                },
                Ok(Some(Ok(Message::Text(text)))) => Read::Text(text.to_string()),
                Ok(Some(Ok(Message::Binary(bytes)))) => Read::Binary(bytes.to_vec()),
                Ok(Some(Ok(Message::Close(_)))) => Read::Closed,
                Ok(Some(Ok(_))) => Read::Text(String::new()),
            },
        }
    }

    /// Handles one text frame through the decoder.
    async fn frame(&mut self, frame: String) {
        if frame.is_empty() {
            return;
        }
        let mut out = Vec::new();
        let outcome = match self.decoder.feed(&frame, &mut out) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.close_and_stop().await;
                self.finish_error(error);
                return;
            }
        };
        self.queue.extend(out.into_iter().map(Ok));
        match outcome {
            FrameOutcome::Continue => {}
            FrameOutcome::Terminal => self.finish_stop(),
            FrameOutcome::Failed(message) => {
                self.close_and_stop().await;
                let message = self.overflow_label.take().unwrap_or(message);
                self.finish_error(status_error(&message));
            }
            FrameOutcome::Action(action) => self.finish_action(action),
            FrameOutcome::Approval => self.approval_round().await,
            FrameOutcome::StepLimit => {
                self.restart(Restart::StepLimit).await;
            }
            FrameOutcome::Retryable(message) => {
                self.retry_message = Some(message);
                self.restart(Restart::Retryable).await;
            }
            FrameOutcome::Stalled => {
                self.restart(Restart::Stalled).await;
            }
        }
    }

    /// Handles one idle deadline: once a fresh workflow, then a terminal stop.
    async fn idle(&mut self) {
        if self.idle_restarted {
            self.close_and_stop().await;
            self.finish_stop();
            return;
        }
        self.idle_restarted = true;
        self.fresh_workflow().await;
    }

    /// Restarts on a fresh workflow while the bounded budget lasts, else
    /// surfaces the terminal error: the stashed retry message, the overflow
    /// label, or the restart's own message. A step limit that exhausts its
    /// restarts errors rather than ending silently, keeping exactly one
    /// terminal.
    async fn restart(&mut self, kind: Restart) {
        let (used, budget) = match kind {
            Restart::StepLimit => (self.step_restarts, MAX_STEP_LIMIT_RESTARTS),
            Restart::Retryable => (self.generic_retries, MAX_GENERIC_ERROR_RETRIES),
            Restart::Stalled => (self.stall_restarts, MAX_STALL_RESTARTS),
        };
        if used >= budget {
            self.close_and_stop().await;
            let message = match kind {
                Restart::Retryable => self
                    .overflow_label
                    .take()
                    .or_else(|| self.retry_message.take())
                    .unwrap_or_else(|| String::from("GitLab Duo Workflow run failed")),
                Restart::Stalled => self
                    .overflow_label
                    .take()
                    .unwrap_or_else(|| String::from(STALL_MESSAGE)),
                Restart::StepLimit => self
                    .overflow_label
                    .take()
                    .unwrap_or_else(|| String::from(
                        "GitLab Duo Workflow reached its maximum step limit and could not complete.",
                    )),
            };
            self.finish_error(status_error(&message));
            return;
        }
        match kind {
            Restart::StepLimit => self.step_restarts += 1,
            Restart::Retryable => self.generic_retries += 1,
            Restart::Stalled => self.stall_restarts += 1,
        }
        self.fresh_workflow().await;
    }

    /// Stops the dead workflow and opens a fresh one with the same goal. A
    /// fresh open or `START` render failure stops the created workflow
    /// before surfacing its terminal, so no created id strands; the send
    /// arm already stopped the same way.
    async fn fresh_workflow(&mut self) {
        self.close_and_stop().await;
        if self.sockets_used >= MAX_SOCKETS {
            self.finish_error(status_error("GitLab Duo Workflow socket budget spent"));
            return;
        }
        let turn = self.turn();
        let created = create_workflow(
            &self.client,
            &turn,
            self.scope
                .namespace_path
                .as_deref()
                .unwrap_or(&rest_namespace_id(&self.scope.root_namespace_id)),
            self.rest_project().as_deref(),
            &self.start.definition.clone(),
        )
        .await;
        let workflow_id = match created {
            Ok(id) => id,
            Err(error) => {
                self.finish_error(error);
                return;
            }
        };
        self.workflow_id.clone_from(&workflow_id);
        self.start.workflow_id = workflow_id;
        let opened = self.open_socket().await;
        match opened {
            Ok(socket) => {
                self.socket = Some(socket);
                self.sockets_used += 1;
                let frame = match render_start(&self.start) {
                    Ok(frame) => frame,
                    Err(error) => {
                        self.close_and_stop().await;
                        self.finish_error(error);
                        return;
                    }
                };
                let cancel = self.cancel.clone();
                if let Some(socket) = self.socket.as_mut()
                    && send_text(socket, &frame, &cancel).await.is_err()
                {
                    self.close_and_stop().await;
                    self.finish_error(ProviderError::WsClosed { code: None });
                }
            }
            Err(error) => {
                self.close_and_stop().await;
                self.finish_error(error);
            }
        }
    }

    /// Resends an approval `START` on a new socket for the live workflow.
    async fn approval_round(&mut self) {
        if self.sockets_used >= MAX_SOCKETS {
            self.finish_error(status_error("GitLab Duo Workflow socket budget spent"));
            return;
        }
        let opened = self.open_socket().await;
        match opened {
            Ok(socket) => {
                self.socket = Some(socket);
                self.sockets_used += 1;
                let frame = match render_approval_start(&self.start) {
                    Ok(frame) => frame,
                    Err(error) => {
                        self.finish_error(error);
                        return;
                    }
                };
                let cancel = self.cancel.clone();
                if let Some(socket) = self.socket.as_mut()
                    && send_text(socket, &frame, &cancel).await.is_err()
                {
                    self.close_and_stop().await;
                    self.finish_error(ProviderError::WsClosed { code: None });
                }
            }
            Err(error) => self.finish_error(error),
        }
    }

    /// Opens one socket for the current workflow and connection.
    async fn open_socket(&self) -> Result<Socket, ProviderError> {
        let turn = self.turn();
        connect_socket(
            &turn,
            &self.connection,
            &self.start.definition,
            &self.selected,
            self.project_id.as_deref(),
        )
        .await
        .map_err(Self::socket_error)
    }

    /// Maps a socket-opening failure into the driver's terminal error shape.
    ///
    /// `connect_socket` only produces provider errors today; the response arm
    /// preserves the rejected-response shape without changing that behavior.
    fn socket_error(failure: AttemptFailure) -> ProviderError {
        match failure {
            AttemptFailure::Provider(error) => error,
            AttemptFailure::Response {
                status, message, ..
            } => ProviderError::Status {
                family: Family::GitLab,
                status,
                message,
            },
        }
    }

    /// The REST project scope of a restart, mirroring the setup choice.
    fn rest_project(&self) -> Option<String> {
        self.scope
            .project_path
            .clone()
            .or_else(|| self.scope.project_id.clone())
    }

    /// Emits one tool call as its own terminal turn and keeps the socket for
    /// the resume: exactly one `ToolUse` stop per action.
    fn finish_action(&mut self, action: ServerAction) {
        let ServerAction {
            request_id,
            name,
            args,
        } = action;
        let fragment = args.as_str().as_bytes().to_vec();
        self.queue.push_back(Ok(StreamEvent::ToolCallStarted {
            id: request_id.clone(),
            name: name.clone(),
        }));
        if !fragment.is_empty() {
            self.queue.push_back(Ok(StreamEvent::ToolArgsDelta {
                id: request_id.clone(),
                fragment: fragment.clone(),
            }));
        }
        self.queue.push_back(Ok(StreamEvent::ToolCallsDone {
            calls: vec![ToolCall {
                id: request_id.clone(),
                name: name.clone(),
                args: ToolArgs::from_bytes(&fragment),
            }],
        }));
        self.queue.push_back(Ok(StreamEvent::Usage {
            usage: self.terminal_usage(),
        }));
        self.queue.push_back(Ok(StreamEvent::Stop {
            reason: crate::stream::StopReason::ToolUse,
        }));
        let decoder = std::mem::replace(
            &mut self.decoder,
            Decoder::new(CheckpointState::default(), None),
        );
        let session = Session {
            socket: self.socket.take(),
            workflow_id: Box::from(self.workflow_id.clone()),
            pending: Some(PendingAction { request_id }),
            checkpoint: decoder.checkpoint,
            last_action_len: decoder.last_action_len,
            connection: self.connection.clone(),
            start: self.start.clone(),
            selected: self.selected.clone(),
            project_id: self.project_id.clone(),
        };
        lock(&self.sessions.inner).insert((self.session, Box::from(&*self.provider)), session);
        self.finished = true;
    }

    /// Emits the terminal success triple and drops the session: a completed
    /// workflow needs no server stop.
    fn finish_stop(&mut self) {
        self.queue
            .push_back(Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }));
        self.queue.push_back(Ok(StreamEvent::Usage {
            usage: self.terminal_usage(),
        }));
        self.queue.push_back(Ok(StreamEvent::Stop {
            reason: crate::stream::StopReason::EndTurn,
        }));
        self.finished = true;
    }

    /// Emits one terminal failure and drops the session.
    fn finish_error(&mut self, error: ProviderError) {
        let mut error = error;
        redact_provider_error(&mut error, &self.secrets);
        self.queue.push_back(Err(error));
        self.finished = true;
    }

    fn terminal_usage(&self) -> Usage {
        Usage {
            input_tokens: self.decoder.context_used.unwrap_or(0),
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
            premium_requests: None,
        }
    }

    /// Closes the socket and stops the workflow best-effort.
    async fn close_and_stop(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            close_socket(&mut socket).await;
        }
        let turn = self.turn();
        stop_workflow(&self.client, &turn, &self.workflow_id.clone()).await;
    }
}

/// Which bounded recovery a failed frame asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Restart {
    StepLimit,
    Retryable,
    Stalled,
}

fn status_error(message: &str) -> ProviderError {
    ProviderError::Status {
        family: Family::GitLab,
        status: 200,
        message: String::from(message),
    }
}
