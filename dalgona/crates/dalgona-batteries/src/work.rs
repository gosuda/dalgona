// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Todo lists and plan review as one session-scoped extension.

use std::sync::Arc;

use dal_agent::error::{SchemeError, ServiceError};
use dal_agent::ext::{
    ArgError, BoxFuture, Caller, CommandCx, CommandHandler, Doc, Extension, ExtensionBuilder, Hook,
    HookCx, HookError, ObserveHook, RawValue, SchemeCx, SchemeResolver, Services, StatusCx,
    StatusPoll, StatusSnapshot, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_core::ext::{
    BeforeTurn, SessionEnd, SessionStart, ToolCallEvent, ToolCallVerdict, Visibility,
};
use dal_core::{
    CommandName, CommandSpec, ModelInfo, Name, Origin, Output, RawJson, RegistrationError, Reply,
    ServiceSet, ToolClass, ToolSpec, Workspace,
};

mod plan;
#[cfg(test)]
mod support;
mod todo;

pub use plan::PlanArgs;
pub(crate) use todo::{TodoItem, load as load_todos, open_todos, todo_all_terminal};

const EXTENSION_NAME: &str = "work";
const TODO_TOOL: &str = "todo";
const PLAN_TOOL: &str = "plan";
const TODO_DESCRIPTION: &str = "Read or replace the complete todo list for this session.";
const PLAN_DESCRIPTION: &str = "Submit a Markdown plan for user review while plan mode is on.";
const TODOS_USAGE: &str = "usage: /todos";

/// Configuration for the plan and todo battery.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanConfig {
    /// Whether the entry product registers this battery.
    pub enabled: bool,
}

impl PlanConfig {
    /// Decodes the strict `[plugin.plan]` section. The embedded default enables the battery.
    ///
    /// # Errors
    /// Returns [`PlanConfigError`] when a present section fails strict decoding.
    pub fn parse_config(section: Option<&toml::Value>) -> Result<Self, PlanConfigError> {
        let Some(section) = section else {
            return Ok(Self { enabled: true });
        };
        section.clone().try_into().map_err(PlanConfigError::from)
    }
}

/// A failure to decode the plan battery's configuration section.
#[derive(Debug, thiserror::Error)]
#[error("plugin.plan: {source}")]
pub struct PlanConfigError {
    #[source]
    source: toml::de::Error,
}

impl From<toml::de::Error> for PlanConfigError {
    fn from(source: toml::de::Error) -> Self {
        Self { source }
    }
}

/// Errors returned by the work battery's tools.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The replacement todo list violates a supported list rule.
    #[error("invalid todo list: {rule}")]
    TodoInvalid {
        /// The violated rule, phrased for the model.
        rule: String,
    },
    /// Plan submission requires plan mode.
    #[error("the plan tool runs only while plan mode is on")]
    NotInPlanMode,
    /// A plan selection is already waiting for a response.
    #[error("a plan is already awaiting review")]
    AlreadyAwaiting,
    /// The Markdown plan exceeded its byte limit.
    #[error("plan is {bytes} bytes; the cap is 16384")]
    PlanTooLarge {
        /// The submitted plan's size in bytes.
        bytes: usize,
    },
    /// The plan summary exceeded its byte limit.
    #[error("summary is {bytes} bytes; the cap is 200")]
    SummaryTooLarge {
        /// The submitted summary's size in bytes.
        bytes: usize,
    },
    /// A dal service failed while handling the operation.
    #[error(transparent)]
    Service(#[from] dal_agent::error::ServiceError),
}

/// User-visible manual page for the plan and todo battery.
pub const PLAN_DOC: &str = "# Plan and todo\n\nUse /plan to enter or leave read-only planning. Use the plan tool to send a plan for approval. Approve leaves plan mode; Revise returns feedback and keeps it on; Reject leaves it. Plan mode resets when a session opens. Use todo to replace or read a journal-backed list. /todos reads the current leaf. todo://current shows that list; todo://terminal reports open work for the goal gate.\n";

fn failure(error: impl std::fmt::Display) -> ToolOutcome {
    ToolOutcome::Err(dal_agent::ToolError::message(error.to_string()))
}

fn service_outcome(error: ServiceError) -> ToolOutcome {
    match error {
        ServiceError::Denied(reason) => ToolOutcome::Err(dal_agent::ToolError::Denied(reason)),
        ServiceError::Cancelled => ToolOutcome::Interrupted,
        error => failure(Error::Service(error)),
    }
}

fn finished(result: Result<String, ToolOutcome>) -> ToolOutcome {
    match result {
        Ok(text) => ToolOutcome::Ok(ToolOutput::from_text(text)),
        Err(outcome) => outcome,
    }
}

async fn append<T: serde::Serialize>(
    services: &dyn Services,
    caller: &Caller,
    kind: &str,
    record: &T,
) -> Result<(), ServiceError> {
    let encoded = sonic_rs::to_string(record)
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
    let body =
        RawJson::parse(&encoded).map_err(|error| ServiceError::failed(None, error.to_string()))?;
    services
        .append_record(caller, kind, Box::new(body))
        .await
        .map(|_| ())
}

fn tool_spec<P: schemars::JsonSchema>(
    name: &str,
    description: &str,
) -> Result<(Name, Arc<ToolSpec>), RegistrationError> {
    let name =
        Name::parse(name).map_err(|_| RegistrationError::InvalidName { name: name.into() })?;
    let schema = sonic_rs::to_string(&schemars::schema_for!(P))
        .map_err(|_| RegistrationError::InvalidParameters)?;
    let parameters = RawJson::parse(&schema).map_err(|_| RegistrationError::InvalidParameters)?;
    if !dal_core::valid_tool_parameters(&parameters) {
        return Err(RegistrationError::InvalidParameters);
    }
    let spec = Arc::new(ToolSpec {
        name: name.clone(),
        description: description.into(),
        parameters,
        grammar: None,
    });
    Ok((name, spec))
}

struct TodoTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for TodoTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            finished(todo::tool(call.args.as_str(), services.as_ref(), cx.caller()).await)
        })
    }
}

struct PlanTool {
    name: Name,
    spec: Arc<ToolSpec>,
    state: Arc<plan::BatteryState>,
}

impl Tool for PlanTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let host = plan::Host {
                services: services.as_ref(),
                caller: cx.caller(),
                session: cx.session(),
            };
            finished(plan::submit(&self.state, call.args.as_str(), host).await)
        })
    }
}

struct PlanCommand {
    state: Arc<plan::BatteryState>,
}

impl CommandHandler for PlanCommand {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let effect = plan::command(&self.state, cx.session(), args)
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            if effect.cancel_turn {
                cx.cancel_turn();
            }
            Ok(Reply::Done(Output::Text(effect.text.into())))
        })
    }
}

struct TodosCommand;

impl CommandHandler for TodosCommand {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let text = todos_reply(args, cx.services().as_ref(), cx.caller()).await?;
            Ok(Reply::Done(Output::Markdown(text.into())))
        })
    }
}

async fn todos_reply(
    args: &str,
    services: &dyn Services,
    caller: &Caller,
) -> Result<String, ServiceError> {
    if !args.trim().is_empty() {
        return Ok(TODOS_USAGE.to_owned());
    }
    let items = load_todos(services, caller).await?;
    Ok(todo::display(&items))
}

struct TodoScheme;

impl SchemeResolver for TodoScheme {
    fn read<'a>(
        &'a self,
        path: &'a str,
        cx: &'a SchemeCx<'a>,
    ) -> BoxFuture<'a, Result<Doc, SchemeError>> {
        Box::pin(async move { resolve_scheme(path, cx.services().as_ref(), cx.caller()).await })
    }
}

async fn resolve_scheme(
    path: &str,
    services: &dyn Services,
    caller: &Caller,
) -> Result<Doc, SchemeError> {
    let uri = format!("todo://{path}");
    if path != "current" && path != "terminal" {
        return Err(SchemeError::NotFound { uri: uri.into() });
    }
    let items = load_todos(services, caller)
        .await
        .map_err(|error| SchemeError::Failed {
            message: error.to_string().into(),
        })?;
    let text = if path == "current" {
        todo::display(&items)
    } else {
        todo_terminal_json(&items).map_err(|error| SchemeError::Failed {
            message: error.to_string().into(),
        })?
    };
    Ok(Doc::new(uri, text))
}

struct PlanStatus {
    state: Arc<plan::BatteryState>,
}

impl StatusPoll for PlanStatus {
    fn snapshot(&self, cx: &StatusCx) -> StatusSnapshot {
        let bodies = cx
            .records(todo::TODO_KIND)
            .iter()
            .map(|record| &record.body);
        let items = todo::fold_bodies(bodies);
        status_snapshot(self.state.phase(cx.session), &items)
    }
}

fn status_snapshot(phase: plan::Phase, items: &[TodoItem]) -> StatusSnapshot {
    StatusSnapshot {
        quiet: status_quiet(phase),
        text: status_line(phase, items).map(String::into_boxed_str),
    }
}

struct SessionOpen {
    state: Arc<plan::BatteryState>,
}

impl ObserveHook<SessionStart> for SessionOpen {
    fn call(&self, input: SessionStart, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.state.session_start(input.session);
        Box::pin(async { Ok(()) })
    }
}

struct SessionClose {
    state: Arc<plan::BatteryState>,
}

impl ObserveHook<SessionEnd> for SessionClose {
    fn call(&self, input: SessionEnd, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.state.session_end(input.session);
        Box::pin(async { Ok(()) })
    }
}

struct TurnContext {
    state: Arc<plan::BatteryState>,
}

impl Hook<BeforeTurn, Option<String>> for TurnContext {
    fn call(
        &self,
        _input: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let state = Arc::clone(&self.state);
        let session = cx.session;
        Box::pin(async move {
            let items = load_todos(cx.services.as_ref(), &cx.caller)
                .await
                .unwrap_or_default();
            Ok(plan::before_turn(state.phase(session), &items))
        })
    }
}

struct ToolGuard {
    state: Arc<plan::BatteryState>,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for ToolGuard {
    fn call(
        &self,
        event: ToolCallEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let verdict = plan::verdict(
            self.state.phase(cx.session),
            &event.class,
            event.tool.as_str(),
        );
        Box::pin(async move { Ok(verdict) })
    }
}

/// Builds the plan and todo extension: the `todo` and `plan` tools, `/plan`
/// and `/todos`, the plan-mode hooks, the `plan` status kind, and the
/// `todo://` scheme. The entry product calls this only when
/// `[plugin.plan]` is enabled. Registration performs no I/O.
///
/// # Errors
/// Returns [`RegistrationError`] when the fixed identity or declarations are
/// rejected.
pub fn work(_config: PlanConfig) -> Result<Extension, RegistrationError> {
    let state = Arc::new(plan::BatteryState::default());
    let (todo_name, todo_spec) = tool_spec::<todo::TodoArgs>(TODO_TOOL, TODO_DESCRIPTION)?;
    let (plan_name, plan_spec) = tool_spec::<PlanArgs>(PLAN_TOOL, PLAN_DESCRIPTION)?;
    let plan_command = CommandSpec {
        name: CommandName::parse("plan")?,
        summary: "Plan mode on or off".into(),
        args_hint: Some("[on|off]".into()),
    };
    let todos_command = CommandSpec {
        name: CommandName::parse("todos")?,
        summary: "Show the current todo list.".into(),
        args_hint: None,
    };
    ExtensionBuilder::new(
        EXTENSION_NAME,
        env!("CARGO_PKG_VERSION"),
        ServiceSet::from_names(["ask"])?,
    )?
    .with_origin(Origin::Bundled, None)
    .tool(
        Arc::new(TodoTool {
            name: todo_name,
            spec: todo_spec,
        }),
        Visibility::Model,
    )
    .tool(
        Arc::new(PlanTool {
            name: plan_name,
            spec: plan_spec,
            state: Arc::clone(&state),
        }),
        Visibility::Model,
    )
    .command(
        plan_command,
        Arc::new(PlanCommand {
            state: Arc::clone(&state),
        }),
    )
    .command(todos_command, Arc::new(TodosCommand))
    .on_session_start_lossless(SessionOpen {
        state: Arc::clone(&state),
    })
    .on_session_end_lossless(SessionClose {
        state: Arc::clone(&state),
    })
    .on_before_turn(TurnContext {
        state: Arc::clone(&state),
    })
    .on_tool_call(ToolGuard {
        state: Arc::clone(&state),
    })
    .status_kind(
        plan::PLAN_KIND,
        Arc::new(PlanStatus {
            state: Arc::clone(&state),
        }),
    )
    .scheme(todo::TODO_KIND, Arc::new(TodoScheme))
    .build()
}

/// Upper bound of the one-line status text shown in the activity row.
const STATUS_LINE_LIMIT: usize = 120;

#[derive(serde::Serialize)]
struct TerminalPayload {
    all_terminal: bool,
    open: usize,
    total: usize,
    first_titles: Vec<String>,
}

pub(crate) fn todo_terminal_json(items: &[TodoItem]) -> Result<String, sonic_rs::Error> {
    let (open, total, first_titles) = open_todos(items);
    sonic_rs::to_string(&TerminalPayload {
        all_terminal: todo_all_terminal(items),
        open,
        total,
        first_titles,
    })
}

pub(crate) fn status_quiet(phase: plan::Phase) -> bool {
    phase != plan::Phase::Awaiting
}

/// Renders the one-line activity text: plan phase, `done/total done`, and the
/// first in-progress subject. `None` when there is nothing to report.
pub(crate) fn status_line(phase: plan::Phase, items: &[TodoItem]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match phase {
        plan::Phase::Off => {}
        plan::Phase::Planning => parts.push("planning".to_owned()),
        plan::Phase::Awaiting => parts.push("awaiting approval".to_owned()),
    }
    if !items.is_empty() {
        let done = items
            .iter()
            .filter(|item| item.state == todo::TodoState::Done)
            .count();
        parts.push(format!("{done}/{} done", items.len()));
    }
    if let Some(active) = items
        .iter()
        .find(|item| item.state == todo::TodoState::InProgress)
    {
        parts.push(line_subject(&active.subject));
    }
    if parts.is_empty() {
        return None;
    }
    let mut line = parts.join(" · ");
    if line.len() > STATUS_LINE_LIMIT {
        let mut end = STATUS_LINE_LIMIT - '…'.len_utf8();
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push('…');
    }
    Some(line)
}

/// Collapses whitespace so a subject cannot break the one-line contract.
fn line_subject(subject: &str) -> String {
    subject.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::todo::TodoState;
    use super::*;

    #[derive(serde::Deserialize)]
    struct TerminalOutput {
        all_terminal: bool,
        open: usize,
        total: usize,
        first_titles: Vec<String>,
    }

    #[test]
    fn plan_config_defaults_to_enabled() {
        assert!(PlanConfig::parse_config(None).is_ok_and(|config| config.enabled));
    }

    #[test]
    fn plan_config_decodes_an_explicit_disabled_section() {
        let section = toml::Value::Table(
            [("enabled".to_owned(), toml::Value::Boolean(false))]
                .into_iter()
                .collect(),
        );

        assert!(PlanConfig::parse_config(Some(&section)).is_ok_and(|config| !config.enabled));
    }

    #[test]
    fn plan_config_rejects_unknown_keys_without_type_reuse() {
        let section = toml::Value::Table(
            [("bogus".to_owned(), toml::Value::Boolean(true))]
                .into_iter()
                .collect(),
        );

        let error = PlanConfig::parse_config(Some(&section));
        assert!(error.is_err());
        if let Err(error) = error {
            assert!(error.to_string().starts_with("plugin.plan: "));
            assert!(error.to_string().contains("bogus"));
        }
    }

    #[test]
    fn todo_terminal_json_reports_first_three_open_titles() {
        let items = [
            TodoItem {
                subject: "one".to_owned(),
                description: String::new(),
                state: TodoState::Pending,
            },
            TodoItem {
                subject: "done".to_owned(),
                description: String::new(),
                state: TodoState::Done,
            },
            TodoItem {
                subject: "two".to_owned(),
                description: String::new(),
                state: TodoState::InProgress,
            },
            TodoItem {
                subject: "three".to_owned(),
                description: String::new(),
                state: TodoState::Pending,
            },
            TodoItem {
                subject: "four".to_owned(),
                description: String::new(),
                state: TodoState::Pending,
            },
        ];
        let json = todo_terminal_json(&items);
        assert!(json.is_ok());
        let decoded = json.and_then(|json| sonic_rs::from_str::<TerminalOutput>(&json));
        assert!(decoded.is_ok());
        if let Ok(output) = decoded {
            assert!(!output.all_terminal);
            assert_eq!(output.open, 4);
            assert_eq!(output.total, 5);
            assert_eq!(output.first_titles, ["one", "two", "three"]);
        }
    }

    #[test]
    fn todo_terminal_json_marks_empty_and_closed_lists_terminal() {
        let empty = todo_terminal_json(&[]);
        assert!(empty.is_ok());
        if let Ok(json) = empty {
            let output = sonic_rs::from_str::<TerminalOutput>(&json);
            assert!(output.is_ok_and(|value| value.all_terminal));
        }
    }

    fn item(subject: &str, state: TodoState) -> TodoItem {
        TodoItem {
            subject: subject.to_owned(),
            description: String::new(),
            state,
        }
    }

    #[test]
    fn plan_status_text_is_one_human_line() {
        let items = [
            item("a", TodoState::Done),
            item("b", TodoState::Done),
            item("c", TodoState::Done),
            item("writing tests", TodoState::InProgress),
            item("e", TodoState::Pending),
        ];
        let snapshot = status_snapshot(plan::Phase::Planning, &items);
        let text = snapshot.text.as_deref().unwrap_or_default();
        assert!(!text.contains(['{', '}', '"']));
        assert_eq!(text, "planning · 3/5 done · writing tests");
        assert!(snapshot.quiet);

        let awaiting = status_snapshot(plan::Phase::Awaiting, &items[..1]);
        assert_eq!(
            awaiting.text.as_deref(),
            Some("awaiting approval · 1/1 done")
        );
        assert!(!awaiting.quiet);

        assert_eq!(status_snapshot(plan::Phase::Off, &[]).text, None);
        let off = status_snapshot(plan::Phase::Off, &items[3..4]);
        assert_eq!(off.text.as_deref(), Some("0/1 done · writing tests"));
    }

    #[test]
    fn plan_status_line_stays_bounded_and_keeps_counts() {
        let long = format!("a{}\n{}", "界".repeat(100), "x".repeat(200));
        let mut items = (0..todo::MAX_ITEMS - 1)
            .map(|_| item("done", TodoState::Done))
            .collect::<Vec<_>>();
        items.push(item(&long, TodoState::InProgress));
        let snapshot = status_snapshot(plan::Phase::Awaiting, &items);
        let text = snapshot.text.as_deref().unwrap_or_default();
        assert!(text.len() <= STATUS_LINE_LIMIT);
        assert!(!text.contains(['\n', '{', '}']));
        assert!(text.starts_with("awaiting approval · 25/26 done · "));
        assert!(text.ends_with('…'));
    }

    #[test]
    fn plan_status_line_cuts_on_a_char_boundary() {
        let items = [item(
            &format!("a{}", "界".repeat(100)),
            TodoState::InProgress,
        )];
        let text = status_line(plan::Phase::Off, &items).unwrap_or_default();
        assert!(text.len() <= STATUS_LINE_LIMIT);
        assert!(text.ends_with("界…"));
    }

    #[tokio::test]
    async fn plan_status_bound() -> Result<(), Box<dyn std::error::Error>> {
        let host = support::ScriptedWorkHost::open();
        let subject = format!("{}{}", "\"".repeat(50), "界".repeat(50));
        let encoded_subject = sonic_rs::to_string(&subject)?;
        let item = format!(r#"{{"subject":{encoded_subject},"state":"pending"}}"#);
        let args = format!(
            r#"{{"action":"write","todos":[{}]}}"#,
            std::iter::repeat_n(item.as_str(), todo::MAX_ITEMS)
                .collect::<Vec<_>>()
                .join(",")
        );
        let rendered_line = format!("- [ ] {subject}");
        let expected_render = std::iter::repeat_n(rendered_line.as_str(), todo::MAX_ITEMS)
            .collect::<Vec<_>>()
            .join("\n");
        let expected_tool_result = format!("{expected_render}\n{}", todo::UPDATED);

        assert_eq!(host.todo_tool(&args).await, expected_tool_result);
        let record_before = host.services.all_bodies(todo::TODO_KIND);
        assert_eq!(record_before.len(), 1);

        let (quiet, status) = host.status();
        assert!(quiet);
        let status = status.unwrap_or_default();
        assert_eq!(status, "0/26 done");
        assert!(!status.contains(['{', '}', '"']));

        assert!(record_before[0].contains(&encoded_subject));
        assert_eq!(host.services.all_bodies(todo::TODO_KIND), record_before);
        Ok(())
    }

    #[tokio::test]
    async fn todo_scheme_terminal() -> support::TestResult {
        let host = support::ScriptedWorkHost::open();
        let terminal = |text: String| sonic_rs::from_str::<TerminalOutput>(&text);

        let empty = terminal(host.scheme("terminal").await?)?;
        assert!(empty.all_terminal);
        assert_eq!((empty.open, empty.total), (0, 0));
        assert_eq!(host.scheme("current").await?, "No todo list.");

        host.todo_tool(
            r#"{"action":"write","todos":[{"subject":"a","state":"done"},{"subject":"b","state":"cancelled"}]}"#,
        )
        .await;
        let closed = terminal(host.scheme("terminal").await?)?;
        assert!(closed.all_terminal);
        assert_eq!((closed.open, closed.total), (0, 2));

        host.todo_tool(
            r#"{"action":"write","todos":[{"subject":"one","state":"pending"},{"subject":"two","state":"in_progress"},{"subject":"three","state":"pending"},{"subject":"four","state":"pending"},{"subject":"gone","state":"done"}]}"#,
        )
        .await;
        let open = terminal(host.scheme("terminal").await?)?;
        assert!(!open.all_terminal);
        assert_eq!((open.open, open.total), (4, 5));
        assert_eq!(open.first_titles, ["one", "two", "three"]);
        assert_eq!(host.scheme("current").await?, host.todos_command("").await?);

        host.services.set_leaf(Some(0));
        let moved = terminal(host.scheme("terminal").await?)?;
        assert!(moved.all_terminal);
        assert_eq!((moved.open, moved.total), (0, 2));

        host.services.set_leaf(None);
        assert_eq!(host.scheme("current").await?, "No todo list.");
        Ok(())
    }

    #[tokio::test]
    async fn todo_scheme_unknown_path_is_not_found() {
        let host = support::ScriptedWorkHost::open();

        let missing = host.scheme("other").await;

        assert!(matches!(
            missing,
            Err(SchemeError::NotFound { ref uri }) if &**uri == "todo://other"
        ));
    }

    #[tokio::test]
    async fn todos_command_lists_or_reports_usage() -> support::TestResult {
        let host = support::ScriptedWorkHost::open();
        assert_eq!(host.todos_command("  ").await?, "No todo list.");
        assert_eq!(host.todos_command("all").await?, "usage: /todos");

        host.todo_tool(r#"{"action":"write","todos":[{"subject":"a","state":"cancelled"}]}"#)
            .await;
        assert_eq!(host.todos_command("").await?, "- [-] a (cancelled)");
        Ok(())
    }

    #[test]
    fn plan_command_reports_mode_and_usage() {
        let host = support::ScriptedWorkHost::open();

        assert_eq!(host.plan_command("  on  "), "Plan mode is on.");
        assert_eq!(host.plan_command(""), "Plan mode is off.");
        assert_eq!(host.plan_command(""), "Plan mode is on.");
        assert_eq!(host.plan_command("off"), "Plan mode is off.");
        assert_eq!(host.plan_command("maybe"), "usage: /plan [on|off]");
        assert_eq!(host.state.phase(host.session), plan::Phase::Off);
    }

    #[test]
    fn work_registers_its_surface() -> Result<(), Box<dyn std::error::Error>> {
        let extension = work(PlanConfig { enabled: true })?;

        let tools: Vec<&str> = extension
            .tools()
            .iter()
            .map(|(tool, _)| tool.name().as_str())
            .collect();
        assert_eq!(tools, ["todo", "plan"]);
        let workspace = ToolCx::for_test(Arc::new(support::FakeServices::default()))
            .workspace()
            .clone();
        let args = RawJson::parse("{}")?;
        for (tool, visibility) in extension.tools() {
            assert_eq!(*visibility, Visibility::Model);
            assert!(matches!(
                tool.classify(&args, &workspace),
                Ok(ToolClass::Read)
            ));
        }
        let commands: Vec<String> = extension
            .commands()
            .iter()
            .map(|(spec, _)| spec.name.to_string())
            .collect();
        assert_eq!(commands, ["plan", "todos"]);
        assert_eq!(extension.status().map(|(kind, _)| kind), Some("plan"));
        let schemes: Vec<&str> = extension
            .schemes()
            .iter()
            .map(|(name, _)| &**name)
            .collect();
        assert_eq!(schemes, ["todo"]);
        Ok(())
    }
}
