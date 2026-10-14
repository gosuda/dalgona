// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: the `[plugin.orchestration.isolation]`
//! key parses, an unknown sub-battery key still fails the build, and the
//! arbiter/inflight coherence refusal names its dep.
use dalgona_gates::support;

use std::time::Duration;

#[test]
fn isolation_key_parses_and_coherence_refusals_name_their_dep() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-isolation-key")?;

    // The isolation key is accepted and reaches the battery config.
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        scratch.path(),
        dalgona::product().defaults,
        Some("[plugin.orchestration.isolation]\nenabled = false\n"),
    )?;
    let cx = dalgon::BuildCx {
        data_root: scratch.path().to_path_buf(),
        config: &config,
    };
    let product = dalgona::build(&cx)?;
    let orchestration = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "orchestration")
        .ok_or("orchestration battery missing")?;
    assert!(
        orchestration
            .tools()
            .iter()
            .any(|(tool, _)| tool.name().as_str() == "agents"),
        "agents must stay registered with isolation off"
    );

    // An unknown sub-battery key is a startup error, not a silent default.
    let error = build_refusal(
        &scratch,
        "[plugin.orchestration.no_such_battery]\nenabled = true\n",
    )?;
    assert!(
        error.contains("no_such_battery"),
        "unknown-key error must name the key: {error}"
    );

    // Coherence: arbiter cannot be off while goal is on.
    let error = build_refusal(
        &scratch,
        "[plugin.orchestration.arbiter]\nenabled = false\n\n\
         [plugin.orchestration.goal]\nenabled = true\n",
    )?;
    assert!(
        error.contains(
            "orchestration: [plugin.orchestration.arbiter] cannot be off while goal is on."
        ),
        "coherence error text drifted: {error}"
    );

    // The built product still serves a live host with the battery intact.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        assert!(doc.text.contains("## Constants"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

/// Loads `toml` over the product defaults, which the generic loader accepts,
/// and returns the refusal the strict battery decode raises when the product
/// is built.
fn build_refusal(scratch: &support::Scratch, toml: &str) -> support::TestResult<String> {
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        scratch.path(),
        dalgona::product().defaults,
        Some(toml),
    )?;
    let cx = dalgon::BuildCx {
        data_root: scratch.path().to_path_buf(),
        config: &config,
    };
    match dalgona::build(&cx) {
        Ok(_) => Err(format!("the product built from {toml:?}").into()),
        Err(error) => Ok(error.to_string()),
    }
}
