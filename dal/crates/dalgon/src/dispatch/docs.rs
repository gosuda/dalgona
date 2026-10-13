use std::{io::Write, process::ExitCode};

use dal_ext::docs::{self, DocsSnapshot, Lookup, Manual};

use crate::{cli, exit};

/// Runs first-party product manuals without loading user plugins.
pub(crate) fn run(args: cli::DocsArgs, product: &str, mut manuals: Vec<Manual>) -> ExitCode {
    manuals.sort_by(|left, right| left.scheme.as_bytes().cmp(right.scheme.as_bytes()));
    let snapshot = DocsSnapshot { manuals };
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let Some(uri) = args.uri else {
        return output_exit(write!(stdout, "{}", docs::listing(&snapshot)));
    };
    match docs::lookup(&snapshot, &uri) {
        Lookup::Page { text, .. } | Lookup::Index { text, .. } => {
            output_exit(write!(stdout, "{text}"))
        }
        Lookup::Miss(miss) => {
            for line in docs::miss_lines(&miss, product) {
                let _ = writeln!(stderr, "{line}");
            }
            exit::code(exit::ExitKind::RequestedFailure)
        }
    }
}

fn output_exit(result: std::io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => exit::code(exit::ExitKind::Success),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
            exit::code(exit::ExitKind::Signal(13))
        }
        Err(error) => crate::two_lines(
            [
                format!("dalgon: could not write documentation output: {error}"),
                "Check that the output stream is open, then try again.".into(),
            ],
            exit::ExitKind::RequestedFailure,
        ),
    }
}
