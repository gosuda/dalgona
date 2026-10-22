//! Cancelling a held hook or approval ends its whole dispatch: no later hook
//! or approver starts, no failure is reported for the cancel, and the held
//! continuation never runs again. Time is paused, so a deadline could never
//! stand in for the cancel.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_core::ext::{BeforeRequest, BeforeTurn};
use dal_core::{
    CallId, Caps, InputEvent, InputVerdict, ModelInfo, ModelRoute, RawJson, RequestParams,
    ToolCallEvent, ToolCallVerdict, ToolResultEvent, TurnId,
};
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{CancelDropGuard, NoSvcs, caller, cx, text_event, tool_event};
use crate::ext::hooks::{
    Approval, ObserverReport, ToolDecision, approve_tool_call, dispatch_before_request,
    dispatch_before_turn, dispatch_input, dispatch_tool_call, dispatch_tool_result,
};
use crate::ext::{BoxFuture, Hook, HookCx, HookError, ObserveHook, Services};

/// Parks every hook future it starts until the dispatch drops it, and reports
/// the first entry. Calls are counted where `call` runs, before any future is
/// polled, so a hook the dispatch merely constructs still counts.
struct ParkHook {
    entered: Mutex<Option<oneshot::Sender<()>>>,
    dropped: Arc<AtomicBool>,
    calls: AtomicUsize,
}

impl ParkHook {
    fn new() -> (Arc<Self>, oneshot::Receiver<()>, Arc<AtomicBool>) {
        let (entered_tx, entered) = oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let hook = Arc::new(Self {
            entered: Mutex::new(Some(entered_tx)),
            dropped: Arc::clone(&dropped),
            calls: AtomicUsize::new(0),
        });
        (hook, entered, dropped)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn enter(&self) -> BoxFuture<'static, ()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let sender = self.entered.lock().expect("entered").take();
        let dropped = Arc::clone(&self.dropped);
        Box::pin(async move {
            let _guard = CancelDropGuard { flag: dropped };
            if let Some(sender) = sender {
                let _ = sender.send(());
            }
            std::future::pending::<()>().await;
        })
    }
}

/// A hook that would answer at once; counted where `call` runs.
#[derive(Default)]
struct CountHook {
    calls: AtomicUsize,
}

impl CountHook {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn enter(&self) -> BoxFuture<'static, ()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(()))
    }
}

macro_rules! guard_hooks {
    ($ty:ty; $($event:ty => $out:ty = $ok:expr),+ $(,)?) => {$(
        impl Hook<$event, $out> for $ty {
            fn call(
                &self,
                _input: $event,
                _cx: HookCx,
            ) -> BoxFuture<'static, Result<$out, HookError>> {
                let entry = self.enter();
                Box::pin(async move {
                    entry.await;
                    Ok($ok)
                })
            }
        }
    )+};
}

guard_hooks!(ParkHook;
    ToolCallEvent => ToolCallVerdict = ToolCallVerdict::Allow,
    InputEvent => InputVerdict = InputVerdict::Continue,
    BeforeTurn => Option<String> = None,
    BeforeRequest => Option<RequestParams> = None,
);
guard_hooks!(CountHook;
    ToolCallEvent => ToolCallVerdict = ToolCallVerdict::Allow,
    InputEvent => InputVerdict = InputVerdict::Continue,
    BeforeTurn => Option<String> = None,
    BeforeRequest => Option<RequestParams> = None,
);

macro_rules! observe_hooks {
    ($ty:ty) => {
        impl ObserveHook<ToolResultEvent> for $ty {
            fn call(
                &self,
                _input: ToolResultEvent,
                _cx: HookCx,
            ) -> BoxFuture<'static, Result<(), HookError>> {
                let entry = self.enter();
                Box::pin(async move {
                    entry.await;
                    Ok(())
                })
            }
        }
    };
}

observe_hooks!(ParkHook);
observe_hooks!(CountHook);

/// Drives `dispatch` until the held hook has entered, cancels the turn, and
/// returns the dispatch result. The clock must not move: only the cancel arm
/// may end the held hook.
async fn cancel_while_held<T>(
    dispatch: impl Future<Output = T>,
    mut entered: oneshot::Receiver<()>,
    cancel: &CancellationToken,
) -> T {
    let mut dispatch = Box::pin(dispatch);
    tokio::select! {
        _ = &mut dispatch => panic!("dispatch ended before the held hook entered"),
        _ = &mut entered => {},
    }
    let start = Instant::now();
    cancel.cancel();
    let settled = dispatch.await;
    assert_eq!(
        Instant::now(),
        start,
        "the cancel branch, not a hook deadline, must end the held hook"
    );
    settled
}

fn raw(args: &str) -> RawJson {
    RawJson::parse(args).expect("args")
}

#[tokio::test(start_paused = true)]
async fn cancelled_tool_call_chain_starts_no_later_hook() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let (park, entered, dropped) = ParkHook::new();
    let later = Arc::new(CountHook::default());
    let hooks: Vec<Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>> =
        vec![park.clone(), later.clone()];
    let event = tool_event(r#"{"a":1}"#);
    let step = cancel_while_held(
        dispatch_tool_call("test", &cx, &hooks, &event, raw(r#"{"a":1}"#)),
        entered,
        &cancel,
    )
    .await;
    assert!(step.block.is_some(), "a cancelled guard must fail closed");
    assert!(
        dropped.load(Ordering::SeqCst),
        "the held hook future must be dropped"
    );
    let again = dispatch_tool_call("test", &cx, &hooks, &event, raw(r#"{"a":1}"#)).await;
    assert!(again.block.is_some(), "a later dispatch stays fail-closed");
    assert_eq!(
        (park.calls(), later.calls()),
        (1, 0),
        "no hook may start once the turn is cancelled"
    );
}

#[tokio::test(start_paused = true)]
async fn cancelled_input_chain_starts_no_later_hook_and_reports_no_failure() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let (park, entered, dropped) = ParkHook::new();
    let later = Arc::new(CountHook::default());
    let hooks: Vec<Arc<dyn Hook<InputEvent, InputVerdict>>> = vec![park.clone(), later.clone()];
    let event = text_event("hi");
    let step = cancel_while_held(
        dispatch_input("test", &cx, false, &hooks, &event),
        entered,
        &cancel,
    )
    .await;
    assert!(
        step.notices.is_empty(),
        "a cancel is not a hook failure: {:?}",
        step.notices
    );
    assert!(!step.stop, "a cancelled chain handles nothing");
    assert_eq!(step.content, event.content, "the content stays unchanged");
    assert!(dropped.load(Ordering::SeqCst));
    let again = dispatch_input("test", &cx, false, &hooks, &event).await;
    assert!(again.notices.is_empty(), "{:?}", again.notices);
    assert_eq!(
        (park.calls(), later.calls()),
        (1, 0),
        "no hook may start once the turn is cancelled"
    );
}

#[tokio::test(start_paused = true)]
async fn cancelled_before_turn_chain_starts_no_later_hook_and_reports_no_failure() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let (park, entered, dropped) = ParkHook::new();
    let later = Arc::new(CountHook::default());
    let hooks: Vec<Arc<dyn Hook<BeforeTurn, Option<String>>>> = vec![park.clone(), later.clone()];
    let event = BeforeTurn {
        turn: TurnId::new(std::num::NonZeroU64::MIN),
        text: "prompt".into(),
    };
    let step = cancel_while_held(
        dispatch_before_turn("test", &cx, &hooks, &event),
        entered,
        &cancel,
    )
    .await;
    assert!(
        step.notices.is_empty(),
        "a cancel is not a hook failure: {:?}",
        step.notices
    );
    assert_eq!(step.texts, [] as [Box<str>; 0]);
    assert!(dropped.load(Ordering::SeqCst));
    let again = dispatch_before_turn("test", &cx, &hooks, &event).await;
    assert!(again.notices.is_empty(), "{:?}", again.notices);
    assert_eq!((park.calls(), later.calls()), (1, 0));
}

#[tokio::test(start_paused = true)]
async fn cancelled_before_request_chain_starts_no_later_hook_and_reports_no_failure() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let (park, entered, dropped) = ParkHook::new();
    let later = Arc::new(CountHook::default());
    let hooks: Vec<Arc<dyn Hook<BeforeRequest, Option<RequestParams>>>> =
        vec![park.clone(), later.clone()];
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
        caps,
        params: params.clone(),
        thinking_explicit: false,
    };
    let step = cancel_while_held(
        dispatch_before_request("test", &cx, &hooks, &event, params.clone()),
        entered,
        &cancel,
    )
    .await;
    assert!(
        step.notices.is_empty(),
        "a cancel is not a hook failure: {:?}",
        step.notices
    );
    assert_eq!(
        step.params, params,
        "a cancelled chain keeps the parameters"
    );
    assert!(dropped.load(Ordering::SeqCst));
    let again = dispatch_before_request("test", &cx, &hooks, &event, params).await;
    assert!(again.notices.is_empty(), "{:?}", again.notices);
    assert_eq!((park.calls(), later.calls()), (1, 0));
}

#[tokio::test(start_paused = true)]
async fn cancelled_observer_chain_starts_no_later_observer_and_counts_no_failure() {
    let caller = caller();
    let services: Arc<dyn Services> = Arc::new(NoSvcs);
    let cancel = CancellationToken::new();
    let cx = cx(&caller, &services, &cancel);
    let (park, entered, dropped) = ParkHook::new();
    let later = Arc::new(CountHook::default());
    let hooks: Vec<Arc<dyn ObserveHook<ToolResultEvent>>> = vec![park.clone(), later.clone()];
    let event = ToolResultEvent {
        turn: TurnId::new(std::num::NonZeroU64::MIN),
        call: CallId::new("call-1"),
        tool: "demo".parse().expect("tool"),
        ok: true,
        preview: "".into(),
    };
    let mut report = ObserverReport::default();
    cancel_while_held(
        dispatch_tool_result("test", &cx, &hooks, &event, &mut report),
        entered,
        &cancel,
    )
    .await;
    assert_eq!(
        report,
        ObserverReport::default(),
        "a cancel is not an observer failure"
    );
    assert!(dropped.load(Ordering::SeqCst));
    dispatch_tool_result("test", &cx, &hooks, &event, &mut report).await;
    assert_eq!((park.calls(), later.calls()), (1, 0));
}

#[tokio::test(start_paused = true)]
async fn precancelled_approval_never_starts_the_approver() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let started = Arc::new(AtomicUsize::new(0));
    let approver = |_event: ToolCallEvent| -> BoxFuture<'static, Approval> {
        started.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Approval::Allow })
    };
    let start = Instant::now();
    let decision = approve_tool_call(
        &tool_event(r#"{"a":1}"#),
        &approver,
        &cancel,
        start + Duration::from_secs(3600),
        5_000,
    )
    .await;
    assert!(
        matches!(&decision, ToolDecision::Denied),
        "pre-cancelled approval must deny, got {decision:?}"
    );
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "a cancelled turn must not open an approval it can never wait for"
    );
}
