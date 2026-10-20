//! `/model <mode>` switches the harness mode, keeps the saved route, and names
//! the model by the route's own provider; a provider-less route keeps the
//! family label. Runs the real handler over a real host with routes set
//! directly by `SetModel`: no catalog and no credentials.

use std::collections::BTreeMap;
use std::sync::Arc;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Family, ModelId, ModelRequest, ModelRoute,
    Output, PageReq, Reply, Save, ServiceSet, ThinkingLevel, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent};

type Outcome<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// A model that answers without reaching any provider. No turn runs here;
/// the fixture only satisfies the config's default model line.
struct Fixture;

impl ModelHandler for Fixture {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async {
            let events: Vec<Result<StreamEvent, ProviderError>> = vec![
                Ok(StreamEvent::TextDelta {
                    text: "done".into(),
                }),
                Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
                Ok(StreamEvent::Usage {
                    usage: dal_core::Usage {
                        input_tokens: 1,
                        cached_input_tokens: 0,
                        output_tokens: 1,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        cost_usd: None,
                        premium_requests: None,
                    },
                }),
                Ok(StreamEvent::Stop {
                    reason: StopReason::EndTurn,
                }),
            ];
            Ok(EventStream::new(futures::stream::iter(events), || {}))
        })
    }
}

/// A `/reload` seam the test never calls.
struct NoPlugins;

impl dal_ext::commands::PluginReload for NoPlugins {
    fn reload<'a>(
        &'a self,
        _cx: &'a dal_agent::ext::command::CommandCx<'a>,
    ) -> BoxFuture<'a, Result<dal_agent::ext::command::ReloadSummary, dal_core::command::ErrorTriple>>
    {
        Box::pin(async {
            Err(dal_core::command::ErrorTriple {
                what: "plugins unavailable".into(),
                why: "the test host publishes no plugins".into(),
                fix: "run without /reload".into(),
            })
        })
    }
}

async fn host(root: &std::path::Path) -> Outcome<Host> {
    let no_reload: Arc<dyn dal_ext::commands::PluginReload> = Arc::new(NoPlugins);
    let commands = dal_ext::commands::extension(&no_reload)?;
    let model = ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse("fixture/end")?,
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Fixture),
            export: None,
        })
        .build()?;
    let data = root.join("data");
    std::fs::create_dir_all(&data)?;
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"fixture/end\"\n"),
    )?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![commands, model],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: root.join("workspace"),
        sandbox_helper: None,
    };
    Ok(Host::start(product, config, env).await?)
}

#[tokio::test]
async fn mode_line_prints_the_saved_route_provider_or_the_legacy_family_label() -> Outcome<()> {
    let root =
        std::env::temp_dir().join(format!("dal-ext-model-route-text-{}", std::process::id()));
    std::fs::create_dir_all(root.join("workspace"))?;
    let host = host(&root).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(root.join("workspace"))?,
                name: None,
            },
            ClientId::new("model-route-text"),
        )
        .await?;

    for (provider, expected, forbidden) in [
        (Some("openrouter"), "openrouter/gpt-4o", "openai/"),
        (None, "openai/gpt-4o", "openrouter"),
    ] {
        let route = ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-4o".into(),
            provider: provider.map(Into::into),
        };
        agent
            .submit(Command::SetModel {
                model: route.clone(),
                save: Save::SessionOnly,
            })
            .await?;
        let reply = agent
            .submit(Command::Run {
                name: "model".into(),
                args: "eval-first".into(),
                expected: None,
            })
            .await?;

        let view = agent.view(PageReq::default())?;
        assert_eq!(
            view.settings.model.as_ref(),
            Some(&route),
            "the session holds the selected route across the mode switch"
        );
        let Reply::Done(Output::Text(text)) = &reply else {
            return Err(format!("`/model eval-first` did not reply with text: {reply:?}").into());
        };
        assert!(
            text.contains(expected),
            "the reply names {expected}: {text}"
        );
        assert!(
            !text.contains(forbidden),
            "the reply does not name {forbidden}: {text}"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
