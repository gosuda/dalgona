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

/// The token endpoint URL of each refreshing OAuth provider in the table (a
/// row whose hook never refreshes, `OpenRouter` or Devin, has none), and the
/// Factory
/// API base the refresh of the `WorkOS` sign-in checks the new session
/// against. A test endpoint set moves both together, as the sign-in's
/// [`LoginEndpoints`](crate::LoginEndpoints) do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenEndpoints {
    urls: Vec<(&'static str, Url)>,
    sessions: Vec<(&'static str, Box<str>)>,
}

impl TokenEndpoints {
    /// The production endpoints: each refreshing OAuth row's `token_url`, and
    /// the row's own API base for the session check.
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
                let oauth = def.oauth.filter(|oauth| oauth.hook.refreshes())?;
                let url = Url::parse(oauth.token_url).expect("the row's token URL parses");
                Some((def.id, url))
            })
            .collect();
        let sessions: Vec<(&'static str, Box<str>)> = PROVIDERS
            .iter()
            .filter(|def| {
                def.oauth
                    .is_some_and(|oauth| oauth.hook == Hook::WorkOsDevice)
            })
            .map(|def| (def.id, Box::from(def.base_url)))
            .collect();
        Self { urls, sessions }
    }

    /// Endpoints under other bases, for tests and replay servers: each
    /// sign-in's endpoint moves to `<base>/<path of its production token
    /// URL`, taking one base per refreshing sign-in, in Claude, Codex,
    /// `WorkOS`, GitLab, Radius order. The `WorkOS` sign-in's session check
    /// goes to its base too. Any other refreshing sign-in (Cursor, the two
    /// Cloud Code Assist rows, xAI, Meta, Kimi Code) gets no endpoint here,
    /// so it never reaches a production host from a test set;
    /// [`TokenEndpoints::with_base`] places it. Meta's endpoint is the key
    /// mint.
    ///
    /// # Errors
    ///
    /// Every [`endpoint`] failure: [`ProviderError::PlainHttp`] for plain
    /// `http` on a non-loopback host and [`ProviderError::Transport`] for a
    /// base that is not an admissible URL.
    pub fn with_bases(
        claude_base: &str,
        codex_base: &str,
        workos_base: &str,
        gitlab_base: &str,
        radius_base: &str,
    ) -> Result<Self, ProviderError> {
        let mut urls = Vec::new();
        let mut sessions: Vec<(&'static str, Box<str>)> = Vec::new();
        for def in PROVIDERS {
            let Some(oauth) = def.oauth else { continue };
            let base = match oauth.hook {
                Hook::ClaudeCode => claude_base,
                Hook::CodexAccount => codex_base,
                Hook::WorkOsDevice => workos_base,
                Hook::GitLabDuo => gitlab_base,
                Hook::RadiusGateway => radius_base,
                Hook::OpenRouterKey
                | Hook::DevinCliToken
                | Hook::CursorPoll
                | Hook::GoogleCloudCodeAssist
                | Hook::GoogleAntigravity
                | Hook::XaiDevice
                | Hook::MetaMuse
                | Hook::KimiCodeDevice => continue,
            };
            urls.push((def.id, relocated(def, oauth.token_url, base)?));
            if oauth.hook == Hook::WorkOsDevice {
                sessions.push((def.id, Box::from(base.trim_end_matches('/'))));
            }
        }
        Ok(Self { urls, sessions })
    }

    /// Moves the token endpoint of `provider`, a refreshing OAuth row, to
    /// `<base>/<path of its production token URL>`, replacing any endpoint
    /// the set already holds for it. The other rows keep theirs.
    ///
    /// # Errors
    ///
    /// [`ProviderError::AuthWrite`] when `provider` has no refreshing OAuth
    /// sign-in, and every [`endpoint`] failure for `base`.
    pub fn with_base(mut self, provider: &ProviderDef, base: &str) -> Result<Self, ProviderError> {
        let Some(oauth) = provider.oauth.filter(|oauth| oauth.hook.refreshes()) else {
            return Err(ProviderError::AuthWrite {
                reason: format!("{} has no refreshing OAuth sign-in", provider.id),
            });
        };
        let url = relocated(provider, oauth.token_url, base)?;
        self.urls.retain(|(id, _)| *id != provider.id);
        self.urls.push((provider.id, url));
        Ok(self)
    }

    /// The token endpoint of `provider`; `None` when it has no OAuth sign-in.
    #[must_use]
    pub fn url(&self, provider: &ProviderDef) -> Option<&Url> {
        self.urls
            .iter()
            .find(|(id, _)| *id == provider.id)
            .map(|(_, url)| url)
    }

    /// The API base `provider`'s refresh checks the new session against;
    /// `None` for a sign-in that checks none.
    #[must_use]
    pub(crate) fn session_base(&self, provider: &ProviderDef) -> Option<&str> {
        self.sessions
            .iter()
            .find(|(id, _)| *id == provider.id)
            .map(|(_, base)| &**base)
    }
}

/// `production`'s path under `base`, for `def`'s family.
fn relocated(def: &ProviderDef, production: &str, base: &str) -> Result<Url, ProviderError> {
    let path = Url::parse(production)
        .map_err(|error| ProviderError::Transport {
            family: def.family,
            reason: error.to_string(),
        })?
        .path()
        .trim_start_matches('/')
        .to_owned();
    endpoint(def.family, base, &path)
}
