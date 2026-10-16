//! Exactly-once request and answer coordination for a session.
//!
//! The first answer wins and resolves the waiter; deadlines and turn
//! cancellation resolve through the actor with core attribution. Resolved
//! slots stay for late answers until deadline plus retention grace.
//!
//! Every resolution carries the [`ResolutionCause`] that explains it; the
//! waiter-facing [`Resolution`] only routes the waiter. A turn cancel claims
//! each unresolved slot of the turn under one lock: reported slots return as a
//! batch, and slots whose `RequestOpened` the fold has not stepped yet keep the
//! claim parked until [`Broker::report_opened`] hands it over.

use dal_core::{Answer, ClientId, Owner, Question, Request, RequestId, ResolutionCause, TurnId};
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

/// Coordinates request ownership, answer attribution, deadlines, and durable
/// resolution delivery.
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
    answer: Option<oneshot::Sender<Settled>>,
    resolved_by: Option<ClientId>,
    deadline: Instant,
    /// Whether the fold has stepped this request's `RequestOpened`.
    reported: bool,
    /// A turn-cancel claim made before the request was reported; handed to the
    /// actor by [`Broker::report_opened`] so its record follows `RequestOpened`.
    parked: Option<Resolved>,
}

impl RequestSlot {
    /// Claims the unresolved slot for `core` with a `Cancel` answer.
    fn claim_cancel(&mut self, cause: ResolutionCause) -> Resolved {
        let by = core_client();
        self.resolved_by = Some(by.clone());
        Resolved {
            request: self.request.clone(),
            answer: Answer::Cancel,
            by,
            resolution: Resolution::Cancelled,
            cause,
            was_default: false,
        }
    }

    /// Claims the unresolved slot for `core` with the request default.
    fn claim_default(&mut self, cause: ResolutionCause) -> Resolved {
        let by = core_client();
        self.resolved_by = Some(by.clone());
        Resolved {
            request: self.request.clone(),
            answer: self.request.default.clone(),
            by,
            resolution: Resolution::Unavailable,
            cause,
            was_default: true,
        }
    }
}

/// How an open request reached its answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Resolution {
    /// A client answered.
    Answered,
    /// No controller answered before the deadline, so the broker applied
    /// the request's fail-closed default. This is not a user decision.
    Unavailable,
    /// The turn ended, or the request went away, before any answer.
    Cancelled,
}

/// The settled end of one request, as its waiter sees it.
#[derive(Debug)]
pub(crate) struct Settled {
    /// The winning answer, or the request default when unavailable.
    pub(crate) answer: Answer,
    /// The winning client, `core` for deadlines and cancellation.
    pub(crate) by: ClientId,
    /// How the request reached this answer.
    pub(crate) resolution: Resolution,
    /// The journaled cause when this settlement released a durable record;
    /// `None` for a fail-closed settlement, which records nothing. The fold
    /// never infers a cause from the answer or attribution, so echo callers
    /// must skip a `None` settlement instead of filling one in.
    pub(crate) cause: Option<ResolutionCause>,
}

/// The answering end of one open request; resolves to its settled end.
pub(crate) struct AnswerWait {
    rx: oneshot::Receiver<Settled>,
}

impl Future for AnswerWait {
    type Output = Settled;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(settled)) => Poll::Ready(settled),
            // The sender is gone, so no record journaled: a `None` cause
            // tells echo callers to skip this settlement, never infer one.
            Poll::Ready(Err(_)) => Poll::Ready(Settled {
                answer: Answer::Cancel,
                by: core_client(),
                resolution: Resolution::Cancelled,
                cause: None,
            }),
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
    /// How the broker reached this resolution.
    pub(crate) resolution: Resolution,
    /// Why the request resolved; provenance for the journal and clients,
    /// independent of how the waiter is routed.
    pub(crate) cause: ResolutionCause,
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

    /// Opens a broker-owned request and returns its waiter. A request with
    /// a turn ends with that turn; one without (a slash command's grant
    /// question) ends only by answer, deadline, or its caller giving up.
    pub(crate) fn open(
        &self,
        owner: Owner,
        question: Question,
        turn: impl Into<Option<TurnId>>,
        deadline: Instant,
    ) -> (Request, AnswerWait) {
        let turn = turn.into();
        let request = Request {
            id: RequestId::new_v7(),
            turn,
            owner,
            default: default_for(&question),
            timeout: deadline.saturating_duration_since(Instant::now()),
            question,
        };
        let (waiter, slot) = slot_for(&request, turn, deadline);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.open_order.push_back(request.id);
        state.slots.insert(request.id, slot);
        (request, waiter)
    }

    /// Tracks a fold-minted request, returning its waiter. The fold stepped
    /// its `RequestOpened` before the actor tracks it, so the slot starts
    /// reported.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Borrowing requires changing the actor call site outside this cleanup scope."
    )]
    pub(crate) fn track(&self, request: Request, deadline: Instant) -> AnswerWait {
        let (waiter, mut slot) = slot_for(&request, request.turn, deadline);
        slot.reported = true;
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
        Ok(Resolved {
            request: slot.request.clone(),
            answer,
            by,
            resolution: Resolution::Answered,
            cause: ResolutionCause::Answered,
            was_default: false,
        })
    }

    /// Records that the fold stepped this request's `RequestOpened` and takes
    /// the turn-cancel claim parked while the request was unreported.
    ///
    /// Marking and taking happen under one lock, so a cancel can neither slip
    /// between them nor be seen twice: a cancel that won first left its claim
    /// here, and a cancel that comes after sees the slot reported and queues
    /// its claim instead. `None` means no claim is waiting, or the request is
    /// unknown.
    pub(crate) fn report_opened(&self, id: RequestId) -> Option<Resolved> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state.slots.get_mut(&id)?;
        slot.reported = true;
        slot.parked.take()
    }

    /// Claims every unresolved request of a cancelled turn as `Cancelled`.
    ///
    /// One pass under one lock, oldest request first. Reported requests return
    /// for the caller to queue; unreported ones keep their claim parked for
    /// [`Self::report_opened`] and are not returned. Either way each slot is
    /// resolved here once, so later answers, withdrawals, deadlines, and turn
    /// resolutions find it taken.
    pub(crate) fn cancel_turn(&self, turn: TurnId) -> Vec<Resolved> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let BrokerState { slots, open_order } = &mut *state;
        let mut queued = Vec::new();
        for id in &*open_order {
            let Some(slot) = slots.get_mut(id) else {
                continue;
            };
            if slot.turn != Some(turn) || slot.resolved_by.is_some() {
                continue;
            }
            let claim = slot.claim_cancel(ResolutionCause::Cancelled);
            if slot.reported {
                queued.push(claim);
            } else {
                slot.parked = Some(claim);
            }
        }
        queued
    }

    /// Snapshots every claim still parked for an unreported request, oldest
    /// first. The slots stay resolved; a closing actor publishes or cancels
    /// these claims.
    pub(crate) fn parked(&self) -> Vec<Resolved> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .open_order
            .iter()
            .filter_map(|id| state.slots.get(id))
            .filter_map(|slot| slot.parked.clone())
            .collect()
    }

    /// Withdraws one unresolved request on its owner's behalf: a `Cancel`
    /// answered by `core` with cause `Withdrawn`. `None` when the request is
    /// unknown or already resolved, so a withdrawal never overrides a winner.
    pub(crate) fn withdraw(&self, id: RequestId) -> Option<Resolved> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state.slots.get_mut(&id)?;
        if slot.resolved_by.is_some() {
            return None;
        }
        Some(slot.claim_cancel(ResolutionCause::Withdrawn))
    }

    /// Resolves every still-unresolved request of one turn as a core `Cancel`,
    /// oldest first, reported or not; resolved slots stay for late answers.
    ///
    /// This is the actor's backstop after a turn's terminal record, so `cause`
    /// is `Cancelled` when the driver confirmed a cancel and `Withdrawn` for
    /// any other end. Slots a cancel or withdrawal already claimed are skipped.
    ///
    /// # Panics
    ///
    /// Panics when `cause` is neither `Cancelled` nor `Withdrawn`.
    pub(crate) fn resolve_turn(&self, turn: TurnId, cause: ResolutionCause) -> Vec<Resolved> {
        assert!(
            matches!(
                cause,
                ResolutionCause::Cancelled | ResolutionCause::Withdrawn
            ),
            "a turn resolution is Cancelled or Withdrawn, not {cause:?}."
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let BrokerState { slots, open_order } = &mut *state;
        let mut resolved = Vec::new();
        for id in &*open_order {
            let Some(slot) = slots.get_mut(id) else {
                continue;
            };
            if slot.turn != Some(turn) || slot.resolved_by.is_some() {
                continue;
            }
            resolved.push(slot.claim_cancel(cause));
        }
        resolved
    }

    /// Resolves requests past their deadline with their default answer.
    ///
    /// Slots resolved longer than the retention grace ago are pruned, so
    /// the map stays bounded in days-long sessions.
    ///
    /// An expired request resolves as `NoController` when `unattended` says no
    /// answerer for its question is attached, otherwise as `TimedOut`. The
    /// probe runs after the broker lock is released, so it may take any lock
    /// an attachment registry needs.
    pub(crate) fn expire(
        &self,
        now: Instant,
        unattended: impl Fn(&Question) -> bool,
    ) -> Vec<Resolved> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut resolved = Vec::new();
        for slot in state.slots.values_mut() {
            if slot.resolved_by.is_some() || slot.deadline > now {
                continue;
            }
            resolved.push(slot.claim_default(ResolutionCause::TimedOut));
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
        drop(state);
        for item in &mut resolved {
            if unattended(&item.request.question) {
                item.cause = ResolutionCause::NoController;
            }
        }
        resolved
    }
    /// Resolves one request past its deadline with its default answer.
    ///
    /// Unlike [`Self::expire`], this leaves unrelated requests untouched.
    /// Local waiters use it when their timer fires before the actor's
    /// periodic sweep. `unattended` says no answerer for the question is
    /// attached, which makes the cause `NoController` instead of `TimedOut`.
    pub(crate) fn expire_one(
        &self,
        id: RequestId,
        now: Instant,
        unattended: bool,
    ) -> Option<Resolved> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state.slots.get_mut(&id)?;
        if slot.resolved_by.is_some() || slot.deadline > now {
            return None;
        }
        let cause = if unattended {
            ResolutionCause::NoController
        } else {
            ResolutionCause::TimedOut
        };
        Some(slot.claim_default(cause))
    }

    /// Delivers a broker resolution after the owning actor has journaled it.
    pub(crate) fn release(&self, resolved: &Resolved) {
        let sender = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .slots
            .get_mut(&resolved.request.id)
            .and_then(|slot| slot.answer.take());
        let Some(sender) = sender else {
            return;
        };
        let _ = sender.send(Settled {
            answer: resolved.answer.clone(),
            by: resolved.by.clone(),
            resolution: resolved.resolution,
            // The actor releases only after the terminal record's journal
            // receipt, so `resolved.cause` is journaled by construction.
            cause: Some(resolved.cause),
        });
    }

    /// Cancels a waiter whose answer could not be made durable.
    pub(crate) fn cancel(&self, resolved: &Resolved) {
        let sender = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .slots
            .get_mut(&resolved.request.id)
            .and_then(|slot| slot.answer.take());
        let Some(sender) = sender else {
            return;
        };
        // The answer could not be made durable, so no record journaled.
        // Echo callers skip a `None` cause instead of inferring one.
        let _ = sender.send(Settled {
            answer: Answer::Cancel,
            by: core_client(),
            resolution: Resolution::Cancelled,
            cause: None,
        });
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
        _ => Duration::from_mins(30),
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
            reported: false,
            parked: None,
        },
    )
}

#[cfg(test)]
mod tests;
