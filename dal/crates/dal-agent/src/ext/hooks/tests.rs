//! Hook composition, deadline, observer, and watcher tests.

use std::collections::BTreeMap;
use std::path::PathBuf;
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
    JobsReply, ModelRequest, Notice, Question, RunOutput, RunRequest, SidecarOp, TurnOp,
    TurnOpReply,
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
    let far = Instant::now() + Duration::from_secs(3600);
    assert!(effective_deadline(far, 5_000) <= far);
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
