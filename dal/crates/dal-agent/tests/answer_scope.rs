#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! Answer scopes at the session boundary: a subscriber that holds only one
//! answerer role is attached for that request kind alone, so the other kind
//! takes its fail-closed default at once while the declared kind waits for
//! and accepts an answer.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, Extension, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{
    Agent, AnswerScope, Delivery, Env, Host, Product, ServiceError, SessionRef, Subscription,
    ToolError,
};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part, Preview,
    Question, RawJson, RequestId, ServiceSet, ToolClass, ToolSpec, UpdateKind, Visibility,
    Workspace,
};

const WAIT: Duration = Duration::from_secs(20);

const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

fn call_step(tool: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c-{tool}\",\"name\":\"{tool}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c-{tool}\",\"name\":\"{tool}\",\"args\":{{\"kind\":\"parsed\",\"value\":{{}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

fn end_step() -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"done\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

fn spec(name: &Name) -> Arc<ToolSpec> {
    Arc::new(ToolSpec {
        name: name.clone(),
        description: "fixture tool".into(),
        parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
        grammar: None,
    })
}

/// Asks for approval and, once approved, reports that it ran.
struct ApprovalProbe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for ApprovalProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let preview = Preview {
                title: "approval-probe".into(),
                body: "printf approved".into(),
                digest: None,
            };
            match cx.authorize(preview).await {
                Ok(_) => ToolOutcome::Ok(Box::new(ToolOutput::from_text("the probe ran"))),
                Err(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
            }
        })
    }
}

type AskOutcome = Result<Option<Answer>, ServiceError>;

/// Raises one extension question and records how it resolved.
struct Asker {
    name: Name,
    spec: Arc<ToolSpec>,
    seen: Arc<Mutex<Option<AskOutcome>>>,
}

impl Tool for Asker {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
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

fn extension(seen: Arc<Mutex<Option<AskOutcome>>>) -> Extension {
    let probe_name = Name::parse("fixture__probe").expect("probe name");
    let probe = Arc::new(ApprovalProbe {
        spec: spec(&probe_name),
        name: probe_name,
    });
    let asker_name = Name::parse("asker").expect("asker name");
    let asker = Arc::new(Asker {
        spec: spec(&asker_name),
        name: asker_name,
        seen,
    });
    ExtensionBuilder::new(
        "fixture",
        "0.1.0",
        ServiceSet::from_names(["ask"]).expect("ask service"),
    )
    .expect("builder")
    .script_tool(
        probe,
        Visibility::Model,
        ExportSpec {
            id: ExportId {
                plugin: Name::parse("fixture").expect("plugin name"),
                kind: ExportKind::Tool,
                local: Name::parse("probe").expect("local name"),
            },
            uses: OpSet::EMPTY,
            input: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
            description: "approval probe export".into(),
        },
    )
    .tool(asker, Visibility::Model)
    .build()
    .expect("extension")
}

struct Session {
    _tmp: tempfile::TempDir,
    _host: Host,
    agent: Agent,
    subscription: Subscription,
    seen: Arc<Mutex<Option<AskOutcome>>>,
}

/// Opens one session whose script runs the approval probe in its first turn
/// and the asking tool in its second, with one subscriber holding `scope`.
async fn start(scope: AnswerScope) -> Session {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let script = format!(
        "{}{}{}{}",
        call_step("fixture__probe"),
        end_step(),
        call_step("asker"),
        end_step()
    );
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, script).expect("script fixture");
    let config_text = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let seen = Arc::new(Mutex::new(None));
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension(Arc::clone(&seen))],
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
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir).expect("workspace"),
                name: None,
            },
            ClientId::new("scope-test"),
        )
        .await
        .expect("session");
    let subscription = agent.subscribe_scoped(None, scope).expect("subscription");
    Session {
        _tmp: tmp,
        _host: host,
        agent,
        subscription,
        seen,
    }
}

impl Session {
    /// Submits one prompt, answers every request that opens with `answer`
    /// when given, and returns the requests seen and the final view text.
    async fn turn(&mut self, answer: Option<Answer>) -> (Vec<RequestId>, String) {
        let agent = &self.agent;
        let subscription = &mut self.subscription;
        tokio::time::timeout(WAIT, async {
            loop {
                let submitted = agent
                    .submit(Command::Prompt {
                        expect: Expect::Idle,
                        content: vec![Part::Text { text: "go".into() }],
                    })
                    .await;
                if submitted.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let mut opened = Vec::new();
            while let Some(delivery) = subscription.next().await {
                let Delivery::Update(update) = delivery else {
                    continue;
                };
                match &update.kind {
                    UpdateKind::RequestOpened(request) => {
                        opened.push(request.id);
                        if let Some(answer) = &answer {
                            loop {
                                if agent.answer(request.id, answer.clone()).await.is_ok() {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }
                    }
                    UpdateKind::TurnEnded { .. } => {
                        let view = agent.view(dal_core::PageReq::default()).expect("view");
                        return (opened, format!("{view:?}"));
                    }
                    _ => {}
                }
            }
            panic!("the session closed before the turn ended");
        })
        .await
        .expect("the turn ended")
    }

    fn ask_outcome(&self) -> Option<AskOutcome> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// An approval-only subscriber holds the approval request open and answers
/// it, while an extension question raised in the same session never opens and
/// takes its fail-closed default.
#[tokio::test]
async fn an_approval_only_scope_answers_approvals_and_defaults_asks() {
    let mut session = start(AnswerScope {
        approval: true,
        ask: false,
    })
    .await;

    let (opened, dump) = session.turn(Some(Answer::Approve)).await;
    assert_eq!(opened.len(), 1, "the approval waited for its answerer");
    assert!(
        dump.contains("the probe ran"),
        "the approval was answered: {dump}"
    );

    let (opened, _) = session.turn(None).await;
    assert!(opened.is_empty(), "the ask opened no request: {opened:?}");
    let outcome = session.ask_outcome();
    assert!(
        matches!(outcome, Some(Ok(None))),
        "the ask took its fail-closed default: {outcome:?}"
    );
}

/// An ask-only subscriber answers the extension question, while an approval
/// raised in the same session never opens and is denied without running.
#[tokio::test]
async fn an_ask_only_scope_answers_asks_and_denies_approvals() {
    let mut session = start(AnswerScope {
        approval: false,
        ask: true,
    })
    .await;

    let (opened, dump) = session.turn(None).await;
    assert!(
        opened.is_empty(),
        "the approval opened no request: {opened:?}"
    );
    assert!(
        dump.contains("Permission denied"),
        "the approval was denied: {dump}"
    );
    assert!(
        !dump.contains("the probe ran"),
        "the denied tool did not run: {dump}"
    );

    let answer = Answer::Value(RawJson::parse("\"ada\"").expect("answer json"));
    let (opened, _) = session.turn(Some(answer)).await;
    assert_eq!(opened.len(), 1, "the ask waited for its answerer");
    let outcome = session.ask_outcome();
    assert!(
        matches!(outcome, Some(Ok(Some(Answer::Value(_))))),
        "the ask was answered: {outcome:?}"
    );
}
