//! `Azure` `OpenAI` over the Chat and Responses codecs: deployment-name routing,
//! the `api-key` header, and the `api-version` query.
//!
//! Azure is not a new wire family. Its bodies are the exact `openai_chat` and
//! `openai_responses` bodies; only four things differ, and this module owns
//! exactly those four:
//!
//! 1. **Authentication.** The key travels as `api-key: <key>`, never as
//!    `authorization: Bearer` (pinned oh-my-pi `azure-openai-responses.ts`
//!    `buildAzureResponsesRequest`; senpi `AzureOpenAI` client). An OAuth sign-in
//!    is refused: Entra bearer tokens are not part of the census.
//! 2. **Deployment routing.** Azure serves a *deployment*, not a model id. The
//!    deployment name is `[providers.<id>.deployments]` (or the parsed
//!    `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` snapshot value, [`DeploymentMap::parse`])
//!    looked up by the catalog model id, falling back to the model id itself.
//!    On Responses it is the body `model`; on Chat it is the body `model` and the
//!    `deployments/<name>` URL segment.
//! 3. **Endpoint shape.** Responses posts to `<base>/responses?api-version=<v>`
//!    with the base normalised to `https://<resource>.openai.azure.com/openai/v1`
//!    (senpi `normalizeAzureBaseUrl`); Chat posts to
//!    `<base>/deployments/<name>/chat/completions?api-version=<v>` on the
//!    `.../openai` base (the legacy deployments surface of the pinned oh-my-pi
//!    `resolveOpenAIRequestSetup`). The default `api-version` is `v1` for
//!    Responses and `2024-10-21` for Chat; an explicit setting wins for both.
//! 4. **Resource addressing.** [`base_url_for_resource`] turns the resource name
//!    (`AZURE_OPENAI_RESOURCE_NAME`) into the default base URL. The library reads
//!    no environment (decision D-29): the binary passes the snapshot values in.
//!
//! # Composition, not a fork
//!
//! The body is built by [`chat::request_body`] or [`responses::request_body`]
//! with the *catalog* route untouched, then only its leading `"model"` member is
//! rewritten to the deployment name ([`rewrite_model`]; the same byte-splice idea
//! as `codex::websocket_frame`). Keeping the route untouched matters: reasoning
//! replay is bound to `(family, catalog model id)` (`responses::input_items`,
//! decision D-14), and the decoder records its replay payloads under the same
//! pair, so a deployment alias never orphans a replayed reasoning item.
//!
//! # Joins (landed, except Responses compaction)
//!
//! The dispatch owner has wired every join except the last one:
//!
//! * `family/mod.rs`: `pub(crate) mod azure;` (and `copilot`, which reuses
//!   [`rewrite_model`] and [`repair_pairing`]).
//! * Config: a named provider with `api = "openai_responses"` (id `azure`) and
//!   `api = "openai_chat"` (id `azure-chat`), `auth = "api-key"` (the
//!   `AuthStyle::ApiKey`, header `api-key`), plus the keys `api_version` and a
//!   `deployments` table in `PROVIDER_KEYS`. Azure is a named-provider shape,
//!   not a built-in preset row (the preset predicate in `def/presets.rs` forbids
//!   rows whose "wire, or required headers depend on the model, the account, or
//!   the credential").
//! * `provider/request.rs::stream_attempt`: an Azure entry (a Chat or Responses
//!   row with the `api-key` style or Azure settings) routes to [`build`] via
//!   `azure_parts`, then `wire.endpoint()` / `wire.into_request(client, url)`;
//!   the decoder stays the surface family's and the decoder model is the
//!   catalog id (`wire.model`), never the deployment.
//! * Transport: [`require_https`] runs when an Azure or Copilot entry is bound
//!   (`provider/api.rs`); both services are HTTPS only.
//! * Config keys: `api_version` and `deployments` in `PROVIDER_KEYS` decode
//!   through the central strict `decode_settings`, and [`AzureSettings::over`]
//!   with [`AzureSettings::from_env`] lets the config win over
//!   `AZURE_OPENAI_API_VERSION` / `AZURE_OPENAI_DEPLOYMENT_NAME_MAP`. A model
//!   id that holds a dot must be a quoted TOML key
//!   (`"gpt-5.5" = "prod-gpt55"`), or TOML reads it as a table path. When an
//!   Azure-shaped entry's table omits `base_url`, provider preparation calls
//!   [`base_url_from_env`], which supplies `AZURE_OPENAI_BASE_URL` or the
//!   `AZURE_OPENAI_RESOURCE_NAME` default; an explicit table URL always wins.
//!
//! # Remaining join: Responses remote compaction (dispatch owner)
//!
//! `provider/request.rs::compact_attempt` still lowers every Responses
//! compaction through the plain Responses wire, so an Azure Responses entry
//! compacts at `<base>/responses/compact` with no `api-version` query, no
//! Azure base normalization, and the catalog model instead of the deployment
//! in the body. (The `api-key` header does flow through: `responses::wire`
//! maps `AuthStyle::ApiKey` to it and the attempt forwards `wire.auth_header`.)
//! The dispatch owner routes that arm through [`build`] (or an equivalent
//! `api-version`-carrying compact path) before Azure compaction can succeed;
//! nothing in this module can reach it, because route metadata and the
//! compaction call live outside this slice.
//!
//! # Strict pairing and the Astra quirk
//!
//! Azure rejects an unpaired tool output (`400 No tool call found for function
//! call output`) and an unpaired call (`400 No tool output found`), as do the
//! chat surface and Copilot. [`repair_pairing`] folds an orphan result into an
//! assistant note and gives an orphan call a placeholder result, exactly the
//! pinned oh-my-pi `repairOrphanResponsesToolOutputs` /
//! `repairOrphanResponsesToolCalls`; a clean request is borrowed untouched.
//! `gpt-6-astra*` on Azure Chat rejects reasoning whenever function tools are
//! present, so the explicit effort `none` is sent there (oh-my-pi
//! `providers/azure.kdl`, `reasoning-disable-mode "none-effort"`).

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    fmt,
};

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, Part, ReplaySource,
};
use url::Url;

use crate::{
    auth::credential::{Credential, EnvSnapshot},
    error::ProviderError,
    family::{chat, responses},
    http,
    provider::Transport,
    thinking::{ThinkingPlan, WireThinking},
};

/// The environment variable that names the Azure API version.
const API_VERSION_ENV: &str = "AZURE_OPENAI_API_VERSION";

/// The environment variable that maps model ids to deployment names.
const DEPLOYMENT_MAP_ENV: &str = "AZURE_OPENAI_DEPLOYMENT_NAME_MAP";

/// The environment variable that names the endpoint base URL.
const BASE_URL_ENV: &str = "AZURE_OPENAI_BASE_URL";

/// The environment variable that names the `Azure` `OpenAI` resource.
const RESOURCE_NAME_ENV: &str = "AZURE_OPENAI_RESOURCE_NAME";

/// The model-id prefix whose Azure Chat rows reject reasoning beside tools.
const ASTRA_PREFIX: &str = "gpt-6-astra";

/// The longest orphan tool output folded into an assistant note, in characters.
const ORPHAN_OUTPUT_LIMIT: usize = 16_000;

/// What replaces the cut tail of an orphan output.
const ORPHAN_OUTPUT_CUT: &str = "\n...[truncated]";

/// The result given to a tool call whose output never reached the context.
const ORPHAN_CALL_PLACEHOLDER: &str =
    "[No tool output recorded: the tool call was interrupted before it produced a result.]";

/// The header that carries an `Azure` `OpenAI` key.
const API_KEY_HEADER: &str = "api-key";

/// The default `api-version` of the Responses surface.
const RESPONSES_API_VERSION: &str = "v1";

/// The default `api-version` of the Chat surface.
const CHAT_API_VERSION: &str = "2024-10-21";

/// The host suffixes whose base path Azure normalises.
const AZURE_HOST_SUFFIXES: [&str; 3] = [
    ".openai.azure.com",
    ".cognitiveservices.azure.com",
    ".ai.azure.com",
];

/// The longest DNS label.
const RESOURCE_NAME_MAX: usize = 63;

/// The text every family body starts with: its `model` member.
const MODEL_MEMBER: &[u8] = br#"{"model":"#;

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

/// Refuses any transport but HTTPS: `Azure` `OpenAI` and GitHub Copilot serve no
/// WebSocket Responses endpoint, so a `websocket` setting would only fail at the
/// first request.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] naming `provider` for any other transport.
pub(crate) fn require_https(provider: &str, transport: Transport) -> Result<(), ProviderError> {
    if transport == Transport::Https {
        Ok(())
    } else {
        Err(invalid(format!(
            "{provider} is served over HTTPS only; transport websocket is not supported"
        )))
    }
}

/// Catalog model id to Azure deployment name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeploymentMap(BTreeMap<Box<str>, Box<str>>);

impl DeploymentMap {
    /// Parses the `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` spelling:
    /// `model=deployment,model2=deployment2`. Blank entries, entries without
    /// `=` or with an empty side or a second `=` are skipped; a later entry for
    /// the same model wins.
    pub(crate) fn parse(text: &str) -> Self {
        text.split(',')
            .filter_map(|entry| {
                let (model, deployment) = entry.split_once('=')?;
                let (model, deployment) = (model.trim(), deployment.trim());
                (!model.is_empty() && !deployment.is_empty() && !deployment.contains('='))
                    .then(|| (Box::<str>::from(model), Box::<str>::from(deployment)))
            })
            .collect()
    }

    /// The deployment serving `model`; the model id when none is mapped.
    pub(crate) fn deployment<'a>(&'a self, model: &'a str) -> &'a str {
        self.0.get(model).map_or(model, |deployment| &**deployment)
    }
}

impl FromIterator<(Box<str>, Box<str>)> for DeploymentMap {
    fn from_iter<I: IntoIterator<Item = (Box<str>, Box<str>)>>(pairs: I) -> Self {
        Self(pairs.into_iter().collect())
    }
}

/// The Azure-specific settings of one configured provider.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AzureSettings {
    /// The `api-version` query value; `None` keeps each surface's default.
    pub(crate) api_version: Option<Box<str>>,
    /// Catalog model id to deployment name.
    pub(crate) deployments: DeploymentMap,
}

impl DeploymentMap {
    /// `self` with every entry of `over` added; `over` wins on a clash.
    fn merged(mut self, over: Self) -> Self {
        self.0.extend(over.0);
        self
    }
}

impl AzureSettings {
    /// The settings in an environment snapshot: `AZURE_OPENAI_API_VERSION` and
    /// `AZURE_OPENAI_DEPLOYMENT_NAME_MAP`. The library reads no environment
    /// (decision D-29); the binary passes the snapshot.
    pub(crate) fn from_env(env: &EnvSnapshot) -> Self {
        Self {
            api_version: env
                .get(API_VERSION_ENV)
                .map(str::trim)
                .filter(|version| !version.is_empty())
                .map(Into::into),
            deployments: env
                .get(DEPLOYMENT_MAP_ENV)
                .map(DeploymentMap::parse)
                .unwrap_or_default(),
        }
    }

    /// `self` over `base`: this table's `api_version` wins, and its deployments
    /// win model by model. The config is `self`; the environment is `base`.
    pub(crate) fn over(self, base: Self) -> Self {
        Self {
            api_version: self.api_version.or(base.api_version),
            deployments: base.deployments.merged(self.deployments),
        }
    }
}

/// The base URL the environment names, for a provider whose `base_url` is
/// omitted: `AZURE_OPENAI_BASE_URL`, else the default URL of
/// `AZURE_OPENAI_RESOURCE_NAME`. `None` when neither is set. Called once per
/// entry during provider preparation; an entry with an explicit table URL
/// never reaches this function, so an unused invalid resource name can
/// neither override that URL nor fail the set.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] naming `provider` when the resource name
/// is not one DNS label.
pub(crate) fn base_url_from_env(
    provider: &str,
    env: &EnvSnapshot,
) -> Result<Option<String>, ProviderError> {
    let set = |name: &str| {
        env.get(name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    if let Some(base) = set(BASE_URL_ENV) {
        return Ok(Some(base.to_owned()));
    }
    set(RESOURCE_NAME_ENV)
        .map(|resource| {
            base_url_for_resource(resource).map_err(|error| match error {
                ProviderError::InvalidRequest { message } => ProviderError::InvalidRequest {
                    message: format!("provider {provider}: {RESOURCE_NAME_ENV}: {message}"),
                },
                other => other,
            })
        })
        .transpose()
}

/// Cuts `text` to `limit` characters, marking the cut.
fn cut_chars(text: String, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}{ORPHAN_OUTPUT_CUT}", &text[..end]),
        None => text,
    }
}

/// The text of a tool result: its text parts joined by LF.
fn result_text(parts: &[Part]) -> String {
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(&**text),
            Part::Image { .. } | Part::Blob { .. } => None,
        })
        .collect();
    texts.join("\n")
}

/// The calls of each assistant item that no later result answers, by item index.
fn unanswered_calls(context: &[ContextItem]) -> Vec<Vec<(&CallId, &str)>> {
    let mut answered: HashSet<&CallId> = HashSet::new();
    let mut unanswered = vec![Vec::new(); context.len()];
    for (index, item) in context.iter().enumerate().rev() {
        match item {
            ContextItem::ToolResult { call, .. } => {
                answered.insert(call);
            }
            ContextItem::Assistant { parts, .. } => {
                unanswered[index] = parts
                    .iter()
                    .filter_map(|part| match part {
                        AssistantPart::ToolCall { call, name, .. } if !answered.contains(call) => {
                            Some((call, &**name))
                        }
                        _ => None,
                    })
                    .collect();
            }
            ContextItem::User { .. } => {}
        }
    }
    unanswered
}

/// The request with every orphan tool exchange repaired, so a strict backend
/// (Azure, Copilot) never sees an unpaired call or output.
///
/// An *orphan result* answers a call no earlier assistant item made; it becomes
/// an assistant note `[Orphan <tool> result; call_id=<id>]: <text>` with the text
/// cut at 16000 characters, which keeps what the model learned without a result
/// the backend would refuse. An *orphan call* has no later result; a placeholder
/// error result follows its assistant item at once, so the model can see the call
/// was interrupted and re-issue it. A request with neither is borrowed unchanged.
pub(crate) fn repair_pairing(request: &ModelRequest) -> Cow<'_, ModelRequest> {
    let ModelRoute::Api { family, model } = &request.model else {
        return Cow::Borrowed(request);
    };
    let unanswered = unanswered_calls(&request.context);
    let mut called: HashSet<&CallId> = HashSet::new();
    let mut repaired = Vec::with_capacity(request.context.len());
    let mut changed = false;
    for (item, orphans) in request.context.iter().zip(&unanswered) {
        match item {
            ContextItem::Assistant { parts, .. } => {
                called.extend(parts.iter().filter_map(|part| match part {
                    AssistantPart::ToolCall { call, .. } => Some(call),
                    _ => None,
                }));
                repaired.push(item.clone());
                for &(call, name) in orphans {
                    changed = true;
                    repaired.push(ContextItem::ToolResult {
                        call: call.clone(),
                        name: name.into(),
                        is_error: true,
                        parts: vec![Part::Text {
                            text: ORPHAN_CALL_PLACEHOLDER.into(),
                        }],
                    });
                }
            }
            ContextItem::ToolResult {
                call, name, parts, ..
            } if !called.contains(call) => {
                changed = true;
                let text = cut_chars(result_text(parts), ORPHAN_OUTPUT_LIMIT);
                repaired.push(ContextItem::Assistant {
                    source: ReplaySource {
                        family: *family,
                        model: model.clone(),
                    },
                    parts: vec![AssistantPart::Text {
                        text: format!("[Orphan {name} result; call_id={}]: {text}", call.as_str())
                            .into(),
                        replay: None,
                    }],
                    replay: None,
                });
            }
            ContextItem::ToolResult { .. } | ContextItem::User { .. } => {
                repaired.push(item.clone());
            }
        }
    }
    if !changed {
        return Cow::Borrowed(request);
    }
    let mut owned = request.clone();
    owned.context = repaired.into();
    Cow::Owned(owned)
}

/// The plan for an Azure request: `gpt-6-astra*` on Chat with tools sends the
/// explicit effort `none`, because that row rejects reasoning beside function
/// tools. Every other request keeps `plan`.
fn azure_thinking<'a>(request: &ModelRequest, plan: &'a ThinkingPlan) -> Cow<'a, ThinkingPlan> {
    let ModelRoute::Api {
        family: Family::Chat,
        model,
    } = &request.model
    else {
        return Cow::Borrowed(plan);
    };
    let reasons = matches!(
        plan.wire,
        WireThinking::OpenAi { effort } if effort != Some("none")
    );
    if model.starts_with(ASTRA_PREFIX) && !request.tools.is_empty() && reasons {
        let mut quirked = plan.clone();
        quirked.wire = WireThinking::OpenAi {
            effort: Some("none"),
        };
        return Cow::Owned(quirked);
    }
    Cow::Borrowed(plan)
}

/// The default base URL of the `Azure` `OpenAI` resource named `resource`.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when `resource` is not one DNS label
/// (letters, digits, and inner hyphens, at most 63 bytes), so a resource name
/// can never redirect the credential to another host.
pub(crate) fn base_url_for_resource(resource: &str) -> Result<String, ProviderError> {
    let resource = resource.trim();
    let label = !resource.is_empty()
        && resource.len() <= RESOURCE_NAME_MAX
        && !resource.starts_with('-')
        && !resource.ends_with('-')
        && resource
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !label {
        return Err(invalid(format!(
            "azure resource name {resource:?} must be one DNS label of letters, digits, and hyphens"
        )));
    }
    Ok(format!("https://{resource}.openai.azure.com/openai/v1"))
}

/// Everything one Azure body needs.
pub(crate) struct AzureRequest<'a> {
    /// The neutral request; its route names the catalog model and the family
    /// (`openai_chat` or `openai_responses`).
    pub(crate) request: &'a ModelRequest,
    /// The composed thinking plan for the route's family.
    pub(crate) thinking: &'a ThinkingPlan,
    /// Whether the resolved model supports reasoning summaries (Responses).
    pub(crate) reasoning_summary: bool,
    /// The configured provider id, for credential errors.
    pub(crate) provider: &'a str,
    /// The configured base URL.
    pub(crate) base_url: &'a str,
    /// The provider's Azure settings.
    pub(crate) settings: &'a AzureSettings,
    /// The credential; only an API key is accepted.
    pub(crate) credential: &'a Credential,
}

/// One lowered Azure request: effective base, path, headers, and body.
#[must_use]
pub(crate) struct AzureWire {
    /// The family whose decoder reads the response.
    pub(crate) family: Family,
    /// The effective base URL, after Azure normalisation.
    pub(crate) base_url: String,
    /// The path and `api-version` query, relative to `base_url`.
    pub(crate) path: String,
    /// Headers in send order; names are lowercase.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The JSON body, with the deployment name as its `model`.
    pub(crate) body: Vec<u8>,
    /// The raw key, for redacting provider-controlled messages.
    pub(crate) secret: Box<str>,
    /// The catalog model id: the decoder's replay binding.
    pub(crate) model: Box<str>,
    /// The deployment name sent on the wire.
    pub(crate) deployment: Box<str>,
}

impl AzureWire {
    /// The request URL, through the shared endpoint trust boundary.
    ///
    /// # Errors
    ///
    /// The [`http::endpoint`] failures.
    pub(crate) fn endpoint(&self) -> Result<Url, ProviderError> {
        http::endpoint(self.family, &self.base_url, &self.path)
    }

    /// Attaches the headers and body to a `POST` of `url`.
    pub(crate) fn into_request(
        self,
        client: &reqwest::Client,
        url: Url,
    ) -> reqwest::RequestBuilder {
        let mut request = client.post(url);
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        request.body(self.body)
    }
}

impl fmt::Debug for AzureWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AzureWire")
            .field("family", &self.family)
            .field("base_url", &self.base_url)
            .field("path", &self.path)
            .field("headers", &"<redacted>")
            .field("body_len", &self.body.len())
            .field("model", &self.model)
            .field("deployment", &self.deployment)
            .finish_non_exhaustive()
    }
}

/// Lowers one request to its Azure endpoint, headers, and body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for a route outside `openai_chat` and
/// `openai_responses`, an OAuth credential, a Chat deployment name that is not
/// one URL segment, an unparsable base URL, or a body the family builder
/// rejects; [`ProviderError::NoCredentials`] without a credential.
pub(crate) fn build(input: &AzureRequest<'_>) -> Result<AzureWire, ProviderError> {
    let ModelRoute::Api { family, model } = &input.request.model else {
        return Err(invalid("an Azure request needs an API model route"));
    };
    let key = match input.credential {
        Credential::ApiKey { key } => key.expose(),
        Credential::OAuth(_) => {
            return Err(invalid(format!(
                "{} needs an API key, not an OAuth sign-in",
                input.provider
            )));
        }
        Credential::None => {
            return Err(ProviderError::NoCredentials {
                provider: input.provider.to_owned(),
            });
        }
    };
    let deployment = input.settings.deployments.deployment(model);
    let version = input.settings.api_version.as_deref();
    let request = repair_pairing(input.request);
    let thinking = azure_thinking(&request, input.thinking);
    let (body, base_url, path) = match family {
        Family::Responses => {
            let body = responses::request_body(&request, thinking.wire, input.reasoning_summary)?;
            let version = encode(version.unwrap_or(RESPONSES_API_VERSION));
            let path = format!("{}?api-version={version}", responses::PATH);
            (body, responses_base(input.base_url)?, path)
        }
        Family::Chat => {
            let (base_url, path) = chat_target(
                input.base_url,
                deployment,
                version.unwrap_or(CHAT_API_VERSION),
            )?;
            let body = chat::request_body(&request, &thinking)
                .map_err(|error| invalid(error.to_string()))?;
            (body, base_url, path)
        }
        // Every other family is named, so a new `Family` variant breaks this
        // match and forces a decision instead of falling through to Azure.
        Family::Codex
        | Family::Anthropic
        | Family::Bedrock
        | Family::Cursor
        | Family::Devin
        | Family::Droid
        | Family::GitLab
        | Family::GoogleGenerativeAi
        | Family::GoogleVertex
        | Family::GoogleGeminiCli
        | Family::GoogleAntigravity
        | Family::Mistral
        | Family::Ollama
        | Family::PiMessages => {
            return Err(invalid(
                "Azure OpenAI serves the openai_chat and openai_responses families",
            ));
        }
    };
    Ok(AzureWire {
        family: *family,
        base_url,
        path,
        headers: vec![(API_KEY_HEADER, key.to_owned())],
        body: rewrite_model(body, model, deployment)?,
        secret: key.into(),
        model: model.clone(),
        deployment: deployment.into(),
    })
}

/// Replaces the value of the leading `"model"` member of a family body.
///
/// Every family body writer emits `model` first, so the member is found by its
/// fixed prefix. The old value must be exactly `from`: a body whose model is
/// not the route's is a builder bug and fails instead of being rewritten.
/// `from == to` returns `body` untouched.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the body does not start with its
/// `model` member or that member is not `from`.
pub(crate) fn rewrite_model(body: Vec<u8>, from: &str, to: &str) -> Result<Vec<u8>, ProviderError> {
    if from == to {
        return Ok(body);
    }
    let start = MODEL_MEMBER.len();
    let mut end = start + 1;
    if !body.starts_with(MODEL_MEMBER) || body.get(start) != Some(&b'"') {
        return Err(invalid(
            "a family body does not start with its model member",
        ));
    }
    while let Some(&byte) = body.get(end) {
        match byte {
            b'\\' => end += 2,
            b'"' => break,
            _ => end += 1,
        }
    }
    let old = sonic_rs::to_string(from).map_err(|error| invalid(error.to_string()))?;
    if body.get(end) != Some(&b'"') || body[start..=end] != *old.as_bytes() {
        return Err(invalid(format!(
            "a family body names a model other than {from:?}"
        )));
    }
    let new = sonic_rs::to_string(to).map_err(|error| invalid(error.to_string()))?;
    let mut out = Vec::with_capacity(body.len() + new.len());
    out.extend_from_slice(MODEL_MEMBER);
    out.extend_from_slice(new.as_bytes());
    out.extend_from_slice(&body[end + 1..]);
    Ok(out)
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn parse_base(base: &str) -> Result<Url, ProviderError> {
    Url::parse(base.trim())
        .map_err(|error| invalid(format!("azure base url {base:?} is not a URL: {error}")))
}

fn base_string(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

fn is_azure_host(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        AZURE_HOST_SUFFIXES
            .iter()
            .any(|suffix| host.ends_with(suffix))
    })
}

/// The Responses base: an Azure host with no path, `/openai`, or a pasted
/// `/openai/v1/responses` becomes `/openai/v1`; any other base is kept.
fn responses_base(base: &str) -> Result<String, ProviderError> {
    let mut url = parse_base(base)?;
    let is_bare = matches!(
        url.path().trim_end_matches('/'),
        "" | "/openai" | "/openai/v1/responses"
    );
    if is_azure_host(&url) && is_bare {
        url.set_path("/openai/v1");
        url.set_query(None);
    }
    Ok(base_string(&url))
}

/// The Chat base and path. A base that already names `/deployments/<name>`
/// keeps it; otherwise the deployment segment is added on the `/openai` base.
fn chat_target(
    base: &str,
    deployment: &str,
    api_version: &str,
) -> Result<(String, String), ProviderError> {
    let mut url = parse_base(base)?;
    let version = encode(api_version);
    let path = url.path().trim_end_matches('/').to_owned();
    if path.contains("/deployments/") {
        return Ok((
            base_string(&url),
            format!("{}?api-version={version}", chat::ENDPOINT_PATH),
        ));
    }
    check_deployment(deployment)?;
    if is_azure_host(&url) && matches!(path.as_str(), "" | "/openai" | "/openai/v1") {
        url.set_path("/openai");
        url.set_query(None);
    }
    Ok((
        base_string(&url),
        format!(
            "deployments/{deployment}/{}?api-version={version}",
            chat::ENDPOINT_PATH
        ),
    ))
}

fn check_deployment(name: &str) -> Result<(), ProviderError> {
    let segment = !matches!(name, "" | "." | "..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if segment {
        Ok(())
    } else {
        Err(invalid(format!(
            "azure deployment name {name:?} must be one path segment of letters, digits, '-', '_', or '.'"
        )))
    }
}

#[cfg(test)]
mod tests {
    use dal_core::{ModelToolSpec, Purpose, RawJson, RequestParams, ThinkingLevel, Usage};
    use futures::{StreamExt, executor::block_on, stream};

    use super::*;
    use crate::{
        auth::credential::{OAuthCredential, SecretString},
        sse::decode_stream,
        stream::{ReplayPayload, StopReason, StreamEvent, ToolArgs, ToolCall},
    };

    const RESOURCE_BASE: &str = "https://res.openai.azure.com/openai/v1";

    /// Every `Family` variant Azure does not serve.
    const NON_AZURE_FAMILIES: [Family; 14] = [
        Family::Codex,
        Family::Anthropic,
        Family::Bedrock,
        Family::Cursor,
        Family::Devin,
        Family::Droid,
        Family::GitLab,
        Family::GoogleGenerativeAi,
        Family::GoogleVertex,
        Family::GoogleGeminiCli,
        Family::GoogleAntigravity,
        Family::Mistral,
        Family::Ollama,
        Family::PiMessages,
    ];

    fn request(family: Family, model: &str, context: Vec<ContextItem>) -> ModelRequest {
        ModelRequest {
            purpose: Purpose::Turn,
            model: ModelRoute::Api {
                family,
                model: model.into(),
            },
            system: "sys".into(),
            tools: Vec::<ModelToolSpec>::new().into(),
            context: context.into(),
            params: RequestParams {
                thinking: ThinkingLevel::High,
                effort: None,
                temperature: None,
                max_output_tokens: None,
            },
            cache_key: Some("sess-1".into()),
        }
    }

    fn user(text: &str) -> ContextItem {
        ContextItem::User {
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    fn plan() -> ThinkingPlan {
        ThinkingPlan {
            level: ThinkingLevel::High,
            wire: WireThinking::OpenAi {
                effort: Some("high"),
            },
            temperature: None,
            notices: Vec::new(),
        }
    }

    fn api_key() -> Credential {
        Credential::ApiKey {
            key: SecretString::from("az-secret"),
        }
    }

    fn lower(
        request: &ModelRequest,
        base_url: &str,
        settings: &AzureSettings,
        credential: &Credential,
    ) -> Result<AzureWire, ProviderError> {
        build(&AzureRequest {
            request,
            thinking: &plan(),
            reasoning_summary: false,
            provider: "azure",
            base_url,
            settings,
            credential,
        })
    }

    fn body_text(wire: AzureWire) -> String {
        String::from_utf8(wire.body).unwrap()
    }

    #[test]
    fn responses_wire_posts_to_the_v1_path_with_an_api_key_header() {
        let request = request(Family::Responses, "gpt-5.5", vec![user("hi")]);
        let wire = lower(
            &request,
            RESOURCE_BASE,
            &AzureSettings::default(),
            &api_key(),
        )
        .unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://res.openai.azure.com/openai/v1/responses?api-version=v1"
        );
        assert_eq!(wire.headers, vec![("api-key", String::from("az-secret"))]);
        assert_eq!(wire.family, Family::Responses);
        let body = body_text(wire);
        assert!(
            body.starts_with(r#"{"model":"gpt-5.5","instructions":"sys","input":["#),
            "{body}"
        );
    }

    #[test]
    fn chat_wire_uses_the_deployments_surface_and_normalises_a_v1_base() {
        let settings = AzureSettings {
            api_version: None,
            deployments: DeploymentMap::parse("gpt-5.5=prod-gpt55"),
        };
        let request = request(Family::Chat, "gpt-5.5", vec![user("hi")]);
        let wire = lower(&request, RESOURCE_BASE, &settings, &api_key()).unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://res.openai.azure.com/openai/deployments/prod-gpt55/chat/completions?api-version=2024-10-21"
        );
        assert_eq!(wire.deployment.as_ref(), "prod-gpt55");
        assert_eq!(wire.model.as_ref(), "gpt-5.5");
        let body = body_text(wire);
        assert!(
            body.starts_with(r#"{"model":"prod-gpt55","messages":["#),
            "{body}"
        );
    }

    #[test]
    fn a_chat_base_that_names_its_deployment_keeps_it() {
        let request = request(Family::Chat, "gpt-5.5", vec![user("hi")]);
        let wire = lower(
            &request,
            "https://res.openai.azure.com/openai/deployments/fixed/",
            &AzureSettings::default(),
            &api_key(),
        )
        .unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://res.openai.azure.com/openai/deployments/fixed/chat/completions?api-version=2024-10-21"
        );
    }

    #[test]
    fn a_custom_gateway_base_keeps_its_path_for_both_surfaces() {
        let settings = AzureSettings::default();
        let chat = request(Family::Chat, "gpt-5.5", vec![user("hi")]);
        let wire = lower(
            &chat,
            "https://gw.example.com/azure/",
            &settings,
            &api_key(),
        )
        .unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://gw.example.com/azure/deployments/gpt-5.5/chat/completions?api-version=2024-10-21"
        );
        let responses = request(Family::Responses, "gpt-5.5", vec![user("hi")]);
        let wire = lower(
            &responses,
            "https://gw.example.com/azure/",
            &settings,
            &api_key(),
        )
        .unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://gw.example.com/azure/responses?api-version=v1"
        );
    }

    #[test]
    fn an_explicit_api_version_wins_on_both_surfaces_and_is_encoded() {
        let settings = AzureSettings {
            api_version: Some("2025-03-01-preview".into()),
            deployments: DeploymentMap::default(),
        };
        let responses = request(Family::Responses, "gpt-5.5", vec![user("hi")]);
        let wire = lower(&responses, RESOURCE_BASE, &settings, &api_key()).unwrap();
        assert_eq!(wire.path, "/responses?api-version=2025-03-01-preview");
        let chat = request(Family::Chat, "gpt-5.5", vec![user("hi")]);
        let wire = lower(&chat, RESOURCE_BASE, &settings, &api_key()).unwrap();
        assert!(
            wire.path.ends_with("?api-version=2025-03-01-preview"),
            "{}",
            wire.path
        );
        assert_eq!(encode("a&b"), "a%26b");
    }

    #[test]
    fn the_deployment_goes_on_the_wire_but_replay_stays_bound_to_the_catalog_model() {
        let item = r#"{"type":"reasoning","id":"rs_1","encrypted_content":"abc"}"#;
        let context = vec![
            user("hi"),
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Responses,
                    model: "gpt-5.5".into(),
                },
                replay: None,
                parts: vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        replay: Some(RawJson::parse(item).unwrap()),
                    },
                    AssistantPart::Text {
                        text: "ok".into(),
                        replay: None,
                    },
                ],
            },
            user("more"),
        ];
        let settings = AzureSettings {
            api_version: None,
            deployments: DeploymentMap::parse("gpt-5.5=prod-gpt55"),
        };
        let request = request(Family::Responses, "gpt-5.5", context);
        let wire = lower(&request, RESOURCE_BASE, &settings, &api_key()).unwrap();
        assert_eq!(wire.model.as_ref(), "gpt-5.5");
        let body = body_text(wire);
        assert!(body.starts_with(r#"{"model":"prod-gpt55","#), "{body}");
        assert!(
            body.contains(item),
            "the replayed reasoning item survived: {body}"
        );
        assert!(!body.contains(r#""model":"gpt-5.5""#));
    }

    #[test]
    fn a_responses_deployment_may_be_any_string_but_a_chat_one_must_be_a_segment() {
        let settings = AzureSettings {
            api_version: None,
            deployments: DeploymentMap::parse("m=a/b"),
        };
        let responses = request(Family::Responses, "m", vec![user("hi")]);
        let wire = lower(&responses, RESOURCE_BASE, &settings, &api_key()).unwrap();
        assert!(body_text(wire).starts_with(r#"{"model":"a/b","#));
        let chat = request(Family::Chat, "m", vec![user("hi")]);
        let error = lower(&chat, RESOURCE_BASE, &settings, &api_key()).unwrap_err();
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn only_an_api_key_authenticates() {
        let request = request(Family::Responses, "gpt-5.5", vec![user("hi")]);
        let settings = AzureSettings::default();
        let oauth = Credential::OAuth(OAuthCredential {
            access_token: SecretString::from("a"),
            refresh_token: SecretString::from("r"),
            expires_at: None,
            id_token: None,
            account_id: None,
        });
        let error = lower(&request, RESOURCE_BASE, &settings, &oauth).unwrap_err();
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{error:?}"
        );
        let error = lower(&request, RESOURCE_BASE, &settings, &Credential::None).unwrap_err();
        assert!(
            matches!(&error, ProviderError::NoCredentials { provider } if provider == "azure"),
            "{error:?}"
        );
    }

    #[test]
    fn every_family_but_chat_and_responses_is_refused() {
        for family in NON_AZURE_FAMILIES {
            let request = request(family, "m", vec![user("hi")]);
            let error = lower(
                &request,
                RESOURCE_BASE,
                &AzureSettings::default(),
                &api_key(),
            )
            .unwrap_err();
            assert!(
                matches!(error, ProviderError::InvalidRequest { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn the_wire_debug_output_hides_the_key() {
        let request = request(Family::Responses, "gpt-5.5", vec![user("hi")]);
        let wire = lower(
            &request,
            RESOURCE_BASE,
            &AzureSettings::default(),
            &api_key(),
        )
        .unwrap();
        let shown = format!("{wire:?}");
        assert!(!shown.contains("az-secret"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn the_deployment_map_parses_the_env_spelling_leniently() {
        let map = DeploymentMap::parse(" a = b ,, bad , =x, y= ,a=c,m=d=e");
        assert_eq!(map.deployment("a"), "c");
        assert_eq!(map.deployment("y"), "y");
        assert_eq!(map.deployment("bad"), "bad");
        assert_eq!(map.deployment("m"), "m");
        assert_eq!(map.deployment("unmapped"), "unmapped");
    }

    #[test]
    fn a_resource_name_must_be_one_dns_label() {
        assert_eq!(
            base_url_for_resource(" my-res ").unwrap(),
            "https://my-res.openai.azure.com/openai/v1"
        );
        let too_long = "a".repeat(64);
        for bad in [
            "",
            "a.b",
            "-x",
            "x-",
            "x/y",
            "x@evil.com",
            too_long.as_str(),
        ] {
            assert!(base_url_for_resource(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_responses_base_normalises_azure_hosts_only() {
        for (base, expected) in [
            (
                "https://r.openai.azure.com",
                "https://r.openai.azure.com/openai/v1",
            ),
            (
                "https://r.openai.azure.com/openai",
                "https://r.openai.azure.com/openai/v1",
            ),
            (
                "https://r.openai.azure.com/openai/v1/responses",
                "https://r.openai.azure.com/openai/v1",
            ),
            (
                "https://r.cognitiveservices.azure.com/?x=1",
                "https://r.cognitiveservices.azure.com/openai/v1",
            ),
            (
                "https://gw.example.com/azure/",
                "https://gw.example.com/azure",
            ),
        ] {
            assert_eq!(responses_base(base).unwrap(), expected, "{base}");
        }
        assert!(responses_base("not a url").is_err());
    }

    #[test]
    fn rewrite_model_splices_only_the_leading_member() {
        let nested = || br#"{"model":"gpt-5.5","x":{"model":"gpt-5.5"}}"#.to_vec();
        assert_eq!(
            rewrite_model(nested(), "gpt-5.5", "prod").unwrap(),
            br#"{"model":"prod","x":{"model":"gpt-5.5"}}"#.to_vec()
        );
        assert_eq!(
            rewrite_model(nested(), "gpt-5.5", "gpt-5.5").unwrap(),
            nested()
        );
        let escaped = br#"{"model":"we\"ird","x":1}"#.to_vec();
        assert_eq!(
            rewrite_model(escaped, "we\"ird", "p\\q").unwrap(),
            br#"{"model":"p\\q","x":1}"#.to_vec()
        );
        assert!(rewrite_model(nested(), "other", "prod").is_err());
        assert!(rewrite_model(br#"{"x":1}"#.to_vec(), "m", "prod").is_err());
        assert!(rewrite_model(br#"{"model":"unterminated"#.to_vec(), "m", "p").is_err());
    }

    #[test]
    fn both_family_bodies_start_with_their_model_member() {
        // `rewrite_model` depends on this member order; pin it per family.
        for (family, lead) in [
            (Family::Chat, r#"{"model":"m","messages":["#),
            (Family::Responses, r#"{"model":"m","instructions":"#),
        ] {
            let request = request(family, "m", vec![user("hi")]);
            let wire = lower(
                &request,
                RESOURCE_BASE,
                &AzureSettings::default(),
                &api_key(),
            )
            .unwrap();
            let body = body_text(wire);
            assert!(body.starts_with(lead), "{family:?}: {body}");
        }
    }

    fn tool(name: &str) -> ModelToolSpec {
        ModelToolSpec {
            name: name.into(),
            description: "d".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).unwrap(),
            grammar: None,
            defer_loading: false,
        }
    }

    fn with_tools(mut request: ModelRequest, tools: Vec<ModelToolSpec>) -> ModelRequest {
        request.tools = tools.into();
        request
    }

    fn call(id: &str, name: &str) -> AssistantPart {
        AssistantPart::ToolCall {
            call: CallId::new(id),
            name: name.into(),
            args: RawJson::parse("{}").unwrap(),
            replay: None,
        }
    }

    fn assistant(parts: Vec<AssistantPart>) -> ContextItem {
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Responses,
                model: "gpt-5.5".into(),
            },
            replay: None,
            parts,
        }
    }

    fn result(id: &str, text: &str) -> ContextItem {
        ContextItem::ToolResult {
            call: CallId::new(id),
            name: "read".into(),
            is_error: false,
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    #[test]
    fn a_paired_context_is_borrowed_unchanged() {
        let context = vec![
            user("hi"),
            assistant(vec![call("c1", "read")]),
            result("c1", "ok"),
        ];
        let request = request(Family::Responses, "gpt-5.5", context);
        assert!(matches!(repair_pairing(&request), Cow::Borrowed(_)));
    }

    #[test]
    fn an_orphan_call_gets_an_error_result_right_after_its_assistant_item() {
        let context = vec![
            user("hi"),
            assistant(vec![call("c1", "read"), call("c2", "read")]),
            result("c2", "ok"),
            user("again"),
        ];
        let request = request(Family::Responses, "gpt-5.5", context);
        let repaired = repair_pairing(&request);
        assert_eq!(
            request.context.len(),
            4,
            "the caller's request is untouched"
        );
        assert_eq!(repaired.context.len(), 5);
        assert!(
            matches!(&repaired.context[2], ContextItem::ToolResult { call, is_error: true, parts, .. }
                if call.as_str() == "c1" && result_text(parts) == ORPHAN_CALL_PLACEHOLDER),
            "{:?}",
            repaired.context[2]
        );
        assert!(
            matches!(&repaired.context[3], ContextItem::ToolResult { call, is_error: false, .. }
                if call.as_str() == "c2")
        );
    }

    #[test]
    fn an_orphan_result_becomes_an_assistant_note_and_a_late_result_does_not_pair() {
        let request = request(
            Family::Responses,
            "gpt-5.5",
            vec![user("hi"), result("ghost", "line1\nline2")],
        );
        let repaired = repair_pairing(&request);
        assert_eq!(repaired.context.len(), 2);
        assert!(
            matches!(&repaired.context[1], ContextItem::Assistant { parts, .. }
                if matches!(parts.as_slice(), [AssistantPart::Text { text, .. }]
                    if &**text == "[Orphan read result; call_id=ghost]: line1\nline2"))
        );

        // A result that comes before its call answers nothing: the result is an
        // orphan, and the call is one too.
        let early = self::request(
            Family::Responses,
            "gpt-5.5",
            vec![result("c1", "early"), assistant(vec![call("c1", "read")])],
        );
        let repaired = repair_pairing(&early);
        assert_eq!(repaired.context.len(), 3);
        assert!(matches!(
            &repaired.context[0],
            ContextItem::Assistant { .. }
        ));
        assert!(matches!(
            &repaired.context[1],
            ContextItem::Assistant { .. }
        ));
        assert!(matches!(
            &repaired.context[2],
            ContextItem::ToolResult { is_error: true, .. }
        ));
    }

    #[test]
    fn a_long_orphan_output_is_cut_on_a_character_boundary() {
        let long = "\u{e9}".repeat(ORPHAN_OUTPUT_LIMIT + 1);
        let request = request(Family::Responses, "gpt-5.5", vec![result("ghost", &long)]);
        let repaired = repair_pairing(&request);
        let ContextItem::Assistant { parts, .. } = &repaired.context[0] else {
            panic!(
                "the orphan result was not folded: {:?}",
                repaired.context[0]
            );
        };
        let [AssistantPart::Text { text, .. }] = parts.as_slice() else {
            panic!("the note is one text part: {parts:?}");
        };
        assert_eq!(text.matches('\u{e9}').count(), ORPHAN_OUTPUT_LIMIT);
        assert!(
            text.ends_with(ORPHAN_OUTPUT_CUT),
            "the cut note does not end with the cut marker"
        );
    }

    #[test]
    fn both_surfaces_send_repaired_exchanges_never_unpaired_ones() {
        let context = vec![
            user("hi"),
            assistant(vec![call("c1", "read")]),
            result("ghost", "stray"),
        ];
        let settings = AzureSettings::default();

        let responses = request(Family::Responses, "gpt-5.5", context.clone());
        let body = body_text(lower(&responses, RESOURCE_BASE, &settings, &api_key()).unwrap());
        assert!(
            body.contains(r#""type":"function_call_output","call_id":"c1""#),
            "{body}"
        );
        assert!(body.contains(ORPHAN_CALL_PLACEHOLDER), "{body}");
        assert!(!body.contains(r#""call_id":"ghost""#), "{body}");
        assert!(
            body.contains("[Orphan read result; call_id=ghost]"),
            "{body}"
        );

        let chat = request(Family::Chat, "gpt-5.5", context);
        let body = body_text(lower(&chat, RESOURCE_BASE, &settings, &api_key()).unwrap());
        assert!(
            body.contains(r#""role":"tool","tool_call_id":"c1""#),
            "{body}"
        );
        assert!(!body.contains(r#""tool_call_id":"ghost""#), "{body}");
        assert!(
            body.contains("[Orphan read result; call_id=ghost]"),
            "{body}"
        );
    }

    #[test]
    fn astra_on_chat_with_tools_sends_the_explicit_none_effort() {
        let settings = AzureSettings::default();
        let effort_in = |request: &ModelRequest| {
            body_text(lower(request, RESOURCE_BASE, &settings, &api_key()).unwrap())
        };
        let plain = || request(Family::Chat, "gpt-6-astra", vec![user("hi")]);

        let with = with_tools(plain(), vec![tool("read")]);
        assert!(effort_in(&with).contains(r#""reasoning_effort":"none""#));
        assert!(effort_in(&plain()).contains(r#""reasoning_effort":"high""#));
        let other = with_tools(
            request(Family::Chat, "gpt-5.5", vec![user("hi")]),
            vec![tool("read")],
        );
        assert!(effort_in(&other).contains(r#""reasoning_effort":"high""#));
        let responses = with_tools(
            request(Family::Responses, "gpt-6-astra", vec![user("hi")]),
            vec![tool("read")],
        );
        assert!(effort_in(&responses).contains(r#""reasoning":{"effort":"high"}"#));

        // An omitted effort is not safe either: the server default reasons.
        let omitted = ThinkingPlan {
            wire: WireThinking::OpenAi { effort: None },
            ..plan()
        };
        assert_eq!(
            azure_thinking(&with, &omitted).wire,
            WireThinking::OpenAi {
                effort: Some("none")
            }
        );
        let already = ThinkingPlan {
            wire: WireThinking::OpenAi {
                effort: Some("none"),
            },
            ..plan()
        };
        assert!(matches!(azure_thinking(&with, &already), Cow::Borrowed(_)));
    }

    #[test]
    fn azure_refuses_the_websocket_transport() {
        assert!(require_https("azure", Transport::Https).is_ok());
        let error = require_https("github-copilot", Transport::Websocket).unwrap_err();
        assert!(
            matches!(&error, ProviderError::InvalidRequest { message } if message.contains("github-copilot")),
            "{error:?}"
        );
    }

    #[test]
    fn the_config_wins_over_the_environment_model_by_model() {
        let env = EnvSnapshot::test(&[
            (API_VERSION_ENV, " 2024-06-01 "),
            (DEPLOYMENT_MAP_ENV, "a=env-a,m=env-m"),
        ]);
        let from_env = AzureSettings::from_env(&env);
        assert_eq!(from_env.api_version.as_deref(), Some("2024-06-01"));

        // Direct typed construction: the obsolete TOML decoder is gone, and
        // the merge contract is pinned without weakening any expectation.
        // The dotted model id the old decoder test covered moves to the
        // central strict decoder test, where TOML quoting is decoded.
        let config = AzureSettings {
            api_version: None,
            deployments: DeploymentMap::parse("m=cfg-m"),
        };
        let merged = config.over(from_env);
        assert_eq!(
            merged.api_version.as_deref(),
            Some("2024-06-01"),
            "no config version: env stays"
        );
        assert_eq!(merged.deployments.deployment("m"), "cfg-m");
        assert_eq!(merged.deployments.deployment("a"), "env-a");

        let config = AzureSettings {
            api_version: Some("v1".into()),
            deployments: DeploymentMap::default(),
        };
        assert_eq!(
            config
                .over(AzureSettings::from_env(&env))
                .api_version
                .as_deref(),
            Some("v1")
        );
        assert_eq!(
            AzureSettings::from_env(&EnvSnapshot::test(&[])),
            AzureSettings::default()
        );
    }

    #[test]
    fn the_environment_names_a_base_url_or_a_resource() {
        let none = EnvSnapshot::test(&[(BASE_URL_ENV, "  ")]);
        assert_eq!(base_url_from_env("azure", &none).unwrap(), None);
        let resource = EnvSnapshot::test(&[(RESOURCE_NAME_ENV, "my-res")]);
        assert_eq!(
            base_url_from_env("azure", &resource).unwrap().as_deref(),
            Some("https://my-res.openai.azure.com/openai/v1")
        );
        let both = EnvSnapshot::test(&[
            (RESOURCE_NAME_ENV, "my-res"),
            (BASE_URL_ENV, " https://gw.example.com/azure "),
        ]);
        assert_eq!(
            base_url_from_env("azure", &both).unwrap().as_deref(),
            Some("https://gw.example.com/azure")
        );
        let bad = EnvSnapshot::test(&[(RESOURCE_NAME_ENV, "evil.example/x")]);
        let error = base_url_from_env("azure", &bad).unwrap_err();
        assert!(
            matches!(&error, ProviderError::InvalidRequest { message }
                if message.contains("azure")
                    && message.contains(RESOURCE_NAME_ENV)
                    && message.contains("evil.example/x")),
            "{error:?}"
        );
    }

    fn sse(data: &[&str]) -> Vec<u8> {
        let mut wire = String::new();
        for chunk in data {
            wire.push_str("data: ");
            wire.push_str(chunk);
            wire.push_str("\n\n");
        }
        wire.into_bytes()
    }

    fn decode_chat(data: &[&str]) -> Vec<Result<StreamEvent, ProviderError>> {
        let chunks: Vec<Vec<u8>> = sse(data).chunks(11).map(<[u8]>::to_vec).collect();
        block_on(
            chat::decode_events(
                decode_stream(stream::iter(chunks)),
                chat::ChatDecoder::new("azure", "gpt-5.5"),
            )
            .collect(),
        )
    }

    fn decode_responses(model: &str, data: &[&str]) -> Vec<Result<StreamEvent, ProviderError>> {
        let chunks: Vec<Vec<u8>> = sse(data).chunks(13).map(<[u8]>::to_vec).collect();
        block_on(
            responses::decode(
                decode_stream(stream::iter(chunks)),
                Family::Responses,
                model,
            )
            .collect(),
        )
    }

    fn events(items: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
        items.into_iter().map(|item| item.unwrap()).collect()
    }

    #[test]
    fn an_azure_chat_tool_turn_decodes_with_filter_noise_and_a_trailing_usage_chunk() {
        let items = decode_chat(&[
            r#"{"choices":[],"created":0,"id":"","model":"","object":"","prompt_filter_results":[{"prompt_index":0,"content_filter_results":{"hate":{"filtered":false,"severity":"safe"}}}]}"#,
            r#"{"choices":[{"content_filter_results":{},"delta":{"content":"","role":"assistant","refusal":null},"finish_reason":null,"index":0}],"id":"chatcmpl-1","object":"chat.completion.chunk"}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"","name":"read"},"id":"call_AbC123","index":0,"type":"function"}]},"finish_reason":null,"index":0}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{\"path\":\"a\"}"},"index":0}]},"finish_reason":null,"index":0}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls","index":0}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":4},"completion_tokens_details":{"reasoning_tokens":2}}}"#,
            "[DONE]",
        ]);
        assert_eq!(
            events(items),
            vec![
                StreamEvent::ToolCallStarted {
                    id: "call_AbC123".into(),
                    name: "read".into()
                },
                StreamEvent::ToolArgsDelta {
                    id: "call_AbC123".into(),
                    fragment: br#"{"path":"a"}"#.to_vec()
                },
                StreamEvent::ToolCallsDone {
                    calls: vec![ToolCall {
                        id: "call_AbC123".into(),
                        name: "read".into(),
                        args: ToolArgs::from_bytes(br#"{"path":"a"}"#),
                    }]
                },
                StreamEvent::Usage {
                    usage: Usage {
                        input_tokens: 12,
                        cached_input_tokens: 4,
                        output_tokens: 3,
                        reasoning_tokens: Some(2),
                        cache_write_tokens: 0,
                        premium_requests: None,
                        cost_usd: None,
                    }
                },
                StreamEvent::Stop {
                    reason: StopReason::ToolUse
                },
            ]
        );
    }

    #[test]
    fn an_azure_content_filter_finish_is_a_refusal() {
        let seen = events(decode_chat(&[
            r#"{"choices":[{"delta":{"content":"par"},"finish_reason":null,"index":0}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"content_filter","index":0}]}"#,
            "[DONE]",
        ]));
        assert_eq!(seen[0], StreamEvent::TextDelta { text: "par".into() });
        assert_eq!(
            seen.last(),
            Some(&StreamEvent::Stop {
                reason: StopReason::Refusal
            })
        );
    }

    #[test]
    fn an_azure_rate_limit_inside_the_stream_is_a_retryable_error_not_a_stop() {
        let items = decode_chat(&[
            r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#,
            r#"{"choices":[{"delta":{"content":"late"},"finish_reason":"stop","index":0}]}"#,
        ]);
        assert_eq!(items.len(), 1, "nothing follows the error: {items:?}");
        assert!(
            matches!(&items[0], Err(ProviderError::RateLimited { message, .. }) if message == "slow down"),
            "{items:?}"
        );
    }

    #[test]
    fn an_azure_responses_turn_keeps_the_call_id_and_binds_replay_to_the_catalog_model() {
        let reasoning =
            r#"{"type":"reasoning","id":"rs_1","encrypted_content":"ENC","summary":[]}"#;
        let added = r#"{"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"type":"function_call","id":"fc_0abc","call_id":"call_Z9","name":"read","arguments":""}}"#;
        let done = r#"{"type":"response.output_item.done","sequence_number":4,"output_index":0,"item":{"type":"function_call","id":"fc_0abc","call_id":"call_Z9","name":"read","arguments":"{\"path\":\"a\"}"}}"#;
        let reasoning_done = format!(
            r#"{{"type":"response.output_item.done","sequence_number":5,"output_index":1,"item":{reasoning}}}"#
        );
        let completed = format!(
            r#"{{"type":"response.completed","sequence_number":6,"response":{{"output":[{{"type":"function_call","id":"fc_0abc","call_id":"call_Z9","name":"read","arguments":"{{\"path\":\"a\"}}"}},{reasoning}],"usage":{{"input_tokens":20,"input_tokens_details":{{"cached_tokens":8}},"output_tokens":9,"output_tokens_details":{{"reasoning_tokens":4}}}}}}}}"#
        );

        // The deployment differs from the catalog id; the decoder gets the
        // catalog id from the wire, so replay stays bound to it.
        let settings = AzureSettings {
            api_version: None,
            deployments: DeploymentMap::parse("gpt-5.5=prod-gpt55"),
        };
        let wire = lower(
            &request(Family::Responses, "gpt-5.5", vec![user("hi")]),
            RESOURCE_BASE,
            &settings,
            &api_key(),
        )
        .unwrap();
        assert_ne!(wire.model, wire.deployment);

        let seen = events(decode_responses(
            &wire.model,
            &[
                r#"{"type":"response.in_progress","sequence_number":0,"response":{"id":"resp_1"}}"#,
                added,
                r#"{"type":"response.function_call_arguments.delta","sequence_number":2,"item_id":"fc_0abc","delta":"{\"path\":"}"#,
                r#"{"type":"response.function_call_arguments.delta","sequence_number":3,"item_id":"fc_0abc","delta":"\"a\"}"}"#,
                done,
                reasoning_done.as_str(),
                completed.as_str(),
            ],
        ));
        assert_eq!(
            seen,
            vec![
                StreamEvent::ToolCallStarted {
                    id: "call_Z9".into(),
                    name: "read".into()
                },
                StreamEvent::ToolArgsDelta {
                    id: "call_Z9".into(),
                    fragment: br#"{"path":"#.to_vec()
                },
                StreamEvent::ToolArgsDelta {
                    id: "call_Z9".into(),
                    fragment: br#""a"}"#.to_vec()
                },
                StreamEvent::Replay {
                    payload: ReplayPayload {
                        family: Family::Responses,
                        model: "gpt-5.5".into(),
                        item: RawJson::parse(reasoning).unwrap(),
                    }
                },
                StreamEvent::ToolCallsDone {
                    calls: vec![ToolCall {
                        id: "call_Z9".into(),
                        name: "read".into(),
                        args: ToolArgs::from_bytes(br#"{"path":"a"}"#),
                    }]
                },
                StreamEvent::Usage {
                    usage: Usage {
                        input_tokens: 20,
                        cached_input_tokens: 8,
                        output_tokens: 5,
                        reasoning_tokens: Some(4),
                        cache_write_tokens: 0,
                        premium_requests: None,
                        cost_usd: None,
                    }
                },
                StreamEvent::Stop {
                    reason: StopReason::ToolUse
                },
            ]
        );
    }
}
