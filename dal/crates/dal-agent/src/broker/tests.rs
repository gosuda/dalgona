use std::num::NonZeroU64;
use std::time::Duration;

use dal_core::{Answer, ClientId, Owner, Question, RequestId, TurnId};
use tokio::time::Instant;

use super::{AnswerWait, Broker, Resolution, Settled};

const LIMIT: Duration = Duration::from_secs(10);

fn turn() -> TurnId {
    TurnId::new(NonZeroU64::new(1).unwrap())
}

fn client(name: &str) -> ClientId {
    ClientId::new(name)
}

fn open_text(broker: &Broker) -> (RequestId, AnswerWait) {
    let (request, waiter) = broker.open(
        Owner::Core,
        Question::Text {
            prompt: "why?".into(),
            placeholder: None,
        },
        turn(),
        Instant::now() + LIMIT,
    );
    (request.id, waiter)
}

async fn settled(waiter: AnswerWait) -> Settled {
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("the request settles")
}

#[tokio::test(start_paused = true)]
async fn a_deadline_with_no_answer_is_unavailable_not_a_decision() {
    let broker = Broker::new();
    let (_, waiter) = open_text(&broker);
    tokio::time::advance(LIMIT).await;
    let resolved = broker.expire(Instant::now());
    assert_eq!(resolved.len(), 1);
    assert!(resolved[0].was_default);
    let end = settled(waiter).await;
    assert_eq!(end.resolution, Resolution::Unavailable);
    assert_eq!(end.by, client("core"));
    assert_eq!(end.answer, Answer::Cancel, "the default stays fail closed");
}

#[tokio::test(start_paused = true)]
async fn a_client_answer_and_a_turn_cancel_are_not_unavailable() {
    let broker = Broker::new();
    let (id, waiter) = open_text(&broker);
    broker
        .answer(id, Answer::Decline, client("tui"))
        .expect("the first answer wins");
    let answered = settled(waiter).await;
    assert_eq!(answered.resolution, Resolution::Answered);
    assert_eq!(answered.by, client("tui"));

    let (_, waiter) = open_text(&broker);
    let cancelled = broker.resolve_turn(turn(), Answer::Cancel, client("core"));
    assert_eq!(cancelled.len(), 1);
    assert_eq!(settled(waiter).await.resolution, Resolution::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn the_deadline_is_absolute_from_raise() {
    let broker = Broker::new();
    let (_, waiter) = open_text(&broker);
    tokio::time::advance(Duration::from_secs(9)).await;
    assert!(
        broker.expire(Instant::now()).is_empty(),
        "the request is open until its deadline"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(broker.expire(Instant::now()).len(), 1);
    assert_eq!(settled(waiter).await.resolution, Resolution::Unavailable);
}
