use std::{
    fs,
    sync::atomic::{AtomicU32, Ordering},
};

use dal_core::Family;

use super::*;
use crate::{AuthStyle, Transport};

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
    ProviderEntry {
        id: Box::from(id),
        def: crate::find(id),
        family: Family::Chat,
        base_url: Box::from("https://example.test"),
        transport: Transport::Https,
        key_env: key_env.map(Box::from),
        auth: AuthStyle::Bearer,
        max_concurrent_requests: 4,
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
    });
    assert!(matches!(
        store.set(OPENAI_CODEX, bare.clone()),
        Err(ProviderError::AuthWrite { .. })
    ));
    assert!(matches!(
        store.set("zenmux", bare.clone()),
        Err(ProviderError::AuthWrite { .. })
    ));
    store
        .set(ANTHROPIC, bare.clone())
        .expect("anthropic takes plain oauth");
    assert_eq!(store.credential(ANTHROPIC), Some(bare));
}

#[test]
fn debug_output_redacts_secrets() {
    let env = EnvSnapshot::test(&[("OPENAI_API_KEY", "SECRET-ENV")]);
    let shown = format!("{env:?} {:?}", codex_oauth(Some(1)));
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(shown.contains("OPENAI_API_KEY"), "{shown}");
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
