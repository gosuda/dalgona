use std::collections::BTreeMap;
use std::net::SocketAddr;

use sonic_rs::JsonValueTrait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use super::support::{
    HttpReply, ask_probe_extension, host_header, http, parse_reply, result_text, rig,
    rig_with_extensions, router_options, text_step, tool_step, with_serve,
};
use crate::error::ServeError;
#[cfg(unix)]
use crate::token::TokenError;

const CHAT: &str = "POST /v1/chat/completions HTTP/1.1";

fn chat_body() -> &'static str {
    r#"{"model":"dalgon/normal","messages":[{"role":"user","content":"hi"}]}"#
}

async fn get(addr: SocketAddr, path: &str) -> HttpReply {
    http(
        addr,
        &format!("GET {path} HTTP/1.1\r\n{}", host_header(addr)),
        "",
    )
    .await
}

#[tokio::test]
async fn route_404_405() {
    let rig = rig(&[]).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        let missing = get(addr, "/v1/nope").await;
        assert_eq!(missing.status, 404);
        assert_eq!(missing.body, "no route for GET /v1/nope");
        let wrong = get(addr, "/v1/chat/completions").await;
        assert_eq!(wrong.status, 405);
        assert_eq!(wrong.header("allow"), Some("POST"));
        assert_eq!(
            wrong.body,
            "GET /v1/chat/completions is not allowed: use POST"
        );
        let card = get(addr, "/.well-known/agent-card.json").await;
        assert_eq!(card.status, 404, "{card:?}");
        let a2a = http(
            addr,
            &format!(
                "POST /a2a HTTP/1.1\r\n{}\r\ncontent-type: application/json",
                host_header(addr)
            ),
            "{}",
        )
        .await;
        assert_eq!(a2a.status, 404, "{a2a:?}");
    })
    .await;
}

#[tokio::test]
async fn http_guard_limits() {
    let rig = rig(&[]).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        let origin = http(
            addr,
            &format!(
                "{CHAT}\r\n{}\r\norigin: https://evil.example\r\ncontent-type: application/json",
                host_header(addr)
            ),
            chat_body(),
        )
        .await;
        assert_eq!(origin.status, 403, "{origin:?}");
        let host = http(
            addr,
            &format!("{CHAT}\r\nhost: evil.example\r\ncontent-type: application/json"),
            chat_body(),
        )
        .await;
        assert_eq!(host.status, 403);
        assert_eq!(
            host.body,
            r#"Host header "evil.example" is not a loopback name"#
        );
        let plain = http(
            addr,
            &format!(
                "{CHAT}\r\n{}\r\ncontent-type: text/plain",
                host_header(addr)
            ),
            chat_body(),
        )
        .await;
        assert_eq!(plain.status, 415);
        assert_eq!(plain.body, "Content-Type must be application/json");
        let large = oversized(addr).await;
        assert_eq!(large.status, 413);
        assert_eq!(large.body, "request body exceeds 33554432 bytes");
        tokio::time::pause();
        let paused = stalled(addr).await;
        tokio::time::resume();
        assert_eq!(paused.status, 408);
        assert_eq!(paused.body, "request body was not received within 60 s");
    })
    .await;
}

/// Streams a 33 MiB body and reads the response the server sends early.
async fn oversized(addr: SocketAddr) -> HttpReply {
    let total = 33 * 1024 * 1024;
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let head = format!(
        "{CHAT}\r\n{}\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {total}\r\n\r\n",
        host_header(addr)
    );
    let (mut reader, mut writer) = stream.split();
    let send = async {
        let chunk = vec![b' '; 1024 * 1024];
        let _ = writer.write_all(head.as_bytes()).await;
        for _ in 0..33 {
            if writer.write_all(&chunk).await.is_err() {
                break;
            }
        }
        std::future::pending::<()>().await;
    };
    let mut bytes = Vec::new();
    tokio::select! {
        read = tokio::time::timeout(super::support::WAIT, reader.read_to_end(&mut bytes)) => {
            read.expect("413 in time").expect("413 read");
        }
        () = send => {}
    }
    parse_reply(&bytes)
}

/// Sends half a body and waits, on the paused clock, for the body timeout.
async fn stalled(addr: SocketAddr) -> HttpReply {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let head = format!(
        "{CHAT}\r\n{}\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: 100\r\n\r\n{{\"model\":",
        host_header(addr)
    );
    stream
        .write_all(head.as_bytes())
        .await
        .expect("partial body");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.expect("408 read");
    parse_reply(&bytes)
}

#[cfg(unix)]
#[tokio::test]
async fn token_file_auth_cases() {
    let rig = rig(&[]).await;
    let path = rig.data().join("serve.token");
    let public = || {
        let mut options = router_options(&rig);
        options.public = true;
        options.a2a = true;
        options
    };
    let stop = tokio_util::sync::CancellationToken::new();
    let missing = crate::serve_router(rig.host.clone(), public(), stop.clone()).await;
    assert!(
        matches!(&missing, Err(ServeError::Token(TokenError::Missing { path: p })) if *p == path),
        "{:?}",
        missing.err()
    );
    std::fs::write(&path, "dal_x\n").expect("token file");
    std::fs::set_permissions(
        &path,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
    )
    .expect("chmod");
    let open = crate::serve_router(rig.host.clone(), public(), stop.clone()).await;
    assert!(
        matches!(
            &open,
            Err(ServeError::Token(TokenError::OpenToOtherUsers { mode, .. })) if mode & 0o777 == 0o644
        ),
        "{:?}",
        open.err()
    );
    std::fs::set_permissions(
        &path,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )
    .expect("chmod");
    std::fs::write(&path, "\n").expect("empty token");
    let empty = crate::serve_router(rig.host.clone(), public(), stop.clone()).await;
    assert!(
        matches!(&empty, Err(ServeError::Token(TokenError::Empty { .. }))),
        "{:?}",
        empty.err()
    );
    std::fs::remove_file(&path).expect("remove empty token");
    let token = crate::token::create(&path, false).expect("token created");
    with_serve(&rig, public(), async |addr| {
        let base = format!("GET /v1/models HTTP/1.1\r\nhost: 127.0.0.1:{}", addr.port());
        let none = http(addr, &base, "").await;
        assert_eq!(none.status, 401);
        assert_eq!(none.json()["error"]["code"].as_str(), Some("missing_token"));
        assert_eq!(
            none.json()["error"]["message"].as_str(),
            Some("dalgon serve needs a bearer token: send Authorization: Bearer <token from serve.token>")
        );
        let wrong = http(addr, &format!("{base}\r\nauthorization: Bearer dal_wrong"), "").await;
        assert_eq!(wrong.status, 401);
        assert_eq!(
            wrong.json()["error"]["message"].as_str(),
            Some("the bearer token does not match serve.token")
        );
        let bearer = http(addr, &format!("{base}\r\nauthorization: Bearer {token}"), "").await;
        assert_eq!(bearer.status, 200, "{bearer:?}");
        let key = http(addr, &format!("{base}\r\nx-api-key: {token}"), "").await;
        assert_eq!(key.status, 200, "{key:?}");
        let card = http(
            addr,
            &format!("GET /.well-known/agent-card.json HTTP/1.1\r\nhost: 127.0.0.1:{}", addr.port()),
            "",
        )
        .await;
        assert_eq!(card.status, 200, "{card:?}");
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn public_card_path_requires_auth_when_a2a_is_disabled() {
    let rig = rig(&[]).await;
    let token =
        crate::token::create(&rig.data().join("serve.token"), false).expect("create serve token");
    let mut options = router_options(&rig);
    options.public = true;
    with_serve(&rig, options, async |addr| {
        let base = format!(
            "GET /.well-known/agent-card.json HTTP/1.1\r\n{}",
            host_header(addr)
        );
        let unauthenticated = http(addr, &base, "").await;
        assert_eq!(unauthenticated.status, 401, "{unauthenticated:?}");
        let authenticated = http(
            addr,
            &format!("{base}\r\nauthorization: Bearer {token}"),
            "",
        )
        .await;
        assert_eq!(authenticated.status, 404, "{authenticated:?}");
    })
    .await;
}

/// A GET declaring only an empty body — every decimal zero spelling —
/// is not a framed body and keeps its route.
#[tokio::test]
async fn get_routes_accept_all_zero_content_length() {
    let rig = rig(&[]).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        for (declared, expect) in [("0", 200), ("00", 200), ("000", 200), ("01", 400)] {
            let mut stream = tokio::net::TcpStream::connect(addr)
                .await
                .expect("connect");
            let head = format!(
                "GET /v1/models HTTP/1.1\r\n{}\r\ncontent-length: {declared}\r\nconnection: close\r\n\r\n",
                host_header(addr)
            );
            stream.write_all(head.as_bytes()).await.expect("request");
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.expect("reply");
            assert_eq!(parse_reply(&bytes).status, expect, "{declared}");
        }
    })
    .await;
}

#[tokio::test]
async fn get_routes_reject_body_headers_before_reading() {
    let rig = rig(&[]).await;
    let mut options = router_options(&rig);
    options.a2a = true;
    with_serve(&rig, options, async |addr| {
        for (path, body_framing) in [
            ("/.well-known/agent-card.json", "content-length: 33554432"),
            ("/v1/models", "content-length: 1"),
            ("/a2a/v1/tasks/a-task", "content-length: 1"),
            ("/.well-known/agent-card.json", "transfer-encoding: chunked"),
        ] {
            let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
            let head = format!(
                "GET {path} HTTP/1.1\r\n{}\r\n{body_framing}\r\nconnection: close\r\n\r\n",
                host_header(addr)
            );
            stream
                .write_all(head.as_bytes())
                .await
                .expect("send headers");
            // Never send the declared body: the response must arrive first.
            let mut reader = tokio::io::BufReader::new(stream);
            let mut status = String::new();
            tokio::time::timeout(super::support::WAIT, reader.read_line(&mut status))
                .await
                .expect("early response")
                .expect("read status");
            assert!(status.starts_with("HTTP/1.1 400 "), "{status:?}");
        }
        let card = get(addr, "/.well-known/agent-card.json").await;
        assert_eq!(card.status, 200, "{card:?}");
    })
    .await;
}

/// A route that skips the budgeted body reader still drains a declared
/// body before its response goes out on a closing connection.
#[tokio::test]
async fn bodyless_route_drains_declared_body() {
    let rig = rig(&[]).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let head = format!(
            "POST /missing HTTP/1.1\r\n{}\r\ncontent-length: 100\r\nconnection: close\r\n\r\n",
            host_header(addr)
        );
        let (reader, mut writer) = stream.into_split();
        writer.write_all(head.as_bytes()).await.expect("headers");
        writer.write_all(&[b'x'; 50]).await.expect("first half");
        let mut reader = tokio::io::BufReader::new(reader);
        let mut status = String::new();
        // The 404 must wait for the rest of the declared body: answering
        // early lets the closing socket reset before the client reads it.
        let early = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            reader.read_line(&mut status),
        )
        .await;
        assert!(
            early.is_err(),
            "404 arrived before the body drained: {status:?}"
        );
        writer.write_all(&[b'x'; 50]).await.expect("second half");
        tokio::time::timeout(super::support::WAIT, reader.read_line(&mut status))
            .await
            .expect("404 after the drain")
            .expect("status line");
        assert!(status.starts_with("HTTP/1.1 404 "), "{status:?}");
    })
    .await;
}

/// Incomplete uploads keep their allocation permits until dispatch ends.
/// A second upload cannot grow past the listener-wide memory ceiling.
#[tokio::test]
async fn concurrent_http_bodies_share_a_memory_ceiling() {
    let rig = rig(&[]).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        let head = format!(
            "{CHAT}\r\n{}\r\ncontent-type: application/json\r\ncontent-length: 33554432\r\nconnection: close\r\n\r\n",
            host_header(addr)
        );
        let chunk = vec![b'x'; 1024 * 1024];
        let mut first = tokio::net::TcpStream::connect(addr).await.expect("first upload");
        first.write_all(head.as_bytes()).await.expect("first headers");
        for _ in 0..20 {
            first.write_all(&chunk).await.expect("first body");
        }

        let mut second = tokio::net::TcpStream::connect(addr).await.expect("second upload");
        let (reader, mut writer) = second.split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut status = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(
                async {
                    writer.write_all(head.as_bytes()).await.expect("second headers");
                    for _ in 0..20 {
                        if writer.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                    let _ = writer.shutdown().await;
                },
                async {
                    reader.read_line(&mut status).await.expect("second status");
                }
            );
        })
        .await
        .expect("budget rejects the second upload");
        assert!(status.starts_with("HTTP/1.1 503 "), "{status:?}");
        drop(first);
    })
    .await;
}

#[tokio::test]
async fn bind_refusals() {
    let rig = rig(&[]).await;
    let stop = tokio_util::sync::CancellationToken::new();
    let mut open = router_options(&rig);
    open.bind = "0.0.0.0".to_owned();
    let refused = crate::serve_router(rig.host.clone(), open, stop.clone()).await;
    let Err(error) = refused else {
        panic!("non-loopback bind without public started");
    };
    assert!(matches!(error, ServeError::NotLoopback { .. }), "{error:?}");
    assert_eq!(
        error.to_string(),
        "serve --bind 0.0.0.0 is not a loopback address: it needs a token"
    );
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy a port");
    let mut busy = router_options(&rig);
    busy.port = taken.local_addr().expect("occupied addr").port();
    let occupied = crate::serve_router(rig.host.clone(), busy, stop).await;
    assert!(
        matches!(occupied, Err(ServeError::Bind { .. })),
        "{:?}",
        occupied.err()
    );
}

#[tokio::test]
async fn alias_shadows_mode() {
    let rig = rig(&[]).await;
    let mut options = router_options(&rig);
    options.aliases = BTreeMap::from([("dalgon/normal".into(), "x".into())]);
    let started = crate::serve_router(
        rig.host.clone(),
        options,
        tokio_util::sync::CancellationToken::new(),
    )
    .await;
    let Err(error) = started else {
        panic!("an alias shadowing a harness mode started");
    };
    assert!(
        matches!(&error, ServeError::AliasShadowsMode { alias } if &**alias == "dalgon/normal"),
        "{error:?}"
    );
}

/// A2A never declares the answer capability, so an approval raised in its
/// session is denied at once as a headless denial instead of leaving the task
/// waiting for input that no client can give.
#[tokio::test]
async fn an_a2a_session_denies_an_approval_at_once() {
    let steps = [tool_step("c1", "ask"), text_step(&["done"], 1, 1)];
    let rig = rig(&steps).await;
    let mut options = router_options(&rig);
    options.a2a = true;
    with_serve(&rig, options, async |addr| {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"role":"ROLE_USER","messageId":"m1","parts":[{"text":"edit"}]}}}"#;
        let reply = http(
            addr,
            &format!(
                "POST /a2a HTTP/1.1\r\n{}\r\ncontent-type: application/json\r\na2a-version: 1.0",
                host_header(addr)
            ),
            body,
        )
        .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        let json = reply.json();
        let task = &json["result"]["task"];
        assert_eq!(
            task["status"]["state"].as_str(),
            Some("TASK_STATE_COMPLETED"),
            "{json}"
        );
        let context = task["contextId"].as_str().expect("context id");
        let denial = result_text(&rig, context, "ask", true).await;
        assert_eq!(
            dal_core::parse_headless_denial(&denial).map(|(tool, _)| tool),
            Some("ask"),
            "{denial}"
        );
    })
    .await;
}

#[tokio::test]
async fn an_a2a_session_defaults_an_extension_question_at_once() {
    let steps = [tool_step("c1", "probe_ask"), text_step(&["done"], 1, 1)];
    let rig = rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await;
    let mut options = router_options(&rig);
    options.a2a = true;
    with_serve(&rig, options, async |addr| {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"role":"ROLE_USER","messageId":"m1","parts":[{"text":"ask"}]}}}"#;
        let reply = http(
            addr,
            &format!(
                "POST /a2a HTTP/1.1\r\n{}\r\ncontent-type: application/json\r\na2a-version: 1.0",
                host_header(addr)
            ),
            body,
        )
        .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        let json = reply.json();
        let task = &json["result"]["task"];
        assert_eq!(
            task["status"]["state"].as_str(),
            Some("TASK_STATE_COMPLETED"),
            "{json}"
        );
        let context = task["contextId"].as_str().expect("context id");
        assert_eq!(
            result_text(&rig, context, "probe_ask", false).await,
            "default"
        );
    })
    .await;
}

/// WebSocket keepalive: ping cadence, the pong deadline, and the lifetime of
/// the task that owns the socket. Every test drives a real tungstenite peer.
mod heartbeat {
    use std::time::Duration;

    use dal_agent::SessionRef;
    use dal_core::{ClientId, Command, Expect, Part, Stop, UpdateKind};
    use futures::{SinkExt, StreamExt};
    use sonic_rs::{JsonValueTrait, Value};
    use tokio::io::DuplexStream;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::{Bytes, Message};

    use super::super::support::{Rig, WAIT, gate_step, result, rig, text_step};
    use crate::transport::{ReadFrameError, Transport, WebSocketTransport};

    type Client = WebSocketStream<DuplexStream>;

    /// A server transport over a duplex pipe, paired with a raw tungstenite client.
    async fn pair() -> (WebSocketTransport, Client) {
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let server = WebSocketTransport::server(server_io).await;
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        (server, client)
    }

    /// Reads the next frame the server sent, failing when none arrives within `limit`.
    async fn next_frame(client: &mut Client, limit: Duration) -> Message {
        tokio::time::timeout(limit, client.next())
            .await
            .expect("a frame arrives within the limit")
            .expect("the socket stays open")
            .expect("the frame is well formed")
    }

    /// Sends one request and returns its reply, skipping everything else.
    async fn call(client: &mut Client, id: i64, method: &str, params: Value) -> Value {
        let request =
            sonic_rs::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let text = sonic_rs::to_string(&request).expect("request json");
        client
            .send(Message::Text(text.into()))
            .await
            .expect("the request sends");
        loop {
            if let Message::Text(text) = next_frame(client, WAIT).await {
                let frame: Value = sonic_rs::from_str(text.as_str()).expect("reply json");
                if frame["id"].as_i64() == Some(id) {
                    return frame;
                }
            }
        }
    }

    /// Waits until the session's turn runs, through a short-lived second client.
    async fn await_running(rig: &Rig, session: &str) {
        let agent = rig
            .host
            .open(
                SessionRef::Resume {
                    key: session.into(),
                    workspace: rig.core_workspace(),
                },
                ClientId::new("observer"),
            )
            .await
            .expect("the session opens for observation");
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let head = agent.view(dal_core::PageReq::default()).expect("view");
            if matches!(head.turn, dal_core::TurnState::Running { .. }) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the turn never started: {:?}",
                head.turn
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_pings_every_fifteen_seconds_while_pongs_keep_it_open() {
        let (mut server, mut client) = pair().await;
        let start = tokio::time::Instant::now();
        // Eight rounds span 120 s, far past the 45 s pong timeout, so only the
        // answered pings keep the connection open. The server never reads a
        // frame: the keepalive must not depend on the serve loop polling it.
        for round in 1..=8_u32 {
            let ping = next_frame(&mut client, Duration::from_secs(20)).await;
            assert!(matches!(ping, Message::Ping(_)), "round {round}: {ping:?}");
            let elapsed = start.elapsed();
            let due = Duration::from_secs(15) * round;
            assert!(
                elapsed >= due && elapsed < due + Duration::from_millis(500),
                "round {round}: ping after {elapsed:?}, due at {due:?}"
            );
            client.flush().await.expect("the pong flushes");
            let reply = format!("reply {round}");
            server
                .write_frame(&reply)
                .await
                .expect("a reply writes between pings");
            let frame = next_frame(&mut client, Duration::from_secs(1)).await;
            assert_eq!(frame, Message::Text(reply.into()));
        }
        server.close().await;
        match next_frame(&mut client, Duration::from_secs(1)).await {
            Message::Close(Some(frame)) => assert_eq!(frame.code, CloseCode::Normal),
            other => panic!("a local close sends a normal close frame, got {other:?}"),
        }
        loop {
            match client.next().await {
                None | Some(Err(_)) => break,
                Some(Ok(Message::Ping(_))) => panic!("the keepalive outlived the close"),
                Some(Ok(_)) => {}
            }
        }
    }

    /// Serves a peer that answers only its first `answered` pings and sends a
    /// text message every five seconds the whole time, which the server echoes.
    /// Returns the echo count and when the server saw the connection close.
    async fn silent_peer_close(answered: usize) -> (u32, Duration) {
        let (mut server, mut client) = pair().await;
        let start = tokio::time::Instant::now();
        let serving = async {
            let mut echoed = 0_u32;
            loop {
                match server.read_frame().await {
                    Ok(frame) => {
                        server.write_frame(&frame).await.expect("the echo writes");
                        echoed += 1;
                    }
                    Err(error) => {
                        assert_eq!(error, ReadFrameError::Closed);
                        return (echoed, start.elapsed());
                    }
                }
            }
        };
        let chatting = async {
            for _ in 0..answered {
                // Reading is what makes the client pong; skip everything but the ping.
                while !matches!(
                    next_frame(&mut client, Duration::from_secs(20)).await,
                    Message::Ping(_)
                ) {}
                client.flush().await.expect("the pong flushes");
            }
            loop {
                let _ = client.send(Message::Text("tick".into())).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(300), async {
            tokio::select! {
                outcome = serving => outcome,
                _ = chatting => panic!("the silent peer only ends with the connection"),
            }
        })
        .await
        .expect("the silent peer is closed within 300 s")
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_without_pongs_closes_at_forty_five_seconds_despite_traffic() {
        let (echoed, elapsed) = silent_peer_close(0).await;
        assert!(
            elapsed >= Duration::from_secs(45) && elapsed < Duration::from_millis(45_500),
            "closed after {elapsed:?}"
        );
        assert!(
            echoed >= 8,
            "messages kept flowing both ways until the close: {echoed}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_pong_moves_the_deadline_one_full_window() {
        let (echoed, elapsed) = silent_peer_close(1).await;
        assert!(
            elapsed >= Duration::from_secs(60) && elapsed < Duration::from_millis(60_500),
            "closed after {elapsed:?}"
        );
        assert!(
            echoed >= 8,
            "messages kept flowing both ways until the close: {echoed}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_drop_stops_the_keepalive_task() {
        let (server, mut client) = pair().await;
        drop(server);
        // A detached driver would keep pinging until its own deadline.
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                match client.next().await {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Ping(_))) => panic!("a dropped transport kept pinging"),
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
        .expect("the socket ends once the transport is dropped");
    }

    #[tokio::test]
    async fn websocket_answers_client_pings_without_a_reader() {
        let (_server, mut client) = pair().await;
        client
            .send(Message::Ping(Bytes::from_static(b"alive")))
            .await
            .expect("the ping sends");
        assert_eq!(
            next_frame(&mut client, WAIT).await,
            Message::Pong(Bytes::from_static(b"alive"))
        );
    }

    /// A transport whose frame reader never ran: the peer sent one text frame
    /// and then `ending`, so the one-slot queue holds the text and the driver
    /// holds the end of the connection. Returns once the peer has seen the
    /// server's close frame, which proves the driver took both.
    async fn blocked_handoff(ending: Message) -> (WebSocketTransport, Client) {
        let (server, mut client) = pair().await;
        client
            .send(Message::Text("queued".into()))
            .await
            .expect("the text sends");
        client.send(ending).await.expect("the ending sends");
        while !matches!(next_frame(&mut client, WAIT).await, Message::Close(_)) {}
        (server, client)
    }

    /// Closes a transport in the state [`blocked_handoff`] builds and returns
    /// the virtual time the close took.
    async fn close_with_blocked_handoff(ending: Message) -> Duration {
        let (mut server, _client) = blocked_handoff(ending).await;
        let start = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(120), server.close())
            .await
            .expect("close returns");
        let elapsed = start.elapsed();
        assert_eq!(server.read_frame().await, Err(ReadFrameError::Closed));
        elapsed
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_close_does_not_wait_for_a_blocked_peer_close_handoff() {
        let elapsed = close_with_blocked_handoff(Message::Close(None)).await;
        assert!(
            elapsed < Duration::from_secs(1),
            "close took {elapsed:?} with the peer's close held for the reader"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_close_does_not_wait_for_a_blocked_binary_end_handoff() {
        let elapsed = close_with_blocked_handoff(Message::Binary(Bytes::from_static(b"\0"))).await;
        assert!(
            elapsed < Duration::from_secs(1),
            "close took {elapsed:?} with the binary end held for the reader"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_write_fails_without_waiting_for_a_blocked_handoff() {
        let (mut server, _client) = blocked_handoff(Message::Close(None)).await;
        let start = tokio::time::Instant::now();
        let written = tokio::time::timeout(Duration::from_secs(120), server.write_frame("late"))
            .await
            .expect("the write ends");
        assert!(written.is_err(), "a write after the peer closed succeeded");
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "the write took {elapsed:?} with the peer's close held for the reader"
        );
    }

    #[tokio::test]
    async fn heartbeat_close_leaves_the_session_turn_running() {
        let rig = rig(&[gate_step("c1"), text_step(&["after"], 1, 1)]).await;
        let (server, mut client) = pair().await;
        let serving = crate::serve_rpc(rig.host.clone(), Transport::websocket(server));
        let script = async {
            let init = call(
                &mut client,
                0,
                "initialize",
                sonic_rs::json!({
                    "protocolVersion": 1,
                    "clientInfo": {"name": "test", "version": "1"},
                    "capabilities": ["sessions"],
                }),
            )
            .await;
            result(&init);
            let opened = call(
                &mut client,
                1,
                "session/open",
                sonic_rs::json!({"ref": {"type": "new", "workspace": rig.ws()}}),
            )
            .await;
            let session = result(&opened)["sessionId"]
                .as_str()
                .expect("session id")
                .to_owned();
            let command = sonic_rs::to_value(&Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "wait".into(),
                }],
            })
            .expect("command json");
            let accepted = call(
                &mut client,
                2,
                "session/submit",
                sonic_rs::json!({"sessionId": session, "command": command}),
            )
            .await;
            assert_eq!(result(&accepted)["type"].as_str(), Some("accepted"));
            await_running(&rig, &session).await;
            // The peer goes silent: it reads nothing and answers no ping.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(46)).await;
            tokio::time::resume();
            session
        };
        let (outcome, session) = Box::pin(tokio::time::timeout(Duration::from_secs(120), async {
            tokio::join!(serving, script)
        }))
        .await
        .expect("the heartbeat closes the connection within the test deadline");
        outcome.expect("the heartbeat close ends the connection cleanly");
        tokio::time::timeout(WAIT, async {
            loop {
                match client.next().await {
                    None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
        .expect("the socket ends after the heartbeat close");

        let agent = rig
            .host
            .open(
                SessionRef::Resume {
                    key: session.as_str().into(),
                    workspace: rig.core_workspace(),
                },
                ClientId::new("observer"),
            )
            .await
            .expect("the session stays open after the heartbeat close");
        let head = agent.view(dal_core::PageReq::default()).expect("view");
        assert!(
            matches!(head.turn, dal_core::TurnState::Running { .. }),
            "the heartbeat close cancelled the turn: {:?}",
            head.turn
        );
        let mut subscription = agent
            .subscribe(Some((head.r#gen, head.seq)))
            .expect("subscribe");
        rig.gate.add_permits(1);
        let stop = loop {
            let delivery = tokio::time::timeout(WAIT, subscription.next())
                .await
                .expect("update in time")
                .expect("subscription open");
            if let dal_agent::Delivery::Update(update) = delivery
                && let UpdateKind::TurnEnded { stop, .. } = update.kind
            {
                break stop;
            }
        };
        assert!(matches!(stop, Stop::EndTurn), "{stop:?}");
    }
}
