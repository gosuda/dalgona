//! Process-edge signal capture for the blocking terminal client.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use dal_tui::term::{CrosstermTermIo, CursorReport, TermIo};
use tokio::task::JoinHandle;

/// Signal subscriptions owned by one interactive invocation.
pub(super) struct Signals {
    stop: Arc<AtomicU8>,
    resize: Arc<AtomicBool>,
    /// Counts every resize notification; coalesced and net-zero resizes move
    /// the screen even when the size flag reads the same twice.
    epoch: Arc<AtomicU64>,
    tasks: Vec<JoinHandle<()>>,
    shutdown: Option<crate::edge::ShutdownSignals>,
}

impl Signals {
    /// Registers handlers before raw mode or the blocking client starts. A
    /// signal whose registration fails logs one warning and is skipped.
    pub(super) fn start() -> Self {
        let stop = Arc::new(AtomicU8::new(0));
        let resize = Arc::new(AtomicBool::new(false));
        let epoch = Arc::new(AtomicU64::new(0));
        let shutdown = crate::edge::ShutdownSignals::register();
        let tasks = start_tasks(Arc::clone(&resize), Arc::clone(&epoch));
        Self {
            stop,
            resize,
            epoch,
            tasks,
            shutdown: Some(shutdown),
        }
    }

    /// Wraps standard input with shutdown and resize observations.
    pub(super) fn terminal(&self, stdin: io::Stdin) -> SignalTermIo {
        SignalTermIo {
            inner: CrosstermTermIo::new(stdin),
            stop: Arc::clone(&self.stop),
            resize: Arc::clone(&self.resize),
            epoch: Arc::clone(&self.epoch),
        }
    }

    /// Returns the signal exit status, when the edge was interrupted.
    pub(super) fn exit_status(&self) -> Option<u8> {
        let status = self.stop.load(Ordering::SeqCst);
        (status != 0).then_some(status)
    }

    pub(super) async fn await_shutdown(&mut self) -> u8 {
        let code = match self.shutdown.as_mut() {
            Some(watcher) => watcher.next().await,
            None => unreachable!("into_shutdown consumed this Signals"),
        };
        self.stop.store(code, Ordering::SeqCst);
        code
    }

    pub(super) fn into_shutdown(mut self) -> crate::edge::ShutdownSignals {
        for task in &self.tasks {
            task.abort();
        }
        match self.shutdown.take() {
            Some(watcher) => watcher,
            None => unreachable!("into_shutdown consumes self exactly once"),
        }
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(unix)]
fn start_tasks(resize: Arc<AtomicBool>, epoch: Arc<AtomicU64>) -> Vec<JoinHandle<()>> {
    use tokio::signal::unix::SignalKind;

    let window_change = crate::edge::register_signal(SignalKind::window_change(), "SIGWINCH");
    #[expect(
        clippy::disallowed_methods,
        reason = "process edge: the process edge owns its signal watchers"
    )]
    let resize_task = tokio::spawn(async move {
        let Some(mut window_change) = window_change else {
            return;
        };
        while window_change.recv().await.is_some() {
            epoch.fetch_add(1, Ordering::SeqCst);
            resize.store(true, Ordering::SeqCst);
        }
    });
    vec![resize_task]
}

/// Watches the console Ctrl+C event (130) and the console close event (129,
/// the terminal-hangup status). Resize arrives as a terminal input event.
#[cfg(windows)]
fn start_tasks(_resize: Arc<AtomicBool>, _epoch: Arc<AtomicU64>) -> Vec<JoinHandle<()>> {
    Vec::new()
}

/// Terminal adapter whose signal state is captured by the process edge.
pub(super) struct SignalTermIo {
    inner: CrosstermTermIo,
    stop: Arc<AtomicU8>,
    resize: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
}

impl TermIo for SignalTermIo {
    fn enable_raw(&self) -> io::Result<()> {
        self.inner.enable_raw()
    }

    fn disable_raw(&self) {
        self.inner.disable_raw();
    }

    fn size(&self) -> io::Result<(u16, u16)> {
        self.inner.size()
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write(bytes)
    }

    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        if self.shutdown_code().is_some() {
            return Ok(Vec::new());
        }
        self.inner.read(timeout)
    }

    fn raise_tstp(&self) {
        self.inner.raise_tstp();
    }

    fn shutdown_code(&self) -> Option<u8> {
        let status = self.stop.load(Ordering::SeqCst);
        (status != 0).then_some(status)
    }

    fn take_resize(&self) -> bool {
        self.resize.swap(false, Ordering::SeqCst)
    }

    /// SIGWINCH count plus any inner resize events (Windows `Event::Resize`).
    fn resize_epoch(&self) -> u64 {
        self.epoch
            .load(Ordering::SeqCst)
            .saturating_add(self.inner.resize_epoch())
    }

    /// Caret queries hold during shutdown; otherwise the adapter measures.
    fn cursor_position(&self, timeout: Duration) -> io::Result<CursorReport> {
        if self.shutdown_code().is_some() {
            return Ok(CursorReport::Silent);
        }
        self.inner.cursor_position(timeout)
    }

    fn open_url(&self, url: &str) -> io::Result<()> {
        crate::edge::open_browser(url)
    }
}
