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
        bytes.extend_from_slice(&encode(record)?);
        bytes.push(b'\n');
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
    assert!(!replay.status.success(), "torn replay should fail");
    assert!(stderr(&replay).contains("line"), "{}", stderr(&replay));
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
    let output = fixture.output(&["dev", "run", scenario.to_str().expect("utf8 path")])?;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("request approval"), "{text}");
    assert!(text.contains("scenario passed"), "{text}");
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
