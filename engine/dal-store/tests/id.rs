//! Session identifier shape, parsing, and ordering.

use std::collections::HashSet;

use dal_core::SessionId;
use uuid::{Timestamp, Uuid};

fn shape(text: &str) {
    assert_eq!(text.len(), 36, "canonical UUID text is 36 characters");
    for (index, byte) in text.bytes().enumerate() {
        match index {
            8 | 13 | 18 | 23 => assert_eq!(byte, b'-', "hyphen at position {index}"),
            14 => assert_eq!(byte, b'7', "UUIDv7 version nibble"),
            19 => assert!(
                matches!(byte, b'8' | b'9' | b'a' | b'b'),
                "RFC 4122 variant nibble at position {index}, got {}",
                char::from(byte)
            ),
            _ => assert!(
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
                "lowercase hex at position {index}"
            ),
        }
    }
}

#[test]
fn uuidv7_id_shape() {
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
}

/// Listing and `newest` order sessions by id text, so a burst of ids from one
/// process must be unique and strictly increasing in text order, not just
/// ordered across two samples taken a moment apart.
#[test]
fn burst_of_ids_is_unique_strictly_increasing_and_round_trips() {
    const BURST: usize = 4096;
    let mut previous: Option<String> = None;
    let mut seen = HashSet::with_capacity(BURST);
    for index in 0..BURST {
        let id = SessionId::new_v7();
        let text = id.to_string();
        shape(&text);
        assert_eq!(
            SessionId::parse(&text).expect("generated id parses"),
            id,
            "id {index} round-trips"
        );
        if let Some(earlier) = &previous {
            assert!(
                earlier.as_str() < text.as_str(),
                "id {index} ({text}) must sort after its predecessor ({earlier})"
            );
        }
        assert!(seen.insert(id), "id {index} ({text}) repeats an earlier id");
        previous = Some(text);
    }
    assert_eq!(seen.len(), BURST, "every id in the burst is distinct");
}

#[test]
fn ids_from_concurrent_threads_never_collide() {
    const THREADS: usize = 4;
    const PER_THREAD: usize = 2048;
    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            std::thread::spawn(|| {
                (0..PER_THREAD)
                    .map(|_| SessionId::new_v7())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut seen = HashSet::new();
    for worker in workers {
        for id in worker.join().expect("generator thread joins") {
            shape(&id.to_string());
            assert!(seen.insert(id), "{id} was generated twice");
        }
    }
    assert_eq!(seen.len(), THREADS * PER_THREAD);
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

const CANONICAL: &str = "0192aa00-0000-7000-8000-000000000001";

#[test]
fn parse_refuses_every_near_miss_of_the_canonical_form() {
    let near_misses = [
        ("", "empty text"),
        ("0192aa00-0000-7000-8000-00000000000", "one character short"),
        (
            "0192aa00-0000-7000-8000-0000000000010",
            "one character long",
        ),
        (
            "0192aa00-0000-7000-8000-0000000000 1",
            "space in place of a digit",
        ),
        (
            "0192aa00000070008000000000000001",
            "simple form without hyphens",
        ),
        ("{0192aa00-0000-7000-8000-000000000001}", "braced form"),
        ("urn:uuid:0192aa00-0000-7000-8000-000000000001", "urn form"),
        (
            "0192aa00-0000-7000-8000-00000000000A",
            "one uppercase hex digit",
        ),
        (
            "0192aa00-0000-7000-8000-00000000000g",
            "non-hex character in the last position",
        ),
        (
            "0192aa000-000-7000-8000-000000000001",
            "hyphens at the wrong positions",
        ),
        (" 0192aa00-0000-7000-8000-000000000001", "leading space"),
        ("0192aa00-0000-7000-8000-000000000001 ", "trailing space"),
        ("0192aa00-0000-7000-8000-000000000001\n", "trailing newline"),
        ("00000000-0000-0000-0000-000000000000", "the nil UUID"),
        ("0192aa00-0000-6000-8000-000000000001", "version 6"),
        ("0192aa00-0000-8000-8000-000000000001", "version 8"),
        (
            "0192aa00-0000-7000-7000-000000000001",
            "variant nibble 7 (below RFC 4122)",
        ),
        (
            "0192aa00-0000-7000-c000-000000000001",
            "variant nibble c (above RFC 4122)",
        ),
        (
            "0192aa00-0000-7000-0000-000000000001",
            "variant nibble 0 (NCS)",
        ),
        (
            "0192aa00-0000-7000-f000-000000000001",
            "variant nibble f (reserved)",
        ),
    ];
    for (text, why) in near_misses {
        assert!(
            SessionId::parse(text).is_err(),
            "{why}: {text:?} must not parse as a session id"
        );
    }
}

#[test]
fn parse_accepts_exactly_the_rfc4122_variant_nibbles() {
    assert_eq!(
        SessionId::parse(CANONICAL)
            .expect("canonical fixture parses")
            .to_string(),
        CANONICAL
    );
    for nibble in ['8', '9', 'a', 'b'] {
        let text = format!("0192aa00-0000-7000-{nibble}000-000000000001");
        let id = SessionId::parse(&text)
            .unwrap_or_else(|error| panic!("variant nibble {nibble} must parse: {error}"));
        assert_eq!(id.to_string(), text, "variant nibble {nibble} round-trips");
    }
}

/// Seeded `UUIDv7` generation orders by timestamp: the deterministic half of
/// the shape-and-order contract. Wall-clock sampling lives in
/// `uuidv7_id_shape`; burst uniqueness lives in
/// `burst_of_ids_is_unique_strictly_increasing_and_round_trips`.
#[test]
fn uuidv7_id_shape_and_order() {
    // Explicit 42-bit counters sit above the random tail, so two ids minted
    // in the same millisecond order by counter and a later millisecond always
    // orders after, regardless of the random bits.
    let earlier_same_ms = Timestamp::from_unix_time(1_700_000_000, 0, 1, 42);
    let later_same_ms = Timestamp::from_unix_time(1_700_000_000, 0, 2, 42);
    let later_ms = Timestamp::from_unix_time(1_700_000_001, 0, 1, 42);
    let texts = [
        Uuid::new_v7(earlier_same_ms).hyphenated().to_string(),
        Uuid::new_v7(later_same_ms).hyphenated().to_string(),
        Uuid::new_v7(later_ms).hyphenated().to_string(),
    ];
    for text in &texts {
        shape(text);
        assert_eq!(
            SessionId::parse(text)
                .expect("seeded v7 id parses")
                .to_string(),
            *text,
            "parse round-trips the seeded id"
        );
    }
    assert!(
        texts[0] < texts[1],
        "same-millisecond ids order by counter: {} before {}",
        texts[0],
        texts[1]
    );
    assert!(
        texts[1] < texts[2],
        "a later timestamp orders after: {} before {}",
        texts[1],
        texts[2]
    );
}
