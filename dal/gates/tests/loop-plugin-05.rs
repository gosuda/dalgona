//! The Rust-only `output_stream` event is excluded from the Starlark hook table.

use std::error::Error;

use dal_core::ext::{RUST_STREAM_EVENT, STAR_EVENTS};

#[test]
fn starlark_hooks_receive_typed_payloads_and_fail_closed()
-> Result<(), Box<dyn Error + Send + Sync>> {
    assert_eq!(
        STAR_EVENTS.len(),
        9,
        "must test the nine Starlark-visible events from the runtime, not a copied list"
    );
    for event in STAR_EVENTS {
        assert_ne!(
            event, RUST_STREAM_EVENT,
            "output_stream must never be Starlark-visible"
        );
    }

    let dir = tempfile::tempdir()?;
    let plugin_dir = dir.path().join("plugins").join("rejects-stream");
    std::fs::create_dir_all(&plugin_dir)?;
    std::fs::write(
        plugin_dir.join("plugin.star"),
        r#"load("@dal/v1", "dal")

def observe_stream(ctx, event):
    return None

hook = dal.on("output_stream", observe_stream)

plugin = dal.plugin(
    name = "rejects-stream",
    version = "0.1.0",
    hooks = [hook],
)
"#,
    )?;
    let roots = dal_star::LoadRoots {
        data_root: dir.path().to_path_buf(),
        bundled: vec![],
    };
    let rejected = dal_star::load(&roots, &dal_star::PluginsConfig::default());
    let error = rejected.expect_err("output_stream load must fail");
    assert!(
        error.to_string().contains(
            "on: unknown event `output_stream`; one of session_start, session_end, input, before_turn, before_request, tool_call, tool_result, turn_end, settled"
        ),
        "exact v1 hook-table rejection, got: {error}"
    );
    Ok(())
}
