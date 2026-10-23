//! Starlark adapter proofs: the Rust-only event boundary, the
//! eval-cell namespace boundary, pure-cell effect denial, and two-plugin
//! reload atomicity (rollback after partial failure).
//!
//! Load-level tests reuse the descriptor-error harness shape; eval tests
//! run the `eval` tool over the recording host; the rollback test drives a
//! two-plugin [`PluginSystem`] and keeps serving both old plugins after a
//! partial reload failure.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
#[path = "support/host.rs"]
pub mod host;
pub mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use dal_agent::ext::Extension;
use dal_agent::ext::script::{HostTerminal, OpOutcome};
use dal_agent::ext::tool::{Tool, ToolCall, ToolOutcome};
use dal_core::ext::{NativeOp, OpId, OpSet};
use dal_core::{Part, PluginLimits, RawJson};
use dal_star::{
    CELL_LIMITS, LoadError, LoadRoots, PluginGeneration, PluginSystem, PluginsConfig,
    eval_extension, load,
};
use host::{HostRecord, RecordingHost, eval_args, native_op, tool_cx};
use support::write_plugin;

const HEADER: &str = "load(\"@dal/v1\", \"dal\")\n";

fn config() -> PluginsConfig {
    PluginsConfig {
        enabled: Vec::new(),
        limits: PluginLimits::default(),
        configs: BTreeMap::new(),
    }
}

fn load_named(
    name: &str,
    source: &str,
    cfg: &PluginsConfig,
) -> (tempfile::TempDir, Result<PluginGeneration, LoadError>) {
    let data = tempfile::tempdir().expect("data root");
    let dir = data.path().join("plugins").join(name);
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(dir.join("plugin.star"), source).expect("plugin source");
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let result = load(&roots, cfg);
    (data, result)
}

fn rejected(body: &str) -> LoadError {
    let source = format!("{HEADER}{body}");
    load_named("probe", &source, &config())
        .1
        .expect_err("the declaration must be rejected")
}

fn assert_eval(error: &LoadError, needle: &str) {
    let LoadError::Eval { message, line, .. } = error else {
        panic!("expected a located Eval error, got {error:?}");
    };
    assert!(*line >= 1);
    assert!(
        message.contains(needle),
        "message lacks `{needle}`: {message}"
    );
}

#[test]
fn output_stream_subscription_is_rejected_at_the_constructor() {
    assert_eval(
        &rejected(
            "def h(ctx, event):\n    return None\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", hooks = [dal.on(\"output_stream\", h)])\n",
        ),
        "unknown event",
    );
}

// --- eval harness -----------------------------------------------------------

struct Run {
    host: Arc<RecordingHost>,
    outcome: ToolOutcome,
}

impl Run {
    fn records(&self) -> Vec<HostRecord> {
        self.host.records()
    }

    fn calls(&self) -> usize {
        self.records()
            .iter()
            .filter(|record| matches!(record, HostRecord::Call { .. }))
            .count()
    }

    fn begins(&self) -> usize {
        self.records()
            .iter()
            .filter(|record| matches!(record, HostRecord::Begin { .. }))
            .count()
    }

    fn finishes(&self) -> usize {
        self.records()
            .iter()
            .filter(|record| matches!(record, HostRecord::Finish { .. }))
            .count()
    }

    fn failure_text(&self) -> String {
        match &self.outcome {
            ToolOutcome::Err(error) => error.to_string(),
            other => panic!("expected a failed tool outcome, got {other:?}"),
        }
    }
}

fn named_tool(extension: &Extension, name: &str) -> Arc<dyn Tool> {
    extension
        .tools()
        .iter()
        .find(|(tool, _)| tool.name().as_str() == name)
        .map_or_else(
            || panic!("tool {name:?} is registered"),
            |(tool, _)| Arc::clone(tool),
        )
}

async fn eval_raw(arguments: &str, allowed: &[&str], replies: Vec<(OpId, OpOutcome)>) -> Run {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let tool = named_tool(&extension, "eval");
    let extensions = [extension];
    let allowed = OpSet::parse(allowed.iter().copied()).expect("allowed operations");
    let host = RecordingHost::new(&extensions, allowed, replies);
    let args = RawJson::parse(arguments).expect("tool arguments are JSON");
    let outcome = tool
        .run(ToolCall::new("test-call", args), tool_cx(host.script_cx()))
        .await;
    Run { host, outcome }
}

async fn eval_with(code: &str, allowed: &[&str], replies: Vec<(OpId, OpOutcome)>) -> Run {
    eval_raw(&eval_args(code), allowed, replies).await
}

async fn eval(code: &str) -> Run {
    eval_with(code, &[], Vec::new()).await
}

#[tokio::test(flavor = "multi_thread")]
async fn cell_namespace_exposes_no_dal_global() {
    let run = eval("dal").await;
    let text = run.failure_text();
    assert!(text.contains("dal"), "{text}");
    assert_eq!(run.calls(), 0, "an unbound global never reaches the host");
    assert_eq!((run.begins(), run.finishes()), (1, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn pure_cell_direct_call_propagates_host_denial_and_finishes_once() {
    let op = native_op(NativeOp::EnvRead);
    let run = eval_with(
        "ctx.env.read(key = \"k\")",
        &[],
        vec![(op, OpOutcome::Terminal(HostTerminal::Revoked))],
    )
    .await;
    let text = run.failure_text();
    assert!(text.contains("\"denied\""), "{text}");
    assert!(text.contains("revoked"), "{text}");
    assert_eq!(run.calls(), 1);
    assert_eq!((run.begins(), run.finishes()), (1, 1));
}

// --- two-plugin reload atomicity --------------------------------------------

fn tool_plugin(name: &str, tool: &str, result: &str) -> String {
    format!(
        "load(\"@dal/v1\", \"dal\")\ndef run(ctx, args):\n    return \"{result}\"\nt = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"{name}\", version = \"0.1.0\", tools = {{\"{tool}\": t}})\n"
    )
}

fn two_plugin_system() -> (tempfile::TempDir, PluginSystem) {
    let data = tempfile::tempdir().expect("data root");
    write_plugin(
        data.path(),
        "keep",
        &tool_plugin("keep", "keep-t", "keep-old"),
    );
    write_plugin(
        data.path(),
        "swap",
        &tool_plugin("swap", "swap-t", "swap-old"),
    );
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let cfg = config();
    let generation = load(&roots, &cfg).expect("two valid plugins load");
    (data, PluginSystem::new(generation, roots, cfg))
}

async fn run_tool_named(extensions: &[Extension], tool_name: &str) -> String {
    let mut found = None;
    for extension in extensions {
        for (tool, _) in extension.tools() {
            if tool.name().as_str() == tool_name {
                found = Some(Arc::clone(tool));
            }
        }
    }
    let tool = found.unwrap_or_else(|| panic!("tool {tool_name:?} is registered"));
    let host = RecordingHost::new(extensions, OpSet::EMPTY, []);
    let args = RawJson::parse("{}").expect("json");
    match tool
        .run(ToolCall::new("test-call", args), tool_cx(host.script_cx()))
        .await
    {
        ToolOutcome::Ok(output) => match &output.parts[0] {
            Part::Text { text } => text.to_string(),
            other => panic!("expected text, got {other:?}"),
        },
        other => panic!("expected a successful tool outcome, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_reload_of_one_plugin_keeps_both_old_plugins_serving() {
    let (data, system) = two_plugin_system();
    let before = system.snapshot().expect("snapshot").id;
    let old = system.extensions().expect("old extensions");
    assert_eq!(run_tool_named(&old, "keep__keep-t").await, "keep-old");
    assert_eq!(run_tool_named(&old, "swap__swap-t").await, "swap-old");

    write_plugin(
        data.path(),
        "keep",
        &tool_plugin("keep", "keep-t", "keep-new"),
    );
    write_plugin(
        data.path(),
        "swap",
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \n",
    );

    let triple = system
        .reload()
        .expect_err("the broken sibling fails the reload");
    assert_eq!(triple.what.as_ref(), "plugin reload");

    assert_eq!(
        system.snapshot().expect("snapshot").id,
        before,
        "a partial failure swaps nothing"
    );
    let kept = system.extensions().expect("kept extensions");
    assert_eq!(run_tool_named(&kept, "keep__keep-t").await, "keep-old");
    assert_eq!(run_tool_named(&kept, "swap__swap-t").await, "swap-old");
    let mut names: Vec<String> = kept
        .iter()
        .flat_map(|extension| {
            extension
                .tools()
                .iter()
                .map(|(tool, _)| tool.name().to_string())
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["keep__keep-t".to_owned(), "swap__swap-t".to_owned()]
    );
}
