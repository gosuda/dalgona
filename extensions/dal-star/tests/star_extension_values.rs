//! Extension-value obligations at the Starlark boundary.
//!
//! Each test pins the boundary's validation behavior for values crossing
//! from Starlark scripts into `dal-core` extension values: exact text where
//! the boundary preserves the core text verbatim (inject errors), and
//! rejection plus semantic content where the boundary wraps it (name,
//! visibility, event texts — see report drifts D1–D4). Nothing here duplicates
//! `ext_grants_proof.rs`, `hooks.rs`, or `runtime*.rs`: those own grants,
//! runtime verdict dispatch, and eval-cell misuse.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]

use std::collections::BTreeMap;

use dal_agent::ext::Extension;
use dal_core::{PluginLimits, Visibility};
use dal_star::{LoadError, LoadRoots, PluginGeneration, PluginsConfig, load};

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
    (data, load(&roots, cfg))
}

/// Loads `body` as plugin `probe` and returns the rejection.
fn rejected(body: &str) -> LoadError {
    let source = format!("{HEADER}{body}");
    load_named("probe", &source, &config())
        .1
        .expect_err("the declaration must be rejected")
}

/// Loads `body` as plugin `probe` and returns the generation.
fn accepted(body: &str) -> (tempfile::TempDir, PluginGeneration) {
    let source = format!("{HEADER}{body}");
    let (data, result) = load_named("probe", &source, &config());
    let generation = result.unwrap_or_else(|error| panic!("declaration must load: {error}"));
    (data, generation)
}

// --- (1) name grammar -------------------------------------------------------

#[test]
fn tool_local_name_grammar_pins_ascii_boundary() {
    // The wire name `probe__<local>` must fit the 64-character tool-name
    // grammar, so with plugin `probe` (5 + 2 separator bytes) a 57-byte
    // local name is the boundary acceptance and a 64-byte local name is
    // rejected at the wire-name check (see `tool_name_grammar_boundary_is_sixty_four`
    // in descriptor_errors.rs, which owns the 64-character wire boundary).
    let ok = "a".repeat(57);
    let source = format!(
        "{HEADER}def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", run = run, input = dal.schema())\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {{\"{ok}\": t}})\n"
    );
    load_named("probe", &source, &config())
        .1
        .unwrap_or_else(|error| panic!("57-byte local name must load: {error}"));

    // A 64-byte local name passes the local grammar but overflows the wire
    // grammar once qualified as `probe__<local>` (71 bytes).
    {
        let local = "a".repeat(64);
        let source = format!(
            "{HEADER}def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", run = run, input = dal.schema())\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {{\"{local}\": t}})\n"
        );
        let error = load_named("probe", &source, &config())
            .1
            .expect_err("64-byte local name must be rejected at the wire-name check");
        assert!(
            matches!(error, LoadError::InvalidPlugin { .. }),
            "expected InvalidPlugin, got {error:?}"
        );
    }

    for local in [
        format!("{}b", "a".repeat(64)),
        "Bad".to_owned(),
        String::new(),
        "café".to_owned(),
    ] {
        let source = format!(
            "{HEADER}def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", run = run, input = dal.schema())\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {{\"{local}\": t}})\n"
        );
        let error = load_named("probe", &source, &config())
            .1
            .expect_err("bad local name must be rejected");
        let LoadError::InvalidPlugin { message, .. } = &error else {
            panic!("expected InvalidPlugin, got {error:?}");
        };
        assert!(
            message.contains("names must match [a-z][a-z0-9_-]{0,63}"),
            "{local:?}: {message}"
        );
    }
}

// --- (2) service vocabulary --------------------------------------------------

const THIRTEEN_SERVICES: &str =
    "fs.read, fs.write, net, run, env, ask, mcp, agents, jobs, turn, sidecar, infer, judge";

#[test]
fn inject_unknown_service_pins_boundary_wrapped_vocabulary_text() {
    // Boundary wrapper: the spec-exact core text
    // (`unknown service "future"; the service vocabulary is ...`, proven in
    // dal-core) arrives prefixed with `plugin inject: `.
    let LoadError::Registration { message, .. } = rejected(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", inject = [\"future\"])\n",
    ) else {
        panic!("expected a Registration error");
    };
    assert_eq!(
        message.as_ref(),
        format!(
            "plugin inject: unknown service \"future\"; the service vocabulary is {THIRTEEN_SERVICES}"
        )
        .as_str(),
        "{message}"
    );
}

#[test]
fn inject_duplicate_service_pins_boundary_wrapped_text() {
    // As above: spec-exact core (`service "run" appears twice in inject`)
    // under the `plugin inject: ` boundary prefix.
    let LoadError::Registration { message, .. } = rejected(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", inject = [\"run\", \"run\"])\n",
    ) else {
        panic!("expected a Registration error");
    };
    assert_eq!(
        message.as_ref(),
        "plugin inject: service \"run\" appears twice in inject",
        "{message}"
    );
}

#[test]
fn inject_accepts_the_twelve_service_vocabulary() {
    let (data, generation) = accepted(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", inject = [\"fs.read\", \"fs.write\", \"net\", \"run\", \"env\", \"ask\", \"mcp\", \"agents\", \"jobs\", \"turn\", \"sidecar\", \"infer\"])\n",
    );
    let system = dal_star::PluginSystem::new(
        generation,
        LoadRoots {
            data_root: data.path().to_path_buf(),
            bundled: Vec::new(),
        },
        config(),
    );
    let extension = system
        .extensions()
        .expect("extensions")
        .into_iter()
        .find(|extension| extension.name() == "probe")
        .expect("probe extension");
    for service in [
        dal_core::Service::FsRead,
        dal_core::Service::FsWrite,
        dal_core::Service::Net,
        dal_core::Service::Run,
        dal_core::Service::Env,
        dal_core::Service::Ask,
        dal_core::Service::Mcp,
        dal_core::Service::Agents,
        dal_core::Service::Jobs,
        dal_core::Service::Turn,
        dal_core::Service::Sidecar,
        dal_core::Service::Infer,
    ] {
        assert!(extension.inject().contains(service), "missing {service:?}");
    }
}

#[test]
fn ask_is_excluded_from_input_hook_grants() {
    let error = rejected(
        "def h(ctx, event):\n    return None\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", hooks = [dal.on(\"input\", h, uses = [\"ask.select\"])])\n",
    );
    assert!(
        matches!(error, LoadError::Uses { .. }),
        "ask must not be grantable to an input hook: {error:?}"
    );
}

// --- (3) visibility literals --------------------------------------------------

#[test]
fn visibility_accepts_the_three_literals_and_stores_them() {
    let body = "def run(ctx, args):\n    return None\nm = dal.tool(description = \"d\", run = run, input = dal.schema(), visibility = \"model\")\nd = dal.tool(description = \"d\", run = run, input = dal.schema(), visibility = \"deferred\")\ne = dal.tool(description = \"d\", run = run, input = dal.schema(), visibility = \"eval_only\")\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {\"m\": m, \"d\": d, \"e\": e})\n";
    let (data, generation) = accepted(body);
    let system = dal_star::PluginSystem::new(
        generation,
        LoadRoots {
            data_root: data.path().to_path_buf(),
            bundled: Vec::new(),
        },
        config(),
    );
    let extension = system
        .extensions()
        .expect("extensions")
        .into_iter()
        .find(|extension| extension.name() == "probe")
        .expect("probe extension");
    let mut stored: Vec<Visibility> = extension.tools().iter().map(|(_, vis)| *vis).collect();
    stored.sort_by_key(|vis| format!("{vis:?}"));
    assert_eq!(
        stored,
        vec![
            Visibility::Deferred,
            Visibility::EvalOnly,
            Visibility::Model
        ]
    );
}

#[test]
fn visibility_unknown_word_is_rejected_naming_the_literals() {
    // Drift: the boundary reports `tool: visibility must be one of model,
    // deferred, eval_only` (no quotes, no `; got "<v>"`); the spec-exact
    // sentence lives in dal-core (`InvalidVisibility`, proven there). This
    // pins the true properties: rejection at the constructor naming the
    // offending literal domain.
    let error = rejected(
        "def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", run = run, input = dal.schema(), visibility = \"public\")\n",
    );
    let LoadError::Eval { message, line, .. } = &error else {
        panic!("expected a located Eval error, got {error:?}");
    };
    assert!(*line >= 1);
    for needle in ["visibility", "model", "deferred", "eval_only"] {
        assert!(message.contains(needle), "lacks `{needle}`: {message}");
    }
}

// --- (4) hook events -----------------------------------------------------------

#[test]
fn unknown_hook_event_is_rejected_naming_the_nine_events() {
    // Drift: the boundary reports `` on: unknown event `shutdown`; one of
    // session_start, ... `` (backticks, `one of`); the spec sentence is
    // `unknown event "<e>"; the events are ...`. True properties pinned:
    // rejection at the constructor naming the bad event and every legal one.
    let error = rejected(
        "def h(ctx, event):\n    return None\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", hooks = [dal.on(\"shutdown\", h)])\n",
    );
    let LoadError::Eval { message, line, .. } = &error else {
        panic!("expected a located Eval error, got {error:?}");
    };
    assert!(*line >= 1);
    for needle in [
        "unknown event",
        "shutdown",
        "session_start",
        "session_end",
        "input",
        "before_turn",
        "before_request",
        "tool_call",
        "tool_result",
        "turn_end",
        "settled",
    ] {
        assert!(message.contains(needle), "lacks `{needle}`: {message}");
    }
}

#[test]
fn all_nine_hook_events_construct() {
    use std::fmt::Write as _;
    let mut hooks = Vec::new();
    for (index, event) in [
        "session_start",
        "session_end",
        "input",
        "before_turn",
        "before_request",
        "tool_call",
        "tool_result",
        "turn_end",
        "settled",
    ]
    .iter()
    .enumerate()
    {
        hooks.push(format!("dal.on(\"{event}\", h{index})"));
    }
    let mut defs = String::new();
    for index in 0..9 {
        let _ = write!(defs, "def h{index}(ctx, event):\n    return None\n");
    }
    accepted(&format!(
        "{defs}plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", hooks = [{}])\n",
        hooks.join(", ")
    ));
}

#[test]
fn output_stream_is_rejected_at_the_constructor() {
    // Drift (report notes): the spec sentence `output_stream is Rust-only;
    // use dal.rule(...)` is absent; the constructor rejects output_stream
    // through the generic unknown-event path. Pins rejection + naming only.
    let error = rejected(
        "def h(ctx, event):\n    return None\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", hooks = [dal.on(\"output_stream\", h)])\n",
    );
    let LoadError::Eval { message, line, .. } = &error else {
        panic!("expected a located Eval error, got {error:?}");
    };
    assert!(*line >= 1);
    for needle in ["unknown event", "output_stream"] {
        assert!(message.contains(needle), "lacks `{needle}`: {message}");
    }
}

// --- (6) dal.rule ---------------------------------------------------------------

#[test]
fn malformed_rule_regex_is_accepted_at_registration() {
    for pattern in ["[(", "(?P<unterminated>", "*bad*"] {
        let body = [
            "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", rules = {\"r\": dal.rule(text = \"t\", pattern = \"",
            pattern,
            "\")})\n",
        ]
        .concat();
        let (data, generation) = accepted(&body);
        let system = dal_star::PluginSystem::new(
            generation,
            LoadRoots {
                data_root: data.path().to_path_buf(),
                bundled: Vec::new(),
            },
            config(),
        );
        let rules: Vec<_> = system
            .extensions()
            .expect("extensions")
            .iter()
            .flat_map(Extension::rules)
            .cloned()
            .collect();
        assert_eq!(rules.len(), 1, "{pattern}");
        assert_eq!(rules[0].patterns.len(), 1, "{pattern}");
    }
}

#[test]
fn rule_single_pattern_shape_and_always_apply_default() {
    let (data, generation) = accepted(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", rules = {\"r\": dal.rule(text = \"t\", pattern = \"x\")})\n",
    );
    let system = dal_star::PluginSystem::new(
        generation,
        LoadRoots {
            data_root: data.path().to_path_buf(),
            bundled: Vec::new(),
        },
        config(),
    );
    let rules: Vec<_> = system
        .extensions()
        .expect("extensions")
        .iter()
        .flat_map(Extension::rules)
        .cloned()
        .collect();
    assert_eq!(rules.len(), 1);
    let rendered: Vec<String> = rules[0].patterns.iter().map(ToString::to_string).collect();
    assert_eq!(rendered, vec!["x".to_owned()]);
    assert!(!rules[0].always_apply);

    let (data, generation) = accepted(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", rules = {\"r\": dal.rule(text = \"t\", always_apply = True)})\n",
    );
    let system = dal_star::PluginSystem::new(
        generation,
        LoadRoots {
            data_root: data.path().to_path_buf(),
            bundled: Vec::new(),
        },
        config(),
    );
    let rules: Vec<_> = system
        .extensions()
        .expect("extensions")
        .iter()
        .flat_map(Extension::rules)
        .cloned()
        .collect();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].patterns, [] as [std::boxed::Box<str>; 0]);
    assert!(rules[0].always_apply);
}

// --- (8) descriptor-map evidence -------------------------------------------------

#[test]
fn descriptor_maps_publish_every_key_regardless_of_order() {
    // Descriptor maps ride `SmallMap` (insertion-ordered per the
    // starlark_map-0.14.2 source); publishing must not depend on key order.
    // Declaration-order preservation deeper in (schema fields, record
    // fields) is audit evidence in the report: no load-surface exposes it.
    let (data, generation) = accepted(
        "def run(ctx, args):\n    return None\nz = dal.tool(description = \"z\", run = run, input = dal.schema())\na = dal.tool(description = \"a\", run = run, input = dal.schema())\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {\"zeta\": z, \"alpha\": a})\n",
    );
    let system = dal_star::PluginSystem::new(
        generation,
        LoadRoots {
            data_root: data.path().to_path_buf(),
            bundled: Vec::new(),
        },
        config(),
    );
    let extension = system
        .extensions()
        .expect("extensions")
        .into_iter()
        .find(|extension| extension.name() == "probe")
        .expect("probe extension");
    let mut names: Vec<String> = extension
        .tools()
        .iter()
        .map(|(tool, _)| tool.name().as_str().to_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[0].ends_with("__alpha"), "{names:?}");
    assert!(names[1].ends_with("__zeta"), "{names:?}");
}
