//! Atomic `models.json` cache behind the live model lists.
//!
//! Writes go through the store part's atomic writer under an advisory file
//! lock; reads tolerate older shapes through defaulted members.

use std::path::{Path, PathBuf};

use dal_core::ThinkingLevel;
use serde::{Deserialize, Serialize};

use super::decode::parse_anthropic_effort;
use super::{CatalogEntry, ImageProfile, Listing, ToolSupport, prices::compiled_temperature};
use crate::thinking::ThinkingSupport;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CacheError {
    #[error("cache I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("models cache JSON is invalid: {0}")]
    Json(#[from] sonic_rs::Error),
    #[error("models cache could not be serialized: {0}")]
    Serialize(sonic_rs::Error),
    #[error("models cache version {0} is not supported")]
    Version(u32),
    #[error("{0}")]
    Invalid(Box<str>),
    #[error(transparent)]
    Store(#[from] dal_store::StoreError),
    #[error("cache task failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}

fn read_cache(path: &Path) -> Result<Vec<CatalogEntry>, CacheError> {
    let bytes = std::fs::read(path)?;
    let cache: CachedCatalog = sonic_rs::from_slice(&bytes)?;
    if cache.version != CACHE_VERSION {
        return Err(CacheError::Version(cache.version));
    }
    cache
        .entries
        .into_iter()
        .map(CachedEntry::into_catalog_entry)
        .collect()
}

pub(crate) async fn read_cache_async(path: PathBuf) -> Result<Vec<CatalogEntry>, CacheError> {
    tokio::task::spawn_blocking(move || read_cache(&path)).await?
}

fn encode_cache(entries: &[CatalogEntry]) -> Result<Vec<u8>, CacheError> {
    sonic_rs::to_vec(&CachedCatalog {
        version: CACHE_VERSION,
        entries: entries.iter().map(CachedEntry::from).collect(),
    })
    .map_err(CacheError::Serialize)
}

pub(crate) async fn write_cache_async(
    path: PathBuf,
    provider: Box<str>,
    entries: Vec<CatalogEntry>,
) -> Result<(), CacheError> {
    tokio::task::spawn_blocking(move || write_cache(&path, &provider, entries)).await?
}

fn write_cache(
    path: &Path,
    provider: &str,
    fresh_rows: Vec<CatalogEntry>,
) -> Result<(), CacheError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_file_name("models.json.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock_file.lock()?;

    let (mut entries, read_error) = match read_cache(path) {
        Ok(entries) => (entries, None),
        Err(CacheError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            (Vec::new(), None)
        }
        Err(error) => (Vec::new(), Some(error)),
    };
    entries.retain(|entry| entry.provider.as_ref() != provider);
    entries.extend(fresh_rows);
    let bytes = encode_cache(&entries)?;
    dal_store::write_atomic(path, &bytes, dal_store::FileMode::Mode0600)?;
    if let Some(error) = read_error {
        return Err(error);
    }
    Ok(())
}

const CACHE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CachedCatalog {
    version: u32,
    entries: Vec<CachedEntry>,
}

#[derive(Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "cache row mirrors CatalogEntry flags one-to-one to keep models.json stable"
)]
struct CachedEntry {
    provider: String,
    id: String,
    display: String,
    hidden: bool,
    context_window: Option<u32>,
    max_output: Option<u32>,
    thinking: CachedThinking,
    image_input: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_profile: Option<ImageProfile>,
    #[serde(default)]
    custom_grammar: bool,
    remote_compact: bool,
    #[serde(default)]
    supports_reasoning_summaries: bool,
    #[serde(default)]
    tool_support: CachedToolSupport,
    #[serde(default)]
    temperature_allowed: Option<bool>,
    #[serde(default)]
    display_supported: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CachedThinking {
    OpenAi {
        accepted: Vec<ThinkingLevel>,
        none_supported: bool,
    },
    Adaptive {
        can_disable: bool,
        #[serde(default)]
        accepted: Vec<String>,
    },
    Budget {
        can_disable: bool,
    },
    UnknownAdaptive,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CachedToolSupport {
    None,
    #[default]
    Any,
    ResponsesOnly,
    ChatWhenNoReasoning,
}

impl From<ToolSupport> for CachedToolSupport {
    fn from(support: ToolSupport) -> Self {
        match support {
            ToolSupport::None => Self::None,
            ToolSupport::Any => Self::Any,
            ToolSupport::ResponsesOnly => Self::ResponsesOnly,
            ToolSupport::ChatWhenNoReasoning => Self::ChatWhenNoReasoning,
        }
    }
}

impl From<CachedToolSupport> for ToolSupport {
    fn from(support: CachedToolSupport) -> Self {
        match support {
            CachedToolSupport::None => Self::None,
            CachedToolSupport::Any => Self::Any,
            CachedToolSupport::ResponsesOnly => Self::ResponsesOnly,
            CachedToolSupport::ChatWhenNoReasoning => Self::ChatWhenNoReasoning,
        }
    }
}
impl From<&CatalogEntry> for CachedEntry {
    fn from(entry: &CatalogEntry) -> Self {
        let thinking = match &entry.thinking {
            ThinkingSupport::OpenAi {
                accepted,
                none_supported,
            } => CachedThinking::OpenAi {
                accepted: accepted.clone(),
                none_supported: *none_supported,
            },
            ThinkingSupport::Adaptive {
                can_disable,
                accepted,
            } => CachedThinking::Adaptive {
                can_disable: *can_disable,
                accepted: accepted
                    .iter()
                    .map(|effort| effort.as_str().to_owned())
                    .collect(),
            },
            ThinkingSupport::Budget { can_disable } => CachedThinking::Budget {
                can_disable: *can_disable,
            },
            ThinkingSupport::UnknownAdaptive => CachedThinking::UnknownAdaptive,
        };
        Self {
            provider: entry.provider.to_string(),
            id: entry.id.to_string(),
            display: entry.display.to_string(),
            hidden: entry.listing == Listing::Hidden,
            context_window: entry.context_window,
            max_output: entry.max_output,
            thinking,
            image_input: entry.image_input,
            image_profile: entry.image_profile,
            remote_compact: entry.remote_compact,
            supports_reasoning_summaries: entry.supports_reasoning_summaries,
            custom_grammar: entry.custom_grammar,
            tool_support: CachedToolSupport::from(entry.tool_support),
            temperature_allowed: Some(entry.temperature_allowed),
            display_supported: entry.display_supported,
        }
    }
}

impl CachedEntry {
    fn into_catalog_entry(self) -> Result<CatalogEntry, CacheError> {
        let provider = super::decode::sanitize_identifier(&self.provider);
        let id = super::decode::sanitize_identifier(&self.id);
        let display = super::decode::sanitize_identifier(&self.display);
        if provider.is_empty() || id.is_empty() || display.is_empty() {
            return Err(CacheError::Invalid(
                "models cache contains an empty provider, id, or display name".into(),
            ));
        }
        let temperature_allowed = self
            .temperature_allowed
            .unwrap_or_else(|| compiled_temperature(&self.provider, &self.id));
        let thinking = match self.thinking {
            CachedThinking::OpenAi {
                accepted,
                none_supported,
            } => ThinkingSupport::OpenAi {
                accepted,
                none_supported,
            },
            CachedThinking::Adaptive {
                can_disable,
                accepted,
            } => ThinkingSupport::Adaptive {
                can_disable,
                accepted: accepted
                    .iter()
                    .filter_map(|effort| parse_anthropic_effort(effort))
                    .collect(),
            },
            CachedThinking::Budget { can_disable } => ThinkingSupport::Budget { can_disable },
            CachedThinking::UnknownAdaptive => ThinkingSupport::UnknownAdaptive,
        };
        Ok(CatalogEntry {
            provider,
            id,
            display,
            listing: if self.hidden {
                Listing::Hidden
            } else {
                Listing::Listed
            },
            context_window: self.context_window,
            max_output: self.max_output,
            thinking,
            image_input: self.image_input,
            image_profile: self.image_profile,
            remote_compact: self.remote_compact,
            supports_reasoning_summaries: self.supports_reasoning_summaries,
            tool_support: self.tool_support.into(),
            custom_grammar: self.custom_grammar,
            temperature_allowed,
            display_supported: self.display_supported,
        })
    }
}
