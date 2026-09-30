use super::{Engine, report};
use dal_agent::ext::hooks::{StreamWatch, TurnInfo, WatchFactory};
use dal_core::TurnId;
use dal_core::ext::{Channel, StreamVerdict};
use regex::RegexSet;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum G8Rule {
    Placeholder,
    BareTodo,
    EmptyComment,
    EqTrue,
    DebugPrint,
    SectionDivider,
}

impl G8Rule {
    pub const ALL: [G8Rule; 6] = [
        Self::Placeholder,
        Self::BareTodo,
        Self::EmptyComment,
        Self::EqTrue,
        Self::DebugPrint,
        Self::SectionDivider,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Placeholder => "placeholder",
            Self::BareTodo => "bare_todo",
            Self::EmptyComment => "empty_comment",
            Self::EqTrue => "eq_true",
            Self::DebugPrint => "debug_print",
            Self::SectionDivider => "section_divider",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<G8Rule> {
        match name {
            "placeholder" => Some(Self::Placeholder),
            "bare_todo" => Some(Self::BareTodo),
            "empty_comment" => Some(Self::EmptyComment),
            "eq_true" => Some(Self::EqTrue),
            "debug_print" => Some(Self::DebugPrint),
            "section_divider" => Some(Self::SectionDivider),
            _ => None,
        }
    }

    fn pattern(self) -> &'static str {
        match self {
            Self::Placeholder => r"\.{3,}|…",
            Self::BareTodo => r"(?i)\btodo\b",
            Self::EmptyComment => r"(//|#|--)\s*$",
            Self::EqTrue => r"===\s*true\b",
            Self::DebugPrint => r"console\.log\(|print\(.*debug",
            Self::SectionDivider => r"-{4,}|={4,}|#{4,}",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|rule| *rule == self)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub text: Box<str>,
    pub positive: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SampleSet {
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone, Default)]
pub struct Calibration {
    pub sets: BTreeMap<G8Rule, SampleSet>,
}

impl Calibration {
    #[must_use]
    pub fn none() -> Calibration {
        Self::default()
    }

    /// True when at least 50 samples match the rule's pattern and at least 95 percent of the matching samples are labeled positive.
    pub fn admits(&self, rule: G8Rule) -> bool {
        let Some(samples) = self.sets.get(&rule) else {
            return false;
        };
        let Ok(regexes) = REGEXES.as_ref() else {
            return false;
        };
        let mut matching = 0_usize;
        let mut positive = 0_usize;
        for sample in &samples.samples {
            if regexes.matches(&sample.text).matched(rule.index()) {
                matching = matching.saturating_add(1);
                if sample.positive {
                    positive = positive.saturating_add(1);
                }
            }
        }
        if matching < 50 {
            return false;
        }
        let required = matching.saturating_mul(95).div_ceil(100);
        positive >= required
    }
}

static REGEXES: LazyLock<Result<RegexSet, regex::Error>> =
    LazyLock::new(|| RegexSet::new(G8Rule::ALL.map(G8Rule::pattern)));

pub(super) struct Router {
    engine: Arc<Engine>,
}

impl Router {
    pub(super) fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }
}

impl WatchFactory for Router {
    fn start(&self, turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        if !self.engine.cfg.enabled {
            return None;
        }
        let state = self.engine.state.lock().ok()?;
        let session_id = state.turns.get(&turn.turn)?;
        let session = state.sessions.get(session_id)?;
        if session
            .turn
            .as_ref()
            .is_none_or(|active| active.id != turn.turn)
        {
            return None;
        }
        drop(state);
        Some(Box::new(Watch {
            turn: turn.turn,
            carry: String::new(),
            counts: BTreeMap::new(),
            fired: BTreeSet::new(),
            engine: Arc::clone(&self.engine),
        }))
    }
}

pub(super) struct Watch {
    turn: TurnId,
    carry: String,
    counts: BTreeMap<G8Rule, u32>,
    fired: BTreeSet<G8Rule>,
    engine: Arc<Engine>,
}

impl StreamWatch for Watch {
    fn feed(&mut self, channel: Channel, delta: &str) -> StreamVerdict {
        if !matches!(channel, Channel::ToolArgs { .. }) {
            return StreamVerdict::Continue;
        }
        self.carry.push_str(delta);
        self.drain_lines()
    }

    fn finish(&mut self) -> StreamVerdict {
        let line = std::mem::take(&mut self.carry);
        let verdict = self.check_line(&line).unwrap_or(StreamVerdict::Continue);
        self.merge_counts();
        verdict
    }
}

impl Watch {
    fn drain_lines(&mut self) -> StreamVerdict {
        let mut cursor = 0;
        let mut line_start = 0;
        while cursor < self.carry.len() {
            let byte = self.carry.as_bytes()[cursor];
            let delimiter = if byte == b'\n' {
                Some(1)
            } else if byte == b'\\'
                && self.carry.as_bytes().get(cursor.saturating_add(1)) == Some(&b'n')
            {
                Some(2)
            } else {
                None
            };
            let Some(delimiter) = delimiter else {
                cursor = cursor.saturating_add(1);
                continue;
            };
            let line = self.carry[line_start..cursor].to_owned();
            cursor = cursor.saturating_add(delimiter);
            line_start = cursor;
            if let Some(verdict) = self.check_line(&line) {
                self.carry.drain(..line_start);
                return verdict;
            }
        }
        if line_start > 0 {
            self.carry.drain(..line_start);
        }
        StreamVerdict::Continue
    }

    fn check_line(&mut self, line: &str) -> Option<StreamVerdict> {
        let regexes = match REGEXES.as_ref() {
            Ok(regexes) => regexes,
            Err(_) => return None,
        };
        let matches = regexes.matches(line);
        for rule in G8Rule::ALL {
            if !matches.matched(rule.index()) {
                continue;
            }
            if line.contains(&format!("guard-allow({})", rule.name())) {
                continue;
            }
            let count = self.counts.entry(rule).or_default();
            *count = count.saturating_add(1);
            if self.engine.cfg.calibrated.contains(&rule) && self.fire_once(rule) {
                return Some(StreamVerdict::Interrupt {
                    rule: rule.name().into(),
                    inject: report::stream_interrupt(rule.name()).into(),
                });
            }
        }
        None
    }

    fn fire_once(&mut self, rule: G8Rule) -> bool {
        if !self.fired.insert(rule) {
            return false;
        }
        let Ok(mut state) = self.engine.state.lock() else {
            self.fired.remove(&rule);
            return false;
        };
        let Some(session_id) = state.turns.get(&self.turn).copied() else {
            self.fired.remove(&rule);
            return false;
        };
        let Some(session) = state.sessions.get_mut(&session_id) else {
            self.fired.remove(&rule);
            return false;
        };
        let Some(turn) = session.turn.as_mut().filter(|turn| turn.id == self.turn) else {
            self.fired.remove(&rule);
            return false;
        };
        turn.fired.insert(rule)
    }

    fn merge_counts(&self) {
        let Ok(mut state) = self.engine.state.lock() else {
            return;
        };
        let Some(session_id) = state.turns.get(&self.turn).copied() else {
            return;
        };
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return;
        };
        let Some(turn) = session.turn.as_mut().filter(|turn| turn.id == self.turn) else {
            return;
        };
        for (rule, count) in &self.counts {
            let total = turn.stream_counts.entry(*rule).or_default();
            *total = total.saturating_add(*count);
        }
    }
}
