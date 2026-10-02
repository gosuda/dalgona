//! Codex schema fixtures match the supported method surface.
use sonic_rs::JsonValueTrait;

const SCHEMA: &str =
    include_str!("../../crates/dalgon/tests/fixtures/codex-app-server/protocol.schema.json");

#[derive(serde::Deserialize)]
struct SupportedMethods {
    source_commit: String,
    requests: Vec<String>,
    notifications: Vec<String>,
    server_requests: Vec<String>,
}

#[test]
fn codex_schema_fixtures_match_supported_methods()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/codex-app-server");
    let supported: SupportedMethods = sonic_rs::from_str(&std::fs::read_to_string(
        root.join("supported-methods.json"),
    )?)?;
    assert_eq!(supported.source_commit, "5c8fc15cc");
    assert_eq!(
        supported.requests,
        [
            "initialize",
            "initialized",
            "thread/start",
            "thread/resume",
            "thread/list",
            "turn/start",
            "turn/interrupt",
        ]
    );
    assert_eq!(
        supported.notifications,
        [
            "thread/started",
            "turn/started",
            "item/started",
            "item/agentMessage/delta",
            "item/completed",
            "turn/completed",
        ]
    );
    assert_eq!(
        supported.server_requests,
        [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/tool/requestUserInput",
        ]
    );

    for schema_type in [
        "InitializeParams",
        "ThreadStartParams",
        "TurnStartParams",
        "ServerNotification",
        "ServerRequest",
    ] {
        assert!(
            SCHEMA.contains(schema_type),
            "missing {schema_type} from pinned schema"
        );
    }
    for name in [
        "initialize.request.json",
        "thread-start.request.json",
        "turn-start.request.json",
        "unknown-method.request.json",
    ] {
        let bytes = std::fs::read(root.join(name))?;
        let text = std::str::from_utf8(&bytes)?;
        assert!(!text.contains("jsonrpc"), "Codex frames omit jsonrpc");
        let frame: sonic_rs::Value = sonic_rs::from_str(text)?;
        let method = frame.get("method").and_then(sonic_rs::Value::as_str);
        assert!(method.is_some(), "{name} must name one method");
        if name == "unknown-method.request.json" {
            assert!(
                !supported
                    .requests
                    .iter()
                    .any(|known| Some(known.as_str()) == method)
            );
        }
    }
    Ok(())
}
