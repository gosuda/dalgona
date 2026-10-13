//! CLI tier for D2 and nomnoml: empty temp cwd, PATH-only env, 2 s budget.

use std::ffi::OsStr;
use std::time::Duration;

use super::{DiagramKind, DiagramOutcome};

const BUDGET: Duration = Duration::from_secs(2);
const OUTPUT_CAP: usize = 4 * 1_024 * 1_024;

/// Renders via the kind's CLI tool or returns the exact fallback reason.
#[must_use]
pub fn render(kind: DiagramKind, source: &[u8], path: Option<&OsStr>) -> DiagramOutcome {
    render_with_path(kind, source, path)
}

fn render_with_path(kind: DiagramKind, source: &[u8], path: Option<&OsStr>) -> DiagramOutcome {
    let tool = kind.tool();
    if tool.is_empty() {
        return fallback(format!("invalid {}", kind_name(kind)));
    }
    if which(tool, path).is_none() {
        return fallback(format!("{tool} is not installed"));
    }
    match run_child(tool, source, path) {
        ChildOutcome::Svg(svg) => super::raster::render_svg(&svg, kind),
        ChildOutcome::Missing => fallback(format!("{tool} is not installed")),
        ChildOutcome::Invalid => fallback(format!("invalid {}", kind_name(kind))),
        ChildOutcome::TimedOut => fallback("timed out".to_owned()),
        ChildOutcome::TooLarge => fallback("too large".to_owned()),
    }
}

fn kind_name(kind: DiagramKind) -> &'static str {
    match kind {
        DiagramKind::D2 => "d2",
        DiagramKind::Nomnoml => "nomnoml",
        DiagramKind::Dot => "dot",
        DiagramKind::Mermaid => "mermaid",
    }
}

fn fallback(reason: String) -> DiagramOutcome {
    DiagramOutcome::Fallback {
        reason: reason.into(),
    }
}

fn which(tool: &str, path: Option<&OsStr>) -> Option<std::path::PathBuf> {
    let path = path?;
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(tool);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

enum ChildOutcome {
    Svg(Vec<u8>),
    Missing,
    Invalid,
    TimedOut,
    TooLarge,
}

fn run_child(tool: &str, source: &[u8], path: Option<&OsStr>) -> ChildOutcome {
    let Ok(tmp) = tempfile::tempdir() else {
        return ChildOutcome::Invalid;
    };
    let dir = tmp.path().to_path_buf();
    #[expect(
        clippy::disallowed_methods,
        reason = "isolated external renderer spawn is the CLI process edge"
    )]
    let mut command = std::process::Command::new(tool);
    command.current_dir(&dir);
    command.env_clear();
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.env("HOME", &dir);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return ChildOutcome::Missing;
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let owned_source = source.to_vec();
    std::thread::scope(|scope| {
        if let Some(mut stdin) = stdin {
            scope.spawn(move || {
                use std::io::Write as _;
                let _ = stdin.write_all(&owned_source);
            });
        }
        // Drain stdout concurrently: a renderer louder than the pipe buffer
        // must never block the child while the budget loop polls.
        let reader = stdout.map(|mut stdout| {
            scope.spawn(move || {
                use std::io::Read as _;
                let mut output = Vec::new();
                let _ = stdout.read_to_end(&mut output);
                output
            })
        });
        let deadline = std::time::Instant::now() + BUDGET;
        let exited_ok = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status.success()),
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break Some(false),
            }
        };
        let output = match reader.map(std::thread::ScopedJoinHandle::join) {
            Some(Ok(output)) => output,
            Some(Err(_)) | None => Vec::new(),
        };
        match exited_ok {
            None => ChildOutcome::TimedOut,
            Some(false) => ChildOutcome::Invalid,
            Some(true) => {
                if output.is_empty() {
                    ChildOutcome::Invalid
                } else if output.len() > OUTPUT_CAP {
                    ChildOutcome::TooLarge
                } else {
                    ChildOutcome::Svg(output)
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::render_with_path;
    use crate::diagram::{DiagramKind, DiagramOutcome};

    #[test]
    fn absent_tool_names_the_missing_executable() {
        let outcome = render_with_path(DiagramKind::D2, b"a -> b", Some(std::ffi::OsStr::new("")));
        assert!(matches!(
            &outcome,
            DiagramOutcome::Fallback { reason } if reason.as_ref() == "d2 is not installed"
        ));
    }

    #[test]
    fn empty_path_search_finds_no_tool() {
        assert!(super::which("d2", None).is_none());
    }
}
