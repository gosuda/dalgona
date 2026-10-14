#![expect(
    clippy::disallowed_methods,
    reason = "SC test launches the real server process"
)]
#![expect(
    dead_code,
    reason = "gate support exposes helpers shared across independent targets"
)]

//! WebSocket authorization failures do not disclose token values.

#[expect(
    dead_code,
    reason = "shared session helpers are used by other gate integration targets"
)]
mod support;

use std::{
    error::Error,
    fs, io,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use sonic_rs::JsonValueTrait;
use support::{TestDir, dalgon_binary};
use tokio::io::AsyncReadExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

struct ServeHandle {
    child: std::process::Child,
    websocket_url: String,
}

impl Drop for ServeHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn spawn_serve(
    binary: &Path,
    workspace: &Path,
    home: &Path,
    data_home: &Path,
) -> io::Result<ServeHandle> {
    let data_root = data_home.join("dal");
    let mut child = Command::new(binary)
        .current_dir(workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", data_home)
        .env("NO_COLOR", "1")
        .env("DAL_LOG", "debug")
        .args(["serve", "--public", "--bind", "127.0.0.1", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(
            data_home.join("serve-ws.log"),
        )?))
        .spawn()?;
    let dir = data_root.join("run/serve");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json")
                    && let Ok(bytes) = std::fs::read(&path)
                    && let Ok(value) = sonic_rs::from_slice::<sonic_rs::Value>(&bytes)
                    && let Some(url) = value.get("websocket").and_then(sonic_rs::Value::as_str)
                {
                    return Ok(ServeHandle {
                        child,
                        websocket_url: url.to_owned(),
                    });
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("serve advertisement never appeared"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn websocket_auth_never_logs_token_values() -> Result<(), Box<dyn Error + Send + Sync>> {
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

    let created = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "token"])
        .output()?;
    assert!(
        created.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&created.stdout),
        String::from_utf8_lossy(&created.stderr)
    );
    let real_token = String::from_utf8(created.stdout)?.trim().to_owned();
    let sentinel = "dal_ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    let serve = spawn_serve(binary, &workspace, &home, &data_home).await?;
    let url = serve.websocket_url.clone();

    assert!(
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async(url.as_str()),
        )
        .await?
        .is_err(),
        "public WebSocket rejects connections without a token"
    );

    let mut bearer = url.as_str().into_client_request()?;
    bearer
        .headers_mut()
        .insert("Authorization", format!("Bearer {sentinel}").parse()?);
    assert!(tokio_tungstenite::connect_async(bearer).await.is_err());

    let mut subprotocol = url.as_str().into_client_request()?;
    subprotocol.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        format!("dal.v1, dal.bearer.{sentinel}").parse()?,
    );
    assert!(tokio_tungstenite::connect_async(subprotocol).await.is_err());

    let mut transport = dal_wire::WebSocketTransport::connect_with_auth(&url, &real_token).await?;
    transport
        .write_frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .await?;
    let reply = tokio::time::timeout(Duration::from_secs(5), transport.read_frame()).await??;
    let reply: sonic_rs::Value = sonic_rs::from_str(&reply)?;
    assert_eq!(reply.get("id").and_then(sonic_rs::Value::as_u64), Some(1));
    assert!(
        reply.get("result").is_some(),
        "authenticated client receives an RPC result"
    );
    drop(transport);
    drop(serve);

    let mut logged = String::new();
    for path in [
        data_home.join("dal/cache/dal.log"),
        data_home.join("serve-ws.log"),
    ] {
        match std::fs::read_to_string(&path) {
            Ok(text) => logged.push_str(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let serve_log = data_home.join("serve-ws.log");
    assert!(
        serve_log.is_file(),
        "serve stderr must be captured in a file"
    );
    let serve_text = std::fs::read_to_string(&serve_log)?;
    assert!(
        serve_text.contains("plain HTTP"),
        "serve stderr must carry the public warning, got: {serve_text:.400}"
    );
    assert!(
        !logged.contains(sentinel),
        "logs must never contain token values"
    );
    assert!(
        !logged.contains(&real_token),
        "logs must never contain token values"
    );
    assert!(
        !logged.contains("Bearer"),
        "logs must never contain header values"
    );
    Ok(())
}

#[tokio::test]
async fn silent_sockets_do_not_exhaust_authenticated_admission()
-> Result<(), Box<dyn Error + Send + Sync>> {
    // The pending budget retains at most this many unverified sockets.
    const PENDING_BUDGET: usize = 32;
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
    let created = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "token"])
        .output()?;
    assert!(
        created.status.success(),
        "serve token creation must succeed"
    );
    let token = String::from_utf8(created.stdout)?.trim().to_owned();

    let serve = spawn_serve(binary, &workspace, &home, &data_home).await?;
    let address: std::net::SocketAddr = serve
        .websocket_url
        .strip_prefix("ws://")
        .and_then(|url| url.split('/').next())
        .ok_or_else(|| io::Error::other("serve WebSocket URL has no authority"))?
        .parse()?;

    // An attacker without a token can hold sockets without sending
    // headers. A TCP connect alone does not prove the listener accepted
    // the socket (the backlog parks unaccepted peers), so establish
    // saturation first: the pending budget retains at most 32 unverified
    // sockets and evicts the stale end FIFO, which means the first 224
    // peers must be closed by the server while the newest stay open.
    let mut peers = Vec::with_capacity(256);
    for _ in 0..256 {
        peers.push(tokio::net::TcpStream::connect(address).await?);
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        for peer in &mut peers[..256 - PENDING_BUDGET] {
            let mut probe = [0_u8; 1];
            match peer.read(&mut probe).await {
                Ok(0) => {}
                Ok(read) => {
                    return Err(io::Error::other(format!(
                        "evicted silent peer sent {read} bytes"
                    )));
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok::<(), io::Error>(())
    })
    .await
    .map_err(|_| {
        io::Error::other("stale silent peers were not evicted before the header deadline")
    })??;
    let mut probe = [0_u8; 1];
    assert!(
        matches!(
            peers[255].try_read(&mut probe),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ),
        "the newest silent peer must still be pending inside the budget"
    );

    // With the listener saturated and stale peers culled, an authenticated
    // WebSocket must still initialize.
    let mut valid = tokio::time::timeout(
        Duration::from_secs(5),
        dal_wire::WebSocketTransport::connect_with_auth(&serve.websocket_url, &token),
    )
    .await??;
    valid
        .write_frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .await?;
    let reply = tokio::time::timeout(Duration::from_secs(5), valid.read_frame()).await??;
    let reply: sonic_rs::Value = sonic_rs::from_str(&reply)?;
    assert_eq!(reply.get("id").and_then(sonic_rs::Value::as_u64), Some(1));
    assert!(reply.get("result").is_some(), "authenticated RPC is served");

    // The retained pending peers still die at the header deadline; they
    // must not hold a slot past it.
    tokio::time::timeout(Duration::from_secs(12), async {
        for peer in &mut peers[256 - PENDING_BUDGET..] {
            let mut discarded = Vec::new();
            match peer.read_to_end(&mut discarded).await {
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok::<(), io::Error>(())
    })
    .await??;
    drop(valid);
    drop(serve);
    Ok(())
}
