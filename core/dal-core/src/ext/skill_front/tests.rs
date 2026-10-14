use std::path::Path;

use super::super::{
    McpBlock, McpBlockError, McpEndpoint, McpLaunch, McpServerDecl, ServerShapeError,
    validate_block,
};
use super::decode_skill_mcp;

fn decode(source: &str) -> Result<Option<McpBlock>, String> {
    decode_skill_mcp(Path::new("plug/SKILL.md"), source).map_err(|error| error.to_string())
}

fn endpoint(url: &str) -> McpEndpoint {
    McpEndpoint::parse(url).unwrap()
}

fn command(argv: &[&str]) -> McpLaunch {
    McpLaunch::command(argv.iter().map(|value| (*value).into()).collect()).unwrap()
}

fn err(source: &str) -> String {
    match decode(source) {
        Err(message) => message,
        Ok(found) => panic!("expected a load error, decoded {found:?}"),
    }
}

const VALID: &str = "\u{feff}---\r\nname: docs\r\ndescription: Docs helper\r\nmcp:\r\n  # remote docs\r\n  servers:\r\n    web:\r\n      url: https://docs.example/mcp\r\n    files:\r\n      launch:\r\n        kind: npx\r\n        package: files-server\r\n        args: [\"files server\"]\r\n      env:\r\n        TOKEN: \"a: b\"  # literal\r\n        MODE: fast\r\n---\r\n# Body\r\n";

const LF_BOM: &str =
    "\u{feff}---\nmcp:\n  servers:\n    web:\n      url: https://docs.example/mcp\n---\n# Body\n";

#[test]
fn decodes_both_transports_from_crlf_bom_front_matter() {
    let block = decode(VALID).unwrap().unwrap();
    assert_eq!(block.servers.len(), 2);
    assert_eq!(
        block.servers["web"],
        McpServerDecl::Http {
            endpoint: endpoint("https://docs.example/mcp")
        }
    );
    assert_eq!(
        block.servers["files"],
        McpServerDecl::Stdio {
            launch: McpLaunch::npx("files-server".into(), None, vec!["files server".into()])
                .unwrap(),
            env: [
                ("MODE".into(), "fast".into()),
                ("TOKEN".into(), "a: b".into())
            ]
            .into(),
        }
    );
}

#[test]
fn decodes_bom_with_lf_fences() {
    let block = decode(LF_BOM).unwrap().unwrap();
    assert_eq!(
        block.servers["web"],
        McpServerDecl::Http {
            endpoint: endpoint("https://docs.example/mcp")
        }
    );
}

#[test]
fn no_block_or_no_mcp_key_declares_nothing() {
    assert_eq!(decode("# Just a body\n"), Ok(None));
    assert_eq!(decode("---\nname: x\n---\nbody"), Ok(None));
    assert_eq!(
        decode("---\nmcp-notes: x\nother:\n  servers: 1\n---\n"),
        Ok(None)
    );
    assert_eq!(decode(""), Ok(None));
}

#[test]
fn unclosed_block_is_a_load_error_at_the_opening_fence() {
    assert_eq!(
        err("---\nmcp:\n  servers:\n"),
        "plug/SKILL.md:1:1: the front matter has no closing \"---\" line"
    );
}

#[test]
fn unknown_key_in_the_mcp_object_names_its_position() {
    let source =
        "---\nname: x\nmcp:\n  servers:\n    a:\n      url: https://h.example/x\n  extra: 1\n---\n";
    assert_eq!(
        err(source),
        "plug/SKILL.md:7:3: unknown key \"extra\"; expected servers"
    );
}

#[test]
fn unknown_key_in_a_server_object_names_its_position() {
    let source =
        "---\nmcp:\n  servers:\n    a:\n      url: https://h.example/x\n      timeout: 5\n---\n";
    assert_eq!(
        err(source),
        "plug/SKILL.md:6:7: unknown key \"timeout\"; expected launch, env, url"
    );
}

#[test]
fn unknown_key_in_a_launch_object_names_its_position() {
    let source = "---\nmcp:\n  servers:\n    a:\n      launch:\n        kind: command\n        argv: [x]\n        shell: true\n---\n";
    assert_eq!(
        err(source),
        "plug/SKILL.md:8:9: unknown key \"shell\"; expected kind, argv, package, version, executable, args"
    );
}

#[test]
fn transport_rules_report_at_the_server_key() {
    let both = "---\nmcp:\n  servers:\n    a:\n      launch:\n        kind: command\n        argv: [x]\n      url: https://h.example\n---\n";
    assert_eq!(
        err(both),
        "plug/SKILL.md:4:5: mcp server \"a\": a server needs exactly one of \"launch\" or \"url\"; found both"
    );
    let neither = "---\nmcp:\n  servers:\n    ok:\n      url: https://h.example\n    b:\n      env:\n        K: v\n---\n";
    let Err(message) = decode(neither) else {
        panic!("a server without a transport must fail")
    };
    assert!(message.starts_with("plug/SKILL.md:6:5:"), "{message}");
    assert!(message.ends_with("found neither"), "{message}");
    let env_url = "---\nmcp:\n  servers:\n    a:\n      url: https://h.example\n      env:\n        K: v\n---\n";
    let Err(message) = decode(env_url) else {
        panic!("env beside url must fail")
    };
    assert!(
        message.contains(&ServerShapeError::EnvWithUrl.to_string()),
        "{message}"
    );
}

#[test]
fn duplicate_launch_fields_are_rejected_at_the_second_key() {
    let source = "---\nmcp:\n  servers:\n    a:\n      launch:\n        kind: command\n        kind: npx\n        argv: [x]\n---\n";
    assert_eq!(err(source), "plug/SKILL.md:7:9: \"kind\" appears twice");
}

#[test]
fn value_rules_are_load_errors_at_the_server_key() {
    let cases = [
        ("url: http://h.example/x", "an absolute https URL"),
        ("url: https://", "an absolute https URL"),
        (
            "launch:\n        kind: command\n        argv: []",
            "a non-empty array",
        ),
        (
            "launch:\n        kind: command\n        argv: [\"\"]",
            "must start with a program name",
        ),
        (
            "launch:\n        kind: npx\n        package: ./local",
            "MCP package name is invalid",
        ),
    ];
    for (server, want) in cases {
        let source = format!("---\nmcp:\n  servers:\n    a:\n      {server}\n---\n");
        let message = err(&source);
        assert!(message.starts_with("plug/SKILL.md:4:5:"), "{message}");
        assert!(message.contains(want), "{message}");
    }
    let bad_name = "---\nmcp:\n  servers:\n    bad name:\n      url: https://h.example\n---\n";
    assert!(err(bad_name).contains("mcp server name \"bad name\" is invalid"));
}

#[test]
fn shape_and_syntax_faults_are_positioned() {
    let cases = [
        ("---\nmcp: {servers: {}}\n---\n", "plug/SKILL.md:2:6:"),
        ("---\nmcp:\n---\n", "plug/SKILL.md:2:1:"),
        ("---\nmcp:\n  servers:\n---\n", "plug/SKILL.md:3:11:"),
        ("---\nmcp:\n  servers:\n\ta: 1\n---\n", "plug/SKILL.md:4:1:"),
        (
            "---\nmcp:\n  servers:\n    a:\n      launch: [x\n---\n",
            "plug/SKILL.md:5:",
        ),
        (
            "---\nmcp:\n  servers:\n    a:\n      url: \"https://h\n---\n",
            "plug/SKILL.md:5:12:",
        ),
        (
            "---\nmcp:\n  servers:\n    a:\n      url: https://h.example\n    a:\n      url: https://i.example\n---\n",
            "plug/SKILL.md:6:5:",
        ),
        (
            "---\nmcp:\n  servers:\n    a:\n      url: https://h.example\n   b:\n---\n",
            "plug/SKILL.md:6:4:",
        ),
        (
            "---\nmcp:\n  servers:\n    a:\n      url: https://h.example\nmcp:\n---\n",
            "plug/SKILL.md:6:1:",
        ),
    ];
    for (source, at) in cases {
        let message = err(source);
        assert!(message.starts_with(at), "{source:?} -> {message}");
    }
}

#[test]
fn every_scalar_stays_a_string() {
    let source = "---\nmcp:\n  servers:\n    a:\n      launch:\n        kind: command\n        argv: [1, true, null]\n---\n";
    let block = decode(source).unwrap().unwrap();
    assert_eq!(
        block.servers["a"],
        McpServerDecl::Stdio {
            launch: command(&["1", "true", "null"]),
            env: std::collections::BTreeMap::new(),
        }
    );
}

#[test]
fn serde_form_rejects_unknown_keys_and_bad_transports() {
    let ok = r#"{"servers":{"a":{"launch":{"kind":"command","argv":["x"]},"env":{"K":"v"}},"b":{"url":"https://h.example"}}}"#;
    let block: McpBlock = sonic_rs::from_str(ok).unwrap();
    let encoded = sonic_rs::to_string(&block).unwrap();
    let restored: McpBlock = sonic_rs::from_str(&encoded).unwrap();
    assert_eq!(restored, block);
    for bad in [
        r#"{"servers":{},"extra":1}"#,
        r#"{"servers":{"a":{"url":"https://h","port":1}}}"#,
        r#"{"servers":{"a":{"launch":{"kind":"command","argv":["x"]},"url":"https://h"}}}"#,
        r#"{"servers":{"a":{}}}"#,
        r#"{"servers":{"a":{"command":["x"]}}}"#,
        r#"{"servers":{"a":{"launch":{"kind":"command","argv":null}}}}"#,
    ] {
        assert!(sonic_rs::from_str::<McpBlock>(bad).is_err(), "{bad}");
    }
}

#[test]
fn malformed_front_matter_operands_are_redacted_at_owned_boundary() {
    let source = "---\nmcp:\n  servers:\n    a:\n      launch:\n        kind: npx\n        package: server\n        args: MCP_ARG_SECRET\n---\n";
    let message = err(source);
    assert!(message.starts_with("plug/SKILL.md:8:"), "{message}");
    assert!(message.contains("\"args\" must be a list"), "{message}");
    assert!(!message.contains("MCP_ARG_SECRET"), "{message}");
}

#[test]
fn validate_block_reports_secret_safe_value_errors() {
    let block = McpBlock {
        servers: [
            (
                "a b".into(),
                McpServerDecl::Http {
                    endpoint: endpoint("https://h.example"),
                },
            ),
            (
                "ok".into(),
                McpServerDecl::Stdio {
                    launch: command(&["x"]),
                    env: [
                        ("-K".into(), "v".into()),
                        ("SAFE".into(), "secret\0value".into()),
                    ]
                    .into(),
                },
            ),
        ]
        .into(),
    };
    let errors = validate_block(&block).unwrap_err();
    assert_eq!(
        errors,
        vec![
            McpBlockError::ServerName {
                server: "a b".into()
            },
            McpBlockError::EnvKey {
                server: "ok".into(),
                key: "-K".into()
            },
            McpBlockError::EnvValue {
                server: "ok".into()
            },
        ]
    );
    assert_eq!(validate_block(&McpBlock::default()), Ok(()));
}

#[test]
fn old_command_field_is_rejected_with_migration_hint() {
    let source = "---\nmcp:\n  servers:\n    a:\n      command: [x]\n---\n";
    let message = err(source);
    assert!(
        message.contains("\"command\"")
            && message.contains("launch")
            && message.contains("renamed"),
        "{message}"
    );
}

#[test]
fn url_userinfo_and_fragment_are_value_errors() {
    for url in [
        "https://user@example.com/mcp",
        "https://user:pass@example.com/mcp",
        "https://example.com/mcp#fragment",
    ] {
        let source = format!("---\nmcp:\n  servers:\n    a:\n      url: {url}\n---\n");
        let message = err(&source);
        assert!(message.contains("absolute https URL"), "{url} -> {message}");
        assert!(!message.contains("example.com"), "{url} -> {message}");
    }
}
