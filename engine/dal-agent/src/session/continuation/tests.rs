//! Slot and cover contracts: exact (id, name) assembly, slot transitions,
//! and fail-closed orderings.
//!
//! These tests use real dal types throughout and never duplicate the run
//! state machine: `plan_cover` is the production assembly validator, and
//! the slot tests drive the production transition methods on slots that
//! hold no run. Live-run paths (open/park/stash/respond with a real
//! `ProviderRun`, keepalive, cancel-while-parked) are covered by integration
//! tests against the loopback server.

use std::collections::HashMap;

use dal_core::{CallId, Part};
use dal_provider::HostToolResult;
use tokio_util::sync::CancellationToken;

use super::{
    ContinuationOwner, CoverError, ParkError, PendingCall, PlanItem, StashError, StashedResult,
    check_pending, check_session_subset, plan_cover,
};
use crate::session::tasks::SessionTasks;

fn id(value: &str) -> CallId {
    CallId::new(value)
}

fn text(value: &str) -> Part {
    Part::Text { text: value.into() }
}

fn pending(id: &str, name: &str) -> PendingCall {
    PendingCall {
        id: self::id(id),
        name: name.into(),
    }
}

fn stashed(name: &str, is_error: bool) -> StashedResult {
    StashedResult {
        name: name.into(),
        is_error,
        parts: vec![text("out")],
    }
}

fn journaled(id: &str, name: &str, is_error: bool) -> HostToolResult {
    HostToolResult {
        call: self::id(id),
        name: name.into(),
        is_error,
        parts: vec![text("out")],
    }
}

fn stash_of(pairs: &[(&str, &str)]) -> HashMap<CallId, StashedResult> {
    pairs
        .iter()
        .map(|(id, name)| (self::id(id), stashed(name, false)))
        .collect()
}

fn idle_slot() -> std::sync::Arc<super::ContinuationSlot> {
    ContinuationOwner::new(SessionTasks::new())
        .downgrade()
        .upgrade()
        .expect("owner is alive")
}

#[test]
fn cover_orders_the_pending_union_of_stash_and_journal() {
    let pending = vec![
        pending("a", "session_tool"),
        pending("b", "private_tool"),
        pending("c", "other_tool"),
    ];
    let stash = stash_of(&[("b", "private_tool")]);
    let session = vec![
        journaled("a", "session_tool", false),
        journaled("c", "other_tool", true),
    ];
    let plan = plan_cover(&pending, &stash, &session).expect("exact cover");
    assert_eq!(
        plan,
        vec![PlanItem::Session, PlanItem::Stash, PlanItem::Session]
    );
}

#[test]
fn cover_empty_pending_with_no_results_is_ok() {
    let plan = plan_cover(&[], &HashMap::new(), &[]).expect("empty cover");
    assert_eq!(plan, [] as [PlanItem; 0]);
}

#[test]
fn cover_missing_result_errors() {
    let pending = vec![pending("a", "tool_a"), pending("b", "tool_b")];
    let stash = stash_of(&[("a", "tool_a")]);
    let error = plan_cover(&pending, &stash, &[]).expect_err("b is uncovered");
    assert_eq!(error, CoverError::Missing(id("b")));
}

#[test]
fn cover_extra_result_errors() {
    let pending = vec![pending("a", "tool_a")];
    let session = vec![
        journaled("a", "tool_a", false),
        journaled("x", "tool_x", false),
    ];
    let error = plan_cover(&pending, &HashMap::new(), &session).expect_err("x is extra");
    assert_eq!(error, CoverError::Extra(id("x")));
}

#[test]
fn cover_missing_plus_extra_never_donates() {
    // Pending b is missing while an extra result for x exists. Exact cover
    // fails instead of donating x to b.
    let pending = vec![pending("a", "tool_a"), pending("b", "tool_b")];
    let session = vec![
        journaled("a", "tool_a", false),
        journaled("x", "tool_x", false),
    ];
    let error = plan_cover(&pending, &HashMap::new(), &session).expect_err("no donation");
    assert_eq!(error, CoverError::Missing(id("b")));
}

#[test]
fn cover_duplicate_journaled_results_error() {
    let pending = vec![pending("a", "tool_a")];
    let session = vec![
        journaled("a", "tool_a", false),
        journaled("a", "tool_a", true),
    ];
    let error = plan_cover(&pending, &HashMap::new(), &session).expect_err("duplicate");
    assert_eq!(error, CoverError::DuplicateResult(id("a")));
}

#[test]
fn cover_stash_plus_journal_for_one_call_is_a_duplicate() {
    let pending = vec![pending("a", "tool_a")];
    let stash = stash_of(&[("a", "tool_a")]);
    let session = vec![journaled("a", "tool_a", false)];
    let error = plan_cover(&pending, &stash, &session).expect_err("duplicate");
    assert_eq!(error, CoverError::DuplicateResult(id("a")));
}

#[test]
fn cover_wrong_stash_name_errors() {
    let pending = vec![pending("a", "tool_a")];
    let stash = stash_of(&[("a", "tool_wrong")]);
    let error = plan_cover(&pending, &stash, &[]).expect_err("wrong name");
    assert_eq!(
        error,
        CoverError::WrongName {
            call: id("a"),
            expected: "tool_a".into(),
            found: "tool_wrong".into(),
        }
    );
}

#[test]
fn cover_wrong_journaled_name_errors() {
    let pending = vec![pending("a", "tool_a")];
    let session = vec![journaled("a", "tool_wrong", false)];
    let error = plan_cover(&pending, &HashMap::new(), &session).expect_err("wrong name");
    assert_eq!(
        error,
        CoverError::WrongName {
            call: id("a"),
            expected: "tool_a".into(),
            found: "tool_wrong".into(),
        }
    );
}

#[test]
fn cover_empty_pending_with_results_is_extra() {
    let session = vec![journaled("a", "tool_a", false)];
    let error = plan_cover(&[], &HashMap::new(), &session).expect_err("extra");
    assert_eq!(error, CoverError::Extra(id("a")));
}

#[test]
fn slot_idle_releases_to_released_and_stays_there() {
    let slot = idle_slot();
    assert!(!slot.is_awaiting());
    assert!(!slot.release_if_live());
    assert!(!slot.is_awaiting());
    assert!(!slot.release_if_live());
}

#[test]
fn slot_without_conversation_rejects_park_stash_and_respond() {
    let slot = idle_slot();
    assert_eq!(
        slot.park(vec![(id("a"), "tool_a".into())]),
        Err(ParkError::NoConversation)
    );
    assert_eq!(
        slot.stash(id("a"), "tool_a".into(), false, vec![text("out")]),
        Err(StashError::NoConversation)
    );
    // State is checked before cancellation: even a cancelled token reports
    // the state error, never a silent cancel.
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(slot.respond(vec![], &cancel));
    assert!(matches!(
        error,
        Err(dal_provider::ProviderError::InvalidRequest { .. })
    ));
}

#[test]
fn park_cover_rejects_duplicate_ids() {
    let pending = vec![(id("a"), "tool_a".into()), (id("a"), "tool_a".into())];
    let error = check_pending(pending, &[id("a")]).expect_err("duplicate");
    assert_eq!(error, ParkError::Duplicate(id("a")));
}

#[test]
fn park_cover_accepts_exact_set_in_announced_order() {
    let offered = vec![(id("a"), "tool_a".into()), (id("b"), "tool_b".into())];
    let checked = check_pending(offered, &[id("b"), id("a")]).expect("exact cover");
    assert_eq!(
        checked,
        vec![pending("a", "tool_a"), pending("b", "tool_b")]
    );
}

#[test]
fn park_cover_reports_missing_and_extra() {
    let pending = vec![(id("a"), "tool_a".into()), (id("x"), "tool_x".into())];
    let error = check_pending(pending, &[id("a"), id("b")]).expect_err("cover");
    assert_eq!(
        error,
        ParkError::Cover {
            missing: vec![id("b")],
            extra: vec![id("x")],
        }
    );
}

#[test]
fn opening_guard_restores_idle_but_leaves_released() {
    let slot = idle_slot();
    {
        let mut state = slot.lock_state();
        *state = super::SlotState::Opening;
    }
    {
        let _opening = super::Opening {
            slot: slot.as_ref(),
            armed: true,
        };
    }
    // A failed handshake restores `Idle`: releasing reports no live
    // conversation.
    assert!(!slot.release_if_live());
    // Reserve again, then end the owner meanwhile: the guard leaves the
    // terminal state alone.
    {
        let mut state = slot.lock_state();
        *state = super::SlotState::Opening;
    }
    {
        let _opening = super::Opening {
            slot: slot.as_ref(),
            armed: true,
        };
        assert!(slot.release_if_live());
    }
    assert!(!slot.release_if_live());
}

#[test]
fn opening_slot_has_no_live_conversation() {
    let slot = idle_slot();
    {
        let mut state = slot.lock_state();
        *state = super::SlotState::Opening;
    }
    assert!(!slot.is_awaiting());
    assert_eq!(slot.park(vec![]), Err(ParkError::NoConversation));
    assert_eq!(
        slot.stash(id("a"), "tool_a".into(), false, vec![text("out")]),
        Err(StashError::NoConversation)
    );
}

#[test]
fn session_subset_matching_parked_cover_is_ok() {
    let parked = vec![pending("p1", "private_tool"), pending("s1", "session_tool")];
    let session = vec![(id("s1"), "session_tool".into())];
    check_session_subset(&session, &parked).expect("exact subset");
    check_session_subset(&[], &parked).expect("empty subset is vacuous");
}

#[test]
fn session_subset_wrong_name_errors() {
    let parked = vec![pending("s1", "session_tool")];
    let session = vec![(id("s1"), "other_tool".into())];
    let error = check_session_subset(&session, &parked).expect_err("wrong name");
    assert_eq!(
        error,
        ParkError::WrongName {
            call: id("s1"),
            expected: "session_tool".into(),
            found: "other_tool".into(),
        }
    );
}

#[test]
fn session_subset_unknown_id_is_extra() {
    let parked = vec![pending("s1", "session_tool")];
    let session = vec![(id("x"), "tool_x".into())];
    let error = check_session_subset(&session, &parked).expect_err("extra");
    assert_eq!(
        error,
        ParkError::Cover {
            missing: vec![],
            extra: vec![id("x")],
        }
    );
}

#[test]
fn session_subset_duplicate_errors() {
    let parked = vec![pending("s1", "session_tool")];
    let session = vec![
        (id("s1"), "session_tool".into()),
        (id("s1"), "session_tool".into()),
    ];
    let error = check_session_subset(&session, &parked).expect_err("duplicate");
    assert_eq!(error, ParkError::Duplicate(id("s1")));
}

#[test]
fn owner_releases_its_slot_at_the_explicit_owner_end() {
    // The owner-end path the turn and relay stream call: a method on the
    // owner itself, since outside modules cannot reach the slot field.
    let owner = ContinuationOwner::new(SessionTasks::new());
    assert!(!owner.release_if_live());
    assert!(!owner.release_if_live());
}

#[test]
fn owner_downgrade_lives_while_the_owner_lives_and_dies_after() {
    let owner = ContinuationOwner::new(SessionTasks::new());
    let weak = owner.downgrade();
    assert!(weak.upgrade().is_some());
    drop(owner);
    assert!(weak.upgrade().is_none());
}
