// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Review battery tests: config, reply decoding, rounds, rendering, and local git.

use std::collections::BTreeSet;

use dal_core::SessionId;

use super::ReviewStatus;
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
    MAX_ERROR_BYTES, MAX_FINDINGS, MAX_STORED_DETAIL_BYTES, REVIEW_REPLY_FORMAT, ReviewError,
    utf8_prefix,
};
const COMMAND_PROMPT_BASE: &str = "Review the current changes with the review tool. Set its `restart` argument to true: you were asked to review, so a capped session starts a new one.";
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
    assert!(capped.starts_with(utf8_prefix(&diff, MAX_DIFF_BYTES)));
    let (captured_prefix, overflowed) = cap_diff(utf8_prefix(&diff, MAX_DIFF_BYTES), true);
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
    assert!(empty.contains("`restart`"), "{empty}");
    assert!(empty.contains("true"), "{empty}");
    assert!(command_prompt("auth").starts_with(COMMAND_PROMPT_BASE));
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
fn stored_detail_is_a_utf8_safe_five_hundred_byte_prefix() -> Result<(), Box<dyn std::error::Error>>
{
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
