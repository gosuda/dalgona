// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! State-machine tests for watch validation, delivery, status, and abort.

use std::collections::HashMap;
use std::error::Error;

use dal_core::{JobId, RawJson, Timestamp};

use super::super::{ControllerMode, JobsView};
use super::delivery::{flush, on_output, update_monitor_only_wake};
use super::state::{
    MonitorConfig, MonitorEffect, MonitorId, MonitorRequest, MonitorState, PAUSE_NOTICE,
    on_job_end, parse_config, parse_request, stop_all, watch,
};
use super::status::{abort_reply, status_json, status_payload, subagent_reply};
use super::*;

fn raw(value: &str) -> Result<RawJson, Box<dyn Error>> {
    Ok(RawJson::parse(value)?)
}

/// Scripted session jobs: display id (`j<n>`) to job id plus liveness.
#[derive(Default)]
struct FakeJobs {
    jobs: HashMap<Box<str>, (JobId, bool)>,
}

impl FakeJobs {
    fn live(display: &str) -> (Self, JobId) {
        let id = JobId::new_v7();
        let mut jobs = Self::default();
        jobs.jobs.insert(display.into(), (id, true));
        (jobs, id)
    }

    fn ended(display: &str) -> Self {
        let mut jobs = Self::default();
        jobs.jobs.insert(display.into(), (JobId::new_v7(), false));
        jobs
    }
}

impl JobsView for FakeJobs {
    fn resolve_job(&self, display: &str) -> Option<JobId> {
        self.jobs.get(display).map(|(id, _)| *id)
    }

    fn is_live_top_level_exec(&self, job: JobId) -> bool {
        self.jobs
            .values()
            .any(|(known, live)| *known == job && *live)
    }
}

fn at_secs(base: Timestamp, secs: u64) -> Timestamp {
    base + jiff::SignedDuration::from_secs(i64::try_from(secs).unwrap_or(i64::MAX))
}

fn watch_request(job: &str, filter: &str) -> Result<MonitorRequest, Box<dyn Error>> {
    Ok(parse_request(&raw(&format!(
        "{{\"action\":\"watch\",\"job\":\"{job}\",\"filter\":\"{filter}\"}}"
    ))?)?)
}

#[test]
fn watch_validates_action_fields_then_missing() -> Result<(), Box<dyn Error>> {
    // Missing action names the action slot.
    let missing = parse_request(&raw("{}")?);
    assert_eq!(
        missing.unwrap_err().to_string(),
        "monitor: action unknown needs action."
    );
    // An inapplicable watch field beats a missing required field.
    let inapplicable = parse_request(&raw(
        "{\"action\":\"watch\",\"id\":\"m1\",\"filter\":\"ok\"}",
    )?);
    assert_eq!(
        inapplicable.unwrap_err().to_string(),
        "monitor: field id does not apply to action watch."
    );
    // Missing job beats a missing filter.
    let no_job = parse_request(&raw("{\"action\":\"watch\",\"filter\":\"ok\"}")?);
    assert_eq!(
        no_job.unwrap_err().to_string(),
        "monitor: action watch needs job."
    );
    let no_filter = parse_request(&raw("{\"action\":\"watch\",\"job\":\"j1\"}")?);
    assert_eq!(
        no_filter.unwrap_err().to_string(),
        "monitor: action watch needs filter."
    );
    // Stop rejects watch-only fields in JSON key order before needing its id.
    let stop_fields = parse_request(&raw(
        "{\"action\":\"stop\",\"job\":\"j1\",\"filter\":\"ok\"}",
    )?);
    assert_eq!(
        stop_fields.unwrap_err().to_string(),
        "monitor: field job does not apply to action stop."
    );
    let stop_no_id = parse_request(&raw("{\"action\":\"stop\"}")?);
    assert_eq!(
        stop_no_id.unwrap_err().to_string(),
        "monitor: action stop needs id."
    );
    Ok(())
}

#[test]
fn watch_rejects_unknown_ended_and_bad_jobs() -> Result<(), Box<dyn Error>> {
    let live = FakeJobs::live("j1").0;
    let ended = FakeJobs::ended("j2");
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let now = Timestamp::UNIX_EPOCH;
    let unknown = watch_request("j9", "ok")?;
    assert_eq!(
        watch(&mut state, &unknown, &live, now, &config)
            .unwrap_err()
            .to_string(),
        "monitor: no job j9 in this session."
    );
    let ended_request = watch_request("j2", "ok")?;
    assert_eq!(
        watch(&mut state, &ended_request, &ended, now, &config)
            .unwrap_err()
            .to_string(),
        "monitor: job j2 is not a running exec job."
    );
    let bad_filter = watch_request("j1", "(")?;
    assert!(
        watch(&mut state, &bad_filter, &live, now, &config)
            .unwrap_err()
            .to_string()
            .starts_with("monitor: filter is not a valid regex: ")
    );
    Ok(())
}

#[test]
fn watch_replies_use_exact_texts_and_default_description() -> Result<(), Box<dyn Error>> {
    let live = FakeJobs::live("j1").0;
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let now = Timestamp::UNIX_EPOCH;
    let request = watch_request("j1", "ok")?;
    let reply = watch(&mut state, &request, &live, now, &config)?;
    assert_eq!(reply.text(), "watching job j1 as m1: \"job j1\".");
    let stop = parse_request(&raw("{\"action\":\"stop\",\"id\":\"m1\"}")?)?;
    assert_eq!(
        watch(&mut state, &stop, &live, now, &config)?.text(),
        "stopped m1."
    );
    let rearm = parse_request(&raw("{\"action\":\"rearm\",\"id\":\"m1\"}")?)?;
    assert_eq!(
        watch(&mut state, &rearm, &live, now, &config)?.text(),
        "rearmed m1."
    );
    let missing = parse_request(&raw("{\"action\":\"stop\",\"id\":\"m9\"}")?)?;
    assert_eq!(
        watch(&mut state, &missing, &live, now, &config)
            .unwrap_err()
            .to_string(),
        "monitor: no monitor m9."
    );
    Ok(())
}

#[test]
fn seventeenth_live_watch_rejects_including_paused_and_muted() -> Result<(), Box<dyn Error>> {
    let (live, _) = FakeJobs::live("j1");
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let now = Timestamp::UNIX_EPOCH;
    for _ in 0..16 {
        let request = watch_request("j1", "ok")?;
        watch(&mut state, &request, &live, now, &config)?;
    }
    // A stopped watch leaves the live count, so one more watch still fits.
    let stop = parse_request(&raw("{\"action\":\"stop\",\"id\":\"m1\"}")?)?;
    watch(&mut state, &stop, &live, now, &config)?;
    let request = watch_request("j1", "ok")?;
    watch(&mut state, &request, &live, now, &config)?;
    // The next watch is seventeenth live and rejects.
    let seventeenth = watch_request("j1", "ok")?;
    assert_eq!(
        watch(&mut state, &seventeenth, &live, now, &config)
            .unwrap_err()
            .to_string(),
        "monitor: 16 monitors are live; stop one first."
    );
    Ok(())
}

#[test]
fn first_batch_coalesces_and_second_obeys_rate_limit() -> Result<(), Box<dyn Error>> {
    let (live, job) = FakeJobs::live("j1");
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let epoch = Timestamp::UNIX_EPOCH;
    let request = watch_request("j1", "ok")?;
    watch(&mut state, &request, &live, epoch, &config)?;
    on_output(&mut state, job, "ok 1", epoch, &config);
    on_output(&mut state, job, "ok 2", at_secs(epoch, 1), &config);
    on_output(&mut state, job, "nope", at_secs(epoch, 1), &config);
    assert!(flush(&mut state, at_secs(epoch, 1), &config).is_empty());
    let effects = flush(&mut state, at_secs(epoch, 2), &config);
    assert_eq!(effects.len(), 1);
    let MonitorEffect::Batch(batch) = &effects[0] else {
        return Err("expected one P3 batch".into());
    };
    assert_eq!(batch.lines.len(), 2);
    assert!(batch.text().contains("Monitor event(job j1): ok 1"));
    // One second later the rate limit still holds; at seven seconds it lifts.
    on_output(&mut state, job, "ok 3", at_secs(epoch, 3), &config);
    assert!(flush(&mut state, at_secs(epoch, 3), &config).is_empty());
    let later = flush(&mut state, at_secs(epoch, 7), &config);
    assert_eq!(later.len(), 1);
    Ok(())
}

#[test]
fn overflow_appends_exact_dropped_trailer() -> Result<(), Box<dyn Error>> {
    let (live, job) = FakeJobs::live("j1");
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let epoch = Timestamp::UNIX_EPOCH;
    let request = watch_request("j1", "ok")?;
    watch(&mut state, &request, &live, epoch, &config)?;
    for index in 0..60 {
        on_output(&mut state, job, &format!("ok {index}"), epoch, &config);
    }
    let effects = flush(&mut state, at_secs(epoch, 2), &config);
    assert_eq!(effects.len(), 1);
    let MonitorEffect::Batch(batch) = &effects[0] else {
        return Err("expected one P3 batch".into());
    };
    assert_eq!(batch.lines.len(), 50);
    assert!(
        batch
            .text()
            .contains("(10 more lines from job j1 were dropped.)")
    );
    Ok(())
}

#[test]
fn fire_budget_delivers_two_hundred_then_mutes_once() -> Result<(), Box<dyn Error>> {
    let (live, job) = FakeJobs::live("j1");
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let epoch = Timestamp::UNIX_EPOCH;
    let request = watch_request("j1", "ok")?;
    watch(&mut state, &request, &live, epoch, &config)?;
    let mut notices = 0;
    for index in 0..201 {
        let at = at_secs(epoch, u64::try_from(index).unwrap_or(u64::MAX));
        for effect in on_output(&mut state, job, "ok", at, &config) {
            if matches!(effect, MonitorEffect::Notice(_)) {
                notices += 1;
            }
        }
    }
    assert_eq!(notices, 1);
    let monitor = state.monitors.values().next().ok_or("watch missing")?;
    assert!(monitor.muted);
    assert_eq!(monitor.matched_lines, 201);
    Ok(())
}

#[test]
fn duplicate_batch_suppresses_second_delivery() -> Result<(), Box<dyn Error>> {
    let (live, job) = FakeJobs::live("j1");
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let epoch = Timestamp::UNIX_EPOCH;
    let request = watch_request("j1", "ok")?;
    watch(&mut state, &request, &live, epoch, &config)?;
    on_output(&mut state, job, "ok", epoch, &config);
    assert_eq!(flush(&mut state, at_secs(epoch, 2), &config).len(), 1);
    assert!(flush(&mut state, at_secs(epoch, 8), &config).is_empty());
    Ok(())
}

#[test]
fn wake_budget_pauses_on_fifth_monitor_only_wake() -> Result<(), Box<dyn Error>> {
    let (live, job) = FakeJobs::live("j1");
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let epoch = Timestamp::UNIX_EPOCH;
    let request = watch_request("j1", "ok")?;
    watch(&mut state, &request, &live, epoch, &config)?;
    let monitor = MonitorId(1);
    for wake in 1..=5u64 {
        on_output(&mut state, job, "ok", at_secs(epoch, wake), &config);
        let effects = flush(&mut state, at_secs(epoch, 7 * wake - 3), &config);
        assert!(!effects.is_empty());
        let pause = update_monitor_only_wake(&mut state, &[monitor], true, &config);
        if wake < 5 {
            assert!(pause.is_empty());
        } else {
            assert_eq!(pause.len(), 1);
            assert_eq!(pause[0], MonitorEffect::Notice(PAUSE_NOTICE.into()));
        }
    }
    assert!(
        state
            .monitors
            .get(&monitor)
            .is_some_and(|watch| watch.paused)
    );
    // A mixed wake resets the streak.
    update_monitor_only_wake(&mut state, &[], false, &config);
    assert_eq!(state.monitor_only_wakes, 0);
    Ok(())
}

#[test]
fn status_renders_waiting_parts_and_exact_json() {
    let counts = inflight_counts(2, 1, 0, false, false);
    let payload = status_payload(ControllerMode::Run, false, counts, 0, None);
    assert_eq!(
        status_json(&payload),
        "{\"mode\":\"run\",\"paused_reason\":null,\"quiet\":false,\"inflight\":{\"jobs\":2,\"monitors\":1,\"asks\":0,\"goal_timer\":0,\"loop_guard\":0},\"silent_jobs\":0,\"goal\":null}"
    );
}

#[test]
fn abort_and_subagent_replies_are_exact() {
    assert_eq!(
        abort_reply(true, 5, 2),
        "aborted: turn cancelled, 5 jobs cancelled, 2 monitors stopped. Automatic turns are paused; your next message resumes them."
    );
    assert_eq!(
        abort_reply(false, 0, 0),
        "aborted: turn idle, 0 jobs cancelled, 0 monitors stopped. Automatic turns are paused; your next message resumes them."
    );
    assert_eq!(
        subagent_reply("/abort"),
        "/abort: not available in a subagent."
    );
}

#[test]
fn monitor_ids_round_trip_and_reject_other_shapes() {
    assert_eq!(MonitorId::parse("m1"), Some(MonitorId(1)));
    assert_eq!(MonitorId(12).render(), "m12");
    assert_eq!(MonitorId::parse("j1"), None);
    assert_eq!(MonitorId::parse("m"), None);
    assert_eq!(MonitorId::parse("m1x"), None);
    assert_eq!(MonitorId::parse(""), None);
}

#[test]
fn job_end_drops_watches_and_stop_all_counts_newly_stopped() -> Result<(), Box<dyn Error>> {
    let (live, job) = FakeJobs::live("j1");
    let mut state = MonitorState::default();
    let config = MonitorConfig::default();
    let now = Timestamp::UNIX_EPOCH;
    for _ in 0..2 {
        watch(&mut state, &watch_request("j1", "ok")?, &live, now, &config)?;
    }
    assert_eq!(stop_all(&mut state), 2);
    assert_eq!(stop_all(&mut state), 0);
    on_output(&mut state, job, "ok", now, &config);
    on_job_end(&mut state, job);
    assert!(state.monitors.is_empty());
    assert!(state.output.is_empty());
    Ok(())
}

#[test]
fn monitor_config_defaults_match_product_values() -> Result<(), Box<dyn Error>> {
    assert_eq!(parse_config(None)?, MonitorConfig::default());
    Ok(())
}

#[test]
fn monitor_config_rejects_values_outside_supported_ranges() {
    let section = toml::Value::Table(
        [("coalesce_ms".to_owned(), toml::Value::Integer(0))]
            .into_iter()
            .collect(),
    );
    let error = parse_config(Some(&section)).unwrap_err();
    assert!(matches!(
        error,
        super::state::MonitorConfigError::NotInteger { key, low: 1, high: 60_000 }
            if key.as_ref() == "coalesce_ms"
    ));
}

#[test]
fn monitor_config_rejects_unknown_keys() {
    let section = toml::Value::Table(
        [("zzz".to_owned(), toml::Value::Integer(1))]
            .into_iter()
            .collect(),
    );
    assert!(matches!(
        parse_config(Some(&section)),
        Err(super::state::MonitorConfigError::UnknownKey { key }) if key.as_ref() == "zzz"
    ));
}

#[test]
fn monitor_config_rejects_values_with_wrong_types() {
    let section = toml::Value::Table(
        [("max_lines".to_owned(), toml::Value::String("x".into()))]
            .into_iter()
            .collect(),
    );
    assert!(matches!(
        parse_config(Some(&section)),
        Err(super::state::MonitorConfigError::NotInteger { key, low: 1, high: 200 })
            if key.as_ref() == "max_lines"
    ));
}
