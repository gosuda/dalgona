// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Worktree isolation policy tests: refusal texts, argv forms, retained body.

use super::*;

#[test]
fn isolation_refusal_before_job() {
    let cases = [
        (
            IsolationRefusal::NotARepository,
            "agents: step build needs a git worktree, but /repo is not inside a git repository. Set isolation = \"shared\" on step build to let it write the real checkout.",
        ),
        (
            IsolationRefusal::NoGit,
            "agents: step build needs a git worktree, but git was not found on PATH. Set isolation = \"shared\" on step build to let it write the real checkout.",
        ),
        (
            IsolationRefusal::TooOld {
                version: "2.16.0".to_owned(),
            },
            "agents: step build needs git 2.17 or newer for worktrees; found 2.16.0. Set isolation = \"shared\" on step build to let it write the real checkout.",
        ),
        (
            IsolationRefusal::NoHead,
            "agents: step build needs a git worktree, but the repository has no commit yet. Set isolation = \"shared\" on step build to let it write the real checkout.",
        ),
        (
            IsolationRefusal::Denied {
                reason: "grant revoked".to_owned(),
            },
            "agents: step build needs a git worktree, but running git was denied: grant revoked. Set isolation = \"shared\" on step build to let it write the real checkout.",
        ),
    ];
    for (refusal, expected) in cases {
        assert_eq!(refusal.text("build", "/repo"), expected);
    }
}

#[test]
fn isolation_git_argv_are_exact_forms() {
    assert_eq!(argv_version(), ["git", "--version"]);
    assert_eq!(
        argv_toplevel("/repo").join(" "),
        "git -C /repo rev-parse --show-toplevel"
    );
    assert_eq!(
        argv_verify_head("/repo").join(" "),
        "git -C /repo rev-parse --verify HEAD"
    );
    assert_eq!(
        argv_stash_create("/repo").join(" "),
        "git -C /repo stash create"
    );
    assert_eq!(
        argv_worktree_add("/repo", "/wt/1", "abc").join(" "),
        "git -C /repo worktree add --detach /wt/1 abc"
    );
    assert_eq!(argv_add_all("/wt/1").join(" "), "git -C /wt/1 add -A");
    assert_eq!(
        argv_cached_diff("/wt/1", "abc").join(" "),
        "git -C /wt/1 diff --cached --binary abc"
    );
    assert_eq!(
        argv_cached_names("/wt/1", "abc").join(" "),
        "git -C /wt/1 diff --cached --name-only abc"
    );
    assert_eq!(
        argv_apply_check("/repo", "/iso/delta.patch").join(" "),
        "git -C /repo apply --check /iso/delta.patch"
    );
    assert_eq!(
        argv_apply("/repo", "/iso/delta.patch").join(" "),
        "git -C /repo apply /iso/delta.patch"
    );
    assert_eq!(
        argv_worktree_remove("/repo", "/wt/1").join(" "),
        "git -C /repo worktree remove --force /wt/1"
    );
}

#[test]
fn isolation_retained_artifacts_match_plan_shapes() {
    let body = retained_body(
        "conflict",
        &"a".repeat(40),
        "/wt/1.retained-123",
        "2026-09-25T10:20:00.000Z",
    )
    .unwrap();
    assert_eq!(
        body,
        format!(
            "{{\"v\":1,\"reason\":\"conflict\",\"base\":\"{}\",\"worktree\":\"/wt/1.retained-123\",\"at\":\"2026-09-25T10:20:00.000Z\"}}\n",
            "a".repeat(40)
        )
    );
    let notice = retained_notice(
        "/wt/1.retained-123",
        "patch does not apply",
        "/repo",
        "/iso/delta.patch",
    );
    assert_eq!(
        notice,
        "isolation: retained at /wt/1.retained-123; the changes did not apply cleanly (patch does not apply). Apply them by hand: git -C /repo apply --3way /iso/delta.patch"
    );
}

#[test]
fn isolation_retained_body_round_trips_quotes_backslashes_and_controls() {
    #[derive(serde::Deserialize)]
    struct Parsed {
        v: u8,
        reason: String,
        base: String,
        worktree: String,
        at: String,
    }
    let reason = "the changes did not apply cleanly (error: \"x\" \\ y\nz\ttab)";
    let worktree = "/wt/\"q\"\\back\nslash.retained-1";
    let base = "ba\"se";
    let body = retained_body(reason, base, worktree, "2026-09-25T10:20:00.000Z").unwrap();
    assert!(body.ends_with("}\n"));
    assert_eq!(body.matches('\n').count(), 1);
    let parsed: Parsed = sonic_rs::from_str(&body).unwrap();
    assert_eq!(parsed.v, 1);
    assert_eq!(parsed.reason, reason);
    assert_eq!(parsed.base, base);
    assert_eq!(parsed.worktree, worktree);
    assert_eq!(parsed.at, "2026-09-25T10:20:00.000Z");
}

#[test]
fn isolation_retain_and_prune_argv_are_exact_forms() {
    assert_eq!(
        argv_worktree_move("/repo", "/wt/1", "/wt/1.retained-42").join(" "),
        "git -C /repo worktree move /wt/1 /wt/1.retained-42"
    );
    assert_eq!(
        argv_worktree_prune("/repo").join(" "),
        "git -C /repo worktree prune"
    );
}
