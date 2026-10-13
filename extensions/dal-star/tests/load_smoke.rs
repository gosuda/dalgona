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
    // The whole manifest line, not just the count: wire kind, plugin and
    // local names, visibility, and the (empty) uses set.
    assert_eq!(
        generation.registrations(),
        "plugin focus/0.1.0,user\ntools focus/focus,model,uses="
    );
    // The converted extension serves the wire name the manifest declares.
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
    let extensions = system.extensions().expect("focus converts");
    assert_eq!(extensions.len(), 1);
    let names: Vec<String> = extensions[0]
        .tools()
        .iter()
        .map(|(tool, _)| tool.name().to_string())
        .collect();
    assert_eq!(names, ["focus__focus".to_owned()]);
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

probe_cmd_tool = dal.tool(
    description = "Probe command tool.",
    input = dal.schema(value = dal.optional(dal.string())),
    run = probe,
    uses = ["jobs.list"],
)

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
    commands = {
        "probe_cmd": dal.command(tool = probe_cmd_tool),
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
        inject.contains(dal_core::Service::Jobs),
        "a command-only tool's uses injects its service"
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

/// The focus fixture with its tool published under a second local name: a
/// reload after this rewrite must serve a fresh generation id and the new
/// wire name.
const FOCUS_SECOND_TOOL: &str = "load(\"@dal/v1\", \"dal\")\n\ndef focus(ctx, args):\n    return args\n\nfocus_tool = dal.tool(\n    description = \"Return the supplied focus data.\",\n    input = dal.schema(value = dal.optional(dal.string())),\n    run = focus,\n)\n\nplugin = dal.plugin(\n    name = \"focus\",\n    version = \"0.1.0\",\n    tools = {\"second\": focus_tool},\n)\n";

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
    let first = generation.id;
    let system = dal_star::PluginSystem::new(generation, roots, cfg);
    let snapshot = system.snapshot().expect("generation snapshot");
    assert_eq!(snapshot.id, first);
    assert_eq!(snapshot.len(), 1);
    // A rewrite followed by reload must move the served snapshot: a stuck
    // snapshot that keeps the first generation stays green nowhere here.
    write_plugin(data.path(), "focus", FOCUS_SECOND_TOOL)?;
    system.reload().expect("rewrite reloads");
    let next = system.snapshot().expect("reloaded snapshot");
    assert_ne!(next.id, first, "reload publishes a fresh generation");
    let dump = next.registrations();
    assert!(dump.contains("tools focus/second,"), "{dump}");
    assert!(!dump.contains("tools focus/focus,"), "{dump}");
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
    let diagnostic = error.render();
    assert!(
        diagnostic.starts_with(&format!("{}:6:7:", skill_path.display())),
        "the invalid key must retain its file and position: {diagnostic}"
    );
    assert!(
        diagnostic.contains("unknown key \"retries\""),
        "{diagnostic}"
    );
    Ok(())
}

#[test]
fn direct_markdown_rule_files_reach_the_extension_with_exact_source_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(
        data.path(),
        "focus",
        include_str!("fixtures/star/focus/plugin.star"),
    )?;
    let directory = data.path().join("plugins/focus");
    for (path, bytes) in [
        ("rules/zeta.MD", b"\xef\xbb\xbfZeta.\r\n".as_slice()),
        ("rules/alpha.md", b"Alpha.\n".as_slice()),
        ("rules/.hidden.md", b"\xff\0Hidden.".as_slice()),
        ("rules/nested/child.md", b"Nested.".as_slice()),
        ("assets/rules/foreign.md", b"Unrelated.".as_slice()),
        ("Rules/wrong-case.md", b"Wrong directory.".as_slice()),
        ("rules/not-markdown.txt", b"Text.".as_slice()),
        ("notes.md", b"Root markdown.".as_slice()),
    ] {
        let path = directory.join(path);
        std::fs::create_dir_all(path.parent().expect("fixture parent"))?;
        std::fs::write(path, bytes)?;
    }
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let config = PluginsConfig::default();
    let generation = load(&roots, &config)?;
    let extensions = dal_star::PluginSystem::new(generation, roots, config).extensions()?;
    let expected: Vec<dal_core::RuleFile> = [
        ("rules/.hidden.md", b"\xff\0Hidden.".as_slice()),
        ("rules/alpha.md", b"Alpha.\n".as_slice()),
        ("rules/zeta.MD", b"\xef\xbb\xbfZeta.\r\n".as_slice()),
    ]
    .into_iter()
    .map(|(path, bytes)| dal_core::RuleFile {
        plugin: dal_core::Name::parse("focus").expect("plugin name"),
        origin: dal_core::Origin::User,
        path: directory.join(path).to_string_lossy().into_owned().into(),
        bytes: std::sync::Arc::from(bytes),
    })
    .collect();
    assert_eq!(extensions[0].rule_files(), expected.as_slice());
    assert_eq!(extensions[0].tools().len(), 1);
    Ok(())
}

#[test]
fn bundled_rule_files_keep_their_bundled_owner_and_display_path()
-> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: vec![dal_star::BundledPlugin {
            name: "focus".to_owned(),
            files: BTreeMap::from([
                (
                    PathBuf::from("plugin.star"),
                    include_bytes!("fixtures/star/focus/plugin.star").as_slice(),
                ),
                (PathBuf::from("rules/one.mD"), b"Bundled.\n".as_slice()),
                (
                    PathBuf::from("docs/rules/other.md"),
                    b"Unrelated.".as_slice(),
                ),
                (PathBuf::from("rules/sub/other.md"), b"Nested.".as_slice()),
            ]),
        }],
    };
    let config = PluginsConfig::default();
    let generation = load(&roots, &config)?;
    let extensions = dal_star::PluginSystem::new(generation, roots, config).extensions()?;
    assert_eq!(
        extensions[0].rule_files(),
        &[dal_core::RuleFile {
            plugin: dal_core::Name::parse("focus")?,
            origin: dal_core::Origin::Bundled,
            path: PathBuf::from("<bundled>/focus")
                .join("rules/one.mD")
                .to_string_lossy()
                .into_owned()
                .into(),
            bytes: std::sync::Arc::from(b"Bundled.\n".as_slice()),
        }]
    );
    Ok(())
}

#[test]
fn retained_rule_files_follow_enabled_plugins_and_user_shadowing()
-> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(
        data.path(),
        "focus",
        include_str!("fixtures/star/focus/plugin.star"),
    )?;
    write_plugin(
        data.path(),
        "ignored",
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"ignored\", version = \"0.1.0\")\n",
    )?;
    let user_rules = data.path().join("plugins/focus/rules");
    std::fs::create_dir_all(&user_rules)?;
    std::fs::write(user_rules.join("user.md"), b"User.")?;
    let ignored_rules = data.path().join("plugins/ignored/rules");
    std::fs::create_dir_all(&ignored_rules)?;
    std::fs::write(ignored_rules.join("ignored.md"), b"Disabled.")?;
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: vec![dal_star::BundledPlugin {
            name: "focus".to_owned(),
            files: BTreeMap::from([
                (
                    PathBuf::from("plugin.star"),
                    include_bytes!("fixtures/star/focus/plugin.star").as_slice(),
                ),
                (PathBuf::from("rules/bundled.md"), b"Shadowed.".as_slice()),
            ]),
        }],
    };
    let config = PluginsConfig {
        enabled: vec!["focus".to_owned()],
        ..PluginsConfig::default()
    };
    let generation = load(&roots, &config)?;
    let extensions = dal_star::PluginSystem::new(generation, roots, config).extensions()?;
    assert_eq!(extensions.len(), 1);
    assert_eq!(
        extensions[0].rule_files(),
        &[dal_core::RuleFile {
            plugin: dal_core::Name::parse("focus")?,
            origin: dal_core::Origin::User,
            path: user_rules
                .join("user.md")
                .to_string_lossy()
                .into_owned()
                .into(),
            bytes: std::sync::Arc::from(b"User.".as_slice()),
        }]
    );
    Ok(())
}

#[test]
fn rule_file_retention_leaves_ttsr_limit_diagnostics_to_the_rule_loader()
-> Result<(), Box<dyn std::error::Error>> {
    let data = temp_data()?;
    write_plugin(
        data.path(),
        "focus",
        include_str!("fixtures/star/focus/plugin.star"),
    )?;
    let directory = data.path().join("plugins/focus/rules");
    std::fs::create_dir_all(&directory)?;
    for index in 0..257 {
        std::fs::write(directory.join(format!("r{index:03}.md")), b"Rule.")?;
    }
    let oversized = vec![b'x'; 65_537];
    std::fs::write(directory.join("r000.md"), &oversized)?;
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let config = PluginsConfig::default();
    let generation = load(&roots, &config)?;
    let extensions = dal_star::PluginSystem::new(generation, roots, config).extensions()?;
    let files = extensions[0].rule_files();
    assert_eq!(files.len(), 257);
    assert_eq!(files[0].bytes.as_ref(), oversized.as_slice());
    assert_eq!(
        files[256].path.as_ref(),
        directory.join("r256.md").to_string_lossy().as_ref()
    );
    Ok(())
}
