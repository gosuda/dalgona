//! Approval classes, decisions, and the rule-1+ call plan.

use serde::{Deserialize, Serialize};

use crate::{config::ApprovalMode, ext::Name, id::CallId};
use std::{collections::BTreeSet, path::PathBuf};

/// The class a tool computes from its arguments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolClass {
    /// A read operation.
    Read,
    /// An edit operation.
    Patch,
    /// A process execution, with its read-only classification and optional grant specification.
    Exec {
        /// Whether the execution is read-only.
        read_only: bool,
        /// The argv prefix and roots proposed for a job grant.
        grant: Option<GrantSpec>,
    },
    /// An evaluation, classified by whether it is pure.
    Eval {
        /// Whether evaluation is pure.
        pure: bool,
    },
    /// A tool whose effects are not classified as reads or patches.
    /// It is dispatched serially and follows the execution approval rung.
    Other,
}

/// The argv prefix and roots proposed for a job grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct GrantSpec {
    /// The command-line prefix covered by the grant.
    pub argv_prefix: Box<str>,
    /// The filesystem roots covered by the grant.
    pub roots: Vec<PathBuf>,
}

/// The approval gate associated with a tool class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gate {
    /// A read-only operation.
    ReadOnly,
    /// An edit operation.
    Mutate,
    /// An operation that executes code or a process.
    Execute,
    /// An operation handled outside this approval ladder.
    None,
}

/// Maps a tool class to its approval gate.
#[must_use]
pub fn gate(class: &ToolClass) -> Gate {
    match class {
        ToolClass::Read | ToolClass::Eval { pure: true } => Gate::ReadOnly,
        ToolClass::Patch => Gate::Mutate,
        ToolClass::Exec { .. } | ToolClass::Eval { pure: false } | ToolClass::Other => {
            Gate::Execute
        }
    }
}

/// The lowest approval mode rung that allows the class without asking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rung {
    /// The edits rung.
    Edits,
    /// The all-actions rung.
    All,
}

/// Returns the lowest mode rung that allows the class without asking.
#[must_use]
pub fn rung(class: &ToolClass) -> Rung {
    match gate(class) {
        Gate::Mutate => Rung::Edits,
        Gate::ReadOnly | Gate::Execute | Gate::None => Rung::All,
    }
}

impl Rung {
    /// The rung's flag name on `--approval`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Rung::Edits => "edits",
            Rung::All => "all",
        }
    }
}

/// The spec's model-visible denial for a gated call with nobody to ask
/// (`approval.denied_headless`). Front ends parse the same text back for the
/// matching user-facing note, so the format and its parser live together.
#[must_use]
pub fn headless_denial_text(tool: &str, rung: Rung) -> String {
    format!(
        "Permission denied: {tool} needs approval, and this run has no one to ask. Continue without it and report what needs the user. (The user can rerun with --approval {}.)",
        rung.as_str()
    )
}

/// Reads the tool name and needed rung back out of a [`headless_denial_text`]
/// denial; returns `None` when the text is some other denial.
#[must_use]
pub fn parse_headless_denial(text: &str) -> Option<(&str, Rung)> {
    let rest = text
        .strip_prefix("Permission denied: ")?
        .split_once(" needs approval, and this run has no one to ask.")?;
    let tool = rest.0;
    let rung = match rest
        .1
        .rsplit_once("--approval ")
        .and_then(|(_, tail)| tail.strip_suffix(".)"))
    {
        Some("edits") => Rung::Edits,
        _ => Rung::All,
    };
    Some((tool, rung))
}

/// Why an approval request cannot proceed.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DenyReason {
    /// The required service was not injected.
    NotInjected,
    /// The required capability was not granted.
    NotGranted,
    /// No interactive approval frontend is attached.
    NoFrontEnd,
    /// A required resource is unavailable.
    Unavailable {
        /// The unavailable service or resource.
        what: Box<str>,
    },
    /// A requested resource is outside the allowed scope.
    OutOfScope {
        /// The resource outside the allowed scope.
        what: Box<str>,
    },
    /// The operation exceeded its wake limit.
    WakeLimit,
}

impl DenyReason {
    /// Denies because `what` is outside the allowed scope.
    #[must_use]
    pub fn out_of_scope(what: impl Into<Box<str>>) -> Self {
        DenyReason::OutOfScope { what: what.into() }
    }
}

/// The result of applying an approval policy to a tool call.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    /// The call can proceed without asking.
    Allow,
    /// Present an approval question to the answerer.
    Ask {
        /// The grant specification proposed for an execution.
        grant: Option<GrantSpec>,
    },
    /// The call cannot proceed.
    Deny {
        /// The reason the call was denied.
        reason: DenyReason,
    },
}

/// One approval policy snapshot shared by calls in a dispatch round.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Policy {
    /// The configured approval mode.
    pub mode: ApprovalMode,
    /// Whether an interactive answerer is attached.
    pub answerer_attached: bool,
    /// Tool names allowed in every mode.
    pub allow_always: BTreeSet<Name>,
}

impl Policy {
    /// Decides whether a tool may proceed, must ask, or must be denied.
    #[must_use]
    pub fn decide(&self, tool: &Name, class: &ToolClass) -> Decision {
        if self.allow_always.contains(tool) {
            return Decision::Allow;
        }

        let asks = match gate(class) {
            Gate::ReadOnly | Gate::None => false,
            Gate::Mutate => self.mode == ApprovalMode::Ask,
            Gate::Execute => self.mode != ApprovalMode::All,
        };

        match (asks, self.answerer_attached) {
            (false, _) => Decision::Allow,
            (true, true) => Decision::Ask {
                grant: grant_of(class),
            },
            (true, false) => Decision::Deny {
                reason: DenyReason::NoFrontEnd,
            },
        }
    }
}

fn grant_of(class: &ToolClass) -> Option<GrantSpec> {
    match class {
        ToolClass::Exec { grant, .. } => grant.clone(),
        ToolClass::Read | ToolClass::Patch | ToolClass::Eval { .. } | ToolClass::Other => None,
    }
}

/// A resolved tool call ready for dispatch planning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedCall {
    /// The provider's identity for the call.
    pub call: CallId,
    /// The registered tool name.
    pub name: Name,
    /// The class computed for this call.
    pub class: ToolClass,
}

/// A dispatch unit in the rule-1+ call plan.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unit {
    /// A maximal contiguous run of read-only calls.
    Reads {
        /// The calls in input order.
        calls: Vec<CallId>,
    },
    /// A call that must be dispatched alone.
    Serial {
        /// The call to dispatch.
        call: CallId,
    },
}

/// Partitions calls in input order into maximal read runs and serial units.
#[must_use]
pub fn plan(calls: &[PlannedCall], policy: &Policy) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut run = Vec::new();

    for planned in calls {
        let read = matches!(planned.class, ToolClass::Read)
            || matches!(
                planned.class,
                ToolClass::Exec {
                    read_only: true,
                    ..
                }
            ) && policy.decide(&planned.name, &planned.class) == Decision::Allow;

        if read {
            run.push(planned.call.clone());
        } else {
            if !run.is_empty() {
                units.push(Unit::Reads {
                    calls: std::mem::take(&mut run),
                });
            }
            units.push(Unit::Serial {
                call: planned.call.clone(),
            });
        }
    }

    if !run.is_empty() {
        units.push(Unit::Reads { calls: run });
    }

    units
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn name(value: &str) -> Name {
        Name::from_str(value).expect("valid tool name")
    }

    fn policy(mode: ApprovalMode, answerer_attached: bool) -> Policy {
        Policy {
            mode,
            answerer_attached,
            allow_always: BTreeSet::new(),
        }
    }

    fn exec(read_only: bool, grant: Option<GrantSpec>) -> ToolClass {
        ToolClass::Exec { read_only, grant }
    }

    #[test]
    fn decision_ladder_covers_every_class() {
        let read = ToolClass::Read;
        let patch = ToolClass::Patch;
        let read_exec = exec(true, None);
        let execute = exec(false, None);
        let pure_eval = ToolClass::Eval { pure: true };
        let effectful_eval = ToolClass::Eval { pure: false };
        let other = ToolClass::Other;

        for (mode, patch_asks, execution_asks) in [
            (ApprovalMode::Ask, true, true),
            (ApprovalMode::Edits, false, true),
            (ApprovalMode::All, false, false),
        ] {
            for answerer_attached in [false, true] {
                let policy = policy(mode, answerer_attached);
                let expected = |asks| match (asks, answerer_attached) {
                    (false, _) => Decision::Allow,
                    (true, true) => Decision::Ask { grant: None },
                    (true, false) => Decision::Deny {
                        reason: DenyReason::NoFrontEnd,
                    },
                };
                assert_eq!(policy.decide(&name("read"), &read), Decision::Allow);
                assert_eq!(
                    policy.decide(&name("read_exec"), &read_exec),
                    expected(execution_asks)
                );
                assert_eq!(
                    policy.decide(&name("pure_eval"), &pure_eval),
                    Decision::Allow
                );
                assert_eq!(
                    policy.decide(&name("other"), &other),
                    expected(execution_asks)
                );
                assert_eq!(policy.decide(&name("patch"), &patch), expected(patch_asks));
                assert_eq!(
                    policy.decide(&name("execute"), &execute),
                    expected(execution_asks)
                );
                assert_eq!(
                    policy.decide(&name("effectful_eval"), &effectful_eval),
                    expected(execution_asks)
                );
            }
        }
    }

    #[test]
    fn headless_asks_are_denied_without_leaking_grants() {
        let grant = GrantSpec {
            argv_prefix: Box::<str>::from("git"),
            roots: vec![PathBuf::from("/repo")],
        };
        let ask_policy = policy(ApprovalMode::Ask, false);

        assert_eq!(
            ask_policy.decide(&name("edit"), &ToolClass::Patch),
            Decision::Deny {
                reason: DenyReason::NoFrontEnd
            }
        );
        assert_eq!(
            ask_policy.decide(&name("command"), &exec(false, Some(grant.clone()))),
            Decision::Deny {
                reason: DenyReason::NoFrontEnd
            }
        );
        assert_eq!(
            ask_policy.decide(&name("read_command"), &exec(true, None)),
            Decision::Deny {
                reason: DenyReason::NoFrontEnd
            }
        );

        let edits_policy = policy(ApprovalMode::Edits, false);
        assert_eq!(
            edits_policy.decide(&name("read_command"), &exec(true, None)),
            Decision::Deny {
                reason: DenyReason::NoFrontEnd
            }
        );
        assert_eq!(
            edits_policy.decide(&name("command"), &exec(false, Some(grant))),
            Decision::Deny {
                reason: DenyReason::NoFrontEnd
            }
        );
    }
    #[test]
    fn allow_always_bypasses_asks_and_never_attaches_a_grant() {
        let mut policy = policy(ApprovalMode::Ask, false);
        policy.allow_always.insert(name("command"));
        let grant = GrantSpec {
            argv_prefix: Box::<str>::from("git"),
            roots: vec![PathBuf::from("/repo")],
        };

        assert_eq!(
            policy.decide(&name("command"), &exec(false, Some(grant))),
            Decision::Allow
        );
    }

    #[test]
    fn ask_carries_exec_grant_only_for_user_approval() {
        let grant = GrantSpec {
            argv_prefix: Box::<str>::from("git"),
            roots: vec![PathBuf::from("/repo")],
        };
        let policy = policy(ApprovalMode::Ask, true);

        assert_eq!(
            policy.decide(&name("command"), &exec(false, Some(grant.clone()))),
            Decision::Ask { grant: Some(grant) }
        );
        assert_eq!(
            policy.decide(&name("patch"), &ToolClass::Patch),
            Decision::Ask { grant: None }
        );
    }

    fn call(id: &str, tool: &str, class: ToolClass) -> PlannedCall {
        PlannedCall {
            call: CallId::new(id),
            name: name(tool),
            class,
        }
    }

    #[test]
    fn planner_groups_only_contiguous_reads_and_keeps_eval_and_other_serial() {
        let policy = policy(ApprovalMode::All, true);
        let calls = [
            call("1", "read", ToolClass::Read),
            call("2", "exec_read", exec(true, None)),
            call("3", "pure_eval", ToolClass::Eval { pure: true }),
            call("4", "read", ToolClass::Read),
            call("5", "other", ToolClass::Other),
            call("6", "read", ToolClass::Read),
        ];

        assert_eq!(
            plan(&calls, &policy),
            vec![
                Unit::Reads {
                    calls: vec![CallId::new("1"), CallId::new("2")],
                },
                Unit::Serial {
                    call: CallId::new("3"),
                },
                Unit::Reads {
                    calls: vec![CallId::new("4")],
                },
                Unit::Serial {
                    call: CallId::new("5"),
                },
                Unit::Reads {
                    calls: vec![CallId::new("6")],
                },
            ]
        );
    }

    #[test]
    fn planner_keeps_approval_required_exec_serial() {
        let policy = policy(ApprovalMode::Ask, true);
        let calls = [
            call("1", "read", ToolClass::Read),
            call("2", "write_exec", exec(false, None)),
            call("3", "read", ToolClass::Read),
        ];

        assert_eq!(
            plan(&calls, &policy),
            vec![
                Unit::Reads {
                    calls: vec![CallId::new("1")],
                },
                Unit::Serial {
                    call: CallId::new("2"),
                },
                Unit::Reads {
                    calls: vec![CallId::new("3")],
                },
            ]
        );
    }

    #[test]
    fn planner_read_only_exec_is_serial_when_asking_and_batched_under_all() {
        let calls = [
            call("1", "read", ToolClass::Read),
            call("2", "read_exec", exec(true, None)),
        ];
        let ask_policy = policy(ApprovalMode::Ask, true);
        assert_eq!(
            ask_policy.decide(&name("read_exec"), &exec(true, None)),
            Decision::Ask { grant: None }
        );
        assert_eq!(
            plan(&calls, &ask_policy),
            vec![
                Unit::Reads {
                    calls: vec![CallId::new("1")],
                },
                Unit::Serial {
                    call: CallId::new("2"),
                },
            ]
        );

        let all_policy = policy(ApprovalMode::All, true);
        assert_eq!(
            plan(&calls, &all_policy),
            vec![Unit::Reads {
                calls: vec![CallId::new("1"), CallId::new("2")],
            }]
        );
    }

    #[test]
    fn planner_handles_empty_and_singleton_inputs() {
        let policy = policy(ApprovalMode::Ask, true);
        assert!(plan(&[], &policy).is_empty());
        assert_eq!(
            plan(&[call("1", "read", ToolClass::Read)], &policy),
            vec![Unit::Reads {
                calls: vec![CallId::new("1")],
            }]
        );
    }
}
