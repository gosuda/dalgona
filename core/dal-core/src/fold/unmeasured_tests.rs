//! A response that reports zero input tokens has not measured the context.
//!
//! The usage grammar says counters are zero when the server reported none, so
//! a run whose steps report an all-zero `Usage` has no measurement at all.
//! Compaction, the displayed context size and the compaction cut point must
//! then follow the shared byte estimate, live and after replay, while the
//! journaled `Usage` stays exactly as the provider reported it. Nonzero
//! reports keep driving all three directly.
use super::*;

/// Tokens retained by the cut-point probes; small enough that one heavy
/// assistant reply decides the boundary.
const KEEP_TOKENS: u64 = 10_000;

/// Tool result size per round: four rounds cross the 85k-token line of a
/// 100k window on the estimate (about 91k tokens), three stay under it
/// (about 69k).
const RESULT_BYTES: usize = 80_000;

fn stamp() -> jiff::Timestamp {
    jiff::Timestamp::UNIX_EPOCH
}

fn route() -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: "test-model".into(),
    }
}

fn unreported() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

fn reported(input_tokens: u64) -> Usage {
    Usage {
        input_tokens,
        output_tokens: 1,
        ..unreported()
    }
}

fn emitted(out: &[Effect]) -> impl Iterator<Item = &Emit> {
    out.iter().filter_map(|effect| match effect {
        Effect::Emit(emit) => Some(emit),
        _ => None,
    })
}

/// The usage and context size a completed response publishes.
fn published_usage(out: &[Effect]) -> (Usage, u64) {
    emitted(out)
        .flat_map(|emit| emit.updates.iter())
        .find_map(|update| match update {
            UpdateKind::Usage(view) => Some((view.usage, view.context_tokens)),
            _ => None,
        })
        .expect("a completed response publishes its usage view")
}

fn journaled_usage(out: &[Effect]) -> Usage {
    emitted(out)
        .flat_map(|emit| emit.records.iter())
        .find_map(|record| match record {
            Record::Assistant(Entry {
                kind: EntryKind::Assistant { usage, .. },
                ..
            }) => Some(*usage),
            _ => None,
        })
        .expect("a completed response journals its assistant entry")
}

fn compacted(out: &[Effect]) -> bool {
    out.iter()
        .any(|effect| matches!(effect, Effect::Compact { .. }))
}

fn inferred(out: &[Effect]) -> bool {
    out.iter().any(|effect| matches!(effect, Effect::Infer(_)))
}

/// A real session driven through its public event stream. Every event also
/// extends the journal, and the journal's replay must agree with the live
/// session about the context size and the compaction cut after each event.
struct Run {
    live: Session,
    journal: Vec<Record>,
    users: Vec<EntryId>,
}

impl Run {
    fn new() -> Self {
        let mut run = Self {
            live: Session::replay([], stamp()).unwrap().0,
            journal: Vec::new(),
            users: Vec::new(),
        };
        run.send(Event::Limits {
            window: 100_000,
            max_steps: 0,
            compact: CompactLimits {
                threshold: 0.85,
                min_tokens: 1,
                keep_tokens: 20_000,
                enabled: true,
                compactor_available: true,
            },
        });
        run
    }

    fn send(&mut self, event: Event) -> Vec<Effect> {
        let mut out = Vec::new();
        self.live.step(event, stamp(), &mut out).unwrap();
        for record in emitted(&out).flat_map(|emit| emit.records.iter()) {
            if let Record::User(entry) = record {
                self.users.push(entry.id);
            }
            self.journal.push(record.clone());
        }
        self.assert_replay_agrees();
        out
    }

    fn assert_replay_agrees(&self) {
        if self.journal.is_empty() {
            return;
        }
        let replayed = Session::replay_declared(self.journal.iter().cloned()).unwrap();
        assert_eq!(
            replayed.measured_context_tokens(),
            self.live.measured_context_tokens(),
            "replay and live disagree on the measured context"
        );
        assert_eq!(
            replayed.tokens_since_last_compaction(),
            self.live.tokens_since_last_compaction(),
            "replay and live disagree on the context size"
        );
        assert_eq!(
            replayed.cut_point(KEEP_TOKENS),
            self.live.cut_point(KEEP_TOKENS),
            "replay and live disagree on the compaction cut"
        );
    }

    fn begin(&mut self, text: &str) -> TurnId {
        self.send(Event::Command {
            cmd: Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: text.into() }],
            },
            by: ClientId::new("test"),
        });
        let turn = match self.live.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("prompt did not open a turn: {phase:?}"),
        };
        self.send(Event::Guard {
            turn,
            call: None,
            extension: None,
            outcome: HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None))
                .unwrap(),
        });
        turn
    }

    fn stream(&mut self, turn: TurnId, events: Vec<StreamEvent>) -> Vec<Effect> {
        self.send(Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Ok(Inference { events }),
            partial: None,
        })
    }

    /// One model step that ends by asking for a single `read_file` call.
    fn respond(&mut self, turn: TurnId, call: &str, usage: Usage) -> Vec<Effect> {
        self.stream(
            turn,
            vec![
                StreamEvent::Usage(usage),
                StreamEvent::ToolCall {
                    call: CallId::new(call),
                    name: "read_file".into(),
                    args: RawJson::parse("{}").unwrap(),
                },
                StreamEvent::Stop(Stop::EndTurn),
            ],
        )
    }

    /// Runs the call to completion with a result of `bytes` bytes and returns
    /// the effects of the settlement, where the next round is decided.
    fn settle(&mut self, turn: TurnId, call: &str, bytes: usize) -> Vec<Effect> {
        self.send(Event::Resolved {
            turn,
            calls: vec![ResolvedCall {
                call: CallId::new(call),
                name: Name::parse("read_file").unwrap(),
                promoted: false,
                result: Ok(ToolClass::Read),
            }],
            answerer_attached: false,
        });
        self.send(Event::CallStarted {
            turn,
            call: CallId::new(call),
        });
        self.send(Event::Settled {
            turn,
            call: CallId::new(call),
            outcome: SettledOutcome::Ok {
                text: "x".repeat(bytes).into(),
                data: None,
            },
            elapsed_ms: None,
        })
    }

    /// A whole prompt turn whose single reply is `reply` text.
    fn answered_turn(&mut self, prompt: &str, reply: &str, usage: Usage) {
        let turn = self.begin(prompt);
        self.stream(
            turn,
            vec![
                StreamEvent::Delta {
                    channel: StreamChannel::Text,
                    text: reply.into(),
                },
                StreamEvent::Usage(usage),
                StreamEvent::Stop(Stop::EndTurn),
            ],
        );
        self.send(Event::Boundary { turn });
    }

    /// The session a restart would rebuild from the journal written so far.
    fn restarted(&self) -> Session {
        Session::replay(self.journal.iter().cloned(), stamp())
            .unwrap()
            .0
    }
}

#[test]
fn unreported_midrun_usage_compacts_when_tool_results_cross_the_threshold() {
    let mut run = Run::new();
    let turn = run.begin("question");
    let mut shown = 0;
    for round in 1..=4_usize {
        let call = format!("call-{round}");
        let out = run.respond(turn, &call, unreported());

        assert_eq!(
            journaled_usage(&out),
            unreported(),
            "round {round}: the journal keeps the provider's report"
        );
        let (published, context) = published_usage(&out);
        assert_eq!(
            published,
            unreported(),
            "round {round}: the published usage is the provider's report"
        );
        assert!(
            context > shown,
            "round {round}: the displayed context must grow with the tool results, not stay at zero ({context} after {shown})"
        );
        let earlier_results = u64::try_from(RESULT_BYTES * (round - 1)).unwrap();
        assert!(
            context >= crate::tokens::estimate_tokens(earlier_results),
            "round {round}: the displayed context must include every earlier tool result"
        );
        assert_eq!(context, run.live.tokens_since_last_compaction());
        shown = context;

        let out = run.settle(turn, &call, RESULT_BYTES);
        if round < 4 {
            assert!(
                !compacted(&out) && inferred(&out),
                "round {round}: the estimate is still under the threshold"
            );
        } else {
            assert!(
                compacted(&out),
                "four rounds of unreported usage whose results cross the threshold must compact"
            );
            assert!(matches!(
                run.live.phase(),
                Phase::Running {
                    stage: TurnStage::Compacting {
                        reason: CompactionReason::Threshold,
                        automatic: true,
                    },
                    ..
                }
            ));
        }
    }
    assert_eq!(
        run.restarted().tokens_since_last_compaction(),
        run.live.tokens_since_last_compaction(),
        "a restart rebuilds the same context size"
    );
}

#[test]
fn reported_usage_keeps_driving_the_context_view_and_compaction() {
    let mut run = Run::new();
    let turn = run.begin("question");

    let out = run.respond(turn, "call-1", reported(5_000));
    let (published, context) = published_usage(&out);
    assert_eq!(published, reported(5_000));
    assert_eq!(
        context, 5_000,
        "a reported input count is the displayed context, not the estimate"
    );
    assert_eq!(run.live.measured_context_tokens(), Some(5_000));
    let out = run.settle(turn, "call-1", 10);
    assert!(
        !compacted(&out) && inferred(&out),
        "5k reported tokens are far below the threshold"
    );

    let out = run.respond(turn, "call-2", reported(90_000));
    assert_eq!(published_usage(&out).1, 90_000);
    let out = run.settle(turn, "call-2", 10);
    assert!(
        compacted(&out),
        "90k reported tokens cross the threshold whatever the tiny projection says"
    );
}

#[test]
fn unreported_assistant_replies_weigh_their_bytes_at_the_compaction_cut() {
    let mut run = Run::new();
    run.answered_turn("q1", "r1", unreported());
    run.answered_turn("q2", &"x".repeat(120_000), unreported());
    run.answered_turn("q3", "r3", unreported());

    assert_eq!(
        run.live.cut_point(KEEP_TOKENS),
        Some(run.users[1]),
        "the heavy unreported reply fills the retained budget, so the cut keeps its prompt"
    );
    let restarted = run.restarted();
    assert_eq!(
        restarted.cut_point(KEEP_TOKENS),
        run.live.cut_point(KEEP_TOKENS)
    );
    assert_eq!(
        restarted.tokens_since_last_compaction(),
        run.live.tokens_since_last_compaction()
    );
}

#[test]
fn light_unreported_replies_do_not_fill_the_retained_budget() {
    let mut run = Run::new();
    run.answered_turn("q1", "r1", unreported());
    run.answered_turn("q2", "r2", unreported());
    run.answered_turn("q3", "r3", unreported());

    assert_eq!(
        run.live.cut_point(KEEP_TOKENS),
        Some(run.users[0]),
        "with nothing heavy the cut falls back to the oldest prompt"
    );
}

#[test]
fn reported_assistant_usage_still_weighs_the_compaction_cut() {
    let mut run = Run::new();
    run.answered_turn("q1", "r1", unreported());
    run.answered_turn("q2", "r2", reported(30_000));
    run.answered_turn("q3", "r3", unreported());

    assert_eq!(
        run.live.cut_point(KEEP_TOKENS),
        Some(run.users[1]),
        "a reported 30k input count fills the retained budget however short the reply"
    );
    assert_eq!(
        run.restarted().cut_point(KEEP_TOKENS),
        run.live.cut_point(KEEP_TOKENS)
    );
}
