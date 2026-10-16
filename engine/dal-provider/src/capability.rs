//! Auxiliary capability vocabulary: kind-safe dialect identity and media bytes.
//!
//! [`Kind`] names what a call produces, one closed [`Api`] enum per kind names
//! the wire dialect, [`AnyApi`] carries any dialect for catalog rows and
//! listings, [`Target`] names one resolved destination, and [`Media`] carries
//! raw image or audio bytes with their media type. Fields stay public and
//! readonly with no getters; callers validate untrusted values at the real use
//! boundary because a directly constructed value skips [`Media::new`].

mod kind;
#[cfg(test)]
mod tests;

pub use kind::{
    AnyApi, Api, EmbeddingApi, GroundingApi, ImageApi, JudgmentApi, Kind, RerankApi, SpeechApi,
    TranscriptionApi, VideoApi,
};

use std::{error::Error, fmt};

/// Why [`Media::new`] refused a media type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaError {
    /// The media type is empty.
    EmptyMime,
    /// The media type is not exactly `type/subtype` of non-empty tokens.
    InvalidMime {
        /// The rejected media type, stored verbatim.
        mime: Box<str>,
    },
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyMime => f.write_str("media mime type is empty."),
            Self::InvalidMime { mime } => {
                write!(
                    f,
                    "invalid media mime type {mime:?}: must be \"type/subtype\" \
                     without whitespace or control characters."
                )
            }
        }
    }
}

impl Error for MediaError {}

/// One resolved destination: which configured provider, which wire model id,
/// which dialect, and which base URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target<A> {
    /// The configured provider id, the key the credential store uses.
    pub provider: Box<str>,
    /// The wire model id the request sends.
    pub model: Box<str>,
    /// The wire dialect of the call.
    pub api: A,
    /// The base URL the request builds on.
    pub base_url: Box<str>,
}

/// Raw bytes with a media type, for images in and out and for audio.
///
/// The buffer is reference counted: cloning is `O(1)`, and building from a
/// decoded [`Vec`] takes ownership without copying it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Media {
    /// The media type, exactly `type/subtype` with no whitespace or controls.
    pub mime: Box<str>,
    /// The raw media bytes.
    pub bytes: bytes::Bytes,
}

impl Media {
    /// Builds media from a media type and an owned or shared buffer.
    ///
    /// The buffer moves in as is: a [`Vec`] becomes the byte buffer without
    /// copying, and a [`bytes::Bytes`] shares its existing storage.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::EmptyMime`] when `mime` is empty, or
    /// [`MediaError::InvalidMime`] when it is not exactly `type/subtype` of
    /// non-empty tokens without whitespace or control characters. There is no
    /// size cap here; response bounds belong to the call, not the value.
    pub fn new(
        mime: impl Into<Box<str>>,
        bytes: impl Into<bytes::Bytes>,
    ) -> Result<Self, MediaError> {
        let mime: Box<str> = mime.into();
        if mime.is_empty() {
            return Err(MediaError::EmptyMime);
        }
        if !is_valid_mime(&mime) {
            return Err(MediaError::InvalidMime { mime });
        }
        Ok(Self {
            mime,
            bytes: bytes.into(),
        })
    }
}

/// Whether `mime` is exactly `type/subtype` of non-empty tokens with no
/// whitespace or control characters.
fn is_valid_mime(mime: &str) -> bool {
    if mime.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((ty, sub)) = mime.split_once('/') else {
        return false;
    };
    if ty.is_empty() || sub.is_empty() {
        return false;
    }
    !sub.contains('/')
}
