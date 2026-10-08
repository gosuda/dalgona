//! Adversarial violation tests for the plugin load pipeline: every malformed
//! plugin must fail with a typed, source-located `LoadError`, never load
//! partially and never panic.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dal_core::PluginLimits;
use dal_star::{BundledPlugin, LoadError, LoadRoots, PluginGeneration, PluginsConfig, load};

const HEADER: &str = "load(\"@dal/v1\", \"dal\")\n";

fn roots(data: &Path) -> LoadRoots {
    LoadRoots {
        data_root: data.to_path_buf(),
        bundled: Vec::new(),
    }
}

fn config() -> PluginsConfig {
    PluginsConfig {
        enabled: Vec::new(),
        limits: PluginLimits::default(),
        configs: BTreeMap::new(),
    }
}

fn write(data: &Path, name: &str, file: &str, bytes: impl AsRef<[u8]>) {
    let dir = data.join("plugins").join(name);
    let target = dir.join(file);
    std::fs::create_dir_all(target.parent().expect("file has a parent")).expect("plugin dir");
    std::fs::write(target, bytes).expect("plugin file");
}

fn plugin_source(name: &str) -> String {
    format!("{HEADER}plugin = dal.plugin(name = \"{name}\", version = \"0.1.0\")\n")
}

fn load_with(
    files: &[(&str, &str, &[u8])],
    cfg: &PluginsConfig,
) -> (tempfile::TempDir, Result<PluginGeneration, LoadError>) {
    let data = tempfile::tempdir().expect("temporary data root");
    for (name, file, bytes) in files {
        write(data.path(), name, file, bytes);
    }
    let result = load(&roots(data.path()), cfg);
    (data, result)
}

fn load_source(name: &str, source: &str) -> Result<PluginGeneration, LoadError> {
    load_with(&[(name, "plugin.star", source.as_bytes())], &config()).1
}

fn expect_eval(name: &str, source: &str, needle: &str) -> u32 {
    let error = load_source(name, source).expect_err("source must be rejected");
    let LoadError::Eval {
        line,
        message,
        path,
        ..
    } = &error
    else {
        panic!("expected a located evaluation error, got {error:?}");
    };
    assert!(
        message.contains(needle),
        "message lacks `{needle}`: {message}"
    );
    assert!(
        path.ends_with("plugin.star"),
        "error must name the source file: {}",
        path.display()
    );
    *line
}

// --- malformed source -------------------------------------------------------

#[test]
fn syntax_error_reports_file_and_line() {
    let line = expect_eval(
        "broken",
        &format!("{HEADER}\nplugin = dal.plugin(name = \n"),
        "",
    );
    assert!(line >= 1);
}

#[test]
fn undefined_name_at_top_level_is_an_evaluation_error() {
    expect_eval(
        "undef",
        &format!("{HEADER}x = missing_name\n"),
        "missing_name",
    );
}

#[test]
fn plugin_bound_to_a_non_descriptor_is_invalid() {
    let error = load_source("scalar", &format!("{HEADER}plugin = 1\n")).expect_err("not a plugin");
    assert!(
        matches!(error, LoadError::InvalidPlugin { .. }),
        "{error:?}"
    );
}

#[test]
fn rebinding_plugin_is_rejected_at_the_second_binding() {
    let source = format!(
        "{HEADER}plugin = dal.plugin(name = \"twice\", version = \"0.1.0\")\nplugin = dal.plugin(name = \"twice\", version = \"0.2.0\")\n"
    );
    let error = load_source("twice", &source).expect_err("rebound plugin");
    let LoadError::PluginRebound { line, .. } = error else {
        panic!("expected PluginRebound, got {error:?}");
    };
    assert_eq!(line, 3);
}

#[test]
fn entry_that_never_loads_the_sdk_is_unsupported() {
    let error = load_source("nosdk", "plugin = 1\n").expect_err("no sdk load");
    assert!(
        matches!(error, LoadError::UnsupportedEntry { .. }),
        "{error:?}"
    );
}

#[test]
fn entry_loading_the_sdk_twice_is_unsupported() {
    let source = format!(
        "{HEADER}load(\"@dal/v1\", d2 = \"dal\")\n{}",
        &plugin_source("dup")[HEADER.len()..]
    );
    let error = load_source("dup", &source).expect_err("two sdk loads");
    assert!(
        matches!(error, LoadError::UnsupportedEntry { .. }),
        "{error:?}"
    );
}

#[test]
fn unknown_virtual_module_labels_are_sdk_module_errors() {
    for label in ["@dal/v2", "@dal", "@other/x"] {
        let source = format!("load(\"{label}\", \"dal\")\nplugin = 1\n");
        let error = load_source("labels", &source).expect_err("unknown label");
        assert!(
            matches!(
                error,
                LoadError::SdkModule { .. } | LoadError::UnsupportedEntry { .. }
            ),
            "{label}: {error:?}"
        );
    }
}

#[test]
fn invalid_utf8_entry_is_a_typed_error() {
    let (_data, result) = load_with(&[("binary", "plugin.star", &[0xff, 0xfe, 0x00])], &config());
    let error = result.expect_err("non-utf8 entry");
    assert!(matches!(error, LoadError::InvalidUtf8 { .. }), "{error:?}");
}

// --- module loading --------------------------------------------------------

fn entry_loading(first: &str) -> String {
    format!(
        "{HEADER}load(\"{first}\", \"v\")\nplugin = dal.plugin(name = \"chain\", version = \"0.1.0\")\n"
    )
}

#[test]
fn missing_relative_module_is_a_typed_error() {
    let error = load_source("chain", &entry_loading("absent.star")).expect_err("absent module");
    let LoadError::MissingModule { module } = error else {
        panic!("expected MissingModule, got {error:?}");
    };
    assert_eq!(module.as_ref(), "absent.star");
}

#[test]
fn modules_escaping_the_plugin_root_are_refused() {
    for target in ["../secret.star", "/etc/passwd", "a\\\\b.star", "./x.star"] {
        let error = load_source("chain", &entry_loading(target)).expect_err("escape");
        assert!(
            matches!(error, LoadError::LoadEscape { .. }),
            "{target}: {error:?}"
        );
    }
}

#[test]
fn load_cycle_is_detected_not_recursed() {
    let (_data, result) = load_with(
        &[
            ("chain", "plugin.star", entry_loading("a.star").as_bytes()),
            ("chain", "a.star", b"load(\"b.star\", \"v\")\nv = 1\n"),
            ("chain", "b.star", b"load(\"a.star\", \"v\")\nv = 1\n"),
        ],
        &config(),
    );
    let error = result.expect_err("cycle");
    assert!(matches!(error, LoadError::LoadCycle { .. }), "{error:?}");
}

/// A plugin whose entry reaches `m1.star` → … → `m{depth}.star`.
fn chain_of(depth: usize) -> (tempfile::TempDir, Result<PluginGeneration, LoadError>) {
    let entry = entry_loading("m1.star");
    let mut modules: Vec<(String, String)> = Vec::new();
    for index in 1..=depth {
        let body = if index == depth {
            "v = 1\n".to_owned()
        } else {
            format!("load(\"m{}.star\", \"v\")\n", index + 1)
        };
        modules.push((format!("m{index}.star"), body));
    }
    let mut files: Vec<(&str, &str, &[u8])> = vec![("chain", "plugin.star", entry.as_bytes())];
    for (file, body) in &modules {
        files.push(("chain", file.as_str(), body.as_bytes()));
    }
    load_with(&files, &config())
}

#[test]
fn module_chain_depth_boundary_is_eight() {
    for depth in [7, 8] {
        let (_data, result) = chain_of(depth);
        assert!(result.is_ok(), "depth {depth}: {:?}", result.err());
    }
    let (_data, result) = chain_of(9);
    let error = result.expect_err("depth 9 exceeds the load depth cap");
    assert!(matches!(error, LoadError::LoadDepth { .. }), "{error:?}");
}

#[test]
fn nested_modules_may_not_load_other_virtual_labels() {
    let (_data, result) = load_with(
        &[
            ("chain", "plugin.star", entry_loading("a.star").as_bytes()),
            ("chain", "a.star", b"load(\"@dal/v2\", \"x\")\nv = 1\n"),
        ],
        &config(),
    );
    let error = result.expect_err("nested virtual label");
    assert!(matches!(error, LoadError::SdkModule { .. }), "{error:?}");
}

// --- size and budget boundaries -------------------------------------------

const MAX_CODE: usize = 256 << 10;

fn padded_entry(total: usize) -> Vec<u8> {
    let mut source = plugin_source("sized").into_bytes();
    source.extend_from_slice(b"#");
    source.resize(total, b'a');
    source
}

#[test]
fn entry_file_size_boundary_is_max_code() {
    for total in [MAX_CODE - 1, MAX_CODE] {
        let (_data, result) =
            load_with(&[("sized", "plugin.star", &padded_entry(total))], &config());
        assert!(result.is_ok(), "{total} bytes: {:?}", result.err());
    }
    let (_data, result) = load_with(
        &[("sized", "plugin.star", &padded_entry(MAX_CODE + 1))],
        &config(),
    );
    let error = result.expect_err("one byte over the cap");
    let LoadError::Eval { message, .. } = &error else {
        panic!("expected Eval, got {error:?}");
    };
    assert!(message.contains("exceeds 262144 bytes"), "{message}");
}

#[test]
fn oversized_sibling_file_fails_the_whole_plugin() {
    let big = vec![b'x'; MAX_CODE + 1];
    let (_data, result) = load_with(
        &[
            ("sized", "plugin.star", plugin_source("sized").as_bytes()),
            ("sized", "notes.txt", &big),
        ],
        &config(),
    );
    assert!(
        matches!(result, Err(LoadError::Eval { .. })),
        "{:?}",
        result.err()
    );
}

static OVERSIZED: [u8; MAX_CODE + 1] = [b'#'; MAX_CODE + 1];

#[test]
fn bundled_file_over_the_cap_is_refused() {
    let big: &'static [u8] = &OVERSIZED;
    let data = tempfile::tempdir().expect("data root");
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: vec![BundledPlugin {
            name: "big".to_owned(),
            files: BTreeMap::from([(PathBuf::from("plugin.star"), big)]),
        }],
    };
    let result = load(&roots, &config());
    assert!(
        matches!(result, Err(LoadError::Eval { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn bundled_plugin_without_entry_file_is_refused() {
    let data = tempfile::tempdir().expect("data root");
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: vec![BundledPlugin {
            name: "empty".to_owned(),
            files: BTreeMap::new(),
        }],
    };
    let result = load(&roots, &config());
    assert!(
        matches!(result, Err(LoadError::MissingEntryFile { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn load_tick_budget_stops_a_spinning_module() {
    let mut cfg = config();
    cfg.limits = PluginLimits {
        load_ticks: 1_000,
        ..PluginLimits::default()
    };
    let source = format!(
        "{HEADER}for _ in range(1000000):\n    pass\n{}",
        &plugin_source("spin")[HEADER.len()..]
    );
    let (_data, result) = load_with(&[("spin", "plugin.star", source.as_bytes())], &cfg);
    assert!(
        matches!(result, Err(LoadError::Eval { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn load_call_depth_budget_stops_runaway_recursion() {
    let mut cfg = config();
    cfg.limits = PluginLimits {
        stack_depth: 10,
        ..PluginLimits::default()
    };
    let source = format!(
        "{HEADER}def f(n):\n    return f(n + 1)\nx = f(0)\n{}",
        &plugin_source("deep")[HEADER.len()..]
    );
    let (_data, result) = load_with(&[("deep", "plugin.star", source.as_bytes())], &cfg);
    assert!(
        matches!(result, Err(LoadError::Eval { .. })),
        "{:?}",
        result.err()
    );
}

// --- directory scan --------------------------------------------------------

#[test]
fn directory_name_length_boundary_is_sixty_four() {
    let ok = "a".repeat(64);
    let (_data, result) = load_with(
        &[(&ok, "plugin.star", plugin_source(&ok).as_bytes())],
        &config(),
    );
    assert!(result.is_ok(), "{:?}", result.err());

    let too_long = "a".repeat(65);
    let (_data, result) = load_with(
        &[(
            &too_long,
            "plugin.star",
            plugin_source(&too_long).as_bytes(),
        )],
        &config(),
    );
    assert!(
        matches!(result, Err(LoadError::InvalidDirectoryName { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn malformed_directory_names_are_refused() {
    for name in ["Upper", "9lives", ".hidden", "has space", "under_score!"] {
        let (_data, result) = load_with(&[(name, "plugin.star", b"")], &config());
        assert!(
            matches!(result, Err(LoadError::InvalidDirectoryName { .. })),
            "{name}: {:?}",
            result.err()
        );
    }
}

#[test]
fn plugin_directory_without_entry_is_refused() {
    let (_data, result) = load_with(&[("lonely", "notes.txt", b"hi")], &config());
    assert!(
        matches!(result, Err(LoadError::MissingEntryFile { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn plugins_path_that_is_a_file_is_an_unreadable_error() {
    let data = tempfile::tempdir().expect("data root");
    std::fs::write(data.path().join("plugins"), b"not a directory").expect("blocker file");
    let result = load(&roots(data.path()), &config());
    let Err(LoadError::Eval { message, .. }) = result else {
        panic!("expected an unreadable error, got {:?}", result.err());
    };
    assert!(message.contains("unreadable"), "{message}");
}

#[test]
fn missing_data_root_is_an_empty_generation_not_an_error() {
    let generation = load(
        &roots(Path::new("/nonexistent/dal-star-adversarial")),
        &config(),
    )
    .expect("a missing plugins directory is not an error");
    assert!(generation.is_empty());
}

#[test]
fn first_bad_plugin_fails_the_whole_generation() {
    let (_data, result) = load_with(
        &[
            ("alpha", "plugin.star", plugin_source("alpha").as_bytes()),
            ("beta", "plugin.star", b"x ="),
        ],
        &config(),
    );
    assert!(
        result.is_err(),
        "a generation is published whole or not at all"
    );
}

#[test]
fn plugin_declaring_another_name_is_a_mismatch() {
    let (_data, result) = load_with(
        &[("alpha", "plugin.star", plugin_source("beta").as_bytes())],
        &config(),
    );
    assert!(
        matches!(result, Err(LoadError::NameMismatch { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn unlisted_user_plugin_is_skipped_and_listed_one_loads() {
    let mut cfg = config();
    cfg.enabled = vec!["alpha".to_owned()];
    let (_data, result) = load_with(
        &[
            ("alpha", "plugin.star", plugin_source("alpha").as_bytes()),
            ("beta", "plugin.star", b"not even starlark ((("),
        ],
        &cfg,
    );
    let generation = result.expect("the disabled plugin is never evaluated");
    assert_eq!(generation.names().collect::<Vec<_>>(), ["alpha"]);
}

// --- `enabled` names that match nothing ------------------------------------

/// Records the fields of every WARN event on the current thread.
struct WarnCapture(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        write!(self.0, "{}={value:?} ", field.name()).expect("string write is infallible");
    }
}

impl tracing::Subscriber for WarnCapture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().expect("warning log lock").push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn warnings_while_loading(enabled: &[&str]) -> Vec<String> {
    let mut cfg = config();
    cfg.enabled = enabled.iter().map(|name| (*name).to_owned()).collect();
    let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let data = tempfile::tempdir().expect("data root");
    write(data.path(), "alpha", "plugin.star", plugin_source("alpha"));
    let bundled_entry: &'static [u8] =
        b"load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"core\", version = \"0.1.0\")\n";
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: vec![BundledPlugin {
            name: "core".to_owned(),
            files: BTreeMap::from([(PathBuf::from("plugin.star"), bundled_entry)]),
        }],
    };
    tracing::subscriber::with_default(WarnCapture(std::sync::Arc::clone(&logs)), || {
        load(&roots, &cfg).expect("load succeeds");
    });
    logs.lock().expect("warning log lock").clone()
}

#[test]
fn misspelled_enabled_name_is_logged_as_a_warning() {
    let warnings = warnings_while_loading(&["alpha", "alhpa"]);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("alhpa"), "{warnings:?}");
}

#[test]
fn enabled_names_that_match_installed_or_bundled_plugins_do_not_warn() {
    assert!(warnings_while_loading(&["alpha", "core"]).is_empty());
    assert!(warnings_while_loading(&[]).is_empty());
}
