// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! MCP battery behavior tests through declared feeds, lifecycle hooks, and tool calls.

use std::{collections::BTreeMap, fs, path::PathBuf, time::Duration};

#[cfg(unix)]
use std::path::Path;

use crate::mcp::{
    McpConfig, STDERR_RING,
    http::auth::{self as token_auth, TokenRecord},
    tools::fold_tool_name,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, ext::ExtensionBuilder};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, JournalPart, Output, PageReq, Part,
    Question, Reply, ServiceSet, SessionId, UpdateKind, Workspace,
    ext::{McpBlock, McpServerDecl, Name, SkillRecord},
};
use sonic_rs::JsonValueTrait;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

const SKILL: &str = "weather";
const SERVER: &str = "fixture";

fn temp_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "dalgona-mcp-{}-{}-{label}",
        std::process::id(),
        SessionId::new_v7()
    ))
}

struct HostedFixture {
    host: Option<Host>,
    agent: Agent,
    subscription: dal_agent::Subscription,
    grant_answer: Answer,
    notices: Vec<String>,
    cleanup: Vec<PathBuf>,
}

impl HostedFixture {
    async fn new(server: McpServerDecl, steps: Vec<String>, grant_answer: Answer) -> Self {
        let data = temp_path("host-data");
        let workspace = temp_path("host-workspace");
        fs::create_dir_all(&data).expect("host data directory");
        fs::create_dir_all(&workspace).expect("host workspace directory");
        let script = data.join("script.jsonl");
        let contents = format!("{}\n", steps.join("\n"));
        fs::write(&script, contents).expect("scripted provider fixture");
        let user = format!(
            "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            script.display()
        );
        let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))
            .expect("scripted host config");
        let mut servers = BTreeMap::new();
        servers.insert(SERVER.into(), server);
        let skill = SkillRecord {
            name: Name::parse(SKILL).expect("test skill name"),
            description: "MCP fixture".into(),
            body: "MCP fixture skill".into(),
            letter2image: false,
            mcp: Some(McpBlock { servers }),
        };
        let inject = ServiceSet::from_names(["mcp", "env"]).expect("MCP service names");
        let skill_extension = ExtensionBuilder::new("weather", "0.1.0", inject)
            .expect("skill extension")
            .with_origin(dal_core::Origin::Bundled, None)
            .skill(skill)
            .build()
            .expect("skill extension registration");
        let client = crate::mcp::mcp(&McpConfig {
            tokens_path: data.join("mcp").join("tokens.json"),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        })
        .expect("MCP extension");
        let product = Product {
            name: "dalgona",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![skill_extension, client],
            bundled: Vec::new(),
        };
        let env = Env {
            vars: BTreeMap::from([
                ("OPENAI_API_KEY".into(), "sk-test".into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                (
                    "HOME".into(),
                    std::env::temp_dir().display().to_string().into(),
                ),
                (
                    "TMPDIR".into(),
                    std::env::temp_dir().display().to_string().into(),
                ),
            ]),
            cwd: workspace.clone(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env)
            .await
            .expect("host starts");
        let agent = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(workspace.clone()).expect("workspace"),
                    name: None,
                },
                ClientId::new("mcp-test"),
            )
            .await
            .expect("session opens");
        let subscription = agent.subscribe(None).expect("subscription");
        Self {
            host: Some(host),
            agent,
            subscription,
            grant_answer,
            notices: Vec::new(),
            cleanup: vec![data, workspace],
        }
    }

    async fn prompt(&mut self) {
        let reply = self
            .agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "exercise the MCP server".into(),
                }],
            })
            .await
            .expect("scripted prompt");
        assert!(matches!(reply, Reply::Accepted { .. }), "prompt: {reply:?}");
    }

    async fn answer_pending(&self) -> bool {
        let view = self.agent.view(PageReq::default()).expect("host view");
        let Some(request) = view.open.first() else {
            return false;
        };
        let answer = match &request.question {
            Question::Grant { .. } => self.grant_answer.clone(),
            Question::Approval { .. } => Answer::ApproveForSession,
            _ => Answer::Decline,
        };
        self.agent
            .answer(request.id, answer)
            .await
            .expect("scripted frontend answer");
        true
    }

    async fn turn_ended(&mut self) {
        for _ in 0..300 {
            if self.answer_pending().await {
                continue;
            }
            let update =
                tokio::time::timeout(Duration::from_millis(100), self.subscription.next()).await;
            let Ok(Some(Delivery::Update(update))) = update else {
                continue;
            };
            if let UpdateKind::Notice(notice) = &update.kind {
                self.notices.push(notice.text.to_string());
            }
            if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
                return;
            }
        }
        panic!("scripted MCP turn did not end");
    }

    async fn status(&self) -> String {
        match self
            .agent
            .submit(Command::Run {
                name: "mcp".into(),
                args: "".into(),
                expected: None,
            })
            .await
            .expect("MCP status command")
        {
            Reply::Done(Output::Markdown(text)) => text.into(),
            reply => panic!("unexpected MCP status reply: {reply:?}"),
        }
    }

    fn tool_results(&self, name: &str) -> Vec<(bool, String)> {
        self.agent
            .view(PageReq::default())
            .expect("host view")
            .entries
            .items
            .iter()
            .filter_map(|entry| {
                let dal_core::EntryKind::ToolResult {
                    name: tool,
                    error,
                    parts,
                    ..
                } = &entry.kind
                else {
                    return None;
                };
                if tool.as_ref() != name {
                    return None;
                }
                Some((*error, journal_text(parts)))
            })
            .collect()
    }

    async fn shutdown(mut self) {
        drop(self.subscription);
        drop(self.agent);
        if let Some(host) = self.host.take() {
            let _ = host.shutdown(Duration::from_secs(5)).await;
        }
        for path in self.cleanup {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn journal_text(parts: &[JournalPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            JournalPart::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn scripted_tool_step(id: &str, name: &str, args: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"{id}","name":"{name}"}},{{"type":"tool_calls_done","calls":[{{"id":"{id}","name":"{name}","args":{{"kind":"parsed","value":{args}}}}}]}},{{"type":"usage","usage":{{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}}},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

fn scripted_final_step() -> String {
    r#"{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#
        .to_owned()
}
#[cfg(unix)]
fn stdio_decl(pid_file: &Path) -> McpServerDecl {
    let mut env = BTreeMap::new();
    env.insert(
        "MCP_PID_FILE".into(),
        pid_file.to_string_lossy().into_owned().into_boxed_str(),
    );
    McpServerDecl::Stdio {
        command: vec!["/bin/sh".into(), "-c".into(), STDIO_SCRIPT.into()],
        env,
    }
}

#[cfg(unix)]
const STDIO_SCRIPT: &str = r#"
printf '%s' "$$" > "$MCP_PID_FILE"
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
    *'"method":"server/discover"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object","properties":{}}},{"name":"wait","inputSchema":{"type":"object","properties":{}}}],"ttlMs":10000}}\n' "$id"
      ;;
    *'"method":"tools/call"'*'"name":"wait"'*)
      printf 'w' >> "$MCP_PID_FILE"
      sleep 30
      ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","method":"notifications/progress","params":{"_meta":{"progressToken":"t-%s"},"message":"working"}}\n' "$id"
      sleep 0.03
      printf '{"jsonrpc":"2.0","method":"notifications/progress","params":{"_meta":{"progressToken":"t-%s"},"message":"working"}}\n' "$id"
      sleep 0.03
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"stdio-ok"}],"isError":false}}\n' "$id"
      ;;
  esac
done
"#;

#[cfg(unix)]
#[tokio::test]
async fn declared_stdio_tools_run_progress_and_session_end_cancels_child() {
    let pid_file = temp_path("stdio.pid");
    let entry = fold_tool_name(SKILL, SERVER, "");
    let echo = fold_tool_name(SKILL, SERVER, "echo");
    let wait = fold_tool_name(SKILL, SERVER, "wait");
    let mut fixture = HostedFixture::new(
        stdio_decl(&pid_file),
        vec![
            scripted_tool_step("entry-1", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step("echo-1", &echo, r#"{"message":"hello"}"#),
            scripted_final_step(),
            scripted_tool_step("wait-1", &wait, "{}"),
        ],
        Answer::ApproveForSession,
    )
    .await;
    assert!(fixture.status().await.contains("declared"));

    fixture.prompt().await;
    fixture.turn_ended().await;
    let status = fixture.status().await;
    assert!(
        status.contains("ready") && status.contains("2 tools"),
        "entry call did not publish both server tools: status={status}; results={:?}",
        fixture.tool_results(&entry)
    );
    fixture.prompt().await;
    fixture.turn_ended().await;
    assert_eq!(
        fixture.tool_results(&entry),
        vec![(false, format!("{echo}\n{wait}"))]
    );
    assert_eq!(
        fixture.tool_results(&echo),
        vec![(false, "stdio-ok".to_owned())]
    );

    assert!(
        fixture
            .notices
            .iter()
            .any(|notice| notice.contains("working")),
        "progress notices reach the host"
    );
    let pid = fs::read_to_string(&pid_file)
        .expect("stdio child wrote its process id")
        .parse::<u32>()
        .expect("valid child process id");
    fixture.prompt().await;
    wait_for_wait_request(&pid_file, &fixture).await;
    fixture.shutdown().await;
    #[cfg(target_os = "linux")]
    wait_for_process_exit(pid).await;
    let _ = fs::remove_file(pid_file);
}

#[cfg(unix)]
async fn wait_for_wait_request(path: &Path, fixture: &HostedFixture) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            fixture.answer_pending().await;
            if fs::read_to_string(path).is_ok_and(|contents| contents.ends_with('w')) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the stdio fixture received the waiting tool call");
}

#[cfg(target_os = "linux")]
async fn wait_for_process_exit(pid: u32) {
    tokio::time::timeout(Duration::from_secs(2), async move {
        loop {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session end reaped the MCP child process");
}

#[cfg(unix)]
#[tokio::test]
async fn denied_mcp_tool_does_not_launch_declared_child() {
    let pid_file = temp_path("denied.pid");
    let entry = fold_tool_name(SKILL, SERVER, "");
    let mut fixture = HostedFixture::new(
        stdio_decl(&pid_file),
        vec![
            scripted_tool_step("entry-denied", &entry, "{}"),
            scripted_final_step(),
        ],
        Answer::Decline,
    )
    .await;
    fixture.prompt().await;
    fixture.turn_ended().await;
    let denied = fixture.tool_results(&entry);
    assert_eq!(denied.len(), 1);
    assert!(denied[0].0);
    assert!(
        denied[0].1.contains("declined"),
        "the scripted grant decline must reach the MCP service: {}",
        denied[0].1
    );
    assert!(!pid_file.exists());
    assert!(fixture.status().await.contains("unasked"));
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_crash_budget_restarts_once_then_latches() {
    let marker = temp_path("stdio-crash-count");
    let entry = fold_tool_name(SKILL, SERVER, "");
    let mut fixture = HostedFixture::new(
        crash_stdio_decl(&marker),
        vec![
            scripted_tool_step("crash-1", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step("crash-2", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step("crash-3", &entry, "{}"),
            scripted_final_step(),
        ],
        Answer::ApproveForSession,
    )
    .await;

    fixture.prompt().await;
    fixture.turn_ended().await;
    let status = fixture.status().await;
    assert!(
        status.contains("failed"),
        "first crash did not mark the server failed: {status}; results={:?}",
        fixture.tool_results(&entry)
    );
    fixture.prompt().await;
    fixture.turn_ended().await;
    let status = fixture.status().await;
    assert!(
        status.contains("latched"),
        "restart budget did not latch after the second crash: {status}; results={:?}",
        fixture.tool_results(&entry)
    );
    fixture.prompt().await;
    fixture.turn_ended().await;
    let crashes = fixture.tool_results(&entry);
    assert_eq!(crashes.len(), 3);
    assert!(crashes.iter().all(|(error, _)| *error));
    assert_eq!(
        fs::read_to_string(&marker)
            .expect("two child starts were recorded")
            .len(),
        2
    );
    fixture.shutdown().await;
    let _ = fs::remove_file(marker);
}

#[cfg(unix)]
fn crash_stdio_decl(marker: &Path) -> McpServerDecl {
    let mut env = BTreeMap::new();
    env.insert(
        "MCP_CRASH_MARKER".into(),
        marker.to_string_lossy().into_owned().into_boxed_str(),
    );
    McpServerDecl::Stdio {
        command: vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf x >> \"$MCP_CRASH_MARKER\"; exit 1".into(),
        ],
        env,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_call_crash_reports_bounded_stderr() {
    let entry = fold_tool_name(SKILL, SERVER, "");
    let echo = fold_tool_name(SKILL, SERVER, "echo");
    let mut fixture = HostedFixture::new(
        stderr_crash_stdio_decl(),
        vec![
            scripted_tool_step("entry-1", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step("echo-1", &echo, "{}"),
            scripted_final_step(),
        ],
        Answer::ApproveForSession,
    )
    .await;
    fixture.prompt().await;
    fixture.turn_ended().await;
    assert!(fixture.status().await.contains("ready"));

    fixture.prompt().await;
    fixture.turn_ended().await;
    let results = fixture.tool_results(&echo);
    assert_eq!(results.len(), 1);
    assert!(results[0].0);
    assert!(results[0].1.contains("status 1"), "{results:?}");
    assert!(
        results[0].1.contains("crash stderr line one")
            && results[0].1.contains("crash stderr line two"),
        "{results:?}"
    );
    assert!(
        results[0].1.len() <= STDERR_RING + 1024,
        "stderr excerpt exceeded the ring bound: {}",
        results[0].1.len()
    );
    fixture.shutdown().await;
}

#[cfg(unix)]
fn stderr_crash_stdio_decl() -> McpServerDecl {
    McpServerDecl::Stdio {
        command: vec![
            "/bin/sh".into(),
            "-c".into(),
            r#"
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
    *'"method":"server/discover"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object","properties":{}}}],"ttlMs":10000}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      head -c 70000 /dev/zero | tr '\0' x >&2
      printf '\ncrash stderr line one\ncrash stderr line two\n' >&2
      exit 1
      ;;
  esac
done
"#
            .into(),
        ],
        env: BTreeMap::new(),
    }
}

#[derive(Clone, Copy)]
enum HttpScenario {
    PaginatedCache,
    PageCap,
}

struct HttpRequest {
    headers: String,
    body: String,
    method: String,
}

fn header_value(headers: &str, name: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        field
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn request_content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            let (field, value) = line.split_once(':')?;
            field
                .eq_ignore_ascii_case("content-length")
                .then_some(value.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0)
}

async fn read_http_request(socket: &mut TcpStream) -> HttpRequest {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let count = socket.read(&mut buffer).await.expect("read HTTP request");
        assert_ne!(count, 0, "HTTP client closed before request headers");
        request.extend_from_slice(&buffer[..count]);
        if let Some(end) = find_header_end(&request) {
            break end;
        }
    };
    let headers = String::from_utf8(request[..header_end].to_vec()).expect("ASCII HTTP headers");
    let total = header_end + request_content_length(&headers);
    while request.len() < total {
        let count = socket
            .read(&mut buffer)
            .await
            .expect("read HTTP request body");
        assert_ne!(count, 0, "HTTP client closed before request body");
        request.extend_from_slice(&buffer[..count]);
    }
    let body = String::from_utf8(request[header_end..total].to_vec()).expect("UTF-8 MCP body");
    let method = header_value(&headers, "mcp-method").unwrap_or_default();
    HttpRequest {
        headers,
        body,
        method,
    }
}

fn rpc_result(id: u64, result: &str) -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}")
}

fn rpc_error(id: u64, code: i64, message: &str) -> String {
    let message = sonic_rs::to_string(message).expect("encode JSON-RPC error");
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":{code},\"message\":{message}}}}}"
    )
}

fn http_reply(scenario: HttpScenario, request: &HttpRequest) -> String {
    let value = sonic_rs::from_str::<sonic_rs::Value>(&request.body).expect("valid MCP request");
    let id = value
        .get("id")
        .and_then(JsonValueTrait::as_u64)
        .unwrap_or(0);
    match request.method.as_str() {
        "server/discover" => match scenario {
            HttpScenario::PaginatedCache => rpc_error(id, -32601, "unsupported method"),
            HttpScenario::PageCap => rpc_result(id, "{}"),
        },
        "initialize" | "notifications/initialized" => rpc_result(id, "{}"),
        "tools/list" => match scenario {
            HttpScenario::PaginatedCache => {
                let cursor = value
                    .get("params")
                    .and_then(|params| params.get("cursor"))
                    .and_then(JsonValueTrait::as_str);
                if cursor.is_some() {
                    rpc_result(
                        id,
                        r#"{"tools":[{"name":"lookup","description":"lookup","inputSchema":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}}}],"nextCursor":null,"ttlMs":0}"#,
                    )
                } else {
                    rpc_result(
                        id,
                        r#"{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}}}],"nextCursor":"page-two","ttlMs":0}"#,
                    )
                }
            }
            HttpScenario::PageCap => rpc_result(
                id,
                r#"{"tools":[{"name":"partial","description":"partial","inputSchema":{"type":"object","properties":{}}}],"nextCursor":"again","ttlMs":0}"#,
            ),
        },
        "tools/call" => {
            let tool = value
                .get("params")
                .and_then(|params| params.get("name"))
                .and_then(JsonValueTrait::as_str)
                .unwrap_or("unknown");
            let region = header_value(&request.headers, "mcp-param-region")
                .unwrap_or_else(|| "missing".to_owned());
            let text =
                sonic_rs::to_string(&format!("{tool}:{region}")).expect("encode tool result text");
            let result = format!(
                "{{\"content\":[{{\"type\":\"text\",\"text\":{text}}}],\"isError\":false}}"
            );
            rpc_result(id, &result)
        }
        _ => rpc_error(id, -32601, "unsupported method"),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the loopback MCP server must accept requests while the test drives the client"
)]
async fn start_http_fixture(
    expected_requests: usize,
    scenario: HttpScenario,
) -> (String, JoinHandle<Vec<HttpRequest>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback MCP server");
    let address = listener.local_addr().expect("loopback address");
    let task = tokio::spawn(async move {
        let mut requests = Vec::with_capacity(expected_requests);
        for _ in 0..expected_requests {
            let (mut socket, _) = listener.accept().await.expect("accept MCP request");
            let request = read_http_request(&mut socket).await;
            let reply = http_reply(scenario, &request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write MCP response");
            requests.push(request);
        }
        requests
    });
    (format!("http://{address}"), task)
}

async fn finish_http_fixture(server: JoinHandle<Vec<HttpRequest>>) -> Vec<HttpRequest> {
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("HTTP fixture finishes within its request budget")
        .expect("HTTP fixture task completes")
}

#[tokio::test]
async fn declared_http_server_falls_back_paginates_expires_cache_and_maps_header() {
    let (url, server) = start_http_fixture(11, HttpScenario::PaginatedCache).await;
    let declaration = McpServerDecl::Http {
        url: url.into_boxed_str(),
    };
    let entry = fold_tool_name(SKILL, SERVER, "");
    let echo = fold_tool_name(SKILL, SERVER, "echo");
    let lookup = fold_tool_name(SKILL, SERVER, "lookup");
    let mut fixture = HostedFixture::new(
        declaration,
        vec![
            scripted_tool_step("entry-http", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step("echo-http", &echo, r#"{"region":"us-west1"}"#),
            scripted_final_step(),
            scripted_tool_step("lookup-http", &lookup, r#"{"region":"eu-north"}"#),
            scripted_final_step(),
        ],
        Answer::ApproveForSession,
    )
    .await;

    fixture.prompt().await;
    fixture.turn_ended().await;
    fixture.prompt().await;
    fixture.turn_ended().await;
    fixture.prompt().await;
    fixture.turn_ended().await;
    assert_eq!(
        fixture.tool_results(&echo),
        vec![(false, "echo:us-west1".to_owned())]
    );
    assert_eq!(
        fixture.tool_results(&lookup),
        vec![(false, "lookup:eu-north".to_owned())]
    );

    let requests = finish_http_fixture(server).await;
    assert_eq!(requests.len(), 11);
    assert_eq!(requests[0].method, "server/discover");
    assert_eq!(requests[1].method, "initialize");
    assert_eq!(requests[2].method, "notifications/initialized");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "tools/list")
            .count(),
        6
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "tools/call")
            .count(),
        2
    );
    let calls = requests
        .iter()
        .filter(|request| request.method == "tools/call")
        .collect::<Vec<_>>();
    assert_eq!(
        header_value(&calls[0].headers, "mcp-param-region").as_deref(),
        Some("us-west1")
    );
    assert_eq!(
        header_value(&calls[1].headers, "mcp-param-region").as_deref(),
        Some("eu-north")
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn http_list_page_limit_keeps_partial_tools_unpublished() {
    let expected_requests = 1 + crate::mcp::LIST_PAGE_MAX;
    let (url, server) = start_http_fixture(expected_requests, HttpScenario::PageCap).await;
    let declaration = McpServerDecl::Http {
        url: url.into_boxed_str(),
    };
    let entry = fold_tool_name(SKILL, SERVER, "");
    let mut fixture = HostedFixture::new(
        declaration,
        vec![
            scripted_tool_step("entry-page-cap", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step(
                "partial-page-cap",
                &fold_tool_name(SKILL, SERVER, "partial"),
                "{}",
            ),
            scripted_final_step(),
        ],
        Answer::ApproveForSession,
    )
    .await;

    fixture.prompt().await;
    fixture.turn_ended().await;
    let failed = fixture.tool_results(&entry);
    assert_eq!(failed.len(), 1);
    assert!(failed[0].0);
    let requests = finish_http_fixture(server).await;
    assert_eq!(requests.len(), expected_requests);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "tools/list")
            .count(),
        crate::mcp::LIST_PAGE_MAX
    );
    let partial_name = fold_tool_name(SKILL, SERVER, "partial");
    fixture.prompt().await;
    fixture.turn_ended().await;
    let partial = fixture.tool_results(&partial_name);
    assert_eq!(partial.len(), 1);
    assert!(partial[0].0);
    assert!(partial[0].1.contains("unknown tool"));
    let status = fixture.status().await;
    assert!(
        status.contains("failed") && status.contains("0 tools"),
        "incomplete page results stay unpublished: {status}"
    );
    fixture.shutdown().await;
}

#[test]
fn oauth_token_file_is_private_and_bound_to_issuer_resource() {
    let path = temp_path("oauth-tokens.json");
    token_auth::persist_token(
        &path,
        "https://issuer.example",
        "https://resource.example/api",
        TokenRecord {
            client_id: "mcp-client".to_owned(),
            access_token: "secret-access".to_owned(),
            refresh_token: Some("secret-refresh".to_owned()),
            scopes: vec!["read".to_owned()],
        },
    )
    .expect("persist OAuth token");
    let tokens = token_auth::read_tokens(&path);
    assert_eq!(
        token_auth::record_for(
            &tokens,
            "https://issuer.example",
            "https://resource.example/api"
        )
        .map(|record| record.access_token.as_str()),
        Some("secret-access")
    );
    assert!(
        token_auth::record_for(
            &tokens,
            "https://other-issuer.example",
            "https://resource.example/api"
        )
        .is_none()
    );
    assert!(
        token_auth::record_for(
            &tokens,
            "https://issuer.example",
            "https://resource.example/other"
        )
        .is_none()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path)
            .expect("OAuth token file exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    fs::remove_file(path).expect("remove OAuth fixture");
}
