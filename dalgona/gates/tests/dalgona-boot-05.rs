// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use gates::support;

#[test]
fn experimental_battery_is_a_no_op() -> support::TestResult<()> {
    let scratch = support::Scratch::new("experimental-battery")?;
    let root = scratch.path().to_path_buf();
    let default_product = support::build_product(root.clone(), None)?;
    let experimental = support::build_product(root, Some("experimental_batteries = [\"ask\"]\n"))?;
    let expected = support::battery_names(&default_product);
    assert_eq!(expected.len(), support::BATTERIES.len());
    assert_eq!(support::battery_names(&experimental), expected);
    Ok(())
}
