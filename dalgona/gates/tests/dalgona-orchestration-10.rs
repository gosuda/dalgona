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
        premium_requests: None,
        cost_usd: Some(0.001),
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
            let id: String = rest
                .chars()
                .take_while(char::is_ascii_alphanumeric)
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
            args: format!(r#"{{"command":{}}}"#, support::json_text(&command)),
        }]);
    }
    Ok(text_stream("parked"))
}

/// One fast-batch call: `sleep 1` past the foreground floor, then the
/// normative half-second tail. Count-based across 200 rounds; wake notices
/// landing mid-turn never re-route it.
fn fast_response(context: &[ContextItem]) -> Result<EventStream, dal_core::RawJsonError> {
    let done = tool_results(context, "exec").len();
    if done >= FAST_PER_SESSION {
        return Ok(text_stream("noted"));
    }
    let command = if done.is_multiple_of(2) {
        format!(
            "sleep {FAST_SLEEP_S}; sleep {FAST_TAIL_S}; echo ok tok-{}",
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
    let held = tokio::task::spawn_blocking({
        let reports = reports.clone();
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
    eprintln!(
        "exec-stress census: {} PIDs in {} groups alive before cancel",
        held_alive.pids.len(),
        held_alive.groups.len()
    );

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
        support::wait_until_gone(&held_census, cancel_at, GONE_WITHIN)
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
    support::grant_bundled_extensions(data, workspace, user, GATE_CLIENT).await?;
    let mut product = support::build_product(data.to_path_buf(), Some(user))?;
    product.extensions.push(jobs_extension()?);
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
    // The last spawn: all 220 trees (200 background plus 20 parked) have
    // written their PID files.
    let reported = tokio::task::spawn_blocking({
        let reports = reports.clone();
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
    eprintln!(
        "cancel census: {} PIDs in {} groups alive before cancel",
        live.pids.len(),
        live.groups.len()
    );
    // 2 s after the last spawn, to the instant.
    let observed = Instant::now();
    let due = observed + CANCEL_DELAY;
    if due > Instant::now() {
        tokio::time::sleep(due - Instant::now()).await;
    }
    let cancel_at = Instant::now();
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
    // The run notice, delivered once, in the parent journal.
    let journal = wait_for_parent_cancel(&data, &session).await?;
    // Every child ledger: exactly 10 cancelled ends.
    for child in &children {
        let ends = wait_for_ledger_ends(&data, child, CANCEL_JOBS_PER_CHILD).await?;
        if ends.iter().any(|end| end.state != "cancelled") {
            return Err(format!("child {child} has a non-cancelled job end").into());
        }
    }
    // The parent ledger: exactly the run's own cancelled end.
    let parent_ends = wait_for_ledger_ends(&data, &session, 1).await?;
    if parent_ends.len() != 1 || parent_ends[0].state != "cancelled" {
        return Err("the parent ledger holds no single cancelled run end".into());
    }
    if !parent_ends[0].text.contains("cancelled after") {
        return Err("the parent run end carries no cancel summary".into());
    }
    eprintln!(
        "cancel ledgers: 200 child exec ends plus 1 run end, all cancelled; journal:\n{journal}"
    );
    // The OS table: nothing recorded survives past 3 s after the cancel.
    let gone_at = tokio::task::spawn_blocking(move || {
        support::wait_until_gone(&reported, cancel_at, GONE_WITHIN)
    })
    .await
    .map_err(|join| format!("census task failed: {join}"))??;
    if gone_at.duration_since(cancel_at) > GONE_WITHIN {
        return Err(format!(
            "process trees took {:?} to vanish, above {GONE_WITHIN:?}",
            gone_at.duration_since(cancel_at)
        )
        .into());
    }
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
    // takes no Resync under the 200-job delivery load.
    let stop = Arc::new(AtomicBool::new(false));
    let slow = {
        let mut subscription = agents[0].subscribe_listen(None)?;
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            let mut ledger = support::SubscriberLedger::default();
            loop {
                tokio::time::sleep(support::SUBSCRIBER_PERIOD).await;
                let mut count = 0;
                let mut resync = 0;
                let mut jobs: Vec<String> = Vec::new();
                let mut ended = false;
                loop {
                    match pump(&mut subscription) {
                        Pump::Delivery(dal_agent::Delivery::Update(update)) => {
                            count += 1;
                            if let dal_core::UpdateKind::JobStarted { job }
                            | dal_core::UpdateKind::JobSettled { job } = &update.kind
                            {
                                jobs.push(job.to_string());
                            }
                        }
                        Pump::Delivery(dal_agent::Delivery::Resync { .. }) => {
                            count += 1;
                            resync += 1;
                        }
                        Pump::Empty => break,
                        Pump::Ended => {
                            ended = true;
                            break;
                        }
                    }
                }
                if ended {
                    ledger.record_early_end();
                    break;
                }
                if count > 0 || stop.load(Ordering::Acquire) {
                    // The 200-job delivery load is job start/settle traffic;
                    // `jobs` holds exactly this drain's job updates.
                    ledger.record_drain(count, resync, &jobs, jobs.len(), Instant::now());
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
            }
            ledger
        })
    };
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
            if end.state != "exited" || end.code != Some(0) || !end.text.contains("ok") {
                return Err(format!("session {session} job {} is not a clean done", end.id).into());
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
