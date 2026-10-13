//! Plugin limits tests.

use super::super::ConfigProduct;
use super::load;

#[test]
fn plugin_limits_default_to_engine_constants() {
    let config = load(ConfigProduct::Dalgon, "").unwrap();
    assert_eq!(
        config.plugin_limits(),
        &crate::config::PluginLimits::default()
    );
    assert_eq!(config.plugin_limits().load_ticks, 1_000_000);
    assert_eq!(config.plugin_limits().stack_depth, 100);
    let custom = load(
        ConfigProduct::Dalgon,
        "[limits.plugins]\nload_ticks = 5\nstack_depth = 50\n",
    )
    .unwrap();
    assert_eq!(custom.plugin_limits().load_ticks, 5);
    assert_eq!(custom.plugin_limits().stack_depth, 50);
    assert_eq!(custom.plugin_limits().handler_ticks, 200_000,);
}
