// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Composition gates for the dalgona product.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use dal_agent::Product;
use dal_core::{Config, ConfigProduct};
use dalgon::BuildCx;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct Root(PathBuf);

impl Root {
    fn create() -> io::Result<Self> {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "dalgona-composition-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn build(user_toml: &str) -> Result<Product, Box<dyn Error>> {
    let root = Root::create()?;
    let factory = dalgona::product();
    let config = Config::load(
        ConfigProduct::Dalgona,
        &root.0,
        factory.defaults,
        Some(user_toml),
    )?;
    let cx = BuildCx {
        data_root: root.0.clone(),
        config: &config,
    };
    Ok(dalgona::build(&cx)?)
}

fn names(product: &Product) -> BTreeSet<&str> {
    product
        .extensions
        .iter()
        .map(dal_agent::ext::Extension::name)
        .collect()
}

fn failure(user_toml: &str) -> Result<String, Box<dyn Error>> {
    match build(user_toml) {
        Ok(_) => Err(io::Error::other(format!("accepted: {user_toml}")).into()),
        Err(error) => Ok(error.to_string()),
    }
}

const COLLAPSE_STREAM_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;
const FABRICATED_STREAM_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"<unavailable-tool-call>"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;
const CLEAN_STREAM_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"clean response"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;
const QUALITY_APPLY_STREAM_STEP: &str = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"quality-apply-missing","name":"quality_apply"},{"type":"tool_calls_done","calls":[{"id":"quality-apply-missing","name":"quality_apply","args":{"kind":"parsed","value":{"id":"missing-offer"}}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}"#;
const CONTROL_TOKEN_LEAK_TEXT: &str = "<|eos|><|eos|><|eos|><|eos|>";
const CONTROL_TOKEN_LEAK_STREAM_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"<|eos|><|eos|><|eos|><|eos|>"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;
const REPETITIVE_TURN_TEXT: &str =
    "The same settled answer remains stable across every repeated turn.";
const REPETITIVE_TURN_STREAM_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"The same settled answer remains stable across every repeated turn."},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

fn streamed_journal(enabled: &str, steps: &[&str]) -> Result<String, Box<dyn Error>> {
    let root = Root::create()?;
    let config = root.0.join("config/dalgona");
    let data = root.0.join("data");
    fs::create_dir_all(&config)?;
    fs::create_dir_all(&data)?;
    let fixture = root.0.join("detector-stream.jsonl");
    fs::write(&fixture, steps.join("\n") + "\n")?;
    fs::write(
        config.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\ndisabled_batteries = [\"judged\"]\n[rule_sets]\nenabled = {enabled}\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_dalgona"))
        .args([
            "-p",
            "--json",
            "--approval",
            "all",
            "Return the supplied stream.",
        ])
        .current_dir(&root.0)
        .env_clear()
        .env("HOME", &root.0)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_CONFIG_HOME", root.0.join("config"))
        .env("XDG_DATA_HOME", &data)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "scripted detector run failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    let journal = find_journal(&data)?;
    Ok(fs::read_to_string(journal)?)
}

fn multi_turn_journal(enabled: &str, steps: &[&str]) -> Result<String, Box<dyn Error>> {
    let root = Root::create()?;
    let config = root.0.join("config/dalgona");
    let data = root.0.join("data");
    fs::create_dir_all(&config)?;
    fs::create_dir_all(&data)?;
    let fixture = root.0.join("detector-stream.jsonl");
    for (turn, step) in steps.iter().enumerate() {
        fs::write(&fixture, format!("{step}\n"))?;
        fs::write(
            config.join("dal.toml"),
            format!(
                "model = \"openai-responses/gpt-6\"\ndisabled_batteries = [\"judged\"]\n[rule_sets]\nenabled = {enabled}\n[providers.scripted]\nfixture = {:?}\n",
                fixture.to_string_lossy()
            ),
        )?;
        let mut args = vec!["-p", "--json", "--approval", "all"];
        if turn > 0 {
            args.push("--continue");
        }
        args.push("Return the supplied stream.");
        let output = Command::new(env!("CARGO_BIN_EXE_dalgona"))
            .args(args)
            .current_dir(&root.0)
            .env_clear()
            .env("HOME", &root.0)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", root.0.join("config"))
            .env("XDG_DATA_HOME", &data)
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "scripted detector turn {turn} failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
    }
    let journal = find_journal(&data)?;
    Ok(fs::read_to_string(journal)?)
}

fn find_journal(root: &Path) -> io::Result<PathBuf> {
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
                return Ok(path);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "scripted detector run produced no journal",
    ))
}

fn journal_has_extension_rule(
    journal: &str,
    rule: &str,
    action: Option<&str>,
) -> Result<bool, Box<dyn Error>> {
    for line in journal.lines() {
        let record = dal_core::decode(line.as_bytes())?.record;
        let dal_core::Record::Ext { kind, body, .. } = record else {
            continue;
        };
        if kind.as_ref() != "rule_fired" {
            continue;
        }
        let fields: BTreeMap<String, dal_core::RawJson> = body.decode_as()?;
        let Some(raw_rule) = fields.get("rule") else {
            continue;
        };
        let fired_rule: String = raw_rule.decode_as()?;
        if fired_rule != rule {
            continue;
        }
        if let Some(expected_action) = action {
            let Some(raw_action) = fields.get("action") else {
                continue;
            };
            let actual_action: String = raw_action.decode_as()?;
            if actual_action != expected_action {
                continue;
            }
        }
        return Ok(true);
    }
    Ok(false)
}

fn journal_has_rule_fired(journal: &str, rule: &str) -> Result<bool, Box<dyn Error>> {
    for line in journal.lines() {
        let record = dal_core::decode(line.as_bytes())?.record;
        if let dal_core::Record::RuleFired { rule: fired, .. } = record
            && fired.as_ref() == rule
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn journal_assistant_text_count(journal: &str, expected: &str) -> Result<usize, Box<dyn Error>> {
    let mut count = 0;
    for line in journal.lines() {
        let record = dal_core::decode(line.as_bytes())?.record;
        let dal_core::Record::Assistant(entry) = record else {
            continue;
        };
        let dal_core::EntryKind::Assistant { content, .. } = entry.kind else {
            continue;
        };
        count += content
            .iter()
            .filter(|block| {
                matches!(
                    block,
                    dal_core::Block::Text { text, .. } if text.as_ref() == expected
                )
            })
            .count();
    }
    Ok(count)
}

fn journal_has_quality_apply_failure(journal: &str) -> Result<bool, Box<dyn Error>> {
    const EXPECTED: &str = "quality_apply: no offer missing-offer.";
    for line in journal.lines() {
        let record = dal_core::decode(line.as_bytes())?.record;
        let dal_core::Record::ToolResult(entry) = record else {
            continue;
        };
        let dal_core::EntryKind::ToolResult {
            name, error, parts, ..
        } = entry.kind
        else {
            continue;
        };
        if name.as_ref() != "quality_apply" || !error {
            continue;
        }
        if parts.iter().any(|part| {
            matches!(
                part,
                dal_core::JournalPart::Text { text } if text.as_ref() == EXPECTED
            )
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[test]
fn diagram_prompt_tracks_the_strict_tui_setting() -> Result<(), Box<dyn Error>> {
    let root = Root::create()?;
    let factory = dalgona::product();
    for (user_toml, enabled) in [("", false), ("[tui]\ndiagrams = true\n", true)] {
        let config = Config::load(
            ConfigProduct::Dalgona,
            &root.0,
            factory.defaults,
            Some(user_toml),
        )?;
        assert_eq!(config.tui().diagrams, enabled);
        let cx = BuildCx {
            data_root: root.0.clone(),
            config: &config,
        };
        let product = dalgona::build(&cx)?;
        let occurrences = product
            .extensions
            .iter()
            .filter_map(dal_agent::ext::Extension::prompt_section)
            .filter(|section| matches!(
                section,
                dal_agent::ext::PromptSection::Static { text, order: dal_agent::ext::PromptOrder::D2, .. }
                    if text.as_ref() == dal_core::PROMPT_DIAGRAMS
            ))
            .count();
        assert_eq!(occurrences, usize::from(enabled));
    }
    Ok(())
}

#[test]
fn docs_command_serves_both_first_party_schemes() -> Result<(), Box<dyn Error>> {
    let root = Root::create()?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_dalgona"));
    command
        .env_clear()
        .env("HOME", &root.0)
        .env("XDG_CONFIG_HOME", root.0.join("config"))
        .env("XDG_DATA_HOME", root.0.join("data"));
    let page = command.args(["docs", "dalgona://config"]).output()?;
    assert_eq!(page.status.code(), Some(0));
    let product = build("")?;
    let manual = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "dalgona")
        .ok_or_else(|| io::Error::other("the product has no dalgona manual extension"))?;
    let expected = manual
        .docs()
        .iter()
        .find(|page| &*page.path == "config")
        .ok_or_else(|| io::Error::other("the dalgona manual has no config page"))?
        .text
        .to_string();
    assert_eq!(
        String::from_utf8_lossy(&page.stdout).as_ref(),
        expected.as_str(),
        "docs output is not the config page"
    );
    assert_eq!(page.stderr, b"");

    let mut command = Command::new(env!("CARGO_BIN_EXE_dalgona"));
    command
        .env_clear()
        .env("HOME", &root.0)
        .env("XDG_CONFIG_HOME", root.0.join("config"))
        .env("XDG_DATA_HOME", root.0.join("data"));
    let listing = command.args(["docs"]).output()?;
    assert_eq!(listing.status.code(), Some(0));
    let text = String::from_utf8_lossy(&listing.stdout);
    assert!(
        text.starts_with("# dal://"),
        "listing is not the manual index"
    );
    assert!(
        text.contains("# dalgona://"),
        "listing has no dalgona index"
    );
    assert!(
        text.contains("dalgona://config"),
        "listing omits the config page"
    );
    assert_eq!(listing.stderr, b"");
    Ok(())
}

#[test]
fn the_manual_is_one_extension_with_every_page() -> Result<(), Box<dyn Error>> {
    let product = build("")?;
    let manual = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "dalgona")
        .ok_or_else(|| io::Error::other("the product has no dalgona manual extension"))?;
    let pages: BTreeSet<&str> = manual.docs().iter().map(|page| &*page.path).collect();
    assert_eq!(
        pages,
        BTreeSet::from([
            "agents",
            "anti-slop",
            "ask",
            "batteries",
            "changelog",
            "config",
            "eval",
            "goal",
            "history",
            "judge",
            "judged",
            "mcp",
            "orchestration",
            "philosophy",
            "plan",
            "procs",
            "quality",
            "review",
            "rules",
            "search",
            "skills",
            "symbols",
            "todo",
            "ttsr/rules",
            "web",
            "work",
            "workflows",
        ])
    );
    for page in manual.docs() {
        assert!(page.text.starts_with("# "), "{} has no heading", page.path);
        assert!(!page.title.is_empty(), "{} has no title", page.path);
    }
    Ok(())
}

#[test]
fn a_bad_section_fails_the_build_and_names_the_bad_key() -> Result<(), Box<dyn Error>> {
    let cases = [
        ("[plugin.ask]\nmisspelled = true\n", "misspelled"),
        ("[plugin.skills]\nmisspelled = true\n", "misspelled"),
        ("[plugin.ttsr-rules]\nmisspelled = true\n", "misspelled"),
        ("[plugin.procs]\nmisspelled = true\n", "misspelled"),
        ("[plugin.work]\nenabled = true\n", "plugin.plan"),
        ("[plugin.web]\nbogus = 1\n", "bogus"),
        (
            "[plugin.review]\nmax_rounds = 11\n",
            "plugin.review.max_rounds",
        ),
        ("[plugin.judged]\nthinkin = true\n", "thinkin"),
        ("[plugin.mcp]\nservers = 1\n", "servers"),
        ("[plugin.quality]\nstrict = true\n", "strict"),
        ("[plugin.plan]\nverbose = true\n", "verbose"),
        ("[plugin.orchestration.goal]\nbogus = 1\n", "bogus"),
        ("[rule_sets]\nenabled = \"steer\"\n", "rule_sets.enabled"),
        ("[rule_sets]\nextra = 1\n", "extra"),
        ("[rule_sets]\nenabled = [\"nope\"]\n", "nope"),
    ];
    for (toml, fragment) in cases {
        let error = failure(toml)?;
        assert!(error.contains(fragment), "{toml:?} -> {error}");
    }
    Ok(())
}

#[test]
fn each_enabled_switch_removes_only_its_battery() -> Result<(), Box<dyn Error>> {
    let full = build("")?;
    let all = names(&full);
    for (battery, toml) in [
        ("web", "[plugin.web]\nenabled = false\n"),
        ("review", "[plugin.review]\nenabled = false\n"),
        ("mcp", "[plugin.mcp]\nenabled = false\n"),
        ("work", "[plugin.plan]\nenabled = false\n"),
        ("procs", "[plugin.procs]\nenabled = false\n"),
    ] {
        let product = build(toml)?;
        let mut expected = all.clone();
        expected.remove(battery);
        assert_eq!(names(&product), expected, "{battery}");
    }
    Ok(())
}

#[test]
fn an_invalid_history_section_still_builds_with_its_failing_compactor() -> Result<(), Box<dyn Error>>
{
    let product = build("[plugin.history]\nshare = 0.9\n")?;
    let history = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "history")
        .ok_or("history extension must stay registered for an invalid section")?;
    assert_eq!(history.compactors().len(), 1);
    // The registered compactor is the real primary one that refuses every
    // compaction, not a silent fallback that pretends success.
    assert!(!history.is_fallback_compactor("history"));
    Ok(())
}

#[test]
fn an_invalid_history_section_does_not_rescue_a_bad_neighbour_section() -> Result<(), Box<dyn Error>>
{
    let toml = "[plugin.history]\nshare = 0.9\n[plugin.web]\nbogus = 1\n";
    let error = failure(toml)?;
    assert!(error.contains("bogus"), "{toml:?} -> {error}");
    Ok(())
}

fn run_scripted_turn(
    extra_toml: &str,
    steps: &[&str],
) -> Result<(std::process::Output, Option<String>), Box<dyn Error>> {
    let root = Root::create()?;
    let config = root.0.join("config/dalgona");
    let data = root.0.join("data");
    fs::create_dir_all(&config)?;
    fs::create_dir_all(&data)?;
    let fixture = root.0.join("stream.jsonl");
    fs::write(&fixture, steps.join("\n") + "\n")?;
    fs::write(
        config.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\ndisabled_batteries = [\"judged\"]\n[providers.scripted]\nfixture = {:?}\n{extra_toml}",
            fixture.to_string_lossy()
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_dalgona"))
        .args([
            "-p",
            "--json",
            "--approval",
            "all",
            "Return the supplied stream.",
        ])
        .current_dir(&root.0)
        .env_clear()
        .env("HOME", &root.0)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_CONFIG_HOME", root.0.join("config"))
        .env("XDG_DATA_HOME", &data)
        .output()?;
    let journal = find_journal(&data)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok());
    Ok((output, journal))
}

#[test]
fn invalid_history_section_composes_load_and_reply_with_typed_warning() -> Result<(), Box<dyn Error>>
{
    let (output, journal) =
        run_scripted_turn("[plugin.history]\nshare = 0.9\n", &[CLEAN_STREAM_STEP])?;
    assert!(
        output.status.success(),
        "invalid history section must not abort startup or reply: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("history: [plugin.history].share must be a number from 0.1 to 0.7."),
        "startup emits the exact typed warning: {stderr}"
    );
    let journal = journal.ok_or("successful run produced a journal")?;
    assert_eq!(
        journal_assistant_text_count(&journal, "clean response")?,
        1,
        "reply was recorded in session history"
    );
    Ok(())
}

#[test]
fn foreign_history_key_composes_load_and_reply_with_typed_warning() -> Result<(), Box<dyn Error>> {
    let (output, journal) =
        run_scripted_turn("[plugin.history]\nunknown = true\n", &[CLEAN_STREAM_STEP])?;
    assert!(
        output.status.success(),
        "unknown history key must not abort startup or reply: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning =
        dalgona_batteries::history::HistoryConfigError::UnknownKey("unknown".into()).to_string();
    assert_eq!(
        stderr.lines().filter(|line| *line == warning).count(),
        1,
        "startup emits the typed warning exactly once: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(&warning),
        "startup warnings must not contaminate reply stdout"
    );
    let journal = journal.ok_or("successful run produced a journal")?;
    assert_eq!(
        journal_assistant_text_count(&journal, "clean response")?,
        1,
        "reply was recorded in session history"
    );
    Ok(())
}

#[test]
fn invalid_history_section_with_unrelated_config_error_still_fails() -> Result<(), Box<dyn Error>> {
    let (output, journal) = run_scripted_turn(
        "[plugin.history]\nshare = 0.9\n[plugin.web]\nbogus = 1\n",
        &[CLEAN_STREAM_STEP],
    )?;
    assert!(
        !output.status.success(),
        "unrelated config error in the same load must fail startup"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bogus"),
        "stderr must report the unrelated error: {stderr}"
    );
    assert!(
        !stderr.contains("history:"),
        "no history warning is emitted when an unrelated config error fails the build"
    );
    assert!(
        journal.is_none(),
        "no journal or reply is produced when config load fails"
    );
    Ok(())
}

#[test]
fn history_disabled_keeps_the_extension_without_a_compactor() -> Result<(), Box<dyn Error>> {
    let enabled = build("")?;
    let disabled = build("[plugin.history]\nenabled = false\n")?;
    let compactors = |product: &Product| {
        product
            .extensions
            .iter()
            .filter(|extension| extension.name() == "history")
            .map(|extension| extension.compactors().len())
            .sum::<usize>()
    };
    assert_eq!(compactors(&enabled), 1);
    assert_eq!(compactors(&disabled), 0);
    Ok(())
}

#[test]
fn the_guard_stays_when_quality_is_disabled() -> Result<(), Box<dyn Error>> {
    let with_quality = build("")?;
    let without = build("disabled_batteries = [\"quality\"]\n")?;
    let count = |product: &Product, name: &str| {
        product
            .extensions
            .iter()
            .filter(|extension| extension.name() == name)
            .count()
    };
    assert_eq!(count(&with_quality, "guard"), 1);
    assert_eq!(count(&with_quality, "quality"), 1);
    assert_eq!(count(&without, "guard"), 1);
    assert_eq!(count(&without, "quality"), 0);
    Ok(())
}

#[test]
fn detector_set_controls_real_streams_without_removing_quality_tools() -> Result<(), Box<dyn Error>>
{
    let quality_apply = streamed_journal("[]", &[QUALITY_APPLY_STREAM_STEP, CLEAN_STREAM_STEP])?;
    assert!(journal_has_quality_apply_failure(&quality_apply)?);
    let quality_apply_detectors = streamed_journal(
        "[\"detectors\"]",
        &[QUALITY_APPLY_STREAM_STEP, CLEAN_STREAM_STEP],
    )?;
    assert!(
        journal_has_quality_apply_failure(&quality_apply_detectors)?,
        "quality_apply missing when detectors are enabled"
    );

    for (enabled, expected_report) in [
        ("[]", false),
        ("[\"steer\"]", false),
        ("[\"detectors\"]", true),
    ] {
        let journal = streamed_journal(enabled, &[COLLAPSE_STREAM_STEP])?;
        let report = journal_has_extension_rule(&journal, "collapse-repetition", None)?;
        assert_eq!(report, expected_report, "collapse report for {enabled}");
    }

    for (enabled, expected_interrupt) in [
        ("[]", false),
        ("[\"steer\"]", false),
        ("[\"detectors\"]", true),
    ] {
        let journal = streamed_journal(enabled, &[FABRICATED_STREAM_STEP, CLEAN_STREAM_STEP])?;
        let control_interrupt =
            journal_has_rule_fired(&journal, "fabricated-unavailable-tool-call")?;
        let extension_interrupt = journal_has_extension_rule(
            &journal,
            "fabricated-unavailable-tool-call",
            Some("interrupt"),
        )?;
        assert_eq!(
            control_interrupt, expected_interrupt,
            "fabricated-call control record for {enabled}"
        );
        assert_eq!(
            extension_interrupt, expected_interrupt,
            "fabricated-call extension record for {enabled}"
        );
    }

    for (enabled, expected_report) in [
        ("[]", false),
        ("[\"steer\"]", false),
        ("[\"detectors\"]", true),
    ] {
        let journal = streamed_journal(enabled, &[CONTROL_TOKEN_LEAK_STREAM_STEP])?;
        assert_eq!(
            journal_assistant_text_count(&journal, CONTROL_TOKEN_LEAK_TEXT)?,
            1,
            "control-token stimulus for {enabled}"
        );
        let report = journal_has_extension_rule(&journal, "control-token-leak", None)?;
        assert_eq!(
            report, expected_report,
            "control-token-leak report for {enabled}"
        );
    }

    for (enabled, expected_report) in [
        ("[]", false),
        ("[\"steer\"]", false),
        ("[\"detectors\"]", true),
    ] {
        let journal = multi_turn_journal(
            enabled,
            &[
                REPETITIVE_TURN_STREAM_STEP,
                REPETITIVE_TURN_STREAM_STEP,
                REPETITIVE_TURN_STREAM_STEP,
            ],
        )?;
        assert_eq!(
            journal_assistant_text_count(&journal, REPETITIVE_TURN_TEXT)?,
            3,
            "repetitive-turn stimuli for {enabled}"
        );
        let report = journal_has_extension_rule(&journal, "repetitive-turns", None)?;
        assert_eq!(
            report, expected_report,
            "repetitive-turns report for {enabled}"
        );
    }

    Ok(())
}

#[test]
fn batteries_register_in_name_byte_order_after_the_builtins() -> Result<(), Box<dyn Error>> {
    const SORTED_BUNDLED: &[&str] = &[
        "ask",
        "history",
        "judged",
        "mcp",
        "orchestration",
        "procs",
        "quality",
        "review",
        "skills",
        "ttsr-rules",
        "web",
        "work",
    ];
    // The battery run is every bundled battery plus the Builtin dalgona
    // manual, which compose() sorts into the same run. The manual is pushed
    // first, so without the sort it would sit before ask instead of between
    // ask and history: this tail pins the sort as well as the order.
    const SORTED_RUN: &[&str] = &[
        "ask",
        "dalgona",
        "history",
        "judged",
        "mcp",
        "orchestration",
        "procs",
        "quality",
        "review",
        "skills",
        "ttsr-rules",
        "web",
        "work",
    ];

    fn battery_run(user_toml: &str) -> Result<Vec<String>, Box<dyn Error>> {
        let product = build(user_toml)?;
        let full: Vec<&str> = product
            .extensions
            .iter()
            .map(dal_agent::ext::Extension::name)
            .collect();
        let start = full
            .iter()
            .position(|name| *name == "ask")
            .ok_or_else(|| io::Error::other("the product has no ask battery"))?;
        let builtins_only = product
            .extensions
            .iter()
            .take(start)
            .all(|extension| extension.origin() != dal_core::Origin::Bundled);
        assert!(builtins_only, "a bundled battery sits among the builtins");
        Ok(full
            .iter()
            .skip(start)
            .map(|name| (*name).to_owned())
            .collect())
    }

    fn expected(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }
    fn sorted_run_without(name: &str) -> Vec<String> {
        let survivors: Vec<&str> = SORTED_RUN
            .iter()
            .copied()
            .filter(|candidate| *candidate != name)
            .collect();
        expected(&survivors)
    }

    // Exact inventory in byte order, not just self-sorted.
    let product = build("")?;
    let bundled: Vec<&str> = product
        .extensions
        .iter()
        .filter(|extension| extension.origin() == dal_core::Origin::Bundled)
        .map(dal_agent::ext::Extension::name)
        .collect();
    assert_eq!(bundled.as_slice(), SORTED_BUNDLED);
    assert_eq!(battery_run("")?, expected(SORTED_RUN));
    // Survivors of a removal stay in relative byte order with the manual still
    // in its sorted slot.
    assert_eq!(
        battery_run("disabled_batteries = [\"history\"]\n")?,
        sorted_run_without("history"),
    );
    assert_eq!(
        battery_run("disabled_batteries = [\"quality\"]\n")?,
        sorted_run_without("quality"),
    );
    // History disabled through its own switch stays registered and keeps its
    // sorted slot between dalgona and judged.
    assert_eq!(
        battery_run("[plugin.history]\nenabled = false\n")?,
        expected(SORTED_RUN),
    );
    Ok(())
}
