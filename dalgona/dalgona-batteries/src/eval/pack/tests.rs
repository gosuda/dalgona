// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use super::{Check, CheckInput, Exit, LoadError, Scoring, ToolCall, Usage, load};

const SMOKE: &str = r##"def has_shebang(result, workspace):
    return result["exit"] == "ok" and workspace.read("scripts/run.sh").startswith("#!/bin/sh")

evalpack.pack(name = "edit-anchor-smoke", cases = [
    evalpack.case(
        id = "shebang",
        prompt = "Add a shebang line to scripts/run.sh so it runs as a shell script.",
        files = {"scripts/run.sh": "set -e\necho ok\n"},
        check = has_shebang,
        repeats = 3,
    ),
])
"##;

fn input<'a>(exit: Exit, text: &'a str, calls: &'a [ToolCall]) -> CheckInput<'a> {
    CheckInput {
        exit,
        text,
        tool_calls: calls,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 4,
            cost_usd: Some(0.5),
        },
    }
}

fn error_text(source: &str) -> String {
    load("t.eval.star", source)
        .err()
        .map_or_else(|| "loaded".to_owned(), |error| error.to_string())
}

fn only_check(source: &str) -> Check {
    let mut pack = load("t.eval.star", source).expect("pack loads");
    match pack.cases.remove(0).scoring {
        Scoring::Check(check) => check,
        Scoring::Judge(_) => panic!("expected a check"),
    }
}

#[test]
fn a_seed_file_pack_loads_one_case_with_its_seed_file() {
    let pack = load("edit-anchor-smoke.eval.star", SMOKE).expect("loads");
    assert_eq!(pack.name, "edit-anchor-smoke");
    assert_eq!(pack.cases.len(), 1);
    let case = &pack.cases[0];
    assert_eq!(case.id, "shebang");
    assert_eq!(case.repeats, 3);
    assert_eq!(
        case.files,
        vec![("scripts/run.sh".to_owned(), "set -e\necho ok\n".to_owned())]
    );
    assert!(case.tools.is_none());
    assert!(matches!(case.scoring, Scoring::Check(_)));
}

#[test]
fn two_pack_calls_and_none_name_the_count() {
    let one = "def c(r, w):\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c)])\n";
    assert_eq!(
        error_text(&format!("{one}{one}")),
        "expected exactly one evalpack.pack call, found 2."
    );
    assert_eq!(
        error_text("x = 1\n"),
        "expected exactly one evalpack.pack call, found 0."
    );
}

#[test]
fn an_unknown_builtin_names_its_line() {
    let text = error_text("x = 1\nevalpack.nope()\n");
    assert!(
        text.starts_with("starlark error at line 2: "),
        "unexpected text: {text}"
    );
}

#[test]
fn a_syntax_error_names_its_line() {
    let text = error_text("x = 1\ny = (\n");
    assert!(text.starts_with("starlark error at line "), "{text}");
}

#[test]
fn a_pack_cannot_reach_dal_ctx_or_any_service() {
    for name in ["dal", "ctx", "tools", "exec", "open"] {
        let text = error_text(&format!("{name}\n"));
        assert!(
            text.starts_with("starlark error at line 1: "),
            "{name} resolved: {text}"
        );
    }
}

#[test]
fn case_fields_are_validated_at_the_call() {
    let case = |fields: &str| {
        format!(
            "def c(r, w):\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c{fields})])\n"
        )
    };
    assert_eq!(
        error_text(&case(", files = {\"/etc/passwd\": \"x\"}")),
        "case \"a\": files paths must be relative."
    );
    assert_eq!(
        error_text(&case(", files = {\"../up\": \"x\"}")),
        "case \"a\": files paths must be relative."
    );
    assert_eq!(
        error_text(&case(", files = {\"a/../../up\": \"x\"}")),
        "case \"a\": files paths must be relative."
    );
    assert_eq!(
        error_text(&case(", repeats = 0")),
        "case \"a\": repeats must be an integer from 1 to 10."
    );
    assert_eq!(
        error_text(&case(", repeats = 11")),
        "case \"a\": repeats must be an integer from 1 to 10."
    );
    assert_eq!(
        error_text(&case(", tools = [1]")),
        "case \"a\": tools must be a list of tool names."
    );
    assert_eq!(
        error_text(&case(", files = {\"f\": \"x\" * 65537}")),
        "case \"a\": file \"f\" is larger than 65536 bytes."
    );
}

#[test]
fn prompt_length_bounds_are_characters_not_bytes() {
    let case = |prompt: &str| {
        format!(
            "def c(r, w):\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = {prompt}, check = c)])\n"
        )
    };
    assert_eq!(
        error_text(&case("\"\"")),
        "case \"a\": prompt must be 1 to 8000 characters."
    );
    assert_eq!(error_text(&case("\"\u{ac00}\" * 8000")), "loaded");
    assert_eq!(
        error_text(&case("\"\u{ac00}\" * 8001")),
        "case \"a\": prompt must be 1 to 8000 characters."
    );
}

#[test]
fn more_than_sixty_four_files_are_refused() {
    let source = "def c(r, w):\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c, files = {str(i): \"x\" for i in range(65)})])\n";
    assert_eq!(
        error_text(source),
        "case \"a\": files has more than 64 entries."
    );
}

#[test]
fn a_duplicate_case_id_is_a_load_error() {
    let source = "def c(r, w):\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c), evalpack.case(id = \"a\", prompt = \"y\", check = c)])\n";
    assert_eq!(error_text(source), "case \"a\": duplicate id.");
}

#[test]
fn an_empty_id_and_a_non_function_check_are_refused() {
    assert!(
        error_text(
            "evalpack.pack(name = \"p\", cases = [evalpack.case(id = \"\", prompt = \"x\", check = 1)])\n"
        )
        .contains("id must be a non-empty string")
    );
    assert_eq!(
        error_text(
            "evalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = 1)])\n"
        ),
        "case \"a\": check must be a function of (result, workspace) or evalpack.judge(question)."
    );
}

#[test]
fn a_judge_case_carries_its_one_line_question() {
    let source = "evalpack.pack(name = \"p\", cases = [evalpack.case(id = \"j\", prompt = \"x\", check = evalpack.judge(\"Did the edit keep the code compiling?\"))])\n";
    let pack = load("p.eval.star", source).expect("loads");
    match &pack.cases[0].scoring {
        Scoring::Judge(question) => assert_eq!(question, "Did the edit keep the code compiling?"),
        Scoring::Check(_) => panic!("expected a judge"),
    }
    let multi = "evalpack.pack(name = \"p\", cases = [evalpack.case(id = \"j\", prompt = \"x\", check = evalpack.judge(\"a\\nb\"))])\n";
    assert!(error_text(multi).starts_with("case \"\": invalid judge question"));
}

#[test]
fn a_check_passes_and_fails_on_the_workspace() {
    let pack = load("edit-anchor-smoke.eval.star", SMOKE).expect("loads");
    let Scoring::Check(check) = &pack.cases[0].scoring else {
        panic!("expected a check")
    };
    let dir = std::env::temp_dir().join(format!("wf-eval-pack-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("scripts/run.sh"), "#!/bin/sh\nset -e\n").unwrap();
    assert!(check.run(&input(Exit::Ok, "done", &[]), &dir).unwrap());
    assert!(!check.run(&input(Exit::Failed, "done", &[]), &dir).unwrap());
    std::fs::write(dir.join("scripts/run.sh"), "set -e\n").unwrap();
    assert!(!check.run(&input(Exit::Ok, "done", &[]), &dir).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_check_sees_text_tool_calls_and_usage() {
    let check = only_check(
        "def c(r, w):\n    return r[\"text\"] == \"hi\" and r[\"tool_calls\"][0][\"name\"] == \"patch\" and r[\"tool_calls\"][0][\"args\"][\"path\"] == \"a.txt\" and r[\"usage\"][\"input_tokens\"] == 10 and r[\"usage\"][\"output_tokens\"] == 4 and r[\"usage\"][\"cost_usd\"] == 0.5\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c)])\n",
    );
    let calls = [ToolCall {
        name: "patch".to_owned(),
        args: "{\"path\":\"a.txt\"}".to_owned(),
    }];
    let dir = std::env::temp_dir();
    assert!(check.run(&input(Exit::Ok, "hi", &calls), &dir).unwrap());
    assert!(!check.run(&input(Exit::Ok, "no", &calls), &dir).unwrap());
}

#[test]
fn a_check_that_raises_names_its_line() {
    let check = only_check(
        "def c(r, w):\n    x = 1\n    fail(\"boom\")\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c)])\n",
    );
    let error = check
        .run(&input(Exit::Ok, "", &[]), &std::env::temp_dir())
        .unwrap_err();
    assert_eq!(error.line, 3);
    assert!(error.message.contains("boom"), "{}", error.message);
}

#[test]
fn reading_a_file_the_run_never_wrote_raises() {
    let check = only_check(
        "def c(r, w):\n    return w.read(\"missing.txt\") == \"\"\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c)])\n",
    );
    let dir = std::env::temp_dir().join(format!("wf-eval-missing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let error = check.run(&input(Exit::Ok, "", &[]), &dir).unwrap_err();
    assert_eq!(error.line, 2);
    assert!(
        error.message.contains("no such file: missing.txt"),
        "{}",
        error.message
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn workspace_read_refuses_paths_that_leave_the_workspace() {
    let check = only_check(
        "def c(r, w):\n    return w.read(r[\"text\"]) == \"\"\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c)])\n",
    );
    let dir = std::env::temp_dir().join(format!("wf-eval-escape-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for path in ["../secret", "/etc/hostname"] {
        let error = check.run(&input(Exit::Ok, path, &[]), &dir).unwrap_err();
        assert!(
            error
                .message
                .contains("must be a relative path inside the workspace"),
            "{path}: {}",
            error.message
        );
    }
    #[cfg(unix)]
    {
        let outside =
            std::env::temp_dir().join(format!("wf-eval-outside-{}.txt", std::process::id()));
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        let error = check.run(&input(Exit::Ok, "link", &[]), &dir).unwrap_err();
        assert!(
            error.message.contains("resolves outside the workspace"),
            "{}",
            error.message
        );
        std::fs::remove_file(&outside).unwrap();
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_runaway_check_stops_at_its_tick_budget() {
    let check = only_check(
        "def c(r, w):\n    for i in range(100000000):\n        pass\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = c)])\n",
    );
    let error = check
        .run(&input(Exit::Ok, "", &[]), &std::env::temp_dir())
        .unwrap_err();
    assert_ne!(error.message, "");
}

#[test]
fn load_leaves_no_state_behind_for_the_next_load() {
    let _ = error_text("evalpack.pack(name = \"p\", cases = [])\nevalpack.nope()\n");
    assert_eq!(
        error_text("x = 1\n"),
        "expected exactly one evalpack.pack call, found 0."
    );
    assert!(load("p.eval.star", "evalpack.pack(name = \"p\", cases = [])\n").is_ok());
    let _ = LoadError::PackCalls(0);
}
