//! Binary-boundary tests for `dalgon dev` journal surgery and fold attribution.

mod support;

use std::{
    error::Error,
    fs,
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use dal_core::{Gen, Header, Product, Record, SessionId, Timestamp, TurnId, Workspace, encode};
use support::CliFixture;

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN)
}

/// Writes a real journal through the wire codec so the dev commands decode
/// exactly what a live session produces.
fn write_journal(dir: &Path, records: &[Record]) -> Result<PathBuf, Box<dyn Error>> {
    let mut bytes = Vec::new();
    for record in records {
        // encode() already terminates the record line; a second LF would be
        // a blank line, which the store grammar reads as damage.
        bytes.extend_from_slice(&encode(record)?);
    }
    let path = dir.join("journal.jsonl");
    fs::write(&path, bytes)?;
    Ok(path)
}

fn sample_records(workspace: &Path, name: Option<&str>) -> Result<Vec<Record>, Box<dyn Error>> {
    let at = Timestamp::now();
    Ok(vec![
        Record::Session(Header {
            id: SessionId::new_v7(),
            at,
            workspace: Workspace::new(workspace.to_path_buf())?,
            product: Product::Dal,
            from: None,
        }),
        Record::Boot {
            at,
            r#gen: Gen::new(nz(1)),
            version: "0.1.0".into(),
        },
        Record::Name {
            at,
            name: name.map(Into::into),
        },
        Record::Archive { at, archived: true },
        Record::TurnStart {
            at,
            turn: TurnId::new(nz(1)),
        },
    ])
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn dev_journal_replay_folds_a_real_journal() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let session = fixture.data.join("session-one");
    fs::create_dir_all(&session)?;
    let journal = write_journal(&session, &sample_records(&fixture.data, Some("probe"))?)?;
    let output = fixture.output(&[
        "dev",
        "journal",
        "replay",
        journal.to_str().expect("utf8 path"),
    ])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("records: 5"), "{text}");
    assert!(text.contains("phase:"), "{text}");
    assert!(text.contains("compactions:"), "{text}");
    Ok(())
}

#[test]
fn dev_journal_diff_reports_only_changed_fields() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let before_dir = fixture.data.join("before");
    let after_dir = fixture.data.join("after");
    fs::create_dir_all(&before_dir)?;
    fs::create_dir_all(&after_dir)?;
    let before = write_journal(&before_dir, &sample_records(&fixture.data, Some("one"))?)?;
    let mut after = sample_records(&fixture.data, Some("two"))?;
    after.push(Record::Archive {
        at: Timestamp::now(),
        archived: false,
    });
    let after = write_journal(&after_dir, &after)?;
    let output = fixture.output(&[
        "dev",
        "journal",
        "diff",
        before.to_str().expect("utf8 path"),
        after.to_str().expect("utf8 path"),
    ])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.lines()
            .any(|line| line.starts_with("- ") || line.starts_with("+ ")),
        "expected a field diff, got: {text}"
    );
    let same = fixture.output(&[
        "dev",
        "journal",
        "diff",
        before.to_str().expect("utf8 path"),
        before.to_str().expect("utf8 path"),
    ])?;
    assert!(same.status.success(), "{}", stderr(&same));
    assert!(
        stdout(&same).contains("states identical"),
        "{}",
        stdout(&same)
    );
    Ok(())
}

#[test]
fn dev_journal_torn_leaves_a_tail_replay_rejects() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let session = fixture.data.join("session-two");
    fs::create_dir_all(&session)?;
    let journal = write_journal(&session, &sample_records(&fixture.data, None)?)?;
    let torn = fixture.data.join("torn.jsonl");
    let output = fixture.output(&[
        "dev",
        "journal",
        "torn",
        journal.to_str().expect("utf8 path"),
        torn.to_str().expect("utf8 path"),
    ])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let torn_bytes = fs::read(&torn)?;
    assert!(
        torn_bytes.len() < usize::try_from(fs::metadata(&journal)?.len()).unwrap_or(usize::MAX)
    );
    let replay = fixture.output(&[
        "dev",
        "journal",
        "replay",
        torn.to_str().expect("utf8 path"),
    ])?;
    // Store semantics: the unterminated tail is not a durable record, so
    // replay succeeds on the complete prefix and reports what it ignored.
    assert!(replay.status.success(), "{}", stderr(&replay));
    let replay_out = String::from_utf8_lossy(&replay.stdout);
    assert!(
        replay_out.contains("torn tail:"),
        "expected a torn-tail note, got {replay_out}"
    );
    Ok(())
}

#[test]
fn dev_journal_sidecar_lists_and_dumps() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let session = fixture.data.join("session-three");
    fs::create_dir_all(&session)?;
    write_journal(&session, &sample_records(&fixture.data, None)?)?;
    fs::write(session.join("state"), "rev = 2\n")?;
    let list = fixture.output(&[
        "dev",
        "journal",
        "sidecar",
        session.to_str().expect("utf8 path"),
    ])?;
    assert!(list.status.success(), "{}", stderr(&list));
    assert!(stdout(&list).contains("state"), "{}", stdout(&list));
    let dump = fixture.output(&[
        "dev",
        "journal",
        "sidecar",
        session.to_str().expect("utf8 path"),
        "state",
    ])?;
    assert!(dump.status.success(), "{}", stderr(&dump));
    assert!(stdout(&dump).contains("rev = 2"), "{}", stdout(&dump));
    let missing = fixture.output(&[
        "dev",
        "journal",
        "sidecar",
        session.to_str().expect("utf8 path"),
        "absent",
    ])?;
    assert!(!missing.status.success());
    Ok(())
}

#[test]
fn dev_fold_attributes_fields_to_records() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let session = fixture.data.join("session-four");
    fs::create_dir_all(&session)?;
    let journal = write_journal(&session, &sample_records(&fixture.data, Some("tagged"))?)?;
    let output = fixture.output(&["dev", "fold", journal.to_str().expect("utf8 path")])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "{text}");
    assert!(lines[0].contains("Session"), "{}", lines[0]);
    assert!(lines[1].contains("Boot"), "{}", lines[1]);
    assert!(lines[4].contains("TurnStart"), "{}", lines[4]);
    assert!(
        text.contains('+'),
        "expected field additions on the first folds: {text}"
    );
    Ok(())
}

/// Writes one scenario file next to the fixture root so step paths resolve
/// against its directory.
fn write_scenario(fixture: &CliFixture, lines: &[&str]) -> Result<PathBuf, Box<dyn Error>> {
    let path = fixture
        .data
        .join(format!("scenario-{}.jsonl", SessionId::new_v7()));
    fs::write(&path, lines.join("\n"))?;
    Ok(path)
}

#[test]
fn dev_run_drives_a_scripted_session() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let scenario = write_scenario(
        &fixture,
        &[
            r#"{"provider":{"script":[{"kind":"events","events":[{"type":"text_delta","text":"hello from the script"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}]}}"#,
            r#"{"write":{"path":"seed.txt","text":"seeded bytes"}}"#,
            r#"{"prompt":"say hi"}"#,
            r#"{"expect":{"update":"turn_ended"}}"#,
            r#"{"expect":{"journal":"TurnEnd"}}"#,
            r#"{"expect":{"file":{"path":"seed.txt","contains":"seeded"}}}"#,
            r#"{"comment":"the turn ended and the journal records it"}"#,
        ],
    )?;
    let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("accepted turn 1"), "{text}");
    assert!(text.contains("update turn_ended"), "{text}");
    assert!(text.contains("journal TurnEnd"), "{text}");
    assert!(text.contains("scenario passed"), "{text}");
    Ok(())
}

#[test]
fn dev_run_surfaces_and_answers_an_approval_request() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let scenario = write_scenario(
        &fixture,
        &[
            r#"{"provider":{"script":[{"kind":"events","events":[{"type":"tool_calls_done","calls":[{"id":"call-1","name":"exec","args":{"kind":"parsed","value":{"command":"echo hi","timeout_seconds":60}}}]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]},{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}]}}"#,
            r#"{"set_approval":"ask"}"#,
            r#"{"prompt":"run echo hi"}"#,
            r#"{"expect":{"request":{"kind":"approval","answer":"approve"}}}"#,
            r#"{"expect":{"update":{"kind":"turn_ended","contains":"end_turn"}}}"#,
        ],
    )?;
    let output = fixture.output(&[
        "dev",
        "run",
        "--consent",
        scenario.to_str().expect("utf8 path"),
    ])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("request approval"), "{text}");
    assert!(text.contains("scenario passed"), "{text}");
    Ok(())
}

/// In-band authorization must come from the invoker's command line: without
/// `--consent`, `set_approval`, an `answer` step, an `expect.request`
/// answer, and a config-declared `approval` all fail closed. Reverting the
/// guard turns each denial into the step's own failure or a pass.
#[test]
fn dev_run_denies_in_band_authorization_without_consent() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    for (name, line) in [
        ("set_approval", r#"{"set_approval":"all"}"#),
        ("answer", r#"{"answer":"approve"}"#),
        (
            "expect-answer",
            r#"{"expect":{"request":{"kind":"approval","answer":"approve"}}}"#,
        ),
        ("config-approval", r#"{"config":"approval = \"all\""}"#),
    ] {
        let scenario = write_scenario(&fixture, &[line])?;
        let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
        assert!(!output.status.success(), "{name} passed without --consent");
        let text = stderr(&output);
        assert!(text.contains("--consent"), "{name}: {text}");
    }
    Ok(())
}

/// An export path is a write like any other: it must stay inside the run
/// workspace. Reverting the containment check lets the export target escape
/// to `../out.md`.
#[test]
fn dev_run_rejects_an_export_path_outside_the_workspace() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let scenario = write_scenario(
        &fixture,
        &[
            r#"{"provider":{"script":[{"kind":"events","events":[{"type":"text_delta","text":"hi"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}]}}"#,
            r#"{"export":{"path":"../out.md","format":"markdown"}}"#,
        ],
    )?;
    let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
    assert!(!output.status.success(), "the escape exported");
    let text = stderr(&output);
    assert!(text.contains("escapes the workspace"), "{text}");
    Ok(())
}

/// Object step variants decode strictly: a typo'd key must fail the scenario
/// instead of being silently dropped. Reverting `deny_unknown_fields` on any
/// of these structs lets the line decode.
#[test]
fn dev_run_rejects_unknown_keys_in_step_objects() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    for line in [
        r#"{"run":{"name":"compact","bogus":1}}"#,
        r#"{"plugin":{"name":"probe","file":"probe.star","bogus":1}}"#,
        r#"{"answer":{"request":"approval","answer":"approve","bogus":1}}"#,
        r#"{"config":{"file":"cfg.toml","bogus":1}}"#,
        r#"{"session":{"new":{"name":"x","bogus":1}}}"#,
        r#"{"expect":{"request":{"kind":"approval","answer":{"value":1,"bogus":2}}}}"#,
    ] {
        let scenario = write_scenario(&fixture, &[line])?;
        let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
        assert!(!output.status.success(), "{line} decoded");
        let text = stderr(&output);
        assert!(
            text.contains("bogus") || text.contains("not a step object"),
            "{line}: {text}"
        );
    }
    Ok(())
}

/// A scenario reached by a relative path anchors its directory at the
/// invocation cwd: `{file}` reads resolve beside the scenario file, not
/// against the run's data root or workspace.
#[test]
fn dev_run_resolves_scenario_files_beside_a_relative_scenario() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let dir = fixture.cwd().join("scen");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("probe.star"), "# probe\n")?;
    let scenario = dir.join("scenario.jsonl");
    fs::write(
        &scenario,
        "{\"plugin\":{\"name\":\"probe\",\"file\":\"probe.star\"}}\n",
    )?;
    let output = fixture.output(&["dev", "run", "scen/scenario.jsonl"])?;
    assert!(
        output.status.success(),
        "the plugin file must resolve beside the scenario: {}",
        stderr(&output)
    );
    Ok(())
}

#[test]
fn dev_run_loads_the_bundled_devprobe_plugin() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let scenario = write_scenario(
        &fixture,
        &[
            r#"{"plugin":"devprobe"}"#,
            r#"{"provider":{"script":[{"kind":"events","events":[{"type":"tool_calls_done","calls":[{"id":"call-1","name":"devprobe__echo","args":{"kind":"parsed","value":{"value":"probe-echoed"}}}]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]},{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}]}}"#,
            r#"{"prompt":"call the echo tool"}"#,
            r#"{"expect":{"update":{"kind":"tool_settled","contains":"probe-echoed"}}}"#,
            r#"{"expect":{"update":{"kind":"turn_ended","contains":"end_turn"}}}"#,
        ],
    )?;
    let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("plugin devprobe"), "{text}");
    assert!(text.contains("scenario passed"), "{text}");
    Ok(())
}

#[test]
fn dev_run_fails_closed_when_no_provider_is_given() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let scenario = write_scenario(&fixture, &[r#"{"prompt":"hi"}"#])?;
    let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("provider"), "{text}");
    Ok(())
}

#[test]
fn dev_run_rejects_a_plugin_name_that_is_not_one_component() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fs::write(fixture.data.join("evil.star"), "def on_boot():\n    pass\n")?;
    let scenario = write_scenario(
        &fixture,
        &[r#"{"plugin":{"name":"../escape","file":"evil.star"}}"#],
    )?;
    let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("single path component"), "{text}");
    Ok(())
}

#[test]
fn dev_run_continue_adopts_a_kept_root() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let first = write_scenario(
        &fixture,
        &[
            r#"{"provider":{"script":[{"kind":"events","events":[{"type":"text_delta","text":"first"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}]}}"#,
            r#"{"session":{"new":{"name":"carried"}}}"#,
            r#"{"prompt":"one"}"#,
            r#"{"expect":{"update":"turn_ended"}}"#,
        ],
    )?;
    let kept = fixture.output(&["dev", "run", "--keep", first.to_str().expect("utf8 path")])?;
    assert!(kept.status.success(), "{}", stderr(&kept));
    let root = stdout(&kept)
        .lines()
        .find_map(|line| line.strip_prefix("kept run root:"))
        .map(str::trim)
        .expect("kept run prints its root")
        .to_owned();

    let second = write_scenario(
        &fixture,
        &[
            r#"{"provider":{"script":[{"kind":"events","events":[{"type":"text_delta","text":"second"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}]}}"#,
            r#"{"session":"continue"}"#,
            r#"{"prompt":"two"}"#,
            r#"{"expect":{"update":"turn_ended"}}"#,
            r#"{"expect":{"journal":"TurnEnd"}}"#,
        ],
    )?;
    let resumed = fixture.output(&[
        "dev",
        "run",
        "--root",
        &root,
        second.to_str().expect("utf8 path"),
    ])?;
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let text = stdout(&resumed);
    assert!(text.contains("scenario passed"), "{text}");
    // An adopted root is never deleted.
    assert!(Path::new(&root).exists(), "adopted root {root} was removed");
    Ok(())
}

/// `dalgon dev` inspects journals without the product's config or
/// workspace resolution: a corrupt `dal.toml` must not block surgery.
/// Reverting the early dispatch fails the subcommand on the config
/// error before the journal is read.
#[test]
fn dev_journal_replay_survives_broken_user_config() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let session = fixture.data.join("session-broken-config");
    fs::create_dir_all(&session)?;
    let journal = write_journal(&session, &sample_records(&fixture.data, None)?)?;
    fixture.write_config("provider = ")?;
    let output = fixture.output(&[
        "dev",
        "journal",
        "replay",
        journal.to_str().expect("utf8 path"),
    ])?;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("records: 5"),
        "{}",
        stdout(&output)
    );
    Ok(())
}

/// A sidecar NAME is one path component: `..`, separators, and absolute
/// spellings must not read outside the session directory. Reverting the
/// component check lets `<session> ../outside.txt` dump the sibling.
#[test]
fn dev_journal_sidecar_rejects_a_traversal_name() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let session = fixture.data.join("session-trav");
    fs::create_dir_all(&session)?;
    write_journal(&session, &sample_records(&fixture.data, None)?)?;
    fs::write(fixture.data.join("outside.txt"), "outside bytes")?;
    for name in ["..", "../outside.txt", "/outside.txt", "a/b"] {
        let output = fixture.output(&[
            "dev",
            "journal",
            "sidecar",
            session.to_str().expect("utf8 path"),
            name,
        ])?;
        assert!(!output.status.success(), "{name} dumped");
    }
    Ok(())
}

/// `expect.file.equals` compares bytes, not a trimmed shape: a file
/// holding `exact` must not satisfy an expectation of `exact\n`.
/// Reverting to the trimmed comparison accepts the trailing newline.
#[test]
fn dev_run_expect_file_equals_is_byte_exact() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let scenario = write_scenario(
        &fixture,
        &[
            r#"{"write":{"path":"exact.txt","text":"exact"}}"#,
            r#"{"expect":{"file":{"path":"exact.txt","equals":"exact\n"}}}"#,
        ],
    )?;
    let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
    assert!(
        !output.status.success(),
        "the trailing newline was trimmed away"
    );
    Ok(())
}

/// `expect.request` must still answer a request an earlier update
/// expectation drained: the queue, not only fresh polls, holds it.
/// Reverting the queue check leaves the open request unmatched until
/// the timeout fails the scenario.
#[test]
fn dev_run_answers_a_request_drained_by_an_update_expectation() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let scenario = write_scenario(
        &fixture,
        &[
            r#"{"provider":{"script":[{"kind":"events","events":[{"type":"tool_calls_done","calls":[{"id":"call-1","name":"exec","args":{"kind":"parsed","value":{"command":"echo hi","timeout_seconds":60}}}]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]},{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}]}}"#,
            r#"{"set_approval":"ask"}"#,
            r#"{"prompt":"run echo hi"}"#,
            r#"{"expect":{"update":{"kind":"request_opened"}}}"#,
            r#"{"expect":{"request":{"kind":"approval","answer":"approve"}}}"#,
            r#"{"expect":{"update":{"kind":"turn_ended","contains":"end_turn"}}}"#,
        ],
    )?;
    let output = fixture.output(&[
        "dev",
        "run",
        "--consent",
        scenario.to_str().expect("utf8 path"),
    ])?;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("scenario passed"),
        "{}",
        stdout(&output)
    );
    Ok(())
}

/// A request an expectation answered leaves the open queue: a later
/// `answer` step finds nothing open instead of re-answering the settled
/// id. Reverting the removal reaches `agent.answer` and surfaces the
/// broker's rejection instead of `no open request`.
#[test]
fn dev_run_rejects_a_second_answer_to_a_settled_request() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let scenario = write_scenario(
        &fixture,
        &[
            r#"{"provider":{"script":[{"kind":"events","events":[{"type":"tool_calls_done","calls":[{"id":"call-1","name":"exec","args":{"kind":"parsed","value":{"command":"echo hi","timeout_seconds":60}}}]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]},{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}]}}"#,
            r#"{"set_approval":"ask"}"#,
            r#"{"prompt":"run echo hi"}"#,
            r#"{"expect":{"request":{"kind":"approval","answer":"approve"}}}"#,
            r#"{"answer":"approve"}"#,
        ],
    )?;
    let output = fixture.output(&[
        "dev",
        "run",
        "--consent",
        scenario.to_str().expect("utf8 path"),
    ])?;
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("no open request"), "{text}");
    Ok(())
}

/// A write through a symlinked directory inside an adopted workspace
/// must not escape: canonical containment resolves the link before the
/// lexical join. Reverting to the string check writes `leak.txt`
/// outside the workspace.
#[cfg(unix)]
#[test]
fn dev_run_rejects_a_write_through_a_workspace_symlink() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let root = fixture.data.join("adopted");
    let work = root.join("work");
    fs::create_dir_all(&work)?;
    fs::create_dir_all(root.join("data"))?;
    std::os::unix::fs::symlink(&fixture.home, work.join("escape"))?;
    let scenario = write_scenario(
        &fixture,
        &[r#"{"write":{"path":"escape/leak.txt","text":"leaked"}}"#],
    )?;
    let output = fixture.output(&[
        "dev",
        "run",
        "--root",
        root.to_str().expect("utf8 path"),
        scenario.to_str().expect("utf8 path"),
    ])?;
    assert!(!output.status.success(), "the symlink wrote outside");
    let text = stderr(&output);
    assert!(text.contains("escapes the workspace"), "{text}");
    assert!(!fixture.home.join("leak.txt").exists());
    Ok(())
}

/// A journal ending inside a turn declares `Running`, not `Idle`: the
/// attributed session shows the open turn the records imply.
#[test]
fn dev_fold_reports_a_trailing_open_turn_as_running() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let session = fixture.data.join("session-running");
    fs::create_dir_all(&session)?;
    let journal = write_journal(&session, &sample_records(&fixture.data, None)?)?;
    let output = fixture.output(&["dev", "fold", journal.to_str().expect("utf8 path")])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    let last = text.lines().next_back().unwrap_or_default();
    assert!(last.contains("TurnStart"), "{text}");
    // `~phase` marks a changed phase field: before the fix the journal
    // folded to Idle, so the declared Running phase never appeared.
    assert!(
        last.contains("~phase"),
        "expected a Running phase, got: {text}"
    );
    Ok(())
}
