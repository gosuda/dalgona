use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::path::Path;
use std::time::Duration;

use dal_agent::{Env, Host, Product};
use dal_core::{
    Answer, AssistantStop, Block, CallGrant, CallId, Choice, Config, ConfigProduct, EntryId,
    EntryKind, EntryView, Family, JobEnd, JobId, JournalPart, Owner, Preview, Question, RawJson,
    Request, RequestId, SessionId, SessionInfo, Stop, StreamChannel, Timestamp, ToolOutcomeView,
    TreeDelta, TurnId, UpdateKind, Usage, Workspace,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use crate::codex::ask::{client_answer, decision_answer, server_request};
use crate::codex::items::{Event, TurnStream, turn_value};
use crate::codex::threads::thread_value;
use crate::serve_codex;
use crate::transport::{MemoryPeer, MemoryTransport};

mod schema_check;

use schema_check::{assert_valid, schema};

fn nonzero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("nonzero")
}

fn usage() -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

fn entries(added: Vec<EntryView>) -> UpdateKind {
    UpdateKind::Tree(TreeDelta { added, leaf: None })
}

fn entry(id: u64, kind: EntryKind) -> EntryView {
    EntryView {
        id: EntryId::new(nonzero(id)),
        parent: None,
        kind,
    }
}

fn assistant(content: Vec<Block>, stop: AssistantStop) -> EntryKind {
    EntryKind::Assistant {
        api: Family::Anthropic,
        model: "m".into(),
        content,
        usage: usage(),
        stop,
    }
}

fn notifications(stream: &mut TurnStream, kinds: &[UpdateKind]) -> Vec<(&'static str, Value)> {
    kinds
        .iter()
        .flat_map(|kind| stream.apply(kind))
        .filter_map(|event| match event {
            Event::Notify(method, params) => Some((method, params)),
            _ => None,
        })
        .collect()
}

fn request(question: Question) -> Request {
    Request {
        id: RequestId::new_v7(),
        turn: Some(TurnId::new(nonzero(1))),
        owner: Owner::Core,
        question,
        timeout: Duration::from_secs(30),
        default: Answer::Decline,
    }
}

fn preview(title: &str, body: &str) -> Preview {
    Preview {
        title: title.into(),
        body: body.into(),
        digest: None,
    }
}

fn questions() -> Vec<Question> {
    vec![
        Question::Approval {
            tool: "exec".into(),
            preview: preview("git push", "git push origin main"),
            grant: Some(CallGrant {
                argv_prefix: "git".into(),
                roots: vec!["/w".into()],
                until: JobEnd(JobId::new_v7()),
            }),
        },
        Question::Approval {
            tool: "patch".into(),
            preview: preview("src/lib.rs", "+fn main() {}"),
            grant: None,
        },
        Question::Grant {
            ext: "web".into(),
            origin: "user".into(),
            capabilities: vec!["net".into(), "fs.write".into(), "run".into()],
            detail: None,
        },
        Question::Select {
            prompt: "Choose a branch".into(),
            options: vec![
                Choice {
                    label: "main".into(),
                    description: None,
                },
                Choice {
                    label: "dev".into(),
                    description: Some("the dev branch".into()),
                },
            ],
            multi: false,
            preview: None,
        },
        Question::Confirm {
            text: "Continue?".into(),
        },
        Question::Text {
            prompt: "Commit message".into(),
            placeholder: None,
        },
    ]
}

fn json(text: &str) -> Value {
    sonic_rs::from_str(text).expect("test JSON")
}

#[test]
fn item_and_turn_notifications_match_pinned_schema() {
    let thread = SessionId::new_v7().to_string();
    let turn = TurnId::new(nonzero(3));
    let mut stream = TurnStream::new(thread.clone(), turn);
    let events = notifications(
        &mut stream,
        &[
            entries(vec![entry(
                1,
                EntryKind::User {
                    parts: vec![JournalPart::Text { text: "hi".into() }],
                },
            )]),
            UpdateKind::Delta {
                turn,
                channel: StreamChannel::Text,
                text: "Hel".into(),
            },
            UpdateKind::Delta {
                turn,
                channel: StreamChannel::Thinking,
                text: "hmm".into(),
            },
            UpdateKind::Delta {
                turn,
                channel: StreamChannel::Text,
                text: "lo".into(),
            },
            entries(vec![entry(
                2,
                assistant(
                    vec![
                        Block::Reasoning {
                            text: "hmm".into(),
                            replay: RawJson::null(),
                        },
                        Block::Text {
                            text: "Hello".into(),
                        },
                    ],
                    AssistantStop::ToolUse,
                ),
            )]),
            UpdateKind::ToolStarted {
                call: CallId::new("call-1"),
                tool: "exec".into(),
                args: RawJson::parse(r#"{"argv":["ls"]}"#).expect("raw args"),
            },
            UpdateKind::ToolSettled {
                call: CallId::new("call-1"),
                outcome: ToolOutcomeView {
                    is_error: true,
                    text: "exit 2".into(),
                    images: Vec::new(),
                },
            },
            UpdateKind::TurnEnded {
                turn,
                stop: Stop::Failed,
            },
            UpdateKind::TurnEnded {
                turn,
                stop: Stop::EndTurn,
            },
        ],
    );
    for (method, params) in &events {
        assert_valid("serverNotifications", method, "params", params);
    }
    let methods: Vec<&str> = events.iter().map(|(method, _)| *method).collect();
    assert_eq!(
        methods,
        [
            "item/started",
            "item/completed",
            "item/started",
            "item/agentMessage/delta",
            "item/agentMessage/delta",
            "item/started",
            "item/completed",
            "item/completed",
            "item/started",
            "item/completed",
            "turn/completed",
        ]
    );
    let streamed = &events[2].1;
    let completed = &events[7].1;
    let item_id = streamed["item"]["id"].as_str().expect("item id");
    assert_eq!(events[3].1["itemId"].as_str(), Some(item_id));
    assert_eq!(completed["item"]["id"].as_str(), Some(item_id));
    assert_eq!(completed["item"]["text"].as_str(), Some("Hello"));
    assert_eq!(events[9].1["item"]["status"].as_str(), Some("failed"));
    assert_eq!(events[10].1["turn"]["status"].as_str(), Some("failed"));
    assert!(stream.is_done());

    let started = sonic_rs::json!({
        "threadId": thread.as_str(),
        "turn": turn_value("3", "inProgress", None, &[]),
    });
    assert_valid("serverNotifications", "turn/started", "params", &started);
}

#[test]
fn cancelled_turn_completes_interrupted_with_open_message() {
    let turn = TurnId::new(nonzero(1));
    let mut stream = TurnStream::new(SessionId::new_v7().to_string(), turn);
    let events = notifications(
        &mut stream,
        &[
            UpdateKind::Delta {
                turn,
                channel: StreamChannel::Text,
                text: "partial".into(),
            },
            UpdateKind::TurnEnded {
                turn,
                stop: Stop::Cancelled,
            },
        ],
    );
    let (method, completed) = &events[2];
    assert_eq!(*method, "item/completed");
    assert_eq!(completed["item"]["text"].as_str(), Some("partial"));
    let (method, ended) = &events[3];
    assert_eq!(*method, "turn/completed");
    assert_eq!(ended["turn"]["status"].as_str(), Some("interrupted"));
    assert!(ended["turn"].get("error").is_none());
}

#[test]
fn thread_started_matches_pinned_schema() {
    let info = SessionInfo {
        id: SessionId::new_v7(),
        name: Some("demo".into()),
        preview: "hi".into(),
        workspace: Workspace::new(std::env::temp_dir().join("w")).expect("absolute"),
        updated_at: Timestamp::now(),
        created_at: None,
        archived: None,
        last_seq: None,
    };
    let idle = sonic_rs::json!({"type": "idle"});
    let thread = thread_value(&info, true, &idle, &[]);
    assert_valid(
        "serverNotifications",
        "thread/started",
        "params",
        &sonic_rs::json!({"thread": thread}),
    );
}

#[test]
fn server_requests_match_pinned_schema() {
    let thread = SessionId::new_v7().to_string();
    let turn = TurnId::new(nonzero(1));
    let mut methods = BTreeSet::new();
    for question in questions() {
        let request = request(question);
        let (method, params) = server_request(&thread, turn, Path::new("/w"), &request)
            .expect("every question kind maps");
        assert_valid("serverRequests", method, "params", &params);
        methods.insert(method);
    }
    let pinned: BTreeSet<&str> = schema()
        .get("serverRequests")
        .and_then(|value| value.as_object())
        .expect("server requests")
        .iter()
        .map(|(method, _)| method)
        .collect();
    assert_eq!(methods, pinned);
}

#[test]
fn approval_grant_text_carries_call_scope() {
    let request = request(questions().remove(0));
    let (_, params) = server_request("t", TurnId::new(nonzero(1)), Path::new("/w"), &request)
        .expect("approval maps");
    assert_eq!(
        params["reason"].as_str(),
        Some("exec git push (also allows git in /w until the job ends)")
    );
    assert_eq!(params["command"].as_str(), Some("git push origin main"));
}
#[test]
fn mcp_grant_text_includes_declared_server_details() {
    let request = request(Question::Grant {
        ext: "web".into(),
        origin: "user".into(),
        capabilities: vec!["mcp".into()],
        detail: Some("search: https://mcp.example/search".into()),
    });
    let (_, params) = server_request("t", TurnId::new(nonzero(1)), Path::new("/w"), &request)
        .expect("grant maps");
    assert_eq!(
        params["reason"].as_str(),
        Some("grant web: mcp\nsearch: https://mcp.example/search")
    );
}

#[test]
fn unknown_approval_decisions_decline() {
    for decision in [
        r#""approveEverything""#,
        r#""ACCEPT""#,
        r#"{"acceptWithEverything":{}}"#,
        r#"{"applyNetworkPolicyAmendment":{"network_policy_amendment":{"host":"x","action":"maybe"}}}"#,
        "7",
        "null",
    ] {
        assert_eq!(
            decision_answer(Some(&json(decision))),
            Answer::Decline,
            "{decision}"
        );
    }
    assert_eq!(decision_answer(None), Answer::Decline);
    for (decision, answer) in [
        (r#""accept""#, Answer::Approve),
        (r#""acceptForSession""#, Answer::ApproveForSession),
        (r#""decline""#, Answer::Decline),
        (r#""cancel""#, Answer::Cancel),
    ] {
        assert_eq!(decision_answer(Some(&json(decision))), answer, "{decision}");
    }
}

#[test]
fn client_results_map_to_answers_fail_closed() {
    let all = questions();
    let exec = request(all[0].clone());
    let patch = request(all[1].clone());
    let grant = request(all[2].clone());
    let select = request(all[3].clone());
    let confirm = request(all[4].clone());
    let text = request(all[5].clone());

    let accepted = json(r#"{"decision":"accept"}"#);
    assert_valid(
        "serverRequests",
        "item/fileChange/requestApproval",
        "result",
        &accepted,
    );
    assert_eq!(client_answer(&patch, Some(&accepted)), Answer::Approve);
    assert_eq!(client_answer(&exec, None), Answer::Decline);
    assert_eq!(
        client_answer(&exec, Some(&json(r#"{"decision":"yolo"}"#))),
        Answer::Decline
    );

    let granted = json(r#"{"permissions":{"network":{"enabled":true}},"scope":"session"}"#);
    let empty = json(r#"{"permissions":{}}"#);
    for result in [&granted, &empty] {
        assert_valid(
            "serverRequests",
            "item/permissions/requestApproval",
            "result",
            result,
        );
    }
    assert_eq!(client_answer(&grant, Some(&granted)), Answer::Decline);
    assert_eq!(client_answer(&grant, Some(&empty)), Answer::Decline);
    let net_write = request(Question::Grant {
        ext: "web".into(),
        origin: "user".into(),
        capabilities: vec!["net".into(), "fs.write".into()],
        detail: None,
    });
    let full =
        json(r#"{"permissions":{"network":{"enabled":true},"fileSystem":{"write":["/w"]}}}"#);
    assert_valid(
        "serverRequests",
        "item/permissions/requestApproval",
        "result",
        &full,
    );
    assert_eq!(client_answer(&net_write, Some(&granted)), Answer::Decline);
    assert_eq!(
        client_answer(&net_write, Some(&full)),
        Answer::ApproveForSession
    );

    let answers = |id: &RequestId, values: &str| {
        json(&format!(
            r#"{{"answers":{{"{id}":{{"answers":{values}}}}}}}"#
        ))
    };
    let picked = answers(&select.id, r#"["dev"]"#);
    assert_valid(
        "serverRequests",
        "item/tool/requestUserInput",
        "result",
        &picked,
    );
    assert_eq!(
        client_answer(&select, Some(&picked)),
        Answer::Value(RawJson::parse(r#""dev""#).expect("raw"))
    );
    assert_eq!(
        client_answer(&select, Some(&answers(&select.id, r#"["prod"]"#))),
        Answer::Cancel
    );
    assert_eq!(
        client_answer(&confirm, Some(&answers(&confirm.id, r#"["yes"]"#))),
        Answer::Approve
    );
    assert_eq!(
        client_answer(&confirm, Some(&answers(&confirm.id, r#"["maybe"]"#))),
        Answer::Decline
    );
    assert_eq!(
        client_answer(&text, Some(&answers(&text.id, r#"["fix bug"]"#))),
        Answer::Value(RawJson::parse(r#""fix bug""#).expect("raw"))
    );
    assert_eq!(client_answer(&text, Some(&json("{}"))), Answer::Cancel);
}

async fn start_host(root: &Path) -> Host {
    let product = Product {
        name: "dal",
        data_root: root.join("data"),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: root.to_path_buf(),
        sandbox_helper: None,
    };
    let config =
        Config::load(ConfigProduct::Dalgon, &root.join("data"), "", None).expect("default config");
    Host::start(product, config, env)
        .await
        .expect("host starts")
}

async fn roundtrip(peer: &mut MemoryPeer, frame: Value) -> Value {
    peer.send_frame(sonic_rs::to_string(&frame).expect("encode"))
        .await
        .expect("send");
    next(peer).await
}

async fn next(peer: &mut MemoryPeer) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(10), peer.read_frame())
        .await
        .expect("frame in time")
        .expect("open transport");
    let value: Value = sonic_rs::from_str(&frame).expect("frame is JSON");
    assert!(
        value.get("jsonrpc").is_none(),
        "codex frame has jsonrpc: {frame}"
    );
    value
}

#[tokio::test]
async fn codex_frames_follow_pinned_schema() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().join("w");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let host = start_host(dir.path()).await;
    let (transport, mut peer) = MemoryTransport::pair(16);
    let server = serve_codex(host.clone(), transport);
    let client = async move {
        let unknown = roundtrip(
            &mut peer,
            sonic_rs::json!({"id": 1, "method": "thread/fork", "params": {}}),
        )
        .await;
        assert_eq!(unknown["id"].as_i64(), Some(1));
        assert_eq!(unknown["error"]["code"].as_i64(), Some(-32601));

        let early = roundtrip(
            &mut peer,
            sonic_rs::json!({"id": 2, "method": "thread/list", "params": {}}),
        )
        .await;
        assert_eq!(early["error"]["code"].as_i64(), Some(-32600));

        let init = roundtrip(
            &mut peer,
            sonic_rs::json!({"id": 3, "method": "initialize", "params": {"clientInfo": {"name": "t", "version": "1"}}}),
        )
        .await;
        assert_valid("clientRequests", "initialize", "result", &init["result"]);
        peer.send_frame(r#"{"method":"initialized"}"#)
            .await
            .expect("send");

        let started = roundtrip(
            &mut peer,
            sonic_rs::json!({"id": 4, "method": "thread/start", "params": {"cwd": workspace.display().to_string(), "ephemeral": true}}),
        )
        .await;
        assert_eq!(started["id"].as_i64(), Some(4));
        assert_valid(
            "clientRequests",
            "thread/start",
            "result",
            &started["result"],
        );
        let notice = next(&mut peer).await;
        assert_eq!(notice["method"].as_str(), Some("thread/started"));
        assert!(notice.get("id").is_none());
        assert_valid(
            "serverNotifications",
            "thread/started",
            "params",
            &notice["params"],
        );
        let thread = started["result"]["thread"]["id"]
            .as_str()
            .expect("thread id")
            .to_owned();

        let resumed = roundtrip(
            &mut peer,
            sonic_rs::json!({"id": 5, "method": "thread/resume", "params": {"threadId": thread.as_str()}}),
        )
        .await;
        assert_valid(
            "clientRequests",
            "thread/resume",
            "result",
            &resumed["result"],
        );

        let listed = roundtrip(
            &mut peer,
            sonic_rs::json!({"id": 6, "method": "thread/list", "params": {}}),
        )
        .await;
        assert_valid("clientRequests", "thread/list", "result", &listed["result"]);

        let bad_turn = roundtrip(
            &mut peer,
            sonic_rs::json!({"id": 7, "method": "turn/interrupt", "params": {"threadId": thread.as_str(), "turnId": "zero"}}),
        )
        .await;
        assert_eq!(bad_turn["error"]["code"].as_i64(), Some(-32602));

        let media = roundtrip(
            &mut peer,
            sonic_rs::json!({"id": 8, "method": "turn/start", "params": {"threadId": thread.as_str(), "input": [{"type": "audio", "url": "x"}]}}),
        )
        .await;
        assert_eq!(media["error"]["code"].as_i64(), Some(-32602));
    };
    let (outcome, ()) = tokio::join!(server, client);
    outcome.expect("serve ends cleanly");
    let _ = host.shutdown(Duration::from_secs(5)).await;
}
