//! The Google family registry: one module per surface behind the shared
//! `GenerateContent` grammar, with the endpoint paths every surface joins to
//! its base URL.

use super::*;

#[test]
fn cca_stream_path_is_the_internal_method() {
    assert_eq!(CCA_STREAM_PATH, "v1internal:streamGenerateContent?alt=sse");
}

#[test]
fn signature_sentinel_spelling_matches_the_pin() {
    assert_eq!(
        shared::SKIP_THOUGHT_SIGNATURE,
        "skip_thought_signature_validator"
    );
}
