use super::*;

/// Every fingerprint byte equals the oh-my-pi reference transcription.
#[test]
fn fingerprint_bytes_match_reference() {
    assert_eq!(CLAUDE_CODE_VERSION, "2.1.280");
    assert_eq!(
        user_agent(CLAUDE_CODE_VERSION),
        "claude-cli/2.1.280 (external, cli)"
    );
    assert_eq!(
        CLAUDE_CODE_SYSTEM_INSTRUCTION,
        "You are Claude Code, Anthropic's official CLI for Claude."
    );
    assert_eq!(CLAUDE_CODE_BETA, "claude-code-20250219");
    assert_eq!(OAUTH_BETA, "oauth-2025-04-20");
    assert_eq!(X_APP, "cli");
    assert_eq!(encode_tool_name("read"), "_read");
    assert_eq!(encode_tool_name("Web_Search"), "Web_Search");
    assert_eq!(encode_tool_name("computer"), "computer");
    assert_eq!(decode_tool_name("_read"), "read");
    assert_eq!(decode_tool_name("__x"), "_x");
    assert_eq!(decode_tool_name("web_search"), "web_search");
}
