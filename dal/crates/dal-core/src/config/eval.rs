//! The strict `[eval]` authority set and its diagnostic lines (E01).

use super::parse::unknown_key;
use super::{ConfigError, ConfigProduct, invalid_value, value_text};
use crate::ext::{OpSet, UsesError};

/// The effective `[eval]` table: the host `EvalEnvironment` authority.
///
/// Empty when unset, which means pure computation only and fail closed.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct EvalConfig {
    uses: OpSet,
}

impl EvalConfig {
    /// Returns the validated operation authority set.
    #[must_use]
    pub(crate) fn uses(&self) -> &OpSet {
        &self.uses
    }

    /// Parses one `[eval]` table value into validated authority.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the value is not a table, names a key
    /// other than `uses`, or holds an id list [`OpSet::parse`] rejects.
    pub(super) fn parse(product: ConfigProduct, value: toml::Value) -> Result<Self, ConfigError> {
        let table = match value {
            toml::Value::Table(table) => table,
            other => {
                return Err(invalid_value(
                    "eval",
                    value_text(&other),
                    "Use an [eval] table with uses.",
                ));
            }
        };
        for key in table.keys() {
            if key != "uses" {
                return Err(unknown_key(&format!("eval.{key}"), product, None));
            }
        }
        let text: EvalText =
            toml::Value::Table(table)
                .try_into()
                .map_err(|error| ConfigError::Syntax {
                    line: None,
                    message: Box::<str>::from(error.message()),
                })?;
        let Some(uses) = text.uses else {
            return Ok(Self::default());
        };
        let list = match uses {
            toml::Value::Array(list) => list,
            other => {
                return Err(invalid_value(
                    "eval.uses",
                    value_text(&other),
                    "Use an array of operation id strings, such as [\"tools.read\"].",
                ));
            }
        };
        let ids = eval_ids(&list)?;
        OpSet::parse(ids)
            .map(|uses| Self { uses })
            .map_err(uses_invalid)
    }
}

impl serde::Serialize for EvalConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let uses: Vec<String> = self.uses.iter().map(|op| op.to_string()).collect();
        let mut out = serializer.serialize_struct("EvalConfig", 1)?;
        out.serialize_field("uses", &uses)?;
        out.end()
    }
}

impl ConfigError {
    /// Returns the two exact diagnostic lines of an invalid `[eval] uses`
    /// value, or `None` for every other error.
    ///
    /// `program` is the binary name, `dalgon` or `dalgona`. The first line names
    /// the key and the value in TOML form; the second line says what to write.
    #[must_use]
    pub fn eval_lines(&self, program: &str) -> Option<[String; 2]> {
        match self {
            Self::InvalidValue {
                key,
                value,
                expected,
            } if key.as_ref() == "eval.uses" => Some([
                format!("{program}: dal.toml: {key} {value} is invalid"),
                expected.to_string(),
            ]),
            _ => None,
        }
    }
}

/// Collects the string ids of one `uses` array.
fn eval_ids(list: &[toml::Value]) -> Result<Vec<&str>, ConfigError> {
    let mut ids = Vec::with_capacity(list.len());
    for entry in list {
        let toml::Value::String(id) = entry else {
            return Err(invalid_value(
                "eval.uses",
                value_text(entry),
                "Use an array of operation id strings, such as [\"tools.read\"].",
            ));
        };
        ids.push(id.as_str());
    }
    Ok(ids)
}

/// Maps one `uses` list failure to its `eval.uses` diagnostic.
fn uses_invalid(error: UsesError) -> ConfigError {
    match error {
        UsesError::Unknown { id } => invalid_value(
            "eval.uses",
            format!("\"{id}\""),
            "Use an exact operation id from the v1 catalog, such as tools.read.",
        ),
        UsesError::Duplicate { id } => invalid_value(
            "eval.uses",
            format!("\"{id}\""),
            "List each operation id once; remove the duplicate.",
        ),
        UsesError::Wildcard { id } => invalid_value(
            "eval.uses",
            format!("\"{id}\""),
            "Use exact operation ids; wildcards are not accepted.",
        ),
        UsesError::TooMany { count } => invalid_value(
            "eval.uses",
            count.to_string(),
            "Use at most 64 operation ids; remove the extra entries.",
        ),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalText {
    uses: Option<toml::Value>,
}
