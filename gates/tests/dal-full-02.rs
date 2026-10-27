//! Scripted session turns against the in-process harness.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{collections::BTreeMap, error::Error, fs, sync::Arc, time::Duration};

use dal_agent::{Delivery, Env, SessionRef};
use dal_core::{
    Command, Config, ConfigProduct, Expect, PageReq, Part, Reply, Stop, UpdateKind, Workspace,
};
use dal_tools::{Calibration, GuardConfig, GuardFindings, guard_extension};
use support::{GateHarness, TestDir, scripted_session};

fn edit_turn_timeout(harness: &GateHarness) -> std::io::Error {
    let state = harness.agent.view(PageReq::default()).map_or_else(
        |error| format!("view unavailable: {error}"),
        |view| {
            format!(
                "turn={:?}, open={:?}, last_entry={:?}",
                view.turn,
                view.open,
                view.entries.items.last().map(|entry| &entry.kind)
            )
        },
    );
    std::io::Error::other(format!("guard edit turn timed out: {state}"))
}

const BEFORE: &str = "fn f() -> i32 {\n    1\n}\n";
const AFTER: &str = "fn f(mut x: i32) -> i32 {\n    if x > 0 { x += 1; }\n    if x > 1 { x += 1; }\n    if x > 2 { x += 1; }\n    if x > 3 { x += 1; }\n    if x > 4 { x += 1; }\n    if x > 5 { x += 1; }\n    if x > 6 { x += 1; }\n    if x > 7 { x += 1; }\n    if x > 8 { x += 1; }\n    if x > 9 { x += 1; }\n    if x > 10 { x += 1; }\n    if x > 11 { x += 1; }\n    if x > 12 { x += 1; }\n    if x > 13 { x += 1; }\n    if x > 14 { x += 1; }\n    x\n}\n";

fn scripted_edit_fixture() -> Result<String, Box<dyn Error + Send + Sync>> {
    let edit = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "tool_call_started", "id": "call-patch", "name": "patch"},
            {"type": "tool_calls_done", "calls": [{
                "id": "call-patch",
                "name": "patch",
                "args": {"kind": "parsed", "value": {"changes": [{
                    "path": "src/lib.rs",
                    "old": BEFORE,
                    "new": AFTER
                }]}}
            }]},
            {"type": "usage", "usage": {
                "input_tokens": 1,
                "cached_input_tokens": 0,
                "output_tokens": 1,
                "reasoning_tokens": null,
                "cache_write_tokens": 0,
                "cost_usd": null
            }},
            {"type": "stop", "reason": "tool_use"}
        ]
    });
    let answer = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "text_delta", "text": "scripted edit complete"},
            {"type": "tool_calls_done", "calls": []},
            {"type": "usage", "usage": {
                "input_tokens": 1,
                "cached_input_tokens": 0,
                "output_tokens": 1,
                "reasoning_tokens": null,
                "cache_write_tokens": 0,
                "cost_usd": null
            }},
            {"type": "stop", "reason": "end_turn"}
        ]
    });
    Ok(format!(
        "{}\n{}\n",
        sonic_rs::to_string(&edit)?,
        sonic_rs::to_string(&answer)?
    ))
}

async fn run_edit(
    enabled: bool,
) -> Result<(String, Option<Arc<GuardFindings>>), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    fs::create_dir_all(workspace.path().join("src"))?;
    fs::write(workspace.path().join("src/lib.rs"), BEFORE)?;
    let fixture = data.path().join("guard-scripted.jsonl");
    fs::write(&fixture, scripted_edit_fixture()?)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\napproval = \"all\"\nedit_style = \"replace\"\n[providers.scripted]\nfixture = {:?}\n[guard]\nenabled = {}\n",
        fixture.to_string_lossy(),
        enabled
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let cx = dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    };
    let mut parts = dalgon::parts(&cx)?;
    let guard = guard_extension(GuardConfig::from_section(
        config.guard(),
        &Calibration::none(),
    )?)?;
    parts.tools.observer = Some(Arc::clone(&guard.observer));
    parts.guard = guard.extension;
    let product = dalgon::assemble(&cx, parts)?;
    let env = Env {
        vars: BTreeMap::default(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let session_id = harness.agent.view(PageReq::default())?.session.id;
    let mut subscription = harness.agent.subscribe(None)?;
    let reply = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Refactor the function to reduce complexity.".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));

    let edit_text = tokio::time::timeout(Duration::from_secs(10), async {
        let mut text = String::new();
        loop {
            let Some(delivery) = subscription.next().await else {
                return Err(std::io::Error::other(
                    "session subscription closed before turn end",
                ));
            };
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::ToolSettled { outcome, .. } => {
                    assert!(!outcome.is_error, "{}", outcome.text);
                    text.push_str(&outcome.text);
                }
                UpdateKind::TurnEnded { stop, .. } => {
                    assert_eq!(*stop, Stop::EndTurn);
                    break;
                }
                _ => {}
            }
        }
        Ok::<_, std::io::Error>(text)
    })
    .await
    .map_err(|_| edit_turn_timeout(&harness))??;
    let findings = guard.findings.last(&session_id);
    assert_eq!(
        fs::read_to_string(workspace.path().join("src/lib.rs"))?,
        AFTER,
        "patch settled with: {edit_text}"
    );
    let report = tokio::time::timeout(
        Duration::from_secs(5),
        harness.host.shutdown(Duration::from_secs(1)),
    )
    .await?;
    assert_eq!(report.sessions_closed, 1);
    Ok((edit_text, findings))
}

#[tokio::test]
async fn guard_emits_four_reports_and_is_off_by_default() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let (edit_text, findings) = run_edit(true).await?;
    let normalized = edit_text.replace('\\', "/");
    assert!(
        normalized.contains("guard: src/lib.rs ploc "),
        "{edit_text}"
    );
    assert!(edit_text.contains("functions 1"), "{edit_text}");
    assert!(edit_text.contains("cc-sum 16"), "{edit_text}");
    assert!(
        edit_text.contains("f f cyclomatic 1→16 (over 15)"),
        "{edit_text}"
    );
    let findings = findings.expect("enabled guard emitted findings");
    let report = findings
        .report
        .as_deref()
        .expect("enabled guard emitted a turn report");
    assert!(
        report.contains("TURN CHANGE SUMMARY. Added 17, deleted 2, net 15, files 1, new files 0."),
        "{report}"
    );
    assert!(
        report.contains("METRICS: files 1, bands crossed 1, erosion 0.00→1.00"),
        "{report}"
    );
    assert!(report.contains("f f mass 1.7→67.9"), "{report}");
    let absolute = findings.files[0]
        .metrics
        .as_ref()
        .expect("guard reported absolute file metrics");
    assert_eq!(absolute.ploc, 18);
    let function = absolute.functions.first().unwrap();
    assert_eq!(function.name.as_ref(), "f");
    assert_eq!(function.ploc, 18);
    assert_eq!(function.cyclomatic, 16);

    let (default_output, default_findings) = run_edit(false).await?;
    assert!(
        !default_output
            .replace('\\', "/")
            .contains("guard: src/lib.rs ploc"),
        "{default_output}"
    );
    assert!(default_findings.is_none());
    Ok(())
}

#[tokio::test]
async fn four_reports_reach_model_and_user() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (edit_text, findings) = run_edit(true).await?;
    let findings = findings.expect("enabled guard emitted findings");
    let report = findings
        .report
        .as_deref()
        .expect("enabled guard emitted a turn report");
    let normalized = edit_text.replace('\\', "/");
    let absolute = findings.files[0]
        .metrics
        .as_ref()
        .expect("guard reported absolute file metrics");
    let expected_receipt = format!(
        "guard: src/lib.rs ploc {} functions {} cog-sum {} cc-sum {}",
        absolute.ploc,
        absolute.functions.len(),
        absolute.cog_sum,
        absolute.cc_sum
    );
    let receipt = normalized
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("guard: src/lib.rs ploc "))
        .expect("patch output holds the absolute receipt")
        .to_owned();
    assert_eq!(
        receipt, expected_receipt,
        "absolute metrics reach patch output byte-identical"
    );
    let edit_band = normalized
        .lines()
        .map(str::trim)
        .find(|line| line.contains("cyclomatic 1→16 (over 15)"))
        .expect("patch output holds the per-edit band line")
        .to_owned();
    assert_eq!(edit_band, "f f cyclomatic 1→16 (over 15)");
    assert!(
        report.lines().any(|line| line.trim() == edit_band),
        "per-edit band line reaches the turn report byte-identical: {report}"
    );
    let ledger = "TURN CHANGE SUMMARY. Added 17, deleted 2, net 15, files 1, new files 0. This is a report only; no automatic rewrite was started.";
    assert!(
        report.lines().any(|line| line.trim() == ledger),
        "growth deltas reach the turn report byte-identical: {report}"
    );
    let metrics = "METRICS: files 1, bands crossed 1, erosion 0.00→1.00 (human baseline 0.34±0.22)";
    assert!(
        report.lines().any(|line| line.trim() == metrics),
        "best-current metrics reach the turn report byte-identical: {report}"
    );
    assert!(
        report
            .lines()
            .any(|line| line.trim() == "f f mass 1.7→67.9"),
        "mass row reaches the turn report byte-identical: {report}"
    );
    second_turn_sees_pending().await?;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the resume scenario replays a full second turn"
)]
async fn second_turn_sees_pending() -> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    fs::create_dir_all(workspace.path().join("src"))?;
    fs::write(workspace.path().join("src/lib.rs"), BEFORE)?;
    let mut fixture = scripted_edit_fixture()?;
    let follow_up = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "text_delta", "text": "second turn complete"},
            {"type": "tool_calls_done", "calls": []},
            {"type": "usage", "usage": {
                "input_tokens": 1,
                "cached_input_tokens": 0,
                "output_tokens": 1,
                "reasoning_tokens": null,
                "cache_write_tokens": 0,
                "cost_usd": null
            }},
            {"type": "stop", "reason": "end_turn"}
        ]
    });
    fixture.push_str(&sonic_rs::to_string(&follow_up)?);
    fixture.push('\n');
    let fixture_path = data.path().join("guard-scripted.jsonl");
    fs::write(&fixture_path, &fixture)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\napproval = \"all\"\nedit_style = \"replace\"\n[providers.scripted]\nfixture = {:?}\n[guard]\nenabled = true\n",
        fixture_path.to_string_lossy(),
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let cx = dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    };
    let mut parts = dalgon::parts(&cx)?;
    let guard = guard_extension(GuardConfig::from_section(
        config.guard(),
        &Calibration::none(),
    )?)?;
    parts.tools.observer = Some(Arc::clone(&guard.observer));
    parts.guard = guard.extension;
    let product = dalgon::assemble(&cx, parts)?;
    let env = Env {
        vars: BTreeMap::default(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let session_id = harness.agent.view(PageReq::default())?.session.id;
    let mut subscription = harness.agent.subscribe(None)?;
    harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Refactor the function to reduce complexity.".into(),
            }],
        })
        .await?;
    let first_report = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let Some(delivery) = subscription.next().await else {
                return Err(std::io::Error::other(
                    "subscription closed before first turn end",
                ));
            };
            let Delivery::Update(update) = delivery else {
                continue;
            };
            if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
                break;
            }
        }
        Ok::<_, std::io::Error>(guard.findings.last(&session_id))
    })
    .await
    .map_err(|_| edit_turn_timeout(&harness))??;
    let first_report = first_report
        .as_deref()
        .and_then(|findings| findings.report.clone())
        .ok_or("first turn emitted no turn report")?;
    harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Follow up.".into(),
            }],
        })
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let Some(delivery) = subscription.next().await else {
                return Err(std::io::Error::other(
                    "subscription closed before second turn end",
                ));
            };
            let Delivery::Update(update) = delivery else {
                continue;
            };
            if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
                break;
            }
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .map_err(|_| edit_turn_timeout(&harness))??;
    let second_findings = guard.findings.last(&session_id);
    assert!(
        second_findings.is_some(),
        "second model turn still records findings after the pending report was delivered"
    );
    assert!(
        first_report.contains("TURN CHANGE SUMMARY. Added 17, deleted 2"),
        "{first_report}"
    );
    let report = tokio::time::timeout(
        Duration::from_secs(5),
        harness.host.shutdown(Duration::from_secs(1)),
    )
    .await?;
    assert_eq!(report.sessions_closed, 1);
    Ok(())
}
