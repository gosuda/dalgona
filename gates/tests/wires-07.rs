//! Every method in supported-methods.json is dispatched by the live app-server.
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

use sonic_rs::JsonValueTrait;
use support::{TestDir, dalgon_binary};

#[derive(serde::Deserialize)]
struct SupportedMethods {
    source_commit: String,
    requests: Vec<String>,
    notifications: Vec<String>,
    server_requests: Vec<String>,
}

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
    let (sender, receiver) = std::sync::mpsc::sync_channel(16);
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

fn reply_has_code(reply: &sonic_rs::Value, code: i64) -> bool {
    reply
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(sonic_rs::Value::as_i64)
        == Some(code)
}

/// Starts the live app-server and completes the initialize handshake,
/// returning stdin, server lines, and the child guard.
fn start_probe_session(
    root: &std::path::Path,
    home: &std::path::Path,
    workspace: &std::path::Path,
    data_home: &std::path::Path,
) -> Result<(std::process::ChildStdin, ServerLines, ChildGuard), Box<dyn Error + Send + Sync>> {
    let binary = dalgon_binary()?;
    let mut child = Command::new(binary)
        .current_dir(workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", data_home)
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "sk-test")
        .args(["app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("app-server stdin was not piped"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("app-server stdout was not piped"))?;
    let guard = ChildGuard(Some(child));
    let lines = server_lines(stdout);
    let initialize = fs::read_to_string(root.join("initialize.request.json"))?;
    let request: sonic_rs::Value = sonic_rs::from_str(&initialize)?;
    let payload = sonic_rs::to_string(&request)?;
    stdin.write_all(payload.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    let reply = read_response(&lines)?;
    assert!(
        !reply_has_code(&reply, -32601),
        "initialize must be dispatched, got {reply:?}"
    );
    stdin.write_all(br#"{"method":"initialized","params":{}}"#)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok((stdin, lines, guard))
}

/// Sends every request named in the fixture inventory and rejects one
/// unknown method. The inventory is the only method list.
fn drive_supported_probes(
    mut stdin: std::process::ChildStdin,
    lines: &ServerLines,
    root: &std::path::Path,
    supported: &SupportedMethods,
    is_notification: &dyn Fn(&str) -> bool,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut next_id: u64 = 100;
    for method in &supported.requests {
        if is_notification(method) {
            stdin.write_all(format!("{{\"method\":\"{method}\",\"params\":{{}}}}").as_bytes())?;
            stdin.write_all(b"\n")?;
            stdin.flush()?;
            match lines.recv_timeout(Duration::from_millis(500)) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Ok(Ok(line)) => assert!(
                    !line.contains("-32601"),
                    "notification {method} drew method-not-found: {line}"
                ),
                Ok(Err(error)) => return Err(error.into()),
                Err(error) => return Err(error.into()),
            }
            continue;
        }
        next_id += 1;
        stdin.write_all(
            format!("{{\"id\":{next_id},\"method\":\"{method}\",\"params\":{{}}}}").as_bytes(),
        )?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        let reply = read_response(lines)?;
        assert!(
            !reply_has_code(&reply, -32601),
            "supported method {method} must be dispatched, got {reply:?}"
        );
    }
    let unknown = fs::read_to_string(root.join("unknown-method.request.json"))?;
    let request: sonic_rs::Value = sonic_rs::from_str(&unknown)?;
    let payload = sonic_rs::to_string(&request)?;
    stdin.write_all(payload.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    drop(stdin);
    let reply = read_response(lines)?;
    assert!(
        reply_has_code(&reply, -32601),
        "unknown method must be rejected with -32601, got {reply:?}"
    );
    Ok(())
}

#[test]
fn codex_schema_fixtures_match_supported_methods() -> Result<(), Box<dyn Error + Send + Sync>> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/codex-app-server");
    let supported: SupportedMethods = sonic_rs::from_str(&std::fs::read_to_string(
        root.join("supported-methods.json"),
    )?)?;
    // The fixture inventory is the single source of method names: this test
    // never repeats the list, so a server that drops a method cannot stay
    // green by editing a copied array.
    assert!(
        !supported.requests.is_empty(),
        "supported-methods.json names no requests"
    );

    let schema: sonic_rs::Value = sonic_rs::from_str(&std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../clients/dal-wire/tests/fixtures/codex-app-server-schema.json"),
    )?)?;
    let commit = schema
        .get("source")
        .and_then(|source| source.get("commit"))
        .and_then(sonic_rs::Value::as_str)
        .unwrap_or("");
    assert!(
        commit.starts_with(supported.source_commit.as_str()),
        "supported-methods pins {}, schema pins {commit}",
        supported.source_commit
    );
    let listed = |table: &str, method: &str| -> bool {
        schema
            .get(table)
            .and_then(|entries| entries.get(method))
            .is_some()
    };
    let client_requests = |method: &str| -> bool { listed("clientRequests", method) };
    let client_notifications = |method: &str| -> bool { listed("clientNotifications", method) };
    for method in &supported.requests {
        assert!(
            client_requests(method) || client_notifications(method),
            "{method} is listed in supported-methods.json but in neither schema table"
        );
    }
    for method in &supported.notifications {
        assert!(
            listed("serverNotifications", method),
            "{method} is listed in supported-methods.json but in no serverNotifications table"
        );
    }
    for method in &supported.server_requests {
        assert!(
            listed("serverRequests", method),
            "{method} is listed in supported-methods.json but in no serverRequests table"
        );
    }

    // Every *.request.json fixture in the directory must be a jsonrpc-free
    // frame naming one method; the directory listing is the enumeration,
    // not a copied list.
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".request.json") {
            continue;
        }
        let text = fs::read_to_string(entry.path())?;
        assert!(!text.contains("jsonrpc"), "{name} must omit jsonrpc");
        let frame: sonic_rs::Value = sonic_rs::from_str(&text)?;
        assert!(
            frame
                .get("method")
                .and_then(sonic_rs::Value::as_str)
                .is_some(),
            "{name} must name one method"
        );
    }

    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    let scripted = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/replay/loop-headless.jsonl");
    fs::write(
        home.join(".config/dal/dal.toml"),
        format!(
            "model = \"openai/gpt-6-luna\"\n[providers.scripted]\nfixture = {:?}\n",
            scripted.to_string_lossy()
        ),
    )?;
    fs::write(workspace.join("test.txt"), "before\n")?;
    let proc_fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/process/grandchild.sh");
    fs::copy(proc_fixture, workspace.join("grandchild.sh"))?;
    let (stdin, lines, mut guard) = start_probe_session(&root, &home, &workspace, &data_home)?;
    drive_supported_probes(stdin, &lines, &root, &supported, &client_notifications)?;
    if let Some(mut child) = guard.0.take() {
        let _ = child.kill();
    }
    Ok(())
}
