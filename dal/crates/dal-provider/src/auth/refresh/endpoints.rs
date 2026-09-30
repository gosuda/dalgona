//! OAuth provider identities and token endpoints.
//!
//! Credential keys (`anthropic`, `openai-codex`) map to one provider each;
//! endpoints default to production with test overrides.

use super::super::oauth::{CLAUDE_CLIENT_ID, CLAUDE_TOKEN_URL, CODEX_CLIENT_ID, CODEX_TOKEN_URL};
use crate::{error::ProviderError, http::endpoint};
use dal_core::Family;
use url::Url;

/// An `auth.json` credential key that holds an OAuth sign-in.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OAuthProvider {
    /// The `anthropic` entry (Claude sign-in).
    Anthropic,
    /// The `openai-codex` entry (`ChatGPT` sign-in).
    OpenAiCodex,
}

impl OAuthProvider {
    /// The provider for an `auth.json` key, when that key can hold an OAuth
    /// sign-in.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "anthropic" => Some(Self::Anthropic),
            "openai-codex" => Some(Self::OpenAiCodex),
            _ => None,
        }
    }

    /// The `auth.json` key and provider id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCodex => "openai-codex",
        }
    }

    /// The API family whose errors name refresh transport failures.
    #[must_use]
    pub const fn family(self) -> Family {
        match self {
            Self::Anthropic => Family::Anthropic,
            Self::OpenAiCodex => Family::Codex,
        }
    }

    pub(crate) const fn client_id(self) -> &'static str {
        match self {
            Self::Anthropic => CLAUDE_CLIENT_ID,
            Self::OpenAiCodex => CODEX_CLIENT_ID,
        }
    }
}

/// Why a caller asks for a refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshReason {
    /// Before a request: refresh only inside the proactive window.
    Expiring,
    /// After a 401 with the held token: refresh even inside the window,
    /// unless the stored token already changed.
    Rejected,
}

/// The token endpoint URL of each OAuth provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenEndpoints {
    anthropic: Url,
    openai_codex: Url,
}

impl TokenEndpoints {
    /// The production endpoints: `https://platform.claude.com/v1/oauth/token`
    /// and `https://auth.openai.com/oauth/token`.
    ///
    /// # Panics
    ///
    /// Never: both are constant `https` URLs.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the production token endpoints are constant https URLs"
    )]
    pub fn production() -> Self {
        Self {
            anthropic: Url::parse(CLAUDE_TOKEN_URL).expect("the Claude token URL parses"),
            openai_codex: Url::parse(CODEX_TOKEN_URL).expect("the Codex token URL parses"),
        }
    }

    /// Endpoints under other bases, for tests and replay servers: the
    /// Anthropic endpoint is `<anthropic_base>/v1/oauth/token`, the Codex
    /// endpoint `<codex_base>/oauth/token`.
    ///
    /// # Errors
    ///
    /// Every [`endpoint`] failure: [`ProviderError::PlainHttp`] for plain
    /// `http` on a non-loopback host and [`ProviderError::Transport`] for a
    /// base that is not an admissible URL.
    pub fn with_bases(anthropic_base: &str, codex_base: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            anthropic: endpoint(
                Family::Anthropic,
                anthropic_base,
                super::ANTHROPIC_TOKEN_PATH,
            )?,
            openai_codex: endpoint(Family::Codex, codex_base, super::CODEX_TOKEN_PATH)?,
        })
    }

    /// The token endpoint of `provider`.
    #[must_use]
    pub const fn url(&self, provider: OAuthProvider) -> &Url {
        match provider {
            OAuthProvider::Anthropic => &self.anthropic,
            OAuthProvider::OpenAiCodex => &self.openai_codex,
        }
    }
}
