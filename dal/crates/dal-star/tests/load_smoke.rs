//! End-to-end proof for the plugin load pipeline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dal_star::{LoadRoots, PluginsConfig, load};

fn write_plugin(data: &Path, name: &str, entry: &str) -> std::io::Result<()> {
    let dir = data.join("plugins").join(name);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("plugin.star"), entry)
}

fn load_all(data: &Path) -> Result<dal_star::PluginGeneration, dal_star::LoadError> {
    load(
        &LoadRoots {
            data_root: data.to_path_buf(),
            bundled: Vec::new(),
        },
        &PluginsConfig {
            enabled: Vec::new(),
            limits: dal_core::PluginLimits::default(),
            configs: BTreeMap::new(),
        },
    )
}

fn temp_data() -> std::io::Result<tempfile::TempDir> {
    tempfile::tempdir()
}

#[test]
fn minimal_plugin_loads_one_tool() -> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(
        data.path(),
        "focus",
        include_str!("fixtures/star/focus/plugin.star"),
    )?;
    let generation = load_all(data.path()).expect("minimal load");
    assert_eq!(generation.len(), 1);
    let dump = generation.registrations();
    assert!(dump.contains("plugin focus/0.1.0,user"), "{dump}");
    assert!(dump.contains("tools focus/focus,model,uses="), "{dump}");
    Ok(())
}

#[test]
fn declared_uses_fold_into_the_inject_manifest() -> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(
        data.path(),
        "probe",
        r#"load("@dal/v1", "dal")

def probe(ctx, args):
    return args

plugin = dal.plugin(
    name = "probe",
    version = "0.1.0",
    tools = {
        "probe": dal.tool(
            description = "Probe.",
            input = dal.schema(value = dal.optional(dal.string())),
            run = probe,
            uses = ["env.read", "ask.select"],
        ),
    },
)
"#,
    )?;
    let generation = load_all(data.path())?;
    let system = dal_star::PluginSystem::new(
        generation,
        LoadRoots {
            data_root: data.path().to_path_buf(),
            bundled: Vec::new(),
        },
        PluginsConfig {
            enabled: Vec::new(),
            limits: dal_core::PluginLimits::default(),
            configs: BTreeMap::new(),
        },
    );
    let probe = system
        .extensions()?
        .into_iter()
        .find(|extension| extension.name() == "probe")
        .expect("probe extension");
    let inject = probe.inject();
    assert!(
        inject.contains(dal_core::Service::Env),
        "env.read injects env"
    );
    assert!(
        inject.contains(dal_core::Service::Ask),
        "ask.select injects ask"
    );
    assert!(
        !inject.contains(dal_core::Service::Run),
        "an undeclared operation never injects its service"
    );
    Ok(())
}

#[test]
fn invalid_tool_name_fails_with_exact_grammar_text() -> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(
        data.path(),
        "focus",
        include_str!("fixtures/star/focus-negative/plugin.star"),
    )?;
    let error = load_all(data.path()).expect_err("bad tool name must fail");
    assert!(
        error
            .render()
            .contains("tool name \"BadName\" is invalid; names must match [a-z][a-z0-9_-]{0,63}"),
        "{}",
        error.render()
    );
    Ok(())
}

#[test]
fn directory_name_mismatch_reports_typed_verdict() -> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(
        data.path(),
        "wrong",
        r#"load("@dal/v1", "dal")

plugin = dal.plugin(name = "right", version = "0.1.0")
"#,
    )?;
    let error = load_all(data.path()).expect_err("mismatch loads");
    assert!(
        matches!(error, dal_star::LoadError::NameMismatch { .. }),
        "{}",
        error.render()
    );
    assert_eq!(
        error.render(),
        "plugin directory \"wrong\" declares name \"right\""
    );
    Ok(())
}

#[test]
fn missing_plugin_export_reports_entry_path() -> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(data.path(), "empty", "x = 1\n")?;
    let error = load_all(data.path()).expect_err("empty plugin exports no descriptor");
    assert!(
        matches!(error, dal_star::LoadError::UnsupportedEntry { .. }),
        "{}",
        error.render()
    );
    assert!(error.render().contains("plugin.star"), "{}", error.render());
    assert!(
        error.render().contains("does not export a `plugin` value"),
        "{}",
        error.render()
    );
    Ok(())
}

#[test]
fn unknown_plugin_injection_service_is_rejected_at_its_declaration_site()
-> Result<(), Box<dyn std::error::Error>> {
    let source = r#"load("@dal/v1", "dal")
plugin = dal.plugin(name = "badinject", version = "0.1.0", inject = ["future"])
"#;
    let data = temp_data()?;
    write_plugin(data.path(), "badinject", source)?;
    let error = load_all(data.path()).expect_err("unknown service must fail validation");
    let expected_line = u32::try_from(
        source
            .lines()
            .position(|line| line.starts_with("plugin = dal.plugin"))
            .expect("plugin declaration line")
            + 1,
    )?;
    let dal_star::LoadError::Registration {
        path,
        line,
        message,
        ..
    } = error
    else {
        panic!("expected a source-located injection error: {error}");
    };
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("plugin.star")
    );
    assert_eq!(line, expected_line);
    assert!(message.contains("unknown service"), "{message}");
    Ok(())
}

#[test]
fn invalid_model_capability_names_its_declaration_site() -> Result<(), Box<dyn std::error::Error>> {
    let source = r#"load("@dal/v1", "dal")
def infer(ctx, request):
    return None

caps = {
    "context_window": 8192,
    "thinking": ["off"],
    "tool_use": True,
    "image_input": False,
    "future_field": True,
}
model = dal.model(id = "dalgona/fusion", caps = caps, run = infer)
plugin = dal.plugin(name = "invalidmodel", version = "0.1.0", models = {"fusion": model})
"#;
    let data = temp_data()?;
    write_plugin(data.path(), "invalidmodel", source)?;
    let error = load_all(data.path()).expect_err("unknown model cap is rejected");
    let expected_line = u32::try_from(
        source
            .lines()
            .position(|line| line.starts_with("model = dal.model"))
            .expect("model declaration line")
            + 1,
    )?;
    let dal_star::LoadError::Registration {
        path,
        line,
        col,
        message,
    } = error
    else {
        panic!("expected a source-located declaration error: {error}");
    };
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("plugin.star")
    );
    assert_eq!(line, expected_line);
    assert!(col > 1, "column should point at the dal.model call: {col}");
    assert!(
        message.contains("unknown field `future_field`"),
        "{message}"
    );
    Ok(())
}

#[test]
fn system_snapshot_serves_the_loaded_generation() -> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(
        data.path(),
        "focus",
        include_str!("fixtures/star/focus/plugin.star"),
    )?;
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let cfg = PluginsConfig {
        enabled: Vec::new(),
        limits: dal_core::PluginLimits::default(),
        configs: BTreeMap::new(),
    };
    let generation = load(&roots, &cfg).expect("minimal load");
    let id = generation.id;
    let system = dal_star::PluginSystem::new(generation, roots, cfg);
    let snapshot = system.snapshot().expect("generation snapshot");
    assert_eq!(snapshot.id, id);
    assert_eq!(snapshot.len(), 1);
    Ok(())
}

#[test]
fn bundled_source_loads_before_user_plugins() {
    let generation = load(
        &LoadRoots {
            data_root: PathBuf::from("/nonexistent"),
            bundled: vec![dal_star::BundledPlugin {
                name: "focus".to_owned(),
                files: [(
                    PathBuf::from("plugin.star"),
                    include_str!("fixtures/star/focus/plugin.star").as_bytes(),
                )]
                .into_iter()
                .collect(),
            }],
        },
        &PluginsConfig {
            enabled: Vec::new(),
            limits: dal_core::PluginLimits::default(),
            configs: BTreeMap::new(),
        },
    )
    .expect("bundled loads");
    assert_eq!(generation.len(), 1);
    assert!(
        generation
            .registrations()
            .contains("plugin focus/0.1.0,bundled"),
        "{}",
        generation.registrations()
    );
}

#[test]
#[cfg(unix)]
fn symlinked_plugin_dir_loads() -> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    let target = data.path().join("real-focus");
    std::fs::create_dir_all(&target).expect("target dir");
    std::fs::write(
        target.join("plugin.star"),
        include_str!("fixtures/star/focus/plugin.star"),
    )
    .expect("fixture entry");
    std::fs::create_dir_all(data.path().join("plugins")).expect("plugins dir");
    std::os::unix::fs::symlink(&target, data.path().join("plugins").join("focus"))
        .expect("symlink");
    let generation = load_all(data.path()).expect("symlinked load");
    assert_eq!(generation.len(), 1);
    Ok(())
}

fn write_skill_plugin(data: &Path, skill_md: &str) -> std::io::Result<()> {
    let entry = "load(\"@dal/v1\", \"dal\")\n\ndocs = dal.skill(description = \"Docs helper\", path = \"SKILL.md\")\n\nplugin = dal.plugin(\n    name = \"docs\",\n    version = \"0.1.0\",\n    inject = [\"mcp\"],\n    skills = {\"docs\": docs},\n)\n";
    write_plugin(data, "docs", entry)?;
    std::fs::write(data.join("plugins/docs/SKILL.md"), skill_md)
}

#[test]
fn skill_front_matter_mcp_reaches_the_extension_record() -> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_skill_plugin(
        data.path(),
        "---\nmcp:\n  servers:\n    web:\n      url: https://docs.example/mcp\n---\n# Docs\n",
    )?;
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let config = PluginsConfig {
        enabled: Vec::new(),
        limits: dal_core::PluginLimits::default(),
        configs: BTreeMap::new(),
    };
    let generation = load(&roots, &config)?;
    let extensions = dal_star::PluginSystem::new(generation, roots, config).extensions()?;
    let skills = extensions[0].skills();
    assert_eq!(skills.len(), 1);
    let block = skills[0]
        .mcp
        .as_ref()
        .expect("the skill declares one server");
    assert_eq!(block.servers.len(), 1);
    assert!(block.servers.contains_key("web"));
    Ok(())
}

#[test]
fn skill_front_matter_unknown_key_is_a_load_error_at_file_line_col()
-> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_skill_plugin(
        data.path(),
        "---\nmcp:\n  servers:\n    web:\n      url: https://docs.example/mcp\n      retries: 3\n---\n",
    )?;
    let error = load_all(data.path()).expect_err("an unknown server key must fail the load");
    let skill_path = data.path().join("plugins").join("docs").join("SKILL.md");
    assert_eq!(
        error.render(),
        format!(
            "{}:6:7: unknown key \"retries\"; expected command, env, url",
            skill_path.display()
        )
    );
    Ok(())
}
