// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Todo lists and plan review as one session-scoped extension.

use std::borrow::Cow;
use std::cell::Cell;
use std::io::{self, Write};
use std::sync::Arc;

use crate::text::{byte_prefix, normalize_ws};
use dal_agent::error::{SchemeError, ServiceError};
use dal_agent::ext::{
    ArgError, BoxFuture, Caller, CommandCx, CommandHandler, Doc, Extension, ExtensionBuilder, Hook,
    HookCx, HookError, ObserveHook, RawValue, SchemeCx, SchemeResolver, Services, StatusCx,
    StatusPoll, StatusSample, StatusSnapshot, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_core::ext::{
    BeforeTurn, SessionEnd, SessionStart, ToolCallEvent, ToolCallVerdict, Visibility,
};
use dal_core::{
    CommandName, CommandSpec, ModelInfo, Name, Origin, Output, RawJson, RegistrationError, Reply,
    STATUS_PAYLOAD_LIMIT, ServiceSet, StatusPayload, ToolClass, ToolSpec, Workspace,
};

mod plan;
#[cfg(test)]
pub(crate) mod support;
mod todo;

pub use plan::PlanArgs;
pub(crate) use todo::{
    TerminalPayload, TodoItem, load as load_todos, open_todos, todo_all_terminal,
};

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
    /// Returns the first strict-decode failure.
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
        Ok(text) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(text))),
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
    let schema = crate::schema_json::encode(&schemars::schema_for!(P))
        .ok_or(RegistrationError::InvalidParameters)?;
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

    fn sample(&self, cx: &StatusCx) -> StatusSample {
        let phase = self.state.phase(cx.session);
        let bodies = cx
            .records(todo::TODO_KIND)
            .iter()
            .map(|record| &record.body);
        let items = todo::fold_bodies(bodies);
        StatusSample {
            snapshot: status_snapshot(phase, &items),
            payload: status_payload(phase, &items),
        }
    }
}

fn status_snapshot(phase: plan::Phase, items: &[TodoItem]) -> StatusSnapshot {
    StatusSnapshot {
        quiet: status_quiet(phase),
        text: status_line(phase, items).map(String::into_boxed_str),
    }
}

/// Suffix marking a status copy of a subject shortened to fit the payload
/// bound; the journal keeps the verbatim subject.
const STATUS_TRUNCATION_SUFFIX: &str = "[...] status truncated";

/// Wire spelling of a plan phase inside the structured status payload.
fn plan_state_wire(phase: plan::Phase) -> &'static str {
    match phase {
        plan::Phase::Off => "off",
        plan::Phase::Planning => "planning",
        plan::Phase::Awaiting => "awaiting",
    }
}

#[derive(serde::Serialize)]
struct StatusPayloadBody<'a> {
    plan_mode: bool,
    plan_state: &'static str,
    todos: Vec<StatusTodoCopy<'a>>,
}

#[derive(serde::Serialize)]
struct StatusTodoCopy<'a> {
    subject: Cow<'a, str>,
    state: &'a todo::TodoState,
}

struct CountingWriter<'a> {
    bytes: &'a Cell<usize>,
}

impl Write for CountingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self
            .bytes
            .get()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("serialized status size overflow"))?;
        self.bytes.set(count);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

type StatusWriter<'a> = sonic_rs::writer::BufferedWriter<CountingWriter<'a>>;

fn serialized_status_bytes<T: ?Sized + serde::Serialize>(
    writer: &mut StatusWriter<'_>,
    bytes: &Cell<usize>,
    value: &T,
) -> Result<usize, sonic_rs::Error> {
    bytes.set(0);
    sonic_rs::to_writer(writer, value)?;
    Ok(bytes.get())
}

fn status_todo_bytes(
    subject: &str,
    state: &todo::TodoState,
    writer: &mut StatusWriter<'_>,
    bytes: &Cell<usize>,
) -> Result<usize, sonic_rs::Error> {
    serialized_status_bytes(
        writer,
        bytes,
        &StatusTodoCopy {
            subject: Cow::Borrowed(subject),
            state,
        },
    )
}

fn status_todo_cut_bytes(
    prefix: &str,
    state: &todo::TodoState,
    suffix_bytes: usize,
    writer: &mut StatusWriter<'_>,
    bytes: &Cell<usize>,
) -> Result<usize, sonic_rs::Error> {
    let prefix_bytes = status_todo_bytes(prefix, state, writer, bytes)?;
    Ok(prefix_bytes + suffix_bytes - 2)
}
fn fitting_cut(
    subject: &str,
    state: &todo::TodoState,
    target: usize,
    suffix_bytes: usize,
    writer: &mut StatusWriter<'_>,
    bytes: &Cell<usize>,
) -> Option<(usize, usize)> {
    let boundaries: Vec<usize> = subject
        .char_indices()
        .map(|(offset, _)| offset)
        .chain([subject.len()])
        .collect();
    let mut low = 0usize;
    let mut high = boundaries.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let candidate = status_todo_cut_bytes(
            &subject[..boundaries[middle]],
            state,
            suffix_bytes,
            writer,
            bytes,
        )
        .ok()?;
        if candidate <= target {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let start = boundaries[low];
    let selected =
        status_todo_cut_bytes(&subject[..start], state, suffix_bytes, writer, bytes).ok()?;
    Some((start, selected))
}

/// Builds the structured status payload from the same fold output as the
/// human line. Presence mirrors [`status_line`]: `None` exactly when the
/// human line is `None`, so a quiet session without a list stays at the wire
/// baseline.
///
/// Measurement uses the production serializer with a counting writer, so
/// escaping remains identical to the final payload without creating
/// intermediate JSON strings. Each shortened subject uses a binary search over
/// its UTF-8 character boundaries; only the selected shortened copies are
/// allocated. All items remain in list order with their original states, and
/// the journal subjects are never modified. Non-empty caller slices are
/// revalidated so malformed raw input returns `None` instead of panicking.
pub(crate) fn status_payload(
    phase: plan::Phase,
    items: &[todo::TodoItem],
) -> Option<StatusPayload> {
    if !items.is_empty() && todo::validate(items).is_err() {
        return None;
    }
    status_line(phase, items)?;

    let measured_bytes = Cell::new(0);
    let mut writer = sonic_rs::writer::BufferedWriter::new(CountingWriter {
        bytes: &measured_bytes,
    });
    let empty_body = serialized_status_bytes(
        &mut writer,
        &measured_bytes,
        &StatusPayloadBody {
            plan_mode: phase != plan::Phase::Off,
            plan_state: plan_state_wire(phase),
            todos: Vec::new(),
        },
    )
    .ok()?;
    let suffix_bytes =
        serialized_status_bytes(&mut writer, &measured_bytes, STATUS_TRUNCATION_SUFFIX).ok()?;

    let sizes: Vec<usize> = items
        .iter()
        .map(|item| status_todo_bytes(&item.subject, &item.state, &mut writer, &measured_bytes))
        .collect::<Result<_, _>>()
        .ok()?;
    let array_separators = items.len().saturating_sub(1);
    let available = STATUS_PAYLOAD_LIMIT.saturating_sub(empty_body + array_separators);
    let mut over = sizes.iter().sum::<usize>().saturating_sub(available);

    let mut cuts: Vec<(usize, String)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if over == 0 {
            break;
        }

        let verbatim = sizes[index];
        let floor =
            status_todo_cut_bytes("", &item.state, suffix_bytes, &mut writer, &measured_bytes)
                .ok()?;
        let reclaim = verbatim.saturating_sub(floor);
        if reclaim == 0 {
            continue;
        }

        let target = verbatim - over.min(reclaim);
        let subject = item.subject.as_str();
        let (start, selected_size) = fitting_cut(
            subject,
            &item.state,
            target,
            suffix_bytes,
            &mut writer,
            &measured_bytes,
        )?;
        over = over.saturating_sub(verbatim - selected_size);

        let mut shortened = String::with_capacity(start + STATUS_TRUNCATION_SUFFIX.len());
        shortened.push_str(&subject[..start]);
        shortened.push_str(STATUS_TRUNCATION_SUFFIX);
        cuts.push((index, shortened));
    }

    if over != 0 {
        return None;
    }

    let mut cut = 0usize;
    let todos: Vec<StatusTodoCopy<'_>> = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let subject = if cut < cuts.len() && cuts[cut].0 == index {
                let subject = cuts[cut].1.as_str();
                cut += 1;
                Cow::Borrowed(subject)
            } else {
                Cow::Borrowed(item.subject.as_str())
            };
            StatusTodoCopy {
                subject,
                state: &item.state,
            }
        })
        .collect();

    let text = sonic_rs::to_string(&StatusPayloadBody {
        plan_mode: phase != plan::Phase::Off,
        plan_state: plan_state_wire(phase),
        todos,
    })
    .ok()?;
    StatusPayload::parse(&text).ok()
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
        parts.push(normalize_ws(&active.subject));
    }
    if parts.is_empty() {
        return None;
    }
    let mut line = parts.join(" · ");
    if line.len() > STATUS_LINE_LIMIT {
        let keep = STATUS_LINE_LIMIT - '…'.len_utf8();
        let end = byte_prefix(&line, keep).len();
        line.truncate(end);
        line.push('…');
    }
    Some(line)
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

    fn assert_status_subject(actual: &str, original: &str) {
        if let Some(prefix) = actual.strip_suffix(STATUS_TRUNCATION_SUFFIX) {
            assert!(
                original.starts_with(prefix),
                "shortened subject must retain an original prefix"
            );
            assert!(
                prefix.len() < original.len(),
                "a truncation marker must shorten the original subject"
            );
        } else {
            assert_eq!(actual, original, "unmarked subjects stay verbatim");
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

    #[test]
    fn plan_status_payload_shape_and_member_order() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ShapeTodo {
            subject: String,
            state: TodoState,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ShapePayload {
            plan_mode: bool,
            plan_state: String,
            todos: Vec<ShapeTodo>,
        }
        let items = [
            item("one", TodoState::InProgress),
            item("two", TodoState::Done),
        ];
        let Some(payload) = status_payload(plan::Phase::Planning, &items) else {
            panic!("planning with todos carries a payload");
        };

        let decoded: ShapePayload = sonic_rs::from_str(payload.as_str())
            .unwrap_or_else(|error| panic!("payload decodes into the exact shape: {error}"));
        assert!(decoded.plan_mode);
        assert_eq!(decoded.plan_state, "planning");
        let subjects: Vec<&str> = decoded
            .todos
            .iter()
            .map(|todo| todo.subject.as_str())
            .collect();
        assert_eq!(subjects, ["one", "two"]);
        assert_eq!(decoded.todos[0].state, TodoState::InProgress);
        assert_eq!(decoded.todos[1].state, TodoState::Done);

        let Some(off) = status_payload(plan::Phase::Off, &items) else {
            panic!("off with todos carries a payload");
        };
        let off: ShapePayload = sonic_rs::from_str(off.as_str())
            .unwrap_or_else(|error| panic!("off payload decodes: {error}"));
        assert!(!off.plan_mode);
        assert_eq!(off.plan_state, "off");
    }

    #[test]
    fn plan_status_payload_quiet_and_baseline() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AwaitTodo {
            subject: String,
            state: TodoState,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AwaitPayload {
            plan_mode: bool,
            plan_state: String,
            todos: Vec<AwaitTodo>,
        }
        assert!(status_payload(plan::Phase::Off, &[]).is_none());
        let items = [item("a", TodoState::Pending)];
        assert_eq!(
            status_payload(plan::Phase::Off, &items).is_some(),
            status_snapshot(plan::Phase::Off, &items).text.is_some(),
            "payload presence mirrors the human line"
        );
        let Some(awaiting) = status_payload(plan::Phase::Awaiting, &items) else {
            panic!("awaiting carries a payload");
        };

        let decoded: AwaitPayload = sonic_rs::from_str(awaiting.as_str())
            .unwrap_or_else(|error| panic!("awaiting payload decodes: {error}"));
        assert!(decoded.plan_mode);
        assert_eq!(decoded.plan_state, "awaiting");
        assert_eq!(decoded.todos.len(), 1);
        assert_eq!(decoded.todos[0].subject, "a");
        assert_eq!(decoded.todos[0].state, TodoState::Pending);
    }

    #[test]
    fn plan_status_payload_shortens_in_list_order() {
        #[derive(serde::Deserialize)]
        struct OrderTodo {
            subject: String,
            state: TodoState,
        }
        #[derive(serde::Deserialize)]
        struct OrderPayload {
            todos: Vec<OrderTodo>,
        }
        // Legal write-shape fixture: 26 items at the 200-byte subject cap
        // with quote-heavy text, whose escaping pushes the verbatim body far
        // past the 4096-byte cap.
        let items: Vec<TodoItem> = (0..todo::MAX_ITEMS)
            .map(|index| {
                let mut subject = "\"".repeat(todo::MAX_SUBJECT_BYTES - 1);
                subject.push(char::from(b"abcdefghijklmnopqrstuvwxyz"[index]));
                item(&subject, TodoState::Pending)
            })
            .collect();
        let Some(payload) = status_payload(plan::Phase::Planning, &items) else {
            panic!("payload fits after list-order shortening");
        };
        assert!(payload.as_str().len() <= STATUS_PAYLOAD_LIMIT);
        let decoded: OrderPayload = sonic_rs::from_str(payload.as_str())
            .unwrap_or_else(|error| panic!("payload decodes after shortening: {error}"));
        assert_eq!(decoded.todos.len(), todo::MAX_ITEMS);
        let mut marked_ended = false;
        for (index, todo) in decoded.todos.iter().enumerate() {
            assert_eq!(
                todo.state, items[index].state,
                "shortening preserves every item's state"
            );
            assert_status_subject(&todo.subject, &items[index].subject);
            if todo.subject.ends_with(STATUS_TRUNCATION_SUFFIX) {
                assert!(
                    !marked_ended,
                    "shortened copies form a prefix in list order"
                );
            } else {
                marked_ended = true;
            }
        }
        assert!(
            decoded
                .todos
                .iter()
                .any(|todo| todo.subject.ends_with(STATUS_TRUNCATION_SUFFIX)),
            "the budget forced at least one shortened copy"
        );
    }

    #[test]
    fn plan_status_payload_json_valid_after_multibyte_cuts() {
        #[derive(serde::Deserialize)]
        struct CutTodo {
            subject: String,
            state: TodoState,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CutPayload {
            plan_mode: bool,
            plan_state: String,
            todos: Vec<CutTodo>,
        }
        // Escaped quotes, multibyte CJK, and a control-bearing subject at
        // the 200-byte cap, 26 items total, so budget cuts land inside
        // escape and multibyte runs.
        let quoted_cjk = format!("{}{}", "\"".repeat(50), "\u{754c}".repeat(50));
        let controls = "x\n".repeat(100);
        let cjk = format!("{}{}", "\u{754c}".repeat(66), "aa");
        let items: Vec<TodoItem> = (0..todo::MAX_ITEMS)
            .map(|index| match index {
                0 => item(&quoted_cjk, TodoState::InProgress),
                1 => item(&controls, TodoState::Pending),
                _ => item(&cjk, TodoState::Pending),
            })
            .collect();
        let Some(payload) = status_payload(plan::Phase::Awaiting, &items) else {
            panic!("payload fits after multibyte shortening");
        };
        assert!(payload.as_str().len() <= STATUS_PAYLOAD_LIMIT);

        let decoded: CutPayload = sonic_rs::from_str(payload.as_str())
            .unwrap_or_else(|error| panic!("payload is valid JSON after cuts: {error}"));
        assert!(decoded.plan_mode);
        assert_eq!(decoded.plan_state, "awaiting");
        assert_eq!(decoded.todos.len(), todo::MAX_ITEMS);
        for (index, todo) in decoded.todos.iter().enumerate() {
            assert_eq!(
                todo.state, items[index].state,
                "multibyte shortening preserves every item's state"
            );
            assert_status_subject(&todo.subject, &items[index].subject);
        }
        assert!(
            decoded
                .todos
                .iter()
                .any(|todo| todo.subject.ends_with(STATUS_TRUNCATION_SUFFIX))
        );
    }

    #[tokio::test]
    async fn plan_status_bound() -> Result<(), Box<dyn std::error::Error>> {
        #[derive(serde::Deserialize)]
        struct BoundTodo {
            subject: String,
            state: String,
        }
        #[derive(serde::Deserialize)]
        struct BoundPayload {
            plan_mode: bool,
            plan_state: String,
            todos: Vec<BoundTodo>,
        }
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

        let sample = host.sample();
        assert!(sample.snapshot.quiet);
        assert_eq!(sample.snapshot.text.as_deref(), Some("0/26 done"));
        let Some(payload) = sample.payload else {
            panic!("the bounded status carries a structured payload");
        };
        assert!(payload.as_str().len() <= STATUS_PAYLOAD_LIMIT);
        let decoded: BoundPayload = sonic_rs::from_str(payload.as_str())?;
        assert!(!decoded.plan_mode);
        assert_eq!(decoded.plan_state, "off");
        assert_eq!(decoded.todos.len(), todo::MAX_ITEMS);
        for todo in &decoded.todos {
            assert_eq!(todo.state, "pending", "all folded states reach the payload");
        }
        for todo in &decoded.todos {
            assert_status_subject(&todo.subject, &subject);
        }
        assert!(
            decoded
                .todos
                .iter()
                .any(|todo| todo.subject.ends_with(STATUS_TRUNCATION_SUFFIX))
        );

        // Replay read: sampling consumed nothing, and a fresh tool read
        // still returns the full verbatim render.
        assert_eq!(host.todos_command("").await?, expected_render);
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
