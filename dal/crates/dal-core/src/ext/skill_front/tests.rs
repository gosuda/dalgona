use std::path::Path;

use super::super::{McpBlock, McpBlockError, McpServerDecl, ServerShapeError, validate_block};
use super::decode_skill_mcp;

fn decode(source: &str) -> Result<Option<McpBlock>, String> {
    decode_skill_mcp(Path::new("plug/SKILL.md"), source).map_err(|error| error.to_string())
}

fn err(source: &str) -> String {
    match decode(source) {
        Err(message) => message,
        Ok(found) => panic!("expected a load error, decoded {found:?}"),
    }
}

const VALID: &str = "\u{feff}---\r\nname: docs\r\ndescription: Docs helper\r\nmcp:\r\n  # remote docs\r\n  servers:\r\n    web:\r\n      url: https://docs.example/mcp\r\n    files:\r\n      command: [npx, \"-y\", 'files server']\r\n      env:\r\n        TOKEN: \"a: b\"  # literal\r\n        MODE: fast\r\n---\r\n# Body\r\n";

const LF_BOM: &str =
    "\u{feff}---\nmcp:\n  servers:\n    web:\n      url: https://docs.example/mcp\n---\n# Body\n";

#[test]
fn decodes_both_transports_from_crlf_bom_front_matter() {
    let block = decode(VALID).unwrap().unwrap();
    assert_eq!(block.servers.len(), 2);
    assert_eq!(
        block.servers["web"],
        McpServerDecl::Http {
            url: "https://docs.example/mcp".into()
        }
    );
    assert_eq!(
        block.servers["files"],
        McpServerDecl::Stdio {
            command: vec!["npx".into(), "-y".into(), "files server".into()],
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
            url: "https://docs.example/mcp".into()
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
        "plug/SKILL.md:6:7: unknown key \"timeout\"; expected command, env, url"
    );
}

#[test]
fn transport_rules_report_at_the_server_key() {
    let both =
        "---\nmcp:\n  servers:\n    a:\n      command: [x]\n      url: https://h.example\n---\n";
    assert_eq!(
        err(both),
        "plug/SKILL.md:4:5: mcp server \"a\": a server needs exactly one of \"command\" or \"url\"; found both"
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
fn value_rules_are_load_errors_at_the_server_key() {
    let cases = [
        ("url: http://h.example/x", "an absolute https URL"),
        ("url: https://", "an absolute https URL"),
        ("command: []", "a non-empty array"),
        ("command: [\"\"]", "must start with a program name"),
        (
            "command: [x]\n      env:\n        1BAD: v",
            "env key \"1BAD\"",
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
            "---\nmcp:\n  servers:\n    a:\n      command: [x\n---\n",
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
    let source = "---\nmcp:\n  servers:\n    a:\n      command: [1, true, null]\n---\n";
    let block = decode(source).unwrap().unwrap();
    assert_eq!(
        block.servers["a"],
        McpServerDecl::Stdio {
            command: vec!["1".into(), "true".into(), "null".into()],
            env: std::collections::BTreeMap::new(),
        }
    );
}

#[test]
fn serde_form_rejects_unknown_keys_and_bad_transports() {
    let ok =
        r#"{"servers":{"a":{"command":["x"],"env":{"K":"v"}},"b":{"url":"https://h.example"}}}"#;
    let block: McpBlock = sonic_rs::from_str(ok).unwrap();
    assert_eq!(sonic_rs::to_string(&block).unwrap(), ok);
    for bad in [
        r#"{"servers":{},"extra":1}"#,
        r#"{"servers":{"a":{"url":"https://h","port":1}}}"#,
        r#"{"servers":{"a":{"command":["x"],"url":"https://h"}}}"#,
        r#"{"servers":{"a":{}}}"#,
    ] {
        assert!(sonic_rs::from_str::<McpBlock>(bad).is_err(), "{bad}");
    }
}

#[test]
fn validate_block_reports_one_error_per_bad_entry() {
    let block = McpBlock {
        servers: [
            (
                "a b".into(),
                McpServerDecl::Http {
                    url: "http://h".into(),
                },
            ),
            (
                "ok".into(),
                McpServerDecl::Stdio {
                    command: vec!["x".into()],
                    env: [("-K".into(), "v".into())].into(),
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
            McpBlockError::Url {
                server: "a b".into()
            },
            McpBlockError::EnvKey {
                server: "ok".into(),
                key: "-K".into()
            },
        ]
    );
    assert_eq!(validate_block(&McpBlock::default()), Ok(()));
    assert!(matches!(
        decode("---\nmcp:\n  servers: x\n---\n"),
        Err(message) if message.contains("a mapping of server names")
    ));
}
