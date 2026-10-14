//! Edit-style input tests.

use super::super::{ConfigProduct, EditStyleInput};
use super::load;

#[test]
fn config_preserves_edit_style_raw() {
    for value in [
        "simple",
        "replace",
        "balanced",
        "anchor",
        "strict",
        "hashline",
        "hashline-light",
        "hashline-enhanced",
        "apply_patch",
    ] {
        let document = format!("edit_style = {value:?}");
        let config = load(ConfigProduct::Dalgon, &document).expect("raw edit style preserved");
        assert_eq!(
            config.edit_style,
            EditStyleInput::Scalar(Box::<str>::from(value))
        );
    }
}

#[test]
fn edit_style_table_preserves_source_order() {
    let config = load(
        ConfigProduct::Dalgon,
        "[edit_style]\ndefault = \"anchor\"\n\"*kimi*\" = \"replace\"\n\"*a*\" = \"hashline\"\n",
    )
    .unwrap();
    match &config.edit_style {
        EditStyleInput::Table(rows) => {
            let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_ref()).collect();
            assert_eq!(keys, vec!["default", "*kimi*", "*a*"]);
        }
        EditStyleInput::Scalar(_) => {
            panic!("expected table, got {:?}", config.edit_style)
        }
    }
}
