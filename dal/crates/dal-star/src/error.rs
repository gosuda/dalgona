//! Typed failures at the Starlark plugin boundary.

use std::{io, path::PathBuf};

use thiserror::Error;

/// An API-usage error a script committed (§R07 category).
///
/// Wrong argument shapes, foreign handles, unknown attributes resolved to a
/// call, and provider-side arity violations surface as this type; it is
/// never a service failure and never terminal. Downcast checks at bridge
/// boundaries keep it distinct from [`crate::outcome::ScriptFailure`] and
/// `HostTerminal`.
#[derive(Debug)]
pub(crate) struct ApiError(pub(crate) Box<str>);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

/// Wraps `message` in a `starlark::Error` tagged as [`ApiError`].
pub(crate) fn api_error(message: impl Into<String>) -> starlark::Error {
    starlark::Error::new_other(ApiError(message.into().into_boxed_str()))
}

/// A plugin could not be scanned, evaluated, or registered.
#[derive(Debug, Error)]
pub enum LoadError {
    /// Starlark parsing or evaluation failed at a source location.
    #[error("{path}:{line}:{col}: {message}")]
    Eval {
        /// Absolute source path.
        path: PathBuf,
        /// One-based source line.
        line: u32,
        /// One-based source column.
        col: u32,
        /// Vendor or evaluator message.
        message: Box<str>,
    },
    /// A `dal.*` declaration failed at a source location.
    #[error("{path}:{line}:{col}: {message}")]
    Registration {
        /// Absolute source path.
        path: PathBuf,
        /// One-based source line.
        line: u32,
        /// One-based source column.
        col: u32,
        /// The declaration validation message.
        message: Box<str>,
    },
    /// The entry module binds `plugin` more than once.
    #[error("{path}:{line}:{col}: `plugin` is bound again; bind the plugin value exactly once")]
    PluginRebound {
        /// The entry file.
        path: PathBuf,
        /// One-based line of the second binding.
        line: u32,
        /// One-based column of the second binding.
        col: u32,
    },
    /// A plugin source did not export the one `plugin` value v1 requires.
    #[error("{path} does not export a `plugin` value; only the @dal/v1 contract is supported")]
    UnsupportedEntry {
        /// Absolute `plugin.star` path.
        path: PathBuf,
    },
    /// The exported `plugin` value failed validation (spec §P03).
    #[error("{path}: plugin descriptor is invalid: {message}")]
    InvalidPlugin {
        /// Absolute `plugin.star` path.
        path: PathBuf,
        /// What the descriptor got wrong.
        message: Box<str>,
    },
    /// A `uses` list failed to parse or violated a phase ceiling.
    #[error("{path}:{line}:{col}: uses {id}: {message}")]
    Uses {
        /// Absolute source path.
        path: PathBuf,
        /// One-based source line.
        line: u32,
        /// One-based source column.
        col: u32,
        /// The rejected operation id.
        id: Box<str>,
        /// Why it was rejected.
        message: Box<str>,
    },
    /// A declared schema or config failed validation.
    #[error("{path}: {message}")]
    Schema {
        /// Absolute source path.
        path: PathBuf,
        /// The schema validation message.
        message: Box<str>,
    },
    /// A plugin asset (skill body) is missing, escaped, or over budget.
    #[error("{path}: asset {asset}: {message}")]
    Asset {
        /// Absolute `plugin.star` path.
        path: PathBuf,
        /// The plugin-relative asset path.
        asset: Box<str>,
        /// What failed.
        message: Box<str>,
    },
    /// A skill body's front matter `mcp` object is malformed, at `path:line:col`.
    #[error(transparent)]
    SkillFront(#[from] dal_core::ext::SkillFrontError),
    /// A plugin declared a different name from its directory.
    #[error("plugin directory \"{dir}\" declares name \"{declared}\"")]
    NameMismatch {
        /// Directory name.
        dir: Box<str>,
        /// Declared name.
        declared: Box<str>,
    },
    /// A plugin directory could not be read.
    #[error("unreadable: {source}")]
    Unreadable {
        /// Absolute directory or file path.
        path: PathBuf,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// A configured plugin directory has no entry file.
    #[error("plugin directory \"{dir}\" has no plugin.star")]
    MissingEntryFile {
        /// Absolute plugin directory.
        dir: PathBuf,
    },
    /// A configured plugin directory name violates the plugin-name grammar.
    #[error("invalid plugin directory name \"{name}\"; names must match [a-z][a-z0-9_-]{{0,63}}")]
    InvalidDirectoryName {
        /// Rejected directory name.
        name: Box<str>,
    },
    /// `plugins` named a plugin absent from the scanned directory and the
    /// bundled set.
    #[error(
        "plugin \"{name}\" not found; searched \"{dir}\" plus bundled plugins; install it there or remove it from `plugins`"
    )]
    UnknownPlugin {
        /// Configured name that matched nothing.
        name: Box<str>,
        /// Plugin directory the scan covered.
        dir: PathBuf,
    },
    /// A nested module load escaped its plugin root.
    #[error("load \"{module}\" escapes the plugin directory")]
    LoadEscape {
        /// Rejected load path.
        module: Box<str>,
    },
    /// A nested module load named a file absent from the plugin directory.
    #[error("load \"{module}\" not found in the plugin directory")]
    MissingModule {
        /// Requested load path.
        module: Box<str>,
    },
    /// A nested module load formed a cycle.
    #[error("load cycle detected at \"{module}\"")]
    LoadCycle {
        /// Repeated module path.
        module: Box<str>,
    },
    /// A nested module load exceeded the maximum depth.
    #[error("load depth exceeds 8 at \"{module}\"")]
    LoadDepth {
        /// Rejected module path.
        module: Box<str>,
    },
    /// The virtual SDK module failed to build or a file loads a label that
    /// is not `@dal/v1`.
    #[error("SDK module failure at \"{module}\": {message}")]
    SdkModule {
        /// The requested load path.
        module: Box<str>,
        /// What went wrong.
        message: Box<str>,
    },
    /// The generation lock was poisoned by a panic in another task.
    #[error("the {what} lock is poisoned")]
    Poisoned {
        /// Which state is unrecoverable.
        what: Box<str>,
    },
    /// An embedded plugin file cannot be interpreted as UTF-8 text.
    #[error("plugin file \"{path}\" is not UTF-8")]
    InvalidUtf8 {
        /// Plugin-relative file path.
        path: Box<str>,
    },
}

impl LoadError {
    /// Renders the startup-facing diagnostic, including source coordinates when present.
    #[must_use]
    pub fn render(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::LoadError;

    #[test]
    fn rendered_evaluation_error_keeps_path_line_and_column() {
        let error = LoadError::Eval {
            path: PathBuf::from("/data/plugins/focus/plugin.star"),
            line: 3,
            col: 5,
            message: "unexpected token".into(),
        };
        assert_eq!(
            error.render(),
            "/data/plugins/focus/plugin.star:3:5: unexpected token",
        );
    }
}
