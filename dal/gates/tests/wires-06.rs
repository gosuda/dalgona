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

use sonic_rs::{JsonValueMutTrait, JsonValueTrait};
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
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut thread_id = String::new();
    for name in [
        "initialize.request.json",
        "thread-start.request.json",
        "turn-start.request.json",
    ] {
        let payload = fs::read_to_string(fixture_root.join(name))?;
        assert!(!payload.contains("jsonrpc"), "{name} must omit jsonrpc");
        let mut request: sonic_rs::Value = sonic_rs::from_str(&payload)?;
        if name == "thread-start.request.json" || name == "turn-start.request.json" {
            let params = request
                .get_mut("params")
                .and_then(|params| params.as_object_mut())
                .ok_or_else(|| std::io::Error::other("fixture request has no params object"))?;
            if name == "thread-start.request.json" {
                let cwd = workspace.display().to_string();
                params.insert("cwd", sonic_rs::Value::from(&cwd));
            }
            if name == "turn-start.request.json" {
                params.insert("threadId", thread_id.as_str());
            }
        }
        send_request(stdin, &request)?;
        if name == "turn-start.request.json" {
            break;
        }
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
    Ok(())
}

fn pump_until_turn_completed(
    stdin: &mut std::process::ChildStdin,
    lines: &ServerLines,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut saw_turn_completed = false;
    let mut answered_once = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let line = match lines.recv_timeout(timeout) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => break,
        };
        if line.is_empty() {
            continue;
        }
        assert!(
            !line.contains("\"jsonrpc\""),
            "every Codex frame omits jsonrpc"
        );
        let value: sonic_rs::Value = sonic_rs::from_str(&line)?;
        let method = value
            .get("method")
            .and_then(sonic_rs::Value::as_str)
            .unwrap_or("")
            .to_owned();
        if method == "turn/completed" {
            saw_turn_completed = true;
            break;
        }
        if method.contains("requestApproval") || method.contains("requestUserInput") {
            let id = value
                .get("id")
                .and_then(sonic_rs::Value::as_u64)
                .unwrap_or(0);
            let answer = format!("{{\"id\":{id},\"result\":{{\"decision\":\"accept\"}}}}\n");
            stdin.write_all(answer.as_bytes())?;
            stdin.flush()?;
            answered_once = true;
        }
    }
    assert!(answered_once, "must answer one approval/user-input request");
    assert!(saw_turn_completed, "must observe turn completed");
    Ok(())
}

#[test]
fn codex_app_server_smoke_uses_pinned_core_subset() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    let replay = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/loop-headless.jsonl");
    fs::write(
        home.join(".config/dal/dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
            replay.to_string_lossy()
        ),
    )?;
    fs::write(workspace.join("test.txt"), "before\n")?;
    let proc_fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/process/grandchild.sh");
    fs::copy(proc_fixture, workspace.join("grandchild.sh"))?;
    let binary = dalgon_binary("dalgon")?;
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/codex-app-server");

    let mut child = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("NO_COLOR", "1")
        .args(["app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut guard = ChildGuard(Some(child));
    let lines = server_lines(stdout);

    drive_handshake(&mut stdin, &lines, &fixture_root, &workspace)?;

    pump_until_turn_completed(&mut stdin, &lines)?;

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
