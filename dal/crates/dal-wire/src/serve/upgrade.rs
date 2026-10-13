//! WebSocket upgrades for `/v1/ws` and `/codex/ws`.

use std::sync::Arc;

use hyper::{Request, Response, StatusCode};

use super::guard::ws_upgrade_denial;
use super::{Driver, Resp, ServeBody, ServeCtx, into_response};
use crate::token::SecretToken;

/// Serves a WebSocket upgrade: validates, answers 101, and drives the
/// protocol in a stream task owned by the accept loop.
pub(super) async fn upgrade_response(
    ctx: Arc<ServeCtx>,
    req: Request<hyper::body::Incoming>,
    route: crate::router::ServeRoute,
) -> Response<ServeBody> {
    let header = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    if !header("upgrade").is_some_and(|value| value.eq_ignore_ascii_case("websocket")) {
        return into_response(Resp::text(426, "upgrade required"));
    }
    let Some(key) = header("sec-websocket-key") else {
        return into_response(Resp::text(400, "missing Sec-WebSocket-Key"));
    };
    let protocols = header("sec-websocket-protocol").unwrap_or_default();
    let offered: Vec<String> = protocols
        .split(',')
        .map(|protocol| protocol.trim().to_owned())
        .collect();
    let allowed: Vec<hyper::header::HeaderValue> = ctx
        .cfg
        .origins
        .iter()
        .filter_map(|name| name.parse().ok())
        .collect();
    if let Some(denial) =
        ws_upgrade_denial(ctx.cfg.token.as_ref(), &allowed, req.headers(), &offered)
    {
        return into_response(denial);
    }
    let subprotocol = offered
        .iter()
        .find(|protocol| protocol.as_str() == "dal.v1")
        .cloned();
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
    let headers = req.headers().clone();
    let token = ctx.cfg.token.clone();
    let on_upgrade = hyper::upgrade::on(req);
    let driver: Driver = match route {
        crate::router::ServeRoute::WebSocket => Box::pin(ws_driver(
            Arc::clone(&ctx),
            on_upgrade,
            token,
            allowed,
            headers,
            offered,
        )),
        _ => Box::pin(codex_driver(
            Arc::clone(&ctx),
            on_upgrade,
            token,
            allowed,
            headers,
            offered,
        )),
    };
    ctx.drive(driver).await;
    let mut builder = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-accept", accept);
    if let Some(protocol) = subprotocol {
        builder = builder.header("sec-websocket-protocol", protocol);
    }
    builder
        .body(ServeBody::Full(None))
        .unwrap_or_else(|_| Response::new(ServeBody::Full(None)))
}

/// Drives one `/v1/ws` connection after its upgrade completes.
pub(super) async fn ws_driver(
    ctx: Arc<ServeCtx>,
    on_upgrade: hyper::upgrade::OnUpgrade,
    token: Option<SecretToken>,
    allowed: Vec<hyper::header::HeaderValue>,
    headers: hyper::header::HeaderMap,
    offered: Vec<String>,
) {
    let upgraded = match on_upgrade.await {
        Ok(upgraded) => upgraded,
        Err(error) => {
            tracing::debug!(%error, "websocket upgrade failed");
            return;
        }
    };
    if ws_upgrade_denial(token.as_ref(), &allowed, &headers, &offered).is_some() {
        tracing::debug!("websocket upgrade denied after handshake");
        return;
    }
    if let Err(error) = crate::transport::serve_websocket(
        ctx.host.clone(),
        upgraded,
        token,
        &allowed,
        ctx.shutdown.clone(),
    )
    .await
    {
        tracing::debug!(%error, "websocket connection ended");
    }
}

/// Drives one `/codex/ws` connection after its upgrade completes.
pub(super) async fn codex_driver(
    ctx: Arc<ServeCtx>,
    on_upgrade: hyper::upgrade::OnUpgrade,
    token: Option<SecretToken>,
    allowed: Vec<hyper::header::HeaderValue>,
    headers: hyper::header::HeaderMap,
    offered: Vec<String>,
) {
    let upgraded = match on_upgrade.await {
        Ok(upgraded) => upgraded,
        Err(error) => {
            tracing::debug!(%error, "codex upgrade failed");
            return;
        }
    };
    if ws_upgrade_denial(token.as_ref(), &allowed, &headers, &offered).is_some() {
        tracing::debug!("codex upgrade denied after handshake");
        return;
    }
    let transport = crate::transport::Transport::websocket(
        crate::transport::WebSocketTransport::accept(upgraded).await,
    );
    if let Err(error) = crate::codex::serve_codex(ctx.host.clone(), transport).await {
        tracing::debug!(%error, "codex connection ended");
    }
}
