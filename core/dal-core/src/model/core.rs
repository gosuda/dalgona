use super::{Deserialize, Deserializer, InferFailure, Serialize, Tagged, de};

/// The provider API family. These wire names differ from the variant names.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Family {
    /// Chat completions API.
    #[serde(rename = "openai_chat")]
    Chat,
    /// Responses API.
    #[serde(rename = "openai_responses")]
    Responses,
    /// Codex API.
    #[serde(rename = "openai_codex")]
    Codex,
    /// Anthropic Messages.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// AWS Bedrock Converse API.
    #[serde(rename = "bedrock")]
    Bedrock,
    /// Cursor agent API over Connect framing.
    #[serde(rename = "cursor")]
    Cursor,
    /// Devin API over Connect framing.
    #[serde(rename = "devin")]
    Devin,
    /// Droid API.
    #[serde(rename = "droid")]
    Droid,
    /// GitLab Duo API.
    #[serde(rename = "gitlab")]
    GitLab,
    /// Google Generative Language API.
    #[serde(rename = "google-generative-ai")]
    GoogleGenerativeAi,
    /// Google Vertex AI API.
    #[serde(rename = "google-vertex")]
    GoogleVertex,
    /// Google Gemini CLI API: the local `gemini` subprocess, which holds no
    /// dal credential.
    #[serde(rename = "google-gemini-cli")]
    GoogleGeminiCli,
    /// Google Cloud Code Assist API: the native HTTPS stream, authorized by a
    /// Google OAuth credential that names a Cloud project. It is a separate
    /// family from the local [`Family::GoogleGeminiCli`] subprocess, so a
    /// stored `google-gemini-cli` record keeps its meaning.
    #[serde(rename = "google-cloud-code-assist")]
    GoogleCloudCodeAssist,
    /// Google Antigravity API.
    #[serde(rename = "google-antigravity")]
    GoogleAntigravity,
    /// Mistral conversations API.
    #[serde(rename = "mistral")]
    Mistral,
    /// Ollama chat API.
    #[serde(rename = "ollama")]
    Ollama,
    /// The pi-messages gateway protocol: one JSON POST, then an SSE stream of
    /// serialized assistant-message events.
    #[serde(rename = "pi-messages")]
    PiMessages,
}

impl Family {
    /// Every family, in declaration order.
    pub const ALL: [Self; 17] = [
        Self::Chat,
        Self::Responses,
        Self::Codex,
        Self::Anthropic,
        Self::Bedrock,
        Self::Cursor,
        Self::Devin,
        Self::Droid,
        Self::GitLab,
        Self::GoogleGenerativeAi,
        Self::GoogleVertex,
        Self::GoogleGeminiCli,
        Self::GoogleCloudCodeAssist,
        Self::GoogleAntigravity,
        Self::Mistral,
        Self::Ollama,
        Self::PiMessages,
    ];

    /// The prefix of a family-qualified model reference, as in
    /// `openai-chat/<model>`.
    #[must_use]
    pub const fn route_prefix(self) -> &'static str {
        match self {
            Self::Chat => "openai-chat",
            Self::Responses => "openai-responses",
            Self::Codex => "openai-codex",
            Self::Anthropic => "anthropic",
            Self::Bedrock => "bedrock",
            Self::Cursor => "cursor",
            Self::Devin => "devin",
            Self::Droid => "droid",
            Self::GitLab => "gitlab",
            Self::GoogleGenerativeAi => "google-generative-ai",
            Self::GoogleVertex => "google-vertex",
            Self::GoogleGeminiCli => "google-gemini-cli",
            Self::GoogleCloudCodeAssist => "google-cloud-code-assist",
            Self::GoogleAntigravity => "google-antigravity",
            Self::Mistral => "mistral",
            Self::Ollama => "ollama",
            Self::PiMessages => "radius",
        }
    }

    /// The family whose [`route_prefix`](Self::route_prefix) is `prefix`.
    #[must_use]
    pub fn from_route_prefix(prefix: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|family| family.route_prefix() == prefix)
    }
}

/// The provider API family and exact model that produced an assistant message.
///
/// Replay adapters use this source to decide whether opaque reasoning payloads
/// can be sent to the current provider route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReplaySource {
    /// Provider API family that produced the message.
    pub family: Family,
    /// Provider-native model name that produced the message; never empty.
    #[cfg_attr(feature = "schema", schemars(length(min = 1)))]
    pub model: Box<str>,
}

#[derive(Deserialize)]
pub(super) struct ReplaySourceFields {
    pub(super) family: Family,
    pub(super) model: Box<str>,
}

impl<'de> Deserialize<'de> for ReplaySource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = ReplaySourceFields::deserialize(deserializer)?;
        if fields.model.is_empty() {
            return Err(de::Error::custom("replay source model must not be empty"));
        }
        Ok(Self {
            family: fields.family,
            model: fields.model,
        })
    }
}

/// Requested reasoning intensity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    /// Disable reasoning.
    Off,
    /// Minimal reasoning.
    Minimal,
    /// Low reasoning.
    Low,
    /// Medium reasoning.
    Medium,
    /// High reasoning.
    High,
    /// Extra-high reasoning.
    Xhigh,
    /// Maximum supported reasoning.
    Max,
}

impl ThinkingLevel {
    /// Returns the canonical spelling of this reasoning level.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// A direct API model, a named synthetic model, or a built-in harness mode.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelRoute {
    /// Route directly to a provider API family and model name.
    Api {
        /// API family selected by the route.
        family: Family,
        /// Provider-native model name.
        model: Box<str>,
        /// The configured provider id the route resolves under, when the
        /// route carries that identity. `None` keeps the pre-provider
        /// semantics: the request re-resolves the bare model id through
        /// aliases, the catalog, and the family retry. `Some(provider)`
        /// resolves `provider/model` exactly, skips synthetic-first lookup
        /// for that id, and never re-binds to another same-family provider.
        /// Absent in older payloads and read as `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<Box<str>>,
    },
    /// A composed model identified by `namespace/name`.
    Synthetic {
        /// Identifier matching `[a-z0-9-]+/[a-z0-9._-]+`.
        id: Box<str>,
    },
    /// One of the three built-in dalgon modes.
    Harness {
        /// `dalgon/normal`, `dalgon/eval-first`, or `dalgon/eval-only`.
        id: Box<str>,
    },
}

/// A synthetic or harness route id does not match its closed grammar.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RouteError {
    /// The synthetic identifier must have one slash and valid components.
    #[error("invalid synthetic model id `{id}`: expected `[a-z0-9-]+/[a-z0-9._-]+`")]
    InvalidSyntheticId {
        /// The rejected identifier.
        id: Box<str>,
    },
    /// Harness routes are limited to the three dalgon modes.
    #[error(
        "invalid harness model id `{id}`: expected dalgon/normal, dalgon/eval-first, or dalgon/eval-only"
    )]
    InvalidHarnessId {
        /// The rejected identifier.
        id: Box<str>,
    },
}

impl ModelRoute {
    /// Returns the provider model name or the synthetic/harness identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Api { model, .. } => model,
            Self::Synthetic { id } | Self::Harness { id } => id,
        }
    }

    /// Returns the configured provider id the route names, when it carries
    /// one. Synthetic and harness routes never carry a provider: their id's
    /// namespace is not a configured provider.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        match self {
            Self::Api { provider, .. } => provider.as_deref(),
            Self::Synthetic { .. } | Self::Harness { .. } => None,
        }
    }

    /// Whether `other` names the same model as this route.
    ///
    /// Two API routes name the same model when family and native id agree
    /// and, if *both* name a provider, the providers agree too. A route that
    /// names no provider (an older record, an old client, a measurement
    /// journaled before providers travelled on routes) has no provider to
    /// disagree with. Every other pairing compares exactly. Use this for
    /// continuity checks across records; use `==` where an exact selection
    /// must be told apart.
    #[must_use]
    pub fn same_model(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Api {
                    family: left_family,
                    model: left_model,
                    provider: left_provider,
                },
                Self::Api {
                    family: right_family,
                    model: right_model,
                    provider: right_provider,
                },
            ) => {
                left_family == right_family
                    && left_model == right_model
                    && match (left_provider, right_provider) {
                        (Some(left), Some(right)) => left == right,
                        _ => true,
                    }
            }
            _ => self == other,
        }
    }

    /// Returns the route with `provider` set, leaving every other member as
    /// it is. Synthetic and harness routes are returned unchanged.
    #[must_use]
    pub fn with_provider(&self, provider: &str) -> Self {
        match self {
            Self::Api {
                family,
                model,
                provider: _,
            } => Self::Api {
                family: *family,
                model: model.clone(),
                provider: Some(provider.into()),
            },
            other => other.clone(),
        }
    }

    /// Renders this route as the `family/id` reference persisted to `dal.toml`.
    ///
    /// API families use their persisted provider prefix (`openai-chat`,
    /// `openai-responses`, `openai-codex`, `anthropic`, `bedrock`, `cursor`,
    /// `devin`, `droid`, `gitlab`, `google-generative-ai`, `google-vertex`,
    /// `google-gemini-cli`, `google-cloud-code-assist`, `google-antigravity`,
    /// `mistral`, `ollama`,
    /// `radius` for [`Family::PiMessages`]); synthetic and harness routes
    /// render as their bare id. This is the legacy `model` string mirror:
    /// best-effort for old readers, never the authoritative decode. The
    /// authoritative form is the structured `model_route` table. The mirror
    /// never names the route's provider: its prefix is the API family, which
    /// cannot say which same-family provider the route belongs to. Note the
    /// intentional divergence from the UI `route_origin` spelling, which
    /// renders [`Family::Responses`] as `openai`: persistence uses
    /// `openai-responses` so old readers decode the family correctly.
    #[must_use]
    pub fn family_qualified_reference(&self) -> String {
        match self {
            Self::Api { family, model, .. } => format!("{}/{model}", family.route_prefix()),
            Self::Synthetic { id } | Self::Harness { id } => id.to_string(),
        }
    }

    /// Builds a provider route from a canonical model identifier.
    ///
    /// Known provider prefixes select their API family. Harness and synthetic
    /// identifiers keep their distinct route forms. Other ids are left for
    /// the provider catalog to resolve as Chat-family model ids.
    #[must_use]
    pub fn from_id(id: &str) -> Self {
        let Some((prefix, model)) = id.split_once('/') else {
            return Self::Api {
                family: Family::Chat,
                model: id.into(),
                provider: None,
            };
        };
        match prefix {
            "dalgon" if Self::is_valid_harness_id(id) => Self::Harness { id: id.into() },
            "openai-chat" => Self::Api {
                family: Family::Chat,
                model: model.into(),
                provider: None,
            },
            "openai-responses" => Self::Api {
                family: Family::Responses,
                model: model.into(),
                provider: None,
            },
            "openai-codex" => Self::Api {
                family: Family::Codex,
                model: model.into(),
                provider: None,
            },
            "anthropic" => Self::Api {
                family: Family::Anthropic,
                model: model.into(),
                provider: None,
            },
            _ if Self::is_valid_synthetic_id(id) => Self::Synthetic { id: id.into() },
            _ => Self::Api {
                family: Family::Chat,
                model: id.into(),
                provider: None,
            },
        }
    }

    /// Builds a synthetic route after checking its `namespace/name` grammar.
    ///
    /// # Errors
    /// Returns [`RouteError::InvalidSyntheticId`] for any other spelling.
    pub fn synthetic(id: impl Into<Box<str>>) -> Result<Self, RouteError> {
        let id = id.into();
        if !Self::is_valid_synthetic_id(&id) {
            return Err(RouteError::InvalidSyntheticId { id });
        }
        Ok(Self::Synthetic { id })
    }

    /// Builds one of the three built-in harness routes.
    ///
    /// # Errors
    /// Returns [`RouteError::InvalidHarnessId`] for an unknown mode.
    pub fn harness(id: impl Into<Box<str>>) -> Result<Self, RouteError> {
        let id = id.into();
        if !Self::is_valid_harness_id(&id) {
            return Err(RouteError::InvalidHarnessId { id });
        }
        Ok(Self::Harness { id })
    }

    /// Checks the closed harness grammar
    /// `dalgon/{normal,eval-first,eval-only}` without allocation.
    #[must_use]
    pub(crate) fn is_valid_harness_id(id: &str) -> bool {
        matches!(
            id,
            "dalgon/normal" | "dalgon/eval-first" | "dalgon/eval-only"
        )
    }

    /// Checks the grammar `[a-z0-9-]+/[a-z0-9._-]+` without alias lookup.
    #[must_use]
    pub fn is_valid_synthetic_id(id: &str) -> bool {
        let Some((namespace, name)) = id.split_once('/') else {
            return false;
        };
        !namespace.is_empty()
            && namespace
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !name.is_empty()
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    }
}

#[derive(Deserialize)]
pub(super) struct ApiRouteFields {
    pub(super) family: Family,
    pub(super) model: Box<str>,
    #[serde(default, deserialize_with = "provider_field")]
    pub(super) provider: Option<Box<str>>,
}

/// Reads the optional `provider`. `null` is malformed, not absent: only a
/// missing member means the route names no provider, as in the journal.
fn provider_field<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Box<str>>, D::Error> {
    Box::<str>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
pub(super) struct IdRouteFields {
    pub(super) id: Box<str>,
}

impl<'de> Deserialize<'de> for ModelRoute {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "kind", &["api", "synthetic", "harness"])?;
        match tagged.kind() {
            "api" => {
                let fields: ApiRouteFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                if fields.provider.as_deref().is_some_and(str::is_empty) {
                    return Err(de::Error::custom(
                        "model route `provider` must not be empty",
                    ));
                }
                Ok(Self::Api {
                    family: fields.family,
                    model: fields.model,
                    provider: fields.provider,
                })
            }
            "synthetic" => {
                let fields: IdRouteFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Self::synthetic(fields.id).map_err(de::Error::custom)
            }
            "harness" => {
                let fields: IdRouteFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Self::harness(fields.id).map_err(de::Error::custom)
            }
            other => Err(de::Error::custom(format!(
                "unknown model route kind `{other}`"
            ))),
        }
    }
}

/// Maximum number of routes on a synthetic expansion path.
pub const MAX_SYNTHETIC_DEPTH: usize = 4;

/// Rejects a resolution path longer than four or a repeated synthetic id.
///
/// The input is the complete path as currently known to the runtime. A path
/// longer than four routes is rejected before checking for repeated ids.
/// This check does no provider lookup or alias resolution.
///
/// # Errors
/// Returns [`InferFailure::SyntheticDepth`] beyond four routes, or
/// [`InferFailure::SyntheticCycle`] for a repeated synthetic id in an admitted
/// path.
pub fn check_synthetic_chain(chain: &[ModelRoute]) -> Result<(), InferFailure> {
    if chain.len() > MAX_SYNTHETIC_DEPTH {
        return Err(InferFailure::SyntheticDepth {
            chain: chain.to_vec(),
        });
    }
    for (index, route) in chain.iter().enumerate() {
        let ModelRoute::Synthetic { id } = route else {
            continue;
        };
        let repeated = chain[..index].iter().any(|previous| {
            matches!(previous, ModelRoute::Synthetic { id: previous_id } if previous_id == id)
        });
        if repeated {
            return Err(InferFailure::SyntheticCycle {
                chain: chain.to_vec(),
            });
        }
    }
    Ok(())
}
