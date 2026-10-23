//! Wire ownership proofs: auth, heartbeat, reconnect, router, ACP,
//! Codex, and A2A.
//!
//! Covers the wire-surfaces-part contracts at the public API
//! boundary: token lifecycle, HTTP dispatch and auth, version selection for
//! ACP, the pinned Codex subset, A2A gating, WebSocket keepalive and close
//! codes, and `RemoteHost` identity plus reconnect. Every proof drives the
//! real serve/router/adapter code over loopback sockets or memory
//! transports; no network beyond localhost, temp dirs for all state.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dal_agent::{Env, Host, Product};
use dal_core::{ApprovalMode, Config, ConfigProduct};
use dal_wire::router::RouterOptions;
use dal_wire::token::{TokenError, create_token, load_token};
#[cfg(unix)]
use dal_wire::{ConnectionFuture, LocalSocketError, default_rpc_path, serve_local};
use dal_wire::{
    MemoryPeer, MemoryTransport, RemoteEndpoint, RemoteHost, Transport, serve_acp, serve_codex,
    serve_router, serve_rpc,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

type Result<T, E = Box<dyn Error + Send + Sync>> = std::result::Result<T, E>;

/// How long a test waits for any one frame, reply, or connection.
const WAIT: Duration = Duration::from_secs(10);

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// --- Host rig: a real Host on a temp root with a scripted provider --------

struct Rig {
    _dir: tempfile::TempDir,
    host: Host,
    workspace: PathBuf,
    data: PathBuf,
}

fn text_step(chunks: &[&str], input: u64, output: u64) -> Result<String> {
    let mut events: Vec<Value> = chunks
        .iter()
        .map(|text| sonic_rs::json!({"type": "text_delta", "text": *text}))
        .collect();
    events.push(sonic_rs::json!({"type": "tool_calls_done", "calls": []}));
    events.push(sonic_rs::json!({"type": "usage", "usage": {
        "input_tokens": input,
        "cached_input_tokens": 0,
        "output_tokens": output,
        "reasoning_tokens": null,
        "cache_write_tokens": 0,
        "cost_usd": null,
    }}));
    events.push(sonic_rs::json!({"type": "stop", "reason": "end_turn"}));
    let step = sonic_rs::json!({"kind": "events", "events": events});
    Ok(sonic_rs::to_string(&step)?)
}

async fn rig(steps: &[String]) -> Result<Rig> {
    let dir = tempfile::tempdir()?;
    let data = dir.path().join("data");
    let workspace = dir.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, steps.join("\n"))?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;
    let no_reload: std::sync::Arc<dyn dal_ext::commands::PluginReload> =
        std::sync::Arc::new(NoReload);
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![
            dal_ext::commands::extension(&no_reload)?,
            dal_ext::docs::extension()?,
        ],
        bundled: Vec::new(),
    };
    let mut vars = BTreeMap::new();
    vars.insert(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"));
    let env = Env {
        vars,
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    Ok(Rig {
        _dir: dir,
        host,
        workspace,
        data,
    })
}

/// Plugin reload is out of scope for the wire contracts; the seam reports it.
struct NoReload;

impl dal_ext::commands::PluginReload for NoReload {
    fn reload<'a>(
        &'a self,
        _cx: &'a dal_agent::ext::command::CommandCx<'a>,
    ) -> dal_agent::ext::BoxFuture<
        'a,
        Result<dal_agent::ext::command::ReloadSummary, dal_core::command::ErrorTriple>,
    > {
        Box::pin(async {
            Err(dal_core::command::ErrorTriple {
                what: "plugins cannot reload".into(),
                why: "the wire contract host loads no plugins".into(),
                fix: "Run the product binary to reload plugins.".into(),
            })
        })
    }
}

fn router_options(rig: &Rig) -> RouterOptions {
    RouterOptions {
        bind: "127.0.0.1".to_owned(),
        port: 0,
        public: false,
        a2a: false,
        token_file: rig.data.join("serve.token"),
        approval: ApprovalMode::Ask,
        origins: Vec::new(),
        aliases: BTreeMap::new(),
        workspace: rig.workspace.clone(),
    }
}

/// Serves `options`, runs `client`, then cancels and drains the listener.
async fn with_serve<F>(
    rig: &Rig,
    options: RouterOptions,
    client: impl FnOnce(SocketAddr) -> F,
) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    let stop = CancellationToken::new();
    let handle = serve_router(rig.host.clone(), options, stop.clone()).await?;
    let addr = handle.local_addr();
    let driven = async {
        let outcome = client(addr).await;
        stop.cancel();
        outcome
    };
    let (waited, outcome) = tokio::join!(handle.wait(), driven);
    waited?;
    outcome
}

// --- Raw HTTP: one request per fresh connection ----------------------------

#[derive(Debug)]
struct HttpReply {
    status: u16,
    head: String,
    body: String,
}

impl HttpReply {
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn json(&self) -> Result<Value> {
        Ok(sonic_rs::from_str(&self.body)?)
    }
}

fn host_header(addr: SocketAddr) -> String {
    format!("host: 127.0.0.1:{}", addr.port())
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
        rest = &tail[size..];
        let Some(stripped) = rest.strip_prefix("\r\n") else {
            break;
        };
        rest = stripped;
    }
    body
}

fn parse_reply(bytes: &[u8]) -> Result<HttpReply> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let (head, rest) = text
        .split_once("\r\n\r\n")
        .ok_or("response has no head terminator")?;
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or("response has no status code")?;
    let chunked = head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
    let body = if chunked {
        dechunk(rest)
    } else {
        rest.to_owned()
    };
    Ok(HttpReply {
        status,
        head: head.to_owned(),
        body,
    })
}

async fn http(addr: SocketAddr, head: &str, body: &str) -> Result<HttpReply> {
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    let request = format!(
        "{head}\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut bytes = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut bytes)).await??;
    parse_reply(&bytes)
}

async fn get(addr: SocketAddr, path: &str) -> Result<HttpReply> {
    http(
        addr,
        &format!("GET {path} HTTP/1.1\r\n{}", host_header(addr)),
        "",
    )
    .await
}

async fn post_json(addr: SocketAddr, path: &str, extra: &str, body: &str) -> Result<HttpReply> {
    http(
        addr,
        &format!(
            "POST {path} HTTP/1.1\r\n{}\r\ncontent-type: application/json{extra}",
            host_header(addr)
        ),
        body,
    )
    .await
}

// --- Authentication ownership: token.rs creates, loads, compares -----------

#[test]
fn token_file_create_load_contract() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("serve.token");

    let token = create_token(&path, false)?;
    assert!(token.starts_with("dal_"), "token carries the dal prefix");
    assert_eq!(token.len(), 68, "dal_ plus 64 hex characters");
    assert!(
        token
            .bytes()
            .skip(4)
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "the suffix is lowercase hex"
    );
    let bytes = std::fs::read(&path)?;
    assert_eq!(bytes, format!("{token}\n").as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600,
            "the token file is owner-only"
        );
    }

    let loaded = load_token(&path)?;
    let redacted = format!("{loaded:?}");
    assert!(
        !redacted.contains(&token),
        "debug never leaks the token: {redacted}"
    );

    let before = bytes.clone();
    let refused = create_token(&path, false);
    assert!(
        matches!(refused, Err(TokenError::AlreadyExists { .. })),
        "second create without force fails: {refused:?}"
    );
    assert_eq!(
        std::fs::read(&path)?,
        before,
        "refusal leaves the file unchanged"
    );

    let rotated = create_token(&path, true)?;
    assert_ne!(rotated, token, "force mints a fresh token");

    let relative = create_token(Path::new("relative.token"), true);
    assert!(
        relative.is_err(),
        "token paths must be absolute: {relative:?}"
    );

    std::fs::write(&path, "dal_nothex!!\n")?;
    assert!(
        load_token(&path).is_err(),
        "a malformed first line fails to load"
    );
    std::fs::write(&path, "\n")?;
    assert!(load_token(&path).is_err(), "an empty file fails to load");
    Ok(())
}

// --- Router ownership: dispatch table, model errors, model list ------------

#[tokio::test]
async fn router_unknown_path_and_method_are_typed() -> Result<()> {
    let rig = rig(&[]).await?;
    with_serve(&rig, router_options(&rig), async |addr| {
        let missing = get(addr, "/v1/nope").await?;
        assert_eq!(missing.status, 404);
        assert_eq!(missing.body, "no route for GET /v1/nope");

        let wrong = get(addr, "/v1/chat/completions").await?;
        assert_eq!(wrong.status, 405);
        assert_eq!(wrong.header("allow"), Some("POST"));
        assert_eq!(
            wrong.body,
            "GET /v1/chat/completions is not allowed: use POST"
        );

        // The query string is stripped before matching.
        let queried = get(addr, "/v1/models?fresh=true").await?;
        assert_eq!(queried.status, 200);

        // A2A routes are absent unless mounted.
        let a2a = post_json(addr, "/a2a", "", "{}").await?;
        assert_eq!(a2a.status, 404);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn router_model_errors_are_typed() -> Result<()> {
    let rig = rig(&[]).await?;
    with_serve(&rig, router_options(&rig), async |addr| {
        let missing = post_json(
            addr,
            "/v1/chat/completions",
            "",
            r#"{"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await?;
        assert_eq!(missing.status, 400);
        assert_eq!(
            missing.json()?["error"]["code"].as_str(),
            Some("invalid_request")
        );
        assert_eq!(
            missing.json()?["error"]["message"].as_str(),
            Some("field \"model\" is required")
        );

        let unknown = post_json(
            addr,
            "/v1/chat/completions",
            "",
            r#"{"model":"dalgon/nope","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await?;
        assert_eq!(unknown.status, 400);
        assert_eq!(
            unknown.json()?["error"]["code"].as_str(),
            Some("model_not_found")
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn router_models_list_serves_harness_ids_first() -> Result<()> {
    let rig = rig(&[]).await?;
    with_serve(&rig, router_options(&rig), async |addr| {
        let listed = get(addr, "/v1/models").await?;
        assert_eq!(listed.status, 200);
        let body = listed.json()?;
        let ids: Vec<&str> = body["data"]
            .as_array()
            .ok_or("models list has a data array")?
            .iter()
            .filter_map(|row| row["id"].as_str())
            .collect();
        assert_eq!(
            &ids[..3],
            ["dalgon/normal", "dalgon/eval-first", "dalgon/eval-only"],
            "harness ids sort first: {ids:?}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn router_harness_chat_completes_with_scripted_text() -> Result<()> {
    let rig = rig(&[text_step(&["Hello"], 10, 5)?]).await?;
    with_serve(&rig, router_options(&rig), async |addr| {
        let chat = post_json(
            addr,
            "/v1/chat/completions",
            "",
            r#"{"model":"dalgon/normal","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await?;
        assert_eq!(chat.status, 200, "body: {}", chat.body);
        let body = chat.json()?;
        assert_eq!(
            body["choices"][0]["message"]["content"].as_str(),
            Some("Hello"),
            "the harness turn reaches the scripted provider: {}",
            chat.body
        );
        Ok(())
    })
    .await
}

// --- Serve auth boundaries over HTTP ----------------------------------------

#[tokio::test]
async fn serve_public_token_boundaries() -> Result<()> {
    let rig = rig(&[]).await?;
    let path = rig.data.join("serve.token");
    let token = create_token(&path, false)?;
    let mut options = router_options(&rig);
    options.public = true;
    options.a2a = true;
    with_serve(&rig, options, async |addr| {
        let base = format!("GET /v1/models HTTP/1.1\r\n{}", host_header(addr));

        let none = http(addr, &base, "").await?;
        assert_eq!(none.status, 401);
        assert_eq!(none.json()?["error"]["code"].as_str(), Some("missing_token"));
        assert_eq!(
            none.json()?["error"]["message"].as_str(),
            Some("dalgon serve needs a bearer token: send Authorization: Bearer <token from serve.token>")
        );

        let wrong = http(addr, &format!("{base}\r\nauthorization: Bearer dal_wrong"), "").await?;
        assert_eq!(wrong.status, 401);
        assert_eq!(
            wrong.json()?["error"]["message"].as_str(),
            Some("the bearer token does not match serve.token")
        );

        let bearer = http(addr, &format!("{base}\r\nauthorization: Bearer {token}"), "").await?;
        assert_eq!(bearer.status, 200, "{bearer:?}");

        let key = http(addr, &format!("{base}\r\nx-api-key: {token}"), "").await?;
        assert_eq!(key.status, 200, "{key:?}");

        // The agent card stays the one unauthenticated public route.
        let card = get(addr, "/.well-known/agent-card.json").await?;
        assert_eq!(card.status, 200, "{card:?}");
        assert_eq!(card.json()?["name"].as_str(), Some("dal"));
        Ok(())
    })
    .await
}

// --- A2A ownership: version gate, card, disabled mount -----------------------

#[tokio::test]
async fn a2a_version_gate_and_card() -> Result<()> {
    let rig = rig(&[]).await?;
    let mut options = router_options(&rig);
    options.a2a = true;
    with_serve(&rig, options, async |addr| {
        let card = get(addr, "/.well-known/agent-card.json").await?;
        assert_eq!(card.status, 200);
        let body = card.json()?;
        assert_eq!(body["name"].as_str(), Some("dal"));
        assert_eq!(body["version"].as_str(), Some(version()));
        let interfaces = body["supportedInterfaces"]
            .as_array()
            .ok_or("card has supportedInterfaces")?;
        assert_eq!(interfaces.len(), 2, "JSONRPC plus HTTP+JSON: {body}");

        // No version selected: the JSON-RPC version failure, not a hang.
        let unversioned = post_json(
            addr,
            "/a2a",
            "",
            r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{}}"#,
        )
        .await?;
        assert!(
            unversioned.body.contains("-32009"),
            "version failure: {}",
            unversioned.body
        );

        // The query selector also negotiates 1.0.
        let query = post_json(
            addr,
            "/a2a?A2A-Version=1.0",
            "",
            r#"{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{}}"#,
        )
        .await?;
        assert!(
            !query.body.contains("-32009"),
            "query version negotiates: {}",
            query.body
        );
        Ok(())
    })
    .await
}

// --- JSON-RPC framing over memory transports --------------------------------

/// Sends one JSON-RPC request on `peer` and returns the matching reply,
/// skipping server notifications.
async fn rpc_call(peer: &mut MemoryPeer, id: i64, method: &str, params: Value) -> Result<Value> {
    let request = sonic_rs::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    peer.send_frame(sonic_rs::to_string(&request)?).await?;
    loop {
        let frame = tokio::time::timeout(WAIT, peer.read_frame())
            .await?
            .ok_or("transport closed before the reply")?;
        let value: Value = sonic_rs::from_str(&frame)?;
        if value["id"].as_i64() == Some(id) {
            return Ok(value);
        }
    }
}

fn init_params(version: i64, caps: &[&str]) -> Value {
    sonic_rs::json!({
        "protocolVersion": version,
        "clientInfo": {"name": "probe", "version": "0"},
        "capabilities": caps,
    })
}

#[tokio::test]
async fn rpc_initialize_gates_and_trims() -> Result<()> {
    let rig = rig(&[]).await?;
    let (transport, mut peer) = MemoryTransport::pair(64);
    let server = serve_rpc(rig.host.clone(), transport);
    let client = async move {
        let early = rpc_call(&mut peer, 1, "session/list", sonic_rs::json!({})).await?;
        assert_eq!(early["error"]["code"].as_i64(), Some(-32006));
        assert_eq!(
            early["error"]["message"].as_str(),
            Some("initialize must be the first request")
        );

        let negotiated = rpc_call(
            &mut peer,
            2,
            "initialize",
            init_params(3, &["sessions", "zzz"]),
        )
        .await?;
        assert_eq!(negotiated["result"]["protocolVersion"].as_i64(), Some(1));
        let caps: Vec<&str> = negotiated["result"]["capabilities"]
            .as_array()
            .ok_or("initialize returns capabilities")?
            .iter()
            .filter_map(|cap| cap.as_str())
            .collect();
        assert_eq!(
            caps,
            ["sessions"],
            "unknown names are ignored: {negotiated}"
        );
        let id = negotiated["result"]["clientId"]
            .as_str()
            .ok_or("client id minted")?;
        assert!(id.starts_with("probe#"), "client id carries the name: {id}");

        let twice = rpc_call(&mut peer, 3, "initialize", init_params(1, &["sessions"])).await?;
        assert_eq!(twice["error"]["code"].as_i64(), Some(-32600));
        assert_eq!(
            twice["error"]["message"].as_str(),
            Some("initialize was already called")
        );

        let refused = rpc_call(&mut peer, 4, "initialize", init_params(0, &[])).await?;
        assert_eq!(refused["error"]["code"].as_i64(), Some(-32600));
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };
    let (outcome, result) = tokio::join!(server, client);
    outcome?;
    result
}

// --- ACP ownership: version selection, init bytes, refusals -----------------

async fn acp_call(peer: &mut MemoryPeer, id: i64, method: &str, params: Value) -> Result<Value> {
    rpc_call(peer, id, method, params).await
}

fn v1_init_frame() -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":1,"agentCapabilities":{{"loadSession":true,"promptCapabilities":{{"image":true,"audio":false,"embeddedContext":true}},"mcpCapabilities":{{"http":false,"sse":false}},"sessionCapabilities":{{"list":{{}},"resume":{{}},"close":{{}}}}}},"authMethods":[],"agentInfo":{{"name":"dal","title":"dal","version":"{}"}}}}}}"#,
        version()
    )
}

fn v2_init_frame(id: i64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":2,"capabilities":{{"session":{{"prompt":{{"image":{{}},"embeddedContext":{{}}}}}}}},"info":{{"name":"dal","title":"dal","version":"{}"}},"authMethods":[]}}}}"#,
        version()
    )
}

#[tokio::test]
async fn acp_version_selection_and_refusals() -> Result<()> {
    let rig_v1 = rig(&[]).await?;
    let (transport, mut peer) = MemoryTransport::pair(64);
    let server = serve_acp(rig_v1.host.clone(), transport);
    let client = async move {
        let v1 = acp_call(
            &mut peer,
            1,
            "initialize",
            sonic_rs::json!({"protocolVersion": 1, "clientInfo": {"name": "zed"}}),
        )
        .await?;
        assert_eq!(
            sonic_rs::to_string(&v1)?,
            v1_init_frame(),
            "v1 init is byte-exact"
        );

        let unknown = acp_call(&mut peer, 2, "session/set_mode", sonic_rs::json!({})).await?;
        assert_eq!(unknown["error"]["code"].as_i64(), Some(-32601));

        let relative = acp_call(
            &mut peer,
            3,
            "session/new",
            sonic_rs::json!({"cwd": "relative/path", "mcpServers": []}),
        )
        .await?;
        assert_eq!(relative["error"]["code"].as_i64(), Some(-32602));
        assert_eq!(
            relative["error"]["message"].as_str(),
            Some("invalid params for session/new: cwd must be an absolute path")
        );
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };
    let (outcome, result) = tokio::join!(server, client);
    outcome?;
    result?;

    // Any other version selects the latest branch, v2.
    let rig2 = rig(&[]).await?;
    let (transport, mut peer) = MemoryTransport::pair(64);
    let server = serve_acp(rig2.host.clone(), transport);
    let client = async move {
        let v2 = acp_call(
            &mut peer,
            9,
            "initialize",
            sonic_rs::json!({"protocolVersion": 7, "clientInfo": {"name": "zed"}}),
        )
        .await?;
        assert_eq!(
            sonic_rs::to_string(&v2)?,
            v2_init_frame(9),
            "protocol 7 negotiates v2"
        );
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };
    let (outcome, result) = tokio::join!(server, client);
    outcome?;
    result
}

// --- Codex ownership: pinned subset, no jsonrpc field ------------------------

#[tokio::test]
async fn codex_subset_rejects_unknown_without_jsonrpc() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path().join("w");
    std::fs::create_dir_all(&workspace)?;
    let product = Product {
        name: "dal",
        data_root: dir.path().join("data"),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace,
        sandbox_helper: None,
    };
    let config = Config::load(ConfigProduct::Dalgon, &dir.path().join("data"), "", None)?;
    let host = Host::start(product, config, env).await?;

    let (transport, mut peer) = MemoryTransport::pair(16);
    let server = serve_codex(host, transport);
    let client = async move {
        for (id, method) in [(1, "thread/fork"), (2, "message/send")] {
            let frame = sonic_rs::json!({"id": id, "method": method, "params": {}});
            peer.send_frame(sonic_rs::to_string(&frame)?).await?;
            let reply = tokio::time::timeout(WAIT, peer.read_frame())
                .await?
                .ok_or("transport closed before the reply")?;
            assert!(
                !reply.contains("jsonrpc"),
                "codex frames omit jsonrpc: {reply}"
            );
            let value: Value = sonic_rs::from_str(&reply)?;
            assert_eq!(value["id"].as_i64(), Some(id));
            assert_eq!(value["error"]["code"].as_i64(), Some(-32601), "{value}");
        }
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };
    let (outcome, result) = tokio::join!(server, client);
    outcome?;
    result
}

// --- WebSocket ownership: heartbeat pings, binary close ---------------------

async fn ws_client(
    addr: SocketAddr,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let url = format!("ws://127.0.0.1:{}/v1/ws", addr.port());
    let (stream, _) = tokio_tungstenite::connect_async(&url).await?;
    Ok(stream)
}

#[tokio::test]
async fn websocket_binary_frame_closes_1003() -> Result<()> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let rig = rig(&[]).await?;
    with_serve(&rig, router_options(&rig), async |addr| {
        // The same method table as RPC: initialize answers over the socket.
        let mut rpc = ws_client(addr).await?;
        rpc.send(Message::Text(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"probe","version":"0"},"capabilities":[]}}"#.into(),
        ))
        .await?;
        let reply = tokio::time::timeout(WAIT, rpc.next()).await?.ok_or("no reply")??;
        let text = reply.into_text()?;
        assert!(text.contains("\"protocolVersion\":1"), "{text}");

        // Binary frames close the connection with code 1003.
        let mut raw = ws_client(addr).await?;
        raw.send(Message::Binary(vec![1u8, 2, 3].into())).await?;
        let close = tokio::time::timeout(WAIT, raw.next()).await?.ok_or("no close")??;
        match close {
            Message::Close(Some(frame)) => assert_eq!(
                u16::from(frame.code),
                1003,
                "binary close code: {:?}",
                frame.code
            ),
            other => return Err(format!("expected a close frame, got {other:?}").into()),
        }
        Ok(())
    })
    .await
}

#[tokio::test]
async fn websocket_server_pings_an_idle_client() -> Result<()> {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let rig = rig(&[]).await?;
    with_serve(&rig, router_options(&rig), async |addr| {
        let mut client = ws_client(addr).await?;
        // The 15-second ping cadence keeps an idle connection alive: one
        // ping must arrive well inside the 45-second pong deadline.
        let frame = tokio::time::timeout(Duration::from_secs(25), client.next())
            .await?
            .ok_or("the socket closed before any frame")??;
        assert!(
            matches!(frame, Message::Ping(_)),
            "first idle frame is a ping: {frame:?}"
        );
        Ok(())
    })
    .await
}

// --- Reconnect ownership: identity, backoff, resubscribe ---------------------

/// One `initialize` observed by the scripted fake RPC server.
#[derive(Debug)]
struct ObservedInit {
    name: String,
    capabilities: Vec<String>,
}

/// Answers one handshake, records the client identity, and returns the
/// result every `session/list` caller needs.
fn init_reply(id: &Value) -> Value {
    sonic_rs::json!({"jsonrpc": "2.0", "id": id, "result": {
        "protocolVersion": 1,
        "serverInfo": {"name": "dal", "version": "0"},
        "capabilities": [],
        "clientId": "fake#1",
    }})
}

fn record_init(frame: &Value) -> ObservedInit {
    let name = frame["params"]["clientInfo"]["name"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let capabilities: Vec<String> = frame["params"]["capabilities"]
        .as_array()
        .map(|caps| {
            caps.iter()
                .filter_map(|cap| cap.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    ObservedInit { name, capabilities }
}

/// Reads one non-blank client frame; `None` once the client goes away.
async fn next_frame(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> Result<Option<Value>> {
    loop {
        let line = tokio::time::timeout(WAIT, lines.next_line()).await??;
        match line {
            None => return Ok(None),
            Some(line) if line.trim().is_empty() => {}
            Some(line) => return Ok(Some(sonic_rs::from_str(&line)?)),
        }
    }
}

async fn send_frame(writer: &mut tokio::net::unix::OwnedWriteHalf, frame: &Value) -> Result<()> {
    let mut text = sonic_rs::to_string(frame)?;
    text.push('\n');
    writer.write_all(text.as_bytes()).await?;
    Ok(())
}

/// A minimal scripted version-1 server: the first connection drops right
/// after its handshake to force a client reconnect; the second answers
/// `session/list` and holds until the client goes away.
async fn fake_rpc_server(
    listener: tokio::net::UnixListener,
    inits: std::sync::Arc<tokio::sync::Mutex<Vec<ObservedInit>>>,
) -> Result<()> {
    let (stream, _) = tokio::time::timeout(WAIT, listener.accept()).await??;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let first = next_frame(&mut lines)
        .await?
        .ok_or("the first handshake never arrived")?;
    inits.lock().await.push(record_init(&first));
    send_frame(&mut writer, &init_reply(&first["id"])).await?;
    // The connect succeeds; the failure lands mid-session so the next
    // call must reconnect transparently.
    drop(lines);
    drop(writer);

    let (stream, _) = tokio::time::timeout(WAIT, listener.accept()).await??;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(frame) = next_frame(&mut lines).await? {
        let id = frame["id"].clone();
        let method = frame["method"].as_str().unwrap_or("").to_owned();
        let reply = if method == "initialize" {
            inits.lock().await.push(record_init(&frame));
            init_reply(&id)
        } else if method == "session/list" {
            sonic_rs::json!({"jsonrpc": "2.0", "id": id, "result": {"sessions": []}})
        } else {
            sonic_rs::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "unknown"}})
        };
        send_frame(&mut writer, &reply).await?;
    }
    Ok(())
}

/// Local-socket guards through the public listener entry: over-long paths,
/// group/other-readable parents, and live listeners refuse; a stale socket
/// file is replaced and serves.
#[tokio::test]
#[cfg(unix)]
async fn local_socket_guards_over_public_api() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fn closer(transport: Transport) -> ConnectionFuture {
        Box::pin(async move {
            let mut transport = transport;
            transport.close().await;
            Ok(())
        })
    }

    assert_eq!(
        default_rpc_path(Path::new("/t/data")),
        PathBuf::from("/t/data/rpc/dal.sock")
    );

    let dir = tempfile::tempdir()?;

    let long = dir.path().join("x".repeat(120));
    let refused = serve_local(&long, None, None, closer).await;
    assert!(
        matches!(refused, Err(LocalSocketError::PathTooLong { .. })),
        "120-byte path refuses: {refused:?}"
    );

    let open = dir.path().join("open");
    std::fs::create_dir_all(&open)?;
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755))?;
    let exposed = serve_local(&open.join("dal.sock"), None, None, closer).await;
    assert!(
        matches!(exposed, Err(LocalSocketError::DirNotPrivate { .. })),
        "group-readable parent refuses: {exposed:?}"
    );

    let busy_dir = dir.path().join("busy");
    std::fs::create_dir_all(&busy_dir)?;
    std::fs::set_permissions(&busy_dir, std::fs::Permissions::from_mode(0o700))?;
    let busy_path = busy_dir.join("dal.sock");
    let _held = std::os::unix::net::UnixListener::bind(&busy_path)?;
    let busy = serve_local(&busy_path, None, None, closer).await;
    assert!(
        matches!(busy, Err(LocalSocketError::InUse { .. })),
        "live listener refuses: {busy:?}"
    );

    let stale_dir = dir.path().join("stale");
    std::fs::create_dir_all(&stale_dir)?;
    std::fs::set_permissions(&stale_dir, std::fs::Permissions::from_mode(0o700))?;
    let stale_path = stale_dir.join("dal.sock");
    {
        let _stale = std::os::unix::net::UnixListener::bind(&stale_path)?;
    }
    let served = serve_local(&stale_path, None, None, closer);
    let client = async {
        // The replacement bind races the first connects against the stale
        // file; retry until the new listener answers.
        let deadline = tokio::time::Instant::now() + WAIT;
        let stream = loop {
            match tokio::net::UnixStream::connect(&stale_path).await {
                Ok(stream) => break stream,
                Err(error) => {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "replaced listener never answers: {error:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        let (mut reader, _writer) = stream.into_split();
        let mut bytes = Vec::new();
        tokio::time::timeout(WAIT, reader.read_to_end(&mut bytes)).await??;
        assert!(bytes.is_empty(), "the closer answers with EOF");
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };
    tokio::select! {
        result = served => result?,
        result = client => result?,
    }
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn remote_host_identity_and_reconnect() -> Result<()> {
    use dal_core::ListQuery;

    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("dal.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let inits = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let server = fake_rpc_server(listener, std::sync::Arc::clone(&inits));
    let client = async {
        let host = RemoteHost::connect(RemoteEndpoint::LocalSocket(socket)).await?;
        let query = ListQuery {
            limit: None,
            cursor: None,
            search: None,
        };
        // The first connection drops right after its handshake: a call
        // already on the dead link fails instead of being re-sent, so no
        // server work is duplicated.
        let first = host.sessions(&query).await;
        let Err(dal_wire::WireError::Transport(_)) = first else {
            panic!("in-flight call on a dropped link fails")
        };
        // Later calls reconnect transparently: retry until the new
        // connection answers or the deadline passes.
        let deadline = tokio::time::Instant::now() + WAIT;
        let page = loop {
            match host.sessions(&query).await {
                Ok(page) => break page,
                Err(error) => {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "reconnect never answers: {error:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        assert!(page.items.is_empty(), "reconnect replays the session list");
        drop(host);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };
    let (server_outcome, client_outcome) = tokio::join!(server, client);
    server_outcome?;
    client_outcome?;

    let inits = inits.lock().await;
    assert_eq!(inits.len(), 2, "one handshake per connection: {inits:?}");
    for init in inits.iter() {
        assert_eq!(init.name, "dal-remotehost", "client identity: {init:?}");
        for required in [
            "sessions",
            "host.updates",
            "blobs",
            "docs",
            "auth",
            "models",
        ] {
            assert!(
                init.capabilities.iter().any(|cap| cap == required),
                "capability {required}: {init:?}"
            );
        }
    }
    Ok(())
}
