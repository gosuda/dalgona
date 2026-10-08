#![expect(clippy::expect_used, reason = "SC test")]
#![expect(
    dead_code,
    reason = "gate support exposes helpers shared across independent targets"
)]

//! Private tools and scope results stay inside the synthetic handler boundary.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    fs,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use dal_agent::{
    Env, Product, SessionRef, Subscription,
    ext::{
        ArgError, BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
        ModelRecord, PrivateTool, RawValue, ScopeValue, Tool, ToolCx, ToolOutcome, ToolOutput,
    },
};
use dal_core::{
    Caps, Command, Config, ConfigProduct, ContextItem, Expect, ModelId, ModelInfo, ModelRequest,
    ModelRoute, Name, OnError, Part, Purpose, RawJson, Reply, RequestParams, ScopeSpec, ServiceSet,
    StreamChannel, StreamEvent, ToolClass, ToolSpec, UpdateKind, Usage, Workspace,
};
use dal_provider::{
    ProviderError, StopReason, StreamEvent as ProviderEvent, ToolArgs, ToolCall as ProviderToolCall,
};
use support::{TestDir, scripted_session};

async fn collect_boundary_stream(
    subscription: &mut Subscription,
) -> Result<(String, Vec<(String, String, String)>), Box<dyn Error + Send + Sync>> {
    let mut assistant_text = String::new();
    let mut session_calls = Vec::new();
    loop {
        let Some(delivery) =
            tokio::time::timeout(Duration::from_secs(10), subscription.next()).await?
        else {
            break;
        };
        let dal_agent::Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::Delta {
                channel: StreamChannel::Text,
                text,
                ..
            } => assistant_text.push_str(text),
            UpdateKind::ToolStarted { call, tool, args } => session_calls.push((
                call.as_str().to_owned(),
                tool.to_string(),
                args.as_str().to_owned(),
            )),
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    Ok((assistant_text, session_calls))
}

struct BoundaryHandler {
    private: PrivateTool,
    scope_call: Arc<Mutex<Option<StreamEvent>>>,
    second_forward: Arc<Mutex<Option<ModelError>>>,
}

impl ModelHandler for BoundaryHandler {
    #[expect(clippy::panic, reason = "SC model aborts on impossible scope results")]
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            if has_tool_result(&request, "forward-session-call") {
                return Ok(text_stream("boundary complete"));
            }
            let scope = cx
                .scope(ScopeSpec {
                    limit: 1,
                    on_error: OnError::Settle,
                    budget: dal_core::Budget::default(),
                })
                .expect("scope specification is valid");
            let handle = scope
                .infer(model_request("gate/scope-child"))
                .expect("scope child is admitted");
            let value = handle.result().await.expect("scope child completes");
            let ScopeValue::Inference(inference) = value else {
                panic!("scope child returned a member report");
            };
            let scope_call = inference.events.into_iter().find(|event| {
                matches!(event, StreamEvent::ToolCall { call, .. } if call.as_str() == "scope-only-call")
            });
            *lock(&self.scope_call) = scope_call;
            let mut forward_request = request.clone();
            forward_request.model = ModelRoute::from_id("gate/forward-child");
            let private = std::slice::from_ref(&self.private);
            let stream = cx.forward(forward_request, private).await?;
            let mut second_request = request;
            second_request.model = ModelRoute::from_id("gate/forward-child");
            if let Err(error) = cx.forward(second_request, private).await {
                *lock(&self.second_forward) = Some(error);
            }
            Ok(stream)
        })
    }
}

struct ScopeChild;

impl ModelHandler for ScopeChild {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async {
            Ok(tool_stream(
                "scope-only-call",
                "read",
                r#"{"path":"synthetic.txt"}"#,
                StopReason::ToolUse,
            ))
        })
    }
}

struct ForwardChild {
    private_name: String,
    private_result_seen: Arc<AtomicBool>,
    private_result_succeeded: Arc<AtomicBool>,
    calls: AtomicUsize,
}

impl ModelHandler for ForwardChild {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let private_result = request.context.iter().find_map(|item| match item {
                ContextItem::ToolResult {
                    call,
                    name,
                    is_error,
                    ..
                } if call.as_str() == "private-call" => Some((name.as_ref().to_owned(), *is_error)),
                _ => None,
            });
            if let Some((name, is_error)) = &private_result {
                self.private_result_seen.store(true, Ordering::SeqCst);
                self.private_result_succeeded.store(
                    !*is_error && name.as_str() == self.private_name.as_str(),
                    Ordering::SeqCst,
                );
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            if private_result.is_some() {
                Ok(tool_stream(
                    "forward-session-call",
                    "read",
                    r#"{"path":"synthetic.txt"}"#,
                    StopReason::ToolUse,
                ))
            } else {
                Ok(tool_stream(
                    "private-call",
                    &self.private_name,
                    r#"{"value":"inside"}"#,
                    StopReason::ToolUse,
                ))
            }
        })
    }
}

struct EchoPrivateTool {
    name: Name,
    spec: Arc<ToolSpec>,
    calls: Arc<AtomicUsize>,
}

impl Tool for EchoPrivateTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(
        &'a self,
        _call: dal_agent::ext::tool::ToolCall,
        _cx: ToolCx<'a>,
    ) -> BoxFuture<'a, ToolOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { ToolOutcome::Ok(ToolOutput::from_text("private result")) })
    }
}

fn private_tool(calls: Arc<AtomicUsize>) -> (PrivateTool, String) {
    let name = Name::parse("private_echo").expect("valid private tool name");
    let spec = Arc::new(ToolSpec {
        name: name.clone(),
        description: "Return one private tool result.".into(),
        parameters: RawJson::parse(
            r#"{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}"#,
        )
        .expect("valid private tool schema"),
        grammar: None,
    });
    (
        PrivateTool(Arc::new(EchoPrivateTool {
            name: name.clone(),
            spec,
            calls,
        })),
        name.as_str().to_owned(),
    )
}

fn model_request(id: &str) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::from_id(id),
        system: "".into(),
        tools: Vec::new().into(),
        context: Vec::<ContextItem>::new().into(),
        params: RequestParams::default(),
        cache_key: None,
    }
}

fn text_stream(text: &str) -> EventStream {
    provider_stream(vec![
        ProviderEvent::TextDelta {
            text: text.to_owned(),
        },
        ProviderEvent::ToolCallsDone { calls: Vec::new() },
        ProviderEvent::Usage {
            usage: zero_usage(),
        },
        ProviderEvent::Stop {
            reason: StopReason::EndTurn,
        },
    ])
}

fn tool_stream(id: &str, name: &str, args: &str, reason: StopReason) -> EventStream {
    provider_stream(vec![
        ProviderEvent::ToolCallStarted {
            id: id.to_owned(),
            name: name.to_owned(),
        },
        ProviderEvent::ToolArgsDelta {
            id: id.to_owned(),
            fragment: args.as_bytes().to_vec(),
        },
        ProviderEvent::ToolCallsDone {
            calls: vec![ProviderToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                args: ToolArgs::from_bytes(args.as_bytes()),
            }],
        },
        ProviderEvent::Usage {
            usage: zero_usage(),
        },
        ProviderEvent::Stop { reason },
    ])
}

fn provider_stream(events: Vec<ProviderEvent>) -> EventStream {
    EventStream::new(
        futures::stream::iter(events.into_iter().map(Ok::<_, ProviderError>)),
        || {},
    )
}

fn zero_usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

fn has_tool_result(request: &ModelRequest, id: &str) -> bool {
    request
        .context
        .iter()
        .any(|item| matches!(item, ContextItem::ToolResult { call, .. } if call.as_str() == id))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn caps() -> Caps {
    Caps {
        context_window: Some(8192),
        thinking: Box::default(),
        tool_use: true,
        image_input: false,
        custom_grammar: false,
    }
}

fn model_product(
    data: &TestDir,
    model: &str,
) -> Result<(Config, Product), Box<dyn Error + Send + Sync>> {
    let factory = dalgon::product();
    let user = format!("model = \"{model}\"\n");
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    Ok((config, product))
}

#[tokio::test]
async fn synthetic_private_tools_and_forward_have_one_boundary()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    fs::write(workspace.path().join("synthetic.txt"), "session file")?;
    let (config, mut product) = model_product(&data, "gate/boundary")?;
    let private_calls = Arc::new(AtomicUsize::new(0));
    let (private, private_name) = private_tool(Arc::clone(&private_calls));
    let scope_call = Arc::new(Mutex::new(None));
    let second_forward = Arc::new(Mutex::new(None));
    let private_result_seen = Arc::new(AtomicBool::new(false));
    let private_result_succeeded = Arc::new(AtomicBool::new(false));
    let forward_child = Arc::new(ForwardChild {
        private_name,
        private_result_seen: Arc::clone(&private_result_seen),
        private_result_succeeded: Arc::clone(&private_result_succeeded),
        calls: AtomicUsize::new(0),
    });
    let extension = ExtensionBuilder::new("gate-boundary", "0.1.0", ServiceSet::EMPTY)?
        .model(ModelRecord {
            id: ModelId::parse("gate/boundary")?,
            caps: caps(),
            handler: Arc::new(BoundaryHandler {
                private,
                scope_call: Arc::clone(&scope_call),
                second_forward: Arc::clone(&second_forward),
            }),
            export: None,
        })
        .model(ModelRecord {
            id: ModelId::parse("gate/scope-child")?,
            caps: caps(),
            handler: Arc::new(ScopeChild),
            export: None,
        })
        .model(ModelRecord {
            id: ModelId::parse("gate/forward-child")?,
            caps: caps(),
            handler: forward_child.clone(),
            export: None,
        })
        .build()?;
    product.extensions.push(extension);
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut subscription = harness.agent.subscribe(None)?;
    let prompt = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Exercise the synthetic boundary.".into(),
            }],
        })
        .await?;
    assert!(matches!(prompt, Reply::Accepted { .. }));
    let (assistant_text, mut session_calls) = collect_boundary_stream(&mut subscription).await?;
    assert_eq!(forward_child.calls.load(Ordering::SeqCst), 2);
    assert_eq!(private_calls.load(Ordering::SeqCst), 1);
    assert!(private_result_seen.load(Ordering::SeqCst));
    assert!(private_result_succeeded.load(Ordering::SeqCst));
    assert!(matches!(
        lock(&scope_call).as_ref(),
        Some(StreamEvent::ToolCall { call, name, .. })
            if call.as_str() == "scope-only-call" && name.as_ref() == "read"
    ));
    let second = lock(&second_forward).take().unwrap();
    assert_eq!(second, ModelError::SecondForward);
    assert_eq!(assistant_text, "boundary complete");
    assert_eq!(session_calls.len(), 1);
    let forwarded = session_calls.pop().unwrap();
    assert_eq!(forwarded.0, "forward-session-call");
    assert_eq!(forwarded.1, "read");
    assert_eq!(forwarded.2, r#"{"path":"synthetic.txt"}"#);
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
