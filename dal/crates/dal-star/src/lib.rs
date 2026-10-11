//! Starlark adapters for dal's extension API (Plugin & Eval v1).

mod adapter;
pub(crate) mod command;
pub(crate) mod context;
mod convert;
pub(crate) mod descriptor;
pub mod engine;
pub mod error;
mod eval;
pub(crate) mod evidence;
mod hooks;
pub(crate) mod invoke;
pub mod load;
mod model;
pub(crate) mod outcome;
pub(crate) mod record;
pub(crate) mod schema;
pub(crate) mod scope;
pub(crate) mod sdk;
pub mod system;
mod tool;
pub(crate) mod validate;
pub(crate) mod value;

pub use dal_core::ext::PluginSource;
pub use engine::{CELL_LIMITS, HANDLER_LIMITS, LOAD_LIMITS, Limits, dialect};
pub use error::LoadError;
pub use eval::eval_extension;
pub use load::{BundledPlugin, LoadRoots, PluginsConfig, load};
pub use system::{PluginGeneration, PluginSystem};
