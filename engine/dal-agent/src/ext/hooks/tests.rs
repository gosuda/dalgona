//! Hook composition, deadline, observer, and watcher tests.

mod actor;
mod cancel;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_core::ToolClass;
use dal_core::ext::{BeforeRequest, BeforeTurn};
use dal_core::{CallId, TurnId};
use dal_core::{
    Caps, InputEvent, InputVerdict, ModelInfo, ModelRoute, Name, Origin, Part, RawJson,
    RequestParams, ServiceSet, SessionId, StateError, StateOp, StateRecord, ToolCallEvent,
    ToolCallVerdict,
};
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    Approval, approve_tool_call, clamp_params, dispatch_before_request, dispatch_before_turn,
    dispatch_input, dispatch_tool_call, effective_deadline,
};
use super::{
    BeforeTurnStep, DispatchCx, InputStep, LosslessQueue, LossyQueue, ToolCallStep, ToolDecision,
    join_before_turn,
};
use crate::ext::services::ServiceFuture;
use crate::ext::{BoxFuture, Caller, CallerKind, Hook, HookError, Services};
use crate::ext::{RawValue, ToolOutcome};
use dal_core::ext::{McpRequest, McpResponse};
use dal_core::{
    AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
    JobsReply, ModelRequest, Notice, PrivateEffect, Question, RunOutput, RunRequest, SidecarOp,
    TurnOp, TurnOpReply,
};
use dal_provider::EventStream;
struct NoSvcs;

impl Services for NoSvcs {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("hooks tests never call services")
    }
    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unreachable!("hooks tests never call services")
    }
    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unreachable!("hooks tests never call services")
    }
    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unreachable!("hooks tests never call services")
    }
    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unreachable!("hooks tests never call services")
    }
    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unreachable!("hooks tests never call services")
    }
    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unreachable!("hooks tests never call services")
    }
    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unreachable!("hooks tests never call services")
    }
    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(
            std::sync::Arc<dyn crate::ext::tool::Tool>,
            dal_core::Visibility,
        )>,
    ) -> ServiceFuture<'_, ()> {
        unreachable!("hooks tests never call services")
    }
    fn mcp_declarations(
        &self,
        _who: &Caller,
    ) -> ServiceFuture<'_, Vec<dal_core::ext::McpDeclaration>> {
        unreachable!("hooks tests never call services")
    }
    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unreachable!("hooks tests never call services")
    }
    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        unreachable!("hooks tests never call services")
    }
    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unreachable!("hooks tests never call services")
    }
    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<crate::ext::Doc>> {
        unreachable!("hooks tests never call services")
    }
    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unreachable!("hooks tests never call services")
    }
    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("hooks tests never call services")
    }
    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unreachable!("hooks tests never call services")
    }
    fn model_ready(
        &self,
        _who: &Caller,
        _model: dal_core::ModelRoute,
    ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
        unreachable!("hooks tests never call services")
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unreachable!("hooks tests never call services")
    }
    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        unreachable!("hooks tests never call services")
    }
    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unreachable!("hooks tests never call services")
    }
    fn notify(&self, _who: &Caller, _notice: Notice) {
        unreachable!("hooks tests never call services")
    }
    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unreachable!("hooks tests never call services")
    }
    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        unreachable!("hooks tests never call services")
    }
    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
        unreachable!("hooks tests never call services")
    }
    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unreachable!("hooks tests never call services")
    }
    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("hooks tests never call services")
    }
}

fn caller() -> Caller {
    Caller::new(
        "test".parse::<Name>().expect("name"),
        Origin::Builtin,
        ServiceSet::EMPTY,
        std::num::NonZeroU32::MIN,
        CallerKind::Hook,
        Some(TurnId::new(std::num::NonZeroU64::MIN)),
    )
}

fn cx<'a>(
    caller: &'a Caller,
    services: &'a Arc<dyn Services>,
    cancel: &'a CancellationToken,
) -> DispatchCx<'a> {
    DispatchCx {
        parent: None,
        caller,
        services,
        session: SessionId::new_v7(),
        process_env: Arc::new(crate::Env {
            vars: BTreeMap::default(),
            cwd: PathBuf::default(),
            sandbox_helper: None,
        }),
        turn: Some(TurnId::new(std::num::NonZeroU64::MIN)),
        cancel,
        turn_deadline: Instant::now() + Duration::from_secs(30),
        script: None,
    }
}

fn tool_event(args: &str) -> ToolCallEvent {
    ToolCallEvent {
        turn: TurnId::new(std::num::NonZeroU64::MIN),
        call: CallId::new("call-1"),
        tool: "demo".parse::<Name>().expect("tool"),
        args: RawJson::parse(args).expect("args"),
        class: ToolClass::Read,
    }
}

struct RewriteHook {
    to: RawJson,
    seen: Mutex<Vec<String>>,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for RewriteHook {
    fn call(
        &self,
        input: ToolCallEvent,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let raw = input.args.as_str().to_owned();
        self.seen.lock().expect("seen").push(raw);
        let args = self.to.clone();
        Box::pin(async move { Ok(ToolCallVerdict::Rewrite { args }) })
    }
}

struct BlockHook {
    calls: Mutex<u32>,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for BlockHook {
    fn call(
        &self,
        _input: ToolCallEvent,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        *self.calls.lock().expect("calls") += 1;
        Box::pin(async move {
            Ok(ToolCallVerdict::Block {
                reason: "nope".into(),
            })
        })
    }
}

struct FailHook;

impl Hook<ToolCallEvent, ToolCallVerdict> for FailHook {
    fn call(
        &self,
        _input: ToolCallEvent,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        Box::pin(async move {
            Err(HookError::Failed {
                message: "hook \"test\" failed: boom".into(),
            })
        })
    }
}

#[tokio::test]
async fn tool_call_rewrite_and_block_compose_in_order() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let rewrite = Arc::new(RewriteHook {
        to: RawJson::parse(r#"{"b":1}"#).expect("to"),
        seen: Mutex::new(Vec::new()),
    });
    let block = Arc::new(BlockHook {
        calls: Mutex::new(0),
    });
    let hooks: Vec<Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>> =
        vec![rewrite.clone(), block.clone()];
    let step: ToolCallStep = dispatch_tool_call(
        "test",
        &cx,
        &hooks,
        &tool_event(r#"{"a":1}"#),
        RawJson::parse(r#"{"a":1}"#).expect("args"),
    )
    .await;
    assert!(step.block.is_some());
    assert_eq!(*block.calls.lock().expect("calls"), 1);
    assert_eq!(rewrite.seen.lock().expect("seen").len(), 1);

    let fail: Vec<Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>> = vec![Arc::new(FailHook)];
    let failed = dispatch_tool_call(
        "test",
        &cx,
        &fail,
        &tool_event("{}"),
        RawJson::parse("{}").expect("args"),
    )
    .await;
    assert!(failed.block.is_some_and(|reason| reason.contains("boom")));

    let allow = |_event: ToolCallEvent| -> BoxFuture<'static, Approval> {
        Box::pin(async { Approval::Allow })
    };
    let decision = approve_tool_call(
        &tool_event(r#"{"b":1}"#),
        &allow,
        &cancel,
        cx.turn_deadline,
        5_000,
    )
    .await;
    assert!(matches!(decision, ToolDecision::Allow { .. }));
}

struct TextHook {
    verdict: InputVerdict,
    seen: Mutex<Vec<String>>,
}

impl Hook<InputEvent, InputVerdict> for TextHook {
    fn call(
        &self,
        input: InputEvent,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let text: String = input
            .content
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.to_string()),
                _ => None,
            })
            .collect();
        self.seen.lock().expect("seen").push(text);
        let verdict = self.verdict.clone();
        Box::pin(async move { Ok(verdict) })
    }
}

fn text_event(text: &str) -> InputEvent {
    InputEvent {
        content: vec![Part::Text { text: text.into() }],
        model: None,
    }
}

#[tokio::test]
async fn input_builtin_handled_runs_later_origins() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let handled = Arc::new(TextHook {
        verdict: InputVerdict::Handled,
        seen: Mutex::new(Vec::new()),
    });
    let later = Arc::new(TextHook {
        verdict: InputVerdict::Transform(vec![Part::Text {
            text: "later".into(),
        }]),
        seen: Mutex::new(Vec::new()),
    });
    let hooks: Vec<Arc<dyn Hook<InputEvent, InputVerdict>>> = vec![handled.clone(), later.clone()];
    let builtin_step: InputStep =
        dispatch_input("test", &cx, true, &hooks, &text_event("hi")).await;
    assert!(!builtin_step.stop);
    assert_eq!(later.seen.lock().expect("seen").len(), 1);
    let user_step: InputStep = dispatch_input("test", &cx, false, &hooks, &text_event("hi")).await;
    assert!(user_step.stop);
}

struct TurnHook {
    text: Option<String>,
}

impl Hook<BeforeTurn, Option<String>> for TurnHook {
    fn call(
        &self,
        _input: BeforeTurn,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let text = self.text.clone();
        Box::pin(async move { Ok(text) })
    }
}

#[tokio::test]
async fn before_turn_joins_without_changing_cached_prefix() {
    use dal_core::{EntryId, EntryKind, EntryView, JournalPart};

    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let event = BeforeTurn {
        turn: TurnId::new(std::num::NonZeroU64::MIN),
        text: "prompt".into(),
    };
    let hooks_one: Vec<Arc<dyn Hook<BeforeTurn, Option<String>>>> = vec![Arc::new(TurnHook {
        text: Some("a".into()),
    })];
    let hooks_two: Vec<Arc<dyn Hook<BeforeTurn, Option<String>>>> = vec![Arc::new(TurnHook {
        text: Some("b".into()),
    })];
    let mut texts = Vec::new();
    for (ext, hooks) in [("one", &hooks_one), ("two", &hooks_two)] {
        let step: BeforeTurnStep = dispatch_before_turn(ext, &cx, hooks, &event).await;
        texts.extend(step.texts);
    }
    let joined = join_before_turn(&texts).expect("both hooks contributed text");
    assert_eq!(joined, "a\n\nb");

    let entry = |id: u64, kind: EntryKind| EntryView {
        id: EntryId::new(std::num::NonZeroU64::new(id).expect("entry id")),
        parent: std::num::NonZeroU64::new(id - 1).map(EntryId::new),
        kind,
    };
    let user = |id: u64, text: &str| {
        entry(
            id,
            EntryKind::User {
                parts: vec![JournalPart::Text { text: text.into() }],
            },
        )
    };
    let hook = entry(
        2,
        EntryKind::Reminder {
            source: dal_core::BEFORE_TURN_SOURCE.into(),
            text: joined.into(),
        },
    );
    let turn_one = vec![user(1, "prompt"), hook];
    let mut later = turn_one.clone();
    later.push(user(3, "next prompt"));

    let before = crate::session::context::context_items(&turn_one);
    let after = crate::session::context::context_items(&later);
    assert_eq!(before.len(), 2, "the hook text is one context item");
    assert_eq!(
        after.get(..before.len()),
        Some(before.as_slice()),
        "a later entry leaves the earlier context bytes, hook text included, unchanged"
    );
}

struct HotHook;
impl Hook<BeforeRequest, Option<RequestParams>> for HotHook {
    fn call(
        &self,
        mut input: BeforeRequest,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<Option<RequestParams>, HookError>> {
        input.params.thinking = dal_core::ThinkingLevel::High;
        let params = input.params.clone();
        Box::pin(async move { Ok(Some(params)) })
    }
}

#[tokio::test]
async fn before_request_preserves_payload_and_clamps_typed_params() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let caps = Caps {
        context_window: Some(4096),
        thinking: Box::new([dal_core::ThinkingLevel::Low]),
        tool_use: false,
        image_input: false,
        custom_grammar: false,
    };
    let params = RequestParams::default();
    let event = BeforeRequest {
        turn: TurnId::new(std::num::NonZeroU64::MIN),
        round: 0,
        model: ModelInfo {
            route: ModelRoute::Synthetic {
                id: "acme/test".into(),
            },
            name: "test".into(),
            provider: "".into(),
            caps: caps.clone(),
        },
        caps: caps.clone(),
        params: params.clone(),
        thinking_explicit: false,
    };
    let before = sonic_rs::to_string(&event).expect("encode");
    let hooks: Vec<Arc<dyn Hook<BeforeRequest, Option<RequestParams>>>> = vec![Arc::new(HotHook)];
    let step = dispatch_before_request("test", &cx, &hooks, &event, params).await;
    let clamped = clamp_params(step.params, &caps);
    assert_eq!(clamped.thinking, dal_core::ThinkingLevel::Low);
    assert_eq!(sonic_rs::to_string(&event).expect("encode"), before);
}

#[test]
fn guard_deadline_and_cancel_bound_waits() {
    // The effective deadline is the earlier of the turn deadline and now plus
    // the wait: a far turn deadline still caps, a near one passes through.
    let before = Instant::now();
    let far = before + Duration::from_secs(3600);
    let capped = effective_deadline(far, 5_000);
    let after = Instant::now();
    assert!(
        capped >= before + Duration::from_millis(5_000),
        "a far deadline caps at now plus the wait: {capped:?}"
    );
    assert!(
        capped <= after + Duration::from_millis(5_000),
        "the cap adds no extra budget: {capped:?}"
    );
    assert!(capped <= far);
    let near = Instant::now() + Duration::from_millis(100);
    assert_eq!(
        effective_deadline(near, 5_000),
        near,
        "a nearer turn deadline passes through uncapped"
    );
}

#[tokio::test]
async fn observer_queue_drops_oldest_and_lossless_queue_backpressures() {
    let mut queue: LossyQueue<u32> = LossyQueue::new();
    for value in 0..300 {
        queue.push(value);
    }
    assert_eq!(queue.dropped(), 44);
    assert_eq!(queue.len(), 256);
    let lossless: LosslessQueue<u32> = LosslessQueue::new();
    let cancel = CancellationToken::new();
    for value in 0..64 {
        assert!(lossless.push(value, &cancel).await);
    }
    assert_eq!(lossless.len(), 64);
    cancel.cancel();
    assert!(!lossless.push(64, &cancel).await);
    assert_eq!(lossless.len(), 64);
    assert_eq!(lossless.pop(), Some(0));
    assert_eq!(lossless.len(), 63);
}

struct CancelHeldHook {
    entered: Mutex<Option<oneshot::Sender<()>>>,
    release: Arc<Notify>,
    completed: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}

struct CancelDropGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for CancelDropGuard {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

impl Hook<ToolCallEvent, ToolCallVerdict> for CancelHeldHook {
    fn call(
        &self,
        _input: ToolCallEvent,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let sender = self.entered.lock().expect("entered").take();
        let release = Arc::clone(&self.release);
        let completed = Arc::clone(&self.completed);
        let dropped = Arc::clone(&self.dropped);
        Box::pin(async move {
            let _guard = CancelDropGuard { flag: dropped };
            if let Some(sender) = sender {
                let _ = sender.send(());
            }
            release.notified().await;
            completed.store(true, Ordering::SeqCst);
            Ok(ToolCallVerdict::Allow)
        })
    }
}

struct ReadyAllowHook;

impl Hook<ToolCallEvent, ToolCallVerdict> for ReadyAllowHook {
    fn call(
        &self,
        _input: ToolCallEvent,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        Box::pin(async move { Ok(ToolCallVerdict::Allow) })
    }
}

#[tokio::test(start_paused = true)]
async fn tool_call_held_hook_cancel_denies_and_drops_future() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let (entered_tx, mut entered) = oneshot::channel::<()>();
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let hook = Arc::new(CancelHeldHook {
        entered: Mutex::new(Some(entered_tx)),
        release: Arc::clone(&release),
        completed: Arc::clone(&completed),
        dropped: Arc::clone(&dropped),
    });
    let hooks: Vec<Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>> = vec![hook];
    let event = tool_event(r#"{"a":1}"#);
    let start = Instant::now();
    let mut dispatch = Box::pin(dispatch_tool_call(
        "test",
        &cx,
        &hooks,
        &event,
        RawJson::parse(r#"{"a":1}"#).expect("args"),
    ));
    tokio::select! {
        step = &mut dispatch => panic!("dispatch completed before held hook entered: {step:?}"),
        _ = &mut entered => {},
    }
    cancel.cancel();
    // Make the held future ready too: only biased cancel-first selection can
    // still deny. A removed cancel arm would return Allow here, and a deadline
    // win would auto-advance paused virtual time.
    release.notify_waiters();
    let step = dispatch.await;
    assert_eq!(
        Instant::now(),
        start,
        "cancelled guard must deny via the cancel branch without advancing to the hook budget deadline"
    );
    assert!(
        step.block.is_some(),
        "cancelled guard must fail closed instead of allowing"
    );
    assert!(
        !completed.load(Ordering::SeqCst),
        "cancelled hook must not run past its held point"
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "cancelled hook future must be dropped"
    );
    release.notify_waiters();
    tokio::task::yield_now().await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "release after cancel must not publish a later side effect"
    );
}

#[tokio::test(start_paused = true)]
async fn approval_held_approver_cancel_denies_and_drops_future() {
    let cancel = CancellationToken::new();
    let turn_deadline = Instant::now() + Duration::from_secs(3600);
    let (entered_tx, mut entered) = oneshot::channel::<()>();
    let entered_tx = Mutex::new(Some(entered_tx));
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let approver = |_event: ToolCallEvent| -> BoxFuture<'static, Approval> {
        let sender = entered_tx.lock().expect("entered").take();
        let release = Arc::clone(&release);
        let completed = Arc::clone(&completed);
        let dropped = Arc::clone(&dropped);
        Box::pin(async move {
            let _guard = CancelDropGuard { flag: dropped };
            if let Some(sender) = sender {
                let _ = sender.send(());
            }
            release.notified().await;
            completed.store(true, Ordering::SeqCst);
            Approval::Allow
        })
    };
    let event = tool_event(r#"{"a":1}"#);
    let mut approval = Box::pin(approve_tool_call(
        &event,
        &approver,
        &cancel,
        turn_deadline,
        5_000,
    ));
    tokio::select! {
        decision = &mut approval => {
            panic!("approval completed before held approver entered: {decision:?}")
        },
        _ = &mut entered => {},
    }
    cancel.cancel();
    // Make the held approver ready too: only biased cancel-first selection can
    // still deny. A removed cancel arm would return Allow here, and a deadline
    // win would auto-advance paused virtual time.
    release.notify_waiters();
    let start = Instant::now();
    let decision = approval.await;
    assert_eq!(
        Instant::now(),
        start,
        "cancelled approval must deny via the cancel branch without advancing to the grant deadline"
    );
    assert!(
        matches!(&decision, ToolDecision::Denied),
        "cancelled approval must deny, got {decision:?}"
    );
    assert!(
        !completed.load(Ordering::SeqCst),
        "cancelled approver must not run past its held point"
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "cancelled approver future must be dropped"
    );
    release.notify_waiters();
    tokio::task::yield_now().await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "release after cancel must not publish a later side effect"
    );
}

#[test]
fn watch_factory_starts_once_and_watcher_is_synchronous() {
    use super::watch::{StreamWatch, TurnInfo, WatchFactory, feed_watchers, start_watchers};
    struct Rec {
        log: Arc<Mutex<Vec<&'static str>>>,
        tag: &'static str,
        interrupt: bool,
    }
    impl StreamWatch for Rec {
        fn feed(&mut self, _: dal_core::Channel, _: &str) -> dal_core::StreamVerdict {
            self.log.lock().expect("log").push(self.tag);
            if self.interrupt {
                return dal_core::StreamVerdict::Interrupt {
                    rule: "r".into(),
                    inject: "stop".into(),
                };
            }
            dal_core::StreamVerdict::Continue
        }
        fn finish(&mut self) -> dal_core::StreamVerdict {
            dal_core::StreamVerdict::Continue
        }
    }
    struct One {
        log: Arc<Mutex<Vec<&'static str>>>,
        tag: &'static str,
        interrupt: bool,
        starts: Arc<Mutex<u32>>,
    }
    impl WatchFactory for One {
        fn start(&self, _: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
            *self.starts.lock().expect("starts") += 1;
            Some(Box::new(Rec {
                log: Arc::clone(&self.log),
                tag: self.tag,
                interrupt: self.interrupt,
            }))
        }
    }
    struct NoneF;
    impl WatchFactory for NoneF {
        fn start(&self, _: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
            None
        }
    }
    let log = Arc::new(Mutex::new(Vec::new()));
    let starts = Arc::new(Mutex::new(0u32));
    let mk = |tag, interrupt| {
        Arc::new(One {
            log: Arc::clone(&log),
            tag,
            interrupt,
            starts: Arc::clone(&starts),
        })
    };
    let factories: Vec<Arc<dyn WatchFactory>> =
        vec![mk("first", false), Arc::new(NoneF), mk("second", true)];
    let turn = TurnInfo::new(SessionId::new_v7(), TurnId::new(std::num::NonZeroU64::MIN));
    let mut watchers = start_watchers(&factories, &turn);
    assert_eq!(
        *starts.lock().expect("starts"),
        2,
        "each returning factory starts exactly once"
    );
    assert_eq!(watchers.len(), 2, "one watcher per returning factory");
    let verdict = feed_watchers(&mut watchers, &dal_core::Channel::Text, "d");
    assert_eq!(
        *log.lock().expect("log"),
        vec!["first", "second"],
        "deltas visit watchers inline in start order"
    );
    assert!(
        matches!(verdict, dal_core::StreamVerdict::Interrupt { .. }),
        "first interrupt wins"
    );
}

#[tokio::test(start_paused = true)]
async fn precancelled_ready_allow_still_denies() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cx = cx(&caller, &services, &cancel);
    let hooks: Vec<Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>> = vec![Arc::new(ReadyAllowHook)];
    let step = dispatch_tool_call(
        "test",
        &cx,
        &hooks,
        &tool_event(r#"{"a":1}"#),
        RawJson::parse(r#"{"a":1}"#).expect("args"),
    )
    .await;
    assert!(
        step.block.is_some(),
        "pre-cancelled guard must fail closed even when the hook is ready to allow"
    );

    let allow = |_event: ToolCallEvent| -> BoxFuture<'static, Approval> {
        Box::pin(async { Approval::Allow })
    };
    let decision = approve_tool_call(
        &tool_event(r#"{"a":1}"#),
        &allow,
        &cancel,
        Instant::now() + Duration::from_secs(3600),
        5_000,
    )
    .await;
    assert!(
        matches!(&decision, ToolDecision::Denied),
        "pre-cancelled approval must deny even when the approver is ready to allow, got {decision:?}"
    );
}
struct FailRequestHook;

impl Hook<BeforeRequest, Option<RequestParams>> for FailRequestHook {
    fn call(
        &self,
        _input: BeforeRequest,
        _cx: crate::ext::HookCx,
    ) -> BoxFuture<'static, Result<Option<RequestParams>, HookError>> {
        Box::pin(async move {
            Err(HookError::Failed {
                message: "boom".into(),
            })
        })
    }
}

#[tokio::test]
async fn before_request_clamp_cap() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let caps = Caps {
        context_window: Some(4096),
        thinking: Box::new([dal_core::ThinkingLevel::Low]),
        tool_use: false,
        image_input: false,
        custom_grammar: false,
    };
    let params = RequestParams {
        thinking: dal_core::ThinkingLevel::High,
        ..RequestParams::default()
    };
    let event = BeforeRequest {
        turn: TurnId::new(std::num::NonZeroU64::MIN),
        round: 0,
        model: ModelInfo {
            route: ModelRoute::Synthetic {
                id: "acme/test".into(),
            },
            name: "test".into(),
            provider: "".into(),
            caps: caps.clone(),
        },
        caps: caps.clone(),
        params: params.clone(),
        thinking_explicit: false,
    };
    // Error half: an erroring hook keeps the running params with exactly
    // one hook_failed notice (ext/hooks.rs dispatch_before_request).
    let failing: Vec<Arc<dyn Hook<BeforeRequest, Option<RequestParams>>>> =
        vec![Arc::new(FailRequestHook)];
    let step = dispatch_before_request("test", &cx, &failing, &event, params.clone()).await;
    assert_eq!(
        step.params.thinking,
        dal_core::ThinkingLevel::High,
        "a failed before_request hook keeps the running params"
    );
    assert_eq!(
        step.notices.len(),
        1,
        "one erroring hook pushes exactly one hook_failed notice"
    );
    assert_eq!(
        step.notices[0].as_ref(),
        "plugin \"test\" hook before_request failed; before_request ignored",
        "the notice names the extension, event, and ignored outcome"
    );
    // Clamp half: the stub hook requests High on a Low-cap model; dispatch
    // carries High untouched and the caller-side clamp lowers it to Low.
    // A clamp that returns its input keeps High and fails this test.
    let hot: Vec<Arc<dyn Hook<BeforeRequest, Option<RequestParams>>>> = vec![Arc::new(HotHook)];
    let over = RequestParams {
        thinking: dal_core::ThinkingLevel::Low,
        ..RequestParams::default()
    };
    let over_event = BeforeRequest {
        params: over.clone(),
        ..event
    };
    let hot_step = dispatch_before_request("test", &cx, &hot, &over_event, over).await;
    assert_eq!(
        hot_step.params.thinking,
        dal_core::ThinkingLevel::High,
        "the stub hook carries its above-cap request through dispatch"
    );
    assert_eq!(
        clamp_params(hot_step.params, &caps).thinking,
        dal_core::ThinkingLevel::Low,
        "clamping a Low-cap model lowers the High request to Low"
    );
    assert!(
        hot_step.notices.is_empty(),
        "a successful hook pushes no notice"
    );
}
