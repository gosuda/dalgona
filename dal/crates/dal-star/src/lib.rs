//! Starlark adapters for dal's extension API.

mod ctx;
mod declare;
pub mod engine;
pub mod error;
mod eval;
pub mod eval_extension;
mod hooks;
mod invoke;
pub mod load;
pub mod names;
pub mod records;
mod rule_files;
pub mod source;
pub mod system;
mod tool;
mod value;

pub(crate) mod command;
pub(crate) mod descriptor;
pub(crate) mod record;
pub(crate) mod schema;

pub mod plugins;

pub use engine::{CELL_LIMITS, HANDLER_LIMITS, LOAD_LIMITS, Limits, dialect};
pub use error::LoadError;
pub use eval::Eval;
pub use eval_extension::eval_extension;
pub use load::{BundledPlugin, LoadRoots, PluginsConfig, load};
pub use plugins::{extensions_from_generation, load_extensions, plugins_extension};
pub use source::PluginSource;
pub use system::{PluginEntry, PluginGeneration, PluginSystem};
