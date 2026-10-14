//! Real-host boundary proof for the git-checkpoint example plugin.
//!
//! The fixture patch tool writes through the host tool loop and reports its
//! changed path. The checked-in plugin must then create one real Git stash
//! through the approved process service and restore it through its command.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, Part, Reply, ToolClass, ToolSpec, UpdateKind,
    Visibility, Workspace,
};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};

const PLUGIN_SOURCE: &str = include_str!("../../../examples/plugins/git-checkpoint/plugin.star");
const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

struct FixturePatch {
    name: dal_core::Name,
    spec: Arc<ToolSpec>,
}

impl Tool for FixturePatch {
    fn name(&self) -> &dal_core::Name {
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
        Ok(ToolClass::Patch)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let path = cx.workspace().as_path().join("tracked.txt");
            if let Err(error) = std::fs::write(&path, "changed\n") {
                return ToolOutcome::Err(dal_agent::ToolError::message(error.to_string()));
            }
            ToolOutcome::Ok(Box::new(ToolOutput {
                parts: vec![Part::Text {
                    text: "patched tracked.txt".into(),
                }],
                files_changed: vec![path],
                data: None,
            }))
        })
    }
}

fn fixture_patch() -> Result<dal_agent::ext::Extension, Box<dyn Error + Send + Sync>> {
    let name = dal_core::Name::parse("patch")?;
    let spec = Arc::new(ToolSpec {
        name: name.clone(),
        description: "Change the tracked fixture file.".into(),
        parameters: dal_core::RawJson::parse(r#"{"type":"object","properties":{}}"#)?,
        grammar: None,
    });
    Ok(
        ExtensionBuilder::new("fixture-patch", "0.1.0", dal_core::ServiceSet::EMPTY)?
            .tool(Arc::new(FixturePatch { name, spec }), Visibility::Model)
            .build()?,
    )
}

fn patch_replay() -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"patch-call\",\"name\":\"patch\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"patch-call\",\"name\":\"patch\",\"args\":{{\"kind\":\"parsed\",\"value\":{{}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"finished\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}}, {USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

#[expect(
    clippy::disallowed_methods,
    reason = "the fixture repository is driven by the real git binary"
)]
fn git(repo: &Path, args: &[&str]) -> Result<String, Box<dyn Error + Send + Sync>> {
    let output = ProcessCommand::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.join(".gitconfig-global"))
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

async fn open_host()
-> Result<(tempfile::TempDir, Host, Agent, PathBuf), Box<dyn Error + Send + Sync>> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let workspace = root.path().join("repo");
    std::fs::create_dir_all(data.join("plugins/git-checkpoint"))?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(
        data.join("plugins/git-checkpoint/plugin.star"),
        PLUGIN_SOURCE,
    )?;
    std::fs::write(workspace.join("tracked.txt"), "before\n")?;
    git(&workspace, &["init", "-q"])?;
    git(
        &workspace,
        &["config", "user.email", "test@example.invalid"],
    )?;
    git(&workspace, &["config", "user.name", "dal test"])?;
    git(&workspace, &["add", "tracked.txt"])?;
    git(&workspace, &["commit", "-q", "-m", "initial"])?;

    let roots = LoadRoots {
        data_root: data.clone(),
        bundled: Vec::new(),
    };
    let plugin_config = PluginsConfig::default();
    let generation = load(&roots, &plugin_config)?;
    let mut extensions = vec![fixture_patch()?];
    extensions.extend(PluginSystem::new(generation, roots, plugin_config).extensions()?);

    let fixture = data.join("git-checkpoint-scripted.jsonl");
    std::fs::write(&fixture, patch_replay())?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;
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
            (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
            (
                OsString::from("GIT_CONFIG_GLOBAL"),
                workspace.join(".gitconfig-global").into_os_string(),
            ),
        ]),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.clone())?,
                name: None,
            },
            ClientId::new("git-checkpoint-test"),
        )
        .await?;
    Ok((root, host, agent, workspace))
}

async fn run_patch_turn(agent: &Agent) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut updates = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Apply the patch.".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(std::io::Error::other(format!("prompt was not accepted: {reply:?}")).into());
    }
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(30), updates.next()).await?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err(std::io::Error::other("the update stream closed before turn end").into());
        };
        if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
            return Ok(());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn git_checkpoint_uses_real_host_stash_and_restores_it()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let (_root, host, agent, workspace) = open_host().await?;
    run_patch_turn(&agent).await?;

    let stashes = git(&workspace, &["stash", "list"])?;
    let entries = stashes.lines().collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        1,
        "one patch/turn state must create exactly one checkpoint: {entries:?}"
    );
    assert!(
        entries[0].contains("dalgon: turn "),
        "checkpoint message must identify its turn: {entries:?}"
    );

    git(&workspace, &["checkout", "--", "tracked.txt"])?;
    let reply = agent
        .submit(Command::Run {
            name: "git-checkpoint:checkpoint".into(),
            args: "restore 0".into(),
            expected: Some(Expect::Idle),
        })
        .await?;
    assert!(
        matches!(reply, Reply::Done(_)),
        "restore command failed: {reply:?}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("tracked.txt"))?,
        "changed\n",
        "restore must apply the recorded checkpoint"
    );

    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}
