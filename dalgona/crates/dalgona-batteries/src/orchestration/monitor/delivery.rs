// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Clock-injected batch delivery: output matching, the shared bounded queue,
//! per-monitor rate limiting and coalescing, and the monitor-only wake
//! budget. Batches return to the orchestration core for P3 routing; this
//! module never sends a message itself.

use dal_core::{JobId, Timestamp};

use super::state::{
    FIRE_BUDGET, MUTED_NOTICE, MonitorBatch, MonitorConfig, MonitorEffect, MonitorId, MonitorState,
    OutputLine, PAUSE_NOTICE, QUEUE_OVERHEAD,
};

/// Rolling fire-budget window: one day in seconds.
const FIRE_WINDOW_SECS: i64 = 24 * 3_600;

/// Normalizes one output line: retains its last 65,536 Unicode scalar
/// values, replaces CR and LF with one space, and trims trailing spaces.
fn normalize_line(line: &str) -> Box<str> {
    let retained: String = line
        .chars()
        .rev()
        .take(super::state::MAX_RETAINED_LINE)
        .collect();
    let retained: String = retained.chars().rev().collect();
    let mut normalized = String::with_capacity(retained.len());
    for ch in retained.chars() {
        if ch == '\r' || ch == '\n' {
            normalized.push(' ');
        } else {
            normalized.push(ch);
        }
    }
    while normalized.ends_with(' ') {
        normalized.pop();
    }
    normalized.into_boxed_str()
}

/// Milliseconds from `earlier` to `now`, saturating skew below zero.
fn ms_since(now: Timestamp, earlier: Timestamp) -> u64 {
    if now < earlier {
        return 0;
    }
    let seconds = now.as_second().saturating_sub(earlier.as_second());
    let nanos =
        i64::from(now.subsec_nanosecond()).saturating_sub(i64::from(earlier.subsec_nanosecond()));
    let total_nanos = i128::from(seconds) * 1_000_000_000 + i128::from(nanos);
    u64::try_from(total_nanos.div_euclid(1_000_000)).unwrap_or(u64::MAX)
}

/// Seconds from `earlier` to `now`, saturating skew below zero.
fn secs_since(now: Timestamp, earlier: Timestamp) -> i64 {
    if now < earlier {
        return 0;
    }
    now.as_second().saturating_sub(earlier.as_second())
}

/// Total queued characters across the shared output queue.
fn queue_chars(state: &MonitorState) -> usize {
    state
        .output
        .iter()
        .map(|line| line.text.chars().count())
        .fold(0_usize, usize::saturating_add)
}

/// Enqueues one matching line, evicting the oldest lines first while the
/// shared queue exceeds `max_lines` or `max_chars - 512`. Each eviction
/// counts toward its owner's dropped-line total. A line that fits nowhere
/// is dropped against its own monitor.
fn push_line(
    state: &mut MonitorState,
    id: MonitorId,
    text: &str,
    at: Timestamp,
    config: &MonitorConfig,
) {
    let capacity = usize::try_from(
        u64::from(u32::try_from(config.max_chars).unwrap_or(u32::MAX))
            .saturating_sub(QUEUE_OVERHEAD),
    )
    .unwrap_or(usize::MAX);
    let width = text.chars().count();
    while state.output.len() >= config.max_lines
        || queue_chars(state).saturating_add(width) > capacity
    {
        let Some(oldest) = state.output.pop_front() else {
            break;
        };
        if let Some(owner) = state.monitors.get_mut(&oldest.monitor) {
            owner.overflow_lines = owner.overflow_lines.saturating_add(1);
        }
    }
    if state.output.is_empty() && width > capacity {
        if let Some(owner) = state.monitors.get_mut(&id) {
            owner.overflow_lines = owner.overflow_lines.saturating_add(1);
        }
        return;
    }
    state.output.push_back(OutputLine {
        monitor: id,
        text: text.into(),
        at,
    });
}

/// Records one matching output line against every live watch on its job.
/// Muted and paused watches still count toward the rolling fire budget but
/// queue nothing. Crossing the fire budget mutes the monitor and emits the
/// exact auto-mute notice once; line 201 is never delivered.
pub(crate) fn on_output(
    state: &mut MonitorState,
    job: JobId,
    line: &str,
    now: Timestamp,
    config: &MonitorConfig,
) -> Vec<MonitorEffect> {
    let mut effects = Vec::new();
    let normalized = normalize_line(line);
    let text: &str = &normalized;
    let mut interested: Vec<MonitorId> = state
        .monitors
        .iter()
        .filter(|(_, monitor)| monitor.job == job && !monitor.stopped)
        .map(|(id, _)| *id)
        .collect();
    interested.sort_by_key(|id| id.0);
    for id in interested {
        let Some(monitor) = state.monitors.get_mut(&id) else {
            continue;
        };
        if !monitor.filter.is_match(text) {
            continue;
        }
        monitor.matched_lines = monitor.matched_lines.saturating_add(1);
        monitor.matched_at.push_back(now);
        while let Some(oldest) = monitor.matched_at.front() {
            if secs_since(now, *oldest) > FIRE_WINDOW_SECS {
                let _ = monitor.matched_at.pop_front();
            } else {
                break;
            }
        }
        if monitor.muted {
            continue;
        }
        if monitor.matched_at.len() > FIRE_BUDGET {
            monitor.muted = true;
            effects.push(MonitorEffect::Notice(MUTED_NOTICE.into()));
            continue;
        }
        if monitor.paused {
            continue;
        }
        push_line(state, id, text, now, config);
    }
    effects
}

/// Flushes deliverable batches to P3 in monitor-id order. A monitor
/// delivers when it holds queued lines, its rate-limit interval has passed
/// since its last batch, and its oldest queued line has coalesced. A duplicate
/// batch with the same line timestamps and rendered text is consumed and
/// suppressed. Stopped watches are reaped with one `Stopped` effect so the
/// core can release their job subscriptions.
pub(crate) fn flush(
    state: &mut MonitorState,
    now: Timestamp,
    config: &MonitorConfig,
) -> Vec<MonitorEffect> {
    let mut effects = Vec::new();
    let mut ids: Vec<MonitorId> = state.monitors.keys().copied().collect();
    ids.sort_by_key(|id| id.0);
    for id in ids {
        let Some(monitor) = state.monitors.get(&id) else {
            continue;
        };
        if monitor.stopped {
            let _ = state.monitors.remove(&id);
            state.output.retain(|line| line.monitor != id);
            effects.push(MonitorEffect::Stopped(id));
            continue;
        }
        if monitor.paused || monitor.muted {
            continue;
        }
        let oldest = state
            .output
            .iter()
            .filter(|line| line.monitor == id)
            .map(|line| line.at)
            .min();
        let Some(oldest) = oldest else {
            continue;
        };
        if let Some(last) = monitor.last_batch_at
            && ms_since(now, last) < config.rate_limit_ms
        {
            continue;
        }
        if ms_since(now, oldest) < config.coalesce_ms {
            continue;
        }
        let mut line_times = Vec::new();
        let mut lines = Vec::new();
        for line in &state.output {
            if line.monitor != id {
                continue;
            }
            line_times.push(line.at);
            lines.push(
                format!("Monitor event({}): {}", monitor.description, line.text).into_boxed_str(),
            );
        }
        state.output.retain(|line| line.monitor != id);
        let dropped = monitor.overflow_lines;
        let batch = MonitorBatch {
            monitor: id,
            job_display: monitor.job_display.clone(),
            description: monitor.description.clone(),
            lines,
            dropped,
        };
        let fingerprint: Box<str> = format!("{line_times:?}\0{}", batch.text()).into_boxed_str();
        if monitor.last_batch_fingerprint.as_deref() == Some(&fingerprint) {
            continue;
        }
        if let Some(monitor) = state.monitors.get_mut(&id) {
            monitor.last_batch_at = Some(now);
            monitor.last_batch_fingerprint = Some(fingerprint);
            monitor.overflow_lines = 0;
        }
        effects.push(MonitorEffect::Batch(batch));
    }
    state.last_flush = Some(now);
    effects
}

/// Tracks monitor-only automatic wakes. A P3-only wake with deliveries
/// extends the streak; anything else resets it. At the wake budget the
/// monitors behind those wakes pause with the exact pause notice, and the
/// streak restarts so a later rearm buys a fresh budget.
pub(crate) fn update_monitor_only_wake(
    state: &mut MonitorState,
    delivered: &[MonitorId],
    was_monitor_only: bool,
    config: &MonitorConfig,
) -> Vec<MonitorEffect> {
    if !was_monitor_only || delivered.is_empty() {
        state.monitor_only_wakes = 0;
        return Vec::new();
    }
    state.monitor_only_wakes = state.monitor_only_wakes.saturating_add(1);
    if state.monitor_only_wakes < config.wake_budget {
        return Vec::new();
    }
    state.monitor_only_wakes = 0;
    let mut paused: Vec<MonitorId> = delivered.to_vec();
    paused.sort_by_key(|id| id.0);
    paused.dedup_by_key(|id| id.0);
    for id in paused {
        if let Some(monitor) = state.monitors.get_mut(&id) {
            monitor.paused = true;
        }
    }
    vec![MonitorEffect::Notice(PAUSE_NOTICE.into())]
}
