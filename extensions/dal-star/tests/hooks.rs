//! Scripted hook events run through the host agent loop and return verdicts.

pub mod support;

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, Output, PageReq, Part, Reply,
    ToolOutcomeView, UpdateKind, Workspace,
};
use support::system_with_plugin;

const REPLAY: &str = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"hook-call","name":"HOOK_TOOL"},{"type":"tool_calls_done","calls":[{"id":"hook-call","name":"HOOK_TOOL","args":{"kind":"parsed","value":{}}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}
{"kind":"events","events":[{"type":"text_delta","text":"finished"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

fn plugin_source(hook_return: &str) -> String {
    let mut source = String::from(
        r#"load("@dal/v1", "dal")
def hit(ctx, args):
    return "tool body ran"

hit_tool = dal.tool(description = "Return a marker.", input = dal.schema(), run = hit)
def guard(ctx, event):
    return "#,
    );
    source.push_str(hook_return);
    source.push_str(
        r#"

plugin = dal.plugin(
    name = "hookcase",
    version = "0.1.0",
    tools = {"hit": hit_tool},
    hooks = [dal.on("tool_call", guard)],
)
"#,
    );
    source
}

async fn settled_tool_call(
    hook_return: &str,
) -> Result<ToolOutcomeView, Box<dyn Error + Send + Sync>> {
    let (data, system) = system_with_plugin("hookcase", &plugin_source(hook_return));
    let extensions = system.extensions()?;
    let data_root = data.path().to_path_buf();
    let workspace = tempfile::tempdir()?;
    let fixture = data_root.join("hook-scripted.jsonl");
    std::fs::write(&fixture, REPLAY.replace("HOOK_TOOL", "hookcase__hit"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data_root, "", Some(&user))?;
    let product = Product {
        name: "dal",
        data_root: data_root.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(workspace.path().to_path_buf())?,
            },
            ClientId::new("hook-test"),
        )
        .await?;
    let result = async {
        let mut subscription = agent.subscribe(None)?;
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "Call hookcase__hit once.".into(),
                }],
            })
            .await?;
        if !matches!(reply, Reply::Accepted { .. }) {
            return Err(std::io::Error::other("the scripted prompt was not accepted").into());
        }
        let mut settled = None;
        let mut seen = Vec::new();
        loop {
            let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await;
            let Ok(delivery) = delivery else {
                let view = agent.view(PageReq::default())?;
                return Err(std::io::Error::other(format!(
                    "no update from the scripted turn; seen={seen:?}; view={view:?}"
                ))
                .into());
            };
            let Some(Delivery::Update(update)) = delivery else {
                break;
            };
            seen.push(format!("{:?}", update.kind));
            match &update.kind {
                UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
                UpdateKind::TurnEnded { .. } if settled.is_some() => break,
                _ => {}
            }
        }
        settled.ok_or_else(|| {
            std::io::Error::other("the host did not settle the scripted tool call").into()
        })
    }
    .await;
    let _ = host.shutdown(Duration::from_secs(1)).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn dictionary_tool_call_hook_result_fails_closed() {
    let outcome = settled_tool_call("{\"ok\": True}")
        .await
        .expect("hook call settles");

    assert!(outcome.is_error, "{outcome:?}");
    assert!(
        outcome
            .text
            .contains("tool_call hooks must return an event verdict"),
        "{outcome:?}"
    );
}

/// The start hook records what the event told it in durable session state;
/// the end hook raises its event as a notice, which is how an observer
/// failure reaches a client.
const LIFECYCLE_PLUGIN: &str = r#"load("@dal/v1", "dal")

def on_start(ctx, event):
    seen = ctx.state.read(key = "lifecycle")
    marks = []
    if seen.present:
        marks = [mark for mark in seen.value]
    marks.append("start resumed=" + ("yes" if event.resumed else "no") + " session=" + event.session + " workspace=" + event.workspace)
    ctx.state.write(key = "lifecycle", value = marks, expected = seen.revision)
    return None

def on_end(ctx, event):
    fail("end reason=" + event.reason + " session=" + event.session)

def show(ctx, args):
    seen = ctx.state.read(key = "lifecycle")
    if not seen.present:
        return "no marks"
    return "\n".join(seen.value)

marks_tool = dal.tool(description = "Show the lifecycle marks.", input = dal.schema(), uses = ["state.read"], run = show)

plugin = dal.plugin(
    name = "lifecycle",
    version = "0.1.0",
    tools = {"marks": marks_tool},
    commands = {"marks": dal.command(tool = marks_tool)},
    hooks = [
        dal.on("session_start", on_start, uses = ["state.read", "state.write"]),
        dal.on("session_end", on_end),
    ],
)
"#;

/// Runs `/lifecycle:marks`, approving the state grant it asks for (a
/// persistent one, so later sessions inherit it), and returns the text it
/// printed.
async fn lifecycle_marks(agent: &dal_agent::Agent) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut updates = agent.subscribe(None)?;
    let command = agent.submit(Command::Run {
        name: "lifecycle:marks".into(),
        args: String::new().into(),
        expected: None,
    });
    tokio::pin!(command);
    let reply = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            tokio::select! {
                biased;
                delivery = updates.next() => {
                    let Some(Delivery::Update(update)) = delivery else {
                        return Err::<Reply, Box<dyn Error + Send + Sync>>(
                            std::io::Error::other("marks subscription ended early").into(),
                        );
                    };
                    if let UpdateKind::RequestOpened(request) = &update.kind {
                        agent.answer(request.id, Answer::Approve).await?;
                    }
                }
                result = &mut command => break Ok(result?),
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other("marks command timed out"))??;
    let Reply::Done(Output::Text(text)) = reply else {
        return Err(std::io::Error::other(format!("unexpected marks reply: {reply:?}")).into());
    };
    Ok(text.into())
}

#[tokio::test(flavor = "multi_thread")]
async fn session_lifecycle_hooks_receive_their_event_and_a_working_script_context()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let (data, system) = system_with_plugin("lifecycle", LIFECYCLE_PLUGIN);
    let extensions = system.extensions()?;
    let data_root = data.path().to_path_buf();
    let workspace = tempfile::tempdir()?;
    let fixture = data_root.join("lifecycle-scripted.jsonl");
    std::fs::write(&fixture, "")?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data_root, "", Some(&user))?;
    let product = Product {
        name: "dal",
        data_root: data_root.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let workspace_ref = Workspace::new(workspace.path().to_path_buf())?;
    let shown = workspace_ref.as_path().display().to_string();

    // A new session starts before any client can grant the plugin state, so
    // its own start hook may not be able to record anything. Asking for the
    // marks is what obtains the grant that the resumed session inherits.
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace_ref.clone(),
                name: None,
            },
            ClientId::new("lifecycle-test"),
        )
        .await?;
    let session = agent.session();
    let first = lifecycle_marks(&agent).await?;
    assert!(
        first == "no marks"
            || first
                .lines()
                .all(|line| line.starts_with("start resumed=no ")),
        "only a fresh start may precede the resume: {first}"
    );

    // Closing the session ends it: the end hook sees the close reason and
    // the same session identity.
    let mut subscription = agent.subscribe(None)?;
    host.close(session).await?;
    let mut notices = Vec::new();
    while let Ok(Some(Delivery::Update(update))) =
        tokio::time::timeout(Duration::from_secs(5), subscription.next()).await
    {
        if let UpdateKind::Notice(notice) = &update.kind {
            notices.push((notice.kind.to_string(), notice.text.to_string()));
        }
    }
    let ended: Vec<&(String, String)> = notices
        .iter()
        .filter(|(kind, _)| kind == "hook.session_end")
        .collect();
    let [(_, text)] = ended.as_slice() else {
        panic!("exactly one session_end notice expected: {notices:?}");
    };
    assert!(
        text.contains(&format!(
            "error: fail: end reason=close session={session}\n"
        )),
        "{text}"
    );

    // Reopening the stored session is a resume, and the new start hook sees
    // the marks the first one persisted.
    let resumed = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: workspace_ref,
            },
            ClientId::new("lifecycle-resume"),
        )
        .await?;
    assert_eq!(resumed.session(), session);
    let marks = lifecycle_marks(&resumed).await?;
    assert_eq!(
        marks.lines().last(),
        Some(format!("start resumed=yes session={session} workspace={shown}").as_str()),
        "the resumed session's start hook must see its own event: {marks}"
    );
    assert_eq!(
        marks
            .lines()
            .filter(|line| line.contains("resumed=yes"))
            .count(),
        1,
        "{marks}"
    );
    let _ = host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_tool_call_hook_verdict_fails_closed() {
    let outcome = settled_tool_call("None").await.expect("hook call settles");

    assert!(outcome.is_error, "{outcome:?}");
    assert!(
        outcome
            .text
            .contains("tool_call hooks must return an event verdict"),
        "{outcome:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_event_rejects_a_verdict_method_from_another_event() {
    let outcome = settled_tool_call("event.continue_()")
        .await
        .expect("hook call settles");

    assert!(outcome.is_error, "{outcome:?}");
    assert!(
        outcome.text.contains("hook \"hookcase\" failed"),
        "{outcome:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_block_verdict_preserves_its_reason() {
    let outcome = settled_tool_call("event.block(\"x\")")
        .await
        .expect("hook call settles");

    assert!(outcome.is_error, "{outcome:?}");
    assert_eq!(outcome.text.as_ref(), "blocked by hookcase: x");
}
