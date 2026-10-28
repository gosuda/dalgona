// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion scale gate (`two-hundred-exec-jobs`): 200 real
//! background exec jobs and 50 output monitors over the real host, with 100
//! `sleep 30` commands rewritten by the sleep classifier. Set
//! `DALGONA_ORCH_STRESS=1` to run it.
//!
//! One session admits at most 16 live watches, and `monitor` is forbidden to
//! subagents, so the product cap stays untouched and the 50 watches are
//! spread over `SESSIONS` top-level sessions of one host: two per session.
//! Each session starts eight jobs in one turn, one tool round per call, as a
//! person's model would:
//!
//! - four held jobs, `echo hold S-K; exec sleep N`, with an explicit
//!   `foreground_s` of one second. They are not sleep waits, so the
//!   classifier leaves them alone, and they outlive the whole run;
//! - four sleep waits, `trap '...' EXIT; echo end S-J; sleep 30`. The
//!   classifier rewrites each one's foreground window to five seconds, so the
//!   call detaches at five seconds instead of parking the turn for thirty.
//!
//! Two watches then bind in one model response: one on the last sleep wait
//! (filter `^end`) and one on the last held job (filter `^hold`). That sleep
//! wait ends by itself thirty seconds after it started; its trap prints a
//! `finished` line, which reaches the model as the delivered job report. The
//! wake turn that delivers it rearms both watches. The watch on the ended job
//! must answer `no monitor`: it ended with its job. The watch on the held
//! job, which still runs, must rearm. Closing every session then cancels the
//! held jobs and their watches, and shutdown must leave no task behind.
//!
//! The job ids exist only after each exec call detaches, so a static JSONL
//! script cannot name them ahead of time. A registered synthetic model (the
//! public `ModelHandler` extension API, as gate 09 uses it) answers each
//! request from its own context: it reads the `detached as job <uuid>`
//! results already in that context and names those ids in the watch calls.
//! The shared FIFO replay fixture cannot give that, because one queue serves
//! every session in script order.
//!
//! Not reachable from this target, and not claimed here: the TUI keypress
//! echo P99 needs the PTY harness of other gates.
//!
//! Four scenarios share this target, each in its own test function and each
//! run by the CI workflow as its own filtered process (one scenario per
//! process keeps the RSS/fd peaks attributable):
//!
//! - `exec_jobs_and_monitors_end_clean_within_the_process_contract`: the
//!   authored 200-job/50-monitor scale above, plus the nonvacuous census:
//!   every held job self-reports its shell PID, sibling sleep PID, and
//!   process group into `.pids/`, the gate proves all 100 held trees alive
//!   before the close, then proves the whole table gone within 3 s of it.
//! - `fake_clock_sleep_detach_one_hundred_jobs`: 100 rewritten sleep waits
//!   over the real `exec` boundary on a paused clock. Startup handshakes
//!   (real PID files) land before any advance, so fake time never
//!   manufactures process startup; no detach may settle before 5 s of
//!   virtual time, and all 100 detach at the 5 s budget edge. The lower
//!   bound holds on every lane; teardown runs on the resumed clock.
//! - `subagent_exec_cancel_kills_every_process_tree`: 20 subagents in `yolo`
//!   mode each start 10 background exec jobs; the run is cancelled 2 s after
//!   the last spawn. All 200 exec jobs end `cancelled` in the subagent
//!   journals, the 20 subagent jobs and the run end `cancelled` in the
//!   parent journal, the run notice is delivered once, and every recorded
//!   process tree is dead within 3 s of the cancel.
//! - `parent_fast_jobs_complete_and_deliver_once`: 200 fast background jobs
//!   (`sleep 1; sleep 0.5; echo ok`, which detaches at the 1 s foreground
//!   floor and exits half a second later) across 25 parent sessions. All 200
//!   end `done` in the durable ledger and each job id reaches exactly one
//!   wake report; peak overlap is derived and logged, never asserted at 200.
//!
//! Two more tests share this target and are not stress-gated: they run in the
//! functional lane and separate the two public causes of a
//! `Delivery::Resync`, each over a real host and a real session with a
//! synthetic `ModelHandler` that streams a controllable number of `TextDelta`
//! events:
//!
//! - `stale_cursor_after_count_eviction_resyncs_first_then_recovers_contiguously`:
//!   retained-window eviction. A live observer that the test drains in
//!   lockstep with the model never lags, so only the replay window can be
//!   exceeded. A cursor read from the view before a turn that published
//!   exactly the window's worth of updates still resumes with every update;
//!   one more update makes the same kind of cursor stale, and the
//!   subscription must then open with exactly one `Resync` at the head and
//!   nothing else before live delivery.
//! - `queue_overflow_with_retained_ring_resyncs_once_then_ends_and_resumes_from_ring`:
//!   live queue overflow. A subscription that is held undrained across a turn
//!   that publishes up to, then past, the queue bound must deliver
//!   everything through the bound, then exactly one `Resync` and an orderly
//!   end, while a new subscription at the same cursor still replays every
//!   update from the ring.
#![expect(
    clippy::disallowed_methods,
    reason = "gate reads its opt-in flags and scratch dirs from the environment"
)]

use dalgona_gates::support;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_core::{
    Caps, ContextItem, ModelId, ModelRequest, Part, RawJson, ServiceSet, ThinkingLevel, Usage,
};
use dal_provider::{ProviderError, StopReason, StreamEvent, ToolArgs, ToolCall};

use futures::StreamExt as _;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use sonic_rs::JsonValueTrait as _;

const SESSIONS: usize = 25;
const HELD_PER_SESSION: usize = 4;
const REWRITTEN_PER_SESSION: usize = 4;
const JOBS_PER_SESSION: usize = HELD_PER_SESSION + REWRITTEN_PER_SESSION;
const JOBS: usize = SESSIONS * JOBS_PER_SESSION;
const REWRITTEN: usize = SESSIONS * REWRITTEN_PER_SESSION;
const MONITORS_PER_SESSION: usize = 2;
const MONITORS: usize = SESSIONS * MONITORS_PER_SESSION;
/// The product's per-session live-watch cap, never raised here.
const PRODUCT_MONITOR_CAP: usize = 16;
const _: () = assert!(MONITORS_PER_SESSION <= PRODUCT_MONITOR_CAP);
// The authored scale: 200 jobs, 50 monitors, 100 rewritten sleeps.
const _: () = assert!(JOBS == 200 && MONITORS == 50 && REWRITTEN == 100);

/// The background sibling every census-tracked command parks: it shares the
/// shell's process group, so the group census sees the whole tree, not just
/// the leader.
const SIBLING_SLEEP_S: u64 = 700;
/// How long after the last process-tree spawn the cancel prompt goes out.
const CANCEL_DELAY: Duration = Duration::from_secs(2);
/// How fast every recorded tree must vanish after a cancel.
const GONE_WITHIN: Duration = Duration::from_secs(3);
/// The fake-clock foreground window the classifier rewrites sleeps to.
const FAKE_JOBS: usize = 100;
/// Virtual time that must pass with no detach before the budget edge.
const FAKE_WINDOW_MS: u64 = 5_000;
/// Extra 1 ms virtual steps tolerated for timer-wheel tick rounding.
const FAKE_TICK_SLACK_STEPS: u32 = 2;
/// The V10 subagent cancel shape: children with background exec each.
const CANCEL_CHILDREN: usize = 20;
/// Background exec jobs per cancel-scenario child.
const CANCEL_JOBS_PER_CHILD: usize = 10;
/// Cancellation trees expected: children times jobs-plus-blocker.
const CANCEL_TREES: usize = CANCEL_CHILDREN * (CANCEL_JOBS_PER_CHILD + 1);
/// `sleep 30` execs per cancel child; the 11th call parks the turn.
const CANCEL_SLEEP_S: u64 = 30;
/// The parked foreground exec of a cancel child: killed by the cancel.
const CANCEL_PARK_S: u64 = 700;
/// The parent fast-batch shape: sessions times jobs per session.
const FAST_SESSIONS: usize = 25;
/// Fast jobs per parent session; every one detaches, then exits.
const FAST_PER_SESSION: usize = 8;
/// Fast jobs in total across the parent batch.
const FAST_JOBS: usize = FAST_SESSIONS * FAST_PER_SESSION;
const _: () = assert!(FAST_JOBS == 200);
/// The fast command sleeps past the 1 s foreground floor, then exits half a
/// second later as a true background completion.
const FAST_SLEEP_S: u64 = 1;
const FAST_TAIL_S: &str = "0.5";

/// The start prompt of the fake-clock sessions.
const FAKE_TAG: &str = "start fake sleeps for stress session ";
/// The start prompt of the cancel-scenario parent: it carries no cancel
/// instruction, so the run starts first and the cancel arrives later.
const CANCEL_TAG: &str = "run the execjobs pool";
/// The second prompt of the cancel-scenario parent, submitted 2 s after the
/// last process-tree spawn: only this prompt may trigger the cancel.
const CANCEL_NOW_TAG: &str = "cancel the execjobs run now";
/// The pool item tag of one cancel-scenario child.
const CHILD_TAG: &str = "cancel-child-";
/// The start prompt of a fast-batch session.
const FAST_TAG: &str = "start fast jobs for stress session ";

/// The `sleep` duration the classifier rewrites.
const REWRITTEN_SLEEP_S: u64 = 30;
/// The foreground window the classifier rewrites a sleep wait to.
const DETACH_WINDOW_MS: u64 = 5_000;
/// The explicit foreground window of a held job.
const HELD_WINDOW_MS: u64 = 1_000;
/// A detach that took as long as the sleep itself was not rewritten.
const NOT_WAITED_OUT_MS: u64 = REWRITTEN_SLEEP_S * 1_000;
/// Measurement jitter allowed above a window on the idle-machine lane: process
/// spawn and journal write, not product budget.
const TIMING_JITTER_MS: u64 = 1_000;
/// The base of the held jobs' distinct `sleep` seconds.
const HELD_SLEEP_BASE_S: usize = 600;
/// Hang guard of one session's whole script: not an acceptance budget.
const HANG_GUARD: Duration = Duration::from_secs(300);
const RSS_LIMIT_MIB: u64 = 256;
const FD_LIMIT: usize = 2_048;
/// How often the peak sampler reads the process counters: every second,
/// except on Windows where each reading spawns a PowerShell process.
#[cfg(windows)]
const SAMPLE_PERIOD: Duration = Duration::from_secs(5);
#[cfg(not(windows))]
const SAMPLE_PERIOD: Duration = Duration::from_secs(1);

const JOBS_MODEL: &str = "stress-gate/jobs";
const GATE_CLIENT: &str = "exec-stress-gate";
const START_TAG: &str = "start jobs for stress session ";
const DETACH_MARKER: &str = "detached as job ";

/// The machine the run is on. Wall-clock bounds hold only on the nightly
/// idle-machine lane; shared CI runners cannot hold them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    /// A shared runner: counts and resource caps only.
    Shared,
    /// The idle-machine lane, `DAL_TIMING_BUDGETS`: wall-clock bounds too.
    Idle,
}

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One message of the context flattened to its text parts; empty for an
/// assistant message.
fn item_text(item: &ContextItem) -> String {
    let (ContextItem::User { parts } | ContextItem::ToolResult { parts, .. }) = item else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(&**text),
            _ => None,
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

/// The texts of every result of `tool` in the context, in call order.
fn tool_results(context: &[ContextItem], tool: &str) -> Vec<String> {
    context
        .iter()
        .filter(|item| matches!(item, ContextItem::ToolResult { name, .. } if &**name == tool))
        .map(item_text)
        .collect()
}

/// The session number the start prompt names.
fn session_index(context: &[ContextItem]) -> Option<usize> {
    context.iter().find_map(|item| {
        let ContextItem::User { .. } = item else {
            return None;
        };
        let text = item_text(item);
        let rest = &text[text.find(START_TAG)? + START_TAG.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

/// The job id a detached exec result names.
fn detached_id(text: &str) -> Option<String> {
    let at = text.find(DETACH_MARKER)? + DETACH_MARKER.len();
    let id: String = text[at..]
        .chars()
        .take_while(|character| character.is_ascii_hexdigit() || *character == '-')
        .collect();
    (!id.is_empty()).then_some(id)
}

/// The monitor id a `watching job ... as m1: ...` result names.
fn monitor_id(text: &str) -> Option<String> {
    let at = text.find(" as ")? + " as ".len();
    let id: String = text[at..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (!id.is_empty()).then_some(id)
}

/// Whether the delivered job report of this session's last sleep wait is in
/// the context: a user message, never a monitor batch (the watch filter skips
/// the trap's `finished` line).
fn job_finished(context: &[ContextItem], session: usize) -> bool {
    let marker = format!("finished {session}-{}", REWRITTEN_PER_SESSION - 1);
    context.iter().any(|item| {
        matches!(item, ContextItem::User { .. }) && item_text(item).contains(marker.as_str())
    })
}

/// One tool call of a scripted response.
struct Call {
    id: String,
    tool: &'static str,
    args: String,
}

/// One tool-call response: the model calls every tool in `calls` in one batch
/// and stops for their results.
///
/// # Errors
/// Returns the parse error when an argument document is not one JSON value.
fn tool_calls_stream(calls: &[Call]) -> Result<EventStream, dal_core::RawJsonError> {
    let mut events = Vec::with_capacity(calls.len() + 3);
    let mut done = Vec::with_capacity(calls.len());
    for call in calls {
        events.push(Ok(StreamEvent::ToolCallStarted {
            id: call.id.clone(),
            name: call.tool.to_owned(),
        }));
        done.push(ToolCall {
            id: call.id.clone(),
            name: call.tool.to_owned(),
            args: ToolArgs::Parsed(RawJson::parse(&call.args)?),
        });
    }
    events.push(Ok(StreamEvent::ToolCallsDone { calls: done }));
    events.push(Ok(StreamEvent::Usage { usage: usage() }));
    events.push(Ok(StreamEvent::Stop {
        reason: StopReason::ToolUse,
    }));
    Ok(stream_of(events))
}

/// Wraps one response's events into an `EventStream`.
fn stream_of(events: Vec<Result<StreamEvent, ProviderError>>) -> EventStream {
    EventStream::new(futures::stream::iter(events), || {})
}

/// One text-only scripted-model response that ends the turn.
fn text_stream(text: &str) -> EventStream {
    stream_of(vec![
        Ok(StreamEvent::TextDelta {
            text: text.to_owned(),
        }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ])
}

fn usage() -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: Some(0.001),
        premium_requests: None,
    }
}

/// The relative report path of one census-tracked command: the exec cwd is
/// the workspace root, so a short relative path keeps commands small.
fn report_rel(label: &str) -> PathBuf {
    Path::new(support::STRESS_PIDS_DIR).join(format!("{label}.pids"))
}

/// One unique token per exec call: consecutive same-tool calls must never
/// share argument bytes, or the loop guard's identical-call detector arms.
fn unique_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::now_v7().simple(),
        uuid::Uuid::now_v7().simple()
    )
}

/// The exec call number `k` of session `session`: four held jobs, then four
/// sleep waits. Every command text differs, so the loop guard's
/// identical-call detector never arms. Held jobs self-report their process
/// tree into the workspace `.pids/` directory before running, so the cancel
/// census is nonvacuous.
fn exec_call(session: usize, k: usize) -> Result<EventStream, dal_core::RawJsonError> {
    let args = if k < HELD_PER_SESSION {
        let seconds = HELD_SLEEP_BASE_S + session * JOBS_PER_SESSION + k;
        let label = format!("held-{session:02}-{k}");
        let prefix = support::tree_report_command(
            &report_rel(&label),
            SIBLING_SLEEP_S,
            &format!("echo hold {session}-{k} tok-{}", unique_token()),
        );
        let command = format!("{prefix}; exec sleep {seconds}");
        format!(
            r#"{{"command":{},"foreground_s":1}}"#,
            support::json_text(&command)
        )
    } else {
        let wait = k - HELD_PER_SESSION;
        format!(
            r#"{{"command":"trap 'echo finished {session}-{wait}' EXIT; echo end {session}-{wait}; sleep {REWRITTEN_SLEEP_S}"}}"#
        )
    };
    tool_calls_stream(&[Call {
        id: format!("e{k}"),
        tool: "exec",
        args,
    }])
}

/// The two watches of one response, on the last sleep wait and the last held
/// job, named by the ids their detach results already carry.
fn watch_calls(session: usize, execs: &[String]) -> Result<EventStream, dal_core::RawJsonError> {
    let ids: Vec<Option<String>> = execs.iter().map(|text| detached_id(text)).collect();
    let held = ids.get(HELD_PER_SESSION - 1).and_then(Option::as_ref);
    let waited = ids.get(JOBS_PER_SESSION - 1).and_then(Option::as_ref);
    let (Some(held), Some(waited)) = (held, waited) else {
        return Ok(text_stream("an exec call did not detach"));
    };
    tool_calls_stream(&[
        Call {
            id: "w-e".to_owned(),
            tool: "monitor",
            args: format!(
                r#"{{"action":"watch","job":"{waited}","filter":"^end","description":"watch end {session}"}}"#
            ),
        },
        Call {
            id: "w-c".to_owned(),
            tool: "monitor",
            args: format!(
                r#"{{"action":"watch","job":"{held}","filter":"^hold","description":"watch hold {session}"}}"#
            ),
        },
    ])
}

/// The rearm of both watches, by the ids their watch results carry.
fn rearm_calls(monitors: &[String]) -> Result<EventStream, dal_core::RawJsonError> {
    let find = |description: &str| {
        monitors
            .iter()
            .find(|text| text.contains(description))
            .and_then(|text| monitor_id(text))
    };
    let (Some(ended), Some(held)) = (find("watch end"), find("watch hold")) else {
        return Ok(text_stream("a watch did not start"));
    };
    tool_calls_stream(&[
        Call {
            id: "r-e".to_owned(),
            tool: "monitor",
            args: format!(r#"{{"action":"rearm","id":"{ended}"}}"#),
        },
        Call {
            id: "r-c".to_owned(),
            tool: "monitor",
            args: format!(r#"{{"action":"rearm","id":"{held}"}}"#),
        },
    ])
}

/// The first user message: the prompt text pins the scenario. Later wake
/// notices carry run and task labels that must never re-route the script,
/// so only this item is ever tested.
fn first_user(context: &[ContextItem]) -> Option<&ContextItem> {
    context
        .iter()
        .find(|item| matches!(item, ContextItem::User { .. }))
}

/// Whether the first user message carries `tag`.
fn first_user_has(context: &[ContextItem], tag: &str) -> bool {
    first_user(context).is_some_and(|item| item_text(item).contains(tag))
}

/// The number after `tag` in the first user message.
fn tag_number(context: &[ContextItem], tag: &str) -> Option<usize> {
    let item = first_user(context)?;
    let text = item_text(item);
    let rest = &text[text.find(tag)? + tag.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The cancel-scenario child index of this request, from its task prompt.
fn child_index(context: &[ContextItem]) -> Option<usize> {
    tag_number(context, CHILD_TAG)
}

/// The next response of one session, from its context alone: the start
/// prompt begins the exec calls, each exec result the next call, the eighth
/// the watches, and the wake turn that delivers the ended job's report the
/// rearms. Every other request ends its turn. Child, fake-clock, fast, and
/// cancel-parent prompts route to their own scripts by prompt tag.
fn next_response(context: &[ContextItem]) -> Result<EventStream, dal_core::RawJsonError> {
    if child_index(context).is_some() {
        return cancel_child_response(context);
    }
    if first_user_has(context, FAKE_TAG) {
        return paused_response(context);
    }
    if first_user_has(context, FAST_TAG) {
        return fast_response(context);
    }
    if first_user_has(context, CANCEL_TAG) {
        return cancel_parent_response(context);
    }
    let Some(session) = session_index(context) else {
        return Ok(text_stream("noted"));
    };
    match context.last() {
        Some(ContextItem::ToolResult { name, .. }) => {
            let tool: &str = name;
            if tool != "exec" {
                return Ok(text_stream("noted"));
            }
            let execs = tool_results(context, "exec");
            if execs.len() < JOBS_PER_SESSION {
                exec_call(session, execs.len())
            } else {
                watch_calls(session, &execs)
            }
        }
        Some(item @ ContextItem::User { .. }) => {
            let monitors = tool_results(context, "monitor");
            if item_text(item).contains(START_TAG) {
                exec_call(session, 0)
            } else if monitors.len() == MONITORS_PER_SESSION && job_finished(context, session) {
                rearm_calls(&monitors)
            } else {
                Ok(text_stream("noted"))
            }
        }
        _ => Ok(text_stream("noted")),
    }
}

/// The one synthetic model the gate registers. Report paths are
/// workspace-relative (`.pids/`), so the model carries no directories.
struct JobsModel;

impl ModelHandler for JobsModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            next_response(request.context.as_ref()).map_err(|_| ModelError::SecondForward)
        })
    }
}

/// The synthetic-model extension the gate adds to the assembled product.
fn jobs_extension() -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    let record = ModelRecord {
        id: ModelId::parse(JOBS_MODEL)?,
        caps: Caps {
            context_window: Some(200_000),
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
        handler: Arc::new(JobsModel),
        export: None,
    };
    ExtensionBuilder::new("stress-jobs", "0.1.0", ServiceSet::default())?
        .model(record)
        .build()
}

/// One fake-clock exec: a classified trailing `sleep 30` (rewritten to the
/// 5 s foreground window) that self-reports its tree first. Called once per
/// session; the detached result needs no follow-up.
fn paused_response(context: &[ContextItem]) -> Result<EventStream, dal_core::RawJsonError> {
    if !tool_results(context, "exec").is_empty() {
        return Ok(text_stream("noted"));
    }
    let index = tag_number(context, FAKE_TAG).unwrap_or(0);
    let label = format!("fake-{index:03}");
    let prefix = support::tree_report_command(
        &report_rel(&label),
        SIBLING_SLEEP_S,
        &format!("trap 'echo finished {index}' EXIT; echo end {index}"),
    );
    let command = format!("{prefix}; sleep {REWRITTEN_SLEEP_S}");
    tool_calls_stream(&[Call {
        id: format!("f{index}"),
        tool: "exec",
        args: format!(r#"{{"command":{}}}"#, support::json_text(&command)),
    }])
}

/// The cancel-scenario parent: one 20-child pool run, then (on the cancel
/// prompt) one `agents cancel` naming the run id the run result carried.
/// The count of `agents` results tells the run call from the cancel call.
fn cancel_parent_response(context: &[ContextItem]) -> Result<EventStream, dal_core::RawJsonError> {
    let results = tool_results(context, "agents");
    if results.is_empty() {
        let items = (0..CANCEL_CHILDREN)
            .map(|child| format!(r#""{CHILD_TAG}{child:02}""#))
            .collect::<Vec<_>>()
            .join(",");
        return tool_calls_stream(&[Call {
            id: "pool".to_owned(),
            tool: "agents",
            args: format!(
                r#"{{"action":"run","name":"execjobs","steps":[{{"name":"jobs","items":[{items}],"workers":{CANCEL_CHILDREN},"tools":["exec"],"isolation":"shared","prompt":"start ten background exec jobs of sleep 30 for {{{{item}}}}, then park"}}]}}"#
            ),
        }]);
    }
    let cancel_now = context.iter().rev().find_map(|item| {
        let ContextItem::User { .. } = item else {
            return None;
        };
        Some(item_text(item).contains(CANCEL_NOW_TAG))
    });
    if results.len() == 1 && cancel_now == Some(true) {
        let Some(run) = results[0].split("started run ").nth(1).and_then(|rest| {
            // Run ids are hyphenated UUIDs: stop at the first quote, not
            // the first hyphen, or the cancel names a run that never
            // existed.
            let id: String = rest
                .chars()
                .take_while(|character| character.is_ascii_hexdigit() || *character == '-')
                .collect();
            (!id.is_empty()).then_some(id)
        }) else {
            // No run id to cancel: say so in the turn. The ledger and
            // notice checks below then fail loudly on the missing cancel.
            return Ok(text_stream("the pool run named no id"));
        };
        return tool_calls_stream(&[Call {
            id: "cancel".to_owned(),
            tool: "agents",
            args: format!(
                r#"{{"action":"cancel","ids":[{run}]}}"#,
                run = support::json_text(&run)
            ),
        }]);
    }
    Ok(text_stream("noted"))
}

/// One cancel-scenario child call: ten background `exec sleep 30` jobs with
/// per-call tokens and tree reports, then one parked foreground exec that
/// the cancel kills. Count-based, so guard steers never re-route it.
fn cancel_child_response(context: &[ContextItem]) -> Result<EventStream, dal_core::RawJsonError> {
    let Some(child) = child_index(context) else {
        return Ok(text_stream("noted"));
    };
    let execs = tool_results(context, "exec");
    if execs.len() < CANCEL_JOBS_PER_CHILD {
        let k = execs.len();
        let label = format!("cancel-{child:02}-{k:02}");
        // Alternate two shapes so adjacent same-tool calls stay dissimilar.
        let tail = if k.is_multiple_of(2) {
            format!(
                "echo bg {child}-{k} tok-{}; exec sleep {CANCEL_SLEEP_S}",
                unique_token()
            )
        } else {
            format!("exec sleep {CANCEL_SLEEP_S}")
        };
        let prefix =
            support::tree_report_command(&report_rel(&label), SIBLING_SLEEP_S, "echo ready");
        let command = format!("{prefix}; {tail}");
        return tool_calls_stream(&[Call {
            id: format!("c{k}"),
            tool: "exec",
            args: format!(
                r#"{{"command":{},"foreground_s":1}}"#,
                support::json_text(&command)
            ),
        }]);
    }
    if execs.len() == CANCEL_JOBS_PER_CHILD {
        let label = format!("cancel-{child:02}-fg");
        let prefix =
            support::tree_report_command(&report_rel(&label), SIBLING_SLEEP_S, "echo park");
        let command = format!("{prefix}; exec sleep {CANCEL_PARK_S}");
        return tool_calls_stream(&[Call {
            id: "park".to_owned(),
            tool: "exec",
            args: format!(
                r#"{{"command":{},"foreground_s":{CANCEL_PARK_S}}}"#,
                support::json_text(&command)
            ),
        }]);
    }
    Ok(text_stream("parked"))
}

/// One fast-batch call: `sleep 1` past the foreground floor, then the
/// normative half-second tail. Count-based across 200 rounds; wake notices
/// landing mid-turn never re-route it.
///
/// Every command ends with a standalone `echo ok`, so the success line the
/// ledger check requires is a whole output line that the unique token (whose
/// `tok-` prefix itself spells `ok`) can never supply. The two shapes still
/// differ, so adjacent calls stay dissimilar for the loop guard.
fn fast_response(context: &[ContextItem]) -> Result<EventStream, dal_core::RawJsonError> {
    let done = tool_results(context, "exec").len();
    if done >= FAST_PER_SESSION {
        return Ok(text_stream("noted"));
    }
    let command = if done.is_multiple_of(2) {
        format!(
            "sleep {FAST_SLEEP_S}; sleep {FAST_TAIL_S}; echo tok-{}; echo ok",
            unique_token()
        )
    } else {
        format!(
            "echo go tok-{}; sleep {FAST_SLEEP_S}; sleep {FAST_TAIL_S}; echo ok",
            unique_token()
        )
    };
    tool_calls_stream(&[Call {
        id: format!("q{done}"),
        tool: "exec",
        args: format!(
            r#"{{"command":{},"foreground_s":1}}"#,
            support::json_text(&command)
        ),
    }])
}

/// One tool result of a session, with the moment this gate received it.
struct Settled {
    call: String,
    is_error: bool,
    text: String,
    elapsed_ms: Option<u64>,
    seen: Instant,
}

/// Everything one session's update stream showed.
struct SessionRecord {
    index: usize,
    settled: Vec<Settled>,
    failed_turns: Vec<String>,
}

impl SessionRecord {
    fn new(index: usize) -> Self {
        Self {
            index,
            settled: Vec::new(),
            failed_turns: Vec::new(),
        }
    }

    fn observe(&mut self, kind: &dal_core::UpdateKind) {
        match kind {
            dal_core::UpdateKind::ToolSettled { call, outcome } => self.settled.push(Settled {
                call: call.as_str().to_owned(),
                is_error: outcome.is_error,
                text: outcome.text.to_string(),
                elapsed_ms: outcome.elapsed_ms,
                seen: Instant::now(),
            }),
            dal_core::UpdateKind::TurnEnded { stop, .. } => {
                let stop = format!("{stop:?}");
                if stop != "EndTurn" {
                    self.failed_turns.push(stop);
                }
            }
            _ => {}
        }
    }

    /// How many of this session's watches the wake turn rearmed.
    fn rearmed(&self) -> usize {
        self.settled
            .iter()
            .filter(|settled| settled.call.starts_with("r-"))
            .count()
    }

    fn settled_call(&self, call: &str) -> support::TestResult<&Settled> {
        self.settled
            .iter()
            .find(|settled| settled.call == call)
            .ok_or_else(|| format!("session {}: no result for call {call}", self.index).into())
    }
}

/// Runs one session's script to its end: the start prompt, then every update
/// until both watches are rearmed. A listen-only subscription never counts as
/// an answerer, so an unexpected question is denied at once instead of
/// waiting for this gate.
async fn drive(index: usize, agent: &dal_agent::Agent) -> support::TestResult<SessionRecord> {
    let mut updates = agent.subscribe_listen(None)?;
    let reply = agent
        .submit(dal_core::Command::Prompt {
            expect: dal_core::Expect::Idle,
            content: vec![Part::Text {
                text: format!("{START_TAG}{index}").into(),
            }],
        })
        .await?;
    if !matches!(reply, dal_core::Reply::Accepted { .. }) {
        return Err(
            format!("session {index}: the start prompt was not accepted: {reply:?}").into(),
        );
    }
    let mut record = SessionRecord::new(index);
    let waited = tokio::time::timeout(HANG_GUARD, async {
        while record.rearmed() < MONITORS_PER_SESSION {
            let delivery = updates.next().await.ok_or("the update stream closed")?;
            match delivery {
                dal_agent::Delivery::Update(update) => record.observe(&update.kind),
                dal_agent::Delivery::Resync { .. } => {
                    return Err("the update stream lost its place".into());
                }
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    let calls: Vec<&str> = record
        .settled
        .iter()
        .map(|settled| settled.call.as_str())
        .collect();
    waited.map_err(|_| {
        format!("session {index}: the watches were not rearmed in time; results seen: {calls:?}")
    })??;
    Ok(record)
}

/// The jobs and spawn moments of every session checked so far.
#[derive(Default)]
struct Ledger {
    jobs: BTreeSet<String>,
    /// When each exec call started, and whether it was a sleep wait.
    spawns: Vec<(Instant, bool)>,
}

/// One session's eight exec results: each detached under its own foreground
/// window, naming a job of its own.
fn check_execs(record: &SessionRecord, lane: Lane, ledger: &mut Ledger) -> support::TestResult<()> {
    let index = record.index;
    for k in 0..JOBS_PER_SESSION {
        let settled = record.settled_call(&format!("e{k}"))?;
        assert!(
            !settled.is_error,
            "session {index}: exec e{k} failed: {}",
            settled.text
        );
        let id = detached_id(&settled.text).ok_or_else(|| {
            format!(
                "session {index}: exec e{k} did not detach: {}",
                settled.text
            )
        })?;
        ledger.jobs.insert(id);
        let rewritten = k >= HELD_PER_SESSION;
        let window = if rewritten {
            DETACH_WINDOW_MS
        } else {
            HELD_WINDOW_MS
        };
        let elapsed = settled
            .elapsed_ms
            .ok_or_else(|| format!("session {index}: exec e{k} reported no run time"))?;
        assert!(
            elapsed >= window,
            "session {index}: exec e{k} detached after {elapsed} ms, before its {window} ms window"
        );
        assert!(
            elapsed < NOT_WAITED_OUT_MS,
            "session {index}: exec e{k} took {elapsed} ms; its sleep was not rewritten"
        );
        if lane == Lane::Idle {
            assert!(
                elapsed <= window + TIMING_JITTER_MS,
                "session {index}: exec e{k} detached after {elapsed} ms; its window is {window} ms"
            );
        }
        let spawned = settled
            .seen
            .checked_sub(Duration::from_millis(elapsed))
            .ok_or_else(|| format!("session {index}: exec e{k} ran longer than the gate did"))?;
        ledger.spawns.push((spawned, rewritten));
    }
    Ok(())
}

/// One session's watches: both bound, the one on the ended job gone, the one
/// on the held job rearmed.
fn check_monitors(record: &SessionRecord) -> support::TestResult<()> {
    let index = record.index;
    for call in ["w-e", "w-c"] {
        let watch = record.settled_call(call)?;
        assert!(
            !watch.is_error && watch.text.starts_with("watching job "),
            "session {index}: {call} did not start a watch: {}",
            watch.text
        );
    }
    let ended = record.settled_call("r-e")?;
    // Either text proves the watch ended with its job: the table drops the
    // watch when the job's end is polled (`no monitor`), but the drop can
    // lag the delivered report, in which case the rearm names the ended job.
    let ended_gone = ended.is_error
        && (ended.text.contains("no monitor") || ended.text.contains("is not a running exec job"));
    assert!(
        ended_gone,
        "session {index}: the watch on the ended job was still live: {}",
        ended.text
    );
    let live = record.settled_call("r-c")?;
    assert!(
        !live.is_error && live.text.starts_with("rearmed "),
        "session {index}: the watch on the held job did not rearm: {}",
        live.text
    );
    Ok(())
}

/// Checks every session and the run as a whole: 200 distinct jobs, all alive
/// at the moment the last one started.
fn check_sessions(records: &[SessionRecord], lane: Lane) -> support::TestResult<()> {
    let mut ledger = Ledger::default();
    for record in records {
        assert!(
            record.failed_turns.is_empty(),
            "session {}: turns ended abnormally: {:?}",
            record.index,
            record.failed_turns
        );
        check_execs(record, lane, &mut ledger)?;
        check_monitors(record)?;
    }
    assert_eq!(
        ledger.jobs.len(),
        JOBS,
        "every exec call must name its own job"
    );
    let last_spawn = ledger
        .spawns
        .iter()
        .map(|spawn| spawn.0)
        .max()
        .ok_or("no exec call was seen")?;
    let first_wait = ledger
        .spawns
        .iter()
        .filter(|spawn| spawn.1)
        .map(|spawn| spawn.0)
        .min()
        .ok_or("no sleep wait was seen")?;
    // A sleep wait lives thirty seconds from its spawn and a held job far
    // longer, so every job was alive when the last one started exactly when
    // the first sleep wait had not yet run out.
    let span = last_spawn.saturating_duration_since(first_wait);
    assert!(
        span < Duration::from_secs(REWRITTEN_SLEEP_S),
        "the jobs were not all alive together: the last started {span:?} after the first sleep wait"
    );
    eprintln!(
        "exec-stress workload: sessions={SESSIONS}, jobs detached={}, rewritten sleeps={REWRITTEN}, \
         watches={MONITORS} (ended with their job: {SESSIONS}, rearmed while held: {SESSIONS}), \
         all {JOBS} alive within {span:?}",
        ledger.jobs.len()
    );
    Ok(())
}

#[test]
fn exec_jobs_and_monitors_end_clean_within_the_process_contract() -> support::TestResult<()> {
    if std::env::var_os("DALGONA_ORCH_STRESS").is_none() {
        return Ok(());
    }
    let lane = if std::env::var_os("DAL_TIMING_BUDGETS").is_some() {
        Lane::Idle
    } else {
        Lane::Shared
    };
    let root = TestRoot(
        std::env::temp_dir().join(format!("dalgona-exec-stress-{}", uuid::Uuid::now_v7())),
    );
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    let user = format!(
        "model = \"{JOBS_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n\n[plugin.orchestration.monitor]\nwake_budget = 100\n"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_scenario(&data, &workspace, &user, lane))
}

/// Prints the completion marker of one scenario: the workflow greps for it.
fn scenario_ok(marker: &str) {
    eprintln!("{}", support::scenario_marker(marker));
}

/// Waits for every held tree to self-report, then proves them all alive
/// before the close. Preserves the 120 s report bound, the PID/group counts,
/// and every error string.
async fn await_held_census(reports: &Path) -> support::TestResult<Vec<support::ReportedProcess>> {
    let held = tokio::task::spawn_blocking({
        let reports = reports.to_path_buf();
        move || {
            support::await_process_reports(
                &reports,
                SESSIONS * HELD_PER_SESSION,
                Duration::from_secs(120),
            )
        }
    })
    .await
    .map_err(|join| format!("report reader task failed: {join}"))??;
    let held_census = held.clone();
    let held_alive = tokio::task::spawn_blocking(move || support::assert_all_alive(&held))
        .await
        .map_err(|join| format!("census task failed: {join}"))??;
    // `tree_report_command` writes the shell PID and its background sibling PID.
    let want_pids = SESSIONS * HELD_PER_SESSION * 2;
    let want_groups = if cfg!(windows) {
        0
    } else {
        SESSIONS * HELD_PER_SESSION
    };
    if held_alive.pids.len() != want_pids || held_alive.groups.len() != want_groups {
        return Err(format!(
            "held census saw {}/{} PIDs and {}/{} groups alive before cancel",
            held_alive.pids.len(),
            want_pids,
            held_alive.groups.len(),
            want_groups
        )
        .into());
    }
    eprintln!(
        "exec-stress census: {} PIDs in {} groups alive before cancel",
        held_alive.pids.len(),
        held_alive.groups.len()
    );
    Ok(held_census)
}

/// Closes every session, then proves every recorded held tree is gone from
/// the OS table within 3 s of the cancel. Preserves the close ordering, the
/// census end-time assertion, and every error string.
async fn close_sessions_and_prove_held_gone(
    host: &dal_agent::Host,
    agents: &[dal_agent::Agent],
    held: Vec<support::ReportedProcess>,
) -> support::TestResult<()> {
    // Cancel: closing a session kills its held jobs and ends its watches.
    let cancel_at = Instant::now();
    let closed =
        futures::future::join_all(agents.iter().map(|agent| host.close(agent.session()))).await;
    for result in closed {
        result?;
    }
    // Every recorded tree must be gone from the OS table within 3 s of the
    // cancel. The census end time (not its start) is asserted: census
    // latency is measurement resolution and is logged, never budgeted.
    let census_started = Instant::now();
    let held_gone_at = tokio::task::spawn_blocking(move || {
        support::wait_until_gone(&held, cancel_at, GONE_WITHIN)
    })
    .await
    .map_err(|join| format!("census task failed: {join}"))??;
    eprintln!(
        "exec-stress census: all held trees gone {:?} after cancel (census took {:?})",
        held_gone_at.duration_since(cancel_at),
        held_gone_at.duration_since(census_started)
    );
    if held_gone_at.duration_since(cancel_at) > GONE_WITHIN {
        return Err(format!(
            "held trees took {:?} to vanish, above {GONE_WITHIN:?}",
            held_gone_at.duration_since(cancel_at)
        )
        .into());
    }
    Ok(())
}

/// Runs every session through one real host, checks them, then cancels the
/// run by closing every session and shuts the host down.
async fn run_scenario(
    data: &Path,
    workspace: &Path,
    user: &str,
    lane: Lane,
) -> support::TestResult<()> {
    let reports = workspace.join(support::STRESS_PIDS_DIR);
    fs::create_dir_all(&reports)?;
    let host = start_jobs_host(data, workspace, user).await?;
    let sampler = support::PeakSampler::start(SAMPLE_PERIOD);
    let mut agents = Vec::with_capacity(SESSIONS);
    for _ in 0..SESSIONS {
        agents.push(
            host.open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace.to_path_buf())?,
                    name: None,
                },
                dal_core::ClientId::new(GATE_CLIENT),
            )
            .await?,
        );
    }
    // Every session runs its script at once, on this one task: each driver
    // spends its time awaiting the host, so no spawned task is needed.
    let records = futures::future::try_join_all(
        agents
            .iter()
            .enumerate()
            .map(|(index, agent)| drive(index, agent)),
    )
    .await?;
    check_sessions(&records, lane)?;

    // Nonvacuous census: every held tree self-reported, so prove them all
    // alive before the close; a later disappearance proof then means the
    // close killed them, not that they never started.
    let held = await_held_census(&reports).await?;
    close_sessions_and_prove_held_gone(&host, &agents, held).await?;
    for (record, agent) in records.iter().zip(&agents) {
        let session = agent.session().to_string();
        let ends = wait_for_ledger_ends(data, &session, HELD_PER_SESSION).await?;
        let started = (0..HELD_PER_SESSION)
            .map(|k| {
                record.settled_call(&format!("e{k}")).and_then(|settled| {
                    detached_id(&settled.text).ok_or_else(|| {
                        format!("session {session} held job e{k} had no detached id").into()
                    })
                })
            })
            .collect::<support::TestResult<BTreeSet<_>>>()?;
        let ended = ends.into_iter().map(|end| end.id).collect::<BTreeSet<_>>();
        assert_eq!(
            ended, started,
            "session {} did not end each started held job exactly once",
            record.index
        );
    }
    let report = host.shutdown(Duration::from_secs(10)).await;
    assert_eq!(
        report.tasks_remaining, 0,
        "monitors or jobs kept tasks alive after their sessions closed"
    );
    assert_eq!(report.sessions_closed, 0);
    let peak = tokio::task::spawn_blocking(move || sampler.finish())
        .await
        .map_err(|join| format!("resource sampler task failed: {join}"))??;
    let peak_mib = peak.resident_bytes / (1_024 * 1_024);
    eprintln!(
        "exec-stress peak: RSS {peak_mib} MiB, open handles {} over {} samples",
        peak.open_handles, peak.samples
    );
    assert!(
        peak_mib < RSS_LIMIT_MIB,
        "peak RSS {peak_mib} MiB exceeds the {RSS_LIMIT_MIB} MiB cap"
    );
    assert!(
        peak.open_handles <= FD_LIMIT,
        "peak open handles {} exceed the {FD_LIMIT} cap",
        peak.open_handles
    );
    scenario_ok("held200");
    Ok(())
}

/// One parked fast-batch ledger end: id, outcome state, exit code, text.
struct LedgerEnd {
    id: String,
    state: String,
    code: Option<i64>,
    text: String,
}

/// Parses the `end` lines of one `jobs/reports.jsonl` ledger.
///
/// # Errors
/// Returns the first malformed line.
fn ledger_ends(text: &str) -> support::TestResult<Vec<LedgerEnd>> {
    let mut ends = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: sonic_rs::Value = sonic_rs::from_str(line)
            .map_err(|error| format!("ledger line is not JSON: {error}"))?;
        if value.get("e").and_then(|entry| entry.as_str()) != Some("end") {
            continue;
        }
        let outcome = value.get("outcome");
        ends.push(LedgerEnd {
            id: value
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or_default()
                .to_owned(),
            state: outcome
                .and_then(|outcome| outcome.get("state"))
                .and_then(|state| state.as_str())
                .unwrap_or_default()
                .to_owned(),
            code: outcome
                .and_then(|outcome| outcome.get("code"))
                .and_then(sonic_rs::JsonValueTrait::as_i64),
            text: value
                .get("text")
                .and_then(|text| text.as_str())
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(ends)
}

/// Polls one subscription once without parking: for paused-clock loops,
/// where a real park would auto-advance virtual time.
fn poll_next(subscription: &mut dal_agent::Subscription) -> Option<dal_agent::Delivery> {
    match pump(subscription) {
        Pump::Delivery(delivery) => Some(delivery),
        Pump::Empty | Pump::Ended => None,
    }
}

/// One pump of a subscription: a ready delivery, an orderly end, or nothing
/// ready yet. `Ended` is distinct from `Empty`: only the former fails the
/// slow-reader verdict.
enum Pump {
    Delivery(dal_agent::Delivery),
    Empty,
    Ended,
}

fn pump(subscription: &mut dal_agent::Subscription) -> Pump {
    use std::task::{Context, Poll, Waker};
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let future = subscription.next();
    tokio::pin!(future);
    match future.poll(&mut context) {
        Poll::Ready(Some(delivery)) => Pump::Delivery(delivery),
        Poll::Ready(None) => Pump::Ended,
        Poll::Pending => Pump::Empty,
    }
}

/// One scratch tree shared by the new scenarios: data, workspace, reports.
fn scratch_tree(label: &str) -> support::TestResult<(TestRoot, PathBuf, PathBuf, PathBuf)> {
    let root = TestRoot(std::env::temp_dir().join(format!("{label}-{}", uuid::Uuid::now_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    let reports = workspace.join(support::STRESS_PIDS_DIR);
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&reports)?;
    Ok((root, data, workspace, reports))
}

/// Starts one jobs-model host on the real clock: every scenario builds its
/// setup (host start, session opens) before any fake-clock pause.
async fn start_jobs_host(
    data: &Path,
    workspace: &Path,
    user: &str,
) -> support::TestResult<dal_agent::Host> {
    start_host_with(data, workspace, user, jobs_extension()?).await
}

/// Starts one host on the real clock with `extension` added to the assembled
/// product.
async fn start_host_with(
    data: &Path,
    workspace: &Path,
    user: &str,
    extension: dal_agent::ext::Extension,
) -> support::TestResult<dal_agent::Host> {
    support::grant_bundled_extensions(data, workspace, user, GATE_CLIENT).await?;
    let mut product = support::build_product(data.to_path_buf(), Some(user))?;
    product.extensions.push(extension);
    let env = dal_agent::Env {
        vars: std::collections::BTreeMap::default(),
        cwd: workspace.to_path_buf(),
        sandbox_helper: None,
    };
    Ok(dal_agent::Host::start(product, support::host_config(data, user)?, env).await?)
}

/// Opens one top-level session.
async fn open_session(
    host: &dal_agent::Host,
    workspace: &Path,
) -> support::TestResult<dal_agent::Agent> {
    Ok(host
        .open(
            dal_agent::SessionRef::New {
                workspace: dal_core::Workspace::new(workspace.to_path_buf())?,
                name: None,
            },
            dal_core::ClientId::new(GATE_CLIENT),
        )
        .await?)
}

/// Submits one prompt and requires its acceptance.
async fn submit_prompt(agent: &dal_agent::Agent, text: String) -> support::TestResult<()> {
    let reply = agent
        .submit(dal_core::Command::Prompt {
            expect: dal_core::Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    if !matches!(reply, dal_core::Reply::Accepted { .. }) {
        return Err(format!("the prompt was not accepted: {reply:?}").into());
    }
    Ok(())
}

/// Submits one prompt on a paused clock without parking: an idle park would
/// auto-advance virtual time to the next armed timer, so the submit future
/// is polled in a yield/sleep spin instead of awaited.
async fn spin_submit(agent: &dal_agent::Agent, text: String) -> support::TestResult<()> {
    use std::future::Future as _;
    use std::task::{Context, Poll, Waker};
    let future = agent.submit(dal_core::Command::Prompt {
        expect: dal_core::Expect::Idle,
        content: vec![Part::Text { text: text.into() }],
    });
    tokio::pin!(future);
    let start = std::time::Instant::now();
    loop {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(reply) => {
                let reply = reply?;
                if !matches!(reply, dal_core::Reply::Accepted { .. }) {
                    return Err(format!("the prompt was not accepted: {reply:?}").into());
                }
                return Ok(());
            }
            Poll::Pending => {}
        }
        if start.elapsed() > Duration::from_secs(60) {
            return Err("the paused-clock prompt was never accepted".into());
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Asserts one finished sampler's peaks against the process contract.
fn check_peak(peak: support::ResourcePeak, what: &str) -> support::TestResult<()> {
    let peak_mib = peak.resident_bytes / (1_024 * 1_024);
    eprintln!(
        "{what} peak: RSS {peak_mib} MiB, open handles {} over {} samples",
        peak.open_handles, peak.samples
    );
    if peak_mib >= RSS_LIMIT_MIB {
        return Err(
            format!("{what}: peak RSS {peak_mib} MiB exceeds the {RSS_LIMIT_MIB} MiB cap").into(),
        );
    }
    if peak.open_handles > FD_LIMIT {
        return Err(format!(
            "{what}: peak open handles {} exceed the {FD_LIMIT} cap",
            peak.open_handles
        )
        .into());
    }
    Ok(())
}

#[test]
fn fake_clock_sleep_detach_one_hundred_jobs() -> support::TestResult<()> {
    if !support::stress_enabled() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(fake_clock_scenario())
}

/// Waits for all 100 sleep trees to report on real I/O without parking the
/// paused clock, then proves the handshake liveness. Preserves the 180 s
/// handshake bound, the 2 ms poll cadence, and every error string.
async fn fake_clock_handshake(
    reports: &Path,
    frozen: &tokio::time::Instant,
) -> support::TestResult<Vec<support::ReportedProcess>> {
    // Handshake on real I/O without parking: poll, yield, brief thread
    // sleep. The runtime never parks, so the clock cannot auto-advance.
    let handshake = std::time::Instant::now();
    loop {
        if support::count_process_reports(reports) >= FAKE_JOBS {
            break;
        }
        if handshake.elapsed() > Duration::from_secs(180) {
            return Err(format!(
                "only {} of {FAKE_JOBS} sleep trees started",
                support::count_process_reports(reports)
            )
            .into());
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(2));
    }
    let reported = support::read_process_reports(reports)?;
    if reported.len() != FAKE_JOBS {
        return Err(format!(
            "expected {FAKE_JOBS} sleep reports, read {}",
            reported.len()
        )
        .into());
    }
    if frozen.elapsed().as_millis() != 0 {
        return Err(
            "virtual time moved before the handshake; fake time could manufacture startup".into(),
        );
    }
    let live_before = support::survivors(&reported)?;
    // Windows reports no process groups: per-PID liveness only.
    let want_groups = if cfg!(windows) { 0 } else { FAKE_JOBS };
    if live_before.pids.len() != FAKE_JOBS * 2 || live_before.groups.len() != want_groups {
        return Err(format!(
            "only {}/{} PIDs and {}/{} groups alive at the handshake",
            live_before.pids.len(),
            FAKE_JOBS * 2,
            live_before.groups.len(),
            FAKE_JOBS
        )
        .into());
    }
    Ok(reported)
}

/// Drains every ready delivery of the fake-clock subscriptions into the
/// records. Preserves the `Resync` failure and the observe effects.
fn drain_fake_updates(
    records: &mut [SessionRecord],
    subscriptions: &mut [dal_agent::Subscription],
) -> support::TestResult<()> {
    for (record, subscription) in records.iter_mut().zip(subscriptions.iter_mut()) {
        while let Some(delivery) = poll_next(subscription) {
            match delivery {
                dal_agent::Delivery::Update(update) => record.observe(&update.kind),
                dal_agent::Delivery::Resync { .. } => {
                    return Err("the update stream lost its place".into());
                }
            }
        }
    }
    Ok(())
}

/// Proves the 4999 ms quiet window detaches nothing, then steps over the 5 s
/// budget edge until all 100 detach. Preserves the 300 ms quiet bound, the
/// tick-slack steps, the 10 s round bound, the 2 ms cadence, and every error.
async fn fake_clock_budget_edge(
    subscriptions: &mut [dal_agent::Subscription],
    frozen: &tokio::time::Instant,
) -> support::TestResult<()> {
    // Just short of the budget: nothing may detach.
    tokio::time::advance(Duration::from_millis(FAKE_WINDOW_MS - 1)).await;
    let quiet = std::time::Instant::now();
    let mut records: Vec<SessionRecord> = (0..FAKE_JOBS).map(SessionRecord::new).collect();
    loop {
        drain_fake_updates(&mut records, subscriptions)?;
        if quiet.elapsed() > Duration::from_millis(300) {
            break;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(2));
    }
    for record in &records {
        if !record.settled.is_empty() {
            return Err(format!("session {} detached before its 5 s window", record.index).into());
        }
    }
    // The budget edge, plus tick-slack steps: all 100 detach here.
    let mut steps = 0;
    loop {
        let detached: usize = records.iter().map(|record| record.settled.len()).sum();
        if detached >= FAKE_JOBS {
            break;
        }
        if steps > FAKE_TICK_SLACK_STEPS {
            return Err(format!(
                "only {detached} of {FAKE_JOBS} sleeps detached at the budget edge"
            )
            .into());
        }
        tokio::time::advance(Duration::from_millis(1)).await;
        steps += 1;
        let round = std::time::Instant::now();
        loop {
            drain_fake_updates(&mut records, subscriptions)?;
            let detached: usize = records.iter().map(|record| record.settled.len()).sum();
            if detached >= FAKE_JOBS || round.elapsed() > Duration::from_secs(10) {
                break;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let virtual_ms = frozen.elapsed().as_millis();
    if virtual_ms > u128::from(FAKE_WINDOW_MS + u64::from(FAKE_TICK_SLACK_STEPS)) {
        return Err(format!("detaches landed at {virtual_ms} ms of virtual time").into());
    }
    for record in &records {
        let settled = record.settled_call(&format!("f{}", record.index))?;
        if settled.is_error || !settled.text.contains(DETACH_MARKER) {
            return Err(
                format!("session {} did not detach: {}", record.index, settled.text).into(),
            );
        }
    }
    eprintln!(
        "fake-clock detach: {FAKE_JOBS} sleeps detached at {virtual_ms} ms virtual, none before 4999 ms"
    );
    Ok(())
}

/// 100 rewritten sleep waits over the real `exec` boundary on a paused
/// clock. All setup runs on the real clock; the pause lands only once every
/// session is open. Real PID files are the startup handshake: virtual time
/// must still read zero when the last one lands, so fake time cannot
/// manufacture process startup. Then 4999 ms of virtual time must detach
/// nothing, and the 5 s budget edge must detach all 100 while every real
/// `sleep 30` still runs underneath.
async fn fake_clock_scenario() -> support::TestResult<()> {
    let (_root, data, workspace, reports) = scratch_tree("dalgona-fake-sleep")?;
    let user = format!("model = \"{JOBS_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n");
    let host = start_jobs_host(&data, &workspace, &user).await?;
    let mut agents = Vec::with_capacity(FAKE_JOBS);
    let mut subscriptions = Vec::with_capacity(FAKE_JOBS);
    for _ in 0..FAKE_JOBS {
        let agent = open_session(&host, &workspace).await?;
        subscriptions.push(agent.subscribe_listen(None)?);
        agents.push(agent);
    }
    // The pause needs the test-util clock: only a current-thread runtime
    // may freeze time, and this whole scenario runs on one.
    tokio::time::pause();
    let frozen = tokio::time::Instant::now();
    for (index, agent) in agents.iter().enumerate() {
        spin_submit(agent, format!("{FAKE_TAG}{index}")).await?;
    }
    let reported = fake_clock_handshake(&reports, &frozen).await?;
    fake_clock_budget_edge(&mut subscriptions, &frozen).await?;
    // The detach came from the budget, not from exits: every real sleep
    // still runs underneath.
    let live_after = support::survivors(&reported)?;
    if live_after.pids.len() != FAKE_JOBS * 2 {
        return Err("sleep trees exited before their 30 s were up".into());
    }
    // Teardown on the resumed clock: closes and shutdowns use real timers.
    tokio::time::resume();
    let cancel_at = Instant::now();
    let closed =
        futures::future::join_all(agents.iter().map(|agent| host.close(agent.session()))).await;
    for result in closed {
        result?;
    }
    let gone_at = tokio::task::spawn_blocking(move || {
        support::wait_until_gone(&reported, cancel_at, GONE_WITHIN)
    })
    .await
    .map_err(|join| format!("census task failed: {join}"))??;
    if gone_at.duration_since(cancel_at) > GONE_WITHIN {
        return Err("fake-clock trees outlived the 3 s census".into());
    }
    let report = host.shutdown(Duration::from_secs(10)).await;
    if report.tasks_remaining != 0 {
        return Err("tasks outlived the fake-clock shutdown".into());
    }
    scenario_ok("fake-clock100");
    Ok(())
}

#[test]
fn subagent_exec_cancel_kills_every_process_tree() -> support::TestResult<()> {
    if !support::stress_enabled() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(cancel_scenario())
}

/// Waits for all 220 cancel trees to self-report, then proves them all alive
/// before the cancel. Preserves the 300 s report bound, the PID/group counts,
/// and every error string.
async fn await_cancel_census(reports: &Path) -> support::TestResult<Vec<support::ReportedProcess>> {
    // The last spawn: all 220 trees (200 background plus 20 parked) have
    // written their PID files.
    let reported = tokio::task::spawn_blocking({
        let reports = reports.to_path_buf();
        move || support::await_process_reports(&reports, CANCEL_TREES, Duration::from_secs(300))
    })
    .await
    .map_err(|join| format!("report reader task failed: {join}"))??;
    let live = tokio::task::spawn_blocking({
        let reported = reported.clone();
        move || support::assert_all_alive(&reported)
    })
    .await
    .map_err(|join| format!("census task failed: {join}"))??;
    // `tree_report_command` writes the shell PID and its background sibling PID.
    let want_pids = CANCEL_TREES * 2;
    let want_groups = if cfg!(windows) { 0 } else { CANCEL_TREES };
    if live.pids.len() != want_pids || live.groups.len() != want_groups {
        return Err(format!(
            "cancel census saw {}/{} PIDs and {}/{} groups alive before cancel",
            live.pids.len(),
            want_pids,
            live.groups.len(),
            want_groups
        )
        .into());
    }
    eprintln!(
        "cancel census: {} PIDs in {} groups alive before cancel",
        live.pids.len(),
        live.groups.len()
    );
    Ok(reported)
}

/// Proves the durable cancel evidence: the run notice delivered once in the
/// parent journal, exactly 10 cancelled ends in every child ledger, and the
/// single cancelled run end in the parent ledger. Preserves every ledger read
/// and every error string.
async fn check_cancel_ledgers(
    data: &Path,
    session: &str,
    children: &[String],
) -> support::TestResult<()> {
    // The run notice, delivered once, in the parent journal.
    let journal = wait_for_parent_cancel(data, session).await?;
    // Every child ledger: exactly 10 cancelled ends.
    for child in children {
        let ends = wait_for_ledger_ends(data, child, CANCEL_JOBS_PER_CHILD).await?;
        if ends.iter().any(|end| end.state != "cancelled") {
            return Err(format!("child {child} has a non-cancelled job end").into());
        }
    }
    // The parent ledger: exactly the run's own cancelled end.
    let parent_ends = wait_for_ledger_ends(data, session, 1).await?;
    if parent_ends.len() != 1 || parent_ends[0].state != "cancelled" {
        return Err("the parent ledger holds no single cancelled run end".into());
    }
    if !parent_ends[0].text.contains("cancelled after") {
        return Err("the parent run end carries no cancel summary".into());
    }
    eprintln!(
        "cancel ledgers: 200 child exec ends plus 1 run end, all cancelled; journal:\n{journal}"
    );
    Ok(())
}

/// 20 subagents in `yolo` mode each start 10 background exec jobs plus one
/// parked foreground exec; the run is cancelled 2 s after the last spawn.
/// Durable proof: 200 `cancelled` ends across the child ledgers, 20
/// cancelled subagent jobs plus 1 cancelled run in the parent journal and
/// ledger, the run notice delivered exactly once, and every recorded tree
/// gone from the OS table within 3 s of the cancel prompt.
async fn cancel_scenario() -> support::TestResult<()> {
    let (_root, data, workspace, reports) = scratch_tree("dalgona-cancel-trees")?;
    let user = format!("model = \"{JOBS_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n");
    let host = start_jobs_host(&data, &workspace, &user).await?;
    let agent = open_session(&host, &workspace).await?;
    let session = agent
        .view(dal_core::PageReq::default())?
        .session
        .id
        .to_string();
    let mut lifecycle = host.subscribe();
    let sampler = support::PeakSampler::start(SAMPLE_PERIOD);
    submit_prompt(&agent, CANCEL_TAG.to_owned()).await?;
    let reported = await_cancel_census(&reports).await?;
    // 2 s after the last spawn, to the instant.
    let observed = Instant::now();
    let due = observed + CANCEL_DELAY;
    if due > Instant::now() {
        tokio::time::sleep(due - Instant::now()).await;
    }
    let cancel_at = Instant::now();
    // The OS census starts at the cancel boundary, so the 3 s bound measures
    // process disappearance, not report settlement: settlement (prompt, drain,
    // ledgers) runs concurrently with the census below.
    let census = tokio::task::spawn_blocking(move || {
        support::wait_until_gone(&reported, cancel_at, GONE_WITHIN)
    });
    // Settlement reports a value instead of `?`-ing out, so the census handle
    // is awaited on every path.
    let settlement: support::TestResult<()> = async {
        submit_prompt(&agent, CANCEL_NOW_TAG.to_owned()).await?;
        // Children of this parent: `ChildStarted` is published per child; no
        // `ChildEnded` exists, so each child's end is proven from its durable
        // ledger below, not from a host event.
        let mut children: Vec<String> = Vec::new();
        let drained = tokio::time::timeout(Duration::from_secs(180), async {
            while children.len() < CANCEL_CHILDREN {
                match lifecycle.next().await {
                    Some(dal_agent::HostUpdate::ChildStarted { parent, child, .. })
                        if parent.to_string() == session =>
                    {
                        children.push(child.to_string());
                    }
                    Some(_) => {}
                    None => break,
                }
            }
        })
        .await;
        drained.map_err(|_| "only a fraction of the 20 children started".to_string())?;
        if children.len() != CANCEL_CHILDREN {
            return Err(format!(
                "saw {} child starts, expected {CANCEL_CHILDREN}",
                children.len()
            )
            .into());
        }
        check_cancel_ledgers(&data, &session, &children).await?;
        Ok(())
    }
    .await;
    // The OS table: nothing recorded survives past 3 s after the cancel.
    let census_outcome = census
        .await
        .map_err(|join| format!("census task failed: {join}"))?;
    // Census first: when trees genuinely survive, settlement usually fails
    // too, and the survival signal must not hide behind it.
    let gone_at = census_outcome?;
    settlement?;
    if gone_at.duration_since(cancel_at) > GONE_WITHIN {
        return Err(format!(
            "process trees took {:?} to vanish, above {GONE_WITHIN:?}",
            gone_at.duration_since(cancel_at)
        )
        .into());
    }
    eprintln!(
        "cancel census: all process trees gone {:?} after cancel",
        gone_at.duration_since(cancel_at)
    );
    let shutdown = host.shutdown(Duration::from_secs(10)).await;
    if shutdown.tasks_remaining != 0 {
        return Err("tasks outlived the cancel shutdown".into());
    }
    let peak = tokio::task::spawn_blocking(move || sampler.finish())
        .await
        .map_err(|join| format!("resource sampler task failed: {join}"))??;
    check_peak(peak, "cancel-20x10")?;
    scenario_ok("cancel-20x10");
    Ok(())
}

/// Waits for the parent journal to carry the run's cancel summary exactly
/// once, holds 2 s against a late duplicate, then returns the line.
async fn wait_for_parent_cancel(data: &Path, session: &str) -> support::TestResult<String> {
    // `counts_line` omits zeros and orders `done, failed, cancelled`, so an
    // all-cancelled run of 20 tasks renders exactly `20 tasks: 20 cancelled`.
    let tally = format!("{CANCEL_CHILDREN} tasks: {CANCEL_CHILDREN} cancelled");
    let found = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if let Some(dir) = support::session_dir(data, session) {
                let text = fs::read_to_string(dir.join("journal.jsonl")).unwrap_or_default();
                let hits: Vec<&str> = support::journal_records(&text, "user")
                    .into_iter()
                    .filter(|record| {
                        record.contains("execjobs")
                            && record.contains("cancelled after")
                            && record.contains(tally.as_str())
                    })
                    .collect();
                if hits.len() == 1 {
                    let line = hits[0].to_owned();
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let again = fs::read_to_string(dir.join("journal.jsonl")).unwrap_or_default();
                    let still: Vec<&str> = support::journal_records(&again, "user")
                        .into_iter()
                        .filter(|record| {
                            record.contains("execjobs")
                                && record.contains("cancelled after")
                                && record.contains(tally.as_str())
                        })
                        .collect();
                    if still.len() == 1 {
                        return Ok::<String, Box<dyn std::error::Error + Send + Sync>>(line);
                    }
                    return Err("the cancel summary was delivered more than once".into());
                }
                if hits.len() > 1 {
                    return Err("the cancel summary was delivered more than once".into());
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    found.map_err(|_| "the run notice never arrived".to_string())?
}

/// Waits for one session ledger to hold `expected` end lines.
async fn wait_for_ledger_ends(
    data: &Path,
    session: &str,
    expected: usize,
) -> support::TestResult<Vec<LedgerEnd>> {
    let ends = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(path) = support::session_ledger(data, session)
                && let Ok(text) = fs::read_to_string(&path)
                && let Ok(parsed) = ledger_ends(&text)
                && parsed.len() == expected
            {
                return Ok::<Vec<LedgerEnd>, Box<dyn std::error::Error + Send + Sync>>(parsed);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    ends.map_err(|_| format!("session {session} never settled {expected} job ends").to_string())?
}

#[test]
fn parent_fast_jobs_complete_and_deliver_once() -> support::TestResult<()> {
    if !support::stress_enabled() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(fast_scenario())
}

/// What one subscription drain saw of the job traffic of its session.
#[derive(Default)]
struct JobDrain {
    /// Every delivery drained.
    count: usize,
    /// How many deliveries were `Resync`.
    resync: usize,
    /// The jobs named by `JobStarted`/`JobSettled` updates, for the per-job
    /// rate bound.
    jobs: Vec<String>,
    /// How many `exec` results named a detached job.
    detached: usize,
    /// Whether the stream ended.
    ended: bool,
}

impl JobDrain {
    /// The drained deliveries that prove job traffic reached the reader.
    ///
    /// Production publishes no `JobStarted`/`JobSettled` for `exec` jobs, so
    /// the `detached as job` result of the call that started one is the job
    /// start a session subscriber can see. Turn, tree, and usage updates
    /// flow with no job at all and prove nothing about this workload.
    fn relevant(&self) -> usize {
        self.jobs.len() + self.detached
    }
}

/// Drains every delivery that is ready now, without parking.
fn drain_jobs(subscription: &mut dal_agent::Subscription) -> JobDrain {
    let mut drain = JobDrain::default();
    loop {
        match pump(subscription) {
            Pump::Delivery(dal_agent::Delivery::Update(update)) => {
                drain.count += 1;
                match &update.kind {
                    dal_core::UpdateKind::JobStarted { job }
                    | dal_core::UpdateKind::JobSettled { job } => drain.jobs.push(job.to_string()),
                    dal_core::UpdateKind::ToolSettled { outcome, .. }
                        if detached_id(&outcome.text).is_some() =>
                    {
                        drain.detached += 1;
                    }
                    _ => {}
                }
            }
            Pump::Delivery(dal_agent::Delivery::Resync { .. }) => {
                drain.count += 1;
                drain.resync += 1;
            }
            Pump::Empty => return drain,
            Pump::Ended => {
                drain.ended = true;
                return drain;
            }
        }
    }
}

/// Spawns the 250 ms slow reader on `agent`: every drain goes to a shared
/// [`support::SubscriberLedger`], which fails on any `Resync`, an early end,
/// or a stream that carried no job traffic.
fn spawn_job_reader(
    agent: &dal_agent::Agent,
    stop: &Arc<AtomicBool>,
) -> support::TestResult<tokio::task::JoinHandle<support::SubscriberLedger>> {
    let mut subscription = agent.subscribe_listen(None)?;
    let stop = Arc::clone(stop);
    Ok(tokio::spawn(async move {
        let mut ledger = support::SubscriberLedger::default();
        loop {
            tokio::time::sleep(support::SUBSCRIBER_PERIOD).await;
            // The flag is read before the drain: a drain that starts after
            // the stop flag sees everything delivered before it.
            let stopping = stop.load(Ordering::Acquire);
            let drain = drain_jobs(&mut subscription);
            if drain.ended {
                ledger.record_early_end();
                break;
            }
            if drain.count > 0 || stopping {
                ledger.record_drain(
                    drain.count,
                    drain.resync,
                    &drain.jobs,
                    drain.relevant(),
                    Instant::now(),
                );
            }
            if stopping {
                break;
            }
        }
        ledger
    }))
}

/// 200 fast background jobs across 25 parent sessions: each `sleep 1`
/// detaches at the 1 s foreground floor and exits half a second later, so
/// every completion travels the real detached-job delivery path. All 200
/// end `done` in the durable ledgers and each job id reaches exactly one
/// wake report; peak overlap is derived from spawn instants and logged,
/// never asserted at 200.
async fn fast_scenario() -> support::TestResult<()> {
    let (_root, data, workspace, _reports) = scratch_tree("dalgona-fast-jobs")?;
    let user = format!("model = \"{JOBS_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n");
    let host = start_jobs_host(&data, &workspace, &user).await?;
    let sampler = support::PeakSampler::start(SAMPLE_PERIOD);
    let mut agents = Vec::with_capacity(FAST_SESSIONS);
    for _ in 0..FAST_SESSIONS {
        agents.push(open_session(&host, &workspace).await?);
    }
    // One slow subscriber on the first session proves the 250 ms reader
    // takes no Resync under the 200-job delivery load. Its workload
    // evidence is the job traffic its own session carries: see
    // `JobDrain::relevant`.
    let stop = Arc::new(AtomicBool::new(false));
    let slow = spawn_job_reader(&agents[0], &stop)?;
    let session_ids: Vec<String> = agents
        .iter()
        .map(|agent| agent.session().to_string())
        .collect();
    let records = futures::future::try_join_all(
        agents
            .iter()
            .enumerate()
            .map(|(index, agent)| drive_fast(index, agent)),
    )
    .await?;
    stop.store(true, Ordering::Release);
    let ledger = slow
        .await
        .map_err(|join| format!("slow reader failed: {join}"))?;
    eprintln!("fast-batch subscriber: {}", ledger.summary());
    ledger.verdict()?;
    // Session 0 detached every one of its jobs before the stop flag, so its
    // reader must have seen each of them, not merely some traffic.
    if ledger.relevant < FAST_PER_SESSION {
        return Err(format!(
            "the slow reader saw {} job starts of its session's {FAST_PER_SESSION}",
            ledger.relevant
        )
        .into());
    }
    check_fast_sessions(&data, &records, &session_ids).await?;
    let shutdown = host.shutdown(Duration::from_secs(10)).await;
    if shutdown.tasks_remaining != 0 {
        return Err("tasks outlived the fast-batch shutdown".into());
    }
    let peak = tokio::task::spawn_blocking(move || sampler.finish())
        .await
        .map_err(|join| format!("resource sampler task failed: {join}"))??;
    check_peak(peak, "fast200")?;
    scenario_ok("fast200");
    Ok(())
}

/// Runs one fast-batch session: the start prompt, then every update until
/// its eight exec calls have all settled detached.
async fn drive_fast(index: usize, agent: &dal_agent::Agent) -> support::TestResult<SessionRecord> {
    let mut updates = agent.subscribe_listen(None)?;
    submit_prompt(agent, format!("{FAST_TAG}{index}")).await?;
    let mut record = SessionRecord::new(index);
    let waited = tokio::time::timeout(HANG_GUARD, async {
        while record
            .settled
            .iter()
            .filter(|settled| settled.call.starts_with('q'))
            .count()
            < FAST_PER_SESSION
        {
            let delivery = updates.next().await.ok_or("the update stream closed")?;
            match delivery {
                dal_agent::Delivery::Update(update) => record.observe(&update.kind),
                dal_agent::Delivery::Resync { .. } => {
                    return Err("the update stream lost its place".into());
                }
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    waited.map_err(|_| format!("session {index}: its fast jobs never all settled"))??;
    Ok(record)
}

/// Whether the ledger output text carries the fast command's final success
/// output: a whole line that is exactly `ok`. A substring test cannot tell
/// that line from the unique token, because `tok-<uuid>` already contains
/// `ok`.
fn has_standalone_ok_line(text: &str) -> bool {
    text.lines().any(|line| line.trim() == "ok")
}

/// Checks every fast session and the batch as a whole: 200 distinct
/// detached jobs, all `done` in the durable ledgers, each delivered by
/// exactly one wake report. Peak overlap is logged, not asserted.
async fn check_fast_sessions(
    data: &Path,
    records: &[SessionRecord],
    session_ids: &[String],
) -> support::TestResult<()> {
    let mut jobs = BTreeSet::new();
    let mut spans: Vec<(Instant, Instant)> = Vec::new();
    for record in records {
        if !record.failed_turns.is_empty() {
            return Err(format!(
                "session {}: turns ended abnormally: {:?}",
                record.index, record.failed_turns
            )
            .into());
        }
        for k in 0..FAST_PER_SESSION {
            let settled = record.settled_call(&format!("q{k}"))?;
            if settled.is_error {
                return Err(format!(
                    "session {}: fast q{k} failed: {}",
                    record.index, settled.text
                )
                .into());
            }
            let id = detached_id(&settled.text)
                .ok_or_else(|| format!("session {}: fast q{k} did not detach", record.index))?;
            jobs.insert(id);
            let elapsed = settled.elapsed_ms.ok_or_else(|| {
                format!("session {}: fast q{k} reported no run time", record.index)
            })?;
            let spawned = settled
                .seen
                .checked_sub(Duration::from_millis(elapsed))
                .ok_or_else(|| {
                    format!(
                        "session {}: fast q{k} ran longer than the gate",
                        record.index
                    )
                })?;
            spans.push((spawned, settled.seen));
        }
    }
    if jobs.len() != FAST_JOBS {
        return Err(format!("only {} of {FAST_JOBS} fast jobs are distinct", jobs.len()).into());
    }
    // Durable ends, per session ledger: every fast job exits cleanly. Ends
    // land up to ~0.5 s after the last detach plus fold latency, so poll
    // each ledger to eight ends first; asserting on a single read races the
    // authored tail.
    for session in session_ids {
        let ends = wait_for_ledger_ends(data, session, FAST_PER_SESSION).await?;
        for end in &ends {
            if end.state != "exited" || end.code != Some(0) || !has_standalone_ok_line(&end.text) {
                return Err(format!(
                    "session {session} job {} is not a clean done: exit state {:?}, code {:?}, output {:?}",
                    end.id, end.state, end.code, end.text
                )
                .into());
            }
        }
    }
    // Exactly-once wake delivery may lag the ledger: poll until every job
    // id sits in exactly one wake report.
    wait_for_fast_delivery(data, session_ids).await?;
    eprintln!("fast-batch workload: {FAST_JOBS} distinct detached jobs");
    // Peak overlap from spawn instants: informational only.
    let mut starts: Vec<Instant> = spans.iter().map(|span| span.0).collect();
    starts.sort();
    let mut peak_overlap = 0;
    for (index, start) in starts.iter().enumerate() {
        let alive = spans
            .iter()
            .filter(|span| span.0 <= *start && span.1 > *start)
            .count();
        let _ = index;
        peak_overlap = peak_overlap.max(alive);
    }
    eprintln!("fast-batch peak overlap (derived, not asserted): {peak_overlap}");
    Ok(())
}

/// Polls until every fast job id of every session sits in exactly one wake
/// report of its own session journal.
async fn wait_for_fast_delivery(data: &Path, session_ids: &[String]) -> support::TestResult<()> {
    let waited = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let mut pending = 0;
            for session in session_ids {
                let covered = support::session_ledger(data, session)
                    .and_then(|path| fs::read_to_string(&path).ok())
                    .and_then(|text| ledger_ends(&text).ok())
                    .filter(|ends| ends.len() == FAST_PER_SESSION)
                    .and_then(|ends| {
                        support::session_dir(data, session).and_then(|dir| {
                            fs::read_to_string(dir.join("journal.jsonl"))
                                .ok()
                                .map(|journal| {
                                    let wakes = support::journal_records(&journal, "wake_attempt");
                                    ends.iter().all(|end| {
                                        let quoted = format!("\"{}\"", end.id);
                                        wakes
                                            .iter()
                                            .filter(|record| record.contains(&quoted))
                                            .count()
                                            == 1
                                    })
                                })
                        })
                    })
                    .unwrap_or(false);
                if !covered {
                    pending += 1;
                }
            }
            if pending == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    waited.map_err(|_| "fast jobs were not all delivered exactly once".to_string())?;
    Ok(())
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A session cursor as a public client holds it: the generation and the last
/// sequence it has seen, exactly the pair `Agent::view` reports.
type Cursor = (dal_core::Gen, dal_core::Seq);

/// The synthetic model of the two resync tests.
const RESYNC_MODEL: &str = "resync-gate/deltas";
/// The prompt of one delta flood: `flood text deltas <n> per chunk <k>`.
const FLOOD_TAG: &str = "flood text deltas ";
const FLOOD_CHUNK_TAG: &str = " per chunk ";
/// Text deltas the model streams between two confirmations of the test. The
/// stream parks after every chunk until the test has received the whole
/// chunk on its always-drained observer, so the observer's queue never holds
/// more than one chunk and cannot be the subscriber that overflows.
const FLOOD_CHUNK: usize = 256;
/// Updates a top-level session's replay window retains: the engine documents
/// `RingCaps::TOP_LEVEL` as 4096 updates or 1 MiB. The count is the case
/// size; the byte side is a measured guard, never an assumption: every
/// window case asserts its flood's serialized total stays below
/// `REPLAY_WINDOW_BYTES`, so only the count eviction can explain the
/// `N + 1` resync.
const REPLAY_WINDOW_UPDATES: usize = 4_096;
/// The companion byte cap of the replay window above. Used only to prove a
/// flood stayed under it, never as a resync oracle.
const REPLAY_WINDOW_BYTES: usize = 1 << 20;
/// Deliveries a live subscriber's queue holds before it sheds. The N/N+1
/// cases below prove this bound behaviorally (N undrained updates fit, N+1
/// sheds), so the number is a case size, not an oracle import.
const LIVE_QUEUE_UPDATES: usize = 1_024;
const _: () = assert!(FLOOD_CHUNK * 2 < LIVE_QUEUE_UPDATES);
const _: () = assert!(2 * LIVE_QUEUE_UPDATES < REPLAY_WINDOW_UPDATES);

/// The newest flood prompt of the context: the delta count and the chunk
/// size, or `None` for any other request.
fn flood_request(context: &[ContextItem]) -> Option<(usize, usize)> {
    let item = context
        .iter()
        .rev()
        .find(|item| matches!(item, ContextItem::User { .. }))?;
    let text = item_text(item);
    let rest = &text[text.find(FLOOD_TAG)? + FLOOD_TAG.len()..];
    let (total, rest) = rest.split_once(FLOOD_CHUNK_TAG)?;
    let chunk: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let chunk: usize = chunk.parse().ok()?;
    (chunk > 0).then_some((total.trim().parse().ok()?, chunk))
}

/// One response of `total` one-character `TextDelta` events, then a clean
/// end. After every `chunk` deltas, and while more remain, the stream parks
/// until the test grants one permit: the controllable, event-gated stream.
fn gated_delta_stream(
    total: usize,
    chunk: usize,
    gate: Arc<tokio::sync::Semaphore>,
) -> EventStream {
    let deltas = futures::stream::unfold(0_usize, move |sent| {
        let gate = Arc::clone(&gate);
        async move {
            if sent == total {
                return None;
            }
            if sent > 0
                && sent.is_multiple_of(chunk)
                && let Ok(permit) = gate.acquire().await
            {
                permit.forget();
            }
            Some((
                Ok::<StreamEvent, ProviderError>(StreamEvent::TextDelta {
                    text: "d".to_owned(),
                }),
                sent + 1,
            ))
        }
    });
    let close = futures::stream::iter([
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ]);
    EventStream::new(deltas.chain(close), || {})
}

/// The synthetic model that streams the flood a prompt asks for and answers
/// every other request with one short text.
struct DeltaModel {
    gate: Arc<tokio::sync::Semaphore>,
}

impl ModelHandler for DeltaModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            Ok(match flood_request(request.context.as_ref()) {
                Some((total, chunk)) => gated_delta_stream(total, chunk, Arc::clone(&self.gate)),
                None => text_stream("noted"),
            })
        })
    }
}

/// The synthetic-model extension of the resync tests.
fn resync_extension(
    gate: Arc<tokio::sync::Semaphore>,
) -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    let record = ModelRecord {
        id: ModelId::parse(RESYNC_MODEL)?,
        caps: Caps {
            context_window: Some(200_000),
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
        handler: Arc::new(DeltaModel { gate }),
        export: None,
    };
    ExtensionBuilder::new("resync-deltas", "0.1.0", ServiceSet::default())?
        .model(record)
        .build()
}

/// One real host with the delta model, its scratch tree, and the gate its
/// model parks on.
struct ResyncFixture {
    _root: TestRoot,
    workspace: PathBuf,
    gate: Arc<tokio::sync::Semaphore>,
    host: dal_agent::Host,
}

impl ResyncFixture {
    async fn start(label: &str) -> support::TestResult<Self> {
        let (root, data, workspace, _reports) = scratch_tree(label)?;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let user =
            format!("model = \"{RESYNC_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n");
        let host = start_host_with(
            &data,
            &workspace,
            &user,
            resync_extension(Arc::clone(&gate))?,
        )
        .await?;
        Ok(Self {
            _root: root,
            workspace,
            gate,
            host,
        })
    }

    async fn open(&self) -> support::TestResult<dal_agent::Agent> {
        open_session(&self.host, &self.workspace).await
    }

    /// Releases a model parked at a chunk boundary, shuts the host down on
    /// every path, then reports `outcome` before the shutdown verdict.
    async fn finish(self, outcome: support::TestResult<()>) -> support::TestResult<()> {
        self.gate.add_permits(REPLAY_WINDOW_UPDATES);
        let report = self.host.shutdown(Duration::from_secs(10)).await;
        outcome?;
        if report.tasks_remaining != 0 {
            return Err("tasks outlived the resync host shutdown".into());
        }
        Ok(())
    }
}

/// The cursor a client reads from the view of an idle session.
fn view_cursor(agent: &dal_agent::Agent) -> support::TestResult<Cursor> {
    let view = agent.view(dal_core::PageReq::default())?;
    Ok((view.r#gen, view.seq))
}

fn is_delta(update: &dal_core::Update) -> bool {
    matches!(update.kind, dal_core::UpdateKind::Delta { .. })
}

/// Every delivery that is ready now, and whether the stream then ended
/// (`true`) or is merely quiet and still live (`false`).
fn drain_ready(subscription: &mut dal_agent::Subscription) -> (Vec<dal_agent::Delivery>, bool) {
    let mut deliveries = Vec::new();
    loop {
        match pump(subscription) {
            Pump::Delivery(delivery) => deliveries.push(delivery),
            Pump::Empty => return (deliveries, false),
            Pump::Ended => return (deliveries, true),
        }
    }
}

/// One finished flood turn as the always-drained observer saw it.
struct Flood {
    /// The cursor the view reported before the prompt.
    cursor: Cursor,
    /// The cursor the view reported after the turn settled.
    head: Cursor,
    /// Every update published after `cursor`, in order.
    updates: Vec<Arc<dal_core::Update>>,
    /// The measured serialized length of every update above, via the same
    /// `sonic_rs::to_vec` the replay ring sizes entries with. The window
    /// cases assert this total stays below the byte cap, so only the
    /// update-count eviction can explain an `N + 1` resync.
    bytes: usize,
}

/// Runs one flood turn of `deltas` text deltas on `agent` and records every
/// update it published after the cursor the view reported just before.
///
/// `observer` is a live subscription of the same session that this function
/// drains in lockstep with the model: it grants the model one permit after
/// each full chunk, so the observer is never behind by more than a chunk.
/// The fixture guards below fail with a fixture message, never as a product
/// verdict, when the turn did not publish a contiguous run ending at the
/// view's head or when a delta did not map to one update.
async fn run_flood(
    agent: &dal_agent::Agent,
    gate: &tokio::sync::Semaphore,
    observer: &mut dal_agent::Subscription,
    deltas: usize,
) -> support::TestResult<Flood> {
    let cursor = view_cursor(agent)?;
    submit_prompt(
        agent,
        format!("{FLOOD_TAG}{deltas}{FLOOD_CHUNK_TAG}{FLOOD_CHUNK}"),
    )
    .await?;
    let mut updates: Vec<Arc<dal_core::Update>> = Vec::new();
    let mut seen_deltas = 0_usize;
    let drained = tokio::time::timeout(HANG_GUARD, async {
        loop {
            match observer.next().await {
                Some(dal_agent::Delivery::Update(update)) => {
                    let ended = matches!(update.kind, dal_core::UpdateKind::TurnEnded { .. });
                    if is_delta(&update) {
                        seen_deltas += 1;
                        if seen_deltas.is_multiple_of(FLOOD_CHUNK) && seen_deltas < deltas {
                            gate.add_permits(1);
                        }
                    }
                    updates.push(update);
                    if ended {
                        return Ok::<(), BoxError>(());
                    }
                }
                Some(dal_agent::Delivery::Resync { .. }) => {
                    return Err(
                        "the always-drained observer was resynchronised: the fixture \
                         fell behind, not the product"
                            .into(),
                    );
                }
                None => return Err("the observer stream ended before the turn did".into()),
            }
        }
    })
    .await;
    drained.map_err(|_| format!("the {deltas}-delta flood turn never ended"))??;
    // The actor has swept its mailbox once the status poll returns, so no
    // update of the turn can still be in flight when the view is read.
    agent.poll_status().await?;
    loop {
        match pump(observer) {
            Pump::Delivery(dal_agent::Delivery::Update(update)) => updates.push(update),
            Pump::Delivery(dal_agent::Delivery::Resync { .. }) => {
                return Err("the observer was resynchronised after the turn".into());
            }
            Pump::Empty => break,
            Pump::Ended => return Err("the observer ended after the turn".into()),
        }
    }
    let head = view_cursor(agent)?;
    if head.0 != cursor.0 {
        return Err("the session generation changed during a flood turn".into());
    }
    let published = u64::try_from(updates.len())?;
    if head.1.get() != cursor.1.get() + published {
        return Err(format!(
            "the view moved from seq {} to {} but the observer received {published} updates",
            cursor.1.get(),
            head.1.get()
        )
        .into());
    }
    for (update, want) in updates.iter().zip(cursor.1.get() + 1..) {
        if update.r#gen != cursor.0 || update.seq.get() != want {
            return Err(format!(
                "the observer received seq {} where {want} was next: the stream is not contiguous",
                update.seq.get()
            )
            .into());
        }
    }
    if seen_deltas != deltas {
        return Err(format!(
            "{deltas} text deltas became {seen_deltas} delta updates: the flood no longer maps \
             one delta to one update"
        )
        .into());
    }
    let mut bytes = 0_usize;
    for update in &updates {
        let len = sonic_rs::to_vec(&**update)
            .map_err(|error| format!("a published update does not serialise: {error}"))?
            .len();
        bytes += len;
    }
    Ok(Flood {
        cursor,
        head,
        updates,
        bytes,
    })
}

/// How many updates one flood turn publishes besides its text deltas. The
/// first turn of a session is a warm-up and discarded; the next two must
/// agree, or the window position of a case could not be set exactly.
async fn flood_overhead(
    agent: &dal_agent::Agent,
    gate: &tokio::sync::Semaphore,
    observer: &mut dal_agent::Subscription,
) -> support::TestResult<usize> {
    run_flood(agent, gate, observer, 1).await?;
    let first = run_flood(agent, gate, observer, 1).await?.updates.len() - 1;
    let second = run_flood(agent, gate, observer, 1).await?.updates.len() - 1;
    if first != second {
        return Err(format!(
            "a flood turn publishes {first} then {second} updates besides its deltas: the case \
             sizes cannot be set exactly"
        )
        .into());
    }
    Ok(first)
}

/// Requires `deliveries` to be exactly the updates the flood published, in
/// order and equal in content.
fn expect_exact_updates(
    label: &str,
    flood: &Flood,
    deliveries: &[dal_agent::Delivery],
) -> support::TestResult<()> {
    if deliveries.len() != flood.updates.len() {
        return Err(format!(
            "{label}: {} deliveries for {} published updates; first {:?}, last {:?}",
            deliveries.len(),
            flood.updates.len(),
            deliveries.first(),
            deliveries.last()
        )
        .into());
    }
    for (index, (delivery, published)) in deliveries.iter().zip(&flood.updates).enumerate() {
        let dal_agent::Delivery::Update(update) = delivery else {
            return Err(
                format!("{label}: delivery {index} is {delivery:?}, not {published:?}").into(),
            );
        };
        if **update != **published {
            return Err(format!(
                "{label}: delivery {index} is {update:?}, published was {published:?}"
            )
            .into());
        }
    }
    Ok(())
}

/// Requires `deliveries` to be exactly one `Resync` at the head the flood
/// left, with no update before it.
fn expect_single_resync_at_head(
    label: &str,
    flood: &Flood,
    deliveries: &[dal_agent::Delivery],
) -> support::TestResult<()> {
    let [dal_agent::Delivery::Resync { generation, seq }] = deliveries else {
        return Err(format!(
            "{label}: a cursor {} updates behind the head must open with exactly one Resync and \
             nothing else, got {} deliveries, first {:?}",
            flood.updates.len(),
            deliveries.len(),
            deliveries.first()
        )
        .into());
    };
    if *generation != flood.head.0 || *seq != flood.head.1 {
        return Err(format!(
            "{label}: the Resync names ({generation:?}, {seq:?}), the view's head is {:?}",
            flood.head
        )
        .into());
    }
    Ok(())
}

/// Recovers a client after a count-eviction `Resync` the way the public
/// contract directs: refetch the view, resubscribe at its cursor, follow a
/// real turn contiguously.
async fn recover_after_resync(
    label: &str,
    agent: &dal_agent::Agent,
    gate: &tokio::sync::Semaphore,
    observer: &mut dal_agent::Subscription,
    stale: &Flood,
) -> support::TestResult<()> {
    let view = agent.view(dal_core::PageReq::default())?;
    let resumed_at = (view.r#gen, view.seq);
    if resumed_at != stale.head {
        return Err(format!(
            "{label}: the recovery view is at {resumed_at:?}, the Resync named {:?}",
            stale.head
        )
        .into());
    }
    if !matches!(view.turn, dal_core::TurnState::Idle) {
        return Err(format!("{label}: the recovery view is not idle: {:?}", view.turn).into());
    }
    let mut follower = agent.subscribe_listen(Some(resumed_at))?;
    let (backlog, ended) = drain_ready(&mut follower);
    if ended || !backlog.is_empty() {
        return Err(format!(
            "{label}: resubscribing at the recovered head must start live and quiet, got {} \
             deliveries, ended {ended}",
            backlog.len()
        )
        .into());
    }
    let follow = run_flood(agent, gate, observer, 3).await?;
    if follow.cursor != resumed_at {
        return Err(format!(
            "{label}: the follow-up turn started at {:?}, not at the recovered head {resumed_at:?}",
            follow.cursor
        )
        .into());
    }
    let (delivered, ended) = drain_ready(&mut follower);
    if ended {
        return Err(format!("{label}: the recovered subscription ended").into());
    }
    expect_exact_updates(label, &follow, &delivered)
}

/// Retained-window eviction, with no lagging subscriber anywhere: the live
/// observer receives every update contiguously throughout.
async fn replay_window_scenario(fixture: &ResyncFixture) -> support::TestResult<()> {
    let agent = fixture.open().await?;
    let mut observer = agent.subscribe_listen(None)?;
    let overhead = flood_overhead(&agent, &fixture.gate, &mut observer).await?;
    for (label, updates) in [
        ("N-1", REPLAY_WINDOW_UPDATES - 1),
        ("N", REPLAY_WINDOW_UPDATES),
        ("N+1", REPLAY_WINDOW_UPDATES + 1),
    ] {
        let deltas = updates.checked_sub(overhead).ok_or_else(|| {
            format!("{label}: the turn overhead {overhead} exceeds the case size {updates}")
        })?;
        let flood = run_flood(&agent, &fixture.gate, &mut observer, deltas).await?;
        if flood.updates.len() != updates {
            return Err(format!(
                "{label}: the turn published {} updates after its cursor, not the {updates} the \
                 case needs",
                flood.updates.len()
            )
            .into());
        }
        if flood.bytes >= REPLAY_WINDOW_BYTES {
            return Err(format!(
                "{label}: the flood serialised to {} bytes, at or above the {REPLAY_WINDOW_BYTES} \
                 byte cap, so the byte eviction could explain a resync instead of the count",
                flood.bytes
            )
            .into());
        }
        let mut resumed = agent.subscribe_listen(Some(flood.cursor))?;
        let (deliveries, ended) = drain_ready(&mut resumed);
        if ended {
            return Err(format!("{label}: the resumed subscription ended").into());
        }
        if updates <= REPLAY_WINDOW_UPDATES {
            // The cursor is still inside the retained window: every update
            // after it comes back, none lost, none replaced by a Resync.
            expect_exact_updates(label, &flood, &deliveries)?;
        } else {
            expect_single_resync_at_head(label, &flood, &deliveries)?;
            recover_after_resync(label, &agent, &fixture.gate, &mut observer, &flood).await?;
        }
    }
    Ok(())
}

#[test]
fn stale_cursor_after_count_eviction_resyncs_first_then_recovers_contiguously()
-> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let fixture = ResyncFixture::start("dalgona-resync-window").await?;
        let outcome = replay_window_scenario(&fixture).await;
        fixture.finish(outcome).await
    })
}

/// Requires the drain of a subscription that was held undrained across a
/// flood past the queue bound: the updates that were queued before the
/// overflow, exactly one `Resync` after them, then the end of the stream.
fn expect_overflow_resync(
    label: &str,
    flood: &Flood,
    deliveries: &[dal_agent::Delivery],
    ended: bool,
) -> support::TestResult<()> {
    if !ended {
        return Err(
            format!("{label}: the overflowed subscription did not end after its Resync").into(),
        );
    }
    let Some((dal_agent::Delivery::Resync { generation, seq }, queued)) = deliveries.split_last()
    else {
        return Err(format!(
            "{label}: an overflowed queue ended with no final Resync; deliveries {}, last {:?}",
            deliveries.len(),
            deliveries.last()
        )
        .into());
    };
    let mut last_seq = None;
    for delivery in queued {
        let dal_agent::Delivery::Update(update) = delivery else {
            return Err(format!("{label}: a second Resync before the end: {delivery:?}").into());
        };
        let index = update
            .seq
            .get()
            .checked_sub(flood.cursor.1.get() + 1)
            .and_then(|offset| usize::try_from(offset).ok())
            .filter(|offset| last_seq.is_none_or(|last| *offset > last));
        let Some(index) = index else {
            return Err(format!(
                "{label}: queued update seq {} is not strictly after the cursor and the update \
                 before it",
                update.seq.get()
            )
            .into());
        };
        if flood.updates.get(index).map(|published| &**published) != Some(&**update) {
            return Err(format!(
                "{label}: queued update seq {} is not what the session published",
                update.seq.get()
            )
            .into());
        }
        last_seq = Some(index);
    }
    if queued.len() >= flood.updates.len() {
        return Err(format!(
            "{label}: nothing was shed ({} queued of {} published) yet a Resync arrived",
            queued.len(),
            flood.updates.len()
        )
        .into());
    }
    let after_queued = last_seq.is_none_or(|last| {
        usize::try_from(seq.get().saturating_sub(flood.cursor.1.get() + 1))
            .is_ok_and(|offset| offset > last)
    });
    if *generation != flood.cursor.0 || !after_queued || *seq > flood.head.1 {
        return Err(format!(
            "{label}: the Resync ({generation:?}, {seq:?}) is not after the queued updates and \
             within the head {:?}",
            flood.head
        )
        .into());
    }
    Ok(())
}

/// Live queue overflow, with the replay ring far inside its window: a held
/// subscription is shed by its own queue bound while the ring keeps every
/// update.
async fn live_queue_scenario(fixture: &ResyncFixture) -> support::TestResult<()> {
    let agent = fixture.open().await?;
    let mut observer = agent.subscribe_listen(None)?;
    let overhead = flood_overhead(&agent, &fixture.gate, &mut observer).await?;
    for (label, updates) in [
        ("N-1", LIVE_QUEUE_UPDATES - 1),
        ("N", LIVE_QUEUE_UPDATES),
        ("N+1", LIVE_QUEUE_UPDATES + 1),
        ("2N", 2 * LIVE_QUEUE_UPDATES),
    ] {
        let deltas = updates.checked_sub(overhead).ok_or_else(|| {
            format!("{label}: the turn overhead {overhead} exceeds the case size {updates}")
        })?;
        let cursor = view_cursor(&agent)?;
        let mut held = agent.subscribe_listen(Some(cursor))?;
        let (preface, ended) = drain_ready(&mut held);
        if ended || !preface.is_empty() {
            return Err(format!(
                "{label}: a subscription at the view's cursor must start live and quiet, got {} \
                 deliveries, ended {ended}",
                preface.len()
            )
            .into());
        }
        // The turn runs while `held` is never drained; only the observer is.
        let flood = run_flood(&agent, &fixture.gate, &mut observer, deltas).await?;
        if flood.cursor != cursor || flood.updates.len() != updates {
            return Err(format!(
                "{label}: the turn published {} updates from {:?}, not the {updates} from \
                 {cursor:?} the case needs",
                flood.updates.len(),
                flood.cursor
            )
            .into());
        }
        let (deliveries, ended) = drain_ready(&mut held);
        if updates <= LIVE_QUEUE_UPDATES {
            if ended {
                return Err(format!(
                    "{label}: {updates} undrained updates fit the queue, yet the subscription ended"
                )
                .into());
            }
            expect_exact_updates(label, &flood, &deliveries)?;
        } else {
            expect_overflow_resync(label, &flood, &deliveries, ended)?;
        }
        // Overflow is the queue's loss, never the ring's: a new subscription
        // at the same cursor still replays every update, with no Resync.
        // It is drained at once, before anything else can publish into a
        // backlog longer than the queue bound.
        let mut replay = agent.subscribe_listen(Some(cursor))?;
        let (replayed, ended) = drain_ready(&mut replay);
        if ended {
            return Err(format!("{label}: the ring replay ended").into());
        }
        expect_exact_updates(label, &flood, &replayed)?;
    }
    Ok(())
}

#[test]
fn queue_overflow_with_retained_ring_resyncs_once_then_ends_and_resumes_from_ring()
-> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let fixture = ResyncFixture::start("dalgona-resync-queue").await?;
        let outcome = live_queue_scenario(&fixture).await;
        fixture.finish(outcome).await
    })
}
