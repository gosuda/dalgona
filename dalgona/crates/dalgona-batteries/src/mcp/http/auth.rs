// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! OAuth token persistence, resource canonicalization, and PKCE helpers.
//!
//! This module owns the mode-0600 token file, per-resource binding, scope
//! parsing, and the private SHA-256 used for PKCE S256.

use std::{collections::BTreeMap, fmt, future::Future, path::Path, sync::Arc};

use dal_store::{FileMode, write_atomic};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio_util::task::AbortOnDropHandle;

use crate::mcp::McpError;

/// One persisted OAuth credential, keyed by issuer and canonical resource.
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct TokenRecord {
    pub(crate) client_id: String,
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    #[serde(default)]
    pub(crate) scopes: Vec<String>,
}

impl fmt::Debug for TokenRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenRecord")
            .field("client_id", &self.client_id)
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// The `<data root>/mcp/tokens.json` document.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct TokenFile {
    #[serde(default)]
    pub(crate) tokens: BTreeMap<String, BTreeMap<String, TokenRecord>>,
}

#[derive(Default)]
pub(crate) struct AuthorizationState {
    pub(crate) cancelled: bool,
    pub(crate) refresh_failed: bool,
}

/// Coalesces one token refresh per credential key.
///
/// A refresh task owns the network request until it completes. Callers may
/// stop waiting, but a later caller still joins the same task and receives its
/// result.
pub(crate) struct RefreshCoordinator {
    flights: Mutex<BTreeMap<String, Arc<RefreshSlot>>>,
}

struct RefreshSlot {
    state: Mutex<RefreshSlotState>,
}

impl RefreshSlot {
    /// Clears a finished flight, keeping its fresh record unless an
    /// interactive record superseded it while it ran.
    async fn settle(&self, flight: &Arc<RefreshFlight>) {
        let fresh = flight.fresh().await;
        let mut state = self.state.lock().await;
        if !state
            .flight
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, flight))
        {
            return;
        }
        if let Some(fresh) = fresh
            && !state.superseded
        {
            state.last = Some(fresh);
        }
        state.flight = None;
        state.task = None;
    }
}

#[derive(Default)]
struct RefreshSlotState {
    flight: Option<Arc<RefreshFlight>>,
    task: Option<AbortOnDropHandle<()>>,
    last: Option<TokenRecord>,
    /// Set when an interactive record replaced `last` while a flight was
    /// still running, so the older flight cannot overwrite it.
    superseded: bool,
}

struct RefreshFlight {
    result: Mutex<Option<Result<Option<TokenRecord>, McpError>>>,
    notify: Notify,
}

/// Names the refresh slot for one stored credential.
///
/// The key matches the token file: one record per issuer and resource.
pub(crate) fn refresh_key(issuer: &str, resource: &str) -> String {
    format!("{issuer}\u{1f}{resource}")
}

impl RefreshCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            flights: Mutex::new(BTreeMap::new()),
        }
    }

    async fn slot(&self, key: &str) -> Arc<RefreshSlot> {
        let mut flights = self.flights.lock().await;
        flights
            .entry(key.to_owned())
            .or_insert_with(|| {
                Arc::new(RefreshSlot {
                    state: Mutex::new(RefreshSlotState::default()),
                })
            })
            .clone()
    }

    /// Records a record obtained outside a refresh, such as an interactive
    /// authorization, so a caller still holding an older access token adopts
    /// it instead of a stale cached refresh result.
    pub(crate) async fn publish(&self, key: &str, record: TokenRecord) {
        let slot = self.slot(key).await;
        let mut state = slot.state.lock().await;
        state.superseded = state.flight.is_some();
        state.last = Some(record);
    }

    pub(crate) async fn run<F, Fut>(
        &self,
        key: &str,
        held_access: &str,
        operation: F,
    ) -> Result<Option<TokenRecord>, McpError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<TokenRecord>, McpError>> + Send + 'static,
    {
        let slot = self.slot(key).await;
        let flight = {
            let mut state = slot.state.lock().await;
            if (state.flight.is_none() || state.superseded)
                && let Some(record) = state.last.as_ref()
                && record.access_token != held_access
            {
                return Ok(Some(record.clone()));
            }
            if let Some(flight) = state.flight.as_ref() {
                Arc::clone(flight)
            } else {
                let flight = Arc::new(RefreshFlight {
                    result: Mutex::new(None),
                    notify: Notify::new(),
                });
                state.flight = Some(Arc::clone(&flight));
                state.superseded = false;
                let weak_flight = Arc::downgrade(&flight);
                let weak_slot = Arc::downgrade(&slot);
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the credential slot owns and aborts the refresh task"
                )]
                let task = AbortOnDropHandle::new(tokio::spawn(async move {
                    let result = operation().await;
                    let (Some(flight), Some(slot)) = (weak_flight.upgrade(), weak_slot.upgrade())
                    else {
                        return;
                    };
                    flight.finish(result).await;
                    slot.settle(&flight).await;
                }));
                state.task = Some(task);
                flight
            }
        };
        flight.wait().await
    }
}

impl RefreshFlight {
    async fn finish(&self, result: Result<Option<TokenRecord>, McpError>) {
        *self.result.lock().await = Some(result);
        self.notify.notify_waiters();
    }

    async fn fresh(&self) -> Option<TokenRecord> {
        match self.result.lock().await.as_ref() {
            Some(Ok(Some(record))) => Some(record.clone()),
            _ => None,
        }
    }

    async fn wait(&self) -> Result<Option<TokenRecord>, McpError> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            let result = self.result.lock().await;
            notified.as_mut().enable();
            if let Some(result) = result.as_ref() {
                return result.clone();
            }
            drop(result);
            notified.await;
        }
    }
}

/// Reads the token file. A corrupt or unreadable file is ignored so the
/// caller re-authorizes; the caller emits the one warning.
pub(crate) fn read_tokens(path: &Path) -> TokenFile {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| sonic_rs::from_slice::<TokenFile>(&bytes).ok())
        .unwrap_or_default()
}

/// Returns the record stored under this exact issuer and canonical resource.
pub(crate) fn record_for<'a>(
    tokens: &'a TokenFile,
    issuer: &str,
    resource: &str,
) -> Option<&'a TokenRecord> {
    tokens.tokens.get(issuer)?.get(resource)
}

/// Persists one token record under its issuer and canonical resource with
/// mode-0600 atomic replacement.
pub(crate) fn persist_token(
    path: &Path,
    issuer: &str,
    resource: &str,
    record: TokenRecord,
) -> Result<(), McpError> {
    let mut file = read_tokens(path);
    file.tokens
        .entry(issuer.to_owned())
        .or_default()
        .insert(resource.to_owned(), record);
    let bytes = sonic_rs::to_string(&file).map_err(|error| McpError::Auth {
        cause: format!("could not encode MCP tokens: {error}"),
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| McpError::Auth {
            cause: format!("could not create MCP token directory: {error}"),
        })?;
    }
    write_atomic(path, bytes.as_bytes(), FileMode::Mode0600).map_err(|error| McpError::Auth {
        cause: format!("could not write MCP tokens: {error}"),
    })
}

/// Canonicalizes an OAuth resource URI once: lowercase scheme and host, no
/// trailing slash, no fragment.
pub(crate) fn canonical_resource(url: &Url) -> String {
    let scheme = url.scheme().to_ascii_lowercase();
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let path = url.path().trim_end_matches('/').to_owned();
    format!("{scheme}://{host}{port}{path}")
}

/// Parses one named auth-param from a `WWW-Authenticate` challenge value.
///
/// Quoted values may contain commas and escaped quotes; bare tokens end at a
/// comma or whitespace. An empty value is reported as absent.
pub(crate) fn challenge_param(challenge: &str, name: &str) -> Option<String> {
    let mut chars = challenge.chars().peekable();
    loop {
        while chars.next_if(|&c| matches!(c, ' ' | '\t' | ',')).is_some() {}
        let key: String =
            std::iter::from_fn(|| chars.next_if(|&c| !matches!(c, ' ' | '\t' | ',' | '=')))
                .collect();
        if key.is_empty() && chars.peek().is_none() {
            return None;
        }
        if chars.next_if_eq(&'=').is_none() {
            continue;
        }
        let value = if chars.next_if_eq(&'"').is_some() {
            quoted_value(&mut chars)
        } else {
            std::iter::from_fn(|| chars.next_if(|&c| !matches!(c, ' ' | '\t' | ','))).collect()
        };
        if key.eq_ignore_ascii_case(name) {
            let value = value.trim().to_owned();
            return (!value.is_empty()).then_some(value);
        }
    }
}

fn quoted_value(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut value = String::new();
    while let Some(character) = chars.next() {
        match character {
            '"' => break,
            '\\' => value.extend(chars.next()),
            other => value.push(other),
        }
    }
    value
}

/// Computes the PKCE S256 code challenge for a verifier.
pub(crate) fn pkce_challenge(verifier: &str) -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()))
}

/// Private SHA-256 over bytes, per FIPS 180-4. No hashing crate is added for
/// the one MCP PKCE challenge.
pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.as_chunks::<64>().0 {
        let mut schedule = [0_u32; 64];
        for (index, word) in schedule.iter_mut().enumerate().take(16) {
            let at = index * 4;
            *word = u32::from_be_bytes([chunk[at], chunk[at + 1], chunk[at + 2], chunk[at + 3]]);
        }
        for index in 16..64 {
            let small0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let small1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(small0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(small1);
        }
        let [
            mut work_a,
            mut work_b,
            mut work_c,
            mut work_d,
            mut work_e,
            mut work_f,
            mut work_g,
            mut work_h,
        ] = state;
        for round in 0..64 {
            let big1 = work_e.rotate_right(6) ^ work_e.rotate_right(11) ^ work_e.rotate_right(25);
            let choice = (work_e & work_f) ^ ((!work_e) & work_g);
            let temp1 = work_h
                .wrapping_add(big1)
                .wrapping_add(choice)
                .wrapping_add(K[round])
                .wrapping_add(schedule[round]);
            let big0 = work_a.rotate_right(2) ^ work_a.rotate_right(13) ^ work_a.rotate_right(22);
            let majority = (work_a & work_b) ^ (work_a & work_c) ^ (work_b & work_c);
            let temp2 = big0.wrapping_add(majority);
            work_h = work_g;
            work_g = work_f;
            work_f = work_e;
            work_e = work_d.wrapping_add(temp1);
            work_d = work_c;
            work_c = work_b;
            work_b = work_a;
            work_a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(work_a);
        state[1] = state[1].wrapping_add(work_b);
        state[2] = state[2].wrapping_add(work_c);
        state[3] = state[3].wrapping_add(work_d);
        state[4] = state[4].wrapping_add(work_e);
        state[5] = state[5].wrapping_add(work_f);
        state[6] = state[6].wrapping_add(work_g);
        state[7] = state[7].wrapping_add(work_h);
    }
    let mut digest = [0_u8; 32];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(digest: [u8; 32]) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn sha256_matches_rfc_vectors() {
        assert_eq!(
            hex(sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn pkce_matches_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn canonical_resource_lowercases_and_strips() {
        let url = Url::parse("HTTPS://Example.COM:8443/api/v1/?x=1#frag").expect("valid url");
        assert_eq!(canonical_resource(&url), "https://example.com:8443/api/v1");
    }

    #[test]
    fn challenge_params_parse_quoted_and_bare_values() {
        let challenge = r#"Bearer error="insufficient_scope", resource_metadata="https://x/.well-known/a,b", scope="files:write files:read", realm=plain"#;
        assert_eq!(
            challenge_param(challenge, "scope"),
            Some("files:write files:read".to_owned())
        );
        assert_eq!(
            challenge_param(challenge, "resource_metadata"),
            Some("https://x/.well-known/a,b".to_owned())
        );
        assert_eq!(
            challenge_param(challenge, "error"),
            Some("insufficient_scope".to_owned())
        );
        assert_eq!(
            challenge_param(challenge, "realm"),
            Some("plain".to_owned())
        );
        assert_eq!(challenge_param("Bearer", "scope"), None);
        assert_eq!(challenge_param(r#"Bearer scope="""#, "scope"), None);
    }

    #[test]
    fn challenge_params_survive_escapes_and_non_ascii() {
        assert_eq!(
            challenge_param(r#"Bearer scope="a\"é b""#, "scope"),
            Some("a\"é b".to_owned())
        );
    }

    #[test]
    fn persisted_token_file_is_mode_0600_and_resource_bound() {
        let path =
            std::env::temp_dir().join(format!("dalgona-mcp-tokens-{}.json", uuid::Uuid::new_v4()));
        let record = TokenRecord {
            client_id: "client".to_owned(),
            access_token: "access".to_owned(),
            refresh_token: Some("refresh".to_owned()),
            scopes: vec!["read".to_owned()],
        };
        persist_token(
            &path,
            "https://issuer.example",
            "https://server.example/mcp",
            record,
        )
        .expect("persist token");
        let loaded = read_tokens(&path);
        assert_eq!(
            record_for(
                &loaded,
                "https://issuer.example",
                "https://server.example/mcp"
            )
            .map(|record| record.client_id.as_str()),
            Some("client")
        );
        assert!(
            record_for(
                &loaded,
                "https://other.example",
                "https://server.example/mcp"
            )
            .is_none()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("token file metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        std::fs::remove_file(path).expect("remove token file");
    }

    #[test]
    fn token_record_debug_redacts_secrets() {
        let record = TokenRecord {
            client_id: "client".to_owned(),
            access_token: "sekret-access".to_owned(),
            refresh_token: Some("sekret-refresh".to_owned()),
            scopes: vec!["a".to_owned()],
        };
        let rendered = format!("{record:?}");
        assert!(!rendered.contains("sekret"));
        assert!(rendered.contains("client"));
    }
}
