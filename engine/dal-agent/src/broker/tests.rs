use std::num::NonZeroU64;
use std::sync::Barrier;
use std::time::Duration;

use dal_core::{Answer, ClientId, Owner, Question, Request, RequestId, ResolutionCause, TurnId};
use tokio::time::Instant;

use super::{AnswerWait, Broker, Resolution, Resolved, Settled};
use crate::error::AgentError;

const LIMIT: Duration = Duration::from_secs(10);

/// Report/cancel races per proof; enough interleavings to catch a split
/// mark-then-take without slowing the suite.
const RACES: usize = 1_000;

fn turn() -> TurnId {
    TurnId::new(NonZeroU64::new(1).unwrap())
}

fn other_turn() -> TurnId {
    TurnId::new(NonZeroU64::new(2).unwrap())
}

fn client(name: &str) -> ClientId {
    ClientId::new(name)
}

fn text() -> Question {
    Question::Text {
        prompt: "why?".into(),
        placeholder: None,
    }
}

fn grant() -> Question {
    Question::Grant {
        ext: "net".into(),
        origin: "user".into(),
        capabilities: vec!["net".into()],
        detail: None,
    }
}

fn open_in(
    broker: &Broker,
    question: Question,
    turn: impl Into<Option<TurnId>>,
) -> (RequestId, AnswerWait) {
    let (request, waiter) = broker.open(Owner::Core, question, turn, Instant::now() + LIMIT);
    (request.id, waiter)
}

fn open_text(broker: &Broker) -> (RequestId, AnswerWait) {
    open_in(broker, text(), turn())
}

/// An open turn request the fold has already stepped `RequestOpened` for.
fn open_reported(broker: &Broker) -> (RequestId, AnswerWait) {
    let (id, waiter) = open_text(broker);
    assert!(
        broker.report_opened(id).is_none(),
        "nothing is parked before a cancel"
    );
    (id, waiter)
}

/// Probe for a deadline sweep where an answerer is attached to every question.
fn with_answerer(_: &Question) -> bool {
    false
}

/// Probe for a deadline sweep where no answerer is attached to any question.
fn without_answerer(_: &Question) -> bool {
    true
}

async fn settled(waiter: AnswerWait) -> Settled {
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("the request settles")
}

fn ids(items: &[Resolved]) -> Vec<RequestId> {
    items.iter().map(|item| item.request.id).collect()
}

/// A core `Cancel` claim for `id` with the given cause.
fn assert_core_cancel(item: &Resolved, id: RequestId, cause: ResolutionCause) {
    assert_eq!(item.request.id, id);
    assert_eq!(item.answer, Answer::Cancel);
    assert_eq!(item.by, client("core"));
    assert_eq!(item.resolution, Resolution::Cancelled);
    assert_eq!(item.cause, cause);
    assert!(!item.was_default, "a cancellation is not a default");
}

fn already_resolved_by(error: &AgentError, id: RequestId, winner: &ClientId) -> bool {
    matches!(
        error,
        AgentError::AlreadyResolved { id: lost, by } if *lost == id && by == winner
    )
}

#[tokio::test(start_paused = true)]
async fn a_deadline_with_no_answer_is_unavailable_not_a_decision() {
    let broker = Broker::new();
    let (_, waiter) = open_text(&broker);
    tokio::time::advance(LIMIT).await;
    let resolved = broker.expire(Instant::now(), with_answerer);
    assert_eq!(resolved.len(), 1);
    assert!(resolved[0].was_default);
    assert_eq!(resolved[0].cause, ResolutionCause::TimedOut);
    // Delivery waits for the actor's post-journal release.
    broker.release(&resolved[0]);
    let end = settled(waiter).await;
    assert_eq!(end.resolution, Resolution::Unavailable);
    assert_eq!(end.by, client("core"));
    assert_eq!(end.answer, Answer::Cancel, "the default stays fail closed");
    assert_eq!(
        end.cause,
        Some(ResolutionCause::TimedOut),
        "a release carries the journaled cause"
    );
}

#[tokio::test(start_paused = true)]
async fn a_client_answer_and_a_turn_cancel_are_not_unavailable() {
    let broker = Broker::new();
    let (id, waiter) = open_text(&broker);
    let resolved = broker
        .answer(id, Answer::Decline, client("tui"))
        .expect("the first answer wins");
    assert_eq!(resolved.cause, ResolutionCause::Answered);
    assert!(!resolved.was_default);
    broker.release(&resolved);
    let answered = settled(waiter).await;
    assert_eq!(answered.resolution, Resolution::Answered);
    assert_eq!(answered.by, client("tui"));
    assert_eq!(
        answered.cause,
        Some(ResolutionCause::Answered),
        "a release carries the journaled cause"
    );

    let (_, waiter) = open_text(&broker);
    let cancelled = broker.resolve_turn(turn(), ResolutionCause::Cancelled);
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].cause, ResolutionCause::Cancelled);
    for resolved in &cancelled {
        broker.release(resolved);
    }
    let end = settled(waiter).await;
    assert_eq!(end.resolution, Resolution::Cancelled);
    assert_eq!(
        end.cause,
        Some(ResolutionCause::Cancelled),
        "a turn-cancel release carries its cause"
    );
}

#[tokio::test(start_paused = true)]
async fn the_deadline_is_absolute_from_raise() {
    let broker = Broker::new();
    let (_, waiter) = open_text(&broker);
    tokio::time::advance(Duration::from_secs(9)).await;
    assert!(
        broker.expire(Instant::now(), with_answerer).is_empty(),
        "the request is open until its deadline"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let resolved = broker.expire(Instant::now(), with_answerer);
    assert_eq!(resolved.len(), 1);
    broker.release(&resolved[0]);
    assert_eq!(settled(waiter).await.resolution, Resolution::Unavailable);
}

#[tokio::test(start_paused = true)]
async fn a_deadline_with_no_answerer_attached_is_no_controller_and_still_fails_closed() {
    let broker = Broker::new();
    let (_, waiter) = open_text(&broker);
    tokio::time::advance(LIMIT).await;
    let resolved = broker.expire(Instant::now(), without_answerer);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].cause, ResolutionCause::NoController);
    assert!(resolved[0].was_default, "the default answer still applies");
    assert_eq!(resolved[0].answer, Answer::Cancel);
    broker.release(&resolved[0]);
    let end = settled(waiter).await;
    assert_eq!(end.resolution, Resolution::Unavailable);
    assert_eq!(end.answer, Answer::Cancel);
    assert_eq!(
        end.cause,
        Some(ResolutionCause::NoController),
        "a release carries the journaled cause"
    );
}

/// A fail-closed settlement records nothing, so its waiter-facing cause is
/// `None`: echo callers skip it instead of inferring a cause from the
/// answer or attribution. Both fail-closed constructors — the explicit
/// `cancel` and a dropped sender — settle the same way.
#[tokio::test(start_paused = true)]
async fn a_fail_closed_settlement_carries_no_cause_for_echo_callers() {
    let broker = Broker::new();
    let (id, waiter) = open_text(&broker);
    let claimed = broker
        .answer(id, Answer::Approve, client("tui"))
        .expect("the first answer wins");
    broker.cancel(&claimed);
    let end = settled(waiter).await;
    assert_eq!(end.resolution, Resolution::Cancelled);
    assert_eq!(end.cause, None, "a cancelled waiter journaled nothing");

    let (_, waiter) = open_text(&broker);
    drop(broker);
    let end = settled(waiter).await;
    assert_eq!(end.resolution, Resolution::Cancelled);
    assert_eq!(end.cause, None, "a dropped sender journaled nothing");
}

#[tokio::test(start_paused = true)]
async fn the_attachment_probe_judges_each_expired_question_by_its_own_kind() {
    let broker = Broker::new();
    let (text_id, _text_waiter) = open_in(&broker, text(), turn());
    let (grant_id, _grant_waiter) = open_in(&broker, grant(), None);
    tokio::time::advance(LIMIT).await;
    let resolved = broker.expire(Instant::now(), |question| {
        matches!(question, Question::Grant { .. })
    });
    assert_eq!(resolved.len(), 2);
    let cause_of = |id: RequestId| {
        resolved
            .iter()
            .find(|item| item.request.id == id)
            .expect("the request expired")
            .cause
    };
    assert_eq!(cause_of(text_id), ResolutionCause::TimedOut);
    assert_eq!(cause_of(grant_id), ResolutionCause::NoController);
    let grant_item = resolved
        .iter()
        .find(|item| item.request.id == grant_id)
        .unwrap();
    assert_eq!(
        grant_item.answer,
        Answer::Decline,
        "a grant still declines fail closed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_local_deadline_resolves_only_its_request_once_with_its_cause() {
    let broker = Broker::new();
    let (first, _first_waiter) = open_text(&broker);
    let (second, _second_waiter) = open_text(&broker);
    assert!(
        broker.expire_one(first, Instant::now(), false).is_none(),
        "the request is open until its deadline"
    );
    tokio::time::advance(LIMIT).await;
    let timed_out = broker
        .expire_one(first, Instant::now(), false)
        .expect("the deadline passed");
    assert_eq!(timed_out.cause, ResolutionCause::TimedOut);
    assert!(timed_out.was_default);
    assert_eq!(timed_out.resolution, Resolution::Unavailable);
    assert!(
        broker.expire_one(first, Instant::now(), true).is_none(),
        "a request expires once"
    );
    let unattended = broker
        .expire_one(second, Instant::now(), true)
        .expect("the sibling is untouched until its own call");
    assert_eq!(unattended.cause, ResolutionCause::NoController);
    assert!(unattended.was_default);
}

#[tokio::test(start_paused = true)]
async fn a_cancel_before_the_report_parks_its_claim_for_the_report() {
    let broker = Broker::new();
    let (id, waiter) = open_text(&broker);

    let queued = broker.cancel_turn(turn());
    assert!(queued.is_empty(), "an unreported request is not queued");
    assert!(
        broker.open_requests().is_empty(),
        "the cancel already claimed the request"
    );
    let lost = broker
        .answer(id, Answer::Approve, client("tui"))
        .expect_err("the cancel won");
    assert!(already_resolved_by(&lost, id, &client("core")), "{lost:?}");

    let parked = broker.parked();
    assert_eq!(ids(&parked), vec![id]);
    let claim = broker
        .report_opened(id)
        .expect("the report receives the parked claim");
    assert_core_cancel(&claim, id, ResolutionCause::Cancelled);
    assert!(broker.parked().is_empty(), "the report took the claim");
    assert!(
        broker.report_opened(id).is_none(),
        "the claim is handed over once"
    );

    broker.release(&claim);
    let end = settled(waiter).await;
    assert_eq!(end.resolution, Resolution::Cancelled);
    assert_eq!(end.answer, Answer::Cancel);
}

#[tokio::test(start_paused = true)]
async fn a_cancel_after_the_report_queues_its_claim_once() {
    let broker = Broker::new();
    let (id, waiter) = open_reported(&broker);

    let queued = broker.cancel_turn(turn());
    assert_eq!(queued.len(), 1);
    assert_core_cancel(&queued[0], id, ResolutionCause::Cancelled);
    assert!(
        broker.parked().is_empty(),
        "a reported request parks nothing"
    );
    assert!(
        broker.report_opened(id).is_none(),
        "nothing is left to hand over"
    );
    assert!(
        broker.cancel_turn(turn()).is_empty(),
        "a second cancel finds the request already claimed"
    );

    broker.release(&queued[0]);
    assert_eq!(settled(waiter).await.resolution, Resolution::Cancelled);
}

#[test]
fn a_racing_report_and_cancel_deliver_the_claim_exactly_once() {
    for _ in 0..RACES {
        let broker = Broker::new();
        let (id, _waiter) = open_text(&broker);
        let start = Barrier::new(2);
        let (reported, queued) = std::thread::scope(|scope| {
            let report = scope.spawn(|| {
                start.wait();
                broker.report_opened(id)
            });
            let cancel = scope.spawn(|| {
                start.wait();
                broker.cancel_turn(turn())
            });
            (report.join().unwrap(), cancel.join().unwrap())
        });
        let delivered = usize::from(reported.is_some()) + queued.len();
        assert_eq!(
            delivered, 1,
            "the claim reaches exactly one of the report and the queue"
        );
        assert!(
            broker.parked().is_empty(),
            "no claim is stranded between the two"
        );
        assert_eq!(broker.open_requests(), [] as [dal_core::Request; 0]);
    }
}

#[test]
fn a_cancel_claims_its_turn_oldest_first_and_leaves_other_requests_open() {
    let broker = Broker::new();
    let (first, _first_waiter) = open_reported(&broker);
    let (unreported, _unreported_waiter) = open_text(&broker);
    let (third, _third_waiter) = open_reported(&broker);
    let (elsewhere, _elsewhere_waiter) = open_in(&broker, text(), other_turn());
    let (turnless, _turnless_waiter) = open_in(&broker, text(), None);

    let queued = broker.cancel_turn(turn());
    assert_eq!(ids(&queued), vec![first, third]);
    for item in &queued {
        assert_core_cancel(item, item.request.id, ResolutionCause::Cancelled);
    }
    assert_eq!(ids(&broker.parked()), vec![unreported]);
    let still_open: Vec<RequestId> = broker
        .open_requests()
        .iter()
        .map(|request| request.id)
        .collect();
    assert_eq!(
        still_open,
        vec![elsewhere, turnless],
        "only the cancelled turn's requests are claimed"
    );

    let claim = broker
        .report_opened(unreported)
        .expect("the late report receives its claim");
    assert_core_cancel(&claim, unreported, ResolutionCause::Cancelled);
}

#[test]
fn a_fold_minted_request_is_reported_from_the_start() {
    let broker = Broker::new();
    let request = Request {
        id: RequestId::new_v7(),
        turn: Some(turn()),
        owner: Owner::Core,
        question: Question::Confirm { text: "go?".into() },
        timeout: LIMIT,
        default: Answer::Cancel,
    };
    let _waiter = broker.track(request.clone(), Instant::now() + LIMIT);

    let queued = broker.cancel_turn(turn());
    assert_eq!(ids(&queued), vec![request.id]);
    assert!(broker.parked().is_empty());
}

#[test]
fn a_value_answer_to_a_question_that_takes_none_is_refused_and_leaves_it_open() {
    let broker = Broker::new();
    let value = || Answer::Value(dal_core::RawJson::parse("\"free text\"").unwrap());
    let confirm = Question::Confirm { text: "go?".into() };
    for question in [grant(), confirm] {
        let (id, _waiter) = open_in(&broker, question, turn());
        let refused = broker
            .answer(id, value(), client("acp"))
            .expect_err("a value cannot answer this question");
        assert!(matches!(refused, AgentError::Invalid(_)), "{refused:?}");
        let won = broker
            .answer(id, Answer::Decline, client("tui"))
            .expect("the refused answer left the request open");
        assert_eq!(won.by, client("tui"), "the refused client won nothing");
    }
    let (id, _waiter) = open_text(&broker);
    let accepted = broker
        .answer(id, value(), client("tui"))
        .expect("a text question takes a value");
    assert_eq!(accepted.answer, value());

    let unknown = broker
        .answer(RequestId::new_v7(), Answer::Approve, client("tui"))
        .expect_err("an unknown request cannot be answered");
    assert!(matches!(unknown, AgentError::Invalid(_)), "{unknown:?}");
}

#[tokio::test(start_paused = true)]
async fn an_unanswered_question_falls_back_to_its_kinds_fail_closed_default() {
    let broker = Broker::new();
    let approval = Question::Approval {
        tool: "bash".into(),
        preview: dal_core::Preview {
            title: "run".into(),
            body: "rm -rf /".into(),
            digest: None,
        },
        grant: None,
        call: None,
    };
    let select = Question::Select {
        prompt: "pick".into(),
        options: Vec::new(),
        multi: false,
        preview: None,
    };
    let confirm = Question::Confirm { text: "go?".into() };
    let mut expected = Vec::new();
    for (question, default) in [
        (approval, Answer::Decline),
        (select, Answer::Cancel),
        (confirm, Answer::Cancel),
    ] {
        let (id, _waiter) = open_in(&broker, question, turn());
        expected.push((id, default));
    }
    tokio::time::advance(LIMIT).await;
    let resolved = broker.expire(Instant::now(), with_answerer);
    assert_eq!(resolved.len(), 3);
    for (id, default) in expected {
        let item = resolved
            .iter()
            .find(|item| item.request.id == id)
            .expect("the request expired");
        assert_eq!(
            item.answer, default,
            "default for {:?}",
            item.request.question
        );
        assert!(item.was_default);
    }
}

#[test]
fn an_answer_before_a_cancel_wins_and_the_cancel_claims_nothing() {
    let broker = Broker::new();
    let (id, _waiter) = open_reported(&broker);
    let first = broker
        .answer(id, Answer::Approve, client("tui"))
        .expect("the first answer wins");
    assert_eq!(first.cause, ResolutionCause::Answered);
    assert_eq!(first.by, client("tui"));

    assert!(broker.cancel_turn(turn()).is_empty());
    assert!(broker.parked().is_empty());
    let second = broker
        .answer(id, Answer::Decline, client("acp"))
        .expect_err("a second answer loses");
    assert!(
        already_resolved_by(&second, id, &client("tui")),
        "{second:?}"
    );
}

#[test]
fn a_cancel_before_an_answer_wins_and_the_answer_is_already_resolved() {
    let broker = Broker::new();
    let (id, _waiter) = open_reported(&broker);
    let queued = broker.cancel_turn(turn());
    assert_eq!(queued.len(), 1);

    let lost = broker
        .answer(id, Answer::Approve, client("tui"))
        .expect_err("the cancel won");
    assert!(already_resolved_by(&lost, id, &client("core")), "{lost:?}");
}

#[test]
fn a_withdrawal_names_its_cause_and_never_overrides_a_winner() {
    let broker = Broker::new();
    let (withdrawn, _withdrawn_waiter) = open_reported(&broker);
    let (answered, _answered_waiter) = open_reported(&broker);

    let item = broker
        .withdraw(withdrawn)
        .expect("an open request withdraws");
    assert_core_cancel(&item, withdrawn, ResolutionCause::Withdrawn);
    assert!(
        broker.withdraw(withdrawn).is_none(),
        "a request withdraws once"
    );
    let lost = broker
        .answer(withdrawn, Answer::Approve, client("tui"))
        .expect_err("the withdrawal won");
    assert!(
        already_resolved_by(&lost, withdrawn, &client("core")),
        "{lost:?}"
    );

    broker
        .answer(answered, Answer::Approve, client("tui"))
        .expect("the answer wins");
    assert!(
        broker.withdraw(answered).is_none(),
        "an answered request cannot be withdrawn"
    );

    let (claimed, _claimed_waiter) = open_reported(&broker);
    assert_eq!(ids(&broker.cancel_turn(turn())), vec![claimed]);
    assert!(
        broker.withdraw(claimed).is_none(),
        "a cancel-claimed request cannot be withdrawn"
    );
    assert!(
        broker.withdraw(RequestId::new_v7()).is_none(),
        "an unknown request has nothing to withdraw"
    );
}

#[tokio::test(start_paused = true)]
async fn an_expiry_and_a_cancel_never_both_resolve_one_request() {
    let broker = Broker::new();
    let (expired_first, _first_waiter) = open_reported(&broker);
    tokio::time::advance(LIMIT).await;
    let expired = broker.expire(Instant::now(), with_answerer);
    assert_eq!(ids(&expired), vec![expired_first]);
    assert!(
        broker.cancel_turn(turn()).is_empty(),
        "an expired request cannot be cancel-claimed"
    );
    let lost = broker
        .answer(expired_first, Answer::Approve, client("tui"))
        .expect_err("the deadline won");
    assert!(
        already_resolved_by(&lost, expired_first, &client("core")),
        "{lost:?}"
    );

    let (cancelled_first, _second_waiter) = open_reported(&broker);
    let queued = broker.cancel_turn(turn());
    assert_eq!(ids(&queued), vec![cancelled_first]);
    tokio::time::advance(LIMIT).await;
    assert!(
        broker.expire(Instant::now(), with_answerer).is_empty(),
        "a cancel-claimed request does not also expire"
    );
    assert!(
        broker
            .expire_one(cancelled_first, Instant::now(), false)
            .is_none()
    );
}

#[test]
fn a_turn_resolution_names_its_cause_and_skips_claimed_requests() {
    let broker = Broker::new();
    let (first, _first_waiter) = open_reported(&broker);
    let (unreported, _unreported_waiter) = open_text(&broker);
    let (third, _third_waiter) = open_reported(&broker);

    assert_eq!(
        ids(&broker.cancel_turn(turn())),
        vec![first, third],
        "the cancel claims the reported requests and parks the other"
    );
    let (fresh, _fresh_waiter) = open_reported(&broker);
    let backstop = broker.resolve_turn(turn(), ResolutionCause::Withdrawn);
    assert_eq!(
        ids(&backstop),
        vec![fresh],
        "the backstop resolves only what no cancel claimed, parked claim included"
    );
    assert_core_cancel(&backstop[0], fresh, ResolutionCause::Withdrawn);

    let parked = broker.parked();
    assert_eq!(ids(&parked), vec![unreported]);
    assert_core_cancel(&parked[0], unreported, ResolutionCause::Cancelled);

    let (late, _late_waiter) = open_reported(&broker);
    let confirmed = broker.resolve_turn(turn(), ResolutionCause::Cancelled);
    assert_eq!(ids(&confirmed), vec![late]);
    assert_core_cancel(&confirmed[0], late, ResolutionCause::Cancelled);
}

#[test]
#[should_panic(expected = "Cancelled or Withdrawn")]
fn a_turn_resolution_rejects_a_cause_that_is_not_a_turn_end() {
    let broker = Broker::new();
    let (_id, _waiter) = open_reported(&broker);
    let _ = broker.resolve_turn(turn(), ResolutionCause::Answered);
}

#[test]
fn parked_lists_only_unreported_claims_oldest_first_and_keeps_them_resolved() {
    let broker = Broker::new();
    let (first, _first_waiter) = open_text(&broker);
    let (reported, _reported_waiter) = open_reported(&broker);
    let (second, _second_waiter) = open_text(&broker);
    assert!(broker.parked().is_empty(), "nothing is claimed yet");

    assert_eq!(ids(&broker.cancel_turn(turn())), vec![reported]);
    assert_eq!(ids(&broker.parked()), vec![first, second]);
    assert_eq!(
        ids(&broker.parked()),
        vec![first, second],
        "a snapshot does not consume the claims"
    );
    assert!(
        broker.open_requests().is_empty(),
        "a parked claim keeps its request resolved"
    );

    broker.report_opened(first).expect("the first claim");
    assert_eq!(ids(&broker.parked()), vec![second]);
}
