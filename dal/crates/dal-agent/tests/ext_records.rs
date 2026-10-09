#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! Extension records ride the session journal on the current leaf, and a
//! tool acts for the extension that registered it.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, CommandCx, CommandHandler, Extension, ExtensionBuilder, RawValue, StatusCx,
    StatusPoll, StatusSnapshot, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, ServiceError, SessionRef, Subscription};
use dal_core::{
    Answer, ClientId, Command, CommandName, CommandSpec, Config, ConfigProduct, EntryId, Expect,
    ModelInfo, Name, Output, Part, Question, RawJson, Reply, ServiceSet, ToolClass, ToolSpec,
    UpdateKind, Visibility, Workspace,
};

const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

fn call_step(tool: &str, args: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c-{tool}\",\"name\":\"{tool}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c-{tool}\",\"name\":\"{tool}\",\"args\":{{\"kind\":\"parsed\",\"value\":{args}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

fn end_step() -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"done\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

fn turn_script(tool: &str, args: &str) -> String {
    format!("{}{}", call_step(tool, args), end_step())
}

struct Recorder {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Recorder {
    fn new(tool: &str) -> Arc<Self> {
        let name = Name::parse(tool).expect("tool name");
        Arc::new(Self {
            spec: Arc::new(ToolSpec {
                name: name.clone(),
                description: "fixture tool".into(),
                parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                grammar: None,
            }),
            name,
        })
    }
}

struct Todo(Arc<Recorder>);

impl Tool for Todo {
    fn name(&self) -> &Name {
        &self.0.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.0.spec)
    }

    fn classify(
        &self,
        _args: &RawValue,
        _workspace: &Workspace,
    ) -> Result<ToolClass, dal_agent::ext::ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            match services
                .append_record(cx.caller(), "todo", Box::new(call.args))
                .await
            {
                Ok(_) => ToolOutcome::Ok(Box::new(ToolOutput::from_text("recorded"))),
                Err(error) => ToolOutcome::Err(dal_agent::ToolError::message(error.to_string())),
            }
        })
    }
}

type AskOutcome = Result<Option<Answer>, ServiceError>;

struct Asker {
    base: Arc<Recorder>,
    seen: Arc<Mutex<Option<AskOutcome>>>,
}

impl Tool for Asker {
    fn name(&self) -> &Name {
        &self.base.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.base.spec)
    }

    fn classify(
        &self,
        _args: &RawValue,
        _workspace: &Workspace,
    ) -> Result<ToolClass, dal_agent::ext::ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let question = Question::Text {
                prompt: "who?".into(),
                placeholder: None,
            };
            let outcome = cx.services().ask(cx.caller(), question).await;
            *self
                .seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("asked")))
        })
    }
}

struct Overflow {
    base: Arc<Recorder>,
    seen: Arc<Mutex<Option<(bool, bool)>>>,
}

impl Tool for Overflow {
    fn name(&self) -> &Name {
        &self.base.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.base.spec)
    }

    fn classify(
        &self,
        _args: &RawValue,
        _workspace: &Workspace,
    ) -> Result<ToolClass, dal_agent::ext::ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let huge = RawJson::parse(&format!("\"{}\"", "a".repeat(67_108_864)))
                .expect("huge json string");
            let refused = services
                .append_record(cx.caller(), "todo", Box::new(huge))
                .await
                .is_err();
            let small = RawJson::parse(r#"{"ok":1}"#).expect("small json");
            let accepted = services
                .append_record(cx.caller(), "todo", Box::new(small))
                .await
                .is_ok();
            *self
                .seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((refused, accepted));
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")))
        })
    }
}

struct ListTodos;

impl CommandHandler for ListTodos {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let rows = cx.services().records(cx.caller(), "todo").await?;
            let text: Vec<&str> = rows.iter().map(|row| row.as_str()).collect();
            Ok(Reply::Done(Output::Text(text.join(";").into())))
        })
    }
}

struct TodoStatus;

impl StatusPoll for TodoStatus {
    fn snapshot(&self, cx: &StatusCx) -> StatusSnapshot {
        StatusSnapshot {
            quiet: true,
            text: Some(format!("todos={}", cx.records("todo").len()).into()),
        }
    }
}

fn work_extension() -> Extension {
    ExtensionBuilder::new("work", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .tool(Arc::new(Todo(Recorder::new("todo"))), Visibility::Model)
        .command(
            CommandSpec {
                name: CommandName::parse("todos").expect("command name"),
                summary: "List the todo records".into(),
                args_hint: None,
            },
            Arc::new(ListTodos),
        )
        .status_kind("work", Arc::new(TodoStatus))
        .build()
        .expect("work extension")
}

fn asker_extension(seen: Arc<Mutex<Option<AskOutcome>>>) -> Extension {
    let inject = ServiceSet::from_names(["ask"]).expect("ask service");
    ExtensionBuilder::new("asker", "0.1.0", inject)
        .expect("builder")
        .tool(
            Arc::new(Asker {
                base: Recorder::new("probe"),
                seen,
            }),
            Visibility::Model,
        )
        .build()
        .expect("asker extension")
}

fn big_extension(seen: Arc<Mutex<Option<(bool, bool)>>>) -> Extension {
    ExtensionBuilder::new("big", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .tool(
            Arc::new(Overflow {
                base: Recorder::new("overflow"),
                seen,
            }),
            Visibility::Model,
        )
        .command(
            CommandSpec {
                name: CommandName::parse("todos").expect("command name"),
                summary: "List the todo records".into(),
                args_hint: None,
            },
            Arc::new(ListTodos),
        )
        .build()
        .expect("big extension")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    host: Host,
    workspace: Workspace,
}

async fn start(extensions: Vec<Extension>, script: String) -> (Fixture, Agent) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, script).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(tmp.path().join("home")),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.path().join("cache")),
            ),
        ]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("records-test"),
        )
        .await
        .expect("session");
    (
        Fixture {
            _tmp: tmp,
            host,
            workspace,
        },
        agent,
    )
}

const WAIT: Duration = Duration::from_secs(20);

async fn until_ok<T, E: std::fmt::Debug>(mut attempt: impl AsyncFnMut() -> Result<T, E>) -> T {
    tokio::time::timeout(WAIT, async {
        loop {
            match attempt().await {
                Ok(value) => return value,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("the session accepted the command")
}

async fn turn_ended(subscription: &mut Subscription) {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && matches!(update.kind, UpdateKind::TurnEnded { .. })
            {
                return;
            }
        }
    })
    .await
    .expect("the turn ended");
}

async fn prompt(agent: &Agent, subscription: &mut Subscription, text: &str) {
    until_ok(async || {
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: text.into() }],
            })
            .await
    })
    .await;
    turn_ended(subscription).await;
}

async fn todos(agent: &Agent) -> String {
    let reply = until_ok(async || {
        agent
            .submit(Command::Run {
                name: "todos".into(),
                args: "".into(),
                expected: None,
            })
            .await
    })
    .await;
    let Reply::Done(Output::Text(text)) = reply else {
        panic!("unexpected reply {reply:?}");
    };
    text.into()
}

fn tip(agent: &Agent) -> EntryId {
    agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .entries
        .items
        .last()
        .expect("a journaled entry")
        .id
}

async fn move_leaf(agent: &Agent, to: EntryId) {
    until_ok(async || agent.submit(Command::MoveLeaf(to)).await).await;
}

#[tokio::test]
async fn a_tool_record_is_read_by_its_owning_extension_command_and_status() {
    let script = turn_script("todo", r#"{"n":1}"#);
    let (fixture, agent) = start(vec![work_extension()], script).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    until_ok(async || {
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "write a todo".into(),
                }],
            })
            .await
    })
    .await;
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && let UpdateKind::ExtStatus(status) = &update.kind
                && status.ext.as_ref() == "work"
                && status.text.as_deref() == Some("todos=1")
            {
                return;
            }
        }
    })
    .await
    .expect("the work status counted the tool's record");
    assert_eq!(todos(&agent).await, r#"{"n":1}"#);
    drop(fixture);
}

#[tokio::test]
async fn a_tool_uses_the_services_its_extension_injects() {
    let seen = Arc::new(Mutex::new(None));
    let script = turn_script("probe", "{}");
    let (fixture, agent) = start(vec![asker_extension(Arc::clone(&seen))], script).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    until_ok(async || {
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "ask me".into(),
                }],
            })
            .await
    })
    .await;
    let answer = Answer::Value(RawJson::parse("\"ada\"").expect("answer json"));
    tokio::time::timeout(WAIT, async {
        loop {
            let open = agent.view(dal_core::PageReq::default()).expect("view").open;
            if let Some(request) = open.first() {
                agent
                    .answer(request.id, answer.clone())
                    .await
                    .expect("answered");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the tool opened a request");
    turn_ended(&mut subscription).await;
    let outcome = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    assert!(
        matches!(outcome, Some(Ok(Some(Answer::Value(_))))),
        "the injected ask reached the client: {outcome:?}"
    );
    drop(fixture);
}

#[tokio::test]
async fn records_follow_the_leaf_across_branches() {
    let script = format!(
        "{}{}{}",
        turn_script("todo", r#"{"n":1}"#),
        turn_script("todo", r#"{"n":2}"#),
        turn_script("todo", r#"{"n":3}"#),
    );
    let (fixture, agent) = start(vec![work_extension()], script).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    prompt(&agent, &mut subscription, "one").await;
    let first_tip = tip(&agent);
    prompt(&agent, &mut subscription, "two").await;
    let second_tip = tip(&agent);
    assert_eq!(todos(&agent).await, r#"{"n":1};{"n":2}"#);
    move_leaf(&agent, first_tip).await;
    assert_eq!(todos(&agent).await, r#"{"n":1}"#);
    prompt(&agent, &mut subscription, "three").await;
    let third_tip = tip(&agent);
    assert_eq!(todos(&agent).await, r#"{"n":1};{"n":3}"#);
    move_leaf(&agent, second_tip).await;
    assert_eq!(todos(&agent).await, r#"{"n":1};{"n":2}"#);
    move_leaf(&agent, third_tip).await;
    assert_eq!(todos(&agent).await, r#"{"n":1};{"n":3}"#);
    drop(fixture);
}

#[tokio::test]
async fn records_survive_resume_from_the_journal() {
    let script = turn_script("todo", r#"{"n":1}"#);
    let (fixture, agent) = start(vec![work_extension()], script).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    prompt(&agent, &mut subscription, "write a todo").await;
    let session = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id;
    fixture.host.close(session).await.expect("session close");
    let resumed = fixture
        .host
        .open(
            SessionRef::Continue {
                workspace: fixture.workspace.clone(),
            },
            ClientId::new("records-resume"),
        )
        .await
        .expect("resumed session");
    assert_eq!(todos(&resumed).await, r#"{"n":1}"#);
}

#[tokio::test]
async fn a_rejected_record_leaves_the_session_writable() {
    let seen = Arc::new(Mutex::new(None));
    let script = turn_script("overflow", "{}");
    let (fixture, agent) = start(vec![big_extension(Arc::clone(&seen))], script).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    prompt(&agent, &mut subscription, "overflow").await;
    let outcome = *seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        outcome,
        Some((true, true)),
        "the oversize record is refused and the next one is journaled"
    );
    assert_eq!(todos(&agent).await, r#"{"ok":1}"#);
    drop(fixture);
}
