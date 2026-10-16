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
}

impl Signals {
    /// Registers handlers before raw mode or the blocking client starts.
    pub(super) fn start() -> io::Result<Self> {
        let stop = Arc::new(AtomicU8::new(0));
        let resize = Arc::new(AtomicBool::new(false));
        let epoch = Arc::new(AtomicU64::new(0));
        let tasks = start_tasks(Arc::clone(&stop), Arc::clone(&resize), Arc::clone(&epoch))?;
        Ok(Self {
            stop,
            resize,
            epoch,
            tasks,
        })
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
}

impl Drop for Signals {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(unix)]
fn start_tasks(
    stop: Arc<AtomicU8>,
    resize: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
) -> io::Result<Vec<JoinHandle<()>>> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut window_change = signal(SignalKind::window_change())?;
    #[expect(
        clippy::disallowed_methods,
        reason = "process edge: the process edge owns its signal watchers"
    )]
    let stop_task = tokio::spawn(async move {
        let code = tokio::select! {
            _ = interrupt.recv() => 130,
            _ = terminate.recv() => 143,
            _ = hangup.recv() => 129,
        };
        stop.store(code, Ordering::SeqCst);
    });
    #[expect(
        clippy::disallowed_methods,
        reason = "process edge: the process edge owns its signal watchers"
    )]
    let resize_task = tokio::spawn(async move {
        while window_change.recv().await.is_some() {
            epoch.fetch_add(1, Ordering::SeqCst);
            resize.store(true, Ordering::SeqCst);
        }
    });
    Ok(vec![stop_task, resize_task])
}

#[cfg(not(unix))]
#[expect(
    clippy::disallowed_methods,
    reason = "process edge: the process edge owns its signal watchers"
)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the unix variant fails on signal-driver setup; this variant cannot fail"
)]
fn start_tasks(
    stop: Arc<AtomicU8>,
    _resize: Arc<AtomicBool>,
    _epoch: Arc<AtomicU64>,
) -> io::Result<Vec<JoinHandle<()>>> {
    Ok(vec![tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            stop.store(130, Ordering::SeqCst);
        }
    })])
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
