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
    SessionCx, SessionInstalls, SessionSetup, StatusCx, StatusSnapshot, StreamWatch, Tool,
    ToolCall, ToolCx, ToolOutcome, ToolOutput, TurnInfo, WatchFactory,
};
use dal_core::ext::{Channel, RegistrationError, ServiceSet, StreamVerdict, Visibility};
use dal_core::{
    ModelInfo, Name, Notice, RawJson, SessionId, ToolClass, ToolSpec, TurnId, Workspace,
};
use dal_ext::ttsr::build::{self, RuleBuildInput};
use dal_tools::guard::FindingsHandle;

use detectors::{collapse, control_leak, fabricated_call, repetitive_turns};

/// The compiled lane rules of the `detectors` set, in flag order.
const LANE_RULES: [&str; 3] = [
    "collapse-repetition",
    "control-token-leak",
    "repetitive-turns",
];

/// The rules-load note for lanes suppressed by `rules.watch = false`.
const LANE_WATCH_NOTE: &str = "rules.watch is false, so dalgona does not watch this rule";

/// Settings of the quality battery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QualityConfig {
    /// Whether the compiled detector rules run, including streaming lanes and
    /// the fabricated-tool-call rule.
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
    lanes: HashMap<SessionId, LaneFlags>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<Inner>,
}

/// Per-session run switches for the three compiled detector lanes.
///
/// A lane runs when the `detectors` set is selected, `rules.watch` is true,
/// `[rules] disabled` does not name it, and no higher-trust rule file claims
/// its name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LaneFlags {
    collapse: bool,
    leak: bool,
    turns: bool,
}

impl LaneFlags {
    /// Flags for a session whose `detectors` set is selected and otherwise
    /// unclaimed.
    fn enabled() -> Self {
        Self {
            collapse: true,
            leak: true,
            turns: true,
        }
    }

    /// Flags with every lane held, e.g. under `rules.watch = false`.
    fn held() -> Self {
        Self::default()
    }

    fn any(self) -> bool {
        self.collapse || self.leak || self.turns
    }

    fn disable_claimed(&mut self, claimed: [bool; 3]) {
        self.collapse &= !claimed[0];
        self.leak &= !claimed[1];
        self.turns &= !claimed[2];
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_lane_flags(&self, session: SessionId, flags: LaneFlags) {
        self.lock().lanes.insert(session, flags);
    }

    fn lane_flags(&self, session: SessionId) -> Option<LaneFlags> {
        self.lock().lanes.get(&session).copied()
    }

    fn drop_lane_flags(&self, session: SessionId) {
        self.lock().lanes.remove(&session);
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
    flags: LaneFlags,
    collapse: collapse::State,
    leak: control_leak::State,
    text: String,
    text_capped: bool,
    collapse_fired: bool,
}

impl StreamWatch for QualityWatcher {
    fn feed(&mut self, channel: Channel, delta: &str) -> StreamVerdict {
        if !self.flags.any() {
            return StreamVerdict::Continue;
        }
        if self.flags.collapse {
            match channel {
                Channel::Text | Channel::Thinking => {
                    self.collapse.set_source(collapse::Source::Prose);
                }
                Channel::ToolArgs { .. } => {
                    self.collapse.set_source(collapse::Source::Tool);
                }
                _ => {}
            }
        }
        if matches!(channel, Channel::Text) && !self.text_capped {
            if self.text.len() + delta.len() <= 64 * 1024 {
                self.text.push_str(delta);
            } else {
                let remaining = 64 * 1024 - self.text.len();
                self.text
                    .push_str(crate::text::byte_prefix(delta, remaining));
                self.text_capped = true;
            }
        }

        let collapse_fire = if self.flags.collapse {
            self.collapse.feed(delta)
        } else {
            None
        };
        let leak_fired = if self.flags.leak {
            self.leak.feed(delta).is_some()
        } else {
            false
        };
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

struct QualityWatcherFactory {
    shared: Arc<Shared>,
    detectors_enabled: bool,
}

impl QualityWatcherFactory {
    /// Flags for a session whose setup has not recorded anything, which
    /// happens only outside a normal session open.
    fn default_flags(&self) -> LaneFlags {
        if self.detectors_enabled {
            LaneFlags::enabled()
        } else {
            LaneFlags::held()
        }
    }
}

impl WatchFactory for QualityWatcherFactory {
    fn start(&self, turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        let flags = self
            .shared
            .lane_flags(turn.session)
            .unwrap_or_else(|| self.default_flags());
        Some(Box::new(QualityWatcher {
            turn: turn.turn,
            shared: Arc::clone(&self.shared),
            flags,
            collapse: collapse::State::new(collapse::Source::Prose),
            leak: control_leak::State::new(),
            text: String::new(),
            text_capped: false,
            collapse_fired: false,
        }))
    }
}

/// Resolves the per-session lane flags from the rules configuration and the
/// built rule view.
///
/// The lanes hold when the `detectors` set is not selected, when
/// `rules.watch` is false, when `[rules] disabled` names a lane, or when a
/// higher-trust rule file claims the lane's name and watches the stream in
/// its place. The claim check uses the shared rule loader and admission
/// walk, so the trust order has exactly one implementation.
fn lane_flags_for(cx: &SessionCx, detectors_enabled: bool) -> LaneFlags {
    let rules = cx.config.rules();
    let claimed = if detectors_enabled && rules.watch {
        claimed_lane_names(cx)
    } else {
        [false; 3]
    };
    lane_flags_from(detectors_enabled, rules.watch, &rules.disabled, claimed)
}

/// The lane decision without session access: a lane runs when the
/// `detectors` set is selected, `rules.watch` is true, `[rules] disabled`
/// does not name it, and no higher-trust rule file claims its name.
fn lane_flags_from(
    detectors_enabled: bool,
    watch: bool,
    disabled: &[Box<str>],
    claimed: [bool; 3],
) -> LaneFlags {
    if !detectors_enabled || !watch {
        return LaneFlags::held();
    }
    let mut flags = LaneFlags::enabled();
    for (index, name) in LANE_RULES.iter().enumerate() {
        if disabled.iter().any(|rule| rule.as_ref() == *name) {
            match index {
                0 => flags.collapse = false,
                1 => flags.leak = false,
                _ => flags.turns = false,
            }
        }
    }
    flags.disable_claimed(claimed);
    flags
}

/// Names the lanes that a user rule file of the same name replaces, in
/// [`LANE_RULES`] order. Trust order puts a user file ahead of a bundled
/// rule and a project or plugin file behind it, so only a user file claims a
/// lane. The shared loader and admission walk preserve the TTSR precedence
/// without compiling unrelated plugin and project matchers.
fn claimed_lane_names(cx: &SessionCx) -> [bool; 3] {
    let records: Vec<dal_ext::ttsr::record::RecordSource> = cx
        .rules
        .iter()
        .map(|source| dal_ext::ttsr::record::RecordSource {
            plugin: source.ext.clone(),
            site: source.site.clone(),
            record: source.record.clone(),
        })
        .collect();
    let tool_names: Vec<&str> = cx.tool_names.iter().map(AsRef::as_ref).collect();
    let agent = if cx.parent.is_none() { "main" } else { "sub" };
    let input = RuleBuildInput {
        records: &records,
        plugin_rules: &cx.rule_files,
        known_tools: &tool_names,
        agent,
    };
    let built = build::user_set_for(
        &input,
        &cx.data_root,
        cx.workspace.as_path(),
        cx.config.rules(),
    );
    claimed_by(&built.user_rule_names())
}

/// Maps user rule names to the lanes they replace, in [`LANE_RULES`] order.
fn claimed_by(user_rule_names: &[&str]) -> [bool; 3] {
    LANE_RULES.map(|lane| user_rule_names.contains(&lane))
}

struct QualitySetup {
    shared: Arc<Shared>,
    detectors_enabled: bool,
}

impl SessionSetup for QualitySetup {
    fn open(&self, cx: SessionCx) -> BoxFuture<'static, Result<SessionInstalls, HookError>> {
        let shared = Arc::clone(&self.shared);
        let detectors_enabled = self.detectors_enabled;
        Box::pin(async move {
            let watch = cx.config.rules().watch;
            let flags = lane_flags_for(&cx, detectors_enabled);
            shared.set_lane_flags(cx.session, flags);
            let notices = watch_notice(detectors_enabled, watch, flags);
            Ok(SessionInstalls {
                watches: Vec::new(),
                notices,
                state: None,
            })
        })
    }
}

/// The rules-load notice that names the lanes a false `rules.watch` holds.
/// Lanes held by `disabled` or by a user rule file stay silent; their
/// reasons live in the rules report.
fn watch_notice(detectors_enabled: bool, watch: bool, flags: LaneFlags) -> Vec<Notice> {
    if !detectors_enabled || watch || flags.any() {
        return Vec::new();
    }
    vec![Notice {
        turn: None,
        kind: "rules".into(),
        text: format!("{LANE_WATCH_NOTE}: {}.", LANE_RULES.join(", ")).into(),
    }]
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
        self.shared.drop_lane_flags(input.session);
        Box::pin(async move { Ok(()) })
    }
}

/// Whether the repetitive-turns lane reports for a session: it runs only when
/// its lane flag runs. Sessions without recorded flags (outside a normal
/// session open) fall back to the caller-supplied default, which the turn-end
/// hook builds from the extension-level switch exactly like the watcher
/// factory default.
fn turns_lane_runs(recorded: Option<LaneFlags>, fallback: LaneFlags) -> bool {
    recorded.unwrap_or(fallback).turns
}

struct TurnEndHook {
    shared: Arc<Shared>,
    findings: FindingsHandle,
    detectors_enabled: bool,
}

impl TurnEndHook {
    /// Flags for a session whose setup has not recorded anything, which
    /// happens only outside a normal session open.
    fn default_flags(&self) -> LaneFlags {
        if self.detectors_enabled {
            LaneFlags::enabled()
        } else {
            LaneFlags::held()
        }
    }
}

impl ObserveHook<dal_core::ext::TurnEnd> for TurnEndHook {
    fn call(
        &self,
        input: dal_core::ext::TurnEnd,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let taken = self.shared.take_turn(input.turn, cx.session);
        // A held turns lane neither advances the streak nor reports: the
        // streaming lanes already stand down via their watcher flags, and the
        // turn-level lane must stand down the same way.
        let runs = turns_lane_runs(self.shared.lane_flags(cx.session), self.default_flags());
        let commit_fire = if runs {
            taken
                .as_ref()
                .and_then(|state| self.shared.commit_history(cx.session, &state.text))
        } else {
            None
        };
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
        anomaly_start_offset: u64,
        garbage_start_offset: u64,
        detail: &'static str,
    }
    let body = StreakFire {
        action: "report",
        rule: "repetitive-turns",
        jaccard,
        // The whole settled turn is the evidence; the lane compares turns,
        // so there is no within-response anomaly offset to carry.
        anomaly_start_offset: 0,
        garbage_start_offset: 0,
        detail: repetitive_turns::FIRE_DETAIL,
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
    ToolOutcome::Ok(Box::new(output))
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
    "human views. Repetition collapse is reported as control-token-leak when\n",
    "the control-token detector confirms a correlated token run; otherwise it\n",
    "stays collapse-repetition. Repetitive-turn detection reports once at the\n",
    "third similar turn. An eligible dissimilar turn resets the streak.\n",
    "Each turn retains up to 32 detector fires. Further fires set\n",
    "`{\"truncated\":true}` in the turn-end record. Stored text excerpts are\n",
    "capped at 64 KiB; that text cap does not set the truncated flag.\n\n",
    "The lanes hold per session: `rules.watch = false` holds every lane and\n",
    "prints the rules watch note; a lane name in `[rules] disabled` holds\n",
    "that lane; and a user rule file named after a lane (for example\n",
    "`collapse-repetition.md` under `<data-root>/rules/`) replaces the\n",
    "compiled lane, which stands down while the user rule watches instead.\n\n",
    "It also registers the text rule\n",
    "fabricated-unavailable-tool-call in always-interrupt mode; a matching\n",
    "pattern interrupts the response.\n\n",
    "For each turn, the guard reports from its retained measurements; the\n",
    "quality battery reads that same per-turn findings snapshot for offers and\n",
    "does not measure those files again:\n\n",
    "- `TURN CHANGE SUMMARY.`: added, deleted, and net lines with the files touched.\n",
    "- `guard:` receipts: up to 20 displayed files per patch call get a receipt\n",
    "  with code lines, function count, and cognitive and cyclomatic sums; any\n",
    "  remaining files are summarized by count.\n",
    "- band crossings: each crossing is one line with the\n",
    "  function and its before-to-after cognitive, cyclomatic, size, or nesting\n",
    "  value; file-size crossings are labeled by path. The turn summary ranks\n",
    "  crossings and keeps the top ten.\n",
    "- `METRICS:`: when erosion rises by the configured threshold, the `METRICS:`\n",
    "  line gives the files, bands crossed, and erosion before/after. The report then\n",
    "  ranks per-function current-mass changes by greatest increase, filling the\n",
    "  remaining rows of the ten-row cap after band crossings.\n\n",
    "Guard findings become codemod offers: delete-commented-code (not for Rust)\n",
    "and rethrow-empty-catch spans that cover whole lines. Offers land as\n",
    "durable quality_offers records, the model sees one notice that lists them,\n",
    "and quality_apply applies one offer through the normal preview and\n",
    "approval path after a byte-for-byte staleness check. A quality offer never\n",
    "writes a file directly.\n\n",
    "The `[plugin.quality]` table accepts no keys. Unknown keys and a non-table\n",
    "value fail startup. The four detector rules belong to the compiled\n",
    "`detectors` rule set, enabled by default through `[rule_sets].enabled`.\n",
    "Omitting `detectors` from that list disables the three streaming detector\n",
    "lanes and fabricated-unavailable-tool-call. It does not remove\n",
    "`quality_apply` or guard metrics. Configure the guard through `[guard]`.\n",
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
    let builder = if cfg.detectors_enabled {
        builder.rule(fabricated_call::rule()?)
    } else {
        builder
    };
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
        detectors_enabled: cfg.detectors_enabled,
    });
    let builder = builder.session_setup(QualitySetup {
        shared: Arc::clone(&shared),
        detectors_enabled: cfg.detectors_enabled,
    });
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled_names(names: &[&str]) -> Vec<Box<str>> {
        names.iter().map(|name| (*name).into()).collect()
    }

    #[test]
    fn watch_false_holds_every_lane_with_the_set_selected() {
        let flags = lane_flags_from(true, false, &[], [false; 3]);
        assert!(!flags.any());
        let notice = watch_notice(true, false, flags);
        assert_eq!(notice.len(), 1);
        assert!(
            notice[0].text.contains(LANE_WATCH_NOTE),
            "{}",
            notice[0].text
        );
        assert!(notice[0].text.contains("collapse-repetition"));
    }

    #[test]
    fn unselected_detectors_hold_lanes_with_no_notice() {
        let flags = lane_flags_from(false, true, &[], [false; 3]);
        assert!(!flags.any());
        assert_eq!(watch_notice(false, true, flags), []);
    }

    #[test]
    fn disabled_names_and_claimed_names_hold_single_lanes() {
        let flags = lane_flags_from(
            true,
            true,
            &disabled_names(&["control-token-leak"]),
            [false, false, true],
        );
        assert!(flags.collapse);
        assert!(!flags.leak);
        assert!(!flags.turns);
    }

    async fn settle_turn_text(
        shared: &Arc<Shared>,
        services: &Arc<crate::work::support::FakeServices>,
        session: SessionId,
        flags: LaneFlags,
        detectors_enabled: bool,
        text: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let turn = TurnId::new(std::num::NonZeroU64::MIN);
        shared.insert_turn(turn, session);
        shared.push_text(turn, text.to_owned());
        shared.set_lane_flags(session, flags);
        let dynamic: Arc<dyn dal_agent::ext::Services> = services.clone();
        let guard = dal_tools::guard::guard_extension(dal_tools::guard::GuardConfig::disabled())?;
        TurnEndHook {
            shared: Arc::clone(shared),
            findings: guard.findings,
            detectors_enabled,
        }
        .call(
            dal_core::ext::TurnEnd {
                turn,
                stop: dal_core::Stop::EndTurn,
                overflowed: false,
            },
            HookCx::for_test(dynamic, session, Some(turn)),
        )
        .await?;
        Ok(services.all_bodies("rule_fired"))
    }

    fn streak_reported(bodies: &[String]) -> bool {
        bodies.iter().any(|body| body.contains("repetitive-turns"))
    }

    #[tokio::test]
    async fn held_turns_lane_neither_advances_nor_reports() -> Result<(), Box<dyn std::error::Error>>
    {
        let first = "the quick brown fox jumps over the lazy dog near the river bank today";
        let second = "the quick brown fox jumps over the lazy dog near the river bank today!";
        let third = "the quick brown fox jumps over the lazy dog near the river bank today?";
        let session = SessionId::new_v7();
        let shared = Arc::new(Shared::default());
        shared.insert_history(
            session,
            repetitive_turns::TurnHistory::restore(&[first.to_owned(), second.to_owned()]),
        );
        let services = Arc::new(crate::work::support::FakeServices::default());
        let held = lane_flags_from(
            true,
            true,
            &disabled_names(&["repetitive-turns"]),
            [false; 3],
        );
        let bodies = settle_turn_text(&shared, &services, session, held, true, third).await?;
        assert!(
            !streak_reported(&bodies),
            "a held turns lane reports nothing: {bodies:?}"
        );
        assert!(
            shared.commit_history(session, third).is_some(),
            "the held turn must not advance the streak"
        );
        Ok(())
    }

    #[tokio::test]
    async fn turn_end_lookup_uses_the_recorded_session_flags()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = "the quick brown fox jumps over the lazy dog near the river bank today";
        let second = "the quick brown fox jumps over the lazy dog near the river bank today!";
        let third = "the quick brown fox jumps over the lazy dog near the river bank today?";
        let session = SessionId::new_v7();
        let shared = Arc::new(Shared::default());
        shared.insert_history(
            session,
            repetitive_turns::TurnHistory::restore(&[first.to_owned(), second.to_owned()]),
        );
        let services = Arc::new(crate::work::support::FakeServices::default());
        let bodies = settle_turn_text(
            &shared,
            &services,
            session,
            LaneFlags::enabled(),
            false,
            third,
        )
        .await?;
        assert!(
            streak_reported(&bodies),
            "recorded running flags report under a held extension fallback: {bodies:?}"
        );
        let session = SessionId::new_v7();
        let shared = Arc::new(Shared::default());
        shared.insert_history(
            session,
            repetitive_turns::TurnHistory::restore(&[first.to_owned(), second.to_owned()]),
        );
        let services = Arc::new(crate::work::support::FakeServices::default());
        let held = lane_flags_from(
            true,
            true,
            &disabled_names(&["repetitive-turns"]),
            [false; 3],
        );
        let bodies = settle_turn_text(&shared, &services, session, held, true, third).await?;
        assert!(
            !streak_reported(&bodies),
            "recorded held flags stay silent under an enabled extension fallback: {bodies:?}"
        );
        Ok(())
    }

    #[test]
    fn unclaimed_lanes_all_run() {
        let flags = lane_flags_from(true, true, &[], [false; 3]);
        assert!(flags.any());
        assert_eq!(watch_notice(true, true, flags), []);
    }
}

#[cfg(test)]
mod watcher_tests {
    use std::num::NonZeroU64;

    use super::*;

    fn new_watcher(shared: &Arc<Shared>, flags: LaneFlags) -> (QualityWatcher, TurnId) {
        let turn = TurnId::new(NonZeroU64::MIN);
        let watcher = QualityWatcher {
            turn,
            shared: Arc::clone(shared),
            flags,
            collapse: collapse::State::new(collapse::Source::Prose),
            leak: control_leak::State::new(),
            text: String::new(),
            text_capped: false,
            collapse_fired: false,
        };
        (watcher, turn)
    }

    /// Feeds one megabyte in 8 KiB deltas and returns whether any delta
    /// stopped the stream.
    fn feed_megabyte(watcher: &mut QualityWatcher, chunk: &str) -> bool {
        let deltas = 1024 * 1024 / chunk.len();
        (0..deltas).any(|_| !matches!(watcher.feed(Channel::Text, chunk), StreamVerdict::Continue))
    }

    #[test]
    fn degenerate_megabyte_streams_never_stop_and_keep_state_fixed() {
        for chunk in [
            "x".repeat(8192),
            " ".repeat(8192),
            "<|endoftext|> ".repeat(8192 / 14),
            "alpha beta\n\n".repeat(8192 / 12),
        ] {
            let shared = Arc::new(Shared::default());
            let (mut watcher, turn) = new_watcher(&shared, LaneFlags::enabled());
            assert!(
                !feed_megabyte(&mut watcher, &chunk),
                "stopped on {chunk:.12?}"
            );
            assert!(watcher.text.len() <= 64 * 1024, "excerpt text is capped");
            let fires = shared
                .lock()
                .turns
                .get(&turn)
                .map_or(0, |state| state.fires.len());
            assert!(fires <= 32, "fires stay inside the per-turn cap: {fires}");
        }
    }

    #[test]
    fn held_lanes_feed_nothing_and_fire_nothing() {
        let shared = Arc::new(Shared::default());
        let (mut watcher, turn) = new_watcher(&shared, LaneFlags::held());
        assert!(!feed_megabyte(&mut watcher, &"!".repeat(8192)));
        assert!(watcher.text.is_empty(), "a held watcher keeps no text");
        assert!(!shared.lock().turns.contains_key(&turn));
    }

    #[test]
    fn only_a_user_rule_name_claims_a_lane() {
        assert_eq!(
            claimed_by(&["collapse-repetition", "other"]),
            [true, false, false]
        );
        assert_eq!(claimed_by(&["repetitive-turns"]), [false, false, true]);
        assert_eq!(claimed_by(&[]), [false; 3]);
    }
}
