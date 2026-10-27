//! Stdio and socket wire requests: framing, dispatch, and drop semantics.
#![expect(clippy::expect_used, reason = "SC test")]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test exercises real wire binaries"
)]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    fs,
    io::Write,
    process::{Command, Stdio},
};

#[cfg(unix)]
use std::{os::unix::fs::PermissionsExt, process::Child};
#[cfg(unix)]
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use sonic_rs::{JsonContainerTrait, JsonValueTrait};
use support::{
    SCOPE_SAVE_SEED, TestDir, assert_scope_save_landed, dalgon_binary, initialize_params,
    scope_save_params,
};

fn stdio_request(
    binary: &std::path::Path,
    args: &[&str],
    payload: &str,
    home: &std::path::Path,
    workspace: &std::path::Path,
    data_home: &std::path::Path,
) -> Result<std::process::Output, Box<dyn Error + Send + Sync>> {
    let mut child = Command::new(binary)
        .current_dir(workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", data_home)
        .env("NO_COLOR", "1")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    stdin.write_all(payload.as_bytes())?;
    drop(stdin);
    Ok(child.wait_with_output()?)
}

#[cfg(unix)]
struct ChildGuard(Child);

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[cfg(unix)]
fn spawn_rpc(
    binary: &std::path::Path,
    home: &std::path::Path,
    workspace: &std::path::Path,
    data_home: &std::path::Path,
    socket: Option<&std::path::Path>,
) -> Result<ChildGuard, Box<dyn Error + Send + Sync>> {
    let mut command = Command::new(binary);
    command
        .current_dir(workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", data_home)
        .args(["rpc", "--socket"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(socket) = socket {
        command.arg(socket);
    }
    Ok(ChildGuard(command.spawn()?))
}

#[cfg(unix)]
async fn rpc_initialize(
    socket: &std::path::Path,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut stream = loop {
        match UnixStream::connect(socket).await {
            Ok(stream) => break stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    stream
        .write_all(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .await?;
    stream.write_all(b"\n").await?;
    let mut line = String::new();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        BufReader::new(stream).read_line(&mut line),
    )
    .await??;
    if read == 0 {
        return Err("local RPC closed before replying to initialize".into());
    }
    Ok(sonic_rs::from_str(&line)?)
}

#[cfg(unix)]
#[tokio::test]
async fn rpc_bare_socket_flag_serves_the_default_local_endpoint()
-> Result<(), Box<dyn Error + Send + Sync>> {
    // The default socket lives under the data root; the deep BSD temp root
    // would push the endpoint past SUN_LEN.
    let dir = TestDir::new_in(std::path::Path::new("/tmp"))?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    fs::write(
        home.join(".config/dal/dal.toml"),
        "model = \"openai-responses/gpt-6\"\n",
    )?;
    let binary = dalgon_binary()?;
    let socket = data_home.join("dal/rpc/dal.sock");
    let mut guard = spawn_rpc(binary, &home, &workspace, &data_home, None)?;
    let response = rpc_initialize(&socket).await?;
    let socket_dir = socket
        .parent()
        .ok_or_else(|| std::io::Error::other("socket has no parent"))?;
    assert_eq!(
        fs::metadata(socket_dir)?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        response.get("id").and_then(sonic_rs::Value::as_u64),
        Some(1)
    );
    assert_eq!(
        response
            .get("result")
            .and_then(|result| result.get("protocolVersion"))
            .and_then(sonic_rs::Value::as_u64),
        Some(1)
    );
    assert!(
        guard.0.try_wait()?.is_none(),
        "local RPC server remains available"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn rpc_explicit_relative_socket_path_is_served() -> Result<(), Box<dyn Error + Send + Sync>> {
    // The resolved socket path must stay under SUN_LEN; the deep BSD temp
    // root pushes workspace/private/rpc.sock past it.
    let dir = TestDir::new_in(std::path::Path::new("/tmp"))?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    fs::write(
        home.join(".config/dal/dal.toml"),
        "model = \"openai-responses/gpt-6\"\n",
    )?;
    let private_dir = workspace.join("private");
    fs::create_dir_all(&private_dir)?;
    fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700))?;
    let socket = private_dir.join("rpc.sock");
    let binary = dalgon_binary()?;
    let mut server = spawn_rpc(
        binary,
        &home,
        &workspace,
        &data_home,
        Some(std::path::Path::new("private/rpc.sock")),
    )?;

    let response = rpc_initialize(&socket).await?;
    assert_eq!(
        response.get("id").and_then(sonic_rs::Value::as_u64),
        Some(1)
    );
    assert_eq!(
        response
            .get("result")
            .and_then(|result| result.get("protocolVersion"))
            .and_then(sonic_rs::Value::as_u64),
        Some(1)
    );
    assert!(
        server.0.try_wait()?.is_none(),
        "local RPC server remains available"
    );
    Ok(())
}

#[test]
fn wire_clients_answer_each_supported_surface() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    fs::write(
        home.join(".config/dal/dal.toml"),
        "model = \"openai-responses/gpt-6\"\n",
    )?;
    let binary = dalgon_binary()?;

    let rpc = stdio_request(
        binary,
        &["rpc"],
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        &home,
        &workspace,
        &data_home,
    )?;
    assert!(
        rpc.status.success(),
        "RPC stderr: {}",
        String::from_utf8_lossy(&rpc.stderr)
    );
    assert!(
        !rpc.stdout.is_empty(),
        "RPC returned no frames; stderr: {}",
        String::from_utf8_lossy(&rpc.stderr)
    );
    let rpc: sonic_rs::Value = sonic_rs::from_slice(&rpc.stdout)?;
    assert_eq!(rpc.get("id").and_then(sonic_rs::Value::as_u64), Some(1));
    assert_eq!(
        rpc.get("result")
            .and_then(|result| result.get("protocolVersion"))
            .and_then(sonic_rs::Value::as_u64),
        Some(1)
    );

    let acp = stdio_request(
        binary,
        &["acp"],
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        &home,
        &workspace,
        &data_home,
    )?;
    assert!(
        acp.status.success(),
        "ACP stderr: {}",
        String::from_utf8_lossy(&acp.stderr)
    );
    let acp: sonic_rs::Value = sonic_rs::from_slice(&acp.stdout)?;
    assert_eq!(acp.get("id").and_then(sonic_rs::Value::as_u64), Some(1));
    assert!(
        acp.get("result").is_some(),
        "ACP initialize returns a result"
    );

    let schema_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/codex-app-server");
    let init = fs::read_to_string(schema_root.join("initialize.request.json"))?;
    let codex = stdio_request(
        binary,
        &["app-server"],
        &format!("{init}\n"),
        &home,
        &workspace,
        &data_home,
    )?;
    assert!(
        codex.status.success(),
        "app-server stderr: {}",
        String::from_utf8_lossy(&codex.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&codex.stdout).contains("jsonrpc"),
        "Codex frames omit jsonrpc"
    );
    let codex: sonic_rs::Value = sonic_rs::from_slice(&codex.stdout)?;
    assert_eq!(codex.get("id").and_then(sonic_rs::Value::as_u64), Some(1));
    assert!(
        codex.get("result").is_some(),
        "Codex initialize returns a result"
    );
    Ok(())
}

/// A persistent Codex app-server child with reader ownership for every exit path.
struct CodexChild {
    child: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    lines: std::sync::mpsc::Receiver<String>,
    pending: std::collections::VecDeque<sonic_rs::Value>,
    stdout_reader: Option<std::thread::JoinHandle<()>>,
    stderr_reader: Option<std::thread::JoinHandle<String>>,
    seen: Vec<String>,
    status_ok: Option<bool>,
    stderr: Option<String>,
}

impl CodexChild {
    fn spawn(
        binary: &std::path::Path,
        home: &std::path::Path,
        workspace: &std::path::Path,
        data_home: &std::path::Path,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        use std::io::{BufRead, BufReader, Read};

        let mut child = Command::new(binary)
            .current_dir(workspace)
            .env_clear()
            .envs(support::captured_shell_vars())
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", data_home)
            .env("NO_COLOR", "1")
            .env("OPENAI_API_KEY", "sk-gate")
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().expect("app-server stdin");
        let stdout = child.stdout.take().expect("app-server stdout");
        let stderr = child.stderr.take().expect("app-server stderr");
        let (sender, lines) = std::sync::mpsc::sync_channel::<String>(256);
        let stdout_reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut text);
            text
        });
        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            lines,
            pending: std::collections::VecDeque::with_capacity(256),
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            seen: Vec::new(),
            status_ok: None,
            stderr: None,
        })
    }

    fn send(&mut self, frame: &sonic_rs::Value) -> Result<(), Box<dyn Error + Send + Sync>> {
        if frame.get("jsonrpc").is_some() || frame.get("router").is_some() {
            return Err("Codex client frame carries a router member".into());
        }
        let text = sonic_rs::to_string(frame)?;
        let stdin = self.stdin.as_mut().ok_or("app-server stdin is closed")?;
        writeln!(stdin, "{text}")?;
        stdin.flush()?;
        Ok(())
    }

    fn read_frame(
        &mut self,
        schema: &sonic_rs::Value,
    ) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
        let line = self
            .lines
            .recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|error| format!("no Codex frame within timeout: {error}"))?;
        self.seen.push(line.clone());
        let frame: sonic_rs::Value = sonic_rs::from_str(&line)
            .map_err(|error| format!("Codex stdout carried non-JSON {line:?}: {error}"))?;
        if frame.get("jsonrpc").is_some() || frame.get("router").is_some() {
            return Err(format!("Codex frame carries a router member: {frame}").into());
        }
        if let Some(method) = frame.get("method").and_then(sonic_rs::Value::as_str) {
            let table = if frame.get("id").is_some() {
                "serverRequests"
            } else {
                "serverNotifications"
            };
            if frame.get("params").is_none() {
                return Err(format!("Codex {table}.{method} frame has no params").into());
            }
            assert_fixture_shape(
                schema,
                table,
                method,
                "params",
                frame.get("params").expect("checked params"),
            )?;
        }
        Ok(frame)
    }

    fn next(
        &mut self,
        schema: &sonic_rs::Value,
    ) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
        self.pending
            .pop_front()
            .map_or_else(|| self.read_frame(schema), Ok)
    }

    fn call(
        &mut self,
        schema: &sonic_rs::Value,
        frame: &sonic_rs::Value,
        id: u64,
    ) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
        self.send(frame)?;
        loop {
            let value = self.read_frame(schema)?;
            if value.get("id").and_then(sonic_rs::Value::as_u64) == Some(id)
                && value.get("method").is_none()
            {
                return Ok(value);
            }
            if self.pending.len() >= 256 {
                return Err("Codex pending-frame bound exceeded".into());
            }
            self.pending.push_back(value);
        }
    }

    fn finish(mut self) -> (bool, String, Vec<String>) {
        self.cleanup();
        (
            self.status_ok.unwrap_or(false),
            self.stderr.take().unwrap_or_default(),
            std::mem::take(&mut self.seen),
        )
    }

    fn cleanup(&mut self) {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut status = None;
            while std::time::Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(done)) => {
                        status = Some(done.success());
                        break;
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    Err(_) => break,
                }
            }
            if status.is_none() {
                let _ = child.kill();
                let kill_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while std::time::Instant::now() < kill_deadline {
                    match child.try_wait() {
                        Ok(Some(done)) => {
                            status = Some(done.success());
                            break;
                        }
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                        Err(_) => break,
                    }
                }
                if status.is_none() {
                    status = Some(false);
                }
            }
            self.status_ok = status;
        }
        if let Some(reader) = self.stdout_reader.take() {
            let reader_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !reader.is_finished() && std::time::Instant::now() < reader_deadline {
                match self
                    .lines
                    .recv_timeout(std::time::Duration::from_millis(50))
                {
                    Ok(line) => self.seen.push(line),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
        if let Some(reader) = self.stderr_reader.take() {
            let reader_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !reader.is_finished() && std::time::Instant::now() < reader_deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if reader.is_finished() {
                self.stderr = reader.join().ok();
            }
        }
        self.seen.extend(self.lines.try_iter());
    }
}

impl Drop for CodexChild {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn fixture_route<'a>(
    schema: &'a sonic_rs::Value,
    table: &str,
    method: &str,
    part: &str,
) -> Result<&'a sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    schema
        .get(table)
        .and_then(|table| table.get(method))
        .and_then(|entry| entry.get(part))
        .ok_or_else(|| format!("fixture has no {table}.{method}.{part}").into())
}

fn fixture_reference<'a>(
    schema: &'a sonic_rs::Value,
    reference: &str,
) -> Result<&'a sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    let mut node = schema;
    for segment in reference
        .strip_prefix("#/")
        .ok_or("Codex fixture uses a non-local schema reference")?
        .split('/')
    {
        let segment = if segment.contains('~') {
            std::borrow::Cow::Owned(segment.replace("~1", "/").replace("~0", "~"))
        } else {
            std::borrow::Cow::Borrowed(segment)
        };
        node = node
            .get(segment.as_ref())
            .ok_or_else(|| format!("fixture reference is unresolved: {reference}"))?;
    }
    Ok(node)
}

fn fixture_type(kind: &str, value: &sonic_rs::Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_str(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value
                    .as_f64()
                    .is_some_and(|number| number.fract().classify() == std::num::FpCategory::Zero)
        }
        _ => false,
    }
}

fn check_fixture_ref<'a>(
    schema: &'a sonic_rs::Value,
    node: &'a sonic_rs::Value,
    reference: &'a str,
    checked: &mut std::collections::HashSet<&'a str>,
    active: &mut std::collections::HashSet<&'a str>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let object = node
        .as_object()
        .ok_or("fixture schema is not an object or boolean")?;
    // Draft-07 ignores $ref siblings. Reject assertions there rather
    // than accidentally validating them under a newer draft.
    if object.iter().any(|(key, _)| {
        !matches!(
            key,
            "$ref" | "title" | "description" | "default" | "$schema"
        )
    }) {
        return Err("fixture $ref has unsupported assertion siblings".into());
    }
    if checked.contains(reference) {
        return Ok(());
    }
    if !active.insert(reference) {
        return Err(format!("recursive fixture reference: {reference}").into());
    }
    check_fixture_node(
        schema,
        fixture_reference(schema, reference)?,
        checked,
        active,
    )?;
    active.remove(reference);
    checked.insert(reference);
    Ok(())
}

fn check_fixture_type(child: &sonic_rs::Value) -> Result<(), Box<dyn Error + Send + Sync>> {
    let known = |kind: &sonic_rs::Value| {
        matches!(
            kind.as_str(),
            Some("object" | "array" | "string" | "boolean" | "null" | "number" | "integer")
        )
    };
    if !(known(child)
        || child
            .as_array()
            .is_some_and(|types| !types.is_empty() && types.iter().all(known)))
    {
        return Err("fixture type is unsupported".into());
    }
    Ok(())
}

/// Fail closed on schema changes, including constraints in unselected union
/// branches and absent optional properties. This is the pinned draft-07 subset,
/// not a general JSON Schema implementation.
fn check_fixture_node<'a>(
    schema: &'a sonic_rs::Value,
    node: &'a sonic_rs::Value,
    checked: &mut std::collections::HashSet<&'a str>,
    active: &mut std::collections::HashSet<&'a str>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if node.as_bool().is_some() {
        return Ok(());
    }
    let object = node
        .as_object()
        .ok_or("fixture schema is not an object or boolean")?;
    for (keyword, child) in object {
        match keyword {
            "$ref" => {
                let reference = child.as_str().ok_or("fixture $ref is not a string")?;
                check_fixture_ref(schema, node, reference, checked, active)?;
            }
            "type" => check_fixture_type(child)?,
            "properties" => {
                for (_, property) in child
                    .as_object()
                    .ok_or("fixture properties is not an object")?
                {
                    check_fixture_node(schema, property, checked, active)?;
                }
            }
            "items" | "additionalProperties" => {
                check_fixture_node(schema, child, checked, active)?;
            }
            "required" => {
                if !child
                    .as_array()
                    .is_some_and(|names| names.iter().all(sonic_rs::JsonValueTrait::is_str))
                {
                    return Err("fixture required is not an array of names".into());
                }
            }
            "enum" => {
                if child.as_array().is_none_or(sonic_rs::Array::is_empty) {
                    return Err("fixture enum is not a nonempty array".into());
                }
            }
            "oneOf" | "anyOf" | "allOf" => {
                let branches = child.as_array().ok_or("fixture union is not an array")?;
                if branches.is_empty() {
                    return Err("fixture union is empty".into());
                }
                for branch in branches {
                    check_fixture_node(schema, branch, checked, active)?;
                }
            }
            "minimum" => {
                // Every exercised bound is zero. Other bounds need exact
                // mixed integer/float comparison before they can be admitted.
                if !child
                    .as_f64()
                    .is_some_and(|bound| bound.classify() == std::num::FpCategory::Zero)
                {
                    return Err("fixture minimum outside the supported zero bound".into());
                }
            }
            "minLength" => {
                if child.as_u64().is_none() {
                    return Err("fixture minLength is not an unsigned integer".into());
                }
            }
            "$schema" => {
                if child.as_str() != Some("http://json-schema.org/draft-07/schema#") {
                    return Err("fixture schema draft is unsupported".into());
                }
            }
            "format" => {
                // These schemars integer labels are annotations, not draft-07
                // standard formats. The type and explicit minimum still apply.
                if !matches!(
                    child.as_str(),
                    Some("int64" | "int32" | "uint16" | "uint" | "uint32" | "uint64")
                ) {
                    return Err("fixture format is unsupported".into());
                }
            }
            "const" | "title" | "description" | "default" => {}
            _ => return Err(format!("unsupported fixture keyword: {keyword}").into()),
        }
    }
    Ok(())
}

fn fixture_matches(
    schema: &sonic_rs::Value,
    node: &sonic_rs::Value,
    value: &sonic_rs::Value,
) -> bool {
    if let Some(allowed) = node.as_bool() {
        return allowed;
    }
    if let Some(reference) = node.get("$ref").and_then(sonic_rs::Value::as_str) {
        return fixture_matches(
            schema,
            fixture_reference(schema, reference).expect("preflight resolved reference"),
            value,
        );
    }
    if let Some(kind) = node.get("type") {
        let matches = kind.as_str().map_or_else(
            || {
                kind.as_array()
                    .expect("preflight checked type")
                    .iter()
                    .any(|kind| fixture_type(kind.as_str().expect("preflight checked type"), value))
            },
            |kind| fixture_type(kind, value),
        );
        if !matches {
            return false;
        }
    }
    if node.get("const").is_some_and(|constant| constant != value)
        || node.get("enum").is_some_and(|choices| {
            !choices
                .as_array()
                .expect("preflight checked enum")
                .iter()
                .any(|choice| choice == value)
        })
    {
        return false;
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = node.get(keyword).and_then(sonic_rs::Value::as_array) {
            let count = branches
                .iter()
                .filter(|branch| fixture_matches(schema, branch, value))
                .count();
            if match keyword {
                "allOf" => count != branches.len(),
                "anyOf" => count == 0,
                "oneOf" => count != 1,
                _ => unreachable!(),
            } {
                return false;
            }
        }
    }
    if let Some(object) = value.as_object() {
        if node
            .get("required")
            .and_then(sonic_rs::Value::as_array)
            .is_some_and(|required| {
                required.iter().any(|name| {
                    value
                        .get(name.as_str().expect("preflight checked required"))
                        .is_none()
                })
            })
        {
            return false;
        }
        for (name, member) in object {
            let property = node
                .get("properties")
                .and_then(|properties| properties.get(name))
                .or_else(|| node.get("additionalProperties"));
            if property.is_some_and(|property| !fixture_matches(schema, property, member)) {
                return false;
            }
        }
    }
    if let Some(array) = value.as_array()
        && let Some(items) = node.get("items")
        && !array
            .iter()
            .all(|item| fixture_matches(schema, items, item))
    {
        return false;
    }
    if let Some(text) = value.as_str()
        && let Some(minimum) = node.get("minLength").and_then(sonic_rs::Value::as_u64)
        && u64::try_from(text.chars().count()).is_ok_and(|length| length < minimum)
    {
        return false;
    }
    if node.get("minimum").is_some() && value.as_f64().is_some_and(|number| number < 0.0) {
        return false;
    }
    true
}

fn assert_fixture_shape(
    schema: &sonic_rs::Value,
    table: &str,
    method: &str,
    part: &str,
    value: &sonic_rs::Value,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let node = fixture_route(schema, table, method, part)?;
    check_fixture_node(
        schema,
        node,
        &mut std::collections::HashSet::new(),
        &mut std::collections::HashSet::new(),
    )?;
    if !fixture_matches(schema, node, value) {
        return Err(format!("{table}.{method}.{part} violates its pinned schema: {value}").into());
    }
    Ok(())
}

fn codex_schema_fixture() -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../clients/dal-wire/tests/fixtures/codex-app-server-schema.json");
    Ok(sonic_rs::from_str(&fs::read_to_string(path)?)?)
}

#[test]
fn codex_pinned_schema_rejects_nested_mutations() -> Result<(), Box<dyn Error + Send + Sync>> {
    use sonic_rs::JsonValueMutTrait;

    let schema = codex_schema_fixture()?;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/codex-app-server");
    let frame = fixture_request(&root, "initialize.request.json", 1)?;
    let params = frame
        .get("params")
        .ok_or("initialize fixture has no params")?;
    assert_fixture_shape(&schema, "clientRequests", "initialize", "params", params)?;
    for name in ["name", "version"] {
        let mut wrong_type = params.clone();
        let info = wrong_type
            .get_mut("clientInfo")
            .ok_or("fixture has no clientInfo")?;
        *info
            .get_mut(name)
            .ok_or("fixture clientInfo has no required member")? = sonic_rs::json!(7);
        assert!(
            assert_fixture_shape(
                &schema,
                "clientRequests",
                "initialize",
                "params",
                &wrong_type
            )
            .is_err(),
            "nested {name} accepted a number"
        );

        let mut missing = params.clone();
        let removed = missing
            .get_mut("clientInfo")
            .and_then(sonic_rs::Value::as_object_mut)
            .ok_or("fixture clientInfo is not an object")?
            .remove(&name);
        assert!(
            removed.is_some(),
            "fixture clientInfo has no required {name}"
        );
        assert!(
            assert_fixture_shape(&schema, "clientRequests", "initialize", "params", &missing)
                .is_err(),
            "nested required {name} was not enforced"
        );
    }
    let answer = sonic_rs::json!({"answers": {"question": {"answers": ["gate answer"]}}});
    assert_fixture_shape(
        &schema,
        "serverRequests",
        "item/tool/requestUserInput",
        "result",
        &answer,
    )?;
    for invalid in [
        sonic_rs::json!({"answers": {"question": {"answers": [7]}}}),
        sonic_rs::json!({"answers": {"question": {}}}),
        sonic_rs::json!({"answers": {"question": null}}),
    ] {
        assert!(
            assert_fixture_shape(
                &schema,
                "serverRequests",
                "item/tool/requestUserInput",
                "result",
                &invalid
            )
            .is_err(),
            "additionalProperties answer schema accepted {invalid}"
        );
    }
    Ok(())
}

#[test]
fn codex_pinned_schema_checks_union_alternatives() -> Result<(), Box<dyn Error + Send + Sync>> {
    let schema = codex_schema_fixture()?;
    for input in [
        sonic_rs::json!({"type": "text", "text": "gate text"}),
        sonic_rs::json!({"type": "image", "url": "https://example.invalid/image", "detail": null}),
        sonic_rs::json!({"type": "image", "fileId": "file-gate", "detail": "auto"}),
        // anyOf admits both locators, unlike oneOf.
        sonic_rs::json!({"type": "image", "url": "https://example.invalid/image", "fileId": "file-gate"}),
        sonic_rs::json!({"type": "localImage", "path": "/gate/image"}),
        sonic_rs::json!({"type": "audio", "url": "https://example.invalid/audio"}),
        sonic_rs::json!({"type": "localAudio", "path": "/gate/audio"}),
        sonic_rs::json!({"type": "skill", "name": "gate", "path": "/gate/skill"}),
        sonic_rs::json!({"type": "mention", "name": "gate", "path": "/gate/file"}),
    ] {
        let params = sonic_rs::json!({"threadId": "gate-thread", "input": [input]});
        assert_fixture_shape(&schema, "clientRequests", "turn/start", "params", &params)?;
    }
    for input in [
        sonic_rs::json!({"type": "text", "text": 7}),
        sonic_rs::json!({"type": "text"}),
        sonic_rs::json!({"type": "image"}),
        sonic_rs::json!({"type": "image", "url": 7}),
        sonic_rs::json!({"type": "image", "fileId": "file-gate", "detail": "not-a-detail"}),
        sonic_rs::json!({"type": "not-an-input", "text": "gate"}),
        sonic_rs::json!(null),
    ] {
        let params = sonic_rs::json!({"threadId": "gate-thread", "input": [input]});
        assert!(
            assert_fixture_shape(&schema, "clientRequests", "turn/start", "params", &params)
                .is_err(),
            "input union accepted {params}"
        );
    }
    for decision in [
        sonic_rs::json!("accept"),
        sonic_rs::json!("acceptForSession"),
        sonic_rs::json!("decline"),
        sonic_rs::json!("cancel"),
        sonic_rs::json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["gate"]}}),
        sonic_rs::json!({"applyNetworkPolicyAmendment": {
            "network_policy_amendment": {"host": "example.invalid", "action": "allow"}
        }}),
    ] {
        let result = sonic_rs::json!({"decision": decision});
        assert_fixture_shape(
            &schema,
            "serverRequests",
            "item/commandExecution/requestApproval",
            "result",
            &result,
        )?;
    }
    for decision in [
        sonic_rs::json!("not-a-decision"),
        sonic_rs::json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": [7]}}),
        sonic_rs::json!({"acceptWithExecpolicyAmendment": {}}),
        sonic_rs::json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": []}, "extra": true}),
    ] {
        let result = sonic_rs::json!({"decision": decision});
        assert!(
            assert_fixture_shape(
                &schema,
                "serverRequests",
                "item/commandExecution/requestApproval",
                "result",
                &result
            )
            .is_err(),
            "approval union accepted {result}"
        );
    }
    Ok(())
}

fn fixture_request(
    root: &std::path::Path,
    name: &str,
    id: u64,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    use sonic_rs::JsonValueMutTrait;

    let mut frame: sonic_rs::Value = sonic_rs::from_str(&fs::read_to_string(root.join(name))?)?;
    frame
        .as_object_mut()
        .ok_or("Codex request fixture is not an object")?
        .insert("id", sonic_rs::Value::from(id));
    Ok(frame)
}

fn client_request(
    schema: &sonic_rs::Value,
    method: &str,
    id: u64,
    params: &sonic_rs::Value,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    assert_fixture_shape(schema, "clientRequests", method, "params", params)?;
    Ok(sonic_rs::json!({"id": id, "method": method, "params": params}))
}

fn assert_result(
    schema: &sonic_rs::Value,
    method: &str,
    frame: &sonic_rs::Value,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if let Some(result) = frame.get("result") {
        assert_fixture_shape(schema, "clientRequests", method, "result", result)?;
    } else {
        let error = frame
            .get("error")
            .ok_or("Codex reply has no result or error")?;
        let code = error
            .get("code")
            .and_then(sonic_rs::Value::as_i64)
            .ok_or("Codex error reply has no numeric code")?;
        let message = error
            .get("message")
            .and_then(sonic_rs::Value::as_str)
            .ok_or("Codex error reply has no message")?;
        assert!(!message.is_empty(), "Codex error message is empty");
        assert!(code <= 0, "Codex error code is not negative: {code}");
    }
    Ok(())
}

fn server_response(
    schema: &sonic_rs::Value,
    request: &sonic_rs::Value,
    result: &sonic_rs::Value,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    let method = request
        .get("method")
        .and_then(sonic_rs::Value::as_str)
        .ok_or("Codex server request has no method")?;
    assert_fixture_shape(schema, "serverRequests", method, "result", result)?;
    let id = request.get("id").ok_or("Codex server request has no id")?;
    Ok(sonic_rs::json!({"id": id, "result": result}))
}

fn scripted_step(
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

/// Constant-size evidence for one correlated turn, shared across request waits
/// and completion draining. Consumed frames are never queued for replay.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TurnMilestone {
    Started,
    ItemStarted,
    ItemCompleted,
    MessageDelta,
}

struct CodexTurnEvidence<'a> {
    thread: &'a str,
    turn: &'a str,
    frames: usize,
    seen: std::collections::BTreeSet<TurnMilestone>,
}

impl<'a> CodexTurnEvidence<'a> {
    fn new(thread: &'a str, turn: &'a str) -> Self {
        Self {
            thread,
            turn,
            frames: 0,
            seen: std::collections::BTreeSet::new(),
        }
    }

    fn has(&self, milestone: TurnMilestone) -> bool {
        self.seen.contains(&milestone)
    }

    fn mark(&mut self, milestone: TurnMilestone) {
        self.seen.insert(milestone);
    }

    fn observe(
        &mut self,
        schema: &sonic_rs::Value,
        frame: &sonic_rs::Value,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        use sonic_rs::JsonValueMutTrait;

        if self.frames >= 256 {
            return Err("Codex per-turn frame bound exceeded".into());
        }
        self.frames += 1;
        let method = frame
            .get("method")
            .and_then(sonic_rs::Value::as_str)
            .ok_or("Codex turn emitted a reply instead of an event")?;
        let params = frame.get("params").ok_or("Codex event has no params")?;
        let turn = if matches!(method, "turn/started" | "turn/completed") {
            params.get("turn").and_then(|turn| turn.get("id"))
        } else {
            params.get("turnId")
        };
        if params.get("threadId").and_then(sonic_rs::Value::as_str) != Some(self.thread)
            || turn.and_then(sonic_rs::Value::as_str) != Some(self.turn)
        {
            return Err("Codex event belongs to a different thread or turn".into());
        }
        if method == "item/started" && !self.has(TurnMilestone::ItemStarted) {
            // Mutate an admitted real frame, not an independently authored
            // approximation of the ThreadItem union.
            let mut invalid = params.clone();
            *invalid
                .get_mut("item")
                .and_then(|item| item.get_mut("type"))
                .ok_or("item/started has no nested item type")? =
                sonic_rs::json!("not-a-pinned-item");
            assert!(
                assert_fixture_shape(schema, "serverNotifications", method, "params", &invalid)
                    .is_err(),
                "captured item/started accepted an invalid union tag"
            );
        }
        match method {
            "turn/started" => self.mark(TurnMilestone::Started),
            "item/started" => self.mark(TurnMilestone::ItemStarted),
            "item/completed" => self.mark(TurnMilestone::ItemCompleted),
            "item/agentMessage/delta" => self.mark(TurnMilestone::MessageDelta),
            _ => {}
        }
        Ok(())
    }
}

fn await_server_request<'a>(
    child: &mut CodexChild,
    schema: &sonic_rs::Value,
    method: &str,
    thread: &'a str,
    turn: &'a str,
) -> Result<(sonic_rs::Value, CodexTurnEvidence<'a>), Box<dyn Error + Send + Sync>> {
    let mut evidence = CodexTurnEvidence::new(thread, turn);
    loop {
        let frame = child.next(schema)?;
        evidence.observe(schema, &frame)?;
        let frame_method = frame
            .get("method")
            .and_then(sonic_rs::Value::as_str)
            .ok_or("Codex turn emitted a reply instead of an event")?;
        if frame_method == method {
            if !evidence.has(TurnMilestone::Started) {
                return Err("Codex turn emitted a request before turn/started".into());
            }
            return Ok((frame, evidence));
        }
        if !matches!(
            frame_method,
            "turn/started" | "item/started" | "item/completed" | "item/agentMessage/delta"
        ) {
            return Err(format!("unexpected Codex turn method {frame_method}").into());
        }
    }
}

fn drain_turn(
    child: &mut CodexChild,
    schema: &sonic_rs::Value,
    mut evidence: CodexTurnEvidence<'_>,
    status: &str,
) -> Result<(bool, bool, bool), Box<dyn Error + Send + Sync>> {
    loop {
        let frame = child.next(schema)?;
        evidence.observe(schema, &frame)?;
        let method = frame
            .get("method")
            .and_then(sonic_rs::Value::as_str)
            .ok_or("Codex turn emitted a reply instead of a notification")?;
        if method == "turn/completed" {
            let params = frame.get("params").ok_or("turn/completed has no params")?;
            let turn_value = params.get("turn").ok_or("turn/completed has no turn")?;
            assert_eq!(
                turn_value.get("status").and_then(sonic_rs::Value::as_str),
                Some(status)
            );
            if !evidence.has(TurnMilestone::ItemStarted) {
                return Err("Codex turn emitted no item/started notification".into());
            }
            return Ok((
                evidence.has(TurnMilestone::ItemStarted),
                evidence.has(TurnMilestone::ItemCompleted),
                evidence.has(TurnMilestone::MessageDelta),
            ));
        }
        if !matches!(
            method,
            "item/started" | "item/completed" | "item/agentMessage/delta"
        ) {
            return Err(format!("unexpected Codex completion method {method}").into());
        }
    }
}

struct SmokeContext {
    workspace: std::path::PathBuf,
    request_root: std::path::PathBuf,
    schema: sonic_rs::Value,
}

fn smoke_setup() -> Result<(TestDir, CodexChild, SmokeContext), Box<dyn Error + Send + Sync>> {
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
    let replay = home.join("wire-gate.jsonl");
    let mut replay_text = String::new();
    replay_text.push_str(&scripted_step(
        "wire-gate__run",
        &sonic_rs::json!({"command": "printf gate"}),
    )?);
    replay_text.push_str(&scripted_step(
        "wire-gate__ask",
        &sonic_rs::json!({"prompt": "enter gate text"}),
    )?);
    replay_text.push_str(&scripted_step(
        "wire-gate__run",
        &sonic_rs::json!({"command": "printf interrupt"}),
    )?);
    fs::write(&replay, replay_text)?;
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "approval = \"ask\"\nmodel = \"openai/gpt-6\"\nplugins = [\"wire-gate\"]\n[providers.scripted]\nfixture = {:?}\n",
            replay.to_string_lossy()
        ),
    )?;
    let schema = codex_schema_fixture()?;
    let request_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/codex-app-server");
    let child = CodexChild::spawn(dalgon_binary()?, &home, &workspace, &data_home)?;
    let context = SmokeContext {
        workspace,
        request_root,
        schema,
    };
    Ok((dir, child, context))
}

fn smoke_initialize(
    child: &mut CodexChild,
    context: &SmokeContext,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let schema = &context.schema;
    let initialize = fixture_request(&context.request_root, "initialize.request.json", 1)?;
    assert_eq!(
        initialize.get("method").and_then(sonic_rs::Value::as_str),
        Some("initialize")
    );
    assert_fixture_shape(
        schema,
        "clientRequests",
        "initialize",
        "params",
        initialize
            .get("params")
            .ok_or("initialize fixture has no params")?,
    )?;
    let handshake = child.call(schema, &initialize, 1)?;
    assert_result(schema, "initialize", &handshake)?;
    assert!(handshake.get("result").is_some(), "initialize must succeed");
    let initialized = sonic_rs::json!({"method": "initialized"});
    assert!(
        schema
            .get("clientNotifications")
            .and_then(|table| table.get("initialized"))
            .is_some()
    );
    child.send(&initialized)?;
    Ok(())
}

fn smoke_start_thread(
    child: &mut CodexChild,
    context: &SmokeContext,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    use sonic_rs::JsonValueMutTrait;

    let schema = &context.schema;
    let mut start = fixture_request(&context.request_root, "thread-start.request.json", 2)?;
    let cwd = context.workspace.display().to_string();
    start
        .get_mut("params")
        .and_then(|params| params.as_object_mut())
        .ok_or("thread fixture params are not an object")?
        .insert("cwd", sonic_rs::Value::from(&cwd));
    assert_fixture_shape(
        schema,
        "clientRequests",
        "thread/start",
        "params",
        start.get("params").ok_or("thread fixture has no params")?,
    )?;
    let started = child.call(schema, &start, 2)?;
    assert_result(schema, "thread/start", &started)?;
    let mut invalid_result = started
        .get("result")
        .ok_or("thread/start has no result")?
        .clone();
    *invalid_result
        .get_mut("thread")
        .and_then(|thread| thread.get_mut("id"))
        .ok_or("thread/start has no nested thread id")? = sonic_rs::json!(7);
    assert!(
        assert_fixture_shape(
            schema,
            "clientRequests",
            "thread/start",
            "result",
            &invalid_result
        )
        .is_err(),
        "captured thread/start accepted a numeric nested thread id"
    );
    let thread = started
        .get("result")
        .and_then(|result| result.get("thread"))
        .and_then(|thread| thread.get("id"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or("thread/start returned no thread id")?
        .to_owned();
    let notice = child.next(schema)?;
    assert_eq!(
        notice.get("method").and_then(sonic_rs::Value::as_str),
        Some("thread/started")
    );
    assert_eq!(
        notice
            .get("params")
            .and_then(|params| params.get("thread"))
            .and_then(|thread| thread.get("id"))
            .and_then(sonic_rs::Value::as_str),
        Some(thread.as_str())
    );
    Ok(thread)
}

fn smoke_reject_second_initialize(
    child: &mut CodexChild,
    context: &SmokeContext,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    use sonic_rs::JsonValueMutTrait;

    let schema = &context.schema;
    let mut second_initialize =
        fixture_request(&context.request_root, "initialize.request.json", 3)?;
    second_initialize
        .get_mut("params")
        .and_then(|params| params.as_object_mut())
        .ok_or("initialize fixture params are not an object")?
        .insert(
            "clientInfo",
            sonic_rs::json!({"name": "gate-client", "version": "0.1.0"}),
        );
    let rejected = child.call(schema, &second_initialize, 3)?;
    assert_result(schema, "initialize", &rejected)?;
    let code = rejected
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(sonic_rs::Value::as_i64)
        .ok_or("second initialize has no numeric error")?;
    assert_eq!(code, -32600);
    Ok(())
}

fn smoke_resume_and_list(
    child: &mut CodexChild,
    context: &SmokeContext,
    thread: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let schema = &context.schema;
    let resumed = child.call(
        schema,
        &client_request(
            schema,
            "thread/resume",
            4,
            &sonic_rs::json!({"threadId": thread}),
        )?,
        4,
    )?;
    assert_result(schema, "thread/resume", &resumed)?;
    assert_eq!(
        resumed
            .get("result")
            .and_then(|result| result.get("thread"))
            .and_then(|thread| thread.get("id"))
            .and_then(sonic_rs::Value::as_str),
        Some(thread)
    );
    let listed = child.call(
        schema,
        &client_request(schema, "thread/list", 5, &sonic_rs::json!({}))?,
        5,
    )?;
    assert_result(schema, "thread/list", &listed)?;
    let data = listed
        .get("result")
        .and_then(|result| result.get("data"))
        .and_then(sonic_rs::Value::as_array)
        .ok_or("thread/list returned no data page")?;
    assert!(
        data.iter()
            .any(|entry| entry.get("id").and_then(sonic_rs::Value::as_str) == Some(thread))
    );
    Ok(())
}

fn smoke_reject_invalid_turn_starts(
    child: &mut CodexChild,
    context: &SmokeContext,
    thread: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let schema = &context.schema;
    for (id, params) in [
        (
            6,
            sonic_rs::json!({"threadId": thread, "input": [{"type": "image"}]}),
        ),
        (
            7,
            sonic_rs::json!({"threadId": "not-a-thread", "input": [{"type": "text", "text": "hi"}]}),
        ),
    ] {
        let method = "turn/start";
        // Deliberately malformed requests must reach the real server. In
        // particular, the image without url/fileId violates the pinned union.
        let frame = sonic_rs::json!({"id": id, "method": method, "params": params});
        let denied = child.call(schema, &frame, id)?;
        assert_result(schema, method, &denied)?;
        let code = denied
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(sonic_rs::Value::as_i64)
            .ok_or("invalid turn/start has no numeric error")?;
        assert_eq!(code, -32602);
    }
    Ok(())
}

fn smoke_interrupt_unknown_turn(
    child: &mut CodexChild,
    context: &SmokeContext,
    thread: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let schema = &context.schema;
    let denied = child.call(
        schema,
        &client_request(
            schema,
            "turn/interrupt",
            8,
            &sonic_rs::json!({"threadId": thread, "turnId": "nope"}),
        )?,
        8,
    )?;
    assert_result(schema, "turn/interrupt", &denied)?;
    assert_eq!(
        denied
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(sonic_rs::Value::as_i64),
        Some(-32602)
    );
    Ok(())
}

fn smoke_unknown_method(
    child: &mut CodexChild,
    context: &SmokeContext,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let schema = &context.schema;
    let unknown = fixture_request(&context.request_root, "unknown-method.request.json", 9)?;
    let unknown_method = unknown
        .get("method")
        .and_then(sonic_rs::Value::as_str)
        .ok_or("unknown-method fixture has no method")?;
    assert!(
        schema
            .get("clientRequests")
            .and_then(|table| table.get(unknown_method))
            .is_none()
    );
    let rejected = child.call(schema, &unknown, 9)?;
    assert_result(schema, unknown_method, &rejected)?;
    assert_eq!(
        rejected
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(sonic_rs::Value::as_i64),
        Some(-32601)
    );
    Ok(())
}

fn smoke_turn_start(
    schema: &sonic_rs::Value,
    id: u64,
    thread: &str,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    client_request(
        schema,
        "turn/start",
        id,
        &sonic_rs::json!({
            "threadId": thread,
            "input": [{"type": "text", "text": "run the scripted wire leg"}],
        }),
    )
}

fn smoke_turn_id(reply: &sonic_rs::Value) -> Result<String, Box<dyn Error + Send + Sync>> {
    Ok(reply
        .get("result")
        .and_then(|result| result.get("turn"))
        .and_then(|turn| turn.get("id"))
        .and_then(sonic_rs::Value::as_str)
        .map(str::to_owned)
        .ok_or("turn/start returned no turn id")?)
}

fn smoke_approval_leg(
    child: &mut CodexChild,
    context: &SmokeContext,
    thread: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let schema = &context.schema;
    let started = child.call(schema, &smoke_turn_start(schema, 10, thread)?, 10)?;
    assert_result(schema, "turn/start", &started)?;
    let first_turn = smoke_turn_id(&started)?;
    let (approval, first_evidence) = await_server_request(
        child,
        schema,
        "item/commandExecution/requestApproval",
        thread,
        &first_turn,
    )?;
    let approval_reply =
        server_response(schema, &approval, &sonic_rs::json!({"decision": "decline"}))?;
    child.send(&approval_reply)?;
    let first_events = drain_turn(child, schema, first_evidence, "completed")?;
    assert!(
        first_events.1,
        "approval turn emitted no item/completed notification"
    );
    assert!(
        first_events.2,
        "approval turn emitted no item/agentMessage/delta notification"
    );
    Ok(())
}

fn smoke_input_leg(
    child: &mut CodexChild,
    context: &SmokeContext,
    thread: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    use sonic_rs::JsonValueMutTrait;

    let schema = &context.schema;
    let started = child.call(schema, &smoke_turn_start(schema, 20, thread)?, 20)?;
    assert_result(schema, "turn/start", &started)?;
    let second_turn = smoke_turn_id(&started)?;
    let (input, second_evidence) = await_server_request(
        child,
        schema,
        "item/tool/requestUserInput",
        thread,
        &second_turn,
    )?;
    let question_id = input
        .get("params")
        .and_then(|params| params.get("questions"))
        .and_then(sonic_rs::Value::as_array)
        .and_then(|questions| questions.first())
        .and_then(|question| question.get("id"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or("user-input request has no question id")?
        .to_owned();
    let mut input_result = sonic_rs::json!({"answers": {}});
    input_result
        .get_mut("answers")
        .and_then(|answers| answers.as_object_mut())
        .ok_or("user-input answers is not an object")?
        .insert(
            question_id.as_str(),
            sonic_rs::json!({"answers": ["gate answer"]}),
        );
    let input_reply = server_response(schema, &input, &input_result)?;
    child.send(&input_reply)?;
    let second_events = drain_turn(child, schema, second_evidence, "completed")?;
    assert!(
        second_events.1,
        "user-input turn emitted no item/completed notification"
    );
    assert!(
        second_events.2,
        "user-input turn emitted no item/agentMessage/delta notification"
    );
    Ok(())
}

fn smoke_interrupt_leg(
    child: &mut CodexChild,
    context: &SmokeContext,
    thread: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let schema = &context.schema;
    let started = child.call(schema, &smoke_turn_start(schema, 30, thread)?, 30)?;
    assert_result(schema, "turn/start", &started)?;
    let third_turn = smoke_turn_id(&started)?;
    let (approval, third_evidence) = await_server_request(
        child,
        schema,
        "item/commandExecution/requestApproval",
        thread,
        &third_turn,
    )?;
    let interrupt = client_request(
        schema,
        "turn/interrupt",
        31,
        &sonic_rs::json!({"threadId": thread, "turnId": third_turn.as_str()}),
    )?;
    let interrupted = child.call(schema, &interrupt, 31)?;
    assert_result(schema, "turn/interrupt", &interrupted)?;
    assert!(
        interrupted.get("result").is_some(),
        "turn/interrupt must succeed"
    );
    let _ = approval;
    let third_events = drain_turn(child, schema, third_evidence, "interrupted")?;
    assert!(
        third_events.1,
        "interrupted turn emitted no item/completed notification"
    );
    Ok(())
}

fn smoke_finish(child: CodexChild) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (status_ok, stderr, seen) = child.finish();
    assert!(status_ok, "app-server exited unsuccessfully: {stderr}");
    let mut reply_ids = Vec::new();
    for line in &seen {
        let frame: sonic_rs::Value = sonic_rs::from_str(line)?;
        assert!(frame.get("jsonrpc").is_none());
        assert!(frame.get("router").is_none());
        if frame.get("method").is_none()
            && let Some(id) = frame.get("id").and_then(sonic_rs::Value::as_u64)
        {
            reply_ids.push(id);
        }
    }
    reply_ids.sort_unstable();
    assert_eq!(
        reply_ids,
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 20, 30, 31],
        "every Codex client request drew exactly one reply; stderr: {stderr}"
    );
    Ok(())
}

/// Drives the real app-server through fixture-bound lifecycle and full-duplex
/// turn legs, including approval, user input, and interruption.
#[test]
fn codex_pinned_subset_smoke() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (_temp, mut child, context) = smoke_setup()?;
    smoke_initialize(&mut child, &context)?;
    let thread = smoke_start_thread(&mut child, &context)?;
    smoke_reject_second_initialize(&mut child, &context)?;
    smoke_resume_and_list(&mut child, &context, &thread)?;
    smoke_reject_invalid_turn_starts(&mut child, &context, &thread)?;
    smoke_interrupt_unknown_turn(&mut child, &context, &thread)?;
    smoke_unknown_method(&mut child, &context)?;
    smoke_approval_leg(&mut child, &context, &thread)?;
    smoke_input_leg(&mut child, &context, &thread)?;
    smoke_interrupt_leg(&mut child, &context, &thread)?;
    smoke_finish(child)
}

/// Sends one ACP request and returns every raw stdout line up to its reply.
#[cfg(target_os = "linux")]
fn acp_call(
    stdin: &mut std::process::ChildStdin,
    lines: &std::sync::mpsc::Receiver<String>,
    seen: &mut Vec<String>,
    frame: &str,
    id: u64,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    writeln!(stdin, "{frame}")?;
    stdin.flush()?;
    loop {
        let line = lines.recv_timeout(std::time::Duration::from_secs(30))?;
        seen.push(line.clone());
        let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(&line) else {
            continue;
        };
        if value.get("id").and_then(sonic_rs::Value::as_u64) == Some(id)
            && value.get("method").is_none()
        {
            return Ok(value);
        }
    }
}

/// Runs initialize, session/new, and one prompt; returns the prompt reply.
#[cfg(target_os = "linux")]
fn acp_prompt_turn(
    stdin: &mut std::process::ChildStdin,
    lines: &std::sync::mpsc::Receiver<String>,
    seen: &mut Vec<String>,
    workspace: &std::path::Path,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    acp_call(
        stdin,
        lines,
        seen,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"gate"}}}"#,
        1,
    )?;
    let session = acp_call(
        stdin,
        lines,
        seen,
        &format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"session/new","params":{{"cwd":{:?},"mcpServers":[]}}}}"#,
            workspace.to_string_lossy()
        ),
        2,
    )?;
    let session_id = session
        .get("result")
        .and_then(|result| result.get("sessionId"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or("session/new returned no session id")?
        .to_owned();
    acp_call(
        stdin,
        lines,
        seen,
        &format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session_id}","prompt":[{{"type":"text","text":"run the tool"}}]}}}}"#
        ),
        3,
    )
}

#[cfg(target_os = "linux")]
#[test]
fn acp_stream_survives_tool_child_writing_to_process_stdout()
-> Result<(), Box<dyn Error + Send + Sync>> {
    use std::io::{BufRead, BufReader};

    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    let replay = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/replay/acp-stdout-tool.jsonl");
    fs::write(
        home.join(".config/dal/dal.toml"),
        format!(
            "model = \"openai/gpt-6-luna\"\napproval = \"all\"\n[providers.scripted]\nfixture = {:?}\n",
            replay.to_string_lossy()
        ),
    )?;
    let mut child = Command::new(dalgon_binary()?)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "sk-test")
        .arg("acp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let mut guard = ChildGuard(child);
    let (sender, lines) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
            if sender
                .send(String::from_utf8_lossy(&line).into_owned())
                .is_err()
            {
                break;
            }
        }
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut text);
        text
    });

    let mut seen = Vec::<String>::new();
    let prompt = acp_prompt_turn(&mut stdin, &lines, &mut seen, &workspace)?;
    drop(stdin);
    guard.0.wait()?;
    reader.join().map_err(|_| "stdout reader panicked")?;
    seen.extend(lines.try_iter());
    let stderr_text = stderr_reader.join().map_err(|_| "stderr reader panicked")?;

    assert_eq!(
        prompt
            .get("result")
            .and_then(|result| result.get("stopReason"))
            .and_then(sonic_rs::Value::as_str),
        Some("end_turn"),
        "the prompt turn must finish; stderr: {stderr_text}"
    );
    for line in &seen {
        let frame: sonic_rs::Value = sonic_rs::from_str(line)
            .map_err(|error| format!("stdout carried a non-JSON line {line:?}: {error}"))?;
        assert_eq!(
            frame.get("jsonrpc").and_then(sonic_rs::Value::as_str),
            Some("2.0"),
            "stdout carried a frame without jsonrpc 2.0: {line}"
        );
    }
    assert!(
        stderr_text.contains("TOOL-OUTPUT-LINE") && stderr_text.contains("TOOL-OUTPUT-PARTIAL"),
        "tool output aimed at the process stdout must land on stderr; stderr: {stderr_text}"
    );
    Ok(())
}

/// A persistent `dalgon rpc` stdio child: one JSON frame per line, replies
/// matched by id, notifications skipped. The child and its reader thread are
/// reaped when the value drops, whatever the test returned.
struct StdioRpc {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    lines: std::sync::mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl StdioRpc {
    fn spawn(
        binary: &std::path::Path,
        home: &std::path::Path,
        workspace: &std::path::Path,
        data_home: &std::path::Path,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        use std::io::{BufRead, BufReader};

        let mut child = Command::new(binary)
            .current_dir(workspace)
            .env_clear()
            .envs(support::captured_shell_vars())
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", data_home)
            .env("NO_COLOR", "1")
            .arg("rpc")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or("rpc stdin was not piped")?;
        let stdout = child.stdout.take().ok_or("rpc stdout was not piped")?;
        let (sender, lines) = std::sync::mpsc::sync_channel::<String>(256);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            lines,
            reader: Some(reader),
        })
    }

    fn call(
        &mut self,
        id: u64,
        method: &str,
        params: &sonic_rs::Value,
    ) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
        let frame =
            sonic_rs::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let stdin = self.stdin.as_mut().ok_or("rpc stdin is closed")?;
        writeln!(stdin, "{}", sonic_rs::to_string(&frame)?)?;
        stdin.flush()?;
        loop {
            let line = self
                .lines
                .recv_timeout(std::time::Duration::from_secs(30))
                .map_err(|error| format!("no rpc frame for {method} within 30s: {error}"))?;
            let value: sonic_rs::Value = sonic_rs::from_str(&line)?;
            if value.get("id").and_then(sonic_rs::Value::as_u64) == Some(id)
                && value.get("method").is_none()
            {
                return Ok(value);
            }
        }
    }
}

impl Drop for StdioRpc {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// An editor or remote client talking to `dalgon rpc` over stdio saves its
/// model scope into the user's `dal.toml`: the wire host knows where the
/// user file lives, the save keeps the user's own text, and the reply says
/// what was saved.
#[test]
fn rpc_stdio_scope_save_reaches_the_users_dal_toml() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    let config = home.join(".config/dal/dal.toml");
    fs::write(&config, SCOPE_SAVE_SEED)?;
    let mut rpc = StdioRpc::spawn(dalgon_binary()?, &home, &workspace, &data_home)?;
    rpc.call(1, "initialize", &initialize_params())?;
    let workspace_text = workspace.to_str().ok_or("workspace path is not UTF-8")?;
    let opened = rpc.call(
        2,
        "session/open",
        &sonic_rs::json!({"ref": {"type": "new", "workspace": workspace_text}}),
    )?;
    let session = opened
        .get("result")
        .and_then(|result| result.get("sessionId"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or_else(|| format!("session/open returned no session: {opened}"))?
        .to_owned();
    let saved = rpc.call(3, "session/submit", &scope_save_params(&session))?;
    assert_scope_save_landed(&saved, &config)
}

/// A client of the local-socket `dalgon rpc --socket` endpoint saves its
/// model scope into the user's `dal.toml`: the second wire host start
/// (`local_socket`) knows where the user file lives, like the stdio one.
#[cfg(unix)]
#[tokio::test]
async fn rpc_socket_scope_save_reaches_the_users_dal_toml()
-> Result<(), Box<dyn Error + Send + Sync>> {
    // The default socket lives under the data root; the deep BSD temp root
    // would push the endpoint past SUN_LEN.
    let dir = TestDir::new_in(std::path::Path::new("/tmp"))?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    let config = home.join(".config/dal/dal.toml");
    fs::write(&config, SCOPE_SAVE_SEED)?;
    let _server = spawn_rpc(dalgon_binary()?, &home, &workspace, &data_home, None)?;
    let socket = data_home.join("dal/rpc/dal.sock");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let stream = loop {
        match UnixStream::connect(&socket).await {
            Ok(stream) => break stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    socket_call(&mut write, &mut lines, 1, "initialize", initialize_params()).await?;
    let workspace_text = workspace.to_str().ok_or("workspace path is not UTF-8")?;
    let opened = socket_call(
        &mut write,
        &mut lines,
        2,
        "session/open",
        sonic_rs::json!({"ref": {"type": "new", "workspace": workspace_text}}),
    )
    .await?;
    let session = opened
        .get("result")
        .and_then(|result| result.get("sessionId"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or_else(|| format!("session/open returned no session: {opened}"))?
        .to_owned();
    let saved = socket_call(
        &mut write,
        &mut lines,
        3,
        "session/submit",
        scope_save_params(&session),
    )
    .await?;
    assert_scope_save_landed(&saved, &config)
}

/// One request over a connected local socket and its matching reply;
/// notifications in between are skipped.
#[cfg(unix)]
async fn socket_call(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    id: u64,
    method: &str,
    params: sonic_rs::Value,
) -> Result<sonic_rs::Value, Box<dyn Error + Send + Sync>> {
    let frame = sonic_rs::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    write
        .write_all(sonic_rs::to_string(&frame)?.as_bytes())
        .await?;
    write.write_all(b"\n").await?;
    let reply = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or("local RPC closed before it replied")?;
            let value: sonic_rs::Value = sonic_rs::from_str(&line)?;
            if value.get("id").and_then(sonic_rs::Value::as_u64) == Some(id)
                && value.get("method").is_none()
            {
                return Ok::<_, Box<dyn Error + Send + Sync>>(value);
            }
        }
    })
    .await??;
    Ok(reply)
}
