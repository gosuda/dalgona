//! Credentials: the environment snapshot, the strict `auth.json` store, and
//! the resolution order that picks one credential for a provider.
//!
//! The library reads no environment variables: the binary calls
//! [`EnvSnapshot::capture`] once and passes the value down. Secrets live in
//! [`SecretString`], whose `Debug` output is redacted and which has no
//! `Display`. `auth.json` holds one member per table provider, keyed by its
//! id; unknown members anywhere are rejected. Reads refuse a
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
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, DeserializeOwned, MapAccess, Visitor},
};

use crate::{Hook, PROVIDERS, ProviderDef, ProviderEntry, ProviderError, find};

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
/// The environment variable is the provider's configured `key_env`; a
/// built-in provider with an API-key row falls back to the row's variables.
/// An empty value counts as unset.
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
    let name_found = |name: &str| env.get(name).filter(|key| !key.is_empty());
    let key = match (
        provider.key_env.as_deref(),
        provider.def.and_then(|def| def.key),
    ) {
        (Some(name), _) => name_found(name),
        (None, Some(spec)) => spec.env.iter().find_map(|name| name_found(name)),
        (None, None) => None,
    };
    if let Some(key) = key {
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
        let def = find(provider)?;
        self.file
            .members
            .get(&MemberKey::of(def))
            .map(|stored| match stored {
                Stored::ApiKey { key } => Credential::ApiKey { key: key.clone() },
                Stored::Oauth {
                    access_token,
                    refresh_token,
                    expires_at,
                    id_token,
                    account_id,
                } => Credential::OAuth(OAuthCredential {
                    access_token: access_token.clone(),
                    refresh_token: refresh_token.clone(),
                    expires_at: *expires_at,
                    id_token: id_token.as_ref().map(|token| token.expose().to_owned()),
                    account_id: account_id.clone(),
                }),
            })
    }

    /// Returns stored credentials in stable provider order without exposing
    /// their contents.
    #[must_use]
    pub fn status(&self) -> Vec<AuthStatus> {
        self.file
            .members
            .iter()
            .map(|(key, stored)| AuthStatus {
                provider: Box::from(key.id),
                kind: match stored {
                    Stored::ApiKey { .. } => CredentialKind::ApiKey,
                    Stored::Oauth { .. } => CredentialKind::OAuth,
                },
            })
            .collect()
    }

    /// Replaces the in-memory entry for `provider`; [`AuthStore::store`]
    /// persists it.
    ///
    /// # Errors
    /// Returns [`ProviderError::AuthWrite`] when `provider` is not a table
    /// provider or the credential kind does not fit its row: an API key needs
    /// a key row, and an OAuth sign-in needs an OAuth row, carrying an ID
    /// token and account id exactly when the row's hook binds an account.
    pub fn set(&mut self, provider: &str, credential: Credential) -> Result<(), ProviderError> {
        let Some(def) = find(provider) else {
            return Err(ProviderError::AuthWrite {
                reason: format!("auth.json has no member for {provider}"),
            });
        };
        let stored = match credential {
            Credential::ApiKey { key } => Stored::ApiKey { key },
            Credential::OAuth(OAuthCredential {
                access_token,
                refresh_token,
                expires_at,
                id_token,
                account_id,
            }) => Stored::Oauth {
                access_token,
                refresh_token,
                expires_at,
                id_token: id_token.map(SecretString::from),
                account_id,
            },
            Credential::None => {
                return Err(ProviderError::AuthWrite {
                    reason: format!("{provider} does not take this credential kind"),
                });
            }
        };
        if !stored.fits(def) {
            return Err(ProviderError::AuthWrite {
                reason: format!("{provider} does not take this credential kind"),
            });
        }
        self.file.members.insert(MemberKey::of(def), stored);
        Ok(())
    }

    /// Drops the in-memory entry for `provider`; returns whether one existed.
    pub fn remove(&mut self, provider: &str) -> bool {
        find(provider).is_some_and(|def| self.file.members.remove(&MemberKey::of(def)).is_some())
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

/// The decoded `auth.json`: one member per provider id, keyed by the table.
///
/// Members are written in the canonical order of [`MemberKey`], which is the
/// order the file has always had, so a load followed by a store reproduces the
/// bytes.
#[derive(Clone, Debug, Default)]
struct AuthFile {
    members: BTreeMap<MemberKey, Stored>,
}

/// The position of a provider's member in `auth.json`: providers without
/// OAuth first, then OAuth providers, each by id.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MemberKey {
    oauth: bool,
    id: &'static str,
}

impl MemberKey {
    fn of(def: &ProviderDef) -> Self {
        Self {
            oauth: def.oauth.is_some(),
            id: def.id,
        }
    }
}

impl Serialize for AuthFile {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.members.iter().map(|(key, stored)| (key.id, stored)))
    }
}

impl<'de> Deserialize<'de> for AuthFile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(AuthFileVisitor)
    }
}

/// Decodes the member map strictly: a key must name a table provider, appear
/// once, and hold a member that provider can hold.
struct AuthFileVisitor;

impl<'de> Visitor<'de> for AuthFileVisitor {
    type Value = AuthFile;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an auth.json object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<AuthFile, A::Error> {
        let mut file = AuthFile::default();
        while let Some(key) = map.next_key::<Box<str>>()? {
            let Some(def) = find(&key) else {
                let ids: Vec<String> = PROVIDERS
                    .iter()
                    .map(|def| format!("`{}`", def.id))
                    .collect();
                return Err(de::Error::custom(format!(
                    "unknown field `{key}`, expected one of {}",
                    ids.join(", ")
                )));
            };
            let member = MemberKey::of(def);
            if file.members.contains_key(&member) {
                return Err(de::Error::custom(format!("duplicate field `{}`", def.id)));
            }
            let stored = map.next_value::<Stored>()?;
            if !stored.fits(def) {
                return Err(de::Error::custom(format!(
                    "{} does not take this credential kind",
                    def.id
                )));
            }
            file.members.insert(member, stored);
        }
        Ok(file)
    }
}

/// What `auth.json` holds for one provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Stored {
    ApiKey {
        key: SecretString,
    },
    Oauth {
        access_token: SecretString,
        refresh_token: SecretString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<i64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        id_token: Option<SecretString>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        account_id: Option<String>,
    },
}

impl Stored {
    /// Whether `def` can hold this member: an API key needs a key row, and an
    /// OAuth sign-in needs an OAuth row whose hook decides whether the grant
    /// carries an ID token and account id (both or neither).
    fn fits(&self, def: &ProviderDef) -> bool {
        match self {
            Self::ApiKey { .. } => def.key.is_some(),
            Self::Oauth {
                id_token,
                account_id,
                ..
            } => def.oauth.is_some_and(|oauth| {
                let account = oauth.hook == Hook::CodexAccount;
                id_token.is_some() == account && account_id.is_some() == account
            }),
        }
    }
}

/// Decodes a field that, when present, must not be `null`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
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
