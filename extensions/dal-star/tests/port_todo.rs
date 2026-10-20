//! Real-host proof for the todo example's state and command behavior.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, Output, Part, Reply, ToolOutcomeView,
    UpdateKind, Workspace,
};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};

const TODO_TOOL: &str = "todo__todo";
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;
const PLUGIN_SOURCE: &str = include_str!("../../../examples/plugins/todo/plugin.star");

fn tool_step(id: &str, args: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"{id}","name":"{TODO_TOOL}"}},{{"type":"tool_calls_done","calls":[{{"id":"{id}","name":"{TODO_TOOL}","args":{{"kind":"parsed","value":{args}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}
"#
    )
}

fn text_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{text}"}},{{"type":"tool_calls_done","calls":[]}}, {USAGE},{{"type":"stop","reason":"end_turn"}}]}}
"#
    )
}

fn durable_replay() -> String {
    [
        tool_step("todo-add-one", r#"{"action":"add","text":"first"}"#),
        tool_step("todo-add-two", r#"{"action":"add","text":"second"}"#),
        tool_step("todo-list", r#"{"action":"list"}"#),
        tool_step("todo-toggle-on", r#"{"action":"toggle","id":1}"#),
        tool_step("todo-list-completed", r#"{"action":"list"}"#),
        tool_step("todo-toggle-off", r#"{"action":"toggle","id":1}"#),
        tool_step("todo-unknown", r#"{"action":"toggle","id":9}"#),
        tool_step("todo-missing-text", r#"{"action":"add"}"#),
        tool_step("todo-missing-id", r#"{"action":"toggle"}"#),
        text_step("done"),
    ]
    .concat()
}

fn ephemeral_replay() -> String {
    [
        tool_step(
            "todo-ephemeral-add",
            r#"{"action":"add","text":"ephemeral item"}"#,
        ),
        text_step("done"),
    ]
    .concat()
}

fn extensions(data: &Path) -> Result<Vec<dal_agent::ext::Extension>, Box<dyn Error + Send + Sync>> {
    let roots = LoadRoots {
        data_root: data.to_path_buf(),
        bundled: Vec::new(),
    };
    let config = PluginsConfig::default();
    let generation = load(&roots, &config)?;
    Ok(PluginSystem::new(generation, roots, config).extensions()?)
}

async fn start_host(
    data: &Path,
    workspace: &Path,
    fixture: &Path,
) -> Result<Host, Box<dyn Error + Send + Sync>> {
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, data, "", Some(&user))?;
    let product = Product {
        name: "dal",
        data_root: data.to_path_buf(),
        defaults: "",
        extensions: extensions(data)?,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.to_path_buf(),
        sandbox_helper: None,
    };
    Ok(Host::start(product, config, env).await?)
}

async fn run_turn(
    agent: &dal_agent::Agent,
    prompt: &str,
) -> Result<Vec<ToolOutcomeView>, Box<dyn Error + Send + Sync>> {
    let mut updates = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: prompt.into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(std::io::Error::other(format!("prompt was not accepted: {reply:?}")).into());
    }

    let mut outcomes = Vec::new();
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(30), updates.next())
            .await
            .map_err(|_| std::io::Error::other("todo turn timed out"))?
            .ok_or_else(|| std::io::Error::other("todo subscription closed"))?;
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) => {
                agent.answer(request.id, Answer::ApproveForSession).await?;
            }
            UpdateKind::ToolSettled { outcome, .. } => outcomes.push(outcome.clone()),
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    Ok(outcomes)
}

async fn run_command(
    agent: &dal_agent::Agent,
    args: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut updates = agent.subscribe(None)?;
    let command = agent.submit(Command::Run {
        name: "todo:todos".into(),
        args: args.into(),
        expected: None,
    });
    tokio::pin!(command);
    let reply = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            tokio::select! {
                biased;
                delivery = updates.next() => {
                    let delivery = delivery
                        .ok_or_else(|| std::io::Error::other("todo command subscription closed"))?;
                    let Delivery::Update(update) = delivery else {
                        return Err::<Reply, Box<dyn Error + Send + Sync>>(std::io::Error::other(
                            "todo command subscription requires resynchronization",
                        )
                        .into());
                    };
                    if let UpdateKind::RequestOpened(request) = &update.kind {
                        agent.answer(request.id, Answer::ApproveForSession).await?;
                    }
                }
                result = &mut command => break Ok(result?),
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other("todo command timed out"))??;
    match reply {
        Reply::Done(Output::Text(text)) => Ok(text.into()),
        other => {
            Err(std::io::Error::other(format!("unexpected todo command reply: {other:?}")).into())
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn todo_tool_uses_durable_state_and_command_boundary()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(data.join("plugins/todo"))?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(data.join("plugins/todo/plugin.star"), PLUGIN_SOURCE)?;
    let fixture = data.join("todo-scripted.jsonl");
    std::fs::write(&fixture, durable_replay())?;

    let host = start_host(&data, &workspace, &fixture).await?;
    let workspace_ref = Workspace::new(workspace.clone())?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace_ref,
                name: Some("todo-session".into()),
            },
            ClientId::new("todo-test"),
        )
        .await?;
    let session = agent.session();
    let outcomes = run_turn(&agent, "exercise the todo list").await?;
    assert_eq!(outcomes.len(), 9, "every scripted todo call must settle");
    let texts: Vec<&str> = outcomes
        .iter()
        .map(|outcome| outcome.text.as_ref())
        .collect();
    assert!(!outcomes[0].is_error);
    assert!(
        texts[0].contains("#1"),
        "first todo receives the stable ID #1"
    );
    assert!(!outcomes[1].is_error);
    assert!(
        texts[1].contains("#2"),
        "second todo receives the stable ID #2"
    );
    assert!(!outcomes[2].is_error);
    assert!(!outcomes[3].is_error);
    assert!(texts[3].contains("#1"), "toggle keeps the todo ID");
    assert!(!outcomes[4].is_error);
    assert_eq!(texts[4], "[x] #1: first\n[ ] #2: second");
    assert!(!outcomes[5].is_error);
    assert!(texts[5].contains("#1"), "second toggle keeps the todo ID");
    assert!(outcomes[6].is_error);
    assert_eq!(texts[6], "todo: #9 not found");
    assert!(outcomes[7].is_error);
    assert_eq!(texts[7], "todo: text is required for add");
    assert!(outcomes[8].is_error);
    assert_eq!(texts[8], "todo: id is required for toggle");
    assert_eq!(
        run_command(&agent, "").await?,
        "[ ] #1: first\n[ ] #2: second"
    );
    host.shutdown(Duration::from_secs(2)).await;
    drop(agent);

    let reopened_host = start_host(&data, &workspace, &fixture).await?;
    let reopened = reopened_host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: Workspace::new(workspace.clone())?,
            },
            ClientId::new("todo-reopen-test"),
        )
        .await?;
    assert_eq!(
        run_command(&reopened, "").await?,
        "[ ] #1: first\n[ ] #2: second",
        "durable state survives reopening the session"
    );
    assert_eq!(
        run_command(&reopened, "--action=clear").await?,
        "Cleared 2 todos"
    );
    assert_eq!(run_command(&reopened, "").await?, "No todos");
    reopened_host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn todo_writes_fail_closed_on_ephemeral_sessions() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(data.join("plugins/todo"))?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(data.join("plugins/todo/plugin.star"), PLUGIN_SOURCE)?;
    let fixture = data.join("todo-ephemeral-scripted.jsonl");
    std::fs::write(&fixture, ephemeral_replay())?;

    let host = start_host(&data, &workspace, &fixture).await?;
    let agent = host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(workspace)?,
            },
            ClientId::new("todo-ephemeral-test"),
        )
        .await?;
    let outcomes = run_turn(&agent, "add an ephemeral todo").await?;
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].is_error, "ephemeral state must fail closed");
    // The refusal is the typed `unavailable` outcome of the first state
    // operation, so a script can tell it from an ordinary failure.
    assert_eq!(
        outcomes[0].text.as_ref(),
        "state.read: unavailable: state is unavailable in this session"
    );
    // Failing closed also means the add left nothing behind: a reader in the
    // same session is refused as well, rather than answering "No todos".
    let listed = run_command(&agent, "")
        .await
        .expect_err("an ephemeral session has no todo list to read");
    assert!(
        listed
            .to_string()
            .contains("state.read: unavailable: state is unavailable in this session"),
        "{listed}"
    );
    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}
