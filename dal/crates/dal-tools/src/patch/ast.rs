//! Symbol and block lowering behind the `symbols` feature.
//!
//! Uses the single read/search parser service; no second parser or index.

#[cfg(feature = "symbols")]
use std::path::Path;

#[cfg(feature = "symbols")]
use super::ir::{EngineError, ErrorClass};

/// Resolves a symbol selector to its definition byte span.
///
/// Returns the existing unsupported-language and missing-definition texts;
/// parser budget and panic outcomes map to the shared parser-failure text.
///
/// # Errors
///
/// Returns an [`ErrorClass::Resolve`] error when the language has no symbol
/// support, the parser fails, or no definition matches `name` and `ordinal`.
#[cfg(feature = "symbols")]
pub async fn symbol_span(
    path: &Path,
    bytes: &[u8],
    name: &str,
    ordinal: Option<usize>,
) -> Result<(usize, usize, String), EngineError> {
    let language = crate::parse::language(path).ok_or_else(|| {
        let lang = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("unknown");
        EngineError::new(
            ErrorClass::Resolve,
            format!("patch: {} has no symbol support ({lang}).", path.display()),
        )
    })?;
    let _ = language;
    let definitions = crate::parse::definitions(path, bytes)
        .await
        .map_err(|failure| {
            EngineError::new(
                ErrorClass::Resolve,
                format!("patch: parser failed on {}: {failure:?}.", path.display()),
            )
        })?;
    let mut matches = Vec::new();
    for definition in &definitions {
        if definition.name.as_ref() == name {
            matches.push(definition);
        }
    }
    if matches.is_empty() {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!("patch: no definition {name} in {}.", path.display()),
        ));
    }
    if matches.len() > 1 && ordinal.is_none() {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!(
                "patch: {} definitions named {name} in {}.",
                matches.len(),
                path.display()
            ),
        ));
    }
    let selected = if let Some(ordinal) = ordinal {
        matches
            .get(ordinal.saturating_sub(1))
            .copied()
            .ok_or_else(|| {
                EngineError::new(
                    ErrorClass::Resolve,
                    format!(
                        "patch: ordinal {ordinal} out of range for {name} in {}.",
                        path.display()
                    ),
                )
            })?
    } else {
        matches[0]
    };
    let tag = super::ir::tag(
        super::ir::TagDomain::Def,
        &bytes[selected.byte_start..selected.byte_end.min(bytes.len())],
    );
    Ok((selected.byte_start, selected.byte_end, tag))
}

/// Resolves the outermost named node beginning at one-based `line`.
///
/// # Errors
///
/// Returns an [`ErrorClass::Resolve`] error when the language has no symbol
/// support, the parser fails, or no named node begins at `line`.
#[cfg(feature = "symbols")]
pub async fn node_span(
    path: &Path,
    bytes: &[u8],
    line: u32,
) -> Result<(usize, usize, u32, u32), EngineError> {
    if crate::parse::language(path).is_none() {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!(
                "patch: line {line}: block ops (N*) need search_symbols = true in config.toml."
            ),
        ));
    }
    let node = crate::parse::node_at(path, bytes, line)
        .await
        .map_err(|failure| {
            EngineError::new(
                ErrorClass::Resolve,
                format!("patch: parser failed on {}: {failure:?}.", path.display()),
            )
        })?;
    node.ok_or_else(|| {
        EngineError::new(
            ErrorClass::Resolve,
            format!(
                "patch: no syntactic block begins at line {line} of {}.",
                path.display()
            ),
        )
    })
}
