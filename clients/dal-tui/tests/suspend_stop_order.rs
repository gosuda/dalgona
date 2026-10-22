//! `TermIo::raise_tstp` must not return before the process has stopped.
//!
//! The terminal client raises `SIGTSTP` from a blocking worker. Naming the
//! process id lets the stop land late: the worker returns at once and keeps
//! running while the shell has not seen the stop. This test watches a
//! re-executed child from the outside: once `waitpid` reports the stop, the
//! post-raise marker must still be empty. Linux-only: only Linux names a
//! thread to the kernel.

#![cfg(test)]
#![cfg(any(target_os = "linux", target_os = "android"))]

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use dal_tui::term::{CrosstermTermIo, TermIo};
use rustix::process::{Pid, Signal, WaitOptions, WaitStatus, kill_process, waitpid};

/// Names the marker file the re-executed child appends to after the raise.
const MARKER_ENV: &str = "DAL_TUI_SUSPEND_ORDER_MARKER";
/// The child half of the test below.
const CHILD_TEST: &str = "raise_tstp_child";
/// A lost race is a failure; many rounds make losing it near certain.
const ROUNDS: usize = 40;
/// Upper bound for one stop or one exit; a loaded host still finishes in time.
const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Owns the child on unwind: kills and reaps it, unless disarmed after a
/// terminal status was reaped (signaling a reaped pid could hit a recycled one).
struct Reaper {
    inner: std::process::Child,
    disarmed: bool,
}

impl Reaper {
    fn new(inner: std::process::Child) -> Self {
        Self {
            inner,
            disarmed: false,
        }
    }

    /// Records that the child is reaped; Drop then does nothing. Call before
    /// any assertion that can unwind once a terminal status is in.
    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let _ = self.inner.kill();
        let _ = self.inner.wait();
    }
}

/// The suspended process: raises the stop from a worker thread, as the
/// blocking terminal client does, then records that the raise returned.
///
/// Does nothing unless the parent test started it with the marker path.
#[test]
#[ignore = "started by raise_tstp_returns_only_after_the_process_stopped"]
#[expect(
    clippy::disallowed_methods,
    reason = "the child reads the parent's marker path and builds the terminal seam from stdin"
)]
fn raise_tstp_child() {
    let Some(path) = std::env::var_os(MARKER_ENV) else {
        return;
    };
    let mut marker = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("the marker file opens");
    let io = CrosstermTermIo::new(std::io::stdin());
    let worker = std::thread::spawn(move || {
        io.raise_tstp();
        marker
            .write_all(b"x")
            .expect("the marker records that the raise returned");
    });
    worker.join().expect("the raising worker finishes");
}

/// Reports the child's next stop, polling without blocking; fails on expiry.
/// A stop is never reaped, so the guard stays armed throughout.
fn wait_reported(pid: Pid, options: WaitOptions, what: &str) -> WaitStatus {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if let Some((_, status)) = waitpid(Some(pid), options).expect("waitpid answers") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "{what} within {}s",
            WAIT_TIMEOUT.as_secs()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Reports the child's exit through its own handle, polling without blocking;
/// fails on expiry. The handle caches the reap, so Drop never signals the pid.
fn wait_exited(child: &mut std::process::Child, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait answers") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "{what} within {}s",
            WAIT_TIMEOUT.as_secs()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test re-executes its own binary as the process that suspends"
)]
fn raise_tstp_returns_only_after_the_process_stopped() {
    let exe = std::env::current_exe().expect("the test binary has a path");
    let dir = tempfile::tempdir().expect("a scratch directory");
    let marker = dir.path().join("returned");
    for round in 0..ROUNDS {
        std::fs::File::create(&marker).expect("the marker starts empty");
        let mut guard = Reaper::new(
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    CHILD_TEST,
                    "--ignored",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(MARKER_ENV, &marker)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .expect("the child starts"),
        );
        let pid = Pid::from_child(&guard.inner);
        let status = wait_reported(
            pid,
            WaitOptions::UNTRACED | WaitOptions::NOHANG,
            "the child stops on SIGTSTP",
        );
        if !status.stopped() {
            // waitpid reaped a terminated child; Drop must not signal the pid.
            guard.disarm();
        }
        assert!(
            status.stopped(),
            "round {round}: no SIGTSTP stop, got {status:?}"
        );
        assert_eq!(status.stopping_signal(), Some(Signal::TSTP.as_raw()));
        assert!(
            std::fs::read(&marker).expect("the marker reads").is_empty(),
            "round {round}: the child ran after raise_tstp before the stop took effect"
        );
        kill_process(pid, Signal::CONT).expect("SIGCONT reaches the stopped child");
        let exit = wait_exited(&mut guard.inner, "the continued child exits");
        guard.disarm();
        assert!(
            exit.success(),
            "round {round}: the continued child failed: {exit:?}"
        );
        assert_eq!(
            std::fs::read(&marker).expect("the marker reads"),
            b"x",
            "round {round}: raise_tstp returns after SIGCONT"
        );
    }
}
