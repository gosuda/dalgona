//! Shared fixtures for plugin-system integration tests.

#![expect(clippy::expect_used, reason = "SC test")]

use std::collections::BTreeMap;
use std::path::Path;

use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};
use tempfile::TempDir;

pub(crate) fn system_with_plugin(name: &str, source: &str) -> (TempDir, PluginSystem) {
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

pub(crate) fn write_plugin(data_root: &Path, name: &str, source: &str) {
    let directory = data_root.join("plugins").join(name);
    std::fs::create_dir_all(&directory).expect("plugin directory");
    std::fs::write(directory.join("plugin.star"), source).expect("plugin source");
}
