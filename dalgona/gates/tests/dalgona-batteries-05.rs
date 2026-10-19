// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Ask fails closed without an answerer and records real RPC and TUI answers.
use dalgona_gates::support;

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use dal_agent::{Env, Host, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, EntryKind, Expect, PageReq, Part, Workspace,
};
use sonic_rs::{JsonValueTrait, Value};

const NO_ANSWER: &str = "No answer (dismissed, timed out, or no user attached). Continue on best judgment; do not ask again this turn.";

#[test]
fn ask_fails_closed_on_print_surface_without_answerer() -> support::TestResult<()> {
    let scratch = support::Scratch::new("ask-front-ends")?;
    // The product registers the ask surface: one extension, one model tool.
    let product = support::build_product(scratch.path().to_path_buf(), None)?;
    let ask = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "ask")
        .ok_or_else(|| io::Error::other("the product has no ask extension"))?;
    assert!(
        ask.tools()
            .iter()
            .any(|(tool, _)| tool.name().as_str() == "ask"),
        "the ask extension publishes its model tool"
    );
    drop(product);

    // Print has no attached user, so the scripted ask call fails closed with
    // the exact decline instead of hanging or inventing an answer.
    let steps = [
        support::tool_step(
            "ask-1",
            "ask",
            r#"{"questions":[{"id":"gate","header":"Gate","question":"Proceed?","kind":"text"}]}"#,
        ),
        support::text_step("done"),
    ];
    let journal = support::run_scripted_print(
        &scratch,
        "disabled_batteries = [\"history\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]",
        &steps,
        "ask me",
    )?;
    let results = support::journal_records(&journal, "tool_result");
    assert_eq!(results.len(), 1, "{journal}");
    assert!(
        results[0].contains(NO_ANSWER),
        "ask fails closed without an answerer: {}",
        results[0]
    );
    Ok(())
}

async fn rpc_send(
    peer: &dal_wire::transport::MemoryPeer,
    id: i64,
    method: &str,
    params: Value,
) -> support::TestResult<()> {
    let frame = sonic_rs::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    peer.send_frame(frame.to_string())
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(())
}

async fn rpc_next(
    peer: &mut dal_wire::transport::MemoryPeer,
    what: &str,
) -> support::TestResult<Value> {
    let frame = tokio::time::timeout(Duration::from_secs(30), peer.read_frame())
        .await
        .map_err(|_| io::Error::other(format!("timed out waiting for {what}")))?;
    let frame = frame
        .ok_or_else(|| io::Error::other(format!("transport closed while waiting for {what}")))?;
    let value: Value =
        sonic_rs::from_str(&frame).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(value)
}

async fn rpc_call(
    peer: &mut dal_wire::transport::MemoryPeer,
    id: i64,
    method: &str,
    params: Value,
) -> support::TestResult<Value> {
    rpc_send(peer, id, method, params).await?;
    loop {
        let frame = rpc_next(peer, method).await?;
        if frame.get("id").and_then(Value::as_i64) == Some(id) {
            if frame.get("error").is_some() {
                return Err(io::Error::other(format!("{method} failed: {frame}")).into());
            }
            return Ok(frame);
        }
    }
}

fn str_field<'a>(update: &'a Value, name: &str) -> Option<&'a str> {
    update.get(name).and_then(Value::as_str)
}

/// What a subscribed wire client has seen of one session: the requests that
/// opened and are not yet answered, the text of every `ask` notice, and
/// whether the turn ended. Concurrent updates may arrive in any order
/// relative to the replies, so every frame the client reads is recorded.
#[derive(Default)]
struct Observed {
    requests: VecDeque<String>,
    ask_notices: Vec<String>,
    ended: bool,
}

impl Observed {
    fn record(&mut self, frame: &Value) -> support::TestResult<()> {
        if frame.get("method").and_then(Value::as_str) != Some("session/update") {
            return Ok(());
        }
        let Some(update) = frame.get("params").and_then(|params| params.get("update")) else {
            return Ok(());
        };
        match str_field(update, "type") {
            Some("request_opened") => {
                let id = str_field(update, "id").ok_or_else(|| {
                    io::Error::other(format!("request opened without an id: {frame}"))
                })?;
                self.requests.push_back(id.to_owned());
            }
            Some("notice") if str_field(update, "kind") == Some("ask") => {
                let text = str_field(update, "text")
                    .ok_or_else(|| io::Error::other(format!("ask notice without text: {frame}")))?;
                self.ask_notices.push(text.to_owned());
            }
            Some("turn_ended") => self.ended = true,
            _ => {}
        }
        Ok(())
    }
}

async fn rpc_observe(
    peer: &mut dal_wire::transport::MemoryPeer,
    observed: &mut Observed,
    what: &str,
) -> support::TestResult<Value> {
    let frame = rpc_next(peer, what).await?;
    observed.record(&frame)?;
    Ok(frame)
}

/// Reads and records frames until the reply to call `id`, which must not be
/// an error.
async fn rpc_reply(
    peer: &mut dal_wire::transport::MemoryPeer,
    observed: &mut Observed,
    id: i64,
    method: &str,
) -> support::TestResult<()> {
    loop {
        let frame = rpc_observe(peer, observed, method).await?;
        if frame.get("id").and_then(Value::as_i64) != Some(id) {
            continue;
        }
        if frame.get("error").is_some() {
            return Err(io::Error::other(format!("{method} failed: {frame}")).into());
        }
        return Ok(());
    }
}

fn newest_tool_result(agent: &dal_agent::Agent, tool: &str) -> support::TestResult<String> {
    let view = agent.view(PageReq::default())?;
    view.entries
        .items
        .into_iter()
        .rev()
        .find_map(|entry| match entry.kind {
            EntryKind::ToolResult { name, parts, .. } if name.as_ref() == tool => {
                Some(format!("{parts:?}"))
            }
            _ => None,
        })
        .ok_or_else(|| io::Error::other(format!("no {tool} result in view")).into())
}

/// Opens a session, submits one prompt, and answers each question the turn
/// raises, in order, with the next of `answers` as a `value` answer. Returns
/// the session and the text of every `ask` notice the client saw by the turn end.
async fn answer_rpc_questions(
    mut peer: dal_wire::transport::MemoryPeer,
    ws: &str,
    answers: &[Value],
) -> support::TestResult<(String, Vec<String>)> {
    rpc_call(
        &mut peer,
        0,
        "initialize",
        sonic_rs::json!({
            "protocolVersion": 1,
            "clientInfo": {"name": "gate", "version": "1"},
            "capabilities": ["sessions", "question"],
        }),
    )
    .await?;
    let opened = rpc_call(
        &mut peer,
        1,
        "session/open",
        sonic_rs::json!({"ref": {"type": "new", "workspace": ws}}),
    )
    .await?;
    let session = opened
        .get("result")
        .and_then(|result| result.get("sessionId"))
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other(format!("open returned no session: {opened}")))?
        .to_owned();
    rpc_call(
        &mut peer,
        2,
        "session/subscribe",
        sonic_rs::json!({"sessionId": session.clone()}),
    )
    .await?;
    let command = Command::Prompt {
        expect: Expect::Idle,
        content: vec![Part::Text {
            text: "ask me".into(),
        }],
    };
    let command_value =
        sonic_rs::to_value(&command).map_err(|error| io::Error::other(error.to_string()))?;
    rpc_send(
        &peer,
        3,
        "session/submit",
        sonic_rs::json!({"sessionId": session.clone(), "command": command_value}),
    )
    .await?;
    let mut observed = Observed::default();
    for (index, answer) in answers.iter().enumerate() {
        let request = loop {
            if let Some(request) = observed.requests.pop_front() {
                break request;
            }
            rpc_observe(&mut peer, &mut observed, "ask request").await?;
        };
        let id = 4 + i64::try_from(index)?;
        rpc_send(
            &peer,
            id,
            "session/answer",
            sonic_rs::json!({
                "sessionId": session,
                "requestId": request,
                "answer": {"type": "value", "value": answer},
            }),
        )
        .await?;
        rpc_reply(&mut peer, &mut observed, id, "session/answer").await?;
    }
    while !observed.ended {
        rpc_observe(&mut peer, &mut observed, "turn end").await?;
    }
    Ok((session, observed.ask_notices))
}

const GATE_QUESTION: &str =
    r#"{"questions":[{"id":"gate","header":"Gate","question":"Proceed?","kind":"text"}]}"#;
const ENVIRONMENT_QUESTION: &str = r#"{"id":"env","header":"Environment","question":"Pick an environment","kind":"single","options":[{"label":"staging"},{"label":"production"}]}"#;
const FEATURES_QUESTION: &str = r#"{"id":"features","header":"Features","question":"Select features","kind":"multi","options":[{"label":"auth"},{"label":"cache"},{"label":"metrics"}]}"#;
const TICKET_QUESTION: &str =
    r#"{"id":"ticket","header":"Ticket","question":"Provide change ticket","kind":"text"}"#;

/// Runs one scripted `ask` call against a real host over the in-memory RPC
/// transport, answering each raised question in order with `answers`. Returns
/// the journaled `ask` result and the `ask` notices the wire client received.
async fn rpc_ask(
    label: &str,
    questions: &str,
    answers: &[Value],
) -> support::TestResult<(String, Vec<String>)> {
    const ASK_ONLY: &str = "disabled_batteries = [\"history\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]";
    let scratch = support::Scratch::new(label)?;
    let root = scratch.path().to_path_buf();
    std::fs::create_dir_all(root.join("workspace"))?;
    let workspace_path = root.join("workspace");
    let workspace = Workspace::new(workspace_path.clone())?;
    let steps = [
        support::tool_step("ask-1", "ask", questions),
        support::text_step("done"),
    ];
    let script = root.join("script.jsonl");
    std::fs::write(&script, steps.join("\n"))?;
    let factory = dalgona::product();
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n{ASK_ONLY}\n[providers.scripted]\nfixture = {:?}\n",
        script.display().to_string()
    );
    let config = Config::load(ConfigProduct::Dalgona, &root, factory.defaults, Some(&user))?;
    let cx = dalgon::BuildCx {
        data_root: root.clone(),
        config: &config,
    };
    let product = dalgona::build(&cx)?;
    let env = Env {
        vars: BTreeMap::from([("OPENAI_API_KEY".into(), "sk-test".into())]),
        cwd: root.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let (transport, peer) = dal_wire::transport::MemoryTransport::pair(64);
    let server = dal_wire::serve_rpc(host.clone(), transport);
    let ws = workspace_path.display().to_string();
    let client = answer_rpc_questions(peer, &ws, answers);
    let (server_outcome, client_outcome) = tokio::join!(server, client);
    server_outcome.map_err(|error| io::Error::other(error.to_string()))?;
    let (session, notices) = client_outcome?;
    let agent = host
        .open(
            SessionRef::Resume {
                workspace,
                key: session.into(),
            },
            ClientId::new("ask-rpc-gate"),
        )
        .await?;
    let report = newest_tool_result(&agent, "ask")?;
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;
    Ok((report, notices))
}

/// The same ask tool call answered by a real wire client holding the question
/// capability, with the strict object the terminal dialog sends. The journal
/// records the supplied answer.
#[tokio::test]
async fn ask_answers_on_rpc_surface_with_shared_record() -> support::TestResult<()> {
    let answers = [sonic_rs::json!({"text": "gate answer"})];
    let (report, notices) = rpc_ask("ask-rpc-answered", GATE_QUESTION, &answers).await?;
    assert!(
        report.contains("Gate:"),
        "the shared record names the question header: {report}"
    );
    assert!(
        report.contains("gate answer"),
        "the shared record carries the wire answer: {report}"
    );
    assert!(
        !report.contains(NO_ANSWER),
        "an answered question never fails closed: {report}"
    );
    assert!(
        notices.is_empty(),
        "an answered question raises no notice: {notices:?}"
    );
    Ok(())
}

/// A wire client answers a single, a multi, and a text question with the
/// canonical bare values: a string, an array of strings, and a string. Every
/// answer reaches the shared record and none is discarded as malformed.
#[tokio::test]
async fn ask_accepts_bare_wire_answers_on_rpc_surface() -> support::TestResult<()> {
    let questions = format!(
        r#"{{"questions":[{ENVIRONMENT_QUESTION},{FEATURES_QUESTION},{TICKET_QUESTION}]}}"#
    );
    let answers = [
        sonic_rs::json!("staging"),
        sonic_rs::json!(["auth", "metrics"]),
        sonic_rs::json!("TICKET-1234"),
    ];
    let (report, notices) = rpc_ask("ask-rpc-bare", &questions, &answers).await?;
    for line in [
        "Environment: staging",
        "Features: auth, metrics",
        "Ticket: TICKET-1234",
    ] {
        assert!(
            report.contains(line),
            "the shared record carries `{line}`: {report}"
        );
    }
    assert!(
        !report.contains(NO_ANSWER) && !report.contains("Unanswered"),
        "every bare answer is taken: {report}"
    );
    assert!(
        notices.is_empty(),
        "no bare answer is reported malformed: {notices:?}"
    );
    Ok(())
}

/// An object keeps its strict member set over the wire: an unknown member
/// never decodes. The question fails closed with the no-answer text and the
/// client receives exactly one notice that names the question.
#[tokio::test]
async fn ask_refuses_unknown_object_members_on_rpc_surface() -> support::TestResult<()> {
    let questions = format!(r#"{{"questions":[{ENVIRONMENT_QUESTION}]}}"#);
    let answers = [sonic_rs::json!({"selected": ["staging"], "admin_override": true})];
    let (report, notices) = rpc_ask("ask-rpc-unknown-member", &questions, &answers).await?;
    assert!(
        report.contains(NO_ANSWER),
        "an object with an unknown member fails closed: {report}"
    );
    assert!(
        !report.contains("Environment:"),
        "no answer line is composed: {report}"
    );
    let [notice] = notices.as_slice() else {
        return Err(format!("expected exactly one ask notice: {notices:?}").into());
    };
    assert!(
        notice.contains("malformed answer for question \"Environment\""),
        "the notice names the question: {notice}"
    );
    Ok(())
}

/// The same ask tool call answered by a person-equivalent on the real TUI: the
/// text-question dialog arms with its own text-input hint (never grant keys),
/// the typed answer submits, and the journaled result is the shared record.
#[cfg(unix)]
#[test]
fn ask_answers_on_tui_surface_with_shared_record() -> support::TestResult<()> {
    let steps = [
        support::tool_step(
            "ask-1",
            "ask",
            r#"{"questions":[{"id":"gate","header":"Gate","question":"Proceed?","kind":"text"}]}"#,
        ),
        support::text_step("done"),
    ];
    let scratch = support::Scratch::new("ask-tui-answered")?;
    let config = "model = \"openai/gpt-6-luna\"\napproval = \"all\"\ndisabled_batteries = [\"history\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\n";
    let run = support::Pane::start_scripted(scratch, config, &steps)?;
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut previous = String::new();
        loop {
            let text = run.text()?;
            if text == previous && text.trim().len() > 32 {
                break;
            }
            previous = text;
            if std::time::Instant::now() > deadline {
                return Err(
                    format!("booted composer: the screen never settled:\n{previous}").into(),
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    run.type_text("ask me")?;
    run.enter()?;
    let dialog_deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let text = run.text()?;
        if text.contains("Gate") && text.contains("enter submit") {
            break;
        }
        if std::time::Instant::now() > dialog_deadline {
            return Err(format!("ask dialog never opened:\n{text}").into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    run.type_text("gate answer")?;
    run.enter()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(journal) = support::session_journal(run.scratch()) {
            let results = support::journal_records(&journal, "tool_result");
            let answered = results
                .iter()
                .any(|record| record.contains("Gate:") && record.contains("gate answer"));
            if answered {
                assert!(
                    !results.iter().any(|record| record.contains(NO_ANSWER)),
                    "an answered question never fails closed: {results:?}"
                );
                return Ok(());
            }
        }
        if std::time::Instant::now() > deadline {
            let text = match run.text() {
                Ok(screen) => screen,
                Err(_) => "<pane unreadable>".to_owned(),
            };
            return Err(
                format!("the TUI answer never journaled the shared record:\n{text}").into(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
