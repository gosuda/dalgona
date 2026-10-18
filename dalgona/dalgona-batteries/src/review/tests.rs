// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Review battery tests: config, reply decoding, rounds, rendering, and local git.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use dal_core::{SessionEnd, SessionId};

use super::config::{ReviewConfig, ReviewConfigError};
use super::git::{cap_diff, diff_argv, git_run_request, status_argv, status_exceeds_limit};
use super::reply::{
    Severity, Verdict, cap_error, command_prompt, enforce_findings_cap, parse_reply, render_report,
    review_content, settle_reply,
};
use super::rounds::{
    ReviewRecord, ReviewRound, RoundRequest, StoredFinding, identity, new_count, next_round,
    prior_findings, stored_findings,
};
use super::{
    DIFF_TRUNCATION_MARKER, FOCUS_TRUNCATION_MARKER, GIT_STDOUT_PREFIX_LIMIT, MAX_DIFF_BYTES,
    MAX_ERROR_BYTES, MAX_FINDINGS, MAX_FOCUS_BYTES, MAX_STORED_DETAIL_BYTES, REVIEW_REPLY_FORMAT,
    REVIEWER_DEADLINE, ReviewError, TRUNCATION_MARKER, cap_with_marker,
};
use super::{ReviewStatus, ReviewTool};
use crate::review::review_round;
use crate::text::byte_prefix;
use dal_agent::ext::{HookCx, ObserveHook, ToolCx};

/// How the scripted host answers the provider probe and the reviewer request.
#[derive(Default)]
enum Provider {
    /// Every provider call is refused; no round in these tests reaches it.
    #[default]
    Unavailable,
    /// The route has no usable credentials.
    NoCredentials,
    /// The route is ready and the request streams these events.
    Replies(Vec<dal_core::StreamEvent>),
    /// The route is ready and the request fails with this text.
    Fails(String),
    /// The route is ready and the request never answers.
    Hangs,
}

/// A scripted `Services` host for driving `review_round` in-crate: the
/// records store keeps appended records, the run service answers two git
/// calls, the provider follows [`Provider`], and everything else refuses.
#[derive(Default)]
struct FakeReviewServices {
    records: std::sync::Mutex<HashMap<String, Vec<dal_core::RawJson>>>,
    runs: std::sync::Mutex<std::collections::VecDeque<dal_core::RunOutput>>,
    provider: std::sync::Mutex<Provider>,
    probes: std::sync::Mutex<Vec<dal_core::ModelRoute>>,
    requests: std::sync::Mutex<Vec<dal_core::ModelRequest>>,
    appended: std::sync::Mutex<Vec<String>>,
    notices: std::sync::Mutex<Vec<dal_core::Notice>>,
    /// When set, `run` executes the requested git argv in this directory
    /// instead of answering from `runs`.
    repo: std::sync::Mutex<Option<std::path::PathBuf>>,
}

fn locked<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl dal_agent::ext::Services for FakeReviewServices {
    fn model_ready(
        &self,
        _who: &dal_agent::ext::Caller,
        model: dal_core::ModelRoute,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Option<dal_core::ModelRoute>> {
        locked(&self.probes).push(model.clone());
        let ready = match &*locked(&self.provider) {
            Provider::Unavailable => return unavailable(),
            Provider::NoCredentials => None,
            Provider::Replies(_) | Provider::Fails(_) | Provider::Hangs => Some(model),
        };
        Box::pin(async move { Ok(ready) })
    }

    fn run(
        &self,
        _who: &dal_agent::ext::Caller,
        req: dal_core::RunRequest,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_core::RunOutput> {
        if let Some(dir) = locked(&self.repo).as_deref() {
            let output = real_git::run(dir, &req);
            return Box::pin(async move { Ok(output) });
        }
        let next = locked(&self.runs).pop_front();
        Box::pin(async move {
            next.ok_or_else(|| {
                dal_agent::error::ServiceError::failed(None, "no scripted git output")
            })
        })
    }

    fn records(
        &self,
        _who: &dal_agent::ext::Caller,
        kind: &str,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Vec<Box<dal_agent::ext::RawValue>>> {
        let bodies = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(kind)
            .cloned()
            .unwrap_or_default();
        Box::pin(async move { Ok(bodies.into_iter().map(Box::new).collect()) })
    }

    fn fs_read(
        &self,
        _who: &dal_agent::ext::Caller,
        _path: &str,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn fs_write(
        &self,
        _who: &dal_agent::ext::Caller,
        _path: &str,
        _bytes: Vec<u8>,
    ) -> dal_agent::ext::services::ServiceFuture<'_, ()> {
        unavailable()
    }

    fn net(
        &self,
        _who: &dal_agent::ext::Caller,
        _req: dal_core::FetchRequest,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_core::FetchResponse> {
        unavailable()
    }

    fn env(
        &self,
        _who: &dal_agent::ext::Caller,
        _key: &str,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Option<String>> {
        unavailable()
    }

    fn ask(
        &self,
        _who: &dal_agent::ext::Caller,
        _question: dal_core::Question,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Option<dal_core::Answer>> {
        unavailable()
    }

    fn mcp(
        &self,
        _who: &dal_agent::ext::Caller,
        _req: dal_core::ext::McpRequest,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_core::ext::McpResponse> {
        unavailable()
    }

    fn mcp_declarations(
        &self,
        _who: &dal_agent::ext::Caller,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Vec<dal_core::ext::McpDeclaration>> {
        unavailable()
    }

    fn agents(
        &self,
        _who: &dal_agent::ext::Caller,
        _op: dal_core::AgentsOp,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_core::AgentsReply> {
        unavailable()
    }

    fn jobs(
        &self,
        _who: &dal_agent::ext::Caller,
        _op: dal_core::JobsOp,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_core::JobsReply> {
        unavailable()
    }

    fn turn(
        &self,
        _who: &dal_agent::ext::Caller,
        _op: dal_core::TurnOp,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_core::TurnOpReply> {
        unavailable()
    }

    fn history_texts(
        &self,
        _who: &dal_agent::ext::Caller,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Vec<String>> {
        unavailable()
    }

    fn private_outcomes(
        &self,
        _who: &dal_agent::ext::Caller,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Vec<dal_core::PrivateEffect>> {
        unavailable()
    }

    fn blob_put(
        &self,
        _who: &dal_agent::ext::Caller,
        _bytes: Vec<u8>,
    ) -> dal_agent::ext::services::ServiceFuture<'_, [u8; 32]> {
        unavailable()
    }

    fn blob_get(
        &self,
        _who: &dal_agent::ext::Caller,
        _digest: [u8; 32],
    ) -> dal_agent::ext::services::ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn add_session_tools(
        &self,
        _who: &dal_agent::ext::Caller,
        _tools: Vec<(Arc<dyn dal_agent::ext::Tool>, dal_core::Visibility)>,
    ) -> dal_agent::ext::services::ServiceFuture<'_, ()> {
        unavailable()
    }

    fn open_asks(
        &self,
        _who: &dal_agent::ext::Caller,
    ) -> dal_agent::ext::services::ServiceFuture<'_, usize> {
        unavailable()
    }

    fn scheme(
        &self,
        _who: &dal_agent::ext::Caller,
        _uri: &str,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        unavailable()
    }

    fn state(
        &self,
        _who: &dal_agent::ext::Caller,
        _op: dal_core::ext::StateOp,
    ) -> dal_agent::ext::services::ServiceFuture<
        '_,
        Result<dal_core::ext::StateRecord, dal_core::ext::StateError>,
    > {
        unavailable()
    }

    fn sidecar(
        &self,
        _who: &dal_agent::ext::Caller,
        _op: dal_core::SidecarOp,
    ) -> dal_agent::ext::services::ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn infer(
        &self,
        _who: &dal_agent::ext::Caller,
        req: dal_core::ModelRequest,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_core::Inference> {
        locked(&self.requests).push(req);
        match &*locked(&self.provider) {
            Provider::Replies(events) => {
                let inference = dal_core::Inference {
                    events: events.clone(),
                };
                Box::pin(async move { Ok(inference) })
            }
            Provider::Fails(text) => {
                let text = text.clone();
                Box::pin(async move { Err(dal_agent::error::ServiceError::failed(None, text)) })
            }
            Provider::Hangs => Box::pin(std::future::pending()),
            Provider::Unavailable | Provider::NoCredentials => unavailable(),
        }
    }

    fn infer_stream(
        &self,
        _who: &dal_agent::ext::Caller,
        _req: dal_core::ModelRequest,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_agent::ext::EventStream> {
        unavailable()
    }

    fn call_tool(
        &self,
        _who: &dal_agent::ext::Caller,
        _name: &str,
        _args: Box<dal_agent::ext::RawValue>,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_agent::ext::ToolOutcome> {
        unavailable()
    }

    fn notify(&self, _who: &dal_agent::ext::Caller, notice: dal_core::Notice) {
        locked(&self.notices).push(notice);
    }

    fn append_record(
        &self,
        _who: &dal_agent::ext::Caller,
        kind: &str,
        body: Box<dal_agent::ext::RawValue>,
    ) -> dal_agent::ext::services::ServiceFuture<'_, dal_core::EntryId> {
        let text = body.as_str().to_owned();
        if let Ok(record) = dal_core::RawJson::parse(&text) {
            locked(&self.records)
                .entry(kind.to_owned())
                .or_default()
                .push(record);
        }
        locked(&self.appended).push(text);
        Box::pin(async { Ok(dal_core::EntryId::new(std::num::NonZeroU64::MIN)) })
    }
}

fn unavailable<T: Send + 'static>() -> dal_agent::ext::services::ServiceFuture<'static, T> {
    Box::pin(async {
        Err(dal_agent::error::ServiceError::failed(
            None,
            "unavailable in the scripted host",
        ))
    })
}

fn outcome_text(outcome: dal_agent::ext::ToolOutcome) -> String {
    match outcome {
        dal_agent::ext::ToolOutcome::Ok(output) => output.to_string(),
        dal_agent::ext::ToolOutcome::Err(error) => error.to_string(),
        dal_agent::ext::ToolOutcome::Interrupted => "interrupted".to_owned(),
        dal_agent::ext::ToolOutcome::Detached(_) => "detached".to_owned(),
    }
}
const COMMAND_PROMPT_BASE: &str = "Review the current changes with the review tool. The user ran /review, which grants one restart; call review with restart set to true.";
const ONE_FINDING: &str = r#"{"verdict":"findings","findings":[{"path":"src/lib.rs","line":null,"severity":"major","title":"  Broken   behavior ","detail":"A failure."}],"summary":"reviewed"}"#;
#[test]
fn overlapping_reviews_keep_status_until_the_last_call_finishes()
-> Result<(), Box<dyn std::error::Error>> {
    #[derive(serde::Deserialize)]
    struct StatusPayload {
        round: u8,
    }

    let status = std::sync::Arc::new(ReviewStatus::new(3));
    let session = SessionId::new_v7();
    let first = status.running(session, 1);
    let second = status.running(session, 2);
    drop(first);

    let payload = status
        .payload(session)
        .ok_or("the second review remains visible")?;
    let payload: StatusPayload = sonic_rs::from_str(&payload)?;
    assert_eq!(payload.round, 2);

    drop(second);
    assert!(status.payload(session).is_none());
    Ok(())
}

#[test]
fn config_defaults_and_rejects_unknown_or_out_of_range_values()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(ReviewConfig::parse_config(None)?, ReviewConfig::default());
    for text in ["max_rounds = 0", "max_rounds = 11"] {
        let value = toml::from_str::<toml::Value>(text)?;
        assert!(matches!(
            ReviewConfig::parse_config(Some(&value)),
            Err(ReviewConfigError::InvalidValue { key, .. })
                if key.as_ref() == "plugin.review.max_rounds"
        ));
    }
    let options_as_revision =
        toml::from_str::<toml::Value>(r#"diff_base = "--output=/tmp/review""#)?;
    assert!(matches!(
        ReviewConfig::parse_config(Some(&options_as_revision)),
        Err(ReviewConfigError::InvalidValue { key, .. })
            if key.as_ref() == "plugin.review.diff_base"
    ));
    let configured = toml::from_str::<toml::Value>(
        "enabled = false\nmax_rounds = 2\nreviewer_model = \"fast\"\ndiff_base = \"main\"",
    )?;
    assert_eq!(
        ReviewConfig::parse_config(Some(&configured))?,
        ReviewConfig {
            enabled: false,
            max_rounds: 2,
            reviewer_model: String::from("fast"),
            diff_base: String::from("main"),
        }
    );
    let unknown = toml::from_str::<toml::Value>("unknown = true")?;
    assert!(matches!(
        ReviewConfig::parse_config(Some(&unknown)),
        Err(ReviewConfigError::UnknownKey { key }) if key.as_ref() == "unknown"
    ));
    let scalar = toml::Value::String("not a table".to_owned());
    assert!(matches!(
        ReviewConfig::parse_config(Some(&scalar)),
        Err(ReviewConfigError::InvalidSection)
    ));
    Ok(())
}
#[test]
fn tolerant_review_decode() -> Result<(), Box<dyn std::error::Error>> {
    let reply = parse_reply(
        r#"{"extra":1,"verdict":"findings","findings":[{"path":"src/lib.rs","line":null,"severity":"major","title":"  Broken   behavior ","detail":"A failure.","extra":2}],"summary":"reviewed"}"#,
        1,
    )?;
    assert_eq!(reply.findings[0].line.0, None);
    assert!(matches!(
        parse_reply(
            r#"{"verdict":"findings","findings":[{"path":"src/lib.rs","severity":"major","title":"Broken","detail":"A failure."}],"summary":"reviewed"}"#,
            1,
        ),
        Err(ReviewError::Parse { reason, .. }) if reason.contains("line")
    ));
    Ok(())
}
#[test]
fn review_contract_breach() {
    let invalid_severity = ONE_FINDING.replace("major", "nit");
    assert!(matches!(
        parse_reply(&invalid_severity, 2),
        Err(ReviewError::Parse { reason, .. }) if reason.contains("severity")
    ));
    let long_title = format!(
        "{{\"verdict\":\"findings\",\"findings\":[{{\"path\":\"x\",\"line\":null,\"severity\":\"minor\",\"title\":\"{}\",\"detail\":\"d\"}}],\"summary\":\"ok\"}}",
        "t".repeat(201)
    );
    assert!(matches!(
        parse_reply(&long_title, 1),
        Err(ReviewError::Parse { reason, .. }) if reason.contains("title")
    ));
    assert!(matches!(
        parse_reply(&format!("{ONE_FINDING} {{}}"), 2),
        Err(ReviewError::Parse { .. })
    ));
}
#[test]
fn parse_failure_is_reported_at_its_round() {
    assert!(matches!(
        parse_reply("not json", 3),
        Err(ReviewError::Parse { round: 3, reason }) if reason.len() <= MAX_ERROR_BYTES
    ));
}
#[test]
fn reply_decode_enforces_verdict_consistency_and_summary_bytes() {
    let mismatch = ONE_FINDING.replace("\"verdict\":\"findings\"", "\"verdict\":\"clean\"");
    assert!(matches!(
        parse_reply(&mismatch, 1),
        Err(ReviewError::Parse { reason, .. }) if reason.contains("verdict")
    ));
    let long_summary = format!(
        "{{\"verdict\":\"clean\",\"findings\":[],\"summary\":\"{}\"}}",
        "é".repeat(251)
    );
    assert!(matches!(
        parse_reply(&long_summary, 1),
        Err(ReviewError::Parse { reason, .. }) if reason.contains("summary")
    ));
    let max_summary = format!(
        "{{\"verdict\":\"clean\",\"findings\":[],\"summary\":\"{}\"}}",
        "x".repeat(500)
    );
    assert!(parse_reply(&max_summary, 1).is_ok());
}
#[test]
fn capped_error_text_ends_on_a_utf8_boundary() {
    let cause = "é".repeat(MAX_ERROR_BYTES / 2 + 1);
    let capped = cap_error(&cause);
    assert_eq!(capped.len(), MAX_ERROR_BYTES);
    assert!(capped.is_char_boundary(capped.len()));
    assert!(capped.starts_with("éé"));
    assert!(capped.ends_with(TRUNCATION_MARKER));
}
#[test]
fn reply_decode_names_finding_field_limits() {
    let long_title = format!(
        "{{\"verdict\":\"findings\",\"findings\":[{{\"path\":\"x\",\"line\":null,\"severity\":\"minor\",\"title\":\"{}\",\"detail\":\"d\"}}],\"summary\":\"ok\"}}",
        "t".repeat(201)
    );
    assert!(matches!(
        parse_reply(&long_title, 1),
        Err(ReviewError::Parse { reason, .. }) if reason.contains("title")
    ));
    let long_path = format!(
        "{{\"verdict\":\"findings\",\"findings\":[{{\"path\":\"{}\",\"line\":null,\"severity\":\"minor\",\"title\":\"t\",\"detail\":\"d\"}}],\"summary\":\"ok\"}}",
        "p".repeat(1025)
    );
    assert!(matches!(
        parse_reply(&long_path, 1),
        Err(ReviewError::Parse { reason, .. }) if reason.contains("path")
    ));
    let long_detail = format!(
        "{{\"verdict\":\"findings\",\"findings\":[{{\"path\":\"x\",\"line\":null,\"severity\":\"minor\",\"title\":\"t\",\"detail\":\"{}\"}}],\"summary\":\"ok\"}}",
        "d".repeat(2001)
    );
    assert!(matches!(
        parse_reply(&long_detail, 1),
        Err(ReviewError::Parse { reason, .. }) if reason.contains("detail")
    ));
}
#[test]
fn findings_cap() -> Result<(), Box<dyn std::error::Error>> {
    let finding =
        r#"{"path":"src/lib.rs","line":null,"severity":"major","title":"bug","detail":"broken"}"#;
    let mut findings = String::new();
    for index in 0..=MAX_FINDINGS {
        if index != 0 {
            findings.push(',');
        }
        findings.push_str(finding);
    }
    let body = format!("{{\"verdict\":\"findings\",\"findings\":[{findings}],\"summary\":\"ok\"}}");
    let reply = parse_reply(&body, 1)?;
    assert!(matches!(
        enforce_findings_cap(reply),
        Err(ReviewError::TooManyFindings { count: 51 })
    ));
    Ok(())
}
#[test]
fn duplicate_current_findings_count_against_earlier_rounds_only()
-> Result<(), Box<dyn std::error::Error>> {
    let body = r#"{"verdict":"findings","findings":[{"path":"src/lib.rs","line":null,"severity":"major","title":"Bug","detail":"first"},{"path":"src/lib.rs","line":null,"severity":"major","title":" bug ","detail":"second"}],"summary":"ok"}"#;
    let reply = parse_reply(body, 1)?;
    let earlier = BTreeSet::new();
    assert!(matches!(new_count(&reply, &earlier), Ok(2)));
    let earlier = BTreeSet::from([identity("src/lib.rs", "bug")]);
    assert!(matches!(new_count(&reply, &earlier), Ok(0)));
    Ok(())
}
#[test]
fn prior_findings_are_leaf_session_scoped_sorted_and_unique() {
    let session = SessionId::new_v7();
    let other_session = SessionId::new_v7();
    let finding = |path: &str, title: &str| StoredFinding {
        path: path.to_owned(),
        line: None,
        severity: Severity::Minor,
        title: title.to_owned(),
        detail: "detail".to_owned(),
    };
    let records = vec![
        ReviewRecord {
            session,
            round: 1,
            verdict: Verdict::Findings,
            new_count: 2,
            findings: vec![finding("z.rs", "Beta"), finding("a.rs", "Alpha")],
        },
        ReviewRecord {
            session: other_session,
            round: 1,
            verdict: Verdict::Findings,
            new_count: 1,
            findings: vec![finding("ignored.rs", "other session")],
        },
        ReviewRecord {
            session,
            round: 2,
            verdict: Verdict::Findings,
            new_count: 0,
            findings: vec![finding("z.rs", " beta "), finding("a.rs", "Alpha")],
        },
    ];
    assert_eq!(prior_findings(&records, session), "a.rs\talpha\nz.rs\tbeta");
}
#[test]
fn review_rounds_resume_only_nonterminal_sessions() {
    let session = SessionId::new_v7();
    let record = |round, verdict, new_count| ReviewRecord {
        session,
        round,
        verdict,
        new_count,
        findings: Vec::new(),
    };
    let pending = [record(1, Verdict::Findings, 1)];
    for request in [RoundRequest::Continue, RoundRequest::Restart] {
        assert!(
            matches!(
                next_round(&pending, 3, request),
                Ok(ReviewRound { session: current, round: 2 }) if current == session
            ),
            "a restart request must not discard in-progress rounds: {request:?}"
        );
    }
    let converged = [record(1, Verdict::Findings, 0)];
    assert!(matches!(
        next_round(&converged, 3, RoundRequest::Continue),
        Ok(ReviewRound { session: current, round: 1 }) if current != session
    ));
    let capped = [record(2, Verdict::Findings, 1)];
    assert!(matches!(
        next_round(&capped, 2, RoundRequest::Continue),
        Err(ReviewError::CapReached { rounds: 2, .. })
    ));
    assert!(matches!(
        next_round(&capped, 2, RoundRequest::Restart),
        Ok(ReviewRound { session: current, round: 1 }) if current != session
    ));
    assert!(matches!(
        next_round(&[], 2, RoundRequest::Restart),
        Ok(ReviewRound { round: 1, .. })
    ));
}
#[test]
fn reviewer_request_sections_follow_the_fixed_order() {
    let content = review_content("diff\n", "M src/lib.rs\n", None, "none");
    assert_eq!(
        content,
        format!(
            "## Diff\ndiff\n## Status\nM src/lib.rs\n## Focus\nnone\n## Prior findings\nnone\n## Reply format\n{REVIEW_REPLY_FORMAT}"
        )
    );
}
#[test]
fn finding_identity_collapses_unicode_whitespace_and_case() {
    assert_eq!(
        identity("src/lib.rs", "  Crème\t  BRÛLÉE \n"),
        identity("src/lib.rs", "crème brûlée")
    );
    assert_ne!(
        identity("src/lib.rs", "bug"),
        identity("src/main.rs", "bug")
    );
}
#[test]
fn diff_and_focus_caps_preserve_utf8_boundaries() {
    let diff = format!("{}a", "é".repeat(MAX_DIFF_BYTES / 2));
    let (capped, truncated) = cap_diff(&diff, false);
    assert!(truncated);
    assert!(capped.starts_with(byte_prefix(&diff, MAX_DIFF_BYTES)));
    let (captured_prefix, overflowed) = cap_diff(byte_prefix(&diff, MAX_DIFF_BYTES), true);
    assert!(overflowed);
    assert!(captured_prefix.ends_with(DIFF_TRUNCATION_MARKER));
    assert!(capped.ends_with(DIFF_TRUNCATION_MARKER));
    assert_eq!(
        command_prompt(&format!("{}b", "é".repeat(251))),
        format!(
            "{COMMAND_PROMPT_BASE}\nFocus: {}{}",
            "é".repeat(250),
            FOCUS_TRUNCATION_MARKER
        )
    );
}
#[test]
fn the_review_command_prompt_tells_the_model_to_restart_a_capped_session() {
    let empty = command_prompt("");
    assert_eq!(empty, COMMAND_PROMPT_BASE);
    assert!(empty.contains("/review"), "{empty}");
    assert!(empty.contains("grants one restart"), "{empty}");
    assert!(command_prompt("auth").starts_with(COMMAND_PROMPT_BASE));
}
#[test]
fn review_restart_schema_requires_user_command() {
    let status = Arc::new(ReviewStatus::new(3));
    let tool = ReviewTool::new(ReviewConfig::default(), status).expect("review tool");
    let schema = tool.spec.parameters.as_str();

    assert!(schema.contains("user runs /review"), "{schema}");
    assert!(schema.contains("grants one restart"), "{schema}");
    assert!(
        schema.contains("review with restart set to true"),
        "{schema}"
    );
}
#[test]
fn status_capture_rejects_overflow_instead_of_truncating() {
    let at_limit = vec![b'x'; MAX_DIFF_BYTES];
    let over_limit = vec![b'x'; MAX_DIFF_BYTES + 1];
    assert!(!status_exceeds_limit(&at_limit, false));
    assert!(status_exceeds_limit(&over_limit, false));
    assert!(status_exceeds_limit(&at_limit, true));
}
#[test]
fn report_escapes_controls_and_preserves_quotes() -> Result<(), Box<dyn std::error::Error>> {
    let reply = parse_reply(
        r#"{"verdict":"findings","findings":[{"path":"src/\nlib.rs","line":null,"severity":"major","title":"New\tproblem","detail":"doesn't retry\nline two"}],"summary":"say \"no\""}"#,
        1,
    )?;
    let earlier = BTreeSet::new();
    let rendered = render_report(1, 3, &reply, &earlier, true);
    assert!(rendered.contains("Summary: \"say \\\"no\\\"\""));
    assert!(rendered.contains("[... diff truncated at 262144 bytes ...]"));
    assert!(rendered.contains("src/\\nlib.rs:? New\\tproblem (new)"));
    assert!(rendered.contains("doesn't retry\\nline two"));
    assert!(rendered.ends_with("Fix the new findings and call review again."));
    Ok(())
}
#[test]
fn stored_detail_is_capped_at_five_hundred_bytes_and_marked()
-> Result<(), Box<dyn std::error::Error>> {
    let reply = parse_reply(
        &format!(
            "{{\"verdict\":\"findings\",\"findings\":[{{\"path\":\"x\",\"line\":null,\"severity\":\"minor\",\"title\":\"t\",\"detail\":\"{}\"}}],\"summary\":\"ok\"}}",
            "é".repeat(1000)
        ),
        1,
    )?;
    let stored = stored_findings(&reply);
    assert_eq!(stored[0].detail.len(), MAX_STORED_DETAIL_BYTES);
    assert!(stored[0].detail.is_char_boundary(stored[0].detail.len()));
    assert!(stored[0].detail.starts_with("éé"));
    assert!(stored[0].detail.ends_with(TRUNCATION_MARKER));
    Ok(())
}

#[test]
fn review_clean_round_settles_with_exact_line() -> Result<(), Box<dyn std::error::Error>> {
    let reply = parse_reply(r#"{"verdict":"clean","findings":[],"summary":"ok"}"#, 1)?;
    let earlier = BTreeSet::new();
    let settled = settle_reply(1, 1, &reply, &earlier, 0, false)?;
    assert_eq!(settled, "Review round 1 of 1: clean. No findings.");
    Ok(())
}

#[test]
fn review_convergence_counts_repeats_as_zero_new() -> Result<(), Box<dyn std::error::Error>> {
    let first = parse_reply(
        r#"{"verdict":"findings","findings":[{"path":"src/lib.rs","line":10,"severity":"major","title":"Null deref","detail":"first"},{"path":"src/main.rs","line":null,"severity":"minor","title":"Typo","detail":"second"}],"summary":"two"}"#,
        1,
    )?;
    let earlier = BTreeSet::new();
    let fresh = new_count(&first, &earlier)?;
    assert_eq!(fresh, 2);
    let stored = stored_findings(&first);
    let session = SessionId::new_v7();
    let records = vec![ReviewRecord {
        session,
        round: 1,
        verdict: Verdict::Findings,
        new_count: fresh,
        findings: stored,
    }];
    let second = parse_reply(
        r#"{"verdict":"findings","findings":[{"path":"src/lib.rs","line":12,"severity":"major","title":"  null  DEREF ","detail":"again"},{"path":"src/main.rs","line":null,"severity":"minor","title":"typo","detail":"again"}],"summary":"same"}"#,
        2,
    )?;
    let earlier = prior_findings(&records, session);
    assert!(earlier.contains("src/lib.rs"));
    let seen = {
        let mut set = BTreeSet::new();
        set.insert(identity("src/lib.rs", "null deref"));
        set.insert(identity("src/main.rs", "typo"));
        set
    };
    let fresh = new_count(&second, &seen)?;
    assert_eq!(fresh, 0);
    let settled = settle_reply(2, 3, &second, &seen, fresh, false)?;
    assert_eq!(settled, "Converged after 2 rounds: no new findings.");
    Ok(())
}

#[test]
fn review_cap_reached_stops_and_reports_outstanding_findings()
-> Result<(), Box<dyn std::error::Error>> {
    let reply = parse_reply(ONE_FINDING, 2)?;
    let earlier = BTreeSet::new();
    let fresh = new_count(&reply, &earlier)?;
    assert_eq!(fresh, 1);
    let settled = settle_reply(2, 2, &reply, &earlier, fresh, false);
    let Err(ReviewError::CapReached {
        rounds: 2,
        outstanding,
    }) = settled
    else {
        return Err("the capped round did not stop with CapReached".into());
    };
    assert!(outstanding.contains("src/lib.rs"), "{outstanding}");
    assert!(outstanding.contains("Broken   behavior"), "{outstanding}");
    let message = ReviewError::CapReached {
        rounds: 2,
        outstanding: outstanding.clone(),
    }
    .to_string();
    assert!(message.contains("must run /review"), "{message}");
    assert!(message.contains("grants one restart"), "{message}");
    assert!(
        message.contains("review with restart set to true"),
        "{message}"
    );
    let session = SessionId::new_v7();
    let capped = [ReviewRecord {
        session,
        round: 2,
        verdict: Verdict::Findings,
        new_count: fresh,
        findings: stored_findings(&reply),
    }];
    let Err(ReviewError::CapReached {
        rounds: 2,
        outstanding: repeated,
    }) = next_round(&capped, 2, RoundRequest::Continue)
    else {
        return Err("a call after the cap did not stop".into());
    };
    assert_eq!(repeated, outstanding);
    Ok(())
}

#[test]
fn cancel_mid_round_appends_no_record_and_resumes() -> Result<(), Box<dyn std::error::Error>> {
    let session = SessionId::new_v7();
    let first = parse_reply(ONE_FINDING, 1)?;
    let fresh = new_count(&first, &BTreeSet::new())?;
    let records = vec![ReviewRecord {
        session,
        round: 1,
        verdict: Verdict::Findings,
        new_count: fresh,
        findings: stored_findings(&first),
    }];
    let count_before = records.len();
    let resumed = next_round(&records, 3, RoundRequest::Continue)?;
    assert_eq!(resumed.session, session);
    assert_eq!(resumed.round, 2);
    assert_eq!(records.len(), count_before);
    Ok(())
}

#[test]
fn git_requests_use_explicit_workspace_and_fixed_argv() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from("/tmp/dalgona-review-workspace");
    let workspace = dal_core::Workspace::new(dir.clone())?;
    let diff_request = git_run_request(&workspace, diff_argv(""));
    assert_eq!(
        diff_request.argv,
        vec![
            std::ffi::OsString::from("git"),
            std::ffi::OsString::from("diff"),
            std::ffi::OsString::from("--no-ext-diff"),
            std::ffi::OsString::from("--no-textconv"),
            std::ffi::OsString::from("HEAD"),
        ]
    );
    assert_eq!(
        git_run_request(&workspace, diff_argv("main")).argv[4],
        std::ffi::OsString::from("main")
    );
    assert_eq!(diff_request.cwd, Some(dir.clone()));
    assert!(diff_request.stdin.is_none());
    assert!(diff_request.env.iter().any(|entry| entry
        == &(
            Box::<str>::from("GIT_OPTIONAL_LOCKS"),
            Box::<str>::from("0")
        )));
    assert_eq!(diff_request.stdout_prefix_limit, GIT_STDOUT_PREFIX_LIMIT);
    let status_request = git_run_request(&workspace, status_argv());
    assert_eq!(
        status_request.argv,
        vec![
            std::ffi::OsString::from("git"),
            std::ffi::OsString::from("status"),
            std::ffi::OsString::from("--short"),
        ]
    );
    assert_eq!(status_request.cwd, Some(dir));
    Ok(())
}

#[test]
fn only_an_authorized_command_grants_one_restart() {
    let status = Arc::new(ReviewStatus::new(3));
    let session = SessionId::new_v7();

    assert!(
        !status.take_restart_authorization(session),
        "a session without /review holds no grant: restart must refuse"
    );

    status.authorize_restart(session);
    assert!(
        status.take_restart_authorization(session),
        "the /review command granted one restart"
    );
    assert!(
        !status.take_restart_authorization(session),
        "the grant was consumed; it cannot be replayed"
    );

    let other = SessionId::new_v7();
    status.authorize_restart(session);
    assert!(
        !status.take_restart_authorization(other),
        "a grant is scoped to its session"
    );
    assert!(status.take_restart_authorization(session));
}
#[tokio::test]
async fn closed_sessions_drop_unused_restart_grants() -> Result<(), Box<dyn std::error::Error>> {
    let status = Arc::new(ReviewStatus::new(3));
    let session = SessionId::new_v7();
    status.authorize_restart(session);

    let services: Arc<dyn dal_agent::ext::Services> = Arc::new(FakeReviewServices::default());
    let cx = HookCx::for_test(services, session, None);
    let hook = super::SessionEndHook {
        status: Arc::clone(&status),
    };
    hook.call(
        SessionEnd {
            session,
            reason: "close".into(),
        },
        cx,
    )
    .await?;

    assert!(
        !status.take_restart_authorization(session),
        "a closed session cannot retain a restart grant"
    );
    Ok(())
}

/// Seeds `max_rounds` non-converged rounds of one capped session.
fn seed_capped_records(
    fake: &FakeReviewServices,
    session: SessionId,
    max_rounds: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut records = fake
        .records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let bodies = records.entry("review".to_owned()).or_default();
    for round in 1..=max_rounds {
        bodies.push(dal_core::RawJson::parse(&format!(
            r#"{{"session":"{session}","round":{round},"verdict":"findings","new_count":1,"findings":[{{"path":"src/lib.rs","line":7,"severity":"major","title":"Unchecked index","detail":"It can panic."}}]}}"#
        ))?);
    }
    Ok(())
}

#[tokio::test]
async fn the_command_grant_restarts_one_capped_session_and_is_spent()
-> Result<(), Box<dyn std::error::Error>> {
    let cfg = ReviewConfig::default();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = FakeReviewServices::default();
    let session = SessionId::new_v7();
    seed_capped_records(&fake, session, cfg.max_rounds)?;
    fake.runs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend([exited_run(""), exited_run("")]);
    let services: Arc<dyn dal_agent::ext::Services> = Arc::new(fake);
    let cx = ToolCx::for_test(Arc::clone(&services));
    let refused = outcome_text(review_round(&cfg, &status, &cx, None, RoundRequest::Restart).await);
    assert!(refused.contains("reached the cap of"), "{refused}");

    status.authorize_restart(cx.session());
    let started = outcome_text(review_round(&cfg, &status, &cx, None, RoundRequest::Restart).await);
    assert_eq!(started, "No changes to review.");

    let spent = outcome_text(review_round(&cfg, &status, &cx, None, RoundRequest::Restart).await);
    assert!(spent.contains("reached the cap of"), "{spent}");
    Ok(())
}

#[tokio::test]
async fn a_continue_call_consumes_the_grant_so_a_later_restart_refuses()
-> Result<(), Box<dyn std::error::Error>> {
    let cfg = ReviewConfig::default();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = FakeReviewServices::default();
    let session = SessionId::new_v7();
    seed_capped_records(&fake, session, cfg.max_rounds)?;
    let services: Arc<dyn dal_agent::ext::Services> = Arc::new(fake);
    let cx = ToolCx::for_test(Arc::clone(&services));

    status.authorize_restart(cx.session());
    // The continue call itself stops at the cap and eats the grant.
    let stopped =
        outcome_text(review_round(&cfg, &status, &cx, None, RoundRequest::Continue).await);
    assert!(stopped.contains("reached the cap of"), "{stopped}");

    let refused = outcome_text(review_round(&cfg, &status, &cx, None, RoundRequest::Restart).await);
    assert!(refused.contains("reached the cap of"), "{refused}");
    assert!(
        refused.contains("Stop and tell the user"),
        "the refused restart must read as a continuation, not a fresh session: {refused}"
    );
    Ok(())
}

/// One successful empty git output for the in-crate host.
fn exited_run(stdout: &str) -> dal_core::RunOutput {
    dal_core::RunOutput {
        status: dal_core::ExitStatusKind::Exited(0),
        stdout_tail: Vec::new(),
        stdout_prefix: stdout.as_bytes().to_vec(),
        stdout_prefix_overflowed: false,
        stderr_tail: Vec::new(),
        log: None,
    }
}

#[test]
fn a_cap_marks_the_cut_inside_the_cap_and_leaves_fitting_text_whole() {
    let exact = "x".repeat(MAX_ERROR_BYTES);
    assert_eq!(cap_with_marker(&exact, MAX_ERROR_BYTES), exact.as_str());
    let one_over = format!("{exact}y");
    let capped = cap_with_marker(&one_over, MAX_ERROR_BYTES);
    assert_eq!(capped.len(), MAX_ERROR_BYTES);
    assert!(capped.ends_with(TRUNCATION_MARKER));
    let wide = "é".repeat(MAX_ERROR_BYTES);
    let capped = cap_with_marker(&wide, MAX_ERROR_BYTES);
    assert!(capped.len() <= MAX_ERROR_BYTES);
    assert!(capped.ends_with(TRUNCATION_MARKER));
    assert!(capped.starts_with("éé"));
}

const CLEAN_REPLY: &str = r#"{"verdict":"clean","findings":[],"summary":"ok"}"#;

/// The untracked files pushing `git status --short` past `MAX_DIFF_BYTES`.
const OVERFLOW_FILES: usize = 2500;

fn text_events(text: &str, usage: Option<(u64, u64)>) -> Vec<dal_core::StreamEvent> {
    let mut events = vec![dal_core::StreamEvent::Delta {
        channel: dal_core::StreamChannel::Text,
        text: text.into(),
    }];
    if let Some((input_tokens, output_tokens)) = usage {
        events.push(dal_core::StreamEvent::Usage(dal_core::Usage {
            input_tokens,
            cached_input_tokens: 0,
            output_tokens,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            premium_requests: None,
            cost_usd: None,
        }));
    }
    events.push(dal_core::StreamEvent::Stop(dal_core::Stop::EndTurn));
    events
}

/// A tool context whose host answers one changed-file `git diff` and `git status`.
fn staged_cx(fake: &Arc<FakeReviewServices>) -> ToolCx<'static> {
    locked(&fake.runs).extend([
        exited_run("diff --git a/x b/x\n+changed\n"),
        exited_run(" M x\n"),
    ]);
    let services: Arc<dyn dal_agent::ext::Services> = fake.clone();
    ToolCx::for_test(services)
}

fn named_route_config() -> ReviewConfig {
    ReviewConfig {
        reviewer_model: "gpt-5.6-luna".into(),
        ..ReviewConfig::default()
    }
}

async fn round(
    cfg: &ReviewConfig,
    status: &Arc<ReviewStatus>,
    cx: &ToolCx<'_>,
    focus: Option<&str>,
) -> String {
    outcome_text(review_round(cfg, status, cx, focus, RoundRequest::Continue).await)
}

fn request_text(request: &dal_core::ModelRequest) -> String {
    let Some(dal_core::ContextItem::User { parts }) = request.context.first() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| match part {
            dal_core::Part::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_reviewer_probes_a_named_route_then_asks_once_and_posts_usage()
-> Result<(), Box<dyn std::error::Error>> {
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, Some((1200, 34))));
    let cx = staged_cx(&fake);

    let text = round(&cfg, &status, &cx, None).await;

    assert_eq!(text, "Review round 1 of 1: clean. No findings.");
    let route = dal_core::ModelRoute::from_id("gpt-5.6-luna");
    assert_eq!(
        locked(&fake.probes).as_slice(),
        std::slice::from_ref(&route)
    );
    let requests = locked(&fake.requests);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, route);
    assert_eq!(requests[0].purpose, dal_core::Purpose::Judge);
    let notices = locked(&fake.notices);
    assert_eq!(notices.len(), 1);
    assert_eq!(&*notices[0].kind, "review");
    assert_eq!(
        &*notices[0].text,
        "review round 1: the reviewer used 1200 input and 34 output tokens"
    );
    assert_eq!(locked(&fake.appended).len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_reply_without_usage_posts_no_notice() {
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
    let cx = staged_cx(&fake);

    let text = round(&cfg, &status, &cx, None).await;

    assert_eq!(text, "Review round 1 of 1: clean. No findings.");
    assert!(locked(&fake.notices).is_empty());
}

#[tokio::test]
async fn an_empty_reviewer_model_reviews_with_the_session_route() {
    let cfg = ReviewConfig::default();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
    let cx = staged_cx(&fake);
    let session_route = dal_core::ModelRoute::Api {
        family: dal_core::Family::Responses,
        model: "gpt-6".into(),
    };
    status.set_session_route(cx.session(), Some(session_route.clone()));

    let text = round(&cfg, &status, &cx, None).await;

    assert_eq!(text, "Review round 1 of 1: clean. No findings.");
    assert_eq!(
        locked(&fake.probes).as_slice(),
        std::slice::from_ref(&session_route)
    );
    assert_eq!(locked(&fake.requests)[0].model, session_route);
}

#[tokio::test]
async fn with_no_session_route_and_no_reviewer_model_the_round_fails_by_name() {
    let cfg = ReviewConfig::default();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
    let cx = staged_cx(&fake);

    let text = round(&cfg, &status, &cx, None).await;

    assert_eq!(
        text,
        "reviewer call failed: no session model is selected; set reviewer_model"
    );
    assert!(locked(&fake.probes).is_empty());
    assert!(locked(&fake.requests).is_empty());
    assert!(locked(&fake.appended).is_empty());
}

#[tokio::test]
async fn a_route_without_credentials_fails_the_round_before_any_request() {
    let cfg = ReviewConfig {
        reviewer_model: "ghost".into(),
        ..ReviewConfig::default()
    };
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::NoCredentials;
    let cx = staged_cx(&fake);

    let text = round(&cfg, &status, &cx, None).await;

    assert_eq!(
        text,
        "reviewer call failed: model \"ghost\" has no usable credentials or is unknown"
    );
    assert!(locked(&fake.requests).is_empty());
    assert!(locked(&fake.appended).is_empty());
}

#[tokio::test]
async fn a_provider_failure_returns_the_provider_form_with_a_marked_cause()
-> Result<(), Box<dyn std::error::Error>> {
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Fails("é".repeat(150));
    let cx = staged_cx(&fake);

    let text = round(&cfg, &status, &cx, None).await;

    let cause = text
        .strip_prefix("reviewer call failed: ")
        .ok_or("not the Provider form")?;
    assert!(cause.len() <= MAX_ERROR_BYTES);
    assert!(cause.starts_with("éé"));
    assert!(cause.ends_with(TRUNCATION_MARKER));
    assert!(locked(&fake.appended).is_empty());
    assert!(
        status.payload(cx.session()).is_none(),
        "status must be idle"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_missed_deadline_names_the_timeout_and_appends_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Hangs;
    let cx = staged_cx(&fake);

    // The outer bound is longer than the reviewer deadline: a round with no
    // deadline fails here instead of hanging the test.
    let text = tokio::time::timeout(REVIEWER_DEADLINE * 2, round(&cfg, &status, &cx, None)).await?;

    assert_eq!(
        text,
        format!(
            "reviewer call failed: timed out after {}s",
            REVIEWER_DEADLINE.as_secs()
        )
    );
    assert_eq!(locked(&fake.requests).len(), 1, "the request was sent once");
    assert!(locked(&fake.appended).is_empty());
    assert!(
        status.payload(cx.session()).is_none(),
        "status must be idle"
    );
    Ok(())
}

#[tokio::test]
async fn cancelling_the_reviewer_request_ends_interrupted_and_the_next_call_keeps_the_round()
-> Result<(), Box<dyn std::error::Error>> {
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    seed_capped_records(&fake, SessionId::new_v7(), 1)?;
    *locked(&fake.provider) = Provider::Hangs;
    let cx = staged_cx(&fake);

    let (outcome, ()) = tokio::join!(
        review_round(&cfg, &status, &cx, None, RoundRequest::Continue),
        async {
            tokio::task::yield_now().await;
            cx.cancel().cancel();
        }
    );

    assert_eq!(outcome_text(outcome), "interrupted");
    assert!(
        locked(&fake.appended).is_empty(),
        "a cancelled round appends no record"
    );
    assert!(
        status.payload(cx.session()).is_none(),
        "status must be idle"
    );
    *locked(&fake.provider) = Provider::Replies(text_events(ONE_FINDING, None));
    let next = staged_cx(&fake);
    let text = round(&cfg, &status, &next, None).await;
    assert!(text.starts_with("Review round 2 of 3: findings."), "{text}");
    Ok(())
}

#[tokio::test]
async fn the_tool_focus_is_cut_to_five_hundred_bytes_with_the_marker() {
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
    let cx = staged_cx(&fake);
    let focus = format!("{}TAIL", "a".repeat(MAX_FOCUS_BYTES));

    let _ = round(&cfg, &status, &cx, Some(&focus)).await;

    let sent = request_text(&locked(&fake.requests)[0]);
    let cut = format!(
        "## Focus\n{}{FOCUS_TRUNCATION_MARKER}\n",
        "a".repeat(MAX_FOCUS_BYTES)
    );
    assert!(sent.contains(&cut), "{sent}");
    assert!(!sent.contains("TAIL"));
}

#[tokio::test]
async fn the_record_keeps_a_marked_prefix_while_the_report_keeps_the_whole_detail()
-> Result<(), Box<dyn std::error::Error>> {
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    let detail = "d".repeat(1500);
    let reply = format!(
        r#"{{"verdict":"findings","summary":"s","findings":[{{"path":"p.rs","line":3,"severity":"major","title":"T","detail":"{detail}"}}]}}"#
    );
    *locked(&fake.provider) = Provider::Replies(text_events(&reply, None));
    let cx = staged_cx(&fake);

    let text = round(&cfg, &status, &cx, None).await;

    assert!(text.contains(&detail), "the report carries the full detail");
    let appended = locked(&fake.appended);
    let record: ReviewRecord = sonic_rs::from_str(&appended[0])?;
    assert_eq!(record.findings[0].detail.len(), MAX_STORED_DETAIL_BYTES);
    assert!(record.findings[0].detail.ends_with(TRUNCATION_MARKER));
    Ok(())
}

/// The one test that drives the real git executable.
#[expect(
    clippy::disallowed_methods,
    reason = "the boundary test intentionally runs the real git executable"
)]
mod real_git {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Ignores the user's and the system's git configuration, so a
    /// `diff.mnemonicPrefix` or signing setting cannot change the output.
    const HERMETIC_GIT: [(&str, &str); 2] = [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ];

    /// A temporary directory removed on drop.
    pub(super) struct TempRepo {
        pub(super) path: PathBuf,
    }

    impl TempRepo {
        pub(super) fn new() -> std::io::Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "dalgona-review-git-{}",
                dal_core::SessionId::new_v7()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        pub(super) fn git(&self, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
            let output = Command::new("git")
                .args([
                    "-c",
                    "user.name=review",
                    "-c",
                    "user.email=review@example.invalid",
                ])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .envs(HERMETIC_GIT)
                .current_dir(&self.path)
                .output()?;
            if !output.status.success() {
                return Err(format!(
                    "git {args:?} failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }
            Ok(())
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Runs the requested argv in `dir`, keeping at most `stdout_prefix_limit`
    /// stdout bytes and flagging an overflow, like the host `run` service.
    pub(super) fn run(dir: &Path, req: &dal_core::RunRequest) -> dal_core::RunOutput {
        let failed = |message: String| dal_core::RunOutput {
            status: dal_core::ExitStatusKind::Exited(127),
            stdout_tail: Vec::new(),
            stdout_prefix: Vec::new(),
            stdout_prefix_overflowed: false,
            stderr_tail: message.into_bytes(),
            log: None,
        };
        let Some((program, args)) = req.argv.split_first() else {
            return failed("empty argv".to_owned());
        };
        let output = Command::new(program)
            .args(args)
            .current_dir(dir)
            .envs(HERMETIC_GIT)
            .envs(
                req.env
                    .iter()
                    .map(|(key, value)| (key.as_ref(), value.as_ref())),
            )
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => return failed(error.to_string()),
        };
        let limit = usize::try_from(req.stdout_prefix_limit).unwrap_or(usize::MAX);
        dal_core::RunOutput {
            status: dal_core::ExitStatusKind::Exited(output.status.code().unwrap_or(-1)),
            stdout_tail: Vec::new(),
            stdout_prefix_overflowed: output.stdout.len() > limit,
            stdout_prefix: output.stdout.into_iter().take(limit).collect(),
            stderr_tail: output.stderr,
            log: None,
        }
    }
}

#[tokio::test]
async fn a_real_git_repository_hands_the_reviewer_the_modified_diff()
-> Result<(), Box<dyn std::error::Error>> {
    let repo = real_git::TempRepo::new()?;
    repo.git(&["init", "--quiet"])?;
    std::fs::write(repo.path.join("greeting.txt"), "hello\n")?;
    repo.git(&["add", "greeting.txt"])?;
    repo.git(&["commit", "--quiet", "-m", "baseline"])?;
    std::fs::write(repo.path.join("greeting.txt"), "hello, review\n")?;

    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
    *locked(&fake.repo) = Some(repo.path.clone());
    let services: Arc<dyn dal_agent::ext::Services> = fake.clone();
    let cx = ToolCx::for_test(services);

    let text = round(&cfg, &status, &cx, None).await;

    assert_eq!(text, "Review round 1 of 1: clean. No findings.");
    let sent = request_text(&locked(&fake.requests)[0]);
    assert!(
        sent.starts_with("## Diff\ndiff --git a/greeting.txt b/greeting.txt\n"),
        "{sent}"
    );
    assert!(sent.contains("\n-hello\n+hello, review\n"), "{sent}");
    assert!(sent.contains("## Status\n M greeting.txt\n"), "{sent}");
    Ok(())
}

/// Private text a stored record can carry. A decode diagnostic that quotes
/// stored bytes would put it in the tool result.
const STORED_SENTINEL: &str = "SENTINEL-private-stored-text";

/// The valid first record that every undecodable case follows.
const STORED_FIRST_RECORD: &str = r#"{"session":"019263a5-7c1a-7000-8000-000000000001","round":1,"verdict":"findings","new_count":1,"findings":[{"path":"src/lib.rs","line":7,"severity":"major","title":"Unchecked index","detail":"It can panic."}]}"#;

/// Stored records that are each one JSON value, so they pass the store
/// boundary, but fail the typed decode with a message that quotes
/// [`STORED_SENTINEL`]: an unknown variant and a wrongly typed value, each at
/// the top level and inside a finding.
fn undecodable_stored_records() -> [String; 4] {
    let session = "019263a5-7c1a-7000-8000-000000000001";
    let record = |round: &str, verdict: &str, findings: &str| {
        format!(
            r#"{{"session":"{session}","round":{round},"verdict":"{verdict}","new_count":1,"findings":[{findings}]}}"#
        )
    };
    let finding = |line: &str, severity: &str| {
        format!(
            r#"{{"path":"src/lib.rs","line":{line},"severity":"{severity}","title":"t","detail":"d"}}"#
        )
    };
    let quoted_sentinel = format!("\"{STORED_SENTINEL}\"");
    [
        record("1", STORED_SENTINEL, ""),
        record(&quoted_sentinel, "findings", ""),
        record("1", "findings", &finding(&quoted_sentinel, "major")),
        record("1", "findings", &finding("7", STORED_SENTINEL)),
    ]
}

/// Seeds the valid first record, then `body`.
fn seed_records(fake: &FakeReviewServices, body: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut records = locked(&fake.records);
    let bodies = records.entry("review".to_owned()).or_default();
    bodies.push(dal_core::RawJson::parse(STORED_FIRST_RECORD)?);
    bodies.push(dal_core::RawJson::parse(body)?);
    Ok(())
}

/// The message of a refused round; any other outcome fails the test.
fn refusal_text(
    outcome: dal_agent::ext::ToolOutcome,
) -> Result<String, Box<dyn std::error::Error>> {
    match outcome {
        dal_agent::ext::ToolOutcome::Err(error) => Ok(error.to_string()),
        other => {
            let message = outcome_text(other);
            Err(format!("the round was not refused: {message}").into())
        }
    }
}

/// Nothing past the stored-record decode ran: both git outputs staged by
/// `staged_cx` are still queued, no route was probed, no reviewer request was
/// sent, and no record was appended.
fn assert_round_stopped_at_decode(fake: &FakeReviewServices) {
    assert_eq!(locked(&fake.runs).len(), 2, "a git call ran");
    assert!(
        locked(&fake.probes).is_empty(),
        "the reviewer route was probed"
    );
    assert!(
        locked(&fake.requests).is_empty(),
        "the reviewer ran over an undecodable record"
    );
    assert!(
        locked(&fake.appended).is_empty(),
        "a refused round appended a record"
    );
}

#[tokio::test]
async fn an_undecodable_stored_record_refuses_the_round_without_echoing_its_content()
-> Result<(), Box<dyn std::error::Error>> {
    for body in undecodable_stored_records() {
        let raw_failure = sonic_rs::from_str::<ReviewRecord>(&body)
            .err()
            .ok_or("the seeded record must not decode")?;
        assert!(
            raw_failure.to_string().contains(STORED_SENTINEL),
            "the case must be one whose raw decode message quotes the stored text: {raw_failure}"
        );
        let cfg = named_route_config();
        let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
        let fake = Arc::new(FakeReviewServices::default());
        seed_records(&fake, &body)?;
        *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
        let cx = staged_cx(&fake);

        let outcome = review_round(&cfg, &status, &cx, None, RoundRequest::Continue).await;
        let refused = refusal_text(outcome)?;

        assert!(
            refused.starts_with("stored review record 2 did not decode: "),
            "{refused}"
        );
        assert!(
            !refused.contains(STORED_SENTINEL),
            "the refusal echoes stored content: {refused}"
        );
        assert!(
            refused.contains(&format!("line {}", raw_failure.line())),
            "{refused}"
        );
        assert!(
            refused.contains(&format!("column {}", raw_failure.column())),
            "{refused}"
        );
        assert_round_stopped_at_decode(&fake);
    }
    Ok(())
}

#[tokio::test]
async fn an_undecodable_stored_record_refuses_a_granted_restart_and_spends_the_grant()
-> Result<(), Box<dyn std::error::Error>> {
    let [body, ..] = undecodable_stored_records();
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    seed_records(&fake, &body)?;
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
    let cx = staged_cx(&fake);
    status.authorize_restart(cx.session());

    let outcome = review_round(&cfg, &status, &cx, None, RoundRequest::Restart).await;
    let refused = refusal_text(outcome)?;

    assert!(
        refused.starts_with("stored review record 2 did not decode: "),
        "{refused}"
    );
    assert_round_stopped_at_decode(&fake);
    assert!(
        !status.take_restart_authorization(cx.session()),
        "a refused call still spends the restart grant"
    );
    Ok(())
}

#[tokio::test]
async fn stored_records_with_unknown_members_still_decode_and_the_round_continues()
-> Result<(), Box<dyn std::error::Error>> {
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    let session = SessionId::new_v7();
    locked(&fake.records)
        .entry("review".to_owned())
        .or_default()
        .push(dal_core::RawJson::parse(&format!(
            r#"{{"session":"{session}","round":1,"verdict":"findings","new_count":1,"findings":[{{"path":"src/lib.rs","line":7,"severity":"major","title":"unchecked index","detail":"It can panic.","confidence":{{"score":[1,2,{{"x":null}}]}}}}],"written_by":"a newer release"}}"#
        ))?);
    *locked(&fake.provider) = Provider::Replies(text_events(ONE_FINDING, None));
    let cx = staged_cx(&fake);

    let text = round(&cfg, &status, &cx, None).await;

    assert!(text.starts_with("Review round 2 of 3: findings."), "{text}");
    let sent = request_text(&locked(&fake.requests)[0]);
    assert!(
        sent.contains("src/lib.rs\tunchecked index"),
        "the stored finding must reach the reviewer as a prior finding: {sent}"
    );
    let appended = locked(&fake.appended);
    assert_eq!(appended.len(), 1);
    let record: ReviewRecord = sonic_rs::from_str(&appended[0])?;
    assert_eq!(record.session, session);
    assert_eq!(record.round, 2);
    Ok(())
}

#[tokio::test]
async fn a_round_is_refused_while_a_job_runs() -> Result<(), Box<dyn std::error::Error>> {
    use crate::orchestration::View;
    use crate::orchestration::live::Published;
    use crate::orchestration::monitor::status::{StatusPayload, inflight_counts};
    let cfg = ReviewConfig::default();
    let view = View::default();
    let status = Arc::new(ReviewStatus::with_view(cfg.max_rounds, view.clone()));
    let fake = FakeReviewServices::default();
    let services: Arc<dyn dal_agent::ext::Services> = Arc::new(fake);
    let cx = ToolCx::for_test(Arc::clone(&services));
    let session = cx.session();
    let live = inflight_counts(1, 0, 0, false, false);
    view.open(session).publish(Published {
        inflight: crate::orchestration::InflightCounts {
            jobs: live.jobs,
            monitors: live.monitors,
            asks: live.asks,
            goal_timer: live.goal_timer > 0,
            loop_guard: live.loop_guard > 0,
        },
        goal: None,
        status: StatusPayload::new(
            crate::orchestration::ControllerMode::Run,
            false,
            live,
            0,
            None,
        ),
    });
    let refused =
        outcome_text(review_round(&cfg, &status, &cx, None, RoundRequest::Continue).await);
    assert!(
        refused.contains("orchestration work is still live"),
        "{refused}"
    );
    assert!(refused.contains("1 job"), "{refused}");
    Ok(())
}

#[tokio::test]
async fn a_malformed_stored_record_keeps_history_and_runs_no_provider_or_append()
-> Result<(), Box<dyn std::error::Error>> {
    let [body, ..] = undecodable_stored_records();
    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    seed_records(&fake, &body)?;
    let before: Vec<String> = locked(&fake.records)
        .get("review")
        .map(|bodies| bodies.iter().map(|body| body.as_str().to_owned()).collect())
        .unwrap_or_default();
    assert_eq!(before.len(), 2);
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
    let cx = staged_cx(&fake);

    let outcome = review_round(&cfg, &status, &cx, None, RoundRequest::Continue).await;
    let refused = refusal_text(outcome)?;

    assert!(
        refused.starts_with("stored review record 2 did not decode: "),
        "{refused}"
    );
    assert!(
        !refused.contains(STORED_SENTINEL),
        "the refusal echoes stored content: {refused}"
    );
    assert_round_stopped_at_decode(&fake);
    assert!(
        locked(&fake.notices).is_empty(),
        "a refused round posts no notice"
    );
    // The stored history is byte-identical: the same two bodies remain, the
    // first still decodes at round 1, the second still refuses.
    {
        let kept = locked(&fake.records);
        let bodies = kept
            .get("review")
            .ok_or("a refused round must keep the stored history")?;
        let after: Vec<String> = bodies.iter().map(|body| body.as_str().to_owned()).collect();
        assert_eq!(after, before, "a refused round must not rewrite history");
        let first: ReviewRecord = sonic_rs::from_str(bodies[0].as_str())?;
        assert_eq!(first.round, 1);
        assert!(
            sonic_rs::from_str::<ReviewRecord>(bodies[1].as_str()).is_err(),
            "a refused round must not repair the undecodable record"
        );
    }
    // Adjacent valid boundary: the same valid first record alone continues
    // at round 2, so the refusal above is conditional, not unconditional.
    let clean = Arc::new(FakeReviewServices::default());
    locked(&clean.records)
        .entry("review".to_owned())
        .or_default()
        .push(dal_core::RawJson::parse(STORED_FIRST_RECORD)?);
    *locked(&clean.provider) = Provider::Replies(text_events(ONE_FINDING, None));
    let next = staged_cx(&clean);
    let outcome = review_round(&cfg, &status, &next, None, RoundRequest::Continue).await;
    assert!(
        matches!(outcome, dal_agent::ext::ToolOutcome::Ok(_)),
        "the valid adjacent history must continue the round"
    );
    let first_stored: ReviewRecord = sonic_rs::from_str(STORED_FIRST_RECORD)?;
    assert_eq!(locked(&clean.requests).len(), 1);
    let appended = locked(&clean.appended);
    assert_eq!(appended.len(), 1);
    let continued: ReviewRecord = sonic_rs::from_str(&appended[0])?;
    assert_eq!(continued.session, first_stored.session);
    assert_eq!(continued.round, 2);
    Ok(())
}

#[tokio::test]
async fn an_over_limit_real_git_status_refuses_before_the_reviewer_runs()
-> Result<(), Box<dyn std::error::Error>> {
    let repo = real_git::TempRepo::new()?;
    repo.git(&["init", "--quiet"])?;
    std::fs::write(repo.path.join("baseline.txt"), "baseline\n")?;
    repo.git(&["add", "baseline.txt"])?;
    repo.git(&["commit", "--quiet", "-m", "baseline"])?;
    // Push `git status --short` past MAX_DIFF_BYTES with real untracked
    // files: each short line is ~127 bytes, so 2500 files are ~317 kB.
    let pad = "x".repeat(90);
    let mut overflow_names = Vec::with_capacity(OVERFLOW_FILES);
    for index in 0..OVERFLOW_FILES {
        let name = format!("review-status-overflow-{index:05}-{pad}.txt");
        std::fs::write(repo.path.join(&name), "overflow\n")?;
        overflow_names.push(name);
    }

    let cfg = named_route_config();
    let status = Arc::new(ReviewStatus::new(cfg.max_rounds));
    let fake = Arc::new(FakeReviewServices::default());
    *locked(&fake.provider) = Provider::Replies(text_events(CLEAN_REPLY, None));
    *locked(&fake.repo) = Some(repo.path.clone());
    let services: Arc<dyn dal_agent::ext::Services> = fake.clone();
    let cx = ToolCx::for_test(services);

    let outcome = review_round(&cfg, &status, &cx, None, RoundRequest::Continue).await;
    let refused = refusal_text(outcome)?;

    assert_eq!(
        refused,
        ReviewError::StatusTooLarge {
            limit: MAX_DIFF_BYTES
        }
        .to_string()
    );
    assert!(
        locked(&fake.probes).is_empty(),
        "an over-limit status must refuse before the route probe"
    );
    assert!(
        locked(&fake.requests).is_empty(),
        "an over-limit status must refuse before the reviewer request"
    );
    assert!(
        locked(&fake.appended).is_empty(),
        "a refused round appends no record"
    );
    assert!(
        locked(&fake.notices).is_empty(),
        "a refused round posts no notice"
    );
    assert!(
        status.payload(cx.session()).is_none(),
        "status must be idle"
    );
    // Adjacent valid boundary: the same harness with a fitting status runs
    // the reviewer and appends round 1, so the refusal is conditional.
    for name in &overflow_names {
        std::fs::remove_file(repo.path.join(name))?;
    }
    std::fs::write(repo.path.join("baseline.txt"), "baseline, reviewed\n")?;
    let outcome = review_round(&cfg, &status, &cx, None, RoundRequest::Continue).await;
    assert!(
        matches!(outcome, dal_agent::ext::ToolOutcome::Ok(_)),
        "the fitting adjacent status must run the reviewer"
    );
    assert_eq!(locked(&fake.requests).len(), 1);
    let appended = locked(&fake.appended);
    assert_eq!(appended.len(), 1);
    let continued: ReviewRecord = sonic_rs::from_str(&appended[0])?;
    assert_eq!(continued.round, 1);
    Ok(())
}
