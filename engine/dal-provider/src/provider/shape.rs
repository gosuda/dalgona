//! Request-time headers of the model-list fetch, by provider shape.

use dal_core::Family;

use super::ProviderEntry;
use crate::{
    auth::{credential::Credential, oauth::CODEX_ORIGINATOR},
    error::ProviderError,
    provider::{AuthStyle, Hook},
};

/// The header set a provider's requests carry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shape {
    /// The configured API-key header, or a bearer token for OAuth.
    Plain,
    /// Plain headers plus the Anthropic version and, for a Claude OAuth
    /// sign-in, the Claude Code beta markers. An OAuth sign-in of another
    /// provider on the Anthropic wire (Kimi Code) carries none of them.
    Claude,
    /// A bearer token plus the `ChatGPT` account and originator markers.
    Codex,
}

impl Shape {
    /// The shape of a configured provider that has no table row.
    #[must_use]
    pub const fn of_family(family: Family) -> Self {
        match family {
            // These families have no dedicated header shape, so `Shape::headers`
            // builds the plain set for them; an adapter that builds its own
            // headers does not consult it. Every one is named, so a new family
            // must choose a shape here.
            Family::Chat
            | Family::Responses
            | Family::Bedrock
            | Family::Cursor
            | Family::Devin
            | Family::Droid
            | Family::GitLab
            | Family::GoogleGenerativeAi
            | Family::GoogleVertex
            | Family::GoogleGeminiCli
            | Family::GoogleCloudCodeAssist
            | Family::GoogleAntigravity
            | Family::Mistral
            | Family::Ollama
            | Family::PiMessages => Self::Plain,
            Family::Anthropic => Self::Claude,
            Family::Codex => Self::Codex,
        }
    }

    pub(crate) fn headers(
        self,
        provider: &ProviderEntry,
        credential: &Credential,
    ) -> Result<Vec<(String, String)>, ProviderError> {
        // `auth = none` is a contract about every shape: the entry sends no
        // credential, whatever a resolver found for it. Checking here, ahead
        // of the family dispatch, keeps a stored OAuth login or an API key
        // from reaching a bearer, Claude-marker, or Codex header.
        if provider.auth == AuthStyle::None && !matches!(credential, Credential::None) {
            return Err(credential_under_none(provider));
        }
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
        Credential::ApiKey { key } => Ok(vec![key_header(provider, key.expose())?]),
        Credential::OAuth(oauth) => Ok(vec![bearer_header(oauth.access_token.expose())]),
        // A `none` entry needs no credential and gets no header; every other
        // style still refuses to build a request without one.
        Credential::None if provider.auth == AuthStyle::None => Ok(Vec::new()),
        Credential::None => Err(no_credentials(provider)),
    }
}

/// The `authorization` header that carries an OAuth access token.
fn bearer_header(token: &str) -> (String, String) {
    (String::from("authorization"), format!("Bearer {token}"))
}

/// The header that carries `key` under the entry's authorization style.
fn key_header(provider: &ProviderEntry, key: &str) -> Result<(String, String), ProviderError> {
    let (name, value) = match provider.auth {
        AuthStyle::Bearer => ("authorization", format!("Bearer {key}")),
        AuthStyle::XApiKey => ("x-api-key", key.to_owned()),
        AuthStyle::ApiKey => ("api-key", key.to_owned()),
        AuthStyle::CfAigAuthorization => ("cf-aig-authorization", format!("Bearer {key}")),
        // Unreachable through `Shape::headers`, which refuses a held
        // credential under `none` first; the arm keeps the match exhaustive
        // and refuses rather than invent a header if a caller reaches it.
        AuthStyle::None => return Err(credential_under_none(provider)),
    };
    Ok((String::from(name), value))
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
    if matches!(credential, Credential::OAuth(_)) && is_claude_oauth(provider) {
        headers.push((
            String::from("anthropic-beta"),
            String::from("claude-code-20250219,oauth-2025-04-20"),
        ));
        headers.push((String::from("x-app"), String::from("cli")));
    }
    Ok(headers)
}

/// Whether an OAuth credential of `provider` is a Claude sign-in and so
/// carries the Claude Code markers: the Claude hook, or an entry that names
/// no sign-in row. Any other OAuth hook on the Anthropic wire (Kimi Code)
/// is a different vendor's session and gets only its bearer.
fn is_claude_oauth(provider: &ProviderEntry) -> bool {
    provider
        .def
        .and_then(|def| def.oauth)
        .is_none_or(|oauth| oauth.hook == Hook::ClaudeCode)
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

/// The refusal for a held credential under `auth = none`. The text is fixed
/// and names only the provider id, never a key, token, or account.
fn credential_under_none(provider: &ProviderEntry) -> ProviderError {
    ProviderError::InvalidRequest {
        message: format!(
            "{} sends no credential, so it cannot send an API key or an OAuth login",
            provider.id
        ),
    }
}
