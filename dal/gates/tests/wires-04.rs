#![expect(
    clippy::disallowed_methods,
    reason = "SC test exercises real websocket server"
)]

//! Serve failure surfaces and websocket request handling.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    fs,
    process::{Child, Command, Stdio},
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use sonic_rs::JsonValueTrait;
use support::{TestDir, dalgon_binary};
use tokio_tungstenite::tungstenite::protocol::Message;

use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

fn serve_failure(
    dir: &std::path::Path,
    stage: &str,
    error: &dyn std::fmt::Display,
) -> Box<dyn Error + Send + Sync> {
    let log = fs::read_to_string(dir.join("serve-ws.log")).unwrap_or_default();
    format!("serve failed at {stage}: {error}\nserve-ws.log:\n{log}").into()
}

fn websocket_request(
    url: &str,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, Box<dyn Error + Send + Sync>> {
    let request = url
        .into_client_request()
        .map_err(|error| format!("invalid websocket url: {error}"))?;
    Ok(request)
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

async fn advertisement_websocket(
    data_root: &std::path::Path,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let dir = data_root.join("run/serve");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "json")
                    && let Ok(bytes) = std::fs::read(entry.path())
                    && let Ok(value) = sonic_rs::from_slice::<sonic_rs::Value>(&bytes)
                    && let Some(url) = value.get("websocket").and_then(sonic_rs::Value::as_str)
                {
                    return Ok(url.to_owned());
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("serve advertisement never appeared".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "SC websocket contract is one long script"
)]
async fn websocket_auth_origin_frame_keepalive_and_disconnect_contract()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    let data_root = data_home.join("dal");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    fs::write(
        home.join(".config/dal/dal.toml"),
        "model = \"openai-responses/gpt-6\"\n",
    )?;
    let binary = dalgon_binary("dalgon")?;

    let token_out = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "token"])
        .output()?;
    assert!(token_out.status.success());
    let token = String::from_utf8(token_out.stdout)?.trim().to_owned();

    let child = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("NO_COLOR", "1")
        .args(["serve", "--public", "--bind", "127.0.0.1", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(fs::File::create(
            dir.path().join("serve-ws.log"),
        )?))
        .spawn()?;
    let _guard = ChildGuard(Some(child));

    let url = advertisement_websocket(&data_root).await?;

    let mut bearer = websocket_request(&url)?;
    bearer.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|error| format!("bad bearer header: {error}"))?,
    );
    let dir_path = dir.path().to_owned();
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(bearer),
    )
    .await
    .map_err(|error| serve_failure(&dir_path, "authenticated-connect", &error))?
    .map_err(|error| serve_failure(&dir_path, "authenticated-connect", &error))?;
    socket
        .send(Message::Text(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}".into(),
        ))
        .await
        .map_err(|error| serve_failure(&dir_path, "initialize-1-send", &error))?;
    socket
        .send(Message::Text(
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":{}}".into(),
        ))
        .await
        .map_err(|error| serve_failure(&dir_path, "initialize-2-send", &error))?;
    for expected in [1u64, 2] {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .map_err(|error| serve_failure(&dir_path, "first-replies-read", &error))?
            .expect("frame")
            .map_err(|error| serve_failure(&dir_path, "first-replies-read", &error))?;
        let text = frame.into_text()?;
        let value: sonic_rs::Value = sonic_rs::from_str(&text)?;
        assert_eq!(
            value.get("id").and_then(sonic_rs::Value::as_u64),
            Some(expected)
        );
    }

    let mut bad = websocket_request(&url)?;
    bad.headers_mut().insert(
        "authorization",
        "Bearer dal_dead"
            .parse()
            .map_err(|error| format!("bad bearer header: {error}"))?,
    );
    let bad_result = tokio_tungstenite::connect_async(bad).await;
    assert!(
        bad_result.is_err(),
        "bad token must be denied: {bad_result:?}"
    );

    let mut evil = websocket_request(&url)?;
    evil.headers_mut().insert(
        "origin",
        "https://evil.example"
            .parse()
            .map_err(|error| format!("bad origin header: {error}"))?,
    );
    let evil_result = tokio_tungstenite::connect_async(evil).await;
    assert!(
        evil_result.is_err(),
        "disallowed origin must be 403: {evil_result:?}"
    );

    socket
        .send(Message::Ping(vec![1, 2, 3].into()))
        .await
        .map_err(|error| serve_failure(&dir_path, "ping-send", &error))?;
    let pong = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .map_err(|error| serve_failure(&dir_path, "pong-read", &error))?
        .expect("pong")
        .map_err(|error| serve_failure(&dir_path, "pong-read", &error))?;
    assert!(matches!(pong, Message::Pong(_)));

    socket
        .send(Message::Binary(vec![0u8; 32].into()))
        .await
        .map_err(|error| serve_failure(&dir_path, "binary-send", &error))?;
    let close = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .map_err(|error| serve_failure(&dir_path, "binary-close-read", &error))?
        .expect("close")
        .map_err(|error| serve_failure(&dir_path, "binary-close-read", &error))?;
    match close {
        Message::Close(Some(frame)) => assert_eq!(
            frame.code,
            tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Unsupported
        ),
        other => return Err(format!("binary frame must close with 1003, got {other:?}").into()),
    }
    drop(socket);

    let mut over = websocket_request(&url)?;
    over.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|error| format!("bad bearer header: {error}"))?,
    );
    let (mut over_socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(over),
    )
    .await
    .map_err(|error| serve_failure(&dir_path, "oversized-connect", &error))?
    .map_err(|error| serve_failure(&dir_path, "oversized-connect", &error))?;
    let _ = over_socket
        .send(Message::Binary(vec![0u8; 16 * 1024 * 1024 + 1].into()))
        .await;
    match tokio::time::timeout(Duration::from_secs(5), over_socket.next())
        .await
        .map_err(|error| serve_failure(&dir_path, "close-read", &error))?
    {
        None | Some(Err(_)) => {}
        Some(Ok(frame)) => {
            return Err(
                format!("oversized frame must end without a close frame, got {frame:?}").into(),
            );
        }
    }
    drop(over_socket);

    let mut bearer2 = websocket_request(&url)?;
    bearer2.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|error| format!("bad bearer header: {error}"))?,
    );
    let (mut socket2, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(bearer2),
    )
    .await
    .map_err(|error| serve_failure(&dir_path, "close-read", &error))?
    .map_err(|error| serve_failure(&dir_path, "close-read", &error))?;
    socket2
        .send(Message::Text(
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"initialize\",\"params\":{}}".into(),
        ))
        .await
        .map_err(|error| serve_failure(&dir_path, "close-read", &error))?;
    let frame = tokio::time::timeout(Duration::from_secs(5), socket2.next())
        .await
        .map_err(|error| serve_failure(&dir_path, "reattach-connect", &error))?
        .expect("frame")
        .map_err(|error| serve_failure(&dir_path, "reattach-connect", &error))?;
    assert!(
        frame.into_text()?.contains("\"id\":3"),
        "disconnect must cancel nothing"
    );
    Ok(())
}

#[tokio::test]
async fn loopback_websocket_accepts_connections_without_a_token()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    let data_root = data_home.join("dal");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    fs::write(
        home.join(".config/dal/dal.toml"),
        "model = \"openai-responses/gpt-6\"\n",
    )?;
    let binary = dalgon_binary("dalgon")?;

    let child = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .env("NO_COLOR", "1")
        .args(["serve", "--bind", "127.0.0.1", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(fs::File::create(
            dir.path().join("serve-ws.log"),
        )?))
        .spawn()?;
    let _guard = ChildGuard(Some(child));

    let url = advertisement_websocket(&data_root).await?;
    let dir_path = dir.path().to_owned();

    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(websocket_request(&url)?),
    )
    .await
    .map_err(|error| serve_failure(&dir_path, "tokenless-connect", &error))?
    .map_err(|error| serve_failure(&dir_path, "tokenless-connect", &error))?;
    socket
        .send(Message::Text(
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"initialize\",\"params\":{}}".into(),
        ))
        .await
        .map_err(|error| serve_failure(&dir_path, "tokenless-send", &error))?;
    let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .map_err(|error| serve_failure(&dir_path, "tokenless-read", &error))?
        .expect("frame")
        .map_err(|error| serve_failure(&dir_path, "tokenless-read", &error))?;
    assert!(
        frame.into_text()?.contains("\"id\":7"),
        "loopback WebSocket must answer without a token"
    );
    Ok(())
}
