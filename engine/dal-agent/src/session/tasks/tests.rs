//! Scope-table scheduling, accounting, and lifecycle tests (E05).

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dal_core::ext::{NativeOp, OpId};
use dal_core::{
    Budget, CallId, ContextItem, HandleStatus, ModelPrice, ModelRequest, ModelRoute, ModelToolSpec,
    OnError, Purpose, RawJson, RequestParams, ScopeSpec, ScopeSpecError, ScopeUsage, Stop,
    StreamEvent, Usage,
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

/// The usage of a response whose provider measured nothing.
const UNREPORTED: Usage = Usage {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_tokens: None,
    cache_write_tokens: 0,
    premium_requests: None,
    cost_usd: None,
};

/// A measured usage of ten input tokens and `output_tokens` output tokens.
fn measured_usage(output_tokens: u64) -> Usage {
    Usage {
        input_tokens: 10,
        output_tokens,
        ..UNREPORTED
    }
}

/// A successful inference outcome carrying one usage event.
fn infer_outcome(table: &ScopeTable, t: TaskId, output_tokens: u64) -> OpOutcome {
    inference_outcome(
        table,
        t,
        vec![StreamEvent::Usage(measured_usage(output_tokens))],
    )
}

/// A successful inference outcome carrying exactly `events`.
fn inference_outcome(table: &ScopeTable, t: TaskId, events: Vec<StreamEvent>) -> OpOutcome {
    let inference = dal_core::Inference { events };
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
            cache_write: None,
            tiers: Box::default(),
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
    assert_eq!(table.cancel(CancelTarget::Scope(scope)), []);
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
            cache_write: None,
            tiers: Box::default(),
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

/// A price function that prices every model at one USD per million tokens.
fn unit_price() -> PriceFn {
    Arc::new(|_| {
        Some(ModelPrice {
            input: 1.0,
            cached_input: 1.0,
            output: 1.0,
            reasoning: 1.0,
            cache_write: None,
            tiers: Box::default(),
        })
    })
}

/// One table pricing every model, with one settle-on-error scope bounded by
/// `budget`.
fn bounded(budget: Budget) -> (ScopeTable, ScopeId) {
    let mut table = ScopeTable::new(Arc::new(AtomicUsize::new(0)), unit_price());
    let mut scoped = spec(4);
    scoped.on_error = OnError::Settle;
    scoped.budget = budget;
    let scope = table
        .open(scoped, CancellationToken::new())
        .expect("scope opens");
    (table, scope)
}

/// A complete response carrying `usage`, or no usage event at all.
fn response(usage: Option<Usage>) -> Vec<StreamEvent> {
    usage
        .map(StreamEvent::Usage)
        .into_iter()
        .chain([StreamEvent::Stop(Stop::EndTurn)])
        .collect()
}

/// Submits one model request, issues it, and settles it successfully with
/// a response carrying `usage`, or none.
fn settle_model(table: &mut ScopeTable, scope: ScopeId, usage: Option<Usage>) -> TaskId {
    let Ok(Submit::Queued(t)) = table.submit(scope, model_req(), None) else {
        panic!("the model request queues");
    };
    let _ = table.runnable();
    table
        .complete(t, inference_outcome(table, t, response(usage)), 0)
        .expect("the outcome records");
    t
}

/// Whether the scope still accepts one more operation.
fn admits(table: &mut ScopeTable, scope: ScopeId) -> bool {
    matches!(table.submit(scope, tool_req(), None), Ok(Submit::Queued(_)))
}

/// Settles one successful model request carrying `usage`, or none, in a
/// scope under each token and USD limit, and requires it to exhaust that
/// scope.
fn assert_exhausts_every_spend_limit(usage: Option<Usage>) {
    let limits = [
        Budget {
            input_tokens: Some(1_000),
            ..Budget::default()
        },
        Budget {
            output_tokens: Some(1_000),
            ..Budget::default()
        },
        Budget {
            usd: Some(1.0),
            ..Budget::default()
        },
    ];
    for limit in limits {
        let (mut table, scope) = bounded(limit.clone());
        let sibling = queued(&mut table, scope);
        settle_model(&mut table, scope, usage);
        assert!(
            matches!(
                table.submit(scope, tool_req(), None),
                Ok(Submit::Refused(ScopeSpecError::Exhausted))
            ),
            "a response without measured usage must exhaust {limit:?}"
        );
        assert!(
            matches!(
                table.deliver(sibling),
                Some(OpOutcome::Failed {
                    failure: OpFailure {
                        code: FailureCode::Cancelled,
                        ..
                    },
                    ..
                })
            ),
            "exhaustion stops the scope's other members under {limit:?}"
        );
    }
}

#[test]
fn unreported_usage_exhausts_spend_limits_instead_of_spending_zero() {
    assert_exhausts_every_spend_limit(Some(UNREPORTED));
}

#[test]
fn a_missing_usage_event_exhausts_spend_limits_instead_of_spending_zero() {
    assert_exhausts_every_spend_limit(None);
}

#[test]
fn an_unreported_response_exhausts_even_below_the_measured_limit() {
    let (mut table, scope) = bounded(Budget {
        output_tokens: Some(1_000),
        ..Budget::default()
    });
    settle_model(&mut table, scope, Some(measured_usage(5)));
    assert!(
        admits(&mut table, scope),
        "measured usage stays under the limit"
    );
    settle_model(&mut table, scope, Some(UNREPORTED));
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Refused(ScopeSpecError::Exhausted))
        ),
        "the unreported response exhausts a scope whose measured total is far below its limit"
    );
}

#[test]
fn reported_zero_measurements_keep_their_current_charge() {
    let reported_zeros = [
        Usage {
            cost_usd: Some(0.0),
            ..UNREPORTED
        },
        Usage {
            reasoning_tokens: Some(0),
            ..UNREPORTED
        },
    ];
    for usage in reported_zeros {
        let (mut table, scope) = bounded(Budget {
            input_tokens: Some(1_000),
            output_tokens: Some(1_000),
            usd: Some(1.0),
            ..Budget::default()
        });
        settle_model(&mut table, scope, Some(usage));
        assert!(
            admits(&mut table, scope),
            "{usage:?} is a reported zero, not a missing measurement"
        );
    }
}

#[test]
fn measured_usage_charges_once() {
    let (mut table, scope) = bounded(Budget {
        input_tokens: Some(25),
        ..Budget::default()
    });
    let measured = Some(measured_usage(5));
    let first = settle_model(&mut table, scope, measured);
    table
        .complete(
            first,
            inference_outcome(&table, first, response(measured)),
            0,
        )
        .expect("a settled task absorbs the late arrival");
    settle_model(&mut table, scope, measured);
    assert!(
        admits(&mut table, scope),
        "two measured responses of 10 input tokens stay under a limit of 25"
    );
    settle_model(&mut table, scope, measured);
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Refused(ScopeSpecError::Exhausted))
        ),
        "the third measured response reaches the limit"
    );
}

#[test]
fn request_and_wall_limits_do_not_need_a_spend_measurement() {
    let limits = [
        Budget {
            requests: Some(8),
            ..Budget::default()
        },
        Budget {
            wall: Some(Duration::from_secs(3600)),
            ..Budget::default()
        },
    ];
    for limit in limits {
        let (mut table, scope) = bounded(limit.clone());
        settle_model(&mut table, scope, Some(UNREPORTED));
        settle_model(&mut table, scope, None);
        assert!(
            admits(&mut table, scope),
            "{limit:?} limits no spend, so an unmeasured response leaves it open"
        );
    }
}

#[test]
fn failed_cancelled_and_non_model_operations_leave_spend_limits_open() {
    let (mut table, scope) = bounded(Budget {
        input_tokens: Some(1_000),
        usd: Some(1.0),
        ..Budget::default()
    });
    let Ok(Submit::Queued(failed)) = table.submit(scope, model_req(), None) else {
        panic!("the failing model request queues");
    };
    let Ok(Submit::Queued(cancelled)) = table.submit(scope, model_req(), None) else {
        panic!("the cancelled model request queues");
    };
    let tool = queued(&mut table, scope);
    let _ = table.runnable();
    table
        .complete(failed, failed_outcome(&table, failed), 0)
        .expect("the failure records");
    assert_eq!(table.cancel(CancelTarget::Task(cancelled)), vec![cancelled]);
    table
        .complete(tool, ok_outcome(&table, tool), 0)
        .expect("the tool result records");
    assert!(
        admits(&mut table, scope),
        "a failed or cancelled request and a tool result carry no usage to measure"
    );
}

#[test]
fn a_result_refused_by_the_byte_budget_still_spends_its_usage() {
    for usage in [Some(measured_usage(5)), None] {
        let (mut table, scope) = bounded(Budget {
            input_tokens: Some(5),
            ..Budget::default()
        });
        let Ok(Submit::Queued(t)) = table.submit(scope, model_req(), None) else {
            panic!("the model request queues");
        };
        let _ = table.runnable();
        let refused = table
            .complete(
                t,
                inference_outcome(&table, t, response(usage)),
                MAX_RESULT_BYTES + 1,
            )
            .expect_err("the invocation budget refuses");
        assert!(
            matches!(
                refused,
                HostTerminal::LimitExceeded {
                    what: "result_bytes"
                }
            ),
            "the refusal names its budget: {refused}"
        );
        // The script host then settles the task with a reference-sized failure.
        table
            .complete(t, failed_outcome(&table, t), 0)
            .expect("the failure records");
        assert!(
            matches!(
                table.submit(scope, tool_req(), None),
                Ok(Submit::Refused(ScopeSpecError::Exhausted))
            ),
            "a refused response ({usage:?}) must not escape the scope's token limit"
        );
    }
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

#[test]
fn mint_raw_hands_out_the_last_handle_below_overflow_then_refuses() {
    let mut next = u64::MAX - 1;
    let last = mint_raw(&mut next, "scope_ids").expect("one below the overflow edge still mints");
    assert_eq!(last.get(), u64::MAX - 1);
    assert_eq!(next, u64::MAX, "the counter advances to the overflow edge");
    assert!(
        matches!(
            mint_raw(&mut next, "scope_ids"),
            Err(HostTerminal::LimitExceeded { what: "scope_ids" })
        ),
        "the edge itself refuses"
    );
    assert_eq!(
        next,
        u64::MAX,
        "a refused mint does not advance the counter"
    );
}

/// Settles one response carrying `spend` in a fresh scope under `budget`
/// and reports whether that scope still admits work.
fn admits_after(budget: &Budget, spend: Usage) -> bool {
    let (mut table, scope) = bounded(budget.clone());
    settle_model(&mut table, scope, Some(spend));
    admits(&mut table, scope)
}

#[test]
fn input_token_limit_exhausts_exactly_at_the_limit() {
    let budget = Budget {
        input_tokens: Some(100),
        ..Budget::default()
    };
    let spend = |input_tokens| Usage {
        input_tokens,
        output_tokens: 1,
        ..UNREPORTED
    };
    assert!(
        admits_after(&budget, spend(99)),
        "one token under the limit stays open"
    );
    assert!(
        !admits_after(&budget, spend(100)),
        "reaching the limit exhausts the scope"
    );
    assert!(
        !admits_after(&budget, spend(101)),
        "one token past the limit is exhausted"
    );
}

#[test]
fn output_token_limit_counts_reasoning_tokens_and_exhausts_at_the_limit() {
    let budget = Budget {
        output_tokens: Some(100),
        ..Budget::default()
    };
    let spend = |output_tokens, reasoning_tokens| Usage {
        input_tokens: 1,
        output_tokens,
        reasoning_tokens,
        ..UNREPORTED
    };
    assert!(
        admits_after(&budget, spend(99, None)),
        "one output token under the limit stays open"
    );
    assert!(
        !admits_after(&budget, spend(100, None)),
        "reaching the limit exhausts the scope"
    );
    assert!(
        !admits_after(&budget, spend(101, None)),
        "one output token past the limit is exhausted"
    );
    assert!(
        admits_after(&budget, spend(60, Some(39))),
        "60 output plus 39 reasoning is 99 and stays open"
    );
    assert!(
        !admits_after(&budget, spend(60, Some(40))),
        "reasoning tokens spend the output limit: 60 plus 40 reaches 100"
    );
}

#[test]
fn usd_limit_exhausts_exactly_at_the_limit_and_sums_across_responses() {
    let budget = Budget {
        usd: Some(1.0),
        ..Budget::default()
    };
    let costing = |cost_usd| Usage {
        input_tokens: 1,
        cost_usd: Some(cost_usd),
        ..UNREPORTED
    };
    assert!(
        admits_after(&budget, costing(0.999_999)),
        "a cost under the limit stays open"
    );
    assert!(
        !admits_after(&budget, costing(1.0)),
        "a cost equal to the limit exhausts the scope"
    );
    assert!(
        !admits_after(&budget, costing(1.000_001)),
        "a cost past the limit is exhausted"
    );

    let (mut table, scope) = bounded(budget);
    settle_model(&mut table, scope, Some(costing(0.5)));
    assert!(
        admits(&mut table, scope),
        "half of the limit leaves the scope open"
    );
    settle_model(&mut table, scope, Some(costing(0.5)));
    assert!(
        !admits(&mut table, scope),
        "two halves sum to the limit and exhaust the scope"
    );
}

#[test]
fn a_provider_cost_that_is_not_a_valid_amount_exhausts_a_usd_budget() {
    let budget = Budget {
        usd: Some(1.0),
        ..Budget::default()
    };
    for cost_usd in [-1.0, -0.000_001] {
        let spend = Usage {
            input_tokens: 1,
            cost_usd: Some(cost_usd),
            ..UNREPORTED
        };
        assert!(
            !admits_after(&budget, spend),
            "a reported cost of {cost_usd} is unknown spend, never free spend"
        );
    }
    let tokens_only = Budget {
        input_tokens: Some(1_000),
        ..Budget::default()
    };
    let spend = Usage {
        input_tokens: 1,
        cost_usd: Some(-1.0),
        ..UNREPORTED
    };
    assert!(
        admits_after(&tokens_only, spend),
        "an unknown cost only matters to a usd limit"
    );
}

#[test]
fn invocation_byte_budget_admits_exactly_the_cap_and_refuses_one_byte_more() {
    let host_bytes = Arc::new(AtomicUsize::new(0));
    let mut table = ScopeTable::new(Arc::clone(&host_bytes), no_price());
    let scope = open(&mut table, 4);
    let first = queued(&mut table, scope);
    let second = queued(&mut table, scope);
    let third = queued(&mut table, scope);
    table
        .complete(first, ok_outcome(&table, first), MAX_RESULT_BYTES - 1)
        .expect("one byte under the cap lands");
    table
        .complete(second, ok_outcome(&table, second), 1)
        .expect("filling the cap exactly lands");
    assert_eq!(host_bytes.load(Ordering::SeqCst), MAX_RESULT_BYTES);
    let error = table
        .complete(third, ok_outcome(&table, third), 1)
        .expect_err("one byte past the cap refuses");
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
        MAX_RESULT_BYTES,
        "the refused byte is not counted"
    );
}

#[test]
fn host_byte_budget_admits_exactly_the_cap_and_refuses_one_byte_more() {
    let host_bytes = Arc::new(AtomicUsize::new(super::MAX_HOST_RESULT_BYTES - 8));
    let mut table = ScopeTable::new(Arc::clone(&host_bytes), no_price());
    let scope = open(&mut table, 4);
    let fits = queued(&mut table, scope);
    let over = queued(&mut table, scope);
    table
        .complete(fits, ok_outcome(&table, fits), 8)
        .expect("filling the host cap exactly lands");
    assert_eq!(
        host_bytes.load(Ordering::SeqCst),
        super::MAX_HOST_RESULT_BYTES
    );
    let error = table
        .complete(over, ok_outcome(&table, over), 1)
        .expect_err("one byte past the host cap refuses");
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
        super::MAX_HOST_RESULT_BYTES,
        "the refused byte is not counted"
    );
}

#[test]
fn outstanding_slot_frees_on_delivery_not_on_completion() {
    let mut table = table();
    let scope = open(&mut table, MAX_SCOPE_CONCURRENCY.into());
    let ids: Vec<TaskId> = (0..MAX_OUTSTANDING)
        .map(|_| queued(&mut table, scope))
        .collect();
    let first = ids[0];
    table
        .complete(first, ok_outcome(&table, first), 0)
        .expect("complete");
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Terminal(HostTerminal::LimitExceeded {
                what: "outstanding"
            }))
        ),
        "a completed but undelivered result still holds its slot"
    );
    assert!(table.deliver(first).is_some());
    assert!(admits(&mut table, scope), "delivery frees one slot");
    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Terminal(HostTerminal::LimitExceeded {
                what: "outstanding"
            }))
        ),
        "delivery frees exactly one slot"
    );
}

#[test]
fn next_deadline_publishes_the_earliest_live_wall_and_forgets_closed_scopes() {
    use tokio::time::Instant;

    let mut table = table();
    let walled = |seconds| {
        let mut walled = spec(1);
        walled.budget.wall = Some(Duration::from_secs(seconds));
        walled
    };
    let near = table
        .open(walled(60), CancellationToken::new())
        .expect("near scope opens");
    let far = table
        .open(walled(3_600), CancellationToken::new())
        .expect("far scope opens");
    open(&mut table, 1);

    let published = table.next_deadline().expect("two walls publish");
    assert!(
        published <= Instant::now() + Duration::from_secs(61),
        "the earliest wall wins: {published:?}"
    );
    assert!(table.seal(near).expect("seal").is_empty());
    let published = table.next_deadline().expect("the far wall remains");
    assert!(
        published > Instant::now() + Duration::from_mins(50),
        "a closed scope no longer publishes its wall: {published:?}"
    );
    assert_eq!(table.cancel(CancelTarget::Scope(far)), []);
    assert!(
        table.next_deadline().is_none(),
        "a cancelled scope publishes no wall"
    );
}

#[test]
fn cleanup_reports_only_unfinished_calls_and_releases_retained_bytes() {
    let host_bytes = Arc::new(AtomicUsize::new(0));
    let mut table = ScopeTable::new(Arc::clone(&host_bytes), no_price());
    let scope = open(&mut table, 4);
    let done = queued(&mut table, scope);
    let pending = queued(&mut table, scope);
    let also_done = queued(&mut table, scope);
    table
        .complete(done, ok_outcome(&table, done), 500)
        .expect("complete");
    table
        .complete(also_done, ok_outcome(&table, also_done), 300)
        .expect("complete");
    assert_eq!(
        host_bytes.load(Ordering::SeqCst),
        800,
        "undelivered results stay retained"
    );
    let (complete, unfinished) = table.cleanup();
    assert!(!complete, "a queued task leaves the cleanup incomplete");
    let expected = table.call_id(pending).expect("call id");
    assert_eq!(&*unfinished, [expected], "only the unfinished call reports");
    assert_eq!(
        host_bytes.load(Ordering::SeqCst),
        0,
        "cleanup releases every retained byte"
    );

    let mut settled = ScopeTable::new(Arc::clone(&host_bytes), no_price());
    let scope = open(&mut settled, 1);
    let only = queued(&mut settled, scope);
    settled
        .complete(only, ok_outcome(&settled, only), 100)
        .expect("complete");
    let (complete, unfinished) = settled.cleanup();
    assert!(complete, "every task settled");
    assert!(unfinished.is_empty());
    assert_eq!(host_bytes.load(Ordering::SeqCst), 0);
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

/// Records when the executor drops one held work future.
struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn child_owner_runs_held_work_and_parent_stop_joins_it() {
    let runtime = tokio::runtime::Handle::current();
    let parent = SessionTasks::new();
    let child = SessionTasks::child(&parent, &runtime);
    assert_eq!(
        parent.tracked(),
        1,
        "exactly one parent-tracked drain watcher is admitted"
    );
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    child.spawn({
        let started = Arc::clone(&started);
        let dropped = Arc::clone(&dropped);
        async move {
            let _guard = DropFlag(dropped);
            started.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "a fresh child is not cancelled on creation: its held work runs"
    );
    assert_eq!(child.tracked(), 1);
    parent.stop().await;
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "parent stop joins the held child task before returning"
    );
    assert_eq!(child.tracked(), 0, "the child drained inside parent stop");
    assert_eq!(
        parent.tracked(),
        0,
        "the drain watcher joined before return"
    );
}

#[tokio::test]
async fn stop_returns_only_after_joined_work_and_refuses_late_registration() {
    let tasks = SessionTasks::new();
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    tasks.spawn({
        let started = Arc::clone(&started);
        let dropped = Arc::clone(&dropped);
        async move {
            let _guard = DropFlag(dropped);
            started.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(started.load(Ordering::SeqCst), 1);
    tasks.stop().await;
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "stop joins the held work"
    );
    assert_eq!(tasks.tracked(), 0);
    // Registration after stop returned is refused: nothing starts, nothing
    // detaches, and the tracker stays empty.
    let late = Arc::new(AtomicUsize::new(0));
    tasks.spawn({
        let late = Arc::clone(&late);
        async move {
            late.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(late.load(Ordering::SeqCst), 0, "late work never starts");
    assert_eq!(tasks.tracked(), 0);
    // Every clone of the owner shares the closed admission gate.
    let clone = tasks.clone();
    let cloned = Arc::new(AtomicUsize::new(0));
    clone.spawn({
        let cloned = Arc::clone(&cloned);
        async move {
            cloned.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(cloned.load(Ordering::SeqCst), 0, "clone work never starts");
    assert_eq!(tasks.tracked(), 0);
    // Stop stays idempotent after the drain.
    tasks.stop().await;
    assert_eq!(tasks.tracked(), 0);
}

#[tokio::test]
async fn admission_closes_before_an_interleaved_stop_returns() {
    // Controlled single-thread interleaving: the current-thread executor
    // runs the spawned stop to its first await (the tracker wait) at the
    // explicit yield, so the raced registration observes the closed gate
    // deterministically instead of racing a real scheduler.
    let tasks = SessionTasks::new();
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    tasks.spawn({
        let started = Arc::clone(&started);
        let dropped = Arc::clone(&dropped);
        async move {
            let _guard = DropFlag(dropped);
            started.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(started.load(Ordering::SeqCst), 1);
    let owner = tasks.clone();
    let mut stop_set = tokio::task::JoinSet::new();
    stop_set.spawn(async move { owner.stop().await });
    tokio::task::yield_now().await;
    let raced = Arc::new(AtomicUsize::new(0));
    tasks.spawn({
        let raced = Arc::clone(&raced);
        async move {
            raced.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        raced.load(Ordering::SeqCst),
        0,
        "registration cannot start once the gate closed"
    );
    stop_set
        .join_next()
        .await
        .expect("stop joins")
        .expect("stop joins");
    assert_eq!(raced.load(Ordering::SeqCst), 0);
    assert_eq!(dropped.load(Ordering::SeqCst), 1, "the held task joined");
    assert_eq!(tasks.tracked(), 0, "nothing stays registered after stop");
}

#[tokio::test]
async fn child_of_a_closed_owner_admits_nothing() {
    let runtime = tokio::runtime::Handle::current();
    let parent = SessionTasks::new();
    parent.stop().await;
    let child = SessionTasks::child(&parent, &runtime);
    assert_eq!(
        parent.tracked(),
        0,
        "no drain watcher is admitted behind a closed parent gate"
    );
    let started = Arc::new(AtomicUsize::new(0));
    child.spawn({
        let started = Arc::clone(&started);
        async move {
            started.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "a closed child drops work instead of detaching it"
    );
    assert_eq!(child.tracked(), 0);
    child.stop().await;
    assert_eq!(child.tracked(), 0);
}

#[test]
fn terminalize_latches_cancelled_once_and_keeps_recorded_usage() {
    let mut table = table();
    let scope = open(&mut table, 1);
    let Ok(Submit::Queued(running)) = table.submit(scope, model_req(), None) else {
        panic!("the model request queues");
    };
    let held = queued(&mut table, scope);
    let task_token = table
        .runnable()
        .into_iter()
        .find(|(t, _, _, _)| *t == running)
        .map(|(_, _, _, token)| token)
        .expect("the issued task token");
    assert_eq!(
        table.probe(held).expect("held").status,
        HandleStatus::Pending
    );
    assert_eq!(
        table.probe(running).expect("running").status,
        HandleStatus::Running
    );
    table
        .complete(running, infer_outcome(&table, running, 5), 0)
        .expect("the outcome records");
    let charged = table.probe(running).expect("running").usage;
    assert!(charged.requests >= 1, "the measured response charged once");
    let late = queued(&mut table, scope);

    table.terminalize();

    assert!(
        task_token.is_cancelled(),
        "the executor-held future unblocks at its cancel point"
    );
    assert_eq!(
        table.probe(held).expect("held").status,
        HandleStatus::Cancelled
    );
    assert_eq!(
        table.probe(late).expect("late").status,
        HandleStatus::Cancelled
    );
    assert_eq!(
        table.probe(held).expect("held").usage,
        ScopeUsage::default(),
        "terminalize fabricates no usage for unfinished members"
    );
    let settled = table.probe(running).expect("running");
    assert_eq!(
        settled.status,
        HandleStatus::Done,
        "a completed task keeps its recorded outcome"
    );
    assert_eq!(settled.usage, charged, "recorded usage stays unchanged");

    assert!(
        matches!(
            table.submit(scope, tool_req(), None),
            Ok(Submit::Refused(ScopeSpecError::Cancelled))
        ),
        "a terminalized scope refuses new submissions"
    );
    assert!(
        table.runnable().is_empty(),
        "no work issues after terminalize"
    );
    assert_eq!(
        table.next_deadline(),
        None,
        "no deadline survives terminalize"
    );

    // Late arrivals absorb: the first terminal stands, nothing double-fires.
    table
        .complete(held, ok_outcome(&table, held), 0)
        .expect("a late arrival absorbs");
    let outcome = table.deliver(held).expect("the terminal outcome records");
    assert!(
        matches!(outcome, OpOutcome::Terminal(HostTerminal::Cancelled)),
        "unfinished members settle terminal-cancelled: {outcome:?}"
    );
    let replay = table.deliver(held).expect("delivery is idempotent");
    assert!(
        matches!(replay, OpOutcome::Terminal(HostTerminal::Cancelled)),
        "repeated waits replay one terminal: {replay:?}"
    );
    let after = table.probe(held).expect("held");
    assert_eq!(after.status, HandleStatus::Cancelled);
    assert_eq!(
        after.usage,
        ScopeUsage::default(),
        "usage never arrives twice"
    );

    // Terminalize is idempotent.
    table.terminalize();
    assert_eq!(
        table.probe(held).expect("held").status,
        HandleStatus::Cancelled
    );
    let (complete, unfinished) = table.cleanup();
    assert!(complete, "every member settled terminal or done");
    assert!(unfinished.is_empty(), "{unfinished:?}");
}
