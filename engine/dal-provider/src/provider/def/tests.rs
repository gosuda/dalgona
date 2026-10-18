use std::{collections::BTreeMap, path::Path};

use dal_core::{Config, ConfigProduct};
use url::Url;

use super::*;
use crate::ProviderConfig;

#[test]
fn ids_are_unique_and_match_the_grammar() {
    for (index, def) in PROVIDERS.iter().enumerate() {
        assert!(
            !def.id.is_empty()
                && def
                    .id
                    .bytes()
                    .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-')),
            "{} is not [a-z0-9-]+",
            def.id
        );
        assert!(
            PROVIDERS[..index]
                .iter()
                .all(|earlier| earlier.id != def.id),
            "{} appears twice",
            def.id
        );
    }
}

#[test]
fn rows_that_share_a_loopback_share_a_client() {
    for (index, first) in PROVIDERS.iter().enumerate() {
        let Some(first_oauth) = first.oauth else {
            continue;
        };
        for second in &PROVIDERS[index + 1..] {
            let Some(second_oauth) = second.oauth else {
                continue;
            };
            let same_loopback = first_oauth.redirect.host == second_oauth.redirect.host
                && first_oauth.redirect.port == second_oauth.redirect.port;
            assert!(
                !same_loopback || first_oauth.client_id == second_oauth.client_id,
                "{} and {} share a loopback but not a client",
                first.id,
                second.id
            );
        }
    }
}

#[test]
fn callback_rows_keep_their_device_fallback() {
    // A `Completion::Callback` row whose registered port is busy falls back
    // to its device sign-in; without one the login has no way to complete
    // on a busy port, and routing it into another row's device flow would
    // be a crash (`gitlab-duo` has no device sign-in, so it stays
    // `PasteCode`). A paste row needs no fallback.
    for def in PROVIDERS {
        let Some(oauth) = def.oauth else { continue };
        assert!(
            oauth.completion != Completion::Callback || oauth.device.is_some(),
            "{} completes by callback but offers no device fallback",
            def.id
        );
    }
}

#[test]
fn every_row_signs_in_somehow() {
    for def in PROVIDERS {
        assert!(
            def.key.is_some() || def.oauth.is_some(),
            "{} has neither a key nor an OAuth sign-in",
            def.id
        );
        assert!(
            def.methods().next().is_some(),
            "{} offers no method",
            def.id
        );
    }
}

#[test]
fn methods_follow_the_row() {
    let methods = |id: &str| find(id).map(|def| def.methods().collect::<Vec<_>>());
    assert_eq!(methods("openai"), Some(vec![Method::ApiKey]));
    assert_eq!(
        methods("anthropic"),
        Some(vec![Method::ApiKey, Method::Browser])
    );
    assert_eq!(
        methods("openai-codex"),
        Some(vec![Method::Browser, Method::Device])
    );
    assert_eq!(methods("nobody"), None);
}

#[test]
fn a_vendor_family_is_named_by_its_first_row() {
    // The catalog names the provider of a route that carries only its family
    // by the first row of that family, so a preset must never precede the
    // vendor row of the Anthropic or Codex family.
    for (family, vendor) in [
        (Family::Codex, "openai-codex"),
        (Family::Anthropic, "anthropic"),
    ] {
        let first = PROVIDERS.iter().find(|def| def.family == family);
        assert_eq!(first.map(|def| def.id), Some(vendor), "{family:?}");
    }
}

#[test]
fn presets_are_https_key_rows_with_their_family_header_set() {
    for def in super::presets::PRESETS {
        let id = def.id;
        assert!(def.oauth.is_none(), "{id} has an OAuth sign-in");
        assert_eq!(def.methods().collect::<Vec<_>>(), [Method::ApiKey], "{id}");
        assert_eq!(def.transport, Transport::Https, "{id}");
        assert_eq!(def.shape, Shape::of_family(def.family), "{id}");
        assert!(
            (def.base_url.starts_with("https://")
                || def.base_url.starts_with("http://localhost:")
                || def.base_url.starts_with("http://127.0.0.1:")
                || def.base_url.starts_with("http://[::1]:"))
                && !def.base_url.ends_with('/'),
            "{id}: {} is not a bare https or loopback-http base",
            def.base_url
        );
        assert!(
            crate::http::check_base_url(def.family, def.base_url).is_ok(),
            "{id}: {} is not an admitted base URL",
            def.base_url
        );
        let key = def.key.expect("a preset takes an API key");
        if matches!(def.family, Family::Chat | Family::Responses) {
            assert_eq!(key.style, AuthStyle::Bearer, "{id}");
        }
        for name in key.env {
            assert!(
                name.starts_with(|first: char| first.is_ascii_uppercase())
                    && name
                        .bytes()
                        .all(|byte| matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_')),
                "{id}: {name} is not an environment variable name"
            );
        }
    }
}

#[test]
fn a_key_variable_is_shared_only_within_one_origin() {
    let origin = |def: &ProviderDef| {
        Url::parse(def.base_url)
            .expect("a table base URL parses")
            .origin()
    };
    let mut owners: BTreeMap<&str, Vec<&ProviderDef>> = BTreeMap::new();
    for def in PROVIDERS {
        for name in def.key.map_or(&[][..], |key| key.env) {
            owners.entry(*name).or_default().push(def);
        }
    }
    for (name, rows) in &owners {
        let origins: Vec<_> = rows.iter().map(|def| origin(def)).collect();
        assert!(
            origins.windows(2).all(|pair| pair[0] == pair[1]),
            "{name} would send one vendor's key to another host: {:?}",
            rows.iter().map(|def| def.id).collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_client_id_override_never_names_a_key_variable() {
    // The override's value is sent to the vendor as a public client id in
    // the authorize URL; a variable that also holds an API key would put
    // that secret in a browser URL.
    for def in PROVIDERS {
        let Some(name) = def.oauth.and_then(|oauth| oauth.client_id_env) else {
            continue;
        };
        assert!(
            name.starts_with(|first: char| first.is_ascii_uppercase())
                && name
                    .bytes()
                    .all(|byte| matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_')),
            "{}: {name} is not an environment variable name",
            def.id
        );
        for other in PROVIDERS {
            let keys = other.key.map_or(&[][..], |key| key.env);
            assert!(
                !keys.contains(&name),
                "{} reads {name} as a client id, but {} reads it as a key",
                def.id,
                other.id
            );
        }
    }
}

#[test]
fn every_row_decodes_to_the_entry_it_describes() {
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new("/tmp/dal-provider-def-test"),
        "",
        Some(""),
    )
    .expect("an empty dal.toml loads");
    let decoded = ProviderConfig::from_config(&config).expect("an empty config decodes");
    assert_eq!(decoded.providers.len(), PROVIDERS.len());
    for (entry, def) in decoded.providers.iter().zip(PROVIDERS) {
        let id = def.id;
        assert_eq!(&*entry.id, id);
        assert_eq!(entry.def.map(|row| row.id), Some(id), "{id}");
        assert_eq!(entry.family, def.family, "{id}");
        assert_eq!(&*entry.base_url, def.base_url, "{id}");
        assert_eq!(entry.transport, def.transport, "{id}");
        assert_eq!(
            entry.auth,
            def.key.map_or(AuthStyle::Bearer, |key| key.style),
            "{id}"
        );
        assert!(entry.key_env.is_none(), "{id}");
        assert_eq!(entry.shape(), def.shape, "{id}");
    }
}
