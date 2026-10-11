//! Harness turn pump: session updates to client events until the turn ends.

use super::super::decode::RouterFail;
use super::super::sink::EventSink;
use super::{HarnessEvent, HarnessShared, HarnessStop, HarnessTurn, UsageSum};
use dal_agent::Agent;
use dal_core::{CancelScope, Command, EntryId, SessionId, Stop};

/// The failure text when the harness subscription fell behind the session.
const SLOW_TEXT: &str = "dalgon serve dropped updates because the client read too slowly";

/// Drives one harness turn to its terminal stop, answering defaults.
///
/// A gone client or a stopping listener cancels the turn once; the turn
/// still reports its terminal stop.
pub(super) async fn drive_turn(
    shared: &HarnessShared,
    agent: &Agent,
    (session, mut subscription): (SessionId, dal_agent::Subscription),
    turn: dal_core::TurnId,
    pre_leaf: Option<EntryId>,
    sink: &mut impl EventSink,
) -> Result<HarnessTurn, RouterFail> {
    let mut pump = TurnPump::new(session, turn);
    let ids = super::super::stream::TurnIds::harness(session, turn);
    let mut live = sink.emit(HarnessEvent::Started(ids)).await;
    let mut cancel_sent = false;
    loop {
        if !live && !cancel_sent {
            cancel_sent = true;
            cancel_turn(agent, turn).await;
        }
        let delivery = tokio::select! {
            biased;
            () = shared.shutdown.cancelled(), if !cancel_sent => {
                cancel_sent = true;
                cancel_turn(agent, turn).await;
                continue;
            }
            delivery = subscription.next() => delivery,
        };
        let stop = match delivery {
            None => {
                return Err(RouterFail::bad(
                    "internal",
                    "the session closed during the turn".to_owned(),
                ));
            }
            Some(dal_agent::Delivery::Resync { .. }) => {
                if !cancel_sent {
                    cancel_turn(agent, turn).await;
                }
                HarnessStop::Failed(SLOW_TEXT.to_owned())
            }
            Some(dal_agent::Delivery::Update(update)) => {
                match pump.apply(agent, &update.kind).await {
                    PumpStep::Continue => continue,
                    PumpStep::Event(event) => {
                        live = sink.emit(event).await && live;
                        continue;
                    }
                    PumpStep::Ended(stop) => stop,
                }
            }
        };
        sink.emit(HarnessEvent::Stop(stop.clone())).await;
        if matches!(stop, HarnessStop::Failed(_))
            && let Some(leaf) = pre_leaf
        {
            let _ = agent.submit(Command::MoveLeaf(leaf)).await;
        }
        return Ok(pump.finish(stop));
    }
}

/// Cancels one running turn; a turn that already ended ignores it.
pub(super) async fn cancel_turn(agent: &Agent, turn: dal_core::TurnId) {
    let cancel = Command::Cancel {
        scope: CancelScope::Turn(turn),
    };
    if let Err(error) = agent.submit(cancel).await {
        tracing::debug!(%error, "router turn cancel was not accepted");
    }
}

/// What one session update means for the harness turn.
enum PumpStep {
    /// Nothing for the client.
    Continue,
    /// One event for the client.
    Event(HarnessEvent),
    /// The prompt turn ended.
    Ended(HarnessStop),
}

/// Accumulates one harness turn from its session updates.
pub(super) struct TurnPump {
    session: SessionId,
    turn: dal_core::TurnId,
    text: String,
    reasoning: String,
    usage: UsageSum,
    failure: Option<String>,
}

impl TurnPump {
    fn new(session: SessionId, turn: dal_core::TurnId) -> Self {
        Self {
            session,
            turn,
            text: String::new(),
            reasoning: String::new(),
            usage: UsageSum::default(),
            failure: None,
        }
    }

    async fn apply(&mut self, agent: &Agent, kind: &dal_core::UpdateKind) -> PumpStep {
        match kind {
            dal_core::UpdateKind::Delta {
                turn,
                channel,
                text,
            } if *turn == self.turn => self.delta(channel, text),
            dal_core::UpdateKind::Usage(view) => {
                add_usage(&mut self.usage, view);
                PumpStep::Event(HarnessEvent::Usage(self.usage.clone()))
            }
            dal_core::UpdateKind::Notice(notice)
                if notice.turn == Some(self.turn) && notice.kind.as_ref() == "error" =>
            {
                self.failure = Some(notice.text.to_string());
                PumpStep::Continue
            }
            dal_core::UpdateKind::RequestOpened(request) => {
                if let Err(error) = agent.answer(request.id, request.default.clone()).await {
                    tracing::debug!(%error, "router default answer was not accepted");
                }
                PumpStep::Continue
            }
            dal_core::UpdateKind::TurnEnded { turn, stop } if *turn == self.turn => {
                PumpStep::Ended(map_stop(*stop, self.failure.take()))
            }
            _ => PumpStep::Continue,
        }
    }

    fn delta(&mut self, channel: &dal_core::StreamChannel, delta: &str) -> PumpStep {
        match channel {
            dal_core::StreamChannel::Text => {
                self.text.push_str(delta);
                PumpStep::Event(HarnessEvent::Text(delta.to_owned()))
            }
            dal_core::StreamChannel::Thinking => {
                self.reasoning.push_str(delta);
                PumpStep::Event(HarnessEvent::Reasoning(delta.to_owned()))
            }
            dal_core::StreamChannel::ToolArgs { .. } => PumpStep::Continue,
        }
    }

    fn finish(self, stop: HarnessStop) -> HarnessTurn {
        HarnessTurn {
            text: self.text,
            reasoning: self.reasoning,
            usage: self.usage,
            stop,
            turn: self.turn,
            session: self.session,
        }
    }
}

/// Maps a core turn stop to its router stop; failures carry the turn's error notice.
pub(super) fn map_stop(stop: Stop, failure: Option<String>) -> HarnessStop {
    match stop {
        Stop::EndTurn => HarnessStop::EndTurn,
        Stop::Length => HarnessStop::MaxTokens,
        Stop::MaxSteps => HarnessStop::MaxTurnRequests,
        Stop::Filter => HarnessStop::Refusal,
        Stop::Cancelled => HarnessStop::Cancelled,
        Stop::Failed => {
            HarnessStop::Failed(failure.unwrap_or_else(|| "the turn failed".to_owned()))
        }
    }
}

/// Adds one usage view into the running sum.
pub(super) fn add_usage(sum: &mut UsageSum, view: &dal_core::UsageView) {
    sum.input += view.usage.input_tokens;
    sum.output += view.usage.output_tokens;
    sum.cache_read += view.usage.cached_input_tokens;
    sum.cache_write += view.usage.cache_write_tokens;
    sum.context_tokens = view.context_tokens;
    sum.context_window = view.context_window;
    if sum.cost.is_none() {
        sum.cost = view.usage.cost_usd;
    }
}
