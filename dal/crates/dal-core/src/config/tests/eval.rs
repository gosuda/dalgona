use super::super::{Config, ConfigError, ConfigProduct};
use super::{DATA_ROOT, load};
use crate::ext::{NativeOp, OpId};
use std::fmt::Write as _;
use std::path::Path;

fn sixty_four_ids() -> Vec<String> {
    let mut ids: Vec<String> = NativeOp::ALL
        .iter()
        .map(|op| op.as_str().to_string())
        .collect();
    for n in 0..36 {
        ids.push(format!("tools.p{n:02}.l"));
    }
    ids
}

fn uses_toml(ids: &[String]) -> String {
    let mut body = String::from("[eval]\nuses = [");
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            body.push_str(", ");
        }
        write!(body, "{id:?}").expect("writing to a string cannot fail");
    }
    body.push(']');
    body
}

#[test]
fn eval_unset_is_empty_and_pure_only() {
    for product in [ConfigProduct::Dalgon, ConfigProduct::Dalgona] {
        let config = load(product, "").expect("empty config loads");
        assert!(config.eval_uses().is_empty());
        assert_eq!(config.eval_uses().iter().count(), 0);
    }
    let bare = load(ConfigProduct::Dalgona, "[eval]").expect("bare eval table loads");
    assert!(bare.eval_uses().is_empty());
    let empty = load(ConfigProduct::Dalgona, "[eval]\nuses = []").expect("empty uses loads");
    assert!(empty.eval_uses().is_empty());
}

#[test]
fn eval_accepts_exactly_64_ids() {
    let ids = sixty_four_ids();
    assert_eq!(ids.len(), 64);
    let config = load(ConfigProduct::Dalgona, &uses_toml(&ids)).expect("64 ids load");
    assert_eq!(config.eval_uses().iter().count(), 64);
    let read = OpId::parse("tools.read").expect("native id parses");
    assert!(config.eval_uses().contains(&read));
    let export = OpId::parse("tools.p00.l").expect("export id parses");
    assert!(config.eval_uses().contains(&export));
}

#[test]
fn eval_rejects_65_ids() {
    let mut ids = sixty_four_ids();
    ids.push("tools.extra.l".to_string());
    let error = load(ConfigProduct::Dalgona, &uses_toml(&ids)).expect_err("65 ids fail");
    assert!(matches!(
        error,
        ConfigError::InvalidValue {
            ref key,
            ref value,
            ref expected
        } if key.as_ref() == "eval.uses"
            && value.as_ref() == "65"
            && expected.as_ref() == "Use at most 64 operation ids; remove the extra entries."
    ));
    assert_eq!(
        error.eval_lines("dalgona"),
        Some([
            "dalgona: dal.toml: eval.uses 65 is invalid".to_string(),
            "Use at most 64 operation ids; remove the extra entries.".to_string(),
        ])
    );
}

#[test]
fn eval_rejects_duplicate_ids() {
    let error = load(
        ConfigProduct::Dalgona,
        "[eval]\nuses = [\"tools.read\", \"tools.read\"]",
    )
    .expect_err("duplicate ids fail");
    assert!(matches!(
        error,
        ConfigError::InvalidValue {
            ref key,
            ref value,
            ref expected
        } if key.as_ref() == "eval.uses"
            && value.as_ref() == "\"tools.read\""
            && expected.as_ref() == "List each operation id once; remove the duplicate."
    ));
    assert_eq!(
        error.eval_lines("dalgona"),
        Some([
            "dalgona: dal.toml: eval.uses \"tools.read\" is invalid".to_string(),
            "List each operation id once; remove the duplicate.".to_string(),
        ])
    );
}

#[test]
fn eval_rejects_wildcards() {
    let error = load(ConfigProduct::Dalgona, "[eval]\nuses = [\"tools.*\"]")
        .expect_err("wildcard ids fail");
    assert!(matches!(
        error,
        ConfigError::InvalidValue {
            ref key,
            ref value,
            ref expected
        } if key.as_ref() == "eval.uses"
            && value.as_ref() == "\"tools.*\""
            && expected.as_ref() == "Use exact operation ids; wildcards are not accepted."
    ));
    assert_eq!(
        error.eval_lines("dalgona"),
        Some([
            "dalgona: dal.toml: eval.uses \"tools.*\" is invalid".to_string(),
            "Use exact operation ids; wildcards are not accepted.".to_string(),
        ])
    );
}

#[test]
fn eval_rejects_unknown_ids() {
    let error =
        load(ConfigProduct::Dalgona, "[eval]\nuses = [\"nope\"]").expect_err("unknown ids fail");
    assert!(matches!(
        error,
        ConfigError::InvalidValue {
            ref key,
            ref value,
            ref expected
        } if key.as_ref() == "eval.uses"
            && value.as_ref() == "\"nope\""
            && expected.as_ref()
                == "Use an exact operation id from the v1 catalog, such as tools.read."
    ));
    assert_eq!(
        error.eval_lines("dalgona"),
        Some([
            "dalgona: dal.toml: eval.uses \"nope\" is invalid".to_string(),
            "Use an exact operation id from the v1 catalog, such as tools.read.".to_string(),
        ])
    );
}

#[test]
fn eval_accepts_export_ids() {
    let config =
        load(ConfigProduct::Dalgona, "[eval]\nuses = [\"tools.p.l\"]").expect("export id loads");
    let export = OpId::parse("tools.p.l").expect("export id parses");
    assert!(config.eval_uses().contains(&export));
    let missing = OpId::parse("tools.q.l").expect("other export parses");
    assert!(!config.eval_uses().contains(&missing));
}

#[test]
fn eval_user_layer_replaces_product_defaults() {
    let defaults =
        "[eval]\nuses = [\"tools.read\", \"tools.search\"]\n[plugin.web]\nmode = \"grep\"\n";
    let user_toml = "[eval]\nuses = [\"tools.patch\"]\n[plugin.web]\npath = \".\"\n";
    let config = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some(user_toml),
    )
    .expect("product and user layers load");
    let uses: Vec<String> = config.eval_uses().iter().map(|op| op.to_string()).collect();
    assert_eq!(uses, vec!["tools.patch".to_string()]);
    let tables: Vec<_> = config.plugin_configs().collect();
    assert_eq!(tables.len(), 1);
    let (name, web) = tables[0];
    assert_eq!(name, "web");
    assert_eq!(web.get("mode").and_then(toml::Value::as_str), Some("grep"));
    assert_eq!(web.get("path").and_then(toml::Value::as_str), Some("."));

    let kept = Config::load(
        ConfigProduct::Dalgona,
        Path::new(DATA_ROOT),
        defaults,
        Some("mode = \"eval-first\""),
    )
    .expect("user layer without eval keeps defaults");
    let uses: Vec<String> = kept.eval_uses().iter().map(|op| op.to_string()).collect();
    assert_eq!(
        uses,
        vec!["tools.read".to_string(), "tools.search".to_string()]
    );
}

#[test]
fn eval_plugin_tables_are_kept_raw() {
    let config = load(ConfigProduct::Dalgona, "[plugin.web]\nmode = \"grep\"\n")
        .expect("plugin table loads");
    let (name, web) = config.plugin_configs().next().expect("plugin.web is kept");
    assert_eq!(name, "web");
    assert_eq!(web.get("mode").and_then(toml::Value::as_str), Some("grep"));
    assert!(config.eval_uses().is_empty());
    let scalar = load(ConfigProduct::Dalgona, "plugin = 3").expect("scalar plugin stays raw");
    assert_eq!(scalar.plugin_configs().count(), 0);
}

#[test]
fn eval_unknown_keys_suggest_known_spellings() {
    let error = load(ConfigProduct::Dalgona, "evl = 1").expect_err("near-miss key fails");
    assert!(matches!(
        error,
        ConfigError::UnknownKey {
            ref key,
            ref suggestion,
            ..
        } if key.as_ref() == "evl" && suggestion.as_deref() == Some("eval")
    ));

    let error =
        load(ConfigProduct::Dalgona, "[eval]\nusse = []").expect_err("near-miss subkey fails");
    assert!(matches!(
        error,
        ConfigError::UnknownKey {
            ref key,
            ref suggestion,
            ..
        } if key.as_ref() == "eval.usse" && suggestion.as_deref() == Some("eval.uses")
    ));

    let error = load(ConfigProduct::Dalgona, "zzz = 1").expect_err("distant key fails");
    assert!(matches!(
        error,
        ConfigError::UnknownKey {
            ref suggestion,
            ..
        } if suggestion.is_none()
    ));

    let error = load(ConfigProduct::Dalgona, "eval = 3").expect_err("scalar eval fails");
    assert!(matches!(
        error,
        ConfigError::InvalidValue { ref key, .. } if key.as_ref() == "eval"
    ));
    assert_eq!(error.eval_lines("dalgona"), None);
}

#[test]
fn eval_rejects_non_array_uses() {
    let error = load(ConfigProduct::Dalgona, "[eval]\nuses = \"tools.read\"")
        .expect_err("string uses fails");
    assert!(matches!(
        error,
        ConfigError::InvalidValue {
            ref key,
            ref value,
            ref expected
        } if key.as_ref() == "eval.uses"
            && value.as_ref() == "tools.read"
            && expected.as_ref()
                == "Use an array of operation id strings, such as [\"tools.read\"]."
    ));
    assert_eq!(
        error.eval_lines("dalgon"),
        Some([
            "dalgon: dal.toml: eval.uses tools.read is invalid".to_string(),
            "Use an array of operation id strings, such as [\"tools.read\"].".to_string(),
        ])
    );

    let error =
        load(ConfigProduct::Dalgona, "[eval]\nuses = [3]").expect_err("numeric entry fails");
    assert!(matches!(
        error,
        ConfigError::InvalidValue { ref key, ref value, .. }
        if key.as_ref() == "eval.uses" && value.as_ref() == "3"
    ));
}
