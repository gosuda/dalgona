//! The Starlark hook table is exactly the runtime's event list: every listed
//! event registers, and the Rust-only `output_stream` event or a near-miss
//! name fails the plugin load closed with the one-of list.

use std::error::Error;

use dal_core::ext::{RUST_STREAM_EVENT, STAR_EVENTS};

/// Loads one plugin whose only hook is registered for `event`; the inner
/// result is the load outcome, the outer one a fixture setup failure.
fn load_with_hook_event(event: &str) -> Result<Result<(), String>, Box<dyn Error + Send + Sync>> {
    let dir = tempfile::tempdir()?;
    let plugin_dir = dir.path().join("plugins").join("hook-probe");
    std::fs::create_dir_all(&plugin_dir)?;
    std::fs::write(
        plugin_dir.join("plugin.star"),
        format!(
            r#"load("@dal/v1", "dal")

def observe(ctx, event):
    return None

hook = dal.on({event:?}, observe)

plugin = dal.plugin(
    name = "hook-probe",
    version = "0.1.0",
    hooks = [hook],
)
"#
        ),
    )?;
    let roots = dal_star::LoadRoots {
        data_root: dir.path().to_path_buf(),
        bundled: vec![],
    };
    Ok(dal_star::load(&roots, &dal_star::PluginsConfig::default())
        .map(|generation| {
            let names: Vec<&str> = generation.names().collect();
            assert_eq!(
                names,
                ["hook-probe"],
                "a successful load must contain the probe plugin"
            );
        })
        .map_err(|error| error.to_string()))
}

#[test]
fn starlark_hook_table_is_exactly_the_runtime_event_list()
-> Result<(), Box<dyn Error + Send + Sync>> {
    for event in STAR_EVENTS {
        assert_eq!(
            load_with_hook_event(event)?,
            Ok(()),
            "the runtime-listed event `{event}` must register"
        );
    }

    let rejected =
        load_with_hook_event(RUST_STREAM_EVENT)?.expect_err("output_stream load must fail");
    assert!(
        rejected.contains(
            "on: unknown event `output_stream`; one of session_start, session_end, input, before_turn, before_request, tool_call, tool_result, turn_end, settled"
        ),
        "exact v1 hook-table rejection, got: {rejected}"
    );

    let listed = STAR_EVENTS.join(", ");
    for near_miss in [
        RUST_STREAM_EVENT,
        "Turn_end",
        "turn_end ",
        "tool-call",
        "turn",
        "settled2",
    ] {
        let error = load_with_hook_event(near_miss)?
            .expect_err("a name outside the hook table must fail the load");
        assert!(
            error.contains(&format!("unknown event `{near_miss}`; one of {listed}")),
            "`{near_miss}` must be rejected against the runtime list, got: {error}"
        );
    }
    Ok(())
}
