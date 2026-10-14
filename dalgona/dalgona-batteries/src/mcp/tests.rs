// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! MCP battery behavior tests through declared feeds, lifecycle hooks, and tool calls.

use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc, time::Duration};

#[cfg(unix)]
use std::path::Path;

use crate::mcp::{
    Budgets, LIST_PAGE_MAX, LIST_RESPONSE_BYTES_MAX, LIST_TOOL_DESCRIPTION_BYTES_MAX,
    LIST_TOOL_NAME_BYTES_MAX, LIST_TOOL_SCHEMA_BYTES_MAX, LIST_TOOLS_MAX, McpConfig, STDERR_RING,
    client::{Client, McpCommand, SessionEndHook, SessionStartHook},
    http::auth::{self as token_auth, TokenRecord},
    tools::{decode_tool_page, fold_tool_name},
};
#[cfg(unix)]
use dal_agent::ext::{ArgError, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_agent::{
    Agent, Delivery, Env, Host, Product, SessionRef,
    ext::{BoxFuture, ExtensionBuilder, HookCx, HookError, ObserveHook},
};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, JournalPart, Output, PageReq, Part,
    Question, Reply, ServiceSet, SessionId, UpdateKind, Workspace,
    ext::{McpBlock, McpEndpoint, McpLaunch, McpServerDecl, Name, SkillRecord},
};
#[cfg(unix)]
use dal_core::{McpRequest, ModelInfo, RawJson, ToolClass, ToolSpec, Visibility};
use reqwest::Url;
use sonic_rs::JsonValueTrait;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
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

struct StartedHook {
    inner: SessionStartHook,
    done: std::sync::Mutex<Option<oneshot::Sender<Result<(), HookError>>>>,
}

impl ObserveHook<dal_core::SessionStart> for StartedHook {
    fn call(
        &self,
        input: dal_core::SessionStart,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let future = self.inner.call(input, cx);
        let done = self.done.lock().expect("startup signal").take();
        Box::pin(async move {
            let result = future.await;
            if let Some(done) = done {
                let _ = done.send(result.clone());
            }
            result
        })
    }
}

fn declared_skill(server: McpServerDecl) -> dal_agent::ext::Extension {
    let skill = SkillRecord {
        name: Name::parse(SKILL).expect("test skill name"),
        description: "MCP fixture".into(),
        body: "MCP fixture skill".into(),
        letter2image: false,
        mcp: Some(McpBlock {
            servers: BTreeMap::from([(SERVER.into(), server)]),
        }),
    };
    let inject = ServiceSet::from_names(["env"]).expect("service names");
    ExtensionBuilder::new("weather", "0.1.0", inject)
        .expect("skill extension")
        .with_origin(dal_core::Origin::Bundled, None)
        .skill(skill)
        .build()
        .expect("skill extension registration")
}

/// Builds the declaring extension with one generation tool that calls the
/// `mcp` service directly, so the probe result shows the service's own
/// verdict.
#[cfg(unix)]
fn declared_skill_with_inject_probe(server: McpServerDecl) -> dal_agent::ext::Extension {
    let skill = SkillRecord {
        name: Name::parse(SKILL).expect("test skill name"),
        description: "MCP fixture".into(),
        body: "MCP fixture skill".into(),
        letter2image: false,
        mcp: Some(McpBlock {
            servers: BTreeMap::from([(SERVER.into(), server)]),
        }),
    };
    let inject = ServiceSet::from_names(["env"]).expect("service names");
    ExtensionBuilder::new("weather", "0.1.0", inject)
        .expect("skill extension")
        .with_origin(dal_core::Origin::Bundled, None)
        .skill(skill)
        .tool(Arc::new(InjectProbe::new()), Visibility::Model)
        .build()
        .expect("skill extension registration")
}

/// One generation tool owned by the declaring extension; its run calls
/// the `mcp` service directly and reports the service's own verdict.
#[cfg(unix)]
struct InjectProbe {
    spec: Arc<ToolSpec>,
}

#[cfg(unix)]
impl InjectProbe {
    fn new() -> Self {
        Self {
            spec: Arc::new(ToolSpec {
                name: Name::parse("probe").expect("probe name"),
                description: "Calls the mcp service directly.".into(),
                parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                    .expect("probe parameters"),
                grammar: None,
            }),
        }
    }
}

#[cfg(unix)]
impl Tool for InjectProbe {
    fn declaring_extension(&self) -> Option<Name> {
        None
    }

    fn name(&self) -> &Name {
        &self.spec.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawJson, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: true,
            grant: None,
        })
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let request = McpRequest {
                session: cx.session(),
                server: format!("{SKILL}.{SERVER}").into(),
                tool: "echo".into(),
                arguments: RawJson::parse("{}").expect("probe arguments"),
            };
            let text = match services.mcp(cx.caller(), request).await {
                Ok(response) => response.text.to_string(),
                Err(error) => error.to_string(),
            };
            ToolOutcome::Ok(Box::new(ToolOutput::from_text(text)))
        })
    }
}

struct HostedFixture {
    host: Option<Host>,
    agent: Agent,
    subscription: dal_agent::Subscription,
    grant_answer: Answer,
    notices: Vec<String>,
    grants_answered: std::sync::Mutex<usize>,
    cleanup: Vec<PathBuf>,
}

impl HostedFixture {
    async fn new(server: McpServerDecl, steps: Vec<String>, grant_answer: Answer) -> Self {
        Self::with_transport(server, steps, grant_answer, None).await
    }

    #[cfg(unix)]
    async fn with_skill(
        skill: dal_agent::ext::Extension,
        steps: Vec<String>,
        grant_answer: Answer,
    ) -> Self {
        Self::with_skill_and_transport(skill, steps, grant_answer, None).await
    }

    async fn with_transport(
        server: McpServerDecl,
        steps: Vec<String>,
        grant_answer: Answer,
        loopback_url: Option<Url>,
    ) -> Self {
        Self::with_skill_and_transport(declared_skill(server), steps, grant_answer, loopback_url)
            .await
    }

    async fn with_skill_and_transport(
        skill: dal_agent::ext::Extension,
        steps: Vec<String>,
        grant_answer: Answer,
        loopback_url: Option<Url>,
    ) -> Self {
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
        let config_mcp = McpConfig {
            tokens_path: data.join("mcp").join("tokens.json"),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let client = match loopback_url {
            Some(url) => Client::with_loopback_transport(config_mcp, Budgets::default(), url),
            None => Client::new(config_mcp, Budgets::default()),
        };
        let (started, startup) = oneshot::channel();
        // The real MCP hooks own the session; the wrapper only acknowledges
        // completion so a prompt cannot race initial host tool publication.
        let mcp = ExtensionBuilder::new(
            "mcp",
            env!("CARGO_PKG_VERSION"),
            ServiceSet::from_names(["mcp", "env", "ask"]).expect("MCP services"),
        )
        .expect("MCP extension")
        .with_origin(dal_core::Origin::Bundled, None)
        .mcp_client(client.clone())
        .on_session_start_lossless(StartedHook {
            inner: SessionStartHook(Arc::clone(&client)),
            done: std::sync::Mutex::new(Some(started)),
        })
        .on_session_end_lossless(SessionEndHook(Arc::clone(&client)))
        .command(
            dal_core::CommandSpec {
                name: dal_core::CommandName::parse("mcp").expect("MCP command name"),
                summary: "Show MCP servers in this session.".into(),
                args_hint: None,
            },
            Arc::new(McpCommand(client)),
        )
        .build()
        .expect("MCP registration");
        let product = Product {
            name: "dalgona",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![skill, mcp],
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
        tokio::time::timeout(Duration::from_secs(5), startup)
            .await
            .expect("MCP session startup completes")
            .expect("MCP startup signal delivered")
            .expect("MCP session startup succeeds");
        Self {
            host: Some(host),
            agent,
            subscription,
            grant_answer,
            notices: Vec::new(),
            grants_answered: std::sync::Mutex::new(0),
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
            Question::Grant { .. } => {
                *self.grants_answered.lock().expect("grant count") += 1;
                self.grant_answer.clone()
            }
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
        launch: McpLaunch::command(vec!["/bin/sh".into(), "-c".into(), STDIO_SCRIPT.into()])
            .expect("fixture launch"),
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
    #[cfg(target_os = "linux")]
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
        launch: McpLaunch::command(vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf x >> \"$MCP_CRASH_MARKER\"; exit 1".into(),
        ])
        .expect("fixture launch"),
        env,
    }
}

#[cfg(unix)]
async fn crashed_echo_result(decl: McpServerDecl) -> (bool, String) {
    let entry = fold_tool_name(SKILL, SERVER, "");
    let echo = fold_tool_name(SKILL, SERVER, "echo");
    let mut fixture = HostedFixture::new(
        decl,
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
    let mut results = fixture.tool_results(&echo);
    assert_eq!(results.len(), 1);
    let result = results.remove(0);
    fixture.shutdown().await;
    result
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_call_crash_reports_bounded_stderr() {
    let result = crashed_echo_result(stderr_crash_stdio_decl(
        r"
      head -c 70000 /dev/zero | tr '\0' x >&2
      printf '\ncrash stderr line one\ncrash stderr line two\n' >&2
      exit 1
",
    ))
    .await;
    assert!(result.0);
    assert!(result.1.contains("status 1"), "{result:?}");
    assert!(
        result.1.contains("crash stderr line one") && result.1.contains("crash stderr line two"),
        "{result:?}"
    );
    assert!(
        result.1.len() <= STDERR_RING + 1024,
        "stderr excerpt exceeded the ring bound: {}",
        result.1.len()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_crash_diagnostic_waits_for_stderr_held_by_grandchild() {
    let result = crashed_echo_result(stderr_crash_stdio_decl(
        r#"
      fifo="$TMPDIR/dalgona-stderr-sync-$$"
      mkfifo "$fifo"
      (
        read -r _ < "$fifo"
        rm -f "$fifo"
        head -c 70000 /dev/zero | tr '\0' x >&2
        printf '\ngrandchild final marker\n' >&2
      ) </dev/null >/dev/null &
      exec 3>"$fifo"
      exit 1
"#,
    ))
    .await;
    assert!(result.0);
    assert!(result.1.contains("status 1"), "{result:?}");
    assert!(
        result.1.contains("grandchild final marker"),
        "diagnostic omitted stderr written after the server exited: {result:?}"
    );
    assert!(
        result.1.len() <= STDERR_RING + 1024,
        "stderr excerpt exceeded the ring bound: {}",
        result.1.len()
    );
}

#[cfg(unix)]
fn stderr_crash_stdio_decl(on_call: &str) -> McpServerDecl {
    let script = r#"
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
@@CALL@@
      ;;
  esac
done
"#
    .replace("@@CALL@@", on_call.trim_matches('\n'));
    McpServerDecl::Stdio {
        launch: McpLaunch::command(vec!["/bin/sh".into(), "-c".into(), script.into()])
            .expect("fixture launch"),
        env: BTreeMap::new(),
    }
}

#[derive(Clone, Copy)]
enum HttpScenario {
    PaginatedCache,
    PageCap,
    AggregateBytes,
    AggregateMetadata,
    AggregateTools,
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
            _ => rpc_result(id, "{}"),
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
            HttpScenario::AggregateBytes => {
                let padding = "x".repeat(LIST_RESPONSE_BYTES_MAX / 8);
                rpc_result(
                    id,
                    &format!(
                        r#"{{"tools":[{{"name":"partial"}}],"nextCursor":"again","padding":"{padding}"}}"#,
                    ),
                )
            }
            HttpScenario::AggregateMetadata => {
                let page = value
                    .get("params")
                    .and_then(|params| params.get("cursor"))
                    .and_then(JsonValueTrait::as_str)
                    .unwrap_or("0")
                    .parse::<usize>()
                    .expect("fixture cursor is numeric");
                let description = "d".repeat(LIST_TOOL_DESCRIPTION_BYTES_MAX);
                let tools = (0..64)
                    .map(|index| {
                        format!(r#"{{"name":"metadata{page}_{index}","description":"{description}"}}"#)
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                rpc_result(
                    id,
                    &format!(
                        r#"{{"tools":[{tools}],"nextCursor":"{}"}}"#,
                        page + 1
                    ),
                )
            }
            HttpScenario::AggregateTools => {
                let page = if value
                    .get("params")
                    .and_then(|params| params.get("cursor"))
                    .is_some()
                {
                    "second"
                } else {
                    "first"
                };
                let tools = (0..=LIST_TOOLS_MAX / 2)
                    .map(|index| format!(r#"{{"name":"{page}{index}"}}"#))
                    .collect::<Vec<_>>()
                    .join(",");
                rpc_result(id, &format!(r#"{{"tools":[{tools}],"nextCursor":"again"}}"#))
            }
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
) -> (McpServerDecl, Url, JoinHandle<Vec<HttpRequest>>) {
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
    (
        McpServerDecl::Http {
            endpoint: McpEndpoint::parse(&format!("https://{address}"))
                .expect("valid public declaration identity"),
        },
        Url::parse(&format!("http://{address}")).expect("internal loopback transport URL"),
        task,
    )
}

async fn finish_http_fixture(server: JoinHandle<Vec<HttpRequest>>) -> Vec<HttpRequest> {
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("HTTP fixture finishes within its request budget")
        .expect("HTTP fixture task completes")
}

/// Internal HTTP protocol proof with real Host services and tool publication.
/// Only transport setup uses plaintext loopback; the separate HTTPS tests
/// exercise the public declaration-to-connection path without that seam.
#[tokio::test]
async fn loopback_http_client_paginates_expires_cache_and_maps_headers() {
    // Handshake (discover answers -32601, so initialize and the initialized
    // notification follow), then three two-page tool listings separated by
    // the ttl-0 cache expiry, then two mapped calls: 3 + 6 + 2.
    let (declaration, url, server) = start_http_fixture(11, HttpScenario::PaginatedCache).await;
    let entry = fold_tool_name(SKILL, SERVER, "");
    let echo = fold_tool_name(SKILL, SERVER, "echo");
    let lookup = fold_tool_name(SKILL, SERVER, "lookup");
    let mut fixture = HostedFixture::with_transport(
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
        Some(url),
    )
    .await;
    fixture.prompt().await;
    fixture.turn_ended().await;
    assert_eq!(
        fixture.tool_results(&entry),
        vec![(false, format!("{echo}\n{lookup}"))],
        "the declared entry and both listing pages use the actual host publication path"
    );
    let status = fixture.status().await;
    assert!(
        status.contains("ready") && status.contains("2 tools"),
        "{status}"
    );
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
    assert!(*fixture.grants_answered.lock().expect("grant count") > 0);

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
        6,
        "the ttl-0 cache re-lists on every lookup after the initial page"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "tools/call")
            .count(),
        2
    );
    // The second page only exists behind its cursor, so reaching `lookup`
    // proves the pagination fallback followed `nextCursor`.
    assert!(
        requests[4].body.contains(r#""cursor":"page-two""#),
        "the second listing page carries the first page's cursor: {}",
        requests[4].body
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

/// A capped listing must leave the real host's partial mapped tool unknown
/// and the owned Client failed with zero tools.
#[tokio::test]
async fn loopback_http_list_page_limit_stops_publishing_partial_tools() {
    let expected_requests = 1 + LIST_PAGE_MAX;
    let (declaration, url, server) =
        start_http_fixture(expected_requests, HttpScenario::PageCap).await;
    let entry = fold_tool_name(SKILL, SERVER, "");
    let partial_name = fold_tool_name(SKILL, SERVER, "partial");
    let mut fixture = HostedFixture::with_transport(
        declaration,
        vec![
            scripted_tool_step("entry-page-cap", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step("partial-page-cap", &partial_name, "{}"),
            scripted_final_step(),
        ],
        Answer::ApproveForSession,
        Some(url),
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
        LIST_PAGE_MAX,
        "the client stops asking once the page cap is reached"
    );
    fixture.prompt().await;
    fixture.turn_ended().await;
    let partial = fixture.tool_results(&partial_name);
    assert_eq!(partial.len(), 1);
    assert!(partial[0].0);
    assert!(partial[0].1.contains("unknown tool"), "{partial:?}");
    let status = fixture.status().await;
    assert!(
        status.contains("failed") && status.contains("0 tools"),
        "incomplete pages stay unpublished in the real host: {status}"
    );
    fixture.shutdown().await;
}

/// Many individually valid pages must not exhaust the process or publish partial tools.
#[tokio::test]
async fn loopback_http_aggregate_list_budgets_reject_before_publication() {
    for (scenario, expected_requests, error_fragment) in [
        (HttpScenario::AggregateBytes, 9, "response bytes limit"),
        (HttpScenario::AggregateMetadata, 9, "metadata bytes limit"),
        (HttpScenario::AggregateTools, 3, "tool count limit"),
    ] {
        let (declaration, url, server) = start_http_fixture(expected_requests, scenario).await;
        let entry = fold_tool_name(SKILL, SERVER, "");
        let partial = fold_tool_name(SKILL, SERVER, "partial");
        let mut fixture = HostedFixture::with_transport(
            declaration,
            vec![
                scripted_tool_step("entry-list-budget", &entry, "{}"),
                scripted_final_step(),
                scripted_tool_step("partial-list-budget", &partial, "{}"),
                scripted_final_step(),
            ],
            Answer::ApproveForSession,
            Some(url),
        )
        .await;
        fixture.prompt().await;
        fixture.turn_ended().await;
        let failed = fixture.tool_results(&entry);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].0, "{failed:?}");
        assert!(
            failed[0].1.contains(error_fragment),
            "expected {error_fragment}, got {failed:?}"
        );
        let requests = finish_http_fixture(server).await;
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "tools/list")
                .count(),
            expected_requests - 1,
        );
        fixture.prompt().await;
        fixture.turn_ended().await;
        let unmapped = fixture.tool_results(&partial);
        assert_eq!(unmapped.len(), 1);
        assert!(unmapped[0].0);
        assert!(unmapped[0].1.contains("unknown tool"), "{unmapped:?}");
        let status = fixture.status().await;
        assert!(
            status.contains("failed") && status.contains("0 tools"),
            "partial listing was incorrectly published: {status}"
        );
        fixture.shutdown().await;
    }
}

#[test]
fn tool_page_rejects_oversized_metadata_fields() {
    let cases = [
        (
            format!(
                r#"{{"tools":[{{"name":"{}"}}]}}"#,
                "n".repeat(LIST_TOOL_NAME_BYTES_MAX + 1)
            ),
            "tool name bytes",
        ),
        (
            format!(
                r#"{{"tools":[{{"name":"desc","description":"{}"}}]}}"#,
                "d".repeat(LIST_TOOL_DESCRIPTION_BYTES_MAX + 1)
            ),
            "tool description bytes",
        ),
        (
            format!(
                r#"{{"tools":[{{"name":"schema","inputSchema":{{"type":"object","description":"{}"}}}}]}}"#,
                "s".repeat(LIST_TOOL_SCHEMA_BYTES_MAX)
            ),
            "tool schema bytes",
        ),
    ];
    for (body, resource) in cases {
        let error = decode_tool_page(&body).expect_err("oversized metadata must be rejected");
        assert!(
            error.to_string().contains(resource),
            "expected {resource}, got {error}"
        );
    }
}

/// This proves public HTTPS authority/port handoff and a TLS attempt only.
/// The listener supplies no certificate and no HTTP response; the existing
/// transport's certificate verification is not changed or bypassed.
#[tokio::test]
async fn approved_https_declaration_reaches_its_listener_with_tls() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("TLS listener");
    let address = listener.local_addr().expect("TLS address");
    let declaration = McpServerDecl::Http {
        endpoint: McpEndpoint::parse(&format!("https://{address}/mcp"))
            .expect("public HTTPS declaration"),
    };
    let entry = fold_tool_name(SKILL, SERVER, "");
    let mut fixture = HostedFixture::new(
        declaration,
        vec![
            scripted_tool_step("entry-https", &entry, "{}"),
            scripted_final_step(),
        ],
        Answer::ApproveForSession,
    )
    .await;
    fixture.prompt().await;
    let observe = tokio::time::timeout(Duration::from_secs(5), async move {
        let (mut socket, _) = listener.accept().await.expect("TLS connection accepted");
        let mut record = [0_u8; 5];
        socket
            .read_exact(&mut record)
            .await
            .expect("TLS record header");
        record
    });
    let ((), record) = tokio::join!(fixture.turn_ended(), observe);
    let record = record.expect("approved declaration makes a local TLS attempt");
    assert_eq!(
        record[0], 0x16,
        "the client sends a TLS handshake record, not HTTP"
    );
    assert_eq!(record[1], 0x03, "the record has a TLS protocol version");
    assert_ne!(u16::from_be_bytes([record[3], record[4]]), 0);
    assert!(*fixture.grants_answered.lock().expect("grant count") > 0);
    let failed = fixture.tool_results(&entry);
    assert_eq!(failed.len(), 1);
    assert!(
        failed[0].0,
        "a TLS attempt is not successful MCP execution: {failed:?}"
    );
    let status = fixture.status().await;
    assert!(
        status.contains("failed") && status.contains("0 tools"),
        "{status}"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn denied_https_declaration_never_connects_to_its_listener() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("TLS listener");
    let address = listener.local_addr().expect("TLS address");
    let declaration = McpServerDecl::Http {
        endpoint: McpEndpoint::parse(&format!("https://{address}/mcp"))
            .expect("public HTTPS declaration"),
    };
    let entry = fold_tool_name(SKILL, SERVER, "");
    let mut fixture = HostedFixture::new(
        declaration,
        vec![
            scripted_tool_step("entry-https-denied", &entry, "{}"),
            scripted_final_step(),
        ],
        Answer::Decline,
    )
    .await;
    fixture.prompt().await;
    fixture.turn_ended().await;
    assert!(*fixture.grants_answered.lock().expect("grant count") > 0);
    let denied = fixture.tool_results(&entry);
    assert_eq!(denied.len(), 1);
    assert!(denied[0].0);
    assert!(denied[0].1.contains("declined"), "{denied:?}");
    assert!(fixture.status().await.contains("unasked"));
    // Turn completion and session shutdown precede this bounded negative
    // observation; no sleep is used to guess when a connection would start.
    fixture.shutdown().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "a denied declaration must not queue a TCP connection"
    );
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

/// The skill declares MCP servers while its extension injects only `env`:
/// the mapped tools still publish after the declared-set grant and the
/// capability grant are both answered, and each call still passes the
/// approval ladder.
#[cfg(unix)]
#[tokio::test]
async fn declared_servers_publish_without_the_mcp_inject() {
    let entry = fold_tool_name(SKILL, SERVER, "");
    let echo = fold_tool_name(SKILL, SERVER, "echo");
    let wait = fold_tool_name(SKILL, SERVER, "wait");
    let mut fixture = HostedFixture::new(
        stdio_decl(&temp_path("no-inject.pid")),
        vec![
            scripted_tool_step("entry-no-inject", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step("echo-no-inject", &echo, r#"{"message":"hi"}"#),
            scripted_final_step(),
        ],
        Answer::ApproveForSession,
    )
    .await;
    fixture.prompt().await;
    fixture.turn_ended().await;
    assert_eq!(
        *fixture.grants_answered.lock().expect("grant count"),
        2,
        "the declared-set grant and the capability grant are separate asks"
    );
    assert_eq!(
        fixture.tool_results(&entry),
        vec![(false, format!("{echo}\n{wait}"))],
        "the mapped tools publish for a skill extension without the mcp inject"
    );
    fixture.prompt().await;
    fixture.turn_ended().await;
    assert_eq!(
        fixture.tool_results(&echo),
        vec![(false, "stdio-ok".to_owned())]
    );
    fixture.shutdown().await;
}

/// A skill-declared server stays hidden until the session approves its
/// declared set, and `ctx.mcp` stays closed to a handler whose extension
/// never injected `mcp`.
#[cfg(unix)]
#[tokio::test]
async fn declared_server_stays_hidden_until_granted_and_ctx_mcp_stays_inject_gated() {
    let pid_file = temp_path("boundary.pid");
    let entry = fold_tool_name(SKILL, SERVER, "");
    let echo = fold_tool_name(SKILL, SERVER, "echo");
    let mut fixture = HostedFixture::with_skill(
        declared_skill_with_inject_probe(stdio_decl(&pid_file)),
        vec![
            scripted_tool_step("boundary-entry", &entry, "{}"),
            scripted_final_step(),
            scripted_tool_step("boundary-echo", &echo, "{}"),
            scripted_final_step(),
            scripted_tool_step("boundary-probe", "probe", "{}"),
            scripted_final_step(),
        ],
        Answer::Decline,
    )
    .await;
    fixture.prompt().await;
    fixture.turn_ended().await;
    let entry_results = fixture.tool_results(&entry);
    assert_eq!(entry_results.len(), 1);
    assert!(
        entry_results[0].0,
        "the declined declared-set grant fails the entry call"
    );
    assert!(
        !pid_file.exists(),
        "a declined grant never starts the server"
    );
    assert!(
        fixture.status().await.contains("unasked"),
        "the declined server stays unasked"
    );
    fixture.prompt().await;
    fixture.turn_ended().await;
    let mapped = fixture.tool_results(&echo);
    assert_eq!(mapped.len(), 1);
    assert!(mapped[0].0);
    assert!(
        mapped[0].1.contains("unknown tool"),
        "an ungranted server publishes no mapped tools: {}",
        mapped[0].1
    );
    fixture.prompt().await;
    fixture.turn_ended().await;
    let probe = fixture.tool_results("probe");
    assert_eq!(probe.len(), 1);
    assert!(!probe[0].0);
    assert!(
        probe[0].1.contains("not in this extension's inject list"),
        "ctx.mcp stays closed without the inject: {}",
        probe[0].1
    );
    fixture.shutdown().await;
    let _ = fs::remove_file(&pid_file);
}

/// Two remote tools with the same name inside one server: the first maps,
/// the second is dropped and counted in the tools-mapping warning.
#[tokio::test]
async fn duplicate_remote_tool_names_drop_and_count() {
    let body = r#"{"tools":[
        {"name":"echo","description":"first","inputSchema":{"type":"object","properties":{}}},
        {"name":"echo","description":"second","inputSchema":{"type":"object","properties":{}}}
    ]}"#;
    let page = decode_tool_page(body).expect("duplicate page decodes");
    assert_eq!(page.tools.len(), 1);
    assert_eq!(page.tools[0].name, "echo");
    assert_eq!(page.tools[0].description, "first");
    let warning = page
        .excluded
        .iter()
        .map(|item| item.warning.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        warning.contains("declared by the server more than once")
            && warning.contains("(2 occurrences)"),
        "the warning names the duplicate and counts occurrences: {warning}"
    );
}

/// Independent oracles for the specified property tests. Every oracle
/// recomputes its expectation from the generated inputs alone; none calls
/// the implementation under test to produce an expectation.
mod property {
    use super::*;
    use crate::mcp::client::{StreamEvent, classify_stream_event, plan_publication};
    use crate::mcp::tools::{Key, RemoteToolPage, shape_result};
    use proptest::prelude::*;

    /// The fold oracle: the mapped-name grammar restated independently of
    /// [`fold_tool_name`] — every byte outside `[A-Za-z0-9._-]` folds to
    /// `_`, parts join with `.`, and the result caps at 200 bytes.
    fn oracle_fold(skill: &str, server: &str, tool: &str) -> String {
        let mut output = String::new();
        for (index, part) in [skill, server, tool].into_iter().enumerate() {
            if index != 0 && output.len() < 200 {
                output.push('.');
            }
            for character in part.chars() {
                if output.len() == 200 {
                    return output;
                }
                output.push(
                    if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                        character
                    } else {
                        '_'
                    },
                );
            }
        }
        output
    }

    fn fold_byte() -> impl Strategy<Value = char> {
        prop::sample::select(vec![
            'a', 'z', 'A', 'Z', '0', '9', '.', '_', '-', '/', ':', ' ', '!', 'é',
        ])
    }

    fn skill_name() -> impl Strategy<Value = String> {
        (
            prop::char::range('a', 'z'),
            prop::collection::vec(prop::char::range('a', 'z'), 0..=3),
        )
            .prop_map(|(first, rest)| std::iter::once(first).chain(rest).collect())
    }

    fn server_name() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(vec!['a', 'B', '9', '_', '-']), 1..=6)
            .prop_map(|chars| chars.into_iter().collect())
    }

    fn tool_name() -> impl Strategy<Value = String> {
        prop::collection::vec(fold_byte(), 0..=220).prop_map(|chars| chars.into_iter().collect())
    }

    fn triple() -> impl Strategy<Value = (String, String, String)> {
        (skill_name(), server_name(), tool_name())
    }

    /// Name injectivity: the implementation fold equals the oracle
    /// fold for every generated triple, and two triples share a mapped
    /// name exactly when the oracle fold collides — the independent
    /// hash-set walk, never the implementation.
    #[test]
    fn fold_matches_oracle_and_collides_only_when_oracle_collides() {
        let mut runner = proptest::test_runner::TestRunner::deterministic();
        runner
            .run(&(prop::collection::vec(triple(), 0..=40)), |triples| {
                let oracle: Vec<String> = triples
                    .iter()
                    .map(|(skill, server, tool)| oracle_fold(skill, server, tool))
                    .collect();
                let mapped: Vec<String> = triples
                    .iter()
                    .map(|(skill, server, tool)| fold_tool_name(skill, server, tool))
                    .collect();
                let mut seen = std::collections::HashSet::new();
                for (index, name) in oracle.iter().enumerate() {
                    prop_assert_eq!(&mapped[index], name, "fold drift at {}", index);
                    seen.insert(name);
                }
                for first in 0..triples.len() {
                    for second in first + 1..triples.len() {
                        prop_assert_eq!(
                            mapped[first] == mapped[second],
                            oracle[first] == oracle[second],
                            "injectivity mismatch on {}/{}",
                            first,
                            second
                        );
                    }
                }
                Ok(())
            })
            .expect("fold oracle holds");
    }

    /// Collision rule: two isolation keys whose fold collides fail
    /// publication with the mapped name and both keys.
    #[test]
    fn fold_collision_error_names_both_keys() {
        let session = SessionId::new_v7();
        let key_of = |skill: &str, server: &str| Key {
            session,
            skill: skill.to_owned(),
            server: server.to_owned(),
        };
        let first = key_of("sk", "a+b");
        let second = key_of("sk", "a_b");
        let ready = std::collections::BTreeMap::from([
            (first.clone(), vec!["p/q".to_owned()]),
            (second.clone(), vec!["p_q".to_owned()]),
        ]);
        let plan = plan_publication(&[first.clone(), second.clone()], &ready);
        let Err(collision) = plan else {
            panic!("folding collisions must fail the plan");
        };
        assert_eq!(collision.name, "sk.a_b.");
        assert_eq!(collision.first.display(), first.display());
        assert_eq!(collision.second.display(), second.display());
    }

    fn schema_value() -> String {
        r#"{"type":"object","properties":{"region":{"type":"string"}}}"#.to_owned()
    }

    /// Builds one tool object plus `count` unknown sibling fields.
    fn tool_json(name: &str, count: usize) -> String {
        let mut extras = Vec::new();
        for index in 0..count {
            extras.push(format!(
                r#""extra{index}":{}"#,
                ["7", "\"x\"", "true", "null", r#"{"a":1}"#, "[1,2]"][index % 6]
            ));
        }
        let extra = if extras.is_empty() {
            String::new()
        } else {
            format!(",{}", extras.join(","))
        };
        format!(
            r#"{{"name":"{name}","description":"{name} doc","inputSchema":{}{extra}}}"#,
            schema_value()
        )
    }

    /// Builds one tools/list result plus `count` unknown page fields.
    fn page_json(tools: &[String], count: usize) -> String {
        let mut extras = Vec::new();
        for index in 0..count {
            extras.push(format!(r#""nextHint{index}":"ignored""#));
        }
        let extra = if extras.is_empty() {
            String::new()
        } else {
            format!(",{}", extras.join(","))
        };
        format!(
            r#"{{"tools":[{}],"nextCursor":null,"ttlMs":5000{extra}}}"#,
            tools.join(",")
        )
    }

    /// The decoded projection the property compares across mutations.
    fn page_projection(page: &RemoteToolPage) -> (Vec<(String, String, String)>, usize) {
        (
            page.tools
                .iter()
                .map(|tool| {
                    (
                        tool.name.clone(),
                        tool.description.clone(),
                        tool.schema.as_str().to_owned(),
                    )
                })
                .collect(),
            page.excluded.len(),
        )
    }

    /// Tolerant decode: adding unknown fields to a tools/list page
    /// and to individual tools never changes the decoded output; the
    /// oracle independently strips the unknown keys before comparing.
    #[test]
    fn tool_page_decode_ignores_unknown_fields() {
        let mut runner = proptest::test_runner::TestRunner::deterministic();
        runner
            .run(
                &(
                    prop::collection::vec("[a-z][a-z0-9]{0,5}", 1..=3),
                    0_usize..=4,
                    0_usize..=4,
                ),
                |(tools, page_extras, tool_extras)| {
                    let decoded: Vec<(String, String, String)> = tools
                        .iter()
                        .map(|name| (name.clone(), format!("{name} doc"), schema_value()))
                        .collect();
                    let plain = page_json(
                        &tools
                            .iter()
                            .map(|name| tool_json(name, 0))
                            .collect::<Vec<_>>(),
                        0,
                    );
                    let mutated = page_json(
                        &tools
                            .iter()
                            .map(|name| tool_json(name, tool_extras))
                            .collect::<Vec<_>>(),
                        page_extras,
                    );
                    let plain = decode_tool_page(&plain).expect("plain page decodes");
                    let mutated = decode_tool_page(&mutated).expect("mutated page decodes");
                    prop_assert_eq!(
                        page_projection(&mutated),
                        page_projection(&plain),
                        "unknown fields changed the decode"
                    );
                    prop_assert_eq!(
                        page_projection(&plain).0,
                        decoded,
                        "decode drifted from the generated projection"
                    );
                    Ok(())
                },
            )
            .expect("tolerant decode holds");
    }

    /// One generated content block with its independent expected rendering.
    fn known_block() -> impl Strategy<Value = (sonic_rs::Value, String)> {
        prop::sample::select(vec![
            (
                r#"{"type":"text","text":"hello"}"#.to_owned(),
                "hello".to_owned(),
            ),
            (
                r#"{"type":"image","mimeType":"image/png","data":"aGk"}"#.to_owned(),
                "[image block: image/png, 3 base64 chars]".to_owned(),
            ),
            (
                r#"{"type":"audio","mimeType":"audio/wav","data":"aGk"}"#.to_owned(),
                "[audio block: audio/wav, 3 base64 chars]".to_owned(),
            ),
            (
                r#"{"type":"resource_link","uri":"file:///tmp/x"}"#.to_owned(),
                "[resource link: file:///tmp/x]".to_owned(),
            ),
            (
                r#"{"type":"resource","resource":{"uri":"file:///y","mimeType":"text/plain","text":"body"}}"#.to_owned(),
                "body".to_owned(),
            ),
        ])
        .prop_map(|(json, expected)| {
            (
                sonic_rs::from_str(&json).expect("generated block parses"),
                expected,
            )
        })
    }

    fn unknown_block() -> impl Strategy<Value = sonic_rs::Value> {
        (
            prop::sample::select(vec!["card", "progress_note", "tool_use"]),
            prop::collection::vec("[a-z]{0,4}", 0..=2),
        )
            .prop_map(|(kind, fields)| {
                let count = fields.len();
                sonic_rs::from_str(&format!(
                    r#"{{"type":"{kind}","payload":{{"n":1}},"meta":{count}}}"#
                ))
                .expect("generated unknown block parses")
            })
    }

    /// Unknown content block types are skipped with a count and
    /// unknown fields on known blocks change nothing; the expected text is
    /// recomputed independently from the generated blocks.
    #[test]
    fn result_shaping_skips_unknown_blocks_with_a_count() {
        let mut runner = proptest::test_runner::TestRunner::deterministic();
        runner
            .run(
                &(
                    prop::collection::vec(known_block(), 0..=4),
                    prop::collection::vec(unknown_block(), 0..=3),
                ),
                |(known, unknown)| {
                    let mut content: Vec<sonic_rs::Value> =
                        known.iter().map(|(block, _)| block.clone()).collect();
                    content.extend(unknown.iter().cloned());
                    let mut expected: Vec<String> =
                        known.iter().map(|(_, text)| text.clone()).collect();
                    if unknown.len() == 1 {
                        expected.push("mcp: skipped 1 unknown content block".to_owned());
                    } else if unknown.len() > 1 {
                        expected.push(format!(
                            "mcp: skipped {} unknown content blocks",
                            unknown.len()
                        ));
                    }
                    let shaped = shape_result(&content, false, Some("complete")).expect("shapes");
                    prop_assert_eq!(
                        shaped.text,
                        expected.join("\n"),
                        "shaping drifted from the oracle projection"
                    );
                    prop_assert!(!shaped.is_error);
                    Ok(())
                },
            )
            .expect("content shaping holds");
    }

    fn stream_event() -> impl Strategy<Value = String> {
        (
            prop::sample::select(vec![
                "notifications/progress",
                "notifications/message",
                "sampling/createMessage",
                "ping",
            ]),
            4_u64..=13,
            "[a-z0-9-]{0,8}",
        )
        .prop_map(|(method, id, token)| {
            format!(
                r#"{{"jsonrpc":"2.0","method":"{method}","id":{id},"params":{{"_meta":{{"progressToken":"t-{token}"}},"extra":"ignored"}}}}"#
            )
        })
    }

    /// Unknown notifications and progress with a foreign token
    /// never satisfy the reply wait; only the exact token resets it.
    #[test]
    fn stream_events_classify_with_unknown_traffic_ignored() {
        let mut runner = proptest::test_runner::TestRunner::deterministic();
        runner
            .run(
                &(
                    prop::collection::vec(stream_event(), 0..=6),
                    0_u64..=3,
                    "[a-z0-9-]{1,8}",
                ),
                |(junk, reply_id, token)| {
                    let reply = format!(r#"{{"jsonrpc":"2.0","id":{reply_id},"result":{{}}}}"#);
                    let reply: sonic_rs::Value =
                        sonic_rs::from_str(&reply).expect("generated reply parses");
                    let progress_token = format!("t-{token}");
                    let mut progress_hits = 0_usize;
                    let mut reply_hits = 0_usize;
                    let mut expected_progress = 0_usize;
                    for event in &junk {
                        let value: sonic_rs::Value =
                            sonic_rs::from_str(event).expect("generated event parses");
                        let matches = value.get("method").and_then(JsonValueTrait::as_str)
                            == Some("notifications/progress")
                            && value
                                .get("params")
                                .and_then(|params| params.get("_meta"))
                                .and_then(|meta| meta.get("progressToken"))
                                .and_then(JsonValueTrait::as_str)
                                == Some(progress_token.as_str());
                        if matches {
                            expected_progress += 1;
                        }
                        match classify_stream_event(&value, reply_id, &progress_token) {
                            StreamEvent::Progress => progress_hits += 1,
                            StreamEvent::Reply => {
                                prop_assert!(false, "junk event answered the wait");
                            }
                            StreamEvent::Ignored => {}
                        }
                    }
                    match classify_stream_event(&reply, reply_id, &progress_token) {
                        StreamEvent::Reply => reply_hits += 1,
                        _ => prop_assert!(false, "the correlated reply was not a reply"),
                    }
                    prop_assert_eq!(reply_hits, 1);
                    prop_assert_eq!(progress_hits, expected_progress);
                    Ok(())
                },
            )
            .expect("stream classification holds");
    }

    /// The property's isolation-key pool. The last two entries fold to the
    /// same mapped names, so generated sequences exercise both the unique
    /// and the colliding regimes.
    fn key_pool() -> Vec<Key> {
        let session = SessionId::new_v7();
        let key = |skill: &str, server: &str| Key {
            session,
            skill: skill.to_owned(),
            server: server.to_owned(),
        };
        vec![key("sk", "alpha"), key("sk", "a+b"), key("sk", "a_b")]
    }

    /// One lifecycle op over the key pool. Ready models the grant and
    /// instance-state machine: only a declared key can become ready.
    #[derive(Clone, Debug)]
    enum Op {
        Declare(usize),
        Undeclare(usize),
        Ready(usize, Vec<String>),
        Crash(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop::sample::select(vec![0_usize, 1, 2, 3])
            .prop_flat_map(|kind| {
                let index = prop::sample::select(vec![0_usize, 1, 2]);
                (Just(kind), index)
            })
            .prop_flat_map(|(kind, index)| match kind {
                0 => Just(Op::Declare(index)).boxed(),
                1 => Just(Op::Undeclare(index)).boxed(),
                2 => (
                    Just(index),
                    prop::collection::vec("[a-z][a-z0-9]{0,5}", 0..=3),
                )
                    .prop_map(|(index, tools)| Op::Ready(index, tools))
                    .boxed(),
                _ => Just(Op::Crash(index)).boxed(),
            })
    }

    /// The shadow model: recomputes the advertised names from the declared
    /// keys and ready caches with the oracle fold and an independent set
    /// walk, mirroring the (grants, instance states, cache) triple.
    #[derive(Default)]
    struct Shadow {
        declared: std::collections::BTreeSet<Key>,
        ready: std::collections::BTreeMap<Key, Vec<String>>,
    }

    impl Shadow {
        fn apply(&mut self, pool: &[Key], op: &Op) {
            match op {
                Op::Declare(index) => {
                    // An identical re-declare is a reconcile no-op: the
                    // instance and its cache persist.
                    self.declared.insert(pool_key(pool, *index));
                }
                Op::Undeclare(index) => {
                    let key = pool_key(pool, *index);
                    self.declared.remove(&key);
                    self.ready.remove(&key);
                }
                Op::Ready(index, tools) => {
                    let key = pool_key(pool, *index);
                    if self.declared.contains(&key) {
                        self.ready.insert(key, tools.clone());
                    }
                }
                Op::Crash(index) => {
                    self.ready.remove(&pool_key(pool, *index));
                }
            }
        }

        /// The expected advertisement: an entry tool per declared key plus
        /// the ready instances' mapped tools, or the colliding name. Keys
        /// walk in publication order (display-sorted) so the first
        /// collision matches the plan's.
        fn plan(&self) -> Result<std::collections::BTreeSet<String>, String> {
            let mut ordered: Vec<&Key> = self.declared.iter().collect();
            ordered.sort_by_key(|key| key.display());
            let mut seen = std::collections::BTreeMap::new();
            let mut names = std::collections::BTreeSet::new();
            for key in ordered {
                let entry = oracle_fold(&key.skill, &key.server, "");
                if let Some(other) = seen.insert(entry.clone(), key) {
                    return Err(format!(
                        "{} collides between {} and {}",
                        entry,
                        other.display(),
                        key.display()
                    ));
                }
                names.insert(entry);
            }
            for (key, tools) in &self.ready {
                for tool in tools {
                    let mapped = oracle_fold(&key.skill, &key.server, tool);
                    if let Some(other) = seen.insert(mapped.clone(), key) {
                        return Err(format!(
                            "{} collides between {} and {}",
                            mapped,
                            other.display(),
                            key.display()
                        ));
                    }
                    names.insert(mapped);
                }
            }
            Ok(names)
        }
    }

    fn pool_key(pool: &[Key], index: usize) -> Key {
        pool[index % pool.len()].clone()
    }

    /// List purity: after any interleaving of declarations, grants,
    /// crashes, and reloads, the planned advertisement equals the shadow
    /// recomputation from (declared set, instance states, caches).
    #[test]
    fn advertised_list_matches_the_shadow_model_at_every_boundary() {
        let mut runner = proptest::test_runner::TestRunner::deterministic();
        runner
            .run(&(prop::collection::vec(op(), 0..=24)), |ops| {
                let pool = key_pool();
                let mut shadow = Shadow::default();
                let mut declared: Vec<Key> = Vec::new();
                let mut ready: std::collections::BTreeMap<Key, Vec<String>> =
                    std::collections::BTreeMap::new();
                for step in &ops {
                    shadow.apply(&pool, step);
                    match step {
                        Op::Declare(index) => {
                            let key = pool_key(&pool, *index);
                            declared.retain(|existing| existing != &key);
                            declared.push(key);
                        }
                        Op::Undeclare(index) => {
                            let key = pool_key(&pool, *index);
                            declared.retain(|existing| existing != &key);
                            ready.remove(&key);
                        }
                        Op::Ready(index, tools) => {
                            let key = pool_key(&pool, *index);
                            if declared.contains(&key) {
                                ready.insert(key, tools.clone());
                            }
                        }
                        Op::Crash(index) => {
                            ready.remove(&pool_key(&pool, *index));
                        }
                    }
                }
                let expected = shadow.plan();
                declared.sort_by_key(Key::display);
                let actual = plan_publication(&declared, &ready);
                match (expected, actual) {
                    (Ok(expected), Ok(actual)) => {
                        let actual: std::collections::BTreeSet<String> = actual
                            .entries
                            .iter()
                            .map(|key| fold_tool_name(&key.skill, &key.server, ""))
                            .chain(actual.mapped.iter().map(|tool| tool.folded.clone()))
                            .collect();
                        prop_assert_eq!(
                            actual,
                            expected,
                            "advertisement drifted from the shadow model"
                        );
                    }
                    (Err(expected), Err(actual)) => {
                        prop_assert_eq!(
                            format!(
                                "{} collides between {} and {}",
                                actual.name,
                                actual.first.display(),
                                actual.second.display()
                            ),
                            expected,
                            "collision drift from the shadow model"
                        );
                    }
                    (Ok(_), Err(actual)) => {
                        prop_assert!(false, "unexpected collision: {:?}", actual);
                    }
                    (Err(expected), Ok(_)) => {
                        prop_assert!(false, "missing expected collision: {}", expected);
                    }
                }
                Ok(())
            })
            .expect("list purity holds");
    }
}
