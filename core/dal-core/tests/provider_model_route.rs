//! Provider-aware model route boundaries through the public API.
//!
//! A model record carrying a provider round-trips through the journal codec
//! with its route intact, a legacy provider-less line still decodes to no
//! provider and re-encodes byte-for-byte, and `same_model` bridges a missing
//! provider without bridging a different one.

use std::num::NonZeroU64;

use dal_core::{Entry, EntryId, EntryKind, Family, ModelRoute, Record, decode, encode};

#[test]
fn provider_model_record_round_trips_through_the_public_codec() {
    let route = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
        provider: Some("acme".into()),
    };
    let record = Record::Model(Entry {
        id: EntryId::new(NonZeroU64::MIN),
        parent: None,
        at: "2026-09-25T10:15:30.125Z"
            .parse()
            .expect("valid fixture timestamp"),
        kind: EntryKind::Model {
            route: route.clone(),
        },
    });
    let encoded = encode(&record).expect("provider record encodes");
    let text = std::str::from_utf8(&encoded).expect("journal line is utf-8");
    assert!(text.contains("\"provider\":\"acme\""), "{text}");
    let decoded = decode(&encoded).expect("provider record decodes").record;
    assert_eq!(decoded, record);
    let Record::Model(entry) = decoded else {
        panic!("expected a model record");
    };
    let EntryKind::Model { route: back } = entry.kind else {
        panic!("expected a model kind");
    };
    assert_eq!(back, route);
}

#[test]
fn legacy_provider_less_record_decodes_to_no_provider_and_re_encodes_identically() {
    let line = "{\"v\":1,\"type\":\"model\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.125Z\",\"api\":\"openai_chat\",\"model\":\"gpt-5\"}\n";
    let decoded = decode(line.as_bytes())
        .expect("legacy record decodes")
        .record;
    let Record::Model(entry) = &decoded else {
        panic!("expected a model record");
    };
    let EntryKind::Model { route } = &entry.kind else {
        panic!("expected a model kind");
    };
    assert_eq!(route.provider(), None);
    assert_eq!(
        encode(&decoded).expect("legacy record encodes").as_slice(),
        line.as_bytes()
    );
}

#[test]
fn same_model_bridges_a_missing_provider_only() {
    let bare = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
        provider: None,
    };
    let scoped = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
        provider: Some("acme".into()),
    };
    let other = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
        provider: Some("other".into()),
    };
    assert!(bare.same_model(&scoped));
    assert!(scoped.same_model(&bare));
    assert!(!scoped.same_model(&other));
    assert_ne!(bare, scoped);
}
