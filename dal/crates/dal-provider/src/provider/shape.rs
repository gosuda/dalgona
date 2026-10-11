//! Request-time headers of the model-list fetch, by provider shape.

use dal_core::Family;

use super::ProviderEntry;
use crate::{
    auth::{credential::Credential, oauth::CODEX_ORIGINATOR},
    error::ProviderError,
    provider::AuthStyle,
};

/// The header set a provider's requests carry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shape {
    /// The configured API-key header, or a bearer token for OAuth.
    Plain,
    /// Plain headers plus the Anthropic version and, for OAuth, the Claude
    /// Code beta markers.
    Claude,
    /// A bearer token plus the `ChatGPT` account and originator markers.
    Codex,
}

impl Shape {
    /// The shape of a configured provider that has no table row.
    #[must_use]
    pub const fn of_family(family: Family) -> Self {
        match family {
            Family::Chat | Family::Responses => Self::Plain,
            Family::Anthropic => Self::Claude,
            Family::Codex => Self::Codex,
        }
    }

    pub(crate) fn headers(
        self,
        provider: &ProviderEntry,
        credential: &Credential,
    ) -> Result<Vec<(String, String)>, ProviderError> {
        match self {
            Self::Plain => plain_headers(provider, credential),
            Self::Claude => claude_headers(provider, credential),
            Self::Codex => codex_headers(provider, credential),
        }
    }
}

fn plain_headers(
    provider: &ProviderEntry,
    credential: &Credential,
) -> Result<Vec<(String, String)>, ProviderError> {
    match credential {
        Credential::ApiKey { key } => Ok(vec![(
            match provider.auth {
                AuthStyle::Bearer => String::from("authorization"),
                AuthStyle::XApiKey => String::from("x-api-key"),
            },
            match provider.auth {
                AuthStyle::Bearer => format!("Bearer {}", key.expose()),
                AuthStyle::XApiKey => key.expose().to_owned(),
            },
        )]),
        Credential::OAuth(oauth) => Ok(vec![(
            String::from("authorization"),
            format!("Bearer {}", oauth.access_token.expose()),
        )]),
        Credential::None => Err(no_credentials(provider)),
    }
}

fn claude_headers(
    provider: &ProviderEntry,
    credential: &Credential,
) -> Result<Vec<(String, String)>, ProviderError> {
    let mut headers = plain_headers(provider, credential)?;
    headers.push((
        String::from("anthropic-version"),
        String::from("2023-06-01"),
    ));
    if matches!(credential, Credential::OAuth(_)) {
        headers.push((
            String::from("anthropic-beta"),
            String::from("claude-code-20250219,oauth-2025-04-20"),
        ));
        headers.push((String::from("x-app"), String::from("cli")));
    }
    Ok(headers)
}

fn codex_headers(
    provider: &ProviderEntry,
    credential: &Credential,
) -> Result<Vec<(String, String)>, ProviderError> {
    let Credential::OAuth(oauth) = credential else {
        return Err(no_credentials(provider));
    };
    let Some(account_id) = oauth.account_id.as_ref() else {
        return Err(ProviderError::NoAccountId);
    };
    Ok(vec![
        (
            String::from("authorization"),
            format!("Bearer {}", oauth.access_token.expose()),
        ),
        (String::from("chatgpt-account-id"), account_id.clone()),
        (String::from("originator"), String::from(CODEX_ORIGINATOR)),
    ])
}

fn no_credentials(provider: &ProviderEntry) -> ProviderError {
    ProviderError::NoCredentials {
        provider: provider.id.to_string(),
    }
}
