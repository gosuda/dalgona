//! Credentials: the environment snapshot, the strict `auth.json` store, and
//! the resolution order that picks one credential for a provider.
//!
//! The library reads no environment variables: the binary calls
//! [`EnvSnapshot::capture`] once and passes the value down. Secrets live in
//! [`SecretString`], whose `Debug` output is redacted and which has no
//! `Display`. `auth.json` holds exactly the members `openai`, `anthropic`, and
//! `openai-codex`; unknown members anywhere are rejected. Reads refuse a
//! symbolic link and, on POSIX, any group or other permission bit. Writes go
//! through the store part's atomic writer at mode 0600.
//!
//! JWT claims are read without checking the signature. The values they yield
//! (expiry hints and the Codex account identity) only shape requests the
//! server authenticates itself; they never grant anything locally.

use std::{
    collections::HashMap,
    fmt,
    fs::{File, Metadata},
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dal_store::{FileMode, write_atomic};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};

use crate::{ProviderEntry, ProviderError};

const OPENAI: &str = "openai";
const ANTHROPIC: &str = "anthropic";
const OPENAI_CODEX: &str = "openai-codex";
const OPENAI_KEY_ENV: &str = "OPENAI_API_KEY";
const ANTHROPIC_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const OPENAI_AUTH_CLAIM: &str = "https://api.openai.com/auth";

/// A secret text value: an API key, access token, refresh token, or ID token.
///
/// `Debug` prints `<redacted>`, there is no `Display`, and the only way to the
/// text is [`SecretString::expose`], so a secret reaches a log only through an
/// explicit call.
#[derive(Clone, Eq, PartialEq)]
pub struct SecretString(Box<str>);

impl SecretString {
    /// The secret text, for building a request header or a token request.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value.into_boxed_str())
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(Box::from(value))
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<str>::deserialize(deserializer).map(Self)
    }
}

/// The process environment, captured once by the binary.
///
/// Variables whose name or value is not UTF-8 are left out. `Debug` lists the
/// variable names only, never their values.
#[derive(Clone, Default)]
pub struct EnvSnapshot {
    vars: HashMap<String, String>,
}

impl EnvSnapshot {
    /// Reads the process environment. The binary calls this once at startup;
    /// the library never calls it.
    #[must_use]
    pub fn capture() -> Self {
        let vars = std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        Self { vars }
    }

    /// The value of `key`, when the snapshot holds it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    /// A snapshot holding exactly `entries`; later duplicates win.
    #[must_use]
    pub fn test(entries: &[(&str, &str)]) -> Self {
        let vars = entries
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        Self { vars }
    }
}

impl fmt::Debug for EnvSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&str> = self.vars.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("EnvSnapshot").field("names", &names).finish()
    }
}

/// The credential a provider request carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Credential {
    /// An API key sent in the provider's configured header style.
    ApiKey {
        /// The key.
        key: SecretString,
    },
    /// An OAuth sign-in.
    OAuth(OAuthCredential),
    /// No credential; used by providers that need none, such as the scripted one.
    None,
}

/// An OAuth sign-in as stored in `auth.json`.
#[derive(Clone, Eq, PartialEq)]
pub struct OAuthCredential {
    /// The bearer access token.
    pub access_token: SecretString,
    /// The refresh token.
    pub refresh_token: SecretString,
    /// Unix seconds at which the access token expires; absent means refresh
    /// only on a 401.
    pub expires_at: Option<i64>,
    /// The Codex ID token; `None` for Anthropic.
    pub id_token: Option<String>,
    /// The `ChatGPT` account id; `None` for Anthropic.
    pub account_id: Option<String>,
}

impl fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("expires_at", &self.expires_at)
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("account_id", &self.account_id)
            .finish()
    }
}

/// The Codex account identity read from an ID token's claims.
///
/// The token signature is not checked: the values only fill request headers
/// that the server verifies against the access token it issued.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexIdentity {
    /// The `chatgpt_account_id` claim.
    pub account_id: String,
    /// The `chatgpt_user_id` claim, else the `user_id` claim; empty or
    /// white-space text is `None`.
    pub user_id: Option<String>,
    /// The `chatgpt_account_is_fedramp` claim; `false` when absent.
    pub fedramp: bool,
}

/// Reads the Codex identity from `id_token`.
///
/// Returns `None` when the token does not have exactly three `.`-separated
/// parts, the payload is not base64url JSON, or the account id claim is
/// missing or blank. Unknown claims are skipped; no signature is checked.
#[must_use]
pub fn codex_identity(id_token: &str) -> Option<CodexIdentity> {
    let claims = jwt_payload::<IdClaims>(id_token)?.auth?;
    let account_id = claims
        .chatgpt_account_id
        .filter(|id| !id.trim().is_empty())?;
    let user_id = claims
        .chatgpt_user_id
        .or(claims.user_id)
        .filter(|id| !id.trim().is_empty());
    Some(CodexIdentity {
        account_id,
        user_id,
        fedramp: claims.chatgpt_account_is_fedramp,
    })
}

/// The `expires_at` of a fresh OAuth token: `now + expires_in` when the token
/// response carried `expires_in`, otherwise the access-token JWT `exp`
/// claim, otherwise absent.
#[must_use]
pub fn oauth_expires_at(now: i64, expires_in: Option<i64>, access_token: &str) -> Option<i64> {
    match expires_in {
        Some(seconds) => Some(now.saturating_add(seconds)),
        None => jwt_payload::<ExpClaims>(access_token)?.exp,
    }
}

/// Picks the credential for `provider`: the `auth.json` entry, then the
/// environment snapshot, then [`ProviderError::NoCredentials`].
///
/// The environment variable is the provider's configured `key_env`; the
/// built-in `openai` and `anthropic` providers fall back to
/// `OPENAI_API_KEY` and `ANTHROPIC_API_KEY`. An empty value counts as unset.
///
/// # Errors
/// Returns [`ProviderError::NoCredentials`] when neither source has a
/// credential.
pub fn resolve(
    provider: &ProviderEntry,
    store: &AuthStore,
    env: &EnvSnapshot,
) -> Result<Credential, ProviderError> {
    if let Some(credential) = store.credential(&provider.id) {
        return Ok(credential);
    }
    let key_env = provider
        .key_env
        .as_deref()
        .or_else(|| match &*provider.id {
            OPENAI => Some(OPENAI_KEY_ENV),
            ANTHROPIC => Some(ANTHROPIC_KEY_ENV),
            _ => None,
        });
    if let Some(key) = key_env.and_then(|name| env.get(name)).filter(|key| !key.is_empty()) {
        return Ok(Credential::ApiKey {
            key: SecretString::from(key),
        });
    }
    Err(ProviderError::NoCredentials {
        provider: provider.id.to_string(),
    })
}

/// The decoded `auth.json` and the path it came from.
///
/// A missing file is an empty store; [`AuthStore::store`] creates it.
#[derive(Clone, Debug)]
pub struct AuthStore {
    path: PathBuf,
    file: AuthFile,
}

impl AuthStore {
    /// Reads and strictly decodes `auth.json` at `path`.
    ///
    /// # Errors
    /// [`ProviderError::AuthFileSymlink`] when `path` is a symbolic link (or
    /// is swapped while opening), [`ProviderError::AuthFilePerms`] when a
    /// group or other permission bit is set on POSIX, and
    /// [`ProviderError::AuthFileInvalid`] when the file cannot be read or is
    /// not the strict JSON shape. Error texts never carry a secret value.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ProviderError> {
        let path = path.into();
        let link = match std::fs::symlink_metadata(&path) {
            Ok(link) => link,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::empty(path)),
            Err(error) => return Err(invalid(&path, error.to_string())),
        };
        if link.file_type().is_symlink() {
            return Err(ProviderError::AuthFileSymlink { path });
        }
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::empty(path)),
            Err(error) => return Err(invalid(&path, error.to_string())),
        };
        let opened = file
            .metadata()
            .map_err(|error| invalid(&path, error.to_string()))?;
        if !same_file(&link, &opened) {
            return Err(ProviderError::AuthFileSymlink { path });
        }
        if !opened.is_file() {
            return Err(invalid(&path, String::from("not a regular file")));
        }
        if has_shared_bits(&opened) {
            return Err(ProviderError::AuthFilePerms { path });
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| invalid(&path, error.to_string()))?;
        let file = sonic_rs::from_slice::<AuthFile>(&bytes)
            .map_err(|error| invalid(&path, parse_message(&error)))?;
        Ok(Self { path, file })
    }

    /// An empty store that [`AuthStore::store`] writes to `path`.
    #[must_use]
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            file: AuthFile::default(),
        }
    }

    /// The `auth.json` path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored credential for `provider`, when `auth.json` has one.
    #[must_use]
    pub fn credential(&self, provider: &str) -> Option<Credential> {
        match provider {
            OPENAI => self.file.openai.as_ref().map(|entry| match entry {
                OpenAiEntry::ApiKey { key } => Credential::ApiKey { key: key.clone() },
            }),
            ANTHROPIC => self.file.anthropic.as_ref().map(|entry| match entry {
                AnthropicEntry::ApiKey { key } => Credential::ApiKey { key: key.clone() },
                AnthropicEntry::Oauth {
                    access_token,
                    refresh_token,
                    expires_at,
                } => Credential::OAuth(OAuthCredential {
                    access_token: access_token.clone(),
                    refresh_token: refresh_token.clone(),
                    expires_at: *expires_at,
                    id_token: None,
                    account_id: None,
                }),
            }),
            OPENAI_CODEX => self.file.openai_codex.as_ref().map(|entry| match entry {
                CodexEntry::Oauth {
                    access_token,
                    refresh_token,
                    expires_at,
                    id_token,
                    account_id,
                } => Credential::OAuth(OAuthCredential {
                    access_token: access_token.clone(),
                    refresh_token: refresh_token.clone(),
                    expires_at: *expires_at,
                    id_token: Some(id_token.expose().to_owned()),
                    account_id: Some(account_id.clone()),
                }),
            }),
            _ => None,
        }
    }

    /// Replaces the in-memory entry for `provider`; [`AuthStore::store`]
    /// persists it.
    ///
    /// # Errors
    /// Returns [`ProviderError::AuthWrite`] when `auth.json` has no member for
    /// `provider` or the credential kind does not fit it: `openai` takes an
    /// API key, `anthropic` an API key or an OAuth sign-in without ID token
    /// or account id, and `openai-codex` an OAuth sign-in with both.
    pub fn set(&mut self, provider: &str, credential: Credential) -> Result<(), ProviderError> {
        match (provider, credential) {
            (OPENAI, Credential::ApiKey { key }) => {
                self.file.openai = Some(OpenAiEntry::ApiKey { key });
            }
            (ANTHROPIC, Credential::ApiKey { key }) => {
                self.file.anthropic = Some(AnthropicEntry::ApiKey { key });
            }
            (
                ANTHROPIC,
                Credential::OAuth(OAuthCredential {
                    access_token,
                    refresh_token,
                    expires_at,
                    id_token: None,
                    account_id: None,
                }),
            ) => {
                self.file.anthropic = Some(AnthropicEntry::Oauth {
                    access_token,
                    refresh_token,
                    expires_at,
                });
            }
            (
                OPENAI_CODEX,
                Credential::OAuth(OAuthCredential {
                    access_token,
                    refresh_token,
                    expires_at,
                    id_token: Some(id_token),
                    account_id: Some(account_id),
                }),
            ) => {
                self.file.openai_codex = Some(CodexEntry::Oauth {
                    access_token,
                    refresh_token,
                    expires_at,
                    id_token: SecretString::from(id_token),
                    account_id,
                });
            }
            (OPENAI | ANTHROPIC | OPENAI_CODEX, _) => {
                return Err(ProviderError::AuthWrite {
                    reason: format!("{provider} does not take this credential kind"),
                });
            }
            _ => {
                return Err(ProviderError::AuthWrite {
                    reason: format!("auth.json has no member for {provider}"),
                });
            }
        }
        Ok(())
    }

    /// Drops the in-memory entry for `provider`; returns whether one existed.
    pub fn remove(&mut self, provider: &str) -> bool {
        match provider {
            OPENAI => self.file.openai.take().is_some(),
            ANTHROPIC => self.file.anthropic.take().is_some(),
            OPENAI_CODEX => self.file.openai_codex.take().is_some(),
            _ => false,
        }
    }

    /// Writes the store to its path atomically at mode 0600.
    ///
    /// # Errors
    /// Returns [`ProviderError::AuthWrite`] when encoding or the atomic write
    /// fails; a failure before the rename leaves the previous file unchanged.
    pub fn store(&self) -> Result<(), ProviderError> {
        let bytes = sonic_rs::to_vec(&self.file).map_err(|error| ProviderError::AuthWrite {
            reason: parse_message(&error),
        })?;
        write_atomic(&self.path, &bytes, FileMode::Mode0600).map_err(|error| {
            ProviderError::AuthWrite {
                reason: error.to_string(),
            }
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    openai: Option<OpenAiEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anthropic: Option<AnthropicEntry>,
    #[serde(
        default,
        rename = "openai-codex",
        skip_serializing_if = "Option::is_none"
    )]
    openai_codex: Option<CodexEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OpenAiEntry {
    ApiKey { key: SecretString },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AnthropicEntry {
    ApiKey {
        key: SecretString,
    },
    Oauth {
        access_token: SecretString,
        refresh_token: SecretString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<i64>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CodexEntry {
    Oauth {
        access_token: SecretString,
        refresh_token: SecretString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<i64>,
        id_token: SecretString,
        account_id: String,
    },
}

#[derive(Deserialize)]
struct ExpClaims {
    #[serde(default)]
    exp: Option<i64>,
}

#[derive(Deserialize)]
struct IdClaims {
    #[serde(default, rename = "https://api.openai.com/auth")]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_user_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    chatgpt_account_is_fedramp: bool,
}

/// Decodes the payload of a three-part JWT without checking its signature.
fn jwt_payload<T: DeserializeOwned>(token: &str) -> Option<T> {
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    sonic_rs::from_slice(&bytes).ok()
}

fn invalid(path: &Path, message: String) -> ProviderError {
    ProviderError::AuthFileInvalid {
        path: path.to_path_buf(),
        message,
    }
}

/// The parser message without its input excerpt and with every quoted value
/// replaced, so no secret from the file reaches the error text.
fn parse_message(error: &sonic_rs::Error) -> String {
    let text = error.to_string();
    let line = text.lines().next().unwrap_or_default();
    let mut out = String::with_capacity(line.len());
    let mut inside = false;
    let mut escaped = false;
    for ch in line.chars() {
        if !inside {
            out.push(ch);
            inside = ch == '"';
        } else if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            out.push_str("<redacted>\"");
            inside = false;
        }
    }
    if inside {
        out.push_str("<redacted>");
    }
    out
}

#[cfg(unix)]
fn same_file(link: &Metadata, opened: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    link.dev() == opened.dev() && link.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(link: &Metadata, opened: &Metadata) -> bool {
    let _ = (link, opened);
    true
}

#[cfg(unix)]
fn has_shared_bits(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o077 != 0
}

#[cfg(not(unix))]
fn has_shared_bits(metadata: &Metadata) -> bool {
    let _ = metadata;
    false
}

#[cfg(test)]
mod tests {
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
            id_token: Some(jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct"}}"#)),
            account_id: Some(String::from("acct")),
        })
    }

    #[test]
    fn auth_json_write_mode() {
        let dir = TestDir::new("write");
        let mut store = AuthStore::load(dir.auth()).expect("missing file is empty");
        assert_eq!(store.credential(OPENAI), None);
        store
            .set(OPENAI, Credential::ApiKey { key: SecretString::from("sk-1") })
            .expect("openai takes a key");
        store.set(OPENAI_CODEX, codex_oauth(Some(1_789_000_000))).expect("codex takes oauth");
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
        assert_eq!(reloaded.credential(OPENAI_CODEX), Some(codex_oauth(Some(1_789_000_000))));
    }

    #[test]
    fn stored_oauth_without_expiry_omits_the_member() {
        let dir = TestDir::new("no-expiry");
        let mut store = AuthStore::empty(dir.auth());
        store.set(OPENAI_CODEX, codex_oauth(None)).expect("codex takes oauth");
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
        write_file(&dir.auth(), r#"{"openai":{"kind":"api_key","key":"sk"}}"#, 0o644);
        let error = AuthStore::load(dir.auth()).expect_err("0644 is refused");
        assert_eq!(error.to_string(), "auth.json has group or other permissions");
        assert_eq!(error.fix(), Some(format!("Run chmod 600 {}.", dir.auth().display())));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_is_refused_even_to_a_private_file() {
        let dir = TestDir::new("symlink");
        let target = dir.0.join("real.json");
        write_file(&target, r#"{"openai":{"kind":"api_key","key":"sk"}}"#, 0o600);
        std::os::unix::fs::symlink(&target, dir.auth()).expect("create symlink");
        let error = AuthStore::load(dir.auth()).expect_err("symlink is refused");
        assert!(matches!(error, ProviderError::AuthFileSymlink { .. }), "{error:?}");
        assert_eq!(error.to_string(), "auth.json is a symbolic link; dalgon does not follow it");
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
            assert!(matches!(error, ProviderError::AuthFileInvalid { .. }), "{text}: {error:?}");
            let shown = error.to_string();
            assert!(shown.starts_with("auth.json is not valid: "), "{shown}");
            assert!(!shown.contains("SECRET") && !format!("{error:?}").contains("SECRET"), "{shown}");
        }
    }

    #[test]
    fn credential_order_is_entry_then_environment_then_error() {
        let dir = TestDir::new("order");
        let mut store = AuthStore::empty(dir.auth());
        store
            .set(OPENAI, Credential::ApiKey { key: SecretString::from("stored") })
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
        assert_eq!(key(resolve(&openai, &store, &env).expect("entry")), "stored");
        let anthropic = entry(ANTHROPIC, None);
        assert_eq!(key(resolve(&anthropic, &store, &env).expect("env")), "anthropic-env");
        let zenmux = entry("zenmux", Some("ZENMUX_API_KEY"));
        assert_eq!(key(resolve(&zenmux, &store, &env).expect("key_env")), "zenmux-env");

        let codex = entry(OPENAI_CODEX, None);
        let error = resolve(&codex, &store, &env).expect_err("codex ignores API-key variables");
        assert_eq!(error.to_string(), "openai-codex has no credentials: auth.json has no entry for it");
        let empty = entry("blank", Some("EMPTY_KEY"));
        assert!(matches!(
            resolve(&empty, &store, &env),
            Err(ProviderError::NoCredentials { provider }) if provider == "blank"
        ));

        assert!(store.remove(OPENAI));
        let error = resolve(&openai, &store, &EnvSnapshot::default()).expect_err("neither source");
        assert_eq!(error.to_string(), "openai has no credentials: auth.json has no entry for it");
        assert_eq!(error.fix().as_deref(), Some("Run dalgon login openai."));
    }

    #[test]
    fn set_rejects_kinds_the_member_cannot_hold() {
        let mut store = AuthStore::empty("unused/auth.json");
        let oauth = codex_oauth(None);
        assert!(matches!(store.set(OPENAI, oauth.clone()), Err(ProviderError::AuthWrite { .. })));
        assert!(matches!(store.set(ANTHROPIC, oauth), Err(ProviderError::AuthWrite { .. })));
        let bare = Credential::OAuth(OAuthCredential {
            access_token: SecretString::from("a"),
            refresh_token: SecretString::from("r"),
            expires_at: None,
            id_token: None,
            account_id: None,
        });
        assert!(matches!(store.set(OPENAI_CODEX, bare.clone()), Err(ProviderError::AuthWrite { .. })));
        assert!(matches!(store.set("zenmux", bare.clone()), Err(ProviderError::AuthWrite { .. })));
        store.set(ANTHROPIC, bare.clone()).expect("anthropic takes plain oauth");
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

        let fallback = jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"a","user_id":"u"}}"#);
        let identity = codex_identity(&fallback).expect("identity");
        assert_eq!(identity.user_id.as_deref(), Some("u"));
        assert!(!identity.fedramp);

        let blank = jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"a","chatgpt_user_id":"  "}}"#);
        assert_eq!(codex_identity(&blank).expect("identity").user_id, None);

        let payload = URL_SAFE_NO_PAD.encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"a"}}"#);
        assert_eq!(codex_identity(&format!("h.{payload}")), None);
        assert_eq!(codex_identity(&format!("h.{payload}.s.x")), None);
        assert_eq!(codex_identity(&jwt(r#"{"https://api.openai.com/auth":{}}"#)), None);
        assert_eq!(codex_identity("h.%%%.s"), None);
    }

    #[test]
    fn expiry_prefers_expires_in_then_jwt_exp_then_absent() {
        let with_exp = jwt(r#"{"exp":1789000000,"sub":"x"}"#);
        assert_eq!(oauth_expires_at(1_000, Some(3_600), &with_exp), Some(4_600));
        assert_eq!(oauth_expires_at(1_000, None, &with_exp), Some(1_789_000_000));
        assert_eq!(oauth_expires_at(1_000, None, &jwt(r#"{"sub":"x"}"#)), None);
        assert_eq!(oauth_expires_at(1_000, None, "opaque-token"), None);
    }
}
