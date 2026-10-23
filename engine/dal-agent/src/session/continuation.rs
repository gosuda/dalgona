//! Owner-bound Cursor continuations: one live provider run per slot.
//!
//! A rightful owner — a turn (`TurnState`), an invocation root (the
//! `Caller::new` sites), or one relay-stream call — holds a
//! [`ContinuationOwner`]. The owner holds the single strong reference to its
//! [`ContinuationSlot`]; requests reach the slot only through a [`Weak`]
//! carried explicitly in [`RequestDeps`](super::turn::RequestDeps) or
//! [`Caller`](crate::ext::Caller). There is no lookup by session, model,
//! owner, or call ids, no adoption by names or history content, no host or
//! global table, no cap, and no idle eviction.
//!
//! One slot holds at most one live [`Conversation`] plus at most one reserved
//! handshake. The first Cursor step
//! opens the provider run ([`open_step`]); a step that ends in tool use is
//! parked ([`ContinuationSlot::park`]); private results are stashed
//! ([`ContinuationSlot::stash`]); the next step resumes with the exact
//! (id, name) cover of the durable journal plus the stash
//! ([`ContinuationSlot::respond`]); the owner end releases explicitly
//! ([`ContinuationSlot::release_if_live`]). Dropping the owner without
//! release still aborts the transport through [`ProviderRun`]'s drop, but
//! that is cleanup, not a protocol path.
//!
//! No [`Mutex`] guard is ever held across an `.await`: [`open_step`]
//! handshakes without the guard, and [`ContinuationSlot::respond`] takes the
//! conversation out of the slot (`InFlight`) for the resume. A restore guard
//! makes every exit terminal — success disarms it, and any error or dropped
//! future restores or releases through it — so the slot never strands
//! mid-resume. A concurrent owner end flips `InFlight` to `Released`, and
//! the returning resume drops the conversation instead of reinstalling it.
//!
//! Every generation step enters through [`Provider::open_run`] or
//! [`Provider::resume_run`], which acquire the admission permit before the
//! handshake and before the respond respectively. This module never
//! calls [`ProviderRun::next_step`] or [`ProviderRun::respond`] directly. A
//! parked run holds no permit; parked heartbeat and control maintenance is
//! not an active request.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use dal_core::{CallId, InferFailure, ModelRequest, ModelRoute, Part};
use dal_provider::{EventStream, HostToolResult, Provider, ProviderError, ProviderRun, RunState};
use tokio_util::sync::CancellationToken;

use super::tasks::SessionTasks;
use super::turn::RequestDeps;

#[cfg(test)]
mod private_receipt_gate;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transport_tests;

/// The strong holder of one continuation slot.
///
/// Minted once per rightful owner: one per turn (with the session tasks),
/// one per invocation root (with the session tasks), or one per relay call
/// (with fresh tasks stopped at stream end). Move-only: exactly one owner
/// holds a slot, so ending the owner means calling
/// [`ContinuationOwner::release_if_live`] and then dropping. Request handles
/// travel as [`Weak`]; they borrow the slot and never share ownership.
pub(crate) struct ContinuationOwner {
    slot: Arc<ContinuationSlot>,
}

impl ContinuationOwner {
    /// Mints an owner with an idle slot. `tasks` spawns the run keepalive;
    /// it must outlive every park of this slot.
    pub(crate) fn new(tasks: SessionTasks) -> Self {
        Self {
            slot: Arc::new(ContinuationSlot {
                state: Mutex::new(SlotState::Idle),
                tasks,
            }),
        }
    }

    /// The request handle: the only path by which requests reach the slot.
    pub(crate) fn downgrade(&self) -> Weak<ContinuationSlot> {
        Arc::downgrade(&self.slot)
    }

    /// Ends the owned conversation if live: the explicit owner-end path
    /// (turn terminal/close, relay stream finish or drop). Idempotent.
    /// Returns whether a live conversation was released.
    pub(crate) fn release_if_live(&self) -> bool {
        self.slot.release_if_live()
    }
}

/// Backstop, not a protocol path: an owner dropped without an explicit
/// release still ends its conversation instead of leaving it parked. Owner
/// ends still call [`ContinuationOwner::release_if_live`] for deterministic
/// timing; this only catches missed paths. Idempotent.
impl Drop for ContinuationOwner {
    fn drop(&mut self) {
        self.slot.release_if_live();
    }
}

/// One owner's slot: at most one live conversation, explicit release.
///
/// Reached only through a [`Weak`] in request inputs. All methods take
/// `&self`; interior mutability is a single [`Mutex`]. `Send + Sync` so the
/// handle travels in [`RequestDeps`](super::turn::RequestDeps) and
/// [`Caller`](crate::ext::Caller).
pub(crate) struct ContinuationSlot {
    state: Mutex<SlotState>,
    /// Spawns the run keepalive. Stored here (not passed per call) so a
    /// request handle alone can park without threading task owners.
    tasks: SessionTasks,
}

// Manual so the `Weak` handle stays `Debug` for its holders no matter what
// the state gains; lock-free and never poisons.
impl fmt::Debug for ContinuationSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContinuationSlot(..)")
    }
}

/// Why parking a tool-use stop failed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ParkError {
    /// The slot holds no live conversation (idle, opening, in flight, or
    /// released).
    #[error("continuation slot holds no live conversation")]
    NoConversation,
    /// The run is not awaiting host results; the step did not park.
    #[error("the run is not awaiting host results")]
    NotAwaiting,
    /// Two parked entries share one call id. Duplicates are rejected here
    /// so the respond cover can never wedge on them.
    #[error("duplicate parked call")]
    Duplicate(CallId),
    /// A re-parked session entry names the wrong tool for its call id.
    #[error("re-parked session call names the wrong tool")]
    WrongName {
        /// The parked call.
        call: CallId,
        /// The name in the parked full cover.
        expected: Box<str>,
        /// The name offered.
        found: Box<str>,
    },
    /// The parked cover names a different call set than the run awaits.
    #[error("parked cover does not match the awaiting calls")]
    Cover {
        /// Awaited ids with no parked entry.
        missing: Vec<CallId>,
        /// Parked ids the run does not await.
        extra: Vec<CallId>,
    },
    /// The slot is already parked on different calls; a second tool-use stop
    /// without an intervening respond is a protocol violation.
    #[error("the slot is already parked on different calls")]
    Changed,
}

/// Why stashing one private result failed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StashError {
    /// The slot holds no live conversation (idle, opening, in flight, or
    /// released).
    #[error("continuation slot holds no live conversation")]
    NoConversation,
    /// The run is not awaiting host results.
    #[error("the run is not awaiting host results")]
    NotAwaiting,
    /// The call id is not in the parked pending set; results are never
    /// buffered for a later step.
    #[error("stashed call is not pending")]
    UnknownCall(CallId),
    /// The name does not match the pending name for that id. Names pass back
    /// exactly as the step announced them.
    #[error("stashed name does not match the pending call")]
    WrongName {
        /// The pending call.
        call: CallId,
        /// The name the step announced.
        expected: Box<str>,
        /// The name offered.
        found: Box<str>,
    },
    /// A second stash for one pending id.
    #[error("duplicate stash for a pending call")]
    Duplicate(CallId),
}

/// Why the respond cover failed. A missing result is never filled from an
/// extra one: gaps stay gaps, and the run is left for the owner to release.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CoverError {
    /// No stashed or journaled result answers a pending call.
    #[error("no result answers a pending call")]
    Missing(CallId),
    /// Two results answer one pending call (within the journaled set, or one
    /// stashed and one journaled).
    #[error("two results answer one pending call")]
    DuplicateResult(CallId),
    /// A result names the wrong tool for its call id.
    #[error("a result names the wrong tool for its call")]
    WrongName {
        /// The pending call.
        call: CallId,
        /// The name the step announced.
        expected: Box<str>,
        /// The name offered.
        found: Box<str>,
    },
    /// A result answers no pending call.
    #[error("a result answers no pending call")]
    Extra(CallId),
}

enum SlotState {
    Idle,
    /// One handshake reserved and in flight. Taken under the initial lock
    /// before any wire traffic, so a concurrent open fails before sending.
    /// The owner end may move this to `Released` meanwhile.
    Opening,
    Live(Box<Conversation>),
    /// A resume owns the conversation; no guard is held across its await.
    /// The owner end may move this to `Released` meanwhile.
    InFlight,
    Released,
}

struct Conversation {
    run: ProviderRun,
    provider: Provider,
    route: ModelRoute,
    pending: Vec<PendingCall>,
    stash: HashMap<CallId, StashedResult>,
    staged: Option<EventStream>,
}

#[derive(Debug, Eq, PartialEq)]
struct PendingCall {
    id: CallId,
    name: Box<str>,
}

struct StashedResult {
    name: Box<str>,
    is_error: bool,
    parts: Vec<Part>,
}

/// Where one pending call's answer comes from. Parallel to `pending`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanItem {
    Stash,
    Session,
}

/// Reserves the slot for one handshake. Dropping it restores `Idle` when
/// the slot still holds the reservation, and leaves a concurrent owner-end
/// `Released` untouched. Disarm by setting `armed` to false once the slot
/// itself is handled (installed `Live` or observed terminal).
struct Opening<'slot> {
    slot: &'slot ContinuationSlot,
    armed: bool,
}

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut state = self.slot.lock_state();
            if matches!(*state, SlotState::Opening) {
                *state = SlotState::Idle;
            }
        }
    }
}

/// Owns a conversation taken out of the slot for one resume. Dropping it
/// restores the conversation when the slot still expects it, and drops it
/// otherwise, so every exit — success, error, or a dropped respond future —
/// leaves the slot terminal (live or released), never stranded mid-resume.
/// Disarm by taking `conversation` on the success path.
struct Restore<'slot> {
    slot: &'slot ContinuationSlot,
    conversation: Option<Box<Conversation>>,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if let Some(conversation) = self.conversation.take() {
            let mut state = self.slot.lock_state();
            if matches!(*state, SlotState::InFlight) {
                *state = SlotState::Live(conversation);
            }
            // Else released meanwhile: the conversation drops here and its
            // transport aborts. `lock_state` recovers from poisoning instead
            // of panicking, so dropping this guard never panics.
        }
    }
}

impl ContinuationSlot {
    fn lock_state(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records the exact pending cover of a step that ended in tool use.
    ///
    /// The caller observes `StopReason::ToolUse` on the step stream and
    /// passes the step's `ToolCallsDone` (id, name) pairs verbatim. Duplicate
    /// ids are rejected before anything is stored. Re-park with identical
    /// pairs is `Ok`; anything else after a park is [`ParkError::Changed`].
    /// Spawns nothing: the keepalive started at open persists across responds
    /// until the run's terminal phase.
    pub(crate) fn park(&self, pending: Vec<(CallId, Box<str>)>) -> Result<(), ParkError> {
        let mut state = self.lock_state();
        let SlotState::Live(conversation) = &mut *state else {
            return Err(ParkError::NoConversation);
        };
        let Some(awaiting) = conversation.run.awaiting() else {
            return Err(ParkError::NotAwaiting);
        };
        let checked = check_pending(pending, &awaiting)?;
        if conversation.pending.is_empty() {
            conversation.pending = checked;
            return Ok(());
        }
        if conversation.pending == checked {
            return Ok(());
        }
        Err(ParkError::Changed)
    }

    /// Re-parks the session-tool subset of an already-parked cover.
    ///
    /// Mixed steps park the full private-plus-session cover first; the
    /// dispatcher then re-announces the session calls it runs. When no cover
    /// is parked yet, the subset must be the whole step: it goes through the
    /// same strict check as [`park`] and is stored as the full cover.
    /// Otherwise each entry must match a parked entry exactly — id present,
    /// name equal, no duplicate ids — and the stored full cover is never
    /// replaced. Respond still answers the full cover.
    pub(crate) fn park_session_calls(
        &self,
        session: Vec<(CallId, Box<str>)>,
    ) -> Result<(), ParkError> {
        let mut state = self.lock_state();
        let SlotState::Live(conversation) = &mut *state else {
            return Err(ParkError::NoConversation);
        };
        let Some(awaiting) = conversation.run.awaiting() else {
            return Err(ParkError::NotAwaiting);
        };
        if conversation.pending.is_empty() {
            conversation.pending = check_pending(session, &awaiting)?;
            return Ok(());
        }
        check_session_subset(&session, &conversation.pending)
    }

    /// Stashes one private result for a parked call.
    ///
    /// Requires a live, awaiting slot, a pending id, the exact pending name,
    /// and no earlier stash for that id. The caller journals
    /// `PrivateEnd(Settled)` before stashing: durable first, memory second.
    /// The stash lives in this conversation only, so a result stashed under
    /// one owner can never answer another invocation.
    pub(crate) fn stash(
        &self,
        call: CallId,
        name: Box<str>,
        is_error: bool,
        parts: Vec<Part>,
    ) -> Result<(), StashError> {
        let mut state = self.lock_state();
        let SlotState::Live(conversation) = &mut *state else {
            return Err(StashError::NoConversation);
        };
        if conversation.run.awaiting().is_none() {
            return Err(StashError::NotAwaiting);
        }
        let expected = conversation
            .pending
            .iter()
            .find(|item| item.id == call)
            .map(|item| item.name.clone());
        let Some(expected) = expected else {
            return Err(StashError::UnknownCall(call));
        };
        if expected != name {
            return Err(StashError::WrongName {
                call,
                expected,
                found: name,
            });
        }
        if conversation.stash.contains_key(&call) {
            return Err(StashError::Duplicate(call));
        }
        conversation.stash.insert(
            call,
            StashedResult {
                name,
                is_error,
                parts,
            },
        );
        Ok(())
    }

    /// Resumes the parked run with the exact pending cover and stages the
    /// next step stream for the following round's [`open_step`].
    ///
    /// `session_results` is the round's journaled session-tool set: the caller
    /// appends every session `ToolResult` before calling, so the only
    /// sources are the durable journal and the private stash — never the
    /// dispatch success subset. Assembly moves payloads (no part is cloned);
    /// any [`CoverError`] becomes `InvalidRequest`.
    ///
    /// The session vec is spent into the resume call; after a pre-admission
    /// error or drop the caller re-supplies it from the journal. The stash
    /// is spent only once the admission permit is secured, so a run still
    /// awaiting after an error keeps its stash: retry re-supplies the
    /// session vec without re-stashing and without re-running any effect.
    ///
    /// On success the provider acquires the step permit before the respond
    /// ([`Provider::resume_run`]) and the stream is staged, not returned.
    /// A concurrent owner end releases meanwhile, and the returning resume
    /// drops the conversation instead of reinstalling it. This method never
    /// releases by itself; only [`ContinuationSlot::release_if_live`] and
    /// drop do.
    pub(crate) async fn respond(
        &self,
        session_results: Vec<HostToolResult>,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        // Take the conversation out of the slot so no guard is held across
        // the resume await. Only one resume can own it. The restore guard
        // makes every exit terminal: success disarms it, and any error or
        // future drop restores or releases through it. State reads before
        // cancellation, so a dead slot reports `InvalidRequest` even under
        // a fired token instead of masking a bug as transient.
        // One mutable guard validates and takes atomically: no check, drop,
        // and relock window for a concurrent release or second resume to
        // slip through.
        let mut taken = {
            let mut state = self.lock_state();
            match &*state {
                SlotState::Live(conversation) if conversation.run.awaiting().is_some() => {}
                SlotState::Live(_) => {
                    return Err(invalid("the run is not awaiting host results"));
                }
                _ => return Err(invalid("the continuation is not live")),
            }
            let SlotState::Live(conversation) = std::mem::replace(&mut *state, SlotState::InFlight)
            else {
                return Err(invalid("the continuation changed during respond"));
            };
            Restore {
                slot: self,
                conversation: Some(conversation),
            }
        };
        if cancel.is_cancelled() {
            // The guard drops here and restores the slot pristine: stash and
            // pending untouched, the run still awaiting.
            return Err(cancelled());
        }
        let outcome = {
            let Some(conversation) = taken.conversation.as_mut() else {
                return Err(invalid("respond already completed"));
            };
            resume_taken(conversation, session_results, cancel).await
        };
        match outcome {
            Ok(stream) => {
                let Some(mut conversation) = taken.conversation.take() else {
                    return Err(invalid("respond already completed"));
                };
                // Success only: the prior step's cover is done. Clear it
                // before staging so the next step parks its own call set;
                // a repeated set would otherwise fail as `Changed` and a
                // new set could never park. On failure below the cover is
                // retained for re-supply retry.
                conversation.pending.clear();
                conversation.staged = Some(stream);
                let mut state = self.lock_state();
                if matches!(*state, SlotState::InFlight) {
                    *state = SlotState::Live(conversation);
                    return Ok(());
                }
                drop(state);
                drop(conversation);
                Err(invalid("the continuation was released during resume"))
            }
            Err(error) => {
                // The batch is spent. Drop the guard: it restores the
                // conversation when the owner has not ended (the run reports
                // its own state afterwards), else drops it so the transport
                // aborts.
                drop(taken);
                Err(error)
            }
        }
    }

    /// Whether the slot holds a live conversation awaiting host results.
    pub(crate) fn is_awaiting(&self) -> bool {
        let state = self.lock_state();
        let SlotState::Live(conversation) = &*state else {
            return false;
        };
        conversation.run.awaiting().is_some()
    }

    /// Ends the owner's conversation if live. Idempotent: safe at every
    /// owner end (turn terminal/close, relay stream finish or drop). An
    /// in-flight resume is released too: it drops instead of reinstalling.
    /// Returns whether a live conversation was released.
    pub(crate) fn release_if_live(&self) -> bool {
        let mut state = self.lock_state();
        match std::mem::replace(&mut *state, SlotState::Released) {
            // The conversation (and its run) drops here; the transport
            // aborts through `ProviderRun`'s drop. Releasing a reservation
            // or an in-flight resume drops it on completion instead.
            SlotState::Live(_) | SlotState::InFlight | SlotState::Opening => true,
            SlotState::Idle | SlotState::Released => false,
        }
    }
    pub(crate) fn abandon(&self) -> bool {
        let mut state = self.lock_state();
        let SlotState::Live(conversation) = &*state else {
            return false;
        };
        if !conversation.stash.is_empty() {
            return false;
        }
        *state = SlotState::Idle;
        true
    }
    pub(crate) fn parked_route(&self) -> Option<ModelRoute> {
        let state = self.lock_state();
        let SlotState::Live(conversation) = &*state else {
            return None;
        };
        conversation.run.awaiting()?;
        Some(conversation.route.clone())
    }
    pub(crate) fn abandon_live(&self) -> bool {
        let mut state = self.lock_state();
        let SlotState::Live(_) = &*state else {
            return false;
        };
        *state = SlotState::Idle;
        true
    }
    pub(crate) fn abandon_if_route_changed(&self, route: &ModelRoute, provider: &str) -> bool {
        let changed = self
            .parked_route()
            .is_some_and(|parked| !route_matches(&parked, route, provider));
        changed && self.abandon_live()
    }
}

fn route_matches(parked: &ModelRoute, current: &ModelRoute, provider: &str) -> bool {
    let ModelRoute::Api {
        family,
        model,
        provider: parked_provider,
    } = parked
    else {
        return false;
    };
    let ModelRoute::Api {
        family: current_family,
        model: current_model,
        ..
    } = current
    else {
        return false;
    };
    family == current_family
        && model == current_model
        && parked_provider.as_deref() == Some(provider)
}

/// Runs the taken conversation through cover validation and the provider
/// resume. Holds no slot guard (the caller took the conversation out); every
/// `?` below returns to the caller's single reinstall tail.
///
/// Validation runs before the call and borrows only. The commit moves into a
/// closure the provider invokes synchronously after the admission permit is
/// secured — never before. A future dropped while the permit is pending
/// never runs it: the stash stays intact, the session vec dies with the
/// future for journal re-supply, and the run stays awaiting. Retry rule,
/// airtight by construction (the closure runs exactly before `run.respond`,
/// so a still-awaiting run never spent its stash): retry if and only if the
/// run is still awaiting, re-supplying the session vec without re-stashing.
async fn resume_taken(
    conversation: &mut Conversation,
    session_results: Vec<HostToolResult>,
    cancel: &CancellationToken,
) -> Result<EventStream, ProviderError> {
    let plan = plan_cover(&conversation.pending, &conversation.stash, &session_results)
        .map_err(|error| invalid(&error.to_string()))?;
    let pending = &conversation.pending;
    let stash = &mut conversation.stash;
    // Commit-on-admission: moves only, no part is cloned. Each removal was
    // validated by the plan, so absence is a desync, not caller input.
    let commit = move || -> Result<Vec<HostToolResult>, ProviderError> {
        let mut table: HashMap<CallId, HostToolResult> = session_results
            .into_iter()
            .map(|result| (result.call.clone(), result))
            .collect();
        let mut ordered = Vec::with_capacity(plan.len());
        for (item, source) in pending.iter().zip(plan.iter()) {
            let next = match source {
                PlanItem::Stash => stash.remove(&item.id).map(|stashed| HostToolResult {
                    call: item.id.clone(),
                    name: stashed.name,
                    is_error: stashed.is_error,
                    parts: stashed.parts,
                }),
                PlanItem::Session => table.remove(&item.id),
            };
            let Some(next) = next else {
                return Err(invalid("cover desync"));
            };
            ordered.push(next);
        }
        stash.clear();
        Ok(ordered)
    };
    conversation
        .provider
        .resume_run(&mut conversation.run, commit, cancel)
        .await
}

/// Validates one parked cover against the awaited ids: no duplicate ids,
/// and exactly the id set the run awaits. Pure, so the duplicate and
/// set-mismatch shapes are unit-tested without a run.
fn check_pending(
    pending: Vec<(CallId, Box<str>)>,
    awaiting: &[CallId],
) -> Result<Vec<PendingCall>, ParkError> {
    let mut seen: HashSet<&CallId> = HashSet::with_capacity(pending.len());
    for (id, _) in &pending {
        if !seen.insert(id) {
            return Err(ParkError::Duplicate(id.clone()));
        }
    }
    let awaited: HashSet<&CallId> = awaiting.iter().collect();
    let missing: Vec<CallId> = awaiting
        .iter()
        .filter(|id| !seen.contains(id))
        .cloned()
        .collect();
    let extra: Vec<CallId> = pending
        .iter()
        .map(|(id, _)| id)
        .filter(|id| !awaited.contains(id))
        .cloned()
        .collect();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(ParkError::Cover { missing, extra });
    }
    Ok(pending
        .into_iter()
        .map(|(id, name)| PendingCall { id, name })
        .collect())
}

/// Validates a re-parked session subset against the parked full cover: every
/// entry matches a parked entry exactly, with no duplicate ids. Pure, so all
/// shapes are unit-tested without a run. The full cover is never modified.
fn check_session_subset(
    session: &[(CallId, Box<str>)],
    parked: &[PendingCall],
) -> Result<(), ParkError> {
    let mut seen: HashSet<&CallId> = HashSet::with_capacity(session.len());
    for (id, name) in session {
        if !seen.insert(id) {
            return Err(ParkError::Duplicate(id.clone()));
        }
        let Some(covered) = parked.iter().find(|item| item.id == *id) else {
            return Err(ParkError::Cover {
                missing: Vec::new(),
                extra: vec![id.clone()],
            });
        };
        if covered.name != *name {
            return Err(ParkError::WrongName {
                call: id.clone(),
                expected: covered.name.clone(),
                found: name.clone(),
            });
        }
    }
    Ok(())
}

/// Validates the respond cover without moving anything: every pending call
/// exactly once, under its own name, from stash or the journaled set, and
/// nothing else. The returned plan is parallel to `pending`. Errors read in
/// pending order first, then extras.
fn plan_cover(
    pending: &[PendingCall],
    stash: &HashMap<CallId, StashedResult>,
    session: &[HostToolResult],
) -> Result<Vec<PlanItem>, CoverError> {
    let mut table: HashMap<&CallId, &HostToolResult> = HashMap::with_capacity(session.len());
    for result in session {
        if table.insert(&result.call, result).is_some() {
            return Err(CoverError::DuplicateResult(result.call.clone()));
        }
    }
    let mut plan = Vec::with_capacity(pending.len());
    for item in pending {
        match (stash.get(&item.id), table.get(&item.id)) {
            (Some(_), Some(_)) => return Err(CoverError::DuplicateResult(item.id.clone())),
            (None, None) => return Err(CoverError::Missing(item.id.clone())),
            (Some(stashed), None) => {
                if stashed.name != item.name {
                    return Err(CoverError::WrongName {
                        call: item.id.clone(),
                        expected: item.name.clone(),
                        found: stashed.name.clone(),
                    });
                }
                plan.push(PlanItem::Stash);
            }
            (None, Some(found)) => {
                if found.name != item.name {
                    return Err(CoverError::WrongName {
                        call: item.id.clone(),
                        expected: item.name.clone(),
                        found: found.name.clone(),
                    });
                }
                plan.push(PlanItem::Session);
            }
        }
    }
    let parked: HashSet<&CallId> = pending.iter().map(|item| &item.id).collect();
    for result in session {
        if !parked.contains(&result.call) {
            return Err(CoverError::Extra(result.call.clone()));
        }
    }
    // Unreachable: `stash` only accepts pending ids. Fail closed anyway.
    for id in stash.keys() {
        if !parked.contains(id) {
            return Err(CoverError::Extra(id.clone()));
        }
    }
    Ok(plan)
}

/// Opens the Cursor step for one request: the single entry behind the
/// turn's Cursor branch and the relay's Cursor path.
///
/// - No slot: today's one-shot `open` (every slot-less caller keeps working;
///   no new refusal).
/// - Dead slot (owner ended): `InvalidRequest`. A silent one-shot downgrade
///   would fork a second server run beside the parked conversation.
/// - Idle: reserves `Opening` under the initial lock, then
///   [`Provider::open_run`]; installs the conversation, spawns the run
///   keepalive once (it persists across responds until the terminal phase),
///   and returns the first step stream. A concurrent open fails before any
///   wire traffic. The handshake holds no guard; an owner end during it
///   drops the fresh run instead of installing it, and a failed handshake
///   restores `Idle` through the reservation guard.
/// - Live with a staged stream: takes and returns it.
/// - Live without one whose run still streams or awaits: `InvalidRequest`
///   (awaiting means respond first).
/// - Live without one whose run already ended (`Closed`, `Released`, or
///   `Lost`): drops the dead conversation and reserves `Opening` for a
///   fresh [`Provider::open_run`], so an interrupted first step retries.
/// - A resume or handshake in flight: `InvalidRequest`.
/// - Released: `InvalidRequest`.
///
/// The provider handle moves in and is stored for the later resumes, so the
/// loop never resolves or carries one.
pub(crate) async fn open_step(
    deps: &RequestDeps,
    provider: Provider,
    req: &ModelRequest,
    cancel: &CancellationToken,
) -> Result<EventStream, ProviderError> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let Some(slot) = deps.continuation.as_ref().and_then(Weak::upgrade) else {
        if deps.continuation.is_some() {
            return Err(invalid("the continuation owner is gone"));
        }
        return provider
            .open(deps.session, req, &[], Arc::new(|_: String| {}), cancel)
            .await;
    };
    {
        let mut state = slot.lock_state();
        if let SlotState::Live(conversation) = &mut *state
            && let Some(staged) = conversation.staged.take()
        {
            return Ok(staged);
        }
        let reopen = matches!(
            &*state,
            SlotState::Live(conversation)
                if matches!(
                    conversation.run.state(),
                    RunState::Closed | RunState::Released | RunState::Lost(_)
                )
        );
        if reopen {
            *state = SlotState::Opening;
        } else {
            match &mut *state {
                SlotState::Released => return Err(invalid("the continuation is released")),
                SlotState::Live(_) => return Err(invalid("the continuation has no ready step")),
                SlotState::InFlight | SlotState::Opening => {
                    return Err(invalid("the continuation has no ready step"));
                }
                SlotState::Idle => {
                    // Reserve before any wire traffic: a concurrent open fails
                    // above instead of sending a second handshake.
                    *state = SlotState::Opening;
                }
            }
        }
    }
    let mut opening = Opening {
        slot: slot.as_ref(),
        armed: true,
    };
    // The handshake runs without the slot guard held.
    let (run, stream) = provider
        .open_run(deps.session, req, &[], Arc::new(|_: String| {}), cancel)
        .await?;
    {
        let mut state = slot.lock_state();
        if !matches!(*state, SlotState::Opening) {
            // The owner ended during the handshake: drop the fresh run here
            // so its transport aborts. The reservation guard finds no
            // reservation and leaves the terminal state alone.
            opening.armed = false;
            drop(state);
            drop(run);
            return Err(invalid("the continuation ended during open"));
        }
        let keeper = run.keepalive();
        *state = SlotState::Live(Box::new(Conversation {
            run,
            provider,
            route: req.model.clone(),
            pending: Vec::new(),
            stash: HashMap::new(),
            staged: None,
        }));
        // Installed: disarm the reservation. Neither the slot guard nor the
        // run is held across the spawn; the keeper owns its run core and
        // resolves at the terminal phase.
        opening.armed = false;
        slot.tasks.spawn(async move {
            keeper.await;
        });
    }
    Ok(stream)
}

/// A terminal request failure with no next-action text.
fn invalid(message: &str) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

/// The typed failure of a request cancelled before it was sent.
fn cancelled() -> ProviderError {
    ProviderError::Synthetic(InferFailure::Cancelled)
}
