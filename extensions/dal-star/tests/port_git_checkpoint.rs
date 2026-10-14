//! Real-host boundary proof for the git-checkpoint example plugin.
//!
//! The checked-in plugin lists and applies real Git stashes through the
//! approved process service; it never creates one.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::Duration;

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, PageReq, Part, Reply,
    ToolOutcomeView, UpdateKind, Workspace,
};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};

const TOOL: &str = "git-checkpoint__checkpoint";
const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";
const PLUGIN_SOURCE: &str = include_str!("../../../examples/plugins/git-checkpoint/plugin.star");
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
    let mut extensions = vec![dal_tools::extension(dal_tools::ToolsConfig::default())?];
    extensions.extend(PluginSystem::new(generation, roots, plugin_config).extensions()?);

    let fixture = data.join("git-checkpoint-scripted.jsonl");
    std::fs::write(&fixture, checkpoint_replay())?;
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

/// One scripted model step that calls the checkpoint tool; `id` is plain ASCII.
fn tool_step(id: &str, args: &str) -> String {
    let id = format!("\"{id}\"");
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":{id},\"name\":\"{TOOL}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":{id},\"name\":\"{TOOL}\",\"args\":{{\"kind\":\"parsed\",\"value\":{args}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

/// One scripted model step that ends the turn; `text` is plain ASCII.
fn text_step(text: &str) -> String {
    let text = format!("\"{text}\"");
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":{text}}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

fn checkpoint_replay() -> String {
    [
        tool_step("empty-list", r#"{"action":"list"}"#),
        text_step("listed nothing"),
        tool_step("list", r#"{"action":"list"}"#),
        text_step("listed"),
        tool_step("restore", r#"{"action":"restore","n":0}"#),
        text_step("restored"),
    ]
    .concat()
}

/// Runs one turn whose scripted model calls the checkpoint tool, approves
/// each `git` run as the user, and returns the tool's settled outcome.
async fn checkpoint_turn(agent: &Agent) -> Result<ToolOutcomeView, Box<dyn Error + Send + Sync>> {
    let mut updates = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Use the checkpoint tool.".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(std::io::Error::other(format!("prompt was not accepted: {reply:?}")).into());
    }
    let mut settled = None;
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(30), updates.next()).await?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err(std::io::Error::other("the update stream closed before turn end").into());
        };
        match &update.kind {
            UpdateKind::RequestOpened(request)
                if agent
                    .view(PageReq::default())?
                    .open
                    .iter()
                    .any(|open| open.id == request.id) =>
            {
                agent.answer(request.id, Answer::Approve).await?;
            }
            UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    settled.ok_or_else(|| std::io::Error::other("the checkpoint tool never settled").into())
}

#[tokio::test(flavor = "multi_thread")]
async fn git_checkpoint_lists_and_restores_a_real_stash() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let (_root, host, agent, workspace) = open_host().await?;
    let empty = checkpoint_turn(&agent).await?;
    assert!(!empty.is_error, "{empty:?}");
    assert_eq!(&*empty.text, "no checkpoints");

    std::fs::write(workspace.join("tracked.txt"), "changed\n")?;
    git(
        &workspace,
        &["stash", "push", "-q", "-m", "before the review"],
    )?;
    assert_eq!(
        std::fs::read_to_string(workspace.join("tracked.txt"))?,
        "before\n"
    );
    let listed = checkpoint_turn(&agent).await?;
    assert!(
        !listed.is_error
            && listed.text.contains("stash@{0}")
            && listed.text.contains("before the review"),
        "list must print the real stash: {listed:?}"
    );

    let restored = checkpoint_turn(&agent).await?;
    assert!(!restored.is_error, "{restored:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("tracked.txt"))?,
        "changed\n",
        "restore must apply the selected stash"
    );

    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}
