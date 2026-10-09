use super::{
    AgentInfo, AgentReport, AgentStart, AgentState, AgentsOp, AgentsReply, CallId, Channel,
    CommandName, EntryId, FetchMethod, FetchRequest, FetchResponse, HookEvent, HookMismatch,
    HookOutcome, HookVerdict, InputVerdict, JobId, Mail, MailMode, McpRequest, McpResponse, Name,
    Part, RUST_STREAM_EVENT, RawJson, RegistrationError, RepeatMode, RuleRecord, RunRequest,
    RunRequestError, STAR_EVENTS, Service, ServiceSet, SessionId, SidecarName, Stop, StreamVerdict,
    ToolCallEvent, ToolCallVerdict, ToolClass, TurnId, valid_tool_parameters, valid_version,
};

use std::num::NonZeroU64;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

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
        name: "child".into(),
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
    assert!(ServiceSet::from_names(["ask"])?.capabilities().is_empty());

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
        name: "reviewer".into(),
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
    assert!(
        sonic_rs::from_str::<RunRequest>(&without_env)?
            .env
            .is_empty()
    );

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
fn mcp_request_and_response_round_trip() -> TestResult {
    let request = McpRequest {
        session: SessionId::new_v7(),
        server: "filesystem".into(),
        tool: "read".into(),
        arguments: RawJson::parse(r#"{"path":"/tmp/x"}"#)?,
    };
    let encoded = sonic_rs::to_string(&request)?;
    assert_eq!(sonic_rs::from_str::<McpRequest>(&encoded)?, request);
    let response = McpResponse {
        text: "done".into(),
        is_error: false,
    };
    let encoded = sonic_rs::to_string(&response)?;
    assert_eq!(sonic_rs::from_str::<McpResponse>(&encoded)?, response);
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
fn mailbox_values_keep_mode_and_cursor() -> TestResult {
    let from = SessionId::new_v7();
    let to = SessionId::new_v7();
    let cursor = EntryId::new(NonZeroU64::MIN);
    for mode in [MailMode::Aside, MailMode::Steer, MailMode::NextTurn] {
        let mail = Mail {
            from,
            to,
            mode,
            text: "continue from this point".into(),
            reply_to: Some(cursor),
        };
        let encoded = sonic_rs::to_string(&mail)?;
        assert_eq!(sonic_rs::from_str::<Mail>(&encoded)?, mail);

        let send = AgentsOp::Send {
            to,
            text: "continue from this point".into(),
            mode,
            reply_to: Some(cursor),
        };
        let encoded = sonic_rs::to_string(&send)?;
        assert_eq!(sonic_rs::from_str::<AgentsOp>(&encoded)?, send);
    }

    let recv = AgentsOp::Recv {
        after: Some(cursor),
        timeout: None,
    };
    let encoded = sonic_rs::to_string(&recv)?;
    assert_eq!(sonic_rs::from_str::<AgentsOp>(&encoded)?, recv);
    let reply = AgentsReply::Received {
        mail: vec![Mail {
            from,
            to,
            mode: MailMode::NextTurn,
            text: "next turn".into(),
            reply_to: None,
        }],
        next: Some(cursor),
    };
    let encoded = sonic_rs::to_string(&reply)?;
    assert_eq!(sonic_rs::from_str::<AgentsReply>(&encoded)?, reply);

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
fn stream_fire_budget_and_gate_seed_round_trip() -> TestResult {
    for action in [
        StreamFireAction::Interrupt,
        StreamFireAction::Reminder,
        StreamFireAction::Report,
    ] {
        let fire = StreamFire {
            index: 2,
            rule: "No.Sleep".into(),
            action,
            text: Some("remember the boundary".into()),
            judged: true,
        };
        let encoded = sonic_rs::to_string(&fire)?;
        assert_eq!(sonic_rs::from_str::<StreamFire>(&encoded)?, fire);
    }

    for budget in [WatchBudget::Interrupts, WatchBudget::RemindersOnly] {
        let encoded = sonic_rs::to_string(&budget)?;
        assert_eq!(sonic_rs::from_str::<WatchBudget>(&encoded)?, budget);
    }

    let seed = GateSeed {
        rule: "No.Sleep".into(),
        turn: TurnId::new(NonZeroU64::new(7).expect("nonzero turn")),
        entry: EntryId::new(NonZeroU64::new(9).expect("nonzero entry")),
    };
    let encoded = sonic_rs::to_string(&seed)?;
    assert_eq!(sonic_rs::from_str::<GateSeed>(&encoded)?, seed);
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
