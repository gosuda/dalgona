// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! HTML to Markdown conversion for `web_fetch`.

/// The media kinds `web_fetch` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaKind {
    /// `text/html` and `application/xhtml+xml`: convert to Markdown.
    Convert,
    /// `text/*` and `application/json`: pass the body through unchanged.
    PassThrough,
}

/// Classifies a `Content-Type` value. MIME matching is case-insensitive; only
/// the part before the first `;` counts. `None` is an unknown media type.
pub(crate) fn classify_media_type(content_type: &str) -> Option<MediaKind> {
    let mime = content_type.split(';').next()?.trim();
    if mime.eq_ignore_ascii_case("text/html") || mime.eq_ignore_ascii_case("application/xhtml+xml")
    {
        Some(MediaKind::Convert)
    } else if mime.eq_ignore_ascii_case("application/json")
        || mime
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/"))
    {
        Some(MediaKind::PassThrough)
    } else {
        None
    }
}

/// Converts HTML to Markdown with `htmd::convert`.
pub(crate) fn to_markdown(html: &str) -> Result<String, std::io::Error> {
    htmd::convert(html)
}

/// Caps Markdown at `cap` bytes. Returns the text and whether it was cut.
/// The truncation marker names the effective cap, and the cut is on a char
/// boundary. The marker is appended after the capped content.
pub(crate) fn cap_markdown(mut markdown: String, cap: usize) -> (String, bool) {
    if markdown.len() <= cap {
        return (markdown, false);
    }

    let mut cut = cap;
    while !markdown.is_char_boundary(cut) {
        cut -= 1;
    }
    markdown.truncate(cut);
    let _ = std::fmt::Write::write_fmt(
        &mut markdown,
        format_args!("\n\n[dalgona: truncated at {cap} bytes]"),
    );
    (markdown, true)
}

#[cfg(test)]
mod tests {
    use super::{MediaKind, cap_markdown, classify_media_type};

    #[test]
    fn html_classifies_media_types() {
        let cases = [
            ("text/html; charset=utf-8", Some(MediaKind::Convert)),
            ("TEXT/HTML", Some(MediaKind::Convert)),
            ("application/xhtml+xml", Some(MediaKind::Convert)),
            ("application/json", Some(MediaKind::PassThrough)),
            ("text/plain", Some(MediaKind::PassThrough)),
            ("text/csv", Some(MediaKind::PassThrough)),
            ("image/png", None),
            ("", None),
        ];
        for (content_type, expected) in cases {
            assert_eq!(
                classify_media_type(content_type),
                expected,
                "{content_type:?}"
            );
        }
    }

    #[test]
    fn cap_markdown_appends_exact_marker() {
        let input = "a".repeat(200_000);
        let (markdown, truncated) = cap_markdown(input, 131_072);

        assert!(truncated);
        assert!(markdown.ends_with("\n\n[dalgona: truncated at 131072 bytes]"));
        assert!(markdown.len() <= 131_072 + "\n\n[dalgona: truncated at 131072 bytes]".len());

        let (actual, truncated) = cap_markdown("short markdown".to_owned(), 100);
        assert!(!truncated);
        assert_eq!(actual, "short markdown");
    }

    #[test]
    fn truncates_at_a_utf8_boundary() {
        let (markdown, truncated) = cap_markdown("界界界".to_owned(), 4);

        assert!(truncated);
        assert_eq!(
            markdown.split_once("\n\n"),
            Some(("界", "[dalgona: truncated at 4 bytes]")),
        );
    }

    #[test]
    fn zero_cap_keeps_only_the_truncation_marker() {
        let (markdown, truncated) = cap_markdown("text".to_owned(), 0);

        assert!(truncated);
        assert_eq!(markdown, "\n\n[dalgona: truncated at 0 bytes]");
    }
}
