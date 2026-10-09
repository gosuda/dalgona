//! Host-level queries: commands, models, docs, login, relay, generation.

use std::sync::Arc;

use dal_core::{ClientId, Family, GenerationId, ModelInfo, ModelRequest, ModelRoute, SessionId};
use dal_provider::{
    AuthStore, Catalog, CatalogEntry, Credential, SecretString, built_in_entries, resolve,
};
use tokio_util::sync::CancellationToken;

use super::{Host, SessionEntry};
use crate::error::{HostError, SchemeError};
use crate::ext::scheme::Doc;
use crate::scheme;

/// One listed document: its full URI plus the display title.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocEntry {
    /// The full URI, including the `://` separator.
    pub uri: Box<str>,
    /// The display title for `docs` listings.
    pub title: Box<str>,
}

impl Host {
    /// Returns the host's owned data root.
    #[must_use]
    pub fn data_root(&self) -> &std::path::Path {
        &self.state.shared.data_root
    }

    /// Returns the merged command table in canonical order.
    #[must_use]
    pub fn commands(&self) -> Arc<[dal_core::CommandSpec]> {
        Arc::clone(&self.state.shared.commands)
    }

    /// Returns the current extension generation id.
    #[must_use]
    pub fn generation(&self) -> GenerationId {
        self.state.shared.generation.borrow().id
    }

    /// Lists models for one family, falling back to compiled rows offline.
    ///
    /// # Errors
    /// This operation does not fail; catalog errors fall back to the compiled rows.
    pub async fn models(&self, family: Option<Family>) -> Result<Vec<ModelInfo>, HostError> {
        let shared = Arc::clone(&self.state.shared);
        let mut models = match shared.providers.catalog().await {
            Ok(catalog) => Self::live_models(&shared, &catalog, family).await,
            Err(_) => Vec::new(),
        };
        if models.is_empty() {
            models = builtin_models(&built_in_entries(), &shared, family);
        }
        if family.is_none() {
            let generation = shared.generation.borrow().clone();
            models.extend(crate::ext::synthetic::listed(&generation));
        }
        Ok(models)
    }

    /// Lists one provider's display rows per distinct catalog provider.
    async fn live_models(
        shared: &Arc<super::HostShared>,
        catalog: &Catalog,
        family: Option<Family>,
    ) -> Vec<ModelInfo> {
        let aliases: Vec<(Box<str>, Box<str>)> = shared
            .config
            .aliases()
            .iter()
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        let mut seen: Vec<Box<str>> = Vec::new();
        let mut models = Vec::new();
        for entry in catalog.entries() {
            if seen.iter().any(|provider| provider == &entry.provider) {
                continue;
            }
            seen.push(entry.provider.clone());
            let reference = format!("{}/{}", entry.provider, entry.id);
            let Ok(resolved) = resolve(catalog, &aliases, &reference) else {
                continue;
            };
            let Ok(provider) = shared.providers.provider(resolved) else {
                continue;
            };
            if let Ok(rows) = provider.models().await {
                models.extend(rows);
            }
        }
        models
            .into_iter()
            .filter(|info| {
                family.is_none_or(|wanted| {
                    crate::session::context::api_family(&info.route) == Some(wanted)
                })
            })
            .collect()
    }

    /// Resolves one document URI through the scheme table.
    ///
    /// # Errors
    /// Returns a [`SchemeError`] when the URI is invalid, its scheme is
    /// unknown, no document exists at the URI, or a resolver fails.
    pub fn doc(&self, uri: &str) -> Result<Doc, SchemeError> {
        let parsed = scheme::parse(uri)?;
        let table = scheme::SchemeTable::new();
        match table.resolver(parsed.scheme) {
            Ok(scheme::BuiltinResolver::Session) => {
                return self.session_doc(uri, parsed.path);
            }
            Ok(scheme::BuiltinResolver::Job) => {
                return Err(SchemeError::Failed {
                    message: format!(
                        "job output at {uri} is unavailable until the session turn driver attaches its job table."
                    )
                    .into(),
                });
            }
            Err(_) => {}
        }
        let generation = self.state.shared.generation.borrow().clone();
        if parsed.scheme == "skill" {
            return generation
                .skill_body(parsed.path)
                .map(|body| Doc::new(uri, body))
                .ok_or_else(|| SchemeError::Failed {
                    message: format!("unknown skill: {}", parsed.path).into(),
                });
        }
        let docs = generation.docs();
        if let Some(page) = docs.find(uri) {
            return Ok(Doc::new(page.uri.clone(), page.text.clone()));
        }
        let uris: Vec<&str> = docs.list().iter().map(|page| &*page.uri).collect();
        match nearest_uri(&uris, uri) {
            Some(near) => Err(SchemeError::Near {
                uri: uri.into(),
                nearest: near.into(),
            }),
            None => Err(SchemeError::NotFound { uri: uri.into() }),
        }
    }

    /// Lists every extension document as URI/title pairs in URI order.
    #[must_use]
    pub fn docs(&self) -> Vec<DocEntry> {
        let generation = self.state.shared.generation.borrow().clone();
        generation
            .docs()
            .list()
            .iter()
            .map(|page| DocEntry {
                uri: page.uri.clone(),
                title: page.title.clone(),
            })
            .collect()
    }
    fn session_doc(&self, uri: &str, path: &str) -> Result<Doc, SchemeError> {
        let (session, entry) = path
            .split_once('/')
            .ok_or_else(|| SchemeError::NotFound { uri: uri.into() })?;
        let session =
            SessionId::parse(session).map_err(|_| SchemeError::NotFound { uri: uri.into() })?;
        let entry = entry
            .parse::<u64>()
            .ok()
            .and_then(std::num::NonZeroU64::new)
            .map(dal_core::EntryId::new)
            .ok_or_else(|| SchemeError::NotFound { uri: uri.into() })?;
        let sessions = self
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let live = sessions
            .get(&session)
            .ok_or_else(|| SchemeError::NotFound { uri: uri.into() })?;
        let view = live.shared.snapshot(snapshot_for_doc(live));
        let found = view
            .entries
            .items
            .into_iter()
            .find(|item| item.id == entry)
            .ok_or_else(|| SchemeError::NotFound { uri: uri.into() })?;
        let text = sonic_rs::to_string(&found).map_err(|error| SchemeError::Failed {
            message: error.to_string().into(),
        })?;
        Ok(Doc::new(uri, text))
    }
    /// Returns true when the provider resolves a credential from auth.json or the captured env.
    #[must_use]
    pub fn has_credential(&self, provider: &str) -> bool {
        self.state.shared.providers.credential(provider).is_ok()
    }

    /// Opens one attributed provider stream with no journal turn or retry.
    ///
    /// # Errors
    /// Returns [`HostError::Config`] when the catalog or provider cannot be
    /// resolved, the route does not match the resolved model, or the provider
    /// fails to open the stream.
    pub async fn relay(
        &self,
        by: ClientId,
        route: ModelRoute,
        request: ModelRequest,
    ) -> Result<dal_provider::EventStream, HostError> {
        let _ = by;
        let shared = Arc::clone(&self.state.shared);
        if let Some(found) = crate::ext::synthetic::find_route(&shared, &route) {
            let deps = crate::session::turn::RequestDeps {
                session: SessionId::new_v7(),
                host: Arc::clone(&self.state),
                script: None,
            };
            let cancel = CancellationToken::new();
            let stream = crate::ext::synthetic::open(&deps, found, request, &cancel).await;
            return Ok(stream);
        }
        let catalog = shared
            .providers
            .catalog()
            .await
            .map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })?;
        let aliases: Vec<(Box<str>, Box<str>)> = shared
            .config
            .aliases()
            .iter()
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        let resolved =
            dal_provider::resolve_route(&catalog, &aliases, &request.model).map_err(|error| {
                HostError::Config {
                    message: error.to_string().into(),
                }
            })?;
        if !route_matches(&route, &resolved.route) {
            return Err(HostError::Config {
                message: format!("request route does not match {route:?}.").into(),
            });
        }
        let provider = shared
            .providers
            .provider(resolved)
            .map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })?;
        let cancel = CancellationToken::new();
        provider
            .open(
                SessionId::new_v7(),
                &request,
                &[],
                Arc::new(|_: String| {}),
                &cancel,
            )
            .await
            .map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })
    }

    /// Stores one provider API key in the mode-0600 credential store.
    ///
    /// Loads `auth.json` under the data root (creating it when absent),
    /// replaces the entry for `provider`, and writes the store back
    /// atomically. Error texts never carry the key.
    ///
    /// # Errors
    /// Returns [`HostError::Config`] when the store cannot be read or
    /// written, or when `provider` takes no API key.
    pub async fn login(&self, provider: &str, api_key: &str) -> Result<(), HostError> {
        let path = self.state.shared.data_root.join("auth.json");
        let provider = provider.to_owned();
        let key = SecretString::from(api_key);
        tokio::task::spawn_blocking(move || {
            let mut store = AuthStore::load(&path).map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })?;
            store
                .set(&provider, Credential::ApiKey { key })
                .map_err(|error| HostError::Config {
                    message: error.to_string().into(),
                })?;
            store.store().map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })
        })
        .await
        .map_err(|error| HostError::Config {
            message: error.to_string().into(),
        })?
    }

    /// Resolves the configured default model through aliases and the catalog.
    ///
    /// Returns `None` when no model is configured or the reference does not
    /// resolve. Mirrors the driver's request-time resolution for preflight
    /// callers that must decide before a turn exists.
    pub async fn default_route(&self) -> Option<ModelRoute> {
        let shared = &self.state.shared;
        let reference = shared.config.model()?;
        if reference.is_empty() {
            return None;
        }
        if let Some(found) = crate::ext::synthetic::find(shared, reference) {
            return Some(found.route());
        }
        let catalog = shared.providers.catalog().await.ok()?;
        shared.cache_catalog(catalog.clone());
        let aliases: Vec<(Box<str>, Box<str>)> = shared
            .config
            .aliases()
            .iter()
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        dal_provider::resolve(&catalog, &aliases, reference)
            .ok()
            .map(|resolved| resolved.route)
    }
}

/// Maps compiled catalog rows to display models for one family.
fn builtin_models(
    entries: &[CatalogEntry],
    shared: &super::HostShared,
    family: Option<Family>,
) -> Vec<ModelInfo> {
    let aliases: Vec<(Box<str>, Box<str>)> = shared
        .config
        .aliases()
        .iter()
        .map(|(name, target)| (name.clone(), target.clone()))
        .collect();
    let catalog = Catalog::with_sources(Vec::new(), entries.to_vec());
    let mut models = Vec::new();
    for entry in catalog.entries() {
        let reference = format!("{}/{}", entry.provider, entry.id);
        let Ok(resolved) = resolve(&catalog, &aliases, &reference) else {
            continue;
        };
        if let Some(wanted) = family
            && crate::session::context::api_family(&resolved.route) != Some(wanted)
        {
            continue;
        }
        models.push(crate::session::context::model_info_for(
            &resolved.entry,
            &resolved.route,
            shared.config.thinking(),
        ));
    }
    models
}
/// Renders a request route as a catalog reference.
pub(crate) fn request_reference(route: &ModelRoute) -> String {
    match route {
        ModelRoute::Api { model, .. } => model.to_string(),
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id.to_string(),
    }
}

/// Whether two routes name the same model.
fn route_matches(wanted: &ModelRoute, resolved: &ModelRoute) -> bool {
    match (wanted, resolved) {
        (
            ModelRoute::Api {
                family: a,
                model: m,
            },
            ModelRoute::Api {
                family: b,
                model: n,
            },
        ) => a == b && m == n,
        _ => wanted == resolved,
    }
}

/// Snapshot arguments for one session document lookup.
fn snapshot_for_doc(entry: &SessionEntry) -> crate::session::projection::SnapshotArgs {
    crate::session::projection::SnapshotArgs {
        generation: entry.generation,
        id: entry.handle.session(),
        workspace: entry.workspace.clone(),
        open: entry.broker.open_requests(),
        updated_at: dal_core::Timestamp::now(),
        created_at: None,
        archived: None,
        page: dal_core::PageReq::default(),
    }
}

/// Returns the nearest record URI within edit distance 2, or `None`.
fn nearest_uri<'a>(uris: &[&'a str], text: &str) -> Option<&'a str> {
    let mut best: Option<(&'a str, usize)> = None;
    for uri in uris {
        let distance = edit_distance(uri, text, 2);
        if distance <= 2 && best.is_none_or(|(_, known)| distance < known) {
            best = Some((uri, distance));
        }
    }
    best.map(|(uri, _)| uri)
}

/// Computes the edit distance of two strings, stopping past `cap`.
fn edit_distance(first: &str, second: &str, cap: usize) -> usize {
    let mut previous: Vec<usize> = (0..=second.len()).collect();
    let mut current = vec![0; second.len() + 1];
    for (row, first_byte) in first.bytes().enumerate() {
        current[0] = row + 1;
        for (column, second_byte) in second.bytes().enumerate() {
            let substitution = previous[column] + usize::from(first_byte != second_byte);
            let insertion = current[column] + 1;
            let deletion = previous[column + 1] + 1;
            current[column + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
        if previous.iter().all(|cost| *cost > cap) {
            return cap + 1;
        }
    }
    previous[second.len()]
}
