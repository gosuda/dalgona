#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test launches the real dalgon sandbox boundary"
)]

//! `rm` tool request fixtures and scripted file-removal turns.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, io, path::Path, time::Duration};

use dal_agent::SessionRef;
use dal_core::{Command as AgentCommand, Expect, Part, Workspace};
use sonic_rs::{JsonValueTrait, Value};
use support::{TestDir, dalgon_binary};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{ChildStdin, ChildStdout, Command},
};
#[cfg(windows)]
const PROCESS_PATH: &str = r"C:\Windows\System32;C:\Windows";
#[cfg(not(windows))]
const PROCESS_PATH: &str = "/usr/bin:/bin";

fn scripted_rm_fixture(sentinel: &Path) -> Result<String, Box<dyn Error + Send + Sync>> {
    let command = format!("/bin/rm -- {}", sentinel.display());
    let attempt = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "tool_call_started", "id": "call-rm", "name": "exec"},
            {"type": "tool_calls_done", "calls": [{
                "id": "call-rm",
                "name": "exec",
                "args": {"kind": "parsed", "value": {"command": command, "timeout_seconds": 10}}
            }]},
            {"type": "usage", "usage": {
                "input_tokens": 1,
                "cached_input_tokens": 0,
                "output_tokens": 1,
                "reasoning_tokens": null,
                "cache_write_tokens": 0,
                "cost_usd": null
            }},
            {"type": "stop", "reason": "tool_use"}
        ]
    });
    let answer = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "text_delta", "text": "sandbox probe completed"},
            {"type": "tool_calls_done", "calls": []},
            {"type": "usage", "usage": {
                "input_tokens": 1,
                "cached_input_tokens": 0,
                "output_tokens": 1,
                "reasoning_tokens": null,
                "cache_write_tokens": 0,
                "cost_usd": null
            }},
            {"type": "stop", "reason": "end_turn"}
        ]
    });
    Ok(format!(
        "{}\n{}\n",
        sonic_rs::to_string(&attempt)?,
        sonic_rs::to_string(&answer)?
    ))
}

async fn write_request(
    input: &mut ChildStdin,
    id: i64,
    method: &str,
    params: Value,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let text = sonic_rs::to_string(&sonic_rs::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    }))?;
    input.write_all(text.as_bytes()).await?;
    input.write_all(b"\n").await?;
    Ok(())
}

async fn read_frame(
    output: &mut Lines<BufReader<ChildStdout>>,
) -> Result<Value, Box<dyn Error + Send + Sync>> {
    let line = tokio::time::timeout(Duration::from_secs(10), output.next_line()).await??;
    let Some(line) = line else {
        return Err(io::Error::other("dalgon RPC closed before replying").into());
    };
    Ok(sonic_rs::from_str(&line)?)
}

async fn response(
    input: &mut ChildStdin,
    output: &mut Lines<BufReader<ChildStdout>>,
    id: i64,
    method: &str,
    params: Value,
) -> Result<Value, Box<dyn Error + Send + Sync>> {
    write_request(input, id, method, params).await?;
    loop {
        let frame = read_frame(output).await?;
        if frame.get("id").and_then(sonic_rs::JsonValueTrait::as_i64) == Some(id) {
            return Ok(frame);
        }
    }
}

fn tool_error_text(update: &Value) -> Option<String> {
    let outcome = update.get("outcome")?;
    if outcome
        .get("isError")
        .and_then(sonic_rs::JsonValueTrait::as_bool)
        != Some(true)
    {
        return None;
    }
    outcome
        .get("text")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

#[expect(
    clippy::too_many_lines,
    reason = "SC sandbox probes are single long scripts"
)]
async fn run_sandbox_probe() -> Result<(String, bool), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let config_dir = home.join(".config/dal");
    let data_home = home.join(".local/share");
    let workspace = dir.path().join("workspace");
    let allowed_temp = dir.path().join("allowed-temp");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&allowed_temp)?;
    fs::create_dir_all(&outside)?;
    let sentinel = outside.join("sandbox-sentinel.txt");
    fs::write(&sentinel, "owned by this test\n")?;
    let fixture = dir.path().join("sandbox-scripted.jsonl");
    fs::write(&fixture, scripted_rm_fixture(&sentinel)?)?;
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\napproval = \"all\"\nsandbox = true\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;

    let mut child = Command::new(dalgon_binary("dalgon")?)
        .args(["rpc"])
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("PATH", PROCESS_PATH)
        .env("TMPDIR", &allowed_temp)
        .env("TMP", &allowed_temp)
        .env("TEMP", &allowed_temp)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().expect("RPC stdin is piped");
    let stdout = child.stdout.take().expect("RPC stdout is piped");
    let mut output = BufReader::new(stdout).lines();
    let initialized = response(
        &mut input,
        &mut output,
        1,
        "initialize",
        sonic_rs::json!({
            "protocolVersion": 1,
            "clientInfo": {"name": "sandbox-gate", "version": "1"},
            "capabilities": ["sessions"]
        }),
    )
    .await?;
    assert!(initialized.get("result").is_some(), "{initialized}");
    let session_ref = sonic_rs::to_value(&SessionRef::Ephemeral {
        workspace: Workspace::new(workspace)?,
    })?;
    let opened = response(
        &mut input,
        &mut output,
        2,
        "session/open",
        sonic_rs::json!({"ref": session_ref}),
    )
    .await?;
    let result = opened
        .get("result")
        .ok_or_else(|| io::Error::other("RPC session/open failed"))?;
    let session_id = result
        .get("sessionId")
        .and_then(|value| value.as_str())
        .ok_or_else(|| io::Error::other("RPC session/open omitted sessionId"))?;
    let generation = result
        .get("gen")
        .and_then(sonic_rs::JsonValueTrait::as_u64)
        .ok_or_else(|| io::Error::other("RPC session/open omitted generation"))?;
    let sequence = result
        .get("view")
        .and_then(|view| view.get("seq"))
        .and_then(sonic_rs::JsonValueTrait::as_u64)
        .ok_or_else(|| io::Error::other("RPC session/open omitted sequence"))?;
    let subscription = response(
        &mut input,
        &mut output,
        3,
        "session/subscribe",
        sonic_rs::json!({"sessionId": session_id, "gen": generation, "after": sequence}),
    )
    .await?;
    assert!(subscription.get("result").is_some(), "{subscription}");
    let prompt = AgentCommand::Prompt {
        expect: Expect::Idle,
        content: vec![Part::Text {
            text: "Try to remove the absolute test-owned sentinel.".into(),
        }],
    };
    let command = sonic_rs::to_value(&prompt)?;
    write_request(
        &mut input,
        4,
        "session/submit",
        sonic_rs::json!({"sessionId": session_id, "command": command}),
    )
    .await?;

    let mut accepted = false;
    let mut turn_ended = false;
    let mut error_text = None;
    loop {
        let frame = read_frame(&mut output).await?;
        if frame.get("id").and_then(sonic_rs::JsonValueTrait::as_i64) == Some(4) {
            if frame.get("result").is_none() {
                return Err(io::Error::other(format!("session/submit failed: {frame}")).into());
            }
            accepted = true;
            if turn_ended {
                break;
            }
            continue;
        }
        if frame.get("method").and_then(|value| value.as_str()) != Some("session/update") {
            continue;
        }
        let Some(update) = frame.get("params").and_then(|params| params.get("update")) else {
            continue;
        };
        let update_type = update.get("type").and_then(|value| value.as_str());
        if update_type == Some("tool_settled") {
            error_text = tool_error_text(update);
        }
        if update_type == Some("turn_ended") {
            turn_ended = true;
            if accepted {
                break;
            }
        }
    }
    assert!(accepted && turn_ended, "RPC turn did not settle");
    drop(input);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait()).await??;
    assert!(status.success(), "dalgon RPC exited with {status}");
    let error_text = error_text.unwrap();
    Ok((error_text, sentinel.exists()))
}

#[tokio::test]
async fn sandbox_rejects_rm_outside_allowed_roots() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (tool_error, sentinel_exists) = run_sandbox_probe().await?;
    assert!(sentinel_exists, "sandbox removed the test-owned sentinel");
    assert!(
        !tool_error.is_empty(),
        "the exec call did not return a tool error"
    );
    #[cfg(target_os = "linux")]
    assert!(
        tool_error.contains("Permission denied") || tool_error.contains("Operation not permitted"),
        "{tool_error}"
    );
    #[cfg(target_os = "windows")]
    assert!(
        tool_error.contains("sandbox = \"on\" is not supported on Windows"),
        "{tool_error}"
    );
    #[cfg(target_os = "macos")]
    assert!(
        tool_error.contains("sandbox = \"on\" needs /usr/bin/sandbox-exec")
            || tool_error.contains("Permission denied")
            || tool_error.contains("Operation not permitted"),
        "{tool_error}"
    );
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    assert!(tool_error.contains("sandbox"), "{tool_error}");
    Ok(())
}
