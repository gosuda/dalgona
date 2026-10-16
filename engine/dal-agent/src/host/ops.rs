//! Host-level queries: commands, models, docs, relay, generation.

use std::sync::Arc;

use dal_core::{ClientId, Family, GenerationId, ModelInfo, ModelRequest, ModelRoute, SessionId};
use dal_provider::{Catalog, CatalogEntry, built_in_entries, resolve};
use tokio_util::sync::CancellationToken;

use super::{Host, SessionEntry};
use crate::error::{HostError, SchemeError, ToolError};
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
    /// A `session://` URI resolves from the live session's memory first. A
    /// session with no live actor is read from its journal on disk instead:
    /// a synchronous, bounded, strictly read-only file scan that takes no
    /// lock and never reopens, repairs, or revives the session.
    ///
    /// # Errors
    /// Returns a [`SchemeError`] when the URI is invalid, its scheme is
    /// unknown, no document exists at the URI, or a resolver fails. A
    /// damaged or unsafe journal is [`SchemeError::Failed`], never a miss.
    pub fn doc(&self, uri: &str) -> Result<Doc, SchemeError> {
        let parsed = scheme::parse(uri)?;
        match scheme::builtin_resolver(parsed.scheme) {
            Ok(scheme::BuiltinResolver::Session) => {
                return self.session_doc(uri, parsed.path);
            }
            Ok(scheme::BuiltinResolver::Job) => {
                return self.job_doc(uri);
            }
            Err(_) => {}
        }
        let generation = self.state.shared.generation.borrow().clone();
        if let Some(resolver) = generation.scheme(parsed.scheme)
            && let Some(doc) = resolver.read_static(parsed.path)
        {
            return doc;
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
        let live = self
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .map(|live| {
                let view = live.shared.snapshot(snapshot_for_doc(live));
                view.entries.items.into_iter().find(|item| item.id == entry)
            });
        // The session actor is gone or never lived here: the journal on disk
        // is the one source of truth, read without lock, recovery append, or
        // marker removal.
        let Some(found) = live else {
            return session_doc_from_disk(&self.state.shared.data_root, uri, session, entry);
        };
        let found = found.ok_or_else(|| SchemeError::NotFound { uri: uri.into() })?;
        entry_doc(uri, &found)
    }

    /// Resolves one `job://` URI through the owning live session's job table.
    ///
    /// Every live session's table is asked and the single claim wins: a
    /// second claim is an invariant failure, never a first-match guess, and
    /// an id no live table owns keeps the dispatch table's
    /// `job <id> is not running` text. `doc` is synchronous while job tables
    /// lock asynchronously, so tables lock with `try_lock`: a table
    /// mutating under a concurrent spawn reports busy instead of blocking
    /// the caller.
    ///
    /// # Errors
    /// Returns a [`SchemeError`] when the URI is not a `job://` document, no
    /// live table owns the id, more than one does, or the owning table
    /// fails the read.
    fn job_doc(&self, uri: &str) -> Result<Doc, SchemeError> {
        let (id, _) = scheme::job_parts(uri)?;
        let tables: Vec<Arc<tokio::sync::Mutex<crate::jobs::JobTable>>> = self
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|entry| Arc::clone(entry.backend.jobs()))
            .collect();
        let not_running = crate::error::ValidationError::job_not_running(id).to_string();
        let mut claims = 0u32;
        let mut settled: Option<Result<Doc, SchemeError>> = None;
        let mut busy = false;
        for table in &tables {
            let Ok(table) = table.try_lock() else {
                busy = true;
                continue;
            };
            let Some(outcome) = read_owned_job(&table, uri, &not_running) else {
                continue;
            };
            claims += 1;
            settled = settled.or(Some(outcome));
        }
        if claims > 1 {
            return Err(SchemeError::Failed {
                message: format!(
                    "job {id} is claimed by more than one live session; refusing to guess."
                )
                .into(),
            });
        }
        if let Some(outcome) = settled {
            return outcome;
        }
        if busy {
            return Err(SchemeError::Failed {
                message: format!(
                    "job {id} is temporarily unreadable; its session job table is busy."
                )
                .into(),
            });
        }
        Err(SchemeError::Failed {
            message: not_running.into(),
        })
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

/// Renders one entry as the public `session://` JSON document.
fn entry_doc(uri: &str, entry: &dal_core::EntryView) -> Result<Doc, SchemeError> {
    let text = sonic_rs::to_string(entry).map_err(|error| SchemeError::Failed {
        message: error.to_string().into(),
    })?;
    Ok(Doc::new(uri, text))
}

/// Reads one closed-session entry from its journal under `data_root`.
///
/// A blocking local file read bounded by the store's record limits: `Host::doc`
/// is synchronous, so the read must not enter an async runtime. A missing
/// session or entry is the typed `NotFound`; damage, an unsafe file, or an
/// I/O failure is `Failed` with the store's own text, never a silent miss.
fn session_doc_from_disk(
    data_root: &std::path::Path,
    uri: &str,
    session: SessionId,
    entry: dal_core::EntryId,
) -> Result<Doc, SchemeError> {
    match dal_store::Store::read_session_entry(data_root, session, entry) {
        Ok(found) => entry_doc(uri, &found),
        Err(
            dal_store::StoreError::NotFound { .. } | dal_store::StoreError::UnknownEntry { .. },
        ) => Err(SchemeError::NotFound { uri: uri.into() }),
        Err(error) => Err(SchemeError::Failed {
            message: error.to_string().into(),
        }),
    }
}

/// Reads one `job://` URI from a single locked job table.
///
/// Returns `None` when the table owns no such row so the caller keeps
/// looking; any other outcome claims the id, including the table's own
/// typed route errors. This keeps the multi-table scan in `job_doc` flat:
/// one claim wins, two claims are an invariant failure.
fn read_owned_job(
    table: &crate::jobs::JobTable,
    uri: &str,
    not_running: &str,
) -> Option<Result<Doc, SchemeError>> {
    match table.read_uri(uri) {
        Ok(doc) => Some(Ok(doc)),
        Err(ToolError::Scheme(error)) => Some(Err(error)),
        Err(ToolError::Failed(error)) if error.to_string() == not_running => None,
        Err(error) => Some(Err(SchemeError::Failed {
            message: error.to_string().into(),
        })),
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
