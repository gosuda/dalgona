// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Tests for the manual page facts: each page names the limits its module
//! enforces, so a page edit that drops a fact fails here.

use super::{AGENTS_DOC, GOAL_DOC, WORKFLOWS_DOC};

#[test]
fn agents_page_states_the_fixed_facts() {
    for fact in [
        "`run` starts a workflow",
        "`wait` blocks",
        "`cancel` stops",
        "`list` shows runs",
        "{{item}}",
        "{{step:<name>}}",
        "{{input}}",
        "There is no escape for `{{`",
        "`read` and `search`",
        "orchestration__report",
        "grace turn of 60 seconds",
        "job://<id>",
        "1 to 32 steps",
        "[a-z][a-z0-9-]{0,31}",
        "1 to 32,000 characters",
        "1 to 1024",
        "1 to 2,000 characters",
        "at most 16 names",
        "16,000 characters",
        "1024 subagents",
        "git 2.17",
    ] {
        assert!(
            AGENTS_DOC.contains(fact),
            "agents page omits {fact:?}:\n{AGENTS_DOC}"
        );
    }
}

#[test]
fn workflows_page_states_the_shape_and_refusals() {
    for fact in [
        "[plugin.orchestration.workflows.<name>]",
        "items_from",
        "workers = 8",
        "{{input}}",
        "at most 200 characters",
        "agents: step <name>: tool <tool> is not for subagents.",
        "1 to 1024",
        "1 to 2,000 characters",
    ] {
        assert!(
            WORKFLOWS_DOC.contains(fact),
            "workflows page omits {fact:?}:\n{WORKFLOWS_DOC}"
        );
    }
}

#[test]
fn goal_page_states_the_lifecycle_and_limits() {
    for fact in [
        "/goal <objective>",
        "/goal pause",
        "/goal resume",
        "/goal clear",
        "`update_goal`",
        "1 to 4,000 characters",
        "continuation cap reached",
        "unattended continuation limit reached",
        "repeated assistant output",
        "output truncation repeated",
        "provider error ended the turn",
        "goal.json",
        "ephemeral session refuses goals",
        "`/continuation run`",
    ] {
        assert!(
            GOAL_DOC.contains(fact),
            "goal page omits {fact:?}:\n{GOAL_DOC}"
        );
    }
}

#[test]
fn pages_have_titles() {
    assert!(AGENTS_DOC.starts_with("# Agents\n"));
    assert!(WORKFLOWS_DOC.starts_with("# Workflows\n"));
    assert!(GOAL_DOC.starts_with("# Goal\n"));
}

#[test]
fn mirrors_match_the_registered_pages() {
    assert_eq!(
        include_str!("../../../../docs/agents.md"),
        AGENTS_DOC,
        "dalgona/docs/agents.md drifted from the registered page"
    );
    assert_eq!(
        include_str!("../../../../docs/workflows.md"),
        WORKFLOWS_DOC,
        "dalgona/docs/workflows.md drifted from the registered page"
    );
    assert_eq!(
        include_str!("../../../../docs/goal.md"),
        GOAL_DOC,
        "dalgona/docs/goal.md drifted from the registered page"
    );
}
