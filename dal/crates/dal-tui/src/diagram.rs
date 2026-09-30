//! Diagram engine: pure seam, four fence kinds, worker budget, LRU cache.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::sync::{Arc, Mutex};

pub mod cli;
pub mod dot;
pub mod mermaid;
pub mod raster;

/// Diagram source language from a fence info string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagramKind {
    /// D2 diagrams via the `d2` CLI.
    D2,
    /// Nomnoml diagrams via the `nomnoml` CLI.
    Nomnoml,
    /// Graphviz DOT, validated and rasterized in process.
    Dot,
    /// Mermaid, rendered in process as text art.
    Mermaid,
}

impl DiagramKind {
    /// Parses exactly the four fence spellings, case-insensitive, no aliases.
    #[must_use]
    pub fn parse(info: &str) -> Option<Self> {
        match info.trim().to_lowercase().as_str() {
            "d2" => Some(Self::D2),
            "nomnoml" => Some(Self::Nomnoml),
            "dot" => Some(Self::Dot),
            "mermaid" => Some(Self::Mermaid),
            _ => None,
        }
    }

    /// Returns the CLI tool name for CLI-backed kinds.
    #[must_use]
    pub const fn tool(self) -> &'static str {
        match self {
            Self::D2 => "d2",
            Self::Nomnoml => "nomnoml",
            Self::Dot | Self::Mermaid => "",
        }
    }
}

/// One styled cell of text art.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtCell {
    /// Cell text (one grapheme).
    pub text: String,
    /// Semantic role.
    pub role: ArtRole,
}

/// Art role mapped from opaline tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtRole {
    /// Box and line glyphs.
    Border,
    /// Node text.
    Text,
    /// Arrows and shapes.
    Edge,
    /// Edge labels.
    EdgeLabel,
    /// Graph labels.
    Title,
}

/// Text art with measured cell width.
#[derive(Debug, Clone)]
pub struct DiagramArt {
    /// Art rows of cells.
    pub rows: Vec<Vec<ArtCell>>,
    /// Cell width of the widest row.
    pub width_cells: usize,
}

/// Pure render outcome.
#[derive(Debug, Clone)]
pub enum DiagramOutcome {
    /// Text art within the width cap.
    Art(DiagramArt),
    /// Source fallback with a reason literal.
    Fallback {
        /// Why the diagram fell back to source text.
        reason: Box<str>,
    },
    /// PNG bytes for the protocol ladder.
    Pixels(Arc<[u8]>),
}

/// Prompt literal directing models to fenced diagrams.
pub use dal_core::PROMPT_DIAGRAMS;

/// Controls whether supported fenced diagrams are routed to the render engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagramSettings {
    /// Whether diagram rendering is enabled.
    pub enabled: bool,
}

/// One rendered result from [`wire_block`].
#[derive(Debug, Clone)]
pub enum WiredBlock {
    /// Role-tagged text art.
    Art(DiagramArt),
    /// Render failure and the original source to show below its card.
    Fallback {
        /// Language of the fenced diagram.
        kind: DiagramKind,
        /// Renderer failure reason.
        reason: Box<str>,
        /// Unmodified fenced source body.
        source: Box<str>,
    },
    /// Encoded PNG for the terminal image renderer.
    Pixels(Arc<[u8]>),
    /// The CLI-backed diagram is queued for the renderer worker.
    Pending {
        /// Language of the fenced diagram.
        kind: DiagramKind,
    },
}

/// Renders one whole supported fenced block when enabled.
///
/// Returns `None` for disabled settings, unsupported blocks, and text that is
/// not exactly one fenced block. The disabled path returns before inspecting
/// the text or touching the cache.
#[must_use]
pub fn wire_block(
    settings: &DiagramSettings,
    cache: &RenderCache,
    text: &str,
    width_cells: usize,
) -> Option<WiredBlock> {
    if !settings.enabled {
        return None;
    }
    let (kind, source) = fenced_source(text)?;
    Some(
        match cache.get_or_schedule(kind, source.as_bytes(), width_cells) {
            Some(DiagramOutcome::Art(art)) => WiredBlock::Art(art),
            Some(DiagramOutcome::Fallback { reason }) => WiredBlock::Fallback {
                kind,
                reason,
                source: source.into(),
            },
            Some(DiagramOutcome::Pixels(pixels)) => WiredBlock::Pixels(pixels),
            None => WiredBlock::Pending { kind },
        },
    )
}

fn fenced_source(text: &str) -> Option<(DiagramKind, &str)> {
    let text = text.trim_end_matches(['\r', '\n']);
    let (opening, body) = text.split_once('\n')?;
    let opening = opening.strip_prefix("```")?;
    let info = opening.strip_suffix('\r').unwrap_or(opening);
    let kind = DiagramKind::parse(info)?;
    if body == "```" {
        return Some((kind, ""));
    }
    let (source, closing) = body.rsplit_once('\n')?;
    if closing != "```" {
        return None;
    }
    Some((kind, source.strip_suffix('\r').unwrap_or(source)))
}

const SOURCE_CAP: usize = 16 * 1_024;
const ART_CELL_CAP: usize = 65_536;
const CACHE_CAP: usize = 32;
const MAX_RENDER_WORKERS: usize = 2;

/// Renders a diagram source within a cell-width cap.
#[must_use]
pub fn render_diagram(
    kind: DiagramKind,
    source: &[u8],
    width_cells: usize,
    path: Option<&OsStr>,
) -> DiagramOutcome {
    let source = strip_one_bom(source);
    if source.len() > SOURCE_CAP {
        return fallback("too large");
    }
    if source.iter().all(u8::is_ascii_whitespace) {
        return fallback(format!("invalid {}", kind_name(kind)));
    }
    let outcome = match kind {
        DiagramKind::Mermaid => mermaid::render(source, width_cells),
        DiagramKind::Dot => dot::render(source, width_cells),
        DiagramKind::D2 | DiagramKind::Nomnoml => cli::render(kind, source, path),
    };
    limit_art_cells(outcome)
}

fn limit_art_cells(outcome: DiagramOutcome) -> DiagramOutcome {
    match outcome {
        DiagramOutcome::Art(art)
            if art.rows.len().saturating_mul(art.width_cells) > ART_CELL_CAP =>
        {
            fallback("too large")
        }
        outcome => outcome,
    }
}

fn kind_name(kind: DiagramKind) -> &'static str {
    match kind {
        DiagramKind::D2 => "d2",
        DiagramKind::Nomnoml => "nomnoml",
        DiagramKind::Dot => "dot",
        DiagramKind::Mermaid => "mermaid",
    }
}

fn fallback(reason: impl Into<Box<str>>) -> DiagramOutcome {
    DiagramOutcome::Fallback {
        reason: reason.into(),
    }
}

fn strip_one_bom(source: &[u8]) -> &[u8] {
    source.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(source)
}

/// Failure text for a settled diagram card.
#[must_use]
pub fn fallback_card(kind: DiagramKind, reason: &str) -> String {
    crate::copy::render(
        crate::copy::ids::DIAGRAM_FAILED,
        &[("kind", kind_name(kind)), ("reason", reason)],
        1,
    )
}

/// In-process LRU over kind, blake3 digest, and width bucket.
#[derive(Debug, Default)]
pub struct RenderCache {
    path: Option<OsString>,
    state: Arc<Mutex<RenderCacheState>>,
}

type RenderKey = (DiagramKind, [u8; 32], usize);

#[derive(Debug, Default)]
struct RenderCacheState {
    entries: HashMap<RenderKey, DiagramOutcome>,
    order: VecDeque<RenderKey>,
    pending: HashSet<RenderKey>,
    renders: usize,
    generation: u64,
}

impl RenderCacheState {
    fn cached(&mut self, key: &RenderKey) -> Option<DiagramOutcome> {
        let outcome = self.entries.get(key)?.clone();
        if let Some(index) = self.order.iter().position(|entry| entry == key) {
            self.order.remove(index);
        }
        self.order.push_back(*key);
        Some(outcome)
    }

    fn insert(&mut self, key: RenderKey, outcome: DiagramOutcome) {
        if self.entries.len() >= CACHE_CAP
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.order.push_back(key);
        self.entries.insert(key, outcome);
    }
}

impl RenderCache {
    /// Creates a cache bound to the PATH snapshot captured by the process edge.
    #[must_use]
    pub fn with_path(path: Option<OsString>) -> Self {
        Self {
            path,
            state: Arc::default(),
        }
    }

    /// Returns a cached outcome or renders once and stores art and failures alike.
    #[must_use]
    pub fn get_or_render(
        &self,
        kind: DiagramKind,
        source: &[u8],
        width_cells: usize,
    ) -> DiagramOutcome {
        let key = cache_key(kind, source, width_cells);
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(outcome) = state.cached(&key) {
                return outcome;
            }
            state.renders += 1;
        }
        let outcome = render_diagram(kind, source, width_cells, self.path.as_deref());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = state.cached(&key) {
            return cached;
        }
        state.insert(key, outcome.clone());
        outcome
    }

    fn get_or_schedule(
        &self,
        kind: DiagramKind,
        source: &[u8],
        width_cells: usize,
    ) -> Option<DiagramOutcome> {
        if !matches!(kind, DiagramKind::D2 | DiagramKind::Nomnoml) {
            return Some(self.get_or_render(kind, source, width_cells));
        }
        let key = cache_key(kind, source, width_cells);
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(outcome) = state.cached(&key) {
                return Some(outcome);
            }
            if state.pending.contains(&key) || state.pending.len() >= MAX_RENDER_WORKERS {
                return None;
            }
            state.pending.insert(key);
            state.renders += 1;
        }
        let shared = Arc::clone(&self.state);
        let path = self.path.clone();
        let source = source.to_vec();
        if std::thread::Builder::new()
            .name("dal-tui-diagram".to_owned())
            .spawn(move || {
                let outcome = render_diagram(kind, &source, width_cells, path.as_deref());
                let mut state = shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.pending.remove(&key);
                state.insert(key, outcome);
                state.generation = state.generation.saturating_add(1);
            })
            .is_err()
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending.remove(&key);
            state.renders = state.renders.saturating_sub(1);
            return Some(fallback("renderer worker unavailable"));
        }
        None
    }

    /// Returns the number of renders performed or queued.
    #[must_use]
    pub fn renders(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .renders
    }

    /// Returns the number of completed background renders.
    #[must_use]
    pub(crate) fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
    }
}

fn cache_key(kind: DiagramKind, source: &[u8], width_cells: usize) -> RenderKey {
    let bucket = width_cells.div_ceil(16) * 16;
    let digest = blake3::hash(source);
    (kind, *digest.as_bytes(), bucket)
}

#[cfg(test)]
mod tests {
    use super::{
        DiagramKind, DiagramOutcome, DiagramSettings, RenderCache, WiredBlock, fallback_card,
        wire_block,
    };

    #[test]
    fn only_four_fence_spellings_parse() {
        assert_eq!(DiagramKind::parse("mermaid"), Some(DiagramKind::Mermaid));
        assert_eq!(DiagramKind::parse("MERMAID"), Some(DiagramKind::Mermaid));
        assert_eq!(DiagramKind::parse("graph"), None);
    }

    #[test]
    fn oversized_and_empty_sources_fall_back() {
        let big = vec![b'x'; 16 * 1_024 + 1];
        assert!(matches!(
            super::render_diagram(DiagramKind::Mermaid, &big, 80, None),
            DiagramOutcome::Fallback { .. }
        ));
        assert!(matches!(
            super::render_diagram(DiagramKind::Mermaid, b"  \n ", 80, None),
            DiagramOutcome::Fallback { .. }
        ));
    }

    #[test]
    fn doubled_bom_fails_while_single_bom_renders() {
        let single = [b"\xef\xbb\xbfgraph TD; A-->B".as_slice(), b""].concat();
        assert!(matches!(
            super::render_diagram(DiagramKind::Mermaid, &single, 80, None),
            DiagramOutcome::Art(_) | DiagramOutcome::Fallback { .. }
        ));
        let doubled = [b"\xef\xbb\xbf\xef\xbb\xbfgraph TD; A-->B".as_slice(), b""].concat();
        assert!(matches!(
            super::render_diagram(DiagramKind::Mermaid, &doubled, 80, None),
            DiagramOutcome::Fallback { .. }
        ));
    }

    #[test]
    fn cache_shares_buckets_and_evicts_oldest() {
        let cache = RenderCache::default();
        let source = b"graph TD; A-->B";
        let _ = cache.get_or_render(DiagramKind::Mermaid, source, 40);
        let _ = cache.get_or_render(DiagramKind::Mermaid, source, 48);
        assert_eq!(cache.renders(), 1);
        let _ = cache.get_or_render(DiagramKind::Mermaid, source, 49);
        assert_eq!(cache.renders(), 2);
        assert!(
            fallback_card(DiagramKind::D2, "d2 is not installed").contains("d2 is not installed")
        );
    }

    fn art(width_cells: usize, height: usize) -> super::DiagramArt {
        let row = vec![
            super::ArtCell {
                text: "x".to_owned(),
                role: super::ArtRole::Text,
            };
            width_cells
        ];
        super::DiagramArt {
            rows: vec![row; height],
            width_cells,
        }
    }

    #[test]
    fn wire_block_routes_each_supported_fence() {
        let settings = DiagramSettings { enabled: true };
        let cache = RenderCache::with_path(Some("/nonexistent-diagram-tools".into()));
        for (language, source) in [
            ("d2", "a -> b"),
            ("nomnoml", "[A]->[B]"),
            ("dot", "digraph { a -> b }"),
            ("mermaid", "graph TD; A-->B"),
        ] {
            let block = format!("```{language}\n{source}\n```");
            assert!(
                wire_block(&settings, &cache, &block, 80).is_some(),
                "{language}"
            );
        }
        assert_eq!(cache.renders(), 4);
    }

    #[test]
    fn disabled_wiring_returns_before_parsing_or_rendering() {
        let cache = RenderCache::default();
        assert!(wire_block(&DiagramSettings::default(), &cache, "not a fence", 80).is_none());
        assert_eq!(cache.renders(), 0);
    }

    #[test]
    fn fallback_keeps_renderer_reason_and_raw_source() {
        let settings = DiagramSettings { enabled: true };
        let cache = RenderCache::with_path(Some("/nonexistent-diagram-tools".into()));
        let source = "```d2\n a -> b \n```";
        let _ = wire_block(&settings, &cache, source, 80);
        for _ in 0..100 {
            if cache.generation() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(
            wire_block(&settings, &cache, source, 80),
            Some(WiredBlock::Fallback { kind: DiagramKind::D2, reason, source })
                if reason.as_ref() == "d2 is not installed" && source.as_ref() == " a -> b "
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cli_projection_returns_pending_before_renderer_finishes() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let tools = tempfile::tempdir().expect("temporary tool directory");
        let d2 = tools.path().join("d2");
        std::fs::write(&d2, b"#!/bin/sh\n/bin/sleep 1\nexit 1\n").expect("fake d2 tool");
        let mut permissions = std::fs::metadata(&d2)
            .expect("fake tool metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&d2, permissions).expect("executable fake tool");
        let cache = RenderCache::with_path(Some(tools.path().as_os_str().to_owned()));
        let started = Instant::now();
        let wired = wire_block(
            &DiagramSettings { enabled: true },
            &cache,
            "```d2\na -> b\n```",
            80,
        );
        assert!(matches!(
            wired,
            Some(WiredBlock::Pending {
                kind: DiagramKind::D2
            })
        ));
        assert!(started.elapsed() < Duration::from_millis(500));
        for _ in 0..200 {
            if cache.generation() > 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("background renderer did not finish");
    }

    #[test]
    fn wire_block_cache_uses_width_buckets() {
        let settings = DiagramSettings { enabled: true };
        let cache = RenderCache::default();
        let source = "```mermaid\ngraph TD; A-->B\n```";
        assert!(wire_block(&settings, &cache, source, 40).is_some());
        assert!(wire_block(&settings, &cache, source, 47).is_some());
        assert_eq!(cache.renders(), 1);
        assert!(wire_block(&settings, &cache, source, 49).is_some());
        assert_eq!(cache.renders(), 2);
    }

    #[test]
    fn unsupported_or_incomplete_fences_remain_unwired() {
        let settings = DiagramSettings { enabled: true };
        let cache = RenderCache::default();
        for text in [
            "before\n```mermaid\ngraph TD; A-->B\n```",
            "```mermaid\ngraph TD; A-->B\n``` trailing",
            "```mermaid\ngraph TD; A-->B",
            "```graph\ngraph TD; A-->B\n```",
        ] {
            assert!(wire_block(&settings, &cache, text, 80).is_none(), "{text}");
        }
        assert_eq!(cache.renders(), 0);
    }

    #[test]
    fn cache_hit_refreshes_lru_recency() {
        let cache = RenderCache::default();
        let mut sources = Vec::with_capacity(super::CACHE_CAP);
        for index in 0..super::CACHE_CAP {
            let source = format!("graph TD; A{index}-->B");
            let _ = cache.get_or_render(DiagramKind::Mermaid, source.as_bytes(), 80);
            sources.push(source);
        }
        let _ = cache.get_or_render(DiagramKind::Mermaid, sources[0].as_bytes(), 80);
        let _ = cache.get_or_render(DiagramKind::Mermaid, b"graph TD; extra-->B", 80);
        let renders = cache.renders();
        let _ = cache.get_or_render(DiagramKind::Mermaid, sources[0].as_bytes(), 80);
        assert_eq!(cache.renders(), renders);
        let _ = cache.get_or_render(DiagramKind::Mermaid, sources[1].as_bytes(), 80);
        assert_eq!(cache.renders(), renders + 1);
    }

    #[test]
    fn art_cell_budget_accepts_the_cap_and_rejects_overflow() {
        assert!(matches!(
            super::limit_art_cells(DiagramOutcome::Art(art(256, 256))),
            DiagramOutcome::Art(_)
        ));
        assert!(matches!(
            super::limit_art_cells(DiagramOutcome::Art(art(257, 256))),
            DiagramOutcome::Fallback { reason } if reason.as_ref() == "too large"
        ));
    }
}
