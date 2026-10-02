//! Exercises Starlark TTSR rules against replayed model streams.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::{Delivery, Env, SessionRef, ext::ExtensionBuilder};
use dal_core::{
    Command, Config, ConfigProduct, EntryKind, Expect, Part, Reply, ServiceSet, Stop, UpdateKind,
    Workspace, ext::Name,
};
use dal_ext::ttsr::{
    TtsrWatchFactory,
    build::{RuleBuildInput, set_for},
    gate::Gate,
    readers::EditStyle,
    record::RecordSource,
};
use support::{TestDir, scripted_session};

const RULE_PLUGIN: &str = r#"load("@dal/v1", "dal")

rule = dal.rule(
    pattern = "TRIPWIRE",
    text = "Respect the gate rule.",
    interrupt_mode = "always",
)

plugin = dal.plugin(
    name = "gate-ttsr",
    version = "0.1.0",
    rules = {"retry-control": rule},
)
"#;

const REPLAY: &str = concat!(
    "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"partial attempt one TRIPWIRE\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
    "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"partial attempt two TRIPWIRE\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
    "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"partial attempt three TRIPWIRE\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
    "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"final answer after the retry cap: TRIPWIRE\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
);

#[expect(clippy::too_many_lines, reason = "SC ttsr scenario is one long script")]
#[tokio::test]
async fn ttsr_replay_interrupts_injects_and_retries_at_most_three_times()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let plugin_dir = data.path().join("plugins/gate-ttsr");
    fs::create_dir_all(&plugin_dir)?;
    fs::write(plugin_dir.join("plugin.star"), RULE_PLUGIN)?;
    let replay_fixture = data.path().join("ttsr-scripted.jsonl");
    fs::write(&replay_fixture, REPLAY)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai/gpt-6\"\nplugins = [\"gate-ttsr\"]\n[providers.scripted]\nfixture = {:?}\n",
        replay_fixture.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let plugin = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "gate-ttsr")
        .expect("Starlark loader registered the rule plugin");
    assert_eq!(plugin.rules().len(), 1);
    let record = plugin.rules().first().unwrap().clone();
    let source = RecordSource {
        plugin: Name::parse(plugin.name())?,
        site: plugin
            .site()
            .cloned()
            .expect("Starlark plugin has a source site"),
        record,
    };
    let sources = [source];
    let build_input = RuleBuildInput {
        records: &sources,
        plugin_rules: &[],
        known_tools: &[],
        agent: "main",
    };
    let set = Arc::new(set_for(
        &build_input,
        data.path(),
        workspace.path(),
        config.rules(),
    ));
    assert!(
        !set.stream.is_empty(),
        "TTSR did not compile a stream rule: {:?}",
        set.problems
    );
    assert_eq!(set.stream[0].name.as_str(), "retry-control");
    let watcher = TtsrWatchFactory::new(
        Arc::clone(&set),
        Arc::new(Mutex::new(Gate::default())),
        config.rules().clone(),
        workspace.path().to_path_buf(),
        EditStyle::Replace,
    );
    let mut direct_watch = dal_ext::ttsr::watch::create(
        &set,
        &Gate::default(),
        dal_core::TurnId::new(std::num::NonZeroU64::MIN),
        dal_ext::ttsr::watch::WatchBudget::Interrupts,
        config.rules(),
        workspace.path(),
        EditStyle::Replace,
    );
    assert_eq!(
        direct_watch.feed(dal_ext::ttsr::watch::SourceKind::Text, "TRIPWIRE",),
        Ok(dal_ext::ttsr::watch::WatchVerdict::Stop),
    );
    let watcher_extension = ExtensionBuilder::new("ttsr-gate", "0.1.0", ServiceSet::EMPTY)?
        .output_stream(Arc::new(watcher))
        .build()?;
    product.extensions.push(watcher_extension);
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut subscription = harness.agent.subscribe(None)?;
    let reply = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Return the scripted response.".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));
    let mut rule_updates = 0;
    let mut ended = false;
    while !ended {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await?;
        let Some(delivery) = delivery else { break };
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::RuleFired { rule, .. } if rule.as_ref() == "retry-control" => {
                rule_updates += 1;
            }
            UpdateKind::TurnEnded {
                stop: Stop::EndTurn,
                ..
            } => ended = true,
            _ => {}
        }
    }
    assert!(ended, "the scripted response must reach a completed turn");
    assert_eq!(
        rule_updates, 3,
        "the fourth match must not retry the interrupted response"
    );
    let view = harness.agent.view(dal_core::PageReq::default())?;
    let reminders: Vec<_> = view
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Reminder { source, text } => Some((source.as_ref(), text.as_ref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        reminders.len(),
        4,
        "the post-cap rule match is still persisted"
    );
    assert!(reminders[..3].iter().all(|(source, text)| {
        *source == "rule:retry-control"
            && text.contains("<system-interrupt")
            && text.contains("Respect the gate rule.")
    }));
    let (source, reminder) = reminders[3];
    assert_eq!(source, "rule:retry-control");
    assert!(reminder.contains("<system-reminder"));
    assert!(reminder.contains("Respect the gate rule."));
    let assistant_text = view
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|block| match block {
                        dal_core::Block::Text { text } => Some(text.as_ref()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(assistant_text, "final answer after the retry cap: TRIPWIRE");
    assert!(!assistant_text.contains("partial attempt"));
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
