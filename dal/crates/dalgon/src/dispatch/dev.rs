//! `dalgon dev` dispatch: journal surgery and the fold explainer.
//!
//! These commands read stored session state for debugging. They never open a
//! `Host`, so they work on directories the store left behind without locks.

mod fold;
mod journal;
mod util;

use std::fmt;
use std::process::ExitCode;

use crate::cli;
use crate::exit;
use crate::two_lines;

/// One `dalgon dev` failure with a suggested next step.
#[derive(Debug)]
pub(super) enum DevError {
    Read {
        path: String,
        source: std::io::Error,
    },
    Write {
        path: String,
        source: std::io::Error,
    },
    Decode {
        path: String,
        line: usize,
        source: dal_core::DecodeError,
    },
    Replay {
        path: String,
        source: dal_core::ReplayError,
    },
    Empty {
        path: String,
    },
    NotSessionDir {
        path: String,
    },
    NoSidecar {
        path: String,
        name: String,
    },
    SamePath {
        path: String,
    },
}

impl DevError {
    /// One sentence pointing at the fix.
    fn hint(&self) -> &'static str {
        match self {
            Self::Read { .. } => "Check that the file exists and is readable, then try again.",
            Self::Write { .. } => {
                "Check that the destination directory is writable, then try again."
            }
            Self::Decode { .. } => {
                "Pass a complete journal.jsonl; a torn tail fails to decode past the cut."
            }
            Self::Replay { .. } => "Check that the records form one consistent session journal.",
            Self::Empty { .. } => "Pass a journal with at least one record line.",
            Self::NotSessionDir { .. } => {
                "Pass a session directory containing journal.jsonl and sidecar files."
            }
            Self::NoSidecar { .. } => "Run without NAME to list the sidecars that exist.",
            Self::SamePath { .. } => "Pass a different output path so the source journal survives.",
        }
    }
}

impl fmt::Display for DevError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(out, "cannot read {path}: {source}"),
            Self::Write { path, source } => write!(out, "cannot write {path}: {source}"),
            Self::Decode { path, line, source } => {
                write!(out, "{path}: line {line} does not decode: {source}")
            }
            Self::Replay { path, source } => {
                write!(out, "{path}: journal does not replay: {source}")
            }
            Self::Empty { path } => write!(out, "{path}: journal is empty"),
            Self::NotSessionDir { path } => write!(out, "{path}: not a session directory"),
            Self::NoSidecar { path, name } => {
                write!(out, "{path}: sidecar \"{name}\" does not exist")
            }
            Self::SamePath { path } => {
                write!(out, "{path}: input and output must differ")
            }
        }
    }
}

impl std::error::Error for DevError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Decode { source, .. } => Some(source),
            Self::Replay { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Runs one `dalgon dev` subcommand, rendering failures to their exit codes.
pub(crate) fn run(args: &cli::DevArgs) -> ExitCode {
    let result = match &args.command {
        cli::DevSubcommand::Journal(args) => journal::run(args),
        cli::DevSubcommand::Fold(args) => fold::run(&args.file),
    };
    match result {
        Ok(code) => code,
        Err(error) => two_lines(
            [format!("dalgon dev: {error}"), error.hint().to_owned()],
            exit::ExitKind::RequestedFailure,
        ),
    }
}
