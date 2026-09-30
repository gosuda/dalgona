//! OpenAI-compatible router normalization over the public host contract.
//!
//! This module owns request decoding, route resolution, and stream encoding
//! shapes. Harness execution and pass-through relay run through `Host` and
//! land with the agent seam.

/// Request decoding and model resolution for the HTTP router.
pub(crate) mod decode;
/// HTTP route handlers for the four OpenAI-compatible routes.
pub(crate) mod handlers;
/// Harness execution for router requests.
pub(crate) mod harness;
/// Pass-through relay for router requests.
pub(crate) mod pass;
/// Router-only listener without token authentication.
pub mod serve;
/// Server-sent event and response-object encoding.
pub(crate) mod sink;
pub(crate) mod stream;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use sonic_rs::{JsonContainerTrait, Value};

/// The execution target selected for one router request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouterTarget {
    /// Run the dal harness with the named mode.
    Harness(HarnessMode),
    /// Relay to a provider route; the client runs its own tools.
    Route(dal_core::ModelRoute),
}

/// A built-in harness mode id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessMode {
    /// Standard harness execution.
    Normal,
    /// List eval tools first.
    EvalFirst,
    /// Only the eval tool is visible.
    EvalOnly,
}

impl HarnessMode {
    /// Returns the wire model id for this mode.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Normal => "dalgon/normal",
            Self::EvalFirst => "dalgon/eval-first",
            Self::EvalOnly => "dalgon/eval-only",
        }
    }

    /// Returns the bare mode name the `mode` command takes.
    #[must_use]
    pub const fn mode_arg(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::EvalFirst => "eval-first",
            Self::EvalOnly => "eval-only",
        }
    }

    /// Parses a harness mode id.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "dalgon/normal" => Some(Self::Normal),
            "dalgon/eval-first" => Some(Self::EvalFirst),
            "dalgon/eval-only" => Some(Self::EvalOnly),
            _ => None,
        }
    }
}

/// Options for the HTTP router mounted on the serve listener.
#[derive(Clone, Debug)]
pub struct RouterOptions {
    /// The loopback or public bind address text.
    pub bind: String,
    /// The bound port.
    pub port: u16,
    /// Whether public token authentication is required.
    pub public: bool,
    /// Whether A2A routes are mounted.
    pub a2a: bool,
    /// The absolute serve token file path.
    pub token_file: PathBuf,
    /// The serve approval mode.
    pub approval: dal_core::ApprovalMode,
    /// The allowed browser origins.
    pub origins: Vec<String>,
    /// One-level model aliases from configuration.
    pub aliases: BTreeMap<Box<str>, Box<str>>,
    /// The serve working directory captured once at the edge.
    pub workspace: PathBuf,
}

/// A route match for one HTTP request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteMatch {
    /// A known route with its canonical path.
    Found(Route),
    /// No route matches the method and path.
    NotFound {
        /// The request method.
        method: String,
        /// The stripped request path.
        path: String,
    },
    /// The path exists but the method is not allowed.
    MethodNotAllowed {
        /// The request method.
        method: String,
        /// The request path.
        path: String,
        /// The allowed method.
        allow: String,
    },
}

/// A known router route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    /// List available models.
    Models,
    /// Chat completions.
    Chat,
    /// Responses.
    Responses,
    /// Anthropic messages.
    Messages,
}

/// Matches one method and path against the router table.
#[must_use]
pub fn match_route(method: &str, path: &str) -> RouteMatch {
    let path = path.split('?').next().unwrap_or(path);
    match (method, path) {
        ("GET", "/v1/models") => RouteMatch::Found(Route::Models),
        ("POST", "/v1/chat/completions") => RouteMatch::Found(Route::Chat),
        ("POST", "/v1/responses") => RouteMatch::Found(Route::Responses),
        ("POST", "/v1/messages") => RouteMatch::Found(Route::Messages),
        (_, "/v1/models" | "/v1/chat/completions" | "/v1/responses" | "/v1/messages") => {
            let allow = if path == "/v1/models" { "GET" } else { "POST" };
            RouteMatch::MethodNotAllowed {
                method: method.to_owned(),
                path: path.to_owned(),
                allow: allow.to_owned(),
            }
        }
        _ => RouteMatch::NotFound {
            method: method.to_owned(),
            path: path.to_owned(),
        },
    }
}
/// An A2A REST route kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum A2aRestKind {
    /// `POST /a2a/v1/message:send`.
    Send,
    /// `POST /a2a/v1/message:stream`.
    Stream,
    /// `GET /a2a/v1/tasks/<id>`.
    GetTask,
    /// `POST /a2a/v1/tasks/<id>:cancel`.
    CancelTask,
}

/// A serve-listener route match for one method and path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServeRoute {
    /// List available models.
    Models,
    /// Chat completions.
    Chat,
    /// Responses.
    Responses,
    /// Anthropic messages.
    Messages,
    /// The A2A agent card.
    A2aCard,
    /// A2A JSON-RPC over `POST /a2a`.
    A2aJsonRpc,
    /// An A2A REST route with its optional task id.
    A2aRest(A2aRestKind, Option<String>),
    /// The dal-protocol WebSocket at `GET /v1/ws`.
    WebSocket,
    /// The Codex WebSocket at its upgrade route.
    CodexWs,
    /// No route matches the method and path.
    NotFound {
        /// The request method.
        method: String,
        /// The stripped request path.
        path: String,
    },
    /// The path exists but the method is not allowed.
    MethodNotAllowed {
        /// The request method.
        method: String,
        /// The request path.
        path: String,
        /// The allowed method.
        allow: String,
    },
}

/// Matches one method and path against the serve table.
///
/// A2A and WebSocket routes mount only when their feature is enabled; every
/// other A2A path is a 404 when `a2a` is false.
#[must_use]
pub fn match_serve_route(method: &str, path: &str, a2a: bool) -> ServeRoute {
    let path = path.split('?').next().unwrap_or(path);
    match (method, path) {
        ("GET", "/v1/models") => ServeRoute::Models,
        ("POST", "/v1/chat/completions") => ServeRoute::Chat,
        ("POST", "/v1/responses") => ServeRoute::Responses,
        ("POST", "/v1/messages") => ServeRoute::Messages,
        ("GET", "/.well-known/agent-card.json") if a2a => ServeRoute::A2aCard,
        ("POST", "/a2a") if a2a => ServeRoute::A2aJsonRpc,
        ("POST", "/a2a/v1/message:send") if a2a => ServeRoute::A2aRest(A2aRestKind::Send, None),
        ("POST", "/a2a/v1/message:stream") if a2a => ServeRoute::A2aRest(A2aRestKind::Stream, None),
        ("GET", task) if a2a && is_task_path(task, "/a2a/v1/tasks/", "") => {
            ServeRoute::A2aRest(A2aRestKind::GetTask, task_id(task, "/a2a/v1/tasks/", ""))
        }
        ("POST", task) if a2a && is_task_path(task, "/a2a/v1/tasks/", ":cancel") => {
            ServeRoute::A2aRest(
                A2aRestKind::CancelTask,
                task_id(task, "/a2a/v1/tasks/", ":cancel"),
            )
        }
        ("GET", "/v1/ws") => ServeRoute::WebSocket,
        ("GET", "/codex/ws") => ServeRoute::CodexWs,
        (_, "/v1/models" | "/v1/chat/completions" | "/v1/responses" | "/v1/messages") => {
            let allow = if path == "/v1/models" { "GET" } else { "POST" };
            ServeRoute::MethodNotAllowed {
                method: method.to_owned(),
                path: path.to_owned(),
                allow: allow.to_owned(),
            }
        }
        (_, "/a2a" | "/a2a/v1/message:send" | "/a2a/v1/message:stream") if a2a => {
            ServeRoute::MethodNotAllowed {
                method: method.to_owned(),
                path: path.to_owned(),
                allow: "POST".to_owned(),
            }
        }
        _ => ServeRoute::NotFound {
            method: method.to_owned(),
            path: path.to_owned(),
        },
    }
}

/// Returns true when a path is a task route with the given suffix.
fn is_task_path(path: &str, prefix: &str, suffix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.ends_with(suffix) && !rest.contains('/'))
}

/// Extracts the task id from a task route path.
fn task_id(path: &str, prefix: &str, suffix: &str) -> Option<String> {
    path.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// Collects unknown request members sorted by bytes for `x-dal-ignored`.
#[must_use]
pub fn ignored_members(request: &Value, known: &[&str]) -> Vec<String> {
    let Some(object) = request.as_object() else {
        return Vec::new();
    };
    let mut ignored: Vec<String> = Vec::new();
    for (key, _) in object {
        if !known.contains(&key) {
            ignored.push(key.to_owned());
        }
    }
    ignored.sort();
    ignored
}

/// Canonicalizes JSON with byte-sorted object keys and no whitespace.
#[must_use]
pub fn canonical_json(value: &Value) -> String {
    canonical_value(value)
}

fn canonical_value(value: &Value) -> String {
    if let Some(object) = value.as_object() {
        let mut entries: Vec<(&str, String)> = Vec::new();
        for (key, item) in object {
            entries.push((key, canonical_value(item)));
        }
        entries.sort_by(|left, right| left.0.cmp(right.0));
        let mut text = String::from("{");
        for (index, (key, item)) in entries.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(&sonic_rs::to_string(&key).unwrap_or_default());
            text.push(':');
            text.push_str(item);
        }
        text.push('}');
        return text;
    }
    if let Some(array) = value.as_array() {
        let mut text = String::from("[");
        for (index, item) in array.as_slice().iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(&canonical_value(item));
        }
        text.push(']');
        return text;
    }
    sonic_rs::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

/// Hashes canonical history JSON for harness session continuation.
#[must_use]
pub fn digest_history(items: &[Value]) -> String {
    let mut canonical = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            canonical.push(',');
        }
        canonical.push_str(&canonical_json(item));
    }
    canonical.push(']');
    blake3::hash(canonical.as_bytes()).to_hex().to_string()
}

/// A least-recently-used digest-to-session table with a fixed capacity.
pub struct DigestTable {
    capacity: usize,
    entries: HashMap<String, String>,
    order: Vec<String>,
}

impl DigestTable {
    /// Creates an empty table holding at most `capacity` digests.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Records one digest-to-session mapping, evicting the oldest entry.
    pub fn insert(&mut self, digest: String, session: String) {
        if self.entries.contains_key(&digest) {
            self.order.retain(|item| item != &digest);
        } else if self.entries.len() >= self.capacity
            && let Some(oldest) = self.order.first().cloned()
        {
            self.order.remove(0);
            self.entries.remove(&oldest);
        }
        self.order.push(digest.clone());
        self.entries.insert(digest, session);
    }

    /// Looks up the session for one digest, refreshing its recency.
    pub fn get(&mut self, digest: &str) -> Option<&str> {
        if !self.entries.contains_key(digest) {
            return None;
        }
        self.order.retain(|item| item != digest);
        self.order.push(digest.to_owned());
        self.entries.get(digest).map(String::as_str)
    }
}
