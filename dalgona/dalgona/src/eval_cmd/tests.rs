// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::sync::atomic::{AtomicU64, Ordering};

use dalgona_batteries::eval::report::{ArmName, CasePlan, JudgeGate, JudgeLine, ReportInput};

use super::*;

fn args(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn run_args(items: &[&str]) -> Args {
    match parse(&args(items)) {
        Ok(Parsed::Run(args)) => args,
        other => panic!("expected a run, got {other:?}"),
    }
}

fn usage(items: &[&str]) -> String {
    match parse(&args(items)) {
        Err(error) => error.to_string(),
        Ok(parsed) => panic!("expected a usage error, got {parsed:?}"),
    }
}

#[test]
fn every_flag_reads_in_both_spellings() {
    let spaced = run_args(&[
        "pack",
        "--case",
        "a",
        "--repeats",
        "2",
        "--model",
        "m/x",
        "--against",
        "b.toml",
    ]);
    let joined = run_args(&[
        "--against=b.toml",
        "--model=m/x",
        "--repeats=2",
        "--case=a",
        "pack",
    ]);
    for parsed in [spaced, joined] {
        assert_eq!(parsed.pack, "pack");
        assert_eq!(parsed.case.as_deref(), Some("a"));
        assert_eq!(parsed.repeats, Some(2));
        assert_eq!(parsed.model.as_deref(), Some("m/x"));
        assert_eq!(parsed.against, Some(PathBuf::from("b.toml")));
    }
}

#[test]
fn a_value_may_hold_an_equals_sign() {
    let parsed = run_args(&["pack", "--model=a=b"]);
    assert_eq!(parsed.model.as_deref(), Some("a=b"));
}

#[test]
fn help_wins_wherever_it_stands() {
    assert!(matches!(parse(&args(&["pack", "-h"])), Ok(Parsed::Help)));
    assert!(matches!(parse(&args(&["--help"])), Ok(Parsed::Help)));
}

#[test]
fn a_bad_command_line_names_its_fault() {
    assert_eq!(usage(&[]), "dalgona eval: the pack name is missing");
    assert_eq!(
        usage(&["--repeats=2"]),
        "dalgona eval: the pack name is missing"
    );
    assert_eq!(
        usage(&["pack", "--case"]),
        "dalgona eval: --case needs a value"
    );
    assert_eq!(
        usage(&["pack", "--against"]),
        "dalgona eval: --against needs a value"
    );
    assert_eq!(
        usage(&["pack", "--bogus"]),
        "dalgona eval: unknown option --bogus"
    );
    assert_eq!(
        usage(&["pack", "other"]),
        "dalgona eval: unexpected argument other; one pack at a time"
    );
}

#[test]
fn repeats_must_be_a_non_negative_integer() {
    for bad in ["x", "-1", "1.5", "", "99999999999"] {
        assert_eq!(
            usage(&["pack", &format!("--repeats={bad}")]),
            "dalgona eval: --repeats must be an integer from 1 to 10",
            "--repeats={bad}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_non_utf8_argument_is_refused_not_mangled() {
    use std::os::unix::ffi::OsStringExt;
    let bad = OsString::from_vec(vec![b'p', 0xff]);
    let error = parse(&[bad]).expect_err("not UTF-8");
    assert!(error.to_string().contains("is not valid UTF-8"), "{error}");
}

fn vars(pairs: &[(&str, &str)]) -> Vars {
    pairs
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect()
}

/// Absolute on the platform under test, so the same cases run on POSIX and
/// Windows.
#[cfg(windows)]
const HOME: &str = "C:\\home\\u";
#[cfg(not(windows))]
const HOME: &str = "/home/u";
#[cfg(windows)]
const XDG_CONFIG: &str = "C:\\c";
#[cfg(not(windows))]
const XDG_CONFIG: &str = "/c";
#[cfg(windows)]
const XDG_DATA: &str = "C:\\d";
#[cfg(not(windows))]
const XDG_DATA: &str = "/d";

fn home() -> PathBuf {
    PathBuf::from(HOME)
}

#[test]
fn roots_follow_the_xdg_variables_then_home() {
    let from_home = roots(&vars(&[("HOME", HOME)])).expect("home resolves");
    assert_eq!(
        from_home.config_file,
        home().join(".config").join("dalgona").join("dal.toml")
    );
    assert_eq!(
        from_home.data,
        home().join(".local").join("share").join("dalgona")
    );

    let overridden = roots(&vars(&[
        ("HOME", HOME),
        ("XDG_CONFIG_HOME", XDG_CONFIG),
        ("XDG_DATA_HOME", XDG_DATA),
    ]))
    .expect("xdg resolves");
    assert_eq!(
        overridden.config_file,
        Path::new(XDG_CONFIG).join("dalgona").join("dal.toml")
    );
    assert_eq!(overridden.data, Path::new(XDG_DATA).join("dalgona"));
}

#[test]
fn a_relative_or_empty_root_is_not_a_root() {
    let refused: [&[(&str, &str)]; 3] = [&[("HOME", "relative/home")], &[("HOME", "")], &[]];
    for env in refused {
        assert!(
            matches!(roots(&vars(env)), Err(dalgon::EdgeError::HomeMissing)),
            "{env:?}"
        );
    }
    let fallback = roots(&vars(&[
        ("HOME", HOME),
        ("XDG_DATA_HOME", "relative"),
        ("XDG_CONFIG_HOME", ""),
    ]))
    .expect("falls back to home");
    assert_eq!(
        fallback.data,
        home().join(".local").join("share").join("dalgona")
    );
    assert_eq!(
        fallback.config_file,
        home().join(".config").join("dalgona").join("dal.toml")
    );
}

/// Windows takes an absolute `HOME` first, then `USERPROFILE`, then the
/// `HOMEDRIVE`+`HOMEPATH` pair.
#[cfg(windows)]
#[test]
fn windows_home_falls_back_to_userprofile_then_homedrive_pair() {
    let profile = roots(&vars(&[("USERPROFILE", "C:\\Users\\bob")])).expect("profile");
    assert_eq!(
        profile.data,
        Path::new("C:\\Users\\bob\\.local\\share\\dalgona")
    );

    let pair = roots(&vars(&[
        ("HOMEDRIVE", "C:"),
        ("HOMEPATH", "\\Users\\carol"),
    ]))
    .expect("drive pair");
    assert_eq!(
        pair.config_file,
        Path::new("C:\\Users\\carol\\.config\\dalgona\\dal.toml")
    );
    assert_eq!(
        pair.data,
        Path::new("C:\\Users\\carol\\.local\\share\\dalgona")
    );

    let home_first = roots(&vars(&[
        ("HOME", "C:\\Users\\alice"),
        ("USERPROFILE", "C:\\Users\\bob"),
        ("HOMEDRIVE", "C:"),
        ("HOMEPATH", "\\Users\\carol"),
    ]))
    .expect("home wins");
    assert_eq!(
        home_first.data,
        Path::new("C:\\Users\\alice\\.local\\share\\dalgona")
    );

    let profile_first = roots(&vars(&[
        ("HOME", "relative"),
        ("USERPROFILE", "C:\\Users\\bob"),
        ("HOMEDRIVE", "C:"),
        ("HOMEPATH", "\\Users\\carol"),
    ]))
    .expect("profile wins over the pair");
    assert_eq!(
        profile_first.data,
        Path::new("C:\\Users\\bob\\.local\\share\\dalgona")
    );
}

/// POSIX reads only `HOME`; the Windows fallbacks name no home here.
#[cfg(not(windows))]
#[test]
fn posix_home_ignores_the_windows_fallbacks() {
    let env = vars(&[
        ("USERPROFILE", "/home/bob"),
        ("HOMEDRIVE", "/"),
        ("HOMEPATH", "home/carol"),
    ]);
    let error = roots(&env).err().expect("no HOME");
    assert!(matches!(error, dalgon::EdgeError::HomeMissing));
    assert_eq!(
        root_failure(&error),
        "cannot find your home directory: set HOME to an absolute path"
    );
}

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "dalgona-eval-cmd-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn line(case: &str, repeat: u32, status: Status) -> RunLine {
    RunLine {
        v: 1,
        pack: "p".into(),
        case: case.into(),
        arm: ArmName::Defaults,
        repeat,
        status,
        reason: None,
        tokens_in: Some(3),
        tokens_out: Some(2),
        latency_ms: Some(10),
        cost_usd: None,
        judge: Some(JudgeLine {
            gate: JudgeGate::On,
            verdict: Some(true),
        }),
    }
}

#[test]
fn each_run_line_is_one_json_line_in_the_run_directory() {
    let scratch = Scratch::new();
    let roots = Roots {
        config_file: scratch.0.join("dal.toml"),
        data: scratch.0.join("data"),
    };
    let mut files = RunDir::create(&roots, "p", "run-1").expect("run dir");
    files.append(&line("a", 1, Status::Passed)).expect("first");
    files.append(&line("a", 2, Status::Failed)).expect("second");
    let text = std::fs::read_to_string(
        roots
            .data
            .join("evals")
            .join("runs")
            .join("p")
            .join("run-1")
            .join("runs.jsonl"),
    )
    .expect("runs.jsonl");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains(r#""status":"passed""#), "{}", lines[0]);
    assert!(lines[1].contains(r#""repeat":2"#), "{}", lines[1]);
    assert!(text.ends_with('\n'));
}

#[test]
fn the_report_lands_whole_and_leaves_no_staging_file() {
    let scratch = Scratch::new();
    let roots = Roots {
        config_file: scratch.0.join("dal.toml"),
        data: scratch.0.join("data"),
    };
    let files = RunDir::create(&roots, "p", "run-1").expect("run dir");
    let plan = [CasePlan {
        id: "a".into(),
        judged: false,
    }];
    let lines = [line("a", 1, Status::Passed)];
    let report = Report::build(&ReportInput {
        pack: "p",
        model: "m",
        against: None,
        started_at: "2026-10-13T00:00:00.000Z",
        plan: &plan,
        lines: &lines,
        interrupted: false,
    });
    files.write_report(&report).expect("write");
    let dir = roots
        .data
        .join("evals")
        .join("runs")
        .join("p")
        .join("run-1");
    let written = std::fs::read_to_string(dir.join("report.json")).expect("report.json");
    assert_eq!(written, report.to_json().expect("json"));
    assert!(!dir.join("report.json.tmp").exists());
}

#[test]
fn a_missing_config_file_is_no_file_and_a_directory_is_an_error() {
    let scratch = Scratch::new();
    assert!(matches!(
        read_optional(&scratch.0.join("absent.toml")),
        Ok(None)
    ));
    let error = read_optional(&scratch.0).expect_err("a directory is not a config file");
    assert!(error.to_string().starts_with("cannot read "), "{error}");
}
