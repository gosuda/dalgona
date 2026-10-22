//! Built-in commands through a real host: the `/session` table, the
//! `/scoped-models` catalog gate, and the bare `/model` and `/settings`
//! pickers.
//!
//! `CommandCx` mints only inside `dal-agent` (`CommandHost`/`CommandCx::new`
//! are `pub(crate)`), so these handler paths cannot be reached from `dal-ext`
//! unit tests; they are driven here through `Agent::submit`, the same
//! dispatch the product uses.

use std::collections::BTreeMap;
use std::sync::Arc;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Env, Host, Product, SessionRef};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, ModelId, ModelRequest, Output, Reply,
    ServiceSet, ThinkingLevel, Workspace,
};

type Outcome<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Never invoked: these tests submit commands only, never prompts.
struct NeverInfer;

impl ModelHandler for NeverInfer {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async { unreachable!("commands_host submits no prompts") })
    }
}

/// Never invoked: these tests never submit `/reload`.
struct NeverReload;

impl dal_ext::commands::PluginReload for NeverReload {
    fn reload<'a>(
        &'a self,
        _cx: &'a dal_agent::ext::command::CommandCx<'a>,
    ) -> BoxFuture<'a, Result<dal_agent::ext::command::ReloadSummary, dal_core::command::ErrorTriple>>
    {
        Box::pin(async { unreachable!("commands_host submits no /reload") })
    }
}

async fn agent() -> Outcome<(Agent, TempRoot)> {
    let root = tempfile_root::new("dal-ext-commands-host")?;
    let data = root.path().join("data");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(root.path().join("workspace"))?;
    let workspace = Workspace::new(root.path().join("workspace"))?;
    let reload: Arc<dyn dal_ext::commands::PluginReload> = Arc::new(NeverReload);
    let commands = dal_ext::commands::extension(&reload)?;
    let models = ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse("fixture/end")?,
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(NeverInfer),
            export: None,
        })
        .build()?;
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
        extensions: vec![commands, models],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: root.path().join("workspace"),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("commands-host"),
        )
        .await?;
    Ok((agent, root))
}

mod tempfile_root {
    use std::path::{Path, PathBuf};

    pub(crate) struct Root(PathBuf);

    impl Root {
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn new(tag: &str) -> std::io::Result<Root> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .map_err(std::io::Error::other)?;
        let dir = std::env::temp_dir().join(format!("{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Root(dir))
    }
}
use tempfile_root::Root as TempRoot;

async fn run(agent: &Agent, name: &str) -> Outcome<Reply> {
    Ok(agent
        .submit(Command::Run {
            name: name.into(),
            args: "".into(),
            expected: None,
        })
        .await?)
}

/// The `/session` table renders the real agent session in the Id row and
/// represents an unpriced model without claiming a cost figure.
#[tokio::test]
async fn session_table_renders_real_session_id_and_unpriced_cost() -> Outcome<()> {
    let (agent, _root) = agent().await?;
    let reply = run(&agent, "session").await?;
    let Reply::Done(Output::Table(rows)) = reply else {
        return Err(format!("/session did not return a table: {reply:?}").into());
    };
    let cell = |label: &str| -> String {
        rows.iter()
            .find(|row| row.first().is_some_and(|first| &**first == label))
            .and_then(|row| row.get(1))
            .map_or_else(
                || panic!("expected a {label} row with a value"),
                ToString::to_string,
            )
    };
    assert_eq!(cell("Id"), agent.session().to_string(), "{rows:?}");
    let cost = cell("Cost");
    assert!(
        !cost.is_empty() && !cost.chars().any(|c| c.is_ascii_digit()),
        "unpriced model must not render a cost figure: {cost:?}"
    );
    Ok(())
}

/// `/scoped-models` with no cached catalog is refused through the live
/// dispatcher rather than opening a chooser, and the refusal points at
/// `/model` as the recovery step.
#[tokio::test]
async fn scoped_models_without_cached_catalog_reports_no_list() -> Outcome<()> {
    let (agent, _root) = agent().await?;
    let error = run(&agent, "scoped-models").await.unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("/model"),
        "no-catalog refusal must point at /model for recovery: {message}"
    );
    Ok(())
}

/// Bare `/model` and `/settings` open their pickers with an empty filter
/// through the live dispatcher.
#[tokio::test]
async fn bare_model_and_settings_open_their_choosers() -> Outcome<()> {
    use dal_core::command::Chooser;

    let (agent, _root) = agent().await?;
    assert_eq!(
        run(&agent, "model").await?,
        Reply::Choose {
            chooser: Chooser::Model,
            filter: "".into(),
        }
    );
    assert_eq!(
        run(&agent, "settings").await?,
        Reply::Choose {
            chooser: Chooser::Settings,
            filter: "".into(),
        }
    );
    Ok(())
}
