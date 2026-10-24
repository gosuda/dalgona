//! Setting commands must keep earlier turns visible to the model.

#![expect(clippy::expect_used, reason = "test assertions abort on failure")]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::ext::{
    ArgError, BoxFuture, CompactError, CompactInput, Compaction, Compactor, ExportSpec,
    ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord, RawValue, Services, Tool,
    ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::ext::{ExportId, ExportKind};
use dal_core::{
    ApprovalMode, Caps, ClientId, Command, Config, ConfigProduct, ContextItem, Entry, EntryId,
    EntryKind, Expect, JournalPart, Mode, ModelId, ModelInfo, ModelRequest, ModelRoute, Name,
    Origin, PageReq, Part, Product as JournalProduct, RawJson, Record, ServiceSet, SessionId,
    ThinkingLevel, ToolClass, ToolSpec, UpdateKind, Usage, View, Visibility, Workspace, ext::OpSet,
};
use dal_provider::{EventStream, ProviderError, StopReason, StreamEvent};
use dal_store::Store;
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
/// Records the thinking level the driver hands to compaction.
struct ThinkingCompactor {
    seen: Arc<Mutex<Option<ThinkingLevel>>>,
}

impl Compactor for ThinkingCompactor {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        _services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        *self.seen.lock().expect("thinking level lock") = Some(input.params.thinking);
        // Refuse the chain: the recorded level is the whole assertion.
        Box::pin(async { Ok(None) })
    }
}

/// One host with a capture model and a thinking-recording compactor.
struct ThinkingRig {
    _tmp: tempfile::TempDir,
    host: Host,
    data_dir: std::path::PathBuf,
    workspace_dir: std::path::PathBuf,
    compacted: Arc<Mutex<Option<ThinkingLevel>>>,
}

fn entry_id(value: u64) -> EntryId {
    EntryId::new(std::num::NonZeroU64::new(value).expect("entry id is nonzero"))
}

fn seed_record(id: u64, parent: Option<EntryId>, text: String) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent,
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

async fn thinking_rig() -> ThinkingRig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"dalgona/capture\"\nthinking = \"high\"\n"),
    )
    .expect("config");
    let compacted = Arc::new(Mutex::new(None));
    let extension = ExtensionBuilder::new("capture", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .with_origin(Origin::Builtin, None)
        .model(ModelRecord {
            id: ModelId::parse("dalgona/capture").expect("model id"),
            caps: Caps {
                // Wide enough to hold the seeded span without tripping
                // automatic compaction during the warm turn below.
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off, ThinkingLevel::Low]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Capture {
                requests: Arc::new(Mutex::new(Vec::new())),
            }),
            export: None,
        })
        .compactor(
            "thinking-probe",
            Arc::new(ThinkingCompactor {
                seen: Arc::clone(&compacted),
            }),
        )
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
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
    // Pins where `SessionAndDefault` persists, so the sibling below can
    // rewrite the host default mid-test.
    host.set_config_path(data.join("dal.toml"));
    ThinkingRig {
        _tmp: tmp,
        host,
        data_dir: data.clone(),
        workspace_dir,
        compacted,
    }
}

/// The compaction turn path reads the owning session's folded thinking, not
/// the live host default: a session folded to `Off` keeps compacting at
/// `Off` while the host-wide default stays `High`, mirroring the mode
/// regression's shape (`EvalModeRig` plus a thinking-recording compactor).
///
/// The normal request path cannot show this end to end: the synthetic
/// capture model reports `ToolSupport::Any`, which makes its `ModelInfo`
/// thinking-independent, and `RequestParams.thinking` is fold-derived, so a
/// request-level assertion would pass with or without the fix. Compaction
/// parameters come straight from the repointed host read, so the compactor
/// record is the discriminating proof.
#[tokio::test]
async fn a_live_sessions_folded_thinking_ignores_the_host_default() {
    let rig = thinking_rig().await;
    let compacted = Arc::clone(&rig.compacted);
    let workspace_dir = rig.workspace_dir.clone();
    let host = rig.host;

    // Seed a large journal directly: the manual compaction below needs a
    // nonzero-token span. A short warm turn installs the fold's compaction
    // limits first; the probe is cleared after it so only the manual pass
    // counts.
    let session_id = SessionId::new_v7();
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    let store = Store::new(rig.data_dir.clone(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session_id);
    journal
        .append(vec![
            seed_record(1, None, "old prefix".into()),
            seed_record(2, Some(entry_id(1)), "retained ".repeat(20_000)),
            seed_record(3, Some(entry_id(2)), "tail".into()),
        ])
        .await
        .expect("seed session journal");
    journal.close().await.expect("close seeded session");
    let settled = host
        .open(
            SessionRef::Resume {
                workspace: workspace.clone(),
                key: session_id.to_string().into(),
            },
            ClientId::new("settled"),
        )
        .await
        .expect("resume seeded session");
    let mut settled_stream = settled.subscribe(None).expect("subscribe");

    let folded = settled
        .submit(Command::SetThinking {
            level: ThinkingLevel::Off,
            save: dal_core::Save::SessionOnly,
        })
        .await
        .expect("fold thinking");
    assert!(
        matches!(folded, dal_core::Reply::Done { .. }),
        "folding the session thinking failed: {folded:?}"
    );

    prompt(&settled, "settled-warm").await;
    assert!(
        await_turn_end(&mut settled_stream).await,
        "the warm turn never ended"
    );
    // Discard any automatic pass so the manual compaction below is the only
    // recorded one.
    compacted.lock().expect("thinking level lock").take();

    // The reply fails closed when every compactor refuses; the recorded
    // level is the assertion, polled until the chain runs.
    let compact_reply = settled.submit(Command::Compact { focus: None }).await;
    let polled = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(level) = compacted.lock().expect("thinking level lock").take() {
                return level;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let Ok(seen) = polled else {
        panic!("compactor never ran; compact reply: {compact_reply:?}")
    };
    assert_eq!(
        seen,
        ThinkingLevel::Off,
        "a session folded to Off must not compact at the host default"
    );
}
