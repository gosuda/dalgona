//! Integration suite for the session judge: gate resolution, typed calls,
//! budgets, admission order, ledgering, and notices over scripted services.

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::tool::{RawValue, ToolOutcome};
use dal_agent::ext::{Caller, HookCx, Services};
use dal_core::ModelRoute;
use dal_core::ext::{McpRequest, McpResponse};
use dal_core::{
    AgentsOp, AgentsReply, Answer, ContextItem, EntryId, FetchRequest, FetchResponse, Inference,
    JobsOp, JobsReply, ModelRequest, Notice, Part, Question, RunOutput, RunRequest, SessionId,
    SidecarOp, StateError, StateOp, StateRecord, StreamChannel, StreamEvent, TurnId, TurnOp,
    TurnOpReply, Usage,
};
use proptest::prelude::*;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};
use tokio::time::sleep;

use super::{
    Gate, GateSetting, Judge, JudgeConfig, JudgeError, JudgeOpen, JudgeQuestion,
    REASON_NO_CREDENTIALS, SHARED_MAX, STREAK_NOTICE_PREFIX, Verdict,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Debug)]
struct ScriptReply {
    body: String,
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Clone, Debug)]
struct ScriptStep {
    reply: Result<ScriptReply, ServiceError>,
    delay: Duration,
}

#[derive(Clone, Debug)]
enum ProbeScript {
    Ready { model_id: String },
    Failing { message: String },
}

struct FakeState {
    probes: VecDeque<ProbeScript>,
    steps: VecDeque<ScriptStep>,
    completions: Vec<String>,
    max_overlap: usize,
    rows: Vec<(Box<str>, String)>,
    notices: Vec<String>,
    next_entry: u64,
}

struct FakeServices {
    state: Mutex<FakeState>,
    overlap: AtomicUsize,
}

impl FakeServices {
    fn new(probes: Vec<ProbeScript>, steps: Vec<ScriptStep>) -> Self {
        Self {
            state: Mutex::new(FakeState {
                probes: probes.into(),
                steps: steps.into(),
                completions: Vec::new(),
                max_overlap: 0,
                rows: Vec::new(),
                notices: Vec::new(),
                next_entry: 1,
            }),
            overlap: AtomicUsize::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state
            .lock()
            .expect("fake services lock is not poisoned")
    }

    fn completions(&self) -> usize {
        self.lock().completions.len()
    }

    fn rows(&self) -> Vec<(String, String)> {
        self.lock()
            .rows
            .iter()
            .map(|(kind, body)| (kind.to_string(), body.clone()))
            .collect()
    }

    fn row_statuses(&self) -> Vec<String> {
        self.rows()
            .iter()
            .map(|(_, body)| {
                sonic_rs::from_str::<sonic_rs::Value>(body)
                    .expect("ledger row parses")
                    .get("status")
                    .and_then(JsonValueTrait::as_str)
                    .expect("ledger row has a status")
                    .to_owned()
            })
            .collect()
    }

    fn notices(&self) -> Vec<String> {
        self.lock().notices.clone()
    }

    fn max_overlap(&self) -> usize {
        self.lock().max_overlap
    }

    fn scripted(body: &str) -> ScriptStep {
        ScriptStep {
            reply: Ok(ScriptReply {
                body: body.to_owned(),
                input_tokens: 0,
                output_tokens: 0,
            }),
            delay: Duration::ZERO,
        }
    }

    fn failing(message: &str) -> ServiceError {
        ServiceError::Failed {
            service: None,
            message: message.into(),
        }
    }
}

fn envelope_text(request: &ModelRequest) -> Option<String> {
    for item in request.context.iter() {
        if let ContextItem::User { parts } = item {
            for part in parts {
                if let Part::Text { text } = part {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

fn inference_for(reply: &ScriptReply) -> Inference {
    Inference {
        events: vec![
            StreamEvent::Delta {
                channel: StreamChannel::Text,
                text: reply.body.clone().into(),
            },
            StreamEvent::Usage(Usage {
                input_tokens: reply.input_tokens,
                cached_input_tokens: 0,
                output_tokens: reply.output_tokens,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
            }),
        ],
    }
}

impl Services for FakeServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never reads files")) })
    }
    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never writes blobs")) })
    }
    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never reads blobs")) })
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never writes files")) })
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never fetches")) })
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never runs")) })
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never reads env")) })
    }

    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never asks")) })
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never calls MCP")) })
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never reads history")) })
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(
            std::sync::Arc<dyn dal_agent::ext::Tool>,
            dal_core::Visibility,
        )>,
    ) -> ServiceFuture<'_, ()> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never adds tools")) })
    }

    fn mcp_declarations(
        &self,
        _who: &Caller,
    ) -> ServiceFuture<'_, Vec<dal_core::ext::McpDeclaration>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never lists MCP blocks")) })
    }

    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never starts agents")) })
    }

    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never runs jobs")) })
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never counts asks")) })
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never resolves schemes")) })
    }

    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never steers turns")) })
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never touches sidecar")) })
    }

    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never touches state")) })
    }

    fn infer(&self, _who: &Caller, req: ModelRequest) -> ServiceFuture<'_, Inference> {
        if req.context.is_empty() {
            let reply = self.lock().probes.pop_front().expect("probe is scripted");
            return match reply {
                ProbeScript::Ready { model_id } => Box::pin(async move {
                    Ok(Inference {
                        events: vec![StreamEvent::Delta {
                            channel: StreamChannel::Text,
                            text: model_id.into(),
                        }],
                    })
                }),
                ProbeScript::Failing { message } => {
                    Box::pin(async move { Err(FakeServices::failing(&message)) })
                }
            };
        }
        let body = envelope_text(&req).expect("judge request carries its envelope");
        self.lock().completions.push(body);
        let step = self
            .lock()
            .steps
            .pop_front()
            .expect("completion is scripted");
        let overlap = &self.overlap;
        Box::pin(async move {
            let in_flight = overlap.fetch_add(1, Ordering::SeqCst) + 1;
            {
                let mut state = self.lock();
                state.max_overlap = state.max_overlap.max(in_flight);
            }
            sleep(step.delay).await;
            overlap.fetch_sub(1, Ordering::SeqCst);
            step.reply.map(|reply| inference_for(&reply))
        })
    }

    fn infer_stream(
        &self,
        _who: &Caller,
        _req: ModelRequest,
    ) -> ServiceFuture<'_, dal_provider::EventStream> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never streams")) })
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        Box::pin(async move { Err(FakeServices::failing("judge fake never calls tools")) })
    }

    fn notify(&self, _who: &Caller, notice: Notice) {
        self.lock().notices.push(notice.text.to_string());
    }

    fn append_record(
        &self,
        _who: &Caller,
        kind: &str,
        body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        let mut state = self.lock();
        state.rows.push((kind.into(), body.as_str().to_owned()));
        let entry =
            EntryId::new(NonZeroU64::new(state.next_entry).expect("entry counter stays nonzero"));
        state.next_entry += 1;
        Box::pin(async move { Ok(entry) })
    }

    fn records(&self, _who: &Caller, kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        let rows: Vec<Box<RawValue>> = self
            .lock()
            .rows
            .iter()
            .filter(|(row_kind, _)| row_kind.as_ref() == kind)
            .map(|(_, body)| {
                Box::new(RawValue::parse(body).expect("stored row parses")) as Box<RawValue>
            })
            .collect();
        Box::pin(async move { Ok(rows) })
    }
}

fn ready_probe(model_id: &str) -> ProbeScript {
    ProbeScript::Ready {
        model_id: model_id.to_owned(),
    }
}

fn failing_probe(message: &str) -> ProbeScript {
    ProbeScript::Failing {
        message: message.to_owned(),
    }
}

fn delayed(body: &str, delay: Duration) -> ScriptStep {
    ScriptStep {
        reply: Ok(ScriptReply {
            body: body.to_owned(),
            input_tokens: 11,
            output_tokens: 3,
        }),
        delay,
    }
}

fn test_config() -> JudgeConfig {
    JudgeConfig {
        gate: GateSetting::Auto,
        model: Box::from(""),
        timeout_ms: 30_000,
        max_concurrent: 4,
        max_per_turn: 16,
    }
}

async fn open_judge(
    services: Arc<FakeServices>,
    config: JudgeConfig,
    turn: Option<TurnId>,
) -> Judge {
    let session = SessionId::new_v7();
    let context = HookCx::for_test(Arc::clone(&services) as Arc<dyn Services>, session, turn);
    Judge::open(JudgeOpen {
        session: context.session,
        config,
        services: Arc::clone(&context.services),
        caller: context.caller.clone(),
        session_route: ModelRoute::from_id("test/model"),
        session_model_id: Box::from("test-session-model"),
    })
    .await
    .expect("judge opens")
}

fn bool_question(prompt: &str) -> JudgeQuestion {
    JudgeQuestion::bool(prompt).expect("valid bool question")
}

fn choice_question() -> JudgeQuestion {
    JudgeQuestion::choice("choose", &["keep", "revert", "ask"]).expect("valid choice question")
}

fn score_question() -> JudgeQuestion {
    JudgeQuestion::score("rate", 5).expect("valid score question")
}

#[tokio::test]
async fn kind_parse() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![
            FakeServices::scripted(r#"{"answers":[true]}"#),
            FakeServices::scripted(r#"{"answers":[1]}"#),
            FakeServices::scripted(r#"{"answers":[4]}"#),
        ],
    ));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    assert_eq!(
        judge
            .judge("ttsr", "shared", bool_question("safe?"), None, None)
            .await?,
        Verdict::Bool(true)
    );
    assert_eq!(
        judge
            .judge("ttsr", "shared", choice_question(), None, None)
            .await?,
        Verdict::Choice(1)
    );
    assert_eq!(
        judge
            .judge("ttsr", "shared", score_question(), None, None)
            .await?,
        Verdict::Score(4)
    );
    assert_eq!(services.completions(), 3);
    assert_eq!(services.row_statuses(), ["ok", "ok", "ok"]);
    Ok(())
}

#[tokio::test]
async fn batch_order() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![FakeServices::scripted(r#"{"answers":[false,2,0]}"#)],
    ));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    let verdicts = judge
        .judge_batch(
            "ttsr",
            "shared",
            vec![bool_question("first?"), choice_question(), score_question()],
            None,
            None,
        )
        .await?;
    assert_eq!(
        verdicts,
        [Verdict::Bool(false), Verdict::Choice(2), Verdict::Score(0)]
    );
    assert_eq!(services.completions(), 1);
    Ok(())
}

#[tokio::test]
async fn parse_rejections() -> TestResult {
    // Each body travels with the question the plan pins it against: `[3]`
    // is out of range for a 3-option choice, `[6]` overflows a score max
    // of 5, and `[1,2]` mismatches a single-question batch.
    let cases: [(&str, JudgeQuestion); 11] = [
        ("true", score_question()),
        ("{}", score_question()),
        (r#"{"answers":[1,2]}"#, score_question()),
        (r#"{"answers":["true"]}"#, score_question()),
        (r#"{"answers":[1.0]}"#, score_question()),
        (r#"{"answers":[3]}"#, choice_question()),
        (r#"{"answers":[6]}"#, score_question()),
        (r#"{"answers":[true],"why":"x"}"#, score_question()),
        (r#"{"answer":[true]}"#, score_question()),
        (r#"{"answers":[null]}"#, score_question()),
        ("", score_question()),
    ];
    let steps = cases
        .iter()
        .map(|(body, _)| FakeServices::scripted(body))
        .collect();
    let services = Arc::new(FakeServices::new(vec![ready_probe("judge-model")], steps));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    for (index, (body, question)) in cases.into_iter().enumerate() {
        let error = judge
            .judge("ttsr", "shared", question, None, None)
            .await
            .expect_err("malformed reply parses");
        assert!(matches!(error, JudgeError::Parse { .. }), "accepted {body}");
        assert_eq!(services.completions(), index + 1);
    }
    assert_eq!(services.row_statuses(), ["parse"; 11].map(str::to_owned));
    Ok(())
}

#[tokio::test]
async fn empty_batch() -> TestResult {
    let services = Arc::new(FakeServices::new(vec![ready_probe("judge-model")], vec![]));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    assert_eq!(
        judge
            .judge_batch("ttsr", "shared", vec![], None, None)
            .await?,
        Vec::new()
    );
    assert_eq!(services.completions(), 0);
    assert_eq!(services.rows(), []);
    Ok(())
}

#[tokio::test]
async fn timeout_aborts_request() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![delayed(r#"{"answers":[true]}"#, Duration::from_secs(5))],
    ));
    let config = JudgeConfig {
        timeout_ms: 1000,
        ..test_config()
    };
    let judge = open_judge(Arc::clone(&services), config, None).await;
    let error = judge
        .judge("ttsr", "shared", bool_question("late?"), None, None)
        .await
        .expect_err("slow reply times out");
    assert!(matches!(error, JudgeError::Timeout { ms: 1000 }));
    assert_eq!(services.row_statuses(), ["timeout"]);
    Ok(())
}

#[tokio::test]
async fn late_verdict_ledger_only() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![delayed(
            r#"{"answers":[true]}"#,
            Duration::from_millis(1500),
        )],
    ));
    let config = JudgeConfig {
        timeout_ms: 200,
        ..test_config()
    };
    let judge = open_judge(Arc::clone(&services), config, None).await;
    let error = judge
        .judge("ttsr", "shared", bool_question("late?"), None, None)
        .await
        .expect_err("late verdict times out");
    assert!(matches!(error, JudgeError::Timeout { ms: 200 }));
    let rows = services.rows();
    assert_eq!(rows.len(), 1);
    let row: sonic_rs::Value = sonic_rs::from_str(&rows[0].1)?;
    assert_eq!(
        row.get("status").and_then(JsonValueTrait::as_str),
        Some("timeout")
    );
    let duration = row
        .get("duration_ms")
        .and_then(JsonValueTrait::as_u64)
        .expect("timeout row carries its duration");
    assert!(
        duration <= 5000,
        "row settled at the deadline, not the reply"
    );
    Ok(())
}

#[tokio::test]
async fn zero_judge_requests() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![failing_probe("judge disabled by gate")],
        vec![],
    ));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    assert!(matches!(judge.state(), Gate::Off));
    let error = judge
        .judge("ttsr", "shared", bool_question("any?"), None, None)
        .await
        .expect_err("off gate never judges");
    assert!(matches!(error, JudgeError::Unavailable));
    assert_eq!(services.completions(), 0);
    assert_eq!(services.rows(), []);
    Ok(())
}

#[tokio::test]
async fn on_without_credentials() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![failing_probe(REASON_NO_CREDENTIALS)],
        vec![],
    ));
    let session = SessionId::new_v7();
    let context = HookCx::for_test(Arc::clone(&services) as Arc<dyn Services>, session, None);
    let error = Judge::open(JudgeOpen {
        session: context.session,
        config: JudgeConfig {
            gate: GateSetting::On,
            ..test_config()
        },
        services: Arc::clone(&context.services),
        caller: context.caller.clone(),
        session_route: ModelRoute::from_id("test/model"),
        session_model_id: Box::from("test-session-model"),
    })
    .await
    .expect_err("enabled role without credentials fails to open");
    assert!(matches!(error, JudgeError::Unavailable));
    Ok(())
}

#[tokio::test]
async fn auto_resolution() -> TestResult {
    let off = Arc::new(FakeServices::new(
        vec![failing_probe("judge disabled by gate")],
        vec![],
    ));
    let judge = open_judge(Arc::clone(&off), test_config(), None).await;
    assert!(matches!(judge.state(), Gate::Off));
    assert!(matches!(
        judge
            .judge("ttsr", "shared", bool_question("any?"), None, None)
            .await,
        Err(JudgeError::Unavailable)
    ));
    assert_eq!(off.notices(), [] as [String; 0]);

    let ready = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![FakeServices::scripted(r#"{"answers":[true]}"#)],
    ));
    let judge = open_judge(Arc::clone(&ready), test_config(), None).await;
    assert!(matches!(judge.state(), Gate::Ready { .. }));
    assert_eq!(
        judge
            .judge("ttsr", "shared", bool_question("go?"), None, None)
            .await?,
        Verdict::Bool(true)
    );
    assert_eq!(ready.notices(), [] as [String; 0]);
    Ok(())
}

#[tokio::test]
async fn budget_exhausted_call() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![
            FakeServices::scripted(r#"{"answers":[true]}"#),
            FakeServices::scripted(r#"{"answers":[false]}"#),
        ],
    ));
    let config = JudgeConfig {
        max_per_turn: 2,
        ..test_config()
    };
    let turn = Some(TurnId::new(NonZeroU64::new(3).expect("nonzero turn")));
    let judge = open_judge(Arc::clone(&services), config, turn).await;
    assert_eq!(
        judge
            .judge("ttsr", "shared", bool_question("one?"), turn, None)
            .await?,
        Verdict::Bool(true)
    );
    assert_eq!(
        judge
            .judge("ttsr", "shared", bool_question("two?"), turn, None)
            .await?,
        Verdict::Bool(false)
    );
    let error = judge
        .judge("ttsr", "shared", bool_question("three?"), turn, None)
        .await
        .expect_err("third call exceeds the turn budget");
    assert!(matches!(
        error,
        JudgeError::BudgetExhausted { max_per_turn: 2 }
    ));
    assert_eq!(services.row_statuses(), ["ok", "ok", "budget"]);
    assert_eq!(services.notices(), [] as [String; 0]);
    assert_eq!(services.completions(), 2);
    Ok(())
}

#[tokio::test]
async fn concurrency_and_fifo() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![
            delayed(r#"{"answers":[true]}"#, Duration::from_millis(200)),
            delayed(r#"{"answers":[true]}"#, Duration::from_millis(200)),
            delayed(r#"{"answers":[true]}"#, Duration::from_millis(200)),
            delayed(r#"{"answers":[true]}"#, Duration::from_millis(200)),
        ],
    ));
    let config = JudgeConfig {
        max_concurrent: 2,
        ..test_config()
    };
    let judge = open_judge(Arc::clone(&services), config, None).await;
    let calls = (0..4)
        .map(|_| {
            let judge = judge.clone();
            async move {
                judge
                    .judge("ttsr", "shared", bool_question("parallel?"), None, None)
                    .await
            }
        })
        .collect::<Vec<_>>();
    for verdict in futures::future::join_all(calls).await {
        assert!(matches!(verdict, Ok(Verdict::Bool(true))));
    }
    assert_eq!(services.max_overlap(), 2);
    let mut waits: Vec<(u64, u64)> = Vec::new();
    for (_, body) in services.rows() {
        let row: sonic_rs::Value = sonic_rs::from_str(&body)?;
        waits.push((
            row.get("call")
                .and_then(JsonValueTrait::as_u64)
                .expect("row call"),
            row.get("slot_wait_ms")
                .and_then(JsonValueTrait::as_u64)
                .expect("row slot wait"),
        ));
    }
    waits.sort_unstable();
    assert_eq!(waits.len(), 4);
    for window in waits.windows(2) {
        assert!(window[0].1 <= window[1].1, "slot waits follow submit order");
    }
    assert!(
        waits[3].1.saturating_sub(waits[0].1) >= 50,
        "queued calls actually waited"
    );
    Ok(())
}

#[tokio::test]
async fn streak_notices() -> TestResult {
    let steps = vec![FakeServices::scripted(r#"{"answers":[]}"#); 8];
    let services = Arc::new(FakeServices::new(vec![ready_probe("judge-model")], steps));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    for _ in 0..3 {
        assert!(matches!(
            judge
                .judge("ttsr", "shared", bool_question("bad?"), None, None)
                .await,
            Err(JudgeError::Parse { .. })
        ));
    }
    assert_eq!(services.notices().len(), 1);
    assert!(matches!(
        judge
            .judge("ttsr", "shared", bool_question("bad?"), None, None)
            .await,
        Err(JudgeError::Parse { .. })
    ));
    assert_eq!(services.notices().len(), 1);
    Ok(())
}

#[tokio::test]
async fn streak_resets_after_success() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![
            FakeServices::scripted(r#"{"answers":[]}"#),
            FakeServices::scripted(r#"{"answers":[]}"#),
            FakeServices::scripted(r#"{"answers":[]}"#),
            FakeServices::scripted(r#"{"answers":[true]}"#),
            FakeServices::scripted(r#"{"answers":[]}"#),
            FakeServices::scripted(r#"{"answers":[]}"#),
            FakeServices::scripted(r#"{"answers":[]}"#),
        ],
    ));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    for _ in 0..3 {
        assert!(matches!(
            judge
                .judge("ttsr", "shared", bool_question("bad?"), None, None)
                .await,
            Err(JudgeError::Parse { .. })
        ));
    }
    assert_eq!(services.notices().len(), 1);
    assert_eq!(
        judge
            .judge("ttsr", "shared", bool_question("good?"), None, None)
            .await?,
        Verdict::Bool(true)
    );
    for _ in 0..3 {
        assert!(matches!(
            judge
                .judge("ttsr", "shared", bool_question("bad?"), None, None)
                .await,
            Err(JudgeError::Parse { .. })
        ));
    }
    assert_eq!(services.notices().len(), 2);
    assert!(
        services.notices()[0].starts_with(STREAK_NOTICE_PREFIX),
        "streak notice keeps its prefix"
    );
    Ok(())
}

#[tokio::test]
async fn eval_helpers() -> TestResult {
    let off = Arc::new(FakeServices::new(
        vec![failing_probe("judge disabled by gate")],
        vec![],
    ));
    let judge = open_judge(Arc::clone(&off), test_config(), None).await;
    assert!(matches!(
        judge
            .judge("eval", "shared", bool_question("any?"), None, None)
            .await,
        Err(JudgeError::Unavailable)
    ));
    assert!(matches!(
        judge
            .judge_batch("eval", "shared", vec![bool_question("any?")], None, None)
            .await,
        Err(JudgeError::Unavailable)
    ));

    let ready = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![FakeServices::scripted(r#"{"answers":[true,false]}"#)],
    ));
    let judge = open_judge(Arc::clone(&ready), test_config(), None).await;
    assert_eq!(
        judge
            .judge_batch(
                "eval",
                "shared",
                vec![bool_question("one?"), bool_question("two?")],
                None,
                None,
            )
            .await?,
        [Verdict::Bool(true), Verdict::Bool(false)]
    );
    Ok(())
}

#[tokio::test]
async fn shared_cap() -> TestResult {
    let services = Arc::new(FakeServices::new(vec![ready_probe("judge-model")], vec![]));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    let shared = "x".repeat(SHARED_MAX + 1);
    let error = judge
        .judge("ttsr", &shared, bool_question("big?"), None, None)
        .await
        .expect_err("oversize shared context is rejected");
    assert!(matches!(error, JudgeError::SharedTooLarge { len: 16385 }));
    assert_eq!(services.completions(), 0);
    assert_eq!(services.rows(), []);
    Ok(())
}

#[tokio::test]
async fn ledger_redaction_size() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![FakeServices::scripted(r#"{"answers":[true]}"#)],
    ));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    let sentinel = "SENTINEL-9f2c-unique-marker";
    let shared = format!("context carrying {sentinel} exactly once");
    judge
        .judge("ttsr", &shared, bool_question("go?"), None, None)
        .await?;
    let rows = services.rows();
    assert_eq!(rows.len(), 1);
    let (kind, body) = &rows[0];
    assert_eq!(kind, "judge");
    assert!(!body.contains(sentinel), "ledger row redacts shared input");
    assert!(body.len() <= 1024);
    let row: sonic_rs::Value = sonic_rs::from_str(body)?;
    for field in [
        "kind",
        "call",
        "turn",
        "feature",
        "questions",
        "status",
        "cause",
        "model",
        "duration_ms",
        "slot_wait_ms",
        "input_tokens",
        "output_tokens",
    ] {
        assert!(row.get(field).is_some(), "row carries {field}");
    }
    Ok(())
}

#[tokio::test]
async fn judged_rule_end_to_end() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![
            FakeServices::scripted(r#"{"answers":[true]}"#),
            FakeServices::scripted(r#"{"answers":[false]}"#),
        ],
    ));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    assert_eq!(
        judge
            .judge("ttsr", "shared", bool_question("fire?"), None, None)
            .await?,
        Verdict::Bool(true)
    );
    assert_eq!(
        judge
            .judge("ttsr", "shared", bool_question("fire?"), None, None)
            .await?,
        Verdict::Bool(false)
    );
    assert_eq!(services.row_statuses(), ["ok", "ok"]);
    assert!(matches!(judge.state(), Gate::Ready { .. }));
    Ok(())
}

#[tokio::test]
async fn config_validation() -> TestResult {
    use dal_core::{Config, ConfigError, ConfigProduct};
    use std::path::Path;
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new("."),
        "",
        Some("[judge]\ngate = \"maybe\"\n"),
    )?;
    assert!(matches!(
        JudgeConfig::from_config(&config),
        Err(ConfigError::InvalidValue { key, value, expected })
            if key.as_ref() == "judge.gate"
                && value.as_ref() == "\"maybe\""
                && expected.as_ref() == "Use one of auto, on, off."
    ));
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new("."),
        "",
        Some("[judge]\ntimeout_ms = 100\n"),
    )?;
    assert!(matches!(
        JudgeConfig::from_config(&config),
        Err(ConfigError::InvalidValue { key, value, expected })
            if key.as_ref() == "judge.timeout_ms"
                && value.as_ref() == "100"
                && expected.as_ref() == "Use a whole number from 1000 to 300000."
    ));
    let config = Config::load(ConfigProduct::Dalgon, Path::new("."), "", None)?;
    assert_eq!(JudgeConfig::from_config(&config)?, JudgeConfig::default());
    Ok(())
}

#[tokio::test]
async fn construction_validation() -> TestResult {
    let option = "a".repeat(201);
    assert!(JudgeQuestion::choice("choose", &[&option, "other"]).is_err());
    for error in [
        JudgeQuestion::choice("choose", &["only"]).expect_err("one option is invalid"),
        JudgeQuestion::choice("choose", &["a"; 27]).expect_err("27 options are invalid"),
        JudgeQuestion::score("rate", 0).expect_err("zero max is invalid"),
        JudgeQuestion::bool("one\ntwo").expect_err("multiline prompt is invalid"),
    ] {
        assert!(matches!(error, JudgeError::InvalidQuestion { .. }));
    }
    let services = Arc::new(FakeServices::new(vec![ready_probe("judge-model")], vec![]));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    let batch = (0..33)
        .map(|index| JudgeQuestion::bool(&format!("question {index}")))
        .collect::<Result<Vec<_>, _>>()?;
    let error = judge
        .judge_batch("ttsr", "shared", batch, None, None)
        .await
        .expect_err("33 questions exceed the batch cap");
    assert!(matches!(error, JudgeError::InvalidQuestion { .. }));
    assert_eq!(services.completions(), 0);
    assert_eq!(services.rows(), []);
    Ok(())
}

#[tokio::test]
async fn provider_error() -> TestResult {
    let services = Arc::new(FakeServices::new(
        vec![ready_probe("judge-model")],
        vec![ScriptStep {
            reply: Err(FakeServices::failing("boom downstream")),
            delay: Duration::ZERO,
        }],
    ));
    let judge = open_judge(Arc::clone(&services), test_config(), None).await;
    let error = judge
        .judge("ttsr", "shared", bool_question("go?"), None, None)
        .await
        .expect_err("provider failure surfaces");
    assert!(matches!(error, JudgeError::Provider { .. }));
    assert!(error.to_string().contains("boom downstream"));
    assert_eq!(services.row_statuses(), ["provider"]);
    assert_eq!(services.notices(), [] as [String; 0]);
    Ok(())
}

fn oracle_questions(kinds: &[u8]) -> Vec<JudgeQuestion> {
    kinds
        .iter()
        .map(|kind| match kind % 3 {
            0 => JudgeQuestion::Bool {
                prompt: Box::from("question"),
            },
            1 => JudgeQuestion::Choice {
                prompt: Box::from("question"),
                options: vec![Box::from("no"), Box::from("yes")].into_boxed_slice(),
            },
            _ => JudgeQuestion::Score {
                prompt: Box::from("question"),
                max: 5,
            },
        })
        .collect()
}

fn json_atom() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<bool>().prop_map(|value| value.to_string()),
        (-3_i64..=300).prop_map(|value| value.to_string()),
        Just(String::from("null")),
        Just(String::from("1.0")),
        Just(String::from("1e0")),
        Just(String::from("\"text\"")),
    ]
}

fn oracle(body: &str, questions: &[JudgeQuestion]) -> Option<Vec<Verdict>> {
    let value: sonic_rs::Value = sonic_rs::from_str(body.trim()).ok()?;
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let answers = value.get("answers")?.as_array()?;
    if answers.len() != questions.len() {
        return None;
    }
    let mut verdicts = Vec::with_capacity(questions.len());
    for (answer, question) in answers.iter().zip(questions) {
        let verdict = match question {
            JudgeQuestion::Bool { .. } => Verdict::Bool(answer.as_bool()?),
            JudgeQuestion::Choice { options, .. } => {
                let index = answer.as_u64()?;
                let option_count = u64::try_from(options.len()).ok()?;
                if index >= option_count {
                    return None;
                }
                Verdict::Choice(u8::try_from(index).ok()?)
            }
            JudgeQuestion::Score { max, .. } => {
                let score = answer.as_u64()?;
                if score > u64::from(*max) {
                    return None;
                }
                Verdict::Score(u8::try_from(score).ok()?)
            }
        };
        verdicts.push(verdict);
    }
    Some(verdicts)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn parser_property(
        kinds in prop::collection::vec(0_u8..3, 0..=8),
        atoms in prop::collection::vec(json_atom(), 0..=8),
    ) {
        use super::parse_answers;
        let questions = oracle_questions(&kinds);
        let body = format!("{{\"answers\":[{}]}}", atoms.join(","));
        match (parse_answers(&body, &questions), oracle(&body, &questions)) {
            (Ok(actual), Some(expected)) => prop_assert_eq!(actual, expected),
            (Err(_), None) => {}
            _ => prop_assert!(false, "parser disagreed with the independent value oracle"),
        }
        for verdicts in [parse_answers(&body, &questions)].into_iter().flatten() {
            prop_assert_eq!(verdicts.len(), questions.len());
        }
    }

    #[test]
    fn parser_never_panics_on_bounded_reply_bodies(
        kinds in prop::collection::vec(0_u8..3, 0..=8),
        body_bytes in prop::collection::vec(0_u8..=127, 0..=256),
    ) {
        use super::parse_answers;
        let questions = oracle_questions(&kinds);
        let body: String = body_bytes.into_iter().map(char::from).collect();
        let _ = parse_answers(&body, &questions);
    }
}
