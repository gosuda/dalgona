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
    collections::{BTreeMap, HashMap},
    ffi::OsString,
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

    /// Builds a snapshot from the process environment values already captured
    /// by the caller. Non-UTF-8 names and values are omitted.
    #[must_use]
    pub fn from_vars(vars: &BTreeMap<OsString, OsString>) -> Self {
        let vars = vars
            .iter()
            .filter_map(|(name, value)| {
                Some((name.to_str()?.to_owned(), value.to_str()?.to_owned()))
            })
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
        f.debug_struct("EnvSnapshot")
            .field("names", &names)
            .finish()
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
/// The kind of a stored provider credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialKind {
    /// A provider API key.
    ApiKey,
    /// An OAuth sign-in.
    OAuth,
}

impl fmt::Display for CredentialKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ApiKey => "API key",
            Self::OAuth => "OAuth",
        })
    }
}

/// Secret-free information about one stored provider credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthStatus {
    /// The configured provider id.
    pub provider: Box<str>,
    /// The stored credential kind.
    pub kind: CredentialKind,
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

impl OAuthCredential {
    /// Whether the access token is inside the proactive refresh window at
    /// `now` (Unix seconds). A credential without an expiry is refreshed
    /// only after a 401, so it is never expiring.
    #[must_use]
    pub fn expiring(&self, now: i64) -> bool {
        self.expires_at
            .is_some_and(|at| at.saturating_sub(now) <= crate::auth::refresh::PROACTIVE_WINDOW_SECS)
    }
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
    let key_env = provider.key_env.as_deref().or(match &*provider.id {
        OPENAI => Some(OPENAI_KEY_ENV),
        ANTHROPIC => Some(ANTHROPIC_KEY_ENV),
        _ => None,
    });
    if let Some(key) = key_env
        .and_then(|name| env.get(name))
        .filter(|key| !key.is_empty())
    {
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

    /// Returns stored credentials in stable provider order without exposing
    /// their contents.
    #[must_use]
    pub fn status(&self) -> Vec<AuthStatus> {
        let mut statuses = Vec::with_capacity(3);
        if self.file.openai.is_some() {
            statuses.push(AuthStatus {
                provider: Box::from(OPENAI),
                kind: CredentialKind::ApiKey,
            });
        }
        if let Some(entry) = &self.file.anthropic {
            statuses.push(AuthStatus {
                provider: Box::from(ANTHROPIC),
                kind: match entry {
                    AnthropicEntry::ApiKey { .. } => CredentialKind::ApiKey,
                    AnthropicEntry::Oauth { .. } => CredentialKind::OAuth,
                },
            });
        }
        if self.file.openai_codex.is_some() {
            statuses.push(AuthStatus {
                provider: Box::from(OPENAI_CODEX),
                kind: CredentialKind::OAuth,
            });
        }
        statuses
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
mod tests;
