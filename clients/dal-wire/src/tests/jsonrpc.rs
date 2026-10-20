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
    /// Every frame is a typed outcome: malformed JSON is `-32700`, an invalid
    /// envelope is `-32600` with a null id, and any decoded message re-encodes
    /// to a frame that decodes again.
    #[test]
    fn decode_reports_typed_errors_or_round_trips(frame in ".*{0,512}") {
        match decode_jsonrpc(&frame) {
            Err(error) => {
                prop_assert!([-32700, -32600].contains(&error.code), "unexpected code {}", error.code);
                prop_assert_eq!(error.id, Id::Null);
            }
            Ok(message) => {
                let encoded = encode_jsonrpc(&message).expect("decoded message encodes");
                let back = decode_jsonrpc(&encoded).expect("encoded message decodes");
                prop_assert_eq!(encode_jsonrpc(&back).expect("re-encodes"), encoded);
            }
        }
    }

    /// Near-miss envelopes fail with `-32600` and a null id: a wrong version,
    /// a missing method/result/error member, and batches of any shape. A bare
    /// method is still a notification, and a well-formed request still decodes.
    #[test]
    fn decode_rejects_near_miss_envelopes_with_typed_codes(
        tag in "[ -~]{0,24}",
        n in any::<i64>(),
        tail in prop_oneof![
            Just(sonic_rs::json!({})),
            Just(sonic_rs::json!([])),
            any::<i64>().prop_map(Into::into),
        ],
    ) {
        let invalid = [
            sonic_rs::to_string(&sonic_rs::json!({"jsonrpc": "9.9", "id": n, "result": tail})).expect("escapes"),
            sonic_rs::to_string(&sonic_rs::json!({"jsonrpc": "2.0", "id": n})).expect("escapes"),
            sonic_rs::to_string(&sonic_rs::json!({"jsonrpc": "9.9", "id": n, "method": tag})).expect("escapes"),
            sonic_rs::to_string(&sonic_rs::json!([n, {"id": tag}, true])).expect("escapes"),
            sonic_rs::to_string(&sonic_rs::json!({"id": n, "error": {"code": n, "message": tag}})).expect("escapes"),
        ];
        for frame in invalid {
            let error = decode_jsonrpc(&frame).expect_err("near-miss envelope is invalid");
            prop_assert_eq!(error.code, -32600, "wrong code for {}", frame);
            prop_assert_eq!(error.id, Id::Null, "near-miss keeps no id for {}", frame);
        }
        let notification = sonic_rs::to_string(&sonic_rs::json!({"jsonrpc": "2.0", "method": tag})).expect("escapes");
        prop_assert!(matches!(decode_jsonrpc(&notification), Ok(Message::Notification { .. })), "bare method is a notification");
        prop_assert!(decode_jsonrpc(r#"{"jsonrpc":"2.0","id":7,"method":"session/list"}"#).is_ok(), "well-formed request decodes");
    }
}

/// Adversarial edge frames decode to typed errors, never a panic: the random
/// property above rarely draws empty, NUL, or exotic-leading inputs, so the
/// panic class on those inputs is pinned here instead.
#[test]
fn decode_edge_frames_are_typed_errors() {
    for frame in [
        "",
        "ÿ",
        "~",
        "~{\"jsonrpc\":\"2.0\"}",
        "\u{0}",
        "{",
        "[[[[[",
    ] {
        let error = decode_jsonrpc(frame).expect_err("edge frame is invalid");
        assert!(
            [-32700, -32600].contains(&error.code),
            "unexpected code {} for {frame:?}",
            error.code
        );
        assert_eq!(error.id, Id::Null, "edge frame keeps no id: {frame:?}");
    }
}

/// Asserts one frame is refused as an invalid envelope: `-32600`, null id.
fn assert_invalid_envelope(frame: &str) {
    let error = decode_jsonrpc(frame).expect_err("the envelope must be refused");
    assert_eq!(error.code, -32600, "wrong code for {frame}");
    assert_eq!(error.id, Id::Null, "refusal keeps no id for {frame}");
}

/// A response is exactly one of `result` or `error`, and a request is neither:
/// a frame that is two of them is ambiguous about what the peer meant, so it
/// is refused instead of being read as whichever member the decoder met first.
#[test]
fn decode_refuses_frames_that_are_two_message_kinds_at_once() {
    for frame in [
        r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-1,"message":"m"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"m","result":{}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"m","error":{"code":-1,"message":"m"}}"#,
        r#"{"jsonrpc":"2.0","method":"m","result":null}"#,
    ] {
        assert_invalid_envelope(frame);
    }
}

/// A response answers a request, so it must carry the id it answers.
#[test]
fn decode_refuses_responses_without_an_id() {
    assert_invalid_envelope(r#"{"jsonrpc":"2.0","result":{}}"#);
    assert_invalid_envelope(r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"m"}}"#);
}

/// `result: null` is a successful response whose value is null; it is not a
/// missing `result`. A present-but-null member must not be read as absent.
#[test]
fn decode_keeps_a_null_result_as_a_successful_response() {
    let message = decode_jsonrpc(r#"{"jsonrpc":"2.0","id":"r","result":null}"#)
        .expect("a null result is a valid response");
    let Message::Result { id, result } = message else {
        panic!("a result frame must decode as a result");
    };
    assert_eq!(id, Id::String("r".to_owned()));
    assert!(result.is_null(), "the null result is kept, got {result}");
}

/// The error object is validated member by member: a code that does not fit
/// `i32` is not truncated into a different code, and a message that is not a
/// string is not replaced by an empty one.
#[test]
fn decode_error_objects_reject_what_cannot_be_represented_exactly() {
    for (code, accepted) in [
        (i64::from(i32::MAX), true),
        (i64::from(i32::MAX) + 1, false),
        (i64::from(i32::MIN), true),
        (i64::from(i32::MIN) - 1, false),
        // 2^32 truncates to 0 and 2^32 + 5 to 5 if the cast is unchecked.
        (i64::from(u32::MAX) + 1, false),
        (i64::from(u32::MAX) + 6, false),
    ] {
        let frame =
            format!(r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":{code},"message":"m"}}}}"#);
        match decode_jsonrpc(&frame) {
            Ok(Message::Error { error, .. }) => {
                assert!(accepted, "code {code} was accepted as {}", error.code);
                assert_eq!(i64::from(error.code), code);
            }
            Ok(other) => panic!("code {code} decoded as {other:?}"),
            Err(error) => {
                assert!(!accepted, "code {code} was refused: {error:?}");
                assert_eq!(error.code, -32600);
            }
        }
    }
    for error_member in [
        r#"{"code":-1}"#,
        r#"{"message":"m"}"#,
        r#"{"code":-1,"message":5}"#,
        r#"{"code":-1,"message":null}"#,
        r#"{"code":"-1","message":"m"}"#,
        r#"{"code":-1.5,"message":"m"}"#,
        r#""boom""#,
        r#"[-1,"m"]"#,
        "null",
    ] {
        assert_invalid_envelope(&format!(
            r#"{{"jsonrpc":"2.0","id":1,"error":{error_member}}}"#
        ));
    }
}

/// Optional `data` survives decoding untouched, whatever JSON it holds.
#[test]
fn decode_error_objects_keep_their_data() {
    let message = decode_jsonrpc(
        r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"m","data":{"k":[1,null]}}}"#,
    )
    .expect("an error response with data is valid");
    let Message::Error { id, error } = message else {
        panic!("an error frame must decode as an error");
    };
    assert_eq!(id, Id::Integer(7));
    assert_eq!((error.code, error.message.as_str()), (-32000, "m"));
    assert_eq!(
        error.data,
        Some(sonic_rs::json!({"k": [1, null]})),
        "the error data is kept verbatim"
    );
}

/// The protocol version is the exact string `2.0`: not a number, and not a
/// near spelling that a lenient comparison would accept.
#[test]
fn decode_requires_the_exact_version_string() {
    for version in ["2.0 ", " 2.0", "2.0\\n", "2.00", "2.", "2", "20", "3.0", ""] {
        assert_invalid_envelope(&format!(r#"{{"jsonrpc":"{version}","id":1,"method":"m"}}"#));
    }
    assert_invalid_envelope(r#"{"jsonrpc":2.0,"id":1,"method":"m"}"#);
    assert_invalid_envelope(r#"{"jsonrpc":null,"id":1,"method":"m"}"#);
    assert_invalid_envelope(r#"{"jsonrpc":["2.0"],"id":1,"method":"m"}"#);
}

/// A request id is an integer that fits `i64`, a string, or null. Everything
/// else is refused; in particular an integer one past `i64::MAX` must not wrap
/// into a negative id that a different request could own.
#[test]
fn decode_ids_stop_exactly_at_the_i64_boundary() {
    for (id, expected) in [
        ("9223372036854775807", Some(Id::Integer(i64::MAX))),
        ("-9223372036854775808", Some(Id::Integer(i64::MIN))),
        ("9223372036854775808", None),
        ("-9223372036854775809", None),
        ("18446744073709551615", None),
        ("true", None),
        ("false", None),
        ("[]", None),
        ("[1]", None),
        ("{}", None),
        ("1e2", None),
    ] {
        let frame = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"m"}}"#);
        match (decode_jsonrpc(&frame), expected) {
            (Ok(Message::Request { id: decoded, .. }), Some(expected)) => {
                assert_eq!(decoded, expected, "id {id}");
            }
            (Err(error), None) => {
                assert_eq!((error.code, &error.id), (-32600, &Id::Null), "id {id}");
            }
            (outcome, expected) => panic!("id {id}: expected {expected:?}, got {outcome:?}"),
        }
    }
}

/// An absent `id` makes a notification, which gets no reply; a null `id` is a
/// request that must be answered, so the two must not be conflated. Absent
/// `params` becomes `{}`, but present params of any JSON type are kept as sent,
/// and unknown envelope members are ignored rather than refused.
#[test]
fn decode_separates_notifications_from_null_id_requests_and_keeps_params() {
    let request = decode_jsonrpc(r#"{"jsonrpc":"2.0","id":null,"method":"m"}"#)
        .expect("a null id is a valid request id");
    assert!(
        matches!(&request, Message::Request { id: Id::Null, method, .. } if method == "m"),
        "a null id is still a request: {request:?}"
    );
    let notification = decode_jsonrpc(r#"{"jsonrpc":"2.0","method":"m"}"#)
        .expect("a frame with no id is a notification");
    assert!(
        matches!(&notification, Message::Notification { method, .. } if method == "m"),
        "an absent id is a notification: {notification:?}"
    );
    for (params, expected) in [
        ("[1,2]", sonic_rs::json!([1, 2])),
        ("5", sonic_rs::json!(5)),
        ("null", sonic_rs::json!(null)),
        ("{}", sonic_rs::json!({})),
    ] {
        let frame = format!(r#"{{"jsonrpc":"2.0","method":"m","params":{params}}}"#);
        let Ok(Message::Notification {
            params: decoded, ..
        }) = decode_jsonrpc(&frame)
        else {
            panic!("a notification with params {params} must decode");
        };
        assert_eq!(decoded, expected, "params {params} are kept as sent");
    }
    let extra = decode_jsonrpc(r#"{"jsonrpc":"2.0","id":3,"method":"m","extra":{"x":1},"z":0}"#)
        .expect("unknown envelope members are ignored");
    assert!(matches!(
        extra,
        Message::Request {
            id: Id::Integer(3),
            ..
        }
    ));
}

/// A method is a string; any other JSON type is not a method name.
#[test]
fn decode_refuses_a_method_that_is_not_a_string() {
    for method in ["5", "null", "true", r#"["m"]"#, r#"{"m":1}"#] {
        assert_invalid_envelope(&format!(r#"{{"jsonrpc":"2.0","id":1,"method":{method}}}"#));
        assert_invalid_envelope(&format!(r#"{{"jsonrpc":"2.0","method":{method}}}"#));
    }
}
