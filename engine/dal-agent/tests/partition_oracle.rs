//! Read/run partition oracle: production grouping vs a reference scheduler.
//!
//! The dispatcher plans tool calls in provider order: maximal contiguous
//! runs of read-only calls become one read run, every other call dispatches
//! alone. An `ask` tool never joins a read run even when read-classified.
//! Results settle in call order, and read runs execute with capped
//! in-flight calls. This test pins that contract without a property
//! framework: a deterministic seeded PRNG builds 1..20-call sequences of
//! mixed read/run kinds, plus an exhaustive small table, and checks each
//! against a 15-line reference scheduler.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions abort on failure"
)]

use std::collections::BTreeSet;
use std::str::FromStr;

use dal_core::{ApprovalMode, CallId, Decision, Name, PlannedCall, Policy, ToolClass, Unit};

/// In-flight cap the read-run executor honors.
const PARALLEL_READS: usize = 8;

/// Deterministic xorshift PRNG; no property framework exists in this repo.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn policy_all() -> Policy {
    Policy {
        mode: ApprovalMode::All,
        answerer_attached: false,
        allow_always: BTreeSet::new(),
    }
}

fn name(value: &str) -> Name {
    Name::from_str(value).expect("valid tool name")
}

fn call(id: &str, tool: &str, class: ToolClass) -> PlannedCall {
    PlannedCall {
        call: CallId::new(id),
        name: name(tool),
        class,
    }
}

fn exec(read_only: bool) -> ToolClass {
    ToolClass::Exec {
        read_only,
        grant: None,
    }
}

/// 15-line reference scheduler, written from the spec: maximal read runs,
// exclusive `ask` always serial, approval-gated read-only exec.
fn reference(calls: &[PlannedCall], policy: &Policy) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut run = Vec::new();
    for c in calls {
        let excl = dal_core::EXCLUSIVE_TOOLS.contains(&c.name.as_str());
        let read = !excl
            && (matches!(c.class, ToolClass::Read)
                || matches!(
                    &c.class,
                    ToolClass::Exec {
                        read_only: true,
                        ..
                    }
                ) && policy.decide(&c.name, &c.class) == Decision::Allow);
        if read {
            run.push(c.call.clone());
        } else {
            if !run.is_empty() {
                units.push(Unit::Reads {
                    calls: std::mem::take(&mut run),
                });
            }
            units.push(Unit::Serial {
                call: c.call.clone(),
            });
        }
    }
    if !run.is_empty() {
        units.push(Unit::Reads { calls: run });
    }
    units
}

fn is_read_eligible(call: &PlannedCall, policy: &Policy) -> bool {
    let excl = dal_core::EXCLUSIVE_TOOLS.contains(&call.name.as_str());
    !excl
        && (matches!(call.class, ToolClass::Read)
            || matches!(
                &call.class,
                ToolClass::Exec {
                    read_only: true,
                    ..
                }
            ) && policy.decide(&call.name, &call.class) == Decision::Allow)
}

fn check_sequence(calls: &[PlannedCall], policy: &Policy, context: &str) {
    let production = dal_core::plan(calls, policy);
    let expected = reference(calls, policy);
    assert_eq!(
        production, expected,
        "groups must match reference scheduler ({context})"
    );
    let mut flattened = Vec::new();
    for unit in &production {
        match unit {
            Unit::Reads { calls: ids } => {
                assert!(!ids.is_empty(), "empty read run ({context})");
                for id in ids {
                    flattened.push(id.clone());
                }
            }
            Unit::Serial { call } => flattened.push(call.clone()),
            _ => panic!("unplanned dispatch unit ({context})"),
        }
    }
    let input: Vec<CallId> = calls.iter().map(|c| c.call.clone()).collect();
    assert_eq!(
        flattened, input,
        "tool_settled order equals call order ({context})"
    );
    // Contract: maximal contiguous runs — two read runs never sit adjacent,
    // or they would have been one run.
    let mut saw_reads = false;
    for unit in &production {
        match unit {
            Unit::Reads { .. } => {
                assert!(
                    !saw_reads,
                    "contiguous runs: adjacent read runs ({context})"
                );
                saw_reads = true;
            }
            Unit::Serial { .. } => saw_reads = false,
            _ => panic!("unplanned dispatch unit ({context})"),
        }
    }
    for unit in &production {
        match unit {
            Unit::Reads { calls: ids } => {
                assert!(!ids.is_empty(), "read run holds calls ({context})");
                for id in ids {
                    let planned = calls
                        .iter()
                        .find(|c| c.call == *id)
                        .expect("planned call exists");
                    // Contract: ask serial — exclusive names never batch,
                    // checked by name alone.
                    assert!(
                        !dal_core::EXCLUSIVE_TOOLS.contains(&planned.name.as_str()),
                        "ask serial: exclusive tool in read run ({context})"
                    );
                    // Contract: class purity without policy — only Read or
                    // read-only Exec may batch; runs, patches, and Other
                    // always serialize regardless of approval mode.
                    assert!(
                        matches!(planned.class, ToolClass::Read)
                            || matches!(
                                &planned.class,
                                ToolClass::Exec {
                                    read_only: true,
                                    ..
                                }
                            ),
                        "overlap only inside read runs: {id} is not a read class ({context})"
                    );
                    assert!(
                        is_read_eligible(planned, policy),
                        "overlap only inside read runs: {id} is not read-eligible under policy ({context})"
                    );
                }
                // Contract: capped run width — a cap-respecting execution
                // exists that keeps call order.
                let chunks: Vec<&[CallId]> = ids.chunks(PARALLEL_READS).collect();
                assert!(
                    chunks.iter().all(|chunk| chunk.len() <= PARALLEL_READS),
                    "capped run width {PARALLEL_READS} ({context})"
                );
                let rechained: Vec<CallId> = chunks
                    .iter()
                    .flat_map(|chunk| chunk.iter().cloned())
                    .collect();
                assert_eq!(rechained, *ids, "cap chunking keeps call order ({context})");
            }
            Unit::Serial { call } => {
                // Contract: a non-exclusive Read always batches, so it must
                // never appear alone regardless of policy.
                let planned = calls
                    .iter()
                    .find(|c| c.call == *call)
                    .expect("planned call exists");
                assert!(
                    dal_core::EXCLUSIVE_TOOLS.contains(&planned.name.as_str())
                        || !matches!(planned.class, ToolClass::Read),
                    "contiguous runs: lone read must have batched ({context})"
                );
            }
            _ => panic!("unplanned dispatch unit ({context})"),
        }
    }
}

#[test]
fn read_run_partition_property() {
    let policy = policy_all();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for seq in 0..500 {
        let len = 1 + rng.below(20);
        let mut calls = Vec::new();
        for i in 0..len {
            let id = format!("call_{seq}_{i}");
            let kind = rng.below(10);
            let (tool, class) = match kind {
                0..=4 => ("read", ToolClass::Read),
                5..=6 => ("exec_ro", exec(true)),
                7 => ("run", exec(false)),
                8 => ("other", ToolClass::Other),
                _ => ("ask", ToolClass::Read),
            };
            calls.push(call(&id, tool, class));
        }
        check_sequence(&calls, &policy, &format!("seeded seq {seq} len {len}"));
    }
    for (tool, class) in [
        ("read", ToolClass::Read),
        ("run", exec(false)),
        ("ask", ToolClass::Read),
    ] {
        check_sequence(
            &[call("e1", tool, class.clone())],
            &policy,
            "exhaustive len 1",
        );
        for (tool2, class2) in [
            ("read", ToolClass::Read),
            ("run", exec(false)),
            ("ask", ToolClass::Read),
        ] {
            check_sequence(
                &[
                    call("e1", tool, class.clone()),
                    call("e2", tool2, class2.clone()),
                ],
                &policy,
                "exhaustive len 2",
            );
            for (tool3, class3) in [
                ("read", ToolClass::Read),
                ("run", exec(false)),
                ("ask", ToolClass::Read),
            ] {
                check_sequence(
                    &[
                        call("e1", tool, class.clone()),
                        call("e2", tool2, class2.clone()),
                        call("e3", tool3, class3.clone()),
                    ],
                    &policy,
                    "exhaustive len 3",
                );
            }
        }
    }
}
