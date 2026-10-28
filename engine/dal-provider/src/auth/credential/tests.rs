use std::{
    fs,
    sync::atomic::{AtomicU32, Ordering},
};

use dal_core::Family;

use super::*;
use crate::{AuthStyle, ProviderSettings, Transport};

const OPENAI: &str = "openai";
const ANTHROPIC: &str = "anthropic";
const OPENAI_CODEX: &str = "openai-codex";
const FACTORY_DROID: &str = "factory-droid";

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "dal-provider-credential-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn auth(&self) -> PathBuf {
        self.0.join("auth.json")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn write_file(path: &Path, text: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, text).expect("write auth file");
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
}

#[cfg(not(unix))]
fn write_file(path: &Path, text: &str, _mode: u32) {
    fs::write(path, text).expect("write auth file");
}

fn entry(id: &str, key_env: Option<&str>) -> ProviderEntry {
    let def = crate::find(id);
    ProviderEntry {
        id: Box::from(id),
        def,
        family: Family::Chat,
        base_url: Box::from("https://example.test"),
        transport: Transport::Https,
        key_env: key_env.map(Box::from),
        auth: AuthStyle::Bearer,
        max_concurrent_requests: 4,
        settings: ProviderSettings::default(),
        origins: match def {
            Some(_) => crate::provider::Origins::default(),
            None => crate::provider::Origins {
                api: crate::provider::Origin::Explicit,
                base: crate::provider::Origin::Explicit,
                auth: crate::provider::Origin::RowDefault,
            },
        },
    }
}

fn jwt(payload: &str) -> String {
    format!(
        "{}.{}.not-a-signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(payload)
    )
}

fn load_error(text: &str) -> ProviderError {
    let dir = TestDir::new("invalid");
    write_file(&dir.auth(), text, 0o600);
    AuthStore::load(dir.auth()).expect_err("strict decode must fail")
}

fn codex_oauth(expires_at: Option<i64>) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("SECRET-A"),
        refresh_token: SecretString::from("SECRET-B"),
        expires_at,
        id_token: Some(jwt(
            r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct"}}"#,
        )),
        account_id: Some(String::from("acct")),
        project_id: None,
    })
}

#[test]
fn auth_json_write_mode() {
    let dir = TestDir::new("write");
    let mut store = AuthStore::load(dir.auth()).expect("missing file is empty");
    assert_eq!(store.credential(OPENAI), None);
    store
        .set(
            OPENAI,
            Credential::ApiKey {
                key: SecretString::from("sk-1"),
            },
        )
        .expect("openai takes a key");
    store
        .set(OPENAI_CODEX, codex_oauth(Some(1_789_000_000)))
        .expect("codex takes oauth");
    store.store().expect("store writes");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir.auth()).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let names: Vec<_> = fs::read_dir(&dir.0)
        .expect("list directory")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from("auth.json")]);

    let reloaded = AuthStore::load(dir.auth()).expect("reload");
    assert_eq!(reloaded.credential(OPENAI), store.credential(OPENAI));
    assert_eq!(
        reloaded.credential(OPENAI_CODEX),
        Some(codex_oauth(Some(1_789_000_000)))
    );
}

#[test]
fn stored_oauth_without_expiry_omits_the_member() {
    let dir = TestDir::new("no-expiry");
    let mut store = AuthStore::empty(dir.auth());
    store
        .set(OPENAI_CODEX, codex_oauth(None))
        .expect("codex takes oauth");
    store.store().expect("store writes");
    let text = fs::read_to_string(dir.auth()).expect("read back");
    assert!(!text.contains("expires_at"), "{text}");
    let reloaded = AuthStore::load(dir.auth()).expect("reload");
    assert_eq!(reloaded.credential(OPENAI_CODEX), Some(codex_oauth(None)));
}

#[cfg(unix)]
#[test]
fn group_readable_file_is_refused_with_the_chmod_fix() {
    let dir = TestDir::new("perms");
    write_file(
        &dir.auth(),
        r#"{"openai":{"kind":"api_key","key":"sk"}}"#,
        0o644,
    );
    let error = AuthStore::load(dir.auth()).expect_err("0644 is refused");
    assert_eq!(
        error.to_string(),
        "auth.json has group or other permissions"
    );
    assert_eq!(
        error.fix(),
        Some(format!("Run chmod 600 {}.", dir.auth().display()))
    );
}

#[cfg(windows)]
#[test]
#[ignore = "file_is_private is informational-only off unix until a Windows DACL probe lands"]
fn stored_file_is_owner_only_and_a_broadly_readable_file_is_refused() {
    let dir = TestDir::new("windows-acl");
    let mut store = AuthStore::empty(dir.auth());
    store
        .set(
            OPENAI,
            Credential::ApiKey {
                key: SecretString::from("sk-1"),
            },
        )
        .expect("openai takes a key");
    store.store().expect("store writes");
    AuthStore::load(dir.auth()).expect("a freshly stored file is private");

    grant_everyone_read(&dir.auth());
    let error = AuthStore::load(dir.auth()).expect_err("a world-readable file is refused");
    assert!(
        matches!(error, ProviderError::AuthFilePerms { .. }),
        "{error:?}"
    );
}

#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "test edge: widening the file DACL runs the OS utility"
)]
fn grant_everyone_read(path: &Path) {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(["/grant", "*S-1-1-0:R"])
        .output()
        .expect("run icacls");
    assert!(output.status.success(), "{output:?}");
}

#[cfg(unix)]
#[test]
fn symlink_is_refused_even_to_a_private_file() {
    let dir = TestDir::new("symlink");
    let target = dir.0.join("real.json");
    write_file(
        &target,
        r#"{"openai":{"kind":"api_key","key":"sk"}}"#,
        0o600,
    );
    std::os::unix::fs::symlink(&target, dir.auth()).expect("create symlink");
    let error = AuthStore::load(dir.auth()).expect_err("symlink is refused");
    assert!(
        matches!(error, ProviderError::AuthFileSymlink { .. }),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "auth.json is a symbolic link; dalgon does not follow it"
    );
}

#[test]
fn malformed_files_are_invalid_without_leaking_values() {
    let cases = [
        r#"{"openai":{"kind":"api_key","key":"SECRET-1" x}}"#,
        r#"{"openai":{"kind":"api_key","key":"SECRET-2","extra":1}}"#,
        r#"{"openai":{"kind":"api_key","key":"SECRET-3"},"zenmux":{}}"#,
        r#"{"openai":{"kind":"oauth","access_token":"SECRET-4","refresh_token":"r"}}"#,
        r#"{"anthropic":{"kind":"oauth","access_token":"SECRET-5","refresh_token":"r","expires_at":"SECRET-6"}}"#,
        r#"{"openai-codex":{"kind":"oauth","access_token":"SECRET-7","refresh_token":"r","id_token":"i"}}"#,
        r#"{"openai-codex":{"kind":"api_key","key":"SECRET-8"}}"#,
        "",
    ];
    for text in cases {
        let error = load_error(text);
        assert!(
            matches!(error, ProviderError::AuthFileInvalid { .. }),
            "{text}: {error:?}"
        );
        let shown = error.to_string();
        assert!(shown.starts_with("auth.json is not valid: "), "{shown}");
        assert!(
            !shown.contains("SECRET") && !format!("{error:?}").contains("SECRET"),
            "{shown}"
        );
    }
}

#[test]
fn credential_order_is_entry_then_environment_then_error() {
    let dir = TestDir::new("order");
    let mut store = AuthStore::empty(dir.auth());
    store
        .set(
            OPENAI,
            Credential::ApiKey {
                key: SecretString::from("stored"),
            },
        )
        .expect("openai takes a key");
    let env = EnvSnapshot::test(&[
        ("OPENAI_API_KEY", "from-env"),
        ("ANTHROPIC_API_KEY", "anthropic-env"),
        ("ZENMUX_API_KEY", "zenmux-env"),
        ("EMPTY_KEY", ""),
    ]);
    let key = |credential: Credential| match credential {
        Credential::ApiKey { key } => key.expose().to_owned(),
        other => panic!("expected an API key, got {other:?}"),
    };

    let openai = entry(OPENAI, None);
    assert_eq!(
        key(resolve(&openai, &store, &env).expect("entry")),
        "stored"
    );
    let anthropic = entry(ANTHROPIC, None);
    assert_eq!(
        key(resolve(&anthropic, &store, &env).expect("env")),
        "anthropic-env"
    );
    let zenmux = entry("zenmux", Some("ZENMUX_API_KEY"));
    assert_eq!(
        key(resolve(&zenmux, &store, &env).expect("key_env")),
        "zenmux-env"
    );

    let codex = entry(OPENAI_CODEX, None);
    let error = resolve(&codex, &store, &env).expect_err("codex ignores API-key variables");
    assert_eq!(
        error.to_string(),
        "openai-codex has no credentials: auth.json has no entry for it"
    );
    let empty = entry("blank", Some("EMPTY_KEY"));
    assert!(matches!(
        resolve(&empty, &store, &env),
        Err(ProviderError::NoCredentials { provider }) if provider == "blank"
    ));

    assert!(store.remove(OPENAI));
    let error = resolve(&openai, &store, &EnvSnapshot::default()).expect_err("neither source");
    assert_eq!(
        error.to_string(),
        "openai has no credentials: auth.json has no entry for it"
    );
    assert_eq!(error.fix().as_deref(), Some("Run dalgon login openai."));
}

#[test]
fn none_auth_style_overrides_stored_oauth_and_env_keys() {
    let dir = TestDir::new("none-override");
    let mut store = AuthStore::load(dir.auth()).expect("missing file is empty");
    let stored_oauth = codex_oauth(Some(1_789_000_000));
    store
        .set(OPENAI_CODEX, stored_oauth.clone())
        .expect("codex takes oauth");
    let stored_key = Credential::ApiKey {
        key: SecretString::from("stored-openai-key"),
    };
    store
        .set(OPENAI, stored_key.clone())
        .expect("openai takes a key");
    store.store().expect("store writes to disk");
    let store = AuthStore::load(dir.auth()).expect("reload from disk");

    let env = EnvSnapshot::test(&[
        ("OPENAI_API_KEY", "env-openai-key"),
        ("CODEX_KEY", "env-codex-key"),
        ("CUSTOM_KEY", "env-custom-key"),
    ]);

    let mut codex = entry(OPENAI_CODEX, Some("CODEX_KEY"));
    let mut openai = entry(OPENAI, None);
    let mut custom = entry("custom", Some("CUSTOM_KEY"));
    let mut unconfigured = entry("unconfigured", None);

    // Explicit AuthStyle::None yields Credential::None before stored lookup and env fallback:
    codex.auth = AuthStyle::None;
    assert_eq!(
        resolve(&codex, &store, &env).expect("none auth resolves"),
        Credential::None
    );

    openai.auth = AuthStyle::None;
    assert_eq!(
        resolve(&openai, &store, &env).expect("none auth resolves"),
        Credential::None
    );

    custom.auth = AuthStyle::None;
    assert_eq!(
        resolve(&custom, &store, &env).expect("none auth resolves"),
        Credential::None
    );

    unconfigured.auth = AuthStyle::None;
    assert_eq!(
        resolve(&unconfigured, &store, &env).expect("none auth resolves"),
        Credential::None
    );

    // Normal styles preserve store-first precedence and standard fallback / error:
    for normal in [AuthStyle::Bearer, AuthStyle::XApiKey, AuthStyle::ApiKey] {
        codex.auth = normal;
        assert_eq!(
            resolve(&codex, &store, &env).expect("normal style resolves"),
            stored_oauth,
            "stored oauth takes precedence under {normal:?}"
        );

        openai.auth = normal;
        assert_eq!(
            resolve(&openai, &store, &env).expect("normal style resolves"),
            stored_key,
            "stored key takes precedence under {normal:?}"
        );

        custom.auth = normal;
        assert_eq!(
            resolve(&custom, &store, &env).expect("normal style falls back to env"),
            Credential::ApiKey {
                key: SecretString::from("env-custom-key"),
            },
            "env fallback under {normal:?}"
        );

        unconfigured.auth = normal;
        let error = resolve(&unconfigured, &store, &env).expect_err("no credentials");
        assert!(
            matches!(
                error,
                ProviderError::NoCredentials { ref provider } if provider == "unconfigured"
            ),
            "expected NoCredentials under {normal:?}, got {error:?}"
        );
    }
}

#[test]
fn set_rejects_kinds_the_member_cannot_hold() {
    let mut store = AuthStore::empty("unused/auth.json");
    let oauth = codex_oauth(None);
    assert!(matches!(
        store.set(OPENAI, oauth.clone()),
        Err(ProviderError::AuthWrite { .. })
    ));
    assert!(matches!(
        store.set(ANTHROPIC, oauth),
        Err(ProviderError::AuthWrite { .. })
    ));
    let bare = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("a"),
        refresh_token: SecretString::from("r"),
        expires_at: None,
        id_token: None,
        account_id: None,
        project_id: None,
    });
    assert!(matches!(
        store.set(OPENAI_CODEX, bare.clone()),
        Err(ProviderError::AuthWrite { .. })
    ));
    assert!(matches!(
        store.set("zenmux", bare.clone()),
        Err(ProviderError::AuthWrite { .. })
    ));
    // The `WorkOS` device row takes no API key and binds no account: a Codex
    // sign-in, an account id alone, an ID token alone, and a key are refused
    // and leave the store untouched, while the plain sign-in the device flow
    // stores fits.
    let account_only = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("a"),
        refresh_token: SecretString::from("r"),
        expires_at: None,
        id_token: None,
        account_id: Some(String::from("acct")),
        project_id: None,
    });
    let id_token_only = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("a"),
        refresh_token: SecretString::from("r"),
        expires_at: None,
        id_token: Some(String::from("id")),
        account_id: None,
        project_id: None,
    });
    let key = Credential::ApiKey {
        key: SecretString::from("sk-droid"),
    };
    for refused in [codex_oauth(None), account_only, id_token_only, key] {
        assert!(matches!(
            store.set(FACTORY_DROID, refused),
            Err(ProviderError::AuthWrite { .. })
        ));
        assert_eq!(store.credential(FACTORY_DROID), None);
    }
    store
        .set(FACTORY_DROID, bare.clone())
        .expect("factory-droid takes plain oauth");
    assert_eq!(store.credential(FACTORY_DROID), Some(bare.clone()));
    store
        .set(ANTHROPIC, bare.clone())
        .expect("anthropic takes plain oauth");
    assert_eq!(store.credential(ANTHROPIC), Some(bare));
}

#[test]
fn a_rejected_set_keeps_the_old_entry_and_the_stored_file_loadable() {
    let dir = TestDir::new("rejected-set");
    let mut store = AuthStore::empty(dir.auth());
    let kept = Credential::ApiKey {
        key: SecretString::from("sk-kept"),
    };
    store.set(OPENAI, kept.clone()).expect("openai takes a key");
    assert!(matches!(
        store.set(OPENAI, codex_oauth(None)),
        Err(ProviderError::AuthWrite { .. })
    ));
    assert!(matches!(
        store.set(
            OPENAI_CODEX,
            Credential::ApiKey {
                key: SecretString::from("sk-wrong-member"),
            },
        ),
        Err(ProviderError::AuthWrite { .. })
    ));
    assert!(matches!(
        store.set(OPENAI, Credential::None),
        Err(ProviderError::AuthWrite { .. })
    ));
    assert_eq!(store.credential(OPENAI), Some(kept.clone()));
    assert_eq!(store.credential(OPENAI_CODEX), None);

    // A refused set must not leave a shape that load() would refuse, or the
    // next store() would write an auth.json the next start cannot read.
    store.store().expect("store writes");
    let reloaded = AuthStore::load(dir.auth()).expect("the stored file still loads");
    assert_eq!(reloaded.credential(OPENAI), Some(kept));
    assert_eq!(reloaded.credential(OPENAI_CODEX), None);
}

#[test]
fn expiring_flips_exactly_at_the_proactive_window_edge() {
    let at = |expires_at: Option<i64>| OAuthCredential {
        access_token: SecretString::from("a"),
        refresh_token: SecretString::from("r"),
        expires_at,
        id_token: None,
        account_id: None,
        project_id: None,
    };
    let now = 1_000_000;
    let window = crate::PROACTIVE_WINDOW_SECS;
    assert!(at(Some(now - 1)).expiring(now), "already expired");
    assert!(at(Some(now)).expiring(now), "expires this second");
    assert!(at(Some(now + window - 1)).expiring(now), "one inside");
    assert!(at(Some(now + window)).expiring(now), "the edge is inside");
    assert!(
        !at(Some(now + window + 1)).expiring(now),
        "one second past the edge is outside"
    );
    assert!(!at(None).expiring(now), "no expiry refreshes only on a 401");
    // The distance saturates instead of wrapping: a far-future token read at
    // a far-past clock stays outside, a far-past token at a far-future clock
    // stays inside.
    assert!(!at(Some(i64::MAX)).expiring(i64::MIN));
    assert!(at(Some(i64::MIN)).expiring(i64::MAX));
    assert!(!at(None).expiring(i64::MAX));
}

#[cfg(unix)]
#[test]
fn every_group_or_other_permission_bit_refuses_and_owner_only_modes_load() {
    let body = r#"{"openai":{"kind":"api_key","key":"sk"}}"#;
    for mode in [0o600, 0o400] {
        let dir = TestDir::new("owner-only");
        write_file(&dir.auth(), body, mode);
        AuthStore::load(dir.auth())
            .unwrap_or_else(|error| panic!("mode {mode:o} must load: {error}"));
    }
    for bit in [0o040, 0o020, 0o010, 0o004, 0o002, 0o001] {
        let mode = 0o600 | bit;
        let dir = TestDir::new("shared-bit");
        write_file(&dir.auth(), body, mode);
        let error = AuthStore::load(dir.auth()).expect_err("a shared bit is refused");
        assert!(
            matches!(error, ProviderError::AuthFilePerms { .. }),
            "mode {mode:o}: {error:?}"
        );
    }
}

#[test]
fn debug_output_redacts_secrets() {
    let env = EnvSnapshot::test(&[("OPENAI_API_KEY", "SECRET-ENV")])
        .with_cwd(PathBuf::from("/secret/cwd-value"));
    let shown = format!("{env:?} {:?}", codex_oauth(Some(1)));
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(!shown.contains("/secret/cwd-value"), "{shown}");
    assert!(shown.contains("OPENAI_API_KEY"), "{shown}");
}

#[test]
fn a_blank_stored_account_id_defers_to_the_id_token_claim() {
    let credential = |account_id: Option<&str>, id_token: Option<String>| OAuthCredential {
        access_token: SecretString::from("a"),
        refresh_token: SecretString::from("r"),
        expires_at: None,
        id_token,
        account_id: account_id.map(String::from),
        project_id: None,
    };
    let claim = jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-claim"}}"#);
    let blank_claim = jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"  "}}"#);
    assert_eq!(
        credential(Some("acct-stored"), Some(claim.clone()))
            .chatgpt_account_id()
            .as_deref(),
        Some("acct-stored"),
        "a stored id wins over the claim"
    );
    for blank in ["", " ", "\t\n"] {
        assert_eq!(
            credential(Some(blank), Some(claim.clone()))
                .chatgpt_account_id()
                .as_deref(),
            Some("acct-claim"),
            "a blank stored id {blank:?} defers to the claim"
        );
    }
    assert_eq!(
        credential(None, Some(claim))
            .chatgpt_account_id()
            .as_deref(),
        Some("acct-claim")
    );
    // A request must never go out with an empty account header, so a blank
    // id and a blank or missing claim leave no id at all.
    assert_eq!(credential(Some(" "), None).chatgpt_account_id(), None);
    assert_eq!(
        credential(Some(" "), Some(blank_claim)).chatgpt_account_id(),
        None
    );
}

#[test]
fn codex_identity_reads_claims_without_a_valid_signature() {
    let token = jwt(
        r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-1","chatgpt_user_id":"user-1","chatgpt_account_is_fedramp":true,"other":[1]},"email":"e"}"#,
    );
    assert_eq!(
        codex_identity(&token),
        Some(CodexIdentity {
            account_id: String::from("acct-1"),
            user_id: Some(String::from("user-1")),
            fedramp: true,
        })
    );

    let fallback =
        jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"a","user_id":"u"}}"#);
    let identity = codex_identity(&fallback).expect("identity");
    assert_eq!(identity.user_id.as_deref(), Some("u"));
    assert!(!identity.fedramp);

    let blank =
        jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"a","chatgpt_user_id":"  "}}"#);
    assert_eq!(codex_identity(&blank).expect("identity").user_id, None);

    let payload =
        URL_SAFE_NO_PAD.encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"a"}}"#);
    assert_eq!(codex_identity(&format!("h.{payload}")), None);
    assert_eq!(codex_identity(&format!("h.{payload}.s.x")), None);
    assert_eq!(
        codex_identity(&jwt(r#"{"https://api.openai.com/auth":{}}"#)),
        None
    );
    assert_eq!(codex_identity("h.%%%.s"), None);
}

#[test]
fn expiry_prefers_expires_in_then_jwt_exp_then_absent() {
    let with_exp = jwt(r#"{"exp":1789000000,"sub":"x"}"#);
    assert_eq!(oauth_expires_at(1_000, Some(3_600), &with_exp), Some(4_600));
    assert_eq!(
        oauth_expires_at(1_000, None, &with_exp),
        Some(1_789_000_000)
    );
    assert_eq!(oauth_expires_at(1_000, None, &jwt(r#"{"sub":"x"}"#)), None);
    assert_eq!(oauth_expires_at(1_000, None, "opaque-token"), None);
}

#[test]
fn issuance_counts_from_created_at_only_when_it_is_an_integer() {
    let value = |text: &str| sonic_rs::from_str::<sonic_rs::Value>(text).expect("valid JSON");
    assert_eq!(oauth_issued_at(None, 9), 9);
    assert_eq!(oauth_issued_at(Some(&value("100")), 9), 100);
    assert_eq!(oauth_issued_at(Some(&value("-5")), 9), -5);
    for text in ["null", r#""100""#, "1.5", "true", "18446744073709551615"] {
        assert_eq!(oauth_issued_at(Some(&value(text)), 9), 9, "{text}");
    }
}

/// Files written by the serializer before the member map replaced the three
/// fixed fields, one per entry shape and two with several members, then the
/// Cloud Code Assist members that carry a project (one with an expiry, two
/// together with none).
const FIXTURES: [(&str, &str); 10] = [
    (
        "openai_api_key",
        include_str!("fixtures/openai_api_key.json"),
    ),
    (
        "anthropic_api_key",
        include_str!("fixtures/anthropic_api_key.json"),
    ),
    (
        "anthropic_oauth",
        include_str!("fixtures/anthropic_oauth.json"),
    ),
    (
        "anthropic_oauth_no_expiry",
        include_str!("fixtures/anthropic_oauth_no_expiry.json"),
    ),
    (
        "openai_codex_oauth",
        include_str!("fixtures/openai_codex_oauth.json"),
    ),
    (
        "openai_codex_oauth_no_expiry",
        include_str!("fixtures/openai_codex_oauth_no_expiry.json"),
    ),
    ("all_members", include_str!("fixtures/all_members.json")),
    (
        "api_keys_and_codex",
        include_str!("fixtures/api_keys_and_codex.json"),
    ),
    (
        "google_cloud_code_assist_oauth",
        include_str!("fixtures/google_cloud_code_assist_oauth.json"),
    ),
    (
        "google_members",
        include_str!("fixtures/google_members.json"),
    ),
];

#[test]
fn every_stored_shape_loads_and_stores_to_the_same_bytes() {
    for (name, text) in FIXTURES {
        let dir = TestDir::new(name);
        write_file(&dir.auth(), text, 0o600);
        let store = AuthStore::load(dir.auth()).unwrap_or_else(|error| panic!("{name}: {error}"));
        store.store().expect("store");
        let written = fs::read_to_string(dir.auth()).expect("read back");
        assert_eq!(written, text, "{name}");
    }
}

#[test]
fn members_are_written_in_the_canonical_order_whatever_the_set_order() {
    let dir = TestDir::new("order");
    let mut store = AuthStore::empty(dir.auth());
    store
        .set(OPENAI_CODEX, codex_oauth(None))
        .expect("codex takes oauth");
    store
        .set(
            ANTHROPIC,
            Credential::ApiKey {
                key: SecretString::from("sk-ant-fixture"),
            },
        )
        .expect("anthropic takes a key");
    store
        .set(
            OPENAI,
            Credential::ApiKey {
                key: SecretString::from("sk-fixture-openai"),
            },
        )
        .expect("openai takes a key");
    let names: Vec<_> = store.status().into_iter().map(|row| row.provider).collect();
    assert_eq!(
        names,
        [OPENAI, ANTHROPIC, OPENAI_CODEX].map(Box::<str>::from)
    );
    let keys: Vec<_> = store.file.members.keys().map(|member| member.id).collect();
    assert_eq!(keys, [OPENAI, ANTHROPIC, OPENAI_CODEX]);
}

#[test]
fn an_unknown_or_repeated_member_is_refused_at_load() {
    for text in [
        r#"{"zenmux":{"kind":"api_key","key":"SECRET-1"}}"#,
        r#"{"openai":{"kind":"api_key","key":"SECRET-2"},"zenmux":{"kind":"api_key","key":"SECRET-3"}}"#,
        r#"{"openai":{"kind":"api_key","key":"SECRET-4"},"openai":{"kind":"api_key","key":"SECRET-5"}}"#,
        r#"{"openai-codex":{"kind":"oauth","access_token":"SECRET-6","refresh_token":"r"}}"#,
        r#"{"anthropic":{"kind":"oauth","access_token":"SECRET-7","refresh_token":"r","id_token":null}}"#,
    ] {
        let error = load_error(text);
        assert!(
            matches!(error, ProviderError::AuthFileInvalid { .. }),
            "{text}: {error:?}"
        );
        let shown = error.to_string();
        assert!(!shown.contains("SECRET"), "{shown}");
    }
    let unknown = load_error(r#"{"zenmux":{"kind":"api_key","key":"k"}}"#).to_string();
    assert!(unknown.contains("unknown field `zenmux`"), "{unknown}");
}

const CLOUD_CODE_ASSIST: &str = "google-cloud-code-assist";
const ANTIGRAVITY: &str = "google-antigravity";

/// A Cloud Code Assist sign-in as the login stores it: plain tokens and the
/// project discovery named.
fn cloud_code_oauth(project_id: Option<&str>) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("ya29.access"),
        refresh_token: SecretString::from("1//refresh"),
        expires_at: Some(1_789_000_000),
        id_token: None,
        account_id: None,
        project_id: project_id.map(String::from),
    })
}

#[test]
fn a_cloud_code_sign_in_stores_its_project_and_reopens_with_it() {
    let dir = TestDir::new("cloud-code-project");
    let mut store = AuthStore::empty(dir.auth());
    for (id, project) in [
        (CLOUD_CODE_ASSIST, "gemini-project"),
        (ANTIGRAVITY, "ag-project"),
    ] {
        store
            .set(id, cloud_code_oauth(Some(project)))
            .unwrap_or_else(|error| panic!("{id} takes a project: {error}"));
    }
    store.store().expect("store writes");
    let text = fs::read_to_string(dir.auth()).expect("read back");
    assert!(text.contains(r#""project_id":"gemini-project""#), "{text}");
    assert!(text.contains(r#""project_id":"ag-project""#), "{text}");
    let reloaded = AuthStore::load(dir.auth()).expect("reload");
    assert_eq!(
        reloaded.credential(CLOUD_CODE_ASSIST),
        Some(cloud_code_oauth(Some("gemini-project")))
    );
    assert_eq!(
        reloaded.credential(ANTIGRAVITY),
        Some(cloud_code_oauth(Some("ag-project")))
    );
    let written = fs::read_to_string(dir.auth()).expect("read back again");
    reloaded.store().expect("store again");
    assert_eq!(fs::read_to_string(dir.auth()).expect("read"), written);
}

#[test]
fn the_project_binds_to_the_cloud_code_rows_only() {
    let mut store = AuthStore::empty("unused/auth.json");
    // A Cloud Code Assist row stores a credential only after discovery named
    // its project: none, a blank one, or a Codex-shaped one are refused.
    let codex = codex_oauth(None);
    for refused in [
        cloud_code_oauth(None),
        cloud_code_oauth(Some("")),
        cloud_code_oauth(Some("  \t")),
        codex,
        Credential::ApiKey {
            key: SecretString::from("sk-none"),
        },
    ] {
        for id in [CLOUD_CODE_ASSIST, ANTIGRAVITY] {
            assert!(
                matches!(
                    store.set(id, refused.clone()),
                    Err(ProviderError::AuthWrite { .. })
                ),
                "{id}: {refused:?}"
            );
            assert_eq!(store.credential(id), None, "{id}");
        }
    }
    // Every other OAuth row refuses a project, whatever the tokens say.
    for id in [
        OPENAI_CODEX,
        ANTHROPIC,
        FACTORY_DROID,
        "gitlab-duo",
        "radius",
        "cursor",
        "devin",
    ] {
        assert!(
            matches!(
                store.set(id, cloud_code_oauth(Some("not-a-project-row"))),
                Err(ProviderError::AuthWrite { .. })
            ),
            "{id}"
        );
        assert_eq!(store.credential(id), None, "{id}");
    }
    // The plain sign-in the other rows store carries none and still fits.
    for id in [
        ANTHROPIC,
        FACTORY_DROID,
        "gitlab-duo",
        "radius",
        "cursor",
        "devin",
    ] {
        store
            .set(id, cloud_code_oauth(None))
            .unwrap_or_else(|error| panic!("{id} takes a plain sign-in: {error}"));
    }
}

#[test]
fn a_stored_cloud_code_member_without_a_usable_project_is_refused_at_load() {
    for text in [
        r#"{"google-cloud-code-assist":{"kind":"oauth","access_token":"SECRET-A","refresh_token":"SECRET-B"}}"#,
        r#"{"google-cloud-code-assist":{"kind":"oauth","access_token":"SECRET-C","refresh_token":"SECRET-D","project_id":""}}"#,
        r#"{"google-antigravity":{"kind":"oauth","access_token":"SECRET-E","refresh_token":"SECRET-F","project_id":"   "}}"#,
        r#"{"google-antigravity":{"kind":"oauth","access_token":"SECRET-G","refresh_token":"SECRET-H","project_id":null}}"#,
        r#"{"google-antigravity":{"kind":"api_key","key":"SECRET-I"}}"#,
        r#"{"anthropic":{"kind":"oauth","access_token":"SECRET-J","refresh_token":"SECRET-K","project_id":"p"}}"#,
        r#"{"openai-codex":{"kind":"oauth","access_token":"SECRET-L","refresh_token":"SECRET-M","id_token":"i","account_id":"a","project_id":"p"}}"#,
    ] {
        let error = load_error(text);
        assert!(
            matches!(error, ProviderError::AuthFileInvalid { .. }),
            "{text}: {error:?}"
        );
        let shown = error.to_string();
        assert!(!shown.contains("SECRET"), "{shown}");
    }
}

#[test]
fn members_stored_before_the_project_existed_still_read_and_write_back_unchanged() {
    // The pre-existing fixtures carry no `project_id`; they must read as
    // projectless credentials and write back without growing the member.
    for (name, text) in FIXTURES.iter().take(8) {
        let dir = TestDir::new(name);
        write_file(&dir.auth(), text, 0o600);
        let store = AuthStore::load(dir.auth()).unwrap_or_else(|error| panic!("{name}: {error}"));
        for status in store.status() {
            if let Some(Credential::OAuth(oauth)) = store.credential(&status.provider) {
                assert_eq!(oauth.project_id, None, "{name}: {}", status.provider);
            }
        }
        assert!(!text.contains("project_id"), "{name}");
    }
}

#[test]
fn debug_prints_the_project_and_redacts_the_tokens() {
    let shown = format!("{:?}", cloud_code_oauth(Some("visible-project")));
    assert!(shown.contains("visible-project"), "{shown}");
    assert!(
        !shown.contains("ya29.access") && !shown.contains("1//refresh"),
        "{shown}"
    );
}
