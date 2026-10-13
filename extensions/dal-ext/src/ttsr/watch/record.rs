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
    /// The pack origin of the fired rule; absent for user and project files,
    /// whose path is the origin. Additive so earlier readers stay valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
}

impl Watch {
    /// A report fire serializes `"action":"report"` with `"entry":null`. A
    /// judged pass writes no record; the caller skips this helper then. An
    /// impossible fire index returns `None` rather than a JSON fallback.
    #[must_use]
    pub fn record_body(
        &self,
        fire: usize,
        at: jiff::Timestamp,
        entry: Option<EntryId>,
    ) -> Option<Box<str>> {
        let fired = self.fires.get(fire)?;
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
            origin: self.origin_label(fire),
        };
        sonic_rs::to_string(&body).ok().map(String::into_boxed_str)
    }

    /// Records one delivered fire in the repeat gate after its batch is
    /// durable. A judged fire records regardless of its verdict: with an
    /// entry for a delivered reminder, without one for a pass.
    pub fn gate_record(&self, gate: &mut Gate, fire: usize, entry: Option<EntryId>) {
        let Some(fired) = self.fires.get(fire) else {
            return;
        };
        match entry {
            Some(entry) => gate.record(fired.rule.as_str(), self.turn, entry),
            None => gate.record_turn(fired.rule.as_str(), self.turn),
        }
    }
}
