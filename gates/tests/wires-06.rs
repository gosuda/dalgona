//! Wire server helpers: process drop and captured server lines.
#![expect(
    clippy::disallowed_methods,
    reason = "SC test drives real app-server stdio"
)]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    fs,
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    time::Duration,
};

use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait};
use support::{TestDir, dalgon_binary};

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

type ServerLines = std::sync::mpsc::Receiver<Result<String, std::io::Error>>;

fn server_lines(stdout: std::process::ChildStdout) -> ServerLines {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

fn read_response(lines: &ServerLines) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    loop {
        let line = lines.recv_timeout(Duration::from_secs(5))??;
        assert!(
            !line.contains("\"jsonrpc\""),
            "every Codex frame omits jsonrpc"
        );
        let value: sonic_rs::Value = sonic_rs::from_str(&line)?;
        if value.get("id").is_some() && value.get("method").is_none() {
            return Ok(value);
        }
    }
}

fn send_request(
    stdin: &mut std::process::ChildStdin,
    request: &sonic_rs::Value,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let payload = sonic_rs::to_string(request)?;
    stdin.write_all(payload.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn drive_handshake(
    stdin: &mut std::process::ChildStdin,
    lines: &ServerLines,
    fixture_root: &std::path::Path,
    workspace: &std::path::Path,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut thread_id = String::new();
    for name in ["initialize.request.json", "thread-start.request.json"] {
        let payload = fs::read_to_string(fixture_root.join(name))?;
        assert!(!payload.contains("jsonrpc"), "{name} must omit jsonrpc");
        let mut request: sonic_rs::Value = sonic_rs::from_str(&payload)?;
        if name == "thread-start.request.json" {
            let params = request
                .get_mut("params")
                .and_then(|params| params.as_object_mut())
                .ok_or_else(|| std::io::Error::other("fixture request has no params object"))?;
            let cwd = workspace.display().to_string();
            params.insert("cwd", sonic_rs::Value::from(&cwd));
        }
        send_request(stdin, &request)?;
        let response = read_response(lines)?;
        if name == "thread-start.request.json" {
            let id = response
                .get("result")
                .and_then(|result| result.get("thread"))
                .and_then(|thread| thread.get("id"))
                .and_then(sonic_rs::Value::as_str)
                .ok_or_else(|| std::io::Error::other("thread/start result names no thread"))?;
            id.clone_into(&mut thread_id);
        } else {
            stdin.write_all(br#"{"method":"initialized","params":{}}"#)?;
            stdin.write_all(b"\n")?;
            stdin.flush()?;
        }
    }
    Ok(thread_id)
}

/// One scripted-model step: tool call then end-turn text.
fn scripted_wire_step(
    tool: &str,
    arguments: &sonic_rs::Value,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let call_id = format!("call-{tool}");
    let first = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "tool_call_started", "id": call_id.as_str(), "name": tool},
            {"type": "tool_calls_done", "calls": [{
                "id": call_id.as_str(),
                "name": tool,
                "args": {"kind": "parsed", "value": arguments}
            }]},
            {"type": "usage", "usage": {
                "input_tokens": 1, "cached_input_tokens": 0, "output_tokens": 1,
                "reasoning_tokens": null, "cache_write_tokens": 0, "cost_usd": null
            }},
            {"type": "stop", "reason": "tool_use"}
        ]
    });
    let second = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "text_delta", "text": "done"},
            {"type": "tool_calls_done", "calls": []},
            {"type": "usage", "usage": {
                "input_tokens": 1, "cached_input_tokens": 0, "output_tokens": 1,
                "reasoning_tokens": null, "cache_write_tokens": 0, "cost_usd": null
            }},
            {"type": "stop", "reason": "end_turn"}
        ]
    });
    Ok(format!(
        "{}\n{}\n",
        sonic_rs::to_string(&first)?,
        sonic_rs::to_string(&second)?
    ))
}

/// Approval accept reply with caller id.
fn approval_reply(
    request: &sonic_rs::Value,
    thread: &str,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    let params = request
        .get("params")
        .ok_or("approval request has no params")?;
    let params_thread = params
        .get("threadId")
        .and_then(sonic_rs::Value::as_str)
        .ok_or("approval request has no threadId")?;
    assert_eq!(params_thread, thread, "approval thread identity must match");
    assert!(
        params
            .get("turnId")
            .and_then(sonic_rs::Value::as_str)
            .is_some(),
        "approval request has no turnId"
    );
    let id = request
        .get("id")
        .cloned()
        .ok_or("approval request has no id")?;
    Ok(sonic_rs::json!({"id": id, "result": {"decision": "accept"}}))
}

/// User-input reply keyed by live question id.
fn input_reply(
    request: &sonic_rs::Value,
    thread: &str,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    let params = request
        .get("params")
        .ok_or("user-input request has no params")?;
    let params_thread = params
        .get("threadId")
        .and_then(sonic_rs::Value::as_str)
        .ok_or("user-input request has no threadId")?;
    assert_eq!(
        params_thread, thread,
        "user-input thread identity must match"
    );
    let question_id = params
        .get("questions")
        .and_then(sonic_rs::Value::as_array)
        .and_then(|questions| questions.first())
        .and_then(|question| question.get("id"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or("user-input request has no question id")?
        .to_owned();
    let mut result = sonic_rs::json!({"answers": {}});
    result
        .get_mut("answers")
        .and_then(|answers| answers.as_object_mut())
        .ok_or("user-input answers is not an object")?
        .insert(
            question_id.as_str(),
            sonic_rs::json!({"answers": ["gate answer"]}),
        );
    let id = request
        .get("id")
        .cloned()
        .ok_or("user-input request has no id")?;
    Ok(sonic_rs::json!({"id": id, "result": result}))
}

fn assert_tool_effect(item: &sonic_rs::Value, expected: &str) {
    assert_eq!(
        item.get("success").and_then(sonic_rs::Value::as_bool),
        Some(true),
        "accepted reply must let the tool succeed: {item}"
    );
    let observed = item
        .get("contentItems")
        .and_then(sonic_rs::Value::as_array)
        .is_some_and(|parts| {
            parts.iter().any(|part| {
                part.get("text")
                    .and_then(sonic_rs::Value::as_str)
                    .is_some_and(|text| text.contains(expected))
            })
        });
    assert!(observed, "tool result must contain {expected:?}: {item}");
}

/// Pumps one turn until completed, answering approval and input legs.
fn pump_one_turn(
    stdin: &mut std::process::ChildStdin,
    lines: &ServerLines,
    thread: &str,
    expected: (&str, &str),
) -> Result<(bool, bool), Box<dyn Error + Send + Sync>> {
    let mut saw_approval = false;
    let mut saw_input = false;
    let mut saw_effect = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let line = match lines.recv_timeout(timeout) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err("timed out waiting for turn/completed".into()),
        };
        if line.is_empty() {
            continue;
        }
        assert!(
            !line.contains("\"jsonrpc\""),
            "every Codex frame omits jsonrpc"
        );
        let value: sonic_rs::Value = sonic_rs::from_str(&line)?;
        if value.get("error").is_some() {
            return Err(format!("Codex reply carries error: {value}").into());
        }
        let Some(method) = value
            .get("method")
            .and_then(sonic_rs::Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        if method == "turn/completed" {
            let status = value
                .get("params")
                .and_then(|params| params.get("turn"))
                .and_then(|turn| turn.get("status"))
                .and_then(sonic_rs::Value::as_str);
            assert_eq!(
                status,
                Some("completed"),
                "turn must complete after accepted answers"
            );
            assert!(saw_effect, "turn must deliver the expected tool result");
            return Ok((saw_approval, saw_input));
        }
        let is_approval = method == "item/commandExecution/requestApproval"
            || method == "item/fileChange/requestApproval";
        if is_approval {
            send_request(stdin, &approval_reply(&value, thread)?)?;
            saw_approval = true;
            continue;
        }
        if method == "item/tool/requestUserInput" {
            send_request(stdin, &input_reply(&value, thread)?)?;
            saw_input = true;
            continue;
        }
        if method == "item/completed" {
            let item = value
                .get("params")
                .and_then(|params| params.get("item"))
                .ok_or("completed item is missing")?;
            if item.get("tool").and_then(sonic_rs::Value::as_str) == Some(expected.0) {
                assert_tool_effect(item, expected.1);
                saw_effect = true;
            }
            continue;
        }
        if matches!(
            method.as_str(),
            "turn/started" | "thread/started" | "item/started" | "item/agentMessage/delta"
        ) {
            continue;
        }
        return Err(format!("unexpected Codex method {method}").into());
    }
}

struct WireGatePaths {
    dir: TestDir,
    home: std::path::PathBuf,
    workspace: std::path::PathBuf,
    data_home: std::path::PathBuf,
}

fn setup_wire_gate_fixture() -> Result<WireGatePaths, Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    let config_dir = home.join(".config/dal");
    let plugin_dir = data_home.join("dal/plugins/wire-gate");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&plugin_dir)?;
    fs::create_dir_all(&workspace)?;
    fs::write(
        plugin_dir.join("plugin.star"),
        r#"load("@dal/v1", "dal")

def run_tool(ctx, args):
    return ctx.tools.exec(args.command)

run = dal.tool(
    description = "Gate command approval.",
    input = dal.schema(command = dal.string()),
    uses = ["tools.exec"],
    run = run_tool,
)

def ask_tool(ctx, args):
    return ctx.ask.text(prompt = args.prompt)

ask = dal.tool(
    description = "Gate user input.",
    input = dal.schema(prompt = dal.string()),
    uses = ["ask.text"],
    run = ask_tool,
)

plugin = dal.plugin(
    name = "wire-gate",
    version = "0.1.0",
    tools = {"run": run, "ask": ask},
)
"#,
    )?;
    let replay_path = home.join("wire-gate.jsonl");
    let mut replay_text = String::new();
    replay_text.push_str(&scripted_wire_step(
        "wire-gate__run",
        &sonic_rs::json!({"command": "printf '%s%s' gate -approved"}),
    )?);
    replay_text.push_str(&scripted_wire_step(
        "wire-gate__ask",
        &sonic_rs::json!({"prompt": "enter gate text"}),
    )?);
    fs::write(&replay_path, replay_text)?;
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "approval = \"ask\"\nmodel = \"openai/gpt-6\"\nplugins = [\"wire-gate\"]\n[providers.scripted]\nfixture = {:?}\n",
            replay_path.to_string_lossy()
        ),
    )?;
    Ok(WireGatePaths {
        dir,
        home,
        workspace,
        data_home,
    })
}

#[test]
fn codex_app_server_smoke_uses_pinned_core_subset() -> Result<(), Box<dyn Error + Send + Sync>> {
    let WireGatePaths {
        dir: _dir,
        home,
        workspace,
        data_home,
    } = setup_wire_gate_fixture()?;
    let binary = dalgon_binary()?;
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/codex-app-server");

    let mut child = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "sk-test")
        .args(["app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut guard = ChildGuard(Some(child));
    let lines = server_lines(stdout);

    let thread = drive_handshake(&mut stdin, &lines, &fixture_root, &workspace)?;

    let first_turn = sonic_rs::json!({
        "id": 10,
        "method": "turn/start",
        "params": {
            "threadId": thread.as_str(),
            "input": [{"type": "text", "text": "run the scripted wire leg"}],
        }
    });
    send_request(&mut stdin, &first_turn)?;
    let (approval_seen, _) = pump_one_turn(
        &mut stdin,
        &lines,
        &thread,
        ("wire-gate__run", "gate-approved"),
    )?;
    assert!(approval_seen, "approval leg must be reached and accepted");

    let second_turn = sonic_rs::json!({
        "id": 20,
        "method": "turn/start",
        "params": {
            "threadId": thread.as_str(),
            "input": [{"type": "text", "text": "run the scripted wire leg"}],
        }
    });
    send_request(&mut stdin, &second_turn)?;
    let (_, input_seen) = pump_one_turn(
        &mut stdin,
        &lines,
        &thread,
        ("wire-gate__ask", "gate answer"),
    )?;
    assert!(input_seen, "user-input leg must be reached and answered");

    stdin.write_all(br#"{"id":99,"method":"unknown/method","params":{}}"#)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    drop(stdin);
    let mut saw_32601 = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let Ok(Ok(line)) = lines.recv_timeout(timeout) else {
            break;
        };
        if line.contains("-32601") {
            saw_32601 = true;
            break;
        }
    }
    assert!(saw_32601, "unsupported method must return -32601");
    if let Some(mut child) = guard.0.take() {
        let _ = child.kill();
    }
    Ok(())
}
