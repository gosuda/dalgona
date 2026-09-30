//! Exactly-once request and answer coordination for a session.
//!
//! The first answer wins and resolves the waiter; deadlines and turn
//! cancellation resolve through the actor with core attribution. Resolved
//! slots stay for late answers until deadline plus retention grace.

use dal_core::{Answer, ClientId, Owner, Question, Request, RequestId, TurnId};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::error::{AgentError, ValidationError};

/// Client identity attributed when deadlines or cancellation resolve a request.
fn core_client() -> ClientId {
    ClientId::new("core")
}

/// Resolved slots older than their deadline plus this grace are pruned.
const RETENTION_GRACE: Duration = Duration::from_secs(3600);

/// Coordinates request ownership, answer attribution, deadlines, and turn cancellation.
pub struct Broker {
    pub(crate) state: Mutex<BrokerState>,
}

pub(crate) struct BrokerState {
    pub(crate) slots: HashMap<RequestId, RequestSlot>,
    pub(crate) open_order: VecDeque<RequestId>,
}

pub(crate) struct RequestSlot {
    request: Request,
    turn: Option<TurnId>,
    answer: Option<oneshot::Sender<(Answer, ClientId)>>,
    resolved_by: Option<ClientId>,
    deadline: Instant,
}

/// The answering end of one open request; resolves to the winning answer.
pub(crate) struct AnswerWait {
    rx: oneshot::Receiver<(Answer, ClientId)>,
}

impl Future for AnswerWait {
    type Output = (Answer, ClientId);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(resolved)) => Poll::Ready(resolved),
            Poll::Ready(Err(_)) => Poll::Ready((Answer::Cancel, core_client())),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// One broker-resolved answer awaiting its journal receipt.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    /// The resolved request.
    pub(crate) request: Request,
    /// The winning answer, or the request default on expiry.
    pub(crate) answer: Answer,
    /// The winning client, `core` for deadlines and cancellation.
    pub(crate) by: ClientId,
    /// Whether the broker selected the request default.
    pub(crate) was_default: bool,
}

impl Broker {
    /// An empty broker with no open or resolved requests.
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(BrokerState {
                slots: HashMap::new(),
                open_order: VecDeque::new(),
            }),
        }
    }

    /// Opens a broker-owned request and returns its waiter.
    pub(crate) fn open(
        &self,
        owner: Owner,
        question: Question,
        turn: TurnId,
        deadline: Instant,
    ) -> (Request, AnswerWait) {
        let request = Request {
            id: RequestId::new_v7(),
            turn: Some(turn),
            owner,
            default: default_for(&question),
            timeout: deadline.saturating_duration_since(Instant::now()),
            question,
        };
        let (waiter, slot) = slot_for(&request, Some(turn), deadline);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.open_order.push_back(request.id);
        state.slots.insert(request.id, slot);
        (request, waiter)
    }

    /// Tracks a fold-minted request, returning its waiter.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Borrowing requires changing the actor call site outside this cleanup scope."
    )]
    pub(crate) fn track(&self, request: Request, deadline: Instant) -> AnswerWait {
        let (waiter, slot) = slot_for(&request, request.turn, deadline);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.open_order.push_back(request.id);
        state.slots.insert(request.id, slot);
        waiter
    }

    /// Resolves one open request; the first answer wins.
    pub(crate) fn answer(
        &self,
        id: RequestId,
        answer: Answer,
        by: ClientId,
    ) -> Result<Resolved, AgentError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state.slots.get_mut(&id).ok_or_else(|| {
            AgentError::Invalid(ValidationError::new(format!("unknown request {id}.")))
        })?;
        if let Some(winner) = slot.resolved_by.clone() {
            return Err(AgentError::AlreadyResolved { id, by: winner });
        }
        if matches!(answer, Answer::Value(_)) && !accepts_value(&slot.request.question) {
            return Err(AgentError::Invalid(ValidationError::bad_answer(
                &answer,
                &slot.request.question,
            )));
        }
        slot.resolved_by = Some(by.clone());
        if let Some(sender) = slot.answer.take() {
            let _ = sender.send((answer.clone(), by.clone()));
        }
        Ok(Resolved {
            request: slot.request.clone(),
            answer,
            by,
            was_default: false,
        })
    }

    /// Resolves every open request of one turn; resolved slots stay for late answers.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Borrowing requires changing the actor call site outside this cleanup scope."
    )]
    pub(crate) fn resolve_turn(&self, turn: TurnId, answer: Answer, by: ClientId) -> Vec<Resolved> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut resolved = Vec::new();
        for slot in state.slots.values_mut() {
            if slot.turn != Some(turn) || slot.resolved_by.is_some() {
                continue;
            }
            slot.resolved_by = Some(by.clone());
            if let Some(sender) = slot.answer.take() {
                let _ = sender.send((answer.clone(), by.clone()));
            }
            resolved.push(Resolved {
                request: slot.request.clone(),
                answer: answer.clone(),
                by: by.clone(),
                was_default: false,
            });
        }
        resolved
    }

    /// Resolves requests past their deadline with their default answer.
    ///
    /// Slots resolved longer than the retention grace ago are pruned, so
    /// the map stays bounded in days-long sessions.
    pub(crate) fn expire(&self, now: Instant) -> Vec<Resolved> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut resolved = Vec::new();
        for slot in state.slots.values_mut() {
            if slot.resolved_by.is_some() || slot.deadline > now {
                continue;
            }
            let by = core_client();
            slot.resolved_by = Some(by.clone());
            let answer = slot.request.default.clone();
            if let Some(sender) = slot.answer.take() {
                let _ = sender.send((answer.clone(), by.clone()));
            }
            resolved.push(Resolved {
                request: slot.request.clone(),
                answer,
                by,
                was_default: true,
            });
        }
        let horizon = now.checked_sub(RETENTION_GRACE).unwrap_or(now);
        let expired_ids: Vec<RequestId> = state
            .slots
            .iter()
            .filter(|(_, candidate)| {
                candidate.resolved_by.is_some() && candidate.deadline <= horizon
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired_ids {
            state.slots.remove(&id);
        }
        let BrokerState { slots, open_order } = &mut *state;
        open_order.retain(|id| slots.contains_key(id));
        resolved
    }
    /// Lists unresolved requests, oldest first.
    pub(crate) fn open_requests(&self) -> Vec<Request> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .open_order
            .iter()
            .filter_map(|id| state.slots.get(id))
            .filter(|slot| slot.resolved_by.is_none())
            .map(|slot| slot.request.clone())
            .collect()
    }
}

/// The default answer a request kind falls back to at its deadline.
fn default_for(question: &Question) -> Answer {
    match question {
        Question::Approval { .. } | Question::Grant { .. } => Answer::Decline,
        Question::Select { .. } | Question::Confirm { .. } | Question::Text { .. } => {
            Answer::Cancel
        }
        _ => Answer::Cancel,
    }
}

/// The request deadline for a fold-minted question.
pub(crate) fn default_timeout(question: &Question) -> Duration {
    match question {
        Question::Approval { .. } => Duration::from_secs(300),
        Question::Grant { .. } => Duration::from_secs(120),
        _ => Duration::from_secs(1800),
    }
}

/// Whether a question kind accepts an arbitrary JSON value answer.
fn accepts_value(question: &Question) -> bool {
    matches!(question, Question::Select { .. } | Question::Text { .. })
}

fn slot_for(
    request: &Request,
    turn: Option<TurnId>,
    deadline: Instant,
) -> (AnswerWait, RequestSlot) {
    let (tx, rx) = oneshot::channel();
    (
        AnswerWait { rx },
        RequestSlot {
            request: request.clone(),
            turn,
            answer: Some(tx),
            resolved_by: None,
            deadline,
        },
    )
}
