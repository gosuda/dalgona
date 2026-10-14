//! CLI tier for D2 and nomnoml: empty temp cwd, PATH-only env, 2 s budget.

use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use process_wrap::std::{ChildWrapper, CommandWrap};

use super::{DiagramKind, DiagramOutcome, FontSnapshot};

const BUDGET: Duration = Duration::from_secs(2);
const OUTPUT_CAP: usize = 4 * 1_024 * 1_024;

/// Renders via the kind's CLI tool or returns the exact fallback reason.
///
/// Setting `stop` kills the renderer's process tree within one poll
/// interval, so the owner of the render can shut down without waiting out
/// the budget.
#[must_use]
pub fn render(
    kind: DiagramKind,
    source: &[u8],
    path: Option<&OsStr>,
    fonts: &FontSnapshot,
    stop: &AtomicBool,
) -> DiagramOutcome {
    let tool = kind.tool();
    if tool.is_empty() {
        return fallback(format!("invalid {}", kind.label()));
    }
    if which(tool, path).is_none() {
        return fallback(format!("{tool} is not installed"));
    }
    match run_child(tool, source, path, stop) {
        ChildOutcome::Svg(svg) => super::raster::render_svg(&svg, kind, fonts),
        ChildOutcome::Missing => fallback(format!("{tool} is not installed")),
        ChildOutcome::Invalid => fallback(format!("invalid {}", kind.label())),
        ChildOutcome::TimedOut => fallback("timed out".to_owned()),
        ChildOutcome::TooLarge => fallback("too large".to_owned()),
        ChildOutcome::Stopped => fallback("cancelled".to_owned()),
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

#[cfg_attr(test, derive(Debug))]
enum ChildOutcome {
    Svg(Vec<u8>),
    Missing,
    Invalid,
    TimedOut,
    TooLarge,
    Stopped,
}

fn run_child(tool: &str, source: &[u8], path: Option<&OsStr>, stop: &AtomicBool) -> ChildOutcome {
    let Ok(tmp) = tempfile::tempdir() else {
        return ChildOutcome::Invalid;
    };
    let dir = tmp.path().to_path_buf();
    #[expect(
        clippy::disallowed_methods,
        reason = "isolated external renderer spawn is the CLI process edge"
    )]
    let mut command = std::process::Command::new(tool);
    if tool == "d2" {
        command.args(["-", "-"]);
    }
    command.current_dir(&dir);
    command.env_clear();
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.env("HOME", &dir);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::null());
    let mut command = CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::std::JobObject);
    let Ok(mut child) = command.spawn() else {
        return ChildOutcome::Missing;
    };
    let deadline = Instant::now() + BUDGET;
    let (Some(mut stdin), Some(stdout)) = (child.stdin().take(), child.stdout().take()) else {
        let _ = child.start_kill();
        let _ = child.wait();
        return ChildOutcome::Invalid;
    };
    std::thread::scope(|scope| {
        let (send, receive) = std::sync::mpsc::sync_channel(2);
        let input_send = send.clone();
        scope.spawn(move || {
            let result = stdin.write_all(source);
            drop(stdin);
            let _ = input_send.send(PipeResult::Input(result));
        });
        scope.spawn(move || {
            let mut output = Vec::new();
            let result = stdout
                .take(OUTPUT_CAP as u64 + 1)
                .read_to_end(&mut output)
                .map(|_| output);
            let _ = send.send(PipeResult::Output(result));
        });
        let outcome = collect_output(child.inner_mut(), &receive, deadline, stop);
        let _ = child.start_kill();
        if child.wait().is_err() {
            return ChildOutcome::Invalid;
        }
        outcome
    })
}

enum PipeResult {
    Input(std::io::Result<()>),
    Output(std::io::Result<Vec<u8>>),
}

fn collect_output(
    leader: &mut dyn ChildWrapper,
    receive: &Receiver<PipeResult>,
    deadline: Instant,
    stop: &AtomicBool,
) -> ChildOutcome {
    let mut input_done = false;
    let mut output = None;
    loop {
        if stop.load(Ordering::Acquire) {
            return ChildOutcome::Stopped;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return ChildOutcome::TimedOut;
        }
        let interval = remaining.min(Duration::from_millis(10));
        if input_done && output.is_some() {
            std::thread::sleep(interval);
        } else {
            match receive.recv_timeout(interval) {
                Ok(PipeResult::Input(Ok(()))) => input_done = true,
                Ok(PipeResult::Output(Ok(bytes))) if bytes.len() > OUTPUT_CAP => {
                    return ChildOutcome::TooLarge;
                }
                Ok(PipeResult::Output(Ok(bytes))) => output = Some(bytes),
                Ok(PipeResult::Input(Err(_)) | PipeResult::Output(Err(_)))
                | Err(RecvTimeoutError::Disconnected) => return ChildOutcome::Invalid,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        match leader.try_wait() {
            Ok(Some(status)) if !status.success() => return ChildOutcome::Invalid,
            Ok(Some(_)) if input_done && output.is_some() => {
                return match output.take() {
                    Some(bytes) if !bytes.is_empty() => ChildOutcome::Svg(bytes),
                    _ => ChildOutcome::Invalid,
                };
            }
            Err(_) => return ChildOutcome::Invalid,
            Ok(Some(_) | None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::render;
    use crate::diagram::{DiagramKind, DiagramOutcome, FontSnapshot};

    #[test]
    fn absent_tool_names_the_missing_executable() {
        let outcome = render(
            DiagramKind::D2,
            b"a -> b",
            Some(std::ffi::OsStr::new("")),
            &FontSnapshot::default(),
            &AtomicBool::new(false),
        );
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

#[cfg(all(test, unix))]
mod process_tests {
    use std::error::Error;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{ChildOutcome, OUTPUT_CAP, run_child};

    fn run(root: &tempfile::TempDir) -> ChildOutcome {
        run_child(
            "d2",
            b"a -> b",
            Some(root.path().as_os_str()),
            &AtomicBool::new(false),
        )
    }

    #[test]
    fn a_stop_kills_a_running_renderer_before_its_budget() -> Result<(), Box<dyn Error>> {
        let root = renderer("/bin/sleep 8 &\nprintf '%s' \"$!\" > \"${0}.pid\"\nwait")?;
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let outcome = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(200));
                stop.store(true, Ordering::Release);
            });
            run_child("d2", b"a -> b", Some(root.path().as_os_str()), &stop)
        });
        let elapsed = started.elapsed();
        assert!(
            matches!(outcome, ChildOutcome::Stopped),
            "renderer returned {outcome:?} after {elapsed:?}",
        );
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
        #[cfg(target_os = "linux")]
        assert_descendant_stopped(root.path())?;
        Ok(())
    }

    fn renderer(body: &str) -> Result<tempfile::TempDir, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("d2");
        std::fs::write(&path, format!("#!/bin/sh\n/bin/cat >/dev/null\n{body}\n"))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        Ok(root)
    }

    #[test]
    fn output_cap_rejects_the_first_excess_byte() -> Result<(), Box<dyn Error>> {
        for size in [OUTPUT_CAP, OUTPUT_CAP + 1] {
            let root = renderer(&format!(
                "/bin/dd if=/dev/zero bs={size} count=1 2>/dev/null"
            ))?;
            let outcome = run(&root);
            match outcome {
                ChildOutcome::Svg(bytes) if size == OUTPUT_CAP => {
                    assert_eq!(bytes.len(), OUTPUT_CAP);
                }
                ChildOutcome::TooLarge if size == OUTPUT_CAP + 1 => {}
                _ => panic!("renderer did not apply the output cap at {size} bytes"),
            }
        }
        Ok(())
    }

    #[test]
    fn output_overflow_stops_a_renderer_before_it_exits() -> Result<(), Box<dyn Error>> {
        let root = renderer(&format!(
            "/bin/dd if=/dev/zero bs={} count=1 2>/dev/null\n/bin/sleep 8",
            OUTPUT_CAP + 1
        ))?;
        let started = Instant::now();
        let outcome = run(&root);
        let elapsed = started.elapsed();
        assert!(
            matches!(outcome, ChildOutcome::TooLarge),
            "renderer returned {outcome:?} after {elapsed:?}",
        );
        assert!(elapsed < Duration::from_secs(7), "elapsed: {elapsed:?}");
        Ok(())
    }

    #[test]
    fn deadline_includes_descendant_held_output() -> Result<(), Box<dyn Error>> {
        for parent_exit in ["exit 0", "wait"] {
            let root = renderer(&format!(
                "/bin/sleep 8 &\nprintf '%s' \"$!\" > \"${{0}}.pid\"\n{parent_exit}"
            ))?;
            let started = Instant::now();
            let outcome = run(&root);
            assert!(matches!(outcome, ChildOutcome::TimedOut));
            assert!(started.elapsed() < Duration::from_secs(7));
            #[cfg(target_os = "linux")]
            assert_descendant_stopped(root.path())?;
        }
        Ok(())
    }

    #[test]
    fn successful_output_does_not_leave_a_background_renderer() -> Result<(), Box<dyn Error>> {
        let root = renderer(
            "/bin/sleep 8 >/dev/null &\nprintf '%s' \"$!\" > \"${0}.pid\"\nprintf '<svg/>'",
        )?;
        let started = Instant::now();
        let outcome = run(&root);
        assert!(matches!(outcome, ChildOutcome::Svg(bytes) if bytes == b"<svg/>"));
        assert!(started.elapsed() < Duration::from_secs(7));
        #[cfg(target_os = "linux")]
        assert_descendant_stopped(root.path())?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn assert_descendant_stopped(root: &std::path::Path) -> Result<(), Box<dyn Error>> {
        let pid = std::fs::read_to_string(root.join("d2.pid"))?;
        let pid: u32 = pid.parse()?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                // A gone descendant can answer ESRCH instead of ENOENT on a
                // racy reaper; both mean it stopped.
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
                {
                    return Ok(());
                }
                Ok(stat)
                    if stat
                        .rsplit_once(')')
                        .is_some_and(|(_, rest)| rest.starts_with(" Z")) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            assert!(
                Instant::now() < deadline,
                "renderer descendant is still running"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
