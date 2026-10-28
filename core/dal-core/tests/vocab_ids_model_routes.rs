#![expect(clippy::expect_used, reason = "integration tests fail loudly")]

//! Vocabulary boundary proofs: id, workspace,
//! `content`, `model` core + pricing) through the public `dal_core::` API.
//!
//! Each test pins a typed-boundary contract: invalid states are rejected at
//! construction AND at deserialization with typed errors, never panics or
//! silent normalisation. Where the code deliberately evolved beyond the
//! original wording (opaque ids accept empty; `Api` routes carry no grammar), the
//! test pins the code behavior and the report records the drift.

use std::path::PathBuf;

use dal_core::{
    BlobId, CallId, ClientId, ContentError, ContentLimits, EntryId, Family, Gen, GenerationId,
    IdError, InferFailure, JobId, ModelPrice, ModelRoute, Part, PriceTier, PrivateEffectId,
    PrivateLoopId, RequestId, Seq, SessionId, TurnId, Usage, Workspace, check_synthetic_chain,
};

fn decode<T>(json: &str) -> Result<T, sonic_rs::Error>
where
    T: for<'de> serde::Deserialize<'de>,
{
    sonic_rs::from_str(json)
}

fn quoted(value: &str) -> String {
    format!("\"{value}\"")
}

// Canonical lowercase UUIDv7, all five id types.
#[test]
fn uuid_ids_reject_noncanonical_shapes_at_parse_and_decode() {
    const CANONICAL: &str = "01927f40-0000-7000-8000-000000000001";
    assert_eq!(
        SessionId::parse(CANONICAL).expect("parse").to_string(),
        CANONICAL
    );
    assert_eq!(
        RequestId::parse(CANONICAL).expect("parse").to_string(),
        CANONICAL
    );
    assert_eq!(
        JobId::parse(CANONICAL).expect("parse").to_string(),
        CANONICAL
    );
    assert_eq!(
        PrivateLoopId::parse(CANONICAL).expect("parse").to_string(),
        CANONICAL
    );
    assert_eq!(
        PrivateEffectId::parse(CANONICAL)
            .expect("parse")
            .to_string(),
        CANONICAL
    );
    assert_eq!(
        decode::<SessionId>(&quoted(CANONICAL)).expect("decode"),
        SessionId::parse(CANONICAL).expect("parse")
    );
    assert_eq!(
        decode::<RequestId>(&quoted(CANONICAL)).expect("decode"),
        RequestId::parse(CANONICAL).expect("parse")
    );
    assert_eq!(
        decode::<JobId>(&quoted(CANONICAL)).expect("decode"),
        JobId::parse(CANONICAL).expect("parse")
    );
    assert_eq!(
        decode::<PrivateLoopId>(&quoted(CANONICAL)).expect("decode"),
        PrivateLoopId::parse(CANONICAL).expect("parse")
    );
    assert_eq!(
        decode::<PrivateEffectId>(&quoted(CANONICAL)).expect("decode"),
        PrivateEffectId::parse(CANONICAL).expect("parse")
    );
    let bad = [
        "01927F40-0000-7000-8000-000000000001",
        "550e8400-e29b-41d4-a716-446655440000",
        "00000000-0000-0000-0000-000000000000",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "01927f40-0000-7000-0000-000000000001",
        "01927f40-0000-7000-c000-000000000001",
        "{01927f40-0000-7000-8000-000000000001}",
        "urn:uuid:01927f40-0000-7000-8000-000000000001",
        "01927f40000070008000000000000001",
        " 01927f40-0000-7000-8000-000000000001",
        "01927f40-0000-7000-8000-000000000001 ",
        "",
        "not-a-uuid",
    ];
    for value in bad {
        assert_eq!(SessionId::parse(value), Err(IdError::Invalid), "{value:?}");
        assert_eq!(RequestId::parse(value), Err(IdError::Invalid), "{value:?}");
        assert_eq!(JobId::parse(value), Err(IdError::Invalid), "{value:?}");
        assert_eq!(
            PrivateLoopId::parse(value),
            Err(IdError::Invalid),
            "{value:?}"
        );
        assert_eq!(
            PrivateEffectId::parse(value),
            Err(IdError::Invalid),
            "{value:?}"
        );
        let json = quoted(value);
        assert!(decode::<SessionId>(&json).is_err(), "{value:?}");
        assert!(decode::<RequestId>(&json).is_err(), "{value:?}");
        assert!(decode::<JobId>(&json).is_err(), "{value:?}");
        assert!(decode::<PrivateLoopId>(&json).is_err(), "{value:?}");
        assert!(decode::<PrivateEffectId>(&json).is_err(), "{value:?}");
    }
    for json in ["1", "null", "[]", "{}"] {
        assert!(decode::<SessionId>(json).is_err(), "{json}");
    }
    let fresh = SessionId::new_v7();
    assert_eq!(
        SessionId::parse(&fresh.to_string()).expect("fresh round-trips"),
        fresh
    );
}

// Nonzero counters, all five counter types.
#[test]
fn counter_ids_reject_zero_and_noninteger_shapes() {
    assert_eq!(decode::<EntryId>("1").expect("decode").get(), 1);
    assert_eq!(decode::<TurnId>("1").expect("decode").get(), 1);
    assert_eq!(decode::<Gen>("1").expect("decode").get(), 1);
    assert_eq!(decode::<Seq>("1").expect("decode").get(), 1);
    assert_eq!(decode::<GenerationId>("1").expect("decode").get(), 1);
    assert_eq!(
        decode::<EntryId>(&u64::MAX.to_string())
            .expect("decode")
            .get(),
        u64::MAX
    );
    for json in [
        "0",
        "-1",
        "01",
        "1.0",
        "\"1\"",
        "\"\"",
        "18446744073709551616",
        "null",
        "[]",
        "{}",
    ] {
        assert!(decode::<EntryId>(json).is_err(), "{json}");
        assert!(decode::<TurnId>(json).is_err(), "{json}");
        assert!(decode::<Gen>(json).is_err(), "{json}");
        assert!(decode::<Seq>(json).is_err(), "{json}");
        assert!(decode::<GenerationId>(json).is_err(), "{json}");
    }
}

// Opaque ids preserve text verbatim, including empty; reject non-strings.
// Code evidence: `engine/dal-provider/src/family/devin/history.rs` skips tool
// calls with an empty call id and `engine/dal-provider/src/family/google/shared.rs`
// mints a fresh id for absent/empty/reused provider ids, so empty text must
// survive the type and be handled at the use site, never rejected at decode.
#[test]
fn opaque_id_decoding_preserves_text_verbatim() {
    for text in [
        "",
        "call_123",
        "  padded  ",
        "héllo-世界-🎉",
        "a/b:c?d=e&f",
        "\"quoted\"",
    ] {
        let json = sonic_rs::to_string(&text).expect("encode fixture");
        let call = CallId::new(text);
        assert_eq!(call.as_str(), text);
        assert_eq!(call.to_string(), text);
        assert_eq!(decode::<CallId>(&json).expect("decode"), call);
        assert_eq!(sonic_rs::to_string(&call).expect("encode"), json);
        let client = ClientId::new(text);
        assert_eq!(client.as_str(), text);
        assert_eq!(decode::<ClientId>(&json).expect("decode"), client);
        assert_eq!(sonic_rs::to_string(&client).expect("encode"), json);
    }
    for json in ["1", "1.0", "true", "null", "[]", "{}"] {
        assert!(decode::<CallId>(json).is_err(), "{json}");
        assert!(decode::<ClientId>(json).is_err(), "{json}");
    }
}

// BLAKE3 digest grammar.
#[test]
fn blob_id_rejects_off_spec_spellings() {
    let canonical = BlobId::from_bytes(b"stored").to_string();
    assert_eq!(canonical.len(), 64);
    let blob = BlobId::parse(&canonical).expect("parse");
    assert_eq!(blob.to_string(), canonical);
    assert_eq!(decode::<BlobId>(&quoted(&canonical)).expect("decode"), blob);
    assert_eq!(
        BlobId::parse(&blob.to_string()).expect("display round-trips"),
        blob
    );
    let bad = [
        canonical.to_uppercase(),
        "g".repeat(64),
        "0".repeat(63),
        "0".repeat(65),
        String::new(),
        format!(" {canonical}"),
        format!("{canonical} "),
        format!(" {canonical} "),
    ];
    for value in &bad {
        assert_eq!(BlobId::parse(value), Err(IdError::Invalid), "{value:?}");
        assert!(decode::<BlobId>(&quoted(value)).is_err(), "{value:?}");
    }
    for json in ["1", "1.0", "true", "null", "[0,0,0,0]", "{}", "\"\""] {
        assert!(decode::<BlobId>(json).is_err(), "{json}");
    }
}

// Absolute workspace, lexical preservation.
#[test]
fn workspace_rejects_relative_and_preserves_absolute_lexically() {
    for relative in ["relative/child", "", ".", "..", "../escape"] {
        let path = PathBuf::from(relative);
        assert!(Workspace::new(path.clone()).is_err(), "{relative:?}");
        assert!(Workspace::try_from(path).is_err(), "{relative:?}");
        assert!(
            decode::<Workspace>(&quoted(relative)).is_err(),
            "{relative:?}"
        );
    }
    for json in ["1", "null", "[]", "{}"] {
        assert!(decode::<Workspace>(json).is_err(), "{json}");
    }
    #[cfg(unix)]
    let absolute = PathBuf::from("/nonexistent-dal-workspace/link/../child");
    #[cfg(windows)]
    let absolute = PathBuf::from(r"C:\nonexistent-dal-workspace\link\..\child");
    let workspace = Workspace::new(absolute.clone()).expect("absolute accepted");
    assert_eq!(workspace.as_path(), absolute);
    assert_eq!(
        Workspace::try_from(absolute.clone())
            .expect("try_from")
            .as_path(),
        absolute
    );
    let encoded = sonic_rs::to_string(&workspace).expect("encode");
    assert_eq!(decode::<Workspace>(&encoded).expect("decode"), workspace);
    assert_eq!(workspace.into_path_buf(), absolute);
}

// Inline limit counts bytes; exact error text is the contract.
#[test]
fn content_inline_limit_counts_bytes_with_exact_error() {
    for length in [ContentLimits::INLINE_BYTES, ContentLimits::INLINE_BYTES + 1] {
        let expected = if length == ContentLimits::INLINE_BYTES {
            Ok(())
        } else {
            Err(ContentError::InlineTooLarge { actual: length })
        };
        assert_eq!(
            ContentLimits::validate(&Part::Text {
                text: "x".repeat(length).into()
            }),
            expected
        );
        assert_eq!(
            ContentLimits::validate(&Part::Image {
                mime: "image/png".into(),
                bytes: vec![0; length].into(),
            }),
            expected
        );
    }
    assert_eq!(
        ContentError::InlineTooLarge { actual: 16_385 }.to_string(),
        "inline content is 16385 bytes; the limit is 16384; use a blob"
    );
    let multibyte = "é".repeat(ContentLimits::INLINE_BYTES);
    assert_eq!(
        ContentLimits::validate(&Part::Text {
            text: multibyte.clone().into()
        }),
        Err(ContentError::InlineTooLarge {
            actual: multibyte.len()
        })
    );
    assert_eq!(
        ContentLimits::validate(&Part::Text { text: "".into() }),
        Ok(())
    );
    assert_eq!(
        ContentLimits::validate(&Part::Image {
            mime: "image/png".into(),
            bytes: Box::default()
        }),
        Ok(())
    );
    assert_eq!(
        ContentLimits::validate(&Part::Text {
            text: "héllo-世界".into()
        }),
        Ok(())
    );
}

// Part shapes, serde tags, and the `validate`-is-the-gate rule.
#[test]
fn content_part_serde_leaves_limit_to_validate() {
    let blob_id = BlobId::from_bytes(b"stored");
    for bytes in [0, u64::MAX] {
        let part = Part::Blob {
            blob_id,
            mime: "image/png".into(),
            bytes,
        };
        assert_eq!(ContentLimits::validate(&part), Ok(()));
        let encoded = sonic_rs::to_string(&part).expect("encode");
        assert_eq!(decode::<Part>(&encoded).expect("decode"), part);
    }
    let text = Part::Text { text: "".into() };
    assert_eq!(ContentLimits::validate(&text), Ok(()));
    assert_eq!(
        decode::<Part>(&sonic_rs::to_string(&text).expect("encode")).expect("decode"),
        text
    );
    // Serde itself never enforces the inline limit: an oversized part decodes
    // fine and only `ContentLimits::validate` rejects it.
    let oversized = Part::Text {
        text: "x".repeat(ContentLimits::INLINE_BYTES + 1).into(),
    };
    let decoded =
        decode::<Part>(&sonic_rs::to_string(&oversized).expect("encode")).expect("decode accepts");
    assert_eq!(decoded, oversized);
    assert_eq!(
        ContentLimits::validate(&decoded),
        Err(ContentError::InlineTooLarge {
            actual: ContentLimits::INLINE_BYTES + 1
        })
    );
    for json in [
        r#"{"type":"video","url":"x"}"#.to_owned(),
        r#"{"type":"text"}"#.to_owned(),
        r#"{"type":"image","mime":"image/png"}"#.to_owned(),
        format!(r#"{{"type":"blob","blob_id":"{blob_id}","mime":"image/png"}}"#),
        r#"{"text":"hi"}"#.to_owned(),
    ] {
        assert!(decode::<Part>(&json).is_err(), "{json}");
    }
}

// Synthetic `id` grammar, shared by construction and decode checks.
fn check_synthetic_route_grammar() {
    for id in ["a/b", "a-0/b.c_0-1", "ns-9/x.y_z-0"] {
        assert_eq!(ModelRoute::synthetic(id).expect("valid").id(), id);
        let json = format!(r#"{{"kind":"synthetic","id":"{id}"}}"#);
        assert_eq!(decode::<ModelRoute>(&json).expect("decode").id(), id);
    }
    for id in [
        "", "a", "/b", "a/", "A/b", "a/B", "a/b/c", "a//b", "a.b/c", "a/b c", "a/b!", "-/",
    ] {
        assert!(ModelRoute::synthetic(id).is_err(), "{id:?}");
        let json = format!(r#"{{"kind":"synthetic","id":"{id}"}}"#);
        assert!(decode::<ModelRoute>(&json).is_err(), "{id:?}");
    }
}

// Harness `id` grammar, shared by construction and decode checks.
fn check_harness_route_grammar() {
    for id in ["dalgon/normal", "dalgon/eval-first", "dalgon/eval-only"] {
        assert_eq!(ModelRoute::harness(id).expect("valid").id(), id);
        let json = format!(r#"{{"kind":"harness","id":"{id}"}}"#);
        assert_eq!(decode::<ModelRoute>(&json).expect("decode").id(), id);
    }
    for id in [
        "dalgon/other",
        "Dalgon/normal",
        "dalgon/normal/",
        "",
        "dalgon/",
    ] {
        assert!(ModelRoute::harness(id).is_err(), "{id:?}");
        let json = format!(r#"{{"kind":"harness","id":"{id}"}}"#);
        assert!(decode::<ModelRoute>(&json).is_err(), "{id:?}");
    }
}

// Rejected kinds and the closed family wire table.
fn check_family_wire_names() {
    assert!(decode::<ModelRoute>(r#"{"kind":"quantum","id":"a/b"}"#).is_err());
    assert!(decode::<ModelRoute>(r#"{"kind":"api","family":"openai_chat"}"#).is_err());
    assert!(decode::<ModelRoute>(r#"{"family":"openai_chat","model":"gpt-5"}"#).is_err());
    for (family, wire) in [
        (Family::Chat, "openai_chat"),
        (Family::Responses, "openai_responses"),
        (Family::Codex, "openai_codex"),
        (Family::Anthropic, "anthropic"),
        (Family::Bedrock, "bedrock"),
        (Family::Cursor, "cursor"),
        (Family::Devin, "devin"),
        (Family::Droid, "droid"),
        (Family::GitLab, "gitlab"),
        (Family::GoogleGenerativeAi, "google-generative-ai"),
        (Family::GoogleVertex, "google-vertex"),
        (Family::GoogleGeminiCli, "google-gemini-cli"),
        (Family::GoogleCloudCodeAssist, "google-cloud-code-assist"),
        (Family::GoogleAntigravity, "google-antigravity"),
        (Family::Mistral, "mistral"),
        (Family::Ollama, "ollama"),
        (Family::PiMessages, "pi-messages"),
    ] {
        let json = quoted(wire);
        assert_eq!(decode::<Family>(&json).expect("decode"), family, "{wire}");
        assert_eq!(
            sonic_rs::to_string(&family).expect("encode"),
            json,
            "{wire:?}"
        );
    }
    assert!(decode::<Family>("\"openai\"").is_err());
    assert!(decode::<Family>("\"Chat\"").is_err());
    assert!(decode::<Family>("1").is_err());
}

// Route grammars at construction AND decode; family wire names;
// `from_id` prefix mapping.
#[test]
fn model_route_grammars_enforced_at_construction_and_decode() {
    check_synthetic_route_grammar();
    check_harness_route_grammar();
    check_family_wire_names();
    let api = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
        provider: None,
    };
    assert_eq!(api.id(), "gpt-5");
    let encoded = sonic_rs::to_string(&api).expect("encode");
    assert_eq!(
        encoded,
        r#"{"kind":"api","family":"openai_chat","model":"gpt-5"}"#
    );
    assert_eq!(decode::<ModelRoute>(&encoded).expect("decode"), api);
    assert_eq!(
        ModelRoute::from_id("openai-chat/gpt-5"),
        ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-5".into(),
            provider: None
        }
    );
    assert_eq!(
        ModelRoute::from_id("openai-responses/gpt-5"),
        ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-5".into(),
            provider: None
        }
    );
    assert_eq!(
        ModelRoute::from_id("openai-codex/codex-1"),
        ModelRoute::Api {
            family: Family::Codex,
            model: "codex-1".into(),
            provider: None
        }
    );
    assert_eq!(
        ModelRoute::from_id("anthropic/claude-opus-5"),
        ModelRoute::Api {
            family: Family::Anthropic,
            model: "claude-opus-5".into(),
            provider: None
        }
    );
    assert_eq!(
        ModelRoute::from_id("dalgon/normal"),
        ModelRoute::Harness {
            id: "dalgon/normal".into()
        }
    );
    assert_eq!(
        ModelRoute::from_id("acme/fast"),
        ModelRoute::Synthetic {
            id: "acme/fast".into()
        }
    );
    assert_eq!(
        ModelRoute::from_id("gpt-5"),
        ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-5".into(),
            provider: None
        }
    );
    // `dalgon/other` is a valid synthetic spelling, so it resolves synthetic.
    assert_eq!(
        ModelRoute::from_id("dalgon/other"),
        ModelRoute::Synthetic {
            id: "dalgon/other".into()
        }
    );
    assert_eq!(
        ModelRoute::from_id("ACME/fast"),
        ModelRoute::Api {
            family: Family::Chat,
            model: "ACME/fast".into(),
            provider: None
        }
    );
}

// Model `Api` empty-model pin: the route type carries no grammar by design
// (journal `AssistantRoute::check_id` accepts every `Api` id and the entry
// layer owns the non-empty rule), so an empty model decodes and round-trips
// here while every use boundary still rejects it.
#[test]
fn api_route_empty_model_decodes_with_journal_holding_nonempty_rule() {
    let json = r#"{"kind":"api","family":"openai_chat","model":""}"#;
    let route = decode::<ModelRoute>(json).expect("empty api model decodes");
    assert_eq!(route.id(), "");
    assert_eq!(
        decode::<ModelRoute>(&sonic_rs::to_string(&route).expect("encode")).expect("re-decode"),
        route
    );
    assert_eq!(
        ModelRoute::from_id(""),
        ModelRoute::Api {
            family: Family::Chat,
            model: "".into(),
            provider: None
        }
    );
}

// The local Gemini CLI subprocess and native Cloud Code Assist are separate
// families: every route prefix finds its own family, and a stored route reads
// back as the family that wrote it, never as the other.
#[test]
fn gemini_cli_and_cloud_code_assist_families_never_read_as_each_other() {
    for family in Family::ALL {
        assert_eq!(
            Family::from_route_prefix(family.route_prefix()),
            Some(family),
            "{family:?}"
        );
    }
    assert_eq!(
        Family::from_route_prefix("google-gemini-cli"),
        Some(Family::GoogleGeminiCli)
    );
    assert_eq!(
        Family::from_route_prefix("google-cloud-code-assist"),
        Some(Family::GoogleCloudCodeAssist)
    );
    let route = |family| ModelRoute::Api {
        family,
        model: "gemini-3-pro".into(),
        provider: None,
    };
    let local = route(Family::GoogleGeminiCli);
    let native = route(Family::GoogleCloudCodeAssist);
    assert!(!local.same_model(&native));
    assert_ne!(
        local.family_qualified_reference(),
        native.family_qualified_reference()
    );
}

// Depth bound precedes cycle scan; plain repeats are not cycles.
#[test]
fn synthetic_chain_reports_depth_before_cycle_and_ignores_plain_repeats() {
    let synth = |id: &str| ModelRoute::synthetic(id).expect("valid synthetic");
    let api = |model: &str| ModelRoute::Api {
        family: Family::Chat,
        model: model.into(),
        provider: None,
    };
    let four = vec![
        synth("a/one"),
        synth("a/two"),
        synth("a/three"),
        synth("a/four"),
    ];
    assert_eq!(check_synthetic_chain(&four), Ok(()));
    assert_eq!(check_synthetic_chain(&[]), Ok(()));
    let five = vec![
        synth("a/one"),
        synth("a/two"),
        synth("a/three"),
        synth("a/four"),
        synth("a/five"),
    ];
    assert!(matches!(
        check_synthetic_chain(&five),
        Err(InferFailure::SyntheticDepth { .. })
    ));
    let repeated = vec![synth("a/one"), synth("a/two"), synth("a/one")];
    assert!(matches!(
        check_synthetic_chain(&repeated),
        Err(InferFailure::SyntheticCycle { .. })
    ));
    let long_repeated = vec![
        synth("a/one"),
        synth("a/two"),
        synth("a/three"),
        synth("a/four"),
        synth("a/one"),
    ];
    assert!(
        matches!(
            check_synthetic_chain(&long_repeated),
            Err(InferFailure::SyntheticDepth { .. })
        ),
        "depth reports before the cycle scan"
    );
    assert_eq!(check_synthetic_chain(&[api("m"), api("m")]), Ok(()));
    assert_eq!(
        check_synthetic_chain(&[synth("a/one"), api("m"), synth("a/two"), api("m")]),
        Ok(())
    );
    assert_eq!(
        check_synthetic_chain(&[api("m"), synth("a/one"), api("n")]),
        Ok(())
    );
}

fn flat_price(input: f64, cached_input: f64, output: f64, reasoning: f64) -> ModelPrice {
    ModelPrice {
        input,
        cached_input,
        output,
        reasoning,
        cache_write: None,
        tiers: Box::default(),
    }
}

fn priced_usage() -> Usage {
    Usage {
        input_tokens: 2_000_000,
        cached_input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        reasoning_tokens: Some(500_000),
        cache_write_tokens: 100_000,
        premium_requests: None,
        cost_usd: None,
    }
}

// Precedence provider > configured > compiled > unknown; unknown is
// never invented as zero; invalid provider costs do not fall back.
#[test]
fn usage_cost_precedence_never_invents_zero() {
    let configured = flat_price(3.0, 3.0, 3.0, 3.0);
    let compiled = flat_price(1.0, 1.0, 1.0, 1.0);
    let usage = priced_usage();
    assert_eq!(usage.cost_usd(None, None), None);
    assert_eq!(usage.cost_usd(None, Some(&compiled)), Some(3.5));
    assert_eq!(
        usage.cost_usd(Some(&configured), Some(&compiled)),
        Some(10.5)
    );
    let reported = Usage {
        cost_usd: Some(7.25),
        ..usage
    };
    assert_eq!(
        reported.cost_usd(Some(&configured), Some(&compiled)),
        Some(7.25)
    );
    let zero_report = Usage {
        cost_usd: Some(0.0),
        ..usage
    };
    assert_eq!(zero_report.cost_usd(None, None), Some(0.0));
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        let invalid = Usage {
            cost_usd: Some(bad),
            ..usage
        };
        assert_eq!(
            invalid.cost_usd(Some(&configured), Some(&compiled)),
            None,
            "{bad}"
        );
        assert_eq!(invalid.cost_usd(None, None), None, "{bad}");
    }
    let zero = Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: None,
        cost_usd: None,
    };
    assert_eq!(
        zero.cost_usd(None, None),
        None,
        "unknown stays unknown, not zero"
    );
    assert_eq!(
        zero.cost_usd(Some(&flat_price(1.0, 1.0, 2.0, 3.0)), None),
        Some(0.0)
    );
}

// Four-rate formula, disjoint counters, invalid inputs stay unknown.
#[test]
fn usage_price_formula_and_invalid_inputs() {
    let usage = priced_usage();
    // ((2M-1M-100k)*1.0 + 100k*1.0 + 1M*0.5 + 1M*2.0 + 500k*0.25)/1e6 = 3.625.
    assert_eq!(
        flat_price(1.0, 0.5, 2.0, 0.25).cost_usd(&usage),
        Some(3.625)
    );
    let no_reasoning = Usage {
        reasoning_tokens: None,
        ..usage
    };
    let zero_reasoning = Usage {
        reasoning_tokens: Some(0),
        ..usage
    };
    assert_eq!(
        flat_price(1.0, 0.5, 2.0, 0.25).cost_usd(&no_reasoning),
        flat_price(1.0, 0.5, 2.0, 0.25).cost_usd(&zero_reasoning)
    );
    assert_eq!(
        flat_price(1.0, 1.0, 1.0, 1.0).cost_usd(&Usage {
            cache_write_tokens: 0,
            ..usage
        }),
        flat_price(1.0, 1.0, 1.0, 1.0).cost_usd(&usage)
    );
    assert_eq!(
        flat_price(1.0, 1.0, 1.0, 1.0).cost_usd(&Usage {
            cached_input_tokens: 2_000_001,
            ..usage
        }),
        None
    );
    assert_eq!(
        flat_price(1.0, 1.0, 1.0, 1.0).cost_usd(&Usage {
            cache_write_tokens: 1_000_001,
            ..usage
        }),
        None
    );
    for (input, cached, output, reasoning) in [
        (-1.0, 1.0, 1.0, 1.0),
        (1.0, f64::NAN, 1.0, 1.0),
        (1.0, 1.0, f64::INFINITY, 1.0),
        (1.0, 1.0, 1.0, f64::NEG_INFINITY),
    ] {
        assert_eq!(
            flat_price(input, cached, output, reasoning).cost_usd(&usage),
            None
        );
    }
    assert_eq!(
        flat_price(1e300, 1.0, 1.0, 1.0).cost_usd(&Usage {
            input_tokens: u64::MAX,
            ..usage
        }),
        None
    );
}

// Price tiers + premium: one request-wide card, highest exceeded boundary;
// premium-only responses price unknown unless the provider reported a cost.
#[test]
fn price_tier_and_premium_semantics() {
    let price = ModelPrice {
        input: 1.0,
        cached_input: 1.0,
        output: 2.0,
        reasoning: 3.0,
        cache_write: None,
        tiers: vec![PriceTier {
            size: 100,
            input: Some(10.0),
            cached_input: None,
            cache_write: None,
            output: None,
            reasoning: None,
        }]
        .into_boxed_slice(),
    };
    let at = |input_tokens: u64| Usage {
        input_tokens,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: None,
        cost_usd: None,
    };
    assert_eq!(price.cost_usd(&at(100)), Some(100.0 / 1_000_000.0));
    assert_eq!(price.cost_usd(&at(101)), Some(1010.0 / 1_000_000.0));
    let mixed = Usage {
        input_tokens: 101,
        output_tokens: 1_000_000,
        ..at(101)
    };
    assert_eq!(
        price.cost_usd(&mixed),
        Some((1010.0 + 2_000_000.0) / 1_000_000.0)
    );
    let tiered = sonic_rs::to_string(&price).expect("encode");
    assert_eq!(decode::<ModelPrice>(&tiered).expect("decode"), price);
    for premium in [0.0, 2.5] {
        let usage = Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            premium_requests: Some(premium),
            cost_usd: None,
        };
        assert!(usage.is_unreported());
        assert_eq!(price.cost_usd(&usage), None);
        assert_eq!(usage.cost_usd(Some(&price), None), None);
    }
    let reported = Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: Some(2.5),
        cost_usd: Some(7.25),
    };
    assert_eq!(reported.cost_usd(Some(&price), None), Some(7.25));
    let legacy = r#"{"input_tokens":1,"cached_input_tokens":0,"output_tokens":0,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}"#;
    assert_eq!(
        decode::<Usage>(legacy)
            .expect("legacy decodes")
            .premium_requests,
        None
    );
    let measured = Usage {
        premium_requests: Some(2.5),
        ..priced_usage()
    };
    let encoded = sonic_rs::to_string(&measured).expect("encode");
    assert_eq!(decode::<Usage>(&encoded).expect("decode"), measured);
}
