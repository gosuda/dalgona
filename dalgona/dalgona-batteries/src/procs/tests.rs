// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Boundary tests for the `procs` tool: validation, ordering, and caps
//! driven through a scripted jobs service. The scripted service answers
//! `jobs` list and text reads and fails every other service call, so a tool
//! that reached for a file or a process would fail these tests.

use std::fmt::Write as _;
use std::sync::Arc;

use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{Caller, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_core::ext::{JobStateView, JobsOp, JobsReply, StateError, StateOp, StateRecord};
use dal_core::{
    AgentsOp, AgentsReply, Answer, DenyReason, FetchRequest, FetchResponse, Inference, JobStatus,
    JobsError, ModelRequest, Notice, Question, RawJson, RunOutput, RunRequest, SidecarOp,
    Timestamp, TurnOp, TurnOpReply, Visibility,
};

use super::text::{escaped_prefix, format_age, list_line};
use super::tool::ProcsTool;

/// One scripted jobs service: `list` and `text` replies, plus a call log.
struct JobsHost {
    jobs: Vec<JobStatus>,
    texts: std::collections::HashMap<String, String>,
    list_calls: std::sync::atomic::AtomicUsize,
    text_calls: std::sync::atomic::AtomicUsize,
    denied: bool,
}

impl JobsHost {
    fn new(jobs: Vec<JobStatus>, texts: &[(&str, &str)]) -> Arc<Self> {
        let mut map = std::collections::HashMap::new();
        for (id, text) in texts {
            map.insert((*id).to_owned(), (*text).to_owned());
        }
        Arc::new(Self {
            jobs,
            texts: map,
            list_calls: std::sync::atomic::AtomicUsize::new(0),
            text_calls: std::sync::atomic::AtomicUsize::new(0),
            denied: false,
        })
    }

    fn denied() -> Arc<Self> {
        Arc::new(Self {
            jobs: Vec::new(),
            texts: std::collections::HashMap::new(),
            list_calls: std::sync::atomic::AtomicUsize::new(0),
            text_calls: std::sync::atomic::AtomicUsize::new(0),
            denied: true,
        })
    }
}

fn unavailable<T: Send + 'static>() -> ServiceFuture<'static, T> {
    Box::pin(async { Err(ServiceError::failed(None, "unused in procs tests")) })
}

/// The wall clock the scripted job ages are measured against.
fn now() -> Timestamp {
    Timestamp::now()
}

impl dal_agent::ext::Services for JobsHost {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unavailable()
    }
    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unavailable()
    }
    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unavailable()
    }
    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unavailable()
    }
    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unavailable()
    }
    fn mcp(
        &self,
        _who: &Caller,
        _req: dal_core::ext::McpRequest,
    ) -> ServiceFuture<'_, dal_core::ext::McpResponse> {
        unavailable()
    }
    fn mcp_declarations(
        &self,
        _who: &Caller,
    ) -> ServiceFuture<'_, Vec<dal_core::ext::McpDeclaration>> {
        unavailable()
    }
    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn dal_agent::ext::Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unavailable()
    }
    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unavailable()
    }
    fn jobs(&self, _who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        if self.denied {
            return Box::pin(async { Err(ServiceError::Denied(DenyReason::NotGranted)) });
        }
        match op {
            JobsOp::List => {
                self.list_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let jobs = self.jobs.clone();
                Box::pin(async move { Ok(JobsReply::Listed(jobs)) })
            }
            JobsOp::Text { id } => {
                self.text_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let text = self.texts.get(&id.to_string()).cloned();
                Box::pin(async move {
                    Ok(match text {
                        Some(text) => JobsReply::Text {
                            id,
                            text: text.into_boxed_str(),
                        },
                        None => JobsReply::Text {
                            id,
                            text: String::new().into_boxed_str(),
                        },
                    })
                })
            }
            _ => Box::pin(async {
                Ok(JobsReply::Refused(JobsError::Unknown {
                    id: dal_core::JobId::new_v7(),
                }))
            }),
        }
    }
    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unavailable()
    }
    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        Box::pin(async { Ok(None) })
    }
    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unavailable()
    }
    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unavailable()
    }
    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
    fn model_ready(
        &self,
        _who: &Caller,
        _model: dal_core::ModelRoute,
    ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
        unavailable()
    }
    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unavailable()
    }
    fn infer_stream(
        &self,
        _who: &Caller,
        _req: ModelRequest,
    ) -> ServiceFuture<'_, dal_agent::ext::EventStream> {
        unavailable()
    }
    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawJson>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unavailable()
    }
    fn notify(&self, _who: &Caller, _notice: Notice) {}
    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawJson>,
    ) -> ServiceFuture<'_, dal_core::EntryId> {
        unavailable()
    }
    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawJson>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unavailable()
    }
    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
}

/// Parses `raw` into one `JobStatus` with the given id, label, and age.
fn job(id: &str, label: &str, state: JobStateView, age_secs: i64) -> JobStatus {
    JobStatus {
        id: dal_core::JobId::parse(id).expect("test id must parse"),
        label: label.into(),
        state,
        log: None,
        last_activity_at: now() - jiff::SignedDuration::from_secs(age_secs),
    }
}

/// Runs one `procs` call and renders its outcome text.
async fn run(host: &Arc<JobsHost>, args: &str) -> String {
    let tool = ProcsTool::new().expect("fixed spec");
    let services: Arc<dyn dal_agent::ext::Services> = host.clone();
    let call = ToolCall::new("call", RawJson::parse(args).expect("args json"));
    match tool.run(call, ToolCx::for_test(services)).await {
        ToolOutcome::Ok(output) => output.to_string(),
        ToolOutcome::Err(error) => error.to_string(),
        ToolOutcome::Interrupted => "interrupted".to_owned(),
        ToolOutcome::Detached(_) => "detached".to_owned(),
    }
}

const UUID_A: &str = "01980000-0000-7000-8000-00000000000a";
const UUID_B: &str = "01980000-0000-7000-8000-00000000000b";
const UUID_C: &str = "01980000-0000-7000-8000-00000000000c";
const UUID_D: &str = "01980000-0000-7000-8000-00000000000d";

#[tokio::test]
async fn list_orders_live_oldest_first_then_ten_terminal_newest_first() {
    // Two live jobs (ages 300s and 5s; service order is start order, so the
    // 300s one starts first) and twelve terminal jobs in settlement order.
    let mut jobs = vec![
        job(UUID_A, "watch", JobStateView::Running, 300),
        job(UUID_B, "build", JobStateView::Detached, 5),
    ];
    for index in 0..12 {
        let id = format!("01980000-0000-7000-8000-{index:012x}");
        jobs.push(job(
            &id,
            &format!("done-{index}"),
            JobStateView::Done(dal_core::JobOutcome::Exited { code: 0 }),
            60 + i64::from(index),
        ));
    }
    let host = JobsHost::new(jobs, &[]);
    let text = run(&host, r#"{"op":"list"}"#).await;
    let lines: Vec<&str> = text.lines().collect();
    // 2 live + 10 terminal = 12 rows.
    assert_eq!(lines.len(), 12, "{text}");
    // Live oldest first.
    assert!(lines[0].starts_with(UUID_A), "{text}");
    assert!(lines[1].starts_with(UUID_B), "{text}");
    // Terminal newest first: the newest terminal job (index 11) leads.
    assert!(lines[2].contains("done-11"), "{text}");
    assert!(lines[11].contains("done-2"), "{text}");
    // Exact row shape. The age is relative to the wall clock, so its exact
    // text is pinned by `list_lines_use_the_jobs_duration_format`.
    assert!(
        lines[0].starts_with(&format!("{UUID_A} running ")),
        "{text}"
    );
    assert!(lines[0].ends_with(" watch: watch"), "{text}");
    assert!(
        lines[1].starts_with(&format!("{UUID_B} running ")),
        "{text}"
    );
    assert!(lines[1].ends_with(" build: build"), "{text}");
}

#[tokio::test]
async fn list_filters_by_query_substring_and_status() {
    let jobs = vec![
        job(UUID_A, "cargo build", JobStateView::Running, 10),
        job(UUID_B, "cargo test", JobStateView::Running, 20),
        job(
            UUID_C,
            "cargo build",
            JobStateView::Done(dal_core::JobOutcome::Failed {
                message: "boom".into(),
            }),
            30,
        ),
    ];
    let host = JobsHost::new(jobs, &[]);
    let text = run(&host, r#"{"op":"list","query":"build"}"#).await;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with(UUID_A), "{text}");
    assert!(lines[1].starts_with(UUID_C), "{text}");

    let text = run(&host, r#"{"op":"list","status":"failed"}"#).await;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].starts_with(UUID_C), "{text}");
    assert!(lines[0].contains(" failed "), "{text}");

    let text = run(&host, r#"{"op":"list","status":"done"}"#).await;
    assert_eq!(text, "procs: no match for \"done\" in 3 job(s).", "{text}");
}

#[tokio::test]
async fn list_respects_limit_and_the_empty_form() {
    let jobs = vec![
        job(UUID_A, "one", JobStateView::Running, 10),
        job(
            UUID_B,
            "two",
            JobStateView::Done(dal_core::JobOutcome::Exited { code: 0 }),
            20,
        ),
    ];
    let host = JobsHost::new(jobs, &[]);
    let text = run(&host, r#"{"op":"list","limit":1}"#).await;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].starts_with(UUID_A), "{text}");

    let empty = JobsHost::new(Vec::new(), &[]);
    let text = run(&empty, r#"{"op":"list"}"#).await;
    assert_eq!(text, "procs: no jobs in this session.", "{text}");
}

#[tokio::test]
async fn grep_follows_the_service_order_then_line_number() {
    // The jobs service lists live jobs oldest first: B started before A, so
    // B's matches print first even though A has the earlier uuid and more
    // matching lines.
    let jobs = vec![
        job(UUID_B, "older", JobStateView::Running, 100),
        job(UUID_A, "newer", JobStateView::Running, 1),
    ];
    let host = JobsHost::new(
        jobs,
        &[
            (UUID_A, "hit one\nnoise\nhit two"),
            (UUID_B, "no\nthird hit"),
        ],
    );
    let text = run(&host, r#"{"op":"grep","query":"hit"}"#).await;
    let expected = format!("{UUID_B}:2: third hit\n{UUID_A}:1: hit one\n{UUID_A}:3: hit two");
    assert_eq!(text, expected, "{text}");
    assert_eq!(host.text_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn grep_caps_matches_and_appends_the_elision_tail() {
    let jobs = vec![job(UUID_A, "chatty", JobStateView::Running, 1)];
    let body = (0..30)
        .map(|i| format!("match {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let host = JobsHost::new(jobs, &[(UUID_A, &body)]);
    let text = run(&host, r#"{"op":"grep","query":"match","max_matches":3}"#).await;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(
        lines[3], "(27 more matches elided; narrow query or raise max_matches.)",
        "{text}"
    );
}

#[tokio::test]
async fn grep_never_exceeds_the_byte_cap_and_counts_every_elided_match() {
    let jobs = vec![job(UUID_A, "wide", JobStateView::Running, 1)];
    let body = (0..60)
        .map(|i| format!("{i:0700}"))
        .collect::<Vec<_>>()
        .join("\n");
    let host = JobsHost::new(jobs, &[(UUID_A, &body)]);
    let text = run(
        &host,
        r#"{"op":"grep","query":"^[0-9]+$","max_matches":200,"match_bytes":700}"#,
    )
    .await;
    let all: Vec<String> = (0..60)
        .map(|i| format!("{UUID_A}:{}: {i:0700}", i + 1))
        .collect();
    // Independent oracle: the longest prefix whose join plus its tail fits.
    let candidate = |shown: usize| {
        let mut text = all[..shown].join("\n");
        if shown < all.len() {
            let _ = write!(
                text,
                "\n({} more matches elided; narrow query or raise max_matches.)",
                all.len() - shown
            );
        }
        text
    };
    let expected = (1..=all.len())
        .rev()
        .map(candidate)
        .find(|text| text.len() <= 16_384)
        .expect("one line always fits");
    assert_eq!(text, expected);
    // The spec caps the whole result, tail included, at 16384 bytes.
    assert!(text.len() <= 16_384, "{}", text.len());
}

#[tokio::test]
async fn the_elision_tail_counts_matches_across_every_job() {
    // B (older) and A each hold five matches; three are shown, seven elided,
    // and both jobs' text is still read once so the count is exact.
    let jobs = vec![
        job(UUID_B, "older", JobStateView::Running, 100),
        job(UUID_A, "newer", JobStateView::Running, 1),
    ];
    let body = "hit\nhit\nhit\nhit\nhit";
    let host = JobsHost::new(jobs, &[(UUID_A, body), (UUID_B, body)]);
    let text = run(&host, r#"{"op":"grep","query":"hit","max_matches":3}"#).await;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert!(lines[0].starts_with(UUID_B), "{text}");
    assert_eq!(
        lines[3],
        "(7 more matches elided; narrow query or raise max_matches.)"
    );
    assert_eq!(host.text_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn grep_honors_match_bytes_on_the_line_prefix() {
    let jobs = vec![job(UUID_A, "bytes", JobStateView::Running, 1)];
    let host = JobsHost::new(jobs, &[(UUID_A, "0123456789")]);
    let text = run(
        &host,
        r#"{"op":"grep","query":"0123456789","match_bytes":4}"#,
    )
    .await;
    assert_eq!(text, format!("{UUID_A}:1: 0123"), "{text}");
}

#[tokio::test]
async fn grep_reports_the_exact_no_match_and_regex_errors() {
    let jobs = vec![job(UUID_A, "quiet", JobStateView::Running, 1)];
    let host = JobsHost::new(jobs, &[(UUID_A, "nothing here")]);
    let text = run(&host, r#"{"op":"grep","query":"absent"}"#).await;
    assert_eq!(
        text, "procs: no match for \"absent\" in 1 job(s).",
        "{text}"
    );

    let text = run(&host, r#"{"op":"grep","query":"(unclosed"}"#).await;
    assert!(
        text.starts_with("procs: query is not a valid regex: "),
        "{text}"
    );

    let text = run(&host, r#"{"op":"grep","query":"(?=look)ahead"}"#).await;
    assert!(
        text.starts_with("procs: query is not a valid regex: "),
        "{text}"
    );
}

#[tokio::test]
async fn grep_rejects_a_missing_query() {
    let jobs = vec![job(UUID_A, "x", JobStateView::Running, 1)];
    let host = JobsHost::new(jobs, &[]);
    let text = run(&host, r#"{"op":"grep"}"#).await;
    assert_eq!(text, "procs: invalid input: grep needs query.", "{text}");
}

#[tokio::test]
async fn the_jobs_denial_names_the_missing_grant() {
    let host = JobsHost::denied();
    let text = run(&host, r#"{"op":"list"}"#).await;
    assert_eq!(
        text,
        "procs: the jobs service is not granted to this battery: denied: the extension's services are not granted.",
        "{text}"
    );
}

#[tokio::test]
async fn strict_decode_rejects_unknown_fields_and_cross_op_fields() {
    let jobs = vec![job(UUID_A, "x", JobStateView::Running, 1)];
    let host = JobsHost::new(jobs, &[]);

    let text = run(&host, r#"{"op":"list","typo":1}"#).await;
    assert!(
        text.starts_with("procs: invalid input: unknown field"),
        "{text}"
    );

    let text = run(&host, r#"{"op":"list","max_matches":5}"#).await;
    assert_eq!(
        text, "procs: invalid input: max_matches and match_bytes apply to grep only.",
        "{text}"
    );

    let text = run(&host, r#"{"op":"grep","query":".","limit":5}"#).await;
    assert_eq!(
        text, "procs: invalid input: limit applies to list only.",
        "{text}"
    );

    let text = run(&host, r#"{"op":"grep","query":".","status":"done"}"#).await;
    assert_eq!(
        text, "procs: invalid input: status applies to list only.",
        "{text}"
    );

    let text = run(&host, r#"{"op":"ping"}"#).await;
    assert_eq!(
        text, "procs: invalid input: op must be list or grep, not ping.",
        "{text}"
    );
}

#[tokio::test]
async fn the_extension_registers_one_deferred_tool() -> Result<(), Box<dyn std::error::Error>> {
    let extension = super::procs()?;
    assert_eq!(extension.origin(), dal_core::Origin::Bundled);
    let tools = extension.tools();
    assert_eq!(tools.len(), 1, "one tool");
    let (tool, visibility) = &tools[0];
    assert_eq!(tool.name().as_str(), "procs");
    assert!(matches!(visibility, Visibility::Deferred), "{visibility:?}");
    Ok(())
}

#[test]
fn ages_format_like_the_jobs_service() {
    assert_eq!(format_age(5), "5.0s");
    assert_eq!(format_age(48), "48.0s");
    assert_eq!(format_age(59), "59.0s");
    assert_eq!(format_age(60), "1m00s");
    assert_eq!(format_age(252), "4m12s");
    assert_eq!(format_age(3_600), "1h00m00s");
    assert_eq!(format_age(3_725), "1h02m05s");
}

#[test]
fn escaping_keeps_bytes_and_names_controls() {
    assert_eq!(escaped_prefix("plain", 10), "plain");
    assert_eq!(escaped_prefix("a\tb", 10), "a\\tb");
    assert_eq!(escaped_prefix("a\u{1}b", 10), "a\\u{01}b");
    assert_eq!(escaped_prefix("0123456789", 4), "0123");
    // A multi-byte character is never split: a cut inside one backs up to the
    // previous boundary, and a cut on a boundary keeps the whole character.
    assert_eq!(escaped_prefix("ééé", 2), "é");
    assert_eq!(escaped_prefix("ééé", 3), "é");
    assert_eq!(escaped_prefix("ééé", 1), "");
    // DEL escapes too.
    assert_eq!(escaped_prefix("\u{7f}", 1), "\\u{7f}");
}

#[test]
fn list_lines_use_the_jobs_duration_format() {
    let at = Timestamp::from_second(1_000_000).expect("a valid timestamp");
    let mut row = job(UUID_A, "build", JobStateView::Running, 0);
    row.last_activity_at = at;
    let line = |seconds: i64| {
        list_line(
            &row,
            at + jiff::SignedDuration::from_secs(seconds),
            "cargo build",
        )
    };
    assert_eq!(line(5), format!("{UUID_A} running 5.0s build: cargo build"));
    assert_eq!(
        line(300),
        format!("{UUID_A} running 5m00s build: cargo build")
    );
    assert_eq!(
        line(3_725),
        format!("{UUID_A} running 1h02m05s build: cargo build")
    );
    // A clock behind the job's last activity reads as zero, never negative.
    assert_eq!(
        line(-30),
        format!("{UUID_A} running 0.0s build: cargo build")
    );
}

#[test]
fn the_list_command_is_cut_at_eighty_characters() {
    let at = Timestamp::from_second(1_000_000).expect("a valid timestamp");
    let mut row = job(UUID_A, "run", JobStateView::Running, 0);
    row.last_activity_at = at;
    let long = list_line(&row, at, &"x".repeat(200));
    // The spec cuts the command at 80 characters; the literal is the oracle.
    assert!(long.ends_with(&format!(": {}", "x".repeat(80))), "{long}");
    assert!(!long.contains(&"x".repeat(81)), "{long}");
    // The cut counts characters, so a multi-byte command is never split.
    let wide = list_line(&row, at, &"é".repeat(100));
    assert!(wide.ends_with(&format!(": {}", "é".repeat(80))), "{wide}");
    assert!(!wide.contains(&"é".repeat(81)), "{wide}");
}

#[tokio::test]
async fn a_nonzero_exit_is_failed_and_every_phase_filters_alone() {
    let exited = |code| JobStateView::Done(dal_core::JobOutcome::Exited { code });
    let jobs = vec![
        job(UUID_A, "ok", exited(0), 40),
        job(UUID_B, "bad", exited(2), 30),
        job(
            UUID_C,
            "stop",
            JobStateView::Done(dal_core::JobOutcome::Cancelled),
            20,
        ),
        job(
            UUID_D,
            "gone",
            JobStateView::Done(dal_core::JobOutcome::Lost),
            10,
        ),
    ];
    let host = JobsHost::new(jobs, &[]);
    let ids = |text: &str| -> Vec<String> {
        text.lines()
            .map(|line| line.split(' ').next().unwrap_or_default().to_owned())
            .collect()
    };
    let failed = run(&host, r#"{"op":"list","status":"failed"}"#).await;
    assert_eq!(ids(&failed), [UUID_D, UUID_B], "{failed}");
    let done = run(&host, r#"{"op":"list","status":"done"}"#).await;
    assert_eq!(ids(&done), [UUID_A], "{done}");
    let cancelled = run(&host, r#"{"op":"list","status":"cancelled"}"#).await;
    assert_eq!(ids(&cancelled), [UUID_C], "{cancelled}");
    let running = run(&host, r#"{"op":"list","status":"running"}"#).await;
    assert_eq!(
        running, "procs: no match for \"running\" in 4 job(s).",
        "{running}"
    );
}

#[tokio::test]
async fn out_of_range_numbers_are_rejected_and_the_bounds_accepted() {
    let host = JobsHost::new(
        vec![job(UUID_A, "x", JobStateView::Running, 1)],
        &[(UUID_A, "x")],
    );
    let cases = [
        (r#"{"op":"list","limit":0}"#, "limit", 1, 50),
        (r#"{"op":"list","limit":51}"#, "limit", 1, 50),
        (r#"{"op":"list","limit":"5"}"#, "limit", 1, 50),
        (r#"{"op":"list","limit":1.5}"#, "limit", 1, 50),
        (
            r#"{"op":"grep","query":"x","max_matches":0}"#,
            "max_matches",
            1,
            200,
        ),
        (
            r#"{"op":"grep","query":"x","max_matches":201}"#,
            "max_matches",
            1,
            200,
        ),
        (
            r#"{"op":"grep","query":"x","match_bytes":0}"#,
            "match_bytes",
            1,
            2048,
        ),
        (
            r#"{"op":"grep","query":"x","match_bytes":2049}"#,
            "match_bytes",
            1,
            2048,
        ),
    ];
    for (args, field, min, max) in cases {
        let text = run(&host, args).await;
        assert_eq!(
            text,
            format!("procs: invalid input: {field} must be an integer from {min} to {max}."),
            "{args}"
        );
    }
    for args in [
        r#"{"op":"list","limit":1}"#,
        r#"{"op":"list","limit":50}"#,
        r#"{"op":"grep","query":"x","max_matches":1}"#,
        r#"{"op":"grep","query":"x","max_matches":200}"#,
        r#"{"op":"grep","query":"x","match_bytes":1}"#,
        r#"{"op":"grep","query":"x","match_bytes":2048}"#,
    ] {
        let text = run(&host, args).await;
        assert!(!text.starts_with("procs: invalid input"), "{args}: {text}");
    }
}

#[tokio::test]
async fn a_bad_pattern_names_the_reason_not_just_the_pattern() {
    let host = JobsHost::new(
        vec![job(UUID_A, "x", JobStateView::Running, 1)],
        &[(UUID_A, "x")],
    );
    let look = run(&host, r#"{"op":"grep","query":"(?=look)ahead"}"#).await;
    assert_eq!(
        look,
        "procs: query is not a valid regex: look-around, including look-ahead and look-behind, is not supported.",
        "{look}"
    );
    let open = run(&host, r#"{"op":"grep","query":"(unclosed"}"#).await;
    assert!(
        open.starts_with("procs: query is not a valid regex: "),
        "{open}"
    );
    assert!(open.contains("unclosed group"), "{open}");
    assert!(open.ends_with('.') && !open.contains('\n'), "{open}");
}

#[test]
fn the_battery_asks_for_the_jobs_service_only() -> Result<(), Box<dyn std::error::Error>> {
    // the jobs service is the only source; no file or process access.
    let extension = super::procs()?;
    assert_eq!(
        extension.inject(),
        dal_core::ServiceSet::from_names(["jobs"])?
    );
    Ok(())
}
