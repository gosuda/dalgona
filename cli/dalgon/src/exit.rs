//! Exit-code mapping and the shutdown ladder for command and signal outcomes.

use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use dal_agent::{Host, ShutdownReport};

/// The whole ladder, from the first exit request to the log flush. It fits
/// inside the Windows close-event window.
const TOTAL: Duration = Duration::from_millis(3500);

/// The grace clock for extension status kinds to go quiet. It is part of
/// `TOTAL`, not added to it.
const GRACE: Duration = Duration::from_secs(3);

/// The runtime stop ceiling, and its budget when no ladder is running.
const RUNTIME_STOP: Duration = Duration::from_millis(500);

/// The instant of the first exit request. The ladder runs once per process:
/// a later request joins it instead of restarting the clock.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// The single monotonic clock of the shutdown ladder.
///
/// Quiet wait, host close, runtime stop, and log flush each take what is left
/// of one deadline, `began + TOTAL`. No phase starts a clock of its own, so
/// the phases the CLI controls can never sum past `TOTAL`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ladder {
    began: Instant,
}

/// The result of closing the host inside the ladder.
#[derive(Debug)]
pub(crate) struct Closed {
    /// What the host reported. The quiet flag reflects the grace clock.
    #[expect(
        dead_code,
        reason = "no caller reads the shutdown report yet; callers consume only `skipped_by`"
    )]
    pub(crate) report: ShutdownReport,
    /// The `128 + signo` status of a further shutdown signal that ended the
    /// quiet wait. The close itself always runs to completion.
    pub(crate) skipped_by: Option<u8>,
}

impl Ladder {
    /// Starts the ladder at the first exit request, or joins the running one.
    pub(crate) fn begin() -> Self {
        Self {
            began: *BEGAN.get_or_init(Instant::now),
        }
    }

    /// The running ladder, if an exit has been requested.
    fn running() -> Option<Self> {
        BEGAN.get().map(|began| Self { began: *began })
    }

    /// What is left of the one deadline at `now`.
    fn remaining_at(self, now: Instant) -> Duration {
        (self.began + TOTAL).saturating_duration_since(now)
    }

    /// What the grace clock has left at `now`.
    fn grace_left_at(self, now: Instant) -> Duration {
        (self.began + GRACE).saturating_duration_since(now)
    }

    /// The time each session close may take: everything left on the clock.
    fn close_grace_at(self, now: Instant) -> Duration {
        self.remaining_at(now)
    }

    fn runtime_budget_at(self, now: Instant) -> Duration {
        self.remaining_at(now).min(RUNTIME_STOP)
    }

    fn flush_budget_at(self, now: Instant) -> Duration {
        self.remaining_at(now).min(crate::log::FLUSH_DEADLINE)
    }

    /// Waits for the host's status kinds to go quiet inside the grace clock,
    /// then closes it.
    ///
    /// A shutdown signal during the quiet wait is the second request: it ends
    /// the wait at once and is reported in [`Closed::skipped_by`], so the
    /// caller exits with that signal's status. Only the quiet poll is ever
    /// abandoned. The close is awaited to completion, because dropping it
    /// would detach a provider credential write that can still commit after
    /// the host is gone. The close grace handed to the host is what is left
    /// of the one deadline. The subscription is the one the run phase used,
    /// so a second signal cannot fall between two registrations.
    pub(crate) async fn close_host(
        self,
        host: Host,
        signals: &mut crate::edge::ShutdownSignals,
    ) -> Closed {
        let until = tokio::time::Instant::now() + self.grace_left_at(Instant::now());
        let (quiet, skipped_by) = tokio::select! {
            biased;
            code = signals.next() => (false, Some(code)),
            quiet = wait_quiet(&host, until) => (quiet, None),
        };
        let report = host
            .shutdown_after_quiet_wait(self.close_grace_at(Instant::now()))
            .await;
        Closed {
            report: ShutdownReport {
                status_quiet: quiet,
                ..report
            },
            skipped_by,
        }
    }

    /// Closes the host after the front end already waited for its status
    /// kinds, so the grace clock is not spent a second time.
    pub(crate) async fn close_host_after_quiet_wait(self, host: Host) -> Closed {
        let report = host
            .shutdown_after_quiet_wait(self.close_grace_at(Instant::now()))
            .await;
        Closed {
            report,
            skipped_by: None,
        }
    }
}

/// Polls the host until every status kind is quiet or `until` passes.
async fn wait_quiet(host: &Host, until: tokio::time::Instant) -> bool {
    loop {
        if host.is_quiet().await {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now >= until {
            return false;
        }
        tokio::time::sleep((until - now).min(crate::print::QUIET_RECHECK)).await;
    }
}

/// How long to wait for the runtime's blocking tasks at process exit: the
/// time left on the ladder's clock, at most the runtime stop ceiling.
pub(crate) fn runtime_budget() -> Duration {
    Ladder::running().map_or(RUNTIME_STOP, |ladder| {
        ladder.runtime_budget_at(Instant::now())
    })
}

/// How long the final log flush may wait: the time left on the ladder's
/// clock, at most the sink's own flush ceiling.
pub(crate) fn flush_budget() -> Duration {
    Ladder::running().map_or(crate::log::FLUSH_DEADLINE, |ladder| {
        ladder.flush_budget_at(Instant::now())
    })
}

/// The outcome category observed by the process edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExitKind {
    Success,
    RequestedFailure,
    Usage,
    Internal,
    Signal(u8),
}

/// Maps one typed outcome to its stable process status.
pub(crate) const fn status(kind: ExitKind) -> u8 {
    match kind {
        ExitKind::Success => 0,
        ExitKind::RequestedFailure => 1,
        ExitKind::Usage => 124,
        ExitKind::Internal => 125,
        ExitKind::Signal(signal) => 128_u8.saturating_add(signal),
    }
}

pub(crate) fn code(kind: ExitKind) -> ExitCode {
    ExitCode::from(status(kind))
}

/// Maps a broken-pipe write failure to its SIGPIPE exit, if it applies.
pub(crate) fn broken_pipe(error: &std::io::Error) -> Option<ExitCode> {
    (error.kind() == std::io::ErrorKind::BrokenPipe).then(|| code(ExitKind::Signal(13)))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{ExitKind, GRACE, Ladder, RUNTIME_STOP, TOTAL, status};
    use crate::log::FLUSH_DEADLINE;

    #[cfg(unix)]
    use dal_agent::ext::{ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};
    #[cfg(unix)]
    use dal_core::ServiceSet;

    #[test]
    fn every_phase_budget_is_the_time_left_on_one_clock() {
        let began = Instant::now();
        let ladder = Ladder { began };
        for elapsed in [0, 1, 1_000, 2_999, 3_000, 3_499, 3_500, 4_000] {
            let elapsed = Duration::from_millis(elapsed);
            let now = began + elapsed;
            let left = TOTAL.saturating_sub(elapsed);
            assert_eq!(ladder.remaining_at(now), left);
            assert_eq!(ladder.close_grace_at(now), left, "close takes what is left");
            assert_eq!(ladder.runtime_budget_at(now), left.min(RUNTIME_STOP));
            assert_eq!(ladder.flush_budget_at(now), left.min(FLUSH_DEADLINE));
            assert!(
                ladder.grace_left_at(now) <= left,
                "the grace clock is inside the total, not added to it"
            );
        }
    }

    #[test]
    fn the_phases_in_order_never_sum_past_the_total() {
        // Worst case for the CLI-controlled phases: the quiet wait uses all
        // of its grace, the close then uses everything left, and the tail
        // steps find nothing left. No phase gets a clock of its own.
        let began = Instant::now();
        let ladder = Ladder { began };
        let after_quiet = began + ladder.grace_left_at(began);
        assert_eq!(after_quiet - began, GRACE);
        let after_close = after_quiet + ladder.close_grace_at(after_quiet);
        assert_eq!(after_close - began, TOTAL);
        assert_eq!(ladder.runtime_budget_at(after_close), Duration::ZERO);
        assert_eq!(ladder.flush_budget_at(after_close), Duration::ZERO);
    }

    #[test]
    fn stable_failure_and_signal_statuses() {
        assert_eq!(status(ExitKind::Success), 0);
        assert_eq!(status(ExitKind::RequestedFailure), 1);
        assert_eq!(status(ExitKind::Usage), 124);
        assert_eq!(status(ExitKind::Internal), 125);
        assert_eq!(status(ExitKind::Signal(2)), 130);
        assert_eq!(status(ExitKind::Signal(13)), 141);
        assert_eq!(status(ExitKind::Signal(15)), 143);
    }

    /// A host with a busy extension status, so `close_host` parks in its
    /// quiet wait until a signal arrives.
    #[cfg(unix)]
    async fn busy_host() -> (tempfile::TempDir, dal_agent::Host) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("data dir");
        let config = dal_core::Config::load(
            dal_core::ConfigProduct::Dalgon,
            &data,
            "",
            Some("model = \"openai/gpt-6-luna\"\n"),
        )
        .expect("config");
        let extension = ExtensionBuilder::new("focus", "0.1.0", ServiceSet::default())
            .expect("builder")
            .status_kind("focus", busy_status())
            .build()
            .expect("extension");
        let product = dal_agent::Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: vec![extension],
            bundled: Vec::new(),
        };
        let env = dal_agent::Env {
            vars: std::collections::BTreeMap::from([(
                std::ffi::OsString::from("OPENAI_API_KEY"),
                std::ffi::OsString::from("sk-test"),
            )]),
            cwd: tmp.path().to_path_buf(),
            sandbox_helper: None,
        };
        let host = dal_agent::Host::start(product, config, env)
            .await
            .expect("host");
        // The busy status kind lives on an open session: without a session
        // the host is trivially quiet and the close never parks.
        let workspace = tmp.path().join("w");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        host.open(
            dal_agent::SessionRef::Ephemeral {
                workspace: dal_core::Workspace::new(workspace).expect("workspace"),
            },
            dal_core::ClientId::new("cli"),
        )
        .await
        .expect("open");
        (tmp, host)
    }

    /// A status poll that always reports busy, so the host never goes quiet.
    #[cfg(unix)]
    fn busy_status() -> std::sync::Arc<dyn StatusPoll> {
        struct Busy;
        impl StatusPoll for Busy {
            fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
                StatusSnapshot {
                    quiet: false,
                    text: None,
                }
            }
        }
        std::sync::Arc::new(Busy)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_host_records_a_second_signal_on_the_shared_subscription() {
        let (tmp, host) = busy_host().await;
        let mut signals = crate::edge::ShutdownSignals::register();
        // The first status plays the run-phase role: the run observes it and
        // starts the ladder, exactly as `drive` would. The same subscription
        // then passes to the close phase.
        signals.enqueue(143);
        let first = signals.next().await;
        assert_eq!(first, 143, "the run phase observes the first SIGTERM");
        let ladder = Ladder {
            began: Instant::now(),
        };
        // The second status arrives while `drive` has returned and the close
        // phase is about to wait: exactly the gap the ladder must cover. It
        // is queued on the one subscription the close receives, so the close
        // records it; the pre-fix code registered a fresh subscription here,
        // saw nothing, and waited out the quiet clock instead.
        signals.enqueue(143);
        let closed = ladder.close_host(host.clone(), &mut signals).await;
        assert_eq!(
            closed.skipped_by,
            Some(143),
            "the second signal during close must decide the exit status"
        );
        drop(host);
        drop(tmp);
    }
}
