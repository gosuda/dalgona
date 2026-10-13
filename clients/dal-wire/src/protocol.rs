use std::sync::atomic::{AtomicU64, Ordering};

use dal_core::ClientId;
use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};

/// The dal protocol version spoken by this crate.
pub const PROTOCOL_VERSION: u32 = 1;

/// The capability names in their stable order.
pub const CAPABILITIES: [&str; 6] = [
    "sessions",
    "host.updates",
    "blobs",
    "docs",
    "auth",
    "models",
];

static CLIENT_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Mints `<name>#<n>` from the process-wide connection counter.
pub fn mint_client_id(name: &str) -> ClientId {
    let index = CLIENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    ClientId::new(format!("{name}#{index}"))
}

/// Intersects the client capability list with the supported table, preserving order.
#[must_use]
pub fn negotiate_capabilities(client: &[String]) -> Vec<String> {
    CAPABILITIES
        .iter()
        .filter(|capability| client.iter().any(|name| name == **capability))
        .map(|capability| (*capability).to_owned())
        .collect()
}

/// Returns the capability guard error for a method requiring `capability`.
#[must_use]
pub fn capability_error(capability: &str) -> (i32, String) {
    (
        -32007,
        format!(r#"capability "{capability}" is not enabled on this connection"#),
    )
}

/// Builds the draft 2020-12 JSON Schema derived from the shared serde types.
#[must_use]
pub fn protocol_schema() -> Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let command = generator.subschema_for::<dal_core::Command>();
    let update = generator.subschema_for::<dal_core::Update>();
    let view = generator.subschema_for::<dal_core::View>();
    let request = generator.subschema_for::<dal_core::Request>();
    let answer = generator.subschema_for::<dal_core::Answer>();
    let grant = generator.subschema_for::<dal_core::CallGrant>();
    let session_info = generator.subschema_for::<dal_core::SessionInfo>();
    let part = generator.subschema_for::<dal_core::Part>();
    let definitions = generator.take_definitions(true);

    let mut root = sonic_rs::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "dal-protocol",
        "type": "object",
    });
    let Some(object) = root.as_object_mut() else {
        return root;
    };
    let mut defs = sonic_rs::json!({});
    let Some(defs_object) = defs.as_object_mut() else {
        return root;
    };
    for (name, schema) in definitions {
        let text = sonic_rs::to_string(&schema).unwrap_or_else(|_| "true".to_owned());
        let value = sonic_rs::from_str::<Value>(&text).unwrap_or_else(|_| sonic_rs::json!(true));
        defs_object.insert(&name, value);
    }
    for (name, schema) in [
        ("Command", command),
        ("Update", update),
        ("View", view),
        ("Request", request),
        ("Answer", answer),
        ("Grant", grant),
        ("SessionInfo", session_info),
        ("Part", part),
    ] {
        // `subschema_for` already registered the real schema under this
        // name in `definitions`; inserting the returned `$ref` stub would
        // replace a real schema with a self-reference.
        if defs_object.get(&name).is_some() {
            continue;
        }
        let text = sonic_rs::to_string(&schema).unwrap_or_else(|_| "true".to_owned());
        let value = sonic_rs::from_str::<Value>(&text).unwrap_or_else(|_| sonic_rs::json!(true));
        defs_object.insert(name, value);
    }
    object.insert("$defs", defs);
    object.insert(
        "methods",
        sonic_rs::json!([
            "initialize",
            "protocol/schema",
            "session/list",
            "session/open",
            "session/close",
            "session/view",
            "session/subscribe",
            "session/unsubscribe",
            "session/submit",
            "session/answer",
            "blob/read",
            "commands/list",
            "models/list",
            "docs/read",
            "host/subscribe",
            "host/unsubscribe",
            "auth/status",
            "auth/login",
            "auth/cancel",
            "auth/logout"
        ]),
    );
    root
}

/// Encodes the ACP `session/prompt` success line payload.
#[must_use]
pub fn acp_prompt_result(stop_reason: &str) -> String {
    let reason = sonic_rs::to_string(&stop_reason).unwrap_or_else(|_| "\"error\"".to_owned());
    format!("{{\"jsonrpc\":\"2.0\",\"id\":null,\"result\":{{\"stopReason\":{reason}}}}}\n")
}

/// Encodes the ACP `session/prompt` failure line payload.
#[must_use]
pub fn acp_prompt_error(code: i32, what: &str, hint: &str) -> String {
    let what = sonic_rs::to_string(&what).unwrap_or_else(|_| "\"internal error\"".to_owned());
    let hint = sonic_rs::to_string(&hint).unwrap_or_else(|_| "\"report\"".to_owned());
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":{code},\"message\":{what},\"data\":{{\"hint\":{hint}}}}}}}\n"
    )
}

/// Reads an optional `clientInfo.name` member for client attribution.
#[must_use]
pub fn client_name(params: &Value, fallback: &str) -> String {
    params
        .get("clientInfo")
        .and_then(|info| info.get("name"))
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}
#[cfg(test)]
mod tests {
    use sonic_rs::JsonValueTrait;

    use super::{acp_prompt_error, acp_prompt_result, protocol_schema};

    #[test]
    fn acp_prompt_lines_are_single_lf_framed_jsonrpc() {
        let result = acp_prompt_result("end_turn");
        let failure = acp_prompt_error(-32000, "bad \"thing\"", "retry");
        for line in [&result, &failure] {
            assert_eq!(
                line.bytes().filter(|byte| *byte == b'\n').count(),
                1,
                "one line carries one frame: {line:?}"
            );
            let body = line.strip_suffix('\n').expect("frame ends with one LF");
            let value: super::Value = sonic_rs::from_str(body).expect("framed line parses as JSON");
            assert_eq!(value.get("jsonrpc").and_then(|id| id.as_str()), Some("2.0"));
            assert!(
                value
                    .get("id")
                    .is_some_and(sonic_rs::JsonValueTrait::is_null)
            );
        }
        let value: super::Value =
            sonic_rs::from_str(result.strip_suffix('\n').expect("frame ends with one LF"))
                .expect("result line parses");
        assert_eq!(
            value
                .get("result")
                .and_then(|body| body.get("stopReason"))
                .and_then(|reason| reason.as_str()),
            Some("end_turn")
        );
        let value: super::Value =
            sonic_rs::from_str(failure.strip_suffix('\n').expect("frame ends with one LF"))
                .expect("error line parses");
        let error = value.get("error").expect("error object");
        assert_eq!(
            error.get("code").and_then(sonic_rs::JsonValueTrait::as_i64),
            Some(-32000)
        );
        assert_eq!(
            error.get("message").and_then(|message| message.as_str()),
            Some("bad \"thing\"")
        );
        assert_eq!(
            error
                .get("data")
                .and_then(|data| data.get("hint"))
                .and_then(|hint| hint.as_str()),
            Some("retry")
        );
        for hostile in ["a\nb", "x\\y", "\"quoted\"", "ünï\"\\\nend"] {
            let line = acp_prompt_result(hostile);
            assert_eq!(
                line.bytes().filter(|byte| *byte == b'\n').count(),
                1,
                "an escaped payload stays one frame: {line:?}"
            );
            let value: super::Value =
                sonic_rs::from_str(line.strip_suffix('\n').expect("frame ends with one LF"))
                    .expect("hostile line parses");
            assert_eq!(
                value
                    .get("result")
                    .and_then(|body| body.get("stopReason"))
                    .and_then(|reason| reason.as_str()),
                Some(hostile),
                "escaping round-trips the payload"
            );
        }
    }

    #[test]
    fn protocol_schema_names_and_references_resolve() {
        let schema = protocol_schema();
        let defs = &schema["$defs"];
        assert!(defs.is_object(), "$defs must be an object map");
        for name in ["Command", "Update", "View", "Request", "Answer"] {
            let def = &defs[name];
            let text = sonic_rs::to_string(def).expect("def serializes");
            assert!(
                text.contains("\"type\"") || text.contains("\"properties\""),
                "$defs must key {name} as a real object schema for clients generating types, got {text}"
            );
        }
        // Every $ref in the document must resolve into $defs — a renamed
        // schema leaves a dangling reference that clients validate against.
        let text = sonic_rs::to_string(&schema).expect("schema serializes");
        let mut missing = Vec::new();
        for fragment in text.split("\"$ref\":").skip(1) {
            if let Some(reference) = fragment
                .trim_start()
                .strip_prefix('"')
                .and_then(|rest| rest.split('"').next())
                && let Some(key) = reference.strip_prefix("#/$defs/")
                && !defs[key].is_object()
            {
                missing.push(reference.to_owned());
            }
        }
        assert!(missing.is_empty(), "dangling $refs: {missing:?}");
    }
}
