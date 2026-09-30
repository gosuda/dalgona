// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::{path::PathBuf, time::Duration};
use dal_core::{Config, ConfigProduct};

#[test]
fn auto_thinking_changes_only_typed_request_params() -> support::TestResult<()> {
    let scratch = support::Scratch::new("judged-typed-params")?;
    let root: PathBuf = scratch.path().to_path_buf();
    let factory = dalgona::product();
    let config = Config::load(ConfigProduct::Dalgona, &root, factory.defaults, Some("[plugin.judged]\nmode = \"auto\"\n"))?;
    let cx = dalgon::BuildCx { data_root: root.clone(), config: &config };
    let product = dalgona::build(&cx)?;
    assert_eq!(product.name, "dalgona");
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async {
        let host = support::start_dalgona_with_config(root, Some("[plugin.judged]\nmode = \"auto\"\n")).await?;
        let doc = host.doc("dalgona://judged")?;
        assert!(format!("{doc:?}").contains("before_request"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
