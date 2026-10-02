//! Anti-complexity and anti-slop guard: a first-party extension, off by default.

mod checks;
mod hooks;
mod metrics;
mod observe;
mod report;
mod stream;
mod strike;
mod summary;
mod tables;
mod terse;
#[cfg(test)]
mod tests;
mod warnings;

pub use checks::{FileFindings, Finding, Rule, Verdict};
use hooks::{
    BeforeTurnHook, SessionEndHook, SessionStartHook, ToolCallHook, ToolResultHook, TurnEndHook,
};
pub use metrics::{FileMetrics, FunctionMetrics};
pub use stream::{Calibration, G8Rule, Sample, SampleSet};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use dal_agent::error::SchemeError;
use dal_agent::ext::{
    Doc, Extension, ExtensionBuilder, PromptOrder, PromptSection, SchemeCx, SchemeResolver,
    SectionCx, SectionFn,
};
use dal_core::{
    CallId, GuardCheckMode, GuardSection, Name, RegistrationError, ServiceSet, SessionId, TurnId,
    Visibility,
};

/// Runtime guard configuration: the enabled mechanisms and their thresholds.
///
/// Plan band defaults: cognitive 15, cyclomatic 15, function PLOC 50,
/// nesting 4, file PLOC 500; erosion threshold 0.05; churn threshold 3.
/// The constructor decoding the core `[guard]` table arrives with
/// `dal_core::GuardSection` (node02Core); until then tests build this
/// struct literally.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent guard toggles; a bitset loses legibility"
)]
pub struct GuardConfig {
    enabled: bool,
    cognitive_band: u32,
    cyclomatic_band: u32,
    function_ploc_band: u32,
    nesting_band: u32,
    file_ploc_band: u32,
    guard_wrap: bool,
    broad_handler: bool,
    helper: bool,
    cannot_block: Vec<&'static str>,
    calibrated: BTreeSet<G8Rule>,
    erosion_threshold: f64,
    churn_threshold: u32,
}

impl GuardConfig {
    /// Builds the dal default: every mechanism off.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            cognitive_band: 15,
            cyclomatic_band: 15,
            function_ploc_band: 50,
            nesting_band: 4,
            file_ploc_band: 500,
            guard_wrap: false,
            broad_handler: false,
            helper: false,
            cannot_block: Vec::new(),
            calibrated: BTreeSet::new(),
            erosion_threshold: 0.05,
            churn_threshold: 3,
        }
    }
    /// Decodes the core `[guard]` table into runtime configuration.
    ///
    /// Every name in `g8_calibrated_rules` must parse as a [`G8Rule`]: the
    /// first unknown name returns [`GuardConfigError::UnknownRule`], and the
    /// first rule `calibration` refuses returns
    /// [`GuardConfigError::Uncalibrated`]. `guard_wrap` and `broad_handler`
    /// report in both modes; `BlockAfterCalibration` adds the rule name to
    /// `cannot_block`. `helper` follows `g4_enabled`.
    ///
    /// # Errors
    /// Returns [`GuardConfigError::UnknownRule`] for an unparseable rule name
    /// and [`GuardConfigError::Uncalibrated`] for a rule without sample data.
    pub fn from_section(
        section: &GuardSection,
        calibration: &Calibration,
    ) -> Result<Self, GuardConfigError> {
        let mut calibrated = BTreeSet::new();
        for name in &section.policies.g8_calibrated_rules {
            match G8Rule::parse(name) {
                Some(rule) => {
                    calibrated.insert(rule);
                }
                None => return Err(GuardConfigError::UnknownRule(name.clone())),
            }
        }
        for rule in &calibrated {
            if !calibration.admits(*rule) {
                return Err(GuardConfigError::Uncalibrated(rule.name()));
            }
        }
        let mut cannot_block = Vec::new();
        if section.policies.g2_mode == GuardCheckMode::BlockAfterCalibration {
            cannot_block.push("guard_wrap");
        }
        if section.policies.g3_mode == GuardCheckMode::BlockAfterCalibration {
            cannot_block.push("broad_handler");
        }
        Ok(Self {
            enabled: section.enabled,
            cognitive_band: section.policies.bands.cognitive,
            cyclomatic_band: section.policies.bands.cyclomatic,
            function_ploc_band: section.policies.bands.function_ploc,
            nesting_band: section.policies.bands.nesting,
            file_ploc_band: section.policies.bands.file_ploc,
            guard_wrap: true,
            broad_handler: true,
            helper: section.policies.g4_enabled,
            cannot_block,
            calibrated,
            erosion_threshold: section.policies.erosion_report_threshold,
            churn_threshold: section.policies.churn_turn_threshold,
        })
    }
}

/// A guard configuration error: an unknown rule name or a rule without a
/// labeled calibration set. Returned by the core-section constructor once
/// `dal_core::GuardSection` lands.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GuardConfigError {
    /// A `g8_calibrated_rules` name that does not parse as a rule.
    #[error("unknown guard g8 rule {0}")]
    UnknownRule(String),
    /// A calibrated rule without the required labeled sample set.
    #[error("rule {0} lacks a labeled sample set (50 required, precision >= 0.95)")]
    Uncalibrated(&'static str),
}

/// The built guard: the extension, the patch observer, and the findings handle.
pub struct GuardParts {
    /// The guard's registered extension.
    pub extension: Extension,
    /// The patch observer watching edits for findings.
    pub observer: Arc<dyn crate::patch::EditObserver>,
    /// The read-only findings handle.
    pub findings: FindingsHandle,
}

/// Builds the guard extension, observer, and findings handle from `cfg`.
///
/// # Errors
/// Returns [`RegistrationError`] when the service injection or builder rejects.
pub fn guard_extension(cfg: GuardConfig) -> Result<GuardParts, RegistrationError> {
    let engine = Arc::new(Engine::new(cfg));
    let extension = ExtensionBuilder::new(
        "guard",
        env!("CARGO_PKG_VERSION"),
        ServiceSet::from_names(["fs.read", "turn"])?,
    )?
    .on_session_start(SessionStartHook(Arc::clone(&engine)))
    .on_session_end(SessionEndHook(Arc::clone(&engine)))
    .on_before_turn(BeforeTurnHook(Arc::clone(&engine)))
    .on_tool_call(ToolCallHook(Arc::clone(&engine)))
    .on_tool_result(ToolResultHook(Arc::clone(&engine)))
    .on_turn_end(TurnEndHook(Arc::clone(&engine)))
    .output_stream(Arc::new(stream::Router::new(Arc::clone(&engine))))
    .scheme("guard", Arc::new(FindingsScheme(Arc::clone(&engine))))
    .prompt_section(PromptSection::Session {
        order: PromptOrder::Tools,
        visibility: Visibility::Model,
        section: Arc::new(TerseSection(Arc::clone(&engine))),
    })
    .build()?;
    Ok(GuardParts {
        extension,
        observer: engine.clone() as Arc<dyn crate::patch::EditObserver>,
        findings: FindingsHandle(engine),
    })
}

/// Per-turn guard findings: files, warnings, stream counts, strikes, report.
#[derive(Debug, Clone, PartialEq)]
pub struct GuardFindings {
    /// The turn these findings belong to.
    pub turn: TurnId,
    /// Per-file findings recorded this turn.
    pub files: Vec<FileFindings>,
    /// New warnings introduced, as `(file, line, message)` rows.
    pub warnings: Vec<(Box<str>, u32, Box<str>)>,
    /// Stream-rule hits as `(rule, count)` pairs.
    pub stream: Vec<(G8Rule, u32)>,
    /// Turn strikes accumulated so far.
    pub strikes: u8,
    /// The rendered guard report, when the turn produced one.
    pub report: Option<Arc<str>>,
}

/// Read-only handle to the last findings of each session.
#[derive(Clone)]
pub struct FindingsHandle(Arc<Engine>);

impl FindingsHandle {
    /// Returns the last findings recorded for `session`, if any.
    #[must_use]
    pub fn last(&self, session: &SessionId) -> Option<Arc<GuardFindings>> {
        self.0
            .state
            .lock()
            .ok()
            .and_then(|state| state.sessions.get(session)?.last.clone())
    }
}

/// Renders `findings` as the `guard://findings` JSON document.
#[must_use]
pub fn findings_json(findings: &GuardFindings) -> String {
    #[derive(serde::Serialize)]
    struct Document<'a> {
        findings: Vec<Entry<'a>>,
    }
    #[derive(serde::Serialize)]
    struct Entry<'a> {
        rule: &'a str,
        path: &'a str,
        line_start: u32,
        line_end: u32,
        text: &'a str,
    }
    let mut entries = Vec::new();
    for file in &findings.files {
        for item in &file.items {
            if matches!(item.rule, Rule::CommentedOutCode | Rule::BroadHandler) {
                entries.push(Entry {
                    rule: item.rule.name(),
                    path: &file.path,
                    line_start: item.line,
                    line_end: item.line_end,
                    text: &item.text,
                });
            }
        }
    }
    entries.sort_by(|a, b| (a.path, a.line_start).cmp(&(b.path, b.line_start)));
    sonic_rs::to_string(&Document { findings: entries })
        .unwrap_or_else(|_| String::from("{\"findings\":[]}"))
}

pub(super) struct Engine {
    cfg: GuardConfig,
    state: Mutex<State>,
}

struct State {
    sessions: HashMap<SessionId, Session>,
    turns: HashMap<TurnId, SessionId>,
}

struct Session {
    reset_due: bool,
    seen_warnings: HashSet<(Box<str>, Box<str>)>,
    turn: Option<TurnState>,
    pending: Option<Arc<str>>,
    announced_blocks: bool,
    last: Option<Arc<GuardFindings>>,
}

pub(super) struct TurnState {
    id: TurnId,
    reduction_ask: bool,
    added: u64,
    deleted: u64,
    files: BTreeSet<Box<str>>,
    new_files: BTreeSet<Box<str>>,
    deletions: BTreeMap<Box<str>, u64>,
    churn: BTreeMap<Box<str>, u32>,
    strikes: strike::Stream,
    last_error: Option<Box<str>>,
    pending_notices: Vec<String>,
    sequence: u128,
    calls: HashMap<CallId, (CallNote, u128)>,
    first_pre: BTreeMap<Box<str>, Vec<FunctionMetrics>>,
    last_post: BTreeMap<Box<str>, Vec<FunctionMetrics>>,
    bands: Vec<(f64, String)>,
    warnings: Vec<warnings::Warning>,
    stream_counts: BTreeMap<G8Rule, u32>,
    fired: BTreeSet<G8Rule>,
    findings: BTreeMap<Box<str>, FileFindings>,
    notices: HashSet<(Rule, Box<str>)>,
}

impl TurnState {
    fn touch(&mut self, path: Box<str>) {
        self.churn
            .entry(path)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
    }
}
struct CallNote {
    tool: Name,
    key: Option<[u8; 16]>,
    command: Option<Box<str>>,
    path: Option<Box<str>>,
}

impl Engine {
    fn new(cfg: GuardConfig) -> Self {
        Self {
            cfg,
            state: Mutex::new(State {
                sessions: HashMap::new(),
                turns: HashMap::new(),
            }),
        }
    }

    fn findings_json_for(&self, session: &SessionId) -> String {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.sessions.get(session)?.last.clone())
            .map_or_else(
                || String::from("{\"findings\":[]}"),
                |findings| findings_json(&findings),
            )
    }
}

fn strike_key(tool: &Name, args: &dal_core::RawJson) -> [u8; 16] {
    if let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(args.as_str()) {
        return strike::call_key(tool.as_str(), &value);
    }
    let digest = blake3::hash(args.as_str().as_bytes());
    let mut key = [0; 16];
    key.copy_from_slice(&digest.as_bytes()[..16]);
    key
}

fn args_text(args: &dal_core::RawJson, field: &str) -> Option<Box<str>> {
    use sonic_rs::{JsonContainerTrait, JsonValueTrait};
    let value: sonic_rs::Value = sonic_rs::from_str(args.as_str()).ok()?;
    let object = value.as_object()?;
    object
        .iter()
        .find(|(key, _)| *key == field)
        .and_then(|(_, item)| item.as_str())
        .map(Box::from)
}

fn cut_preview(preview: &str) -> &str {
    let end = preview
        .char_indices()
        .take_while(|(index, _)| *index < 200)
        .last()
        .map(|(index, character)| index + character.len_utf8())
        .unwrap_or_default();
    &preview[..end.min(preview.len())]
}

struct TerseSection(Arc<Engine>);

impl std::fmt::Debug for TerseSection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TerseSection")
    }
}

impl SectionFn for TerseSection {
    fn render(&self, cx: &SectionCx<'_>) -> Option<String> {
        if self.0.cfg.enabled {
            Some(terse::section(cx.tools))
        } else {
            Some(String::new())
        }
    }
}

struct FindingsScheme(Arc<Engine>);

impl FindingsScheme {
    fn document(&self, session: &SessionId, path: &str) -> Option<String> {
        (path == "findings").then(|| self.0.findings_json_for(session))
    }
}

impl SchemeResolver for FindingsScheme {
    fn read<'a>(
        &'a self,
        path: &'a str,
        cx: &'a SchemeCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, Result<Doc, SchemeError>> {
        Box::pin(async move {
            self.document(&cx.session(), path).map_or_else(
                || {
                    Err(SchemeError::NotFound {
                        uri: format!("guard://{path}").into(),
                    })
                },
                |text| Ok(Doc::new(format!("guard://{path}"), text)),
            )
        })
    }
}
