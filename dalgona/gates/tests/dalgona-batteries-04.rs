// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Auto-thinking changes only the typed thinking field of the request.
use dalgona_gates::support;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::ext::{BoxFuture, ExtensionBuilder, Hook, HookCx, HookError};
use dal_agent::{Host, SessionRef};
use dal_core::ext::BeforeRequest;
use dal_core::{ClientId, Command, Expect, Origin, Part, RequestParams, ThinkingLevel, Workspace};

const WAIT: Duration = Duration::from_secs(30);
const NO_BATTERIES: &str = "disabled_batteries = [\"ask\", \"history\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]";
/// The judge's classification reply: the first thinking level.
const JUDGE_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"{\"answers\":[0]}"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

#[derive(Debug, Clone)]
struct Captured {
    round: u32,
    thinking: ThinkingLevel,
    effort: Option<Box<str>>,
    temperature: Option<f64>,
    max_output_tokens: Option<u32>,
    levels: Vec<ThinkingLevel>,
}

struct Probe {
    calls: Arc<Mutex<Vec<Captured>>>,
}

impl Hook<BeforeRequest, Option<RequestParams>> for Probe {
    fn call(
        &self,
        event: BeforeRequest,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<RequestParams>, HookError>> {
        let captured = Captured {
            round: event.round,
            thinking: event.params.thinking,
            effort: event.params.effort.clone(),
            temperature: event.params.temperature,
            max_output_tokens: event.params.max_output_tokens,
            levels: event.caps.thinking.to_vec(),
        };
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(captured);
        }
        Box::pin(async move { Ok(None) })
    }
}

/// Runs one scripted turn on the full product with a request probe attached,
/// and returns every captured typed request.
async fn captured_requests(
    label: &str,
    judged_toml: &str,
    steps: &[String],
    calls: Arc<Mutex<Vec<Captured>>>,
) -> support::TestResult<()> {
    let scratch = support::Scratch::new(label)?;
    let root = scratch.path().to_path_buf();
    std::fs::create_dir_all(root.join("workspace"))?;
    let script = root.join("script.jsonl");
    std::fs::write(&script, steps.join("\n"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n{NO_BATTERIES}\n{judged_toml}\n[providers.scripted]\nfixture = {:?}\n",
        script.display().to_string()
    );
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        Some(&user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: root.clone(),
        config: &config,
    })?;
    // Bundled and named after `judged`, so the probe observes the typed
    // request after the battery's before-request hooks ran.
    let probe = ExtensionBuilder::new("probe", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .with_origin(Origin::Bundled, None)
        .on_before_request(Probe { calls })
        .build()?;
    product.extensions.push(probe);
    let env = dal_agent::Env {
        vars: std::collections::BTreeMap::from([("OPENAI_API_KEY".into(), "sk-test".into())]),
        cwd: root.clone(),
        sandbox_helper: None,
    };
    let host: Host = Host::start(product, config, env).await?;
    let workspace = Workspace::new(root.join("workspace"))?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("judged-thinking-gate"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "summarize the fixture notes".into(),
            }],
        })
        .await?;
    while let Some(delivery) = tokio::time::timeout(WAIT, subscription.next()).await? {
        if let dal_agent::Delivery::Update(update) = delivery
            && let dal_core::UpdateKind::TurnEnded { .. } = &update.kind
        {
            break;
        }
    }
    let key = agent.view(dal_core::PageReq::default())?.session.id;
    host.close(key).await?;
    drop(subscription);
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}

fn round_zero(calls: &[Captured]) -> Vec<&Captured> {
    calls.iter().filter(|call| call.round == 0).collect()
}

#[test]
fn auto_thinking_changes_only_typed_request_params() -> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // Thinking on: the judge picks the first level and only the thinking
        // field of the typed request moves.
        let on_calls = Arc::new(Mutex::new(Vec::new()));
        captured_requests(
            "judged-thinking-on",
            "[plugin.judged]\nthinking = true",
            &[JUDGE_STEP.to_owned(), support::text_step("done")],
            Arc::clone(&on_calls),
        )
        .await?;
        // The guard's scope ends with this block, before the next await;
        // only the compared fields outlive it.
        let on_main = {
            let on = on_calls.lock().map_err(|_| "probe lock")?;
            let on_zero = round_zero(&on);
            assert!(!on_zero.is_empty(), "the turn issued a request");
            let main = on_zero.last().ok_or("the turn issued a request")?;
            let first = main
                .levels
                .first()
                .ok_or("the model publishes thinking levels")?;
            assert_eq!(
                main.thinking, *first,
                "thinking follows the judge's first choice"
            );
            (
                main.thinking,
                main.effort.clone(),
                main.temperature,
                main.max_output_tokens,
            )
        };
        let (on_thinking, on_effort, on_temperature, on_max_output) = on_main;
        // Thinking off: every request keeps the default level.
        let off_calls = Arc::new(Mutex::new(Vec::new()));
        captured_requests(
            "judged-thinking-off",
            "[plugin.judged]\nthinking = false",
            &[support::text_step("done")],
            Arc::clone(&off_calls),
        )
        .await?;
        let off = off_calls.lock().map_err(|_| "probe lock")?;
        let off_zero = round_zero(&off);
        assert_eq!(off_zero.len(), 1, "one request, no judge call");
        assert_eq!(
            off_zero[0].thinking,
            ThinkingLevel::Medium,
            "thinking keeps the session default without the classifier"
        );
        // Every other typed field is identical between the two mains.
        assert_eq!(on_effort, off_zero[0].effort, "effort is untouched");
        assert_eq!(
            on_temperature, off_zero[0].temperature,
            "temperature is untouched"
        );
        assert_eq!(
            on_max_output, off_zero[0].max_output_tokens,
            "output cap is untouched"
        );
        assert_ne!(
            on_thinking, off_zero[0].thinking,
            "only the thinking field moves"
        );
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
