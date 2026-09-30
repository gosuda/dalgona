// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The quality battery: report-only detector lanes, codemod offers, and apply.

/// Codemod span checks and replacement text.
pub mod codemods;
/// Detector lanes.
pub mod detectors;
/// Codemod offer construction and panel rendering.
pub mod offers;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dal_agent::ext::{
    ArgError, BoxFuture, Extension, ExtensionBuilder, HookCx, HookError, ObserveHook, RawValue,
    StatusCx, StatusSnapshot, StreamWatch, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
    TurnInfo, WatchFactory,
};
use dal_core::ext::{Channel, RegistrationError, ServiceSet, StreamVerdict, Visibility};
use dal_core::{
    ModelInfo, Name, Notice, RawJson, SessionId, ToolClass, ToolSpec, TurnId, Workspace,
};
use dal_tools::guard::FindingsHandle;

use detectors::{collapse, control_leak, fabricated_call, repetitive_turns};

/// Settings of the quality battery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QualityConfig {
    /// Whether the streaming detector lanes run.
    pub detectors_enabled: bool,
}

/// The `[plugin.quality]` table is invalid.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum QualityConfigError {
    /// The table holds a key; the battery accepts none.
    #[error("unknown quality config key {key}")]
    UnknownKey {
        /// The rejected key.
        key: Box<str>,
    },
    /// The section is not a table.
    #[error("expected [plugin.quality] to be a table")]
    NotATable,
}

impl QualityConfig {
    /// Parses the `[plugin.quality]` section; an absent or empty table enables the detectors.
    ///
    /// # Errors
    ///
    /// Returns [`QualityConfigError::NotATable`] for a non-table section and
    /// [`QualityConfigError::UnknownKey`] for any key, because the battery has none.
    pub fn parse_config(section: Option<&toml::Value>) -> Result<Self, QualityConfigError> {
        let Some(section) = section else {
            return Ok(Self {
                detectors_enabled: true,
            });
        };
        let Some(table) = section.as_table() else {
            return Err(QualityConfigError::NotATable);
        };
        let Some(key) = table.keys().next() else {
            return Ok(Self {
                detectors_enabled: true,
            });
        };
        Err(QualityConfigError::UnknownKey {
            key: key.as_str().into(),
        })
    }
}

#[derive(Clone, Debug)]
struct StoredFire {
    rule: String,
    reason: String,
    anomaly: u64,
    garbage: u64,
    detail: String,
    excerpt: String,
}

#[derive(Default)]
struct TurnState {
    session: Option<SessionId>,
    fires: Vec<StoredFire>,
    text: String,
    truncated: bool,
}

#[derive(Default)]
struct Inner {
    turns: HashMap<TurnId, TurnState>,
    order: VecDeque<TurnId>,
    histories: HashMap<SessionId, repetitive_turns::TurnHistory>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<Inner>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn insert_turn(&self, turn: TurnId, session: SessionId) {
        let mut state = self.lock();
        let entry = state.turns.entry(turn).or_default();
        entry.session = Some(session);
        if !state.order.contains(&turn) {
            state.order.push_back(turn);
        }
        while state.order.len() > 64 {
            if let Some(oldest) = state.order.pop_front() {
                state.turns.remove(&oldest);
            }
        }
    }

    fn push_fire(&self, turn: TurnId, fire: StoredFire) {
        let mut state = self.lock();
        let entry = state.turns.entry(turn).or_default();
        if entry.fires.len() >= 32 {
            entry.truncated = true;
            return;
        }
        entry.fires.push(fire);
    }

    fn push_text(&self, turn: TurnId, text: String) {
        self.lock().turns.entry(turn).or_default().text = text;
    }

    fn take_turn(&self, turn: TurnId, session: SessionId) -> Option<TurnState> {
        let mut state = self.lock();
        let mut entry = state.turns.remove(&turn)?;
        entry.session = Some(session);
        state.order.retain(|candidate| *candidate != turn);
        Some(entry)
    }

    fn drop_session_turns(&self, session: SessionId) {
        let mut state = self.lock();
        state
            .turns
            .retain(|_, entry| entry.session != Some(session));
        let Inner { turns, order, .. } = &mut *state;
        order.retain(|turn| turns.contains_key(turn));
    }

    fn insert_history(&self, session: SessionId, history: repetitive_turns::TurnHistory) {
        self.lock().histories.insert(session, history);
    }

    fn remove_history(&self, session: SessionId) {
        self.lock().histories.remove(&session);
    }

    fn commit_history(&self, session: SessionId, text: &str) -> Option<repetitive_turns::Fire> {
        let mut state = self.lock();
        state
            .histories
            .get_mut(&session)
            .and_then(|history| history.commit(text))
    }
}

struct QualityWatcher {
    turn: TurnId,
    shared: Arc<Shared>,
    detectors_enabled: bool,
    collapse: collapse::State,
    leak: control_leak::State,
    text: String,
    text_capped: bool,
    collapse_fired: bool,
}

impl StreamWatch for QualityWatcher {
    fn feed(&mut self, channel: Channel, delta: &str) -> StreamVerdict {
        if !self.detectors_enabled {
            return StreamVerdict::Continue;
        }
        match channel {
            Channel::Text | Channel::Thinking => {
                self.collapse.set_source(collapse::Source::Prose);
            }
            Channel::ToolArgs { .. } => {
                self.collapse.set_source(collapse::Source::Tool);
            }
            _ => {}
        }
        if matches!(channel, Channel::Text) && !self.text_capped {
            if self.text.len() + delta.len() <= 64 * 1024 {
                self.text.push_str(delta);
            } else {
                let remaining = 64 * 1024 - self.text.len();
                let end = floor_char_boundary(delta, remaining);
                self.text.push_str(&delta[..end]);
                self.text_capped = true;
            }
        }

        let collapse_fire = self.collapse.feed(delta);
        let leak_fired = self.leak.feed(delta).is_some();
        if let Some(collapse_fire) = collapse_fire {
            self.collapse_fired = true;
            let (rule, reason) = if self.leak.corroborates(&collapse_fire) {
                ("control-token-leak", "control_token_run")
            } else {
                ("collapse-repetition", collapse_fire.reason)
            };
            self.shared.push_fire(
                self.turn,
                StoredFire {
                    rule: rule.to_owned(),
                    reason: reason.to_owned(),
                    anomaly: collapse_fire.anomaly_start_offset,
                    garbage: collapse_fire.garbage_start_offset,
                    detail: collapse_fire.detail,
                    excerpt: excerpt(&self.text),
                },
            );
        }
        if leak_fired && !self.collapse_fired {
            self.shared.push_fire(
                self.turn,
                StoredFire {
                    rule: "control-token-leak".to_owned(),
                    reason: "control_token_run".to_owned(),
                    anomaly: 0,
                    garbage: 0,
                    detail: "control token run".to_owned(),
                    excerpt: excerpt(&self.text),
                },
            );
        }
        StreamVerdict::Continue
    }

    fn finish(&mut self) -> StreamVerdict {
        self.shared
            .push_text(self.turn, std::mem::take(&mut self.text));
        StreamVerdict::Continue
    }
}

fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

struct QualityWatcherFactory {
    shared: Arc<Shared>,
    detectors_enabled: bool,
}

impl WatchFactory for QualityWatcherFactory {
    fn start(&self, turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        Some(Box::new(QualityWatcher {
            turn: turn.turn,
            shared: Arc::clone(&self.shared),
            detectors_enabled: self.detectors_enabled,
            collapse: collapse::State::new(collapse::Source::Prose),
            leak: control_leak::State::new(),
            text: String::new(),
            text_capped: false,
            collapse_fired: false,
        }))
    }
}

fn excerpt(text: &str) -> String {
    text.chars().take(256).collect()
}

struct BeforeTurnHook {
    shared: Arc<Shared>,
}

impl dal_agent::ext::Hook<dal_core::ext::BeforeTurn, Option<String>> for BeforeTurnHook {
    fn call(
        &self,
        input: dal_core::ext::BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        self.shared.insert_turn(input.turn, cx.session);
        Box::pin(async move { Ok(None) })
    }
}

struct SessionStartHook {
    shared: Arc<Shared>,
}

impl ObserveHook<dal_core::ext::SessionStart> for SessionStartHook {
    fn call(
        &self,
        input: dal_core::ext::SessionStart,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            let texts = if input.resumed {
                cx.services
                    .history_texts(&cx.caller)
                    .await
                    .map_err(|error| HookError::Failed {
                        message: error.to_string().into(),
                    })?
            } else {
                Vec::new()
            };
            shared.insert_history(
                input.session,
                repetitive_turns::TurnHistory::restore(&texts),
            );
            Ok(())
        })
    }
}

struct SessionEndHook {
    shared: Arc<Shared>,
}

impl ObserveHook<dal_core::ext::SessionEnd> for SessionEndHook {
    fn call(
        &self,
        input: dal_core::ext::SessionEnd,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        self.shared.remove_history(input.session);
        self.shared.drop_session_turns(input.session);
        Box::pin(async move { Ok(()) })
    }
}

struct TurnEndHook {
    shared: Arc<Shared>,
    findings: FindingsHandle,
}

impl ObserveHook<dal_core::ext::TurnEnd> for TurnEndHook {
    fn call(
        &self,
        input: dal_core::ext::TurnEnd,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let taken = self.shared.take_turn(input.turn, cx.session);
        let commit_fire = taken
            .as_ref()
            .and_then(|state| self.shared.commit_history(cx.session, &state.text));
        let (fires, truncated) = taken
            .map(|state| (state.fires, state.truncated))
            .unwrap_or_default();
        let findings = self.findings.clone();
        Box::pin(async move {
            settle_turn(&cx, input.turn, fires, truncated, commit_fire, &findings).await;
            Ok(())
        })
    }
}

fn raw_body<T: serde::Serialize>(body: &T) -> Option<RawJson> {
    sonic_rs::to_string(body)
        .ok()
        .and_then(|text| RawJson::parse(&text).ok())
}

fn fire_record(fire: &StoredFire) -> Option<RawJson> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    struct LaneFire<'a> {
        action: &'static str,
        rule: &'a str,
        reason: &'a str,
        anomaly_start_offset: u64,
        garbage_start_offset: u64,
        detail: &'a str,
        excerpt: &'a str,
    }
    let body = LaneFire {
        action: "report",
        rule: &fire.rule,
        reason: &fire.reason,
        anomaly_start_offset: fire.anomaly,
        garbage_start_offset: fire.garbage,
        detail: &fire.detail,
        excerpt: &fire.excerpt,
    };
    raw_body(&body)
}

fn streak_record(jaccard: f64) -> Option<RawJson> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    struct StreakFire {
        action: &'static str,
        rule: &'static str,
        jaccard: f64,
    }
    let body = StreakFire {
        action: "report",
        rule: "repetitive-turns",
        jaccard,
    };
    raw_body(&body)
}

#[derive(serde::Serialize)]
struct OffersRecord<'a> {
    turn: u64,
    offers: &'a [offers::Offer],
}

#[derive(serde::Deserialize)]
struct StoredOffers {
    #[serde(rename = "turn")]
    _turn: u64,
    offers: Vec<offers::Offer>,
}

async fn settle_turn(
    cx: &HookCx,
    turn: TurnId,
    fires: Vec<StoredFire>,
    truncated: bool,
    commit_fire: Option<repetitive_turns::Fire>,
    findings: &FindingsHandle,
) {
    for fire in &fires {
        if let Some(body) = fire_record(fire) {
            let _ = cx
                .services
                .append_record(&cx.caller, "rule_fired", Box::new(body))
                .await;
        }
    }
    if let Some(fire) = commit_fire
        && let Some(body) = streak_record(fire.jaccard)
    {
        let _ = cx
            .services
            .append_record(&cx.caller, "rule_fired", Box::new(body))
            .await;
    }
    if truncated && let Ok(body) = RawJson::parse(r#"{"truncated":true}"#) {
        let _ = cx
            .services
            .append_record(&cx.caller, "rule_fired", Box::new(body))
            .await;
    }
    let turn_offers = build_offers(cx, turn, findings).await;
    if turn_offers.offers.is_empty() {
        return;
    }
    let record = OffersRecord {
        turn: turn.get(),
        offers: &turn_offers.offers,
    };
    if let Ok(body) = sonic_rs::to_string(&record)
        && let Ok(body) = RawJson::parse(&body)
    {
        let _ = cx
            .services
            .append_record(&cx.caller, "quality_offers", Box::new(body))
            .await;
    }
    let gaps: Vec<(&str, &str)> = turn_offers
        .gaps
        .iter()
        .map(|(path, codemod)| (path.as_str(), *codemod))
        .collect();
    let text = offers::panel(&turn_offers.offers, &gaps);
    cx.services.notify(
        &cx.caller,
        Notice {
            turn: Some(turn),
            kind: Box::from("quality"),
            text: text.into(),
        },
    );
}

/// One turn's codemod offers plus the finding paths whose query has no entry.
struct TurnOffers {
    offers: Vec<offers::Offer>,
    gaps: Vec<(String, &'static str)>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Codemod {
    DeleteCommentedCode,
    RethrowEmptyCatch,
}

impl Codemod {
    const fn name(self) -> &'static str {
        match self {
            Self::DeleteCommentedCode => offers::DELETE_COMMENTED_CODE,
            Self::RethrowEmptyCatch => offers::RETHROW_EMPTY_CATCH,
        }
    }

    const fn rule(self) -> dal_tools::guard::Rule {
        match self {
            Self::DeleteCommentedCode => dal_tools::guard::Rule::CommentedOutCode,
            Self::RethrowEmptyCatch => dal_tools::guard::Rule::BroadHandler,
        }
    }
}

const CODEMODS: [Codemod; 2] = [Codemod::DeleteCommentedCode, Codemod::RethrowEmptyCatch];

async fn codemod_offers(
    turn_label: &str,
    path: &str,
    bytes: &[u8],
    codemod: Codemod,
    start_index: usize,
) -> Option<Vec<offers::Offer>> {
    let parse_path = PathBuf::from(path);
    let parse_bytes = bytes.to_vec();
    let matches = tokio::task::spawn_blocking(move || match codemod {
        Codemod::DeleteCommentedCode => {
            dal_tools::parse::codemod_delete_commented_code(&parse_path, &parse_bytes)
        }
        Codemod::RethrowEmptyCatch => {
            dal_tools::parse::codemod_rethrow_empty_catch(&parse_path, &parse_bytes)
        }
    })
    .await
    .ok()?
    .ok()?;
    Some(match codemod {
        Codemod::DeleteCommentedCode => {
            offers::delete_offers(turn_label, path, bytes, &matches, start_index)
        }
        Codemod::RethrowEmptyCatch => {
            offers::rethrow_offers(turn_label, path, bytes, &matches, start_index)
        }
    })
}

fn is_rust_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

async fn build_offers(cx: &HookCx, turn: TurnId, findings: &FindingsHandle) -> TurnOffers {
    let mut offers: Vec<offers::Offer> = Vec::new();
    let mut gaps: Vec<(String, &'static str)> = Vec::new();
    let Some(snapshot) = findings.last(&cx.session) else {
        return TurnOffers { offers, gaps };
    };
    let turn_label = turn.get().to_string();
    for file in &snapshot.files {
        let wanted: Vec<Codemod> = CODEMODS
            .into_iter()
            .filter(|codemod| file.items.iter().any(|item| item.rule == codemod.rule()))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let path = file.path.to_string();
        let Ok(Some(bytes)) = cx.services.fs_read(&cx.caller, &path).await else {
            continue;
        };
        for codemod in wanted {
            let Some(produced) =
                codemod_offers(&turn_label, &path, &bytes, codemod, offers.len()).await
            else {
                continue;
            };
            if produced.is_empty() && codemod == Codemod::DeleteCommentedCode && is_rust_path(&path)
            {
                gaps.push((path.clone(), codemod.name()));
            }
            offers.extend(produced);
        }
    }
    TurnOffers { offers, gaps }
}

struct ApplyTool {
    name: Name,
    spec: Arc<ToolSpec>,
    observer: Arc<dyn dal_tools::EditObserver>,
}

impl ApplyTool {
    fn new(observer: Arc<dyn dal_tools::EditObserver>) -> Result<Self, RegistrationError> {
        let name = Name::parse("quality_apply")?;
        let parameters = RawJson::parse(concat!(
            r#"{"type":"object","properties":{"id":{"type":"string","#,
            r#""description":"Offer id from the codemod offers notice."}}"#,
            r#","required":["id"],"additionalProperties":false}"#,
        ))
        .map_err(|_| RegistrationError::InvalidParameters)?;
        if !dal_core::valid_tool_parameters(&parameters) {
            return Err(RegistrationError::InvalidParameters);
        }
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Apply a codemod offer to its source file.".into(),
            parameters,
            grammar: None,
        });
        Ok(Self {
            name,
            spec,
            observer,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyArgs {
    id: String,
}

impl Tool for ApplyTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Patch)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(apply_offer(call, cx, &self.observer))
    }
}

async fn apply_offer<'a>(
    call: ToolCall,
    mut cx: ToolCx<'a>,
    observer: &'a Arc<dyn dal_tools::EditObserver>,
) -> ToolOutcome {
    let args = match decode_apply_args(&call.args) {
        Ok(args) => args,
        Err(outcome) => return *outcome,
    };
    let services = cx.services();
    let caller = cx.caller().clone();
    let records = match services.records(&caller, "quality_offers").await {
        Ok(records) => records,
        Err(error) => return service_failure(error),
    };
    let offer = records
        .into_iter()
        .filter_map(|record| record.decode_as::<StoredOffers>().ok())
        .flat_map(|record| record.offers)
        .find(|offer| offer.id == args.id);
    let Some(offer) = offer else {
        return ToolOutcome::Err(dal_agent::ToolError::message(offers::unknown_offer_error(
            &args.id,
        )));
    };
    let applied = dal_tools::patch::apply_replacement(
        &mut cx,
        &offer.path,
        offer.before.as_bytes(),
        offer.after.as_bytes(),
        offer.line_start,
        std::slice::from_ref(observer),
    )
    .await;
    if let Err(error) = applied {
        if cx.cancel().is_cancelled() {
            return ToolOutcome::Interrupted;
        }
        let error = match error {
            dal_tools::patch::ApplyError::Blocked(reason) => dal_agent::ToolError::Denied(reason),
            dal_tools::patch::ApplyError::Engine(error)
                if error.class == dal_tools::patch::ir::ErrorClass::Resolve
                    && error
                        .message
                        .contains("before does not match the current file bytes") =>
            {
                dal_agent::ToolError::message(offers::stale_offer_error(&offer.path))
            }
            dal_tools::patch::ApplyError::Engine(error) => {
                dal_agent::ToolError::message(error.message)
            }
            dal_tools::patch::ApplyError::ObserverBlocked(finding) => {
                dal_agent::ToolError::message(format!(
                    "quality_apply: blocked by edit observer ({}): {}",
                    finding.rule, finding.text
                ))
            }
        };
        return ToolOutcome::Err(error);
    }
    let mut output = ToolOutput::from_text(format!(
        "quality_apply: applied {} to {} (lines {}-{}).",
        offer.codemod, offer.path, offer.line_start, offer.line_end
    ));
    output.files_changed.push(PathBuf::from(offer.path));
    ToolOutcome::Ok(output)
}

fn decode_apply_args(args: &RawValue) -> Result<ApplyArgs, Box<ToolOutcome>> {
    sonic_rs::from_str::<ApplyArgs>(args.as_str()).map_err(|error| {
        Box::new(ToolOutcome::Err(dal_agent::ToolError::message(format!(
            "quality_apply: invalid input: {error}."
        ))))
    })
}

fn service_failure(error: dal_agent::ServiceError) -> ToolOutcome {
    match error {
        dal_agent::ServiceError::Denied(reason) => {
            ToolOutcome::Err(dal_agent::ToolError::Denied(reason))
        }
        dal_agent::ServiceError::Cancelled => ToolOutcome::Interrupted,
        error => ToolOutcome::Err(dal_agent::ToolError::Failed(Box::new(error))),
    }
}

struct CodemodOffersQuiet;

impl dal_agent::ext::StatusPoll for CodemodOffersQuiet {
    fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
        StatusSnapshot {
            quiet: true,
            text: None,
        }
    }
}

/// The text of the `dalgona://quality` manual page.
pub const QUALITY_DOC: &str = concat!(
    "# quality\n\n",
    "The quality battery watches streamed model output with three report-only\n",
    "detector lanes: collapse-repetition, control-token-leak, and repetitive-\n",
    "turns. Their fires land as durable rule_fired records and stay silent in\n",
    "human views. It also registers the text rule\n",
    "fabricated-unavailable-tool-call in always-interrupt mode; a matching\n",
    "pattern interrupts the response.\n\n",
    "For each turn, the guard reports from its retained measurements; the\n",
    "quality battery reads that same per-turn findings snapshot for offers and\n",
    "does not measure those files again:\n\n",
    "- turn growth: added, deleted, and net lines and the files touched.\n",
    "- per-file absolute metrics: up to 20 displayed files per patch call get a receipt\n",
    "  with code lines, function count, and cognitive and cyclomatic sums; any\n",
    "  remaining files are summarized by count.\n",
    "- per-function metric changes: each band crossing is one line with the\n",
    "  function and its before-to-after cognitive, cyclomatic, size, or nesting\n",
    "  value; file-size crossings are labeled by path. The turn summary ranks\n",
    "  crossings and keeps the top ten.\n",
    "- best-current: when erosion rises by the configured threshold, `METRICS`\n",
    "  gives the files, bands crossed, and erosion before/after. The report then\n",
    "  ranks per-function current-mass changes by greatest increase, filling the\n",
    "  remaining rows of the ten-row cap after band crossings.\n\n",
    "Guard findings become codemod offers: delete-commented-code (not for Rust)\n",
    "and rethrow-empty-catch spans that cover whole lines. Offers land as\n",
    "durable quality_offers records, the model sees one notice that lists them,\n",
    "and quality_apply applies one offer through the normal preview and\n",
    "approval path after a byte-for-byte staleness check. A quality offer never\n",
    "writes a file directly.\n\n",
    "Config: the `[plugin.quality]` table accepts no keys; the guard is turned\n",
    "on through the product's `[guard]` table.\n",
);

/// Builds the quality extension over guard findings and the registered patch observer.
///
/// # Errors
///
/// Returns a [`RegistrationError`] when a declared name, service, tool schema, or rule is invalid.
pub fn quality(
    cfg: QualityConfig,
    findings: FindingsHandle,
    observer: Arc<dyn dal_tools::EditObserver>,
) -> Result<Extension, RegistrationError> {
    let apply = ApplyTool::new(observer)?;
    let shared = Arc::new(Shared::default());
    let services = ServiceSet::from_names(["fs.read"])?;
    let builder = ExtensionBuilder::new("quality", "0.1.0", services)?
        .with_origin(dal_core::Origin::Bundled, None);
    let builder = builder.rule(fabricated_call::rule()?);
    let builder = builder.tool(Arc::new(apply), Visibility::Model);
    let builder = builder.status_kind("codemod_offers", Arc::new(CodemodOffersQuiet));
    let builder = builder.output_stream(Arc::new(QualityWatcherFactory {
        shared: Arc::clone(&shared),
        detectors_enabled: cfg.detectors_enabled,
    }));
    let builder = builder.on_before_turn(BeforeTurnHook {
        shared: Arc::clone(&shared),
    });
    let builder = builder.on_session_start(SessionStartHook {
        shared: Arc::clone(&shared),
    });
    let builder = builder.on_session_end(SessionEndHook {
        shared: Arc::clone(&shared),
    });
    let builder = builder.on_turn_end(TurnEndHook {
        shared: Arc::clone(&shared),
        findings,
    });
    builder.build()
}
