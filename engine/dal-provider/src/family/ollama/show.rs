//! Ollama model discovery beyond `api/tags`: the `api/show` capability probe,
//! the cloud-host rules, and the per-model thinking ladders.
//!
//! `api/tags` lists ids and nothing else. `POST <base>/api/show` with
//! `{"model": <id>}` answers per model with the `capabilities` it serves
//! (`completion`, `tools`, `thinking`, `vision`, ...), a `model_info`
//! object whose `<architecture>.context_length` member is the trained context
//! window, and, on current servers, a `thinking` object whose `values` list
//! the exact `think` values the model accepts: strings such as `"low"` or
//! `"max"`, and booleans (`false` turns reasoning off, `true` turns it on for
//! a model without levels). [`decode_show`] reads exactly those three
//! members; every other member is ignored, and a body without them is an
//! empty probe, never an error: the catalog keeps the row it already has and
//! invents no limit. Wire sources: the pinned oh-my-pi reference
//! (`packages/catalog/src/provider-models/ollama.ts` and `openai-compat.ts`)
//! and senpi's `providers/ollama.ts`, both read on disk, and the live
//! `https://ollama.com/api/show` replies recorded in this module's tests.
//!
//! Ollama Cloud (`https://ollama.com`) speaks the same wire with three
//! differences the rows carry: `/api/chat` rejects `options.num_predict`
//! above [`CLOUD_NUM_PREDICT_CAP`] with HTTP 400 for every cloud model, the
//! `DeepSeek` V4 Pro and Flash deployments cap their output at the same value
//! while advertising a far larger window (`/api/show` never reports it), and
//! several cloud models reject assistant `thinking` in replayed history.
//! [`Deployment::of`] decides which deployment a provider row is.

use dal_core::{Family, ThinkingLevel};
use serde::Serialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use crate::{error::ProviderError, thinking::ThinkingSupport};

/// The model-probe endpoint path, relative to the provider base URL.
pub(crate) const SHOW_PATH: &str = "api/show";

/// The provider id of the built-in Ollama Cloud row.
pub(crate) const CLOUD_PROVIDER_ID: &str = "ollama-cloud";

/// The output-token ceiling Ollama Cloud enforces: `/api/chat` answers
/// `max_tokens (...) exceeds model's maximum output tokens (65536)` with
/// HTTP 400 above it.
pub(crate) const CLOUD_NUM_PREDICT_CAP: u32 = 65_536;

/// The `model_info` key suffixes that carry a context window, in the order
/// the pinned source tries them.
const CONTEXT_SUFFIXES: [&str; 3] = [".context_length", ".num_ctx", ".context_window"];

/// Untagged base ids whose Ollama Cloud deployment enforces
/// [`CLOUD_NUM_PREDICT_CAP`] below their advertised window. Only these two are
/// known to cap output; every other cloud model keeps what it discovers.
const CLOUD_OUTPUT_CAPPED: [&str; 2] = ["deepseek-v4-flash", "deepseek-v4-pro"];

/// Which Ollama deployment answers a provider row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Deployment {
    /// A local or self-hosted server: no output ceiling, thinking replays.
    SelfHosted,
    /// Ollama Cloud: the output ceiling and the thinking-history rule apply.
    Cloud,
}

impl Deployment {
    /// The deployment of `provider` at `base_url`.
    ///
    /// The built-in cloud row id is cloud whatever its base URL says, so a
    /// proxy configured on that row keeps the cloud rules; any other row is
    /// cloud when its host is `ollama.com` or a subdomain of it.
    #[must_use]
    pub(crate) fn of(provider: &str, base_url: &str) -> Self {
        let cloud_host = url::Url::parse(base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .is_some_and(|host| host == "ollama.com" || host.ends_with(".ollama.com"));
        if provider == CLOUD_PROVIDER_ID || cloud_host {
            Self::Cloud
        } else {
            Self::SelfHosted
        }
    }
}

/// The model id without its `:tag`, lowercased.
fn base_id(model: &str) -> String {
    model
        .split_once(':')
        .map_or(model, |(base, _)| base)
        .to_ascii_lowercase()
}

/// Whether the cloud deployment of `model` enforces the output cap below its
/// advertised window.
#[must_use]
pub(crate) fn cloud_output_capped(model: &str) -> bool {
    let base = base_id(model);
    CLOUD_OUTPUT_CAPPED.contains(&base.as_str())
}

/// The reasoning levels a thinking-capable model accepts.
///
/// `api/show` reports only whether a model thinks, never a tier vocabulary,
/// so the ladder comes from the model id exactly like the reference rules:
/// GPT-OSS keeps its Harmony `low`/`medium`/`high`; `DeepSeek` V4 keeps
/// `low`/`high`/`max`; on the cloud, GLM 5.2 takes `high`/`max`, GLM 5.3 takes
/// `low`/`high`/`max`, and any other cloud model takes the four-step default
/// whose `minimal` spells `low` on the wire; any other self-hosted model takes
/// `low`/`medium`/`high`/`max`.
#[must_use]
pub(crate) fn thinking_ladder(deployment: Deployment, model: &str) -> &'static [ThinkingLevel] {
    use ThinkingLevel::{High, Low, Max, Medium, Minimal};
    let base = base_id(model);
    if base.starts_with("gpt-oss") {
        return &[Low, Medium, High];
    }
    if base.starts_with("deepseek-v4") {
        return &[Low, High, Max];
    }
    match deployment {
        Deployment::Cloud if base == "glm-5.2" => &[High, Max],
        Deployment::Cloud if base.starts_with("glm-5.3") => &[Low, High, Max],
        Deployment::Cloud => &[Minimal, Low, Medium, High],
        Deployment::SelfHosted => &[Low, Medium, High, Max],
    }
}

/// The one level the ladder of a boolean-only model carries.
///
/// Some models reason but take no level: their `thinking.values` is
/// `[false, true]` or `[true]`, and the wire spelling of "on" is `think:
/// true`. The planner only knows levels, so such a model's ladder is exactly
/// this level, and [`is_boolean_ladder`] reads that back at request time to
/// spell `true`. Ollama never lists `minimal` as a think value, which is why
/// the level cannot clash with a real ladder.
pub(crate) const BOOLEAN_ON: ThinkingLevel = ThinkingLevel::Minimal;

/// Whether `support` is the ladder of a boolean-only model.
#[must_use]
pub(crate) fn is_boolean_ladder(support: &ThinkingSupport) -> bool {
    matches!(
        support,
        ThinkingSupport::OpenAi { accepted, .. } if accepted.as_slice() == [BOOLEAN_ON]
    )
}

/// The `think` values one model accepts, from `api/show`'s `thinking.values`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ThinkValues {
    /// The named levels, in the server's order.
    pub(crate) levels: Vec<ThinkingLevel>,
    /// Whether `false` is accepted: reasoning can be turned off.
    pub(crate) off: bool,
    /// Whether `true` is accepted.
    pub(crate) on: bool,
}

impl ThinkValues {
    /// The ladder these values give the planner, and whether the `none`
    /// spelling (`think: false`) is accepted. Named levels win; a model with
    /// only `true` carries the [`BOOLEAN_ON`] ladder.
    #[must_use]
    pub(crate) fn ladder(&self) -> (Vec<ThinkingLevel>, bool) {
        let accepted = if !self.levels.is_empty() {
            self.levels.clone()
        } else if self.on {
            vec![BOOLEAN_ON]
        } else {
            Vec::new()
        };
        (accepted, self.off)
    }
}

/// Whether the cloud deployment of `model` takes assistant `thinking` back in
/// replayed history. Every cloud model rejects it except GLM 5.2, the one
/// verified to take it.
#[must_use]
pub(crate) fn cloud_replays_thinking(model: &str) -> bool {
    matches!(
        model.to_ascii_lowercase().as_str(),
        "glm-5.2" | "glm-5.2:cloud"
    )
}

#[derive(Serialize)]
struct ShowRequest<'a> {
    model: &'a str,
}

/// The body of `POST <base>/api/show` for `model`.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the JSON writer fails.
pub(crate) fn show_body(model: &str) -> Result<Vec<u8>, ProviderError> {
    sonic_rs::to_vec(&ShowRequest { model }).map_err(|error| ProviderError::InvalidRequest {
        message: format!("ollama show body encoding failed: {error}"),
    })
}

/// What one `api/show` probe reports about a model.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OllamaShow {
    /// The reported capabilities; `None` when the body names none, which
    /// leaves every capability unknown rather than absent.
    pub(crate) capabilities: Option<Vec<String>>,
    /// The trained context window in tokens, when `model_info` names one.
    pub(crate) context_window: Option<u32>,
    /// The accepted `think` values, when the server reports them.
    pub(crate) thinking: Option<ThinkValues>,
}

impl OllamaShow {
    /// Whether the model reports `capability`; `None` when the probe named no
    /// capabilities at all.
    #[must_use]
    pub(crate) fn reports(&self, capability: &str) -> Option<bool> {
        self.capabilities
            .as_ref()
            .map(|list| list.iter().any(|item| item == capability))
    }
}

fn protocol(detail: String) -> ProviderError {
    ProviderError::Protocol {
        family: Family::Ollama,
        detail,
    }
}

/// Decodes a complete `POST api/show` body.
///
/// `capabilities` keeps its string members; a non-array value counts as
/// absent. The context window is the `<general.architecture>.context_length`
/// member when `model_info` has it, else the first member whose key ends in
/// `.context_length`, `.num_ctx`, or `.context_window`; only a positive
/// integer that fits `u32` counts.
///
/// # Errors
///
/// [`ProviderError::Protocol`] when the body is not one JSON object.
pub(crate) fn decode_show(bytes: &[u8]) -> Result<OllamaShow, ProviderError> {
    let body: sonic_rs::Value = sonic_rs::from_slice(bytes)
        .map_err(|error| protocol(format!("ollama show body is not valid JSON: {error}")))?;
    let object = body
        .as_object()
        .ok_or_else(|| protocol(String::from("ollama show body is not a JSON object")))?;
    let capabilities = object
        .get(&"capabilities")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        });
    let context_window = object.get(&"model_info").and_then(context_window);
    let thinking = object.get(&"thinking").and_then(think_values);
    Ok(OllamaShow {
        capabilities,
        context_window,
        thinking,
    })
}

/// Reads `thinking.values`; a string that names no level (`none`, a future
/// spelling) is skipped, and a body without a `values` array is no report.
fn think_values(thinking: &sonic_rs::Value) -> Option<ThinkValues> {
    let items = thinking.as_object()?.get(&"values")?.as_array()?;
    let mut values = ThinkValues::default();
    for item in items {
        if let Some(flag) = item.as_bool() {
            *(if flag {
                &mut values.on
            } else {
                &mut values.off
            }) = true;
            continue;
        }
        let level = match item.as_str() {
            Some("minimal") => ThinkingLevel::Minimal,
            Some("low") => ThinkingLevel::Low,
            Some("medium") => ThinkingLevel::Medium,
            Some("high") => ThinkingLevel::High,
            Some("xhigh") => ThinkingLevel::Xhigh,
            Some("max") => ThinkingLevel::Max,
            _ => continue,
        };
        if !values.levels.contains(&level) {
            values.levels.push(level);
        }
    }
    Some(values)
}

fn positive_window(value: &sonic_rs::Value) -> Option<u32> {
    value
        .as_u64()
        .filter(|tokens| *tokens > 0)
        .and_then(|tokens| u32::try_from(tokens).ok())
}

fn context_window(info: &sonic_rs::Value) -> Option<u32> {
    let object = info.as_object()?;
    if let Some(architecture) = object
        .get(&"general.architecture")
        .and_then(|value| value.as_str())
        && let Some(window) = object
            .get(&format!("{architecture}.context_length"))
            .and_then(positive_window)
    {
        return Some(window);
    }
    object.iter().find_map(|(key, value)| {
        CONTEXT_SUFFIXES
            .iter()
            .any(|suffix| key.ends_with(suffix))
            .then(|| positive_window(value))
            .flatten()
    })
}

#[cfg(test)]
mod tests;
