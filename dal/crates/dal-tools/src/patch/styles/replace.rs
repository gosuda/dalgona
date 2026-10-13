//! Strict JSON replacement dialect parser.

use std::path::PathBuf;

use serde::Deserialize;

use super::super::ir::{Action, Edit, Guard, Locator, ParseError, Window};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    changes: Vec<Change>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    path: String,
    old: Option<String>,
    line: Option<usize>,
    all: Option<bool>,
    new: Option<String>,
    tag: Option<String>,
    create: Option<String>,
    delete: Option<bool>,
    rename: Option<String>,
    #[cfg(feature = "symbols")]
    symbol: Option<String>,
    #[cfg(feature = "symbols")]
    at: Option<SymbolAction>,
}

#[cfg(feature = "symbols")]
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SymbolAction {
    Replace,
    Before,
    After,
}

/// Parses strict JSON changes into the shared edit IR.
///
/// # Errors
/// Returns a parse-class error when the JSON shape is invalid or an entry does
/// not describe exactly one supported action.
pub(crate) fn parse(input: &str, symbols: bool) -> Result<Vec<Edit>, ParseError> {
    let payload: Payload = sonic_rs::from_str(input)
        .map_err(|error| ParseError::new(format!("patch: invalid input: {error}.")))?;
    if !(1..=64).contains(&payload.changes.len()) {
        return Err(ParseError::new(
            "patch: changes must contain 1 to 64 entries.",
        ));
    }
    payload
        .changes
        .into_iter()
        .enumerate()
        .map(|(index, change)| lower(index, change, symbols))
        .collect()
}

fn lower(index: usize, change: Change, symbols: bool) -> Result<Edit, ParseError> {
    #[cfg(feature = "symbols")]
    if !symbols && change.symbol.is_some() {
        return Err(unknown_symbol_field("symbol"));
    }
    #[cfg(feature = "symbols")]
    if !symbols && change.at.is_some() {
        return Err(unknown_symbol_field("at"));
    }
    #[cfg(not(feature = "symbols"))]
    let _ = symbols;

    let modify = change.old.is_some() || change.tag.is_some() || change.new.is_some();
    let create = change.create.is_some();
    let delete = change.delete == Some(true);
    let rename = change.rename.is_some();
    #[cfg(feature = "symbols")]
    let symbol = change.symbol.is_some();
    #[cfg(not(feature = "symbols"))]
    let symbol = false;

    let action_count = [modify, create, delete, rename, symbol]
        .into_iter()
        .filter(|present| *present)
        .count();
    if action_count == 0 {
        return Err(no_action(index, symbols));
    }
    if action_count > 1 {
        let fields = [
            (modify, "old and new"),
            (create, "create"),
            (delete, "delete"),
            (rename, "rename"),
            (symbol, "symbol"),
        ];
        let names: Vec<&str> = fields
            .into_iter()
            .filter_map(|(present, name)| present.then_some(name))
            .collect();
        return Err(conflicting_fields(index, names[0], names[1]));
    }

    if create {
        return only_create(index, change);
    }
    if delete {
        return only_delete(index, change);
    }
    if rename {
        return only_rename(index, change);
    }
    #[cfg(feature = "symbols")]
    if symbol {
        return lower_symbol(index, change);
    }
    lower_text(index, change)
}

fn only_create(index: usize, change: Change) -> Result<Edit, ParseError> {
    let Some(body) = change.create else {
        return Err(no_action(index, false));
    };
    if change.old.is_some() {
        return Err(conflicting_fields(index, "old", "create"));
    }
    if change.new.is_some() {
        return Err(conflicting_fields(index, "create", "new"));
    }
    if change.tag.is_some() {
        return Err(conflicting_fields(index, "create", "tag"));
    }
    if change.line.is_some() || change.all == Some(true) {
        return Err(conflicting_fields(index, "create", "line or all"));
    }
    Ok(Edit::Create {
        index,
        path: PathBuf::from(change.path),
        body,
    })
}

fn only_delete(index: usize, change: Change) -> Result<Edit, ParseError> {
    if change.old.is_some() {
        return Err(conflicting_fields(index, "old", "delete"));
    }
    if change.new.is_some() {
        return Err(conflicting_fields(index, "delete", "new"));
    }
    if change.tag.is_some() {
        return Err(conflicting_fields(index, "delete", "tag"));
    }
    if change.line.is_some() || change.all == Some(true) {
        return Err(conflicting_fields(index, "delete", "line or all"));
    }
    Ok(Edit::Delete {
        index,
        path: PathBuf::from(change.path),
        reference: None,
    })
}

fn only_rename(index: usize, change: Change) -> Result<Edit, ParseError> {
    let Some(to) = change.rename else {
        return Err(no_action(index, false));
    };
    if change.old.is_some() {
        return Err(conflicting_fields(index, "old", "rename"));
    }
    if change.new.is_some() {
        return Err(conflicting_fields(index, "rename", "new"));
    }
    if change.tag.is_some() {
        return Err(conflicting_fields(index, "rename", "tag"));
    }
    if change.line.is_some() || change.all == Some(true) {
        return Err(conflicting_fields(index, "rename", "line or all"));
    }
    Ok(Edit::Rename {
        index,
        from: PathBuf::from(change.path),
        to: PathBuf::from(to),
        reference: None,
    })
}

fn lower_text(index: usize, change: Change) -> Result<Edit, ParseError> {
    if change.tag.is_some() && change.old.is_some() {
        return Err(conflicting_fields(index, "old", "tag"));
    }
    if let Some(old) = change.old {
        if old.is_empty() {
            return Err(ParseError::new(format!(
                "patch: changes[{index}]: old must not be empty."
            )));
        }
        let Some(body) = change.new else {
            return Err(ParseError::new(format!(
                "patch: changes[{index}]: old needs new. Use \"new\": \"\" to remove old."
            )));
        };
        let all = change.all == Some(true);
        let guard = if all {
            change.tag.map_or(Guard::Seen, Guard::WholeTag)
        } else {
            Guard::Quoted
        };
        return Ok(Edit::Change {
            index,
            path: PathBuf::from(change.path),
            locator: Locator::Text {
                old,
                line_hint: change.line,
                all,
                window: Window::BeforePayload,
                context: None,
                at_eof: false,
            },
            action: Action::Replace,
            guard,
            body,
            window: Window::BeforePayload,
        });
    }

    if let Some(tag) = change.tag {
        let Some(body) = change.new else {
            return Err(ParseError::new(format!(
                "patch: changes[{index}]: tag needs new."
            )));
        };
        if change.line.is_some() || change.all == Some(true) {
            return Err(conflicting_fields(index, "tag", "line or all"));
        }
        return Ok(Edit::Change {
            index,
            path: PathBuf::from(change.path),
            locator: Locator::Whole,
            action: Action::Replace,
            guard: Guard::WholeTag(tag),
            body,
            window: Window::BeforePayload,
        });
    }

    let Some(body) = change.new else {
        return Err(no_action(index, false));
    };
    if change.line.is_some() || change.all == Some(true) {
        return Err(conflicting_fields(index, "new", "line or all"));
    }
    Ok(Edit::Change {
        index,
        path: PathBuf::from(change.path),
        locator: Locator::Whole,
        action: Action::Replace,
        guard: Guard::Seen,
        body,
        window: Window::BeforePayload,
    })
}

#[cfg(feature = "symbols")]
fn lower_symbol(index: usize, change: Change) -> Result<Edit, ParseError> {
    let Some(raw_name) = change.symbol else {
        return Err(no_action(index, true));
    };
    let (name, ordinal) = parse_symbol_name(&raw_name).ok_or_else(|| {
        ParseError::new(format!(
            "patch: changes[{index}]: invalid symbol selector {raw_name:?}."
        ))
    })?;
    let Some(body) = change.new else {
        return Err(ParseError::new(format!(
            "patch: changes[{index}]: symbol needs new."
        )));
    };
    if change.create.is_some() || change.delete == Some(true) || change.rename.is_some() {
        return Err(no_action(index, true));
    }
    if change.all == Some(true) || change.line.is_some() {
        return Err(conflicting_fields(index, "symbol", "line or all"));
    }
    let (action, old, guard) = match (change.at.unwrap_or(SymbolAction::Replace), change.old) {
        (SymbolAction::Replace, old) => {
            let guard = if let Some(old) = old.as_deref() {
                if old.is_empty() {
                    return Err(ParseError::new(format!(
                        "patch: changes[{index}]: old must not be empty."
                    )));
                }
                Guard::Quoted
            } else {
                change
                    .tag
                    .map_or_else(|| Guard::DefTag(String::new()), Guard::DefTag)
            };
            (Action::Replace, old, guard)
        }
        (SymbolAction::Before, None) => {
            if change.tag.is_some() {
                return Err(conflicting_fields(index, "symbol at before", "tag"));
            }
            (Action::InsertBefore, None, Guard::Exists)
        }
        (SymbolAction::After, None) => {
            if change.tag.is_some() {
                return Err(conflicting_fields(index, "symbol at after", "tag"));
            }
            (Action::InsertAfter, None, Guard::Exists)
        }
        (SymbolAction::Before | SymbolAction::After, Some(_)) => {
            return Err(conflicting_fields(
                index,
                "symbol old",
                "at before or after",
            ));
        }
    };
    Ok(Edit::Change {
        index,
        path: PathBuf::from(change.path),
        locator: Locator::Symbol { name, ordinal, old },
        action,
        guard,
        body,
        window: Window::BeforePayload,
    })
}

#[cfg(feature = "symbols")]
fn parse_symbol_name(value: &str) -> Option<(String, Option<usize>)> {
    let Some((name, ordinal)) = value
        .strip_suffix(']')
        .and_then(|value| value.rsplit_once('['))
    else {
        return (!value.is_empty()).then(|| (value.to_owned(), None));
    };
    if name.is_empty() || ordinal.is_empty() || !ordinal.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let ordinal = ordinal.parse::<usize>().ok()?;
    (ordinal > 0).then(|| (name.to_owned(), Some(ordinal)))
}

#[cfg(feature = "symbols")]
fn unknown_symbol_field(field: &str) -> ParseError {
    ParseError::new(format!(
        "patch: invalid input: unknown field `{field}`; symbols are disabled."
    ))
}

fn no_action(index: usize, symbols: bool) -> ParseError {
    let actions = if symbols {
        "old and new, tag and new, symbol and new, create, delete, or rename"
    } else {
        "old and new, tag and new, create, delete, or rename"
    };
    ParseError::new(format!(
        "patch: changes[{index}]: say what to change: {actions}."
    ))
}

fn conflicting_fields(index: usize, first: &str, second: &str) -> ParseError {
    ParseError::new(format!(
        "patch: changes[{index}]: {first} cannot be used with {second}."
    ))
}

/// Minimal shape probe for classification without staging.
#[derive(serde::Deserialize)]
pub(crate) struct ReplaceProbe {
    #[expect(
        dead_code,
        reason = "decoding proves the shape; the field is never read"
    )]
    changes: Vec<serde::de::IgnoredAny>,
}
