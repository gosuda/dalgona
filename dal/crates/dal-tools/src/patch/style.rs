//! Ordered edit-style configuration and model matching.

use regex::{Regex, RegexBuilder};

use super::ir::{DialectId, Tier};

/// Raw ordered edit-style input; the patch consumer owns model glob rows.
/// Re-exported from the core config vocabulary (single definition).
pub use dal_core::EditStyleInput;

/// A malformed edit-style value in `config.toml`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigError {
    /// A scalar or table value is not a recognized style or tier alias.
    #[error(
        "config.toml: edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict."
    )]
    InvalidStyle,
    /// A model table has no literal `default` entry.
    #[error("config.toml: edit_style table needs a default entry.")]
    MissingDefault,
    /// A model table contains an empty key.
    #[error("config.toml: edit_style keys must not be empty.")]
    EmptyKey,
}

/// One ordered model-glob row; first match wins at pick time.
#[derive(Clone, Debug)]
pub struct ModelRule {
    pattern: Regex,
    dialect: DialectId,
}

/// A validated fixed style or an ordered model-to-style table.
#[derive(Clone, Debug)]
pub enum EditStyleConfig {
    /// One style for every model.
    Scalar(DialectId),
    /// First matching model glob wins; `default` is the fallback.
    Table {
        /// Fallback style for models with no matching row.
        default: DialectId,
        /// Non-default patterns in their original configuration order.
        rules: Vec<ModelRule>,
    },
}

/// Parses raw edit-style configuration without sorting model rows.
///
/// # Errors
/// Returns the exact config error for an unknown value, missing default, or
/// an empty key.
pub fn parse_edit_style(input: &EditStyleInput) -> Result<EditStyleConfig, ConfigError> {
    match input {
        EditStyleInput::Scalar(style) => parse_style(style).map(EditStyleConfig::Scalar),
        EditStyleInput::Table(rows) => {
            if rows.iter().any(|(key, _)| key.is_empty()) {
                return Err(ConfigError::EmptyKey);
            }
            let default = rows
                .iter()
                .find(|(key, _)| key.as_ref() == "default")
                .map(|(_, style)| style)
                .ok_or(ConfigError::MissingDefault)?;
            let default = parse_style(default)?;
            let mut rules = Vec::with_capacity(rows.len().saturating_sub(1));
            for (key, style) in rows {
                if key.as_ref() == "default" {
                    continue;
                }
                let dialect = parse_style(style)?;
                let pattern = model_glob(key).map_err(|_| ConfigError::InvalidStyle)?;
                rules.push(ModelRule { pattern, dialect });
            }
            Ok(EditStyleConfig::Table { default, rules })
        }
    }
}

/// Selects the first matching table row, or the configured default.
#[must_use]
pub fn pick(config: &EditStyleConfig, model_id: &str) -> DialectId {
    match config {
        EditStyleConfig::Scalar(dialect) => *dialect,
        EditStyleConfig::Table { default, rules } => rules
            .iter()
            .find(|rule| rule.pattern.is_match(model_id))
            .map_or(*default, |rule| rule.dialect),
    }
}

/// Returns the prompt-intensity tier for one concrete dialect.
#[must_use]
pub const fn tier_of(dialect: DialectId) -> Tier {
    match dialect {
        DialectId::Replace | DialectId::ApplyPatch => Tier::Simple,
        DialectId::Anchor => Tier::Balanced,
        DialectId::Hashline | DialectId::HashlineLight | DialectId::HashlineEnhanced => {
            Tier::Strict
        }
    }
}

fn parse_style(value: &str) -> Result<DialectId, ConfigError> {
    match value {
        "anchor" | "balanced" => Ok(DialectId::Anchor),
        "replace" | "simple" => Ok(DialectId::Replace),
        "hashline" | "strict" => Ok(DialectId::Hashline),
        "hashline-light" => Ok(DialectId::HashlineLight),
        "hashline-enhanced" => Ok(DialectId::HashlineEnhanced),
        "apply_patch" => Ok(DialectId::ApplyPatch),
        _ => Err(ConfigError::InvalidStyle),
    }
}

fn model_glob(pattern: &str) -> Result<Regex, regex::Error> {
    let mut source = String::with_capacity(pattern.len().saturating_add(2));
    source.push('^');
    for scalar in pattern.chars() {
        match scalar {
            '*' => source.push_str(".*"),
            '?' => source.push('.'),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '[' | ']' | '{' | '}' | '\\' => {
                source.push('\\');
                source.push(scalar);
            }
            literal => source.push(literal),
        }
    }
    source.push('$');
    RegexBuilder::new(&source)
        .case_insensitive(true)
        .dot_matches_new_line(true)
        .build()
}

/// Removes one complete outer Markdown fence without changing its body.
pub(crate) fn strip_outer_fence(input: &str) -> &str {
    if !input.starts_with("```") {
        return input;
    }
    let Some(opening_end) = input.find('\n') else {
        return input;
    };
    let mut rest = &input[opening_end + 1..];
    if let Some(without_final_lf) = rest.strip_suffix('\n') {
        rest = without_final_lf;
    }
    let Some(body) = rest.strip_suffix("```") else {
        return input;
    };
    if body.ends_with('\n') { body } else { input }
}

/// Removes one optional anchor begin/end envelope.
pub(crate) fn strip_anchor_envelope(input: &str) -> &str {
    let input = input.strip_prefix("*** Begin Patch\n").unwrap_or(input);
    if let Some(body) = input.strip_suffix("\n*** End Patch\n") {
        body
    } else {
        input.strip_suffix("\n*** End Patch").unwrap_or(input)
    }
}
