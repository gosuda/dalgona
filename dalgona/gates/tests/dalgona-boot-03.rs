// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies that disabling MCP removes only its records.
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

#[test]
fn disabled_mcp_removes_its_records_and_nothing_else() -> support::TestResult<()> {
    let scratch = support::Scratch::new("disabled-mcp")?;
    let root = scratch.path().to_path_buf();
    let default_product = support::build_product(root.clone(), None)?;
    let disabled = support::build_product(root, Some("disabled_batteries = [\"mcp\"]\n"))?;
    let expected = support::battery_names(&default_product);
    assert!(expected.contains("mcp"));
    let mut without = expected.clone();
    without.remove("mcp");
    assert_eq!(support::battery_names(&disabled), without);
    Ok(())
}
