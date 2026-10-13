use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use dal_agent::error::{DenyReason, ServiceError};
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, HookCx, ObserveHook, RawValue, Services, Tool, ToolCall, ToolCx, ToolOutcome,
};
use dal_core::ext::{McpRequest, McpResponse};
use dal_core::{
    AgentInfo, AgentReport, AgentState, AgentsOp, AgentsReply, Answer, EntryId, FetchRequest,
    FetchResponse, Inference, JobsOp, JobsReply, ModelRequest, Name, Question, RunOutput,
    RunRequest, Service, SidecarOp, StateError, StateOp, StateRecord, Stop, TurnOp, TurnOpReply,
};
use dal_provider::EventStream;

use super::action::{decode_action, validate_action};
use super::child::{child_id, child_tool_names, child_workspace, start_child};
use super::report::{bounded_report, report_text, report_uri};
use super::view::list_children;
use super::*;

fn entry(sequence: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(sequence).expect("nonzero entry"))
}

fn model() -> dal_core::ModelInfo {
    dal_core::ModelInfo {
        route: dal_core::ModelRoute::from_id("dalgon/normal"),
        name: "test".into(),
        caps: dal_core::Caps {
            context_window: None,
            thinking: Box::new([]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
    }
}

fn workspace() -> dal_core::Workspace {
    dal_core::Workspace::new(std::env::temp_dir()).expect("absolute workspace")
}

/// Scripted `Services` double: queued agents replies plus a notify log.
#[derive(Default)]
struct FakeServices {
    replies: Mutex<VecDeque<Result<AgentsReply, ServiceError>>>,
    notices: Mutex<Vec<dal_core::Notice>>,
}

impl FakeServices {
    fn with(replies: Vec<Result<AgentsReply, ServiceError>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            notices: Mutex::new(Vec::new()),
        })
    }
}

/// Views a fake as the capability-scoped trait object contexts require.
fn share(fake: &Arc<FakeServices>) -> Arc<dyn Services> {
    fake.clone()
}

impl Services for FakeServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Err(ServiceError::Declined) })
    }
    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        Box::pin(async { Err(ServiceError::Declined) })
    }
    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        Box::pin(async { Ok(None) })
    }

    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        Box::pin(async { Ok(None) })
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(
            std::sync::Arc<dyn dal_agent::ext::Tool>,
            dal_core::Visibility,
        )>,
    ) -> ServiceFuture<'_, ()> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn mcp_declarations(
        &self,
        _who: &Caller,
    ) -> ServiceFuture<'_, Vec<dal_core::ext::McpDeclaration>> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        let next = self
            .replies
            .lock()
            .expect("replies")
            .pop_front()
            .expect("scripted agents reply");
        Box::pin(async move { next })
    }

    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(None) })
    }

    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        Box::pin(async { Ok(Err(StateError::Unavailable)) })
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        Box::pin(async { Err(ServiceError::Declined) })
    }

    fn notify(&self, _who: &Caller, notice: dal_core::Notice) {
        self.notices.lock().expect("notices").push(notice);
    }

    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        Box::pin(async { Ok(entry(99)) })
    }

    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

fn agent() -> AgentTool {
    AgentTool::new().expect("agent tool builds")
}

fn call(json: &str) -> ToolCall {
    let raw = dal_core::RawJson::parse(json).expect("test args parse");
    ToolCall::new("call-1", raw)
}

fn outcome_text(outcome: ToolOutcome) -> Result<String, String> {
    match outcome {
        ToolOutcome::Ok(output) => Ok(output.to_string()),
        ToolOutcome::Err(error) => Err(error.to_string()),
        ToolOutcome::Interrupted => Err("interrupted".into()),
        ToolOutcome::Detached(job) => Err(format!("detached {job}")),
    }
}

fn info(id: dal_core::SessionId, name: &str, state: AgentState) -> AgentInfo {
    AgentInfo {
        id,
        name: name.into(),
        state,
    }
}

#[test]
fn strict_action_decode_rejects_unknown_fields() {
    assert!(decode_action(r#"{"action":"list","extra":true}"#).is_err());
    assert!(decode_action(r#"{"action":"spawn","name":"w","prompt":"t","extra":true}"#).is_err());
    assert!(decode_action(r#"{"action":"wait","id":"x","extra":true}"#).is_err());
    assert!(decode_action(r#"{"action":"cancel","id":"x","extra":true}"#).is_err());
}

#[test]
fn schema_property_names_match_the_decoder_allowlist() {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let schema: sonic_rs::Value = sonic_rs::from_str(super::AGENT_SCHEMA).expect("schema parses");
    let variants = schema
        .get("oneOf")
        .and_then(|one| one.as_array())
        .expect("oneOf array");
    assert_eq!(variants.len(), 4);
    for variant in variants {
        let tag = variant
            .get("properties")
            .and_then(|properties| properties.get("action"))
            .and_then(|action| action.get("const"))
            .and_then(|name| name.as_str())
            .expect("action tag");
        let properties = variant
            .get("properties")
            .and_then(|properties| properties.as_object())
            .expect("properties");
        let mut keys: Vec<&str> = properties.iter().map(|(key, _)| key).collect();
        keys.sort_unstable();
        let mut allowed: Vec<&str> = super::action::allowed_keys(tag)
            .expect("known tag")
            .to_vec();
        allowed.sort_unstable();
        assert_eq!(keys, allowed, "schema and decoder agree on {tag}");
    }
}

#[test]
fn strict_action_decode_rejects_wrong_types() {
    assert!(
        decode_action(r#"{"action":"spawn","name":"worker","prompt":"task","tools":{}}"#).is_err()
    );
}

#[test]
fn spawn_rejects_role_and_system_before_a_service_call() {
    let action = decode_action(
        r#"{"action":"spawn","name":"worker","prompt":"task","role":"reviewer","system":"override"}"#,
    )
    .expect("action decodes");
    let error = validate_action(&action).expect_err("role and system conflict");
    assert_eq!(
        error.to_string(),
        "agents: role and system cannot both be set."
    );
}

#[test]
fn spawn_allowlist_distinguishes_empty_from_omitted_and_null() {
    let omitted = decode_action(r#"{"action":"spawn","name":"w","prompt":"t"}"#).expect("decode");
    let nulled = decode_action(r#"{"action":"spawn","name":"w","prompt":"t","tools":null}"#)
        .expect("decode");
    let empty =
        decode_action(r#"{"action":"spawn","name":"w","prompt":"t","tools":[]}"#).expect("decode");
    for action in [omitted, nulled] {
        let AgentAction::Spawn { tools, .. } = action else {
            panic!("expected spawn");
        };
        assert_eq!(tools, None);
    }
    let AgentAction::Spawn { tools, .. } = empty else {
        panic!("expected spawn");
    };
    assert_eq!(tools, Some(Vec::new()));
}

#[test]
fn child_tools_default_to_read_search_and_empty_stays_empty() {
    let defaults = child_tool_names(None).expect("defaults");
    assert_eq!(
        defaults.iter().map(Name::as_str).collect::<Vec<_>>(),
        DEFAULT_TOOLS
    );
    assert!(
        child_tool_names(Some(Vec::new()))
            .expect("empty")
            .is_empty()
    );
}

#[test]
fn child_tool_names_accept_mapped_names() {
    let names = child_tool_names(Some(vec!["deploy.web-x.list".into()])).expect("mapped tool");
    assert_eq!(names[0].as_str(), "deploy.web-x.list");
}

#[test]
fn invalid_child_tool_name_returns_the_contract_error() {
    let error = child_tool_names(Some(vec!["bad name".into()])).expect_err("bad name");
    assert_eq!(
        error.to_string(),
        "unknown tool \"bad name\" in agents.tools"
    );
}

#[test]
fn child_workspace_rejects_relative_paths() {
    let error = child_workspace(Some("relative".into())).expect_err("relative");
    assert_eq!(error.to_string(), "workspace path must be absolute");
}

#[test]
fn report_at_limit_keeps_every_unicode_scalar() {
    let report = "🧪".repeat(MAX_REPORT_CHARS);
    assert_eq!(bounded_report(report.clone(), "session://child/1"), report);
}

#[test]
fn report_spill_truncates_on_scalar_boundary_and_links_original_entry() {
    let mut report = "a".repeat(MAX_REPORT_CHARS - 1);
    report.push('🧪');
    report.push_str("tail");
    let prefix = format!("{}🧪", "a".repeat(MAX_REPORT_CHARS - 1));
    let uri = "session://child/7";
    let expected = format!(
        "{prefix}\n[report truncated at 50000 characters; full report: {uri}]\nnext_step: Read the full report with the read tool at {uri}."
    );
    assert_eq!(bounded_report(report, uri), expected);
}

#[test]
fn start_operation_uses_parent_call_and_default_tools() {
    let operation = start_child(
        dal_core::CallId::new("call-17"),
        "worker",
        "inspect the change".into(),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("start");
    let AgentsOp::Start(start) = operation else {
        panic!("expected start");
    };
    assert_eq!(start.call.as_str(), "call-17");
    assert_eq!(start.name.as_ref(), "worker");
    let tools = start.tools.expect("default tools");
    assert_eq!(
        tools.iter().map(Name::as_str).collect::<Vec<_>>(),
        DEFAULT_TOOLS
    );
}

#[test]
fn child_list_preserves_order_and_terminal_state_words() {
    let children = [
        info(dal_core::SessionId::new_v7(), "queued", AgentState::Queued),
        info(
            dal_core::SessionId::new_v7(),
            "running",
            AgentState::Running,
        ),
        info(
            dal_core::SessionId::new_v7(),
            "done",
            AgentState::Done(Stop::EndTurn),
        ),
        info(
            dal_core::SessionId::new_v7(),
            "failed",
            AgentState::Done(Stop::Failed),
        ),
        info(
            dal_core::SessionId::new_v7(),
            "cancelled",
            AgentState::Done(Stop::Cancelled),
        ),
    ];
    let expected = format!(
        "{} queued queued\n{} running running\n{} done done\n{} failed failed\n{} failed cancelled",
        children[0].id, children[1].id, children[2].id, children[3].id, children[4].id
    );
    assert_eq!(list_children(&children).expect("list"), expected);
    assert_eq!(list_children(&[]).expect("empty"), "No child sessions.");
}

#[test]
fn invalid_child_id_returns_the_named_error() {
    let error = child_id("not-a-session").expect_err("bad id");
    assert_eq!(
        error.to_string(),
        "agents: unknown child \"not-a-session\"."
    );
}

#[test]
fn report_text_uses_the_child_journal_entry_uri() {
    let session = dal_core::SessionId::new_v7();
    let text = format!("{}tail", "a".repeat(MAX_REPORT_CHARS));
    let report = AgentReport {
        stop: Stop::EndTurn,
        text: text.into_boxed_str(),
        session,
        entry: entry(7),
    };
    let uri = report_uri(&session, entry(7));
    let expected = format!(
        "{}\n[report truncated at 50000 characters; full report: {uri}]\nnext_step: Read the full report with the read tool at {uri}.",
        "a".repeat(MAX_REPORT_CHARS)
    );
    assert_eq!(report_text(report), expected);
}

#[test]
fn tool_registers_with_exact_schema_and_other_class() {
    let tool = agent();
    assert_eq!(tool.name().as_str(), "agent");
    let spec = tool.spec(&model());
    assert!(dal_core::valid_tool_parameters(&spec.parameters));
    assert!(matches!(
        tool.classify(
            &dal_core::RawJson::parse(r#"{"action":"list"}"#).expect("args"),
            &workspace()
        ),
        Ok(dal_core::ToolClass::Other)
    ));
    assert!(
        tool.classify(
            &dal_core::RawJson::parse(
                r#"{"action":"spawn","name":"w","prompt":"t","role":"r","system":"s"}"#
            )
            .expect("args"),
            &workspace()
        )
        .expect_err("role and system")
        .to_string()
        .contains("role and system")
    );
}

#[test]
fn extension_registers_one_model_tool_with_agents_injection() {
    let built = extension().expect("extension builds");
    assert_eq!(built.name(), "subagent");
    assert_eq!(built.tools().len(), 1);
    assert!(built.inject().contains(dal_core::Service::Agents));
}

#[tokio::test]
async fn spawn_returns_the_exact_success_text() {
    let id = dal_core::SessionId::new_v7();
    let services = FakeServices::with(vec![Ok(AgentsReply::Started { id })]);
    let outcome = agent()
        .run(
            call(r#"{"action":"spawn","name":"worker","prompt":"inspect"}"#),
            ToolCx::for_test(share(&services)),
        )
        .await;
    assert_eq!(
        outcome_text(outcome).expect("spawn succeeds"),
        format!(
            "spawned child worker ({id}); call agent with action=\"wait\" and id=\"{id}\" for its report."
        )
    );
}

#[tokio::test]
async fn a_refused_spawn_shows_the_model_the_exact_reason() {
    let services = FakeServices::with(vec![Ok(AgentsReply::Refused {
        reason: dal_core::AgentRefusal::MaxDepth { max_depth: 1 },
    })]);
    let outcome = agent()
        .run(
            call(r#"{"action":"spawn","name":"worker","prompt":"inspect"}"#),
            ToolCx::for_test(share(&services)),
        )
        .await;
    assert_eq!(
        outcome_text(outcome).expect_err("a refused spawn is an error"),
        "child sessions cannot start children here: agents.max_depth = 1."
    );
}

#[tokio::test]
async fn wait_returns_the_report_and_publishes_the_completion_notice() {
    let id = dal_core::SessionId::new_v7();
    let services = FakeServices::with(vec![
        Ok(AgentsReply::Await {
            report: AgentReport {
                stop: Stop::EndTurn,
                text: "done".into(),
                session: id,
                entry: entry(3),
            },
        }),
        Ok(AgentsReply::Listed(vec![info(
            id,
            "worker",
            AgentState::Done(Stop::EndTurn),
        )])),
    ]);
    let outcome = agent()
        .run(
            call(&format!(r#"{{"action":"wait","id":"{id}"}}"#)),
            ToolCx::for_test(share(&services)),
        )
        .await;
    assert_eq!(outcome_text(outcome).expect("wait succeeds"), "done");
    let notices = services.notices.lock().expect("notices");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].text.as_ref(), "child worker finished (end_turn)");
}

#[tokio::test]
async fn cancel_returns_the_exact_text_and_is_idempotent() {
    let id = dal_core::SessionId::new_v7();
    let services = FakeServices::with(vec![
        Ok(AgentsReply::Cancelled { id }),
        Ok(AgentsReply::Cancelled { id }),
    ]);
    for _ in 0..2 {
        let outcome = agent()
            .run(
                call(&format!(r#"{{"action":"cancel","id":"{id}"}}"#)),
                ToolCx::for_test(share(&services)),
            )
            .await;
        assert_eq!(
            outcome_text(outcome).expect("cancel succeeds"),
            format!("cancelled child \"{id}\".")
        );
    }
}

#[tokio::test]
async fn unknown_child_id_maps_to_the_named_error() {
    let missing = dal_core::SessionId::new_v7();
    let services = FakeServices::with(vec![Err(ServiceError::failed(
        Some(Service::Agents),
        "agent session is gone",
    ))]);
    let outcome = agent()
        .run(
            call(&format!(r#"{{"action":"wait","id":"{missing}"}}"#)),
            ToolCx::for_test(share(&services)),
        )
        .await;
    assert_eq!(
        outcome_text(outcome).expect_err("unknown child fails"),
        format!("agents: unknown child \"{missing}\".")
    );
}

#[tokio::test]
async fn nested_start_maps_to_the_depth_error() {
    let services = FakeServices::with(vec![Err(ServiceError::failed(
        Some(Service::Agents),
        "child sessions cannot start children here: agents.max_depth = 1.",
    ))]);
    let outcome = agent()
        .run(
            call(r#"{"action":"spawn","name":"nested","prompt":"deep"}"#),
            ToolCx::for_test(share(&services)),
        )
        .await;
    assert_eq!(
        outcome_text(outcome).expect_err("nested start fails"),
        "subagents cannot spawn subagents (agents.max_depth = 1)"
    );
}

#[tokio::test]
async fn denial_and_cancellation_map_to_typed_outcomes() {
    let denied = FakeServices::with(vec![Err(ServiceError::Denied(DenyReason::NotInjected))]);
    let outcome = agent()
        .run(
            call(r#"{"action":"list"}"#),
            ToolCx::for_test(share(&denied)),
        )
        .await;
    assert!(
        outcome_text(outcome)
            .expect_err("denied")
            .contains("denied")
    );

    let cancelled = FakeServices::with(vec![Err(ServiceError::Cancelled)]);
    let outcome = agent()
        .run(
            call(r#"{"action":"list"}"#),
            ToolCx::for_test(share(&cancelled)),
        )
        .await;
    assert!(matches!(outcome, ToolOutcome::Interrupted));
}

#[tokio::test]
async fn session_end_cancels_only_active_children() {
    let queued = dal_core::SessionId::new_v7();
    let running = dal_core::SessionId::new_v7();
    let done = dal_core::SessionId::new_v7();
    let services = FakeServices::with(vec![
        Ok(AgentsReply::Listed(vec![
            info(queued, "q", AgentState::Queued),
            info(running, "r", AgentState::Running),
            info(done, "d", AgentState::Done(Stop::EndTurn)),
        ])),
        Ok(AgentsReply::Cancelled { id: queued }),
        Ok(AgentsReply::Cancelled { id: running }),
    ]);
    let session = dal_core::SessionId::new_v7();
    CancelChildren
        .call(
            dal_core::SessionEnd {
                session,
                reason: "closed".into(),
            },
            HookCx::for_test(share(&services), session, None),
        )
        .await
        .expect("hook succeeds");
    assert!(
        services.replies.lock().expect("replies").is_empty(),
        "every cancel was consumed and the finished child was left alone"
    );
}
