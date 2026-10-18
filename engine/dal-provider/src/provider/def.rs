//! The built-in provider table: the only place that knows a built-in provider.
//!
//! Every other site asks [`PROVIDERS`] or carries a [`ProviderDef`] resolved
//! once from it. The rows are plain `const` data; a sign-in method is derived
//! from a row by [`ProviderDef::methods`] and never stored. The vendor rows
//! live here; the API-key presets live in `presets` and join the same table.

use dal_core::Family;

use super::{AuthStyle, Shape, Transport};
use crate::auth::{login::Method, oauth::CODEX_ORIGINATOR};

mod presets;

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
    /// How an explicit `providers.<id>.api` override treats the row's family.
    pub(crate) family_policy: FamilyPolicy,
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

/// How a built-in row treats an explicit `providers.<id>.api` override.
///
/// The distinction lives on the row, set once at its single construction
/// site, so no module keeps its own list of which ids are native and which
/// are presets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FamilyPolicy {
    /// A native vendor row: the endpoint, credentials, and header set speak
    /// one family, so any other `api` is refused. (`openai` additionally
    /// accepts its documented Chat alternate; see `decode_builtin` in the
    /// parent module.)
    Native,
    /// A generic API-key compatibility preset: an explicit `api` names any
    /// supported key family and wins, exactly as the named-provider path did
    /// before the row existed. The row's endpoint default, key material, and
    /// key-only sign-in are unchanged.
    Compatible,
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
    /// The environment variable whose non-blank value replaces `client_id`
    /// for this row's authorize, exchange, refresh, and revoke requests;
    /// `None` when the client id is fixed. The caller resolves it from a
    /// captured `EnvSnapshot`; the library never reads the process
    /// environment.
    pub client_id_env: Option<&'static str>,
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
    /// Whether the row signs in through the user's browser. A device-only
    /// row sets this to `false` and offers no `Method::Browser`; its OAuth
    /// sign-in runs only as `Method::Device`.
    pub browser: bool,
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
    /// `WorkOS` sign-in (Factory Droid): the device authorization and token
    /// endpoints take form-encoded bodies and the device poll delivers the
    /// tokens directly, with no second code exchange. After sign-in and
    /// after refresh the Factory whoami endpoint verifies the session.
    WorkOsDevice,
    /// `GitLab Duo` sign-in: the code exchange and refresh are form-encoded
    /// with the `api` scope. After sign-in and after refresh the
    /// direct-access cache drops its grants.
    GitLabDuo,
}

impl ProviderDef {
    /// The sign-in methods the row supports, in `api_key`, `browser`,
    /// `device` order.
    pub fn methods(&self) -> impl Iterator<Item = Method> {
        [
            (self.key.is_some(), Method::ApiKey),
            (
                self.oauth.is_some_and(|oauth| oauth.browser),
                Method::Browser,
            ),
            (
                self.oauth.is_some_and(|oauth| oauth.device.is_some())
                    || self.id == GITHUB_COPILOT.id,
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
    family_policy: FamilyPolicy::Native,
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
    family_policy: FamilyPolicy::Native,
    base_url: "https://api.anthropic.com",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::XApiKey,
        env: &["ANTHROPIC_API_KEY"],
    }),
    oauth: Some(OAuthSpec {
        client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        client_id_env: None,
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
        browser: true,
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
    family_policy: FamilyPolicy::Native,
    base_url: "https://chatgpt.com/backend-api/codex",
    transport: Transport::Websocket,
    key: None,
    oauth: Some(OAuthSpec {
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        client_id_env: None,
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
        browser: true,
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

pub(crate) const FACTORY_DROID: ProviderDef = ProviderDef {
    id: "factory-droid",
    name: "Factory Droid",
    family: Family::Droid,
    family_policy: FamilyPolicy::Native,
    base_url: "https://api.factory.ai",
    transport: Transport::Https,
    // Factory API keys are control-plane only and answer 403 on the
    // inference proxy; the row takes no API key.
    key: None,
    oauth: Some(OAuthSpec {
        client_id: "client_01HNM792M5G5G1A2THWPXKFMXB",
        client_id_env: None,
        authorize_url: "https://api.workos.com/user_management/authorize/device",
        token_url: "https://api.workos.com/user_management/authenticate",
        scopes: &[],
        query: &[],
        redirect: Redirect {
            host: "localhost",
            port: 0,
            path: "",
        },
        completion: Completion::Callback,
        // Device-only: the vendor registered no browser redirect, so no
        // `Method::Browser` is offered. `Method::Device` drives
        // `authorize_device` directly without binding a callback listener.
        browser: false,
        device: Some(Device {
            usercode_url: "https://api.workos.com/user_management/authorize/device",
            poll_url: "https://api.workos.com/user_management/authenticate",
            page_url: "https://workos.com/device",
            redirect_uri: "",
        }),
        revoke_url: None,
        hook: Hook::WorkOsDevice,
    }),
    shape: Shape::Plain,
};

pub(crate) const GITLAB_DUO: ProviderDef = ProviderDef {
    id: "gitlab-duo",
    name: "GitLab Duo",
    family: Family::GitLab,
    family_policy: FamilyPolicy::Native,
    base_url: "https://gitlab.com",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["GITLAB_TOKEN"],
    }),
    oauth: Some(OAuthSpec {
        client_id: "da4edff2e6ebd2bc3208611e2768bc1c1dd7be791dc5ff26ca34ca9ee44f7d4b",
        client_id_env: Some("GITLAB_CLIENT_ID"),
        authorize_url: "https://gitlab.com/oauth/authorize",
        token_url: "https://gitlab.com/oauth/token",
        scopes: &["api"],
        query: &[
            ("client_id", QueryValue::ClientId),
            ("redirect_uri", QueryValue::RedirectUri),
            ("response_type", QueryValue::Text("code")),
            ("state", QueryValue::State),
            ("scope", QueryValue::Scope),
            ("code_challenge", QueryValue::Challenge),
            ("code_challenge_method", QueryValue::Text("S256")),
        ],
        redirect: Redirect {
            host: "localhost",
            port: 8080,
            path: "/callback",
        },
        // Pasted code or redirect URL, never the device fallback: the row
        // has no device sign-in, so a busy port must not route into one.
        completion: Completion::PasteCode,
        browser: true,
        device: None,
        revoke_url: Some("https://gitlab.com/oauth/revoke"),
        hook: Hook::GitLabDuo,
    }),
    shape: Shape::Plain,
};

pub(crate) const GITHUB_COPILOT: ProviderDef = ProviderDef {
    id: "github-copilot",
    name: "GitHub Copilot",
    family: Family::Chat,
    family_policy: FamilyPolicy::Native,
    base_url: "https://api.githubcopilot.com",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["COPILOT_GITHUB_TOKEN"],
    }),
    oauth: None,
    shape: Shape::Plain,
};

/// The Radius gateway, which speaks the pi-messages wire.
///
/// Key sign-in only. The reference also signs in through the gateway's own
/// OAuth (`/v1/oauth` discovery of a per-gateway authorize endpoint, a
/// loopback callback on port 1456, and an RFC 8628 device flow), and keeps
/// the gateway config on the credential. The table's `OAuthSpec` fixes the
/// authorize URL and has no hook for either step, so the OAuth flow is not
/// offered.
///
/// The base URL is the one every Radius model carries in the reference
/// (`https://radius.pi.dev/v1`): the request goes to `<base>/messages`, while
/// the reference's `DEFAULT_RADIUS_GATEWAY` (`https://radius.pi.dev`) is the
/// origin of the `/v1/config` model list and the OAuth endpoints.
pub(crate) const RADIUS: ProviderDef = ProviderDef {
    id: "radius",
    name: "Radius",
    family: Family::PiMessages,
    family_policy: FamilyPolicy::Native,
    base_url: "https://radius.pi.dev/v1",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["RADIUS_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
};

/// The `OpenRouter` gateway, which serves one OpenAI-style endpoint for every
/// upstream model.
///
/// Key sign-in only. The reference also signs in through a browser PKCE flow
/// that exchanges the authorization code for a durable key at
/// `/api/v1/auth/keys` and stores the key as a plain API key. The table's
/// `OAuthSpec` can express neither the stateless callback (`OpenRouter` echoes
/// no `state`) nor a browser grant that yields an API key instead of the
/// token pair the generic flow refreshes and revokes, so the flow is not
/// ported.
///
/// The row's family is the reference's default: the Responses wire. The
/// reference downgrades a stream to the Chat Completions wire when its
/// captured `PI_OPENROUTER_RESPONSES` names `"0"`; the request adapter
/// applies the same override under `DAL_OPENROUTER_RESPONSES`, so the row
/// keeps one endpoint, one credential, and both wires.
pub(crate) const OPENROUTER: ProviderDef = ProviderDef {
    id: "openrouter",
    name: "OpenRouter",
    family: Family::Responses,
    family_policy: FamilyPolicy::Native,
    base_url: "https://openrouter.ai/api/v1",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["OPENROUTER_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
};

/// Google Gemini over the generative language API.
///
/// Key sign-in only (`GEMINI_API_KEY`): the request adapter hardcodes the
/// Google `x-goog-api-key` header, so the row's key style is a stored-catalog
/// label with no wire effect. OAuth sign-in is deliberately not offered: the
/// reference signs in through Google's installed-app OAuth flow, which needs
/// a third-party client secret dal does not own.
pub(crate) const GOOGLE: ProviderDef = ProviderDef {
    id: "google",
    name: "Google Gemini",
    family: Family::GoogleGenerativeAi,
    family_policy: FamilyPolicy::Native,
    base_url: "https://generativelanguage.googleapis.com/v1beta",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::XApiKey,
        env: &["GEMINI_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
};

/// Google Vertex AI.
///
/// Key sign-in only (`GOOGLE_CLOUD_API_KEY`, Express mode): the Vertex
/// adapter hardcodes `x-goog-api-key` for keys and builds its own
/// `Authorization: Bearer` header from the credential, so the row's key
/// style is a stored-catalog label with no wire effect. A bearer token can
/// also come from the captured environment
/// (`GOOGLE_CLOUD_ACCESS_TOKEN`/`CLOUDSDK_AUTH_ACCESS_TOKEN`); minting and
/// refreshing one is not offered: the reference mints through Application
/// Default Credentials, which needs a Google OAuth client dal does not own.
pub(crate) const GOOGLE_VERTEX: ProviderDef = ProviderDef {
    id: "google-vertex",
    name: "Google Vertex AI",
    family: Family::GoogleVertex,
    family_policy: FamilyPolicy::Native,
    base_url: "https://aiplatform.googleapis.com",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::XApiKey,
        env: &["GOOGLE_CLOUD_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
};

const VENDOR_ROWS: &[ProviderDef] = &[
    OPENAI,
    OPENAI_CODEX,
    ANTHROPIC,
    FACTORY_DROID,
    GITLAB_DUO,
    GITHUB_COPILOT,
    RADIUS,
    OPENROUTER,
    GOOGLE,
    GOOGLE_VERTEX,
];
const ROWS: usize = VENDOR_ROWS.len() + presets::PRESETS.len();

/// The vendor rows, then the presets, joined at compile time.
static TABLE: [ProviderDef; ROWS] = {
    let mut rows = [OPENAI; ROWS];
    let mut index = 0;
    while index < ROWS {
        rows[index] = if index < VENDOR_ROWS.len() {
            VENDOR_ROWS[index]
        } else {
            presets::PRESETS[index - VENDOR_ROWS.len()]
        };
        index += 1;
    }
    rows
};

/// Every built-in provider, in the order the configuration lists them: the
/// vendor rows, then the API-key presets by id.
pub static PROVIDERS: &[ProviderDef] = &TABLE;

/// The built-in provider with `id`, if any.
#[must_use]
pub fn find(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|def| def.id == id)
}

#[cfg(test)]
mod tests;
