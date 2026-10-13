//! Magic-byte recognition of the image formats `read` returns as images.

/// Image parts of this size and larger leave the call as blob ids at the
/// dispatch output boundary; smaller images stay inline.
pub(crate) const IMAGE_BLOB_THRESHOLD: usize = 16 * 1024;
/// The largest image file `read` returns.
pub(crate) const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

// The read tool relies on the core inline bound matching its blob threshold.
const _: () = assert!(IMAGE_BLOB_THRESHOLD == dal_core::ContentLimits::INLINE_BYTES);

/// An image format recognized from its leading bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl ImageKind {
    /// Recognizes an image from the first bytes of a file.
    pub(crate) fn detect(prefix: &[u8]) -> Option<Self> {
        if prefix.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(Self::Png)
        } else if prefix.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if prefix.starts_with(b"GIF8") {
            Some(Self::Gif)
        } else if prefix.starts_with(b"RIFF") && prefix.get(8..12) == Some(b"WEBP".as_slice()) {
            Some(Self::Webp)
        } else {
            None
        }
    }

    /// Returns the media type of the format.
    pub(crate) fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ImageKind;

    #[test]
    fn signatures_and_near_misses() {
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00];
        assert_eq!(ImageKind::detect(&png), Some(ImageKind::Png));
        assert_eq!(ImageKind::detect(&png[..7]), None);
        assert_eq!(
            ImageKind::detect(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageKind::Jpeg)
        );
        assert_eq!(ImageKind::detect(&[0xFF, 0xD8]), None);
        assert_eq!(ImageKind::detect(b"GIF89a"), Some(ImageKind::Gif));
        assert_eq!(ImageKind::detect(b"GIF"), None);
        assert_eq!(
            ImageKind::detect(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(ImageKind::Webp)
        );
        assert_eq!(ImageKind::detect(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(ImageKind::detect(b"RIFF\0\0\0\0WEB"), None);
        assert_eq!(ImageKind::detect(b""), None);
        assert_eq!(ImageKind::Webp.mime(), "image/webp");
        assert_eq!(ImageKind::Jpeg.mime(), "image/jpeg");
    }
}
