// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! OAuth token persistence, resource canonicalization, and PKCE helpers.
//!
//! This module owns the mode-0600 token file, per-resource binding, scope
//! parsing, and the private SHA-256 used for PKCE S256.

use std::{collections::BTreeMap, fmt, path::Path};

use dal_store::{FileMode, write_atomic};
use reqwest::Url;
use serde::{Deserialize, Serialize};

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
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
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
    let port = url.port().map(|port| format!(":{port}")).unwrap_or_default();
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
            std::iter::from_fn(|| chars.next_if(|&c| !matches!(c, ' ' | '\t' | ',' | '='))).collect();
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
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()))
}

/// Private SHA-256 over bytes, per FIPS 180-4. No hashing crate is added for
/// the one MCP PKCE challenge.
pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.chunks_exact(64) {
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
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h) = (
            state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
        );
        for index in 0..64 {
            let big1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(big1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let big0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = big0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
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
            hex(sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
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
        assert_eq!(challenge_param(challenge, "realm"), Some("plain".to_owned()));
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
        let path = std::env::temp_dir().join(format!(
            "dalgona-mcp-tokens-{}.json",
            uuid::Uuid::new_v4()
        ));
        let record = TokenRecord {
            client_id: "client".to_owned(),
            access_token: "access".to_owned(),
            refresh_token: Some("refresh".to_owned()),
            scopes: vec!["read".to_owned()],
        };
        persist_token(&path, "https://issuer.example", "https://server.example/mcp", record)
            .expect("persist token");
        let loaded = read_tokens(&path);
        assert_eq!(
            record_for(&loaded, "https://issuer.example", "https://server.example/mcp")
                .map(|record| record.client_id.as_str()),
            Some("client")
        );
        assert!(record_for(&loaded, "https://other.example", "https://server.example/mcp").is_none());
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
