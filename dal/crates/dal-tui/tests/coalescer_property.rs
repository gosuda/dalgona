//! Coalescer property: lossless order, delta merge, shed accounting.

use dal_core::{Gen, Seq, StreamChannel, TurnId, Update, UpdateKind};
use dal_tui::frame::Coalescer;
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
            queue.push_update(update(index as u64 + 1, UpdateKind::Delta {
                turn: turn(),
                channel: StreamChannel::Text,
                text: text.clone().into_boxed_str(),
            }));
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
    fn lossless_events_survive_overflow(count in 4_100usize..4_200) {
        let mut queue = Coalescer::default();
        for index in 0..count {
            queue.push_update(update(index as u64 + 1, UpdateKind::Delta {
                turn: turn(),
                channel: StreamChannel::Text,
                text: "x".into(),
            }));
        }
        let shed = queue.take_shed_count();
        prop_assert!(shed > 0);
        prop_assert_eq!(queue.take_updates().len(), 4_096 - usize::try_from(shed).unwrap_or(0));
    }
}
