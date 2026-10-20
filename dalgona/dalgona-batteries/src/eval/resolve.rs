// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pack lookup: a named pack under the data root's `evals/` directory, or an
//! explicit file. Every pack comes from disk; no pack ships with the product.

use std::io;
use std::path::{Path, PathBuf};

/// The suffix of a pack file.
pub const PACK_SUFFIX: &str = ".eval.star";

/// The longest pack name.
const MAX_NAME: usize = 64;

/// Where a resolved pack came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Origin {
    /// A pack file on disk.
    User {
        /// The file the pack was read from.
        path: PathBuf,
    },
}

/// A pack's name, source text, and origin.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The pack name: the requested name, or the file name minus `.eval.star`.
    pub name: String,
    /// The Starlark source.
    pub source: String,
    /// Where the source came from.
    pub origin: Origin,
}

/// A pack that cannot be found.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The name is not a pack name or a path.
    #[error(
        "not a pack name: use 1 to 64 letters, digits, `-` and `_`, or give a path to a {PACK_SUFFIX} file"
    )]
    BadName,
    /// No user pack has the name.
    #[error("no eval pack with this name; packs go in {dir}")]
    Unknown {
        /// The user pack directory.
        dir: String,
    },
    /// The file exists but could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// The I/O error.
        #[source]
        source: io::Error,
    },
    /// An explicit path does not end in `.eval.star`.
    #[error("{path} is not a {PACK_SUFFIX} file")]
    NotAPack {
        /// The path.
        path: String,
    },
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && name.len() <= MAX_NAME
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn read(path: &Path) -> Result<Option<String>, ResolveError> {
    match std::fs::read_to_string(path) {
        Ok(source) => Ok(Some(source)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ResolveError::Read {
            path: path.display().to_string(),
            source,
        }),
    }
}

/// Finds the pack `requested` names.
///
/// A request containing a path separator, or naming a `.star` file, is a file
/// path. Any other request is a name: the file
/// `<user_root>/<name>.eval.star`.
///
/// # Errors
/// Returns [`ResolveError`] for a malformed name, an unknown pack, an
/// unreadable file, or a file whose stem cannot serve as a pack name.
pub fn resolve(requested: &str, user_root: &Path) -> Result<Resolved, ResolveError> {
    let is_file = requested.contains(['/', '\\'])
        || Path::new(requested)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("star"));
    if is_file {
        return resolve_file(requested);
    }
    if !valid_name(requested) {
        return Err(ResolveError::BadName);
    }
    let path = user_root.join(format!("{requested}{PACK_SUFFIX}"));
    if let Some(source) = read(&path)? {
        return Ok(Resolved {
            name: requested.to_owned(),
            source,
            origin: Origin::User { path },
        });
    }
    Err(ResolveError::Unknown {
        dir: user_root.display().to_string(),
    })
}

fn resolve_file(requested: &str) -> Result<Resolved, ResolveError> {
    let path = PathBuf::from(requested);
    let name = path
        .file_name()
        .and_then(|file| file.to_str())
        .and_then(|file| file.strip_suffix(PACK_SUFFIX))
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ResolveError::NotAPack {
            path: requested.to_owned(),
        })?;
    // The stem becomes the report directory name under `evals/runs/`: `..`
    // would resolve outside it, and neither dot name is a usable pack name.
    if matches!(name, "." | "..") {
        return Err(ResolveError::BadName);
    }
    let source = read(&path)?.ok_or_else(|| ResolveError::Read {
        path: requested.to_owned(),
        source: io::Error::from(io::ErrorKind::NotFound),
    })?;
    Ok(Resolved {
        name: name.to_owned(),
        source,
        origin: Origin::User { path },
    })
}

#[cfg(test)]
mod tests;
