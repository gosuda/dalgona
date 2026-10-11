//! `DAL_DEBUG` frame counters and echo-sample ring for the test harness.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Bounded diagnostics kept only when debug mode is enabled.
#[derive(Debug)]
pub struct DebugLog {
    echo_samples: VecDeque<(Instant, Duration)>,
    frames: u64,
    slow_frames: u64,
    last_slow_log: Option<Instant>,
}

impl DebugLog {
    /// Creates an empty log holding at most 10,000 echo samples.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            echo_samples: VecDeque::new(),
            frames: 0,
            slow_frames: 0,
            last_slow_log: None,
        }
    }

    /// Records one key-to-paint echo sample.
    pub fn sample(&mut self, at: Instant, latency: Duration) {
        if self.echo_samples.len() >= 10_000 {
            self.echo_samples.pop_front();
        }
        self.echo_samples.push_back((at, latency));
    }

    /// Records one painted frame; slow frames log at most once per second.
    pub fn frame(&mut self, wall: Duration, now: Instant) -> Option<String> {
        self.frames += 1;
        if wall <= Duration::from_millis(16) {
            return None;
        }
        self.slow_frames += 1;
        if self
            .last_slow_log
            .is_some_and(|last| now - last < Duration::from_secs(1))
        {
            return None;
        }
        self.last_slow_log = Some(now);
        Some(format!("slow_paint {}ms", wall.as_millis()))
    }

    /// Returns frame and slow-frame counters.
    #[must_use]
    pub const fn counters(&self) -> (u64, u64) {
        (self.frames, self.slow_frames)
    }
}

impl Default for DebugLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::DebugLog;
    use std::time::{Duration, Instant};

    #[test]
    fn slow_paint_logs_at_most_once_per_second() {
        let mut log = DebugLog::new();
        let start = Instant::now();
        assert!(log.frame(Duration::from_millis(20), start).is_some());
        assert!(log.frame(Duration::from_millis(20), start).is_none());
        assert!(
            log.frame(Duration::from_millis(20), start + Duration::from_secs(2))
                .is_some()
        );
        assert_eq!(log.counters(), (3, 3));
    }
}
