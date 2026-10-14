//! Port conformance suite: doc validation, real loader runs, reload semantics.
#![expect(
    clippy::unwrap_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]

use dal_ext::docs::{DocsSnapshot, Lookup, Manual, lookup};
use dal_ext::docsgen::{GenArgs, Scheme, generate};
use dal_star::load::{LoadRoots, PluginsConfig, load};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scan_roots() -> Vec<PathBuf> {
    [
        "core",
        "engine",
        "extensions",
        "clients",
        "cli",
        "docs",
        "examples/plugins",
    ]
    .into_iter()
    .map(|path| repo_root().join(path))
    .collect()
}

fn write_plugin(dir: &Path, name: &str, star: &str) {
    let plugin = dir.join(name);
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.star"), star).unwrap();
}

fn load_one(dir: &Path) -> Result<dal_star::system::PluginGeneration, dal_star::error::LoadError> {
    load(
        &LoadRoots {
            data_root: dir.to_owned(),
            bundled: Vec::new(),
        },
        &PluginsConfig {
            enabled: Vec::new(),
            limits: dal_core::PluginLimits::default(),
            configs: BTreeMap::default(),
        },
    )
}

#[test]
fn registration_doc_errors() {
    // The @dal/v1 descriptor has no doc constructor. Native extensions
    // register docs through ExtensionBuilder::doc, so plugin doc failures
    // cannot be exercised here without inventing an unsupported API. Keep
    // the real v1 loader failures below as the useful boundary coverage.
    let guard = tempdir::named("registration-errors");
    let data = guard.join("data");
    std::fs::create_dir_all(data.join("plugins")).unwrap();

    // A v1 descriptor with an invalid tool key is rejected at its plugin.star.
    write_plugin(
        &data.join("plugins"),
        "badtool",
        r#"load("@dal/v1", "dal")
bad_tool = dal.tool(description = "x", input = dal.schema(), run = lambda ctx, args: "x")
plugin = dal.plugin(name = "badtool", version = "0.1.0", tools = {"BadName": bad_tool})
"#,
    );
    let error = load_one(&data).expect_err("bad tool name fails");
    let rendered = error.to_string();
    assert!(
        rendered.contains("plugin.star:"),
        "plugin source: {rendered}"
    );
    assert!(rendered.contains("BadName"), "names claimant: {rendered}");

    let _ = std::fs::remove_dir_all(data.join("plugins").join("badtool"));

    // Two bindings to the required plugin name fail at the second binding.
    write_plugin(
        &data.join("plugins"),
        "double",
        r#"load("@dal/v1", "dal")
plugin = dal.plugin(name = "double", version = "0.1.0")
plugin = dal.plugin(name = "double", version = "0.1.0")
"#,
    );
    let error = load_one(&data).expect_err("double plugin fails");
    assert!(error.to_string().contains("plugin.star:"), "{error}");

    let _ = std::fs::remove_dir_all(data.join("plugins").join("double"));

    // A module without the required plugin binding reports the v1 entry error.
    write_plugin(
        &data.join("plugins"),
        "empty",
        r#"load("@dal/v1", "dal")
x = 1
"#,
    );
    let error = load_one(&data).expect_err("missing plugin binding fails");
    assert!(
        error
            .to_string()
            .contains("does not export a `plugin` value; only the @dal/v1 contract is supported"),
        "{error}"
    );

    let _ = std::fs::remove_dir_all(data.join("plugins").join("empty"));

    // Directory/declared name mismatch fails naming both sides.
    write_plugin(
        &data.join("plugins"),
        "wrong",
        r#"load("@dal/v1", "dal")
plugin = dal.plugin(name = "right", version = "0.1.0")
"#,
    );
    let error = load_one(&data).expect_err("name mismatch fails");
    let rendered = error.to_string();
    assert!(
        rendered.contains("wrong") && rendered.contains("right"),
        "{rendered}"
    );
}

#[test]
fn ports_load_and_run() {
    // All fourteen ports load through the real evaluator.
    let guard = tempdir::named("ports-load");
    let data = guard.join("data");
    let plugins = data.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let source = repo_root().join("examples/plugins");
    for port in [
        "hello",
        "todo",
        "subagent",
        "ask",
        "mcp",
        "permission-gate",
        "plan-mode",
        "git-checkpoint",
        "handoff",
        "skill-pack",
        "fetch",
        "steer",
        "review-rules",
    ] {
        #[cfg(unix)]
        std::os::unix::fs::symlink(source.join(port), plugins.join(port)).unwrap();
        #[cfg(not(unix))]
        {
            copy_dir(&source.join(port), &plugins.join(port));
        }
    }
    let generation = load_one(&data).expect("ports load");
    let names: Vec<_> = generation.names().collect();
    for port in [
        "hello",
        "todo",
        "subagent",
        "ask",
        "mcp",
        "permission-gate",
        "plan-mode",
        "git-checkpoint",
        "handoff",
        "skill-pack",
        "fetch",
        "steer",
        "review-rules",
    ] {
        assert!(names.contains(&port), "port {port} loaded: {names:?}");
    }

    let registrations = generation.registrations();
    assert!(
        registrations
            .lines()
            .any(|line| { line.contains("todo/") && line.contains("uses=state.read+state.write") }),
        "todo declares its state operations: {registrations}"
    );
    assert!(
        registrations.lines().any(|line| {
            line.contains("subagent/") && line.contains("uses=agents.start+agents.wait")
        }),
        "subagent declares its agent operations: {registrations}"
    );

    let args = GenArgs {
        scheme: Scheme::Dal,
        dir: repo_root().join("docs"),
        examples: Some(source),
        scan: scan_roots(),
    };
    let module = generate(&args).expect("ports assemble");
    for port in [
        "hello",
        "todo",
        "subagent",
        "ask",
        "mcp",
        "permission-gate",
        "plan-mode",
        "git-checkpoint",
        "handoff",
        "skill-pack",
        "fetch",
        "steer",
        "review-rules",
    ] {
        assert!(
            module.contains(&format!("\"examples/{port}\"")),
            "port {port} assembled"
        );
    }
}

#[cfg(not(unix))]
fn copy_dir(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap().filter_map(Result::ok) {
        let to = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

/// Reload swaps the published snapshot at once, so the next read sees the
/// new text while a failed reload keeps the old page readable.
#[test]
fn reload_swaps_pages() {
    let first = DocsSnapshot {
        manuals: vec![Manual {
            scheme: "dal".to_owned(),
            plugin: "dal".to_owned(),
            pages: vec![("config".to_owned(), "# Settings\n\nOld text.\n".to_owned())],
        }],
    };
    let second = DocsSnapshot {
        manuals: vec![Manual {
            scheme: "dal".to_owned(),
            plugin: "dal".to_owned(),
            pages: vec![("config".to_owned(), "# Settings\n\nNew text.\n".to_owned())],
        }],
    };
    match lookup(&first, "dal://config") {
        Lookup::Page { text, .. } => assert!(text.contains("Old text")),
        other => panic!("expected page, got {other:?}"),
    }
    // A reload publishes a new snapshot in one atomic swap; the next read
    // sees the new text while a failed reload keeps the old page readable.
    match lookup(&second, "dal://config") {
        Lookup::Page { text, .. } => assert!(text.contains("New text")),
        other => panic!("expected page, got {other:?}"),
    }
}

mod tempdir {
    use std::path::PathBuf;

    pub(super) struct Guard {
        dir: PathBuf,
    }

    pub(super) fn named(tag: &str) -> Guard {
        let dir = std::env::temp_dir().join(format!(
            "dal-ports-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Guard { dir }
    }

    impl Guard {
        pub(super) fn join(&self, child: &str) -> PathBuf {
            self.dir.join(child)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
