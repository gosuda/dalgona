use std::collections::BTreeMap;
use std::net::SocketAddr;

use sonic_rs::JsonValueTrait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::support::{HttpReply, host_header, http, parse_reply, rig, router_options, with_serve};
use crate::error::ServeError;
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
