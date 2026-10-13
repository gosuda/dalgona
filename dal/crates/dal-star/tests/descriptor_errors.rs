//! Adversarial violation tests for `dal.*` constructors and descriptor
//! validation: bad argument shapes, limits, and capability declarations must
//! fail the load with a located, descriptive `LoadError`.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
use std::collections::BTreeMap;

use dal_core::PluginLimits;
use dal_star::{LoadError, LoadRoots, PluginGeneration, PluginSystem, PluginsConfig, load};

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

/// Loads `body` (after the SDK header) as plugin `probe` and returns the error text.
fn rejected(body: &str) -> LoadError {
    let source = format!("{HEADER}{body}");
    load_named("probe", &source, &config())
        .1
        .expect_err("the declaration must be rejected")
}

fn accepted(body: &str) -> PluginGeneration {
    let source = format!("{HEADER}{body}");
    load_named("probe", &source, &config())
        .1
        .unwrap_or_else(|error| panic!("declaration must load: {error}"))
}

const PLUGIN: &str = "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\")\n";

fn tool_plugin(tool_args: &str) -> String {
    format!(
        "def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", run = run, {tool_args})\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {{\"t\": t}})\n"
    )
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

// --- dal.schema constructors ----------------------------------------------

#[test]
fn string_length_bounds_reject_inverted_and_negative_limits() {
    for (body, needle) in [
        (
            "s = dal.schema(a = dal.string(min_len = 3, max_len = 2))\n",
            "exceeds",
        ),
        ("s = dal.schema(a = dal.string(min_len = -1))\n", "negative"),
        ("s = dal.schema(a = dal.string(max_len = -1))\n", "negative"),
    ] {
        assert_eval(&rejected(&format!("{body}{PLUGIN}")), needle);
    }
    accepted(&format!(
        "s = dal.schema(a = dal.string(min_len = 2, max_len = 2))\n{PLUGIN}"
    ));
}

#[test]
fn integer_and_number_bounds_reject_min_above_max() {
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.integer(min = 5, max = 4))\n{PLUGIN}"
        )),
        "exceeds",
    );
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.number(min = 1.5, max = 1.0))\n{PLUGIN}"
        )),
        "exceeds",
    );
    accepted(&format!(
        "s = dal.schema(a = dal.integer(min = 4, max = 4))\n{PLUGIN}"
    ));
}

#[test]
fn defaults_must_satisfy_their_own_field_type() {
    for body in [
        "s = dal.schema(a = dal.integer(default = \"x\"))\n",
        "s = dal.schema(a = dal.integer(min = 1, default = 0))\n",
        "s = dal.schema(a = dal.string(max_len = 1, default = \"ab\"))\n",
        "s = dal.schema(a = dal.boolean(default = 1))\n",
        "s = dal.schema(a = dal.enum(values = [\"a\"], default = \"b\"))\n",
    ] {
        assert_eval(&rejected(&format!("{body}{PLUGIN}")), "default");
    }
}

#[test]
fn non_transport_defaults_are_rejected() {
    let error = rejected(&format!(
        "s = dal.schema(a = dal.string(default = lambda: 1))\n{PLUGIN}"
    ));
    assert_eval(&error, "transport");
}

#[test]
fn enum_needs_distinct_values() {
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.enum(values = []))\n{PLUGIN}"
        )),
        "at least one",
    );
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.enum(values = [\"a\", \"a\"]))\n{PLUGIN}"
        )),
        "duplicate",
    );
}

#[test]
fn field_names_must_be_public_non_keyword_identifiers() {
    for name in ["if", "_private", "has space", "9lead", "None"] {
        let error = rejected(&format!(
            "s = dal.schema(**{{\"{name}\": dal.string()}})\n{PLUGIN}"
        ));
        assert_eval(&error, "field name");
    }
}

#[test]
fn schema_fields_must_be_dal_types() {
    assert_eval(
        &rejected(&format!("s = dal.schema(a = 5)\n{PLUGIN}")),
        "dal type value",
    );
}

#[test]
fn presence_wrappers_reject_conflicting_rules() {
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.optional(dal.string(default = \"x\")))\n{PLUGIN}"
        )),
        "optional with a default",
    );
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.optional(dal.optional(dal.string())))\n{PLUGIN}"
        )),
        "already has",
    );
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.list(dal.optional(dal.string())))\n{PLUGIN}"
        )),
        "bare type",
    );
}

#[test]
fn list_length_bounds_are_validated_like_strings() {
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.list(dal.string(), min_len = 2, max_len = 1))\n{PLUGIN}"
        )),
        "exceeds",
    );
    assert_eval(
        &rejected(&format!(
            "s = dal.schema(a = dal.list(dal.string(), min_len = -1))\n{PLUGIN}"
        )),
        "negative",
    );
}

// --- dal.plugin / dal.tool arguments ---------------------------------------

#[test]
fn state_version_must_be_a_positive_integer() {
    assert_eval(
        &rejected(
            "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", state_version = 0)\n",
        ),
        "nonzero",
    );
    assert_eval(
        &rejected(
            "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", state_version = -1)\n",
        ),
        "positive integer",
    );
    accepted("plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", state_version = 1)\n");
}

#[test]
fn plugin_requires_named_keywords_only() {
    assert_eval(&rejected("plugin = dal.plugin(\"probe\", \"0.1.0\")\n"), "");
    assert_eval(
        &rejected("plugin = dal.plugin(name = \"probe\")\n"),
        "version",
    );
    assert_eval(
        &rejected("plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", bogus = 1)\n"),
        "bogus",
    );
}

#[test]
fn plugin_name_grammar_rejects_bad_names() {
    for name in ["Probe", "", "9a", "a b"] {
        let source =
            format!("{HEADER}plugin = dal.plugin(name = \"{name}\", version = \"0.1.0\")\n");
        let (_data, result) = load_named("probe", &source, &config());
        assert!(
            matches!(
                result,
                Err(LoadError::InvalidPlugin { .. } | LoadError::NameMismatch { .. })
            ),
            "{name:?}: {:?}",
            result.err()
        );
    }
}

#[test]
fn tool_input_must_be_a_schema_and_visibility_a_known_word() {
    assert_eval(
        &rejected(
            "def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", run = run, input = 5)\n",
        ),
        "dal.schema",
    );
    assert_eval(
        &rejected(
            "def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", run = run, input = dal.schema(), visibility = \"public\")\n",
        ),
        "visibility",
    );
}

#[test]
fn published_tool_must_be_a_tool_descriptor() {
    let error = rejected(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {\"t\": 5})\n",
    );
    assert!(
        matches!(error, LoadError::InvalidPlugin { .. }),
        "{error:?}"
    );
}

#[test]
fn a_tool_published_under_two_keys_is_rejected() {
    let error = rejected(
        "def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {\"a\": t, \"b\": t})\n",
    );
    assert!(
        matches!(error, LoadError::InvalidPlugin { .. }),
        "{error:?}"
    );
}

#[test]
fn tool_name_grammar_boundary_is_sixty_four() {
    let ok = "a".repeat(64);
    let source = format!(
        "{HEADER}def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {{\"{ok}\": t}})\n"
    );
    // The wire name `probe__<tool>` overflows the 64-character tool-name
    // grammar long before the local name does.
    let (_data, result) = load_named("probe", &source, &config());
    assert!(
        matches!(result, Err(LoadError::InvalidPlugin { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn wire_name_boundary_is_sixty_four_characters() {
    // `<plugin>__<tool>`: 31 + 2 + 31 = 64 passes, 31 + 2 + 32 = 65 fails.
    let plugin = "p".repeat(31);
    for (local_len, fits) in [(31, true), (32, false)] {
        let local = "t".repeat(local_len);
        let source = format!(
            "{HEADER}def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"{plugin}\", version = \"0.1.0\", tools = {{\"{local}\": t}})\n"
        );
        let (_data, result) = load_named(&plugin, &source, &config());
        assert_eq!(
            result.is_ok(),
            fits,
            "local length {local_len}: {:?}",
            result.err()
        );
    }
}

// --- uses declarations ------------------------------------------------------

#[test]
fn unknown_operation_in_uses_is_a_located_uses_error() {
    let error = rejected(&tool_plugin("input = dal.schema(), uses = [\"nope.nope\"]"));
    let LoadError::Uses { line, .. } = error else {
        panic!("expected Uses, got {error:?}");
    };
    assert!(line >= 1);
}

#[test]
fn malformed_uses_strings_are_rejected() {
    for id in [
        "",
        "tools",
        "tools.",
        ".read",
        "tools.read extra",
        "TOOLS.READ",
    ] {
        let error = rejected(&tool_plugin(&format!(
            "input = dal.schema(), uses = [\"{id}\"]"
        )));
        assert!(matches!(error, LoadError::Uses { .. }), "{id:?}: {error:?}");
    }
}

#[test]
fn duplicate_uses_entries_are_rejected() {
    let source = format!(
        "{HEADER}{}",
        tool_plugin("input = dal.schema(), uses = [\"tools.read\", \"tools.read\"]")
    );
    let (_data, result) = load_named("probe", &source, &config());
    assert!(
        matches!(result, Err(LoadError::Uses { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn uses_list_boundary_at_sixty_four_entries() {
    let ids = |count: usize| {
        (0..count)
            .map(|index| format!("\"tools.probe.t{index}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let source = |count: usize| {
        format!(
            "{HEADER}{}",
            tool_plugin(&format!("input = dal.schema(), uses = [{}]", ids(count)))
        )
    };
    let (_data, at_cap) = load_named("probe", &source(64), &config());
    assert!(at_cap.is_ok(), "{:?}", at_cap.err());
    let (_data, over_cap) = load_named("probe", &source(65), &config());
    assert!(
        matches!(over_cap, Err(LoadError::Uses { .. })),
        "65 uses entries exceed the R10 cap: {:?}",
        over_cap.err()
    );
}

#[test]
fn hook_uses_above_the_phase_ceiling_are_rejected() {
    // `input` hooks may request no operations (P05).
    let error = rejected(
        "def h(ctx, event):\n    return None\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", hooks = [dal.on(\"input\", h, uses = [\"env.read\"])])\n",
    );
    assert!(matches!(error, LoadError::Uses { .. }), "{error:?}");
}

#[test]
fn unknown_hook_event_fails_at_the_constructor() {
    assert_eval(
        &rejected(
            "def h(ctx, event):\n    return None\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", hooks = [dal.on(\"shutdown\", h)])\n",
        ),
        "unknown event",
    );
}

#[test]
fn published_hook_must_be_a_hook_descriptor() {
    let error =
        rejected("plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", hooks = [5])\n");
    assert!(
        matches!(error, LoadError::InvalidPlugin { .. }),
        "{error:?}"
    );
}

#[test]
fn model_forward_is_not_declarable_by_a_tool() {
    let error = rejected(&tool_plugin(
        "input = dal.schema(), uses = [\"models.forward\"]",
    ));
    assert!(matches!(error, LoadError::Uses { .. }), "{error:?}");
}

#[test]
fn model_uses_are_checked_against_the_model_phase() {
    // Only a model handler may declare `models.forward`: this must load.
    accepted(
        "def infer(ctx, request):\n    return None\nm = dal.model(id = \"dalgona/m\", caps = {\"context_window\": 1, \"thinking\": [\"off\"], \"tool_use\": False, \"image_input\": False}, run = infer, uses = [\"models.forward\"])\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", models = {\"m\": m})\n",
    );
}

// --- commands ---------------------------------------------------------------

fn command_plugin(schema: &str, positional: &str) -> String {
    format!(
        "def run(ctx, args):\n    return None\nt = dal.tool(description = \"d\", input = dal.schema({schema}), run = run)\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {{\"t\": t}}, commands = {{\"c\": dal.command(tool = t, positional = [{positional}])}})\n"
    )
}

#[test]
fn command_positional_bindings_are_checked_at_load() {
    for (schema, positional) in [
        ("a = dal.string()", "\"missing\""),
        ("a = dal.string()", "\"a\", \"a\""),
        ("a = dal.list(dal.string())", "\"a\""),
        (
            "a = dal.string(default = \"x\"), b = dal.string()",
            "\"a\", \"b\"",
        ),
    ] {
        let error = rejected(&command_plugin(schema, positional));
        assert!(
            matches!(error, LoadError::InvalidPlugin { .. }),
            "{schema} / {positional}: {error:?}"
        );
    }
    accepted(&command_plugin(
        "a = dal.string(), b = dal.string(default = \"x\")",
        "\"a\", \"b\"",
    ));
}

#[test]
fn published_command_must_be_a_command_descriptor() {
    let error = rejected(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", commands = {\"c\": 5})\n",
    );
    assert!(
        matches!(error, LoadError::InvalidPlugin { .. }),
        "{error:?}"
    );
}

// --- models -----------------------------------------------------------------

fn model_plugin(id: &str, caps: &str) -> String {
    format!(
        "def infer(ctx, request):\n    return None\nm = dal.model(id = \"{id}\", caps = {caps}, run = infer)\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", models = {{\"m\": m}})\n"
    )
}

#[test]
fn model_declarations_reject_malformed_ids_and_caps() {
    let good = "{\"context_window\": 8, \"thinking\": [\"off\"], \"tool_use\": False, \"image_input\": False}";
    for (id, caps) in [
        ("", good),
        ("no-slash", good),
        ("dalgona/m", "5"),
        (
            "dalgona/m",
            "{\"context_window\": -1, \"thinking\": [\"off\"], \"tool_use\": False, \"image_input\": False}",
        ),
        (
            "dalgona/m",
            "{\"context_window\": 8, \"thinking\": [\"sideways\"], \"tool_use\": False, \"image_input\": False}",
        ),
        (
            "dalgona/m",
            "{\"context_window\": 8, \"thinking\": [\"off\"], \"tool_use\": 1, \"image_input\": False}",
        ),
    ] {
        let error = rejected(&model_plugin(id, caps));
        assert!(
            matches!(error, LoadError::Registration { .. }),
            "{id:?} {caps}: {error:?}"
        );
    }
}

#[test]
fn published_model_must_be_a_model_descriptor() {
    let error = rejected(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", models = {\"m\": 5})\n",
    );
    assert!(matches!(error, LoadError::Registration { .. }), "{error:?}");
}

// --- skills and rules -------------------------------------------------------

fn skill_plugin(path: &str) -> String {
    format!(
        "s = dal.skill(description = \"d\", path = \"{path}\")\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", skills = {{\"s\": s}})\n"
    )
}

#[test]
fn skill_paths_cannot_escape_or_point_at_nothing() {
    for path in [
        "../SKILL.md",
        "/etc/passwd",
        "./SKILL.md",
        "nested/../SKILL.md",
        "missing.md",
    ] {
        let error = rejected(&skill_plugin(path));
        assert!(
            matches!(error, LoadError::Asset { .. }),
            "{path}: {error:?}"
        );
    }
}

#[test]
fn skill_body_that_is_not_utf8_is_refused() {
    let data = tempfile::tempdir().expect("data root");
    let dir = data.path().join("plugins").join("probe");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("plugin.star"),
        format!("{HEADER}{}", skill_plugin("SKILL.md")),
    )
    .expect("entry");
    std::fs::write(dir.join("SKILL.md"), [0xff, 0xfe]).expect("skill");
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let result = load(&roots, &config());
    assert!(
        matches!(result, Err(LoadError::InvalidUtf8 { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn rules_reject_missing_trigger_and_bad_policy_words() {
    for rule in [
        "dal.rule(text = \"t\")",
        "dal.rule(text = \"t\", pattern = \"x\", scope = [])",
        "dal.rule(text = \"t\", pattern = \"x\", scope = [\"nonsense\"])",
        "dal.rule(text = \"t\", pattern = \"x\", interrupt_mode = \"sometimes\")",
        "dal.rule(text = \"t\", pattern = \"x\", repeat_mode = \"twice\")",
        "dal.rule(text = \"t\", pattern = \"x\", repeat_mode = \"after-gap\", repeat_gap = 0)",
        "dal.rule(text = \"t\", pattern = \"x\", repeat_mode = \"after-gap\", repeat_gap = 1001)",
        "dal.rule(text = \"t\", pattern = \"x\", judge = lambda: 1)",
    ] {
        let source = format!(
            "{HEADER}plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", rules = {{\"r\": {rule}}})\n"
        );
        let (_data, result) = load_named("probe", &source, &config());
        assert!(result.is_err(), "{rule} must be rejected");
    }
}

#[test]
fn rule_repeat_gap_boundary_is_one_to_one_thousand() {
    for gap in [1, 1000] {
        accepted(&format!(
            "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", rules = {{\"r\": dal.rule(text = \"t\", pattern = \"x\", repeat_mode = \"after-gap\", repeat_gap = {gap})}})\n"
        ));
    }
}

// --- config -----------------------------------------------------------------

fn with_config(source: &str, config_json: Option<&str>) -> Result<PluginGeneration, LoadError> {
    let mut cfg = config();
    if let Some(json) = config_json {
        cfg.configs.insert("probe".to_owned(), json.to_owned());
    }
    load_named("probe", &format!("{HEADER}{source}"), &cfg).1
}

const SCHEMA_PLUGIN: &str = "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", config = dal.schema(level = dal.integer(min = 1, max = 5), label = dal.string(default = \"x\")))\n";

#[test]
fn config_without_a_schema_is_refused() {
    let error = with_config(PLUGIN, Some("{\"k\": 1}")).expect_err("undeclared config");
    assert!(matches!(error, LoadError::Schema { .. }), "{error:?}");
}

#[test]
fn config_must_be_a_json_object_matching_the_schema() {
    for json in [
        "[]",
        "5",
        "not json",
        "{\"level\": 6}",
        "{\"level\": 0}",
        "{\"level\": \"a\"}",
        "{\"level\": 2, \"extra\": 1}",
        "{}",
    ] {
        let error = with_config(SCHEMA_PLUGIN, Some(json)).expect_err(json);
        assert!(
            matches!(error, LoadError::Schema { .. }),
            "{json}: {error:?}"
        );
    }
    for json in ["{\"level\": 1}", "{\"level\": 5}"] {
        with_config(SCHEMA_PLUGIN, Some(json)).unwrap_or_else(|error| panic!("{json}: {error}"));
    }
}

#[test]
fn absent_config_still_enforces_required_schema_fields() {
    let error = with_config(SCHEMA_PLUGIN, None).expect_err("required field missing");
    assert!(matches!(error, LoadError::Schema { .. }), "{error:?}");
}

#[test]
fn plugin_config_must_be_a_schema_value() {
    let error = with_config(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", config = 5)\n",
        None,
    )
    .expect_err("config = 5");
    assert!(matches!(error, LoadError::Schema { .. }), "{error:?}");
}

// --- injection and version --------------------------------------------------

#[test]
fn unknown_inject_service_is_located_at_the_declaration() {
    let error = rejected(
        "plugin = dal.plugin(name = \"probe\", version = \"0.1.0\", inject = [\"warp\"])\n",
    );
    assert!(matches!(error, LoadError::Registration { .. }), "{error:?}");
}

#[test]
fn a_non_semver_version_does_not_reach_a_published_extension() {
    let source =
        format!("{HEADER}plugin = dal.plugin(name = \"probe\", version = \"not-a-version\")\n");
    let (data, result) = load_named("probe", &source, &config());
    let rejected = match result {
        Err(_) => true,
        Ok(generation) => {
            let roots = LoadRoots {
                data_root: data.path().to_path_buf(),
                bundled: Vec::new(),
            };
            PluginSystem::new(generation, roots, config())
                .extensions()
                .is_err()
        }
    };
    assert!(
        rejected,
        "the loader documents a SemVer version; a garbage version must be refused at load or conversion"
    );
}
