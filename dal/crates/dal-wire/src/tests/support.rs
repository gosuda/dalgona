//! A real [`Host`] on a temporary data root, driven by a replay script, plus
//! JSON-RPC and raw HTTP clients for the Host-backed wire tests.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dal_agent::error::ToolError;
use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_agent::ext::{BoxFuture, ExtensionBuilder};
use dal_agent::{Env, Host, Product};
use dal_core::{
    Config, ConfigProduct, ModelInfo, Name, Preview, RawJson, ServiceSet, ToolClass, ToolSpec,
    Visibility, Workspace,
};
use sonic_rs::{JsonValueTrait, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use crate::transport::MemoryPeer;

/// How long a test waits for any one frame or response.
pub(super) const WAIT: Duration = Duration::from_secs(10);

/// One started Host with its temporary directories and tool gate.
pub(super) struct Rig {
    /// Owns the temporary root; dropping it removes every file.
    pub(super) dir: tempfile::TempDir,
    /// The running host.
    pub(super) host: Host,
    /// The absolute session workspace.
    pub(super) workspace: PathBuf,
    /// Permits released to the `gate` tool; each permit ends one call.
    pub(super) gate: Arc<Semaphore>,
}

impl Rig {
    /// Returns the workspace as a JSON string value.
    pub(super) fn ws(&self) -> String {
        self.workspace.display().to_string()
    }

    /// Returns the core workspace value.
    pub(super) fn core_workspace(&self) -> Workspace {
        Workspace::new(self.workspace.clone()).expect("workspace is absolute")
    }

    /// Returns the data root.
    pub(super) fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }
}

/// Starts a Host whose provider serves `steps` in order.
pub(super) async fn rig(steps: &[String]) -> Rig {
    rig_with(steps, "").await
}

/// Starts a Host with extra user TOML appended after the scripted model.
pub(super) async fn rig_with(steps: &[String], extra: &str) -> Rig {
    rig_with_extensions(steps, extra, Vec::new()).await
}

/// Starts a Host with `more` extensions registered after the standard set.
pub(super) async fn rig_with_extensions(
    steps: &[String],
    extra: &str,
    more: Vec<dal_agent::ext::Extension>,
) -> Rig {
    let dir = tempfile::tempdir().expect("temporary root");
    let data = dir.path().join("data");
    let workspace = dir.path().join("w");
    std::fs::create_dir_all(&data).expect("data root");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, steps.join("\n")).expect("script fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n{extra}\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(&user)).expect("test config loads");
    let gate = Arc::new(Semaphore::new(0));
    let no_reload: Arc<dyn dal_ext::commands::PluginReload> = Arc::new(NoReload);
    let mut extensions = vec![
        dal_ext::commands::extension(&no_reload).expect("commands extension"),
        dal_ext::docs::extension().expect("docs extension"),
        tools_extension(&gate),
    ];
    extensions.extend(more);
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let mut vars = BTreeMap::new();
    vars.insert(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"));
    let env = Env {
        vars,
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env)
        .await
        .expect("host starts");
    Rig {
        dir,
        host,
        workspace,
        gate,
    }
}

/// Plugin reload is not part of the wire tests; the command reports that.
struct NoReload;

impl dal_ext::commands::PluginReload for NoReload {
    fn reload<'a>(
        &'a self,
        _cx: &'a dal_agent::ext::command::CommandCx<'a>,
    ) -> BoxFuture<'a, Result<dal_agent::ext::command::ReloadSummary, dal_core::command::ErrorTriple>>
    {
        Box::pin(async {
            Err(dal_core::command::ErrorTriple {
                what: "plugins cannot reload".into(),
                why: "the wire test host loads no plugins".into(),
                fix: "Run the product binary to reload plugins.".into(),
            })
        })
    }
}

/// Builds the test extension holding the `gate` and `ask` tools.
fn tools_extension(gate: &Arc<Semaphore>) -> dal_agent::ext::Extension {
    ExtensionBuilder::new("wiretest", "0.0.0", ServiceSet::EMPTY)
        .expect("extension name")
        .tool(Arc::new(GateTool::new(Arc::clone(gate))), Visibility::Model)
        .tool(Arc::new(AskTool::new()), Visibility::Model)
        .build()
        .expect("test extension builds")
}

/// An edit-class tool whose approval pauses the turn for a client answer.
struct AskTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl AskTool {
    fn new() -> Self {
        let name = Name::parse("ask").expect("tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Edit-class marker tool; asks for approval.".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("schema json"),
            grammar: None,
        });
        Self { name, spec }
    }
}

impl Tool for AskTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Patch)
    }

    fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let preview = Preview {
                title: "ask".into(),
                body: String::new().into(),
                digest: None,
            };
            match cx.authorize(preview).await {
                Ok(_) => ToolOutcome::Ok(Box::new(ToolOutput::from_text("asked-ok"))),
                Err(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
            }
        })
    }
}

/// A read-class tool whose call ends when the test releases a permit or the
/// turn is cancelled.
struct GateTool {
    name: Name,
    spec: Arc<ToolSpec>,
    gate: Arc<Semaphore>,
}

impl GateTool {
    fn new(gate: Arc<Semaphore>) -> Self {
        let name = Name::parse("gate").expect("tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Waits until the test opens the gate.".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("schema json"),
            grammar: None,
        });
        Self { name, spec, gate }
    }
}

impl Tool for GateTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            tokio::select! {
                permit = self.gate.acquire() => match permit {
                    Ok(permit) => {
                        permit.forget();
                        ToolOutcome::Ok(Box::new(ToolOutput::from_text("opened")))
                    }
                    Err(_) => ToolOutcome::Interrupted,
                },
                () = cx.cancel().cancelled() => ToolOutcome::Interrupted,
            }
        })
    }
}

/// One replay step streaming `chunks` as text, then `end_turn`.
pub(super) fn text_step(chunks: &[&str], input: u64, output: u64) -> String {
    let mut events: Vec<Value> = chunks
        .iter()
        .map(|text| sonic_rs::json!({"type": "text_delta", "text": *text}))
        .collect();
    events.push(sonic_rs::json!({"type": "tool_calls_done", "calls": []}));
    events.push(usage_event(input, output));
    events.push(sonic_rs::json!({"type": "stop", "reason": "end_turn"}));
    sonic_rs::to_string(&sonic_rs::json!({"kind": "events", "events": events})).expect("step json")
}

/// One replay step that calls the `gate` tool and stops for tool use.
pub(super) fn gate_step(call: &str) -> String {
    tool_step(call, "gate")
}

/// One replay step that calls the tool `name` and stops for tool use.
pub(super) fn tool_step(call: &str, name: &str) -> String {
    let events = sonic_rs::json!([
        {"type": "tool_call_started", "id": call, "name": name},
        {"type": "tool_calls_done", "calls": [
            {"id": call, "name": name, "args": {"kind": "parsed", "value": {}}}
        ]},
        usage_event(1, 1),
        {"type": "stop", "reason": "tool_use"},
    ]);
    sonic_rs::to_string(&sonic_rs::json!({"kind": "events", "events": events})).expect("step json")
}

fn usage_event(input: u64, output: u64) -> Value {
    sonic_rs::json!({"type": "usage", "usage": {
        "input_tokens": input,
        "cached_input_tokens": 0,
        "output_tokens": output,
        "reasoning_tokens": null,
        "cache_write_tokens": 0,
        "cost_usd": null,
    }})
}

/// A JSON-RPC peer that keeps notifications read while waiting for a reply.
pub(super) struct Rpc {
    peer: MemoryPeer,
    stash: VecDeque<Value>,
}

impl Rpc {
    pub(super) fn new(peer: MemoryPeer) -> Self {
        Self {
            peer,
            stash: VecDeque::new(),
        }
    }

    /// Sends one raw frame.
    pub(super) async fn send_raw(&self, frame: &str) {
        self.peer
            .send_frame(frame.to_owned())
            .await
            .expect("peer send");
    }

    /// Sends one request without waiting for its reply.
    pub(super) async fn send(&self, id: i64, method: &str, params: Value) {
        let frame =
            sonic_rs::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send_raw(&sonic_rs::to_string(&frame).expect("frame json"))
            .await;
    }

    /// Splits the peer after setup so a flood can send and receive
    /// independently in one select loop.
    pub(super) fn into_parts(
        self,
    ) -> (
        tokio::sync::mpsc::Sender<String>,
        tokio::sync::mpsc::Receiver<String>,
    ) {
        self.peer.into_parts()
    }

    /// Sends one notification.
    pub(super) async fn notify(&self, method: &str, params: Value) {
        let frame = sonic_rs::json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.send_raw(&sonic_rs::to_string(&frame).expect("frame json"))
            .await;
    }

    /// Reads the next raw frame from the server.
    pub(super) async fn raw(&mut self) -> String {
        tokio::time::timeout(WAIT, self.peer.read_frame())
            .await
            .expect("frame in time")
            .expect("server transport open")
    }

    /// Reads the next frame, preferring stashed ones.
    pub(super) async fn next(&mut self) -> Value {
        if let Some(value) = self.stash.pop_front() {
            return value;
        }
        let frame = self.raw().await;
        sonic_rs::from_str(&frame).expect("server frame is JSON")
    }

    /// Reads frames until the reply to `id`, stashing everything else.
    pub(super) async fn reply(&mut self, id: i64) -> Value {
        if let Some(index) = self
            .stash
            .iter()
            .position(|value| value.get("id").and_then(Value::as_i64) == Some(id))
        {
            return self.stash.remove(index).expect("stashed reply");
        }
        loop {
            let frame = self.raw().await;
            let value: Value = sonic_rs::from_str(&frame).expect("server frame is JSON");
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                return value;
            }
            self.stash.push_back(value);
        }
    }

    /// Sends one request and returns its reply.
    pub(super) async fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(id, method, params).await;
        self.reply(id).await
    }

    /// Returns the frames read so far that were not replies.
    pub(super) fn stashed(&self) -> &VecDeque<Value> {
        &self.stash
    }

    /// Asserts that nothing is waiting to be read.
    pub(super) async fn assert_quiet(&mut self, wait: Duration) {
        assert!(self.stash.is_empty(), "unexpected frames: {:?}", self.stash);
        let pending = tokio::time::timeout(wait, self.peer.read_frame()).await;
        assert!(pending.is_err(), "unexpected frame: {pending:?}");
    }
}

/// Asserts one JSON-RPC error reply's code and message.
#[track_caller]
pub(super) fn assert_error(reply: &Value, code: i64, message: &str) {
    assert_eq!(
        reply["error"]["code"].as_i64(),
        Some(code),
        "error code in {reply}"
    );
    assert_eq!(
        reply["error"]["message"].as_str(),
        Some(message),
        "error message in {reply}"
    );
}

/// Asserts a `-32602` refusal scoped to `method` that names the bad value.
#[track_caller]
pub(super) fn assert_invalid_params(reply: &Value, method: &str, bad: &str) {
    assert_eq!(reply["error"]["code"].as_i64(), Some(-32602), "{reply}");
    let message = reply["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with(&format!("invalid params for {method}: ")),
        "{reply}"
    );
    assert!(message.contains(bad), "{reply}");
}

/// Returns a successful reply's result, failing on an error reply.
#[track_caller]
pub(super) fn result(reply: &Value) -> &Value {
    assert!(reply.get("error").is_none(), "error reply: {reply}");
    &reply["result"]
}

/// Initializes a dal RPC connection with every capability.
pub(super) async fn initialize(rpc: &mut Rpc) -> Value {
    let reply = rpc
        .call(
            0,
            "initialize",
            sonic_rs::json!({
                "protocolVersion": 1,
                "clientInfo": {"name": "test", "version": "1"},
                "capabilities": ["sessions", "host.updates", "blobs", "docs", "auth", "models"],
            }),
        )
        .await;
    result(&reply).clone()
}

/// One raw HTTP/1.1 response.
#[derive(Debug)]
pub(super) struct HttpReply {
    pub(super) status: u16,
    pub(super) head: String,
    pub(super) body: String,
}

impl HttpReply {
    /// Returns one header value by lowercase name.
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    /// Decodes the body as JSON.
    pub(super) fn json(&self) -> Value {
        sonic_rs::from_str(&self.body).expect("response body is JSON")
    }
}

/// Sends one request over a fresh connection and reads the whole response.
///
/// `head` holds the request line and headers without the blank line; the
/// helper adds `connection: close` and the body length.
pub(super) async fn http(addr: std::net::SocketAddr, head: &str, body: &str) -> HttpReply {
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to serve");
    let request = format!(
        "{head}\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request write");
    read_reply(&mut stream).await
}

/// Reads one full response until the server closes the connection.
pub(super) async fn read_reply(stream: &mut tokio::net::TcpStream) -> HttpReply {
    let mut bytes = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut bytes))
        .await
        .expect("response in time")
        .expect("response read");
    parse_reply(&bytes)
}

/// Splits raw response bytes, decoding a chunked body.
pub(super) fn parse_reply(bytes: &[u8]) -> HttpReply {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let (head, rest) = text.split_once("\r\n\r\n").expect("response head");
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status code");
    let chunked = head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
    let body = if chunked {
        dechunk(rest)
    } else {
        rest.to_owned()
    };
    HttpReply {
        status,
        head: head.to_owned(),
        body,
    }
}

fn dechunk(mut rest: &str) -> String {
    let mut body = String::new();
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || tail.len() < size {
            break;
        }
        body.push_str(&tail[..size]);
        rest = tail[size..].trim_start_matches("\r\n");
    }
    body
}

/// Returns the loopback `host` header for one bound address.
pub(super) fn host_header(addr: std::net::SocketAddr) -> String {
    format!("host: 127.0.0.1:{}", addr.port())
}

/// Reads the text of every `data:` line of an SSE body.
pub(super) fn sse_data(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(str::to_owned)
        .collect()
}

/// Lists every directory below `root` named exactly `name`.
pub(super) fn find_dirs(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if entry.file_name().to_string_lossy() == name {
                found.push(path);
            } else {
                stack.push(path);
            }
        }
    }
    found
}

/// Loopback router options at an ephemeral port for one rig.
pub(super) fn router_options(rig: &Rig) -> crate::router::RouterOptions {
    crate::router::RouterOptions {
        bind: "127.0.0.1".to_owned(),
        port: 0,
        public: false,
        a2a: false,
        token_file: rig.data().join("serve.token"),
        approval: dal_core::ApprovalMode::Ask,
        origins: Vec::new(),
        aliases: BTreeMap::new(),
        workspace: rig.workspace.clone(),
    }
}

/// Serves `options` and runs `client` while the listener's wait is polled.
pub(super) async fn with_serve<F>(
    rig: &Rig,
    options: crate::router::RouterOptions,
    client: impl FnOnce(std::net::SocketAddr) -> F,
) where
    F: Future<Output = ()>,
{
    let stop = tokio_util::sync::CancellationToken::new();
    let handle = crate::serve_router(rig.host.clone(), options, stop.clone())
        .await
        .expect("serve starts");
    let addr = handle.local_addr();
    let driven = async {
        client(addr).await;
        stop.cancel();
    };
    let (waited, ()) = tokio::join!(handle.wait(), driven);
    waited.expect("serve drains cleanly");
}

/// Reopens `session` and returns the text of its result for the tool `name`
/// whose error flag equals `error`; panics with the journal entries when no
/// such result exists.
pub(super) async fn result_text(rig: &Rig, session: &str, name: &str, error: bool) -> String {
    let agent = rig
        .host
        .open(
            dal_agent::SessionRef::Resume {
                key: session.into(),
                workspace: rig.core_workspace(),
            },
            crate::protocol::mint_client_id("test"),
        )
        .await
        .expect("the session reopens");
    let view = agent
        .view(dal_core::PageReq::default())
        .expect("session view");
    view.entries
        .items
        .iter()
        .find_map(|entry| match &entry.kind {
            dal_core::EntryKind::ToolResult {
                name: tool,
                error: failed,
                parts,
                ..
            } if tool.as_ref() == name && *failed == error => {
                parts.iter().find_map(|part| match part {
                    dal_core::JournalPart::Text { text } => Some(text.to_string()),
                    _ => None,
                })
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "no result for tool {name} with error={error}: {:?}",
                view.entries.items
            )
        })
}

/// Reopens `session` and returns the text of its error result for the tool
/// `name`; panics with the journal entries when no such result exists.
pub(super) async fn denied_result_text(rig: &Rig, session: &str, name: &str) -> String {
    let agent = rig
        .host
        .open(
            dal_agent::SessionRef::Resume {
                key: session.into(),
                workspace: rig.core_workspace(),
            },
            crate::protocol::mint_client_id("test"),
        )
        .await
        .expect("the session reopens");
    let view = agent
        .view(dal_core::PageReq::default())
        .expect("session view");
    view.entries
        .items
        .iter()
        .find_map(|entry| match &entry.kind {
            dal_core::EntryKind::ToolResult {
                name: tool,
                error: true,
                parts,
                ..
            } if tool.as_ref() == name => parts.iter().find_map(|part| match part {
                dal_core::JournalPart::Text { text } => Some(text.to_string()),
                _ => None,
            }),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "the denied call left an error result: {:?}",
                view.entries.items
            )
        })
}

/// An extension whose `probe_ask` tool raises one text question through the
/// `ask` service and reports `answered` or `default` as its result text.
pub(super) fn ask_probe_extension() -> dal_agent::ext::Extension {
    let inject = ServiceSet::from_names(["ask"]).expect("ask service");
    ExtensionBuilder::new("askprobe", "0.0.0", inject)
        .expect("extension name")
        .tool(Arc::new(AskProbe::new()), Visibility::Model)
        .build()
        .expect("ask probe extension builds")
}

struct AskProbe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl AskProbe {
    fn new() -> Self {
        let name = Name::parse("probe_ask").expect("tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Asks one question through the ask service.".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("schema json"),
            grammar: None,
        });
        Self { name, spec }
    }
}

impl Tool for AskProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let question = dal_core::Question::Text {
                prompt: "who?".into(),
                placeholder: None,
            };
            let text = match cx.services().ask(cx.caller(), question).await {
                Ok(Some(_)) => "answered",
                Ok(None) => "default",
                Err(_) => "failed",
            };
            ToolOutcome::Ok(Box::new(ToolOutput::from_text(text)))
        })
    }
}
