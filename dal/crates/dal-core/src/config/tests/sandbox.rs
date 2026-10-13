use super::super::ConfigProduct;
use super::load;

#[test]
fn sandbox_writable_reads_through_toml_layer() {
    let config = load(
        ConfigProduct::Dalgona,
        "sandbox_writable = [\"/srv/data\", \"~/extra\"]",
    )
    .expect("writable roots load");
    assert_eq!(
        config.sandbox_writable(),
        [Box::<str>::from("/srv/data"), Box::<str>::from("~/extra")].as_slice(),
    );
    let empty = load(ConfigProduct::Dalgona, "").expect("empty config loads");
    assert_eq!(empty.sandbox_writable(), []);
}
