//! Session identifier shape, parsing, and ordering.

use dal_core::SessionId;

fn shape(text: &str) {
    assert_eq!(text.len(), 36, "canonical UUID text is 36 characters");
    for (index, byte) in text.bytes().enumerate() {
        match index {
            8 | 13 | 18 | 23 => assert_eq!(byte, b'-', "hyphen at position {index}"),
            14 => assert_eq!(byte, b'7', "UUIDv7 version nibble"),
            _ => assert!(
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
                "lowercase hex at position {index}"
            ),
        }
    }
}

#[test]
fn uuidv7_id_shape_and_order() {
    let first = SessionId::new_v7();
    let second = SessionId::new_v7();
    for id in [first, second] {
        let text = id.to_string();
        shape(&text);
        assert_eq!(
            SessionId::parse(&text).expect("canonical id parses"),
            id,
            "parse round-trips"
        );
    }
    let earlier =
        SessionId::parse("0192aa00-0000-7000-8000-000000000001").expect("earlier fixture parses");
    let later =
        SessionId::parse("0192aa00-0001-7000-8000-000000000001").expect("later fixture parses");
    assert!(
        earlier.to_string() < later.to_string(),
        "earlier timestamp orders first"
    );
    assert!(
        first.to_string() <= second.to_string(),
        "monotonic generator never goes backward"
    );
}

#[test]
fn reject_wrong_uuid_forms() {
    assert!(
        SessionId::parse("0192AA00-0000-7000-8000-000000000001").is_err(),
        "uppercase UUID is rejected"
    );
    assert!(
        SessionId::parse("550e8400-e29b-41d4-a716-446655440000").is_err(),
        "version-4 UUID is rejected"
    );
}
