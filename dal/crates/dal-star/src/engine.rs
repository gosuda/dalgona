//! Shared Starlark dialect and evaluator limits.

use starlark::{
    environment::{Globals, LibraryExtension},
    syntax::{AstModule, Dialect, DialectTypes},
};

/// A resource budget for one Starlark evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum evaluator ticks.
    pub ticks: u64,
    /// Maximum heap size in bytes.
    pub heap_bytes: u64,
    /// Maximum call-stack depth.
    pub stack_depth: u32,
}

/// Default limits for loading plugin source.
pub const LOAD_LIMITS: Limits = Limits {
    ticks: 1_000_000,
    heap_bytes: 16_777_216,
    stack_depth: 100,
};

/// Default limits for invoking a plugin handler.
pub const HANDLER_LIMITS: Limits = Limits {
    ticks: 200_000,
    heap_bytes: 8_388_608,
    stack_depth: 100,
};

/// Default limits for a Starlark evaluation cell.
pub const CELL_LIMITS: Limits = Limits {
    ticks: 2_000_000,
    heap_bytes: 33_554_432,
    stack_depth: 100,
};

/// Hard cap: transport value nesting depth (R10).
pub const MAX_VALUE_DEPTH: usize = 64;

/// Hard cap: one source file or eval cell in bytes (R10).
pub const MAX_CODE: usize = 256 << 10;

/// Hard cap: eval `data` argument in bytes (R10).
pub const MAX_DATA: usize = 1 << 20;

/// Hard cap: encoded operation result in bytes (R10).
pub const MAX_RESULT: usize = 1 << 20;

/// Hard cap: captured print output per invocation in bytes (R10).
pub const MAX_PRINTS: usize = 64 << 10;

/// Hard cap: `uses` entries per declaration or cell (R10).
pub const MAX_USES: usize = 64;

/// Hard cap: issued operations across one invocation tree (R10).
pub const MAX_ISSUED: u32 = 512;

/// Hard cap: outstanding scope tasks per root (E05).
pub const MAX_TASKS: usize = 64;

/// Hard cap: live scopes per root (E05).
pub const MAX_SCOPES: usize = 4;

/// Hard cap: invocation depth and module load depth (R10).
pub const MAX_NESTING: u8 = 8;

/// Hard cap: eval cell wall time (R10).
pub const CELL_WALL: std::time::Duration = std::time::Duration::from_secs(60);

/// Returns the single dialect used for plugins and evaluation cells.
#[must_use]
pub fn dialect() -> Dialect {
    Dialect {
        enable_keyword_only_arguments: true,
        enable_positional_only_arguments: true,
        enable_top_level_stmt: true,
        enable_f_strings: true,
        enable_types: DialectTypes::ParseOnly,
        ..Dialect::Standard
    }
}

/// Parses one source module under the shared dialect.
///
/// # Errors
/// Returns the parse message for any syntax failure; the caller attaches the
/// source path and position.
pub(crate) fn parse(path: &str, source: String) -> Result<AstModule, Box<str>> {
    AstModule::parse(path, source, &dialect()).map_err(|error| error.to_string().into())
}

/// Returns the evaluator globals shared by plugin load and eval cells.
///
/// Only the safe library extensions are enabled; the `dal` surface is provided
/// per module through `load("@dal/v1", ...)`, never as a global.
#[must_use]
pub(crate) fn globals() -> Globals {
    Globals::extended_by(&[
        LibraryExtension::Json,
        LibraryExtension::StructType,
        LibraryExtension::Typing,
        LibraryExtension::CallStack,
        LibraryExtension::Print,
    ])
}
