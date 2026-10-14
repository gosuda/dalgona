//! OAuth token endpoints of the providers in the table.
//!
//! Endpoints default to production with test overrides.

use crate::{Hook, PROVIDERS, ProviderDef, ProviderError, http::endpoint};
use url::Url;

/// Why a caller asks for a refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshReason {
    /// Before a request: refresh only inside the proactive window.
    Expiring,
    /// After a 401 with the held token: refresh even inside the window,
    /// unless the stored token already changed.
    Rejected,
}

/// The token endpoint URL of each OAuth provider in the table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenEndpoints {
    urls: Vec<(&'static str, Url)>,
}

impl TokenEndpoints {
    /// The production endpoints: each OAuth row's `token_url`.
    ///
    /// # Panics
    ///
    /// Never: every row's token URL is a constant `https` URL.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the production token endpoints are constant https URLs"
    )]
    pub fn production() -> Self {
        let urls = PROVIDERS
            .iter()
            .filter_map(|def| {
                let oauth = def.oauth?;
                let url = Url::parse(oauth.token_url).expect("the row's token URL parses");
                Some((def.id, url))
            })
            .collect();
        Self { urls }
    }

    /// Endpoints under other bases, for tests and replay servers: the Claude
    /// sign-in's endpoint moves to `<claude_base>/<path of its production
    /// token URL>` and the Codex sign-in's to `<codex_base>/<path of its
    /// production token URL>`.
    ///
    /// # Errors
    ///
    /// Every [`endpoint`] failure: [`ProviderError::PlainHttp`] for plain
    /// `http` on a non-loopback host and [`ProviderError::Transport`] for a
    /// base that is not an admissible URL.
    pub fn with_bases(claude_base: &str, codex_base: &str) -> Result<Self, ProviderError> {
        let mut urls = Vec::new();
        for def in PROVIDERS {
            let Some(oauth) = def.oauth else { continue };
            let base = match oauth.hook {
                Hook::ClaudeCode => claude_base,
                Hook::CodexAccount => codex_base,
            };
            let path = Url::parse(oauth.token_url)
                .map_err(|error| ProviderError::Transport {
                    family: def.family,
                    reason: error.to_string(),
                })?
                .path()
                .trim_start_matches('/')
                .to_owned();
            urls.push((def.id, endpoint(def.family, base, &path)?));
        }
        Ok(Self { urls })
    }

    /// The token endpoint of `provider`; `None` when it has no OAuth sign-in.
    #[must_use]
    pub fn url(&self, provider: &ProviderDef) -> Option<&Url> {
        self.urls
            .iter()
            .find(|(id, _)| *id == provider.id)
            .map(|(_, url)| url)
    }
}
