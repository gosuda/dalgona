use super::exec_reads_only;

#[test]
fn classifier_accepts_read_commands() {
    let commands = [
        "git status",
        "git log --oneline -5",
        "git diff",
        "git branch -a",
        "git stash list",
        "git config --get user.name",
        "ls -la",
        "rg foo",
        "find . -name '*.rs'",
        "cat README.md",
        "cargo metadata",
        "env",
        "pwd",
    ];
    for command in commands {
        assert!(
            exec_reads_only(command, None),
            "classified read-only: {command}"
        );
    }
}

#[test]
fn classifier_rejects_mutating_commands() {
    const BASE_COMMANDS: &[&str] = &[
        "rm -rf /tmp/x",
        "rm file",
        "mv a b",
        "mkdir d",
        "touch f",
        "git commit -m x",
        "git push",
        "git checkout main",
        "git clean -fd",
        "git reset --hard",
        "git apply p.diff",
        "git -c core.fsmonitor.command=x status",
        "cargo build",
        "find . -delete",
        "find . -exec rm {} \\;",
        "sed -i s/a/b/ f",
        "tee out.txt",
        "python -c 'import os'",
        "node -e 1",
        "bash -c x",
    ];
    const MODIFIERS: &[&str] = &[
        "",
        "> /tmp/o",
        ">> /tmp/o",
        "2>/dev/null",
        "| sh",
        "| tee /tmp/o",
        "; touch /tmp/o",
        "&& rm -rf /tmp/o",
        "|| touch /tmp/o",
        "`touch /tmp/o`",
        "$(touch /tmp/o) arg",
        "& sleep 0",
        "< /tmp/i",
        "FOO=1",
        "(cd /tmp; sh x)",
    ];

    let mut checked = 0;
    for base in BASE_COMMANDS {
        for modifier in MODIFIERS {
            let command = format!("{base} {modifier}");
            assert!(
                !exec_reads_only(&command, None),
                "mutating command classified read-only: {command}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 300);
}

#[test]
fn classifier_rejects_quoted_and_expanded_find_actions() {
    for command in [
        "find . '-delete'",
        "find . \"-exec\" rm {} \\;",
        "find . '-del''ete'",
        "find . \\-delete",
        "find . \"$ACTION\"",
    ] {
        assert!(
            !exec_reads_only(command, None),
            "classified unsafe: {command}"
        );
    }
}

#[test]
fn classifier_rejects_parse_doubt_and_multiline_shells() {
    for command in [
        "",
        " \t",
        "cat 'unterminated",
        "cat trailing\\",
        "cat\0README.md",
        "git status\nrm file",
        "git status\r\nrm file",
    ] {
        assert!(
            !exec_reads_only(command, None),
            "classified unsafe: {command:?}"
        );
    }
}

#[test]
fn classifier_does_not_consider_cwd() {
    assert!(exec_reads_only("git status", Some("/outside/workspace")));
}

#[test]
fn classifier_rejects_command_execution_and_output_options() {
    for command in [
        "find . -fprint0 /tmp/out",
        "find . *",
        "find . {-delete,-print}",
        "git log --output=/tmp/out",
        "git show --output /tmp/out",
        "git diff -o /tmp/out",
        "fd -x rm",
        "fd -X rm",
        "fd --exec rm",
        "fd --exec-batch rm",
        "fd '--exec=rm'",
        "rg --pre rm",
        "rg --pre=rm",
    ] {
        assert!(
            !exec_reads_only(command, None),
            "classified unsafe: {command}"
        );
    }
}

#[test]
fn classifier_accepts_literal_quoted_find_patterns() {
    for command in ["find . -name '$HOME/*.rs'", "find . -name \\*.rs"] {
        assert!(
            exec_reads_only(command, None),
            "classified read-only: {command}"
        );
    }
}
