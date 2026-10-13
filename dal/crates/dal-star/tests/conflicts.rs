//! Registration conflicts between plugins: a name two plugins both claim must
//! fail the host start, never resolve by silent shadowing.

#![expect(
    clippy::expect_used,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
pub mod support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::ext::Extension;
use dal_agent::{Env, Host, Product};
use dal_core::{Config, ConfigProduct};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};
use support::write_plugin;

const TOOL_BODY: &str = "def run(ctx, args):\n    return None\n";

fn tool_plugin(name: &str, tool: &str) -> String {
    format!(
        "load(\"@dal/v1\", \"dal\")\n{TOOL_BODY}t = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"{name}\", version = \"0.1.0\", tools = {{\"{tool}\": t}})\n"
    )
}

fn model_plugin(name: &str) -> String {
    format!(
        "load(\"@dal/v1\", \"dal\")\ndef infer(ctx, request):\n    return None\nm = dal.model(id = \"dalgona/shared\", caps = {{\"context_window\": 8, \"thinking\": [\"off\"], \"tool_use\": False, \"image_input\": False}}, run = infer)\nplugin = dal.plugin(name = \"{name}\", version = \"0.1.0\", models = {{\"m\": m}})\n"
    )
}

/// Loads every `(directory, source)` plugin and converts the generation.
fn convert(plugins: &[(&str, String)]) -> (tempfile::TempDir, Result<Vec<Extension>, String>) {
    let data = tempfile::tempdir().expect("data root");
    for (name, source) in plugins {
        write_plugin(data.path(), name, source);
    }
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let config = PluginsConfig::default();
    let generation = match load(&roots, &config) {
        Ok(generation) => generation,
        Err(error) => return (data, Err(format!("load: {error}"))),
    };
    let converted = PluginSystem::new(generation, roots, config)
        .extensions()
        .map_err(|error| format!("convert: {error}"));
    (data, converted)
}

/// Starts a host over `extensions`; returns the refusal text, or `None` when
/// the host started.
async fn refusal(data: &tempfile::TempDir, extensions: Vec<Extension>) -> Option<String> {
    let fixture = data.path().join("conflict.jsonl");
    std::fs::write(&fixture, "").expect("provider fixture");
    let user = format!(
        "model = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, data.path(), "", Some(&user)).expect("config");
    let workspace = tempfile::tempdir().expect("workspace");
    let product = Product {
        name: "dal",
        data_root: data.path().to_path_buf(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    match Host::start(product, config, env).await {
        Ok(host) => {
            let _ = host.shutdown(Duration::from_secs(1)).await;
            None
        }
        Err(error) => Some(error.to_string()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn control_distinct_plugins_start() {
    let (data, converted) = convert(&[
        ("alpha", tool_plugin("alpha", "t")),
        ("beta", tool_plugin("beta", "t")),
    ]);
    let extensions = converted.expect("distinct plugins convert");
    assert_eq!(refusal(&data, extensions).await, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn wire_names_that_collide_through_underscores_are_refused() {
    // `a` + `b__c` and `a__b` + `c` both spell the wire name `a__b__c`.
    let (data, converted) = convert(&[
        ("a", tool_plugin("a", "b__c")),
        ("a__b", tool_plugin("a__b", "c")),
    ]);
    let extensions =
        converted.expect("each plugin converts on its own; the host owns cross-plugin conflicts");
    let refused = refusal(&data, extensions).await;
    assert!(
        refused.is_some(),
        "two tools with one wire name must not both register"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_plugins_declaring_one_public_model_id_are_refused() {
    let (data, converted) = convert(&[
        ("alpha", model_plugin("alpha")),
        ("beta", model_plugin("beta")),
    ]);
    let extensions =
        converted.expect("each plugin converts on its own; the host owns cross-plugin conflicts");
    let refused = refusal(&data, extensions).await;
    assert!(
        refused.is_some(),
        "one public model id cannot route to two handlers"
    );
}
