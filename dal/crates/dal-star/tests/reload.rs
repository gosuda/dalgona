//! Reload invariants: a generation is published whole or not at all, bound
//! handlers keep the generation they were converted from, and every failure
//! arrives as a command error triple with the prior generation intact.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
#[path = "support/host.rs"]
pub mod host;
pub mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use dal_agent::ext::Extension;
use dal_agent::ext::tool::{Tool, ToolCall, ToolOutcome};
use dal_core::RawJson;
use dal_core::ext::OpSet;
use dal_star::{BundledPlugin, LoadRoots, PluginSystem, PluginsConfig, load};
use host::{RecordingHost, tool_cx};
use support::{system_with_plugin, write_plugin};

fn plugin(tool_result: &str) -> String {
    format!(
        "load(\"@dal/v1\", \"dal\")\ndef run(ctx, args):\n    return \"{tool_result}\"\nt = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"swap\", version = \"0.1.0\", tools = {{\"t\": t}})\n"
    )
}

fn two_tools() -> String {
    "load(\"@dal/v1\", \"dal\")\ndef run(ctx, args):\n    return \"two\"\na = dal.tool(description = \"d\", input = dal.schema(), run = run)\nb = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"swap\", version = \"0.1.0\", tools = {\"a\": a, \"b\": b})\n"
        .to_owned()
}

async fn run_first_tool(extensions: &[Extension]) -> String {
    let tool: Arc<dyn Tool> = extensions[0]
        .tools()
        .iter()
        .map(|(tool, _)| Arc::clone(tool))
        .next()
        .expect("the plugin exports a tool");
    let host = RecordingHost::new(extensions, OpSet::EMPTY, []);
    let args = RawJson::parse("{}").expect("json");
    match tool
        .run(ToolCall::new("reload", args), tool_cx(host.script_cx()))
        .await
    {
        ToolOutcome::Ok(output) => match &output.parts[0] {
            dal_core::Part::Text { text } => text.to_string(),
            other => panic!("expected text, got {other:?}"),
        },
        other => panic!("expected a successful tool outcome, got {other:?}"),
    }
}

#[test]
fn successful_reload_publishes_a_fresh_generation_with_whole_counts() {
    let (data, system) = system_with_plugin("swap", &plugin("one"));
    let before = system.snapshot().expect("snapshot").id;
    write_plugin(data.path(), "swap", &two_tools());

    let counts = system.reload().expect("a valid replacement reloads");

    assert_eq!((counts.plugins, counts.tools), (1, 2));
    assert_ne!(system.snapshot().expect("snapshot").id, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn bound_tools_keep_their_generation_across_reload() {
    let (data, system) = system_with_plugin("swap", &plugin("old"));
    let old = system.extensions().expect("old extensions");
    write_plugin(data.path(), "swap", &plugin("new"));
    system.reload().expect("reload succeeds");
    let new = system.extensions().expect("new extensions");

    assert_eq!(run_first_tool(&old).await, "old");
    assert_eq!(run_first_tool(&new).await, "new");
}

#[test]
fn reload_after_the_plugin_directory_is_deleted_publishes_an_empty_generation() {
    let (data, system) = system_with_plugin("swap", &plugin("one"));
    std::fs::remove_dir_all(data.path().join("plugins").join("swap")).expect("remove plugin");

    let counts = system.reload().expect("an empty plugin set is valid");

    assert_eq!(
        (counts.plugins, counts.tools, counts.commands, counts.skills),
        (0, 0, 0, 0)
    );
    assert!(system.snapshot().expect("snapshot").is_empty());
    assert!(system.extensions().expect("extensions").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_reload_reports_a_triple_and_keeps_serving_the_old_generation() {
    let (data, system) = system_with_plugin("swap", &plugin("old"));
    let before = system.snapshot().expect("snapshot").id;
    let extensions = system.extensions().expect("extensions");
    write_plugin(
        data.path(),
        "swap",
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \n",
    );

    let triple = system
        .reload()
        .expect_err("a syntax error fails the reload");

    assert_eq!(triple.what.as_ref(), "plugin reload");
    assert!(triple.why.contains("plugin.star"), "{}", triple.why);
    assert!(
        !triple.why.contains('\n'),
        "the reason is one line: {}",
        triple.why
    );
    assert!(triple.fix.contains("/reload"), "{}", triple.fix);
    assert_eq!(system.snapshot().expect("snapshot").id, before);
    assert_eq!(run_first_tool(&extensions).await, "old");
}

#[test]
fn every_rejection_class_keeps_the_published_generation() {
    let cases = [
        "plugin = 1\n",
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"other\", version = \"0.1.0\")\n",
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"swap\", version = \"0.1.0\", inject = [\"warp\"])\n",
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"swap\", version = \"not-a-version\")\n",
        "load(\"@dal/v1\", \"dal\")\nfor _ in range(100000000):\n    pass\nplugin = dal.plugin(name = \"swap\", version = \"0.1.0\")\n",
    ];
    let (data, system) = system_with_plugin("swap", &plugin("old"));
    let before = system.snapshot().expect("snapshot").id;
    for source in cases {
        write_plugin(data.path(), "swap", source);
        assert!(system.reload().is_err(), "must fail: {source}");
        assert_eq!(
            system.snapshot().expect("snapshot").id,
            before,
            "a failed reload must not swap: {source}"
        );
    }
}

#[test]
fn concurrent_reloads_all_succeed_and_leave_one_consistent_generation() {
    let (_data, system) = system_with_plugin("swap", &plugin("one"));
    let system = Arc::new(system);
    let outcomes: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8).map(|_| scope.spawn(|| system.reload())).collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("reload thread does not panic"))
            .collect()
    });
    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    assert_eq!(system.snapshot().expect("snapshot").len(), 1);
}

#[test]
fn user_extensions_exclude_bundled_plugins_and_a_user_plugin_shadows_a_bundled_name() {
    let entry: &'static [u8] =
        b"load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"core\", version = \"0.1.0\")\n";
    let data = tempfile::tempdir().expect("data root");
    write_plugin(data.path(), "swap", &plugin("one"));
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: vec![BundledPlugin {
            name: "core".to_owned(),
            files: BTreeMap::from([(PathBuf::from("plugin.star"), entry)]),
        }],
    };
    let config = PluginsConfig::default();
    let generation = load(&roots, &config).expect("load");
    let system = PluginSystem::new(generation, roots, config);

    assert_eq!(system.extensions().expect("all").len(), 2);
    let user = system.user_extensions().expect("user");
    assert_eq!(user.len(), 1);
    assert_eq!(user[0].name(), "swap");

    // A user plugin with a bundled plugin's name replaces it instead of merging.
    write_plugin(
        data.path(),
        "core",
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"core\", version = \"9.9.9\")\n",
    );
    system.reload().expect("shadowing reload");
    let registrations = system.snapshot().expect("snapshot").registrations();
    assert!(
        registrations.contains("plugin core/9.9.9,user"),
        "{registrations}"
    );
    assert!(!registrations.contains("core/0.1.0"), "{registrations}");
}
