//! Connect envelope framing and error mapping for the Cursor `Run` RPC.
//!
//! Every `Run` body is a stream of frames: one flag byte, a 4-byte
//! big-endian payload length, then the payload. [`encode_frame`] writes
//! them; [`FrameDecoder`] reads them back from arbitrarily split network
//! chunks, exactly as the reference `ConnectFrameDecoder` does. Payloads
//! are protobuf `AgentClientMessage` / `AgentServerMessage` bytes, except
//! the single end-of-stream frame, whose payload is a JSON error trailer
//! (parsed by [`parse_trailer`]).
//!
//! [`map_connect_error`] and [`map_status`] translate both error terminals
//! into [`ProviderError`]: structured `aiserver.v1.ErrorDetails` first,
//! then the Connect string code, using oh-my-pi's status table verbatim.
//! A compressed frame is gunzipped (the flag exists on the wire even though
//! this client never negotiates compression); anything else unknown in a
//! header is tolerated, as the reference tolerates it.

use dal_core::Family;
use prost::Message as _;

use super::proto::{CustomErrorDetails, ErrorDetails};
use crate::error::ProviderError;

/// Bytes of one frame header: one flag byte plus the length prefix.
pub(crate) const FRAME_HEADER_LEN: usize = 5;

/// Flag bit: the payload is gzip-compressed.
pub(crate) const COMPRESSED_FLAG: u8 = 0x01;

/// Flag bit: the payload is the end-of-stream JSON trailer.
pub(crate) const END_STREAM_FLAG: u8 = 0x02;

/// Largest framed payload accepted, matching the crate's 16 MiB body cap.
pub(crate) const MAX_FRAME_BYTES: usize = 16 << 20;

/// Read cap for one gzip bomb, one byte past [`MAX_FRAME_BYTES`].
const GZIP_TAKE_LIMIT: u64 = (16 << 20) + 1;

/// The `type` URL Cursor uses for its error envelope in Connect details.
const ERROR_DETAILS_TYPE: &str = "aiserver.v1.ErrorDetails";

/// The same URL with the protobuf type prefix some backends add.
const ERROR_DETAILS_TYPE_URL: &str = "type.googleapis.com/aiserver.v1.ErrorDetails";

/// A framing failure: oversize, gzip, or trailer errors.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum FrameError {
    /// A length prefix above [`MAX_FRAME_BYTES`], or a buffer past it.
    #[error("cursor frame of {len} bytes exceeds the 16 MiB frame limit")]
    TooLarge {
        /// The offending length in bytes.
        len: usize,
    },
    /// A gzip payload that failed to decode or decoded past the limit.
    #[error("cursor gzip frame failed: {message}")]
    Gzip {
        /// The decoder message.
        message: Box<str>,
    },
    /// An end-of-stream payload that is not a JSON error trailer.
    #[error("cursor end-of-stream trailer is not JSON: {message}")]
    MalformedTrailer {
        /// The parser message.
        message: Box<str>,
    },
}

/// One decoded Connect frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Frame {
    /// The header flag byte.
    pub(crate) flags: u8,
    /// The payload bytes, decompressed when the flag asked for it.
    pub(crate) payload: Vec<u8>,
}

/// Encodes one frame: flag byte, big-endian length, payload.
///
/// # Errors
///
/// Returns [`FrameError::TooLarge`] when the payload does not fit the
/// 4-byte length prefix.
pub(crate) fn encode_frame(payload: &[u8], flags: u8) -> Result<Vec<u8>, FrameError> {
    let len =
        u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge { len: payload.len() })?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    frame.push(flags);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Encodes one protobuf message as an unflagged frame.
///
/// # Errors
///
/// Returns [`FrameError::TooLarge`] when the message does not fit the
/// length prefix.
pub(crate) fn encode_message(
    message: &impl prost::Message,
    flags: u8,
) -> Result<Vec<u8>, FrameError> {
    encode_frame(&message.encode_to_vec(), flags)
}

/// The incremental frame decoder: chunk-split-safe, header-joined in
/// place, payload-capped before buffering.
#[derive(Debug, Default)]
pub(crate) struct FrameDecoder {
    /// Bytes not yet forming a complete frame.
    buf: Vec<u8>,
    /// Set after an oversize prefix; every later push fails the same way.
    poisoned: bool,
}

impl FrameDecoder {
    /// An empty decoder.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Buffers `chunk` and appends every now-complete frame to `out`, in
    /// wire order. A gzip payload is decompressed under the frame cap.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::TooLarge`] on the first length prefix above
    /// [`MAX_FRAME_BYTES`] (and on every later call), or
    /// [`FrameError::Gzip`] for a corrupt or over-limit compressed payload.
    pub(crate) fn push(&mut self, chunk: &[u8], out: &mut Vec<Frame>) -> Result<(), FrameError> {
        if self.poisoned {
            return Err(FrameError::TooLarge { len: usize::MAX });
        }
        self.buf.extend_from_slice(chunk);
        loop {
            if self.buf.len() < FRAME_HEADER_LEN {
                return Ok(());
            }
            let mut prefix = [0_u8; 4];
            prefix.copy_from_slice(&self.buf[1..FRAME_HEADER_LEN]);
            let declared = u32::from_be_bytes(prefix);
            let len = usize::try_from(declared).map_err(|_| FrameError::TooLarge {
                len: self.buf.len(),
            })?;
            if len > MAX_FRAME_BYTES {
                self.poisoned = true;
                self.buf.clear();
                return Err(FrameError::TooLarge { len });
            }
            if self.buf.len() - FRAME_HEADER_LEN < len {
                if self.buf.len() > MAX_FRAME_BYTES + FRAME_HEADER_LEN {
                    self.poisoned = true;
                    self.buf.clear();
                    return Err(FrameError::TooLarge {
                        len: self.buf.len(),
                    });
                }
                return Ok(());
            }
            let flags = self.buf[0];
            let payload: Vec<u8> = self.buf[FRAME_HEADER_LEN..FRAME_HEADER_LEN + len].to_vec();
            self.buf.drain(..FRAME_HEADER_LEN + len);
            let payload = if flags & COMPRESSED_FLAG != 0 {
                decompress(&payload)?
            } else {
                payload
            };
            out.push(Frame { flags, payload });
        }
    }

    /// Buffered bytes that never formed a frame; nonzero at a clean end
    /// means the server cut a frame in half.
    #[must_use]
    pub(crate) fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// Gunzips one compressed payload under the frame cap.
///
/// # Errors
///
/// Returns [`FrameError::Gzip`] for corrupt input or output past
/// [`MAX_FRAME_BYTES`].
fn decompress(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    use std::io::Read as _;

    let decoder = flate2::read::GzDecoder::new(payload);
    let mut out = Vec::new();
    decoder
        .take(GZIP_TAKE_LIMIT)
        .read_to_end(&mut out)
        .map_err(|error| FrameError::Gzip {
            message: error.to_string().into_boxed_str(),
        })?;
    if out.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Gzip {
            message: String::from("decompressed payload exceeds the 16 MiB frame limit")
                .into_boxed_str(),
        });
    }
    Ok(out)
}

/// One Connect error object: `{ code, message, details }`.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub(crate) struct ConnectError {
    /// The Connect code, or `"unknown"`.
    #[serde(default)]
    pub(crate) code: String,
    /// The server message, possibly empty.
    #[serde(default)]
    pub(crate) message: String,
    /// Typed detail entries.
    #[serde(default)]
    pub(crate) details: Vec<ConnectDetail>,
}

/// One Connect error detail entry.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub(crate) struct ConnectDetail {
    /// The detail type URL.
    #[serde(default, rename = "type")]
    pub(crate) detail_type: Option<String>,
    /// The base64 protobuf payload, when present.
    #[serde(default)]
    pub(crate) value: Option<String>,
    /// The JSON debug form, when present.
    #[serde(default)]
    pub(crate) debug: Option<sonic_rs::Value>,
}

/// The end-of-stream trailer: `{"error": {...}}`, or no error.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
struct Trailer {
    /// The error, when the trailer carries one.
    #[serde(default)]
    pub(crate) error: Option<TrailerError>,
}

/// The error member of a trailer.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
struct TrailerError {
    /// The Connect code as sent; anything but a string reads unknown.
    #[serde(default)]
    pub(crate) code: Option<sonic_rs::Value>,
    /// The server message as sent.
    #[serde(default)]
    pub(crate) message: Option<sonic_rs::Value>,
    /// Typed detail entries.
    #[serde(default)]
    pub(crate) details: Option<Vec<ConnectDetail>>,
}

/// Parses an end-of-stream payload: `{"error": {...}}` or `{}`.
///
/// # Errors
///
/// Returns [`FrameError::MalformedTrailer`] when the payload is not a JSON
/// object with an optional error member.
pub(crate) fn parse_trailer(payload: &[u8]) -> Result<Option<ConnectError>, FrameError> {
    use sonic_rs::JsonValueTrait;

    let trailer: Trailer =
        sonic_rs::from_slice(payload).map_err(|error| FrameError::MalformedTrailer {
            message: error.to_string().into_boxed_str(),
        })?;
    let Some(error) = trailer.error else {
        return Ok(None);
    };
    Ok(Some(ConnectError {
        code: error
            .code
            .as_ref()
            .and_then(JsonValueTrait::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        message: error
            .message
            .as_ref()
            .and_then(JsonValueTrait::as_str)
            .unwrap_or("Unknown error")
            .to_owned(),
        details: error.details.unwrap_or_default(),
    }))
}

/// Parses a non-success HTTP body as a top-level Connect error object.
///
/// Returns `None` when the body is not one, leaving the caller to report
/// the raw status.
pub(crate) fn parse_status_body(body: &[u8]) -> Option<ConnectError> {
    use sonic_rs::JsonValueTrait;

    let value: sonic_rs::Value = sonic_rs::from_slice(body).ok()?;
    let code = value
        .get("code")
        .and_then(JsonValueTrait::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let message = value
        .get("message")
        .and_then(JsonValueTrait::as_str)
        .unwrap_or("Unknown error")
        .to_owned();
    if value.get("code").is_none() && value.get("message").is_none() {
        return None;
    }
    let details: Vec<ConnectDetail> = value
        .get("details")
        .and_then(|details| sonic_rs::from_str(&details.to_string()).ok())
        .unwrap_or_default();
    Some(ConnectError {
        code,
        message,
        details,
    })
}

/// Decodes the first `aiserver.v1.ErrorDetails` entry of `details`.
///
/// Returns the numeric code, the display message, and the retry flag.
fn decode_error_details(details: &[ConnectDetail]) -> Option<(i32, String, Option<bool>)> {
    for entry in details {
        let is_cursor = entry
            .detail_type
            .as_deref()
            .is_some_and(|kind| kind == ERROR_DETAILS_TYPE || kind == ERROR_DETAILS_TYPE_URL);
        if !is_cursor {
            continue;
        }
        // A Cursor detail without a decodable payload says nothing; a later
        // entry may still carry the structured verdict, as in the reference.
        // The reference decodes with Node `Buffer.from(value, "base64")`
        // (cursor.ts `decodeCursorStructuredError`), which accepts the
        // standard and URL-safe alphabets, padded or not, ignoring
        // whitespace. Mirror that leniency with the two engines; anything
        // else stays undecodable and falls through to the next entry.
        let Some(encoded) = entry.value.as_deref().filter(|value| !value.is_empty()) else {
            continue;
        };
        let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let trimmed = cleaned.trim_end_matches('=');
        let raw = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            trimmed,
        )
        .or_else(|_| {
            base64::Engine::decode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                trimmed,
            )
        })
        .ok();
        let Some(raw) = raw else {
            continue;
        };
        let Ok(detail) = ErrorDetails::decode(raw.as_slice()) else {
            continue;
        };
        let custom: CustomErrorDetails = detail.details.clone().unwrap_or(CustomErrorDetails {
            title: String::new(),
            detail: String::new(),
            is_retryable: None,
        });
        let title = custom.title.trim();
        let body = custom.detail.trim();
        let message = match (title.is_empty(), body.is_empty()) {
            (true, true) => String::new(),
            (false, true) => title.to_owned(),
            (true, false) => body.to_owned(),
            (false, false) => format!("{title}: {body}"),
        };
        return Some((detail.error, message, custom.is_retryable));
    }
    None
}

/// Whether any detail entry declares the failure retryable in its JSON
/// debug form (`details.isRetryable` under the Cursor type URL).
#[must_use]
pub(crate) fn has_retryable_detail(details: &[ConnectDetail]) -> bool {
    use sonic_rs::JsonValueTrait;

    details.iter().any(|entry| {
        entry
            .detail_type
            .as_deref()
            .is_some_and(|kind| kind == ERROR_DETAILS_TYPE || kind == ERROR_DETAILS_TYPE_URL)
            && entry
                .debug
                .as_ref()
                .and_then(|debug| debug.get("details"))
                .and_then(|inner| inner.get("isRetryable"))
                .and_then(JsonValueTrait::as_bool)
                == Some(true)
    })
}

/// Maps one Connect error object to its [`ProviderError`], trying the
/// structured `ErrorDetails` entry first, then the string code.
#[must_use]
pub(crate) fn map_connect_error(provider: &str, error: &ConnectError) -> ProviderError {
    if let Some((code, message, retryable)) = decode_error_details(&error.details) {
        return map_structured_error(provider, code, &message, retryable);
    }
    let retryable = has_retryable_detail(&error.details);
    map_code_error(
        provider,
        error.code.as_str(),
        error.message.as_str(),
        retryable,
    )
}

/// Maps one structured Cursor failure to its [`ProviderError`], following
/// the reference status table: 401 is authentication, usage-limit 429s are
/// quota, other 429s are rate limits, 413 on a too-long conversation is
/// context overflow, retryable codes are overload, and anything else is an
/// invalid request.
#[must_use]
pub(crate) fn map_structured_error(
    provider: &str,
    code: i32,
    message: &str,
    retryable: Option<bool>,
) -> ProviderError {
    use super::proto::CursorError as Code;

    let name = Code::name(code);
    let text = if message.trim().is_empty() {
        name.clone()
    } else {
        message.to_string()
    };
    let full = format!("Cursor {name}: {text}");
    if code == Code::USER_ABORTED_REQUEST {
        return ProviderError::Transport {
            family: Family::Cursor,
            reason: String::from("the server recorded the request as aborted"),
        };
    }
    if matches!(
        code,
        Code::BAD_API_KEY
            | Code::NOT_LOGGED_IN
            | Code::INVALID_AUTH_ID
            | Code::AUTH_TOKEN_NOT_FOUND
            | Code::AUTH_TOKEN_EXPIRED
            | Code::AGENT_REQUIRES_LOGIN
            | Code::UNAUTHORIZED
            | Code::BAD_USER_API_KEY
            | Code::ACCOUNT_CLOSED
    ) {
        return ProviderError::AuthRejected {
            provider: provider.to_owned(),
        };
    }
    if matches!(
        code,
        Code::FREE_USER_USAGE_LIMIT
            | Code::PRO_USER_USAGE_LIMIT
            | Code::USAGE_PRICING_REQUIRED
            | Code::USAGE_PRICING_REQUIRED_CHANGEABLE
    ) {
        return ProviderError::Quota { message: full };
    }
    if matches!(
        code,
        Code::FREE_USER_RATE_LIMIT_EXCEEDED
            | Code::PRO_USER_RATE_LIMIT_EXCEEDED
            | Code::OPENAI_RATE_LIMIT_EXCEEDED
            | Code::GENERIC_RATE_LIMIT_EXCEEDED
            | Code::GPT_4_VISION_PREVIEW_RATE_LIMIT
            | Code::API_KEY_RATE_LIMIT
            | Code::DEBOUNCED
            | Code::RESOURCE_EXHAUSTED
            | Code::RATE_LIMITED
            | Code::RATE_LIMITED_CHANGEABLE
    ) {
        return ProviderError::RateLimited {
            message: full,
            retry_after: None,
        };
    }
    if code == Code::CONVERSATION_TOO_LONG {
        return ProviderError::ContextOverflow {
            family: Family::Cursor,
            code: String::from("conversation_too_long"),
            message: full,
        };
    }
    if matches!(
        code,
        Code::NOT_HIGH_ENOUGH_PERMISSIONS
            | Code::PRO_USER_ONLY
            | Code::GITHUB_NO_USER_CREDENTIALS
            | Code::GITHUB_USER_NO_ACCESS
            | Code::GITHUB_APP_NO_ACCESS
            | Code::GITHUB_MULTIPLE_OWNERS
            | Code::SUSPICIOUS_USAGE_BLOCKED
            | Code::UNSUPPORTED_REGION
            | Code::MODEL_BLOCKED
    ) {
        return ProviderError::Status {
            family: Family::Cursor,
            status: 403,
            message: full,
        };
    }
    if matches!(
        code,
        Code::BAD_MODEL_NAME
            | Code::USER_NOT_FOUND
            | Code::FILE_NOT_FOUND
            | Code::NOT_FOUND
            | Code::MODEL_NO_LONGER_SUPPORTED
    ) {
        return ProviderError::Status {
            family: Family::Cursor,
            status: 404,
            message: full,
        };
    }
    if code == Code::MAX_TOKENS {
        return ProviderError::Status {
            family: Family::Cursor,
            status: 413,
            message: full,
        };
    }
    if retryable == Some(true) || (retryable.is_none() && Code::RETRYABLE.contains(&code)) {
        return ProviderError::Overloaded;
    }
    ProviderError::InvalidRequest { message: full }
}

/// Maps one unstructured Connect code to its [`ProviderError`]. Unknown
/// codes become a 500 status carrying the server's text verbatim.
fn map_code_error(provider: &str, code: &str, message: &str, retryable: bool) -> ProviderError {
    let text = if message.trim().is_empty() {
        format!("Connect error {code}: Unknown error")
    } else {
        format!("Connect error {code}: {message}")
    };
    match code {
        "unauthenticated" => ProviderError::AuthRejected {
            provider: provider.to_owned(),
        },
        "resource_exhausted" => ProviderError::RateLimited {
            message: text,
            retry_after: None,
        },
        "unavailable" => ProviderError::Overloaded,
        "deadline_exceeded" | "cancelled" => ProviderError::Transport {
            family: Family::Cursor,
            reason: text,
        },
        "permission_denied" => ProviderError::Status {
            family: Family::Cursor,
            status: 403,
            message: text,
        },
        "not_found" => ProviderError::Status {
            family: Family::Cursor,
            status: 404,
            message: text,
        },
        "invalid_argument"
        | "failed_precondition"
        | "out_of_range"
        | "unimplemented"
        | "already_exists" => ProviderError::InvalidRequest { message: text },
        _ if retryable => ProviderError::Overloaded,
        _ => ProviderError::Status {
            family: Family::Cursor,
            status: 500,
            message: text,
        },
    }
}

/// Maps a non-success pre-stream HTTP status to its [`ProviderError`]: a
/// Connect error body when the body parses as one, else the raw status.
#[must_use]
pub(crate) fn map_status(provider: &str, status: u16, body: &[u8]) -> ProviderError {
    if let Some(error) = parse_status_body(body) {
        return map_connect_error(provider, &error);
    }
    ProviderError::Status {
        family: Family::Cursor,
        status,
        message: short_line(body),
    }
}

/// The first line of `body` cut at 300 bytes on a UTF-8 boundary.
fn short_line(body: &[u8]) -> String {
    const LIMIT: usize = 300;

    let text = String::from_utf8_lossy(body);
    let line = text.lines().next().unwrap_or_default();
    if line.len() <= LIMIT {
        return line.to_owned();
    }
    let mut cut = LIMIT;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    line[..cut].to_owned()
}

/// Decodes one framed protobuf message, mapping corruption to a protocol
/// error.
///
/// # Errors
///
/// Returns [`ProviderError::Protocol`] when `payload` is not the message.
pub(crate) fn decode_message<M: prost::Message + Default>(
    what: &'static str,
    payload: &[u8],
) -> Result<M, ProviderError> {
    M::decode(payload).map_err(|_| ProviderError::Protocol {
        family: Family::Cursor,
        detail: format!("cursor {what} frame is not valid protobuf"),
    })
}
