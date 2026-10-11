//! Per-session repeat eligibility, restored from reminder visibility.

use std::collections::HashMap;

use dal_core::{EntryId, RulesConfig, TurnId};

use super::{Name, RepeatMode, Rule};

/// One persisted fire and whether its reminder is still visible to the model.
///
/// The session layer builds these rows from fired-rule records and the store's
/// `in_context` result. A compacted-away reminder is supplied with `visible`
/// set to `false`, so restoring a gate never scans transcript contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateSeed {
    /// The rule whose reminder was persisted.
    pub rule: Name,
    /// The turn in which the rule fired.
    pub turn: TurnId,
    /// The durable entry id of the reminder.
    pub entry: EntryId,
    /// Whether the model can still see this reminder in its current context.
    pub visible: bool,
}

/// Effective repeat behavior after applying a rule's optional overrides.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepeatCfg {
    /// The rule's repeat mode.
    pub mode: RepeatMode,
    /// The minimum number of turns between fires; valid settings are `1..=1000`.
    pub gap: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Fire {
    turn: TurnId,
    entry: Option<EntryId>,
}

/// Per-session record of each rule's newest visible reminder.
///
/// `eligible` is allocation-free. Cloning or forking a gate starts empty;
/// child sessions restore their own visible reminders from a fresh seed.
#[must_use]
#[derive(Debug, Default)]
pub struct Gate {
    fires: HashMap<Box<str>, Fire>,
}

impl Clone for Gate {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl Gate {
    /// Rebuilds a gate from the reminders visible in the supplied snapshot.
    ///
    /// Invisible rows are discarded. If a seed has multiple visible fires for
    /// one rule, the greatest `(turn, entry)` pair wins, independent of input
    /// order.
    pub fn restore(seed: &[GateSeed]) -> Self {
        let mut gate = Self::default();
        for row in seed.iter().filter(|row| row.visible) {
            gate.record(row.rule.as_str(), row.turn, row.entry);
        }
        gate
    }

    /// Creates an empty gate for a forked session.
    pub fn fork(&self) -> Self {
        self.clone()
    }

    /// Reports whether the rule may fire on this turn under `cfg`.
    ///
    /// Once rules are blocked while any reminder for them remains visible.
    /// After-gap rules become eligible at `last_turn + gap`; subtraction is
    /// used instead of addition so turns near the identifier limit stay safe.
    #[must_use]
    pub fn eligible(&self, rule: &Rule, turn: TurnId, cfg: &RepeatCfg) -> bool {
        let Some(last) = self.fires.get(rule.name.as_str()) else {
            return true;
        };
        if cfg.mode != RepeatMode::AfterGap {
            return last.entry.is_none() && last.turn != turn;
        }

        turn.get()
            .checked_sub(last.turn.get())
            .is_some_and(|elapsed| elapsed >= u64::from(cfg.gap.max(1)))
    }

    /// Records a durably journaled reminder as visible for `name` in `turn`.
    ///
    /// Recording an older event cannot replace a newer fire restored from the
    /// session snapshot.
    pub fn record(&mut self, name: &str, turn: TurnId, entry: EntryId) {
        self.record_fire(
            name,
            Fire {
                turn,
                entry: Some(entry),
            },
        );
    }

    /// Remembers a report or judged pass for the current turn without claiming
    /// a visible reminder entry.
    pub(crate) fn record_turn(&mut self, name: &str, turn: TurnId) {
        self.record_fire(name, Fire { turn, entry: None });
    }

    fn record_fire(&mut self, name: &str, fire: Fire) {
        let Some(previous) = self.fires.get_mut(name) else {
            self.fires.insert(Box::<str>::from(name), fire);
            return;
        };

        if fire.turn > previous.turn {
            previous.turn = fire.turn;
            if fire.entry.is_some() {
                previous.entry = fire.entry;
            }
        } else if fire.turn == previous.turn && fire.entry.is_some() && fire.entry > previous.entry
        {
            previous.entry = fire.entry;
        }
    }
}

/// Resolves each optional rule repeat override against `[rules]` configuration.
#[must_use]
pub fn resolve_cfg(rule: &Rule, cfg: &RulesConfig) -> RepeatCfg {
    RepeatCfg {
        mode: rule.repeat_mode.unwrap_or(cfg.repeat),
        gap: rule.repeat_gap.unwrap_or(cfg.repeat_gap),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;
    use crate::ttsr::{Origin, ScopeSpec, ToolScope};

    fn turn(value: u64) -> TurnId {
        TurnId::new(NonZeroU64::new(value).expect("test turns are nonzero"))
    }

    fn entry(value: u64) -> EntryId {
        EntryId::new(NonZeroU64::new(value).expect("test entries are nonzero"))
    }

    fn seed(rule: &str, turn_value: u64, entry_value: u64, visible: bool) -> GateSeed {
        GateSeed {
            rule: Name::parse(rule).expect("test rule name is valid"),
            turn: turn(turn_value),
            entry: entry(entry_value),
            visible,
        }
    }

    fn rule(name: &str, repeat_mode: Option<RepeatMode>, repeat_gap: Option<u16>) -> Rule {
        Rule {
            name: Name::parse(name).expect("test rule name is valid"),
            origin: Origin::Record {
                plugin: "test".into(),
            },
            description: None,
            conditions: Vec::new(),
            scope: ScopeSpec {
                text: true,
                thinking: false,
                tools: ToolScope::Tools(Vec::new()),
            },
            globs: None,
            agents: None,
            always_apply: false,
            report: false,
            enabled: true,
            interrupt_mode: None,
            repeat_mode,
            repeat_gap,
            judge: None,
            body: String::new(),
        }
    }

    fn after_gap_by_turn_count(last_fire: Option<u64>, turn: u64, gap: u16) -> bool {
        let Some(mut cursor) = last_fire else {
            return true;
        };
        let needed = u64::from(gap.max(1));
        let mut elapsed = 0;
        while cursor < turn && elapsed < needed {
            cursor += 1;
            elapsed += 1;
        }
        elapsed >= needed
    }

    #[test]
    fn repeat_gate_eligibility() {
        let once_rule = rule("once", None, None);
        let gap_rule = rule("gap", None, None);
        let once_cfg = RepeatCfg {
            mode: RepeatMode::Once,
            gap: 10,
        };
        let gap_cfg = RepeatCfg {
            mode: RepeatMode::AfterGap,
            gap: 2,
        };
        let mut gate = Gate::default();
        gate.record("once", turn(3), entry(1));
        gate.record("gap", turn(3), entry(2));

        assert!(!gate.eligible(&once_rule, turn(3), &once_cfg));
        assert!(!gate.eligible(&once_rule, turn(9), &once_cfg));
        assert!(!gate.eligible(&gap_rule, turn(4), &gap_cfg));
        assert!(gate.eligible(&gap_rule, turn(5), &gap_cfg));
    }

    #[test]
    fn zero_gap_still_blocks_a_second_fire_in_the_same_turn() {
        let rule = rule("r", None, None);
        let cfg = RepeatCfg {
            mode: RepeatMode::AfterGap,
            gap: 0,
        };
        let mut gate = Gate::default();
        gate.record("r", turn(3), entry(1));

        assert!(!gate.eligible(&rule, turn(3), &cfg));
        assert!(gate.eligible(&rule, turn(4), &cfg));
    }

    #[test]
    fn restored_after_gap_matches_independent_turn_count_oracle() {
        let seeds = [
            seed("r", 3, 5, true),
            seed("r", 6, 11, true),
            seed("r", 18, 99, false),
            seed("other", 17, 90, true),
        ];
        let mut gate = Gate::restore(&seeds);
        let rule = rule("r", None, None);
        let cfg = RepeatCfg {
            mode: RepeatMode::AfterGap,
            gap: 3,
        };
        let mut last_visible_turn = None;
        for row in &seeds {
            if row.visible && row.rule.as_str() == rule.name.as_str() {
                last_visible_turn = Some(last_visible_turn.unwrap_or(0).max(row.turn.get()));
            }
        }

        for current in 1..=20 {
            let expected = after_gap_by_turn_count(last_visible_turn, current, cfg.gap);
            assert_eq!(
                gate.eligible(&rule, turn(current), &cfg),
                expected,
                "turn {current} with last visible fire at {last_visible_turn:?}"
            );
            if expected {
                gate.record(rule.name.as_str(), turn(current), entry(100 + current));
                last_visible_turn = Some(current);
            }
        }
    }

    #[test]
    fn restore_selects_latest_visible_fire_deterministically() {
        let rows = [
            seed("r", 8, 20, true),
            seed("r", 8, 30, true),
            seed("r", 9, 40, false),
            seed("r", 7, 50, true),
        ];
        let gate = Gate::restore(&rows);
        let reversed = Gate::restore(&rows.iter().rev().cloned().collect::<Vec<_>>());
        let newest = Fire {
            turn: turn(8),
            entry: Some(entry(30)),
        };

        assert_eq!(gate.fires.get("r"), Some(&newest));
        assert_eq!(reversed.fires.get("r"), Some(&newest));
    }

    #[test]
    fn compaction_visibility_removes_once_suppression() {
        let rule = rule("r", None, None);
        let once = RepeatCfg {
            mode: RepeatMode::Once,
            gap: 10,
        };
        let before_compaction = Gate::restore(&[seed("r", 3, 4, true)]);
        assert!(!before_compaction.eligible(&rule, turn(4), &once));

        let after_compaction = Gate::restore(&[seed("r", 3, 4, false)]);
        assert!(after_compaction.eligible(&rule, turn(4), &once));
    }

    #[test]
    fn recorded_judged_pass_stays_gated_while_its_reminder_is_visible() {
        let rule = rule("judged", None, None);
        let once = RepeatCfg {
            mode: RepeatMode::Once,
            gap: 1,
        };
        let mut gate = Gate::default();
        gate.record("judged", turn(4), entry(12));
        let restored = Gate::restore(&[seed("judged", 4, 12, true)]);

        assert!(!gate.eligible(&rule, turn(4), &once));
        assert!(!restored.eligible(&rule, turn(5), &once));
    }

    #[test]
    fn nonvisible_fires_count_for_gap_without_erasing_visible_reminders() {
        let rule = rule("r", None, None);
        let once = RepeatCfg {
            mode: RepeatMode::Once,
            gap: 2,
        };
        let after_gap = RepeatCfg {
            mode: RepeatMode::AfterGap,
            gap: 2,
        };
        let mut gate = Gate::default();
        gate.record_turn("r", turn(4));

        assert!(!gate.eligible(&rule, turn(4), &once));
        assert!(gate.eligible(&rule, turn(5), &once));
        assert!(!gate.eligible(&rule, turn(5), &after_gap));
        assert!(gate.eligible(&rule, turn(6), &after_gap));

        gate.record("r", turn(7), entry(12));
        gate.record_turn("r", turn(8));
        assert!(!gate.eligible(&rule, turn(9), &once));
    }
    #[test]
    fn after_gap_is_safe_at_large_turn_ids() {
        let rule = rule("r", None, None);
        let gap = RepeatCfg {
            mode: RepeatMode::AfterGap,
            gap: 2,
        };
        let max = u64::MAX;
        let mut gate = Gate::default();
        gate.record("r", turn(max - 1), entry(1));

        assert!(!gate.eligible(&rule, turn(max), &gap));
        assert!(!gate.eligible(&rule, turn(max - 2), &gap));

        let mut boundary = Gate::default();
        boundary.record("r", turn(max - 2), entry(2));
        assert!(!boundary.eligible(&rule, turn(max - 1), &gap));
        assert!(boundary.eligible(&rule, turn(max), &gap));
    }

    #[test]
    fn cloned_and_forked_gates_start_empty() {
        let rule = rule("r", None, None);
        let once = RepeatCfg {
            mode: RepeatMode::Once,
            gap: 1,
        };
        let mut gate = Gate::default();
        gate.record("r", turn(1), entry(1));
        assert!(!gate.eligible(&rule, turn(2), &once));

        assert!(gate.clone().eligible(&rule, turn(2), &once));
        assert!(gate.fork().eligible(&rule, turn(2), &once));
    }

    #[test]
    fn resolve_cfg_uses_rule_overrides_per_field() {
        let defaults = RulesConfig {
            repeat: RepeatMode::AfterGap,
            repeat_gap: 10,
            ..RulesConfig::default()
        };

        let both = resolve_cfg(&rule("both", Some(RepeatMode::Once), Some(2)), &defaults);
        let mode_only = resolve_cfg(&rule("mode", Some(RepeatMode::Once), None), &defaults);
        let gap_only = resolve_cfg(&rule("gap", None, Some(3)), &defaults);

        assert_eq!(both.mode, RepeatMode::Once);
        assert_eq!(both.gap, 2);
        assert_eq!(mode_only.mode, RepeatMode::Once);
        assert_eq!(mode_only.gap, 10);
        assert_eq!(gap_only.mode, RepeatMode::AfterGap);
        assert_eq!(gap_only.gap, 3);
    }
}
