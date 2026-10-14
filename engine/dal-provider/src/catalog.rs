//! Provider model lists, reference resolution, and the checked-in price snapshot.

use std::path::Path;

use dal_core::{Family, ModelRoute, ThinkingLevel};

use crate::{
    auth::credential::Credential, error::ProviderError, provider::ProviderEntry,
    thinking::ThinkingSupport,
};

mod builtin;
mod cache;
mod decode;
mod fetch;
mod prices;
mod resolve;

pub use builtin::built_in_entries;
pub use fetch::load_models;
pub use prices::{compiled_price, compiled_temperature, price_source};
pub use resolve::{resolve, resolve_route};

/// One complete catalog-owned image budget for a model.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageProfile {
    /// Number of glyph cells across a full image frame.
    pub cols: u16,
    /// Number of glyph cells down a full image frame.
    pub rows: u16,
    /// Horizontal glyph-cell pitch in pixels.
    pub cell_w: u8,
    /// Vertical glyph-cell pitch in pixels.
    pub cell_h: u8,
    /// Maximum image parts accepted in one request.
    pub max_images: usize,
    /// Closed-form token bill for one full-frame image.
    pub image_tokens: u64,
}

impl ImageProfile {
    /// Anthropic high-resolution frame and billing profile.
    #[must_use]
    pub const fn anthropic_high_resolution(window: u32) -> Self {
        Self {
            cols: 175,
            rows: 120,
            cell_w: 11,
            cell_h: 16,
            max_images: if window <= 200_000 { 100 } else { 600 },
            image_tokens: 4_761,
        }
    }

    /// Anthropic standard-resolution frame and billing profile.
    #[must_use]
    pub const fn anthropic_standard(window: u32) -> Self {
        Self {
            cols: 142,
            rows: 49,
            cell_w: 11,
            cell_h: 16,
            max_images: if window <= 200_000 { 100 } else { 600 },
            image_tokens: 1_568,
        }
    }

    /// `OpenAI` image frame and billing profile.
    #[must_use]
    pub const fn openai() -> Self {
        Self {
            cols: 256,
            rows: 56,
            cell_w: 8,
            cell_h: 22,
            max_images: 1_500,
            image_tokens: 2_996,
        }
    }
}

/// One provider model and the capabilities known for it.
#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent model capability flags map one-to-one onto provider metadata"
)]
pub struct CatalogEntry {
    /// The provider id used in model references.
    pub provider: Box<str>,
    /// The provider-native model id.
    pub id: Box<str>,
    /// The display name supplied by the provider or built-in table.
    pub display: Box<str>,
    /// Whether the provider's model list exposes this model.
    pub listing: Listing,
    /// Known input-context limit, in tokens.
    pub context_window: Option<u32>,
    /// Known output limit, in tokens.
    pub max_output: Option<u32>,
    /// Supported reasoning shape for the provider family.
    pub thinking: ThinkingSupport,
    /// Whether the model accepts image input.
    pub image_input: bool,
    /// Complete image frame and billing data; absent means image billing is unknown.
    pub image_profile: Option<ImageProfile>,
    /// Whether the model supports the family's remote-compaction endpoint.
    pub remote_compact: bool,
    /// Whether the Codex catalog reports reasoning-summary support.
    pub supports_reasoning_summaries: bool,
    /// Provider tool routes permitted by the model's catalog capabilities.
    pub tool_support: ToolSupport,
    /// Whether verified model metadata permits sending a temperature parameter.
    pub temperature_allowed: bool,
    /// Whether the provider model metadata confirms adaptive thinking display support.
    pub display_supported: bool,
    /// Whether verified provider metadata allows constrained custom-grammar tools.
    pub custom_grammar: bool,
}

/// Visibility in a provider model listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Listing {
    /// The provider makes the model visible in its model list.
    Listed,
    /// The model is routable but hidden from ordinary listings.
    Hidden,
}

/// Model-level tool capability constrained by the provider API family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolSupport {
    /// The model does not accept tools on any supported route.
    None,
    /// The model accepts tools on any supported route.
    Any,
    /// The model accepts tools through Responses-style routes only.
    ResponsesOnly,
    /// Responses-style routes accept tools; Chat accepts them only with
    /// reasoning disabled.
    ChatWhenNoReasoning,
}

impl ToolSupport {
    /// Reports whether tools are supported for one resolved route and level.
    #[must_use]
    pub const fn allows(self, family: Family, thinking: ThinkingLevel) -> bool {
        match self {
            Self::None => false,
            Self::Any => true,
            Self::ResponsesOnly => matches!(family, Family::Responses | Family::Codex),
            Self::ChatWhenNoReasoning => match family {
                Family::Chat => matches!(thinking, ThinkingLevel::Off),
                Family::Responses | Family::Codex => true,
                Family::Anthropic => false,
            },
        }
    }
}

/// A reference resolved to its provider route and catalog metadata.
#[derive(Clone, Debug)]
pub struct ResolvedModel {
    /// The provider id used in the reference.
    pub provider: Box<str>,
    /// The API or built-in harness route.
    pub route: ModelRoute,
    /// The selected model row, or its typed-id fallback.
    pub entry: CatalogEntry,
}

/// The provider model catalog used by resolution.
#[derive(Clone, Debug)]
pub struct Catalog {
    providers: Vec<(ProviderEntry, CatalogSource)>,
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// Builds a catalog with an explicit load source for every configured provider.
    ///
    /// `entries` contains fetched or cached rows; proven built-in rows are
    /// added unless a provider/id row is already present.
    #[must_use]
    pub fn with_sources(
        providers: Vec<(ProviderEntry, CatalogSource)>,
        mut entries: Vec<CatalogEntry>,
    ) -> Self {
        for builtin in built_in_entries() {
            if !entries
                .iter()
                .any(|entry| entry.provider == builtin.provider && entry.id == builtin.id)
            {
                entries.push(builtin);
            }
        }
        Self { providers, entries }
    }

    /// Returns the rows available for resolution, including built-ins.
    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    fn source_for(&self, provider: &str) -> CatalogSource {
        self.providers
            .iter()
            .find(|(candidate, _)| candidate.id.as_ref() == provider)
            .map_or(CatalogSource::Typed, |(_, source)| *source)
    }
}

/// Identifies how a model list was obtained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogSource {
    /// A provider request succeeded; updating the cache was attempted.
    Live,
    /// The provider request failed and a valid on-disk cache was used.
    Cache,
    /// Neither a live list nor usable cached rows were available; resolution
    /// can still create typed ids for configured providers.
    Typed,
}

/// Rows and fallback evidence returned by [`load_models`].
#[derive(Debug)]
#[must_use]
pub struct CatalogFetch {
    /// Rows for the requested provider from the live list or its cache; empty
    /// when `source` is `Typed`.
    pub entries: Vec<CatalogEntry>,
    /// The source selected by the live/cache/typed fallback sequence.
    pub source: CatalogSource,
    /// The live request or decode error, when live loading failed.
    pub live_error: Option<ProviderError>,
    /// A cache write/read failure, when one occurred.
    pub cache_error: Option<Box<str>>,
    /// Provider whose live list was requested and whose cache fallback was read.
    pub provider: Box<str>,
}

/// Inputs for one provider model-list refresh.
#[derive(Debug)]
pub struct ModelFetch<'a> {
    /// The host's HTTP client.
    pub client: &'a reqwest::Client,
    /// The configured provider and its family/base URL.
    pub provider: &'a ProviderEntry,
    /// The credential to use for the model-list request.
    pub credential: &'a Credential,
    /// The dal cache directory; `models.json` is created inside it.
    pub cache_dir: &'a Path,
    /// The per-request user-agent value.
    pub user_agent: &'a str,
    /// The client version sent to the Codex catalog endpoint.
    pub version: &'a str,
}

#[cfg(test)]
mod tests;
