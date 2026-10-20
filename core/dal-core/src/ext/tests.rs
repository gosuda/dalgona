use super::{
    AgentInfo, AgentRefusal, AgentReport, AgentStart, AgentState, AgentsOp, AgentsReply, CallId,
    Channel, CommandName, EntryId, FetchMethod, FetchRequest, FetchResponse, HookEvent,
    HookMismatch, HookOutcome, HookVerdict, InputVerdict, JobId, Mail, MailMode, McpRequest,
    McpResponse, Name, Part, RUST_STREAM_EVENT, RawJson, RedirectPolicy, RegistrationError,
    RepeatMode, RuleRecord, RunRequest, RunRequestError, STAR_EVENTS, Service, ServiceSet,
    SessionId, SidecarName, Stop, StreamVerdict, ToolCallEvent, ToolCallVerdict, ToolClass, TurnId,
    valid_tool_parameters, valid_version,
};

use std::num::NonZeroU64;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const WIRE_SESSION: &str = "01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f";
const WIRE_PEER: &str = "01927f40-0000-7000-8000-000000000001";

fn wire_session(text: &str) -> SessionId {
    SessionId::parse(text).expect("fixture session id parses")
}

fn rule_record(patterns: Vec<Box<str>>, repeat_gap: Option<u16>) -> RuleRecord {
    RuleRecord {
        name: Name("rule".into()),
        patterns,
        text: "reminder".into(),
        judge: None,
        scope: None,
        globs: None,
        agents: None,
        mode: None,
        repeat_mode: None,
        repeat_gap,
        always_apply: false,
        report: false,
        enabled: true,
    }
}

#[test]
fn sidecar_name_enforces_file_name_grammar_and_serde() -> TestResult {
    let maximum = "a".repeat(64);
    assert_eq!(SidecarName::parse(&maximum)?.as_str(), maximum);
    assert_eq!(SidecarName::parse("goal.json")?.as_str(), "goal.json");
    assert_eq!(SidecarName::parse("a..b")?.as_str(), "a..b");
    for invalid in [
        "",
        ".hidden",
        "-leading-dash",
        "..",
        "bad/name",
        "Bad.json",
        &"a".repeat(65),
    ] {
        assert!(SidecarName::parse(invalid).is_err(), "{invalid:?}");
        assert!(
            sonic_rs::from_str::<SidecarName>(&format!("{invalid:?}")).is_err(),
            "{invalid:?} must be rejected by serde"
        );
    }
    let encoded = sonic_rs::to_string(&SidecarName::parse("goal.json")?)?;
    assert_eq!(
        sonic_rs::from_str::<SidecarName>(&encoded)?,
        SidecarName::parse("goal.json")?
    );
    Ok(())
}

#[test]
fn name_boundary_and_ascii_only() -> TestResult {
    assert_eq!(Name::parse("a")?.as_str(), "a");
    let longest = "a".repeat(64);
    assert_eq!(Name::parse(&longest)?.as_str(), longest.as_str());

    let too_long = "a".repeat(65);
    for value in ["BadName", too_long.as_str(), "", "é"] {
        let error = Name::parse(value)
            .err()
            .ok_or("invalid name was accepted")?;
        assert!(matches!(error, RegistrationError::InvalidName { .. }));
        assert_eq!(
            error.to_string(),
            format!("invalid name \"{value}\"; names must match [a-z][a-z0-9_-]{{0,63}}")
        );
    }

    let legal = Name::parse("lower_name-2")?;
    let encoded = sonic_rs::to_string(&legal)?;
    assert_eq!(encoded, "\"lower_name-2\"");
    assert_eq!(sonic_rs::from_str::<Name>(&encoded)?, legal);
    assert!(sonic_rs::from_str::<Name>("\"BadName\"").is_err());
    assert!(sonic_rs::from_str::<Name>("\"deploy.web-x.list\"").is_err());
    Ok(())
}

#[test]
fn mapped_tool_name_accepts_dotted_trailing_dot_and_uppercase_segments() -> TestResult {
    assert_eq!(
        Name::parse_mapped_tool("deploy.web-x.List")?.as_str(),
        "deploy.web-x.List"
    );
    assert_eq!(
        Name::parse_mapped_tool("deploy.web.")?.as_str(),
        "deploy.web."
    );
    Ok(())
}

#[test]
fn mapped_tool_name_rejects_empty() {
    assert!(matches!(
        Name::parse_mapped_tool(""),
        Err(super::names::NameError::Empty)
    ));
}

#[test]
fn mapped_tool_name_rejects_bytes_outside_its_alphabet() {
    assert!(matches!(
        Name::parse_mapped_tool("deploy.web bad"),
        Err(super::names::NameError::Byte { index: 10 })
    ));
}

#[test]
fn mapped_tool_name_accepts_200_bytes() -> TestResult {
    let name = format!("a{}", "x".repeat(199));
    assert_eq!(Name::parse_mapped_tool(&name)?.as_str(), name);
    Ok(())
}

#[test]
fn mapped_tool_name_rejects_201_bytes() {
    let name = format!("a{}", "x".repeat(200));
    assert!(matches!(
        Name::parse_mapped_tool(&name),
        Err(super::names::NameError::TooLong { len: 201 })
    ));
}

#[test]
fn mapped_tool_name_requires_lowercase_first_byte() {
    assert!(matches!(
        Name::parse_mapped_tool("Deploy.web.list"),
        Err(super::names::NameError::FirstByte)
    ));
}

#[test]
fn ordinary_name_parser_still_rejects_dots() {
    assert!(matches!(
        Name::parse("deploy.web.list"),
        Err(RegistrationError::InvalidName { .. })
    ));
}

#[test]
fn tool_spec_deserializes_a_mapped_tool_name() -> TestResult {
    let spec: super::ToolSpec = sonic_rs::from_str(
        r#"{"name":"deploy.web-x.list","description":"MCP tool","parameters":{"type":"object","properties":{}}}"#,
    )?;
    assert_eq!(spec.name.as_str(), "deploy.web-x.list");
    Ok(())
}

#[test]
fn mapped_names_deserialize_in_hook_scope_and_child_tool_values() -> TestResult {
    let mapped = Name::parse_mapped_tool("deploy.web-x.list")?;
    let turn = TurnId::new(NonZeroU64::new(1).unwrap());
    let call = CallId::new("call-1");
    let call_event = ToolCallEvent {
        turn,
        call: call.clone(),
        tool: mapped.clone(),
        class: ToolClass::Read,
        args: RawJson::parse("{}")?,
    };
    let call_event_wire = sonic_rs::to_string(&call_event)?;
    let decoded_call_event: ToolCallEvent = sonic_rs::from_str(&call_event_wire)?;
    assert_eq!(decoded_call_event.tool, mapped);
    let result_event = super::ToolResultEvent {
        turn,
        call: call.clone(),
        tool: Name::parse_mapped_tool("deploy.web-x.list")?,
        ok: true,
        preview: "done".into(),
    };
    let result_event_wire = sonic_rs::to_string(&result_event)?;
    let decoded_result_event: super::ToolResultEvent = sonic_rs::from_str(&result_event_wire)?;
    assert_eq!(decoded_result_event.tool.as_str(), "deploy.web-x.list");
    let channel: Channel =
        sonic_rs::from_str(r#"{"type":"tool_args","tool":"deploy.web-x.list"}"#)?;
    assert!(matches!(
        channel,
        Channel::ToolArgs { tool } if tool.as_str() == "deploy.web-x.list"
    ));
    let scope: super::Scope = sonic_rs::from_str(
        r#"{"text":true,"thinking":false,"tool":true,"namedTools":["deploy.web-x.list"]}"#,
    )?;
    assert_eq!(scope.named_tools[0].as_str(), "deploy.web-x.list");
    let child = AgentStart {
        call,
        name: Some("child".into()),
        prompt: "run mapped tool".into(),
        model: None,
        role: None,
        system: None,
        tools: Some(vec![mapped].into()),
        workspace: None,
    };
    let child_wire = sonic_rs::to_string(&child)?;
    let decoded_child: AgentStart = sonic_rs::from_str(&child_wire)?;
    assert_eq!(
        decoded_child
            .tools
            .as_deref()
            .map(|tools| tools[0].as_str()),
        Some("deploy.web-x.list")
    );
    Ok(())
}

#[test]
fn command_names_allow_skill_commands_without_widening_extension_names() -> TestResult {
    let command = CommandName::parse("model")?;
    let skill = CommandName::parse("skill:review-rules")?;
    assert_eq!(command.as_str(), "model");
    assert_eq!(skill.as_str(), "skill:review-rules");
    assert!(Name::parse("skill:review-rules").is_err());
    assert_eq!(
        CommandName::parse("quality:todos")?.as_str(),
        "quality:todos"
    );

    for invalid in [
        "",
        "BadName",
        "skill:",
        "skill:Upper",
        "skill:with_underscore",
        "quality:",
        ":todos",
        "quality:to:dos",
        "Quality:todos",
    ] {
        assert!(matches!(
            CommandName::parse(invalid),
            Err(RegistrationError::InvalidCommandName { .. })
        ));
    }
    let encoded = sonic_rs::to_string(&skill)?;
    assert_eq!(encoded, r#""skill:review-rules""#);
    assert_eq!(sonic_rs::from_str::<CommandName>(&encoded)?, skill);
    Ok(())
}

#[test]
fn service_set_has_stable_order_and_excludes_ask_from_grants() -> TestResult {
    let reverse = Service::ALL
        .into_iter()
        .rev()
        .map(Service::as_str)
        .collect::<Vec<_>>();
    let requested = ServiceSet::from_names(reverse)?;
    let expected = [
        "fs.read", "fs.write", "net", "run", "env", "ask", "mcp", "agents", "jobs", "turn",
        "sidecar", "infer",
    ];
    assert_eq!(
        requested.iter().map(Service::as_str).collect::<Vec<_>>(),
        expected
    );
    let capabilities = requested.capabilities();
    assert!(requested.contains(Service::Ask));
    assert!(!capabilities.contains(Service::Ask));
    assert!(capabilities.contains(Service::Infer));
    assert!(requested.contains(Service::Infer));
    assert!(
        !ServiceSet::from_names(["ask"])?
            .capabilities()
            .contains(Service::Ask)
    );
    assert_eq!(
        ServiceSet::from_names(["ask"])?.capabilities(),
        ServiceSet::EMPTY
    );

    let unknown = ServiceSet::from_names(["nope"])
        .err()
        .ok_or("unknown service accepted")?;
    assert_eq!(
        unknown.to_string(),
        "unknown service \"nope\"; the service vocabulary is fs.read, fs.write, net, run, env, ask, mcp, agents, jobs, turn, sidecar, infer"
    );
    let duplicate = ServiceSet::from_names(["run", "run"])
        .err()
        .ok_or("duplicate service accepted")?;
    assert_eq!(
        duplicate.to_string(),
        "service \"run\" appears twice in inject"
    );
    Ok(())
}

#[test]
fn semver_and_object_schema_reject_invalid_inputs() -> TestResult {
    assert!(valid_version("1.0.0"));
    assert!(valid_version("1.2.3-alpha.1+build.2"));
    assert!(valid_version("0.0.0-0+00"));
    for version in [
        "x",
        "v1.2.3",
        "01.2.3",
        "1.2.3-",
        "1.2.3+",
        "1.2.3-01",
        "1.2.3+build+other",
        "1.2.3 ",
        "18446744073709551616.0.0",
    ] {
        assert!(
            !valid_version(version),
            "accepted invalid version {version:?}"
        );
    }

    let object_type = RawJson::parse(r#"{"type":"object","properties":{}}"#)?;
    let no_type = RawJson::parse("{}")?;
    let string_type = RawJson::parse(r#"{"type":"string"}"#)?;
    let null_type = RawJson::parse(r#"{"type":null}"#)?;
    let array = RawJson::parse("[]")?;
    assert!(valid_tool_parameters(&object_type));
    assert!(valid_tool_parameters(&no_type));
    assert!(!valid_tool_parameters(&string_type));
    assert!(!valid_tool_parameters(&null_type));
    assert!(!valid_tool_parameters(&array));
    assert!(RawJson::parse("{").is_err());
    assert_eq!(
        RegistrationError::InvalidParameters.to_string(),
        "parameters must be a JSON object schema ({\"type\": \"object\", ...})"
    );
    Ok(())
}

#[test]
fn rule_declaration_keeps_bad_pattern_until_set_build() -> TestResult {
    let malformed_pattern = rule_record(vec!["(".into()], None);
    assert!(malformed_pattern.validate().is_ok());

    let empty = rule_record(Vec::new(), None);
    let count_error = empty.validate().err().ok_or("empty patterns accepted")?;
    assert_eq!(
        count_error.to_string(),
        "rule \"rule\" must declare one to sixteen patterns, or none with always_apply"
    );
    let unconditional = RuleRecord {
        always_apply: true,
        ..rule_record(Vec::new(), None)
    };
    assert!(unconditional.validate().is_ok());
    let too_many = rule_record(vec!["(".into(); 17], None);
    assert_eq!(
        too_many
            .validate()
            .err()
            .ok_or("seventeen patterns accepted")?
            .to_string(),
        "rule \"rule\" must declare one to sixteen patterns, or none with always_apply"
    );
    let invalid_gap = rule_record(vec!["(".into()], Some(0));
    assert_eq!(
        invalid_gap
            .validate()
            .err()
            .ok_or("zero repeat gap accepted")?
            .to_string(),
        "rule \"rule\" repeat_gap must be within 1..=1000"
    );

    assert_eq!(sonic_rs::to_string(&RepeatMode::Once)?, "\"once\"");
    assert_eq!(sonic_rs::to_string(&RepeatMode::AfterGap)?, "\"after-gap\"");
    assert_eq!(
        sonic_rs::from_str::<RepeatMode>("\"after-gap\"")?,
        RepeatMode::AfterGap
    );
    assert!(sonic_rs::from_str::<RepeatMode>("\"after_gap\"").is_err());
    Ok(())
}

#[test]
fn agent_start_validates_before_effects() -> TestResult {
    let invalid_start = AgentStart {
        call: CallId::new("parent-call"),
        name: Some("reviewer".into()),
        prompt: "child prompt".into(),
        model: None,
        role: Some("reviewer".into()),
        system: Some("custom system".into()),
        tools: None,
        workspace: None,
    };
    let error = invalid_start
        .validate()
        .err()
        .ok_or("role and system accepted")?;
    assert_eq!(error.to_string(), "role and system cannot both be set");
    let valid_start = AgentStart {
        role: None,
        ..invalid_start
    };
    assert_eq!(valid_start.validate(), Ok(()));
    Ok(())
}

#[test]
fn run_request_environment_defaults_empty_and_validates_names() -> TestResult {
    let mut request = RunRequest {
        argv: vec![std::ffi::OsString::from("git")],
        cwd: None,
        stdin: None,
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 512,
    };
    let encoded = sonic_rs::to_string(&request)?;
    let without_env = encoded.replace("\"env\":[],", "");
    assert_ne!(without_env, encoded);
    assert_eq!(sonic_rs::from_str::<RunRequest>(&without_env)?.env, []);

    request.env = vec![("GIT_OPTIONAL_LOCKS".into(), "0".into())];
    assert_eq!(request.validate_env(), Ok(()));
    let encoded = sonic_rs::to_string(&request)?;
    assert_eq!(sonic_rs::from_str::<RunRequest>(&encoded)?, request);

    for name in ["", "KEY=VALUE", "NUL\0NAME"] {
        request.env = vec![(name.into(), "value".into())];
        assert_eq!(
            request.validate_env(),
            Err(RunRequestError::InvalidEnvironmentName { name: name.into() })
        );
    }
    Ok(())
}

#[test]
fn child_agent_replies_preserve_typed_state_and_report_pointer() -> TestResult {
    let id = SessionId::new_v7();
    let entry = EntryId::new(NonZeroU64::MIN);
    let info = AgentInfo {
        id,
        name: "reviewer".into(),
        state: AgentState::Done(Stop::EndTurn),
    };
    let report = AgentReport {
        stop: Stop::EndTurn,
        text: "review complete".into(),
        session: id,
        entry,
    };
    let replies = [
        AgentsReply::Started { id },
        AgentsReply::Prompted { id },
        AgentsReply::Await { report },
        AgentsReply::Cancelled { id },
        AgentsReply::Listed(vec![info]),
    ];
    for reply in replies {
        let encoded = sonic_rs::to_string(&reply)?;
        assert_eq!(sonic_rs::from_str::<AgentsReply>(&encoded)?, reply);
    }
    let state = sonic_rs::to_string(&AgentState::Done(Stop::Cancelled))?;
    assert_eq!(state, r#"{"type":"done","value":"cancelled"}"#);
    Ok(())
}

#[test]
fn fetch_request_and_response_round_trip_with_bounded_read() -> TestResult {
    let request = FetchRequest {
        method: FetchMethod::Get,
        url: "https://example.com/x".into(),
        headers: vec![("User-Agent".into(), "dalgon".into())],
        body: Vec::new(),
        redirects: RedirectPolicy::Follow,
    };
    let encoded = sonic_rs::to_string(&request)?;
    assert!(encoded.contains("\"GET\""));
    assert_eq!(sonic_rs::from_str::<FetchRequest>(&encoded)?, request);
    let response = FetchResponse {
        status: 200,
        headers: vec![("Content-Type".into(), "text/html".into())],
        body: b"<h1>Hello</h1>".to_vec(),
    };
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-type"), Some("text/html"));
    assert_eq!(response.read_bytes(4), b"<h1>");
    assert_eq!(response.read_bytes(4096).len(), response.body.len());
    let encoded = sonic_rs::to_string(&response)?;
    assert_eq!(sonic_rs::from_str::<FetchResponse>(&encoded)?, response);
    Ok(())
}

#[test]
fn mcp_wire_is_camel_case_and_rejects_missing_or_mistyped_members() -> TestResult {
    let request = McpRequest {
        session: wire_session(WIRE_SESSION),
        server: "filesystem".into(),
        tool: "read".into(),
        arguments: RawJson::parse(r#"{ "path" : "/tmp/x" }"#)?,
    };
    let wire = format!(
        r#"{{"session":"{WIRE_SESSION}","server":"filesystem","tool":"read","arguments":{{ "path" : "/tmp/x" }}}}"#
    );
    assert_eq!(sonic_rs::to_string(&request)?, wire);
    assert_eq!(sonic_rs::from_str::<McpRequest>(&wire)?, request);
    for bad in [
        r#"{"server":"s","tool":"t","arguments":{}}"#,
        r#"{"session":"nope","server":"s","tool":"t","arguments":{}}"#,
        &format!(r#"{{"session":"{WIRE_SESSION}","tool":"t","arguments":{{}}}}"#),
        &format!(r#"{{"session":"{WIRE_SESSION}","server":"s","tool":"t"}}"#),
        &format!(r#"{{"session":"{WIRE_SESSION}","server":"s","tool":9,"arguments":{{}}}}"#),
    ] {
        assert!(
            sonic_rs::from_str::<McpRequest>(bad).is_err(),
            "accepted {bad}"
        );
    }

    let response = McpResponse {
        text: "done".into(),
        is_error: true,
    };
    let wire = r#"{"text":"done","isError":true}"#;
    assert_eq!(sonic_rs::to_string(&response)?, wire);
    assert_eq!(sonic_rs::from_str::<McpResponse>(wire)?, response);
    for bad in [
        r#"{"text":"done","is_error":true}"#,
        r#"{"text":"done"}"#,
        r#"{"text":"done","isError":"yes"}"#,
        r#"{"text":7,"isError":false}"#,
    ] {
        assert!(
            sonic_rs::from_str::<McpResponse>(bad).is_err(),
            "accepted {bad}"
        );
    }
    Ok(())
}

#[test]
fn tool_call_event_carries_final_class() -> TestResult {
    let event = ToolCallEvent {
        turn: TurnId::new(NonZeroU64::MIN),
        call: CallId::new("call"),
        tool: Name::parse("read")?,
        class: ToolClass::Exec {
            read_only: true,
            grant: None,
        },
        args: RawJson::parse(r#"{"path":"/tmp/x"}"#)?,
    };
    let encoded = sonic_rs::to_string(&event)?;
    assert!(encoded.contains(r#""class":{"type":"exec","readOnly":true,"grant":null}"#));
    assert_eq!(sonic_rs::from_str::<ToolCallEvent>(&encoded)?, event);
    Ok(())
}

#[test]
fn mailbox_wire_pins_mode_spellings_and_reply_cursor() -> TestResult {
    let from = wire_session(WIRE_SESSION);
    let to = wire_session(WIRE_PEER);
    let cursor = EntryId::new(NonZeroU64::new(41).expect("nonzero cursor"));
    for (mode, spelling) in [
        (MailMode::Aside, "aside"),
        (MailMode::Steer, "steer"),
        (MailMode::NextTurn, "next_turn"),
    ] {
        let mail = Mail {
            from,
            to,
            mode,
            text: "go".into(),
            reply_to: Some(cursor),
        };
        let wire = format!(
            r#"{{"from":"{WIRE_SESSION}","to":"{WIRE_PEER}","mode":"{spelling}","text":"go","replyTo":41}}"#
        );
        assert_eq!(sonic_rs::to_string(&mail)?, wire);
        assert_eq!(sonic_rs::from_str::<Mail>(&wire)?, mail);

        let send = AgentsOp::Send {
            to,
            text: "go".into(),
            mode,
            reply_to: Some(cursor),
        };
        let wire = format!(
            r#"{{"type":"send","to":"{WIRE_PEER}","text":"go","mode":"{spelling}","replyTo":41}}"#
        );
        assert_eq!(sonic_rs::to_string(&send)?, wire);
        assert_eq!(sonic_rs::from_str::<AgentsOp>(&wire)?, send);
    }

    let unanswered = Mail {
        from,
        to,
        mode: MailMode::Aside,
        text: "go".into(),
        reply_to: None,
    };
    let wire = format!(
        r#"{{"from":"{WIRE_SESSION}","to":"{WIRE_PEER}","mode":"aside","text":"go","replyTo":null}}"#
    );
    assert_eq!(sonic_rs::to_string(&unanswered)?, wire);
    assert_eq!(sonic_rs::from_str::<Mail>(&wire)?, unanswered);

    for bad in ["next-turn", "NextTurn", "nextTurn", "interrupt", ""] {
        assert!(
            sonic_rs::from_str::<MailMode>(&format!("\"{bad}\"")).is_err(),
            "accepted mode {bad:?}"
        );
    }

    // The reply cursor is a nonzero u64: 0 and the value past u64::MAX are
    // refused, u64::MAX and 1 are kept exactly.
    let mail_with = |cursor: &str| {
        format!(
            r#"{{"from":"{WIRE_SESSION}","to":"{WIRE_PEER}","mode":"aside","text":"go","replyTo":{cursor}}}"#
        )
    };
    for (cursor, kept) in [("1", Some(1_u64)), ("18446744073709551615", Some(u64::MAX))] {
        let mail = sonic_rs::from_str::<Mail>(&mail_with(cursor))?;
        assert_eq!(mail.reply_to.map(EntryId::get), kept, "{cursor}");
    }
    for cursor in ["0", "-1", "18446744073709551616", "\"41\"", "1.5"] {
        assert!(
            sonic_rs::from_str::<Mail>(&mail_with(cursor)).is_err(),
            "accepted replyTo {cursor}"
        );
    }
    // The snake spelling is not an alias: the cursor stays unset.
    let snake = format!(
        r#"{{"from":"{WIRE_SESSION}","to":"{WIRE_PEER}","mode":"aside","text":"go","reply_to":41}}"#
    );
    assert_eq!(sonic_rs::from_str::<Mail>(&snake)?.reply_to, None);
    Ok(())
}

#[test]
fn a_refused_start_round_trips_with_its_typed_reason_and_text() -> TestResult {
    let cases = [
        (
            AgentRefusal::MaxDepth { max_depth: 2 },
            "child sessions cannot start children here: agents.max_depth = 2.",
        ),
        (
            AgentRefusal::WorkspaceUnresolved,
            "the child workspace cannot be resolved.",
        ),
        (
            AgentRefusal::WorkspaceOutsideRoot,
            "the child workspace must stay inside the caller's workspace.",
        ),
        (
            AgentRefusal::ModelUnroutable {
                model: "acme/none".into(),
            },
            "the child model acme/none cannot be routed.",
        ),
    ];
    for (reason, text) in cases {
        assert_eq!(reason.to_string(), text);
        let reply = AgentsReply::Refused { reason };
        let encoded = sonic_rs::to_string(&reply)?;
        assert_eq!(sonic_rs::from_str::<AgentsReply>(&encoded)?, reply);
    }
    let encoded = sonic_rs::to_string(&AgentsReply::Refused {
        reason: AgentRefusal::MaxDepth { max_depth: 1 },
    })?;
    assert_eq!(
        encoded,
        r#"{"type":"refused","value":{"reason":{"type":"max_depth","maxDepth":1}}}"#
    );
    Ok(())
}

#[test]
fn agents_op_wire_is_internally_tagged_camel_case_and_rejects_a_zero_step_budget() -> TestResult {
    let id = wire_session(WIRE_SESSION);
    let cursor = EntryId::new(NonZeroU64::new(41).expect("nonzero cursor"));
    let ops = [
        (
            AgentsOp::Prompt {
                id,
                text: "go".into(),
                interrupt: Some(Duration::from_secs(60)),
                max_steps: std::num::NonZeroU32::new(1),
            },
            format!(
                r#"{{"type":"prompt","id":"{WIRE_SESSION}","text":"go","interrupt":{{"secs":60,"nanos":0}},"maxSteps":1}}"#
            ),
        ),
        (
            AgentsOp::Prompt {
                id,
                text: "go".into(),
                interrupt: None,
                max_steps: None,
            },
            format!(
                r#"{{"type":"prompt","id":"{WIRE_SESSION}","text":"go","interrupt":null,"maxSteps":null}}"#
            ),
        ),
        (
            AgentsOp::Await { id, timeout: None },
            format!(r#"{{"type":"await","id":"{WIRE_SESSION}","timeout":null}}"#),
        ),
        (
            AgentsOp::Cancel { id },
            format!(r#"{{"type":"cancel","id":"{WIRE_SESSION}"}}"#),
        ),
        (AgentsOp::List, r#"{"type":"list"}"#.to_owned()),
        (
            AgentsOp::Recv {
                after: Some(cursor),
                timeout: Some(Duration::from_secs(2)),
            },
            r#"{"type":"recv","after":41,"timeout":{"secs":2,"nanos":0}}"#.to_owned(),
        ),
        (
            AgentsOp::Broadcast {
                text: "go".into(),
                mode: MailMode::Steer,
            },
            r#"{"type":"broadcast","text":"go","mode":"steer"}"#.to_owned(),
        ),
    ];
    for (op, wire) in ops {
        assert_eq!(sonic_rs::to_string(&op)?, wire);
        assert_eq!(sonic_rs::from_str::<AgentsOp>(&wire)?, op, "{wire}");
    }

    // The per-turn step budget is a nonzero u32: 0 would read as "no rounds
    // at all" and is refused at the wire, not coerced to unbounded.
    let prompt_with = |steps: &str| {
        format!(
            r#"{{"type":"prompt","id":"{WIRE_SESSION}","text":"go","interrupt":null,"maxSteps":{steps}}}"#
        )
    };
    for (steps, kept) in [("1", 1_u32), ("4294967295", u32::MAX)] {
        let AgentsOp::Prompt { max_steps, .. } = sonic_rs::from_str(&prompt_with(steps))? else {
            return Err("expected a prompt".into());
        };
        assert_eq!(max_steps.map(std::num::NonZeroU32::get), Some(kept));
    }
    for steps in ["0", "-1", "4294967296", "\"3\"", "1.5"] {
        assert!(
            sonic_rs::from_str::<AgentsOp>(&prompt_with(steps)).is_err(),
            "accepted maxSteps {steps}"
        );
    }
    // The snake spelling is not an alias: the budget stays unbounded.
    let snake = format!(
        r#"{{"type":"prompt","id":"{WIRE_SESSION}","text":"go","interrupt":null,"max_steps":3}}"#
    );
    let AgentsOp::Prompt { max_steps, .. } = sonic_rs::from_str(&snake)? else {
        return Err("expected a prompt".into());
    };
    assert_eq!(max_steps, None);
    for bad in [
        r#"{"type":"pause"}"#,
        r#"{"id":"x"}"#,
        r#"{"type":"await"}"#,
        r#"{"type":"cancel","id":"nope"}"#,
        r#"{"type":"cancel","type":"list"}"#,
    ] {
        assert!(
            sonic_rs::from_str::<AgentsOp>(bad).is_err(),
            "accepted {bad}"
        );
    }
    Ok(())
}

#[test]
fn hook_verdicts_cannot_represent_an_untyped_result() -> TestResult {
    assert_eq!(
        STAR_EVENTS,
        [
            "session_start",
            "session_end",
            "input",
            "before_turn",
            "before_request",
            "tool_call",
            "tool_result",
            "turn_end",
            "settled",
        ]
    );
    assert_eq!(RUST_STREAM_EVENT, "output_stream");

    let input_verdicts = [
        InputVerdict::Continue,
        InputVerdict::Transform(vec![Part::Text {
            text: "transformed".into(),
        }]),
        InputVerdict::Handled,
    ];
    for verdict in input_verdicts {
        let encoded = sonic_rs::to_string(&verdict)?;
        assert_eq!(sonic_rs::from_str::<InputVerdict>(&encoded)?, verdict);
    }

    let raw_args = r#"{"b": 2, "a":1e+02}"#;
    let rewrite = ToolCallVerdict::Rewrite {
        args: RawJson::parse(raw_args)?,
    };
    let encoded = sonic_rs::to_string(&rewrite)?;
    assert!(
        encoded.contains(raw_args),
        "raw arguments changed: {encoded}"
    );
    assert_eq!(sonic_rs::from_str::<ToolCallVerdict>(&encoded)?, rewrite);
    for verdict in [
        ToolCallVerdict::Allow,
        ToolCallVerdict::Block {
            reason: "denied".into(),
        },
        rewrite,
    ] {
        let encoded = sonic_rs::to_string(&verdict)?;
        assert_eq!(sonic_rs::from_str::<ToolCallVerdict>(&encoded)?, verdict);
    }

    let channels = [
        Channel::Text,
        Channel::Thinking,
        Channel::ToolArgs {
            tool: Name::parse("shell")?,
        },
    ];
    for channel in channels {
        let encoded = sonic_rs::to_string(&channel)?;
        assert_eq!(sonic_rs::from_str::<Channel>(&encoded)?, channel);
    }

    let stream_verdicts = [
        StreamVerdict::Continue,
        StreamVerdict::Interrupt {
            rule: "guard".into(),
            inject: "remember the boundary".into(),
        },
    ];
    for verdict in stream_verdicts {
        let encoded = sonic_rs::to_string(&verdict)?;
        assert_eq!(sonic_rs::from_str::<StreamVerdict>(&encoded)?, verdict);
    }
    Ok(())
}

#[test]
fn hook_verdict_wire_is_externally_tagged_by_event_and_refuses_observer_verdicts() -> TestResult {
    let verdicts = [
        (
            HookVerdict::Input(InputVerdict::Continue),
            r#"{"input":{"type":"continue"}}"#,
        ),
        (
            HookVerdict::Input(InputVerdict::Handled),
            r#"{"input":{"type":"handled"}}"#,
        ),
        (
            HookVerdict::Input(InputVerdict::Transform(vec![Part::Text {
                text: "x".into(),
            }])),
            r#"{"input":{"type":"transform","value":[{"type":"text","text":"x"}]}}"#,
        ),
        (
            HookVerdict::BeforeTurn(Some("note".into())),
            r#"{"before_turn":"note"}"#,
        ),
        (HookVerdict::BeforeTurn(None), r#"{"before_turn":null}"#),
        (
            HookVerdict::BeforeRequest(None),
            r#"{"before_request":null}"#,
        ),
        (
            HookVerdict::ToolCall(ToolCallVerdict::Allow),
            r#"{"tool_call":{"type":"allow"}}"#,
        ),
        (
            HookVerdict::ToolCall(ToolCallVerdict::Block {
                reason: "denied".into(),
            }),
            r#"{"tool_call":{"type":"block","reason":"denied"}}"#,
        ),
        (
            HookVerdict::ToolCall(ToolCallVerdict::Rewrite {
                args: RawJson::parse(r#"{ "b": 2 }"#)?,
            }),
            r#"{"tool_call":{"type":"rewrite","args":{ "b": 2 }}}"#,
        ),
    ];
    for (verdict, wire) in verdicts {
        assert_eq!(sonic_rs::to_string(&verdict)?, wire);
        assert_eq!(sonic_rs::from_str::<HookVerdict>(wire)?, verdict, "{wire}");
    }
    // Observer events return nothing: no spelling of theirs decodes, and a
    // guarding verdict's tag never accepts another verdict's payload.
    for bad in [
        r#"{"session_start":null}"#,
        r#"{"session_end":null}"#,
        r#"{"tool_result":{"type":"allow"}}"#,
        r#"{"turn_end":null}"#,
        r#"{"settled":null}"#,
        r#"{"output_stream":{"type":"continue"}}"#,
        r#"{"input":{"type":"allow"}}"#,
        r#"{"tool_call":{"type":"continue"}}"#,
        r#"{"tool_call":{"type":"block"}}"#,
        r#"{"tool_call":{"type":"rewrite"}}"#,
        r#"{"input":{"type":"transform"}}"#,
        r#"{"input":{"type":"continue"},"tool_call":{"type":"allow"}}"#,
    ] {
        assert!(
            sonic_rs::from_str::<HookVerdict>(bad).is_err(),
            "accepted {bad}"
        );
    }

    let stream = [
        (StreamVerdict::Continue, r#"{"type":"continue"}"#),
        (
            StreamVerdict::Interrupt {
                rule: "guard".into(),
                inject: "remember the boundary".into(),
            },
            r#"{"type":"interrupt","rule":"guard","inject":"remember the boundary"}"#,
        ),
    ];
    for (verdict, wire) in stream {
        assert_eq!(sonic_rs::to_string(&verdict)?, wire);
        assert_eq!(
            sonic_rs::from_str::<StreamVerdict>(wire)?,
            verdict,
            "{wire}"
        );
    }
    for bad in [
        r#"{"type":"Continue"}"#,
        r#"{"type":"interrupt","rule":"guard"}"#,
        r#"{"type":"interrupt","inject":"x"}"#,
        r#"{"type":"abort"}"#,
    ] {
        assert!(
            sonic_rs::from_str::<StreamVerdict>(bad).is_err(),
            "accepted {bad}"
        );
    }
    Ok(())
}

#[test]
fn stream_fire_budget_and_gate_seed_wire_is_pinned() -> TestResult {
    for (action, spelling) in [
        (StreamFireAction::Interrupt, "interrupt"),
        (StreamFireAction::Reminder, "reminder"),
        (StreamFireAction::Report, "report"),
    ] {
        let fire = StreamFire {
            index: 2,
            rule: "No.Sleep".into(),
            action,
            text: Some("remember the boundary".into()),
            judged: true,
        };
        let wire = format!(
            r#"{{"index":2,"rule":"No.Sleep","action":"{spelling}","text":"remember the boundary","judged":true}}"#
        );
        assert_eq!(sonic_rs::to_string(&fire)?, wire);
        assert_eq!(sonic_rs::from_str::<StreamFire>(&wire)?, fire);
    }
    let report = StreamFire {
        index: 0,
        rule: "r".into(),
        action: StreamFireAction::Report,
        text: None,
        judged: false,
    };
    let wire = r#"{"index":0,"rule":"r","action":"report","text":null,"judged":false}"#;
    assert_eq!(sonic_rs::to_string(&report)?, wire);
    assert_eq!(sonic_rs::from_str::<StreamFire>(wire)?, report);
    for bad in ["Interrupt", "INTERRUPT", "reminders", "log", ""] {
        assert!(
            sonic_rs::from_str::<StreamFireAction>(&format!("\"{bad}\"")).is_err(),
            "accepted action {bad:?}"
        );
    }
    assert!(
        sonic_rs::from_str::<StreamFire>(
            r#"{"index":-1,"rule":"r","action":"report","text":null,"judged":false}"#
        )
        .is_err(),
        "a negative stable index is refused"
    );

    for (budget, spelling) in [
        (WatchBudget::Interrupts, "interrupts"),
        (WatchBudget::RemindersOnly, "reminders_only"),
    ] {
        let wire = format!("\"{spelling}\"");
        assert_eq!(sonic_rs::to_string(&budget)?, wire);
        assert_eq!(sonic_rs::from_str::<WatchBudget>(&wire)?, budget);
    }
    for bad in ["reminders-only", "RemindersOnly", "interrupt", ""] {
        assert!(
            sonic_rs::from_str::<WatchBudget>(&format!("\"{bad}\"")).is_err(),
            "accepted budget {bad:?}"
        );
    }

    let seed = GateSeed {
        rule: "No.Sleep".into(),
        turn: TurnId::new(NonZeroU64::new(7).expect("nonzero turn")),
        entry: EntryId::new(NonZeroU64::new(9).expect("nonzero entry")),
    };
    let wire = r#"{"rule":"No.Sleep","turn":7,"entry":9}"#;
    assert_eq!(sonic_rs::to_string(&seed)?, wire);
    assert_eq!(sonic_rs::from_str::<GateSeed>(wire)?, seed);
    // A zero turn or entry id names nothing: both are refused.
    for bad in [
        r#"{"rule":"r","turn":0,"entry":9}"#,
        r#"{"rule":"r","turn":7,"entry":0}"#,
        r#"{"rule":"r","turn":7}"#,
        r#"{"turn":7,"entry":9}"#,
    ] {
        assert!(
            sonic_rs::from_str::<GateSeed>(bad).is_err(),
            "accepted {bad}"
        );
    }
    Ok(())
}

#[test]
fn agents_reply_wire_is_adjacently_tagged_with_camel_case_fields() -> TestResult {
    let id = wire_session(WIRE_SESSION);
    let peer = wire_session(WIRE_PEER);
    let cursor = EntryId::new(NonZeroU64::new(41).expect("nonzero cursor"));
    let replies = [
        (
            AgentsReply::Started { id },
            format!(r#"{{"type":"started","value":{{"id":"{WIRE_SESSION}"}}}}"#),
        ),
        (
            AgentsReply::Pending { id },
            format!(r#"{{"type":"pending","value":{{"id":"{WIRE_SESSION}"}}}}"#),
        ),
        (
            AgentsReply::Await {
                report: AgentReport {
                    stop: Stop::EndTurn,
                    text: "review complete".into(),
                    session: id,
                    entry: cursor,
                },
            },
            format!(
                r#"{{"type":"await","value":{{"report":{{"stop":"end_turn","text":"review complete","session":"{WIRE_SESSION}","entry":41}}}}}}"#
            ),
        ),
        (
            AgentsReply::Listed(vec![AgentInfo {
                id,
                name: "reviewer".into(),
                state: AgentState::Running,
            }]),
            format!(
                r#"{{"type":"listed","value":[{{"id":"{WIRE_SESSION}","name":"reviewer","state":{{"type":"running"}}}}]}}"#
            ),
        ),
        (
            AgentsReply::Delivered(Receipt::Woken),
            r#"{"type":"delivered","value":"woken"}"#.to_owned(),
        ),
        (
            AgentsReply::Received {
                mail: vec![Mail {
                    from: id,
                    to: peer,
                    mode: MailMode::NextTurn,
                    text: "next turn".into(),
                    reply_to: None,
                }],
                next: Some(cursor),
            },
            format!(
                r#"{{"type":"received","value":{{"mail":[{{"from":"{WIRE_SESSION}","to":"{WIRE_PEER}","mode":"next_turn","text":"next turn","replyTo":null}}],"next":41}}}}"#
            ),
        ),
    ];
    for (reply, wire) in replies {
        assert_eq!(sonic_rs::to_string(&reply)?, wire);
        assert_eq!(sonic_rs::from_str::<AgentsReply>(&wire)?, reply, "{wire}");
    }
    for bad in [
        r#"{"type":"started"}"#,
        r#"{"type":"started","value":{}}"#,
        r#"{"type":"delivered","value":"sent"}"#,
        r#"{"type":"await","value":{"report":{"stop":"end_turn","text":"t","session":"nope","entry":1}}}"#,
        r#"{"type":"await","value":{"report":{"stop":"done","text":"t","entry":1}}}"#,
        r#"{"type":"future","value":{}}"#,
    ] {
        assert!(
            sonic_rs::from_str::<AgentsReply>(bad).is_err(),
            "accepted {bad}"
        );
    }
    Ok(())
}

#[test]
fn jobs_ops_round_trip_with_raw_payloads() -> TestResult {
    // The spawn payload keeps its bytes verbatim through the tagged
    // decode; a byte-exact payload is the point of the member.
    let raw_payload = r#"{"script": "x",  "n":1e+03}"#;
    let spawned: JobsOp = sonic_rs::from_str(&format!(
        r#"{{"type":"spawn","name":"backup","payload":{raw_payload}}}"#
    ))?;
    let JobsOp::Spawn { name, payload, .. } = spawned else {
        panic!("expected spawn");
    };
    assert_eq!(name.as_str(), "backup");
    assert_eq!(payload.as_str(), raw_payload);

    let id = JobId::new_v7();
    for op in [
        JobsOp::Status { id },
        JobsOp::Cancel { id },
        JobsOp::Wait { id, timeout: None },
        JobsOp::List,
        JobsOp::Text { id },
    ] {
        let wire = sonic_rs::to_string(&op)?;
        assert_eq!(sonic_rs::from_str::<JobsOp>(&wire)?, op, "{wire}");
    }
    assert!(sonic_rs::from_str::<JobsOp>(r#"{"type":"pause","id":"x"}"#).is_err());
    Ok(())
}

#[test]
fn hook_events_round_trip_and_verdicts_answer_only_their_event() -> TestResult {
    for (event, name) in HookEvent::ALL.into_iter().zip(STAR_EVENTS) {
        assert_eq!(event.as_str(), name);
        let encoded = sonic_rs::to_string(&event)?;
        assert_eq!(encoded, format!("\"{name}\""));
        assert_eq!(sonic_rs::from_str::<HookEvent>(&encoded)?, event);
    }
    assert!(sonic_rs::from_str::<HookEvent>("\"output_stream\"").is_err());

    let verdicts = [
        HookVerdict::Input(InputVerdict::Continue),
        HookVerdict::BeforeTurn(Some("note".into())),
        HookVerdict::BeforeRequest(None),
        HookVerdict::ToolCall(ToolCallVerdict::Rewrite {
            args: RawJson::parse(r#"{"path": "a"}"#)?,
        }),
    ];
    for verdict in verdicts {
        let answered = verdict.event();
        assert!(answered.is_guarding());
        let encoded = sonic_rs::to_string(&verdict)?;
        assert_eq!(sonic_rs::from_str::<HookVerdict>(&encoded)?, verdict);
        for event in HookEvent::ALL {
            let expected = if event == answered {
                Ok(event)
            } else if event.is_guarding() {
                Err(HookMismatch::WrongEvent {
                    event,
                    verdict: answered,
                })
            } else {
                Err(HookMismatch::Observer {
                    event,
                    verdict: answered,
                })
            };
            let paired = HookOutcome::new(event, verdict.clone()).map(|outcome| outcome.event());
            assert_eq!(paired, expected);
        }
    }
    let observed = HookEvent::ALL
        .into_iter()
        .filter(|event| !event.is_guarding())
        .map(HookEvent::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        [
            "session_start",
            "session_end",
            "tool_result",
            "turn_end",
            "settled"
        ]
    );
    assert_eq!(
        HookOutcome::new(
            HookEvent::Settled,
            HookVerdict::Input(InputVerdict::Handled)
        )
        .err()
        .ok_or("observer verdict was accepted")?
        .to_string(),
        "observe-only hook event settled returns no verdict; got the input verdict"
    );
    Ok(())
}
