//! Per-response output-stream watch: incremental matching and fire delivery.
//!
//! A [`Watch`] is created per response attempt. It snapshots the rules the
//! repeat gate admits at the turn, splits them per source kind, and feeds
//! streamed deltas through the incremental matcher. Text and reasoning deltas
//! feed their matcher directly; tool deltas flow through the typed argument
//! readers so matches see decoded added text and path-qualified items.
//!
//! States are `Fresh`, `Feeding`, `Stopped`, and `Finished`. After all events
//! of one feed call, a new unjudged `Interrupt` fire moves the watch to
//! `Stopped`. Every fire, including earlier reminders and judged matches,
//! stays in the fire list for the driver to record; only unjudged
//! `Interrupt` fires stop the stream.
//!
//! The runtime adapter in [`factory`] maps the first new unjudged interrupt
//! of a call to the `StreamWatch` interrupt verdict and every other outcome
//! to continue, because the trait carries no reminder channel.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use dal_core::ext::WatchBudget;
use dal_core::{RulesConfig, TurnId};

mod action;
pub mod factory;
mod record;
mod tool;

use super::gate::{Gate, resolve_cfg};
use super::matcher::{Compiled, StreamState};
use super::readers::EditStyle;
use super::texts::{
    fire_description, render_interrupt_text, render_reminder_text, retry_limit_note,
};
use super::value::{InterruptMode, Name, Rule, RuleAction};

/// Largest judged-context window in bytes; judged rules keep this instead of
/// the 256-byte excerpt ring.
pub const JUDGE_WINDOW_BYTES: usize = 4096;

/// Which stream location a delta came from.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SourceKind {
    /// Assistant-visible text.
    Text,
    /// Reasoning text.
    Thinking,
    /// Arguments for a named tool.
    Tool {
        /// The tool whose arguments are streamed.
        tool: Box<str>,
    },
}

/// The watch's answer for one feed or finish call.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WatchVerdict {
    /// Keep streaming.
    Continue,
    /// Stop the stream; the fires deliver with the retry.
    Stop,
}

/// A direct-watch error. The runtime trait adapter never surfaces this: the
/// loop guarantees it never feeds a watcher after `Stop` or finish.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum WatchError {
    /// The watch already stopped or finished; the loop drops it.
    #[error("WatchStopped")]
    WatchStopped,
}

/// One rule fire built eagerly when its match lands.
///
/// Field-for-field this mirrors the core seam fire pending its landing, so
/// the trait adapter is a move, not a translation.
#[derive(Clone, Debug)]
pub struct Fire {
    /// The rule that matched.
    pub rule: Name,
    /// What the fire does.
    pub action: RuleAction,
    /// Where the match landed.
    pub source: SourceKind,
    /// The item path for path-qualified tool fires.
    pub path: Option<Box<str>>,
    /// The condition source that matched.
    pub pattern: Box<str>,
    /// The stream tail at the fire, or the judge window for judged rules.
    pub excerpt: Box<str>,
    /// The canonical displayed subject.
    pub subject: Box<str>,
    /// The rule description or its first body line.
    pub description: Box<str>,
    /// The text injected with the retry; `None` for reports.
    pub inject: Option<Box<str>>,
    /// Whether a judge verdict gates delivery.
    pub judged: bool,
}

/// Per-response watch state. See the module docs for the lifecycle.
#[derive(Debug)]
pub struct Watch {
    turn: TurnId,
    budget: WatchBudget,
    default_interrupt: InterruptMode,
    max_retries: u32,
    ws_root: PathBuf,
    edit_style: EditStyle,
    text: StreamState,
    thinking: StreamState,
    tools: HashMap<Box<str>, tool::ToolStream>,
    admitted: Vec<Admitted>,
    fired_rules: HashSet<usize>,
    fires: Vec<Fire>,
    judge_window: JudgeWindow,
    state: State,
    limit_emitted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Fresh,
    Feeding,
    Stopped,
    Finished,
}

#[derive(Debug)]
struct Admitted {
    rule: Arc<Rule>,
    tools: bool,
    compiled: Vec<Arc<Compiled>>,
}

#[derive(Debug, Default)]
struct JudgeWindow {
    bytes: VecDeque<u8>,
}

impl JudgeWindow {
    fn push(&mut self, bytes: &[u8]) {
        let bytes = if bytes.len() > JUDGE_WINDOW_BYTES {
            &bytes[bytes.len() - JUDGE_WINDOW_BYTES..]
        } else {
            bytes
        };
        self.bytes.extend(bytes.iter().copied());
        while self.bytes.len() > JUDGE_WINDOW_BYTES {
            self.bytes.pop_front();
        }
    }

    fn tail_string(&self) -> String {
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        lead_aligned_string(&bytes)
    }
}

fn lead_aligned_string(tail: &[u8]) -> String {
    let start = tail
        .iter()
        .position(|byte| byte & 0xC0 != 0x80)
        .unwrap_or(tail.len());
    let tail = &tail[start..];
    let end = match tail.iter().rposition(|byte| byte & 0xC0 != 0x80) {
        Some(last) if tail.len() - last < utf8_width(tail[last]) => last,
        _ => tail.len(),
    };
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

fn utf8_width(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead >> 5 == 0b110 {
        2
    } else if lead >> 4 == 0b1110 {
        3
    } else if lead >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Creates one watch for a response attempt.
///
/// Snapshots the stream rules the gate admits at `turn`, split per source
/// kind. `cfg` supplies the default interrupt mode and the retry budget;
/// `ws_root` grounds tool path globs; `edit_style` selects the patch-argument
/// reader.
#[must_use]
pub fn create(
    set: &super::build::RuleSet,
    gate: &Gate,
    turn: TurnId,
    budget: WatchBudget,
    cfg: &RulesConfig,
    ws_root: &Path,
    edit_style: EditStyle,
) -> Watch {
    let mut admitted = Vec::new();
    let mut text = StreamState::new();
    let mut thinking = StreamState::new();

    for rule in &set.stream {
        let repeat = resolve_cfg(rule, cfg);
        if !gate.eligible(rule, turn, &repeat) {
            continue;
        }
        let Some(compiled) = set.compiled_conditions(rule.name.as_str()) else {
            continue;
        };
        if compiled.is_empty() {
            continue;
        }
        let watches_text = rule.scope.text;
        let watches_thinking = rule.scope.thinking;
        let watches_tools = match &rule.scope.tools {
            super::value::ToolScope::All => true,
            super::value::ToolScope::Tools(patterns) => !patterns.is_empty(),
        };
        if !watches_text && !watches_thinking && !watches_tools {
            continue;
        }
        let slot = admitted.len();
        if watches_text {
            text.add_rule(slot, compiled);
        }
        if watches_thinking {
            thinking.add_rule(slot, compiled);
        }
        admitted.push(Admitted {
            rule: Arc::clone(rule),
            tools: watches_tools,
            compiled: compiled.to_vec(),
        });
    }

    Watch {
        turn,
        budget,
        default_interrupt: cfg.interrupt,
        max_retries: cfg.max_retries,
        ws_root: ws_root.to_path_buf(),
        edit_style,
        text,
        thinking,
        tools: HashMap::new(),
        admitted,
        fired_rules: HashSet::new(),
        fires: Vec::new(),
        judge_window: JudgeWindow::default(),
        state: State::Fresh,
        limit_emitted: false,
    }
}

impl Watch {
    /// The turn this watch observes.
    #[must_use]
    pub fn turn(&self) -> TurnId {
        self.turn
    }

    /// Every fire in delivery order, including earlier reminders.
    #[must_use]
    pub fn fires(&self) -> &[Fire] {
        &self.fires
    }

    /// Feeds one delta and answers with the watch verdict.
    ///
    /// Empty bytes return `Continue` without touching matcher state. After
    /// all events of the call, a new `Interrupt` fire moves the watch to
    /// `Stopped` and answers `Stop`.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::WatchStopped`] if the watch already stopped or finished.
    pub fn feed(&mut self, source: SourceKind, bytes: &str) -> Result<WatchVerdict, WatchError> {
        if matches!(self.state, State::Stopped | State::Finished) {
            return Err(WatchError::WatchStopped);
        }
        if bytes.is_empty() {
            return Ok(WatchVerdict::Continue);
        }
        self.state = State::Feeding;
        let interrupts_before = self.has_interrupt();
        match source {
            SourceKind::Text => {
                self.judge_window.push(bytes.as_bytes());
                self.feed_state_text(bytes);
            }
            SourceKind::Thinking => {
                self.judge_window.push(bytes.as_bytes());
                self.feed_state_thinking(bytes);
            }
            SourceKind::Tool { tool } => self.feed_tool(&tool, bytes),
        }
        Ok(self.settle_verdict(interrupts_before))
    }

    /// Feeds added text for one tool item without parsing JSON.
    ///
    /// The offline `dalgon rules test` prover and other non-JSON tool
    /// sources call this with the item path and its added text. A missing
    /// path parks path-gated matches until [`Watch::finish`] drops them.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::WatchStopped`] if the watch already stopped or finished.
    pub fn feed_added(
        &mut self,
        tool: &str,
        path: Option<&str>,
        text: &str,
    ) -> Result<WatchVerdict, WatchError> {
        if matches!(self.state, State::Stopped | State::Finished) {
            return Err(WatchError::WatchStopped);
        }
        if text.is_empty() {
            return Ok(WatchVerdict::Continue);
        }
        self.state = State::Feeding;
        let interrupts_before = self.has_interrupt();
        self.ensure_tool(tool);
        if let Some(stream) = self.tools.get_mut(tool) {
            stream.item_path = path.map(str::to_owned);
        }
        self.apply_added(tool, text);
        Ok(self.settle_verdict(interrupts_before))
    }

    /// Downgrades the interrupt budget in place; later matches resolve
    /// against the new budget while fires and matcher state carry over.
    pub fn set_budget(&mut self, budget: WatchBudget) {
        self.budget = budget;
        if self.state == State::Stopped {
            self.state = State::Feeding;
        }
    }

    /// Closes every open reader, handles its events, and finishes the watch.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::WatchStopped`] if the watch already stopped or finished.
    pub fn finish(&mut self) -> Result<WatchVerdict, WatchError> {
        if matches!(self.state, State::Stopped | State::Finished) {
            return Err(WatchError::WatchStopped);
        }
        let tools: Vec<Box<str>> = self.tools.keys().cloned().collect();
        for tool in &tools {
            let mut events = Vec::new();
            if let Some(stream) = self.tools.get_mut(tool) {
                stream.reader.close(&mut events);
            }
            self.apply_tool_events(tool, &events);
        }
        for stream in self.tools.values_mut() {
            stream.pending.clear();
            stream.item_path = None;
        }
        self.state = State::Finished;
        if self.has_interrupt() {
            Ok(WatchVerdict::Stop)
        } else {
            Ok(WatchVerdict::Continue)
        }
    }

    /// Returns the once-per-turn retry-cap notice when `interrupts` reaches
    /// the configured budget.
    pub fn notice_limit(&mut self, interrupts: u32) -> Option<Box<str>> {
        if self.limit_emitted || interrupts < self.max_retries {
            return None;
        }
        self.limit_emitted = true;
        Some(retry_limit_note(self.max_retries as usize).into_boxed_str())
    }

    /// A judged fire never stops the sync watch: its interrupt waits on the
    /// async bool verdict, which the judged consumer delivers.
    fn has_interrupt(&self) -> bool {
        self.fires
            .iter()
            .any(|fire| fire.action == RuleAction::Interrupt && !fire.judged)
    }

    fn feed_state_text(&mut self, bytes: &str) {
        let mut matched = Vec::new();
        self.text.feed(bytes.as_bytes(), &mut matched);
        for fire in matched {
            self.emit_match(fire, SourceKind::Text, None);
        }
    }

    fn feed_state_thinking(&mut self, bytes: &str) {
        let mut matched = Vec::new();
        self.thinking.feed(bytes.as_bytes(), &mut matched);
        for fire in matched {
            self.emit_match(fire, SourceKind::Thinking, None);
        }
    }

    fn settle_verdict(&mut self, interrupts_before: bool) -> WatchVerdict {
        if !interrupts_before && self.has_interrupt() {
            self.state = State::Stopped;
            WatchVerdict::Stop
        } else {
            WatchVerdict::Continue
        }
    }

    fn emit_match(
        &mut self,
        fire: super::matcher::MatchFire,
        source: SourceKind,
        path: Option<String>,
    ) {
        if self.fired_rules.contains(&fire.rule) {
            return;
        }
        let slot = fire.rule;
        let pattern: Box<str> = fire.condition.src().into();
        let excerpt = fire.excerpt;
        self.push_fire(
            slot,
            source,
            path.map(std::string::String::into_boxed_str),
            pattern,
            excerpt,
        );
    }

    fn emit_tool_fire(
        &mut self,
        slot: usize,
        tool: &str,
        path: Option<String>,
        pattern: Box<str>,
        excerpt: String,
    ) {
        if self.fired_rules.contains(&slot) {
            return;
        }
        let source = SourceKind::Tool { tool: tool.into() };
        self.push_fire(
            slot,
            source,
            path.map(std::string::String::into_boxed_str),
            pattern,
            excerpt,
        );
    }

    fn push_fire(
        &mut self,
        slot: usize,
        source: SourceKind,
        path: Option<Box<str>>,
        pattern: Box<str>,
        excerpt: String,
    ) {
        let admitted = Arc::clone(&self.admitted[slot].rule);
        let action = action::action_of(&admitted, &source, self.budget, self.default_interrupt);
        let subject = action::subject_of(&source, path.as_deref()).render();
        let excerpt = if admitted.judge.is_some() {
            self.judge_window.tail_string().into_boxed_str()
        } else {
            excerpt.into_boxed_str()
        };
        let inject = match action {
            RuleAction::Interrupt => Some(render_interrupt_text(&admitted).into_boxed_str()),
            RuleAction::Remind => {
                let subject = action::subject_of(&source, path.as_deref());
                Some(render_reminder_text(&admitted, subject).into_boxed_str())
            }
            RuleAction::Report => None,
        };
        self.fired_rules.insert(slot);
        self.fires.push(Fire {
            rule: admitted.name.clone(),
            action,
            source,
            path,
            pattern,
            excerpt,
            subject: subject.into_boxed_str(),
            description: fire_description(&admitted).to_owned().into_boxed_str(),
            inject,
            judged: admitted.judge.is_some(),
        });
    }
}

#[cfg(test)]
mod tests;
