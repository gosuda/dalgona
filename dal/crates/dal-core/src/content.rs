use serde::{Deserialize, Serialize};

use crate::BlobId;

/// A message's text, image bytes, or reference to stored content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    /// UTF-8 text stored inline.
    Text {
        /// The text content.
        text: Box<str>,
    },
    /// Image bytes stored inline.
    Image {
        /// The image's media type.
        mime: Box<str>,
        /// The encoded image content.
        bytes: Box<[u8]>,
    },
    /// Content stored under its digest in the session store.
    Blob {
        /// The content digest.
        blob_id: BlobId,
        /// The stored content's media type.
        mime: Box<str>,
        /// The stored content's byte length, as reported when the blob was published.
        bytes: u64,
    },
}

/// The boundary between inline and separately stored content.
#[derive(Clone, Copy, Debug)]
pub struct ContentLimits;

impl ContentLimits {
    /// The maximum number of bytes allowed in one inline content part.
    pub const INLINE_BYTES: usize = 16_384;

    /// Checks the inline byte count without reading a blob.
    ///
    /// # Errors
    /// Returns `ContentError::InlineTooLarge` when inline content exceeds the limit.
    pub fn validate(part: &Part) -> Result<(), ContentError> {
        let actual = match part {
            Part::Text { text } => text.len(),
            Part::Image { bytes, .. } => bytes.len(),
            Part::Blob { .. } => return Ok(()),
        };
        if actual > Self::INLINE_BYTES {
            return Err(ContentError::InlineTooLarge { actual });
        }
        Ok(())
    }
}

/// Inline content exceeds the journal's size boundary.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ContentError {
    /// The caller must store this content as a blob instead.
    #[error("inline content is {actual} bytes; the limit is 16384; use a blob")]
    InlineTooLarge {
        /// The actual inline byte count.
        actual: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_limit_counts_bytes_and_accepts_the_boundary() {
        for length in [ContentLimits::INLINE_BYTES, ContentLimits::INLINE_BYTES + 1] {
            let expected = if length == ContentLimits::INLINE_BYTES {
                Ok(())
            } else {
                Err(ContentError::InlineTooLarge { actual: length })
            };
            assert_eq!(
                ContentLimits::validate(&Part::Text {
                    text: "x".repeat(length).into()
                }),
                expected
            );
            assert_eq!(
                ContentLimits::validate(&Part::Image {
                    mime: "image/png".into(),
                    bytes: vec![0; length].into()
                }),
                expected
            );
        }
        let text = "é".repeat(ContentLimits::INLINE_BYTES);
        assert_eq!(
            ContentLimits::validate(&Part::Text {
                text: text.clone().into()
            }),
            Err(ContentError::InlineTooLarge { actual: text.len() })
        );
        assert_eq!(
            ContentLimits::validate(&Part::Blob {
                blob_id: BlobId::from_bytes(text.as_bytes()),
                mime: "text/plain".into(),
                bytes: u64::try_from(text.len()).expect("test length fits in u64"),
            }),
            Ok(())
        );
    }

    #[test]
    fn blob_part_serializes_its_stored_length_at_both_boundaries() {
        // The store's publish cap (dal-store `MAX_BLOB`); 64 MiB exceeds the inline bound
        // and must still validate, since the declared length is not inline content.
        const MAX_BLOB: u64 = 64 * 1024 * 1024;
        let blob_id = BlobId::from_bytes(b"stored");
        for bytes in [0, MAX_BLOB] {
            let part = Part::Blob {
                blob_id,
                mime: "image/png".into(),
                bytes,
            };
            assert_eq!(ContentLimits::validate(&part), Ok(()));
            let encoded = sonic_rs::to_string(&part).expect("serialize blob part");
            assert_eq!(
                encoded,
                format!(
                    r#"{{"type":"blob","blob_id":"{blob_id}","mime":"image/png","bytes":{bytes}}}"#
                )
            );
            assert_eq!(
                sonic_rs::from_str::<Part>(&encoded).expect("deserialize blob part"),
                part
            );
        }
        let missing = format!(r#"{{"type":"blob","blob_id":"{blob_id}","mime":"image/png"}}"#);
        assert!(sonic_rs::from_str::<Part>(&missing).is_err());
    }
}
