//! Empty-host and headless-denial regression tests.
//!
//! An empty [`Product`] (no extensions) runs text-only turns with an empty
//! system prompt and zero tools, keeps resume data in [`View`](dal_core::View),
//! answers model tool calls with the typed unknown-tool error, and fails
//! context overflow with the exact `compaction_none` notice plus the
//! `Context overflow recovery failed` turn end. A headless session with
//! nobody to ask denies patch (edits rung) and exec (all rung) calls with
//! the spec's stderr wording and never opens a request.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions abort on failure"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, EntryKind, Expect, JournalPart, PageReq, Part, Reply,
    Stop, UpdateKind, Workspace,
};

const TEXT_HELLO: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"Hel\"},{\"type\":\"text_delta\",\"text\":\"lo\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

const UNKNOWN_TOOL_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"call-1\",\"name\":\"read\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";

fn fixture_path(data: &std::path::Path, name: &str, lines: &[&str]) -> String {
    let path = data.join(name);
    std::fs::write(&path, lines.concat()).expect("fixture writes");
    path.to_string_lossy().replace('\\', "\\\\")
}

fn empty_product(data_root: std::path::PathBuf) -> Product {
    Product {
        name: "dal",
        data_root,
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    }
}

async fn start_host(
    data: &std::path::Path,
    workspace_dir: &std::path::Path,
    fixture: &str,
    extra_config: &str,
) -> Host {
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n{extra_config}\n[providers.scripted]\nfixture = \"{fixture}\"\n",
    );
    let config =
        Config::load(ConfigProduct::Dalgon, data, "", Some(user.as_str())).expect("config loads");
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.to_path_buf(),
        sandbox_helper: None,
    };
    Host::start(empty_product(data.to_path_buf()), config, env)
        .await
        .expect("host starts")
}

async fn collect_until_end(
    subscription: &mut dal_agent::Subscription,
) -> (Option<Stop>, Vec<dal_core::Update>) {
    let mut updates = Vec::new();
    for _ in 0..60 {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("delivery arrives")
            .expect("stream stays open");
        let Delivery::Update(update) = delivery else {
            continue;
        };
        if let UpdateKind::TurnEnded { stop, .. } = &update.kind {
            let stop = *stop;
            updates.push((*update).clone());
            return (Some(stop), updates);
        }
        updates.push((*update).clone());
    }
    panic!("turn never ended");
}

fn update_texts(updates: &[dal_core::Update]) -> Vec<String> {
    let mut texts = Vec::new();
    for update in updates {
        match &update.kind {
            UpdateKind::Notice(notice) => {
                texts.push(format!("notice:{}:{}", notice.kind, notice.text));
            }
            UpdateKind::ToolSettled { outcome, .. } => {
                texts.push(format!(
                    "settled:is_error={}:{}",
                    outcome.is_error, outcome.text
                ));
            }
            UpdateKind::Delta { text, .. } => {
                texts.push(format!("delta:{text}"));
            }
            UpdateKind::Tree(delta) => {
                for entry in &delta.added {
                    if let dal_core::EntryKind::ToolResult { error, parts, .. } = &entry.kind {
                        let body = parts
                            .iter()
                            .filter_map(|part| match part {
                                dal_core::JournalPart::Text { text } => Some(text.as_ref()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        texts.push(format!("result:is_error={error}:{body}"));
                    }
                }
            }
            _ => {}
        }
    }
    texts
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the scenario drives a whole empty host in one walk"
)]
async fn empty_host_prompt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = fixture_path(&data, "hello.jsonl", &[TEXT_HELLO]);
    let host = start_host(&data, &workspace_dir, &fixture, "").await;
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "hi".into() }],
        })
        .await
        .expect("submit");
    assert!(
        matches!(reply, Reply::Accepted { .. }),
        "text prompt is accepted on the empty host: {reply:?}"
    );
    let (stop, updates) = collect_until_end(&mut subscription).await;
    assert_eq!(
        stop,
        Some(Stop::EndTurn),
        "text answers end the turn on the empty host"
    );
    let texts = update_texts(&updates);
    assert!(
        texts.iter().all(|text| !text.starts_with("settled:")),
        "zero tools means no tool settles on the empty host: {texts:?}"
    );
    let view = agent.view(PageReq::default()).expect("view");
    assert_eq!(
        view.entries.items.len(),
        2,
        "one user entry plus one answer stay in View.entries: {}",
        view.entries.items.len()
    );
    let [first, last] = view.entries.items.as_slice() else {
        panic!("expected exactly the user entry and the answer");
    };
    let EntryKind::User { parts } = &first.kind else {
        panic!("the first entry is the user prompt: {:?}", first.kind);
    };
    assert!(
        parts.iter().any(|part| matches!(
            part,
            JournalPart::Text { text } if text.as_ref() == "hi"
        )),
        "the user entry carries the prompt text"
    );
    let EntryKind::Assistant { content, .. } = &last.kind else {
        panic!("the last entry is the model answer: {:?}", last.kind);
    };
    assert!(
        content.iter().any(|block| matches!(
            block,
            dal_core::Block::Text { text, .. } if text.as_ref().contains("Hello")
        )),
        "the answer entry carries the scripted Hello text"
    );
    assert!(
        !view
            .entries
            .items
            .iter()
            .any(|entry| matches!(entry.kind, EntryKind::ToolResult { .. })),
        "zero tools means no tool results on the empty host"
    );
    assert_eq!(
        view.turn,
        dal_core::TurnState::Idle,
        "the turn settles back to idle"
    );
    let session_id = view.session.id.to_string();
    drop(subscription);
    drop(agent);
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let resumed = host
        .open(
            SessionRef::Resume {
                key: session_id.into(),
                workspace,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("resume");
    let reopened = resumed.view(PageReq::default()).expect("view");
    assert_eq!(
        reopened.entries.items.len(),
        view.entries.items.len(),
        "resume keeps View.entries on the empty host"
    );
    host.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn empty_host_unknown_tool() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = fixture_path(&data, "unknown.jsonl", &[UNKNOWN_TOOL_CALL, TEXT_HELLO]);
    let host = start_host(&data, &workspace_dir, &fixture, "").await;
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "read it".into(),
            }],
        })
        .await
        .expect("submit");
    assert!(
        matches!(reply, Reply::Accepted { .. }),
        "prompt is accepted: {reply:?}"
    );
    let (_stop, updates) = collect_until_end(&mut subscription).await;
    let texts = update_texts(&updates);
    assert!(
        texts
            .iter()
            .any(|text| text.contains("unknown tool") && text.contains("read")),
        "the empty host answers the unknown read call with the typed error text: {texts:?}\nupdates: {updates:#?}"
    );
    host.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn empty_host_overflow_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let first = "{\"kind\":\"fail\",\"status\":413,\"family\":\"openai_chat\",\"message\":\"request too large\"}\n";
    let fixture = fixture_path(&data, "overflow.jsonl", &[first, first]);
    let host = start_host(&data, &workspace_dir, &fixture, "").await;
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "hi".into() }],
        })
        .await
        .expect("submit");
    let (stop, updates) = collect_until_end(&mut subscription).await;
    assert_eq!(
        stop,
        Some(Stop::Failed),
        "context overflow on the empty host ends the turn failed"
    );
    let texts = update_texts(&updates);
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.starts_with("notice:compaction_none"))
            .count(),
        1,
        "the empty host emits exactly one compaction_none notice: {texts:?}"
    );
    assert!(
        matches!(
            updates.last().map(|update| &update.kind),
            Some(UpdateKind::TurnEnded { .. })
        ),
        "the failed TurnEnded closes the update stream"
    );
    let view = agent.view(PageReq::default()).expect("view");
    let mut journal_text = texts.join("\n");
    for entry in &view.entries.items {
        if let EntryKind::ToolResult { parts, .. } = &entry.kind {
            for part in parts {
                if let JournalPart::Text { text } = part {
                    journal_text.push_str(text.as_ref());
                    journal_text.push('\n');
                }
            }
        }
    }
    assert!(
        journal_text.contains("Context overflow recovery failed"),
        "the turn ends failed with Context overflow recovery failed: {journal_text}"
    );
    host.shutdown(Duration::from_secs(5)).await;
}

struct DenyTool {
    name: dal_core::Name,
    class: dal_core::ToolClass,
    spec: Arc<dal_core::ToolSpec>,
}

impl dal_agent::ext::tool::Tool for DenyTool {
    fn name(&self) -> &dal_core::Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<dal_core::ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(
        &self,
        _args: &dal_agent::ext::tool::RawValue,
        _ws: &Workspace,
    ) -> Result<dal_core::ToolClass, dal_agent::ext::tool::ArgError> {
        Ok(self.class.clone())
    }

    fn run<'a>(
        &'a self,
        _call: dal_agent::ext::tool::ToolCall,
        mut cx: dal_agent::ext::tool::ToolCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, dal_agent::ext::tool::ToolOutcome> {
        Box::pin(async move {
            // A gated tool enters the approval ladder through its context;
            // the dispatcher never authorizes a call on the tool's behalf.
            let preview = dal_core::Preview {
                title: self.name.as_str().into(),
                body: String::new().into(),
                digest: None,
            };
            match cx.authorize(preview).await {
                Ok(_) => dal_agent::ext::tool::ToolOutcome::Ok(Box::new(
                    dal_agent::ext::tool::ToolOutput::from_text("ran"),
                )),
                Err(reason) => dal_agent::ext::tool::ToolOutcome::Err(
                    dal_agent::error::ToolError::Denied(reason),
                ),
            }
        })
    }
}

fn deny_tool(name: &str, class: dal_core::ToolClass) -> Arc<DenyTool> {
    let parsed: dal_core::Name = name.parse().expect("valid tool name");
    Arc::new(DenyTool {
        name: parsed.clone(),
        class,
        spec: Arc::new(dal_core::ToolSpec {
            name: parsed,
            description: format!("test tool {name}").into(),
            parameters: dal_core::RawJson::parse(r#"{"type":"object"}"#).expect("valid schema"),
            grammar: None,
        }),
    })
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the scenario drives a whole headless session in one walk"
)]
async fn headless_denial_stderr() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let calls = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"call-patch\",\"name\":\"patchgate\",\"args\":{\"kind\":\"parsed\",\"value\":{}}},{\"id\":\"call-exec\",\"name\":\"execgate\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
    let fixture = fixture_path(&data, "denial.jsonl", &[calls, TEXT_HELLO]);
    let extension =
        dal_agent::ext::ExtensionBuilder::new("headless", "1.0.0", dal_core::ServiceSet::EMPTY)
            .expect("valid extension identity")
            .tool(
                deny_tool("patchgate", dal_core::ToolClass::Patch),
                dal_core::Visibility::Model,
            )
            .tool(
                deny_tool(
                    "execgate",
                    dal_core::ToolClass::Exec {
                        read_only: false,
                        grant: None,
                    },
                ),
                dal_core::Visibility::Model,
            )
            .build()
            .expect("valid extension");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\napproval = \"ask\"\n[providers.scripted]\nfixture = \"{fixture}\"\n",
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config loads");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env)
        .await
        .expect("host starts");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(SessionRef::Ephemeral { workspace }, ClientId::new("probe"))
        .await
        .expect("open");
    // Listen-only: with nobody attached to answer, ask-mode approvals take
    // the headless denial path without ever opening a request.
    let mut subscription = agent.subscribe_listen(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Call patchgate then execgate.".into(),
            }],
        })
        .await
        .expect("submit");
    let mut updates = Vec::new();
    let mut opened_requests = 0usize;
    let mut saw_end = false;
    for _ in 0..60 {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("delivery arrives")
            .expect("stream stays open");
        let Delivery::Update(update) = delivery else {
            continue;
        };
        if let UpdateKind::RequestOpened(_) = &update.kind {
            opened_requests += 1;
        }
        let ended = matches!(update.kind, UpdateKind::TurnEnded { .. });
        updates.push((*update).clone());
        if ended {
            saw_end = true;
            break;
        }
    }
    let settled: Vec<String> = update_texts(&updates)
        .into_iter()
        .filter(|text| text.starts_with("result:"))
        .collect();
    assert!(saw_end, "the denied turn still ends");
    assert_eq!(
        opened_requests, 0,
        "a headless denial never opens an approval request"
    );
    assert_eq!(
        settled.len(),
        2,
        "both the patch and the exec call settle denied: {settled:?}"
    );
    assert!(
        settled[0].contains("result:is_error=true")
            && settled[0].contains("patchgate")
            && settled[0].contains("needs approval, and this run has no one to ask.")
            && settled[0].contains("rerun with --approval edits"),
        "the patch denial carries the edits-rung stderr line: {}",
        settled[0]
    );
    assert!(
        settled[1].contains("result:is_error=true")
            && settled[1].contains("execgate")
            && settled[1].contains("needs approval, and this run has no one to ask.")
            && settled[1].contains("rerun with --approval all"),
        "the exec denial carries the all-rung stderr line: {}",
        settled[1]
    );
    for (outcome, tool, rung) in [
        (&settled[0], "patchgate", dal_core::Rung::Edits),
        (&settled[1], "execgate", dal_core::Rung::All),
    ] {
        let text = outcome
            .strip_prefix("result:is_error=true:")
            .expect("outcome text follows the flag");
        let (parsed_tool, parsed_rung) =
            dal_core::parse_headless_denial(text).expect("denial parses back");
        assert!(
            parsed_tool.contains(tool),
            "the denial names its tool: {parsed_tool}"
        );
        assert_eq!(
            parsed_rung, rung,
            "the denial names the lowest rung that allows it"
        );
    }
    host.shutdown(Duration::from_secs(5)).await;
}
