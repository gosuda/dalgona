//! Coalescer property: lossless order, delta merge, shed accounting.

use dal_core::{Gen, Seq, StreamChannel, TurnId, Update, UpdateKind};
use dal_tui::frame::{Coalescer, QueueFull};
use proptest::prelude::*;
use std::num::NonZeroU64;

fn update(seq: u64, kind: UpdateKind) -> Update {
    Update {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::new(seq.max(1)).unwrap_or(NonZeroU64::MIN)),
        kind,
    }
}

fn turn() -> TurnId {
    TurnId::new(NonZeroU64::MIN)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn coalescer_property(deltas in prop::collection::vec("a|b|cd", 1..20)) {
        let mut queue = Coalescer::default();
        for (index, text) in deltas.iter().enumerate() {
            let next = update(
                index as u64 + 1,
                UpdateKind::Delta {
                    turn: turn(),
                    channel: StreamChannel::Text,
                    text: text.clone().into_boxed_str(),
                },
            );
            prop_assert!(queue.push_update(next).is_ok());
        }
        let drained = queue.take_updates();
        prop_assert_eq!(drained.len(), 1);
        let expected: String = deltas.concat();
        match &drained[0].kind {
            UpdateKind::Delta { text, .. } => prop_assert_eq!(text.as_ref(), expected),
            _ => prop_assert!(false, "expected merged delta"),
        }
        prop_assert_eq!(queue.take_shed_count(), 0);
    }

    #[test]
    fn lossless_events_survive_overflow(
        lossless in 1usize..64,
        replaceable in 4_096usize..4_200,
    ) {
        let mut queue = Coalescer::default();
        let mut seq = 0u64;
        for _ in 0..lossless {
            seq += 1;
            let next = update(
                seq,
                UpdateKind::Notice(dal_core::Notice {
                    turn: Some(turn()),
                    kind: "test.notice".into(),
                    text: "lossless".into(),
                }),
            );
            prop_assert!(queue.push_update(next).is_ok());
        }
        for index in 0..replaceable {
            seq += 1;
            let next = update(
                seq,
                UpdateKind::ToolProgress {
                    call: dal_core::CallId::new(format!("call-{index}")),
                    tail: format!("progress {index}").into_boxed_str(),
                },
            );
            prop_assert!(queue.push_update(next).is_ok());
        }
        let shed = queue.take_shed_count();
        prop_assert_eq!(
            usize::try_from(shed).unwrap_or(usize::MAX),
            lossless + replaceable - 4_096
        );
        let drained = queue.take_updates();
        prop_assert_eq!(drained.len(), 4_096);
        prop_assert_eq!(
            drained
                .iter()
                .filter(|item| matches!(item.kind, UpdateKind::Notice(_)))
                .count(),
            lossless
        );
    }
}

#[test]
fn a_full_queue_of_lossless_updates_refuses_the_next_one_instead_of_shedding() {
    let notice = |seq: u64| {
        update(
            seq,
            UpdateKind::Notice(dal_core::Notice {
                turn: Some(turn()),
                kind: "test.notice".into(),
                text: format!("transition {seq}").into_boxed_str(),
            }),
        )
    };
    let mut queue = Coalescer::default();
    for seq in 1..=4_096 {
        assert!(queue.push_update(notice(seq)).is_ok());
    }
    for seq in 4_097..=4_200 {
        let refused = queue.push_update(notice(seq));
        assert_eq!(refused, Err(QueueFull(Box::new(notice(seq)))));
    }
    assert_eq!(queue.take_shed_count(), 0);
    let drained = queue.take_updates();
    assert_eq!(drained.len(), 4_096);
    assert!(
        drained
            .iter()
            .zip(1..)
            .all(|(item, seq)| item.seq.get() == seq),
        "the oldest transitions are intact and in order"
    );
    assert!(
        queue.push_update(notice(4_097)).is_ok(),
        "draining reopens the queue"
    );
}
