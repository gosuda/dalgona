//! The normalized tool schema (spec §R05, §P04).
//!
//! One `Schema` serves provider requests, command binding, native calls and
//! generated docs. SDK constructors build and validate it at load; `validate`
//! applies the same rules to every call site: no unknown fields, no
//! coercion, no truthy-to-boolean conversion. Defaults live in the schema so
//! a documented default and an enforced default can never diverge.

use std::fmt::{self, Write as _};

use allocative::Allocative;
use dal_core::RawJson;

use crate::value::Value;

/// A normalized field type.
#[derive(Clone, Debug, Allocative)]
pub(crate) enum Schema {
    /// UTF-8 text with optional length bounds (in characters).
    Str {
        /// Inclusive minimum length.
        min_len: Option<usize>,
        /// Inclusive maximum length.
        max_len: Option<usize>,
    },
    /// Signed integer with optional bounds.
    Int {
        /// Inclusive minimum.
        min: Option<i64>,
        /// Inclusive maximum.
        max: Option<i64>,
    },
    /// Double with optional bounds.
    Num {
        /// Inclusive minimum.
        min: Option<f64>,
        /// Inclusive maximum.
        max: Option<f64>,
    },
    /// `True`/`False`.
    Bool,
    /// One of a fixed set of strings, order preserved for docs.
    Enum(Box<[Box<str>]>),
    /// A sequence with element schema and optional length bounds.
    List {
        /// The element schema.
        item: Box<Schema>,
        /// Inclusive minimum element count.
        min_len: Option<usize>,
        /// Inclusive maximum element count.
        max_len: Option<usize>,
    },
    /// A record of named fields; extra fields are rejected.
    Object(Box<[Field]>),
}

/// One object field: name, type and presence rule.
#[derive(Clone, Debug, Allocative)]
pub(crate) struct Field {
    /// The field name; a valid Starlark identifier, never `_`-prefixed.
    pub(crate) name: Box<str>,
    /// The field schema.
    pub(crate) ty: Schema,
    /// Whether the field may be absent, and what absence means.
    pub(crate) presence: Presence,
}

/// How an absent field is treated at validation.
#[derive(Clone, Debug, Allocative)]
pub(crate) enum Presence {
    /// The field must be present.
    Required,
    /// Absent fields receive this copy of the default.
    Default(Value),
    /// Absent fields are omitted from the args record (the `Missing` case).
    Optional,
    /// `None` is a valid value; the field itself is still required.
    Nullable,
    /// `None` is valid, and absent fields receive this default.
    NullableDefault(Value),
}

/// One schema rejection, carrying the failing site.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SchemaError {
    /// The schema itself is malformed (constructor-side failure).
    #[error("schema {path}: {message}")]
    Schema {
        /// The field path where the defect sits.
        path: String,
        /// What is wrong with it.
        message: String,
    },
    /// A value failed validation against the schema.
    #[error("argument {path}: {message}")]
    Argument {
        /// The field path where the failure sits.
        path: String,
        /// What is wrong with it.
        message: String,
    },
}

impl SchemaError {
    fn schema(path: &str, message: impl Into<String>) -> Self {
        Self::Schema {
            path: path.into(),
            message: message.into(),
        }
    }

    fn argument(path: &str, message: impl Into<String>) -> Self {
        Self::Argument {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Starlark keywords that cannot name a field (spec §P04: identifiers that
/// collide with the language are unreachable through `args.<name>`).
const ST_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "break", "continue", "def", "elif", "else", "for", "if", "in",
    "lambda", "load", "not", "or", "pass", "return",
];

/// True for a valid Starlark identifier `[A-Za-z_][A-Za-z0-9_]*` that is not
/// private (`_`-prefixed) and not a keyword.
fn is_public_ident(name: &str) -> bool {
    if ST_KEYWORDS.contains(&name) {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Field {
    /// Constructor validation (§P04): names are public identifiers.
    pub(crate) fn check(&self, path: &str) -> Result<(), SchemaError> {
        let here = if path.is_empty() {
            self.name.to_string()
        } else {
            format!("{path}.{}", self.name)
        };
        if !is_public_ident(&self.name) {
            return Err(SchemaError::schema(
                &here,
                "field name must be a public identifier",
            ));
        }
        self.ty.check(&here)?;
        if let Presence::Default(default) | Presence::NullableDefault(default) = &self.presence {
            let valid = if matches!(default, Value::Null) {
                matches!(self.presence, Presence::NullableDefault(_))
            } else {
                self.ty.validate(default, &here).is_ok()
            };
            if !valid {
                return Err(SchemaError::schema(
                    &here,
                    "default does not satisfy its field schema",
                ));
            }
        }
        Ok(())
    }
}

impl Schema {
    /// Validates the schema itself at load time.
    pub(crate) fn check(&self, path: &str) -> Result<(), SchemaError> {
        match self {
            Self::Str { min_len, max_len } => {
                if let (Some(min), Some(max)) = (min_len, max_len)
                    && min > max
                {
                    return Err(SchemaError::schema(
                        path,
                        format!("min_len {min} exceeds max_len {max}"),
                    ));
                }
            }
            Self::Int { min, max } => {
                if let (Some(min), Some(max)) = (min, max)
                    && min > max
                {
                    return Err(SchemaError::schema(
                        path,
                        format!("min {min} exceeds max {max}"),
                    ));
                }
            }
            Self::Num { min, max } => {
                if let (Some(min), Some(max)) = (min, max)
                    && min > max
                {
                    return Err(SchemaError::schema(
                        path,
                        format!("min {min} exceeds max {max}"),
                    ));
                }
            }
            Self::Enum(values) => {
                if values.is_empty() {
                    return Err(SchemaError::schema(path, "enum needs at least one value"));
                }
                let mut seen = std::collections::BTreeSet::new();
                for value in values {
                    if !seen.insert(value.as_ref()) {
                        return Err(SchemaError::schema(
                            path,
                            format!("duplicate enum value `{value}`"),
                        ));
                    }
                }
            }
            Self::List {
                item,
                min_len,
                max_len,
            } => {
                if let (Some(min), Some(max)) = (min_len, max_len)
                    && min > max
                {
                    return Err(SchemaError::schema(
                        path,
                        format!("min_len {min} exceeds max_len {max}"),
                    ));
                }
                item.check(&format!("{path}[]"))?;
            }
            Self::Object(fields) => {
                let mut seen = std::collections::BTreeSet::new();
                for field in fields {
                    if !seen.insert(field.name.as_ref()) {
                        return Err(SchemaError::schema(
                            path,
                            format!("duplicate field `{}`", field.name),
                        ));
                    }
                    field.check(path)?;
                }
            }
            Self::Bool => {}
        }
        Ok(())
    }

    /// Whether the type accepts scalar command tokens (`--f=v`, `--f v`,
    /// `--f`/`--no-f` for booleans, verbatim strings and enum literals);
    /// object and list are container forms bound through JSON per §P04.
    pub(crate) fn is_scalar(&self) -> bool {
        matches!(
            self,
            Self::Str { .. } | Self::Int { .. } | Self::Num { .. } | Self::Bool | Self::Enum(_)
        )
    }

    /// Validates `value`, applying defaults, and returns the normalized form.
    ///
    /// Missing optional fields stay absent in the returned object; the
    /// record projection inserts `Missing` for them. `Missing` itself never
    /// appears here: presence rules decide at the object level.
    pub(crate) fn validate(&self, value: &Value, path: &str) -> Result<Value, SchemaError> {
        match (self, value) {
            (Self::Str { min_len, max_len }, Value::Str(text)) => {
                Self::validate_string(text, min_len.as_ref(), max_len.as_ref(), path)
            }
            (Self::Int { min, max }, Value::Int(number)) => {
                Self::validate_integer(*number, min.as_ref(), max.as_ref(), path)
            }
            (Self::Num { min, max }, Value::Num(number)) => {
                Self::validate_number(*number, min.as_ref(), max.as_ref(), path)
            }
            (Self::Bool, Value::Bool(flag)) => Ok(Value::Bool(*flag)),
            (Self::Enum(values), Value::Str(text)) => Self::validate_enum(values, text, path),
            (
                Self::List {
                    item,
                    min_len,
                    max_len,
                },
                Value::List(items),
            ) => Self::validate_list(item, min_len.as_ref(), max_len.as_ref(), items, path),
            (Self::Object(schema_fields), Value::Object(given)) => {
                Self::validate_object(schema_fields, given, path)
            }
            (_, Value::Null) => Err(SchemaError::argument(
                path,
                format!("expected {}, got null", self.kind()),
            )),
            (expected, got) => Err(SchemaError::argument(
                path,
                format!("expected {}, got {}", expected.kind(), kind_of(got)),
            )),
        }
    }

    fn validate_string(
        text: &str,
        min_len: Option<&usize>,
        max_len: Option<&usize>,
        path: &str,
    ) -> Result<Value, SchemaError> {
        let len = text.chars().count();
        if let Some(min) = min_len
            && len < *min
        {
            return Err(SchemaError::argument(
                path,
                format!("string length {len} below min_len {min}"),
            ));
        }
        if let Some(max) = max_len
            && len > *max
        {
            return Err(SchemaError::argument(
                path,
                format!("string length {len} above max_len {max}"),
            ));
        }
        Ok(Value::Str(text.into()))
    }

    fn validate_integer(
        number: i64,
        min: Option<&i64>,
        max: Option<&i64>,
        path: &str,
    ) -> Result<Value, SchemaError> {
        if let Some(min) = min
            && number < *min
        {
            return Err(SchemaError::argument(
                path,
                format!("{number} below min {min}"),
            ));
        }
        if let Some(max) = max
            && number > *max
        {
            return Err(SchemaError::argument(
                path,
                format!("{number} above max {max}"),
            ));
        }
        Ok(Value::Int(number))
    }

    fn validate_number(
        number: f64,
        min: Option<&f64>,
        max: Option<&f64>,
        path: &str,
    ) -> Result<Value, SchemaError> {
        if !number.is_finite() {
            return Err(SchemaError::argument(path, "non-finite number"));
        }
        if let Some(min) = min
            && number < *min
        {
            return Err(SchemaError::argument(
                path,
                format!("{number} below min {min}"),
            ));
        }
        if let Some(max) = max
            && number > *max
        {
            return Err(SchemaError::argument(
                path,
                format!("{number} above max {max}"),
            ));
        }
        Ok(Value::Num(number))
    }

    fn validate_enum(values: &[Box<str>], text: &str, path: &str) -> Result<Value, SchemaError> {
        if values.iter().any(|value| value.as_ref() == text) {
            Ok(Value::Str(text.into()))
        } else {
            Err(SchemaError::argument(
                path,
                format!("`{text}` is not one of {}", values.join(", ")),
            ))
        }
    }

    fn validate_list(
        item: &Schema,
        min_len: Option<&usize>,
        max_len: Option<&usize>,
        items: &[Value],
        path: &str,
    ) -> Result<Value, SchemaError> {
        if let Some(min) = min_len
            && items.len() < *min
        {
            return Err(SchemaError::argument(
                path,
                format!("list length {} below min_len {min}", items.len()),
            ));
        }
        if let Some(max) = max_len
            && items.len() > *max
        {
            return Err(SchemaError::argument(
                path,
                format!("list length {} above max_len {max}", items.len()),
            ));
        }
        let mut out = Vec::with_capacity(items.len());
        for (index, element) in items.iter().enumerate() {
            out.push(item.validate(element, &format!("{path}[{index}]"))?);
        }
        Ok(Value::List(out.into_boxed_slice()))
    }

    fn validate_object(
        schema_fields: &[Field],
        given: &[(Box<str>, Value)],
        path: &str,
    ) -> Result<Value, SchemaError> {
        let mut out: Vec<(Box<str>, Value)> = Vec::with_capacity(schema_fields.len());
        for field in schema_fields {
            let found = given
                .iter()
                .find(|(name, _)| name.as_ref() == field.name.as_ref());
            let field_path = if path.is_empty() {
                field.name.to_string()
            } else {
                format!("{path}.{}", field.name)
            };
            match (found, &field.presence) {
                (Some((_, value)), _) => {
                    let normalized = match &field.presence {
                        Presence::Nullable | Presence::NullableDefault(_)
                            if matches!(value, Value::Null) =>
                        {
                            Value::Null
                        }
                        _ => field.ty.validate(value, &field_path)?,
                    };
                    out.push((field.name.clone(), normalized));
                }
                (None, Presence::Required) => {
                    return Err(SchemaError::argument(
                        &field_path,
                        "required field is missing",
                    ));
                }
                (None, Presence::Default(default) | Presence::NullableDefault(default)) => {
                    out.push((field.name.clone(), default.clone()));
                }
                (None, Presence::Optional) => {}
                (None, Presence::Nullable) => {
                    return Err(SchemaError::argument(
                        &field_path,
                        "nullable field must be present (use None explicitly)",
                    ));
                }
            }
        }
        for (name, _) in given {
            if !schema_fields
                .iter()
                .any(|field| field.name.as_ref() == name.as_ref())
            {
                return Err(SchemaError::argument(
                    path,
                    format!("unknown field `{name}`"),
                ));
            }
        }
        Ok(Value::Object(out.into_boxed_slice()))
    }

    /// The plain-English type name for error text.
    fn kind(&self) -> &'static str {
        match self {
            Self::Str { .. } => "string",
            Self::Int { .. } => "integer",
            Self::Num { .. } => "number",
            Self::Bool => "boolean",
            Self::Enum(_) => "enum value",
            Self::List { .. } => "list",
            Self::Object(_) => "object",
        }
    }

    /// The provider/docs form as raw JSON schema text.
    ///
    /// A malformed schema is a load-time bug; the writer below produces only
    /// valid text, so a parse failure here is unreachable in practice but
    /// still surfaces rather than degrading to an accepting `null` schema.
    pub(crate) fn to_json_schema(&self) -> Result<RawJson, SchemaError> {
        let mut out = String::new();
        self.write_json_schema(&mut out);
        RawJson::parse(&out)
            .map_err(|_| SchemaError::schema("", "internal: emitted invalid JSON schema"))
    }

    fn write_json_schema(&self, out: &mut String) {
        match self {
            Self::Str { min_len, max_len } => {
                out.push_str("{\"type\":\"string\"");
                if let Some(min) = min_len {
                    let _ = write!(out, ",\"minLength\":{min}");
                }
                if let Some(max) = max_len {
                    let _ = write!(out, ",\"maxLength\":{max}");
                }
                out.push('}');
            }
            Self::Int { min, max } => {
                out.push_str("{\"type\":\"integer\"");
                if let Some(min) = min {
                    let _ = write!(out, ",\"minimum\":{min}");
                }
                if let Some(max) = max {
                    let _ = write!(out, ",\"maximum\":{max}");
                }
                out.push('}');
            }
            Self::Num { min, max } => {
                out.push_str("{\"type\":\"number\"");
                if let Some(min) = min {
                    let _ = write!(out, ",\"minimum\":{min}");
                }
                if let Some(max) = max {
                    let _ = write!(out, ",\"maximum\":{max}");
                }
                out.push('}');
            }
            Self::Bool => out.push_str("{\"type\":\"boolean\"}"),
            Self::Enum(values) => {
                out.push_str("{\"type\":\"string\",\"enum\":[");
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&sonic_rs::to_string(value).unwrap_or_default());
                }
                out.push_str("]}");
            }
            Self::List {
                item,
                min_len,
                max_len,
            } => {
                out.push_str("{\"type\":\"array\",\"items\":");
                item.write_json_schema(out);
                if let Some(min) = min_len {
                    let _ = write!(out, ",\"minItems\":{min}");
                }
                if let Some(max) = max_len {
                    let _ = write!(out, ",\"maxItems\":{max}");
                }
                out.push('}');
            }
            Self::Object(fields) => {
                out.push_str("{\"type\":\"object\",\"properties\":{");
                let mut required = Vec::new();
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&sonic_rs::to_string(&field.name).unwrap_or_default());
                    out.push(':');
                    let mut ty = String::new();
                    field.ty.write_json_schema(&mut ty);
                    let nullable = matches!(
                        field.presence,
                        Presence::Nullable | Presence::NullableDefault(_)
                    );
                    if nullable {
                        // Strip the outer braces to wrap in anyOf.
                        let inner = ty.trim_start_matches('{').trim_end_matches('}');
                        let _ = write!(out, "{{\"anyOf\":[{{{inner}}},{{\"type\":\"null\"}}]}}");
                    } else {
                        out.push_str(&ty);
                    }
                    if matches!(field.presence, Presence::Required | Presence::Nullable) {
                        required.push(&field.name);
                    }
                }
                out.push_str("},\"required\":[");
                for (index, name) in required.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&sonic_rs::to_string(name).unwrap_or_default());
                }
                out.push_str("],\"additionalProperties\":false}");
            }
        }
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Int(_) => "integer",
        Value::Num(_) => "number",
        Value::Str(_) => "string",
        Value::List(_) => "list",
        Value::Object(_) => "object",
    }
}

impl fmt::Display for Presence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Required => "required",
            Self::Default(_) => "defaulted",
            Self::Optional => "optional",
            Self::Nullable => "nullable",
            Self::NullableDefault(_) => "nullable defaulted",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Field, Presence, Schema};
    use crate::value::Value;

    fn int_list(items: Vec<Value>) -> Value {
        Value::List(items.into_boxed_slice())
    }

    #[test]
    fn list_elements_are_validated_against_the_item_schema() {
        let schema = Schema::List {
            item: Box::new(Schema::Int {
                min: None,
                max: None,
            }),
            min_len: None,
            max_len: None,
        };
        assert!(schema.check("").is_ok(), "the list schema itself is valid");
        let error = schema
            .validate(&int_list(vec![Value::Int(1), Value::Str("bad".into())]), "")
            .expect_err("a string element must fail an Int list");
        let text = error.to_string();
        assert!(
            text.contains("[1]"),
            "error must name the element index: {text}"
        );
    }

    #[test]
    fn object_defaults_must_satisfy_their_field_schema() {
        let schema = Schema::Object(Box::new([Field {
            name: "count".into(),
            ty: Schema::Int {
                min: Some(0),
                max: None,
            },
            presence: Presence::Default(Value::Int(-1)),
        }]));
        let error = schema
            .check("")
            .expect_err("a default below min is malformed");
        assert!(error.to_string().contains("default"), "got: {error}");
    }

    #[test]
    fn keyword_names_are_rejected_in_object_fields() {
        let schema = Schema::Object(Box::new([Field {
            name: "if".into(),
            ty: Schema::Bool,
            presence: Presence::Optional,
        }]));
        schema
            .check("")
            .expect_err("`if` is not a usable field name");
    }
}
