//! The MCP example crosses the real host service boundary with a loopback JSON-RPC fixture.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{BoxFuture, Caller, ExtensionBuilder, McpClient};
use dal_agent::{Delivery, Env, Host, Product, ServiceError, SessionRef};
use dal_core::ext::{McpRequest, McpResponse};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, Part, Reply, Service,
    ToolOutcomeView, UpdateKind, Workspace,
};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const PLUGIN_SOURCE: &str = include_str!("../../../examples/plugins/mcp/plugin.star");
const FIXTURE_MODEL: &str = "openai/gpt-6";

#[derive(Clone, Copy)]
enum FixtureReply {
    List,
    Success,
    Error,
    /// Answers `tools/call` with the exact `params` the request carried.
    Echo,
}

struct FixtureMcp {
    addr: std::net::SocketAddr,
}

fn render_tool(tool: &Value) -> Option<String> {
    let name = tool.get("name").and_then(JsonValueTrait::as_str)?;
    let description = tool
        .get("description")
        .and_then(JsonValueTrait::as_str)
        .unwrap_or("");
    Some(if description.is_empty() {
        name.to_owned()
    } else {
        format!("{name}: {description}")
    })
}

impl McpClient for FixtureMcp {
    fn call<'a>(
        &'a self,
        _who: &'a Caller,
        request: McpRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<McpResponse, ServiceError>> {
        let addr = self.addr;
        Box::pin(async move {
            let method = if request.tool.is_empty() {
                "tools/list"
            } else {
                "tools/call"
            };
            let params = if request.tool.is_empty() {
                "{}".to_owned()
            } else {
                format!(
                    "{{\"name\":{},\"arguments\":{}}}",
                    sonic_rs::to_string(request.tool.as_ref()).map_err(|error| {
                        ServiceError::failed(Some(Service::Mcp), error.to_string())
                    })?,
                    request.arguments.as_str()
                )
            };
            let payload = format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":{},\"params\":{params}}}",
                sonic_rs::to_string(method)
                    .map_err(|error| ServiceError::failed(Some(Service::Mcp), error.to_string()))?,
            );
            let mut socket = TcpStream::connect(addr)
                .await
                .map_err(|error| ServiceError::failed(Some(Service::Mcp), error.to_string()))?;
            let request_text = format!(
                "POST /mcp HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                addr,
                payload.len(),
                payload
            );
            socket
                .write_all(request_text.as_bytes())
                .await
                .map_err(|error| ServiceError::failed(Some(Service::Mcp), error.to_string()))?;
            let body = read_http_message(&mut socket)
                .await
                .map_err(|error| ServiceError::failed(Some(Service::Mcp), error.to_string()))?;
            let value = sonic_rs::from_str::<Value>(&body)
                .map_err(|error| ServiceError::failed(Some(Service::Mcp), error.to_string()))?;
            if let Some(error) = value.get("error") {
                let code = error
                    .get("code")
                    .and_then(JsonValueTrait::as_i64)
                    .unwrap_or(-32600);
                let message = error
                    .get("message")
                    .and_then(JsonValueTrait::as_str)
                    .unwrap_or("MCP fixture error");
                return Err(ServiceError::failed(
                    Some(Service::Mcp),
                    format!("mcp protocol error {code}: {message}"),
                ));
            }
            let result = value.get("result").ok_or_else(|| {
                ServiceError::failed(Some(Service::Mcp), "MCP fixture omitted result")
            })?;
            let is_error = result
                .get("isError")
                .and_then(JsonValueTrait::as_bool)
                .unwrap_or(false);
            let text = if request.tool.is_empty() {
                result
                    .get("tools")
                    .and_then(|tools| tools.as_array())
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(render_tool)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            } else {
                result
                    .get("content")
                    .and_then(|content| content.as_array())
                    .and_then(|content| content.first())
                    .and_then(|item| item.get("text"))
                    .and_then(JsonValueTrait::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            Ok(McpResponse {
                text: text.into(),
                is_error,
            })
        })
    }
}

async fn read_http_message(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let body_start = header_end + 4;
        let header = String::from_utf8_lossy(&bytes[..body_start]);
        let length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if bytes.len() >= body_start + length {
            return String::from_utf8(bytes[body_start..body_start + length].to_vec())
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "HTTP message ended before its body",
    ))
}

async fn serve_fixture(listener: TcpListener, reply: FixtureReply) -> std::io::Result<()> {
    // A client may open a connection and close it, or send a head with no
    // body, before the one request this fixture answers. Such a probe is not
    // a protocol error: keep accepting until a real JSON-RPC request arrives,
    // so the fixture never dies on a warm-up connection.
    let (mut socket, body) = loop {
        let (mut socket, _) = listener.accept().await?;
        if let Ok(body) = read_http_message(&mut socket).await
            && !body.trim().is_empty()
        {
            break (socket, body);
        }
    };
    let request = sonic_rs::from_str::<Value>(&body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let id = request
        .get("id")
        .and_then(JsonValueTrait::as_u64)
        .unwrap_or(1);
    let method = request
        .get("method")
        .and_then(JsonValueTrait::as_str)
        .unwrap_or("");
    let result = match (reply, method) {
        (FixtureReply::List, "tools/list") => {
            "{\"tools\":[{\"name\":\"echo\",\"description\":\"Echo\",\"inputSchema\":{\"type\":\"object\",\"properties\":{}},\"futureField\":true}],\"nextCursor\":null,\"ttlMs\":1000,\"futureResultField\":\"ignored\"}".to_owned()
        }
        (FixtureReply::Success, "tools/call") => {
            "{\"content\":[{\"type\":\"text\",\"text\":\"fixture reply\",\"futureContentField\":true}],\"isError\":false,\"futureResultField\":\"ignored\"}".to_owned()
        }
        (FixtureReply::Echo, "tools/call") => {
            let params = request
                .get("params")
                .map(sonic_rs::to_string)
                .transpose()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
                .unwrap_or_default();
            let text = sonic_rs::to_string(&params)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            format!("{{\"content\":[{{\"type\":\"text\",\"text\":{text}}}],\"isError\":false}}")
        }
        (FixtureReply::Error, "tools/call") => {
            "{\"content\":[{\"type\":\"text\",\"text\":\"fixture failure\",\"futureContentField\":true}],\"isError\":true,\"futureResultField\":\"ignored\"}".to_owned()
        }
        (_, _) => {
            // A server that does not serve the method answers with a
            // JSON-RPC error object beside, not inside, `result`.
            let response = format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32601,\"message\":\"unexpected method\",\"futureErrorField\":true}},\"futureTopField\":true}}"
            );
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            return socket.write_all(http.as_bytes()).await;
        }
    };
    let response =
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result},\"futureTopField\":true}}");
    let http = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.len(),
        response
    );
    socket.write_all(http.as_bytes()).await
}

fn plugin_system(data_root: &Path, server_url: &str) -> TestResult<PluginSystem> {
    let plugin_dir = data_root.join("plugins").join("mcp");
    std::fs::create_dir_all(&plugin_dir)?;
    std::fs::write(plugin_dir.join("plugin.star"), PLUGIN_SOURCE)?;
    let servers_json = format!(
        "{{\"servers\":{{\"local\":{{\"url\":{}}}}}}}",
        sonic_rs::to_string(server_url)?
    );
    let config = format!(
        "{{\"servers_json\":{}}}",
        sonic_rs::to_string(&servers_json)?
    );
    let roots = LoadRoots {
        data_root: data_root.to_path_buf(),
        bundled: Vec::new(),
    };
    let cfg = PluginsConfig {
        enabled: Vec::new(),
        limits: dal_core::PluginLimits::default(),
        configs: BTreeMap::from([(String::from("mcp"), config)]),
    };
    Ok(PluginSystem::new(load(&roots, &cfg)?, roots, cfg))
}

fn model_fixture(tool: &str, args: &str) -> String {
    let template = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"mcp-call","name":"__TOOL__"},{"type":"tool_calls_done","calls":[{"id":"mcp-call","name":"__TOOL__","args":{"kind":"parsed","value":__ARGS__}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}
{"kind":"events","events":[{"type":"text_delta","text":"finished"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}
"#;
    template.replace("__TOOL__", tool).replace("__ARGS__", args)
}

async fn run_tool(
    reply: FixtureReply,
    tool: &str,
    args: &str,
    with_provider: bool,
) -> TestResult<ToolOutcomeView> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the loopback MCP server must accept requests while the test drives the client"
    )]
    let (server_url, server_task) = if with_provider {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        (
            format!("http://{address}"),
            Some(tokio::spawn(serve_fixture(listener, reply))),
        )
    } else {
        (String::from("http://127.0.0.1:9"), None)
    };
    let data = tempfile::tempdir()?;
    let system = plugin_system(data.path(), &server_url)?;
    let mut extensions = system.extensions()?;
    if with_provider {
        let address = server_url
            .strip_prefix("http://")
            .ok_or("fixture URL is not HTTP")?
            .parse()?;
        let mcp = ExtensionBuilder::new(
            "fixture-mcp",
            "0.1.0",
            dal_core::ServiceSet::from_names(["mcp"])?,
        )?
        .mcp_client(Arc::new(FixtureMcp { addr: address }))
        .build()?;
        extensions.insert(0, mcp);
    }
    let fixture = data.path().join("scripted.jsonl");
    std::fs::write(&fixture, model_fixture(tool, args))?;
    let user = format!(
        "model = \"{FIXTURE_MODEL}\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, data.path(), "", Some(&user))?;
    let workspace = tempfile::tempdir()?;
    let product = Product {
        name: "dal",
        data_root: data.path().to_path_buf(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(workspace.path().to_path_buf())?,
            },
            ClientId::new("mcp-port"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let accepted = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Use the MCP example tool once.".into(),
            }],
        })
        .await?;
    if !matches!(accepted, Reply::Accepted { .. }) {
        return Err("the scripted MCP prompt was not accepted".into());
    }
    let mut settled = None;
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await?;
        let Some(Delivery::Update(update)) = delivery else {
            break;
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) if with_provider => {
                agent.answer(request.id, Answer::ApproveForSession).await?;
            }
            UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
            UpdateKind::TurnEnded { .. } if settled.is_some() => break,
            _ => {}
        }
    }
    let outcome = settled.ok_or("the scripted MCP call did not settle")?;
    let _ = host.shutdown(Duration::from_secs(1)).await;
    if let Some(server) = server_task {
        let result = tokio::time::timeout(Duration::from_secs(2), server).await??;
        result?;
    }
    Ok(outcome)
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_port_lists_server_and_tool_from_local_fixture() -> TestResult<()> {
    let outcome = run_tool(
        FixtureReply::List,
        "mcp__mcp_list",
        r#"{"server":"local"}"#,
        true,
    )
    .await?;
    assert!(!outcome.is_error, "{outcome:?}");
    // The header line comes from the plugin's own configuration, the tool
    // line from the server's reply; nothing else may be in the text.
    let (header, tools) = outcome
        .text
        .split_once('\n')
        .ok_or_else(|| format!("no tool line after the header: {outcome:?}"))?;
    let port = header
        .strip_prefix("local  http://127.0.0.1:")
        .ok_or_else(|| format!("unexpected header: {header:?}"))?;
    assert!(
        !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()),
        "the header must carry the configured server address: {header:?}"
    );
    assert_eq!(tools, "echo: Echo");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_port_forwards_the_tool_name_and_arguments_object_verbatim() -> TestResult<()> {
    let arguments = r#"{"message":"he said \"hi\"","n":9007199254740991,"neg":-9007199254740991,"flag":true,"none":null,"nested":{"list":[1,2.5,"x"],"empty":{}},"uni":"h\u00e9llo"}"#;
    let args = format!(
        r#"{{"server":"local","tool":"echo","arguments_json":{}}}"#,
        sonic_rs::to_string(arguments)?
    );
    let outcome = run_tool(FixtureReply::Echo, "mcp__mcp_call", &args, true).await?;
    assert!(!outcome.is_error, "{outcome:?}");
    // The fixture answers with the JSON-RPC params it received, so the text
    // is exactly what crossed the plugin, the host service, and the client.
    // Object member order is not part of JSON equality, so both sides are
    // compared in a canonical member order.
    let sent: Value = sonic_rs::from_str(&outcome.text)?;
    let expected: Value =
        sonic_rs::from_str(&format!(r#"{{"name":"echo","arguments":{arguments}}}"#))?;
    assert_eq!(
        canonical(&sent),
        canonical(&expected),
        "received {}",
        outcome.text
    );
    Ok(())
}

/// Renders `value` with every object's members in key order.
fn canonical(value: &Value) -> String {
    if let Some(object) = value.as_object() {
        let mut members: Vec<(String, String)> = object
            .iter()
            .map(|(key, member)| (key.to_owned(), canonical(member)))
            .collect();
        members.sort();
        let members: Vec<String> = members
            .into_iter()
            .map(|(key, member)| format!("{key:?}:{member}"))
            .collect();
        return format!("{{{}}}", members.join(","));
    }
    if let Some(array) = value.as_array() {
        let items: Vec<String> = array.iter().map(canonical).collect();
        return format!("[{}]", items.join(","));
    }
    value.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_port_propagates_mcp_error_result() -> TestResult<()> {
    let outcome = run_tool(
        FixtureReply::Error,
        "mcp__mcp_call",
        r#"{"server":"local","tool":"echo","arguments_json":"{}"}"#,
        true,
    )
    .await?;
    assert!(outcome.is_error, "{outcome:?}");
    // The plugin tags the server's `isError` text with its own failure code.
    assert_eq!(outcome.text.as_ref(), "mcp_error: fixture failure");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_port_reports_a_protocol_error_apart_from_a_tool_error() -> TestResult<()> {
    // The list-only fixture answers `tools/call` with a JSON-RPC error object,
    // which is a transport failure and never an `isError` tool result.
    let outcome = run_tool(
        FixtureReply::List,
        "mcp__mcp_call",
        r#"{"server":"local","tool":"echo","arguments_json":"{}"}"#,
        true,
    )
    .await?;
    assert!(outcome.is_error, "{outcome:?}");
    assert_eq!(
        outcome.text.as_ref(),
        "mcp.call: failed: mcp protocol error -32601: unexpected method"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_port_reports_uncomposed_provider_at_catalog_boundary() -> TestResult<()> {
    let outcome = run_tool(
        FixtureReply::Success,
        "mcp__mcp_call",
        r#"{"server":"local","tool":"echo","arguments_json":"{}"}"#,
        false,
    )
    .await?;
    assert!(outcome.is_error, "{outcome:?}");
    assert!(
        outcome
            .text
            .contains("operation mcp.call has no backend in this generation"),
        "{outcome:?}"
    );
    Ok(())
}
