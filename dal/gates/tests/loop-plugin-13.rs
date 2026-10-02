#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]

//! Synthetic model recursion, private rounds, and USD admission expose typed failures.

mod support;

use std::{
    collections::BTreeMap,
    error::Error,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use dal_agent::{
    Env, Product, SessionRef,
    ext::{
        ArgError, BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
        ModelRecord, PrivateTool, RawValue, ScopeError, ScopeValue, Tool, ToolCx, ToolOutcome,
        ToolOutput,
    },
};
use dal_core::{
    Budget, Caps, Command, Config, ConfigProduct, ContextItem, Expect, InferFailure, ModelId,
    ModelInfo, ModelRequest, ModelRoute, Name, OnError, Part, Purpose, RawJson, Reply,
    RequestParams, ScopeSpec, ServiceSet, StreamChannel, ToolClass, ToolSpec, UpdateKind, Usage,
    Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent as ProviderEvent};
use support::{TestDir, scripted_session};

#[derive(Default)]
struct ObservedErrors {
    cycle: Option<ModelError>,
    depth: Option<ModelError>,
    unpriced: Option<ScopeError>,
    private_rounds: Option<ProviderError>,
}

struct ProbeHandler {
    private: PrivateTool,
    state: Arc<Mutex<ObservedErrors>>,
}

impl ModelHandler for ProbeHandler {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let scope = cx
                .scope(ScopeSpec {
                    limit: 2,
                    on_error: OnError::Settle,
                    budget: Budget::default(),
                })
                .expect("probe scope is valid");
            let cycle = scope
                .infer(model_request("gate/cycle"))
                .expect("cycle probe is admitted");
            let cycle_result = cycle.result().await.expect("cycle probe completes");
            let ScopeValue::Inference(_) = cycle_result else {
                panic!("cycle probe returned a member report");
            };
            let depth = scope
                .infer(model_request("gate/depth-0"))
                .expect("depth probe is admitted");
            let depth_result = depth.result().await.expect("depth probe completes");
            let ScopeValue::Inference(_) = depth_result else {
                panic!("depth probe returned a member report");
            };
            let usd_scope = cx
                .scope(ScopeSpec {
                    limit: 1,
                    on_error: OnError::Settle,
                    budget: Budget {
                        usd: Some(0.01),
                        ..Budget::default()
                    },
                })
                .expect("USD-limited scope is valid");
            let unpriced = usd_scope.infer(model_request("gate/no-price")).err();
            lock(&self.state).unpriced = unpriced;
            let mut round_request = request;
            round_request.model = ModelRoute::from_id("gate/round-model");
            let private = [self.private.clone()];
            let mut stream = cx.forward(round_request, &private).await?;
            let mut private_rounds = None;
            while let Some(event) = stream.next().await {
                if let Err(error) = event {
                    private_rounds = Some(error);
                    break;
                }
            }
            lock(&self.state).private_rounds = private_rounds;
            Ok(text_stream("synthetic probes complete"))
        })
    }
}

struct CycleHandler {
    state: Arc<Mutex<ObservedErrors>>,
}

impl ModelHandler for CycleHandler {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let mut repeated = request;
            repeated.model = ModelRoute::from_id("gate/cycle");
            if let Err(error) = cx.forward(repeated, &[]).await {
                lock(&self.state).cycle = Some(error)
            }
            Ok(text_stream("cycle checked"))
        })
    }
}

struct DepthHandler {
    next: Option<String>,
    state: Arc<Mutex<ObservedErrors>>,
}

impl ModelHandler for DepthHandler {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let Some(next) = &self.next else {
                return Ok(text_stream("depth leaf"));
            };
            let mut nested = request;
            nested.model = ModelRoute::from_id(next);
            match cx.forward(nested, &[]).await {
                Err(error) => {
                    lock(&self.state).depth = Some(error);
                    Ok(text_stream("depth boundary checked"))
                }
                Ok(stream) => Ok(stream),
            }
        })
    }
}

struct RoundModel {
    private_name: String,
    calls: Arc<AtomicUsize>,
    max_private_results: Arc<AtomicUsize>,
}

impl ModelHandler for RoundModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let completed_private = request
            .context
            .iter()
            .filter(|item| {
                matches!(
                    item,
                    ContextItem::ToolResult {
                        name,
                        is_error: false,
                        ..
                    } if name.as_ref() == self.private_name.as_str()
                )
            })
            .count();
        self.max_private_results
            .fetch_max(completed_private, Ordering::SeqCst);
        let call_id = format!("private-round-{call_index}");
        let private_name = self.private_name.clone();
        Box::pin(async move {
            Ok(tool_stream(
                &call_id,
                &private_name,
                r#"{"value":"round"}"#,
                StopReason::ToolUse,
            ))
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
            calls: vec![dal_provider::ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                args: dal_provider::ToolArgs::from_bytes(args.as_bytes()),
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

fn model_record(
    id: &str,
    handler: Arc<dyn ModelHandler>,
) -> Result<ModelRecord, Box<dyn Error + Send + Sync>> {
    Ok(ModelRecord {
        id: ModelId::parse(id)?,
        caps: caps(),
        handler,
        export: None,
    })
}

fn model_product(data: &TestDir) -> Result<(Config, Product), Box<dyn Error + Send + Sync>> {
    let factory = dalgon::product();
    let user = "model = \"gate/probe\"\n";
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    Ok((config, product))
}

#[tokio::test]
async fn synthetic_cycle_depth_round_and_unpriced_errors_are_typed()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let (config, mut product) = model_product(&data)?;
    let private_calls = Arc::new(AtomicUsize::new(0));
    let (private, private_name) = private_tool(Arc::clone(&private_calls));
    let state = Arc::new(Mutex::new(ObservedErrors::default()));
    let calls = Arc::new(AtomicUsize::new(0));
    let max_private_results = Arc::new(AtomicUsize::new(0));
    let mut builder = ExtensionBuilder::new("gate-model-probes", "0.1.0", ServiceSet::EMPTY)?
        .model(model_record(
            "gate/probe",
            Arc::new(ProbeHandler {
                private,
                state: Arc::clone(&state),
            }),
        )?)
        .model(model_record(
            "gate/cycle",
            Arc::new(CycleHandler {
                state: Arc::clone(&state),
            }),
        )?);
    for index in 0..6 {
        let next = (index < 5).then(|| format!("gate/depth-{}", index + 1));
        builder = builder.model(model_record(
            &format!("gate/depth-{index}"),
            Arc::new(DepthHandler {
                next,
                state: Arc::clone(&state),
            }),
        )?);
    }
    builder = builder.model(model_record(
        "gate/round-model",
        Arc::new(RoundModel {
            private_name,
            calls: Arc::clone(&calls),
            max_private_results: Arc::clone(&max_private_results),
        }),
    )?);
    product.extensions.push(builder.build()?);
    let env = Env {
        vars: BTreeMap::new(),
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
                text: "Run typed synthetic model probes.".into(),
            }],
        })
        .await?;
    assert!(matches!(prompt, Reply::Accepted { .. }));
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
            UpdateKind::ToolStarted { call, .. } => session_calls.push(call.as_str().to_owned()),
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    let mut observed = lock(&state);
    let cycle = observed.cycle.take().unwrap();
    let ModelError::SyntheticCycle { chain } = cycle else {
        panic!("cycle probe did not return SyntheticCycle");
    };
    assert_eq!(
        chain.iter().map(ModelId::as_str).collect::<Vec<_>>(),
        ["gate/probe", "gate/cycle", "gate/cycle"]
    );
    let depth = observed.depth.take().unwrap();
    let ModelError::SyntheticDepth { chain } = depth else {
        panic!("depth probe did not return SyntheticDepth");
    };
    assert_eq!(
        chain.iter().map(ModelId::as_str).collect::<Vec<_>>(),
        [
            "gate/probe",
            "gate/depth-0",
            "gate/depth-1",
            "gate/depth-2",
            "gate/depth-3",
        ]
    );
    let unpriced = observed.unpriced.take().unwrap();
    assert!(matches!(
        unpriced,
        ScopeError::UnpricedModel { model } if model.as_ref() == "gate/no-price"
    ));
    let private_failure = observed.private_rounds.take().unwrap();
    let expected_private_rounds = ModelError::PrivateRounds.to_string();
    assert!(matches!(
        private_failure,
        ProviderError::Synthetic(InferFailure::Fatal { message, fix })
            if message.as_ref() == expected_private_rounds.as_str() && fix.is_none()
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 9);
    assert_eq!(private_calls.load(Ordering::SeqCst), 8);
    assert_eq!(max_private_results.load(Ordering::SeqCst), 8);
    assert!(session_calls.is_empty());
    assert_eq!(assistant_text, "synthetic probes complete");
    drop(observed);
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
