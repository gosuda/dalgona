//! Real `[limits.plugins]` budget attacks through `Config` into the tool VM.
//!
//! Each budget case travels the production path: `Config::load` parses the
//! `[limits.plugins]` table, the test copies `config.plugin_limits()` into
//! `PluginsConfig`, `dal_star::load` evaluates the fixture, and the handler
//! cases continue through `PluginSystem::extensions` into a real tool call
//! over `RecordingHost` with no scripted replies. A small budget fails with
//! its typed, located failure naming the configured key; the same workload
//! under a larger budget succeeds with its known value.
//!
//! The two trailing controls carry the vendor budget wording without a real
//! budget breach: an explicit `fail()` and a floor-division error whose
//! source mentions the tick wording. Neither may gain a
//! `raise limits.plugins.*` remedy.

#[path = "support/host.rs"]
pub mod host;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use dal_agent::ext::Extension;
use dal_agent::ext::tool::{Tool, ToolCall, ToolOutcome};
use dal_core::ext::OpSet;
use dal_core::{Config, ConfigProduct, Part, RawJson};
use dal_star::{LoadError, LoadRoots, PluginGeneration, PluginSystem, PluginsConfig, load};
use host::{RecordingHost, tool_cx_approved};

fn write_plugin(data_root: &Path, name: &str, source: &str) -> std::io::Result<()> {
    let dir = data_root.join("plugins").join(name);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("plugin.star"), source)?;
    Ok(())
}

fn plugins_config_from_toml(
    user_toml: &str,
    data_root: &Path,
) -> Result<PluginsConfig, Box<dyn Error + Send + Sync>> {
    let config = Config::load(ConfigProduct::Dalgon, data_root, "", Some(user_toml))?;
    Ok(PluginsConfig {
        enabled: Vec::new(),
        limits: *config.plugin_limits(),
        configs: BTreeMap::new(),
    })
}

fn check_contains(
    haystack: &str,
    needle: &str,
    context: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{context}: expected {haystack:?} to contain {needle:?}"
        ))
        .into())
    }
}

fn check_absent(
    haystack: &str,
    needle: &str,
    context: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if haystack.contains(needle) {
        Err(std::io::Error::other(format!(
            "{context}: expected {haystack:?} to omit {needle:?}"
        ))
        .into())
    } else {
        Ok(())
    }
}

fn find_tool(
    extensions: &[Extension],
    name: &str,
) -> Result<Arc<dyn Tool>, Box<dyn Error + Send + Sync>> {
    for extension in extensions {
        for (tool, _) in extension.tools() {
            if tool.name().as_str() == name {
                return Ok(Arc::clone(tool));
            }
        }
    }
    Err(std::io::Error::other(format!("tool {name:?} is not registered")).into())
}

fn ok_text(outcome: &ToolOutcome) -> Result<String, Box<dyn Error + Send + Sync>> {
    match outcome {
        ToolOutcome::Ok(output) => match output.parts.first() {
            Some(Part::Text { text }) => Ok(text.to_string()),
            Some(other) => {
                Err(std::io::Error::other(format!("expected a text part, got {other:?}")).into())
            }
            None => Err(std::io::Error::other("tool output has no parts").into()),
        },
        other => Err(std::io::Error::other(format!(
            "expected a successful tool outcome, got {other:?}"
        ))
        .into()),
    }
}

fn err_text(outcome: &ToolOutcome) -> Result<String, Box<dyn Error + Send + Sync>> {
    match outcome {
        ToolOutcome::Err(error) => Ok(error.to_string()),
        other => Err(std::io::Error::other(format!(
            "expected a failed tool outcome, got {other:?}"
        ))
        .into()),
    }
}

fn eval_failure(
    result: Result<PluginGeneration, LoadError>,
) -> Result<(std::path::PathBuf, u32, u32, String), Box<dyn Error + Send + Sync>> {
    match result {
        Err(LoadError::Eval {
            path,
            line,
            col,
            message,
        }) => Ok((path, line, col, message.to_string())),
        Err(other) => Err(std::io::Error::other(format!(
            "expected a located evaluation failure, got {other:?}"
        ))
        .into()),
        Ok(generation) => Err(std::io::Error::other(format!(
            "expected a located evaluation failure, load succeeded with {} plugin(s): {:?}",
            generation.len(),
            generation.names().collect::<Vec<_>>(),
        ))
        .into()),
    }
}

#[test]
fn load_tick_budget_reports_its_key_and_a_roomier_budget_loads_the_same_module()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let workload = "load(\"@dal/v1\", \"dal\")\nfor _ in range(80000):\n    pass\nplugin = dal.plugin(name = \"tickbound\", version = \"0.1.0\")\n";
    let small_toml = "[limits.plugins]\nload_ticks = 1500\n";
    let data_small = tempfile::tempdir()?;
    write_plugin(data_small.path(), "tickbound", workload)?;
    let small_cfg = plugins_config_from_toml(small_toml, data_small.path())?;
    let roots_small = LoadRoots {
        data_root: data_small.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let (path, line, col, message) = eval_failure(load(&roots_small, &small_cfg))?;
    if line == 0 || col == 0 {
        return Err(std::io::Error::other(format!(
            "tick failure must carry a one-based location, got {line}:{col}"
        ))
        .into());
    }
    if !path.ends_with("plugin.star") {
        return Err(std::io::Error::other(format!(
            "tick failure must name plugin.star, got {}",
            path.display()
        ))
        .into());
    }
    check_contains(
        &message,
        "Execution duration limit",
        "load tick vendor prefix",
    )?;
    check_contains(&message, "1500", "load tick applied budget")?;
    check_contains(
        &message,
        "raise limits.plugins.load_ticks to allow more",
        "load tick remedy key",
    )?;

    let roomy_toml = "[limits.plugins]\nload_ticks = 10000000\n";
    let data_roomy = tempfile::tempdir()?;
    write_plugin(data_roomy.path(), "tickbound", workload)?;
    let roomy_cfg = plugins_config_from_toml(roomy_toml, data_roomy.path())?;
    let roots_roomy = LoadRoots {
        data_root: data_roomy.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let generation = load(&roots_roomy, &roomy_cfg).map_err(|error| {
        std::io::Error::other(format!(
            "the roomier load_ticks budget must load: {error:?}"
        ))
    })?;
    if generation.len() != 1 {
        return Err(std::io::Error::other(format!(
            "the roomier load must hold one plugin, got {}",
            generation.len()
        ))
        .into());
    }
    let names: Vec<String> = generation.names().map(str::to_owned).collect();
    if names != vec!["tickbound".to_owned()] {
        return Err(std::io::Error::other(format!(
            "the roomier load must hold tickbound, got {names:?}"
        ))
        .into());
    }
    Ok(())
}

#[test]
fn load_heap_budget_reports_its_key_and_a_roomier_budget_loads_the_same_module()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let workload = "load(\"@dal/v1\", \"dal\")\nitems = []\nfor _ in range(1500):\n    items.append(\"x\" * 256)\nplugin = dal.plugin(name = \"heapbound\", version = \"0.1.0\")\n";
    let small_toml = "[limits.plugins]\nload_heap_bytes = 49152\nload_ticks = 20000000\n";
    let data_small = tempfile::tempdir()?;
    write_plugin(data_small.path(), "heapbound", workload)?;
    let small_cfg = plugins_config_from_toml(small_toml, data_small.path())?;
    let roots_small = LoadRoots {
        data_root: data_small.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let (path, line, col, message) = eval_failure(load(&roots_small, &small_cfg))?;
    if line == 0 || col == 0 {
        return Err(std::io::Error::other(format!(
            "heap failure must carry a one-based location, got {line}:{col}"
        ))
        .into());
    }
    if !path.ends_with("plugin.star") {
        return Err(std::io::Error::other(format!(
            "heap failure must name plugin.star, got {}",
            path.display()
        ))
        .into());
    }
    check_contains(&message, "Heap memory limit", "load heap vendor prefix")?;
    check_contains(&message, "49152", "load heap applied budget")?;
    check_contains(
        &message,
        "raise limits.plugins.load_heap_bytes to allow more",
        "load heap remedy key",
    )?;

    let roomy_toml = "[limits.plugins]\nload_ticks = 20000000\n";
    let data_roomy = tempfile::tempdir()?;
    write_plugin(data_roomy.path(), "heapbound", workload)?;
    let roomy_cfg = plugins_config_from_toml(roomy_toml, data_roomy.path())?;
    let roots_roomy = LoadRoots {
        data_root: data_roomy.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let generation = load(&roots_roomy, &roomy_cfg).map_err(|error| {
        std::io::Error::other(format!("the roomier load_heap budget must load: {error:?}"))
    })?;
    if generation.len() != 1 {
        return Err(std::io::Error::other(format!(
            "the roomier load must hold one plugin, got {}",
            generation.len()
        ))
        .into());
    }
    let names: Vec<String> = generation.names().map(str::to_owned).collect();
    if names != vec!["heapbound".to_owned()] {
        return Err(std::io::Error::other(format!(
            "the roomier load must hold heapbound, got {names:?}"
        ))
        .into());
    }
    Ok(())
}

async fn run_plugin_tool(
    plugin: &str,
    tool: &str,
    source: &str,
    user_toml: &str,
) -> Result<(tempfile::TempDir, ToolOutcome, Arc<RecordingHost>), Box<dyn Error + Send + Sync>> {
    let data = tempfile::tempdir()?;
    write_plugin(data.path(), plugin, source)?;
    let cfg = plugins_config_from_toml(user_toml, data.path())?;
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let generation = load(&roots, &cfg).map_err(|error| {
        std::io::Error::other(format!("the handler fixture must load: {error:?}"))
    })?;
    let system = PluginSystem::new(generation, roots, cfg);
    let extensions = system.extensions().map_err(|error| {
        std::io::Error::other(format!("the handler fixture must convert: {error:?}"))
    })?;
    let qualified = format!("{plugin}__{tool}");
    let handle = find_tool(&extensions, &qualified)?;
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let args = RawJson::parse("{}")?;
    let outcome = handle
        .run(
            ToolCall::new("test-call", args),
            tool_cx_approved(host.script_cx()),
        )
        .await;
    Ok((data, outcome, host))
}

#[tokio::test(flavor = "multi_thread")]
async fn handler_tick_budget_reports_its_key_and_a_roomier_budget_returns_its_marker()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let source = "load(\"@dal/v1\", \"dal\")\ndef spin(ctx, args):\n    for _ in range(80000):\n        pass\n    return \"handler-tick-ok\"\nspin_tool = dal.tool(description = \"Spin.\", input = dal.schema(), run = spin)\nplugin = dal.plugin(name = \"ticktool\", version = \"0.1.0\", tools = {\"spin\": spin_tool})\n";
    let (_data, outcome, _host) = run_plugin_tool(
        "ticktool",
        "spin",
        source,
        "[limits.plugins]\nhandler_ticks = 1700\n",
    )
    .await?;
    let text = err_text(&outcome)?;
    check_contains(
        &text,
        "Execution duration limit",
        "handler tick vendor prefix",
    )?;
    check_contains(&text, "1700", "handler tick applied budget")?;
    check_contains(
        &text,
        "raise limits.plugins.handler_ticks to allow more",
        "handler tick remedy key",
    )?;

    let (_roomy, fits, _) = run_plugin_tool(
        "ticktool",
        "spin",
        source,
        "[limits.plugins]\nhandler_ticks = 10000000\n",
    )
    .await?;
    let text = ok_text(&fits)?;
    if text != "handler-tick-ok" {
        return Err(std::io::Error::other(format!(
            "the roomier handler_ticks budget must return its marker, got {text:?}"
        ))
        .into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn handler_heap_budget_reports_its_key_and_a_roomier_budget_returns_its_count()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let source = "load(\"@dal/v1\", \"dal\")\ndef grow(ctx, args):\n    items = []\n    for _ in range(1500):\n        items.append(\"x\" * 256)\n    return len(items)\ngrow_tool = dal.tool(description = \"Grow.\", input = dal.schema(), run = grow)\nplugin = dal.plugin(name = \"heaptool\", version = \"0.1.0\", tools = {\"grow\": grow_tool})\n";
    let (_data, outcome, _host) = run_plugin_tool(
        "heaptool",
        "grow",
        source,
        "[limits.plugins]\nhandler_heap_bytes = 49152\nhandler_ticks = 20000000\n",
    )
    .await?;
    let text = err_text(&outcome)?;
    check_contains(&text, "Heap memory limit", "handler heap vendor prefix")?;
    check_contains(&text, "49152", "handler heap applied budget")?;
    check_contains(
        &text,
        "raise limits.plugins.handler_heap_bytes to allow more",
        "handler heap remedy key",
    )?;

    let (_roomy, fits, _) = run_plugin_tool(
        "heaptool",
        "grow",
        source,
        "[limits.plugins]\nhandler_ticks = 20000000\n",
    )
    .await?;
    let text = ok_text(&fits)?;
    if text != "1500" {
        return Err(std::io::Error::other(format!(
            "the roomier handler_heap budget must return its count, got {text:?}"
        ))
        .into());
    }
    Ok(())
}

fn chain_source(depth: u32) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut source = String::from("load(\"@dal/v1\", \"dal\")\n");
    for index in 0..depth {
        let next = index + 1;
        write!(source, "def f{index}():\n    return f{next}()\n")?;
    }
    write!(source, "def f{depth}():\n    return {depth}\n")?;
    source.push_str(
        "def entry(ctx, args):\n    return f0()\nentry_tool = dal.tool(description = \"Walk a call chain.\", input = dal.schema(), run = entry)\nplugin = dal.plugin(name = \"chain\", version = \"0.1.0\", tools = {\"entry\": entry_tool})\n",
    );
    Ok(source)
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_stack_depth_reports_its_key_and_a_roomier_depth_returns_its_peak()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let source = chain_source(25)?;
    let (_data, outcome, _host) = run_plugin_tool(
        "chain",
        "entry",
        &source,
        "[limits.plugins]\nstack_depth = 12\n",
    )
    .await?;
    let text = err_text(&outcome)?;
    check_contains(&text, "Starlark call stack overflow", "stack vendor prefix")?;
    check_contains(
        &text,
        "raise limits.plugins.stack_depth to allow more",
        "stack remedy key",
    )?;

    let (_roomy, fits, _) = run_plugin_tool(
        "chain",
        "entry",
        &source,
        "[limits.plugins]\nstack_depth = 100\n",
    )
    .await?;
    let text = ok_text(&fits)?;
    if text != "25" {
        return Err(std::io::Error::other(format!(
            "the roomier stack depth must return its peak, got {text:?}"
        ))
        .into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_fail_carrying_vendor_wording_gains_no_remedy()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let source = "load(\"@dal/v1\", \"dal\")\ndef bad(ctx, args):\n    fail(\"Execution duration limit of 9999 ticks has been exceeded\")\nbad_tool = dal.tool(description = \"Fail with vendor wording.\", input = dal.schema(), run = bad)\nplugin = dal.plugin(name = \"failprobe\", version = \"0.1.0\", tools = {\"bad\": bad_tool})\n";
    let (_data, outcome, _host) = run_plugin_tool("failprobe", "bad", source, "").await?;
    let text = err_text(&outcome)?;
    check_contains(&text, "9999", "explicit fail keeps its marker")?;
    check_absent(
        &text,
        "raise limits.plugins.",
        "explicit fail must not invent a budget remedy",
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn division_error_whose_source_mentions_vendor_wording_gains_no_remedy()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let source = "load(\"@dal/v1\", \"dal\")\n# Budget probe comment only: Execution duration limit of 9999 ticks has been exceeded\ndef bad(ctx, args):\n    return 1 // 0\nbad_tool = dal.tool(description = \"Divide by zero.\", input = dal.schema(), run = bad)\nplugin = dal.plugin(name = \"divprobe\", version = \"0.1.0\", tools = {\"bad\": bad_tool})\n";
    let (_data, outcome, _host) = run_plugin_tool("divprobe", "bad", source, "").await?;
    let text = err_text(&outcome)?;
    check_contains(
        &text,
        "Floor division by zero",
        "division error keeps its vendor wording",
    )?;
    check_absent(
        &text,
        "raise limits.plugins.",
        "division error must not invent a budget remedy",
    )?;
    Ok(())
}
