#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! Exercises observer delivery through a public scripted session turn.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, ExportSpec, Extension, ExtensionBuilder, HookCx, HookError, ObserveHook, RawValue,
    StreamWatch, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput, TurnInfo, WatchFactory,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::ext::{Channel, ExportId, ExportKind, OpSet, Settled, StreamVerdict, TurnEnd};
use dal_core::{
    CallId, ClientId, Command, Config, ConfigProduct, Expect, Name, Part, RawJson, SessionEnd,
    SessionId, SessionStart, Stop, ToolClass, ToolResultEvent, ToolSpec, TurnId, Visibility,
    Workspace,
};

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"probe-call\",\"name\":\"fixture__probe\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"probe-call\",\"name\":\"fixture__probe\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const STEP_WATCH: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"trigger\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

#[derive(Debug, PartialEq)]
enum Observed {
    SessionStart {
        session: SessionId,
        resumed: bool,
    },
    SessionEnd {
        session: SessionId,
        reason: Box<str>,
    },
    ToolResult {
        turn: TurnId,
        call: CallId,
        tool: Name,
        ok: bool,
        preview: Box<str>,
    },
    TurnEnd {
        turn: TurnId,
        stop: Stop,
    },
    Settled {
        turn: TurnId,
        reply_text: Box<str>,
    },
}

struct Recorder<T> {
    events: Arc<Mutex<Vec<Observed>>>,
    record: fn(&T) -> Observed,
}

impl<T: Send + 'static> ObserveHook<T> for Recorder<T> {
    fn call(&self, input: T, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let event = (self.record)(&input);
        let events = Arc::clone(&self.events);
        Box::pin(async move {
            events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event);
            Ok(())
        })
    }
}

fn record_session_start(event: &SessionStart) -> Observed {
    Observed::SessionStart {
        session: event.session,
        resumed: event.resumed,
    }
}

fn record_session_end(event: &SessionEnd) -> Observed {
    Observed::SessionEnd {
        session: event.session,
        reason: event.reason.clone(),
    }
}

fn record_tool_result(event: &ToolResultEvent) -> Observed {
    Observed::ToolResult {
        turn: event.turn,
        call: event.call.clone(),
        tool: event.tool.clone(),
        ok: event.ok,
        preview: event.preview.clone(),
    }
}

fn record_turn_end(event: &TurnEnd) -> Observed {
    Observed::TurnEnd {
        turn: event.turn,
        stop: event.stop,
    }
}

fn record_settled(event: &Settled) -> Observed {
    Observed::Settled {
        turn: event.turn,
        reply_text: event.reply_text.clone(),
    }
}

struct TriggerWatcherFactory {
    triggered: Arc<AtomicBool>,
}

struct TriggerWatcher {
    triggered: Arc<AtomicBool>,
}

impl WatchFactory for TriggerWatcherFactory {
    fn start(&self, _turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        Some(Box::new(TriggerWatcher {
            triggered: Arc::clone(&self.triggered),
        }))
    }
}

impl StreamWatch for TriggerWatcher {
    fn feed(&mut self, _channel: Channel, delta: &str) -> StreamVerdict {
        if delta == "trigger" && !self.triggered.swap(true, Ordering::Relaxed) {
            StreamVerdict::Interrupt {
                rule: "watch-owner-test".into(),
                inject: "Complete the response.".into(),
            }
        } else {
            StreamVerdict::Continue
        }
    }

    fn finish(&mut self) -> StreamVerdict {
        StreamVerdict::Continue
    }
}

struct Probe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for Probe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(
        &self,
        _args: &RawValue,
        _workspace: &Workspace,
    ) -> Result<ToolClass, dal_agent::ext::ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async {
            ToolOutcome::Ok(Box::new(ToolOutput::from_text(format!(
                "{}💥",
                "a".repeat(4095)
            ))))
        })
    }
}

fn observer_extension(events: Arc<Mutex<Vec<Observed>>>) -> Extension {
    let probe_name = Name::parse("fixture__probe").expect("tool name");
    let probe = Arc::new(Probe {
        name: probe_name.clone(),
        spec: Arc::new(ToolSpec {
            name: probe_name,
            description: "observer probe".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("tool schema"),
            grammar: None,
        }),
    });
    ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)
        .expect("extension builder")
        .on_session_start(Recorder {
            events: Arc::clone(&events),
            record: record_session_start,
        })
        .on_session_end(Recorder {
            events: Arc::clone(&events),
            record: record_session_end,
        })
        .on_tool_result(Recorder {
            events: Arc::clone(&events),
            record: record_tool_result,
        })
        .on_turn_end(Recorder {
            events: Arc::clone(&events),
            record: record_turn_end,
        })
        .on_settled(Recorder {
            events,
            record: record_settled,
        })
        .script_tool(
            probe,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture").expect("plugin name"),
                    kind: ExportKind::Tool,
                    local: Name::parse("probe").expect("export name"),
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#).expect("export schema"),
                description: "observer probe export".into(),
            },
        )
        .build()
        .expect("extension")
}

async fn start_session(
    events: Arc<Mutex<Vec<Observed>>>,
) -> (tempfile::TempDir, Host, dal_agent::Agent) {
    start_session_with(
        vec![observer_extension(events)],
        format!("{STEP_CALL}{STEP_END}"),
    )
    .await
}

async fn start_session_with(
    extensions: Vec<Extension>,
    script: String,
) -> (tempfile::TempDir, Host, dal_agent::Agent) {
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
        data_root: data.clone(),
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
                workspace,
                name: None,
            },
            ClientId::new("observer-test"),
        )
        .await
        .expect("session");
    (tmp, host, agent)
}

fn watcher_extension() -> Extension {
    ExtensionBuilder::new("owner", "0.1.0", dal_core::ServiceSet::EMPTY)
        .expect("extension builder")
        .output_stream(Arc::new(TriggerWatcherFactory {
            triggered: Arc::new(AtomicBool::new(false)),
        }))
        .build()
        .expect("watcher extension")
}

fn assert_settled_follows_tool_result(events: &Arc<Mutex<Vec<Observed>>>) {
    let seen = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (turn_end_position, turn) = seen
        .iter()
        .enumerate()
        .find_map(|(position, event)| match event {
            Observed::TurnEnd {
                turn,
                stop: Stop::EndTurn,
            } => Some((position, *turn)),
            _ => None,
        })
        .expect("turn-end observer fired");
    let tool_result_position = seen
        .iter()
        .position(|event| matches!(event, Observed::ToolResult { .. }))
        .expect("tool-result observer fired");
    let settled: Vec<_> = seen
        .iter()
        .enumerate()
        .filter_map(|(position, event)| match event {
            Observed::Settled {
                turn: settled_turn,
                reply_text,
            } => Some((position, *settled_turn, reply_text.as_ref())),
            _ => None,
        })
        .collect();
    assert_eq!(settled.len(), 1);
    let (settled_position, settled_turn, reply_text) = settled[0];
    assert_eq!(settled_turn, turn);
    assert_eq!(reply_text, "done");
    assert!(tool_result_position < settled_position);
    assert!(turn_end_position < settled_position);
}

async fn wait_for(events: &Arc<Mutex<Vec<Observed>>>, predicate: impl Fn(&[Observed]) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if predicate(
                &events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observer event arrived");
}

#[tokio::test]
async fn session_turn_and_settled_observers_fire_for_one_scripted_call() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (_tmp, host, agent) = start_session(Arc::clone(&events)).await;
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let session = view.session.id;
    let workspace = view.session.workspace;
    wait_for(&events, |seen| {
        seen.iter().any(|event| {
            matches!(event, Observed::SessionStart { session: id, resumed: false } if *id == session)
        })
    })
    .await;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the observer probe".into(),
            }],
        })
        .await
        .expect("prompt accepted");
    assert!(matches!(reply, dal_core::Reply::Accepted { .. }));
    wait_for(&events, |seen| {
        let tool_result = seen.iter().any(|event| {
            matches!(
                event,
                Observed::ToolResult {
                    call,
                    tool,
                    ok: true,
                    preview,
                    ..
                } if call.as_str() == "probe-call"
                    && tool.as_str() == "fixture__probe"
                    && preview.len() == 4095
                    && preview.chars().all(|character| character == 'a')
            )
        });
        let turn_end = seen.iter().any(|event| {
            matches!(
                event,
                Observed::TurnEnd {
                    stop: Stop::EndTurn,
                    ..
                }
            )
        });
        let settled = seen
            .iter()
            .any(|event| matches!(event, Observed::Settled { .. }));
        tool_result && turn_end && settled
    })
    .await;
    assert_settled_follows_tool_result(&events);
    host.close(session).await.expect("session close");
    let resumed = host
        .open(
            SessionRef::Continue { workspace },
            ClientId::new("observer-continue"),
        )
        .await
        .expect("continued session");
    assert_eq!(
        resumed
            .view(dal_core::PageReq::default())
            .expect("view")
            .session
            .id,
        session
    );
    wait_for(&events, |seen| {
        seen.iter().any(|event| {
            matches!(event, Observed::SessionStart { session: id, resumed: true } if *id == session)
        })
    })
    .await;
    host.close(session).await.expect("resumed session close");
    let session_ends = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|event| {
            matches!(event, Observed::SessionEnd { session: id, reason } if *id == session && reason.as_ref() == "close")
        })
        .count();
    assert_eq!(session_ends, 2);
}

#[tokio::test]
async fn watcher_verdict_notice_names_its_owning_extension() {
    let (_tmp, host, agent) =
        start_session_with(vec![watcher_extension()], format!("{STEP_WATCH}{STEP_END}")).await;
    let session = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id;
    let mut subscription = agent.subscribe(None).expect("subscription");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "trigger the output watcher".into(),
            }],
        })
        .await
        .expect("prompt accepted");
    assert!(matches!(reply, dal_core::Reply::Accepted { .. }));
    let notice = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let Some(dal_agent::Delivery::Update(update)) = subscription.next().await else {
                continue;
            };
            if let dal_core::UpdateKind::Notice(notice) = &update.kind
                && notice.kind.as_ref() == "watcher.verdict"
            {
                return notice.text.clone();
            }
        }
    })
    .await
    .expect("watcher notice arrived");
    assert!(notice.contains("Extension \"owner\""), "{notice}");
    host.close(session).await.expect("session close");
}
