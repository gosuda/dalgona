#![expect(clippy::expect_used, reason = "integration tests fail loudly")]

//! Vocabulary wire proofs: command, request, update, and view shapes.
//!
//! Every test goes
//! through the public `dal_core::` API; `sonic_rs` is the wire codec.

use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::time::Duration;

use dal_core::{
    Answer, ApprovalMode, AutoCompaction, CallGrant, CancelScope, Choice, Command, EntryId,
    EntryKind, Expect, ExportFormat, Gen, JobEnd, JobId, ListQuery, Mode, Owner, Page, PageReq,
    PageReqError, Preview, Question, RawJson, Rejection, Reply, Request, RequestId,
    ResolutionCause, Save, Seq, SessionId, SessionInfo, SessionSummary, SettingsView, Stats,
    ThinkingLevel, TreeDelta, TurnCause, TurnId, TurnState, Update, UpdateKind, UsageView,
    Workspace,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn turn() -> TurnId {
    TurnId::new(NonZeroU64::MIN)
}

fn entry() -> EntryId {
    EntryId::new(NonZeroU64::MIN)
}

fn nonzero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("fixture limit is nonzero")
}

// --- PageReq typed boundary ---------------------------------------

#[test]
fn page_req_deserialization_enforces_max_limit() -> TestResult {
    for wire in [r#"{"limit":5000}"#, r#"{"limit":1001}"#] {
        let error = sonic_rs::from_str::<PageReq>(wire)
            .expect_err("over-limit page request must be rejected at decode");
        let text = error.to_string();
        assert!(
            text.contains("exceeds maximum"),
            "unexpected error text for {wire}: {text}"
        );
    }
    assert!(sonic_rs::from_str::<PageReq>(r#"{"limit":0}"#).is_err());
    assert!(sonic_rs::from_str::<PageReq>("{}").is_err());
    assert!(sonic_rs::from_str::<PageReq>(r#"{"before":1}"#).is_err());

    let max = sonic_rs::from_str::<PageReq>(r#"{"limit":1000}"#)?;
    assert_eq!(max.limit.get(), 1_000);
    assert_eq!(max.before, None);

    let one = sonic_rs::from_str::<PageReq>(r#"{"limit":1,"before":5}"#)?;
    assert_eq!(one.limit.get(), 1);
    assert_eq!(
        one.before,
        Some(EntryId::new(NonZeroU64::new(5).expect("5 is nonzero")))
    );
    Ok(())
}

#[test]
fn page_req_decode_matches_constructor_bounds() {
    assert_eq!(PageReq::DEFAULT_LIMIT, 200);
    assert_eq!(PageReq::MAX_LIMIT, 1_000);
    assert_eq!(PageReq::default().limit.get(), PageReq::DEFAULT_LIMIT);
    assert!(PageReq::new(nonzero(1_000), None).is_ok());
    assert_eq!(
        PageReq::new(nonzero(1_001), None),
        Err(PageReqError::LimitTooLarge {
            limit: 1_001,
            max: PageReq::MAX_LIMIT,
        })
    );
}

#[test]
fn page_req_wire_keeps_camel_case_names() -> TestResult {
    let encoded = sonic_rs::to_string(&PageReq::default())?;
    assert_eq!(encoded, r#"{"limit":200,"before":null}"#);
    assert_eq!(sonic_rs::from_str::<PageReq>(&encoded)?, PageReq::default());

    let before = EntryId::new(NonZeroU64::new(7).expect("7 is nonzero"));
    let req = PageReq::new(nonzero(50), Some(before))?;
    let encoded = sonic_rs::to_string(&req)?;
    assert_eq!(encoded, r#"{"limit":50,"before":7}"#);
    assert_eq!(sonic_rs::from_str::<PageReq>(&encoded)?, req);
    Ok(())
}

// --- command vocabulary -------------------------------------------

#[test]
fn command_rejects_unknown_type_tags() {
    for wire in [
        r#"{"type":"frobnicate"}"#,
        r#"{"type":"prompt2","expect":"idle","content":[]}"#,
        r#"{"type":"Prompt","expect":"idle","content":[]}"#,
        r#"{"type":"","content":[]}"#,
    ] {
        assert!(
            sonic_rs::from_str::<Command>(wire).is_err(),
            "unknown command tag accepted: {wire}"
        );
    }
}

#[test]
fn prompt_and_steer_preserve_empty_content() -> TestResult {
    let prompt = Command::Prompt {
        expect: Expect::Idle,
        content: Vec::new(),
    };
    let encoded = sonic_rs::to_string(&prompt)?;
    assert_eq!(encoded, r#"{"type":"prompt","expect":"idle","content":[]}"#);
    assert_eq!(sonic_rs::from_str::<Command>(&encoded)?, prompt);

    let steer = Command::Steer {
        turn: turn(),
        content: Vec::new(),
    };
    let encoded = sonic_rs::to_string(&steer)?;
    assert_eq!(sonic_rs::from_str::<Command>(&encoded)?, steer);
    Ok(())
}

#[test]
fn expect_and_cancel_scope_round_trip() -> TestResult {
    assert_eq!(sonic_rs::to_string(&Expect::Idle)?, r#""idle""#);
    let after = Expect::After(turn());
    assert_eq!(
        sonic_rs::from_str::<Expect>(&sonic_rs::to_string(&after)?)?,
        after
    );

    let scope = CancelScope::Turn(turn());
    assert_eq!(sonic_rs::to_string(&scope)?, r#"{"type":"turn","turn":1}"#);
    assert_eq!(
        sonic_rs::from_str::<CancelScope>(&sonic_rs::to_string(&scope)?)?,
        scope
    );
    let job_scope = CancelScope::Job(JobId::new_v7());
    assert_eq!(
        sonic_rs::from_str::<CancelScope>(&sonic_rs::to_string(&job_scope)?)?,
        job_scope
    );
    assert!(sonic_rs::from_str::<CancelScope>(r#"{"type":"process","turn":1}"#).is_err());
    Ok(())
}

#[test]
fn every_command_variant_round_trips() -> TestResult {
    let route = dal_core::ModelRoute::synthetic("test/model")?;
    let commands = [
        Command::Prompt {
            expect: Expect::Idle,
            content: Vec::new(),
        },
        Command::CancelQueued { turn: turn() },
        Command::Cancel {
            scope: CancelScope::Turn(turn()),
        },
        Command::SetModel {
            model: route,
            save: Save::SessionOnly,
        },
        Command::SetThinking {
            level: ThinkingLevel::High,
            save: Save::SessionAndDefault,
        },
        Command::SetApproval {
            mode: ApprovalMode::Edits,
            save: Save::SessionOnly,
        },
        Command::SetMode {
            mode: Mode::EvalFirst,
            save: Save::SessionOnly,
        },
        Command::Compact { focus: None },
        Command::MoveLeaf(entry()),
        Command::Fork(entry()),
        Command::Clone,
        Command::Rename("renamed".into()),
        Command::Run {
            name: "tree".into(),
            args: String::new().into_boxed_str(),
            expected: None,
        },
        Command::SetScopedModels(vec!["a/b".into()]),
        Command::Export {
            path: None,
            format: ExportFormat::Markdown,
        },
        Command::ReloadPlugins,
    ];
    for command in commands {
        let encoded = sonic_rs::to_string(&command)?;
        assert_eq!(
            sonic_rs::from_str::<Command>(&encoded)?,
            command,
            "{encoded}"
        );
    }
    Ok(())
}

// --- replies and rejections ----------------------------------------

#[test]
fn every_rejection_has_distinct_nonempty_display() {
    let cases = [
        Rejection::WrongTurn {
            expected: Expect::Idle,
            actual: TurnState::Idle,
        },
        Rejection::SessionClosed,
        Rejection::BusyTurn,
        Rejection::Compacting,
        Rejection::SteerFull,
        Rejection::Denied {
            reason: dal_core::DenyReason::WakeLimit,
        },
        Rejection::Invalid {
            reason: "bad entry id".into(),
        },
    ];
    let mut seen = Vec::new();
    for rejection in cases {
        let text = rejection.to_string();
        assert!(!text.is_empty(), "{rejection:?} has empty display");
        assert!(
            !seen.contains(&text),
            "duplicate rejection display text: {text}"
        );
        seen.push(text);
    }
}

#[test]
fn replies_use_snake_case_tags_and_reject_unknown() -> TestResult {
    let accepted = Reply::Accepted {
        turn: turn(),
        message_id: entry(),
    };
    assert_eq!(
        sonic_rs::to_string(&accepted)?,
        r#"{"type":"accepted","turn":1,"messageId":1}"#
    );
    assert_eq!(
        sonic_rs::from_str::<Reply>(r#"{"type":"accepted","turn":1,"messageId":1}"#)?,
        accepted
    );
    assert_eq!(
        sonic_rs::to_string(&Reply::Queued { turn: None })?,
        r#"{"type":"queued"}"#
    );
    let started = Reply::Started(JobId::new_v7());
    let encoded = sonic_rs::to_string(&started)?;
    assert!(encoded.starts_with(r#"{"type":"started","job":""#));
    assert_eq!(sonic_rs::from_str::<Reply>(&encoded)?, started);
    assert!(sonic_rs::from_str::<Reply>(r#"{"type":"bogus"}"#).is_err());
    Ok(())
}

// --- request vocabulary --------------------------------------------

fn preview() -> Preview {
    Preview {
        title: "Patch".into(),
        body: "Update the selected files.".into(),
        digest: Some([0x5a; 32]),
    }
}

#[test]
fn every_question_shape_round_trips() -> TestResult {
    let call = dal_core::CallId::new("call-1");
    let questions = [
        Question::Approval {
            tool: "exec".into(),
            preview: preview(),
            grant: None,
            call: Some(call),
        },
        Question::Grant {
            ext: "web".into(),
            origin: "user".into(),
            capabilities: vec!["net".into()],
            detail: Some("exact decl".into()),
        },
        Question::Select {
            prompt: "Choose".into(),
            options: vec![Choice {
                label: "main".into(),
                description: None,
            }],
            multi: true,
            preview: None,
        },
        Question::Confirm {
            text: "Continue?".into(),
        },
        Question::Text {
            prompt: "Message".into(),
            placeholder: None,
        },
    ];
    for question in questions {
        let encoded = sonic_rs::to_string(&question)?;
        assert_eq!(
            sonic_rs::from_str::<Question>(&encoded)?,
            question,
            "{encoded}"
        );
    }
    Ok(())
}

#[test]
fn approval_call_is_optional_absent_vs_present() -> TestResult {
    let without_call = Question::Approval {
        tool: "exec".into(),
        preview: preview(),
        grant: None,
        call: None,
    };
    let encoded = sonic_rs::to_string(&without_call)?;
    assert!(!encoded.contains("\"call\""), "{encoded}");
    let decoded: Question = sonic_rs::from_str(&encoded)?;
    assert_eq!(decoded, without_call);

    let with_call = Question::Approval {
        tool: "exec".into(),
        preview: preview(),
        grant: None,
        call: Some(dal_core::CallId::new("call-1")),
    };
    let encoded = sonic_rs::to_string(&with_call)?;
    assert!(encoded.contains(r#""call":"call-1""#), "{encoded}");
    assert_eq!(sonic_rs::from_str::<Question>(&encoded)?, with_call);
    Ok(())
}

#[test]
fn request_round_trips_with_owner_and_default() -> TestResult {
    let request = Request {
        id: RequestId::new_v7(),
        turn: Some(turn()),
        owner: Owner::Extension {
            name: "web".into(),
            origin: "user".into(),
        },
        question: Question::Confirm {
            text: "Continue?".into(),
        },
        timeout: Duration::from_secs(30),
        default: Answer::Cancel,
    };
    let encoded = sonic_rs::to_string(&request)?;
    assert_eq!(sonic_rs::from_str::<Request>(&encoded)?, request);
    Ok(())
}

#[test]
fn answer_value_preserves_raw_bytes() -> TestResult {
    for payload in [
        r#"{ "z" : 1e+03, "a":9007199254740993123456789 }"#,
        r#"["one", 2]"#,
        "-0.0",
        "null",
    ] {
        let answer = Answer::Value(RawJson::parse(payload)?);
        let wire = format!(r#"{{"value":{payload},"type":"value"}}"#);
        let decoded = sonic_rs::from_str::<Answer>(&wire)?;
        let Answer::Value(raw) = &decoded else {
            return Err("expected a value answer".into());
        };
        assert_eq!(raw.as_str(), payload, "raw text changed");
        let encoded = sonic_rs::to_string(&decoded)?;
        assert!(encoded.contains(payload), "payload changed: {encoded}");
        assert_eq!(answer, decoded);
    }
    Ok(())
}

#[test]
fn unknown_question_answer_and_owner_tags_rejected() -> TestResult {
    for wire in [
        r#"{"type":"bogus","text":"x"}"#,
        r#"{"type":"Approval","text":"x"}"#,
    ] {
        let error =
            sonic_rs::from_str::<Question>(wire).expect_err("unknown question tag accepted");
        assert!(error.to_string().contains("bogus") || error.to_string().contains("Approval"));
    }
    assert!(sonic_rs::from_str::<Answer>(r#"{"type":"bogus"}"#).is_err());
    assert!(sonic_rs::from_str::<Answer>(r#"{"type":"value"}"#).is_err());
    assert!(sonic_rs::from_str::<Owner>(r#"{"type":"bogus"}"#).is_err());
    assert_eq!(
        sonic_rs::from_str::<Owner>(r#"{"type":"core"}"#)?,
        Owner::Core
    );
    Ok(())
}

// --- call grant -----------------------------------------------------

#[test]
fn call_grant_until_is_transparent_job_id() -> TestResult {
    let job = JobId::new_v7();
    let end = JobEnd(job);
    assert_eq!(
        sonic_rs::to_string(&end)?,
        format!(r#""{job}""#),
        "JobEnd must stay a transparent job id on the wire"
    );
    assert_eq!(
        sonic_rs::from_str::<JobEnd>(&sonic_rs::to_string(&end)?)?,
        end
    );

    let grant = CallGrant {
        argv_prefix: "git".into(),
        roots: vec![PathBuf::from("/workspace")],
        until: end,
    };
    let encoded = sonic_rs::to_string(&grant)?;
    let decoded: CallGrant = sonic_rs::from_str(&encoded)?;
    assert_eq!(decoded, grant);
    assert_eq!(decoded.until.0, job, "grant must not outlive its job id");
    Ok(())
}

// --- updates ---------------------------------------------------------

#[test]
fn update_contrasts_unknown_kind_with_rejected_known_kind() -> TestResult {
    let unknown: Update = sonic_rs::from_str(
        r#"{"gen":1,"seq":2,"kind":{"type":"future_kind","payload":{"value":7}}}"#,
    )?;
    assert!(matches!(unknown.kind, UpdateKind::Unknown));

    let ignored: Update = sonic_rs::from_str(
        r#"{"gen":1,"seq":1,"kind":{"type":"turn_started","turn":1,"cause":"user"},"future":true}"#,
    )?;
    assert!(matches!(
        ignored.kind,
        UpdateKind::TurnStarted {
            turn: _,
            cause: TurnCause::User
        }
    ));

    for wire in [
        r#"{"gen":1,"seq":1,"kind":{"type":"turn_started","cause":"user"}}"#,
        r#"{"gen":1,"seq":1,"kind":{"type":"turn_started","turn":0,"cause":"user"}}"#,
        r#"{"gen":1,"seq":1,"kind":{"type":"turn_started","turn":1,"cause":"naptime"}}"#,
    ] {
        assert!(
            sonic_rs::from_str::<Update>(wire).is_err(),
            "malformed known kind accepted: {wire}"
        );
    }
    Ok(())
}

#[test]
fn update_sequences_reject_zero_and_kinds_use_wire_names() -> TestResult {
    let started = UpdateKind::TurnStarted {
        turn: turn(),
        cause: TurnCause::FollowUp,
    };
    assert_eq!(
        sonic_rs::to_string(&started)?,
        r#"{"type":"turn_started","turn":1,"cause":"follow_up"}"#
    );
    for (cause, wire) in [
        (TurnCause::User, r#""user""#),
        (TurnCause::Wake, r#""wake""#),
        (TurnCause::FollowUp, r#""follow_up""#),
    ] {
        assert_eq!(sonic_rs::to_string(&cause)?, wire);
        assert_eq!(sonic_rs::from_str::<TurnCause>(wire)?, cause);
    }
    for (cause, wire) in [
        (ResolutionCause::Answered, r#""answered""#),
        (ResolutionCause::TimedOut, r#""timed_out""#),
        (ResolutionCause::Cancelled, r#""cancelled""#),
        (ResolutionCause::Withdrawn, r#""withdrawn""#),
        (ResolutionCause::NoController, r#""no_controller""#),
    ] {
        assert_eq!(sonic_rs::to_string(&cause)?, wire);
        assert_eq!(sonic_rs::from_str::<ResolutionCause>(wire)?, cause);
    }

    let update = Update {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::MIN),
        kind: started,
    };
    let encoded = sonic_rs::to_string(&update)?;
    assert_eq!(sonic_rs::from_str::<Update>(&encoded)?, update);
    for wire in [
        r#"{"gen":0,"seq":1,"kind":{"type":"turn_started","turn":1,"cause":"user"}}"#,
        r#"{"gen":1,"seq":0,"kind":{"type":"turn_started","turn":1,"cause":"user"}}"#,
    ] {
        assert!(
            sonic_rs::from_str::<Update>(wire).is_err(),
            "zero sequence accepted: {wire}"
        );
    }
    Ok(())
}

// --- views ------------------------------------------------------------

#[test]
fn turn_state_uses_state_tag() -> TestResult {
    assert_eq!(
        sonic_rs::to_string(&TurnState::Idle)?,
        r#"{"state":"idle"}"#
    );
    let running = TurnState::Running { turn: turn() };
    assert_eq!(
        sonic_rs::to_string(&running)?,
        r#"{"state":"running","turn":1}"#
    );
    assert_eq!(
        sonic_rs::from_str::<TurnState>(r#"{"state":"settling","turn":1}"#)?,
        TurnState::Settling { turn: turn() }
    );
    assert!(sonic_rs::from_str::<TurnState>(r#"{"state":"dozing"}"#).is_err());
    Ok(())
}

#[test]
fn session_info_rejects_relative_workspace_at_decode() -> TestResult {
    let session = SessionInfo {
        id: SessionId::new_v7(),
        name: None,
        preview: "latest".into(),
        workspace: Workspace::new(PathBuf::from("/workspace"))?,
        updated_at: jiff::Timestamp::UNIX_EPOCH,
        created_at: None,
        archived: None,
        last_seq: None,
        message_count: None,
    };
    let encoded = sonic_rs::to_string(&session)?;
    assert_eq!(sonic_rs::from_str::<SessionInfo>(&encoded)?, session);

    let relative = format!(
        r#"{{"id":"{}","preview":"latest","workspace":"relative/path","updatedAt":"1970-01-01T00:00:00Z"}}"#,
        session.id
    );
    assert!(
        sonic_rs::from_str::<SessionInfo>(&relative).is_err(),
        "relative workspace accepted at decode"
    );
    Ok(())
}

#[test]
fn view_shapes_use_camel_case_fields() -> TestResult {
    let stats = Stats {
        steers_queued: 1,
        follow_ups_queued: 2,
        retries: 3,
        dropped_observations: 4,
        auto_compaction: AutoCompaction::On,
    };
    assert_eq!(
        sonic_rs::to_string(&stats)?,
        r#"{"steersQueued":1,"followUpsQueued":2,"retries":3,"droppedObservations":4,"autoCompaction":"on"}"#
    );
    assert_eq!(
        sonic_rs::from_str::<Stats>(&sonic_rs::to_string(&stats)?)?,
        stats
    );

    let query = ListQuery {
        limit: Some(50),
        cursor: Some("cursor".into()),
        search: None,
    };
    let encoded = sonic_rs::to_string(&query)?;
    assert!(encoded.contains(r#""search":null"#), "{encoded}");
    assert_eq!(sonic_rs::from_str::<ListQuery>(&encoded)?, query);

    let summary = SessionSummary {
        id: SessionId::new_v7(),
        name: Some("session".into()),
        message_count: 7,
        updated_at: jiff::Timestamp::UNIX_EPOCH,
    };
    let encoded = sonic_rs::to_string(&summary)?;
    assert!(encoded.contains(r#""messageCount":7"#), "{encoded}");
    assert_eq!(sonic_rs::from_str::<SessionSummary>(&encoded)?, summary);

    let delta = TreeDelta {
        added: vec![dal_core::EntryView {
            id: entry(),
            parent: None,
            kind: EntryKind::User { parts: Vec::new() },
        }],
        leaf: Some(entry()),
    };
    assert_eq!(
        sonic_rs::from_str::<TreeDelta>(&sonic_rs::to_string(&delta)?)?,
        delta
    );

    let page: Page<dal_core::EntryView> = Page {
        items: Vec::new(),
        next_before: Some(entry()),
    };
    let encoded = sonic_rs::to_string(&page)?;
    assert!(encoded.contains(r#""nextBefore":1"#), "{encoded}");
    assert_eq!(
        sonic_rs::from_str::<Page<dal_core::EntryView>>(&encoded)?,
        page
    );
    Ok(())
}

#[test]
fn settings_and_usage_views_round_trip() -> TestResult {
    let settings = SettingsView {
        model: None,
        thinking: ThinkingLevel::Off,
        approval: ApprovalMode::Ask,
        mode: Mode::Normal,
        name: None,
        scoped_models: Vec::new(),
    };
    assert_eq!(
        sonic_rs::from_str::<SettingsView>(&sonic_rs::to_string(&settings)?)?,
        settings
    );
    let usage = UsageView {
        usage: dal_core::Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
            premium_requests: None,
        },
        context_tokens: 0,
        context_window: 128_000,
    };
    let encoded = sonic_rs::to_string(&usage)?;
    assert!(encoded.contains(r#""contextTokens":0"#), "{encoded}");
    assert_eq!(sonic_rs::from_str::<UsageView>(&encoded)?, usage);
    Ok(())
}
