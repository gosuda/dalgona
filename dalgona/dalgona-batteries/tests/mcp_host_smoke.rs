// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Real-Host MCP smoke (v2): discover → invoke (identity) → cancel held call →
//! close → durable journal read-back → no publish after close.
//!
//! PARENT PLACEMENT (exact): `dalgona/dalgona-batteries/tests/mcp_host_smoke.rs`.
//! No Cargo.toml change: `tests/` auto-discovers; every crate used here
//! (`dal-agent`, `dal-core`, `dal-store`, `dalgona-batteries`, `tokio`) is
//! already a dependency of `dalgona-batteries`.
//! PARENT RUN (exact, from workspace root):
//!   env -u CARGO_BUILD_BUILD_DIR CARGO_TARGET_DIR=<target> TMPDIR=<tmp> CARGO_BUILD_JOBS=2 \
//!     cargo test --locked --offline -p dalgona-batteries --test mcp_host_smoke
//! FEATURE FLAGS: none. Default features. Unix only (`#![cfg(unix)]`).
//!
//! SCOPE: cancels only an in-flight `tools/call` on a ready server. It never
//! cancels during the start handshake and never abandons a concurrent send
//! on the shared stdin pipe; one call runs at a time.
//!
//! BOUNDARY: real `Host`, real bundled `dalgona_batteries::mcp::mcp()`
//! battery, real Services, approval door, and stdio transport. The only
//! scripted seam is the model edge (`[providers.scripted]` fixture, the
//! convention of `src/mcp/tests.rs`). The child is an owned `/bin/sh` stdio
//! server. Durability is read back after close through the product store:
//! `dal_store::Store::read_session_entry` (read-only, lock-free) and the
//! journal path from `Store::session_file`, decoded with `dal_core::decode`.
//! No journal record is written or manufactured by the harness.
//!
//! EVIDENCE: the data root, workspace, and fixture files are kept on pass
//! and on failure; every failure message names the evidence directory.

#![cfg(unix)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, ShutdownReport};
use dal_core::{
    Answer, CallId, CancelScope, ClientId, Command, Config, ConfigProduct, EntryId, EntryKind,
    Expect, JournalPart, Output, PageReq, Part, Question, Record, Reply, SessionId, TurnEndStop,
    TurnId, UpdateKind, Workspace,
    ext::{McpBlock, McpLaunch, McpServerDecl, Name, SkillRecord},
};

const SKILL: &str = "weather";
const SERVER: &str = "fixture";
/// `fold_tool_name(skill, server, "")` pushes `.` before the empty third
/// segment, so the server entry tool keeps a trailing dot.
const ENTRY_TOOL: &str = "weather.fixture.";
const ECHO_TOOL: &str = "weather.fixture.echo";
const WAIT_TOOL: &str = "weather.fixture.wait";
/// The exact bytes the fixture server returns for `echo`.
const ECHO_TOKEN: &str = "HOST-SMOKE-OK";
/// Progress text the fixture emits before it answers `echo`.
const PROGRESS_MARK: &str = "smoke-working";
/// Time the journal must stay byte-identical after close and child exit.
const QUIET_WINDOW: Duration = Duration::from_millis(500);

/// Owned stdio fixture server, one call at a time. `wait` never answers;
/// `echo` answers with [`ECHO_TOKEN`]; a cancellation is recorded verbatim.
const SERVER_SCRIPT: &str = r#"
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
      printf '%s' "$id" > "$MCP_CALL_FILE"
      printf 'w' >> "$MCP_PID_FILE"
      ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","method":"notifications/progress","params":{"_meta":{"progressToken":"t-%s"},"message":"smoke-working"}}\n' "$id"
      sleep 0.05
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"HOST-SMOKE-OK"}],"isError":false}}\n' "$id"
      ;;
    *'"method":"notifications/cancelled"'*)
      printf '%s' "$line" > "$MCP_CANCEL_FILE"
      ;;
  esac
done
"#;

/// One evidence directory per run; never removed by the harness.
fn evidence_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "dalgona-mcp-host-smoke-{}-{}",
        std::process::id(),
        SessionId::new_v7()
    ))
}

fn hosted_env(workspace: &Path) -> Env {
    Env {
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
        cwd: workspace.to_path_buf(),
        sandbox_helper: None,
    }
}

fn scripted_tool_step(id: &str, name: &str, args: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"{id}","name":"{name}"}},{{"type":"tool_calls_done","calls":[{{"id":"{id}","name":"{name}","args":{{"kind":"parsed","value":{args}}}}}]}},{{"type":"usage","usage":{{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}}},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

fn scripted_final_step() -> String {
    r#"{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#.to_owned()
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

/// Monotonic bounded async wait: polls every 10 ms until `deadline`.
async fn wait_until(deadline: Instant, what: &str, mut ready: impl FnMut() -> bool) {
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(ready(), "host-smoke timed out waiting for {what}");
}

#[cfg(target_os = "linux")]
fn process_is_live(pid: u32) -> bool {
    // A zombie already exited; only a non-zombie /proc entry is live.
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
    })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_is_live(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

/// One tool-result entry as the live view and the durable journal show it.
#[derive(Clone, Debug, PartialEq)]
struct ResultEntry {
    id: EntryId,
    call: CallId,
    name: String,
    error: bool,
    text: String,
}

fn result_entry(id: EntryId, kind: &EntryKind) -> Option<ResultEntry> {
    let EntryKind::ToolResult {
        call,
        name,
        error,
        parts,
        ..
    } = kind
    else {
        return None;
    };
    Some(ResultEntry {
        id,
        call: call.clone(),
        name: name.to_string(),
        error: *error,
        text: journal_text(parts),
    })
}

/// The real Host, its one session handle, and the live subscription.
/// No `Drop` impl: the explicit [`SmokeHost::close`] owns orderly shutdown,
/// and a panic drops fields in declaration order without moving them.
struct SmokeHost {
    host: Host,
    agent: Agent,
    subscription: dal_agent::Subscription,
    notices: Vec<String>,
    evidence: PathBuf,
}

impl SmokeHost {
    async fn start(
        server: McpServerDecl,
        steps: Vec<String>,
        data: &Path,
        workspace: &Path,
    ) -> Self {
        let evidence = data.parent().unwrap_or(data).to_path_buf();
        fs::create_dir_all(data).expect("host data directory");
        fs::create_dir_all(workspace).expect("host workspace directory");
        let script = data.join("script.jsonl");
        fs::write(&script, format!("{}\n", steps.join("\n"))).expect("scripted provider fixture");
        let user = format!(
            "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            script.display()
        );
        let config =
            Config::load(ConfigProduct::Dalgon, data, "", Some(&user)).expect("host config");
        let mcp_config = dalgona_batteries::mcp::McpConfig {
            tokens_path: data.join("mcp").join("tokens.json"),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let skill = SkillRecord {
            name: Name::parse(SKILL).expect("skill name"),
            description: "Host smoke fixture".into(),
            body: "Host smoke fixture skill".into(),
            letter2image: false,
            mcp: Some(McpBlock {
                servers: BTreeMap::from([(SERVER.into(), server)]),
            }),
        };
        let inject = dal_core::ServiceSet::from_names(["env"]).expect("service names");
        let skill_extension = dal_agent::ext::ExtensionBuilder::new("weather", "0.1.0", inject)
            .expect("skill extension")
            .with_origin(dal_core::Origin::Bundled, None)
            .skill(skill)
            .build()
            .expect("skill extension registration");
        let battery =
            dalgona_batteries::mcp::mcp(&mcp_config).expect("bundled mcp battery registration");
        let product = Product {
            name: "dalgona",
            data_root: data.to_path_buf(),
            defaults: "",
            extensions: vec![skill_extension, battery],
            bundled: Vec::new(),
        };
        let host = Host::start(product, config, hosted_env(workspace))
            .await
            .expect("host starts");
        let agent = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(workspace.to_path_buf()).expect("workspace"),
                    name: None,
                },
                ClientId::new("mcp-host-smoke"),
            )
            .await
            .expect("session opens");
        let subscription = agent.subscribe(None).expect("subscription");
        let smoke = Self {
            host,
            agent,
            subscription,
            notices: Vec::new(),
            evidence,
        };
        // Public readiness gate: `/mcp` reports the declared server once the
        // session's extension generation is published.
        let ready_by = Instant::now() + Duration::from_secs(10);
        let mut declared = false;
        while Instant::now() < ready_by && !declared {
            smoke.answer_pending().await;
            declared = smoke.status().await.contains("declared");
            if !declared {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        assert!(
            declared,
            "mcp status never reported the declared server; evidence {}",
            smoke.evidence.display()
        );
        smoke
    }

    /// Answers one pending question through the real approval door.
    async fn answer_pending(&self) -> bool {
        let view = self.agent.view(PageReq::default()).expect("host view");
        let Some(request) = view.open.first() else {
            return false;
        };
        let answer = match &request.question {
            Question::Grant { .. } | Question::Approval { .. } => Answer::ApproveForSession,
            _ => Answer::Decline,
        };
        self.agent
            .answer(request.id, answer)
            .await
            .expect("front-end answer");
        true
    }

    /// Drives updates until a turn ends; at most 300 rounds of 100 ms.
    async fn turn_ended(&mut self) -> TurnId {
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
            if let UpdateKind::TurnEnded { turn, .. } = update.kind {
                return turn;
            }
        }
        panic!(
            "scripted turn did not end within its bound; evidence {}",
            self.evidence.display()
        );
    }

    async fn prompt_turn(&self) -> TurnId {
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
        let Reply::Accepted { turn, .. } = reply else {
            panic!("prompt: {reply:?}");
        };
        turn
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
            .expect("mcp status command")
        {
            Reply::Done(Output::Markdown(text)) => text.into(),
            reply => panic!("unexpected mcp status reply: {reply:?}"),
        }
    }

    fn results(&self, name: &str) -> Vec<ResultEntry> {
        self.agent
            .view(PageReq::default())
            .expect("host view")
            .entries
            .items
            .iter()
            .filter_map(|entry| result_entry(entry.id, &entry.kind))
            .filter(|entry| entry.name == name)
            .collect()
    }

    /// Closes every session through the real Host and returns its report.
    /// The session handle stays alive until the Host has closed the session,
    /// so the report counts this session.
    async fn close(self) -> ShutdownReport {
        let Self {
            host,
            agent,
            subscription,
            ..
        } = self;
        let report = host.shutdown(Duration::from_secs(5)).await;
        drop(subscription);
        drop(agent);
        report
    }
}

/// Decodes every complete line of the durable journal with the product codec.
fn durable_records(journal: &Path, evidence: &Path) -> Vec<Record> {
    let bytes = fs::read(journal).unwrap_or_else(|error| {
        panic!(
            "durable journal {} unreadable: {error}; evidence {}",
            journal.display(),
            evidence.display()
        )
    });
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            dal_core::decode(line)
                .unwrap_or_else(|error| {
                    panic!(
                        "journal line failed product decode: {error:?}; evidence {}",
                        evidence.display()
                    )
                })
                .record
        })
        .collect()
}

fn durable_results(records: &[Record], call: &CallId) -> Vec<ResultEntry> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::ToolResult(entry) => result_entry(entry.id, &entry.kind),
            _ => None,
        })
        .filter(|entry| &entry.call == call)
        .collect()
}

#[tokio::test]
async fn host_smoke_discover_invoke_cancel_close_and_read_back() {
    let root = evidence_root();
    let data = root.join("host-data");
    let workspace = root.join("host-workspace");
    let pid_file = root.join("server.pid");
    let call_file = root.join("server.call");
    let cancel_file = root.join("server.cancel");
    fs::create_dir_all(&root).expect("evidence directory");

    let mut env = BTreeMap::new();
    for (name, path) in [
        ("MCP_PID_FILE", &pid_file),
        ("MCP_CALL_FILE", &call_file),
        ("MCP_CANCEL_FILE", &cancel_file),
    ] {
        env.insert(
            name.into(),
            path.to_string_lossy().into_owned().into_boxed_str(),
        );
    }
    let server = McpServerDecl::Stdio {
        launch: McpLaunch::command(vec!["/bin/sh".into(), "-c".into(), SERVER_SCRIPT.into()])
            .expect("fixture launch"),
        env,
    };

    let mut smoke = SmokeHost::start(
        server,
        vec![
            scripted_tool_step("entry-1", ENTRY_TOOL, "{}"),
            scripted_final_step(),
            scripted_tool_step("echo-1", ECHO_TOOL, r#"{"message":"hello"}"#),
            scripted_final_step(),
            scripted_tool_step("wait-1", WAIT_TOOL, "{}"),
        ],
        &data,
        &workspace,
    )
    .await;
    let session = smoke.agent.session();
    let at = root.display().to_string();

    // 1. Discover through the real tool boundary.
    smoke.prompt_turn().await;
    smoke.turn_ended().await;
    assert!(
        smoke.status().await.contains("ready"),
        "server not ready after discovery; evidence {at}"
    );
    let discovered = smoke.results(ENTRY_TOOL);
    assert_eq!(
        discovered.len(),
        1,
        "entry tool result count; evidence {at}"
    );
    assert!(
        !discovered[0].error
            && discovered[0].text.contains(ECHO_TOOL)
            && discovered[0].text.contains(WAIT_TOOL),
        "discovery did not publish both tools: {:?}; evidence {at}",
        discovered[0]
    );

    // 2. Invoke: the result text is the server's own bytes.
    smoke.prompt_turn().await;
    smoke.turn_ended().await;
    let echo = smoke.results(ECHO_TOOL);
    assert_eq!(echo.len(), 1, "echo result count; evidence {at}");
    let echo = echo[0].clone();
    assert!(
        !echo.error && echo.text == ECHO_TOKEN,
        "echo result identity mismatch: {echo:?}; evidence {at}"
    );
    assert!(
        smoke
            .notices
            .iter()
            .any(|notice| notice.contains(PROGRESS_MARK)),
        "server progress never reached the host: {:?}; evidence {at}",
        smoke.notices
    );

    // 3. Hold a call, then cancel its turn once the server has it.
    let held_turn = smoke.prompt_turn().await;
    let call_seen_by = Instant::now() + Duration::from_secs(10);
    let mut saw_call = false;
    while Instant::now() < call_seen_by && !saw_call {
        smoke.answer_pending().await;
        saw_call = fs::read_to_string(&pid_file).is_ok_and(|contents| contents.ends_with('w'))
            && fs::read_to_string(&call_file).is_ok_and(|id| !id.is_empty());
        if !saw_call {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    assert!(
        saw_call,
        "server never received the held call; evidence {at}"
    );
    let request_id = fs::read_to_string(&call_file).expect("recorded request id");

    let cancelled_at = Instant::now();
    smoke
        .agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(held_turn),
        })
        .await
        .expect("cancel the held turn");
    let ended = smoke.turn_ended().await;
    assert_eq!(ended, held_turn, "a different turn ended; evidence {at}");
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(10),
        "cancelled turn waited on the call: {:?}; evidence {at}",
        cancelled_at.elapsed()
    );
    assert!(
        smoke.status().await.contains("ready"),
        "a cancelled call must not read as a crash; evidence {at}"
    );
    // The cancel fold settles every in-flight call with exactly one error
    // result (dal-core fold cancel path), so an empty set is a failure.
    let held_live = smoke.results(WAIT_TOOL);
    assert!(
        held_live.len() == 1 && held_live[0].error,
        "held call must have exactly one error result: {held_live:?}; evidence {at}"
    );

    let notice_by = Instant::now() + Duration::from_secs(5);
    wait_until(notice_by, "server cancellation notice", || {
        fs::read_to_string(&cancel_file).is_ok_and(|note| !note.is_empty())
    })
    .await;
    let note = fs::read_to_string(&cancel_file).expect("cancellation notice");
    assert!(
        note.contains("notifications/cancelled")
            && note.contains(&format!("\"requestId\":{request_id}")),
        "notice does not name request {request_id}: {note}; evidence {at}"
    );

    // 4. Close through the real Host and assert its report contract.
    let pid: u32 = fs::read_to_string(&pid_file)
        .expect("child pid")
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("numeric child pid");
    let evidence = smoke.evidence.clone();
    let report = smoke.close().await;
    assert_eq!(
        report,
        ShutdownReport {
            sessions_closed: 1,
            status_quiet: true,
            tasks_remaining: 0,
        },
        "host shutdown report; evidence {at}"
    );
    let exit_by = Instant::now() + Duration::from_secs(5);
    wait_until(exit_by, "MCP child exit after close", || {
        !process_is_live(pid)
    })
    .await;

    // 5. Durable read-back after close, through the product store.
    let store = dal_store::Store::new(
        data.clone(),
        Workspace::new(workspace.clone()).expect("workspace"),
        dal_core::Product::Dalgona,
    );
    let journal = store.session_file(session);
    let durable_echo = dal_store::Store::read_session_entry(&data, session, echo.id)
        .unwrap_or_else(|error| panic!("durable echo entry: {error:?}; evidence {at}"));
    assert_eq!(
        result_entry(durable_echo.id, &durable_echo.kind),
        Some(echo.clone()),
        "durable echo entry differs from the live result; evidence {at}"
    );

    let closed_bytes = fs::read(&journal).expect("journal bytes after close");
    let records = durable_records(&journal, &evidence);
    assert_eq!(
        durable_results(&records, &echo.call),
        vec![echo.clone()],
        "echo call must have exactly one durable result; evidence {at}"
    );
    for held in &held_live {
        let durable = durable_results(&records, &held.call);
        assert_eq!(
            durable,
            vec![held.clone()],
            "held call durable result differs from the live one; evidence {at}"
        );
    }
    let durable_held: Vec<ResultEntry> = records
        .iter()
        .filter_map(|record| match record {
            Record::ToolResult(entry) => result_entry(entry.id, &entry.kind),
            _ => None,
        })
        .filter(|entry| entry.name == WAIT_TOOL)
        .collect();
    assert_eq!(
        durable_held, held_live,
        "held call must have exactly one durable error result; evidence {at}"
    );
    assert!(
        records.iter().any(|record| matches!(
            record,
            Record::TurnEnd { turn, stop: TurnEndStop::Cancelled, .. } if *turn == held_turn
        )),
        "journal lacks the cancelled end of the held turn; evidence {at}"
    );

    // 6. No publish after close: the journal and the server's cancel record
    //    stay byte-identical across a quiet window after the child is gone.
    tokio::time::sleep(QUIET_WINDOW).await;
    assert_eq!(
        fs::read(&journal).expect("journal bytes after quiet window"),
        closed_bytes,
        "journal changed after close; evidence {at}"
    );
    assert_eq!(
        fs::read_to_string(&cancel_file).expect("cancel record after quiet window"),
        note,
        "server cancel record changed after close; evidence {at}"
    );
}
