//! The Cloud Code Assist project lifecycle and the Gemini CLI plain request
//! envelope.
//!
//! Two jobs live here, both over the selected HTTP client and nothing else:
//!
//! - **Project discovery** ([`discover_project`]) mints the Google Cloud
//!   project a Cloud Code Assist sign-in needs. The Gemini CLI surface
//!   follows oh-my-pi `registry/oauth/google-gemini-cli.ts`: `loadCodeAssist`
//!   with the captured environment project, the VPC-SC `SECURITY_POLICY_VIOLATED`
//!   refusal read as the standard tier, the default (else legacy) tier
//!   onboarded through `onboardUser`, and a long-running operation polled
//!   every 5 s for at most 24 attempts. The Antigravity surface follows
//!   `registry/oauth/google-antigravity.ts`: free-tier eligibility, one
//!   onboarding, then a second `loadCodeAssist` that must name the project.
//! - **Native request encoding** ([`build`]) lowers one request to the plain
//!   `{project, model, request}` envelope of the Gemini CLI surface through
//!   [`Dialect::CloudCodePlain`]. The `request` member is the flat
//!   `GenerateContent` body of the shared lowering, so tool, signature, and
//!   image rules stay in one owner; the envelope carries no `requestId`,
//!   `userAgent`, `requestType`, `labels`, or `sessionId` (those belong to
//!   Antigravity alone). An explicit thinking-off is spelled out on the wire
//!   for the models the pinned catalog flags ([`thinking_off`]), because an
//!   omitted `thinkingConfig` re-applies the backend's per-id default.
//!
//! Every call is bounded three ways: the caller's cancellation token, the
//! caller's monotonic deadline, and the shared per-request totals of
//! [`http::send`] and [`http::read_body`]. Nothing here reads the process
//! environment, the clock, or the platform: the captured [`EnvSnapshot`],
//! [`ProviderIdentity`], and [`Clock`] arrive as inputs, and the polling pause
//! goes through the [`Clock`] so a test drives 24 polls without waiting.
//! Discovery never invents a project: it returns the id the service or the
//! captured environment named, or a typed refusal that says how to fix it.

use std::time::Duration;

use dal_core::{Family, ModelRequest, ThinkingLevel};
use serde::Serialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};
use tokio_util::sync::CancellationToken;

use super::{
    antigravity,
    shared::{
        self, Compat, Dialect, GoogleRequest, GoogleWire, SignaturePolicy, ThinkingOff, ToolMode,
        generation_of, is_claude_model,
    },
};
use crate::{
    auth::credential::EnvSnapshot,
    error::ProviderError,
    http::{self, Exchange},
    lifecycle::Clock,
    provider::ProviderIdentity,
    thinking::{AnthropicThinking, ThinkingPlan, WireThinking},
};

/// The Gemini CLI version the user agent names. The pinned client reads it
/// from a process environment override; dal has no such knob, so the pinned
/// fallback is the one value.
pub(crate) const GEMINI_CLI_VERSION: &str = "0.46.0";

/// The `client-metadata` header value every Gemini CLI request carries.
pub(crate) const CLIENT_METADATA: &str =
    "ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI";

/// The pause between two operation polls of the Gemini CLI surface.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The most operation polls of the Gemini CLI surface: 24 requests, 23
/// pauses, then a bounded timeout error.
pub(crate) const POLL_MAX_ATTEMPTS: u32 = 24;

/// The suggested total budget of one discovery run: 23 pauses of
/// [`POLL_INTERVAL`] plus room for the requests between them. The caller
/// turns it into the monotonic [`Discovery::deadline`].
pub(crate) const DISCOVERY_BUDGET: Duration = Duration::from_secs(180);

/// The pause between two operation polls of the Antigravity surface.
const ANTIGRAVITY_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The most operation polls of the Antigravity surface: the 30 s onboarding
/// window of the pinned client at its 1 s cadence.
const ANTIGRAVITY_POLL_ATTEMPTS: u32 = 30;

/// The model id the discovery `User-Agent` names; the pinned client's
/// default when a request names no model.
pub(crate) const DISCOVERY_MODEL: &str = "gemini-3.1-pro-preview";

/// The captured-environment names of the project, in precedence order.
const PROJECT_ENV: [&str; 2] = ["GOOGLE_CLOUD_PROJECT", "GOOGLE_CLOUD_PROJECT_ID"];

/// Where the pinned client sends accounts that need their own project.
const WORKSPACE_DOCS: &str = "https://geminicli.com/docs/get-started/authentication/#set-gcp";

/// The fixed prefix of the account-verification message; it matches the
/// prefix the request path renders for the same classifier.
const ACCOUNT_VERIFICATION_PREFIX: &str = "Account verification required.";

const TIER_FREE: &str = "free-tier";
const TIER_LEGACY: &str = "legacy-tier";

const LOAD_PATH: &str = "v1internal:loadCodeAssist";
const ONBOARD_PATH: &str = "v1internal:onboardUser";

/// Which Cloud Code Assist surface one discovery run belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Surface<'a> {
    /// The Gemini CLI surface: environment project precedence, tiered
    /// onboarding, and the Gemini CLI headers.
    GeminiCli,
    /// The Antigravity surface: free-tier eligibility and a second
    /// `loadCodeAssist` that names the project.
    Antigravity {
        /// The client version the Antigravity `User-Agent` names.
        client_version: &'a str,
    },
}

impl Surface<'_> {
    fn family(self) -> Family {
        match self {
            Self::GeminiCli => Family::GoogleCloudCodeAssist,
            Self::Antigravity { .. } => Family::GoogleAntigravity,
        }
    }
}

/// Everything one project discovery run needs. All inputs are typed and
/// captured by the caller; the run reads nothing ambient.
pub(crate) struct Discovery<'a, C: Clock> {
    /// The surface being signed in to.
    pub(crate) surface: Surface<'a>,
    /// The one shared HTTP client of the provider set.
    pub(crate) client: &'a reqwest::Client,
    /// The root endpoint without a path: the row's base URL in production
    /// (`https://cloudcode-pa.googleapis.com` for the Gemini CLI surface), a
    /// loopback origin in a fixture. [`http::endpoint`] applies the
    /// plain-HTTP rule to it.
    pub(crate) endpoint: &'a str,
    /// The OAuth access token minted by the token exchange. It is redacted
    /// from every error text this module returns.
    pub(crate) access_token: &'a str,
    /// The captured environment: only [`env_project`] reads it, and only on
    /// the Gemini CLI surface.
    pub(crate) env: &'a EnvSnapshot,
    /// The captured platform for the Gemini CLI `User-Agent`.
    pub(crate) identity: &'a ProviderIdentity,
    /// The pause source of the operation polls.
    pub(crate) clock: &'a C,
    /// The caller's cancellation: it wins over every wait in the run.
    pub(crate) cancel: &'a CancellationToken,
    /// The monotonic instant after which the run fails; see
    /// [`DISCOVERY_BUDGET`].
    pub(crate) deadline: tokio::time::Instant,
}

/// The project the captured environment names: `GOOGLE_CLOUD_PROJECT`, then
/// `GOOGLE_CLOUD_PROJECT_ID`. The first value that is not blank wins and is
/// returned exactly as captured; a blank value counts as unset, the same
/// rule the Vertex project lookup applies.
#[must_use]
pub(crate) fn env_project(env: &EnvSnapshot) -> Option<&str> {
    PROJECT_ENV
        .iter()
        .find_map(|name| env.get(name).filter(|value| !value.trim().is_empty()))
}

/// Resolves the Google Cloud project of one Cloud Code Assist sign-in.
///
/// Gemini CLI surface: `loadCodeAssist` carries the environment project when
/// present. A reply with a `currentTier` answers with its project, else the
/// environment project. A reply without one picks the flagged default tier of
/// `allowedTiers`, else `legacy-tier` (a tier entry without an id means
/// `free-tier`); a paid tier needs the environment project and sends it with
/// `onboardUser`. The operation is polled until `done`, at most
/// [`POLL_MAX_ATTEMPTS`] times [`POLL_INTERVAL`] apart. The project is the
/// operation's, else the environment's; an unfinished operation that names
/// neither a poll target nor a project is an error, never a success on the
/// environment project. A `SECURITY_POLICY_VIOLATED` refusal
/// of `loadCodeAssist` (VPC Service Controls) reads as `standard-tier`.
///
/// Antigravity surface: `loadCodeAssist`, a free-tier eligibility check,
/// `onboardUser` for the free tier when no tier is current, and a second
/// `loadCodeAssist` whose `cloudaicompanionProject` is the answer.
///
/// # Errors
///
/// [`ProviderError::Cancelled`] when the token fires;
/// [`ProviderError::Transport`] for a missed deadline, an exhausted poll
/// bound, a reply that is not JSON, or a failure of the shared client;
/// [`ProviderError::Status`] for a refused call, with the fixed
/// account-verification text when the body carries a validated
/// `VALIDATION_REQUIRED` URL; [`ProviderError::InvalidRequest`] when the
/// account needs a project the environment does not name, the free tier is
/// refused, or the operation failed.
pub(crate) async fn discover_project<C: Clock>(
    input: &Discovery<'_, C>,
) -> Result<String, ProviderError> {
    let link = Link::new(input);
    match input.surface {
        Surface::GeminiCli => link.gemini_cli().await,
        Surface::Antigravity { .. } => link.antigravity().await,
    }
}

/// Reads only the project a plain `loadCodeAssist` names, for catalog
/// discovery: the project is optional there, so every failure but
/// cancellation is `None`, never an error. The body carries no environment
/// project, matching the pinned catalog client.
///
/// # Errors
///
/// [`ProviderError::Cancelled`] when the token fires, and
/// [`ProviderError::InvalidRequest`] for an Antigravity surface, which has no
/// such read.
#[cfg(test)]
pub(crate) async fn load_project_id<C: Clock>(
    input: &Discovery<'_, C>,
) -> Result<Option<String>, ProviderError> {
    if !matches!(input.surface, Surface::GeminiCli) {
        return Err(ProviderError::InvalidRequest {
            message: String::from("only the Gemini CLI surface reads a plain project"),
        });
    }
    let link = Link::new(input);
    let body = encode(&LoadCodeAssist {
        project: None,
        metadata: Metadata::gemini_cli(None),
    })?;
    let reply = match link.post(LOAD_PATH, body).await {
        Ok(reply) => reply,
        Err(ProviderError::Cancelled) => return Err(ProviderError::Cancelled),
        Err(_) => return Ok(None),
    };
    if !reply.success() {
        return Ok(None);
    }
    let Ok(value) = link.value(&reply.body, "loadCodeAssist") else {
        return Ok(None);
    };
    Ok(project_field(value.get("cloudaicompanionProject")).map(str::to_owned))
}

/// One reply head and its bounded body.
struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// How one surface polls a long-running operation.
#[derive(Clone, Copy)]
struct PollPlan {
    interval: Duration,
    attempts: u32,
    /// Whether the first poll waits one interval too.
    pause_first: bool,
}

const GEMINI_CLI_POLL: PollPlan = PollPlan {
    interval: POLL_INTERVAL,
    attempts: POLL_MAX_ATTEMPTS,
    pause_first: false,
};

const ANTIGRAVITY_POLL: PollPlan = PollPlan {
    interval: ANTIGRAVITY_POLL_INTERVAL,
    attempts: ANTIGRAVITY_POLL_ATTEMPTS,
    pause_first: true,
};

/// One discovery run: the typed inputs plus the surface's identity headers.
struct Link<'a, C: Clock> {
    surface: Surface<'a>,
    family: Family,
    client: &'a reqwest::Client,
    root: &'a str,
    token: &'a str,
    env: &'a EnvSnapshot,
    clock: &'a C,
    cancel: &'a CancellationToken,
    deadline: tokio::time::Instant,
    user_agent: String,
}

impl<'a, C: Clock> Link<'a, C> {
    fn new(input: &Discovery<'a, C>) -> Self {
        let agent = match input.surface {
            Surface::GeminiCli => user_agent(DISCOVERY_MODEL, input.identity),
            Surface::Antigravity { client_version } => antigravity::user_agent(client_version),
        };
        Self {
            surface: input.surface,
            family: input.surface.family(),
            client: input.client,
            root: input.endpoint,
            token: input.access_token,
            env: input.env,
            clock: input.clock,
            cancel: input.cancel,
            deadline: input.deadline,
            user_agent: agent,
        }
    }

    // -- transport ----------------------------------------------------------

    async fn post(&self, path: &str, body: Vec<u8>) -> Result<Reply, ProviderError> {
        let url = http::endpoint(self.family, self.root, path)?;
        self.send(self.client.post(url).body(body)).await
    }

    async fn get(&self, path: &str) -> Result<Reply, ProviderError> {
        let url = http::endpoint(self.family, self.root, path)?;
        self.send(self.client.get(url)).await
    }

    /// Sends one request and reads its bounded body, racing the caller's
    /// cancellation and deadline. The token goes in `authorization` only.
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Reply, ProviderError> {
        if self.cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let remaining = self
            .deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(self.expired());
        }
        let mut request = request
            .header("authorization", format!("Bearer {}", self.token))
            .header("content-type", "application/json");
        if matches!(self.surface, Surface::GeminiCli) {
            request = request.header("client-metadata", CLIENT_METADATA);
        }
        let exchange = Exchange::Json {
            total: remaining.min(http::OAUTH_TIMEOUT),
        };
        let family = self.family;
        let round_trip = async {
            let response = http::send(
                family,
                request,
                &self.user_agent,
                exchange,
                tokio::time::sleep,
            )
            .await?;
            let status = response.status().as_u16();
            let body = http::read_body(family, response).await?;
            Ok::<Reply, ProviderError>(Reply { status, body })
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(ProviderError::Cancelled),
            () = tokio::time::sleep_until(self.deadline) => Err(self.expired()),
            reply = round_trip => reply,
        }
    }

    /// Waits one polling pause on the caller's clock, still bound by
    /// cancellation and the deadline.
    async fn pause(&self, interval: Duration) -> Result<(), ProviderError> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(ProviderError::Cancelled),
            () = tokio::time::sleep_until(self.deadline) => Err(self.expired()),
            () = self.clock.sleep(interval) => Ok(()),
        }
    }

    fn expired(&self) -> ProviderError {
        ProviderError::Transport {
            family: self.family,
            reason: String::from(
                "Google project discovery ran past its deadline; try signing in again",
            ),
        }
    }

    fn invalid(&self, label: &str) -> ProviderError {
        ProviderError::Transport {
            family: self.family,
            reason: format!("{label} returned a body that is not valid JSON"),
        }
    }

    fn value(&self, body: &[u8], label: &str) -> Result<sonic_rs::Value, ProviderError> {
        sonic_rs::from_slice::<sonic_rs::Value>(body).map_err(|_| self.invalid(label))
    }

    /// Maps a refused call to a typed status error. A validated
    /// `VALIDATION_REQUIRED` URL renders the fixed account-verification text;
    /// anything else keeps the server message. The access token never
    /// survives into the text, and the URL is rendered, never fetched.
    fn rejection(&self, label: &str, reply: &Reply) -> ProviderError {
        let body = self.redact(String::from_utf8_lossy(&reply.body).into_owned());
        let message = match antigravity::validation_url(&body) {
            Some(url) => self.redact(format!(
                "{ACCOUNT_VERIFICATION_PREFIX} Visit {url} to continue, then sign in again."
            )),
            None => format!("{label} failed: {}", shared::error_fields(&body).1),
        };
        ProviderError::Status {
            family: self.family,
            status: reply.status,
            message,
        }
    }

    fn redact(&self, text: String) -> String {
        if self.token.is_empty() || !text.contains(self.token) {
            return text;
        }
        text.replace(self.token, "<redacted>")
    }

    // -- operations ---------------------------------------------------------

    fn lro(&self, body: &[u8], label: &str) -> Result<Lro, ProviderError> {
        let value = self.value(body, label)?;
        let response = value.get("response").filter(|response| !response.is_null());
        Ok(Lro {
            name: text(value.get("name")).map(str::to_owned),
            done: value
                .get("done")
                .and_then(sonic_rs::JsonValueTrait::as_bool)
                == Some(true),
            project: response
                .and_then(|response| project_field(response.get("cloudaicompanionProject")))
                .map(str::to_owned),
            failure: value
                .get("error")
                .filter(|error| !error.is_null())
                .map(describe_failure),
            has_response: response.is_some(),
        })
    }

    /// The service path of an operation, refused when the untrusted name is
    /// not a plain path: a name never reaches the URL as a query, a
    /// fragment, an escape, or a traversal.
    fn operation_path(&self, name: &str) -> Result<String, ProviderError> {
        let plain = !name.is_empty()
            && name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'~')
            });
        if !plain {
            return Err(ProviderError::Transport {
                family: self.family,
                reason: String::from("onboardUser returned an operation name that is not a path"),
            });
        }
        Ok(format!("v1internal/{name}"))
    }

    /// Polls one operation until `done`: the first request comes at once
    /// unless the plan pauses first, every later one after one interval.
    async fn poll(&self, name: &str, plan: PollPlan) -> Result<Lro, ProviderError> {
        let path = self.operation_path(name)?;
        for attempt in 0..plan.attempts {
            if attempt > 0 || plan.pause_first {
                self.pause(plan.interval).await?;
            }
            let reply = self.get(&path).await?;
            if !reply.success() {
                return Err(self.rejection("operation poll", &reply));
            }
            let lro = self.lro(&reply.body, "operation poll")?;
            if lro.done {
                return Ok(lro);
            }
        }
        Err(ProviderError::Transport {
            family: self.family,
            reason: format!(
                "project provisioning did not finish after {} polls; try again in a few minutes",
                plan.attempts
            ),
        })
    }

    // -- Gemini CLI surface -------------------------------------------------

    async fn gemini_cli(&self) -> Result<String, ProviderError> {
        let env_project = env_project(self.env);
        let loaded = self.gemini_cli_load(env_project).await?;
        if loaded.current_tier {
            return loaded
                .project
                .or_else(|| env_project.map(str::to_owned))
                .ok_or_else(project_required);
        }
        let tier = loaded.default_tier;
        let paid = tier != TIER_FREE;
        if paid && env_project.is_none() {
            return Err(project_required());
        }
        let attached = if paid { env_project } else { None };
        let lro = self.gemini_cli_onboard(&tier, attached).await?;
        lro.project
            .or_else(|| env_project.map(str::to_owned))
            .ok_or_else(project_undiscoverable)
    }

    async fn gemini_cli_load(&self, env_project: Option<&str>) -> Result<Loaded, ProviderError> {
        let body = encode(&LoadCodeAssist {
            project: env_project,
            metadata: Metadata::gemini_cli(env_project),
        })?;
        let reply = self.post(LOAD_PATH, body).await?;
        if reply.success() {
            return Ok(Loaded::from_value(
                &self.value(&reply.body, "loadCodeAssist")?,
            ));
        }
        if security_policy_violated(&reply.body) {
            return Ok(Loaded::standard_tier());
        }
        Err(self.rejection("loadCodeAssist", &reply))
    }

    async fn gemini_cli_onboard(
        &self,
        tier: &str,
        project: Option<&str>,
    ) -> Result<Lro, ProviderError> {
        let body = encode(&OnboardUser {
            tier_id: tier,
            metadata: Metadata::gemini_cli(project),
            project,
        })?;
        let reply = self.post(ONBOARD_PATH, body).await?;
        if !reply.success() {
            return Err(self.rejection("onboardUser", &reply));
        }
        let mut lro = self.lro(&reply.body, "onboardUser")?;
        if !lro.done {
            match lro.name.take() {
                Some(name) => lro = self.poll(&name, GEMINI_CLI_POLL).await?,
                // An unfinished operation that names neither a poll target
                // nor a project has provisioned nothing; the environment
                // project must not turn that into a success.
                None if lro.project.is_none() => {
                    return Err(ProviderError::Transport {
                        family: self.family,
                        reason: String::from(
                            "onboardUser returned an unfinished operation without a name",
                        ),
                    });
                }
                None => {}
            }
        }
        Ok(lro)
    }

    // -- Antigravity surface ------------------------------------------------

    async fn antigravity(&self) -> Result<String, ProviderError> {
        let loaded = self.antigravity_load().await?;
        if !loaded.free_allowed
            && let Some(refusal) = loaded.free_refusal
        {
            return Err(refusal.into_error());
        }
        if !loaded.current_tier {
            self.antigravity_onboard().await?;
        }
        let refreshed = self.antigravity_load().await?;
        refreshed.project.ok_or_else(|| ProviderError::Transport {
            family: self.family,
            reason: String::from("loadCodeAssist did not return a cloudaicompanionProject"),
        })
    }

    /// The pinned two-step load: a second call restates the project when the
    /// first named one but carried no `paidTier`.
    async fn antigravity_load(&self) -> Result<Loaded, ProviderError> {
        let first = self.antigravity_load_once(None).await?;
        if first.paid_tier {
            return Ok(first);
        }
        let Some(project) = first.project.clone() else {
            return Ok(first);
        };
        self.antigravity_load_once(Some(&project)).await
    }

    async fn antigravity_load_once(&self, project: Option<&str>) -> Result<Loaded, ProviderError> {
        let body = encode(&LoadCodeAssist {
            project,
            metadata: Metadata::antigravity(),
        })?;
        let reply = self.post(LOAD_PATH, body).await?;
        if !reply.success() {
            return Err(self.rejection("loadCodeAssist", &reply));
        }
        Ok(Loaded::from_value(
            &self.value(&reply.body, "loadCodeAssist")?,
        ))
    }

    async fn antigravity_onboard(&self) -> Result<(), ProviderError> {
        let body = encode(&OnboardUser {
            tier_id: TIER_FREE,
            metadata: Metadata::antigravity(),
            project: None,
        })?;
        let reply = self.post(ONBOARD_PATH, body).await?;
        if !reply.success() {
            return Err(self.rejection("onboardUser", &reply));
        }
        let mut lro = self.lro(&reply.body, "onboardUser")?;
        if !lro.done {
            let Some(name) = lro.name.take() else {
                return Err(ProviderError::Transport {
                    family: self.family,
                    reason: String::from("onboardUser returned an operation without a name"),
                });
            };
            lro = self.poll(&name, ANTIGRAVITY_POLL).await?;
        }
        if let Some(failure) = lro.failure {
            return Err(ProviderError::InvalidRequest {
                message: format!("onboardUser operation failed: {failure}"),
            });
        }
        if !lro.has_response {
            return Err(ProviderError::Transport {
                family: self.family,
                reason: String::from("onboardUser finished without a response"),
            });
        }
        Ok(())
    }
}

// -- wire bodies --------------------------------------------------------------

/// The `metadata` object of `loadCodeAssist` and `onboardUser`.
#[derive(Serialize)]
struct Metadata<'a> {
    #[serde(rename = "ideType")]
    ide_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<&'static str>,
    #[serde(rename = "pluginType", skip_serializing_if = "Option::is_none")]
    plugin_type: Option<&'static str>,
    #[serde(rename = "duetProject", skip_serializing_if = "Option::is_none")]
    duet_project: Option<&'a str>,
}

impl<'a> Metadata<'a> {
    fn gemini_cli(duet_project: Option<&'a str>) -> Self {
        Self {
            ide_type: "IDE_UNSPECIFIED",
            platform: Some("PLATFORM_UNSPECIFIED"),
            plugin_type: Some("GEMINI"),
            duet_project,
        }
    }

    fn antigravity() -> Self {
        Self {
            ide_type: "ANTIGRAVITY",
            platform: None,
            plugin_type: None,
            duet_project: None,
        }
    }
}

/// The `loadCodeAssist` request body.
#[derive(Serialize)]
struct LoadCodeAssist<'a> {
    #[serde(
        rename = "cloudaicompanionProject",
        skip_serializing_if = "Option::is_none"
    )]
    project: Option<&'a str>,
    metadata: Metadata<'a>,
}

/// The `onboardUser` request body: the project member trails the metadata,
/// as the pinned client builds it.
#[derive(Serialize)]
struct OnboardUser<'a> {
    #[serde(rename = "tierId")]
    tier_id: &'a str,
    metadata: Metadata<'a>,
    #[serde(
        rename = "cloudaicompanionProject",
        skip_serializing_if = "Option::is_none"
    )]
    project: Option<&'a str>,
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, ProviderError> {
    sonic_rs::to_vec(value).map_err(|error| ProviderError::InvalidRequest {
        message: error.to_string(),
    })
}

// -- wire replies ---------------------------------------------------------------

/// What one `loadCodeAssist` reply says.
#[derive(Default)]
struct Loaded {
    /// The `cloudaicompanionProject`, a string or an object with an `id`.
    project: Option<String>,
    /// A `currentTier` is present: the account is already onboarded.
    current_tier: bool,
    /// A `paidTier` is present.
    paid_tier: bool,
    /// The flagged default of `allowedTiers`; `legacy-tier` without one.
    default_tier: String,
    /// `allowedTiers` lists the free tier.
    free_allowed: bool,
    /// Why the free tier is refused, when the reply says.
    free_refusal: Option<Refusal>,
}

/// An `ineligibleTiers` entry for the free tier.
struct Refusal {
    reason: String,
    url: Option<String>,
}

impl Refusal {
    fn into_error(self) -> ProviderError {
        let message = match self.url {
            Some(url) => format!("{}\n{url}", self.reason),
            None => self.reason,
        };
        ProviderError::InvalidRequest { message }
    }
}

impl Loaded {
    /// The VPC Service Controls reading of a refused `loadCodeAssist`: an
    /// onboarded standard-tier account that names no project of its own.
    fn standard_tier() -> Self {
        Self {
            current_tier: true,
            ..Self::default()
        }
    }

    fn from_value(value: &sonic_rs::Value) -> Self {
        let allowed = value.get("allowedTiers").and_then(|tiers| tiers.as_array());
        let free_allowed = allowed.is_some_and(|tiers| {
            tiers
                .iter()
                .any(|tier| text(tier.get("id")) == Some(TIER_FREE))
        });
        let default_tier = match allowed.and_then(|tiers| {
            tiers.iter().find(|tier| {
                tier.get("isDefault")
                    .and_then(sonic_rs::JsonValueTrait::as_bool)
                    == Some(true)
            })
        }) {
            Some(tier) => text(tier.get("id")).unwrap_or(TIER_FREE).to_owned(),
            None => TIER_LEGACY.to_owned(),
        };
        let free_refusal = value
            .get("ineligibleTiers")
            .and_then(|tiers| tiers.as_array())
            .and_then(|tiers| {
                tiers
                    .iter()
                    .find(|tier| text(tier.get("tierId")) == Some(TIER_FREE))
            })
            .and_then(|tier| {
                let reason = text(tier.get("reasonMessage"))?;
                Some(Refusal {
                    reason: reason.to_owned(),
                    url: text(tier.get("validationUrl")).map(str::to_owned),
                })
            });
        Self {
            project: project_field(value.get("cloudaicompanionProject")).map(str::to_owned),
            current_tier: present(value.get("currentTier")),
            paid_tier: present(value.get("paidTier")),
            default_tier,
            free_allowed,
            free_refusal,
        }
    }
}

/// What one long-running operation reply says.
struct Lro {
    name: Option<String>,
    done: bool,
    /// The `response.cloudaicompanionProject` id.
    project: Option<String>,
    /// The rendered `error`, when the operation failed.
    failure: Option<String>,
    /// A non-null `response` is present.
    has_response: bool,
}

fn describe_failure(error: &sonic_rs::Value) -> String {
    match text(error.get("message")) {
        Some(message) => match error.get("code").and_then(sonic_rs::JsonValueTrait::as_i64) {
            Some(code) => format!("{code}: {message}"),
            None => message.to_owned(),
        },
        None => sonic_rs::to_string(error).unwrap_or_else(|_| String::from("unknown failure")),
    }
}

/// A non-blank string member, trimmed.
fn text(value: Option<&sonic_rs::Value>) -> Option<&str> {
    value
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// A project member: a non-blank string, or an object carrying an `id`.
fn project_field(value: Option<&sonic_rs::Value>) -> Option<&str> {
    text(value).or_else(|| text(value.and_then(|object| object.get("id"))))
}

/// Whether a member is present and not `null`.
fn present(value: Option<&sonic_rs::Value>) -> bool {
    value.is_some_and(|value| !value.is_null())
}

/// Whether a refused `loadCodeAssist` body carries the VPC Service Controls
/// reason `SECURITY_POLICY_VIOLATED` in `error.details`.
fn security_policy_violated(body: &[u8]) -> bool {
    let Ok(parsed) = sonic_rs::from_slice::<sonic_rs::Value>(body) else {
        return false;
    };
    let Some(details) = parsed
        .get("error")
        .and_then(|error| error.get("details"))
        .and_then(|details| details.as_array())
    else {
        return false;
    };
    details.iter().any(|detail| {
        detail.get("reason").and_then(|reason| reason.as_str()) == Some("SECURITY_POLICY_VIOLATED")
    })
}

fn project_required() -> ProviderError {
    ProviderError::InvalidRequest {
        message: format!(
            "this account needs a Google Cloud project: set {} or {}, then sign in again. See {WORKSPACE_DOCS}.",
            PROJECT_ENV[0], PROJECT_ENV[1]
        ),
    }
}

fn project_undiscoverable() -> ProviderError {
    ProviderError::InvalidRequest {
        message: format!(
            "could not discover or provision a Google Cloud project: set {} or {}, then sign in again. See {WORKSPACE_DOCS}.",
            PROJECT_ENV[0], PROJECT_ENV[1]
        ),
    }
}

// -- native request encoding --------------------------------------------------------

/// The Node.js platform name the pinned client reports for a Rust OS name.
fn node_platform(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// The Node.js architecture name the pinned client reports for a Rust one.
fn node_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// The Gemini CLI `User-Agent` value:
/// `GeminiCLI/<version>/<model> (<platform>; <arch>; terminal)`, with the
/// captured platform spelled the way the pinned client reports it.
#[must_use]
pub(crate) fn user_agent(model: &str, identity: &ProviderIdentity) -> String {
    format!(
        "GeminiCLI/{GEMINI_CLI_VERSION}/{model} ({}; {}; terminal)",
        node_platform(&identity.os),
        node_arch(&identity.arch)
    )
}

/// The wire policy of one Gemini CLI model id, from the pinned compat
/// classes. Gemini 3+ takes the signature bypass sentinel on the first
/// unsigned call only and nests tool images, and no Gemini id carries
/// function-part ids; Claude and the open-weight routes carry them, and
/// Claude takes the legacy `parameters` schema. Thinking replays on every
/// route (only Antigravity drops unsigned Claude thinking), and the tool mode
/// stays at the wire default.
#[must_use]
pub(crate) fn compat(wire_model: &str) -> Compat {
    if is_claude_model(wire_model) {
        return Compat {
            part_ids: true,
            skip: SignaturePolicy::Never,
            nested_tool_images: false,
            legacy_parameters: true,
            replay_thinking: true,
            tool_mode: ToolMode::Auto,
        };
    }
    if wire_model.starts_with("gemini-") {
        let three = generation_of(wire_model).is_three_or_later();
        return Compat {
            part_ids: false,
            skip: if three {
                SignaturePolicy::FirstCall
            } else {
                SignaturePolicy::Never
            },
            nested_tool_images: three,
            legacy_parameters: false,
            replay_thinking: true,
            tool_mode: ToolMode::Auto,
        };
    }
    Compat {
        part_ids: true,
        skip: SignaturePolicy::Never,
        nested_tool_images: false,
        legacy_parameters: false,
        replay_thinking: true,
        tool_mode: ToolMode::Auto,
    }
}

/// Everything one native Gemini CLI request needs besides the neutral
/// request.
pub(crate) struct NativeRequest<'a> {
    /// The neutral request; its route id is the wire model.
    pub(crate) request: &'a ModelRequest,
    /// The thinking fragment from the composed plan.
    pub(crate) thinking: &'a ThinkingPlan,
    /// The provider id the request runs under; it names the sign-in fix.
    pub(crate) provider: &'a str,
    /// The OAuth access token.
    pub(crate) access_token: &'a str,
    /// The project the sign-in stored.
    pub(crate) project: &'a str,
    /// The captured platform for the `User-Agent`.
    pub(crate) identity: &'a ProviderIdentity,
    /// Whether the catalog entry accepts image input.
    pub(crate) image_input: bool,
}

/// The wire ids whose thinking-off the pinned catalog spells out: the members
/// of the `gemini-3.5-flash`, `gemini-3.1-pro`, and `gemini-3-pro` families
/// of the `google-gemini-cli` provider that carry `suppress-when-off`
/// (oh-my-pi `packages/catalog/src/compat/rules/taxonomy/_collapse.kdl`; the
/// retired `gemini-3.1-pro-high` member is not listed). No other id is
/// flagged, so every other model keeps an omitted `thinkingConfig`: the
/// backend rejects a suppression it does not support for such a model.
const SUPPRESS_WHEN_OFF: [&str; 7] = [
    "gemini-3.5-flash-extra-low",
    "gemini-3.5-flash-low",
    "gemini-3-flash-agent",
    "gemini-3.1-pro-low",
    "gemini-pro-agent",
    "gemini-3-pro-low",
    "gemini-3-pro-high",
];

/// Whether this request needs an explicit thinking-off on the wire, and
/// which spelling.
///
/// An omitted `thinkingConfig` re-applies the backend's per-id default, so a
/// flagged model must say it. Off counts when the request asked for it
/// (`request.params.thinking` is `off`: the session resolves its own level,
/// `medium` by default, before it builds the request, and utility requests
/// that are built `off` follow the same path) and the composed plan kept it
/// (`plan.level` is `off`; a hook effort that raised the level leaves the
/// plan's own `thinkingConfig` alone). A request for another level that the
/// capability row clamped down to `off` stays omitted. The model must be one
/// of [`SUPPRESS_WHEN_OFF`]. The capability row must express off, as the
/// owner planner decides it, and names the spelling: an effort row plans the
/// effort `none` (`WireThinking::OpenAi { effort: Some("none") }`) and spells
/// `thinkingLevel: "MINIMAL"` ([`ThinkingOff::Level`]); a budget row plans
/// `AnthropicThinking::Disabled` and spells `thinkingBudget: 0`
/// ([`ThinkingOff::Budget`]). A row that cannot disable plans an omitted
/// member, and that is respected.
#[must_use]
pub(crate) fn thinking_off(
    request: &ModelRequest,
    plan: &ThinkingPlan,
    model: &str,
) -> Option<ThinkingOff> {
    if request.params.thinking != ThinkingLevel::Off
        || plan.level != ThinkingLevel::Off
        || !SUPPRESS_WHEN_OFF.contains(&model)
    {
        return None;
    }
    match plan.wire {
        WireThinking::OpenAi {
            effort: Some("none"),
        } => Some(ThinkingOff::Level),
        WireThinking::Anthropic {
            thinking: AnthropicThinking::Disabled,
            ..
        } => Some(ThinkingOff::Budget),
        _ => None,
    }
}

/// Lowers one request to its path, headers, and body: a `POST` to
/// `<base>/v1internal:streamGenerateContent?alt=sse` with the configured base
/// alone (no failover), `authorization`, `client-metadata`, and the Gemini
/// CLI `User-Agent`, and the exact body `{"project":…,"model":…,"request":…}`.
/// The reply decodes with the shared `GenerateContent` decoder.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for a route of another family, a blob
/// part, a blank project, or an unserializable body.
pub(crate) fn build(input: &NativeRequest<'_>) -> Result<GoogleWire, ProviderError> {
    let family = Surface::GeminiCli.family();
    let model = shared::route_model(input.request, family)?;
    if input.project.trim().is_empty() {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "{provider} has no Google Cloud project for this sign-in; run dalgon login {provider} again.",
                provider = input.provider
            ),
        });
    }
    let body = shared::request_body(&GoogleRequest {
        family,
        request: input.request,
        thinking: input.thinking,
        compat: compat(model),
        image_input: input.image_input,
        dialect: Dialect::CloudCodePlain {
            project: input.project,
            off: thinking_off(input.request, input.thinking, model),
        },
    })?;
    Ok(GoogleWire {
        path: super::CCA_STREAM_PATH.to_owned(),
        base_override: None,
        headers: vec![
            ("authorization", format!("Bearer {}", input.access_token)),
            ("client-metadata", CLIENT_METADATA.to_owned()),
        ],
        user_agent: Some(user_agent(model, input.identity)),
        body,
        fallback: None,
    })
}

#[cfg(test)]
mod tests;
