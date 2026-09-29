#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]

//! Hook payloads: `output_stream` is Rust-only and must fail load.

use std::error::Error;

use dal_core::ext::{RUST_STREAM_EVENT, STAR_EVENTS};

#[test]
fn starlark_hooks_receive_typed_payloads_and_fail_closed() -> Result<(), Box<dyn Error + Send + Sync>> {
    assert_eq!(STAR_EVENTS.len(), 9, "must test the nine Starlark-visible events from the runtime, not a copied list");
    for event in STAR_EVENTS {
        assert_ne!(event, RUST_STREAM_EVENT, "output_stream must never be Starlark-visible");
    }

    let dir = tempfile::tempdir()?;
    let plugin_dir = dir.path().join("plugins").join("rejects-stream");
    std::fs::create_dir_all(&plugin_dir)?;
    std::fs::write(
        plugin_dir.join("plugin.star"),
        "dal.on(\"output_stream\", lambda x: x)\n",
    )?;
    let roots = dal_star::LoadRoots {
        data_root: dir.path().to_path_buf(),
        bundled: vec![],
    };
    let rejected = dal_star::load(&roots, &dal_star::PluginsConfig::default());
    let error = rejected.expect_err("output_stream load must fail");
    assert!(
        error.to_string().contains("output_stream is Rust-only"),
        "exact Rust-only rejection, got: {error}"
    );
    Ok(())
}
