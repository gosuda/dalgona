use std::sync::Arc;
use std::time::Duration;

use dal_core::ext::{NativeOp, OpId, OpSet, Phase};
use proptest::prelude::*;
use tokio::time::Instant;

use super::{HostTerminal, Invocation, InvocationParts, MAX_DEPTH, MAX_ISSUED};

fn set(ops: &[NativeOp]) -> OpSet {
    OpSet::parse(ops.iter().map(|op| op.as_str())).expect("native ops form a valid set")
}

fn child(
    parent: &Arc<Invocation>,
    ceiling: OpSet,
    deadline: Instant,
) -> Result<Arc<Invocation>, HostTerminal> {
    Invocation::mint(InvocationParts {
        parent: Some(Arc::clone(parent)),
        session: parent.session(),
        generation: parent.generation(),
        caller: parent.caller().clone(),
        export: None,
        phase: Phase::Eval,
        consumer: None,
        deadline,
        declared: ceiling.clone(),
        ceiling,
        cutoff: None,
        cancel: tokio_util::sync::CancellationToken::new(),
        permit: None,
    })
}

fn subset(mask: u32) -> Vec<NativeOp> {
    NativeOp::ALL
        .into_iter()
        .enumerate()
        .filter(|(index, _)| mask & (1 << index) != 0)
        .map(|(_, op)| op)
        .collect()
}

proptest! {
    #[test]
    fn r04_child_never_widens_its_parent(parent_mask in 0u32..(1 << 28), child_mask in 0u32..(1 << 28)) {
        let root = Invocation::for_test(set(&subset(parent_mask)), Phase::Eval, Duration::from_secs(60))
            .expect("root mints");
        let nested = child(&root, set(&subset(child_mask)), root.deadline()).expect("depth one mints");
        for op in NativeOp::ALL {
            let id = OpId::Native(op);
            prop_assert_eq!(
                nested.ceiling().contains(&id),
                root.ceiling().contains(&id) && set(&subset(child_mask)).contains(&id)
            );
            prop_assert!(!nested.allows(&id) || root.allows(&id));
        }
    }
}

#[test]
fn r10_depth_and_issue_limits_are_tree_wide() {
    let root = Invocation::for_test(
        set(&[NativeOp::ToolsRead]),
        Phase::Eval,
        Duration::from_secs(60),
    )
    .expect("root mints");
    let mut node = Arc::clone(&root);
    for depth in 1..=MAX_DEPTH {
        node = child(&node, root.ceiling().clone(), root.deadline()).expect("within depth");
        assert_eq!(node.depth(), depth);
        assert_eq!(node.root(), root.id());
    }
    assert!(matches!(
        child(&node, root.ceiling().clone(), root.deadline()),
        Err(HostTerminal::LimitExceeded { what: "depth" })
    ));
    for issued in 1..=MAX_ISSUED {
        let from = if issued % 2 == 0 { &root } else { &node };
        assert_eq!(from.issue().expect("under the cap"), issued);
    }
    assert!(matches!(
        root.issue(),
        Err(HostTerminal::LimitExceeded { .. })
    ));
    assert!(matches!(
        node.issue(),
        Err(HostTerminal::LimitExceeded { .. })
    ));
}

#[test]
fn r07_child_shares_gate_and_inherits_bounds() {
    let root = Invocation::for_test(
        set(&[NativeOp::ToolsRead]),
        Phase::Eval,
        Duration::from_secs(60),
    )
    .expect("root mints");
    let later = root.deadline() + Duration::from_secs(600);
    let nested = child(&root, set(&[NativeOp::ToolsRead]), later).expect("depth one mints");
    assert_eq!(nested.deadline(), root.deadline());
    assert_eq!(nested.parent(), Some(root.id()));
    assert_ne!(nested.id(), root.id());
    root.cancel().cancel();
    assert!(nested.cancel().is_cancelled());
    nested.gate().close();
    assert!(!root.gate().is_open());
}
