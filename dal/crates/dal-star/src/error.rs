//! Typed failures at the Starlark plugin boundary.

use std::{io, path::PathBuf};

/// A plugin could not be scanned, evaluated, or registered.
#[derive(Debug, thiserror::Error)]
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
    /// A plugin source did not declare its identity.
    #[error("no dal.plugin(...) call in {path}")]
    MissingPluginCall {
        /// Absolute `plugin.star` path.
        path: PathBuf,
    },
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
    /// The module does not export the single `plugin` value the v1 contract
    /// requires (ambient-registration style is unsupported).
    #[error("{path} does not export `plugin`; only the @dal/v1 contract is supported")]
    UnsupportedEntry {
        /// Absolute `plugin.star` path.
        path: PathBuf,
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
