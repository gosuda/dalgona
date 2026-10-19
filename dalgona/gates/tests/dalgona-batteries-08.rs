// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Declared MCP servers stay unmapped and unstarted until the session
//! grants the declared server set.
use dalgona_gates::support;

use std::{collections::BTreeMap, io, time::Duration};

use dal_agent::ext::ExtensionBuilder;
use dal_agent::{Env, Host, SessionRef};
use dal_core::ext::{McpBlock, McpLaunch, McpServerDecl, Name, SkillRecord};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, EntryKind, Expect, JournalPart, Origin,
    Output, PageReq, Part, Reply, ServiceSet, Workspace,
};

const WAIT: Duration = Duration::from_secs(30);
const SKILL: &str = "gate-weather";
const SERVER: &str = "fixture";
const ENTRY: &str = "gate-weather.fixture.";
const MISSING_PROGRAM: &str = "dalgona-gates-missing-binary";

/// A skill extension declaring one stdio server whose program is not on PATH.
fn declaring_skill() -> support::TestResult<dal_agent::ext::Extension> {
    let launch = McpLaunch::command(vec![MISSING_PROGRAM.into()])?;
    let block = McpBlock {
        servers: BTreeMap::from([(
            SERVER.into(),
            McpServerDecl::Stdio {
                launch,
                env: BTreeMap::new(),
            },
        )]),
    };
    let skill = SkillRecord {
        name: Name::parse(SKILL)?,
        description: "gate MCP fixture".into(),
        body: "gate MCP fixture skill".into(),
        letter2image: false,
        mcp: Some(block),
    };
    Ok(
        ExtensionBuilder::new("gate-skill", "0.1.0", ServiceSet::from_names(["env"])?)?
            .with_origin(Origin::Bundled, None)
            .skill(skill)
            .build()?,
    )
}

async fn mcp_status(agent: &dal_agent::Agent) -> support::TestResult<String> {
    match agent
        .submit(Command::Run {
            name: "mcp".into(),
            args: "".into(),
            expected: None,
        })
        .await?
    {
        Reply::Done(Output::Markdown(text)) => Ok(text.into()),
        reply => Err(io::Error::other(format!("unexpected mcp reply: {reply:?}")).into()),
    }
}

/// Runs one scripted turn, declining every frontend question it raises.
async fn declined_turn(
    agent: &dal_agent::Agent,
    subscription: &mut dal_agent::Subscription,
) -> support::TestResult<()> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "exercise the MCP server".into(),
            }],
        })
        .await?;
    let turn: support::TestResult<()> = tokio::time::timeout(WAIT, async {
        loop {
            let view = agent.view(PageReq::default())?;
            if let Some(request) = view.open.first() {
                agent.answer(request.id, Answer::Decline).await?;
                continue;
            }
            let Some(delivery) = subscription.next().await else {
                return Err(io::Error::other("subscription closed before the turn ended").into());
            };
            if let dal_agent::Delivery::Update(update) = delivery
                && let dal_core::UpdateKind::TurnEnded { .. } = &update.kind
            {
                return Ok(());
            }
        }
    })
    .await
    .map_err(|_| io::Error::other("the scripted turn did not end"))?;
    turn
}

/// The one tool result the journal holds for `name`, as error flag and text.
fn tool_result(
    agent: &dal_agent::Agent,
    name: &str,
) -> support::TestResult<Option<(bool, String)>> {
    let view = agent.view(PageReq::default())?;
    Ok(view
        .entries
        .items
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::ToolResult {
                name: tool,
                error,
                parts,
                ..
            } if tool.as_ref() == name => Some((
                *error,
                parts
                    .iter()
                    .filter_map(|part| match part {
                        JournalPart::Text { text } => Some(text.to_string()),
                        _ => None,
                    })
                    .collect::<String>(),
            )),
            _ => None,
        }))
}

#[tokio::test]
async fn mcp_stdio_tools_are_listed_as_deferred() -> support::TestResult<()> {
    let scratch = support::Scratch::new("mcp-deferred")?;
    let root = scratch.path().to_path_buf();
    std::fs::create_dir_all(root.join("workspace"))?;

    // Registration declares the server without starting a transport: the
    // product's own tool list stays empty of mapped tools.
    let factory = dalgona::product();
    let script = root.join("script.jsonl");
    std::fs::write(&script, support::tool_step("entry-declined", ENTRY, "{}"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\ndisabled_batteries = [\"ask\", \"history\", \"judged\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\n[providers.scripted]\nfixture = {:?}\n",
        script.display().to_string()
    );
    let config = Config::load(ConfigProduct::Dalgona, &root, factory.defaults, Some(&user))?;
    let cx = dalgon::BuildCx {
        data_root: root.clone(),
        config: &config,
    };
    let mut product = dalgona::build(&cx)?;
    product.extensions.push(declaring_skill()?);
    let mcp = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "mcp")
        .ok_or_else(|| io::Error::other("the product has no mcp extension"))?;
    assert!(
        mcp.tools().is_empty(),
        "declared MCP tools stay deferred at registration"
    );
    assert!(
        !root.join("mcp").exists(),
        "registration starts no transport"
    );

    let env = Env {
        vars: BTreeMap::from([("OPENAI_API_KEY".into(), "sk-test".into())]),
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
            ClientId::new("mcp-deferred-gate"),
        )
        .await?;

    // Before any call the declaration is listed unmapped and unasked, and
    // the missing program was never launched.
    let before = mcp_status(&agent).await?;
    assert!(
        before.contains(":gate-weather:fixture"),
        "status names the declared server: {before}"
    );
    assert!(
        before.contains("declared") && before.contains("0 tools") && before.contains("unasked"),
        "an unasked server is declared with no tools: {before}"
    );

    // The entry call asks the declared-set grant; the decline keeps the
    // server unstarted and every mapped tool unpublished, and the failed
    // call names the denial instead of the missing program.
    let mut subscription = agent.subscribe(None)?;
    declined_turn(&agent, &mut subscription).await?;
    drop(subscription);
    let denied = tool_result(&agent, ENTRY)?;
    let Some((error, text)) = denied else {
        return Err(io::Error::other("the entry tool never ran").into());
    };
    assert!(error, "the declined grant fails the entry call: {text}");
    assert!(
        text.contains("declined"),
        "the result names the declined grant: {text}"
    );
    assert!(
        !text.contains("was not found on PATH"),
        "a declared server never starts without its grant: {text}"
    );
    let after = mcp_status(&agent).await?;
    assert!(
        after.contains("0 tools") && after.contains("unasked"),
        "a declined grant never maps or starts: {after}"
    );
    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}
