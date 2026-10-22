// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The `procs` tool: decode, list ordering, and the one-pass grep scan.

use std::sync::Arc;

use dal_agent::error::ServiceError;
use dal_agent::ext::{
    ArgError, BoxFuture, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_core::ext::{JobStateView, JobsOp, JobsReply};
use dal_core::{JobStatus, Name, RegistrationError, ToolClass, ToolSpec};
use regex_automata::meta::Regex;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Object, Value};

use super::text::{
    TERMINAL_ROWS, escaped_prefix, finish_grep, list_line, matching_lines, state_matches,
};
use crate::text::normalize_ws;

/// The description registered for the `procs` tool.
pub const PROCS_DESCRIPTION: &str = "Search the background jobs of this session: list them by label, command, or status, or grep their retained output. Other sessions' jobs are invisible.";

/// The input schema registered for the `procs` tool.
pub(super) const PROCS_SCHEMA: &str = "{\"type\":\"object\",\"properties\":{\"op\":{\"type\":\"string\",\"enum\":[\"list\",\"grep\"],\"description\":\"list: the job table. grep: regex over retained job output.\"},\"query\":{\"type\":\"string\",\"description\":\"grep: the regex. list: a substring over label and command.\"},\"status\":{\"type\":\"string\",\"enum\":[\"running\",\"done\",\"failed\",\"cancelled\"],\"description\":\"list only.\"},\"limit\":{\"type\":\"integer\",\"minimum\":1,\"maximum\":50,\"description\":\"list rows, default 20.\"},\"max_matches\":{\"type\":\"integer\",\"minimum\":1,\"maximum\":200,\"description\":\"grep matches, default 50.\"},\"match_bytes\":{\"type\":\"integer\",\"minimum\":1,\"maximum\":2048,\"description\":\"grep bytes per line, default 256.\"}},\"required\":[\"op\"],\"additionalProperties\":false}";

/// One decoded `procs` call.
#[derive(Debug, Clone)]
struct ProcsArgs {
    op: ProcsOp,
    query: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
    max_matches: Option<usize>,
    match_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcsOp {
    List,
    Grep,
}

impl ProcsArgs {
    /// List rows; the schema range 1..=50 is checked at decode.
    fn limit(&self) -> usize {
        self.limit.unwrap_or(20)
    }

    /// Grep matches shown; the schema range 1..=200 is checked at decode.
    fn max_matches(&self) -> usize {
        self.max_matches.unwrap_or(50)
    }

    /// Grep bytes kept per line; the schema range 1..=2048 is checked at decode.
    fn match_bytes(&self) -> usize {
        self.match_bytes.unwrap_or(256)
    }
}

/// One `procs` failure with the exact plan text.
#[derive(Debug, thiserror::Error)]
enum ProcsError {
    #[error("procs: no jobs in this session.")]
    NoJobs,
    #[error("procs: no match for \"{query}\" in {jobs} job(s).")]
    NoMatch { query: Box<str>, jobs: usize },
    #[error("procs: query is not a valid regex: {error}.")]
    BadRegex { error: String },
    #[error("procs: the jobs service is not granted to this battery: {denied}.")]
    Denied { denied: String },
    #[error("procs: the jobs service failed: {reason}.")]
    Failed { reason: String },
    #[error("procs: invalid input: {message}.")]
    InvalidInput { message: String },
}

/// The `procs` tool shell; every call scans the session's jobs service.
pub(crate) struct ProcsTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl ProcsTool {
    /// Builds one tool; registration performs no I/O.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError`] when the fixed identity or schema is
    /// rejected.
    pub(super) fn new() -> Result<Self, RegistrationError> {
        let name = Name::parse("procs")?;
        let parameters =
            RawValue::parse(PROCS_SCHEMA).map_err(|_| RegistrationError::InvalidParameters)?;
        if !dal_core::valid_tool_parameters(&parameters) {
            return Err(RegistrationError::InvalidParameters);
        }
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: PROCS_DESCRIPTION.into(),
            parameters,
            grammar: None,
        });
        Ok(Self { name, spec })
    }
}

impl Tool for ProcsTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(
        &self,
        _args: &RawValue,
        _workspace: &dal_core::Workspace,
    ) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let args = match decode(call.args.as_str()) {
                Ok(args) => args,
                Err(message) => {
                    return ToolOutcome::Err(dal_agent::ToolError::message(message.to_string()));
                }
            };
            match run_args(cx.services().as_ref(), cx.caller(), &args).await {
                Ok(text) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(text.into_boxed_str()))),
                Err(error) => ToolOutcome::Err(dal_agent::ToolError::message(error.to_string())),
            }
        })
    }
}

/// Decodes one `procs` call against the exact shape of the schema, including
/// the per-op fields the schema can only describe in prose.
fn decode(raw: &str) -> Result<ProcsArgs, ProcsError> {
    let value: Value = sonic_rs::from_str(raw).map_err(|error| ProcsError::InvalidInput {
        message: error.to_string(),
    })?;
    let Some(object) = value.as_object() else {
        return Err(ProcsError::InvalidInput {
            message: "the arguments must be an object".into(),
        });
    };
    for (key, _) in object {
        if !matches!(
            key,
            "op" | "query" | "status" | "limit" | "max_matches" | "match_bytes"
        ) {
            return Err(ProcsError::InvalidInput {
                message: format!("unknown field {key}"),
            });
        }
    }
    let op = match object.get(&"op").and_then(sonic_rs::JsonValueTrait::as_str) {
        Some("list") => ProcsOp::List,
        Some("grep") => ProcsOp::Grep,
        Some(other) => {
            return Err(ProcsError::InvalidInput {
                message: format!("op must be list or grep, not {other}"),
            });
        }
        None => {
            return Err(ProcsError::InvalidInput {
                message: "op is required and must be list or grep".into(),
            });
        }
    };
    let query = string_field(object, "query");
    let status = string_field(object, "status");
    if let Some(status) = &status
        && !matches!(status.as_str(), "running" | "done" | "failed" | "cancelled")
    {
        return Err(ProcsError::InvalidInput {
            message: format!("status must be running, done, failed, or cancelled, not {status}"),
        });
    }
    if op == ProcsOp::Grep && query.is_none() {
        return Err(ProcsError::InvalidInput {
            message: "grep needs query".into(),
        });
    }
    if op == ProcsOp::Grep && status.is_some() {
        return Err(ProcsError::InvalidInput {
            message: "status applies to list only".into(),
        });
    }
    let limit = integer_field(object, "limit", 1, 50)?;
    let max_matches = integer_field(object, "max_matches", 1, 200)?;
    let match_bytes = integer_field(object, "match_bytes", 1, 2048)?;
    if op == ProcsOp::List && (max_matches.is_some() || match_bytes.is_some()) {
        return Err(ProcsError::InvalidInput {
            message: "max_matches and match_bytes apply to grep only".into(),
        });
    }
    if op == ProcsOp::Grep && limit.is_some() {
        return Err(ProcsError::InvalidInput {
            message: "limit applies to list only".into(),
        });
    }
    Ok(ProcsArgs {
        op,
        query,
        status,
        limit,
        max_matches,
        match_bytes,
    })
}

fn string_field(object: &Object, key: &str) -> Option<String> {
    object
        .get(&key)
        .and_then(sonic_rs::JsonValueTrait::as_str)
        .map(str::to_owned)
}

/// One optional integer field, rejected unless it lies in `min..=max`.
fn integer_field(
    object: &Object,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<usize>, ProcsError> {
    let Some(value) = object.get(&key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
        .filter(|number| (min..=max).contains(number))
        .map(Some)
        .ok_or_else(|| ProcsError::InvalidInput {
            message: format!("{key} must be an integer from {min} to {max}"),
        })
}

/// Runs one decoded call against the jobs service. One `list` call issues
/// one `JobsOp::List`; one `grep` call reads each job's text once.
async fn run_args(
    services: &dyn dal_agent::ext::Services,
    caller: &dal_agent::ext::Caller,
    args: &ProcsArgs,
) -> Result<String, ProcsError> {
    let jobs = jobs_list(services, caller).await?;
    if jobs.is_empty() {
        return Err(ProcsError::NoJobs);
    }
    match args.op {
        ProcsOp::List => {
            let rendered = render_list(&jobs, args);
            if rendered.is_empty() {
                let needle = args
                    .query
                    .as_deref()
                    .or(args.status.as_deref())
                    .unwrap_or_default();
                return Err(ProcsError::NoMatch {
                    query: needle.into(),
                    jobs: jobs.len(),
                });
            }
            Ok(rendered)
        }
        ProcsOp::Grep => grep(services, caller, &jobs, args).await,
    }
}

/// The session's jobs, or the exact denial text.
async fn jobs_list(
    services: &dyn dal_agent::ext::Services,
    caller: &dal_agent::ext::Caller,
) -> Result<Vec<JobStatus>, ProcsError> {
    match services.jobs(caller, JobsOp::List).await {
        Ok(JobsReply::Listed(jobs)) => Ok(jobs),
        Err(error @ ServiceError::Denied(_)) => Err(ProcsError::Denied {
            denied: error.to_string(),
        }),
        Err(other) => Err(ProcsError::Failed {
            reason: other.to_string(),
        }),
        Ok(_) => Err(ProcsError::Failed {
            reason: "unexpected reply to a list request".into(),
        }),
    }
}

/// Whether the job passes the `query` substring and `status` phase filters.
/// The jobs service exposes one label per job, which for an exec job carries
/// the command text, so both filters read it.
fn keep(job: &JobStatus, query: &str, status: Option<&str>) -> bool {
    let status_ok = status.is_none_or(|wanted| state_matches(&job.state, wanted));
    let query_ok = query.is_empty() || job.label.contains(query);
    status_ok && query_ok
}

/// Orders live jobs oldest first, then at most ten terminal jobs newest
/// first, applies the filters and the row limit, and renders one line per row.
fn render_list(jobs: &[JobStatus], args: &ProcsArgs) -> String {
    let now = dal_core::Timestamp::now();
    let query = args.query.as_deref().unwrap_or("");
    let status = args.status.as_deref();
    // The service lists live jobs in start order, oldest first, and finished
    // jobs in settlement order, oldest settled first, so the terminal side
    // reads newest first only reversed.
    let live: Vec<&JobStatus> = jobs
        .iter()
        .filter(|job| matches!(job.state, JobStateView::Running | JobStateView::Detached))
        .filter(|job| keep(job, query, status))
        .collect();
    let terminal: Vec<&JobStatus> = jobs
        .iter()
        .filter(|job| matches!(job.state, JobStateView::Done(_)))
        .filter(|job| keep(job, query, status))
        .rev()
        .take(TERMINAL_ROWS)
        .collect();
    live.into_iter()
        .chain(terminal)
        .take(args.limit())
        .map(|job| list_line(job, now, &job.label))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The collected output text of one job; the only job-data read the tool makes.
async fn job_text(
    services: &dyn dal_agent::ext::Services,
    caller: &dal_agent::ext::Caller,
    id: dal_core::JobId,
) -> Result<String, ProcsError> {
    match services.jobs(caller, JobsOp::Text { id }).await {
        Ok(JobsReply::Text { text, .. }) => Ok(text.to_string()),
        Err(error @ ServiceError::Denied(_)) => Err(ProcsError::Denied {
            denied: error.to_string(),
        }),
        // A job that ends or drains mid-scan contributes no lines; the scan
        // never retries (5.3).
        _ => Ok(String::new()),
    }
}

/// The reason a pattern failed to compile. The meta builder's own text names
/// only the pattern, so the syntax error behind it supplies the reason, for
/// example that look-around is not supported.
fn regex_reason(error: &regex_automata::meta::BuildError) -> String {
    let detail =
        std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string);
    // A syntax error is a multi-line report that ends in `error: <reason>`.
    match detail
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("error: "))
    {
        Some(reason) => reason.to_owned(),
        None => normalize_ws(&detail),
    }
}

/// Reads each job's text once through the jobs service and renders the
/// bounded match list. The caps stop what is shown, not what is counted: the
/// elision tail reports every further match the scan saw.
async fn grep(
    services: &dyn dal_agent::ext::Services,
    caller: &dal_agent::ext::Caller,
    jobs: &[JobStatus],
    args: &ProcsArgs,
) -> Result<String, ProcsError> {
    let Some(query) = args.query.as_deref() else {
        return Err(ProcsError::InvalidInput {
            message: "grep needs query".into(),
        });
    };
    let regex = Regex::new(query).map_err(|error| ProcsError::BadRegex {
        error: regex_reason(&error),
    })?;
    let max_matches = args.max_matches();
    let match_bytes = args.match_bytes();
    let mut rendered: Vec<String> = Vec::new();
    let mut seen_total = 0usize;
    for job in jobs {
        let text = job_text(services, caller, job.id).await?;
        for (number, line) in matching_lines(&regex, &text) {
            seen_total += 1;
            if rendered.len() < max_matches {
                rendered.push(format!(
                    "{}:{number}: {}",
                    job.id,
                    escaped_prefix(line, match_bytes)
                ));
            }
        }
    }
    if rendered.is_empty() {
        return Err(ProcsError::NoMatch {
            query: query.into(),
            jobs: jobs.len(),
        });
    }
    Ok(finish_grep(rendered, seen_total))
}
