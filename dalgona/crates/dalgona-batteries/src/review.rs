// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The review battery: one reviewer round over the session's git changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub mod config;
pub mod git;
pub mod reply;
pub mod rounds;

#[cfg(test)]
mod tests;

pub use config::{ReviewConfig, ReviewConfigError};

use dal_agent::ext::{
    ArgError, BoxFuture, Extension, ExtensionBuilder, RawValue, StatusCx, StatusSnapshot, ToolCall,
    ToolCx, ToolOutcome, ToolOutput,
};
use dal_core::{
    CommandName, CommandSpec, Name, RawJson, RegistrationError, ServiceSet, SessionId, ToolClass,
    ToolSpec, Workspace,
};

const REVIEW_COMMAND_ARGS_HINT: &str = "[focus]";
const REVIEW_COMMAND_SUMMARY: &str = "Review the current changes.";

pub(crate) const REVIEW_SYSTEM: &str = "You are a code reviewer. You review the diff of one coding session. You report defects only: bugs, broken behavior, security problems, and missed requirements. You do not report style preferences. You answer with one JSON object and no other text.";
pub(crate) const MAX_DIFF_BYTES: usize = 262_144;
pub(crate) const MAX_FINDINGS: usize = 50;
pub(crate) const DIFF_TRUNCATION_MARKER: &str = "\n[... diff truncated at 262144 bytes ...]";
pub(crate) const FOCUS_TRUNCATION_MARKER: &str = "[... focus truncated]";
pub(crate) const MAX_ERROR_BYTES: usize = 200;
pub(crate) const MAX_STORED_DETAIL_BYTES: usize = 500;
pub(crate) const REVIEW_REPLY_FORMAT: &str = "One JSON object: {\"verdict\":\"clean\"|\"findings\",\"summary\":\"...\",\"findings\":[{\"path\":\"...\",\"line\":null,\"severity\":\"critical\"|\"major\"|\"minor\",\"title\":\"...\",\"detail\":\"...\"}]}";
pub(crate) const GIT_STDOUT_PREFIX_LIMIT: u32 = 262_145;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum ReviewError {
    #[error("review needs a git repository: {stderr}")]
    NoGit { stderr: Box<str> },
    #[error("reviewer call failed: {cause}")]
    Provider { cause: Box<str> },
    #[error("reviewer reply did not parse at round {round}: {reason}")]
    Parse { round: u8, reason: Box<str> },
    #[error("reviewer returned {count} findings; the cap is 50; resolve the reported ones first")]
    TooManyFindings { count: usize },
    #[error(
        "review session reached the cap of {rounds} rounds with these findings still open:\n{outstanding}\nStop and tell the user. Ask whether to start a new review session; the user can run /review for that. Call review with restart set to true only when the user asks for a new session."
    )]
    CapReached { rounds: u8, outstanding: Box<str> },
    #[error("workspace status exceeds {limit} bytes; commit or stash unrelated changes")]
    StatusTooLarge { limit: usize },
}

pub(crate) fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// The text of the `dalgona://review` manual page.
pub const REVIEW_DOC: &str = concat!(
    "# review\n\n",
    "The review battery reviews one session's changes with one reviewer\n",
    "completion per round. It reads only `git status --short` and\n",
    "`git diff` against the configured base inside the session workspace;\n",
    "it never writes or runs project code.\n\n",
    "Call the `review` tool with an optional `focus`, or run `/review [focus]`\n",
    "to hand the focus to the model as the next prompt. With no changes the\n",
    "review reports `No changes to review.` without calling the reviewer.\n\n",
    "Each round appends one durable review record. A round converges when the\n",
    "verdict is clean or no finding is new; otherwise the report lists every\n",
    "finding marked new or repeat and asks for the new findings. After\n",
    "`max_rounds` (1 to 10, default 3) non-converged rounds the review session\n",
    "reaches its cap and stops. The review then reports the findings still open\n",
    "and asks the user what to do; the model never starts a new session on its\n",
    "own. Running `/review` after the cap is the request for a new session: the\n",
    "command asks the model to call `review` with `restart` set to true.\n",
    "`restart` only takes effect at the cap; mid-session it continues the open\n",
    "rounds. The reviewer\n",
    "model is `reviewer_model` (empty selects the session model); the diff base\n",
    "is `diff_base` (empty selects `HEAD`).\n",
);

struct ReviewTool {
    cfg: ReviewConfig,
    status: Arc<ReviewStatus>,
    name: Name,
    spec: Arc<ToolSpec>,
}

impl ReviewTool {
    fn new(cfg: ReviewConfig, status: Arc<ReviewStatus>) -> Result<Self, RegistrationError> {
        let name = Name::parse("review")?;
        let parameters = RawJson::parse(concat!(
            r#"{"type":"object","properties":{"focus":{"type":"string","#,
            r#""description":"What the reviewer should look at first."},"#,
            r#""restart":{"type":"boolean","#,
            r#""description":"Start a new review session after the round cap. Set it only when the user asked for a new session."}},"#,
            r#""additionalProperties":false}"#,
        ))
        .map_err(|_| RegistrationError::InvalidParameters)?;
        if !dal_core::valid_tool_parameters(&parameters) {
            return Err(RegistrationError::InvalidParameters);
        }
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Review the current changes with one reviewer round.".into(),
            parameters,
            grammar: None,
        });
        Ok(Self {
            cfg,
            status,
            name,
            spec,
        })
    }
}

impl dal_agent::ext::Tool for ReviewTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let args = match sonic_rs::from_str::<ReviewArgs>(call.args.as_str()) {
                Ok(args) => args,
                Err(error) => {
                    return ToolOutcome::Err(dal_agent::ToolError::message(format!(
                        "review: invalid input: {error}."
                    )));
                }
            };
            review_round(
                &self.cfg,
                &self.status,
                &cx,
                args.focus.as_deref(),
                args.request(),
            )
            .await
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewArgs {
    focus: Option<String>,
    #[serde(default)]
    restart: bool,
}

impl ReviewArgs {
    fn request(&self) -> rounds::RoundRequest {
        if self.restart {
            rounds::RoundRequest::Restart
        } else {
            rounds::RoundRequest::Continue
        }
    }
}

struct ReviewStatus {
    max_rounds: u8,
    running: Mutex<HashMap<SessionId, Vec<u8>>>,
}

impl ReviewStatus {
    #[must_use]
    fn new(max_rounds: u8) -> Self {
        Self {
            max_rounds,
            running: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, Vec<u8>>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn running(self: &Arc<Self>, session: SessionId, round: u8) -> RunningGuard {
        self.lock().entry(session).or_default().push(round);
        RunningGuard {
            status: Arc::clone(self),
            session,
            round,
        }
    }

    fn payload(&self, session: SessionId) -> Option<String> {
        #[derive(serde::Serialize)]
        struct Payload {
            state: &'static str,
            round: u8,
            max_rounds: u8,
        }
        let round = *self.lock().get(&session)?.last()?;
        sonic_rs::to_string(&Payload {
            state: "running",
            round,
            max_rounds: self.max_rounds,
        })
        .ok()
    }
}

#[must_use]
struct RunningGuard {
    status: Arc<ReviewStatus>,
    session: SessionId,
    round: u8,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut running = self.status.lock();
        let remove = if let Some(rounds) = running.get_mut(&self.session) {
            if let Some(index) = rounds.iter().rposition(|round| *round == self.round) {
                rounds.remove(index);
            }
            rounds.is_empty()
        } else {
            false
        };
        if remove {
            running.remove(&self.session);
        }
    }
}

impl dal_agent::ext::StatusPoll for ReviewStatus {
    fn snapshot(&self, cx: &StatusCx) -> StatusSnapshot {
        StatusSnapshot {
            quiet: true,
            text: self.payload(cx.session).map(String::into_boxed_str),
        }
    }
}

struct ReviewCommand;

impl dal_agent::ext::CommandHandler for ReviewCommand {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: dal_agent::ext::CommandCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, Result<dal_core::Reply, dal_agent::error::ServiceError>>
    {
        Box::pin(async move {
            let prompt = reply::command_prompt(args.trim());
            let content = vec![dal_core::Part::Text {
                text: prompt.into(),
            }];
            cx.submit_wait(dal_core::Command::Prompt {
                expect: dal_core::Expect::Idle,
                content,
            })
            .await
            .map_err(|error| dal_agent::error::ServiceError::failed(None, error.to_string()))?;
            Ok(dal_core::Reply::Done(dal_core::Output::Nothing))
        })
    }
}

async fn review_round(
    cfg: &ReviewConfig,
    status: &Arc<ReviewStatus>,
    cx: &ToolCx<'_>,
    focus: Option<&str>,
    request: rounds::RoundRequest,
) -> ToolOutcome {
    let services = cx.services();
    let caller = cx.caller().clone();
    let raw_records = match services.records(&caller, "review").await {
        Ok(records) => records,
        Err(error) => return service_outcome(error),
    };
    let records: Vec<rounds::ReviewRecord> = raw_records
        .iter()
        .filter_map(|record| sonic_rs::from_str(record.as_str()).ok())
        .collect();
    let work_round = match rounds::next_round(&records, cfg.max_rounds, request) {
        Ok(work_round) => work_round,
        Err(error) => return ToolOutcome::Err(dal_agent::ToolError::message(error.to_string())),
    };
    let _running = status.running(cx.session(), work_round.round);
    let workspace = cx.workspace();
    let diff_request = git::git_run_request(workspace, git::diff_argv(&cfg.diff_base));
    let diff = match git_capture(&services, &caller, diff_request).await {
        Ok(diff) => diff,
        Err(outcome) => return *outcome,
    };
    let status_request = git::git_run_request(workspace, git::status_argv());
    let git_status = match git_capture(&services, &caller, status_request).await {
        Ok(git_status) => git_status,
        Err(outcome) => return *outcome,
    };
    if git::status_exceeds_limit(git_status.text.as_bytes(), git_status.overflowed) {
        return ToolOutcome::Err(dal_agent::ToolError::message(
            ReviewError::StatusTooLarge {
                limit: MAX_DIFF_BYTES,
            }
            .to_string(),
        ));
    }
    let (diff_text, diff_truncated) = git::cap_diff(&diff.text, diff.overflowed);
    if diff_text.is_empty() && git_status.text.trim().is_empty() {
        return ToolOutcome::Ok(ToolOutput::from_text("No changes to review."));
    }
    let earlier = rounds::earlier_identities(&records, work_round.session);
    let content = reply::review_content(
        &diff_text,
        git_status.text.trim_end(),
        focus,
        &rounds::prior_findings(&records, work_round.session),
    );
    let reviewer_reply = match reviewer_completion(cfg, &services, &caller, content).await {
        Ok(reviewer_reply) => reviewer_reply,
        Err(outcome) => return *outcome,
    };
    let reviewer_reply = match reply::parse_reply(&reviewer_reply, work_round.round)
        .and_then(reply::enforce_findings_cap)
    {
        Ok(reviewer_reply) => reviewer_reply,
        Err(error) => {
            return ToolOutcome::Err(dal_agent::ToolError::message(error.to_string()));
        }
    };
    let new = match rounds::new_count(&reviewer_reply, &earlier) {
        Ok(new) => new,
        Err(error) => return ToolOutcome::Err(dal_agent::ToolError::message(error.to_string())),
    };
    let record = rounds::ReviewRecord {
        session: work_round.session,
        round: work_round.round,
        verdict: reviewer_reply.verdict,
        new_count: new,
        findings: rounds::stored_findings(&reviewer_reply),
    };
    let record_body = match record_body(&record) {
        Ok(record_body) => record_body,
        Err(outcome) => return *outcome,
    };
    if let Err(error) = services
        .append_record(&caller, "review", Box::new(record_body))
        .await
    {
        return service_outcome(error);
    }
    match reply::settle_reply(
        work_round.round,
        cfg.max_rounds,
        &reviewer_reply,
        &earlier,
        new,
        diff_truncated,
    ) {
        Ok(text) => ToolOutcome::Ok(ToolOutput::from_text(text)),
        Err(error) => ToolOutcome::Err(dal_agent::ToolError::message(error.to_string())),
    }
}

fn record_body(record: &rounds::ReviewRecord) -> Result<RawJson, Box<ToolOutcome>> {
    let unserializable = || {
        Box::new(ToolOutcome::Err(dal_agent::ToolError::message(
            "review: the record did not serialize.".to_owned(),
        )))
    };
    let body = sonic_rs::to_string(record).map_err(|_| unserializable())?;
    RawJson::parse(&body).map_err(|_| unserializable())
}

struct GitCapture {
    text: String,
    overflowed: bool,
}

async fn git_capture(
    services: &Arc<dyn dal_agent::ext::Services>,
    caller: &dal_agent::ext::Caller,
    request: dal_core::RunRequest,
) -> Result<GitCapture, Box<ToolOutcome>> {
    let output = match services.run(caller, request).await {
        Ok(output) => output,
        Err(error) => return Err(Box::new(service_outcome(error))),
    };
    let failed = !matches!(output.status, dal_core::ExitStatusKind::Exited(0));
    if failed {
        let stderr = String::from_utf8_lossy(&output.stderr_tail).into_owned();
        return Err(Box::new(ToolOutcome::Err(dal_agent::ToolError::message(
            ReviewError::NoGit {
                stderr: reply::cap_error(&stderr),
            }
            .to_string(),
        ))));
    }
    Ok(GitCapture {
        overflowed: output.stdout_prefix_overflowed,
        text: String::from_utf8_lossy(&output.stdout_prefix).into_owned(),
    })
}

async fn reviewer_completion(
    cfg: &ReviewConfig,
    services: &Arc<dyn dal_agent::ext::Services>,
    caller: &dal_agent::ext::Caller,
    content: String,
) -> Result<String, Box<ToolOutcome>> {
    let request = dal_core::ModelRequest {
        purpose: dal_core::Purpose::Judge,
        model: dal_core::ModelRoute::from_id(&cfg.reviewer_model),
        system: Arc::from(REVIEW_SYSTEM),
        tools: Arc::from(Vec::<dal_core::ModelToolSpec>::new()),
        context: Arc::from(vec![dal_core::ContextItem::User {
            parts: vec![dal_core::Part::Text {
                text: content.into(),
            }],
        }]),
        params: dal_core::RequestParams::default(),
        cache_key: None,
    };
    let inference = match services.infer(caller, request).await {
        Ok(inference) => inference,
        Err(error) => return Err(Box::new(provider_outcome(error))),
    };
    let mut text = String::new();
    for event in inference.events {
        if let dal_core::StreamEvent::Delta {
            channel: dal_core::StreamChannel::Text,
            text: delta,
        } = event
        {
            text.push_str(&delta);
        }
    }
    Ok(text)
}

fn provider_outcome(error: dal_agent::error::ServiceError) -> ToolOutcome {
    match error {
        dal_agent::error::ServiceError::Denied(_) | dal_agent::error::ServiceError::Cancelled => {
            service_outcome(error)
        }
        error => ToolOutcome::Err(dal_agent::ToolError::message(
            ReviewError::Provider {
                cause: reply::cap_error(&error.to_string()),
            }
            .to_string(),
        )),
    }
}

fn service_outcome(error: dal_agent::error::ServiceError) -> ToolOutcome {
    match error {
        dal_agent::error::ServiceError::Denied(reason) => {
            ToolOutcome::Err(dal_agent::ToolError::Denied(reason))
        }
        dal_agent::error::ServiceError::Cancelled => ToolOutcome::Interrupted,
        error => ToolOutcome::Err(dal_agent::ToolError::Failed(Box::new(error))),
    }
}

/// Builds the review extension: the model tool, the `/review` command, and
/// the one `review` status kind. Registration performs no I/O.
///
/// # Errors
/// Returns [`RegistrationError`] when the fixed identity or declarations
/// are rejected.
pub fn review(cfg: ReviewConfig) -> Result<Extension, RegistrationError> {
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let tool = ReviewTool::new(cfg, Arc::clone(&status))?;
    ExtensionBuilder::new(
        "review",
        env!("CARGO_PKG_VERSION"),
        ServiceSet::from_names(["run", "infer"])?,
    )?
    .with_origin(dal_core::Origin::Bundled, None)
    .tool(Arc::new(tool), dal_core::Visibility::Model)
    .command(
        CommandSpec {
            name: CommandName::parse("review")?,
            summary: REVIEW_COMMAND_SUMMARY.into(),
            args_hint: Some(REVIEW_COMMAND_ARGS_HINT.into()),
        },
        Arc::new(ReviewCommand),
    )
    .status_kind("review", status)
    .build()
}
