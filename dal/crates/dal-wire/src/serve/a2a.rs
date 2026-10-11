//! A2A 1.0.0 route handlers over the serve listener.
//!
//! Context is a session and task is a turn. `PascalCase` JSON-RPC lives only
//! at `POST /a2a`; HTTP+JSON lives only at `/a2a/v1`. Every call needs
//! `A2A-Version: 1.0` in the header or query.

use std::sync::Arc;

use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};

use super::{Resp, ServeCtx};
use crate::router::A2aRestKind;

pub(crate) mod error;
pub(crate) mod events;
pub(crate) mod follow;
pub(crate) mod parts;
pub(crate) mod table;
pub(crate) mod task;

pub(crate) use table::A2aState;

use error::Fail;
use table::Framing;
use task::Outcome;

/// Methods that exist in A2A 1.0 but that dalgon does not support.
const UNSUPPORTED: [&str; 8] = [
    "ListTasks",
    "SubscribeToTask",
    "CreateTaskPushNotificationConfig",
    "GetTaskPushNotificationConfig",
    "ListTaskPushNotificationConfig",
    "ListTaskPushNotificationConfigs",
    "DeleteTaskPushNotificationConfig",
    "GetExtendedAgentCard",
];

/// Serves `GET /.well-known/agent-card.json`.
pub(crate) fn card(ctx: &Arc<ServeCtx>, host: Option<&str>) -> Resp {
    let base = card_base(host, ctx.cfg.bind, ctx.cfg.port);
    let mut document = sonic_rs::json!({
        "name": "dal",
        "description": "dal, a coding agent. It runs in the directory where dalgon serve started.",
        "version": crate::rpc::crate_version(),
        "supportedInterfaces": [
            {"url": format!("{base}/a2a"), "protocolBinding": "JSONRPC", "protocolVersion": "1.0"},
            {"url": format!("{base}/a2a/v1"), "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0"},
        ],
        "capabilities": {"streaming": true, "pushNotifications": false, "extendedAgentCard": false},
        "defaultInputModes": ["text/plain", "image/png", "image/jpeg", "image/gif", "image/webp"],
        "defaultOutputModes": ["text/plain"],
        "skills": [
            {"id": "normal", "name": "normal", "description": "The model gets read, search, patch, and exec.", "tags": ["coding"]},
            {"id": "eval-first", "name": "eval-first", "description": "Normal tools with eval listed first.", "tags": ["coding"]},
            {"id": "eval-only", "name": "eval-only", "description": "Only the eval tool is visible.", "tags": ["coding"]},
        ],
    });
    if ctx.cfg.public
        && let Some(object) = document.as_object_mut()
    {
        object.insert(
            "securitySchemes",
            sonic_rs::json!({"bearer": {"httpAuthSecurityScheme": {"scheme": "Bearer"}}}),
        );
        object.insert(
            "securityRequirements",
            sonic_rs::json!([{"schemes": {"bearer": []}}]),
        );
    }
    let mut resp = Resp::json(200, &document);
    resp.headers
        .push(("cache-control".to_owned(), "max-age=300".to_owned()));
    resp
}

/// Returns the card base URL: the Host header, else the bind, plus the port.
pub(crate) fn card_base(host: Option<&str>, bind: std::net::IpAddr, port: u16) -> String {
    match host.map(str::trim).filter(|host| !host.is_empty()) {
        Some(host) if has_port(host) => format!("http://{host}"),
        Some(host) => format!("http://{host}:{port}"),
        None => format!("http://{}", std::net::SocketAddr::new(bind, port)),
    }
}

/// Returns true when a Host header value already names its port.
fn has_port(host: &str) -> bool {
    host.rsplit_once(':').is_some_and(|(name, port)| {
        !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && (!name.starts_with('[') || name.ends_with(']'))
    })
}

/// Serves `POST /a2a`: `PascalCase` JSON-RPC only.
pub(crate) async fn json_rpc(
    ctx: &Arc<ServeCtx>,
    body: &[u8],
    query: &str,
    version: Option<&str>,
) -> Resp {
    let Ok(text) = std::str::from_utf8(body) else {
        return rpc_fail(
            &Value::default(),
            &Fail::new("PARSE_ERROR", "frame is not valid UTF-8"),
        );
    };
    let (id, method, params) = match crate::jsonrpc::decode_jsonrpc(text) {
        Ok(crate::jsonrpc::Message::Request { id, method, params }) => {
            (id_value(&id), method, params)
        }
        Ok(_) => {
            return rpc_fail(
                &Value::default(),
                &Fail::new("INVALID_REQUEST", "A2A requests need an id"),
            );
        }
        Err(error) => {
            let reason = if error.code == -32700 {
                "PARSE_ERROR"
            } else {
                "INVALID_REQUEST"
            };
            return rpc_fail(&id_value(&error.id), &Fail::new(reason, error.message));
        }
    };
    if !error::version_ok(version, query) {
        return rpc_fail(&id, &Fail::version());
    }
    let streaming = (method == "SendStreamingMessage").then(|| Framing::JsonRpc(id.clone()));
    match dispatch_method(ctx, &method, &params, streaming).await {
        Ok(Outcome::Value(result)) => Resp::json(
            200,
            &sonic_rs::json!({"jsonrpc": "2.0", "id": id, "result": result}),
        ),
        Ok(Outcome::Stream(receiver)) => Resp::stream(receiver, "text/event-stream"),
        Err(fail) => rpc_fail(&id, &fail),
    }
}

/// Builds one JSON-RPC error response; JSON-RPC errors travel with HTTP 200.
fn rpc_fail(id: &Value, fail: &Fail) -> Resp {
    Resp::json(200, &fail.rpc_envelope(id))
}

/// Converts one JSON-RPC id into its JSON value.
fn id_value(id: &crate::jsonrpc::Id) -> Value {
    match id {
        crate::jsonrpc::Id::Integer(number) => Value::from(*number),
        crate::jsonrpc::Id::String(text) => Value::from(text.as_str()),
        crate::jsonrpc::Id::Null => Value::default(),
    }
}

/// Dispatches one JSON-RPC method.
async fn dispatch_method(
    ctx: &Arc<ServeCtx>,
    method: &str,
    params: &Value,
    streaming: Option<Framing>,
) -> Result<Outcome, Fail> {
    match method {
        "SendMessage" | "SendStreamingMessage" => task::send_message(ctx, params, streaming).await,
        "GetTask" => task::get_task(ctx, params).await,
        "CancelTask" => task::cancel_task(ctx, params).await,
        _ if UNSUPPORTED.contains(&method) => Err(Fail::new(
            "UNSUPPORTED_OPERATION",
            format!("dalgon does not support {method}"),
        )),
        _ => Err(Fail::new(
            "METHOD_NOT_FOUND",
            format!("unknown method \"{method}\""),
        )),
    }
}

/// Serves one A2A HTTP+JSON route.
pub(crate) async fn rest(
    ctx: &Arc<ServeCtx>,
    kind: A2aRestKind,
    id: Option<String>,
    body: &[u8],
    query: &str,
    version: Option<&str>,
) -> Resp {
    if !error::version_ok(version, query) {
        return Fail::version().rest();
    }
    let outcome = match kind {
        A2aRestKind::Send | A2aRestKind::Stream => match rest_body(body) {
            Ok(params) => {
                let streaming = (kind == A2aRestKind::Stream).then_some(Framing::Rest);
                task::send_message(ctx, &params, streaming).await
            }
            Err(fail) => Err(fail),
        },
        A2aRestKind::GetTask => {
            let params = sonic_rs::json!({"id": id.unwrap_or_default()});
            task::get_task(ctx, &params).await
        }
        A2aRestKind::CancelTask => {
            let params = sonic_rs::json!({"id": id.unwrap_or_default()});
            task::cancel_task(ctx, &params).await
        }
    };
    match outcome {
        Ok(Outcome::Value(result)) => Resp::json(200, &result),
        Ok(Outcome::Stream(receiver)) => Resp::stream(receiver, "text/event-stream"),
        Err(fail) => fail.rest(),
    }
}

/// Decodes one HTTP+JSON request body; an empty body is `{}`.
fn rest_body(body: &[u8]) -> Result<Value, Fail> {
    let text = std::str::from_utf8(body).map_err(|_| Fail::invalid("request body is not UTF-8"))?;
    if text.trim().is_empty() {
        return Ok(sonic_rs::json!({}));
    }
    let value: Value = sonic_rs::from_str(text)
        .map_err(|error| Fail::invalid(format!("request body is not valid JSON: {error}")))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(Fail::invalid("request body must be a JSON object"))
    }
}
