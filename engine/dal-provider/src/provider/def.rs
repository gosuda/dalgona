//! The built-in provider table: the only place that knows a built-in provider.
//!
//! Every other site asks [`PROVIDERS`] or carries a [`ProviderDef`] resolved
//! once from it. The rows are plain `const` data; a sign-in method is derived
//! from a row by [`ProviderDef::methods`] and never stored. The vendor rows
//! live here; the API-key presets live in `presets` and join the same table.

use dal_core::Family;

use super::wire::{
    AZURE_ENDPOINT_TEMPLATE, BasePart, EnvShape, ModelIdAlias, WireBase, WireDef, WireSelect,
};
use super::{AuthStyle, Shape, Transport};
use crate::{
    auth::{login::Method, oauth::CODEX_ORIGINATOR},
    family::google::antigravity::DAILY_ENDPOINT,
};

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
    /// Endpoint template, static row data only. Renders only while the entry
    /// keeps the row's default base; an explicit table `base_url` is a plain
    /// literal and always wins.
    pub base_template: &'static [BasePart],
    /// Wires this row serves per model id; empty for the single-wire contract
    /// every existing row keeps. The row's own `family` stays the logical
    /// default: it fixes `Shape` for the model-list fetch, the Azure/Copilot
    /// predicates, and the entry's family for rows without wires.
    pub wires: &'static [WireDef],
    /// Header name that carries the dal session id on every request of this
    /// row (`opencode`: `Some("x-opencode-session")`); `None` elsewhere.
    /// Fed only from the explicit `SessionId` argument of `Provider::open`.
    pub session_header: Option<&'static str>,
    /// Catalog id aliasing for this row; only `bai` sets `DotHyphen`.
    pub model_id_alias: ModelIdAlias,
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
    /// The public OAuth client id; `None` when the vendor registers none and
    /// its requests carry no client id (`OpenRouter`: the PKCE verifier is the
    /// only proof of identity).
    pub client_id: Option<&'static str>,
    /// The environment variable whose non-blank value replaces `client_id`
    /// for this row's authorize, exchange, refresh, and revoke requests;
    /// `None` when the client id is fixed. The caller resolves it from a
    /// captured `EnvSnapshot`; the library never reads the process
    /// environment.
    pub client_id_env: Option<&'static str>,
    /// The installed-application client secret the vendor registered with
    /// this public client; `None` for every PKCE-only row. An installed
    /// application cannot keep a secret, so the value is non-confidential
    /// vendor registration data that ships in every copy of the vendor's own
    /// client, not a credential of the user: the code exchange and the
    /// refresh send it as a form field beside the client id.
    pub client_secret: Option<&'static str>,
    /// The authorization endpoint. A row whose hook discovers the endpoint at
    /// run time ([`Hook::RadiusGateway`]) names its discovery document here
    /// instead, and the browser opens the endpoint that document names. A
    /// [`Completion::Poll`] row names the page the browser opens; its hook
    /// builds that page's query.
    pub authorize_url: &'static str,
    /// The token endpoint, for code exchange and refresh. For a row whose
    /// hook does not refresh it is the endpoint that exchanges the code for
    /// the stored credential: a durable API key ([`Hook::OpenRouterKey`]) or
    /// a non-renewable token ([`Hook::DevinCliToken`]). A
    /// [`Completion::Poll`] row only refreshes here. For [`Hook::MetaMuse`]
    /// it is the key mint, where the sign-in and every renewal exchange the
    /// identity token for an API key.
    pub token_url: &'static str,
    /// The requested scopes.
    pub scopes: &'static [&'static str],
    /// The authorization query, in the exact order the vendor expects. A
    /// query that sends no [`QueryValue::State`] belongs to a vendor that
    /// echoes none: the flow then binds the loopback callback to an
    /// unguessable per-attempt path instead.
    pub query: &'static [(&'static str, QueryValue)],
    /// The vendor-registered loopback address the browser redirects to;
    /// `None` for a row whose sign-in has no redirect at all: a device-only
    /// row ([`Hook::WorkOsDevice`]) or a [`Completion::Poll`] row, whose
    /// tokens arrive from a poll.
    pub redirect: Option<Redirect>,
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
    /// Only the loopback redirect on the vendor-registered port, or a pasted
    /// redirect URL, completes it. A busy port fails the sign-in with
    /// [`ProviderError::CallbackBind`](crate::ProviderError::CallbackBind):
    /// the vendor has no fallback, so none is invented.
    Loopback,
    /// The loopback redirect or a pasted code or redirect URL completes it;
    /// a busy port falls back to any free port.
    PasteCode,
    /// No redirect: the browser approves the sign-in server-side and the
    /// client polls the vendor until it releases the tokens
    /// ([`Hook::CursorPoll`]). Nothing listens and nothing is pasted.
    Poll,
}

/// A device-code sign-in: the user enters a code at a verification page.
///
/// Only [`Hook::CodexAccount`] names both optional URLs: its device poll ends
/// in an authorization code that the ordinary token exchange redeems with
/// `redirect_uri`. The RFC 8628 hooks deliver tokens from the poll itself, so
/// they carry no redirect, and a vendor whose authorization response names
/// the verification page carries no page of its own.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    /// The endpoint that issues the user code.
    pub usercode_url: &'static str,
    /// The endpoint polled until the user approves.
    pub poll_url: &'static str,
    /// The page where the user enters the code; `None` when the vendor's
    /// authorization response always names it ([`Hook::RadiusGateway`]).
    /// For [`Hook::WorkOsDevice`] it is the page shown when that response
    /// names none.
    pub page_url: Option<&'static str>,
    /// The redirect URI sent with the final code exchange; `None` when the
    /// device poll delivers the tokens directly.
    pub redirect_uri: Option<&'static str>,
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
    /// Radius gateway sign-in: the authorization endpoint is discovered from
    /// the gateway's OAuth discovery document (`OAuthSpec::authorize_url`),
    /// the loopback callback is fixed to the registered redirect, and the
    /// code exchange and refresh are form-encoded. The RFC 8628 device
    /// sign-in sends the row's scopes, shows the verification page the
    /// gateway names, and receives the tokens from the poll with no second
    /// exchange.
    RadiusGateway,
    /// `OpenRouter` sign-in: a PKCE browser grant with no client id and no
    /// `state` (the callback path carries an unguessable per-attempt value
    /// instead). The JSON code exchange returns a durable API key, which is
    /// stored as a plain API key: it never expires, is never refreshed, and
    /// needs no revocation.
    OpenRouterKey,
    /// Cursor sign-in: the browser opens the deep-control login page with a
    /// PKCE challenge and a per-attempt `uuid`, approves server-side, and the
    /// client polls the vendor's token release endpoint with the `uuid` and
    /// the verifier until the tokens arrive. There is no redirect, no
    /// `state`, and no code exchange. The refresh is a JSON grant whose
    /// `200` answer may end the session (`shouldLogout`) or rotate the
    /// refresh token. After sign-in the model list is fetched with the new
    /// access token.
    CursorPoll,
    /// Devin CLI sign-in: a PKCE browser grant with a per-attempt `state`
    /// and no client id, completed by the loopback callback or a pasted
    /// code. The JSON exchange carries only the code and the verifier and
    /// answers one token, stored as both halves of an OAuth credential that
    /// is never refreshed: a fresh sign-in re-issues it.
    DevinCliToken,
    /// Native Google Cloud Code Assist sign-in (the Gemini CLI's public
    /// client): Google's installed-application authorization-code grant, with no PKCE, a
    /// random `state` the callback echoes, and the registered client secret
    /// in the form-encoded code exchange and refresh. After the exchange the
    /// Google Cloud project the account uses is discovered through
    /// `loadCodeAssist` (and, for an account with no tier, onboarded) and
    /// stored on the credential; a refresh keeps the stored project.
    GoogleCloudCodeAssist,
    /// Antigravity sign-in: the same installed-application grant as
    /// [`Hook::GoogleCloudCodeAssist`] under Antigravity's own client, scopes, and
    /// callback, with the Antigravity free-tier eligibility check and
    /// onboarding as the project discovery.
    GoogleAntigravity,
    /// xAI sign-in (`SuperGrok` or X Premium): a device-only RFC 8628 grant at
    /// the xAI issuer. The authorization request names the `referrer`, the
    /// verification page shown is the complete URI when the issuer sends one,
    /// the first poll waits one interval, and a `slow_down` answer moves the
    /// interval to the one the issuer names. The poll delivers the tokens
    /// directly (an access token that is a JWT and a refresh token) with no
    /// second exchange; a token answer without `expires_in` lives one hour.
    /// The refresh is the standard form grant at the same token endpoint.
    XaiDevice,
    /// Meta Muse sign-in: a device-only RFC 8628 grant at the Meta issuer
    /// that answers only an identity token, which the Model API does not
    /// accept. The sign-in mints a Model API key from the identity token at
    /// the row's token endpoint and stores the key as the access token and
    /// the identity token as the renewal secret; the refresh is a new mint
    /// with the identity token, not a refresh grant. A key lives 24 hours.
    MetaMuse,
    /// Kimi Code sign-in: a device-only RFC 8628 grant at the Kimi auth host
    /// whose requests (sign-in, refresh, and every request authenticated with
    /// the stored credential) carry the Kimi client identity. The refresh is
    /// the standard form grant. A Kimi Code token is never sent as a Claude
    /// OAuth token: it takes no Claude beta, system identity, or `x-app`.
    KimiCodeDevice,
}

impl Hook {
    /// Whether a sign-in through the hook discovers a Google Cloud project
    /// and stores it on the credential
    /// ([`OAuthCredential::project_id`](crate::OAuthCredential::project_id)):
    /// the two Cloud Code Assist hooks. Every other credential carries none.
    #[must_use]
    pub const fn binds_project(self) -> bool {
        matches!(self, Self::GoogleCloudCodeAssist | Self::GoogleAntigravity)
    }

    /// Whether a sign-in through the hook stores an OAuth credential. Only
    /// [`Hook::OpenRouterKey`] does not: it stores a durable API key.
    #[must_use]
    pub const fn stores_oauth(self) -> bool {
        !matches!(self, Self::OpenRouterKey)
    }

    /// Whether the refresher renews the hook's stored credential. Neither
    /// [`Hook::OpenRouterKey`] (a durable API key) nor
    /// [`Hook::DevinCliToken`] (an OAuth token with no refresh grant) does.
    #[must_use]
    pub const fn refreshes(self) -> bool {
        !matches!(self, Self::OpenRouterKey | Self::DevinCliToken)
    }

    /// Whether the hook's requests carry the vendor's client-identity
    /// headers: only [`Hook::KimiCodeDevice`]. The headers are those of the
    /// Kimi Code client and are derived from the captured host identity; an
    /// API key of the same row carries none.
    #[must_use]
    pub const fn sends_client_identity(self) -> bool {
        matches!(self, Self::KimiCodeDevice)
    }
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
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
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
        client_id: Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
        client_id_env: None,
        client_secret: None,
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
        redirect: Some(Redirect {
            host: "localhost",
            port: 53692,
            path: "/callback",
        }),
        completion: Completion::PasteCode,
        browser: true,
        device: None,
        revoke_url: None,
        hook: Hook::ClaudeCode,
    }),
    shape: Shape::Claude,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
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
        client_id: Some("app_EMoamEEZ73f0CkXaXp7hrann"),
        client_id_env: None,
        client_secret: None,
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
        redirect: Some(Redirect {
            host: "localhost",
            port: 1455,
            path: "/auth/callback",
        }),
        completion: Completion::Callback,
        browser: true,
        device: Some(Device {
            usercode_url: "https://auth.openai.com/api/accounts/deviceauth/usercode",
            poll_url: "https://auth.openai.com/api/accounts/deviceauth/token",
            page_url: Some("https://auth.openai.com/codex/device"),
            redirect_uri: Some("https://auth.openai.com/deviceauth/callback"),
        }),
        revoke_url: Some("https://auth.openai.com/oauth/revoke"),
        hook: Hook::CodexAccount,
    }),
    shape: Shape::Codex,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
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
        client_id: Some("client_01HNM792M5G5G1A2THWPXKFMXB"),
        client_id_env: None,
        client_secret: None,
        authorize_url: "https://api.workos.com/user_management/authorize/device",
        token_url: "https://api.workos.com/user_management/authenticate",
        scopes: &[],
        query: &[],
        redirect: None,
        completion: Completion::Callback,
        // Device-only: the vendor registered no browser redirect, so no
        // `Method::Browser` is offered. `Method::Device` drives
        // `authorize_device` directly without binding a callback listener.
        browser: false,
        device: Some(Device {
            usercode_url: "https://api.workos.com/user_management/authorize/device",
            poll_url: "https://api.workos.com/user_management/authenticate",
            page_url: Some("https://workos.com/device"),
            redirect_uri: None,
        }),
        revoke_url: None,
        hook: Hook::WorkOsDevice,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
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
        client_id: Some("da4edff2e6ebd2bc3208611e2768bc1c1dd7be791dc5ff26ca34ca9ee44f7d4b"),
        client_id_env: Some("GITLAB_CLIENT_ID"),
        client_secret: None,
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
        redirect: Some(Redirect {
            host: "localhost",
            port: 8080,
            path: "/callback",
        }),
        // Pasted code or redirect URL, never the device fallback: the row
        // has no device sign-in, so a busy port must not route into one.
        completion: Completion::PasteCode,
        browser: true,
        device: None,
        revoke_url: Some("https://gitlab.com/oauth/revoke"),
        hook: Hook::GitLabDuo,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
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
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// The Radius gateway, which speaks the pi-messages wire.
///
/// Signs in by API key, by the gateway's browser OAuth, or by its RFC 8628
/// device flow. The browser flow discovers the authorization endpoint from
/// the gateway's `/v1/oauth` document, redirects to the registered loopback
/// callback on `127.0.0.1:1456` (a busy port fails the sign-in; the
/// reference has no device fallback), and exchanges the code at
/// `/v1/oauth/token` with the public client `pi-gateway`. The device flow
/// posts to `/v1/oauth/device` with the same client and scopes and polls the
/// token endpoint. Both store an expiring OAuth credential that the same
/// token endpoint refreshes. The gateway config the reference keeps on the
/// credential is a catalog concern, not part of the sign-in.
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
    oauth: Some(OAuthSpec {
        client_id: Some("pi-gateway"),
        client_id_env: None,
        client_secret: None,
        authorize_url: "https://radius.pi.dev/v1/oauth",
        token_url: "https://radius.pi.dev/v1/oauth/token",
        scopes: &["gateway", "offline_access"],
        query: &[
            ("response_type", QueryValue::Text("code")),
            ("client_id", QueryValue::ClientId),
            ("redirect_uri", QueryValue::RedirectUri),
            ("scope", QueryValue::Scope),
            ("code_challenge", QueryValue::Challenge),
            ("code_challenge_method", QueryValue::Text("S256")),
            ("handoff", QueryValue::Text("url")),
            ("state", QueryValue::State),
        ],
        redirect: Some(Redirect {
            host: "127.0.0.1",
            port: 1456,
            path: "/oauth/callback",
        }),
        completion: Completion::Loopback,
        browser: true,
        device: Some(Device {
            usercode_url: "https://radius.pi.dev/v1/oauth/device",
            poll_url: "https://radius.pi.dev/v1/oauth/token",
            page_url: None,
            redirect_uri: None,
        }),
        revoke_url: None,
        hook: Hook::RadiusGateway,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// The `OpenRouter` gateway, which serves one OpenAI-style endpoint for every
/// upstream model.
///
/// Signs in by API key or by a browser PKCE flow that exchanges the
/// authorization code for a durable key at `/api/v1/auth/keys` and stores the
/// key as a plain API key, never as an expiring OAuth credential. `OpenRouter`
/// registers no client and echoes no `state`, so the authorize query sends
/// neither: the loopback callback listens on an ephemeral port under an
/// unguessable per-attempt path, and the PKCE verifier (sent only in the key
/// exchange) is the proof that the code belongs to this sign-in.
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
    oauth: Some(OAuthSpec {
        client_id: None,
        client_id_env: None,
        client_secret: None,
        authorize_url: "https://openrouter.ai/auth",
        token_url: "https://openrouter.ai/api/v1/auth/keys",
        scopes: &[],
        query: &[
            ("callback_url", QueryValue::RedirectUri),
            ("code_challenge", QueryValue::Challenge),
            ("code_challenge_method", QueryValue::Text("S256")),
        ],
        redirect: Some(Redirect {
            host: "127.0.0.1",
            port: 0,
            path: "/oauth/callback",
        }),
        completion: Completion::PasteCode,
        browser: true,
        device: None,
        revoke_url: None,
        hook: Hook::OpenRouterKey,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
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
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
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
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

pub(crate) const AMAZON_BEDROCK: ProviderDef = ProviderDef {
    id: "amazon-bedrock",
    name: "Amazon Bedrock",
    family: Family::Bedrock,
    family_policy: FamilyPolicy::Native,
    base_url: "https://bedrock-runtime.us-east-1.amazonaws.com",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["AWS_BEARER_TOKEN_BEDROCK"],
    }),
    oauth: None,
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// Cursor, over the Connect-framed agent `Run` wire.
///
/// Signs in by API key (`CURSOR_ACCESS_TOKEN`, or its `CURSOR_API_KEY`
/// alias) or by the Cursor CLI's browser poll grant: the browser opens the
/// deep-control login page, approves server-side, and the client polls
/// `api2.cursor.sh/auth/poll` until the access and refresh tokens arrive.
/// The grant has no redirect and no `state`; the per-attempt `uuid` and the
/// PKCE verifier prove it. The public client id appears only in the JSON
/// refresh grant at `/oauth/token`, whose `200` answer may end the session
/// (`shouldLogout`) or rotate the refresh token. Both credentials are sent
/// as `Authorization: Bearer` by the family's own wire headers.
pub(crate) const CURSOR: ProviderDef = ProviderDef {
    id: "cursor",
    name: "Cursor",
    family: Family::Cursor,
    family_policy: FamilyPolicy::Native,
    base_url: "https://api2.cursor.sh",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["CURSOR_ACCESS_TOKEN", "CURSOR_API_KEY"],
    }),
    oauth: Some(OAuthSpec {
        client_id: Some("KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB"),
        client_id_env: None,
        client_secret: None,
        authorize_url: "https://cursor.com/loginDeepControl",
        token_url: "https://api2.cursor.sh/oauth/token",
        scopes: &[],
        query: &[],
        redirect: None,
        completion: Completion::Poll,
        browser: true,
        device: None,
        revoke_url: None,
        hook: Hook::CursorPoll,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// Devin, over the Codeium Cascade Connect wire.
///
/// Signs in by API key (`DEVIN_API_KEY`) or by the Devin CLI browser grant:
/// PKCE S256 with a per-attempt `state`, `prompt=select_account`, and no
/// client id, landing on the registered `127.0.0.1:59653/callback` or
/// completed by a pasted code or redirect URL. The vendor rejects any other
/// port, so a busy 59653 keeps the registered URI and completes by paste. The code and verifier alone are exchanged at
/// `api.devin.ai/auth/cli/token` for one long-lived token, stored as both
/// halves of an OAuth credential and never refreshed. The login host is
/// never a model endpoint: requests go to the Cascade base.
pub(crate) const DEVIN: ProviderDef = ProviderDef {
    id: "devin",
    name: "Devin",
    family: Family::Devin,
    family_policy: FamilyPolicy::Native,
    base_url: "https://server.codeium.com",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["DEVIN_API_KEY"],
    }),
    oauth: Some(OAuthSpec {
        client_id: None,
        client_id_env: None,
        client_secret: None,
        authorize_url: "https://app.devin.ai/auth/cli/continue",
        token_url: "https://api.devin.ai/auth/cli/token",
        scopes: &[],
        query: &[
            ("response_type", QueryValue::Text("code")),
            ("redirect_uri", QueryValue::RedirectUri),
            ("code_challenge", QueryValue::Challenge),
            ("code_challenge_method", QueryValue::Text("S256")),
            ("state", QueryValue::State),
            ("prompt", QueryValue::Text("select_account")),
        ],
        redirect: Some(Redirect {
            host: "127.0.0.1",
            port: 59653,
            path: "/callback",
        }),
        completion: Completion::PasteCode,
        browser: true,
        device: None,
        revoke_url: None,
        hook: Hook::DevinCliToken,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// The authorization endpoint of Google's OAuth 2.0 installed-application
/// flow, shared by both Cloud Code Assist sign-ins.
const GOOGLE_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// The token endpoint of Google's OAuth 2.0 server, for the code exchange and
/// the refresh of both Cloud Code Assist sign-ins.
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// The authorization query of both Cloud Code Assist sign-ins, in the order
/// of the reference's declarative flow: the standard parameters (client id,
/// `code`, redirect, scope, `state`), then `access_type=offline` and
/// `prompt=consent` so Google issues a refresh token. There is no PKCE
/// challenge: the vendor registered none for these installed clients.
const CLOUD_CODE_QUERY: &[(&str, QueryValue)] = &[
    ("client_id", QueryValue::ClientId),
    ("response_type", QueryValue::Text("code")),
    ("redirect_uri", QueryValue::RedirectUri),
    ("scope", QueryValue::Scope),
    ("state", QueryValue::State),
    ("access_type", QueryValue::Text("offline")),
    ("prompt", QueryValue::Text("consent")),
];

/// Native Google Cloud Code Assist, signed in with the Gemini CLI's public
/// client.
///
/// Browser sign-in only: Google's installed-application grant with that
/// client (id and non-confidential registered secret), the
/// registered loopback callback `127.0.0.1:8085/oauth2callback`, no PKCE, and a
/// random `state` the redirect echoes. A busy callback port moves the listener
/// to any free loopback port, which Google accepts for this client, and a
/// pasted redirect URL or code completes the sign-in as well. After the token
/// exchange the account's Google Cloud project is discovered through Cloud
/// Code Assist `loadCodeAssist` (and `onboardUser` for an account with no
/// tier) and stored on the credential; a refresh keeps it.
///
/// The row takes no API key: every request carries the OAuth access token and
/// the stored project. Its provider id and family (`google-cloud-code-assist`)
/// are distinct from the credential-less local `gemini` subprocess
/// (`google-gemini-cli`), which keeps its own family, id, policy file, and
/// configuration unchanged; the upstream name `google-gemini-cli` maps to this
/// row only in the reference census, never as a runtime alias.
pub(crate) const GOOGLE_CLOUD_CODE_ASSIST: ProviderDef = ProviderDef {
    id: "google-cloud-code-assist",
    name: "Google Cloud Code Assist (Gemini CLI)",
    family: Family::GoogleCloudCodeAssist,
    family_policy: FamilyPolicy::Native,
    base_url: "https://cloudcode-pa.googleapis.com",
    transport: Transport::Https,
    key: None,
    oauth: Some(OAuthSpec {
        client_id: Some("681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com"),
        client_id_env: None,
        client_secret: Some("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"),
        authorize_url: GOOGLE_AUTHORIZE_URL,
        token_url: GOOGLE_TOKEN_URL,
        scopes: &[
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
        ],
        query: CLOUD_CODE_QUERY,
        redirect: Some(Redirect {
            host: "127.0.0.1",
            port: 8085,
            path: "/oauth2callback",
        }),
        completion: Completion::PasteCode,
        browser: true,
        device: None,
        revoke_url: None,
        hook: Hook::GoogleCloudCodeAssist,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// Antigravity, over the Cloud Code Assist endpoints.
///
/// Browser sign-in only, with Antigravity's own installed-application client
/// (its own public id and non-confidential registered secret), its wider scope
/// set, and the registered callback `127.0.0.1:51121/oauth-callback`; the grant
/// is otherwise the Cloud Code Assist grant above. The project discovery checks free-tier
/// eligibility, onboards an account with no current tier, and requires the
/// second `loadCodeAssist` to name the project, which is stored on the
/// credential and kept across refreshes. The base URL is the daily endpoint
/// that discovery and the first request attempt use; the request path adds
/// its own sandbox failover.
pub(crate) const GOOGLE_ANTIGRAVITY: ProviderDef = ProviderDef {
    id: "google-antigravity",
    name: "Antigravity (Gemini 3, Claude, GPT-OSS)",
    family: Family::GoogleAntigravity,
    family_policy: FamilyPolicy::Native,
    base_url: DAILY_ENDPOINT,
    transport: Transport::Https,
    key: None,
    oauth: Some(OAuthSpec {
        client_id: Some(
            "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com",
        ),
        client_id_env: None,
        client_secret: Some("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"),
        authorize_url: GOOGLE_AUTHORIZE_URL,
        token_url: GOOGLE_TOKEN_URL,
        scopes: &[
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
            "https://www.googleapis.com/auth/cclog",
            "https://www.googleapis.com/auth/experimentsandconfigs",
        ],
        query: CLOUD_CODE_QUERY,
        redirect: Some(Redirect {
            host: "127.0.0.1",
            port: 51121,
            path: "/oauth-callback",
        }),
        completion: Completion::PasteCode,
        browser: true,
        device: None,
        revoke_url: None,
        hook: Hook::GoogleAntigravity,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// xAI, over the Responses wire.
///
/// Signs in by API key (`XAI_API_KEY`) or by the `SuperGrok` and X Premium
/// device grant: the public Grok CLI client at `auth.x.ai`, a device-only
/// RFC 8628 flow with no PKCE, redirect, or client secret. The access token
/// is a JWT that authenticates the Responses endpoint exactly as a key does,
/// as a bearer. The row keeps the compatibility family policy of the key
/// preset it replaces, so an explicit `providers.xai.api` still names any
/// supported key family.
pub(crate) const XAI: ProviderDef = ProviderDef {
    id: "xai",
    name: "xAI API",
    family: Family::Responses,
    family_policy: FamilyPolicy::Compatible,
    base_url: "https://api.x.ai/v1",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["XAI_API_KEY"],
    }),
    oauth: Some(OAuthSpec {
        client_id: Some("b1a00492-073a-47ea-816f-4c329264a828"),
        client_id_env: None,
        client_secret: None,
        // Unused by a device-only row; the table needs a URL here.
        authorize_url: "https://auth.x.ai/oauth2/device/code",
        token_url: "https://auth.x.ai/oauth2/token",
        scopes: &[
            "openid",
            "profile",
            "email",
            "offline_access",
            "grok-cli:access",
            "api:access",
        ],
        query: &[],
        redirect: None,
        completion: Completion::Callback,
        browser: false,
        device: Some(Device {
            usercode_url: "https://auth.x.ai/oauth2/device/code",
            poll_url: "https://auth.x.ai/oauth2/token",
            page_url: None,
            redirect_uri: None,
        }),
        revoke_url: None,
        hook: Hook::XaiDevice,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// The Meta Model API, over the Responses wire.
///
/// Signs in by API key (`MODEL_API_KEY`, then `META_API_KEY`) or by the Muse
/// Code device grant: the public Muse Code CLI client at `auth.meta.com`, a
/// device-only RFC 8628 flow with no scopes, PKCE, redirect, or client
/// secret. The grant answers an identity token only; the row's token
/// endpoint is the Muse Code key mint, where the sign-in exchanges it for a
/// 24-hour Model API key and where every renewal mints a new one. The key
/// authenticates the Responses endpoint as a bearer. The row keeps the
/// compatibility family policy of the key preset it replaces.
pub(crate) const META: ProviderDef = ProviderDef {
    id: "meta",
    name: "Meta Model API",
    family: Family::Responses,
    family_policy: FamilyPolicy::Compatible,
    base_url: "https://api.meta.ai/v1",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["MODEL_API_KEY", "META_API_KEY"],
    }),
    oauth: Some(OAuthSpec {
        client_id: Some("1031625952748946"),
        client_id_env: None,
        client_secret: None,
        // Unused by a device-only row; the table needs a URL here.
        authorize_url: "https://auth.meta.com/oidc/device/authorization/",
        token_url: "https://api.meta.ai/muse-code/key",
        scopes: &[],
        query: &[],
        redirect: None,
        completion: Completion::Callback,
        browser: false,
        device: Some(Device {
            usercode_url: "https://auth.meta.com/oidc/device/authorization/",
            poll_url: "https://auth.meta.com/oidc/device/token/",
            page_url: None,
            redirect_uri: None,
        }),
        revoke_url: None,
        hook: Hook::MetaMuse,
    }),
    shape: Shape::Plain,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// Kimi Code, over the Anthropic Messages wire.
///
/// Signs in by API key (`KIMI_CODING_API_KEY`, sent as `x-api-key`) or by
/// the Kimi Code device grant: the public Kimi Code CLI client at
/// `auth.kimi.com`, a device-only RFC 8628 flow with no scopes, PKCE,
/// redirect, or client secret. The access token authenticates the coding
/// endpoint as a bearer together with the Kimi client identity; it never
/// takes Claude's OAuth fingerprint. The key path carries no identity
/// headers: a platform key is not a subscription session. The global region
/// (`auth.kimi.ai`) is not offered; the sign-in is the default region's.
///
/// The default key variable is this row's own `KIMI_CODING_API_KEY`: the
/// `moonshot` row already reads `KIMI_API_KEY` for another origin, and a
/// key never reaches a second origin by default. An account that wants to
/// reuse that key here opts in explicitly with
/// `providers.kimi-coding.key_env = ["KIMI_API_KEY"]`; the row is new, so
/// no existing consumer migrates.
pub(crate) const KIMI_CODING: ProviderDef = ProviderDef {
    id: "kimi-coding",
    name: "Kimi For Coding",
    family: Family::Anthropic,
    family_policy: FamilyPolicy::Native,
    base_url: "https://api.kimi.com/coding",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::XApiKey,
        env: &["KIMI_CODING_API_KEY"],
    }),
    oauth: Some(OAuthSpec {
        client_id: Some("17e5f671-d194-4dfb-9706-5516cb48c098"),
        client_id_env: None,
        client_secret: None,
        // Unused by a device-only row; the table needs a URL here.
        authorize_url: "https://auth.kimi.com/api/oauth/device_authorization",
        token_url: "https://auth.kimi.com/api/oauth/token",
        scopes: &[],
        query: &[],
        redirect: None,
        completion: Completion::Callback,
        browser: false,
        device: Some(Device {
            usercode_url: "https://auth.kimi.com/api/oauth/device_authorization",
            poll_url: "https://auth.kimi.com/api/oauth/token",
            page_url: None,
            redirect_uri: None,
        }),
        revoke_url: None,
        hook: Hook::KimiCodeDevice,
    }),
    shape: Shape::Claude,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// Cloudflare AI Gateway: one account's gateway serves upstream model ids
/// verbatim, each prefix on its upstream's wire. The endpoint is account
/// fact, not row data: the template renders the captured
/// `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_GATEWAY_ID`, and a missing or
/// malformed variable refuses the request typed at materialization, never
/// at startup. Every wire authenticates with the gateway's own
/// `cf-aig-authorization` header and never sends `authorization` or
/// `x-api-key`; the account id travels in the endpoint, never in a key
/// list, so it cannot leak into another origin's key resolution.
pub(crate) const CLOUDFLARE_AI_GATEWAY: ProviderDef = ProviderDef {
    id: "cloudflare-ai-gateway",
    name: "Cloudflare AI Gateway",
    family: Family::Chat,
    family_policy: FamilyPolicy::Compatible,
    base_url: "https://gateway.ai.cloudflare.com/v1",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::CfAigAuthorization,
        env: &["CLOUDFLARE_AI_GATEWAY_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
    base_template: &[
        BasePart::Lit("https://gateway.ai.cloudflare.com/v1/"),
        BasePart::Env {
            name: "CLOUDFLARE_ACCOUNT_ID",
            shape: EnvShape::HostLabel,
        },
        BasePart::Lit("/"),
        BasePart::Env {
            name: "CLOUDFLARE_GATEWAY_ID",
            shape: EnvShape::PathSegment,
        },
    ],
    wires: &[
        WireDef {
            select: WireSelect::Prefix("anthropic/"),
            family: Family::Anthropic,
            base: WireBase::Suffix("/anthropic"),
            auth: AuthStyle::CfAigAuthorization,
        },
        WireDef {
            select: WireSelect::Prefix("openai/"),
            family: Family::Responses,
            base: WireBase::Suffix("/openai"),
            auth: AuthStyle::CfAigAuthorization,
        },
        WireDef {
            select: WireSelect::Prefix("workers-ai/"),
            family: Family::Chat,
            base: WireBase::Suffix("/compat"),
            auth: AuthStyle::CfAigAuthorization,
        },
    ],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// Cloudflare Workers AI: the account's own inference endpoint, one Chat
/// wire. The account id is endpoint material (the typed template), never a
/// key variable, so the default key list stays disjoint from the gateway
/// row's origin; the shared spellings remain an explicit `key_env` opt-in.
pub(crate) const CLOUDFLARE_WORKERS_AI: ProviderDef = ProviderDef {
    id: "cloudflare-workers-ai",
    name: "Cloudflare Workers AI",
    family: Family::Chat,
    family_policy: FamilyPolicy::Native,
    base_url: "https://api.cloudflare.com/client/v4",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_AUTH_TOKEN"],
    }),
    oauth: None,
    shape: Shape::Plain,
    base_template: &[
        BasePart::Lit("https://api.cloudflare.com/client/v4/accounts/"),
        BasePart::Env {
            name: "CLOUDFLARE_ACCOUNT_ID",
            shape: EnvShape::HostLabel,
        },
        BasePart::Lit("/ai/v1"),
    ],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// `OpenCode` Zen: one operator endpoint that speaks the Anthropic wire for
/// `claude-*` (and the three `qwen` exceptions the operator pins to it) at
/// the root, and the Chat, Responses, and Google wires under `/v1`. Every
/// request carries the operator's session header, fed from the explicit
/// `SessionId` of `Provider::open`, never an inferred value. The
/// classifier ids stay catalog-visible census records: they match no wire
/// selector and refuse typed.
pub(crate) const OPENCODE: ProviderDef = ProviderDef {
    id: "opencode",
    name: "OpenCode Zen",
    family: Family::Chat,
    family_policy: FamilyPolicy::Compatible,
    base_url: "https://opencode.ai/zen",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["OPENCODE_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
    base_template: &[],
    wires: &[
        WireDef {
            select: WireSelect::Prefix("claude"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.5-plus"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.6-plus"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.8-flash"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Prefix("deepseek"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("glm"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("kimi"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("longcat"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("mimo"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("minimax"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("nemotron"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("big-pickle"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("cat-jam-free"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("mistral-large-4"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.8-max"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("space-bunny-free"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("step-5-preview-free"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("gpt-"),
            family: Family::Responses,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("grok"),
            family: Family::Responses,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("muse"),
            family: Family::Responses,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("gemini"),
            family: Family::GoogleGenerativeAi,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::XApiKey,
        },
    ],
    session_header: Some("x-opencode-session"),
    model_id_alias: ModelIdAlias::None,
};

/// `OpenCode` Go: the Go host of the same operator catalog. The Anthropic
/// wire rides the `/go` root (`x-api-key`), Chat and Responses ride
/// `/go/v1` (bearer), and the session header travels on every request. The
/// row deliberately carries no Google wire: the operator's one Google id is
/// listed by the catalog and refuses typed with `UnboundModel`, mirroring
/// the upstream dispatch gap instead of inventing an adapter for it.
pub(crate) const OPENCODE_GO: ProviderDef = ProviderDef {
    id: "opencode-go",
    name: "OpenCode Go",
    family: Family::Chat,
    family_policy: FamilyPolicy::Compatible,
    base_url: "https://opencode.ai/zen/go",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["OPENCODE_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
    base_template: &[],
    wires: &[
        WireDef {
            select: WireSelect::Prefix("claude"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Exact("minimax-m3"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.7-plus"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.8-flash"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.8-max"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::XApiKey,
        },
        WireDef {
            select: WireSelect::Prefix("deepseek"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("glm"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("kimi"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("longcat"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("mimo"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("cat-jam-free"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("hy3"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("hy4-preview"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("space-bunny"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("step-5-preview-free"),
            family: Family::Chat,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("gpt-"),
            family: Family::Responses,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("grok"),
            family: Family::Responses,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("muse"),
            family: Family::Responses,
            base: WireBase::Suffix("/v1"),
            auth: AuthStyle::Bearer,
        },
    ],
    session_header: Some("x-opencode-session"),
    model_id_alias: ModelIdAlias::None,
};

/// B.AI: one operator endpoint per wire — the Anthropic ids at the bare
/// host, everything else under `/v1` — with dotted catalog ids aliased to
/// their hyphenated live twins (`DotHyphen`). Every selector is a pinned
/// exact id from the operator's published catalog (11 Anthropic, 27 Chat,
/// and 18 Responses, 56 bindings total), so a live id outside the
/// selectors refuses typed (`UnboundModel`), never guesses. The dotted
/// catalog spelling binds exactly and the hyphenated
/// live twin binds through the same wire's alias, keeping the queried
/// spelling verbatim on both.
pub(crate) const BAI: ProviderDef = ProviderDef {
    id: "bai",
    name: "B.AI",
    family: Family::Chat,
    family_policy: FamilyPolicy::Compatible,
    base_url: "https://api.b.ai/v1",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["BAI_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
    base_template: &[],
    wires: &[
        // The 11 Anthropic ids ride the bare host: the Override base
        // replaces the row's `/v1` default whole on the row's own base.
        WireDef {
            select: WireSelect::Exact("claude-fable-5"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-fable-5.1"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-haiku-4.5"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-opus-4.5"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-opus-4.6"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-opus-4.7"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-opus-4.8"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-opus-5"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-sonnet-4.5"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-sonnet-4.6"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("claude-sonnet-5"),
            family: Family::Anthropic,
            base: WireBase::Override("https://api.b.ai"),
            auth: AuthStyle::Bearer,
        },
        // The 27 Chat ids ride the row base under `/v1`.
        WireDef {
            select: WireSelect::Exact("gemini-3-flash"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gemini-3.1-pro"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gemini-3.5-flash"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gemini-3.5-flash-lite"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gemini-3.6-flash"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gemini-3.8-flash"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("glm-5.1"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("glm-5.2"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("glm-5.3"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("glm-5.3-flash"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("grok-4.5"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("grok-4.6"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("hy3"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("hy4-preview"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("kimi-k2.5"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("kimi-k2.6"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("kimi-k3"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("mimo-v2.5"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("mimo-v2.5-pro"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("minimax-m2.7"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("minimax-m3"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("muse-spark-1-3"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.6-27b"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.7-max"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.8-27b"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.8-flash"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("qwen3.8-max"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        // The 18 Responses ids ride the row base under `/v1`.
        WireDef {
            select: WireSelect::Exact("deepseek-v3.2"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("deepseek-v4-flash"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("deepseek-v4-flash-vision-exp"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("deepseek-v4-pro"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("deepseek-v4.1-flash"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5-mini"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5-nano"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.2"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.4"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.4-mini"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.4-nano"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.4-pro"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.5"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.5-instant"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.6-luna"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.6-sol"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-5.6-terra"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Exact("gpt-6-astra"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
    ],
    session_header: None,
    model_id_alias: ModelIdAlias::DotHyphen,
};

/// Azure `OpenAI`, Responses surface. The endpoint is account fact, so the
/// row carries no stored default base: `base_template` renders the
/// captured `AZURE_OPENAI_BASE_URL` when it names a URL, else the canonical
/// endpoint of the captured `AZURE_OPENAI_RESOURCE_NAME`
/// (`https://<resource>.openai.azure.com/openai/v1`). The static `base_url`
/// is the host those names join — honest data the admission checks, never a
/// request base while the row keeps its default. Rendering happens at the
/// bind, discovery, and request boundary: an unset environment refuses typed
/// `MissingEnvVariable` naming both variables before any auth or network
/// use, a malformed value refuses typed `MalformedEnvVariable`, and an
/// unconfigured row can neither fail the set nor redirect another provider.
/// Set `providers."azure-openai-responses".base_url` explicitly to name the
/// resource instead: explicit always wins. `ApiKey` fixes the `api-key`
/// discovery header; the row is not a compatibility preset.
pub(crate) const AZURE_OPENAI_RESPONSES: ProviderDef = ProviderDef {
    id: "azure-openai-responses",
    name: "Azure OpenAI",
    family: Family::Responses,
    family_policy: FamilyPolicy::Native,
    base_url: "https://openai.azure.com/openai/v1",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::ApiKey,
        env: &["AZURE_OPENAI_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Plain,
    base_template: AZURE_ENDPOINT_TEMPLATE,
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
};

/// Vercel AI Gateway: one operator endpoint whose whole catalog rides the
/// Anthropic Messages wire under vendor-prefixed ids, authenticated by the
/// gateway key as a bearer. Single wire: the row keeps the plain
/// single-family contract, and the row family is the authoritative one.
pub(crate) const VERCEL_AI_GATEWAY: ProviderDef = ProviderDef {
    id: "vercel-ai-gateway",
    name: "Vercel AI Gateway",
    family: Family::Anthropic,
    family_policy: FamilyPolicy::Native,
    base_url: "https://ai-gateway.vercel.sh",
    transport: Transport::Https,
    key: Some(KeySpec {
        style: AuthStyle::Bearer,
        env: &["AI_GATEWAY_API_KEY"],
    }),
    oauth: None,
    shape: Shape::Claude,
    base_template: &[],
    wires: &[],
    session_header: None,
    model_id_alias: ModelIdAlias::None,
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
    AMAZON_BEDROCK,
    CURSOR,
    DEVIN,
    GOOGLE_CLOUD_CODE_ASSIST,
    GOOGLE_ANTIGRAVITY,
    XAI,
    META,
    KIMI_CODING,
    CLOUDFLARE_AI_GATEWAY,
    CLOUDFLARE_WORKERS_AI,
    OPENCODE,
    OPENCODE_GO,
    BAI,
    AZURE_OPENAI_RESPONSES,
    VERCEL_AI_GATEWAY,
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
