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
fn every_shell_operator_turns_a_read_only_command_into_a_mutating_one() {
    // Each base is read-only on its own, so a rejection below can only come
    // from the appended operator.
    const BASES: &[&str] = &[
        "cat README.md",
        "ls -la",
        "git status",
        "rg foo",
        "pwd",
        "find . -name x",
        "cargo metadata",
        "env",
    ];
    const OPERATORS: &[&str] = &[
        "> /tmp/o",
        ">> /tmp/o",
        "2>/dev/null",
        "| sh",
        "; touch /tmp/o",
        "&& rm -rf /tmp/o",
        "|| touch /tmp/o",
        "`touch /tmp/o`",
        "$(touch /tmp/o)",
        "& sleep 0",
        "< /tmp/i",
    ];
    for base in BASES {
        assert!(exec_reads_only(base, None), "control: {base}");
        for operator in OPERATORS {
            for command in [format!("{base} {operator}"), format!("{base}{operator}")] {
                assert!(
                    !exec_reads_only(&command, None),
                    "operator after a read-only command classified read-only: {command}"
                );
            }
        }
    }
}

#[test]
fn writer_flags_on_read_only_words_are_never_read_only() {
    // `tree -o FILE` and `file -C` write files; short clusters, quoting and
    // option abbreviations must not hide them.
    for command in [
        "tree -o out.txt",
        "tree -ao out.txt",
        "tree -ofoo",
        "tree '-o' out.txt",
        "tree -a -o out.txt",
        "file -C -m magic",
        "file -bC -m magic",
        "file --compile -m magic",
        "file --comp -m magic",
        "file '--compile' -m magic",
    ] {
        assert!(
            !exec_reads_only(command, None),
            "file writer classified read-only: {command}"
        );
    }
    for command in [
        "tree",
        "tree -a -L 2",
        "tree -I target src",
        "file -b README.md",
        "file -i README.md",
        "file --mime-type README.md",
    ] {
        assert!(
            exec_reads_only(command, None),
            "read-only use rejected: {command}"
        );
    }
}

#[test]
fn quoted_shell_operators_are_rejected_like_bare_ones() {
    for command in [
        "cat 'a;b'",
        "cat \"a|b\"",
        "rg 'a>b'",
        "rg \"a<b\"",
        "ls 'a&b'",
        "cat '$(id)'",
        "cat '`id`'",
    ] {
        assert!(
            !exec_reads_only(command, None),
            "quoted operator classified read-only: {command}"
        );
    }
}

#[test]
fn classifier_does_not_consider_cwd() {
    // Classification is cwd-independent: pinning read-only only when a cwd
    // is present must turn this test red.
    let reads = ["git status", "ls -la", "rg foo", "cat README.md"];
    let mutations = ["rm file", "git push", "cargo build", "echo hi > out.txt"];
    let cwds = [None, Some("/outside/workspace"), Some("/"), Some("")];
    for command in reads {
        for cwd in cwds {
            assert!(exec_reads_only(command, cwd), "read {command:?} at {cwd:?}");
        }
    }
    for command in mutations {
        for cwd in cwds {
            assert!(
                !exec_reads_only(command, cwd),
                "mutating {command:?} at {cwd:?}"
            );
        }
    }
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
fn classifier_rejects_write_flags_on_plain_read_commands() {
    for command in [
        "tree -o /tmp/out",
        "tree --output=/tmp/out",
        "tree -Jo JSON /tmp/out",
        "tree '-Jo' /tmp/out",
        "tree \\-Jo /tmp/out",
        "file -C",
        "file --compile",
        "file -zC -m magic",
        "file '-zC' -m magic",
    ] {
        assert!(
            !exec_reads_only(command, None),
            "classified unsafe: {command}"
        );
    }
    for command in [
        "tree -L 2",
        "tree -a -I target",
        "file a.txt",
        "file -m magic a.txt",
        "file -b a.txt",
    ] {
        assert!(
            exec_reads_only(command, None),
            "classified read-only: {command}"
        );
    }
}

#[test]
fn classifier_bounds_git_remote_to_read_forms() {
    for command in [
        "git remote",
        "git remote -v",
        "git remote show",
        "git remote show origin",
        "git remote show 'origin'",
        "git remote get-url origin",
    ] {
        assert!(
            exec_reads_only(command, None),
            "classified read-only: {command}"
        );
    }
    for command in [
        "git remote add origin https://example.test/x",
        "git remote -v add origin https://example.test/x",
        "git remote -v -v",
        "git remote show origin extra",
        "git remote show '-n'",
        "git remote get-url '--all'",
        "git remote remove origin",
        "git remote rename a b",
    ] {
        assert!(
            !exec_reads_only(command, None),
            "classified unsafe: {command}"
        );
    }
}

#[test]
fn classifier_rejects_program_running_search_options() {
    for command in [
        "git grep -Oless pattern",
        "git grep -iOless pattern",
        "git grep -nOvim pattern",
        "git grep --open-files-in-pager=less pattern",
        "git grep --textconv pattern",
        "git log --ext-diff",
        "git log -pOless",
        "fd -Hx rm",
        "fd '-Hx' rm",
        "rg --hostname-bin fill pattern",
        "rg --hostname-bin=fill pattern",
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
