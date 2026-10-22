// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! `dalgona eval` through the real binary over a scripted provider.
//!
//! Each gate writes a pack and a `dal.toml` into a temporary home, runs the
//! `dalgona` binary, and reads back what a person would: the exit code, the
//! text on stdout and stderr, and the `report.json` and `runs.jsonl` files.
#![expect(
    clippy::disallowed_methods,
    reason = "the gate runs the real dalgona binary"
)]
#![expect(
    clippy::expect_used,
    reason = "a gate aborts on a setup failure and names it"
)]

use std::fs;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Child;
#[cfg(unix)]
use std::process::Stdio;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::time::{Duration, Instant};

const MODEL: &str = "openai-responses/gpt-6";
const USAGE: &str = r#"{"input_tokens":12,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}"#;

const SMOKE_PACK: &str = include_str!("fixtures/edit-anchor-smoke.eval.star");

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

fn text_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":{text:?}}},{{"type":"tool_calls_done","calls":[]}},{{"type":"usage","usage":{USAGE}}},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

fn tool_step(id: &str, name: &str, args_json: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":{id:?},"name":{name:?}}},{{"type":"tool_calls_done","calls":[{{"id":{id:?},"name":{name:?},"args":{{"kind":"parsed","value":{args_json}}}}}]}},{{"type":"usage","usage":{USAGE}}},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

/// The judge's reply to one bool question.
fn judge_step(answer: bool) -> String {
    text_step(&format!(r#"{{"answers":[{answer}]}}"#))
}

/// A temporary home with a `dal.toml`, a scripted provider, and a pack root.
///
/// The judged battery is off: its own judge calls would consume scripted
/// replies that these gates give to the run and to the eval judge.
struct Rig {
    root: PathBuf,
}

impl Rig {
    fn new(steps: &[String], extra_toml: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "dalgona-eval-gate-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let rig = Self { root };
        fs::create_dir_all(rig.data().join("evals")).expect("data dir");
        fs::create_dir_all(rig.root.join("xdg-config/dalgona")).expect("config dir");
        let fixture = rig.root.join("scripted.jsonl");
        fs::write(&fixture, steps.join("\n") + "\n").expect("fixture");
        fs::write(
            rig.root.join("xdg-config/dalgona/dal.toml"),
            format!(
                "model = \"{MODEL}\"\ndisabled_batteries = [\"judged\"]\n{extra_toml}\n[providers.scripted]\nfixture = {:?}\n",
                fixture.to_string_lossy()
            ),
        )
        .expect("dal.toml");
        rig
    }

    fn data(&self) -> PathBuf {
        self.root.join("xdg-data/dalgona")
    }

    fn pack(&self, name: &str, source: &str) {
        fs::write(
            self.data().join("evals").join(format!("{name}.eval.star")),
            source,
        )
        .expect("pack file");
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dalgona"));
        command
            .arg("eval")
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", self.root.join("xdg-config"))
            .env("XDG_DATA_HOME", self.root.join("xdg-data"));
        command
    }

    fn run(&self, args: &[&str]) -> Run {
        let output = self.command(args).output().expect("dalgona eval runs");
        Run::from(output)
    }

    /// The one run directory of `pack`.
    fn run_dir(&self, pack: &str) -> PathBuf {
        let runs = self.data().join("evals/runs").join(pack);
        let mut dirs: Vec<_> = fs::read_dir(&runs)
            .expect("the run directory exists")
            .map(|entry| entry.expect("entry").path())
            .collect();
        assert_eq!(dirs.len(), 1, "one run directory: {dirs:?}");
        dirs.remove(0)
    }

    fn report(&self, pack: &str) -> String {
        fs::read_to_string(self.run_dir(pack).join("report.json")).expect("report.json")
    }

    fn lines(&self, pack: &str) -> Vec<String> {
        fs::read_to_string(self.run_dir(pack).join("runs.jsonl"))
            .expect("runs.jsonl")
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

fn contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "missing {needle:?} in:\n{haystack}"
    );
}

#[test]
fn an_exec_call_is_refused_and_the_case_reports_failure() {
    let write = tool_step(
        "write-1",
        "exec",
        r#"{"command":"printf '#!/bin/sh\\nset -e\\necho ok\\n' > scripts/run.sh"}"#,
    );
    let rig = Rig::new(&[write, text_step("done"), text_step("done")], "");
    rig.pack("edit-anchor-smoke", SMOKE_PACK);
    let run = rig.run(&["edit-anchor-smoke", "--repeats", "2"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);

    // The run has no controller to approve the exec call, so it is refused
    // and `scripts/run.sh` is never written: both repeats fail the check.
    let report = rig.report("edit-anchor-smoke");
    contains(
        &report,
        r#""cases":[{"id":"shebang","repeats":2,"passed":0,"failed":2,"skipped":0,"pass_rate":0.0,"#,
    );
}

#[test]
fn a_patch_to_the_case_workspace_proceeds_without_a_controller() {
    let create = tool_step(
        "create-1",
        "patch",
        r#"{"input":"[scripts/new.sh#NEW]\nPUT >$:\n+#!/bin/sh\n+echo ok"}"#,
    );
    let rig = Rig::new(&[create, text_step("done")], "");
    rig.pack(
        "edit-anchor-smoke",
        r##"def created(result, workspace):
    return workspace.read("scripts/new.sh").startswith("#!/bin/sh")

evalpack.pack(name = "edit-anchor-smoke", cases = [
    evalpack.case(id = "create", prompt = "p", files = {"scripts/run.sh": "echo ok\n"}, check = created),
])
"##,
    );
    let run = rig.run(&["edit-anchor-smoke", "--repeats", "1"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);

    // Edits to the run's own workspace need no controller, so the patch
    // lands and the check sees the new file.
    let report = rig.report("edit-anchor-smoke");
    contains(
        &report,
        r#""cases":[{"id":"create","repeats":1,"passed":1,"failed":0,"skipped":0,"pass_rate":1.0,"#,
    );
}

#[test]
fn a_pack_reports_one_pass_and_one_fail_with_the_report_shape() {
    let rig = Rig::new(&[text_step("pass"), text_step("fail")], "");
    rig.pack(
        "edit-anchor-smoke",
        r#"def ok(result, workspace):
    return result["text"] == "pass"

evalpack.pack(name = "edit-anchor-smoke", cases = [
    evalpack.case(id = "shebang", prompt = "p", check = ok),
])
"#,
    );
    let run = rig.run(&["edit-anchor-smoke", "--repeats", "2"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);

    let report = rig.report("edit-anchor-smoke");
    contains(
        &report,
        r#"{"v":1,"pack":"edit-anchor-smoke","model":"openai-responses/gpt-6","against":null,"started_at":""#,
    );
    contains(
        &report,
        r#""cases":[{"id":"shebang","repeats":2,"passed":1,"failed":1,"skipped":0,"pass_rate":0.5,"tokens_in_mean":12.0,"tokens_out_mean":5.0,"latency_ms_mean":"#,
    );
    contains(&report, r#""cost_usd_mean":null,"judge":null}]}"#);
    assert!(!report.contains("interrupted"), "{report}");

    let lines = rig.lines("edit-anchor-smoke");
    assert_eq!(lines.len(), 2, "one runs.jsonl line per run: {lines:?}");
    contains(
        &lines[0],
        r#""case":"shebang","arm":"defaults","repeat":1,"status":"passed","reason":null,"tokens_in":12,"tokens_out":5"#,
    );
    contains(
        &lines[1],
        r#""repeat":2,"status":"failed","reason":"the check returned false","tokens_in":12,"tokens_out":5"#,
    );

    contains(
        &run.stdout,
        "Eval report: edit-anchor-smoke (model openai-responses/gpt-6, repeats 2, against none)",
    );
    contains(&run.stdout, "shebang  pass 1/2");
    contains(&run.stdout, "totals: 1 passed, 1 failed, 0 skipped");
    contains(&run.stderr, "shebang run 2");
}

#[test]
fn a_check_that_raises_fails_its_case_and_the_run_goes_on() {
    let rig = Rig::new(&[text_step("one"), text_step("two")], "");
    rig.pack(
        "isolation",
        r#"def boom(result, workspace):
    return workspace.read("absent.txt") == "x"

def fine(result, workspace):
    return result["exit"] == "ok" and result["text"] == "two"

evalpack.pack(name = "isolation", cases = [
    evalpack.case(id = "first", prompt = "p1", check = boom, repeats = 1),
    evalpack.case(id = "second", prompt = "p2", check = fine, repeats = 1),
])
"#,
    );
    let run = rig.run(&["isolation"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines = rig.lines("isolation");
    assert_eq!(lines.len(), 2, "{lines:?}");
    contains(&lines[0], r#""case":"first""#);
    contains(&lines[0], r#""status":"failed""#);
    contains(
        &lines[0],
        r#"isolation: case \"first\": starlark error at line"#,
    );
    contains(
        &lines[1],
        r#""case":"second","arm":"defaults","repeat":1,"status":"passed""#,
    );
    contains(&run.stdout, "totals: 1 passed, 1 failed, 0 skipped");
}

const JUDGE_PACK: &str = r#"def ok(result, workspace):
    return result["exit"] == "ok"

evalpack.pack(name = "judged", cases = [
    evalpack.case(id = "plain", prompt = "p1", check = ok, repeats = 1),
    evalpack.case(id = "scored", prompt = "p2", check = evalpack.judge("Did it answer?"), repeats = 2),
])
"#;

#[test]
fn a_judge_case_is_skipped_never_failed_while_the_gate_is_off() {
    // One scripted reply exists. A session started for the judge case would
    // find the script empty and fail, so a skip cannot be a hidden failure.
    let rig = Rig::new(&[text_step("plain")], "[judge]\ngate = \"off\"\n");
    rig.pack("judged", JUDGE_PACK);
    let run = rig.run(&["judged"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let report = rig.report("judged");
    contains(
        &report,
        r#"{"id":"scored","repeats":2,"passed":0,"failed":0,"skipped":2,"#,
    );
    contains(&report, r#""judge":{"gate":"off","verdicts":[]}"#);
    let lines = rig.lines("judged");
    assert_eq!(lines.len(), 3, "{lines:?}");
    contains(
        &lines[1],
        r#""status":"skipped","reason":"skipped: judge gate is off""#,
    );
    contains(&run.stdout, "totals: 1 passed, 0 failed, 2 skipped");
}

#[test]
fn a_judge_case_is_scored_by_the_verdict_when_the_gate_is_on() {
    let steps = [
        text_step("plain"),
        text_step("an answer"),
        judge_step(true),
        text_step("another answer"),
        judge_step(false),
    ];
    let rig = Rig::new(&steps, "[judge]\ngate = \"on\"\n");
    rig.pack("judged", JUDGE_PACK);
    let run = rig.run(&["judged"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let report = rig.report("judged");
    contains(
        &report,
        r#"{"id":"scored","repeats":2,"passed":1,"failed":1,"skipped":0,"pass_rate":0.5,"#,
    );
    contains(&report, r#""judge":{"gate":"on","verdicts":[true,false]}"#);
    let lines = rig.lines("judged");
    contains(
        &lines[2],
        r#""status":"failed","reason":"the judge answered no""#,
    );
}

#[test]
fn against_runs_every_case_under_both_arms_and_a_bad_file_fails_only_its_arm() {
    let rig = Rig::new(&[text_step("hello")], "");
    rig.pack(
        "pair",
        r#"def ok(result, workspace):
    return result["text"] == "hello"

evalpack.pack(name = "pair", cases = [evalpack.case(id = "c", prompt = "p", check = ok, repeats = 1)])
"#,
    );
    let layered = rig.root.join("against.toml");
    fs::write(&layered, "edit_style = \"replace\"\n").expect("against file");
    let run = rig.run(&["pair", "--against", &layered.to_string_lossy()]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines = rig.lines("pair");
    assert_eq!(lines.len(), 2, "{lines:?}");
    contains(
        &lines[0],
        r#""arm":"defaults","repeat":1,"status":"passed""#,
    );
    contains(&lines[1], r#""arm":"against","repeat":1,"status":"passed""#);
    let report = rig.report("pair");
    contains(&report, r#""arm":"defaults""#);
    contains(&report, r#""arm":"against""#);
    contains(&run.stdout, "against ");
}

#[test]
fn an_against_file_reaches_the_against_arm_and_never_the_defaults_arm() {
    let rig = Rig::new(&[text_step("hello")], "");
    let other = rig.root.join("other.jsonl");
    fs::write(&other, text_step("goodbye") + "\n").expect("other script");
    rig.pack(
        "layer",
        r#"def ok(result, workspace):
    return result["text"] == "hello"

evalpack.pack(name = "layer", cases = [evalpack.case(id = "c", prompt = "p", check = ok, repeats = 1)])
"#,
    );
    let layered = rig.root.join("against.toml");
    fs::write(
        &layered,
        format!(
            "[providers.scripted]\nfixture = {:?}\n",
            other.to_string_lossy()
        ),
    )
    .expect("against file");
    let run = rig.run(&["layer", "--against", &layered.to_string_lossy()]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines = rig.lines("layer");
    assert_eq!(lines.len(), 2, "{lines:?}");
    contains(
        &lines[0],
        r#""arm":"defaults","repeat":1,"status":"passed""#,
    );
    contains(&lines[1], r#""arm":"against","repeat":1,"status":"failed""#);
}

#[test]
fn a_missing_against_file_fails_the_against_arm_and_not_the_report() {
    let rig = Rig::new(&[text_step("hello")], "");
    rig.pack(
        "pair",
        r#"def ok(result, workspace):
    return result["text"] == "hello"

evalpack.pack(name = "pair", cases = [evalpack.case(id = "c", prompt = "p", check = ok, repeats = 1)])
"#,
    );
    let run = rig.run(&["pair", "--against", "/definitely/not/here.toml"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines = rig.lines("pair");
    assert_eq!(lines.len(), 2, "{lines:?}");
    contains(
        &lines[0],
        r#""arm":"defaults","repeat":1,"status":"passed""#,
    );
    contains(&lines[1], r#""arm":"against","repeat":1,"status":"failed""#);
    contains(&lines[1], "/definitely/not/here.toml");
}

#[test]
fn a_pack_that_does_not_load_exits_one_with_its_exact_line() {
    let rig = Rig::new(&[], "");
    rig.pack("twice", "p = evalpack.pack(name = \"twice\", cases = [])\nq = evalpack.pack(name = \"twice\", cases = [])\n");
    let run = rig.run(&["twice"]);
    assert_eq!(run.code, Some(1));
    assert_eq!(
        run.stderr.trim_end(),
        "twice: expected exactly one evalpack.pack call, found 2."
    );
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    assert!(!rig.data().join("evals/runs").exists());
}

#[test]
fn usage_errors_exit_one_before_anything_runs() {
    let rig = Rig::new(&[text_step("x")], "");
    rig.pack("edit-anchor-smoke", SMOKE_PACK);
    for args in [
        vec![],
        vec!["--bogus"],
        vec!["edit-anchor-smoke", "--repeats", "11"],
    ] {
        let run = rig.run(&args);
        assert_eq!(run.code, Some(1), "{args:?}: {}", run.stderr);
        assert!(run.stdout.is_empty(), "{args:?}: {}", run.stdout);
    }
    assert!(!rig.data().join("evals/runs").exists());
    let unknown = rig.run(&["no-such-pack"]);
    assert_eq!(unknown.code, Some(1));
    contains(&unknown.stderr, "no-such-pack: ");
}

#[test]
fn no_model_anywhere_is_a_usage_error() {
    let rig = Rig::new(&[], "");
    fs::write(rig.root.join("xdg-config/dalgona/dal.toml"), "").expect("empty dal.toml");
    rig.pack("edit-anchor-smoke", SMOKE_PACK);
    let run = rig.run(&["edit-anchor-smoke"]);
    assert_eq!(run.code, Some(1));
    contains(&run.stderr, "no model: pass --model <id>");
}

/// Kills the child when the gate fails before it reaps it.
#[cfg(unix)]
struct Reap(Child);

#[cfg(unix)]
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
#[test]
fn sigint_writes_the_partial_report_marks_it_and_exits_130() {
    // Case two's turn loops on refused exec calls: each denial consumes one
    // scripted step, so the turn stays open long enough to signal inside it.
    let mut steps = vec![text_step("first")];
    for index in 0..400 {
        steps.push(tool_step(
            &format!("hang-{index}"),
            "exec",
            r#"{"command":"sleep 600","foreground_s":86400}"#,
        ));
    }
    steps.push(text_step("never"));
    let rig = Rig::new(&steps, "");
    rig.pack(
        "three",
        r#"def ok(result, workspace):
    return result["exit"] == "ok"

evalpack.pack(name = "three", cases = [
    evalpack.case(id = "one", prompt = "p", check = ok, repeats = 1),
    evalpack.case(id = "two", prompt = "p", check = ok, repeats = 1),
    evalpack.case(id = "three", prompt = "p", check = ok, repeats = 1),
])
"#,
    );
    let mut child = Reap(
        rig.command(&["three"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn dalgona eval"),
    );
    wait_for("the second case to start", || {
        let Ok(entries) = fs::read_dir(rig.data().join("evals/runs/three")) else {
            return false;
        };
        entries.flatten().any(|entry| {
            fs::read_to_string(entry.path().join("runs.jsonl"))
                .is_ok_and(|text| text.contains(r#""case":"one""#))
        })
    });
    let signalled = Command::new("kill")
        .args(["-INT", &child.0.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(signalled.success());
    let finished = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = child.0.try_wait().expect("try_wait") {
                break status;
            }
            assert!(Instant::now() < deadline, "dalgona eval ignored SIGINT");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    assert_eq!(finished.code(), Some(130));

    let report = rig.report("three");
    contains(&report, r#""interrupted":true"#);
    contains(
        &report,
        r#"{"id":"one","repeats":1,"passed":1,"failed":0,"skipped":0,"#,
    );
    contains(
        &report,
        r#"{"id":"two","repeats":1,"passed":0,"failed":0,"skipped":1,"#,
    );
    contains(
        &report,
        r#"{"id":"three","repeats":1,"passed":0,"failed":0,"skipped":1,"#,
    );
    let lines = rig.lines("three");
    assert_eq!(lines.len(), 3, "every case has a line: {lines:?}");
    contains(
        &lines[1],
        r#""status":"skipped","reason":"skipped: interrupted""#,
    );
    contains(
        &lines[2],
        r#""status":"skipped","reason":"skipped: interrupted""#,
    );
}
