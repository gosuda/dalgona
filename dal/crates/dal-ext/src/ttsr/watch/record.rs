//! Journal payloads and repeat-gate latches for rule fires.
//!
//! [`Watch::record_body`] serializes one fire with keys in delivery order.
//! [`Watch::gate_record`] latches the fire in the repeat gate after its
//! batch is durable: with a journal entry for a delivered reminder, with
//! only the turn for a judged pass that wrote no record.

use dal_core::EntryId;

use super::super::gate::Gate;
use super::Watch;

#[derive(serde::Serialize)]
struct Body<'a> {
    v: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    at: String,
    turn: u64,
    rule: &'a str,
    action: &'static str,
    subject: &'a str,
    pattern: &'a str,
    excerpt: &'a str,
    entry: Option<u64>,
}

impl Watch {
    /// Builds the journal payload for one fire, with keys in delivery order.
    ///
    /// A report fire serializes `"action":"report"` with `"entry":null`. A
    /// judged pass writes no record; the caller skips this helper then.
    #[must_use]
    pub fn record_body(
        &self,
        fire: usize,
        at: jiff::Timestamp,
        entry: Option<EntryId>,
    ) -> Box<str> {
        let Some(fired) = self.fires.get(fire) else {
            debug_assert!(false, "rule-fire index out of range");
            return "{}".into();
        };
        let body = Body {
            v: 1,
            kind: "rule_fired",
            at: format!("{at:.3}"),
            turn: self.turn.get(),
            rule: fired.rule.as_str(),
            action: fired.action.as_str(),
            subject: &fired.subject,
            pattern: &fired.pattern,
            excerpt: &fired.excerpt,
            entry: entry.map(EntryId::get),
        };
        sonic_rs::to_string(&body)
            .unwrap_or_else(|_| "{}".to_owned())
            .into_boxed_str()
    }

    /// Records one delivered fire in the repeat gate after its batch is
    /// durable. A judged fire records regardless of its verdict: with an
    /// entry for a delivered reminder, without one for a pass.
    pub fn gate_record(&self, gate: &mut Gate, fire: usize, entry: Option<EntryId>) {
        let Some(fired) = self.fires.get(fire) else {
            debug_assert!(false, "rule-fire index out of range");
            return;
        };
        match entry {
            Some(entry) => gate.record(fired.rule.as_str(), self.turn, entry),
            None => gate.record_turn(fired.rule.as_str(), self.turn),
        }
    }
}
