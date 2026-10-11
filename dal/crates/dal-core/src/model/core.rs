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
            };
        };
        match prefix {
            "dalgon"
                if matches!(
                    id,
                    "dalgon/normal" | "dalgon/eval-first" | "dalgon/eval-only"
                ) =>
            {
                Self::Harness { id: id.into() }
            }
            "openai-chat" => Self::Api {
                family: Family::Chat,
                model: model.into(),
            },
            "openai-responses" => Self::Api {
                family: Family::Responses,
                model: model.into(),
            },
            "openai-codex" => Self::Api {
                family: Family::Codex,
                model: model.into(),
            },
            "anthropic" => Self::Api {
                family: Family::Anthropic,
                model: model.into(),
            },
            _ if Self::is_valid_synthetic_id(id) => Self::Synthetic { id: id.into() },
            _ => Self::Api {
                family: Family::Chat,
                model: id.into(),
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
        if !matches!(
            id.as_ref(),
            "dalgon/normal" | "dalgon/eval-first" | "dalgon/eval-only"
        ) {
            return Err(RouteError::InvalidHarnessId { id });
        }
        Ok(Self::Harness { id })
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
                Ok(Self::Api {
                    family: fields.family,
                    model: fields.model,
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
