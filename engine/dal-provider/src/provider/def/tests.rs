use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

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
    assert_eq!(methods("factory-droid"), Some(vec![Method::Device]));
    assert_eq!(
        methods("gitlab-duo"),
        Some(vec![Method::ApiKey, Method::Browser])
    );
    assert_eq!(methods("radius"), Some(vec![Method::ApiKey]));
    assert_eq!(methods("openrouter"), Some(vec![Method::ApiKey]));
    assert_eq!(methods("google"), Some(vec![Method::ApiKey]));
    assert_eq!(methods("google-vertex"), Some(vec![Method::ApiKey]));
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
        (Family::Droid, "factory-droid"),
        (Family::GitLab, "gitlab-duo"),
        (Family::PiMessages, "radius"),
        (Family::GoogleGenerativeAi, "google"),
        (Family::GoogleVertex, "google-vertex"),
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

#[test]
fn the_google_rows_key_sign_in_only_with_their_wire_headers() {
    for (id, name, family, base, env) in [
        (
            "google",
            "Google Gemini",
            Family::GoogleGenerativeAi,
            "https://generativelanguage.googleapis.com/v1beta",
            ["GEMINI_API_KEY"],
        ),
        (
            "google-vertex",
            "Google Vertex AI",
            Family::GoogleVertex,
            "https://aiplatform.googleapis.com",
            ["GOOGLE_CLOUD_API_KEY"],
        ),
    ] {
        let def = find(id).unwrap_or_else(|| panic!("the {id} row is registered"));
        assert_eq!(def.id, id);
        assert_eq!(def.name, name);
        assert_eq!(def.family, family, "{id}");
        assert_eq!(def.base_url, base, "{id}");
        assert_eq!(def.transport, Transport::Https, "{id}");
        assert!(def.oauth.is_none(), "{id} signs in by key only");
        let key = def.key.unwrap_or_else(|| panic!("{id} takes an API key"));
        // The Google adapters hardcode their own key header, so the row
        // style never reaches a request header; it only labels the row.
        assert_eq!(key.style, AuthStyle::XApiKey, "{id}");
        assert_eq!(key.env, env, "{id}");
        assert_eq!(def.shape, Shape::Plain, "{id}");
        assert!(
            crate::http::check_base_url(def.family, def.base_url).is_ok(),
            "{id}: {} is not an admitted base URL",
            def.base_url
        );
    }
}

#[test]
fn every_row_id_is_a_kebab_word_and_none_is_a_reserved_provider_id() {
    for def in PROVIDERS {
        let id = def.id;
        assert!(
            id.starts_with(|first: char| first.is_ascii_lowercase()),
            "{id} does not start with a letter"
        );
        assert!(
            !id.ends_with('-') && !id.contains("--"),
            "{id} has an empty hyphenated word"
        );
        // `scripted` selects the replay fixture and `dal` is refused as a
        // named provider; a row under either id would be decoded twice.
        assert!(
            !matches!(id, "dal" | "scripted"),
            "{id} is a reserved provider id"
        );
    }
}

#[test]
fn the_table_lists_the_vendor_rows_then_the_presets_by_id() {
    let presets = super::presets::PRESETS;
    assert_eq!(PROVIDERS.len(), VENDOR_ROWS.len() + presets.len());
    let table_ids: Vec<&str> = PROVIDERS.iter().map(|def| def.id).collect();
    let listed: Vec<&str> = VENDOR_ROWS
        .iter()
        .chain(presets)
        .map(|def| def.id)
        .collect();
    assert_eq!(table_ids, listed);
    for pair in presets.windows(2) {
        assert!(
            pair[0].id < pair[1].id,
            "{} is listed before {}",
            pair[0].id,
            pair[1].id
        );
    }
    for def in VENDOR_ROWS {
        assert_eq!(def.family_policy, FamilyPolicy::Native, "{}", def.id);
    }
    for def in presets {
        assert_eq!(def.family_policy, FamilyPolicy::Compatible, "{}", def.id);
    }
}

#[test]
fn row_display_names_are_distinct_trimmed_text() {
    let mut seen = BTreeSet::new();
    for def in PROVIDERS {
        let name = def.name;
        assert!(!name.is_empty(), "{} has no display name", def.id);
        assert_eq!(name, name.trim(), "{}: {name:?} is not trimmed", def.id);
        assert!(
            !name.chars().any(char::is_control),
            "{}: {name:?} holds a control character",
            def.id
        );
        assert!(
            seen.insert(name),
            "{}: {name:?} names two rows in sign-in messages",
            def.id
        );
    }
}

#[test]
fn every_key_row_names_at_least_one_distinct_key_variable() {
    for def in PROVIDERS {
        let Some(key) = def.key else { continue };
        assert!(
            !key.env.is_empty(),
            "{} takes a key but names no variable to read it from",
            def.id
        );
        let distinct: BTreeSet<_> = key.env.iter().collect();
        assert_eq!(
            distinct.len(),
            key.env.len(),
            "{} lists a variable twice",
            def.id
        );
    }
}

#[test]
fn every_row_base_url_is_a_bare_admitted_endpoint_with_no_embedded_secret() {
    for def in PROVIDERS {
        let id = def.id;
        let admitted = crate::http::check_base_url(def.family, def.base_url);
        assert!(
            admitted.is_ok(),
            "{id}: {} is refused: {admitted:?}",
            def.base_url
        );
        assert!(!def.base_url.ends_with('/'), "{id}: {}", def.base_url);
        let url = Url::parse(def.base_url).expect("a table base URL parses");
        assert!(url.host_str().is_some(), "{id}: no host");
        assert!(url.username().is_empty(), "{id}: user name in the URL");
        assert!(url.password().is_none(), "{id}: password in the URL");
        assert!(url.query().is_none(), "{id}: query in the URL");
        assert!(url.fragment().is_none(), "{id}: fragment in the URL");
    }
}

#[test]
fn oauth_rows_use_https_endpoints_pkce_state_and_an_unprivileged_loopback_redirect() {
    let https_endpoint = |id: &str, what: &str, endpoint: &str| {
        let url = Url::parse(endpoint).unwrap_or_else(|error| panic!("{id} {what}: {error}"));
        assert_eq!(url.scheme(), "https", "{id} {what}: {endpoint}");
        assert!(url.host_str().is_some(), "{id} {what}: no host");
        assert!(
            url.username().is_empty() && url.password().is_none(),
            "{id} {what}: credentials in the URL"
        );
        assert!(url.fragment().is_none(), "{id} {what}: fragment in the URL");
    };
    for def in PROVIDERS {
        let Some(oauth) = def.oauth else { continue };
        let id = def.id;
        assert!(!oauth.client_id.is_empty(), "{id}: empty client id");
        https_endpoint(id, "authorize_url", oauth.authorize_url);
        https_endpoint(id, "token_url", oauth.token_url);
        if let Some(revoke) = oauth.revoke_url {
            https_endpoint(id, "revoke_url", revoke);
        }
        if let Some(device) = oauth.device {
            https_endpoint(id, "usercode_url", device.usercode_url);
            https_endpoint(id, "poll_url", device.poll_url);
            https_endpoint(id, "page_url", device.page_url);
            if !device.redirect_uri.is_empty() {
                https_endpoint(id, "redirect_uri", device.redirect_uri);
            }
        }
        if !oauth.browser {
            continue;
        }
        assert!(
            matches!(oauth.redirect.host, "localhost" | "127.0.0.1"),
            "{id}: redirect host {} is not loopback",
            oauth.redirect.host
        );
        assert!(
            oauth.redirect.port >= 1024,
            "{id}: privileged redirect port"
        );
        assert!(oauth.redirect.path.starts_with('/'), "{id}: redirect path");
        for (param, value) in [
            ("client_id", QueryValue::ClientId),
            ("redirect_uri", QueryValue::RedirectUri),
            ("code_challenge", QueryValue::Challenge),
            ("state", QueryValue::State),
        ] {
            let sent = oauth
                .query
                .iter()
                .filter(|(name, sent)| *name == param && *sent == value)
                .count();
            assert_eq!(sent, 1, "{id}: the authorize query must send {param} once");
        }
        assert!(
            oauth
                .query
                .contains(&("code_challenge_method", QueryValue::Text("S256"))),
            "{id}: PKCE method is not S256"
        );
    }
}

#[test]
fn github_copilot_offers_device_sign_in_without_an_oauth_row() {
    let copilot = find("github-copilot").expect("the github-copilot row is registered");
    assert!(copilot.oauth.is_none());
    assert_eq!(
        copilot.methods().collect::<Vec<_>>(),
        [Method::ApiKey, Method::Device]
    );
    assert!(copilot.offers(Method::ApiKey));
    assert!(!copilot.offers(Method::Browser));
    assert!(copilot.offers(Method::Device));
    let droid = find("factory-droid").expect("the factory-droid row is registered");
    assert!(!droid.offers(Method::ApiKey));
    assert!(!droid.offers(Method::Browser));
    assert!(droid.offers(Method::Device));
    let openai = find("openai").expect("the openai row is registered");
    assert!(openai.offers(Method::ApiKey));
    assert!(!openai.offers(Method::Device));
}

#[test]
fn the_openrouter_row_defaults_to_responses_with_a_bearer_key() {
    let def = find("openrouter").expect("the openrouter row is registered");
    assert_eq!(def.id, "openrouter");
    assert_eq!(def.name, "OpenRouter");
    assert_eq!(def.family, Family::Responses);
    assert_eq!(def.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(def.transport, Transport::Https);
    assert!(def.oauth.is_none(), "openrouter signs in by key only");
    let key = def.key.expect("openrouter takes an API key");
    assert_eq!(key.style, AuthStyle::Bearer);
    assert_eq!(key.env, ["OPENROUTER_API_KEY"]);
    assert_eq!(def.shape, Shape::Plain);
    assert!(
        crate::http::check_base_url(def.family, def.base_url).is_ok(),
        "{} is not an admitted base URL",
        def.base_url
    );
}
