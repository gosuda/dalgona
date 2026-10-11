//! Browser, host, token, and content-type guards run before route handlers.

use std::sync::Arc;

use hyper::Request;
use hyper::header::{HeaderMap, HeaderValue};

use super::{Resp, ServeCtx};
use crate::token::SecretToken;

/// Returns true for the WebSocket upgrade path.
pub(super) fn is_upgrade(req: &Request<hyper::body::Incoming>) -> bool {
    req.headers()
        .get("upgrade")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

/// Returns true for the unauthenticated agent card route.
pub(super) fn is_card(method: &str, path: &str) -> bool {
    method == "GET" && path == "/.well-known/agent-card.json"
}

/// Enforces the loopback Host header check.
pub(super) fn host_guard(
    ctx: &Arc<ServeCtx>,
    req: &Request<hyper::body::Incoming>,
) -> Result<(), Resp> {
    let port = ctx.cfg.port;
    let host = req
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let allowed = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    if allowed.iter().any(|name| name == host) {
        Ok(())
    } else {
        Err(Resp::text(
            403,
            &format!("Host header \"{host}\" is not a loopback name"),
        ))
    }
}

/// Enforces public token authentication.
pub(super) fn auth_guard(
    ctx: &Arc<ServeCtx>,
    req: &Request<hyper::body::Incoming>,
) -> Result<(), Resp> {
    let Some(token) = ctx.cfg.token.as_ref() else {
        return Err(Resp::router_error(
            401,
            "missing_token",
            "dalgon serve needs a bearer token: send Authorization: Bearer <token from serve.token>",
        ));
    };
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    let api_key = req
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !bearer.is_empty() && token.matches(bearer) {
        return Ok(());
    }
    if !api_key.is_empty() && token.matches(api_key) {
        return Ok(());
    }
    if bearer.is_empty() && api_key.is_empty() {
        return Err(Resp::router_error(
            401,
            "missing_token",
            "dalgon serve needs a bearer token: send Authorization: Bearer <token from serve.token>",
        ));
    }
    Err(Resp::router_error(
        401,
        "invalid_token",
        "the bearer token does not match serve.token",
    ))
}

/// Returns true for routes that require a JSON body.
pub(super) fn requires_json(route: &crate::router::ServeRoute) -> bool {
    !matches!(route, crate::router::ServeRoute::NotFound { .. })
}

/// Enforces the JSON content type for POST routes.
pub(super) fn content_guard(
    req: &Request<hyper::body::Incoming>,
    route: &crate::router::ServeRoute,
) -> Result<(), Resp> {
    if req.method() == hyper::Method::GET {
        return Ok(());
    }
    let content = req
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let main = content.split(';').next().unwrap_or("").trim();
    let allowed = main == "application/json"
        || (main == "application/a2a+json"
            && matches!(route, crate::router::ServeRoute::A2aRest(_, _)));
    if allowed {
        Ok(())
    } else {
        Err(Resp::text(415, "Content-Type must be application/json"))
    }
}

/// Rejects a WebSocket upgrade with a missing origin or token.
///
/// Any present `Origin` header must match the allowlist: an empty allowlist
/// denies every origin while a missing header passes. A configured serve
/// token must appear as `Authorization: Bearer`, `x-api-key`, or the
/// `dal.bearer.<token>` subprotocol. Presented values are digest-compared
/// and never logged. Returns the denial response.
pub(super) fn ws_upgrade_denial(
    token: Option<&SecretToken>,
    allowed: &[HeaderValue],
    headers: &HeaderMap,
    offered: &[String],
) -> Option<Resp> {
    if let Some(origin) = headers.get("origin")
        && !allowed.iter().any(|name| name == origin)
    {
        return Some(Resp::text(403, "browser requests are not allowed"));
    }
    let expected = token?;
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if !bearer.is_empty() && expected.matches(bearer) {
        return None;
    }
    let api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !api_key.is_empty() && expected.matches(api_key) {
        return None;
    }
    if offered
        .iter()
        .filter_map(|protocol| protocol.strip_prefix("dal.bearer."))
        .any(|candidate| !candidate.is_empty() && expected.matches(candidate))
    {
        return None;
    }
    if bearer.is_empty() && api_key.is_empty() {
        return Some(Resp::router_error(
            401,
            "missing_token",
            "dalgon serve needs a bearer token: send Authorization: Bearer <token from serve.token>",
        ));
    }
    Some(Resp::router_error(
        401,
        "invalid_token",
        "the bearer token does not match serve.token",
    ))
}
