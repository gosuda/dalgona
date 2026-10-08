//! Shared fixtures for the dal-star integration tests.

#![expect(
    clippy::expect_used,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]

use std::collections::BTreeMap;
use std::path::Path;

use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};
use tempfile::TempDir;

/// Builds a plugin system over one written plugin fixture.
///
/// # Panics
///
/// Panics if the fixture cannot be written or the generated plugin fails to
/// load; every caller depends on the fixture being valid.
#[must_use]
pub fn system_with_plugin(name: &str, source: &str) -> (TempDir, PluginSystem) {
    let data = tempfile::tempdir().expect("temporary plugin root");
    write_plugin(data.path(), name, source);
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let config = PluginsConfig {
        enabled: Vec::new(),
        limits: dal_core::PluginLimits::default(),
        configs: BTreeMap::new(),
    };
    let generation = load(&roots, &config).expect("valid plugin fixture loads");
    (data, PluginSystem::new(generation, roots, config))
}

/// Writes one plugin fixture under the data root.
///
/// # Panics
///
/// Panics if the plugin directory cannot be created or the source cannot be
/// written.
pub fn write_plugin(data_root: &Path, name: &str, source: &str) {
    let directory = data_root.join("plugins").join(name);
    std::fs::create_dir_all(&directory).expect("plugin directory");
    std::fs::write(directory.join("plugin.star"), source).expect("plugin source");
}
