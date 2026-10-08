//! Native read, search, patch, and process tools for dal.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

use dal_agent::ext::{BoxFuture, Extension, ExtensionBuilder, HookCx, HookError, ObserveHook};
use dal_core::{RegistrationError, ServiceSet, SessionId, ToolResultEvent, TurnId, Visibility};
use search::index::Index;

pub mod evidence;
pub mod exec;
#[cfg(feature = "symbols")]
pub mod guard;
#[cfg(feature = "symbols")]
pub mod parse;
pub mod patch;
pub mod read;
pub mod search;

#[cfg(feature = "symbols")]
pub use guard::{
    Calibration, FindingsHandle, G8Rule, GuardConfig, GuardConfigError, GuardFindings, GuardParts,
    Sample, SampleSet, guard_extension,
};
pub use patch::ir::EditObserver;

/// Computes the full BLAKE3 digest used to verify raw source bytes.
#[must_use]
pub fn digest32(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Computes the first eight uppercase hexadecimal digits of a domain-separated BLAKE3 digest.
#[must_use]
pub fn tag8(domain: &str, bytes: &[u8]) -> String {
    // Hash sequential chunks without concatenating an intermediate buffer.
    // https://docs.rs/blake3/1.8.7/blake3/struct.Hasher.html
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(b":");
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut tag = String::with_capacity(8);
    for byte in &digest.as_bytes()[..4] {
        tag.push(char::from(HEX[usize::from(*byte >> 4)]));
        tag.push(char::from(HEX[usize::from(*byte & 0x0f)]));
    }
    tag
}
/// Context supplied to a search reranker.
pub struct RerankCall<'a> {
    /// The user's search pattern.
    pub query: &'a str,
    /// The active search mode: `find`, `grep`, or `symbol`.
    pub mode: &'a str,
    /// Candidates in deterministic ranking order.
    pub candidates: &'a [RerankCandidate],
    /// The session running the search.
    pub session: SessionId,
    /// The active turn, when the search runs during one.
    pub turn: Option<TurnId>,
    /// Host services available to the reranker.
    pub services: Arc<dyn dal_agent::ext::Services>,
}

impl fmt::Debug for RerankCall<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RerankCall")
            .field("query", &self.query)
            .field("mode", &self.mode)
            .field("candidates", &self.candidates)
            .field("session", &self.session)
            .field("turn", &self.turn)
            .field("services", &"<host services>")
            .finish()
    }
}

/// One search candidate passed to a reranker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RerankCandidate {
    /// Workspace-relative path.
    pub path: Box<str>,
    /// Display text for the candidate.
    pub display: Box<str>,
}

/// The candidate order and optional note returned by a reranker.
#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RerankOutput {
    /// A permutation of the candidate indices `0..len`.
    pub order: Vec<u32>,
    /// Optional note appended after search output and footers.
    pub note: Option<Box<str>>,
}

/// Reorders deterministic search candidates through the configured judged battery.
pub trait Rerank: Send + Sync + 'static {
    /// Returns the candidate-index permutation and an optional user-visible note.
    fn rerank<'a>(&'a self, call: RerankCall<'a>) -> dal_agent::ext::BoxFuture<'a, RerankOutput>;
}
/// Configuration shared by the native read, search, patch, and exec tools.
#[must_use]
pub struct ToolsConfig {
    /// Whether symbol search is enabled in the current product configuration.
    pub search_symbols: Arc<AtomicBool>,
    /// Persistent index root supplied by the product's data directory.
    pub index_root: Option<PathBuf>,
    /// Optional reranker applied after deterministic search ranking.
    pub rerank: Option<Arc<dyn Rerank>>,
    /// Patch dialect selection: the core table until the product supplies one.
    pub edit_style: dal_core::EditStyleInput,
    /// Guard observer wired into patch; `None` runs patch without a guard.
    pub observer: Option<Arc<dyn EditObserver>>,
    /// Process execution budgets for the exec tool.
    pub exec: exec::ExecConfig,
    /// Guard thresholds; the host builds the guard extension separately.
    #[cfg(feature = "symbols")]
    pub guard: GuardConfig,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            search_symbols: Arc::new(AtomicBool::new(false)),
            index_root: None,
            rerank: None,
            edit_style: dal_core::EditStyleInput::default(),
            observer: None,
            exec: exec::ExecConfig::default(),
            #[cfg(feature = "symbols")]
            guard: GuardConfig::disabled(),
        }
    }
}

impl fmt::Debug for ToolsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolsConfig")
            .field("search_symbols", &self.search_symbols)
            .field("index_root", &self.index_root)
            .field("rerank_configured", &self.rerank.is_some())
            .field("edit_style", &self.edit_style)
            .field("observer_configured", &self.observer.is_some())
            .field("exec", &self.exec)
            .finish_non_exhaustive()
    }
}

/// Builds the `tools` extension registering the read, search, patch, and exec
/// tools with model visibility. The guard extension is built separately by the
/// host with [`guard_extension`]; its observer travels as [`ToolsConfig::observer`].
///
/// # Errors
///
/// Returns [`RegistrationError`] when a fixed tool name, schema, or the
/// extension record is rejected.
pub fn extension(cfg: ToolsConfig) -> Result<Extension, RegistrationError> {
    let index = Index::new(cfg.index_root.clone());
    let seen = Seen::new();
    let snapshots = Arc::new(patch::snapshot::SnapshotStore::new(patch::process_boot()));
    let evidence = Arc::new(evidence::SnapshotEvidence::new(Arc::clone(&snapshots)));
    let read_tool = read::tool(seen.clone(), Arc::clone(&snapshots))?;
    let search_tool = search::tool(
        cfg.search_symbols.clone(),
        index.clone(),
        seen.clone(),
        Arc::clone(&snapshots),
        cfg.rerank.clone(),
    )?;
    let patch_tool = patch::tool(
        &cfg.edit_style,
        index.clone(),
        seen,
        snapshots,
        cfg.observer.clone(),
        cfg.search_symbols.clone(),
    )?;
    let exec_tool = exec::exec_extension(cfg.exec)?;
    ExtensionBuilder::new("tools", env!("CARGO_PKG_VERSION"), ServiceSet::EMPTY)?
        .tool(read_tool, Visibility::Model)
        .tool(search_tool, Visibility::Model)
        .tool(patch_tool, Visibility::Model)
        .tool(exec_tool, Visibility::Model)
        .on_tool_result_lossless(observe_results(index))
        .evidence(evidence)
        .build()
}

/// The exec freshness lever: every settled call whose tool is not
/// read, search, or patch marks the index for a stat-walk republish.
fn observe_results(index: Arc<Index>) -> ObserveResults {
    ObserveResults { index }
}

struct ObserveResults {
    index: Arc<Index>,
}

impl ObserveHook<ToolResultEvent> for ObserveResults {
    fn call(
        &self,
        input: ToolResultEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let index = Arc::clone(&self.index);
        Box::pin(async move {
            let tool = input.tool.as_str();
            if tool != "read" && tool != "search" && tool != "patch" {
                index.exec_ran();
            }
            Ok(())
        })
    }
}

/// Records which exact source lines a session has fully seen.
///
/// The store is bounded by session, path, and content-version LRUs. A digest change
/// never inherits line coverage from an older version.
#[must_use]
#[derive(Debug, Default)]
pub struct Seen {
    state: Mutex<SeenState>,
}

#[derive(Debug, Default)]
struct SeenState {
    sessions: HashMap<SessionId, SessionSeen>,
    session_lru: VecDeque<SessionId>,
}

#[derive(Debug, Default)]
struct SessionSeen {
    paths: HashMap<Arc<str>, PathSeen>,
    path_lru: VecDeque<Arc<str>>,
}

#[derive(Debug, Default)]
struct PathSeen {
    versions: VecDeque<VersionSeen>,
}

#[derive(Debug)]
struct VersionSeen {
    digest: [u8; 32],
    intervals: Vec<(u64, u64)>,
}

const MAX_SESSIONS: usize = 1024;
const MAX_PATHS_PER_SESSION: usize = 256;
const MAX_VERSIONS_PER_PATH: usize = 16;

impl Seen {
    /// Creates an empty, shared line-coverage store.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records an interval whose every line was displayed for this exact content digest.
    pub fn show(&self, session: SessionId, path: &str, digest: [u8; 32], first: u64, last: u64) {
        if first == 0 || first > last {
            return;
        }

        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        touch(&mut state.session_lru, session);
        let session_seen = state.sessions.entry(session).or_default();
        let path_key: Arc<str> = Arc::from(path);
        touch(&mut session_seen.path_lru, Arc::clone(&path_key));
        let path_seen = session_seen.paths.entry(path_key).or_default();

        let version_index = path_seen
            .versions
            .iter()
            .position(|version| version.digest == digest);
        if let Some(index) = version_index {
            let version = path_seen.versions.remove(index);
            if let Some(version) = version {
                path_seen.versions.push_back(version);
            }
        } else {
            path_seen.versions.push_back(VersionSeen {
                digest,
                intervals: Vec::new(),
            });
        }
        while path_seen.versions.len() > MAX_VERSIONS_PER_PATH {
            path_seen.versions.pop_front();
        }
        if let Some(version) = path_seen
            .versions
            .iter_mut()
            .find(|version| version.digest == digest)
        {
            insert_interval(&mut version.intervals, first, last);
        }

        while session_seen.paths.len() > MAX_PATHS_PER_SESSION {
            if let Some(oldest) = session_seen.path_lru.pop_front() {
                session_seen.paths.remove(&oldest);
            } else {
                break;
            }
        }
        while state.sessions.len() > MAX_SESSIONS {
            if let Some(oldest) = state.session_lru.pop_front() {
                state.sessions.remove(&oldest);
            } else {
                break;
            }
        }
    }

    /// Returns the merged intervals recorded for the exact session, path, and digest.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "read.rs records coverage with `show` and reads it back in lib tests"
        )
    )]
    #[must_use]
    pub(crate) fn intervals(
        &self,
        session: SessionId,
        path: &str,
        digest: [u8; 32],
    ) -> Vec<(u64, u64)> {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state
            .sessions
            .get(&session)
            .and_then(|session_seen| session_seen.paths.get(path))
            .and_then(|path_seen| {
                path_seen
                    .versions
                    .iter()
                    .find(|version| version.digest == digest)
            })
            .map_or_else(Vec::new, |version| version.intervals.clone())
    }

    /// Returns whether the full line interval is present for this exact content digest.
    #[must_use]
    pub(crate) fn covers(
        &self,
        session: SessionId,
        path: &str,
        digest: [u8; 32],
        first: u64,
        last: u64,
    ) -> bool {
        if first == 0 || first > last {
            return false;
        }
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state
            .sessions
            .get(&session)
            .and_then(|session_seen| session_seen.paths.get(path))
            .and_then(|path_seen| {
                path_seen
                    .versions
                    .iter()
                    .find(|version| version.digest == digest)
            })
            .is_some_and(|version| {
                version
                    .intervals
                    .iter()
                    .any(|&(seen_first, seen_last)| seen_first <= first && seen_last >= last)
            })
    }
}

fn touch<T: PartialEq>(lru: &mut VecDeque<T>, value: T) {
    lru.retain(|existing| existing != &value);
    lru.push_back(value);
}

fn insert_interval(intervals: &mut Vec<(u64, u64)>, first: u64, last: u64) {
    let insert_at =
        intervals.partition_point(|&(_, current_last)| current_last.saturating_add(1) < first);
    let mut merged_first = first;
    let mut merged_last = last;
    let mut merge_end = insert_at;
    while let Some(&(current_first, current_last)) = intervals.get(merge_end) {
        if merged_last.saturating_add(1) < current_first {
            break;
        }
        merged_first = merged_first.min(current_first);
        merged_last = merged_last.max(current_last);
        merge_end += 1;
    }

    if insert_at == merge_end {
        intervals.reserve(1);
        intervals.insert(insert_at, (first, last));
        return;
    }
    intervals[insert_at] = (merged_first, merged_last);
    intervals.drain(insert_at + 1..merge_end);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_store_bounds() {
        let seen = Seen::new();
        let session = SessionId::new_v7();
        let digest = [1; 32];
        for index in 0..257 {
            seen.show(session, &format!("path-{index}"), digest, 1, 1);
        }
        assert!(seen.intervals(session, "path-0", digest).is_empty());
        assert_eq!(seen.intervals(session, "path-1", digest), [(1, 1)]);

        for version in 0..17 {
            seen.show(session, "one-path", [version; 32], 1, 1);
        }
        assert!(seen.intervals(session, "one-path", [0; 32]).is_empty());
        assert_eq!(seen.intervals(session, "one-path", [1; 32]), [(1, 1)]);

        let session_store = Seen::new();
        let oldest = SessionId::new_v7();
        session_store.show(oldest, "retained", digest, 1, 4);
        for _ in 0..1023 {
            session_store.show(SessionId::new_v7(), "other", digest, 1, 1);
        }
        assert_eq!(
            session_store.intervals(oldest, "retained", digest),
            [(1, 4)]
        );
        session_store.show(SessionId::new_v7(), "newest", digest, 1, 1);
        assert!(
            session_store
                .intervals(oldest, "retained", digest)
                .is_empty()
        );
    }

    #[test]
    fn tag_domains_differ() {
        let whole = tag8("whole", b"same bytes");
        let definition = tag8("def", b"same bytes");
        assert_ne!(whole, definition);
        assert_eq!(whole.len(), 8);
        assert_eq!(definition.len(), 8);
        assert!(
            whole
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())
        );
        assert!(
            definition
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())
        );
    }

    #[test]
    fn seen_intervals_merge_only_adjacent_or_overlapping_lines() {
        let seen = Seen::new();
        let session = SessionId::new_v7();
        let digest = [9; 32];
        seen.show(session, "file", digest, 5, 7);
        seen.show(session, "file", digest, 1, 2);
        seen.show(session, "file", digest, 3, 4);
        seen.show(session, "file", digest, 10, 10);
        assert_eq!(seen.intervals(session, "file", digest), [(1, 7), (10, 10)]);
        assert!(seen.covers(session, "file", digest, 2, 6));
        assert!(!seen.covers(session, "file", digest, 7, 10));
    }
}
