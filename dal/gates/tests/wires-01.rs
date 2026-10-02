#![expect(clippy::expect_used, reason = "SC test")]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test exercises real wire binaries"
)]

//! Stdio and socket wire requests: framing, dispatch, and drop semantics.
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

use sonic_rs::JsonValueTrait;
use support::{TestDir, dalgon_binary};

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
    let binary = dalgon_binary("dalgon")?;
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
    let private_dir = workspace.join("private");
    fs::create_dir_all(&private_dir)?;
    fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700))?;
    let socket = private_dir.join("rpc.sock");
    let binary = dalgon_binary("dalgon")?;
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
    let binary = dalgon_binary("dalgon")?;

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
        .join("../crates/dalgon/tests/fixtures/codex-app-server");
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
