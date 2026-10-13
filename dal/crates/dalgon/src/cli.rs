//! Command-line arguments and the canonical `dalgon` command tree.

use std::ffi::OsString;

use clap::{Command, CommandFactory, FromArgMatches};

mod completion;
pub(crate) mod texts;
mod tree;
mod validation;

#[cfg(test)]
mod tests;

pub(crate) use completion::completion_script;
pub(crate) use tree::*;
pub(crate) use validation::{RootValidation, is_mode_id, validate_root};

/// Builds the command tree used by the binaries, completion, and release man pages.
#[must_use]
pub fn command() -> Command {
    Cli::command()
}

/// Parses process arguments against the tree renamed for one binary alias.
///
/// The first argument is the invoked program name. Help and version surface
/// as [`clap::Error`] with kinds `DisplayHelp` and `DisplayVersion`, whose
/// rendered text already carries the binary name and package version.
pub(crate) fn parse_cli(binary: &'static str, args: Vec<OsString>) -> Result<Cli, clap::Error> {
    let matches = command_for(binary).try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

pub(crate) fn command_for(binary: &'static str) -> Command {
    command().name(binary).version(env!("CARGO_PKG_VERSION"))
}
