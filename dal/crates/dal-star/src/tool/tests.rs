use std::sync::Arc;

use crate::load::{LoadRoots, PluginsConfig, load};
use dal_core::ext::ExportKind;

use crate::validate::{ExportBody, LoadedPlugin};

use super::{bind_command, decode_args};

const FIXTURE: &str = r#"load("@dal/v1", "dal")
def dispatch(ctx, args):
    return args

tool = dal.tool(
    description = "Return normalized command arguments.",
    input = dal.schema(
        path = dal.string(),
        count = dal.integer(default = 2),
        enabled = dal.boolean(default = False),
    ),
    run = dispatch,
)
plugin = dal.plugin(
    name = "binding",
    version = "0.1.0",
    tools = {"dispatch": tool},
    commands = {"dispatch": dal.command(tool = tool, positional = ["path"])},
)
"#;

fn loaded_plugin() -> Arc<LoadedPlugin> {
    let data = tempfile::tempdir().expect("temporary plugin root");
    let directory = data.path().join("plugins/binding");
    std::fs::create_dir_all(&directory).expect("plugin directory");
    std::fs::write(directory.join("plugin.star"), FIXTURE).expect("plugin source");
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let config = PluginsConfig {
        enabled: Vec::new(),
        limits: dal_core::PluginLimits::default(),
        configs: std::collections::BTreeMap::default(),
    };
    let generation = load(&roots, &config).expect("binding fixture loads");
    Arc::clone(&generation.plugins[0])
}

fn command_schema(plugin: &LoadedPlugin) -> &crate::schema::Schema {
    &plugin.commands[0].tool.schema
}

#[test]
fn tool_and_command_inputs_share_validated_values_and_handler() {
    let plugin = loaded_plugin();
    let export = plugin
        .exports
        .iter()
        .find(|export| export.id.kind == ExportKind::Tool)
        .expect("tool export");
    let ExportBody::Tool {
        schema: tool_schema,
        run: tool_handler,
        ..
    } = &export.body;
    let command = &plugin.commands[0];
    let model_args = decode_args(
        r#"{"path":"src/lib.rs","count":3,"enabled":true}"#,
        tool_schema,
    )
    .expect("model tool arguments validate");
    let command_args = bind_command(
        "src/lib.rs --count=3 --enabled",
        &command.positional,
        command_schema(&plugin),
    )
    .expect("command tail binds through the same schema");

    assert_eq!(model_args, command_args);
    assert!(command.tool.run.to_value().ptr_eq(tool_handler.to_value()));
}

#[test]
fn unclosed_command_quote_returns_the_fixed_lexer_error() {
    let plugin = loaded_plugin();
    let error = bind_command(
        "src/lib.rs \"unfinished",
        &["path".into()],
        command_schema(&plugin),
    )
    .expect_err("unclosed quotes are rejected");

    assert_eq!(error.to_string(), "unclosed \" quote at byte 11");
}

#[test]
fn duplicate_command_flag_returns_the_fixed_binder_error() {
    let plugin = loaded_plugin();
    let error = bind_command(
        "src/lib.rs --enabled --enabled",
        &["path".into()],
        command_schema(&plugin),
    )
    .expect_err("a flag may be assigned only once");

    assert_eq!(error.to_string(), "field `enabled` assigned more than once");
}

#[test]
fn shell_substitution_text_is_rejected_as_an_invalid_integer() {
    let plugin = loaded_plugin();
    let error = bind_command(
        "src/lib.rs --count=$(id)",
        &["path".into()],
        command_schema(&plugin),
    )
    .expect_err("command substitution is not evaluated as shell syntax");

    assert_eq!(error.to_string(), "`$(id)` is not an integer for `count`");
}

fn bind_error(tail: &str) -> String {
    let plugin = loaded_plugin();
    bind_command(tail, &["path".into()], command_schema(&plugin))
        .expect_err("the tail must be rejected")
        .to_string()
}

#[test]
fn command_binding_rejects_each_malformed_tail_with_a_named_reason() {
    let cases = [
        ("", "missing positional arguments: path"),
        ("a b", "unexpected argument `b`"),
        ("a --bogus", "unknown option `--bogus`"),
        ("a --no-bogus", "unknown option `--bogus`"),
        ("a --count", "`--count` expects a value"),
        ("a --count=x", "`x` is not an integer for `count`"),
        ("a --count=1.5", "`1.5` is not an integer for `count`"),
        (
            "a --count=9223372036854775808",
            "not an integer for `count`",
        ),
        ("a --enabled=true", "boolean `--enabled` takes no value"),
        ("a --no-count", "unknown option `--no-count`"),
        ("a --path=b", "field `path` assigned more than once"),
        (
            "a --count=1 --count=2",
            "field `count` assigned more than once",
        ),
    ];
    for (tail, reason) in cases {
        let message = bind_error(tail);
        assert!(message.contains(reason), "{tail:?}: {message}");
    }
}

#[test]
fn double_dash_ends_option_parsing_for_positional_values() {
    let plugin = loaded_plugin();
    let args = bind_command("-- --dashed", &["path".into()], command_schema(&plugin))
        .expect("a `--` tail makes the next token positional");
    assert!(
        args.to_json().contains("\"path\":\"--dashed\""),
        "{}",
        args.to_json()
    );
}

#[test]
fn command_and_model_paths_agree_on_the_signed_53_bit_integer_bound() {
    let plugin = loaded_plugin();
    let export = plugin
        .exports
        .iter()
        .find(|export| export.id.kind == ExportKind::Tool)
        .expect("tool export");
    let ExportBody::Tool { schema, .. } = &export.body;
    for (count, fits) in [
        ("9007199254740990", true),
        ("9007199254740991", true),
        ("9007199254740992", false),
        ("-9007199254740991", true),
        ("-9007199254740992", false),
    ] {
        let model = decode_args(&format!(r#"{{"path":"p","count":{count}}}"#), schema);
        let command = bind_command(
            &format!("p --count={count}"),
            &["path".into()],
            command_schema(&plugin),
        );
        assert_eq!(model.is_ok(), fits, "model path {count}: {model:?}");
        assert_eq!(command.is_ok(), fits, "command path {count}: {command:?}");
    }
}

#[test]
fn non_finite_and_malformed_model_arguments_are_rejected() {
    let plugin = loaded_plugin();
    let export = plugin
        .exports
        .iter()
        .find(|export| export.id.kind == ExportKind::Tool)
        .expect("tool export");
    let ExportBody::Tool { schema, .. } = &export.body;
    for raw in [
        "",
        "{",
        "[]",
        "null",
        r#"{"path":"p","count":"3"}"#,
        r#"{"path":"p","count":3.0}"#,
        r#"{"path":"p","enabled":1}"#,
        r#"{"path":"p","extra":1}"#,
        r#"{"path":"p","path":"q"}"#,
        r#"{"path":null}"#,
    ] {
        assert!(
            decode_args(raw, schema).is_err(),
            "{raw:?} must be rejected"
        );
    }
}
