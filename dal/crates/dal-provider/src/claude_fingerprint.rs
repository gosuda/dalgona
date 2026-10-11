//! The Claude Code request fingerprint a Claude OAuth token travels with.
//!
//! Every byte here is transcribed from oh-my-pi
//! (`packages/ai/src/providers/claude-code-fingerprint.ts` and the tool-name
//! rules of `anthropic-identity.ts`); nothing is added beyond them. The
//! Anthropic family applies the fingerprint only to requests signed with a
//! Claude OAuth access token: the `user-agent` and `x-app` headers, the two
//! leading betas, the identity system block, and the wire-only tool prefix.
//!
//! The pinned [`CLAUDE_CODE_VERSION`] can go stale. A live
//! `claude_code_version_too_old` rejection is answered by raising this pin
//! by hand at the source, as the provider-layer contingency row requires;
//! nothing here adopts a version or retries at run time.

use std::borrow::Cow;

/// The pinned Claude Code CLI version represented on the wire.
pub(crate) const CLAUDE_CODE_VERSION: &str = "2.1.280";

/// The identity text of the first system block of every OAuth request.
pub(crate) const CLAUDE_CODE_SYSTEM_INSTRUCTION: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// The beta that marks a Claude Code agent request; sent first.
pub(crate) const CLAUDE_CODE_BETA: &str = "claude-code-20250219";

/// The beta that admits an OAuth bearer token; sent second.
pub(crate) const OAUTH_BETA: &str = "oauth-2025-04-20";

/// The `x-app` header value.
pub(crate) const X_APP: &str = "cli";

/// The wire-only prefix that isolates custom tools from built-in tools.
pub(crate) const CLAUDE_TOOL_PREFIX: &str = "_";

/// Tool names Anthropic reserves for its built-in tools; they keep their
/// spelling on the wire. Compared case-insensitively.
const BUILTIN_TOOL_NAMES: [&str; 4] = ["web_search", "code_execution", "text_editor", "computer"];

/// The `user-agent` value for `version`: `claude-cli/<version> (external, cli)`.
#[must_use]
pub(crate) fn user_agent(version: &str) -> String {
    format!("claude-cli/{version} (external, cli)")
}

/// The wire name of a tool: [`CLAUDE_TOOL_PREFIX`] plus `name`, except for
/// the built-in tool names, which are sent unchanged.
#[must_use]
pub(crate) fn encode_tool_name(name: &str) -> Cow<'_, str> {
    if BUILTIN_TOOL_NAMES
        .iter()
        .any(|builtin| builtin.eq_ignore_ascii_case(name))
    {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(format!("{CLAUDE_TOOL_PREFIX}{name}"))
    }
}

/// Removes one leading [`CLAUDE_TOOL_PREFIX`] from a wire tool name.
#[must_use]
pub(crate) fn decode_tool_name(name: &str) -> &str {
    name.strip_prefix(CLAUDE_TOOL_PREFIX).unwrap_or(name)
}

#[cfg(test)]
mod tests;
