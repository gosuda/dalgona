//! The bounded process log sink owned by the process edge.
//!
//! One writer thread appends to the family log file so callers only enqueue
//! into a fixed-capacity channel and never block. The queue sheds on full,
//! the drop count is appended once at flush, and nothing here ever writes to
//! the terminal. `DAL_LOG` selects the highest stored level. Product
//! `tracing` events forward into the sink through a process-wide
//! dispatcher; see [`install_tracing`].

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Barrier, Condvar, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use tracing::{
    Event, Id, Level, Metadata, Subscriber,
    field::{Field, Visit},
    span,
};

use crate::edge::LogLevel;

/// Lines the in-memory queue holds before shedding begins.
pub(crate) const QUEUE_CAPACITY: usize = 4096;

/// Size at which the previous log rotates to `<name>.log.1` at startup.
pub(crate) const ROTATE_BYTES: u64 = 10 * 1024 * 1024;

/// Longest shutdown wait for the writer to drain and confirm the flush.
pub(crate) const FLUSH_DEADLINE: Duration = Duration::from_secs(3);

/// The process-wide sink installed once by the edge.
static GLOBAL: OnceLock<LogSink> = OnceLock::new();

/// Typed failures of opening, rotating, or installing the log sink.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LogError {
    /// The log file or its parent directory could not be opened.
    #[error("cannot open the log at {path}: {source}")]
    Open {
        /// The log file path.
        path: PathBuf,
        /// The operating-system error.
        source: std::io::Error,
    },
    /// The oversized previous log could not move to its rotation slot.
    #[error("cannot rotate the log {from} to {to}: {source}")]
    Rotate {
        /// The previous log path.
        from: PathBuf,
        /// The rotation path.
        to: PathBuf,
        /// The operating-system error.
        source: std::io::Error,
    },
    /// The log file could not be restricted to its owner.
    #[cfg(unix)]
    #[error("cannot restrict the log at {path} to its owner: {source}")]
    Permissions {
        /// The log file path.
        path: PathBuf,
        /// The operating-system error.
        source: std::io::Error,
    },
    /// The writer thread could not start.
    #[error("cannot start the log writer: {source}")]
    Writer {
        /// The operating-system error.
        source: std::io::Error,
    },
    /// The process-wide slot already holds a sink.
    #[error("a process-wide log sink is already installed")]
    AlreadyInstalled,
}

/// Why one shutdown flush did not fully report or persist.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FlushFailure {
    /// A log line or the drop summary could not be written.
    #[error("cannot write the log: {source}")]
    Write {
        /// The operating-system error.
        source: std::io::Error,
    },
    /// The writer thread stopped before the flush was confirmed.
    #[error("the log writer has stopped")]
    WriterStopped,
    /// The writer did not confirm the flush inside the deadline.
    #[error("timed out waiting for the log writer to flush")]
    Timeout,
}

/// One enqueue command for the owned writer thread.
enum Message {
    /// One rendered log line.
    Line(Box<str>),
    /// Drain, flush, and append the drop summary if lines were shed.
    Flush(Arc<FlushShared>),
}

/// Shared state one flush wait resolves through.
#[derive(Default)]
struct FlushShared {
    /// Set once the writer processed this flush.
    done: Mutex<bool>,
    /// The write failure, if any; taken by the waiter.
    error: Mutex<Option<FlushFailure>>,
    /// Wakes the waiting thread when `done` moves.
    condvar: Condvar,
}

/// The state shared by every sink handle.
struct SinkState {
    /// Bounded queue into the writer; full means shed.
    sender: Mutex<SyncSender<Message>>,
    /// Highest verbosity the configured `DAL_LOG` level admits.
    level: LogLevel,
    /// Lines shed at enqueue, cumulative over the sink lifetime.
    dropped: Arc<AtomicU64>,
    /// Set at flush so later events shed instead of bypassing the summary.
    closed: AtomicBool,
}

/// A cloneable handle onto the bounded process log. Dropping the last
/// handle does not flush; call [`LogSink::flush`] at shutdown.
#[derive(Clone)]
#[must_use]
pub(crate) struct LogSink {
    /// The shared queue, level, and drop accounting.
    inner: Arc<SinkState>,
}

impl LogSink {
    /// Opens the family log at `log_path`, rotating an oversized previous
    /// file, and starts the one owned writer thread.
    ///
    /// # Errors
    /// Returns [`LogError`] when the file, its rotation, or the writer thread
    /// cannot start.
    pub(crate) fn open(level: Option<LogLevel>, log_path: &Path) -> Result<LogSink, LogError> {
        Self::open_gated(level, log_path, None)
    }

    /// Opens the sink with an optional gate that parks the writer before its
    /// first dequeue, so a test can fill the queue deterministically.
    ///
    /// # Errors
    /// Returns [`LogError`] under the same conditions as [`LogSink::open`].
    pub(crate) fn open_gated(
        level: Option<LogLevel>,
        log_path: &Path,
        gate: Option<Arc<Barrier>>,
    ) -> Result<LogSink, LogError> {
        let file = open_rotated(log_path)?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = LogSink {
            inner: Arc::new(SinkState {
                sender: Mutex::new(sender),
                level: level.unwrap_or(LogLevel::Warning),
                dropped: Arc::clone(&dropped),
                closed: AtomicBool::new(false),
            }),
        };
        thread::Builder::new()
            .name("dalgon-log".into())
            .spawn(move || writer_loop(BufWriter::new(file), receiver, &dropped, gate))
            .map_err(|source| LogError::Writer { source })?;
        Ok(sink)
    }

    /// Installs this sink into the process-wide slot.
    ///
    /// # Errors
    /// Returns [`LogError::AlreadyInstalled`] when the slot is taken.
    pub(crate) fn install(&self) -> Result<(), LogError> {
        GLOBAL
            .set(self.clone())
            .map_err(|_| LogError::AlreadyInstalled)
    }

    /// Returns the installed process-wide sink, if any.
    pub(crate) fn global() -> Option<&'static LogSink> {
        GLOBAL.get()
    }

    /// Reports every line shed so far.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Acquire)
    }

    /// Whether one level passes the configured `DAL_LOG` verbosity.
    fn admits(&self, level: LogLevel) -> bool {
        severity(level) <= severity(self.inner.level)
    }

    /// Stores one log line unless it sits above the configured level.
    ///
    /// Filtered lines are neither stored nor counted; shed lines increment
    /// the drop counter. The call never blocks on the writer: a full queue
    /// sheds.
    pub(crate) fn log(&self, level: LogLevel, target: &str, message: impl fmt::Display) {
        if !self.admits(level) {
            return;
        }
        if self.inner.closed.load(Ordering::Acquire) {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let line = format!("{} {target}: {message}", label(level));
        let sent = self
            .inner
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_send(Message::Line(line.into_boxed_str()));
        if sent.is_err() {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Closes the sink, waits for the writer to drain, and reports failure.
    ///
    /// The writer appends `dropped <n> log lines` once when the count is
    /// non-zero. Later events shed and increment the counter without a
    /// second summary. This blocks on the writer handshake, so the process
    /// edge calls it only after the async runtime has shut down.
    ///
    /// # Errors
    /// Returns [`FlushFailure`] when a write failed, the writer stopped, or
    /// the writer did not confirm inside `deadline`.
    pub(crate) fn flush(&self, deadline: Duration) -> Result<(), FlushFailure> {
        self.inner.closed.store(true, Ordering::Release);
        let shared = Arc::new(FlushShared::default());
        let started = Instant::now();
        loop {
            let sent = self
                .inner
                .sender
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_send(Message::Flush(Arc::clone(&shared)));
            match sent {
                Ok(()) => {
                    return wait_for_flush(&shared, deadline.saturating_sub(started.elapsed()));
                }
                Err(TrySendError::Disconnected(_)) => return Err(FlushFailure::WriterStopped),
                // The writer drains the full queue ahead of the flush, so a
                // full channel frees up; a stalled writer hits the deadline.
                Err(TrySendError::Full(_)) if started.elapsed() >= deadline => {
                    return Err(FlushFailure::Timeout);
                }
                Err(TrySendError::Full(_)) => thread::sleep(Duration::from_millis(1)),
            }
        }
    }
}

/// Routes product `tracing` events into the installed process-wide sink.
///
/// The dispatcher is process lifetime and holds no sink state of its own:
/// every event forwards to whatever sink currently occupies the one global
/// slot, and is dropped silently while no sink is installed. Spans are not
/// tracked; only events carry line content.
struct TracingBridge {
    /// Source of distinct span ids; the ids are never stored.
    span_ids: AtomicU64,
}

impl TracingBridge {
    const fn new() -> Self {
        Self {
            span_ids: AtomicU64::new(1),
        }
    }
}

impl Subscriber for TracingBridge {
    /// Admits events whose level passes the installed sink's `DAL_LOG`
    /// filter; without a sink nothing is captured and nothing passes.
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        LogSink::global().is_some_and(|sink| sink.admits(level_of(*metadata.level())))
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> Id {
        Id::from_u64(self.span_ids.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _span: &Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let Some(sink) = LogSink::global() else {
            return;
        };
        let level = level_of(*event.metadata().level());
        if !sink.admits(level) {
            return;
        }
        let mut visitor = EventMessage::default();
        event.record(&mut visitor);
        let Some(text) = visitor.render() else {
            return;
        };
        sink.log(level, event.metadata().target(), &text);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Installs the tracing bridge as the process-wide dispatcher.
///
/// A second call keeps the first dispatcher, so repeated in-process runs
/// and unrelated tests sharing the process are not poisoned. Events flow
/// from every thread and async task from this call onward.
pub(crate) fn install_tracing() {
    let _ = tracing::subscriber::set_global_default(TracingBridge::new());
}

/// Collects one event's `message` field plus its structured fields.
#[derive(Default)]
struct EventMessage {
    message: Option<String>,
    fields: Vec<(&'static str, String)>,
}

impl Visit for EventMessage {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message.get_or_insert(format!("{value:?}"));
        } else {
            self.fields.push((field.name(), format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.get_or_insert(value.to_owned());
        } else {
            self.fields.push((field.name(), value.to_owned()));
        }
    }
}

impl EventMessage {
    /// Renders the message followed by `field=value` pairs, or nothing when
    /// the event carried neither.
    fn render(self) -> Option<String> {
        let mut text = self.message.unwrap_or_default();
        for (name, value) in self.fields {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(name);
            text.push('=');
            text.push_str(&value);
        }
        (!text.is_empty()).then_some(text)
    }
}

/// Maps a `tracing` level onto the `DAL_LOG` vocabulary by name, so the
/// mapping never depends on comparison direction; `TRACE` lands on the
/// finest stored level.
fn level_of(level: Level) -> LogLevel {
    match level.as_str() {
        "WARN" => LogLevel::Warning,
        "INFO" => LogLevel::Info,
        "DEBUG" | "TRACE" => LogLevel::Debug,
        _ => LogLevel::Error,
    }
}

/// Ranks one level by severity; a higher rank passes more lines.
fn severity(level: LogLevel) -> u8 {
    match level {
        LogLevel::Error => 0,
        LogLevel::Warning => 1,
        LogLevel::Info => 2,
        LogLevel::Debug => 3,
    }
}

/// Renders one level with the `DAL_LOG` vocabulary.
const fn label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warning => "warning",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    }
}

/// Opens the log file after rotating an oversized previous one.
fn open_rotated(log_path: &Path) -> Result<File, LogError> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LogError::Open {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    if let Ok(metadata) = std::fs::metadata(log_path)
        && metadata.len() > ROTATE_BYTES
    {
        let rotated = rotated_path(log_path);
        #[cfg(windows)]
        {
            // A Windows rename refuses an existing destination.
            let _ = std::fs::remove_file(&rotated);
        }
        std::fs::rename(log_path, &rotated).map_err(|source| LogError::Rotate {
            from: log_path.to_path_buf(),
            to: rotated,
            source,
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|source| LogError::Open {
            path: log_path.to_path_buf(),
            source,
        })?;
    restrict_to_owner(log_path, &file)?;
    Ok(file)
}

/// Restricts the log file to its owner on systems with file modes.
fn restrict_to_owner(log_path: &Path, file: &File) -> Result<(), LogError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| LogError::Permissions {
                path: log_path.to_path_buf(),
                source,
            })?;
    }
    #[cfg(not(unix))]
    {
        let _ = (log_path, file);
    }
    Ok(())
}

/// Names the rotation slot of one log file: `<name>.log.1`.
fn rotated_path(log_path: &Path) -> PathBuf {
    let mut name = log_path.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    log_path.with_file_name(name)
}

/// Drains the queue into the file until every sender is gone.
fn writer_loop(
    mut writer: BufWriter<File>,
    receiver: Receiver<Message>,
    dropped: &AtomicU64,
    gate: Option<Arc<Barrier>>,
) {
    if let Some(gate) = gate {
        gate.wait();
    }
    let mut summary_written = false;
    let mut write_error: Option<std::io::Error> = None;
    for message in receiver {
        match message {
            Message::Line(line) => {
                let timestamp = jiff::Timestamp::now();
                if let Err(source) = writeln!(writer, "{timestamp} {line}") {
                    write_error.get_or_insert(source);
                }
            }
            Message::Flush(shared) => {
                let error = finish(
                    &mut writer,
                    dropped,
                    &mut summary_written,
                    write_error.take(),
                );
                *shared.error.lock().unwrap_or_else(PoisonError::into_inner) = error;
                *shared.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
                shared.condvar.notify_all();
            }
        }
    }
    let _ = finish(
        &mut writer,
        dropped,
        &mut summary_written,
        write_error.take(),
    );
    drop(writer);
}

/// Flushes the writer and appends the drop summary at most once, reporting
/// the first earlier line-write failure ahead of any later one.
fn finish(
    writer: &mut BufWriter<File>,
    dropped: &AtomicU64,
    summary_written: &mut bool,
    earlier: Option<std::io::Error>,
) -> Option<FlushFailure> {
    let mut failure = earlier.map(|source| FlushFailure::Write { source });
    if !*summary_written {
        let count = dropped.load(Ordering::Acquire);
        if count > 0 {
            *summary_written = true;
            if let Err(source) = writeln!(writer, "dropped {count} log lines") {
                failure.get_or_insert(FlushFailure::Write { source });
            }
        }
    }
    if let Err(source) = writer.flush() {
        failure.get_or_insert(FlushFailure::Write { source });
    }
    failure
}

/// Waits until the writer confirms one flush or the remaining time runs out.
fn wait_for_flush(shared: &FlushShared, remaining: Duration) -> Result<(), FlushFailure> {
    let started = Instant::now();
    let mut done = shared.done.lock().unwrap_or_else(PoisonError::into_inner);
    while !*done {
        let left = remaining.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(FlushFailure::Timeout);
        }
        done = shared
            .condvar
            .wait_timeout_while(done, left, |done| !*done)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    drop(done);
    let error = shared
        .error
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::{
        FLUSH_DEADLINE, LogSink, QUEUE_CAPACITY, ROTATE_BYTES, install_tracing, rotated_path,
    };
    use crate::edge::LogLevel;
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;
    use tempfile::tempdir;

    /// Stores `count` warning lines through the sink boundary.
    fn emit(sink: &LogSink, count: usize) {
        for index in 0..count {
            sink.log(LogLevel::Warning, "dalgon.test", format!("line {index}"));
        }
    }

    #[test]
    fn oversized_log_rotates_and_replaces_the_previous_rotation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("dal.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(rotated_path(&path), vec![b'o'; 64]).unwrap();
        let oversized = vec![b'x'; usize::try_from(ROTATE_BYTES).unwrap() + 1];
        fs::write(&path, &oversized).unwrap();

        let sink = LogSink::open(Some(LogLevel::Warning), &path).unwrap();
        emit(&sink, 1);
        sink.flush(FLUSH_DEADLINE).expect("flush the sink");

        assert_eq!(fs::read(rotated_path(&path)).unwrap(), oversized);
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("line 0"),
            "the fresh log holds new lines: {text}"
        );
        assert!(
            !text.contains("xxxx"),
            "the fresh log is not the old file: {text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fresh_log_files_are_restricted_to_the_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("dal.log");
        let sink = LogSink::open(Some(LogLevel::Warning), &path).unwrap();
        emit(&sink, 1);
        sink.flush(FLUSH_DEADLINE).expect("flush the sink");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the log keeps owner-only mode");
    }

    #[test]
    fn shedding_counts_dropped_lines_and_flush_appends_the_summary_once() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("dal.log");
        // The parked writer cannot drain, so the fill below sheds exactly the
        // lines past the authored queue capacity.
        let gate = Arc::new(Barrier::new(2));
        let sink =
            LogSink::open_gated(Some(LogLevel::Warning), &path, Some(Arc::clone(&gate))).unwrap();
        emit(&sink, QUEUE_CAPACITY + 5);
        assert_eq!(
            sink.dropped(),
            5,
            "the parked writer cannot drain the queue"
        );
        gate.wait();

        sink.flush(FLUSH_DEADLINE).expect("flush the sink");
        assert_eq!(sink.dropped(), 5);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("dropped 5 log lines").count(), 1, "{text}");
        assert!(
            text.contains("line 0"),
            "the first line is retained: {text}"
        );
        assert!(
            text.contains(&format!("line {}", QUEUE_CAPACITY - 1)),
            "the queue holds exactly its capacity: {text}"
        );
        assert!(
            !text.contains(&format!("line {QUEUE_CAPACITY}")),
            "shed lines are absent: {text}"
        );
    }

    #[test]
    fn level_gate_keeps_errors_and_warnings_and_filters_info_and_debug() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("dal.log");
        let sink = LogSink::open(Some(LogLevel::Warning), &path).unwrap();
        sink.log(LogLevel::Debug, "dalgon.test", "debug line");
        sink.log(LogLevel::Info, "dalgon.test", "info line");
        sink.log(LogLevel::Warning, "dalgon.test", "warn line");
        sink.log(LogLevel::Error, "dalgon.test", "error line");
        sink.flush(FLUSH_DEADLINE).expect("flush the sink");
        assert_eq!(sink.dropped(), 0, "filtered lines are not shed lines");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("error dalgon.test: error line"), "{text}");
        assert!(text.contains("warning dalgon.test: warn line"), "{text}");
        assert!(!text.contains("info line"), "{text}");
        assert!(!text.contains("debug line"), "{text}");
        assert!(
            !text.contains("dropped"),
            "no summary without shed lines: {text}"
        );
    }

    #[test]
    fn debug_level_admits_every_level() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("dal.log");
        let sink = LogSink::open(Some(LogLevel::Debug), &path).unwrap();
        sink.log(LogLevel::Debug, "dalgon.test", "debug line");
        sink.log(LogLevel::Info, "dalgon.test", "info line");
        sink.flush(FLUSH_DEADLINE).expect("flush the sink");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("debug dalgon.test: debug line"), "{text}");
        assert!(text.contains("info dalgon.test: info line"), "{text}");
    }

    #[test]
    fn flush_after_close_sheds_and_counts_without_a_second_summary() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("dal.log");
        let sink = LogSink::open(Some(LogLevel::Warning), &path).unwrap();
        emit(&sink, 2);
        sink.flush(FLUSH_DEADLINE).expect("first flush");
        assert_eq!(sink.dropped(), 0, "nothing was shed before the close");
        emit(&sink, 3);
        assert_eq!(sink.dropped(), 3, "a closed sink sheds and counts");
        sink.flush(Duration::from_millis(500))
            .expect("second flush");
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("dropped 3 log lines").count(), 1, "{text}");
        assert!(
            !text.contains("line 2"),
            "post-close lines are not stored: {text}"
        );
    }

    #[test]
    fn concurrent_producers_lose_nothing_below_capacity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("dal.log");
        let sink = LogSink::open(Some(LogLevel::Warning), &path).unwrap();
        let handles: Vec<_> = (0..4)
            .map(|worker| {
                let sink = sink.clone();
                std::thread::spawn(move || {
                    for index in 0..64 {
                        sink.log(
                            LogLevel::Warning,
                            "dalgon.test",
                            format!("worker {worker} line {index}"),
                        );
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        sink.flush(FLUSH_DEADLINE).expect("flush the sink");
        assert_eq!(sink.dropped(), 0);
        let text = fs::read_to_string(&path).unwrap();
        for worker in 0..4 {
            for index in 0..64 {
                let needle = format!("worker {worker} line {index}\n");
                assert!(text.contains(&needle), "missing {needle:?}");
            }
        }
        assert_eq!(text.lines().count(), 4 * 64, "every line stored once");
    }

    #[test]
    fn install_is_once_and_the_global_slot_serves_the_installed_sink() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("dal.log");
        let sink = LogSink::open(Some(LogLevel::Warning), &path).unwrap();
        sink.install().expect("the first install wins the slot");
        let other = LogSink::open(Some(LogLevel::Warning), &path).unwrap();
        assert!(
            other.install().is_err(),
            "a second install must not replace the slot"
        );
        let global = LogSink::global().expect("the installed sink is reachable");
        global.log(LogLevel::Warning, "dalgon.test", "global line");
        global.flush(FLUSH_DEADLINE).expect("flush the sink");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("global line"), "{text}");
    }

    #[test]
    fn rotation_path_appends_one_suffix() {
        assert_eq!(
            rotated_path(Path::new("/t/data/dal/cache/dal.log")),
            Path::new("/t/data/dal/cache/dal.log.1")
        );
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the dispatch proof owns its subprocess environment and streams"
    )]
    fn tracing_dispatch_routes_events_across_threads_into_the_sink() {
        use std::process::{Command, Stdio};

        const CHILD: &str = "DAL_TEST_TRACE_CHILD";
        const LOG_DIR: &str = "DAL_TEST_TRACE_LOG_DIR";
        if std::env::var_os(CHILD).is_some() {
            let dir = std::path::PathBuf::from(std::env::var_os(LOG_DIR).expect("log dir"));
            let path = dir.join("cache").join("dal.log");
            let sink = LogSink::open(Some(LogLevel::Warning), &path).expect("sink");
            sink.install().expect("install the process sink");
            install_tracing();
            install_tracing();
            tracing::warn!(target: "dalgon.store", "sync dispatch line");
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async {
                tokio::spawn(async {
                    tracing::warn!(target: "dalgon.agent", session = 7, "async dispatch line");
                    tracing::info!(target: "dalgon.agent", "filtered info line");
                })
                .await
                .expect("task");
            });
            runtime.shutdown_timeout(Duration::from_millis(500));
            sink.flush(FLUSH_DEADLINE).expect("flush the sink");
            let text = std::fs::read_to_string(&path).expect("read the log");
            assert!(
                text.contains("warning dalgon.store: sync dispatch line"),
                "{text}"
            );
            assert!(
                text.contains("warning dalgon.agent: async dispatch line session=7"),
                "{text}"
            );
            assert!(!text.contains("filtered info line"), "{text}");
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let test_filter = "log::tests::tracing_dispatch_routes_events_across_threads_into_the_sink";
        let child = Command::new(std::env::current_exe().expect("current exe"))
            .args(["--exact", test_filter, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env(LOG_DIR, dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the dispatch child");
        let output = child.wait_with_output().expect("wait for the child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(
            stderr.is_empty(),
            "the bridge must not write to stderr: {stderr}"
        );
        for text in [
            "sync dispatch line",
            "async dispatch line",
            "dalgon.store",
            "dalgon.agent",
        ] {
            assert!(
                !stdout.contains(text),
                "the bridge must not write {text:?} to stdout: {stdout}"
            );
        }
    }
}
