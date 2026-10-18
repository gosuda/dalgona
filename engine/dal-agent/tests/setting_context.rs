//! Setting commands must keep earlier turns visible to the model.

#![expect(clippy::expect_used, reason = "test assertions abort on failure")]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::ext::{BoxFuture, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ApprovalMode, Caps, ClientId, Command, Config, ConfigProduct, ContextItem, EntryKind, Expect,
    Mode, ModelId, ModelRequest, ModelRoute, Origin, PageReq, Part, ServiceSet, ThinkingLevel,
    UpdateKind, Usage, View, Workspace, ext::OpSet,
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
