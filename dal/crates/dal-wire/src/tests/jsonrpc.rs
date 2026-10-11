use proptest::prelude::*;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use crate::jsonrpc::{ErrorObject, Id, Message, decode_jsonrpc, encode_jsonrpc};

fn ids() -> BoxedStrategy<Id> {
    prop_oneof![
        any::<i64>().prop_map(Id::Integer),
        "[ -~]{0,32}".prop_map(Id::String),
        Just(Id::Null),
    ]
    .boxed()
}

proptest! {
    #[test]
    fn encode_decode_roundtrip(id in ids(), method in "[a-zA-Z./_-]{1,24}", n in any::<i64>(), text in "[a-zA-Z0-9 ]{0,40}") {
        let params = sonic_rs::json!({
            "count": n,
            "text": text,
            "nested": [true, null, {"key": "value"}],
        });
        let message = Message::Request {
            id: id.clone(),
            method: method.clone(),
            params: params.clone(),
        };
        let encoded = encode_jsonrpc(&message).expect("message is JSON encodable");
        let independently_parsed = sonic_rs::from_str::<Value>(&encoded).expect("encoded message is JSON");
        let expected = sonic_rs::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        prop_assert_eq!(independently_parsed, expected);
        let decoded = decode_jsonrpc(&encoded).expect("encoded message is a valid JSON-RPC request");
        prop_assert_eq!(encode_jsonrpc(&decoded).expect("decoded message is JSON encodable"), encoded);
    }
}

#[test]
fn absent_params_decode_as_empty_object() {
    let message = decode_jsonrpc(r#"{"jsonrpc":"2.0","id":7,"method":"session/list"}"#)
        .expect("request without params is valid");
    let Message::Request { params, .. } = message else {
        panic!("request frame must decode as a request");
    };
    assert!(params.is_object() && params.as_object().is_some_and(sonic_rs::Object::is_empty));
}

#[test]
fn empty_batches_and_float_ids_have_invalid_request_errors() {
    let batch = decode_jsonrpc("[]").expect_err("empty batches are invalid");
    assert_eq!(
        (batch.code, batch.message.as_str()),
        (-32600, "batch is empty")
    );

    let float_id = decode_jsonrpc(r#"{"jsonrpc":"2.0","id":1.25,"method":"x"}"#)
        .expect_err("fractional identifiers are invalid");
    assert_eq!(float_id.code, -32600);
}

#[test]
fn malformed_json_has_a_parse_error() {
    let error = decode_jsonrpc("nope").expect_err("invalid JSON must be rejected");
    assert_eq!(error.code, -32700);
    assert!(error.message.starts_with("frame is not valid JSON:"));
}

#[test]
fn result_encoding_uses_a_standard_json_rpc_envelope() {
    let message = Message::Result {
        id: Id::Integer(1),
        result: sonic_rs::json!({}),
    };
    assert_eq!(
        encode_jsonrpc(&message).expect("response is JSON encodable"),
        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
    );
}

#[test]
fn error_encoding_omits_absent_data() {
    let message = Message::Error {
        id: Id::Null,
        error: ErrorObject {
            code: -32601,
            message: "unknown method \"x\"".to_owned(),
            data: None,
        },
    };
    let encoded = encode_jsonrpc(&message).expect("error response is JSON encodable");
    let value = sonic_rs::from_str::<Value>(&encoded).expect("error response is JSON");
    assert!(
        value
            .get("error")
            .is_some_and(|error| error.get("data").is_none())
    );
}

proptest! {
    /// The frame decoder must never panic on arbitrary input: malformed frames
    /// surface a typed `JsonRpcError`, never a crash.
    #[test]
    fn decode_never_panics_on_arbitrary_frames(frame in ".*{0,512}") {
        let _ = decode_jsonrpc(&frame);
    }

    /// Structurally valid JSON that is not a well-formed request or response
    /// must decode or fail typed — never panic.
    #[test]
    fn decode_near_miss_json_values(
        tag in "[ -~]{0,24}",
        n in any::<i64>(),
        tail in prop_oneof![
            Just(sonic_rs::json!({})),
            Just(sonic_rs::json!([])),
            any::<i64>().prop_map(Into::into),
        ],
    ) {
        let frames = [
            sonic_rs::to_string(&sonic_rs::json!({"jsonrpc": "2.0", "method": tag})).expect("escapes"),
            sonic_rs::to_string(&sonic_rs::json!({"jsonrpc": "9.9", "id": n, "result": tail})).expect("escapes"),
            sonic_rs::to_string(&sonic_rs::json!([n, {"id": tag}, true])).expect("escapes"),
            sonic_rs::to_string(&sonic_rs::json!({"id": n, "error": {"code": n, "message": tag}})).expect("escapes"),
        ];
        for frame in frames {
            let _ = decode_jsonrpc(&frame);
        }
    }
}
