// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Workflow decoder and renderer tests.

use super::render::render;
use super::*;
use dal_core::{JobId, RawJson};

fn decode(raw: &str, input: Option<&str>) -> Result<Workflow, WorkflowError> {
    let raw = RawJson::parse(raw).expect("test input is valid JSON");
    decode_steps(&raw, input, "test")
}

fn error(raw: &str, expected: &str) {
    assert_eq!(decode(raw, None).unwrap_err().to_string(), expected);
}

#[test]
fn workflow_validation_order() {
    error("[]", "agents: steps must hold 1 to 32 steps.");
    error(
        r#"[{"name":"ok","prompt":"work","mystery":true}]"#,
        "agents: step ok: unknown field \"mystery\".",
    );
    error(
        r#"[{"name":"bad_name","prompt":"work"}]"#,
        "agents: step name \"bad_name\" must match [a-z][a-z0-9-]{0,31}.",
    );
    error(
        r#"[{"name":"same","prompt":"one"},{"name":"same","prompt":"two"}]"#,
        "agents: step name \"same\" is used twice.",
    );
    error(
        r#"[{"name":"short","prompt":""}]"#,
        "agents: step short: prompt must be 1 to 32000 characters.",
    );
    error(
        r#"[{"name":"pool","prompt":"{{item}}","items":[],"items_from":"upstream"}]"#,
        "agents: step pool: items and items_from cannot both be set.",
    );
    error(
        r#"[{"name":"task","prompt":"work","workers":1}]"#,
        "agents: step task: workers applies only to a pool.",
    );
    error(
        r#"[{"name":"task","prompt":"work","after":["missing"]}]"#,
        "agents: step task waits for missing, which does not exist.",
    );
    error(
        r#"[{"name":"pool","prompt":"{{item}} {{step:source}}","items_from":"source","after":[]}]"#,
        "agents: step pool: items_from names source, which is not in after.",
    );
    error(
        r#"[{"name":"task","prompt":"{{input}}"}]"#,
        "agents: step task: {{input}} needs a saved workflow and input.",
    );
    error(
        r#"[{"name":"task","prompt":"work","tools":["read","monitor"]}]"#,
        "agents: step task: tool monitor is not for subagents.",
    );
    error(
        r#"[{"name":"task","prompt":"work","isolation":"checkout"}]"#,
        "agents: step task: isolation must be \"worktree\" or \"shared\".",
    );
}

#[test]
fn workflow_cycles_reference() {
    let mut seed = 0x8c67_8d31_4a9b_0f25_u64;
    for count in 1..=32 {
        let mut edges = vec![Vec::new(); count];
        for (step, dependencies) in edges.iter_mut().enumerate() {
            for dependency in 0..count {
                if step != dependency {
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    if seed >> 61 == 0 {
                        dependencies.push(dependency);
                    }
                }
            }
        }
        let mut json = String::from("[");
        for (index, dependencies) in edges.iter().enumerate() {
            if index != 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"name\":\"s{index}\",\"prompt\":\"work\",\"after\":["
            ));
            for (position, dependency) in dependencies.iter().enumerate() {
                if position != 0 {
                    json.push(',');
                }
                json.push_str(&format!("\"s{dependency}\""));
            }
            json.push_str("]}");
        }
        json.push(']');

        let actual = decode(&json, None);
        let mut indegree = vec![0_usize; count];
        let mut dependents = vec![Vec::new(); count];
        for (step, dependencies) in edges.iter().enumerate() {
            indegree[step] = dependencies.len();
            for &dependency in dependencies {
                dependents[dependency].push(step);
            }
        }
        let mut ready = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, &degree)| (degree == 0).then_some(index))
            .collect::<std::collections::VecDeque<_>>();
        let mut visited = 0;
        while let Some(node) = ready.pop_front() {
            visited += 1;
            for &dependent in &dependents[node] {
                indegree[dependent] -= 1;
                if indegree[dependent] == 0 {
                    ready.push_back(dependent);
                }
            }
        }
        assert_eq!(actual.is_ok(), visited == count, "graph with {count} steps");
    }
}

#[test]
fn workflow_cycle_reports_the_real_back_edge_path() {
    error(
        r#"[{"name":"a","prompt":"work","after":["b"]},{"name":"b","prompt":"work","after":["c"]},{"name":"c","prompt":"work","after":["a"]}]"#,
        "agents: steps form a cycle: a -> b -> c -> a.",
    );
}

#[test]
fn workflow_role_system_conflict() {
    error(
        r#"[{"name":"step","prompt":"work","role":"reviewer","system":"be concise"}]"#,
        "agents: step step: role and system cannot both be set.",
    );
}

#[test]
fn workflow_forbidden_member_tool() {
    error(
        r#"[{"name":"step","prompt":"work","tools":["read","create_goal"]}]"#,
        "agents: step step: tool create_goal is not for subagents.",
    );
}

#[test]
fn workflow_render_dependencies() {
    let workflow = decode(
            r#"[{"name":"task","prompt":"do work"},{"name":"pool","prompt":"inspect {{item}}","items":["one","two"]},{"name":"summary","prompt":"{{input}}: {{step:task}} / {{step:pool}}","after":["task","pool"]}]"#,
            Some("continue"),
        )
        .expect("workflow should validate");
    let summary = &workflow.steps[2];
    let reports = [
        StepResult {
            name: "task".to_owned(),
            task_report: Some("task report".into()),
            pool_items: None,
        },
        StepResult {
            name: "pool".to_owned(),
            task_report: None,
            pool_items: Some(vec![
                PoolItemResult {
                    item: "one".into(),
                    state: "done".into(),
                    summary: "first".into(),
                },
                PoolItemResult {
                    item: "two".into(),
                    state: "blocked".into(),
                    summary: "second".into(),
                },
            ]),
        },
    ];
    assert_eq!(
        render(summary, None, &reports, Some("continue"), JobId::new_v7()),
        "continue: task report / - one: done: first\n- two: blocked: second"
    );
}

#[test]
fn workflow_pool_dependency_cut_is_utf8_safe_and_bounded() {
    let step = Step {
        name: "summary".to_owned(),
        prompt: "{{step:pool}}".to_owned(),
        items: Items::Task,
        workers: 4,
        after: vec![0],
        tools: vec!["read".to_owned()],
        model: None,
        role: None,
        system: None,
        isolation: Isolation::Shared,
    };
    let reports = [StepResult {
        name: "pool".to_owned(),
        task_report: None,
        pool_items: Some(vec![PoolItemResult {
            item: "界".repeat(40).into(),
            state: "done".into(),
            summary: "é".repeat(12_000).into(),
        }]),
    }];
    let rendered = render(&step, None, &reports, None, JobId::new_v7());
    assert!(rendered.len() <= POOL_REPORT_LIMIT);
    assert!(rendered.ends_with("(cut; read job://") == false);
    assert!(rendered.contains("(cut; read job://"));
    assert!(rendered.is_char_boundary(rendered.len()));
}

#[test]
fn workflow_invalid_isolation() {
    error(
        r#"[{"name":"task","prompt":"work","isolation":"checkout"}]"#,
        "agents: step task: isolation must be \"worktree\" or \"shared\".",
    );
    error(
        r#"[{"name":"task","prompt":"work","isolation":7}]"#,
        "agents: step task: isolation must be \"worktree\" or \"shared\".",
    );
    let shared = decode(
        r#"[{"name":"task","prompt":"work","isolation":"shared"}]"#,
        None,
    )
    .expect("shared isolation validates");
    assert_eq!(shared.steps[0].isolation, Isolation::Shared);
    let inferred = decode(
        r#"[{"name":"task","prompt":"work","tools":["exec"]}]"#,
        None,
    )
    .expect("exec infers worktree");
    assert_eq!(inferred.steps[0].isolation, Isolation::Worktree);
}
