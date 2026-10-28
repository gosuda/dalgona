//! Mailbox coverage through the member mailbox (`task.send`).
//!
//! Every Starlark line here runs through the real evaluator against a host
//! double: the mode vocabulary, the outside-session denial, and the
//! `Full`/`Gone` receipt spellings all travel the production send path.
//! The receive half pins the `Recv` wire and the cursor-advance rule over
//! journaled mail: a read after a cursor returns exactly the later mail
//! with the cursor for the next read.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
#[path = "support/host.rs"]
pub mod host;
pub mod support;

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use dal_agent::ext::Extension;
use dal_agent::ext::generation::catalog::Catalog;
use dal_agent::ext::script::{
    CancelTarget, Cleanup, Collect, Collected, Entry, EvalEnvironment, HostTerminal, Invocation,
    OpFailure, OpOutcome, OpRequest, Parent, ScopeId, ScriptCx, ScriptHost, Submit, TaskId,
};
use dal_agent::ext::tool::{ToolCall, ToolOutcome};
use dal_core::ext::{Mail, OpSet};
use dal_core::{
    AgentsOp, AgentsOpError, AgentsReply, EntryId, GenerationId, MailMode, Part, RawJson, ReadView,
    Receipt, ScopeSpec, SessionId,
};
use dal_star::{CELL_LIMITS, eval_extension};
use host::{RecordingHost, tool_cx_approved};

const SCOPE_ALL: &str = include_str!("fixtures/mailbox/scope_all.star");
const TASK_SEND: &str = include_str!("fixtures/mailbox/task_send.star");
const TASK_SEND_BAD_MODE: &str = include_str!("fixtures/mailbox/task_send_bad_mode.star");

/// A host double that answers member sends from a scripted queue and
/// delegates every other facet to the recording host.
///
/// The queue holds the receipts production would return for saturated or
/// gone mailboxes, so the Starlark `task.send` spelling travels the real
/// receipt mapping. An empty queue behaves like the default host: sends
/// outside a session are denied.
struct ScriptedMailboxHost {
    inner: Arc<RecordingHost>,
    receipts: Mutex<VecDeque<Receipt>>,
    environment: Arc<EvalEnvironment>,
}

impl ScriptedMailboxHost {
    fn with_receipts(extensions: &[Extension], receipts: Vec<Receipt>) -> Arc<Self> {
        let inner = RecordingHost::new(extensions, OpSet::EMPTY, []);
        // Mirrors the environment `RecordingHost::new` builds for the same
        // empty ceiling; a future shared accessor should replace this copy.
        let environment = EvalEnvironment::capture(
            GenerationId::new(NonZeroU64::MIN),
            OpSet::EMPTY,
            None,
            [0; 32],
        )
        .expect("test environment is below the capture limit");
        Arc::new(Self {
            inner,
            receipts: Mutex::new(receipts.into()),
            environment: Arc::new(environment),
        })
    }

    fn script_cx(self: &Arc<Self>) -> ScriptCx {
        ScriptCx::new(
            Arc::clone(self) as Arc<dyn ScriptHost>,
            Arc::clone(&self.environment),
            None,
        )
    }

    fn next_receipt(&self) -> Option<Receipt> {
        self.receipts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }
}

impl ScriptHost for ScriptedMailboxHost {
    fn begin(&self, parent: Parent<'_>, entry: Entry) -> Result<Arc<Invocation>, HostTerminal> {
        self.inner.begin(parent, entry)
    }

    fn call(
        &self,
        invocation: &Arc<Invocation>,
        request: OpRequest,
    ) -> dal_agent::ext::BoxFuture<'static, OpOutcome> {
        self.inner.call(invocation, request)
    }

    fn open_scope(
        &self,
        invocation: &Arc<Invocation>,
        spec: ScopeSpec,
    ) -> Result<ScopeId, HostTerminal> {
        self.inner.open_scope(invocation, spec)
    }

    fn submit(&self, invocation: &Arc<Invocation>, scope: ScopeId, request: OpRequest) -> Submit {
        self.inner.submit(invocation, scope, request)
    }

    fn collect(
        &self,
        invocation: &Arc<Invocation>,
        which: Collect,
    ) -> dal_agent::ext::BoxFuture<'static, Result<Collected, HostTerminal>> {
        self.inner.collect(invocation, which)
    }

    fn cancel(&self, invocation: &Arc<Invocation>, target: CancelTarget) {
        self.inner.cancel(invocation, target);
    }

    fn adopt(&self, invocation: &Arc<Invocation>, reference: &str) -> Result<ReadView, OpFailure> {
        self.inner.adopt(invocation, reference)
    }

    fn catalog(&self) -> &Catalog {
        self.inner.catalog()
    }

    fn finish(&self, invocation: &Arc<Invocation>) -> dal_agent::ext::BoxFuture<'static, Cleanup> {
        self.inner.finish(invocation)
    }

    fn abandon(&self, invocation: &Arc<Invocation>) {
        self.inner.abandon(invocation);
    }

    fn send_member(
        &self,
        invocation: &Arc<Invocation>,
        task: TaskId,
        text: &str,
        mode: MailMode,
    ) -> dal_agent::ext::BoxFuture<'static, Result<Receipt, HostTerminal>> {
        if let Some(receipt) = self.next_receipt() {
            return Box::pin(async move { Ok(receipt) });
        }
        self.inner.send_member(invocation, task, text, mode)
    }
}

async fn run_fixture(code: &str) -> ToolOutcome {
    run_scripted(code, Vec::new()).await
}

async fn run_scripted(code: &str, receipts: Vec<Receipt>) -> ToolOutcome {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let tool = extension
        .tools()
        .iter()
        .find(|(tool, _)| tool.name().as_str() == "eval")
        .map(|(tool, _)| Arc::clone(tool))
        .expect("eval tool registered");
    let host = ScriptedMailboxHost::with_receipts(std::slice::from_ref(&extension), receipts);
    let args = RawJson::parse(&host_cell_args(code)).expect("cell args");
    tool.run(
        ToolCall::new("test-call", args),
        tool_cx_approved(host.script_cx()),
    )
    .await
}

fn host_cell_args(code: &str) -> String {
    format!(
        "{{\"code\":{},\"uses\":[]}}",
        sonic_rs::to_string(code).expect("cell code JSON")
    )
}

fn ok_text(outcome: &ToolOutcome) -> &str {
    match outcome {
        ToolOutcome::Ok(output) => match &output.parts[0] {
            Part::Text { text } => text,
            other => panic!("expected text part, got {other:?}"),
        },
        other => panic!("expected completed cell, got {other:?}"),
    }
}

fn err_text(outcome: &ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Err(error) => error.to_string(),
        other => panic!("expected failed cell, got {other:?}"),
    }
}

fn entry(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero cursor"))
}

fn mail(from: SessionId, to: SessionId, text: &str) -> Mail {
    Mail {
        from,
        to,
        mode: MailMode::Aside,
        text: text.into(),
        reply_to: None,
    }
}

/// Returns the mail after `after`, with the cursor for the next read.
fn recv_after(mailbox: &[(EntryId, Mail)], after: Option<EntryId>) -> (Vec<Mail>, Option<EntryId>) {
    let after_value = after.map(EntryId::get);
    let selected: Vec<Mail> = mailbox
        .iter()
        .filter(|(id, _)| after_value.is_none_or(|cursor| id.get() > cursor))
        .map(|(_, item)| item.clone())
        .collect();
    let next = mailbox.last().map(|(id, _)| *id).or(after);
    (selected, next)
}

/// Send and cursor advance over the Mail service.
///
/// The send half runs through Starlark and breaks if the member send stops
/// reaching the host mailbox or the mode vocabulary changes. The cursor
/// half is a mirror, not the pager itself: it pins the `Recv` wire and the
/// advance rule the session pager owns, so a Starlark `recv` projection
/// remains uncovered until the owning adapter change exposes it.
#[tokio::test(flavor = "multi_thread")]
async fn mailbox_send_recv_cursor() {
    // Starlark half: an empty scope seals and settles through the real host.
    let outcome = run_fixture(SCOPE_ALL).await;
    let text = ok_text(&outcome);
    assert!(text.contains("\"status\":\"completed\""), "{text}");
    assert!(text.contains("\"value\":[]"), "{text}");

    // Starlark half: a member send outside a session is denied, not dropped.
    let outcome = run_fixture(TASK_SEND).await;
    let text = err_text(&outcome);
    assert!(
        text.contains("member sessions outside a session"),
        "member send must surface its denial: {text}"
    );

    // Service half: three journaled mails read back by cursor.
    let peer = SessionId::new_v7();
    let self_id = SessionId::new_v7();
    let mailbox: Vec<(EntryId, Mail)> = [1, 2, 3]
        .into_iter()
        .map(|position| {
            (
                entry(position),
                mail(peer, self_id, &format!("n{position}")),
            )
        })
        .collect();

    let (first, next) = recv_after(&mailbox, None);
    assert_eq!(first.len(), 3, "an empty cursor reads everything");
    assert_eq!(next, Some(entry(3)));
    let received = AgentsReply::Received { mail: first, next };
    let AgentsReply::Received { mail, next } = received else {
        panic!("expected a received batch");
    };
    assert_eq!(mail.len(), 3);
    assert_eq!(next, Some(entry(3)));

    let (tail, next) = recv_after(&mailbox, Some(entry(1)));
    assert_eq!(tail.len(), 2, "a cursor skips what was already read");
    assert_eq!(tail[0].text.as_ref(), "n2");
    assert_eq!(next, Some(entry(3)));

    let (empty, next) = recv_after(&mailbox, Some(entry(3)));
    assert!(empty.is_empty(), "a current cursor reads nothing new");
    assert_eq!(next, Some(entry(3)));

    // The send operation itself carries its cursor for threading replies.
    let send = AgentsOp::Send {
        to: peer,
        text: "go".into(),
        mode: MailMode::Aside,
        reply_to: Some(entry(41)),
    };
    let wire = sonic_rs::to_string(&send).expect("send wire");
    assert_eq!(
        sonic_rs::from_str::<AgentsOp>(&wire).expect("send round trip"),
        send
    );
    let recv = AgentsOp::Recv {
        after: Some(entry(1)),
        timeout: None,
    };
    let wire = sonic_rs::to_string(&recv).expect("recv wire");
    assert_eq!(
        sonic_rs::from_str::<AgentsOp>(&wire).expect("recv round trip"),
        recv
    );
}

/// The saturated mailbox states on the Mail service.
///
/// Breaks if the `Full`/`Gone` receipts stop spelling their wire words, if
/// the send path stops delivering them as receipts, or if the member-send
/// vocabulary drifts: the unknown mode must keep failing at the method, and
/// both receipts must keep their exact surface.
#[tokio::test(flavor = "multi_thread")]
async fn mailbox_full_and_gone() {
    // Starlark half: the mode vocabulary is closed to three words.
    let outcome = run_fixture(TASK_SEND_BAD_MODE).await;
    let text = err_text(&outcome);
    assert!(
        text.contains("unknown mode"),
        "mode vocabulary is closed: {text}"
    );

    // Host half: each saturated receipt travels the production send path
    // and surfaces under its exact spelling.
    for (receipt, spelling) in [(Receipt::Full, "\"full\""), (Receipt::Gone, "\"gone\"")] {
        let outcome = run_scripted(TASK_SEND, vec![receipt]).await;
        let text = ok_text(&outcome);
        assert!(
            text.contains("\"status\":\"completed\""),
            "{spelling} send must complete: {text}"
        );
        assert!(
            text.contains(&format!("\"value\":{spelling}")),
            "{spelling} send must surface its receipt: {text}"
        );
    }

    // Wire half: both receipts spell exactly what the member send returns.
    assert_eq!(
        sonic_rs::to_string(&Receipt::Full).expect("full wire"),
        "\"full\""
    );
    assert_eq!(
        sonic_rs::to_string(&Receipt::Gone).expect("gone wire"),
        "\"gone\""
    );
    assert_eq!(
        sonic_rs::from_str::<Receipt>("\"full\"").expect("full word"),
        Receipt::Full
    );
    assert_eq!(
        sonic_rs::from_str::<Receipt>("\"gone\"").expect("gone word"),
        Receipt::Gone
    );

    // Capacity and liveness failures keep their typed reasons.
    assert_eq!(AgentsOpError::Full.to_string(), "agent capacity is full",);
    assert_eq!(AgentsOpError::Gone.to_string(), "agent session is gone",);
}
