// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::{fs, io, path::PathBuf, time::Duration};

#[test]
fn dalgona_never_scans_the_dal_data_root() -> support::TestResult<()> {
    let scratch = support::Scratch::new("product-root-isolation")?;
    let dal_root = scratch.path().join("dalgon");
    let dalgona_root: PathBuf = scratch.path().join("dalgona");
    let plugin = dal_root.join("plugins/ask/plugin.star");
    fs::create_dir_all(plugin.parent().ok_or("plugin has no parent directory")?)?;
    fs::write(&plugin, "dal.plugin(name = \"ask\")\n")?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async {
        let result = support::start_dalgona_with_config(dalgona_root.clone(), Some("plugins = [\"ask\"]\n")).await;
        let error = match result {
            Ok(host) => {
                let _ = host.shutdown(Duration::from_secs(2)).await;
                return Err(io::Error::other("Dalgona loaded a plugin from dal's data root").into());
            }
            Err(error) => error.to_string(),
        };
        assert!(error.contains(&dalgona_root.display().to_string()), "{error}");
        assert!(!dalgona_root.join("plugins/ask/plugin.star").exists());
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
