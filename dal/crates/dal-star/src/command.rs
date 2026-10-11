//! Command argument binding for plugin commands (spec §P04).
//!
//! `bind` walks already-lexed `tokens` (`dal_core::command::tokens` owns the
//! lexer), applies the command's `positional` field order, and then hands
//! the assembled object to [`Schema::validate`] — the same schema and
//! defaults a model tool call sees. No glob, variable, command substitution,
//! or shell execution; every literal stays a literal.

use crate::{
    schema::{Field, Schema, SchemaError},
    value::Value,
};

/// One command-binding rejection.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CommandError {
    /// Token grammar or assignment rules were violated.
    #[error("{0}")]
    Usage(String),
    /// The assembled arguments failed schema validation.
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

impl CommandError {
    fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }
}

/// Whether a schema type accepts `--name=value` and `--name value` tokens.
fn is_scalar(schema: &Schema) -> bool {
    matches!(
        schema,
        Schema::Str { .. }
            | Schema::Int { .. }
            | Schema::Num { .. }
            | Schema::Bool
            | Schema::Enum(_)
    )
}

/// Parses one exact scalar token into a transport `Value`.
///
/// Strings take the token verbatim (including an empty string); numbers
/// require an exact parse; booleans never arrive here because `--flag` and
/// `--no-flag` cover them.
fn scalar(token: &str, schema: &Schema, path: &str) -> Result<Value, CommandError> {
    match schema {
        Schema::Str { .. } | Schema::Enum(_) => Ok(Value::Str(token.into())),
        Schema::Int { .. } => token
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| CommandError::usage(format!("`{token}` is not an integer for `{path}`"))),
        Schema::Num { .. } => token
            .parse::<f64>()
            .map(Value::Num)
            .map_err(|_| CommandError::usage(format!("`{token}` is not a number for `{path}`"))),
        _ => Err(CommandError::usage(format!(
            "field `{path}` does not take a scalar token"
        ))),
    }
}

/// Parses `--field=JSON` into a transport `Value` for container fields.
fn container(token: &str, path: &str) -> Result<Value, CommandError> {
    Value::decode(token)
        .map_err(|e| CommandError::usage(format!("`{path}` expects one strict JSON value: {e}")))
}

/// Binds `tokens` against `positional` fields and `schema`, producing the
/// normalized args object.
///
/// Rules (§P04): `positional` names are consumed in order, `--field=value`
/// and `--field value` assign scalars, `--field`/`--no-field` set booleans,
/// containers take `--field=JSON`, `--` ends option parsing, and a field may
/// be assigned at most once.
pub(crate) fn bind(
    tokens: &[Box<str>],
    positional: &[Box<str>],
    schema: &Schema,
) -> Result<Value, CommandError> {
    let Schema::Object(fields) = schema else {
        return Err(CommandError::usage(
            "command schema must be an object of fields",
        ));
    };
    for name in positional {
        let field = fields
            .iter()
            .find(|f| f.name.as_ref() == name.as_ref())
            .ok_or_else(|| {
                CommandError::usage(format!("positional `{name}` is not a schema field"))
            })?;
        if !is_scalar(&field.ty) {
            return Err(CommandError::usage(format!(
                "positional `{name}` must be a scalar field"
            )));
        }
    }

    let mut given: Vec<(Box<str>, Value)> = Vec::new();
    let mut positionals_used = 0usize;
    let mut options_ended = false;
    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        index += 1;
        if options_ended || !token.starts_with("--") || token.as_ref() == "--" {
            if token.as_ref() == "--" && !options_ended {
                options_ended = true;
                continue;
            }
            let Some(field_name) = positional.get(positionals_used) else {
                return Err(CommandError::usage(format!(
                    "unexpected argument `{token}`"
                )));
            };
            let field = field_of(fields, field_name)?;
            let value = scalar(token, &field.ty, field_name)?;
            assign(&mut given, field_name, value)?;
            positionals_used += 1;
            continue;
        }

        let body = &token[2..];
        let (name, inline) = match body.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (body, None),
        };
        if let Some(target) = name.strip_prefix("no-") {
            let field = field_of(fields, target)?;
            if let Schema::Bool = field.ty {
                assign(&mut given, target, Value::Bool(false))?;
                continue;
            }
        }
        let field = field_of(fields, name)?;
        match &field.ty {
            Schema::Bool => {
                if inline.is_some() {
                    return Err(CommandError::usage(format!(
                        "boolean `--{name}` takes no value; use `--{name}` or `--no-{name}`"
                    )));
                }
                assign(&mut given, name, Value::Bool(true))?;
            }
            Schema::List { .. } | Schema::Object(_) => {
                let Some(text) = inline else {
                    return Err(CommandError::usage(format!(
                        "`--{name}` expects one strict JSON value (use --{name}=JSON)"
                    )));
                };
                assign(&mut given, name, container(text, name)?)?;
            }
            _ => {
                let value = if let Some(text) = inline {
                    scalar(text, &field.ty, name)?
                } else {
                    let Some(next) = tokens.get(index) else {
                        return Err(CommandError::usage(format!("`--{name}` expects a value")));
                    };
                    index += 1;
                    scalar(next, &field.ty, name)?
                };
                assign(&mut given, name, value)?;
            }
        }
    }

    if positionals_used < positional.len() {
        let missing = positional[positionals_used..].join(", ");
        return Err(CommandError::usage(format!(
            "missing positional arguments: {missing}"
        )));
    }
    schema
        .validate(&Value::Object(given.into_boxed_slice()), "")
        .map_err(CommandError::Schema)
}

fn field_of<'a>(fields: &'a [Field], name: &str) -> Result<&'a Field, CommandError> {
    fields
        .iter()
        .find(|f| f.name.as_ref() == name)
        .ok_or_else(|| CommandError::usage(format!("unknown option `--{name}`")))
}

fn assign(
    given: &mut Vec<(Box<str>, Value)>,
    name: &str,
    value: Value,
) -> Result<(), CommandError> {
    if given.iter().any(|(seen, _)| seen.as_ref() == name) {
        return Err(CommandError::usage(format!(
            "field `{name}` assigned more than once"
        )));
    }
    given.push((name.into(), value));
    Ok(())
}
