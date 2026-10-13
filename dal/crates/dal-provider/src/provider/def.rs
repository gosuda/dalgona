//! The built-in provider table: the only place that knows a built-in provider.
//!
//! Every other site asks [`PROVIDERS`] or carries a [`ProviderDef`] resolved
//! once from it. The rows are plain `const` data; a sign-in method is derived
//! from a row by [`ProviderDef::methods`] and never stored.

use dal_core::Family;

use super::{AuthStyle, Shape, Transport};
use crate::auth::{login::Method, oauth::CODEX_ORIGINATOR};

/// One built-in provider: identity, wire family, endpoint, and how it signs in.
#[derive(Clone, Copy, Debug)]
pub struct ProviderDef {
    /// The provider id used in model references and as the `auth.json` key;
    /// lowercase letters, digits, and hyphens.
    pub id: &'static str,
    /// The display name used in sign-in messages.
    pub name: &'static str,
    /// The API family the provider implements.
    pub family: Family,
    /// The default endpoint base URL.
    pub base_url: &'static str,
    /// The default transport.
    pub transport: Transport,
    /// API-key sign-in; `None` when the provider takes no API key.
    pub key: Option<KeySpec>,
    /// OAuth sign-in; `None` when the provider has no OAuth flow.
    pub oauth: Option<OAuthSpec>,
    /// The request-time header set.
    pub shape: Shape,
}

/// How a provider takes an API key.
#[derive(Clone, Copy, Debug)]
pub struct KeySpec {
    /// The default authorization header style.
    pub style: AuthStyle,
    /// Environment variables that may hold the key, in resolve order.
    pub env: &'static [&'static str],
}

/// How a provider signs in with OAuth.
#[derive(Clone, Copy, Debug)]
pub struct OAuthSpec {
    /// The public OAuth client id.
    pub client_id: &'static str,
    /// The authorization endpoint.
    pub authorize_url: &'static str,
    /// The token endpoint, for code exchange and refresh.
    pub token_url: &'static str,
    /// The requested scopes.
    pub scopes: &'static [&'static str],
    /// The authorization query, in the exact order the vendor expects.
    pub query: &'static [(&'static str, QueryValue)],
    /// The loopback address the browser redirects to.
    pub redirect: Redirect,
    /// How the browser sign-in completes.
    pub completion: Completion,
    /// The device-code sign-in; `None` when the provider has none.
    pub device: Option<Device>,
    /// The refresh-token revocation endpoint called on sign-out.
    pub revoke_url: Option<&'static str>,
    /// The vendor steps the generic flow cannot express.
    pub hook: Hook,
}

/// One value of an authorization query: a fixed text or a per-attempt value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryValue {
    /// A fixed text.
    Text(&'static str),
    /// The row's OAuth client id.
    ClientId,
    /// The loopback redirect URI of this attempt.
    RedirectUri,
    /// The row's scopes, joined by spaces.
    Scope,
    /// The PKCE code challenge of this attempt.
    Challenge,
    /// The `state` of this attempt.
    State,
}

/// The loopback redirect a vendor registered for its OAuth client.
#[derive(Clone, Copy, Debug)]
pub struct Redirect {
    /// The host named in the redirect URL; the listener binds `127.0.0.1`.
    pub host: &'static str,
    /// The vendor-registered port.
    pub port: u16,
    /// The vendor-registered path, with its leading slash.
    pub path: &'static str,
}

/// How a browser sign-in completes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    /// Only the loopback redirect completes it; a busy port falls back to the
    /// device sign-in.
    Callback,
    /// The loopback redirect or a pasted code or redirect URL completes it;
    /// a busy port falls back to any free port.
    PasteCode,
}

/// A device-code sign-in: the user enters a code at a verification page.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    /// The endpoint that issues the user code.
    pub usercode_url: &'static str,
    /// The endpoint polled until the user approves.
    pub poll_url: &'static str,
    /// The page where the user enters the code.
    pub page_url: &'static str,
    /// The redirect URI sent with the final code exchange.
    pub redirect_uri: &'static str,
}

/// A vendor step the generic OAuth flow cannot express. A new variant is a
/// Rust change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hook {
    /// `ChatGPT` sign-in: the code exchange is form-encoded and the ID token
    /// names the account the grant belongs to. The authorize query encodes
    /// spaces as `%20`. After sign-in the model list is fetched with the
    /// account headers.
    CodexAccount,
    /// Claude sign-in: the code exchange is JSON and echoes the PKCE verifier
    /// as `state`. The authorize query encodes spaces as `+`.
    ClaudeCode,
}

impl ProviderDef {
    /// The sign-in methods the row supports, in `api_key`, `browser`,
    /// `device` order.
    pub fn methods(&self) -> impl Iterator<Item = Method> {
        [
            (self.key.is_some(), Method::ApiKey),
            (self.oauth.is_some(), Method::Browser),
            (
                self.oauth.is_some_and(|oauth| oauth.device.is_some()),
                Method::Device,
            ),
        ]
        .into_iter()
        .filter_map(|(offered, method)| offered.then_some(method))
    }

    /// Whether the row supports `method`.
    #[must_use]
    pub fn offers(&self, method: Method) -> bool {
        self.methods().any(|offered| offered == method)
    }
}

pub(crate) const OPENAI: ProviderDef = ProviderDef {
    id: "openai",
    name: "OpenAI",
    family: Family::Responses,
    base_url: "https://api.openai.com/v1",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["OPENAI_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
};

pub(crate) const ANTHROPIC: ProviderDef = ProviderDef {
    id: "anthropic",
    name: "Anthropic",
    family: Family::Anthropic,
    base_url: "https://api.anthropic.com",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::XApiKey,
        env: &["ANTHROPIC_API_KEY"],
    }),
    oauth: Some(OAuthSpec {
        client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        authorize_url: "https://claude.ai/oauth/authorize",
        token_url: "https://platform.claude.com/v1/oauth/token",
        scopes: &[
            "org:create_api_key",
            "user:profile",
            "user:inference",
            "user:sessions:claude_code",
            "user:mcp_servers",
            "user:file_upload",
        ],
        query: &[
            ("code", QueryValue::Text("true")),
            ("client_id", QueryValue::ClientId),
            ("response_type", QueryValue::Text("code")),
            ("redirect_uri", QueryValue::RedirectUri),
            ("scope", QueryValue::Scope),
            ("code_challenge", QueryValue::Challenge),
            ("code_challenge_method", QueryValue::Text("S256")),
            ("state", QueryValue::State),
        ],
        redirect: Redirect {
            host: "localhost",
            port: 53692,
            path: "/callback",
        },
        completion: Completion::PasteCode,
        device: None,
        revoke_url: None,
        hook: Hook::ClaudeCode,
    }),
    shape: Shape::Claude,
};

pub(crate) const OPENAI_CODEX: ProviderDef = ProviderDef {
    id: "openai-codex",
    name: "OpenAI Codex",
    family: Family::Codex,
    base_url: "https://chatgpt.com/backend-api/codex",
    transport: Transport::Websocket,
    key: None,
    oauth: Some(OAuthSpec {
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        authorize_url: "https://auth.openai.com/oauth/authorize",
        token_url: "https://auth.openai.com/oauth/token",
        scopes: &[
            "openid",
            "profile",
            "email",
            "offline_access",
            "api.connectors.read",
            "api.connectors.invoke",
        ],
        query: &[
            ("response_type", QueryValue::Text("code")),
            ("client_id", QueryValue::ClientId),
            ("redirect_uri", QueryValue::RedirectUri),
            ("scope", QueryValue::Scope),
            ("code_challenge", QueryValue::Challenge),
            ("code_challenge_method", QueryValue::Text("S256")),
            ("state", QueryValue::State),
            ("id_token_add_organizations", QueryValue::Text("true")),
            ("codex_cli_simplified_flow", QueryValue::Text("true")),
            ("originator", QueryValue::Text(CODEX_ORIGINATOR)),
        ],
        redirect: Redirect {
            host: "localhost",
            port: 1455,
            path: "/auth/callback",
        },
        completion: Completion::Callback,
        device: Some(Device {
            usercode_url: "https://auth.openai.com/api/accounts/deviceauth/usercode",
            poll_url: "https://auth.openai.com/api/accounts/deviceauth/token",
            page_url: "https://auth.openai.com/codex/device",
            redirect_uri: "https://auth.openai.com/deviceauth/callback",
        }),
        revoke_url: Some("https://auth.openai.com/oauth/revoke"),
        hook: Hook::CodexAccount,
    }),
    shape: Shape::Codex,
};

/// Every built-in provider, in the order the configuration lists them.
pub const PROVIDERS: &[ProviderDef] = &[OPENAI, OPENAI_CODEX, ANTHROPIC];

/// The built-in provider with `id`, if any.
#[must_use]
pub fn find(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|def| def.id == id)
}

#[cfg(test)]
mod tests;
