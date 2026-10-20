use super::super::{Config, ConfigError, ConfigProduct};
use super::{DATA_ROOT, load};
use std::path::Path;

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

#[test]
fn sandbox_writable_refuses_anything_but_an_array_of_strings() {
    for (document, value) in [
        ("sandbox_writable = \"/srv/data\"", "/srv/data"),
        ("sandbox_writable = 1", "1"),
        ("sandbox_writable = true", "true"),
        ("sandbox_writable = [\"/a\", 2]", "2"),
        ("sandbox_writable = [1]", "1"),
        ("sandbox_writable = [[\"/a\"]]", "[\"/a\"]"),
        ("sandbox_writable = [true, \"/a\"]", "true"),
    ] {
        let error = load(ConfigProduct::Dalgona, document).expect_err(document);
        assert!(
            matches!(
                &error,
                ConfigError::InvalidValue { key, value: shown, expected }
                    if key.as_ref() == "sandbox_writable"
                        && shown.as_ref() == value
                        && expected.as_ref() == "array of path strings"
            ),
            "{document}: {error:?}"
        );
    }
}

#[test]
fn sandbox_writable_keeps_order_and_duplicates_for_the_engine_to_resolve() {
    let config = load(
        ConfigProduct::Dalgona,
        "sandbox_writable = [\"/b\", \"/a\", \"/b\"]",
    )
    .expect("writable roots load");
    assert_eq!(
        config.sandbox_writable(),
        [
            Box::<str>::from("/b"),
            Box::<str>::from("/a"),
            Box::<str>::from("/b"),
        ]
        .as_slice(),
        "config order is kept; resolving and deduplicating belong to the sandbox"
    );
}

#[test]
fn a_later_sandbox_writable_layer_replaces_the_list_instead_of_extending_it() {
    let defaults = "sandbox_writable = [\"/product\"]\n";
    let replaced = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some("sandbox_writable = [\"/user\"]\n"),
    )
    .expect("both layers load");
    assert_eq!(
        replaced.sandbox_writable(),
        [Box::<str>::from("/user")].as_slice()
    );
    let cleared = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some("sandbox_writable = []\n"),
    )
    .expect("an explicit empty list loads");
    assert_eq!(
        cleared.sandbox_writable(),
        [],
        "an explicit empty user list withdraws the product roots"
    );
    let kept = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some("mode = \"normal\"\n"),
    )
    .expect("a user layer without the key loads");
    assert_eq!(
        kept.sandbox_writable(),
        [Box::<str>::from("/product")].as_slice()
    );
}
