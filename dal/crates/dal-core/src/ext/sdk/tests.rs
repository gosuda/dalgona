use std::collections::BTreeSet;

use proptest::prelude::*;

use super::{ExportId, ExportKind, NativeOp, OpId, OpSet, Phase, UsesError};
use crate::ext::{HookEvent, Name, Service};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn r03_uses_ids_are_exact_and_round_trip() -> TestResult {
    let mut seen = BTreeSet::new();
    for op in NativeOp::ALL {
        assert!(seen.insert(op.as_str()), "{op} repeated");
        assert_eq!(OpId::parse(op.as_str())?, OpId::Native(op));
        assert_eq!(OpId::Native(op).to_string(), op.as_str());
    }
    let export = OpId::parse("tools.quality.todos")?;
    assert_eq!(
        export,
        OpId::Export(ExportId {
            plugin: Name::parse("quality")?,
            kind: ExportKind::Tool,
            local: Name::parse("todos")?,
        })
    );
    assert_eq!(export.to_string(), "tools.quality.todos");
    let route = OpId::parse("models.router.pick")?;
    assert_eq!(route.to_string(), "models.router.pick");

    for unknown in [
        "",
        "fs.write",
        "run",
        "tools",
        "tools.read.x.y",
        "commands.quality.todos",
        "hooks.quality.settled",
        "tools.Quality.todos",
        "tools..todos",
        "TOOLS.READ",
    ] {
        assert!(
            matches!(OpId::parse(unknown), Err(UsesError::Unknown { .. })),
            "{unknown:?} was accepted"
        );
    }
    for wildcard in ["tools.*", "*", "tools.quality.*"] {
        assert!(matches!(
            OpId::parse(wildcard),
            Err(UsesError::Wildcard { .. })
        ));
    }
    Ok(())
}

#[test]
fn e01_uses_list_rejects_duplicates_and_oversize() -> TestResult {
    assert!(matches!(
        OpSet::parse(["tools.read", "tools.read"]),
        Err(UsesError::Duplicate { .. })
    ));
    assert!(matches!(
        OpSet::parse(["tools.q.a", "tools.read", "tools.q.a"]),
        Err(UsesError::Duplicate { .. })
    ));
    let many = (0..65).map(|n| format!("tools.p.t{n}")).collect::<Vec<_>>();
    assert_eq!(
        OpSet::parse(many.iter().map(String::as_str)),
        Err(UsesError::TooMany { count: 65 })
    );
    assert_eq!(
        OpSet::parse(many[..64].iter().map(String::as_str))?
            .iter()
            .count(),
        64
    );
    assert!(OpSet::parse([])?.is_empty());
    assert_eq!(OpSet::parse([])?, OpSet::EMPTY);
    assert_eq!(OpSet::default(), OpSet::EMPTY);
    Ok(())
}

#[test]
fn r03_service_map_keeps_ask_ungranted_and_exports_serviceless() -> TestResult {
    let set = OpSet::parse([
        "tools.read",
        "tools.search",
        "ask.text",
        "state.delete",
        "tools.quality.todos",
    ])?;
    let services = set.services().iter().collect::<Vec<_>>();
    assert_eq!(services, [Service::FsRead, Service::Sidecar]);
    assert_eq!(NativeOp::ToolsPatch.service(), Some(Service::FsWrite));
    assert_eq!(NativeOp::ToolsExec.service(), Some(Service::Run));
    assert_eq!(NativeOp::ModelsForward.service(), Some(Service::Infer));
    for ask in [NativeOp::AskConfirm, NativeOp::AskSelect, NativeOp::AskText] {
        assert_eq!(ask.service(), None);
    }
    Ok(())
}

#[test]
fn p05_phase_table_is_closed() -> TestResult {
    let state = ["state.read", "state.write", "state.delete"];
    let observers = [
        HookEvent::SessionStart,
        HookEvent::SessionEnd,
        HookEvent::ToolResult,
        HookEvent::TurnEnd,
    ];
    let pure = [
        HookEvent::Input,
        HookEvent::BeforeTurn,
        HookEvent::BeforeRequest,
        HookEvent::ToolCall,
    ];
    let export = OpId::parse("tools.quality.todos")?;
    for op in NativeOp::ALL.map(OpId::Native).into_iter().chain([export]) {
        let name = op.to_string();
        let forward = name == "models.forward";
        assert!(Phase::Model.permits(&op), "{name}");
        for phase in [Phase::Tool, Phase::Command, Phase::Eval] {
            assert_eq!(phase.permits(&op), !forward, "{phase:?} {name}");
        }
        for event in observers {
            let allowed = state.contains(&name.as_str());
            assert_eq!(Phase::Hook(event).permits(&op), allowed, "{event} {name}");
        }
        for event in pure {
            assert!(!Phase::Hook(event).permits(&op), "{event} {name}");
        }
        let settled = name == "state.read";
        assert_eq!(
            Phase::Hook(HookEvent::Settled).permits(&op),
            settled,
            "{name}"
        );
    }
    Ok(())
}

fn op_id() -> impl Strategy<Value = String> {
    prop_oneof![
        (0..NativeOp::ALL.len()).prop_map(|index| NativeOp::ALL[index].as_str().to_owned()),
        ("[ab]", "[xy]").prop_map(|(plugin, local)| format!("tools.{plugin}.{local}")),
        ("[ab]", "[xy]").prop_map(|(plugin, local)| format!("models.{plugin}.{local}")),
    ]
}

fn op_set() -> impl Strategy<Value = BTreeSet<String>> {
    proptest::collection::btree_set(op_id(), 0..20)
}

fn build(ids: &BTreeSet<String>) -> Result<OpSet, TestCaseError> {
    OpSet::parse(ids.iter().map(String::as_str))
        .map_err(|error| TestCaseError::fail(error.to_string()))
}

proptest! {
    #[test]
    fn r04_opset_matches_a_set_oracle(left in op_set(), right in op_set(), probe in op_id()) {
        let (a, b) = (build(&left)?, build(&right)?);
        let rendered = |set: &OpSet| set.iter().map(|op| op.to_string()).collect::<BTreeSet<_>>();
        prop_assert_eq!(&rendered(&a), &left);
        prop_assert_eq!(a.len(), left.len());
        let probe_op = OpId::parse(&probe).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(a.contains(&probe_op), left.contains(&probe));
        prop_assert_eq!(a.is_subset(&b), left.is_subset(&right));
        let meet = a.intersect(&b);
        prop_assert_eq!(rendered(&meet), left.intersection(&right).cloned().collect::<BTreeSet<_>>());
        prop_assert!(meet.is_subset(&a) && meet.is_subset(&b));
        prop_assert_eq!(a.is_empty(), left.is_empty());
        let reordered = OpSet::parse(left.iter().rev().map(String::as_str))
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(reordered, a);
    }
}
