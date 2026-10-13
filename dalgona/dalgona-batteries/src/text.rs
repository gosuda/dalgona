// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Shared text cuts for the batteries: one byte-prefix owner and one
//! whitespace-collapse owner.

/// Returns the longest prefix of `value` with at most `max` bytes.
///
/// The cut never splits a UTF-8 sequence.
pub(crate) fn byte_prefix(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Collapses whitespace runs to one ASCII space and trims both ends.
pub(crate) fn normalize_ws(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_keeps_short_text_whole() {
        assert_eq!(byte_prefix("", 0), "");
        assert_eq!(byte_prefix("abc", 3), "abc");
        assert_eq!(byte_prefix("abc", 99), "abc");
    }

    #[test]
    fn prefix_is_the_longest_boundary_prefix_at_every_limit() {
        let text = "aé界b";
        for max in 0..=text.len() + 1 {
            let prefix = byte_prefix(text, max);
            assert!(prefix.len() <= max, "limit {max}");
            assert!(text.starts_with(prefix), "limit {max}");
            let rest = &text[prefix.len()..];
            if !rest.is_empty() {
                let next_len = rest.chars().next().map_or(0, char::len_utf8);
                assert!(
                    prefix.len() + next_len > max,
                    "limit {max}: could keep more"
                );
            }
        }
    }

    #[test]
    fn normalization_collapses_and_trims() {
        assert_eq!(normalize_ws("  a\tb\n c  "), "a b c");
        assert_eq!(normalize_ws(""), "");
        assert_eq!(normalize_ws("one"), "one");
    }
}
