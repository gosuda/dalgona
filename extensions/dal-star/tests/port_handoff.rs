//! The handoff port starts a real child, persists its report through the
//! durable state service, and refuses an ephemeral session.

pub mod support;

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use dal_agent::{Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Output, PageReq, Reply, UpdateKind, Workspace,
};
use support::system_with_plugin;

const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";
const HANDOFF_RESULT: &str =
    "Handoff written to handoff.md. Start the next session and ask for it.";
const EPHEMERAL_RESULT: &str = "handoff: no sidecar in an ephemeral session";

#[derive(serde::Deserialize)]
struct StateFile {
    format: u8,
    entries: Vec<StateEntry>,
}

#[derive(serde::Deserialize)]
struct StateEntry {
    key: String,
    present: bool,
    value: Option<dal_core::RawJson>,
}

fn text_step(text: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{text}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}}, {USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

async fn open_handoff(
    ephemeral: bool,
) -> Result<
    (tempfile::TempDir, tempfile::TempDir, Host, dal_agent::Agent),
    Box<dyn Error + Send + Sync>,
> {
    let (data, system) = system_with_plugin(
        "handoff",
        include_str!("../../../examples/plugins/handoff/plugin.star"),
    );
    let data_root = data.path().to_path_buf();
    let workspace = tempfile::tempdir()?;
    let fixture = data_root.join("handoff-scripted.jsonl");
    std::fs::write(
        &fixture,
        format!(
            "{}{}",
            text_step("child-generated handoff"),
            text_step("direct child"),
        ),
    )?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data_root, "", Some(&user))?;
    let extensions = system.extensions()?;
    let product = Product {
        name: "dal",
        data_root: data_root.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(workspace.path().join("home")),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(workspace.path().join("cache")),
            ),
        ]),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let workspace_ref = Workspace::new(workspace.path().to_path_buf())?;
    let session = if ephemeral {
        SessionRef::Ephemeral {
            workspace: workspace_ref,
        }
    } else {
        SessionRef::New {
            workspace: workspace_ref,
            name: None,
        }
    };
    let agent = host.open(session, ClientId::new("handoff-test")).await?;
    Ok((data, workspace, host, agent))
}

async fn run_command(
    agent: &dal_agent::Agent,
    args: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut updates: Subscription = agent.subscribe(None)?;
    let command = agent.submit(Command::Run {
        name: "handoff:handoff".into(),
        args: args.into(),
        expected: None,
    });
    tokio::pin!(command);
    let reply = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            tokio::select! {
                delivery = updates.next() => {
                    let Some(Delivery::Update(update)) = delivery else {
                        return Err::<Reply, Box<dyn Error + Send + Sync>>(
                            std::io::Error::other("handoff subscription closed or lost updates").into(),
                        );
                    };
                    if let UpdateKind::RequestOpened(request) = &update.kind
                        && agent
                            .view(PageReq::default())?
                            .open
                            .iter()
                            .any(|open| open.id == request.id)
                    {
                        agent.answer(request.id, Answer::ApproveForSession).await?;
                    }
                }
                result = &mut command => break Ok(result?),
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other("handoff command timed out"))??;
    let Reply::Done(Output::Text(text)) = reply else {
        return Err(std::io::Error::other(format!(
            "handoff command did not return text: {reply:?}"
        ))
        .into());
    };
    Ok(text.into())
}

fn state_contents(data: &Path) -> Result<Vec<StateFile>, Box<dyn Error + Send + Sync>> {
    let sessions = match std::fs::read_dir(data.join("sessions")) {
        Ok(sessions) => sessions,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut contents = Vec::new();
    for workspace in sessions {
        let workspace = workspace?;
        let sessions = match std::fs::read_dir(workspace.path()) {
            Ok(sessions) => sessions,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for session in sessions {
            let path = session?.path().join("state");
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let state: StateFile = sonic_rs::from_str(&text)?;
            if state.format != 1 {
                return Err(std::io::Error::other("unsupported state sidecar format").into());
            }
            contents.push(state);
        }
    }
    Ok(contents)
}

fn state_has(states: &[StateFile], expected: &str) -> bool {
    states.iter().any(|state| {
        state.entries.iter().any(|entry| {
            entry.key == "handoff.md"
                && entry.present
                && entry.value.as_ref().is_some_and(|value| {
                    sonic_rs::from_str::<String>(value.as_str()).ok().as_deref() == Some(expected)
                })
        })
    })
}

fn state_has_key(states: &[StateFile], key: &str) -> bool {
    states
        .iter()
        .any(|state| state.entries.iter().any(|entry| entry.key == key))
}

#[tokio::test(flavor = "multi_thread")]
async fn durable_handoff_persists_the_real_child_report() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let (data, workspace, host, agent) = open_handoff(false).await?;
    let result = run_command(&agent, "--note='capture the current state'").await?;
    assert_eq!(result, HANDOFF_RESULT);
    assert!(
        state_has(&state_contents(data.path())?, "child-generated handoff",),
        "the child report must be durable under the handoff.md state key"
    );
    let session = agent.session();
    host.close(session).await?;
    let agent = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: Workspace::new(workspace.path().to_path_buf())?,
            },
            ClientId::new("handoff-resume"),
        )
        .await?;
    let command_result = run_command(&agent, "").await?;
    assert_eq!(command_result, HANDOFF_RESULT);
    assert!(
        state_has(&state_contents(data.path())?, "direct child"),
        "the second child report must replace the durable handoff.md state value"
    );
    let _ = host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn ephemeral_handoff_refuses_without_writing_state()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let (data, _workspace, host, agent) = open_handoff(true).await?;
    let result = run_command(&agent, "").await?;
    assert_eq!(result, EPHEMERAL_RESULT);
    assert!(
        !state_has_key(&state_contents(data.path())?, "handoff.md"),
        "ephemeral handoff must not write a state record"
    );
    let _ = host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}
