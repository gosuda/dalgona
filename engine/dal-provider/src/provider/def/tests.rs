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
            let (Some(first_redirect), Some(second_redirect)) =
                (first_oauth.redirect, second_oauth.redirect)
            else {
                continue;
            };
            let same_loopback = first_redirect.host == second_redirect.host
                && first_redirect.port == second_redirect.port;
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
    assert_eq!(
        methods("radius"),
        Some(vec![Method::ApiKey, Method::Browser, Method::Device])
    );
    assert_eq!(
        methods("openrouter"),
        Some(vec![Method::ApiKey, Method::Browser])
    );
    assert_eq!(methods("google"), Some(vec![Method::ApiKey]));
    assert_eq!(methods("google-vertex"), Some(vec![Method::ApiKey]));
    assert_eq!(
        methods("cursor"),
        Some(vec![Method::ApiKey, Method::Browser])
    );
    assert_eq!(
        methods("devin"),
        Some(vec![Method::ApiKey, Method::Browser])
    );
    assert_eq!(
        methods("google-cloud-code-assist"),
        Some(vec![Method::Browser])
    );
    assert_eq!(methods("google-antigravity"), Some(vec![Method::Browser]));
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
        (Family::Cursor, "cursor"),
        (Family::Devin, "devin"),
        (Family::GoogleCloudCodeAssist, "google-cloud-code-assist"),
        (Family::GoogleAntigravity, "google-antigravity"),
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
        let expected = match def.id {
            "xai" | "meta" | "cloudflare-ai-gateway" | "opencode" | "opencode-go" | "bai" => {
                FamilyPolicy::Compatible
            }
            _ => FamilyPolicy::Native,
        };
        assert_eq!(def.family_policy, expected, "{}", def.id);
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
        assert!(
            oauth.client_id.is_some()
                || matches!(oauth.hook, Hook::OpenRouterKey | Hook::DevinCliToken),
            "{id}: missing client id"
        );
        https_endpoint(id, "authorize_url", oauth.authorize_url);
        https_endpoint(id, "token_url", oauth.token_url);
        if let Some(revoke) = oauth.revoke_url {
            https_endpoint(id, "revoke_url", revoke);
        }
        if let Some(device) = oauth.device {
            https_endpoint(id, "usercode_url", device.usercode_url);
            https_endpoint(id, "poll_url", device.poll_url);
            if let Some(page_url) = device.page_url {
                https_endpoint(id, "page_url", page_url);
            }
            if let Some(redirect_uri) = device.redirect_uri {
                https_endpoint(id, "redirect_uri", redirect_uri);
            }
        }
        let Some(redirect) = oauth.redirect else {
            continue;
        };
        assert!(
            matches!(redirect.host, "localhost" | "127.0.0.1"),
            "{id}: redirect host {} is not loopback",
            redirect.host
        );
        assert!(
            redirect.port == 0 || redirect.port >= 1024,
            "{id}: privileged redirect port"
        );
        assert!(redirect.path.starts_with('/'), "{id}: redirect path");
        for &(param, value) in oauth.query {
            let sent = oauth
                .query
                .iter()
                .filter(|(name, sent)| *name == param && *sent == value)
                .count();
            assert_eq!(sent, 1, "{id}: the authorize query must send {param} once");
        }
        // Every row pins PKCE with the S256 method except the two Cloud Code
        // Assist rows, whose installed clients the vendor registered with no
        // PKCE: they send `state` alone, and nothing else may drop it.
        let sends_challenge = oauth
            .query
            .contains(&("code_challenge", QueryValue::Challenge));
        assert_eq!(
            sends_challenge,
            !oauth.hook.binds_project(),
            "{id}: only a Cloud Code Assist row sends no PKCE challenge"
        );
        if sends_challenge {
            assert!(
                oauth
                    .query
                    .contains(&("code_challenge_method", QueryValue::Text("S256"))),
                "{id}: PKCE method is not S256"
            );
        } else {
            assert!(
                oauth
                    .query
                    .iter()
                    .all(|(name, _)| !name.starts_with("code_challenge")),
                "{id}: a row with no PKCE sends no challenge parameter"
            );
            assert!(
                oauth.query.contains(&("state", QueryValue::State)),
                "{id}: a row with no PKCE still guards the callback with state"
            );
        }
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
    assert!(def.oauth.is_some(), "openrouter offers browser OAuth");
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

#[test]
fn a_redirect_is_registered_exactly_for_the_redirecting_browser_rows() {
    // A browser row that completes through a redirect names the address the
    // vendor registered; a device-only row and a poll row have no redirect at
    // all, so no placeholder host, port, or path can reach a sign-in.
    for def in PROVIDERS {
        let Some(oauth) = def.oauth else { continue };
        let redirects = oauth.browser && oauth.completion != Completion::Poll;
        assert_eq!(oauth.redirect.is_some(), redirects, "{}", def.id);
        if let Some(redirect) = oauth.redirect {
            assert!(
                !redirect.host.is_empty() && !redirect.path.is_empty(),
                "{}: placeholder redirect",
                def.id
            );
        }
    }
    for id in ["cursor", "factory-droid"] {
        let oauth = find(id).and_then(|def| def.oauth).expect("an OAuth row");
        assert!(oauth.redirect.is_none(), "{id}");
    }
}

#[test]
fn hooks_split_credential_storage_from_refresh() {
    // (stores an OAuth credential, refreshes it) per hook; no wildcard, so a
    // new hook must choose both.
    for hook in [
        Hook::CodexAccount,
        Hook::ClaudeCode,
        Hook::WorkOsDevice,
        Hook::GitLabDuo,
        Hook::RadiusGateway,
        Hook::OpenRouterKey,
        Hook::CursorPoll,
        Hook::DevinCliToken,
        Hook::GoogleCloudCodeAssist,
        Hook::GoogleAntigravity,
        Hook::XaiDevice,
        Hook::MetaMuse,
        Hook::KimiCodeDevice,
    ] {
        let expected = match hook {
            Hook::CodexAccount
            | Hook::ClaudeCode
            | Hook::WorkOsDevice
            | Hook::GitLabDuo
            | Hook::RadiusGateway
            | Hook::CursorPoll
            | Hook::GoogleCloudCodeAssist
            | Hook::GoogleAntigravity
            | Hook::XaiDevice
            | Hook::MetaMuse
            | Hook::KimiCodeDevice => (true, true),
            Hook::DevinCliToken => (true, false),
            Hook::OpenRouterKey => (false, false),
        };
        assert_eq!(
            (hook.stores_oauth(), hook.refreshes()),
            expected,
            "{hook:?}"
        );
    }
}

#[test]
fn only_the_kimi_hook_sends_a_client_identity() {
    for hook in [
        Hook::CodexAccount,
        Hook::ClaudeCode,
        Hook::WorkOsDevice,
        Hook::GitLabDuo,
        Hook::RadiusGateway,
        Hook::OpenRouterKey,
        Hook::CursorPoll,
        Hook::DevinCliToken,
        Hook::GoogleCloudCodeAssist,
        Hook::GoogleAntigravity,
        Hook::XaiDevice,
        Hook::MetaMuse,
        Hook::KimiCodeDevice,
    ] {
        assert_eq!(
            hook.sends_client_identity(),
            hook == Hook::KimiCodeDevice,
            "{hook:?}"
        );
    }
}

#[test]
fn the_cursor_row_pins_the_poll_grant_and_its_refresh_client() {
    let def = find("cursor").expect("the cursor row is registered");
    assert_eq!(def.name, "Cursor");
    assert_eq!(def.family, Family::Cursor);
    assert_eq!(def.family_policy, FamilyPolicy::Native);
    assert_eq!(def.base_url, "https://api2.cursor.sh");
    assert_eq!(def.transport, Transport::Https);
    assert_eq!(def.shape, Shape::Plain);
    let key = def.key.expect("cursor takes an access token as a key");
    assert_eq!(key.style, AuthStyle::Bearer);
    assert_eq!(key.env, ["CURSOR_ACCESS_TOKEN", "CURSOR_API_KEY"]);
    let oauth = def.oauth.expect("cursor signs in by its poll grant");
    assert_eq!(oauth.client_id, Some("KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB"));
    assert_eq!(oauth.client_id_env, None);
    assert_eq!(oauth.authorize_url, "https://cursor.com/loginDeepControl");
    assert_eq!(oauth.token_url, "https://api2.cursor.sh/oauth/token");
    assert!(oauth.scopes.is_empty() && oauth.query.is_empty());
    assert!(oauth.redirect.is_none());
    assert_eq!(oauth.completion, Completion::Poll);
    assert!(oauth.browser);
    assert!(oauth.device.is_none() && oauth.revoke_url.is_none());
    assert_eq!(oauth.hook, Hook::CursorPoll);
}

#[test]
fn the_devin_row_pins_the_cli_grant_without_a_client_or_refresh() {
    let def = find("devin").expect("the devin row is registered");
    assert_eq!(def.name, "Devin");
    assert_eq!(def.family, Family::Devin);
    assert_eq!(def.family_policy, FamilyPolicy::Native);
    assert_eq!(def.base_url, "https://server.codeium.com");
    assert_eq!(def.transport, Transport::Https);
    assert_eq!(def.shape, Shape::Plain);
    let key = def.key.expect("devin takes an API key");
    assert_eq!(key.env, ["DEVIN_API_KEY"]);
    let oauth = def.oauth.expect("devin signs in by its CLI grant");
    assert_eq!(oauth.client_id, None);
    assert_eq!(oauth.client_id_env, None);
    assert_eq!(
        oauth.authorize_url,
        "https://app.devin.ai/auth/cli/continue"
    );
    assert_eq!(oauth.token_url, "https://api.devin.ai/auth/cli/token");
    assert_eq!(
        oauth.query,
        [
            ("response_type", QueryValue::Text("code")),
            ("redirect_uri", QueryValue::RedirectUri),
            ("code_challenge", QueryValue::Challenge),
            ("code_challenge_method", QueryValue::Text("S256")),
            ("state", QueryValue::State),
            ("prompt", QueryValue::Text("select_account")),
        ]
    );
    let redirect = oauth.redirect.expect("devin registers a loopback callback");
    assert_eq!(
        (redirect.host, redirect.port, redirect.path),
        ("127.0.0.1", 59653, "/callback")
    );
    assert_eq!(oauth.completion, Completion::PasteCode);
    assert!(oauth.browser);
    assert!(oauth.device.is_none() && oauth.revoke_url.is_none());
    assert_eq!(oauth.hook, Hook::DevinCliToken);
    assert!(oauth.hook.stores_oauth() && !oauth.hook.refreshes());
}

/// The SHA-256 of a pinned credential, as lowercase hex.
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;

    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[test]
fn the_google_cloud_code_rows_pin_the_installed_application_grants() {
    // Each row pins the SHA-256 digest of its client id and secret. Any edit to
    // the row changes its digest, so the fixture proves byte-for-byte
    // transcription without storing the credential itself.
    for (id, name, family, base, client_sha, secret_sha, scopes, port, path, hook) in [
        (
            "google-cloud-code-assist",
            "Google Cloud Code Assist (Gemini CLI)",
            Family::GoogleCloudCodeAssist,
            "https://cloudcode-pa.googleapis.com",
            "636d7183f3ca4ad92c7803811a3f635643f1da657a22eeedf280283161cb6e17",
            "6a5f78b8b99dd4025e41ba11bf54c304c6af29f924c5569cc7865b2428ce03a9",
            &[
                "https://www.googleapis.com/auth/cloud-platform",
                "https://www.googleapis.com/auth/userinfo.email",
                "https://www.googleapis.com/auth/userinfo.profile",
            ][..],
            8085,
            "/oauth2callback",
            Hook::GoogleCloudCodeAssist,
        ),
        (
            "google-antigravity",
            "Antigravity (Gemini 3, Claude, GPT-OSS)",
            Family::GoogleAntigravity,
            "https://daily-cloudcode-pa.googleapis.com",
            "bf00c418024ba6bf606ccdc37120976e41bc429dd1d46ecf16a729aa532626ea",
            "1d2f041093fd95aa8995a038c711d50a7960da09a505381c09a745d6ad0ecc60",
            &[
                "https://www.googleapis.com/auth/cloud-platform",
                "https://www.googleapis.com/auth/userinfo.email",
                "https://www.googleapis.com/auth/userinfo.profile",
                "https://www.googleapis.com/auth/cclog",
                "https://www.googleapis.com/auth/experimentsandconfigs",
            ][..],
            51121,
            "/oauth-callback",
            Hook::GoogleAntigravity,
        ),
    ] {
        let def = find(id).unwrap_or_else(|| panic!("the {id} row is registered"));
        assert_eq!(def.name, name, "{id}");
        assert_eq!(def.family, family, "{id}");
        assert_eq!(def.family_policy, FamilyPolicy::Native, "{id}");
        assert_eq!(def.base_url, base, "{id}");
        assert_eq!(def.transport, Transport::Https, "{id}");
        assert_eq!(def.shape, Shape::Plain, "{id}");
        assert!(def.key.is_none(), "{id} takes no API key");
        let oauth = def
            .oauth
            .unwrap_or_else(|| panic!("{id} signs in with OAuth"));
        assert_eq!(oauth.hook, hook, "{id}");
        assert!(oauth.hook.binds_project() && oauth.hook.stores_oauth() && oauth.hook.refreshes());
        let client_id = oauth.client_id.expect("a registered client id");
        assert_eq!(digest(client_id.as_bytes()), client_sha, "{id}");
        assert!(client_id.ends_with(".apps.googleusercontent.com"), "{id}");
        assert_eq!(oauth.client_id_env, None, "{id}");
        let secret = oauth
            .client_secret
            .expect("a registered installed-app secret");
        assert_eq!(digest(secret.as_bytes()), secret_sha, "{id}");
        assert!(secret.starts_with("GOCSPX-"), "{id}");
        assert_eq!(
            oauth.authorize_url, "https://accounts.google.com/o/oauth2/v2/auth",
            "{id}"
        );
        assert_eq!(
            oauth.token_url, "https://oauth2.googleapis.com/token",
            "{id}"
        );
        assert_eq!(oauth.scopes, scopes, "{id}");
        assert_eq!(
            oauth.query,
            [
                ("client_id", QueryValue::ClientId),
                ("response_type", QueryValue::Text("code")),
                ("redirect_uri", QueryValue::RedirectUri),
                ("scope", QueryValue::Scope),
                ("state", QueryValue::State),
                ("access_type", QueryValue::Text("offline")),
                ("prompt", QueryValue::Text("consent")),
            ],
            "{id}"
        );
        let redirect = oauth.redirect.expect("a registered loopback callback");
        assert_eq!(
            (redirect.host, redirect.port, redirect.path),
            ("127.0.0.1", port, path),
            "{id}"
        );
        assert_eq!(oauth.completion, Completion::PasteCode, "{id}");
        assert!(oauth.browser, "{id}");
        assert!(oauth.device.is_none() && oauth.revoke_url.is_none(), "{id}");
    }
    // Every other row carries no client secret: only an installed-app client
    // registers one.
    for def in PROVIDERS {
        let Some(oauth) = def.oauth else { continue };
        assert_eq!(
            oauth.client_secret.is_some(),
            oauth.hook.binds_project(),
            "{}",
            def.id
        );
    }
}
