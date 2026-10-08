// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use dal_core::{Config, ConfigProduct};
use std::{io, path::PathBuf, time::Duration};

#[test]
fn dalgona_docs_resolve_only_in_dalgona() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-doc-scheme")?;
    let root = scratch.path().to_path_buf();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let dalgona_host = support::start_dalgona(root.join("dalgona")).await?;
        assert!(dalgona_host.doc("dalgona://config").is_ok());
        let factory = dalgon::product();
        let dal_root: PathBuf = root.join("dalgon");
        let config = Config::load(ConfigProduct::Dalgon, &dal_root, factory.defaults, None)?;
        let cx = dalgon::BuildCx {
            data_root: dal_root.clone(),
            config: &config,
        };
        let product = (factory.build)(&cx)?;
        let dal_host =
            dal_agent::Host::start(product, config, dal_agent::Env {
                vars: std::collections::BTreeMap::new(),
                cwd: dal_root,
                sandbox_helper: None,
            }).await?;
        let error = match dal_host.doc("dalgona://config") {
            Ok(_) => {
                return Err(io::Error::other(
                    "dalgon unexpectedly resolved a Dalgona document scheme",
                )
                .into());
            }
            Err(error) => error.to_string(),
        };
        assert!(error.contains("dalgona"), "{error}");
        let dalgona_report = dalgona_host.shutdown(Duration::from_secs(2)).await;
        let dal_report = dal_host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(dalgona_report.sessions_closed, 0);
        assert_eq!(dal_report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
