//! Scope-table scheduling, accounting, and lifecycle tests (E05).

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dal_core::ext::{NativeOp, OpId};
use dal_core::{
    Budget, CallId, ContextItem, ModelPrice, ModelRequest, ModelRoute, ModelToolSpec, OnError,
    Purpose, RawJson, RequestParams, ScopeSpec, ScopeSpecError, StreamEvent, Usage,
};
use tokio_util::sync::CancellationToken;

use super::{
    MAX_LIVE_SCOPES, MAX_OUTSTANDING, MAX_RESULT_BYTES, MAX_SCOPE_CONCURRENCY, ScopeError,
    ScopeTable, SessionTasks, mint_raw,
};
use crate::ext::scope::PriceFn;
use crate::ext::{
    CancelTarget, EffectStatus, FailureCode, HostTerminal, OpFailure, OpOutcome, OpRecord,
    OpRequest, OpValue, ScopeId, Submit, TaskId,
};

/// One table whose price lookup never resolves.
fn table() -> ScopeTable {
    ScopeTable::new(Arc::new(AtomicUsize::new(0)), no_price())
}

/// A price function with no known model prices.
fn no_price() -> PriceFn {
    Arc::new(|_| None)
}

/// The spec of a scope with no budgets.
fn spec(limit: u16) -> ScopeSpec {
    ScopeSpec {
        limit,
        on_error: OnError::Cancel,
        budget: Budget::default(),
    }
}

/// One non-model operation request.
fn tool_req() -> OpRequest {
    OpRequest::new(
        OpId::Native(NativeOp::ToolsRead),
        RawJson::parse("{}").expect("args"),
    )
}

/// Opens one unbudgeted scope.
fn open(table: &mut ScopeTable, limit: u16) -> ScopeId {
    table
        .open(spec(limit), CancellationToken::new())
        .expect("scope opens")
}

/// Submits one queued tool task.
fn queued(table: &mut ScopeTable, scope: ScopeId) -> TaskId {
    match table.submit(scope, tool_req(), None) {
        Ok(Submit::Queued(id)) => id,
        other => panic!("submission must queue: {other:?}"),
    }
}

/// A successful outcome for one task, carrying its minted call id.
fn ok_outcome(table: &ScopeTable, t: TaskId) -> OpOutcome {
    OpOutcome::Ok {
        value: OpValue::Json(RawJson::parse("null").expect("value")),
        record: OpRecord {
            call: table.call_id(t).expect("call id"),
            op: OpId::Native(NativeOp::ToolsRead),
            status: EffectStatus::Completed,
        },
    }
}

/// A recoverable failed outcome for one task.
fn failed_outcome(table: &ScopeTable, t: TaskId) -> OpOutcome {
    OpOutcome::Failed {
        failure: OpFailure {
            code: FailureCode::Failed,
            message: Box::from("fixture failure"),
            details: None,
        },
        record: OpRecord {
            call: table.call_id(t).expect("call id"),
            op: OpId::Native(NativeOp::ToolsRead),
            status: EffectStatus::Failed,
        },
    }
}

/// One `ModelsInfer` request for the priced/unpriced paths.
fn model_req() -> OpRequest {
    let request = ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Harness {
            id: "dalgon/normal".into(),
        },
        system: "".into(),
        tools: Arc::from(Vec::<ModelToolSpec>::new()),
        context: Arc::from(Vec::<ContextItem>::new()),
        params: RequestParams::default(),
        cache_key: None,
    };
    let encoded = sonic_rs::to_string(&request).expect("request encodes");
    OpRequest::new(
        OpId::Native(NativeOp::ModelsInfer),
        RawJson::parse(&encoded).expect("request JSON validates"),
    )
}

/// A successful inference outcome carrying one usage event.
fn infer_outcome(table: &ScopeTable, t: TaskId, output_tokens: u64) -> OpOutcome {
    let usage = Usage {
        input_tokens: 10,
        cached_input_tokens: 0,
        output_tokens,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    };
    let inference = dal_core::Inference {
        events: vec![StreamEvent::Usage(usage)],
    };
    let encoded = sonic_rs::to_string(&inference).expect("inference encodes");
    OpOutcome::Ok {
        value: OpValue::Json(RawJson::parse(&encoded).expect("inference JSON")),
        record: OpRecord {
            call: table.call_id(t).expect("call id"),
            op: OpId::Native(NativeOp::ModelsInfer),
            status: EffectStatus::Completed,
        },
    }
}

#[test]
fn open_rejects_limits_outside_the_member_cap() {
    let mut table = table();
    for limit in [0, u16::from(MAX_SCOPE_CONCURRENCY) + 1] {
        let mut bad = spec(limit);
        bad.limit = limit;
        let error = table
            .open(bad, CancellationToken::new())
            .expect_err("out-of-range limit refuses");
        assert!(
            matches!(
                error,
                HostTerminal::InvalidScope(ScopeSpecError::InvalidLimit { .. })
            ),
            "limit {limit} must be invalid_scope: {error}"
        );
    }
}

#[test]
fn live_scope_bound_counts_only_unclosed_scopes() {
    let mut table = table();
    let first = open(&mut table, 1);
    for _ in 1..MAX_LIVE_SCOPES {
        open(&mut table, 1);
    }
    let error = table
        .open(spec(1), CancellationToken::new())
        .expect_err("the fifth live scope refuses");
    assert!(
        matches!(
            error,
            HostTerminal::LimitExceeded {
                what: "live_scopes"
            }
        ),
        "the live-scope limit names itself: {error}"
    );
    // An empty scope closes on seal and frees the bound.
    assert!(table.seal(first).expect("seal").is_empty());
    open(&mut table, 1);
}

#[test]
fn submit_mints_task_ids_in_submission_order() {
    let mut table = table();
    let scope = open(&mut table, 4);
    let first = queued(&mut table, scope);
    let second = queued(&mut table, scope);
    assert!(first < second, "submission order is mint order");
    assert_eq!(table.call_id(first).expect("call").as_str(), "scope-1");
    assert_eq!(table.outstanding(), 2);
}

#[test]
fn runnable_honors_the_scope_limit_without_blocking_others() {
    let mut table = table();
    let one = open(&mut table, 1);
    let other = open(&mut table, 4);
    let a1 = queued(&mut table, one);
    let a2 = queued(&mut table, one);
    let b1 = queued(&mut table, other);

    let issued: Vec<TaskId> = table.runnable().into_iter().map(|(t, ..)| t).collect();
    assert_eq!(issued, vec![a1, b1], "a scope at its limit waits in FIFO");

    table
        .complete(a1, ok_outcome(&table, a1), 0)
        .expect("complete");
    let issued: Vec<TaskId> = table.runnable().into_iter().map(|(t, ..)| t).collect();
    assert_eq!(
        issued,
        vec![a2],
        "the freed slot admits the next queued task"
    );
}

#[test]
fn submit_freezes_the_observation_cutoff() {
    let mut table = table();
    let scope = open(&mut table, 2);
    let Ok(Submit::Queued(t)) = table.submit(scope, tool_req(), Some(7)) else {
        panic!("submission must queue");
    };
    let issued = table.runnable();
    let [(id, _req, cutoff, _cancel)] = issued.as_slice() else {
        panic!("one task issues: {issued:?}");
    };
    assert_eq!(*id, t);
    assert_eq!(*cutoff, Some(7));
}

#[test]
fn submit_to_unknown_or_sealed_scopes_fails() {
    let mut table = table();
    let unknown = ScopeId::new(NonZeroU64::new(99).expect("nonzero"));
    assert!(
        matches!(
            table.submit(unknown, tool_req(), None),
            Err(ScopeError::Unknown(id)) if id == unknown
        ),
        "an unknown scope names itself"
    );
    let scope = open(&mut table, 1);
    table.seal(scope).expect("seal");
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Err(ScopeError::NotOpen(id)) if id == scope
        ),
        "a sealed scope refuses submissions"
    );
}

#[test]
fn request_budget_refuses_before_minting() {
    let mut table = table();
    let mut bounded = spec(4);
    bounded.budget.requests = Some(1);
    let scope = table
        .open(bounded, CancellationToken::new())
        .expect("scope opens");
    queued(&mut table, scope);
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Refused(ScopeSpecError::Exhausted))
        ),
        "the requests bound refuses new handles"
    );
    assert_eq!(table.outstanding(), 1, "a refusal mints no task");
}

#[test]
fn usd_budget_refuses_an_unpriced_model_and_admits_a_priced_one() {
    let mut table = table();
    let mut bounded = spec(4);
    bounded.budget.usd = Some(1.0);
    let scope = table
        .open(bounded, CancellationToken::new())
        .expect("scope opens");
    let refused = table.submit(scope, model_req(), None);
    assert!(
        matches!(
            refused,
            Ok(Submit::Refused(ScopeSpecError::UnpricedModel { .. }))
        ),
        "an unpriced model under a usd budget refuses: {refused:?}"
    );

    let price: PriceFn = Arc::new(|_| {
        Some(ModelPrice {
            input: 1.0,
            cached_input: 1.0,
            output: 1.0,
            reasoning: 1.0,
        })
    });
    let mut priced = ScopeTable::new(Arc::new(AtomicUsize::new(0)), price);
    let mut bounded = spec(4);
    bounded.budget.usd = Some(1.0);
    let scope = priced
        .open(bounded, CancellationToken::new())
        .expect("scope opens");
    assert!(
        matches!(
            priced.submit(scope, model_req(), None),
            Ok(Submit::Queued(_))
        ),
        "a priced model queues"
    );
}

#[test]
fn outstanding_cap_is_a_terminal_limit() {
    let mut table = table();
    let scope = open(&mut table, MAX_SCOPE_CONCURRENCY.into());
    for _ in 0..MAX_OUTSTANDING {
        queued(&mut table, scope);
    }
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Terminal(HostTerminal::LimitExceeded {
                what: "outstanding"
            }))
        ),
        "the outstanding cap is terminal"
    );
}

#[test]
fn deliver_replays_the_recorded_outcome_and_credits_once() {
    let mut table = table();
    let scope = open(&mut table, 2);
    let t = queued(&mut table, scope);
    let _ = table.runnable();
    table
        .complete(t, ok_outcome(&table, t), 0)
        .expect("complete");
    let first = table.deliver(t).expect("recorded outcome");
    let second = table.deliver(t).expect("delivery is idempotent");
    assert!(
        matches!(first, OpOutcome::Ok { .. }) && matches!(second, OpOutcome::Ok { .. }),
        "repeated waits re-serve the same outcome"
    );
    assert_eq!(table.outstanding(), 0, "the count decreases once");
}

#[test]
fn seal_orders_members_and_the_last_delivery_closes_the_scope() {
    let host_bytes = Arc::new(AtomicUsize::new(0));
    let mut table = ScopeTable::new(Arc::clone(&host_bytes), no_price());
    let scope = open(&mut table, 4);
    let a = queued(&mut table, scope);
    let b = queued(&mut table, scope);
    assert_eq!(*table.seal(scope).expect("seal"), [a, b]);

    for t in [a, b] {
        table
            .complete(t, ok_outcome(&table, t), 500)
            .expect("complete");
        assert!(table.deliver(t).is_some());
    }
    assert_eq!(
        host_bytes.load(Ordering::SeqCst),
        0,
        "closing releases retained bytes"
    );
    // A closed scope frees the live bound; four more opens fit again.
    for _ in 0..MAX_LIVE_SCOPES {
        open(&mut table, 1);
    }
}

#[test]
fn owner_cancel_records_a_recoverable_cancelled_outcome() {
    let mut table = table();
    let scope = open(&mut table, 2);
    let t = queued(&mut table, scope);
    assert_eq!(table.cancel(CancelTarget::Task(t)), vec![t]);
    let outcome = table.deliver(t).expect("the cancelled outcome records");
    assert!(
        matches!(
            outcome,
            OpOutcome::Failed {
                failure: OpFailure {
                    code: FailureCode::Cancelled,
                    ..
                },
                ..
            }
        ),
        "an owner cancel is recoverable: {outcome:?}"
    );
    assert!(
        table.cancel(CancelTarget::Task(t)).is_empty(),
        "repeated cancels absorb"
    );
}

#[test]
fn scope_cancel_stops_unfinished_members_once() {
    let mut table = table();
    let scope = open(&mut table, 4);
    let a = queued(&mut table, scope);
    let b = queued(&mut table, scope);
    let mut stopped = table.cancel(CancelTarget::Scope(scope));
    stopped.sort();
    assert_eq!(stopped, vec![a, b]);
    assert!(table.cancel(CancelTarget::Scope(scope)).is_empty());
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Refused(ScopeSpecError::Cancelled))
        ),
        "a cancelled scope refuses new work"
    );
}

#[test]
fn root_cancel_marks_unfinished_tasks_terminal() {
    let mut table = table();
    let scope = open(&mut table, 2);
    let t = queued(&mut table, scope);
    assert_eq!(table.cancel_root(), vec![t]);
    let outcome = table.deliver(t).expect("the terminal outcome records");
    assert!(
        matches!(outcome, OpOutcome::Terminal(HostTerminal::Cancelled)),
        "external root cancellation stays terminal: {outcome:?}"
    );
}

#[test]
fn complete_absorbs_unknown_and_settled_tasks() {
    let mut table = table();
    let scope = open(&mut table, 2);
    let t = queued(&mut table, scope);
    let ghost = TaskId::new(NonZeroU64::new(77).expect("nonzero"));
    table
        .complete(ghost, ok_outcome(&table, t), 0)
        .expect("a late arrival for an unknown task absorbs");
    table
        .complete(t, ok_outcome(&table, t), 0)
        .expect("complete");
    table
        .complete(t, failed_outcome(&table, t), 0)
        .expect("a settled task keeps its terminal record");
    let outcome = table.deliver(t).expect("recorded outcome");
    assert!(
        matches!(outcome, OpOutcome::Ok { .. }),
        "the first outcome stands: {outcome:?}"
    );
}

#[test]
fn result_byte_budgets_refuse_overflow_before_counting() {
    let host_bytes = Arc::new(AtomicUsize::new(0));
    let mut table = ScopeTable::new(Arc::clone(&host_bytes), no_price());
    let scope = open(&mut table, 2);
    let t = queued(&mut table, scope);
    let error = table
        .complete(t, ok_outcome(&table, t), MAX_RESULT_BYTES + 1)
        .expect_err("the invocation budget refuses");
    assert!(
        matches!(
            error,
            HostTerminal::LimitExceeded {
                what: "result_bytes"
            }
        ),
        "the 4 MiB invocation cap names itself: {error}"
    );
    assert_eq!(
        host_bytes.load(Ordering::SeqCst),
        0,
        "refusal counts nothing"
    );
    table
        .complete(t, ok_outcome(&table, t), 16)
        .expect("small results still land");
    assert_eq!(host_bytes.load(Ordering::SeqCst), 16);
}

#[test]
fn host_result_budget_refuses_overflow() {
    let host_bytes = Arc::new(AtomicUsize::new(super::MAX_HOST_RESULT_BYTES - 8));
    let mut table = ScopeTable::new(Arc::clone(&host_bytes), no_price());
    let scope = open(&mut table, 2);
    let t = queued(&mut table, scope);
    let error = table
        .complete(t, ok_outcome(&table, t), 16)
        .expect_err("the host budget refuses");
    assert!(
        matches!(
            error,
            HostTerminal::LimitExceeded {
                what: "result_bytes"
            }
        ),
        "the 16 MiB host cap names itself: {error}"
    );
    assert_eq!(
        host_bytes.load(Ordering::SeqCst),
        super::MAX_HOST_RESULT_BYTES - 8,
        "the host counter does not move"
    );
}

#[test]
fn on_error_cancel_stops_siblings_and_settle_keeps_them() {
    let mut cancel_table = table();
    let scope = open(&mut cancel_table, 4);
    let failed = queued(&mut cancel_table, scope);
    let sibling = queued(&mut cancel_table, scope);
    cancel_table
        .complete(failed, failed_outcome(&cancel_table, failed), 0)
        .expect("complete");
    let outcome = cancel_table
        .deliver(sibling)
        .expect("sibling outcome records");
    assert!(
        matches!(
            outcome,
            OpOutcome::Failed {
                failure: OpFailure {
                    code: FailureCode::Cancelled,
                    ..
                },
                ..
            }
        ),
        "on_error cancel settles the sibling cancelled: {outcome:?}"
    );

    let mut settle_table = table();
    let mut settling = spec(4);
    settling.on_error = OnError::Settle;
    let scope = settle_table
        .open(settling, CancellationToken::new())
        .expect("scope opens");
    let failed = queued(&mut settle_table, scope);
    let sibling = queued(&mut settle_table, scope);
    settle_table
        .complete(failed, failed_outcome(&settle_table, failed), 0)
        .expect("complete");
    assert!(
        !settle_table.settled(sibling),
        "on_error settle leaves siblings running"
    );
}

#[test]
fn usage_exhaustion_refuses_new_submissions() {
    let price: PriceFn = Arc::new(|_| {
        Some(ModelPrice {
            input: 1.0,
            cached_input: 1.0,
            output: 1.0,
            reasoning: 1.0,
        })
    });
    let mut table = ScopeTable::new(Arc::new(AtomicUsize::new(0)), price);
    let mut bounded = spec(4);
    bounded.budget.output_tokens = Some(5);
    let scope = table
        .open(bounded, CancellationToken::new())
        .expect("scope opens");
    let Ok(Submit::Queued(t)) = table.submit(scope, model_req(), None) else {
        panic!("the priced model queues");
    };
    table
        .complete(t, infer_outcome(&table, t, 10), 0)
        .expect("usage charges");
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Refused(ScopeSpecError::Exhausted))
        ),
        "the exhausted budget refuses new handles"
    );
}

#[test]
fn wall_deadline_expires_the_scope() {
    let mut table = table();
    let mut timed = spec(4);
    timed.budget.wall = Some(Duration::from_millis(1));
    let scope = table
        .open(timed, CancellationToken::new())
        .expect("scope opens");
    assert!(table.next_deadline().is_some(), "the wall clock publishes");
    std::thread::sleep(Duration::from_millis(10));
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Refused(ScopeSpecError::Exhausted))
        ),
        "an expired scope reports exhausted"
    );
}

#[test]
fn cleanup_reports_unfinished_calls_in_submission_order() {
    let mut table = table();
    let scope = open(&mut table, 4);
    let a = queued(&mut table, scope);
    let b = queued(&mut table, scope);
    let (complete, outstanding) = table.cleanup();
    assert!(!complete, "unfinished work reports incomplete");
    let calls: Vec<&str> = outstanding.iter().map(CallId::as_str).collect();
    assert_eq!(calls, ["scope-1", "scope-2"]);
    assert_eq!(
        table.cleanup().1,
        outstanding,
        "a second cleanup reports the same unfinished calls"
    );
    let _ = (a, b);
}

#[test]
fn mint_raw_refuses_overflow() {
    let mut next = u64::MAX;
    assert!(
        matches!(
            mint_raw(&mut next, "scope_ids"),
            Err(HostTerminal::LimitExceeded { what: "scope_ids" })
        ),
        "the counter names its budget"
    );
}

#[tokio::test]
async fn session_tasks_stop_cancels_tracked_work() {
    let tasks = SessionTasks::new();
    let flag = Arc::new(AtomicUsize::new(0));
    let saw = Arc::clone(&flag);
    tasks.spawn(async move {
        saw.fetch_add(1, Ordering::SeqCst);
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    assert_eq!(flag.load(Ordering::SeqCst), 1, "the task started");
    assert_eq!(tasks.tracked(), 1);
    tasks.stop().await;
    assert_eq!(tasks.tracked(), 0, "stop drains the tracker");
}
