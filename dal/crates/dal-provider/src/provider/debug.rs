//! Provider debug formatting for HTTP adapters.

use super::api::{Http, Provider};

impl std::fmt::Debug for Http {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Http")
            .field("family", &self.entry.family)
            .field("provider", &self.entry.id)
            .field("model", &self.resolved.entry.id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chat(http) => formatter.debug_tuple("Chat").field(http).finish(),
            Self::Responses(http) => formatter.debug_tuple("Responses").field(http).finish(),
            Self::Codex(http) => formatter.debug_tuple("Codex").field(http).finish(),
            Self::Anthropic(http) => formatter.debug_tuple("Anthropic").field(http).finish(),
            Self::Scripted(_) => formatter.write_str("Scripted"),
        }
    }
}
