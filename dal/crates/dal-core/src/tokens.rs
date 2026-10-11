//! Shared input-size estimation.
//!
//! One rate owns every token estimate in the platform: 3.5 characters per
//! token, rounded up. Compaction triggers, compaction inputs, and compaction
//! summaries must agree on it, so it lives here and nowhere else. Estimates
//! are never measured counts; providers report only whole-request usage.

/// Estimates tokens for `characters` Unicode scalar values at 3.5
/// characters per token, rounded up. Callers that hold byte lengths keep
/// bytes standing in for characters, the same basis the projection tracks.
#[must_use]
pub fn estimate_tokens(characters: u64) -> u64 {
    characters.saturating_mul(2).div_ceil(7)
}

/// Estimates the tokens in `text` at 3.5 characters per token, rounded up.
/// A character is one Unicode scalar value, so multi-byte text is not
/// over-counted.
#[must_use]
pub fn estimate_text_tokens(text: &str) -> u64 {
    estimate_tokens(u64::try_from(text.chars().count()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_counts_three_and_a_half_characters_per_token_rounded_up() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("a"), 1);
        assert_eq!(estimate_text_tokens(&"a".repeat(7)), 2);
        assert_eq!(estimate_text_tokens(&"a".repeat(35)), 10);
        assert_eq!(estimate_text_tokens(&"a".repeat(36)), 11);
    }

    #[test]
    fn estimate_counts_characters_not_bytes() {
        assert_eq!(
            estimate_text_tokens(&"é".repeat(35)),
            estimate_text_tokens(&"e".repeat(35)),
        );
    }
}
