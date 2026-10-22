// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Defaults reach search, patch, and guard behavior on the live product.
use dalgona_gates::support;

use std::{path::Path, time::Duration};

use dal_agent::{Host, SessionRef};
use dal_core::{ClientId, Command, Expect, Part, Workspace};

const WAIT: Duration = Duration::from_secs(30);
/// The refusal text, with the quotes in the form the journal record's JSON
/// encoding carries.
const SYMBOL_OFF: &str =
    "search: mode \\\"symbol\\\" is off. Set search_symbols = true in dal.toml.";
const NO_BATTERIES: &str = "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]";
/// The product wires the guard through the quality battery, so the guard
/// halves keep it while every other battery stays off.
const WITH_QUALITY: &str = "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]";

fn escape_json(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// The decoded text of a raw `tool_result` journal record. Raw records carry
/// JSON-escaped bytes, so dialect names with quotes only match after decoding.
fn tool_text(record: &str) -> support::TestResult<String> {
    use sonic_rs::{JsonContainerTrait, JsonValueTrait as _};
    let value: sonic_rs::Value = sonic_rs::from_str(record.trim_end())?;
    let text = value
        .get("parts")
        .and_then(sonic_rs::Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(sonic_rs::Value::as_str)
        .ok_or("tool_result record carries no text part")?;
    Ok(text.to_owned())
}

/// Runs one scripted tool call through the real binary and returns its single
/// `tool_result` record.
fn call_tool(
    label: &str,
    top_level_toml: &str,
    id: &str,
    tool: &str,
    args_json: &str,
) -> support::TestResult<String> {
    let scratch = support::Scratch::new(label)?;
    std::fs::write(
        scratch.path().join("probe.rs"),
        "pub fn gate_symbol_probe() -> u64 {\n    41\n}\n",
    )?;
    let journal = support::run_scripted_print(
        &scratch,
        top_level_toml,
        &[
            support::tool_step(id, tool, args_json),
            support::text_step("done"),
        ],
        "search the workspace",
    )?;
    let results = support::journal_records(&journal, "tool_result");
    assert_eq!(results.len(), 1, "{journal}");
    Ok(results[0].to_owned())
}

/// A live host over the real product with a scripted provider.
async fn scripted_host(
    root: &Path,
    isolation: &str,
    user_extra: &str,
    steps: &[String],
) -> support::TestResult<Host> {
    let script = root.join("script.jsonl");
    std::fs::write(&script, steps.join("\n"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n{isolation}\n{user_extra}\n[providers.scripted]\nfixture = {:?}\n",
        script.display().to_string()
    );
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        root,
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: root.into(),
        config: &config,
    })?;
    let env = dal_agent::Env {
        vars: std::collections::BTreeMap::from([("OPENAI_API_KEY".into(), "sk-test".into())]),
        cwd: root.into(),
        sandbox_helper: None,
    };
    Ok(Host::start(product, config, env).await?)
}

async fn prompt(
    agent: &dal_agent::Agent,
    subscription: &mut dal_agent::Subscription,
    text: &str,
) -> support::TestResult<()> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    while let Some(delivery) = tokio::time::timeout(WAIT, subscription.next()).await? {
        if let dal_agent::Delivery::Update(update) = delivery
            && let dal_core::UpdateKind::TurnEnded { .. } = &update.kind
        {
            return Ok(());
        }
    }
    Err("subscription closed before the turn ended".into())
}

/// The pending guard report visible in the session after a measuring turn.
async fn turn_summary(
    root: &Path,
    isolation: &str,
    guard_toml: &str,
    payload: &str,
) -> support::TestResult<String> {
    let body = payload
        .lines()
        .map(|line| format!("+{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let create = format!("[metrics_probe.rs#NEW]\nPUT >$:\n{body}");
    let host = scripted_host(
        root,
        isolation,
        guard_toml,
        &[
            support::tool_step(
                "patch-1",
                "patch",
                &format!("{{\"input\":\"{}\"}}", escape_json(&create)),
            ),
            support::text_step("done"),
            support::text_step("again"),
        ],
    )
    .await?;
    let workspace = Workspace::new(root.join("workspace"))?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("defaults-guard-gate"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    prompt(&agent, &mut subscription, "add the probe file").await?;
    prompt(&agent, &mut subscription, "second turn").await?;
    let view = format!("{:?}", agent.view(dal_core::PageReq::default())?);
    let key = agent.view(dal_core::PageReq::default())?.session.id;
    host.close(key).await?;
    drop(subscription);
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;
    Ok(view)
}

#[test]
fn defaults_reach_patch_search_and_guard() -> support::TestResult<()> {
    // Symbol search answers from the default product and refuses with the
    // exact symbol-off error once the default is overridden.
    let symbol_args = r#"{"mode":"symbol","pattern":"gate_symbol_probe"}"#;
    let found = call_tool(
        "search-symbols-on",
        NO_BATTERIES,
        "s1",
        "search",
        symbol_args,
    )?;
    assert!(
        found.contains("probe.rs"),
        "default symbol search answers: {found}"
    );
    let off = call_tool(
        "search-symbols-off",
        "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\nsearch_symbols = false",
        "s1",
        "search",
        symbol_args,
    )?;
    assert!(
        off.contains(SYMBOL_OFF),
        "search_symbols = false refuses symbol mode: {off}"
    );
    // Grep mode is untouched by the same override: the override moves only
    // what it names.
    let grep_args = r#"{"mode":"grep","pattern":"gate_symbol_probe","literal":true}"#;
    let grep_on = call_tool("search-grep-on", NO_BATTERIES, "s1", "search", grep_args)?;
    let grep_off = call_tool(
        "search-grep-off",
        "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\nsearch_symbols = false",
        "s1",
        "search",
        grep_args,
    )?;
    assert!(grep_on.contains("probe.rs"), "{grep_on}");
    assert!(grep_off.contains("probe.rs"), "{grep_off}");

    // The patch dialect follows edit_style: a malformed payload is refused in
    // the name of the active dialect on each product.
    let bad = call_tool(
        "patch-hashline",
        NO_BATTERIES,
        "p1",
        "patch",
        r#"{"input":"this is not a patch"}"#,
    )?;
    assert!(
        tool_text(&bad)?.contains("\"hashline\""),
        "default patch refuses as hashline: {bad}"
    );
    let anchored = call_tool(
        "patch-anchor",
        "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\nedit_style = \"anchor\"",
        "p1",
        "patch",
        r#"{"input":"this is not a patch"}"#,
    )?;
    assert!(
        tool_text(&anchored)?.contains("\"anchor\""),
        "the anchor override refuses as anchor: {anchored}"
    );

    // The guard reports its measurements by default and stays silent off.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let scratch = support::Scratch::new("defaults-guard")?;
        let root = scratch.path().to_path_buf();
        std::fs::create_dir_all(root.join("workspace"))?;
        let payload = "pub fn probe_alpha(values: &[u64]) -> u64 {\n    41\n}\n";
        let summary = turn_summary(&root, WITH_QUALITY, "", payload).await?;
        assert!(
            summary.contains("TURN CHANGE SUMMARY"),
            "default guard reports turn growth: {summary}"
        );
        let written = std::fs::read_to_string(root.join("workspace/metrics_probe.rs"))?;
        assert!(
            written.contains("probe_alpha"),
            "the measuring turn writes the probe file"
        );
        // A fresh workspace keeps the silent run's create from failing over
        // the first run's file, which would leave the guard with nothing to
        // stay silent about.
        let quiet = support::Scratch::new("defaults-guard-off")?;
        let quiet_root = quiet.path().to_path_buf();
        std::fs::create_dir_all(quiet_root.join("workspace"))?;
        let silent = turn_summary(
            &quiet_root,
            WITH_QUALITY,
            "[guard]\nenabled = false",
            payload,
        )
        .await?;
        let quiet_written = std::fs::read_to_string(quiet_root.join("workspace/metrics_probe.rs"))?;
        assert!(
            quiet_written.contains("probe_alpha"),
            "the silent turn still writes the probe file"
        );
        assert!(
            !silent.contains("TURN CHANGE SUMMARY"),
            "a disabled guard emits no report"
        );
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
