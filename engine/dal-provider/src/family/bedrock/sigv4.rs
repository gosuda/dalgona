//! AWS `SigV4` request signing for the Bedrock family.
//!
//! [`SignedRequest::sign`] builds the canonical request and returns the
//! header set the transport puts on the wire. The canonical request follows
//! the AWS `SigV4` specification: each URI path segment percent-encoded with
//! `/` kept as separator, query names and values percent-decoded, encoded
//! once, and sorted by the encoded form, canonical headers sorted by name,
//! the payload hash from the crate's SHA-256 stack, and the
//! `AWS4-HMAC-SHA256` string to sign over the credential scope
//! `<date>/<region>/<service>/aws4_request`. Every byte the signature covers
//! is a byte the wire carries, so the caller sends the returned headers
//! unchanged and signs the `host` value exactly as the transport sends it.
//!
//! The service is chosen by the caller. The region comes from the provider
//! entry. A session token adds `x-amz-security-token` to the signed set and
//! the wire; without one the header is absent from both. A credential whose
//! key id or token a header cannot carry unchanged is refused before it
//! signs ([`AwsCredential::verify_usable`]).

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use jiff::Timestamp;
use sha2::{Digest, Sha256};

/// The approved HMAC-SHA256 instantiation.
type HmacSha256 = Hmac<Sha256>;

/// The signature algorithm name of the `authorization` header.
const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The terminal step of the `SigV4` credential scope.
const KEY_TYPE: &str = "aws4_request";

/// The AWS service name of every signed Bedrock runtime call.
pub(crate) const SERVICE: &str = "bedrock";

/// An AWS credential resolved out of band, before the request is built.
#[must_use]
#[derive(Clone)]
pub(crate) struct AwsCredential {
    /// The access key id of the credential.
    pub(crate) access_key_id: Box<str>,
    /// The secret access key of the credential.
    pub(crate) secret_access_key: Box<str>,
    /// The STS session token, present on temporary credentials.
    pub(crate) session_token: Option<Box<str>>,
    /// Unix seconds at which the credential stops signing; `None` for a
    /// long-lived key. Resolved by the credential owner, never by this
    /// module.
    pub(crate) expires_at: Option<i64>,
}

/// A credential that cannot sign.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CredentialError {
    /// The credential's lifetime ended before the request was signed.
    #[error("the AWS credential expired at unix second {expires_at}")]
    Expired {
        /// The expiry the credential carried.
        expires_at: i64,
    },
    /// A member would sign differently from how it travels: it is empty or
    /// holds whitespace, a control byte, or a non-ASCII byte. The error
    /// names the member, never its value.
    #[error("the AWS credential's {field} holds characters a signed header cannot carry")]
    Unsignable {
        /// The offending member.
        field: &'static str,
    },
}

/// Whether `value` is non-empty visible ASCII, the only text that signs and
/// travels as the same bytes: a header value is trimmed and folded by the
/// canonical form but sent verbatim.
fn is_header_token(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_graphic())
}

impl AwsCredential {
    /// Rejects a credential that cannot sign before any bytes are signed:
    /// an expired one, so the failure is a typed refusal instead of AWS's
    /// post-hoc `ExpiredTokenException`, and one whose access key id or
    /// session token holds bytes a header cannot carry unchanged.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Expired`] when `now_unix` is at or past the
    /// stored expiry, and [`CredentialError::Unsignable`] for a key id or
    /// token that is empty or not visible ASCII.
    pub(crate) fn verify_usable(&self, now_unix: i64) -> Result<(), CredentialError> {
        if let Some(expires_at) = self.expires_at
            && now_unix >= expires_at
        {
            return Err(CredentialError::Expired { expires_at });
        }
        if !is_header_token(&self.access_key_id) {
            return Err(CredentialError::Unsignable {
                field: "access key id",
            });
        }
        if self
            .session_token
            .as_deref()
            .is_some_and(|token| !is_header_token(token))
        {
            return Err(CredentialError::Unsignable {
                field: "session token",
            });
        }
        Ok(())
    }
}

impl fmt::Debug for AwsCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsCredential")
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The two `x-amz-date` spellings one signature is built from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AmzDate {
    /// The long date of the canonical headers and string to sign,
    /// `YYYYMMDDTHHMMSSZ`.
    pub(crate) long: String,
    /// The short date of the credential scope, the first eight bytes of
    /// [`Self::long`].
    pub(crate) short: String,
}

impl AmzDate {
    /// Formats `now_unix` as the UTC `x-amz-date` pair.
    #[must_use]
    pub(crate) fn from_unix(now_unix: i64) -> Self {
        // `from_second` rejects only out-of-range seconds; a broken caller
        // clock falls back to the epoch so the request still carries a
        // well-formed date AWS can reject by skew.
        let timestamp = Timestamp::from_second(now_unix).unwrap_or(Timestamp::UNIX_EPOCH);
        let zoned = timestamp.to_zoned(jiff::tz::TimeZone::UTC);
        let long = format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            zoned.year(),
            i16::from(zoned.month()),
            zoned.day(),
            zoned.hour(),
            zoned.minute(),
            zoned.second(),
        );
        let short = long[..8].to_owned();
        Self { long, short }
    }
}

/// Everything one signed Bedrock request needs besides the body.
#[must_use]
#[derive(Clone, Debug)]
pub(crate) struct SignedRequest<'a> {
    /// The HTTP method; Bedrock streams are `POST`.
    pub(crate) method: &'a str,
    /// The `host` header value exactly as the transport sends it: the host,
    /// plus `:port` when the URL names a port other than the scheme's
    /// default. No scheme and no default-port spelling.
    pub(crate) host: &'a str,
    /// The URI path, starting with `/`.
    pub(crate) path: &'a str,
    /// The raw query string without the leading `?`, when the base URL
    /// carried one.
    pub(crate) query: Option<&'a str>,
    /// Extra headers the signature covers besides the signer-owned ones.
    /// Names are compared case-insensitively.
    pub(crate) headers: &'a [(&'a str, String)],
    /// The AWS service name of the credential scope.
    pub(crate) service: &'a str,
    /// The region of the credential scope.
    pub(crate) region: &'a str,
    /// The credential to sign with.
    pub(crate) credential: &'a AwsCredential,
    /// The date strings the signature is built from.
    pub(crate) date: AmzDate,
}

/// The signed header set of one request. Send exactly these values,
/// unchanged, alongside the caller's unsigned headers.
#[must_use]
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Signed {
    /// The `host` header value.
    pub(crate) host: String,
    /// The `x-amz-date` header value.
    pub(crate) amz_date: String,
    /// The `x-amz-content-sha256` header value.
    pub(crate) content_sha256: String,
    /// The `x-amz-security-token` header value, when the credential is
    /// temporary.
    pub(crate) security_token: Option<String>,
    /// The `authorization` header value.
    pub(crate) authorization: String,
}

impl fmt::Debug for Signed {
    /// The session token and the authorization value never print.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signed")
            .field("host", &self.host)
            .field("amz_date", &self.amz_date)
            .field("content_sha256", &self.content_sha256)
            .field(
                "security_token",
                &self.security_token.as_ref().map(|_| "<redacted>"),
            )
            .field("authorization", &"<redacted>")
            .finish()
    }
}

/// Headers this signer owns; a caller value never reaches the wire for one
/// of these, or the signature would cover bytes the request does not send.
const SIGNER_OWNED: [&str; 4] = [
    "host",
    "x-amz-date",
    "x-amz-content-sha256",
    "x-amz-security-token",
];

/// Headers that never join the signed set: hop-by-hop, managed by the
/// transport, or excluded by the AWS SDK's own signer. `content-length` is
/// recomputed by the HTTP client from the body, so it stays unsigned like
/// every other family's requests.
const UNSIGNED: [&str; 16] = [
    "authorization",
    "cache-control",
    "connection",
    "content-length",
    "expect",
    "from",
    "keep-alive",
    "max-forwards",
    "pragma",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
    "x-amzn-trace-id",
];

fn signable(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !UNSIGNED.contains(&lower.as_str()) && !SIGNER_OWNED.contains(&lower.as_str())
}

/// Folds one header value into its canonical form: trimmed, and every
/// interior run of spaces or tabs collapsed to one space, the AWS
/// canonical-header rule.
#[must_use]
fn fold(value: &str) -> String {
    let trimmed = value.trim_matches([' ', '\t']);
    let mut folded = String::with_capacity(trimmed.len());
    let mut pending_space = false;
    for character in trimmed.chars() {
        if character == ' ' || character == '\t' {
            // A run collapses to one separator only between values, never
            // before the first (the trim already removed those).
            pending_space = !folded.is_empty();
            continue;
        }
        if pending_space {
            folded.push(' ');
            pending_space = false;
        }
        folded.push(character);
    }
    folded
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0F)]));
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex(&hasher.finalize()[..])
}

#[expect(
    clippy::expect_used,
    reason = "hmac 0.13 documents that HMAC accepts a key of any length, so construction cannot fail"
)]
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes()[..].to_vec()
}

/// The `SigV4` signing key: `kSecret -> kDate -> kRegion -> kService -> kSigning`.
fn signing_key(secret: &str, short_date: &str, region: &str, service: &str) -> Vec<u8> {
    let key_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), short_date.as_bytes());
    let key_region = hmac_sha256(&key_date, region.as_bytes());
    let key_service = hmac_sha256(&key_region, service.as_bytes());
    hmac_sha256(&key_service, KEY_TYPE.as_bytes())
}

/// URI-encodes one path or query component per RFC 3986, uppercase hex.
fn encode_component(component: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(component.len());
    for &byte in component {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 0x0F)]));
            }
        }
    }
    out
}

/// The canonical URI path: every segment of `path` percent-encoded, `/`
/// kept as separator. Bedrock model ids carry `:`, so
/// `/model/anthropic.claude-…:0/converse-stream` canonicalizes the id.
fn canonical_path(path: &str) -> String {
    path.split('/')
        .map(|segment| encode_component(segment.as_bytes()))
        .collect::<Vec<_>>()
        .join("/")
}

/// The value of one hexadecimal digit.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes one query name or value. A `%` that is not followed by
/// two hexadecimal digits stays a literal byte, and `+` is a literal plus,
/// not a space.
fn percent_decode(component: &str) -> Vec<u8> {
    let bytes = component.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(&[high, low]) = bytes.get(index + 1..index + 3)
            && let (Some(high), Some(low)) = (hex_value(high), hex_value(low))
        {
            out.push((high << 4) | low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

/// The canonical query string: decode each name and value out of the wire
/// spelling, encode the decoded bytes once, then sort by the encoded forms,
/// so `a=%7B1%7D` on the wire is `a=%7B1%7D` here, not `a=%257B1%257D`.
///
/// AWS sorts by the encoded name, then the encoded value, and rejects a
/// signature built from any other order. An absent or empty query is the
/// empty string.
fn canonical_query(query: Option<&str>) -> String {
    let Some(query) = query else {
        return String::new();
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let (name, value) = match part.split_once('=') {
            Some((name, value)) => (name, value),
            None => (part, ""),
        };
        pairs.push((
            encode_component(&percent_decode(name)),
            encode_component(&percent_decode(value)),
        ));
    }
    pairs.sort();
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The canonical request over `headers`, already lowercased, folded, and
/// sorted by name, with the `SignedHeaders` list that names them.
fn canonical_request(
    method: &str,
    path: &str,
    query: Option<&str>,
    headers: &[(String, String)],
    payload_hash: &str,
) -> (String, String) {
    let mut canonical_headers = String::new();
    for (name, value) in headers {
        use std::fmt::Write as _;
        let _ = writeln!(canonical_headers, "{name}:{value}");
    }
    let signed_names = headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let request = [
        method,
        canonical_path(path).as_str(),
        canonical_query(query).as_str(),
        canonical_headers.as_str(),
        signed_names.as_str(),
        payload_hash,
    ]
    .join("\n");
    (request, signed_names)
}

/// The string to sign: algorithm, long date, credential scope, and the
/// hash of the canonical request.
fn string_to_sign(long_date: &str, scope: &str, canonical_request: &str) -> String {
    [
        ALGORITHM,
        long_date,
        scope,
        sha256_hex(canonical_request.as_bytes()).as_str(),
    ]
    .join("\n")
}

/// The hex signature of `string_to_sign` under the derived signing key.
fn signature(
    secret: &str,
    short_date: &str,
    region: &str,
    service: &str,
    string_to_sign: &str,
) -> String {
    let key = signing_key(secret, short_date, region, service);
    hex(&hmac_sha256(&key, string_to_sign.as_bytes()))
}

impl SignedRequest<'_> {
    /// Signs the request body and returns the header set to send.
    ///
    /// The caller merges the returned values into its header map and sends
    /// them unchanged. `content-type: application/json` and
    /// `accept: application/vnd.amazon.eventstream` arrive through
    /// [`Self::headers`]; both are signable and always travel on the wire.
    pub(crate) fn sign(&self, body: &[u8]) -> Signed {
        let payload_hash = sha256_hex(body);

        let mut signed: Vec<(String, String)> = Vec::with_capacity(4 + self.headers.len());
        signed.push((String::from("host"), self.host.to_owned()));
        signed.push((String::from("x-amz-date"), self.date.long.clone()));
        signed.push((String::from("x-amz-content-sha256"), payload_hash.clone()));
        if let Some(token) = &self.credential.session_token {
            signed.push((String::from("x-amz-security-token"), token.to_string()));
        }
        for (name, value) in self.headers {
            if !signable(name) {
                continue;
            }
            signed.push((name.to_ascii_lowercase(), fold(value)));
        }
        signed.sort_by(|left, right| left.0.cmp(&right.0));
        let (canonical_request, signed_names) = canonical_request(
            self.method,
            self.path,
            self.query,
            &signed,
            &payload_hash,
        );

        let scope = format!(
            "{}/{}/{}/{KEY_TYPE}",
            self.date.short, self.region, self.service
        );
        let string_to_sign = string_to_sign(&self.date.long, &scope, &canonical_request);
        let signature = signature(
            &self.credential.secret_access_key,
            &self.date.short,
            self.region,
            self.service,
            &string_to_sign,
        );
        let authorization = format!(
            "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_names}, Signature={signature}",
            self.credential.access_key_id,
        );

        Signed {
            host: self.host.to_owned(),
            amz_date: self.date.long.clone(),
            content_sha256: payload_hash,
            security_token: self
                .credential
                .session_token
                .as_ref()
                .map(ToString::to_string),
            authorization,
        }
    }
}

#[cfg(test)]
mod tests;
