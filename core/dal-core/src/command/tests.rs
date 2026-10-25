use std::num::NonZeroU64;
use std::path::PathBuf;

use proptest::prelude::*;
use serde::Deserialize;

use super::{
    BusyState, CancelScope, Chooser, Classify, Command, CommandError, Expect, ExportFormat,
    FrontAction, ImportFailure, LexError, Output, Rejection, Reply, Save, classify, tokens,
};
use crate::approval::DenyReason;
use crate::config::ApprovalMode;
use crate::content::Part;
use crate::id::{EntryId, JobId, TurnId};
use crate::model::{Family, ModelRoute, ThinkingLevel};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn text_content() -> Vec<Part> {
    vec![Part::Text {
        text: "hello".into(),
    }]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentCommandWire {
    #[serde(rename = "type")]
    kind: Box<str>,
    expect: Option<Expect>,
    turn: Option<TurnId>,
    content: Vec<Part>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactCommandWire {
    #[serde(rename = "type")]
    kind: Box<str>,
    focus: Box<str>,
}

#[test]
fn content_commands_encode_under_content() -> TestResult {
    let turn = TurnId::new(NonZeroU64::MIN);
    let commands = [
        (
            Command::Prompt {
                expect: Expect::Idle,
                content: text_content(),
            },
            "prompt",
        ),
        (
            Command::Steer {
                turn,
                content: text_content(),
            },
            "steer",
        ),
        (
            Command::FollowUp {
                turn,
                content: text_content(),
            },
            "follow_up",
        ),
    ];

    for (command, expected_kind) in commands {
        let encoded = sonic_rs::to_string(&command)?;
        let wire: ContentCommandWire = sonic_rs::from_str(&encoded)?;
        assert_eq!(wire.kind.as_ref(), expected_kind);
        if expected_kind == "prompt" {
            assert_eq!(wire.expect, Some(Expect::Idle));
        } else {
            assert_eq!(wire.turn, Some(turn));
        }
        assert_eq!(wire.content, text_content());
    }
    Ok(())
}

#[test]
fn content_commands_decode_from_content() -> TestResult {
    let turn = TurnId::new(NonZeroU64::MIN);
    let commands = [
        (
            r#"{"type":"prompt","expect":"idle","content":[{"type":"text","text":"hello"}]}"#,
            Command::Prompt {
                expect: Expect::Idle,
                content: text_content(),
            },
        ),
        (
            r#"{"type":"steer","turn":1,"content":[{"type":"text","text":"hello"}]}"#,
            Command::Steer {
                turn,
                content: text_content(),
            },
        ),
        (
            r#"{"type":"follow_up","turn":1,"content":[{"type":"text","text":"hello"}]}"#,
            Command::FollowUp {
                turn,
                content: text_content(),
            },
        ),
    ];

    for (encoded, expected) in commands {
        let decoded: Command = sonic_rs::from_str(encoded)?;
        assert_eq!(decoded, expected);
    }
    Ok(())
}

#[test]
fn prompt_parts_is_not_a_content_alias() {
    assert!(
        sonic_rs::from_str::<Command>(
            r#"{"type":"prompt","expect":"idle","parts":[{"type":"text","text":"hello"}]}"#
        )
        .is_err()
    );
}

#[test]
fn compact_focus_encodes_and_decodes_under_focus() -> TestResult {
    let command = Command::Compact {
        focus: Some("preserve the API".into()),
    };
    let encoded = sonic_rs::to_string(&command)?;
    let wire: CompactCommandWire = sonic_rs::from_str(&encoded)?;
    assert_eq!(wire.kind.as_ref(), "compact");
    assert_eq!(wire.focus.as_ref(), "preserve the API");

    let decoded: Command = sonic_rs::from_str(r#"{"type":"compact","focus":"preserve the API"}"#)?;
    assert_eq!(decoded, command);
    Ok(())
}

#[test]
fn compact_instructions_is_not_a_focus_alias() -> TestResult {
    let decoded: Command =
        sonic_rs::from_str(r#"{"type":"compact","instructions":"preserve the API"}"#)?;
    assert_eq!(decoded, Command::Compact { focus: None });
    Ok(())
}

#[test]
fn command_variants_round_trip_with_wire_fields() -> TestResult {
    let turn = TurnId::new(NonZeroU64::MIN);
    let job = JobId::new_v7();
    let entry = EntryId::new(NonZeroU64::MIN);
    let route = ModelRoute::synthetic("test/model")?;
    let commands = [
        Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "hello".into(),
            }],
        },
        Command::Prompt {
            expect: Expect::After(turn),
            content: Vec::new(),
        },
        Command::Steer {
            turn,
            content: Vec::new(),
        },
        Command::FollowUp {
            turn,
            content: Vec::new(),
        },
        Command::Cancel {
            scope: CancelScope::Turn(turn),
        },
        Command::Cancel {
            scope: CancelScope::Job(job),
        },
        Command::SetModel {
            model: route,
            save: Save::SessionOnly,
        },
        Command::SetThinking {
            level: ThinkingLevel::High,
            save: Save::SessionOnly,
        },
        Command::SetApproval {
            mode: ApprovalMode::Edits,
            save: Save::SessionOnly,
        },
        Command::Compact {
            focus: Some("preserve the API".into()),
        },
        Command::MoveLeaf(entry),
        Command::Fork(entry),
        Command::Clone,
        Command::Rename("renamed".into()),
        Command::Run {
            name: "inspect".into(),
            args: "--status".into(),
            expected: None,
        },
    ];

    for command in commands {
        let encoded = sonic_rs::to_string(&command)?;
        let decoded: Command = sonic_rs::from_str(&encoded)?;
        assert_eq!(decoded, command, "round-trip failed for {encoded}");
    }

    assert_eq!(
        sonic_rs::to_string(&Command::SetThinking {
            level: ThinkingLevel::High,
            save: Save::SessionAndDefault,
        })?,
        r#"{"type":"set_thinking","level":"high","save":"session_and_default"}"#
    );
    assert_eq!(
        sonic_rs::to_string(&Command::Cancel {
            scope: CancelScope::Turn(turn)
        })?,
        r#"{"type":"cancel","scope":{"type":"turn","turn":1}}"#
    );
    Ok(())
}

#[test]
fn new_rejections_have_meaningful_display_text() {
    let cases = [
        (Rejection::SessionClosed, "session is closed"),
        (
            Rejection::BusyTurn,
            "command requires an idle session; a turn is running",
        ),
        (
            Rejection::Compacting,
            "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.",
        ),
        (
            Rejection::SteerFull,
            "steer queue is full (16); wait for the next step or cancel.",
        ),
    ];

    for (rejection, expected) in cases {
        assert_eq!(rejection.to_string(), expected);
    }
}

#[test]
fn denied_rejections_preserve_the_typed_approval_reason() {
    let wake_limit = Rejection::Denied {
        reason: DenyReason::WakeLimit,
    };
    assert_eq!(wake_limit.to_string(), "command denied: WakeLimit");

    let no_front_end = Rejection::Denied {
        reason: DenyReason::NoFrontEnd,
    };
    assert_eq!(no_front_end.to_string(), "command denied: NoFrontEnd");
    assert_ne!(no_front_end, Rejection::SessionClosed);
}
#[test]
fn builtin_commands_round_trip_with_wire_fields() -> TestResult {
    use crate::config::Mode;

    let turn = TurnId::new(NonZeroU64::MIN);
    let commands = [
        Command::SetMode {
            mode: Mode::EvalFirst,
            save: Save::SessionAndDefault,
        },
        Command::SetScopedModels(vec!["a/b".into(), "c/d".into()]),
        Command::SetScopedModels(Vec::new()),
        Command::Export {
            path: None,
            format: ExportFormat::Markdown,
        },
        Command::Export {
            path: Some(PathBuf::from("/tmp/out.jsonl")),
            format: ExportFormat::Jsonl,
        },
        Command::ReloadPlugins,
        Command::CancelQueued { turn },
        Command::Run {
            name: "tree".into(),
            args: "".into(),
            expected: Some(Expect::Idle),
        },
        Command::Run {
            name: "resume".into(),
            args: "x".into(),
            expected: Some(Expect::After(turn)),
        },
    ];

    for command in commands {
        let encoded = sonic_rs::to_string(&command)?;
        let decoded: Command = sonic_rs::from_str(&encoded)?;
        assert_eq!(decoded, command, "round-trip failed for {encoded}");
    }

    assert_eq!(
        sonic_rs::to_string(&Command::Export {
            path: None,
            format: ExportFormat::Jsonl,
        })?,
        r#"{"type":"export","path":null,"format":"jsonl"}"#
    );
    assert_eq!(
        sonic_rs::to_string(&Command::CancelQueued { turn })?,
        r#"{"type":"cancel_queued","turn":1}"#
    );
    Ok(())
}

#[test]
fn planned_replies_round_trip_with_wire_tags() -> TestResult {
    let turn = TurnId::new(NonZeroU64::MIN);
    let entry = EntryId::new(NonZeroU64::MIN);
    let job = JobId::new_v7();
    let replies = [
        Reply::Accepted {
            turn,
            message_id: entry,
        },
        Reply::Queued { turn: None },
        Reply::Queued { turn: Some(turn) },
        Reply::Done(Output::Nothing),
        Reply::Done(Output::Text("hi".into())),
        Reply::Done(Output::Markdown("# hi".into())),
        Reply::Done(Output::Table(vec![vec!["a".into(), "b".into()]])),
        Reply::Choose {
            chooser: Chooser::Model,
            filter: "".into(),
            thinking_levels: None,
        },
        Reply::Front(FrontAction::NewSession),
        Reply::Front(FrontAction::CopyReply { text: "x".into() }),
        Reply::Started(job),
    ];

    for reply in replies {
        let encoded = sonic_rs::to_string(&reply)?;
        let decoded: Reply = sonic_rs::from_str(&encoded)?;
        assert_eq!(decoded, reply, "round-trip failed for {encoded}");
    }

    assert_eq!(
        sonic_rs::to_string(&Reply::Done(Output::Nothing))?,
        r#"{"type":"done","output":{"type":"nothing"}}"#
    );
    assert_eq!(
        sonic_rs::to_string(&Reply::Queued { turn: None })?,
        r#"{"type":"queued"}"#
    );
    assert_eq!(
        sonic_rs::to_string(&Reply::Queued { turn: Some(turn) })?,
        r#"{"type":"queued","turn":1}"#
    );
    assert_eq!(
        sonic_rs::to_string(&Reply::Choose {
            chooser: Chooser::Model,
            filter: "".into(),
            thinking_levels: None,
        })?,
        r#"{"type":"choose","chooser":"model","filter":""}"#
    );
    assert_eq!(
        sonic_rs::from_str::<Reply>(r#"{"type":"queued"}"#)?,
        Reply::Queued { turn: None }
    );
    Ok(())
}

#[test]
fn command_error_wire_is_adjacently_tagged_with_snake_case_tags() -> TestResult {
    let errors = [
        (
            CommandError::Unknown {
                name: "modle".into(),
                suggestion: Some("model".into()),
            },
            r#"{"type":"unknown","value":{"name":"modle","suggestion":"model"}}"#,
        ),
        (
            CommandError::Unknown {
                name: "zzz".into(),
                suggestion: None,
            },
            r#"{"type":"unknown","value":{"name":"zzz","suggestion":null}}"#,
        ),
        (
            CommandError::Busy {
                cmd: "fork".into(),
                state: BusyState::Compacting,
            },
            r#"{"type":"busy","value":{"cmd":"fork","state":"compacting"}}"#,
        ),
        (
            CommandError::Busy {
                cmd: "fork".into(),
                state: BusyState::Running,
            },
            r#"{"type":"busy","value":{"cmd":"fork","state":"running"}}"#,
        ),
        (
            CommandError::Arity {
                cmd: "copy".into(),
                args: "1".into(),
            },
            r#"{"type":"arity","value":{"cmd":"copy","args":"1"}}"#,
        ),
        (
            CommandError::Lex {
                args: "\"abc".into(),
                error: LexError::UnclosedQuote { quote: '"', at: 0 },
            },
            r#"{"type":"lex","value":{"args":"\"abc","error":{"type":"unclosed_quote","quote":"\"","at":0}}}"#,
        ),
        (
            CommandError::Lex {
                args: "ab\\".into(),
                error: LexError::TrailingEscape { at: 2 },
            },
            r#"{"type":"lex","value":{"args":"ab\\","error":{"type":"trailing_escape","at":2}}}"#,
        ),
        (
            CommandError::JobCap {
                cmd: "compact".into(),
            },
            r#"{"type":"job_cap","value":{"cmd":"compact"}}"#,
        ),
        (CommandError::PluginCap, r#"{"type":"plugin_cap"}"#),
        (
            CommandError::Import(ImportFailure::MissingFile {
                abs: PathBuf::from("/x.jsonl"),
            }),
            r#"{"type":"import","value":{"type":"missing_file","abs":"/x.jsonl"}}"#,
        ),
        (
            CommandError::Import(ImportFailure::NotSession {
                abs: PathBuf::from("/x.jsonl"),
                line: 3,
                reason: "no header".into(),
            }),
            r#"{"type":"import","value":{"type":"not_session","abs":"/x.jsonl","line":3,"reason":"no header"}}"#,
        ),
        (
            CommandError::Import(ImportFailure::LineTooLong {
                abs: PathBuf::from("/x.jsonl"),
                line: 41,
            }),
            r#"{"type":"import","value":{"type":"line_too_long","abs":"/x.jsonl","line":41}}"#,
        ),
    ];
    for (error, wire) in errors {
        assert_eq!(sonic_rs::to_string(&error)?, wire);
        assert_eq!(sonic_rs::from_str::<CommandError>(wire)?, error, "{wire}");
    }
    Ok(())
}

#[test]
fn command_error_decoder_rejects_shape_drift() {
    for bad in [
        r#"{"type":"plugin-cap"}"#,
        r#"{"type":"future","value":{}}"#,
        r#"{"type":"busy","value":{"cmd":"fork","state":"idle"}}"#,
        r#"{"type":"busy","value":{"cmd":"fork"}}"#,
        r#"{"type":"lex","value":{"args":"a","error":{"type":"unclosed_quote","quote":"ab","at":0}}}"#,
        r#"{"type":"lex","value":{"args":"a","error":{"type":"unclosed_quote","quote":"\"","at":-1}}}"#,
        r#"{"type":"lex","value":{"args":"a","error":{"type":"unterminated","at":0}}}"#,
        r#"{"type":"import","value":{"type":"line_too_long","abs":"/x.jsonl","line":-1}}"#,
        r#"{"type":"import","value":{"type":"line_too_long","abs":"/x.jsonl"}}"#,
        r#"{"type":"import","value":{"type":"empty_file","abs":"/x.jsonl"}}"#,
    ] {
        assert!(
            sonic_rs::from_str::<CommandError>(bad).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn classify_table() {
    assert_eq!(
        classify("/model x"),
        Classify::Command {
            name: "model".into(),
            args: "x".into(),
        }
    );
    assert_eq!(
        classify("/skill:foo bar"),
        Classify::Command {
            name: "skill:foo".into(),
            args: "bar".into(),
        }
    );
    assert_eq!(
        classify("/quality:todos src --limit=5"),
        Classify::Command {
            name: "quality:todos".into(),
            args: "src --limit=5".into(),
        }
    );
    for text in ["", "hello", " /model", "/", "/ ", "/skill:", "/skill: "] {
        assert_eq!(classify(text), Classify::Text, "line {text:?}");
    }
}

#[test]
fn plugin_command_tail_lexes_to_tool_style_tokens() -> TestResult {
    let Classify::Command { name, args } = classify("/quality:todos src --limit=5") else {
        return Err("plugin command classified as text".into());
    };
    assert_eq!(name.as_ref(), "quality:todos");
    let got = tokens(&args)?;
    let got = got.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
    assert_eq!(got, ["src", "--limit=5"]);
    Ok(())
}

#[test]
fn shell_syntax_stays_literal() -> TestResult {
    let cases: &[(&str, &[&str])] = &[
        ("$HOME", &["$HOME"]),
        ("*.rs", &["*.rs"]),
        ("`x`", &["`x`"]),
        ("$(x)", &["$(x)"]),
        ("${x}", &["${x}"]),
        ("a|b", &["a|b"]),
        ("a;b", &["a;b"]),
        ("a&&b", &["a&&b"]),
        ("~/x", &["~/x"]),
        ("a>b", &["a>b"]),
        ("--limit=5 --limit=5", &["--limit=5", "--limit=5"]),
        ("$(rm -rf)", &["$(rm", "-rf)"]),
    ];
    for (raw, expected) in cases {
        let got = tokens(raw)?;
        let got = got.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
        assert_eq!(&got, expected, "input {raw:?}");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReferenceQuote {
    Single(usize),
    Double(usize),
}

fn reference_tokens(raw: &str) -> Result<Vec<Box<str>>, LexError> {
    let mut out = Vec::new();
    let mut token = String::new();
    let mut started = false;
    let mut quote: Option<ReferenceQuote> = None;
    let mut rest = raw.char_indices();
    while let Some((at, ch)) = rest.next() {
        if let Some(active) = quote {
            match (active, ch) {
                (ReferenceQuote::Single(_), '\'') | (ReferenceQuote::Double(_), '"') => {
                    quote = None;
                }
                (ReferenceQuote::Double(_), '\\') => {
                    if let Some((_, escaped)) = rest.next() {
                        token.push(escaped);
                    }
                }
                (_, other) => token.push(other),
            }
            continue;
        }
        match ch {
            ' ' | '\t' => {
                if started {
                    out.push(Box::<str>::from(token.as_str()));
                    token.clear();
                    started = false;
                }
            }
            '\'' => {
                started = true;
                quote = Some(ReferenceQuote::Single(at));
            }
            '"' => {
                started = true;
                quote = Some(ReferenceQuote::Double(at));
            }
            '\\' => {
                started = true;
                let (_, escaped) = rest.next().ok_or(LexError::TrailingEscape { at })?;
                token.push(escaped);
            }
            other => {
                started = true;
                token.push(other);
            }
        }
    }
    match quote {
        Some(ReferenceQuote::Single(at)) => Err(LexError::UnclosedQuote { quote: '\'', at }),
        Some(ReferenceQuote::Double(at)) => Err(LexError::UnclosedQuote { quote: '"', at }),
        None => {
            if started {
                out.push(Box::<str>::from(token.as_str()));
            }
            Ok(out)
        }
    }
}

fn grammar_char() -> impl Strategy<Value = char> {
    prop_oneof![
        Just(' '),
        Just('\t'),
        Just('\''),
        Just('"'),
        Just('\\'),
        Just('\n'),
        Just('\u{a0}'),
        Just('é'),
        proptest::char::range('a', 'z'),
    ]
}

proptest! {
    #[test]
    fn lexer_matches_reference_tokenizer(
        chars in proptest::collection::vec(grammar_char(), 0..32),
    ) {
        let raw: String = chars.into_iter().collect();
        prop_assert_eq!(reference_tokens(&raw), tokens(&raw));
    }
}

#[test]
fn set_model_with_provider_route_round_trips() -> TestResult {
    let route = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
        provider: Some("acme-proxy".into()),
    };
    let command = Command::SetModel {
        model: route,
        save: Save::SessionOnly,
    };
    let encoded = sonic_rs::to_string(&command)?;
    assert!(
        encoded.contains(r#""provider":"acme-proxy""#),
        "provider lost on the wire: {encoded}"
    );
    let decoded: Command = sonic_rs::from_str(&encoded)?;
    assert_eq!(decoded, command);
    Ok(())
}

#[test]
fn set_model_without_provider_decodes_to_no_provider() -> TestResult {
    let decoded: Command = sonic_rs::from_str(
        r#"{"type":"set_model","model":{"kind":"api","family":"openai_chat","model":"gpt-5"},"save":"session_only"}"#,
    )?;
    assert_eq!(
        decoded,
        Command::SetModel {
            model: ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-5".into(),
                provider: None,
            },
            save: Save::SessionOnly,
        }
    );
    Ok(())
}

#[test]
fn choose_reply_legacy_wire_decodes_without_thinking_levels() -> TestResult {
    let wire = r#"{"type":"choose","chooser":"model","filter":""}"#;
    let decoded: Reply = sonic_rs::from_str(wire)?;
    assert_eq!(
        decoded,
        Reply::Choose {
            chooser: Chooser::Model,
            filter: "".into(),
            thinking_levels: None,
        }
    );
    Ok(())
}

#[test]
fn choose_reply_with_thinking_levels_decodes_and_round_trips() -> TestResult {
    let wire = r#"{"type":"choose","chooser":"model","filter":"claude","thinkingLevels":["off","low","medium","high"]}"#;
    let decoded: Reply = sonic_rs::from_str(wire)?;
    let expected = Reply::Choose {
        chooser: Chooser::Model,
        filter: "claude".into(),
        thinking_levels: Some(
            vec![
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
            ]
            .into_boxed_slice(),
        ),
    };
    assert_eq!(decoded, expected);

    // Serialization omits thinkingLevels when None, includes when Some
    let encoded_none = sonic_rs::to_string(&Reply::Choose {
        chooser: Chooser::Model,
        filter: "".into(),
        thinking_levels: None,
    })?;
    assert_eq!(
        encoded_none,
        r#"{"type":"choose","chooser":"model","filter":""}"#
    );

    let encoded_some = sonic_rs::to_string(&expected)?;
    assert_eq!(encoded_some, wire);

    let round_tripped: Reply = sonic_rs::from_str(&encoded_some)?;
    assert_eq!(round_tripped, expected);

    // Explicit empty list represents supported chooser with no choices
    let empty_levels = Reply::Choose {
        chooser: Chooser::Model,
        filter: "".into(),
        thinking_levels: Some(Box::new([])),
    };
    let encoded_empty = sonic_rs::to_string(&empty_levels)?;
    assert_eq!(
        encoded_empty,
        r#"{"type":"choose","chooser":"model","filter":"","thinkingLevels":[]}"#
    );
    let decoded_empty: Reply = sonic_rs::from_str(&encoded_empty)?;
    assert_eq!(decoded_empty, empty_levels);

    Ok(())
}

#[test]
fn choose_reply_ignores_forward_unknown_fields() -> TestResult {
    let wire_with_unknown = r#"{"type":"choose","chooser":"model","filter":"","thinkingLevels":["low"],"futureField":true,"version":42}"#;
    let decoded: Reply = sonic_rs::from_str(wire_with_unknown)?;
    assert_eq!(
        decoded,
        Reply::Choose {
            chooser: Chooser::Model,
            filter: "".into(),
            thinking_levels: Some(vec![ThinkingLevel::Low].into_boxed_slice()),
        }
    );

    let legacy_with_unknown =
        r#"{"type":"choose","chooser":"model","filter":"","nextProtocol":123}"#;
    let decoded_legacy: Reply = sonic_rs::from_str(legacy_with_unknown)?;
    assert_eq!(
        decoded_legacy,
        Reply::Choose {
            chooser: Chooser::Model,
            filter: "".into(),
            thinking_levels: None,
        }
    );
    Ok(())
}
