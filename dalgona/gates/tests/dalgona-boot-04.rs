// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use dal_core::{Config, ConfigProduct};
use std::{io, path::PathBuf};

#[test]
fn unknown_battery_uses_the_config_error() -> support::TestResult<()> {
    let scratch = support::Scratch::new("unknown-battery")?;
    let root: PathBuf = scratch.path().to_path_buf();
    let factory = dalgona::product();
    let config = Config::load(
        ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        Some("disabled_batteries = [\"orchestraton\"]\n"),
    )?;
    let cx = dalgon::BuildCx {
        data_root: root,
        config: &config,
    };
    let error = match dalgona::build(&cx) {
        Ok(_) => return Err(io::Error::other("unknown battery name was accepted").into()),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("orchestraton"), "{error}");
    assert!(error.contains("orchestration"), "{error}");
    Ok(())
}
