//! ENG-01 shared-contract regression tests for the provider surface.
//!
//! Covers provider/call identity (`provider/def.rs`), terminal outcomes and
//! cancellation ordering (`error/`), and credential-ownership boundaries plus
//! catalog readability (`catalog/`).

use dal_core::{Family, InferFailure};
use dal_provider::{PROVIDERS, ProviderError, RunLostReason, find};

#[test]
fn provider_ids_are_unique_and_stable_call_identity() {
    let mut seen = std::collections::HashSet::new();
    for def in PROVIDERS {
        assert_ne!(def.id, "");
        assert!(
            def.id
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-')),
            "{} is not [a-z0-9-]+",
            def.id
        );
        assert!(seen.insert(def.id), "{} appears twice", def.id);
        let found = find(def.id).expect("find returns its own row");
        assert_eq!(found.id, def.id);
        assert_eq!(found.family, def.family);
        assert_eq!(found.base_url, def.base_url);
    }
    assert!(find("nobody").is_none());
}

#[test]
fn every_row_signs_in_somehow_and_names_its_credential_owner() {
    for def in PROVIDERS {
        assert!(
            def.key.is_some() || def.oauth.is_some(),
            "{} has no sign-in",
            def.id
        );
        assert!(
            def.methods().next().is_some(),
            "{} offers no method",
            def.id
        );
        if let Some(key) = def.key {
            assert!(!key.env.is_empty(), "{} key has no env owner", def.id);
            for name in key.env {
                assert!(
                    name.bytes()
                        .all(|b| matches!(b, b'A'..=b'Z' | b'0'..=b'9' | b'_')),
                    "{}: {name} is not an env name",
                    def.id
                );
            }
        }
    }
}

#[test]
fn key_env_ownership_never_crosses_origins() {
    let origin_of = |base_url: &str| {
        url::Url::parse(base_url)
            .expect("table base parses")
            .origin()
    };
    let mut owners: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for def in PROVIDERS {
        let env = def.key.map_or(&[][..], |key| key.env);
        for name in env {
            owners.entry(name).or_default().push(def.id);
        }
    }
    for (name, ids) in &owners {
        let origins: Vec<_> = ids
            .iter()
            .map(|id| origin_of(find(id).expect("row").base_url))
            .collect();
        for pair in origins.windows(2) {
            assert_eq!(
                pair[0], pair[1],
                "{name} would send one vendor key to another host: {ids:?}"
            );
        }
    }
}

#[test]
fn client_id_overrides_never_name_a_key_variable() {
    for def in PROVIDERS {
        let Some(oauth) = def.oauth else { continue };
        let Some(name) = oauth.client_id_env else {
            continue;
        };
        for other in PROVIDERS {
            let keys = other.key.map_or(&[][..], |key| key.env);
            assert!(
                !keys.contains(&name),
                "{} reads {name} as client id but {} reads it as key",
                def.id,
                other.id
            );
        }
    }
}

#[test]
fn retryable_by_loop_names_only_the_four_replayable_failures() {
    let replayable = [
        ProviderError::StreamCut,
        ProviderError::WsClosed { code: None },
        ProviderError::Overloaded,
        ProviderError::Transport {
            family: Family::Chat,
            reason: "x".into(),
        },
    ];
    for error in &replayable {
        assert!(error.retryable_by_loop(), "{error} should be replayable");
    }
    let terminal = [
        ProviderError::Status {
            family: Family::Chat,
            status: 500,
            message: "x".into(),
        },
        ProviderError::InvalidRequest {
            message: "x".into(),
        },
        ProviderError::Quota {
            message: "x".into(),
        },
        ProviderError::AuthRejected {
            provider: "openai".into(),
        },
        ProviderError::SignInExpired {
            provider: "openai".into(),
        },
        ProviderError::NoCredentials {
            provider: "openai".into(),
        },
        ProviderError::Protocol {
            family: Family::Chat,
            detail: "x".into(),
        },
        ProviderError::Cancelled,
        ProviderError::UnknownModel {
            reference: "x".into(),
        },
    ];
    for error in &terminal {
        assert!(!error.retryable_by_loop(), "{error} must be terminal");
    }
}

#[test]
fn a_lost_run_is_fatal_whatever_its_reason() {
    let reasons = [
        RunLostReason::ServerClosed,
        RunLostReason::Transport("x".into()),
        RunLostReason::ResultsMismatch,
        RunLostReason::Protocol("x".into()),
        RunLostReason::BufferExceeded,
        RunLostReason::Authority("tool".into()),
    ];
    for reason in reasons {
        let error = ProviderError::RunLost {
            family: Family::Chat,
            reason,
        };
        assert!(!error.retryable_by_loop());
        let failure = InferFailure::from(error);
        assert!(matches!(failure, InferFailure::Fatal { .. }), "{failure}");
    }
}

#[test]
fn cancellation_is_terminal_and_never_replayable() {
    assert_eq!(
        ProviderError::Cancelled.to_string(),
        "provider operation cancelled."
    );
    let error = ProviderError::Cancelled;
    assert!(!error.retryable_by_loop());
    let failure = InferFailure::from(error);
    assert!(
        matches!(failure, InferFailure::Fatal { .. }),
        "cancellation must end the turn, got {failure}"
    );
}

#[test]
fn terminal_failures_map_to_exactly_one_infer_outcome() {
    let overflow = ProviderError::ContextOverflow {
        family: Family::Chat,
        code: "context_length_exceeded".into(),
        message: "too big".into(),
    };
    assert!(matches!(
        InferFailure::from(overflow),
        InferFailure::Overflow { .. }
    ));
    let retryable = ProviderError::RateLimited {
        message: "slow".into(),
        retry_after: None,
    };
    assert!(matches!(
        InferFailure::from(retryable),
        InferFailure::Retryable { .. }
    ));
    let fatal = ProviderError::AuthRejected {
        provider: "openai".into(),
    };
    let failure = InferFailure::from(fatal);
    assert!(matches!(failure, InferFailure::Fatal { .. }), "{failure}");
}

#[test]
fn vendor_families_are_named_by_their_first_row() {
    for (family, vendor) in [
        (Family::Codex, "openai-codex"),
        (Family::Anthropic, "anthropic"),
        (Family::PiMessages, "radius"),
    ] {
        let first = PROVIDERS.iter().find(|def| def.family == family);
        assert_eq!(first.map(|def| def.id), Some(vendor), "{family:?}");
    }
}

#[test]
fn key_only_gateway_rows_carry_no_oauth() {
    for id in ["radius", "openrouter"] {
        let def = find(id).expect("gateway row registered");
        assert!(def.key.is_some(), "{id} takes a key");
        assert!(def.oauth.is_none(), "{id} must stay key-only");
    }
}
