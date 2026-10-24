//! Typed product settings the interactive client may edit in `dal.toml`.
//!
//! A product describes its documented, non-secret settings as
//! [`ProductSetting`] rows built from its own decoded configuration. Every row
//! is startup-only: the product reads it when the host starts, so a saved
//! edit takes effect on the next start. The edit itself is a
//! [`SettingEdit`] applied to the user document by [`edit_user_toml`], which
//! changes one dotted key and keeps comments, decor and unrelated tables.

use super::{ConfigError, invalid_value};

/// One setting value as the product decoded it or as the user edits it.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Integer(i64),
    /// A floating-point number.
    Number(f64),
    /// A string.
    Text(Box<str>),
    /// A list of names; an edit replaces the whole list.
    List(Vec<Box<str>>),
}

impl SettingValue {
    fn to_toml(&self) -> toml_edit::Value {
        match self {
            Self::Bool(value) => (*value).into(),
            Self::Integer(value) => (*value).into(),
            Self::Number(value) => (*value).into(),
            Self::Text(value) => value.as_ref().into(),
            Self::List(values) => {
                toml_edit::Value::Array(values.iter().map(AsRef::<str>::as_ref).collect())
            }
        }
    }
}

impl std::fmt::Display for SettingValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(true) => f.write_str("on"),
            Self::Bool(false) => f.write_str("off"),
            Self::Integer(value) => write!(f, "{value}"),
            Self::Number(value) => write!(f, "{value}"),
            Self::Text(value) => f.write_str(value),
            Self::List(values) if values.is_empty() => f.write_str("none"),
            Self::List(values) => f.write_str(&values.join(", ")),
        }
    }
}

/// How the client edits a setting's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingControl {
    /// Flips a [`SettingValue::Bool`].
    Toggle,
    /// Picks one of the named values, stored as [`SettingValue::Text`]; the
    /// product decoder still validates the saved value.
    Choice(Vec<Box<str>>),
    /// Accepts a typed [`SettingValue::Integer`]; the product decoder owns the range.
    Integer,
    /// Accepts a typed [`SettingValue::Number`]; the product decoder owns the range.
    Number,
    /// Accepts typed non-empty [`SettingValue::Text`]; the product decoder
    /// owns the accepted spellings.
    Text,
    /// Checks any subset of the named values, stored as [`SettingValue::List`].
    Subset(Vec<Box<str>>),
}

impl SettingControl {
    /// Parses text typed for an [`Integer`](Self::Integer),
    /// [`Number`](Self::Number), or [`Text`](Self::Text) setting.
    ///
    /// # Errors
    /// Returns the expected shape when `text` does not parse, or when the
    /// control does not take typed text.
    pub fn parse(&self, text: &str) -> Result<SettingValue, &'static str> {
        let text = text.trim();
        match self {
            Self::Integer => text
                .parse()
                .map(SettingValue::Integer)
                .map_err(|_| "expected a whole number"),
            Self::Number => text
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(SettingValue::Number)
                .ok_or("expected a number"),
            Self::Text if text.is_empty() => Err("expected a non-empty value"),
            Self::Text => Ok(SettingValue::Text(text.into())),
            Self::Toggle | Self::Choice(_) | Self::Subset(_) => {
                Err("pick one of the listed values")
            }
        }
    }
}

/// One documented, non-secret, startup-only product setting.
#[derive(Clone, Debug, PartialEq)]
pub struct ProductSetting {
    /// The dotted `dal.toml` path, such as `plugin.web.timeout_secs`.
    pub key: Box<str>,
    /// The display group, usually the owning battery.
    pub group: Box<str>,
    /// The short display label.
    pub label: Box<str>,
    /// The saved value the next start reads: every configuration layer of
    /// the latest user `dal.toml`, without command-line overrides.
    pub value: SettingValue,
    /// The value the running host started with, when it differs from
    /// [`value`](Self::value); `None` when the two agree.
    pub running: Option<SettingValue>,
    /// How the client edits the value.
    pub control: SettingControl,
}

/// One requested change: set `key` to `value` in the user `dal.toml`.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingEdit {
    /// The dotted `dal.toml` path.
    pub key: Box<str>,
    /// The new value; a list replaces the existing list whole.
    pub value: SettingValue,
}

/// Applies `edit` to the user TOML document and returns the new text.
///
/// Missing tables are created; comments, value decor, and unrelated keys and
/// tables are kept. The result is not validated: callers load it with
/// [`Config::load`](super::Config::load) and the product decoder before
/// writing it.
///
/// # Errors
/// Returns [`ConfigError::Syntax`] when the document does not parse and
/// [`ConfigError::InvalidValue`] when a path member is not a table.
pub fn edit_user_toml(user_toml: Option<&str>, edit: &SettingEdit) -> Result<String, ConfigError> {
    edit_user_value(user_toml, &edit.key, |_| Ok(edit.value.to_toml()))
}

/// Sets the dotted `key` in the user document to the value `replace` returns
/// for the existing value, keeping the existing value's decor.
pub(super) fn edit_user_value(
    user_toml: Option<&str>,
    key: &str,
    replace: impl FnOnce(Option<&toml_edit::Value>) -> Result<toml_edit::Value, ConfigError>,
) -> Result<String, ConfigError> {
    let mut document = user_toml
        .unwrap_or_default()
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| ConfigError::Syntax {
            line: None,
            message: error.to_string().into_boxed_str(),
        })?;
    let (parents, leaf) = key
        .rsplit_once('.')
        .map_or((None, key), |(parents, leaf)| (Some(parents), leaf));
    let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
    let mut path = String::new();
    for member in parents.into_iter().flat_map(|parents| parents.split('.')) {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(member);
        if !table.contains_key(member) {
            let mut created = toml_edit::Table::new();
            created.set_implicit(true);
            table.insert(member, toml_edit::Item::Table(created));
        }
        table = table
            .get_mut(member)
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| invalid_value(&path, "non-table", "table"))?;
    }
    if !table.contains_key(leaf) {
        table.insert(leaf, toml_edit::Item::Value(replace(None)?));
        return Ok(document.to_string());
    }
    let value = table
        .get_mut(leaf)
        .and_then(toml_edit::Item::as_value_mut)
        .ok_or_else(|| invalid_value(key, "table", "a value"))?;
    let decor = value.decor().clone();
    let mut replacement = replace(Some(value))?;
    *replacement.decor_mut() = decor;
    *value = replacement;
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::{SettingControl, SettingEdit, SettingValue, edit_user_toml};
    use crate::config::ConfigError;

    fn edit(key: &str, value: SettingValue) -> SettingEdit {
        SettingEdit {
            key: key.into(),
            value,
        }
    }

    #[test]
    fn an_edit_changes_only_its_key_and_keeps_comments_and_decor() {
        let user = "# my settings\nmodel = \"a/b\"\n\n[plugin.web]\n# keep\ntimeout_secs = 30 # inline\nprovider = \"brave\"\n\n[plugin.user_tool]\nanything = 1\n";
        let updated = edit_user_toml(
            Some(user),
            &edit("plugin.web.timeout_secs", SettingValue::Integer(60)),
        )
        .expect("edit applies");
        assert_eq!(
            updated,
            user.replace("timeout_secs = 30 # inline", "timeout_secs = 60 # inline")
        );
    }

    #[test]
    fn missing_tables_are_created_and_lists_replace_whole() {
        let updated = edit_user_toml(
            None,
            &edit(
                "plugin.orchestration.monitor.enabled",
                SettingValue::Bool(false),
            ),
        )
        .expect("edit creates tables");
        assert_eq!(updated, "[plugin.orchestration.monitor]\nenabled = false\n");
        let replaced = edit_user_toml(
            Some("disabled_batteries = [\"web\", \"mcp\"] # off\n"),
            &edit(
                "disabled_batteries",
                SettingValue::List(vec!["review".into()]),
            ),
        )
        .expect("list edit applies");
        assert_eq!(replaced, "disabled_batteries = [\"review\"] # off\n");
    }

    #[test]
    fn a_non_table_path_member_and_a_broken_document_are_refused() {
        assert!(matches!(
            edit_user_toml(
                Some("plugin = 1\n"),
                &edit("plugin.web.enabled", SettingValue::Bool(true))
            ),
            Err(ConfigError::InvalidValue { .. })
        ));
        assert!(matches!(
            edit_user_toml(
                Some("[plugin\n"),
                &edit("plugin.web.enabled", SettingValue::Bool(true))
            ),
            Err(ConfigError::Syntax { .. })
        ));
    }

    #[test]
    fn typed_entries_parse_by_control() {
        assert_eq!(
            SettingControl::Integer.parse(" 42 "),
            Ok(SettingValue::Integer(42))
        );
        assert!(SettingControl::Integer.parse("4.2").is_err());
        assert_eq!(
            SettingControl::Number.parse("0.5"),
            Ok(SettingValue::Number(0.5))
        );
        assert!(SettingControl::Number.parse("NaN").is_err());
        assert!(
            SettingControl::Choice(vec!["brave".into()])
                .parse("brave")
                .is_err()
        );
        assert!(SettingControl::Toggle.parse("true").is_err());
        assert_eq!(
            SettingControl::Text.parse(" main "),
            Ok(SettingValue::Text("main".into()))
        );
        assert!(SettingControl::Text.parse("  ").is_err());
    }
}
