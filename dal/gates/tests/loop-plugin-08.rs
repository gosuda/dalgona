#![expect(clippy::expect_used, reason = "SC test")]
//! Exercises Deferred promotion persistence across durable session resume.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{collections::BTreeMap, error::Error, fs, io, path::Path, sync::Arc, time::Duration};

use dal_agent::ext::tool::{Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_agent::{
    Agent, Delivery, Env, SessionRef, Subscription,
    ext::{ArgError, BoxFuture, ExtensionBuilder},
};
use dal_core::{
    Command, Config, ConfigProduct, EntryKind, EntryView, Expect, JournalPart, ModelInfo, Part,
    Product, RawJson, Record, Reply, Stop, ToolClass, ToolSpec, UpdateKind, View, Visibility,
    Workspace,
};
use dal_store::Store;
use support::{TestDir, scripted_session};

const PARAMETERS: &str = r#"{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}"#;

const SEARCH_RESPONSE: &str = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"search","name":"tool_search"},{"type":"tool_calls_done","calls":[{"id":"search","name":"tool_search","args":{"kind":"parsed","value":{"query":"deep_lookup"}}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}
"#;
const PLUGIN_RESPONSE: &str = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"lookup","name":"deep_lookup"},{"type":"tool_calls_done","calls":[{"id":"lookup","name":"deep_lookup","args":{"kind":"parsed","value":{"query":"guide"}}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}
"#;
const FINAL_RESPONSE: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"lookup completed"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}
"#;

struct DeferredLookup {
    spec: Arc<ToolSpec>,
}

impl Tool for DeferredLookup {
    fn name(&self) -> &dal_core::Name {
        &self.spec.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawJson, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Ok(ToolOutput::from_text("deferred lookup result")) })
    }
}

fn deferred_extension() -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    let spec = Arc::new(ToolSpec {
        name: dal_core::Name::parse("deep_lookup")?,
        description: "Find a guide in the deferred catalog.".into(),
        parameters: RawJson::parse(PARAMETERS)
            .map_err(|_| dal_core::RegistrationError::InvalidParameters)?,
        grammar: None,
    });
    ExtensionBuilder::new("gate-deferred", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .tool(Arc::new(DeferredLookup { spec }), Visibility::Deferred)
        .build()
}

async fn scripted_host(
    data_root: &Path,
    workspace: &Path,
    replay: &Path,
    session: SessionRef,
) -> Result<support::GateHarness, Box<dyn Error + Send + Sync>> {
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        replay.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data_root,
        factory.defaults,
        Some(&user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data_root.to_path_buf(),
        config: &config,
    })?;
    product.extensions.push(deferred_extension()?);
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.to_path_buf(),
        sandbox_helper: None,
    };
    scripted_session(product, config, env, session).await
}

async fn prompt_turn(
    agent: &Agent,
    subscription: &mut Subscription,
    text: &str,
    label: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(io::Error::other("scripted prompt was not accepted").into());
    }
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
            .await
            .map_err(|_| {
                let view = agent.view(dal_core::PageReq::default());
                let state = view.map_or_else(
                    |error| format!("view unavailable: {error}"),
                    |view| {
                        let last = view.entries.items.last().map(|entry| &entry.kind);
                        format!(
                            "turn={:?}, open={}, last_entry={last:?}",
                            view.turn,
                            view.open.len()
                        )
                    },
                );
                io::Error::other(format!(
                    "{label} turn timed out waiting for a session update: {state}"
                ))
            })?;
        let Some(delivery) = delivery else {
            return Err(io::Error::other("session subscription closed before turn end").into());
        };
        let Delivery::Update(update) = delivery else {
            continue;
        };
        if matches!(&update.kind, UpdateKind::RequestOpened(_)) {
            return Err(io::Error::other("deferred read tool unexpectedly opened an ask").into());
        }
        let UpdateKind::TurnEnded { stop, .. } = &update.kind else {
            continue;
        };
        if *stop == Stop::EndTurn {
            return Ok(());
        }
        return Err(io::Error::other(format!(
            "{label} turn ended before its lookup completed: {stop:?}"
        ))
        .into());
    }
}

fn last_tool_result(entries: &[EntryView], tool: &str) -> Option<(bool, String)> {
    entries.iter().rev().find_map(|entry| {
        let EntryKind::ToolResult {
            name, error, parts, ..
        } = &entry.kind
        else {
            return None;
        };
        if name.as_ref() != tool {
            return None;
        }
        let text = parts
            .iter()
            .filter_map(|part| match part {
                JournalPart::Text { text } => Some(text.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some((*error, text))
    })
}

fn assert_lookup_completed(view: &View) {
    let (error, text) = last_tool_result(&view.entries.items, "deep_lookup")
        .expect("the deferred tool call must have a journaled result");
    assert!(!error, "deferred tool failed: {text}");
    assert_eq!(text, "deferred lookup result");
}

fn promoted_tool(records: &[Record]) -> Option<Box<str>> {
    records.iter().find_map(|record| match record {
        Record::ToolPromoted { tool, .. } if tool.as_ref() == "deep_lookup" => Some(tool.clone()),
        _ => None,
    })
}

fn promotion_count(records: &[Record]) -> usize {
    records
        .iter()
        .filter(|record| matches!(record, Record::ToolPromoted { tool, .. } if tool.as_ref() == "deep_lookup"))
        .count()
}

async fn durable_promoted_tool(
    data_root: &Path,
    workspace: Workspace,
    session: dal_core::SessionId,
) -> Result<Option<Box<str>>, Box<dyn Error + Send + Sync>> {
    let store = Store::new(data_root.to_path_buf(), workspace, Product::Dal);
    let (journal, _) = store.open_session(session).await?;
    Ok(promoted_tool(journal.records()))
}

async fn durable_promotion_count(
    data_root: &Path,
    workspace: Workspace,
    session: dal_core::SessionId,
) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let store = Store::new(data_root.to_path_buf(), workspace, Product::Dal);
    let (journal, _) = store.open_session(session).await?;
    Ok(promotion_count(journal.records()))
}

#[tokio::test]
async fn deferred_tool_promotes_and_promotion_survives_resume()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let first_replay = data.path().join("deferred-first.jsonl");
    let resume_replay = data.path().join("deferred-resume.jsonl");
    fs::write(
        &first_replay,
        format!("{SEARCH_RESPONSE}{PLUGIN_RESPONSE}{FINAL_RESPONSE}"),
    )?;
    fs::write(&resume_replay, format!("{PLUGIN_RESPONSE}{FINAL_RESPONSE}"))?;
    let workspace_root = Workspace::new(workspace.path().to_path_buf())?;
    let first_session = SessionRef::New {
        workspace: workspace_root.clone(),
        name: Some("deferred-promotion".into()),
    };
    let first = scripted_host(data.path(), workspace.path(), &first_replay, first_session).await?;
    let mut first_subscription = first.agent.subscribe(None)?;
    prompt_turn(
        &first.agent,
        &mut first_subscription,
        "Find the guide.",
        "first",
    )
    .await?;
    let first_view = first.agent.view(dal_core::PageReq::default())?;
    let (search_error, listing) = last_tool_result(&first_view.entries.items, "tool_search")
        .expect("the search result must be journaled");
    assert!(!search_error, "tool_search failed: {listing}");
    assert!(listing.contains("deep_lookup"), "{listing}");
    assert_lookup_completed(&first_view);
    let session_id = first_view.session.id;
    first.host.close(session_id).await?;
    let _ = first.host.shutdown(Duration::from_secs(1)).await;
    let promotion = durable_promoted_tool(data.path(), workspace_root.clone(), session_id).await?;
    assert!(
        promotion.is_some(),
        "the successful deferred call must be promoted"
    );
    assert_eq!(promotion.unwrap().as_ref(), "deep_lookup");

    let resumed = scripted_host(
        data.path(),
        workspace.path(),
        &resume_replay,
        SessionRef::Resume {
            key: session_id.to_string().into_boxed_str(),
            workspace: workspace_root.clone(),
        },
    )
    .await?;
    let mut resumed_subscription = resumed.agent.subscribe(None)?;
    prompt_turn(
        &resumed.agent,
        &mut resumed_subscription,
        "Use the retained guide tool.",
        "resumed",
    )
    .await?;
    assert_lookup_completed(&resumed.agent.view(dal_core::PageReq::default())?);
    resumed.host.close(session_id).await?;
    let _ = resumed.host.shutdown(Duration::from_secs(1)).await;
    assert_eq!(
        durable_promotion_count(data.path(), workspace_root, session_id).await?,
        1
    );
    Ok(())
}
