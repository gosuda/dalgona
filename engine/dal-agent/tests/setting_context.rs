//! Setting commands must keep earlier turns visible to the model.

#![expect(clippy::expect_used, reason = "test assertions abort on failure")]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
    ModelRecord, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::ext::{ExportId, ExportKind};
use dal_core::{
    ApprovalMode, Caps, ClientId, Command, Config, ConfigProduct, ContextItem, EntryKind, Expect,
    Mode, ModelId, ModelInfo, ModelRequest, ModelRoute, Name, Origin, PageReq, Part, RawJson,
    ServiceSet, ThinkingLevel, ToolClass, ToolSpec, UpdateKind, Usage, View, Visibility, Workspace,
    ext::OpSet,
};
use dal_provider::{EventStream, ProviderError, StopReason, StreamEvent};
use futures::stream;

fn scripted_stream() -> EventStream {
    let events = vec![
        StreamEvent::TextDelta { text: "ok".into() },
        StreamEvent::ToolCallsDone { calls: Vec::new() },
        StreamEvent::Usage {
            usage: Usage {
                input_tokens: 10,
                cached_input_tokens: 0,
                output_tokens: 5,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
                premium_requests: None,
            },
        },
        StreamEvent::Stop {
            reason: StopReason::EndTurn,
        },
    ];
    EventStream::new(
        stream::iter(events.into_iter().map(Ok::<_, ProviderError>)),
        || {},
    )
}

struct Capture {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ModelHandler for Capture {
    fn uses(&self) -> OpSet {
        OpSet::parse(["models.infer"]).expect("model uses")
    }

    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.requests.lock().expect("requests lock").push(request);
        Box::pin(async { Ok(scripted_stream()) })
    }
}

fn user_texts(view: &View) -> Vec<String> {
    view.entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::User { parts } => Some(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        dal_core::JournalPart::Text { text } => Some(text.as_ref()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

fn context_user_texts(request: &ModelRequest) -> Vec<String> {
    let mut texts = Vec::new();
    for item in request.context.iter() {
        if let ContextItem::User { parts } = item {
            for part in parts {
                if let Part::Text { text } = part {
                    texts.push(text.to_string());
                }
            }
        }
    }
    texts
}

#[expect(
    clippy::too_many_lines,
    reason = "setting fixture walks three turns and four commands in place"
)]
#[tokio::test]
async fn setting_commands_keep_earlier_turns_visible_to_the_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"dalgona/capture\"\n"),
    )
    .expect("config");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let extension = ExtensionBuilder::new("capture", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .with_origin(Origin::Builtin, None)
        .model(ModelRecord {
            id: ModelId::parse("dalgona/capture").expect("model id"),
            caps: Caps {
                context_window: Some(4096),
                thinking: Box::new([ThinkingLevel::Off, ThinkingLevel::Low]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Capture {
                requests: Arc::clone(&requests),
            }),
            export: None,
        })
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
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
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");

    for (index, text) in ["one", "two", "three"].into_iter().enumerate() {
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: text.into() }],
            })
            .await
            .expect("submit");
        assert!(
            matches!(reply, dal_core::Reply::Accepted { .. }),
            "prompt {text} not accepted: {reply:?}"
        );
        let mut ended = false;
        for _ in 0..40 {
            let delivery = tokio::time::timeout(Duration::from_secs(3), subscription.next()).await;
            let Ok(delivery) = delivery else {
                break;
            };
            let delivery = delivery.expect("stream open");
            if let Delivery::Update(update) = &delivery
                && matches!(update.kind, UpdateKind::TurnEnded { .. })
            {
                ended = true;
                break;
            }
        }
        assert!(ended, "the turn for {text} never ended");
        if index == 0 {
            for command in [
                Command::SetThinking {
                    level: ThinkingLevel::Low,
                    save: dal_core::Save::SessionOnly,
                },
                Command::SetModel {
                    model: ModelRoute::Synthetic {
                        id: "dalgona/capture".into(),
                    },
                    save: dal_core::Save::SessionOnly,
                },
                Command::SetApproval {
                    mode: ApprovalMode::Edits,
                    save: dal_core::Save::SessionOnly,
                },
                Command::SetMode {
                    mode: Mode::Normal,
                    save: dal_core::Save::SessionOnly,
                },
            ] {
                agent.submit(command).await.expect("setting command");
            }
        }
    }

    let view = agent.view(PageReq::default()).expect("view");
    assert_eq!(
        user_texts(&view),
        vec!["one", "two", "three"],
        "the leaf view must hold every turn across the setting changes"
    );

    let captured = requests.lock().expect("requests lock");
    let last = captured.last().expect("capture model received requests");
    let context = context_user_texts(last);
    for text in ["one", "two", "three"] {
        assert!(
            context.iter().any(|item| item.as_str() == text),
            "the last model request is missing {text:?}: {context:?}"
        );
    }
}

/// A script tool only evaluation flows may see, so the request path's mode
/// sourcing is observable in the advertised tool surface.
struct EvalProbe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for EvalProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Ok(Box::new(ToolOutput::from_text("eval probe"))) })
    }
}

async fn prompt(agent: &dal_agent::Agent, text: &'static str) {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("submit");
    assert!(
        matches!(reply, dal_core::Reply::Accepted { .. }),
        "prompt {text} not accepted: {reply:?}"
    );
}

async fn open_session(
    host: &Host,
    workspace_dir: &std::path::Path,
    name: &'static str,
) -> (dal_agent::Agent, dal_agent::Subscription) {
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir.to_path_buf()).expect("workspace"),
                name: None,
            },
            ClientId::new(name),
        )
        .await
        .expect("open");
    let subscription = agent.subscribe(None).expect("subscribe");
    (agent, subscription)
}

async fn await_turn_end(subscription: &mut dal_agent::Subscription) -> bool {
    for _ in 0..40 {
        let delivery = tokio::time::timeout(Duration::from_secs(3), subscription.next()).await;
        let Ok(delivery) = delivery else {
            break;
        };
        let delivery = delivery.expect("stream open");
        if let Delivery::Update(update) = &delivery
            && matches!(update.kind, UpdateKind::TurnEnded { .. })
        {
            return true;
        }
    }
    false
}

/// One host with a tool-capable capture model and one eval-only script tool.
struct EvalModeRig {
    _tmp: tempfile::TempDir,
    host: Host,
    workspace_dir: std::path::PathBuf,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

async fn eval_mode_rig() -> EvalModeRig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"dalgona/capture\"\nmode = \"eval-first\"\n"),
    )
    .expect("config");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let probe_name = Name::parse("capture__eval").expect("probe name");
    let extension = ExtensionBuilder::new("capture", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .with_origin(Origin::Builtin, None)
        .model(ModelRecord {
            id: ModelId::parse("dalgona/capture").expect("model id"),
            caps: Caps {
                context_window: Some(4096),
                thinking: Box::new([ThinkingLevel::Off, ThinkingLevel::Low]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Capture {
                requests: Arc::clone(&requests),
            }),
            export: None,
        })
        .script_tool(
            Arc::new(EvalProbe {
                spec: Arc::new(ToolSpec {
                    name: probe_name.clone(),
                    description: "eval probe tool".into(),
                    parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                    grammar: None,
                }),
                name: probe_name,
            }),
            Visibility::EvalOnly,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("capture").expect("plugin name"),
                    kind: ExportKind::Tool,
                    local: Name::parse("eval").expect("local name"),
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                description: "eval probe export".into(),
            },
        )
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    EvalModeRig {
        _tmp: tmp,
        host,
        workspace_dir,
        requests,
    }
}

/// The request path reads the owning session's folded mode, not the live
/// host default: a session that chose Normal keeps advertising only its own
/// mode's tools while the host-wide default stays eval-first, and a session
/// opened later still seeds the host default.
#[tokio::test]
async fn a_live_sessions_folded_mode_ignores_the_host_default() {
    let rig = eval_mode_rig().await;
    let requests = Arc::clone(&rig.requests);
    let workspace_dir = rig.workspace_dir.clone();
    let host = rig.host;
    let advertises_eval = |index: usize| {
        let captured = requests.lock().expect("requests lock");
        let request = captured
            .get(index)
            .expect("capture model received requests");
        request
            .tools
            .iter()
            .any(|tool| tool.name.as_ref() == "capture__eval")
    };

    let (settled, mut settled_stream) = open_session(&host, &workspace_dir, "settled").await;
    let folded = settled
        .submit(Command::SetMode {
            mode: Mode::Normal,
            save: dal_core::Save::SessionOnly,
        })
        .await
        .expect("fold mode");
    assert!(
        matches!(folded, dal_core::Reply::Done { .. }),
        "folding the session mode failed: {folded:?}"
    );

    prompt(&settled, "settled-normal").await;
    assert!(
        await_turn_end(&mut settled_stream).await,
        "the turn for settled-normal never ended"
    );
    let settled_index = requests.lock().expect("requests lock").len() - 1;
    assert!(
        !advertises_eval(settled_index),
        "a session folded to Normal must not advertise eval-only tools"
    );

    let (fresh, mut fresh_stream) = open_session(&host, &workspace_dir, "fresh").await;
    let fresh_view = fresh.view(PageReq::default()).expect("fresh view");
    assert_eq!(
        fresh_view.settings.mode,
        Mode::EvalFirst,
        "a new session seeds the host default mode"
    );
    prompt(&fresh, "fresh").await;
    assert!(
        await_turn_end(&mut fresh_stream).await,
        "the turn for fresh never ended"
    );
    let fresh_index = requests.lock().expect("requests lock").len() - 1;
    assert!(
        advertises_eval(fresh_index),
        "a new session advertises the eval-only tool"
    );
}
