//! Codex client answers decode through the rich-ask contract.
//!
//! A real Host raises one text question through the `ask` service, the Codex
//! adapter turns it into `item/tool/requestUserInput`, and the client replies
//! with a well-formed or malformed result. The journaled tool result says
//! whether the question counted as answered.

use std::time::Duration;

use dal_core::{Answer, Choice, Question, RawJson, Request};
use sonic_rs::{JsonValueTrait, Value};

use super::super::support::{
    ask_probe_extension, result_text, rig_with_extensions, text_step, tool_step,
};
use super::{json, request};
use crate::codex::ask::{AnswerError, ask_answer, client_answer};
use crate::serve_codex;
use crate::transport::{MemoryPeer, MemoryTransport};

async fn read(peer: &mut MemoryPeer) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(10), peer.read_frame())
        .await
        .expect("frame in time")
        .expect("open transport");
    sonic_rs::from_str(&frame).expect("frame is JSON")
}

async fn send(peer: &mut MemoryPeer, frame: &Value) {
    peer.send_frame(sonic_rs::to_string(frame).expect("encode"))
        .await
        .expect("send");
}

/// Reads frames until the reply to client request `id`.
async fn reply_to(peer: &mut MemoryPeer, id: i64) -> Value {
    loop {
        let frame = read(peer).await;
        if frame.get("method").is_none() && frame["id"].as_i64() == Some(id) {
            return frame;
        }
    }
}

/// Runs one real Codex turn whose tool raises a text question, answers the
/// pending user-input request with `reply(question_id)`, and returns the text
/// of the tool result the model saw: `answered` or `default`.
async fn text_question_outcome(reply: impl FnOnce(&str) -> Value) -> String {
    let steps = [tool_step("c1", "probe_ask"), text_step(&["done"], 1, 1)];
    let rig = rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await;
    let workspace = rig.ws();
    let (transport, mut peer) = MemoryTransport::pair(16);
    let server = serve_codex(rig.host.clone(), transport);
    let rig = &rig;
    let client = async move {
        send(
            &mut peer,
            &sonic_rs::json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "t", "version": "1"}}}),
        )
        .await;
        reply_to(&mut peer, 1).await;
        peer.send_frame(r#"{"method":"initialized"}"#.to_owned())
            .await
            .expect("send");
        send(
            &mut peer,
            &sonic_rs::json!({"id": 2, "method": "thread/start", "params": {"cwd": workspace.as_str()}}),
        )
        .await;
        let started = reply_to(&mut peer, 2).await;
        let thread = started["result"]["thread"]["id"]
            .as_str()
            .expect("thread id")
            .to_owned();
        send(
            &mut peer,
            &sonic_rs::json!({"id": 3, "method": "turn/start", "params": {
                "threadId": thread.as_str(),
                "input": [{"type": "text", "text": "ask"}],
            }}),
        )
        .await;
        let (server_id, question) = loop {
            let frame = read(&mut peer).await;
            if frame["method"].as_str() == Some("item/tool/requestUserInput") {
                let question = frame["params"]["questions"][0]["id"]
                    .as_str()
                    .expect("question id")
                    .to_owned();
                break (frame["id"].clone(), question);
            }
        };
        send(
            &mut peer,
            &sonic_rs::json!({"id": server_id, "result": reply(&question)}),
        )
        .await;
        loop {
            let frame = read(&mut peer).await;
            if frame["method"].as_str() == Some("turn/completed") {
                break;
            }
        }
        result_text(rig, &thread, "probe_ask", false).await
    };
    let (outcome, text) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(server, client)
    })
    .await
    .expect("the codex turn settles in time");
    outcome.expect("codex serve ends cleanly");
    text
}

fn answers(question: &str, values: &str) -> Value {
    sonic_rs::from_str(&format!(
        r#"{{"answers":{{"{question}":{{"answers":{values}}}}}}}"#
    ))
    .expect("test JSON")
}

fn select(multi: bool) -> Request {
    let choice = |label: &str| Choice {
        label: label.into(),
        description: None,
    };
    request(Question::Select {
        prompt: "Pick".into(),
        options: vec![choice("main"), choice("dev")],
        multi,
        preview: None,
    })
}

fn confirm() -> Request {
    request(Question::Confirm { text: "Go?".into() })
}

fn text() -> Request {
    request(Question::Text {
        prompt: "Why?".into(),
        placeholder: None,
    })
}

/// A user-input result carrying `values` as the answer list for `request`.
fn result(request: &Request, values: &str) -> Value {
    answers(&request.id.to_string(), values)
}

fn raw(text: &str) -> Answer {
    Answer::Value(RawJson::parse(text).expect("raw JSON"))
}

#[test]
fn malformed_results_fail_with_typed_errors() {
    let single = select(false);
    let multi = select(true);
    let confirm = confirm();
    let text = text();
    let unknown = |label: &str| AnswerError::UnknownLabel {
        label: label.to_owned(),
    };
    let not_a_list = |request: &Request| AnswerError::NotAList {
        question: request.id.to_string(),
    };
    let cases = [
        (
            &single,
            result(&single, r#"["main", 7]"#),
            AnswerError::NotString { index: 1 },
        ),
        (
            &single,
            result(&single, r#"["main", "dev"]"#),
            AnswerError::Arity { count: 2 },
        ),
        (&single, result(&single, r#"["prod"]"#), unknown("prod")),
        (&single, result(&single, r#""main""#), not_a_list(&single)),
        (
            &single,
            json(&format!(r#"{{"answers":{{"{}":{{}}}}}}"#, single.id)),
            not_a_list(&single),
        ),
        (&single, json("{}"), AnswerError::MissingAnswers),
        (
            &single,
            json(r#"{"answers":["main"]}"#),
            AnswerError::MissingAnswers,
        ),
        (
            &multi,
            result(&multi, "[null]"),
            AnswerError::NotString { index: 0 },
        ),
        (
            &multi,
            result(&multi, r#"["main", "main"]"#),
            AnswerError::DuplicateLabel {
                label: "main".to_owned(),
            },
        ),
        (
            &multi,
            result(&multi, r#"["main", "prod"]"#),
            unknown("prod"),
        ),
        (
            &text,
            result(&text, "[7]"),
            AnswerError::NotString { index: 0 },
        ),
        (
            &text,
            result(&text, r#"["a", "b"]"#),
            AnswerError::Arity { count: 2 },
        ),
        (&confirm, result(&confirm, r#"["maybe"]"#), unknown("maybe")),
        (
            &confirm,
            result(&confirm, r#"["yes", "no"]"#),
            AnswerError::Arity { count: 2 },
        ),
    ];
    for (request, value, error) in cases {
        assert_eq!(ask_answer(request, &value), Err(error), "{value}");
    }
    assert_eq!(
        AnswerError::NotString { index: 1 }.to_string(),
        "answer 1 is not a string"
    );
}

#[test]
fn malformed_results_fail_closed_like_a_dismissal() {
    let single = select(false);
    let confirm = confirm();
    let text = text();
    for request in [&single, &text] {
        let value = result(request, r#"["main", 7]"#);
        assert_eq!(client_answer(request, Some(&value)), Answer::Cancel);
    }
    let value = result(&confirm, r#"["yes", 7]"#);
    assert_eq!(client_answer(&confirm, Some(&value)), Answer::Decline);
}

#[test]
fn empty_and_absent_answer_lists_are_dismissals_not_errors() {
    for request in [select(false), select(true), text()] {
        for value in [json(r#"{"answers":{}}"#), result(&request, "[]")] {
            assert_eq!(ask_answer(&request, &value), Ok(Answer::Cancel));
        }
    }
    let confirm = confirm();
    for value in [json(r#"{"answers":{}}"#), result(&confirm, "[]")] {
        assert_eq!(ask_answer(&confirm, &value), Ok(Answer::Decline));
    }
}

#[test]
fn well_formed_results_decode_to_the_contract_shapes() {
    let single = select(false);
    let multi = select(true);
    let confirm = confirm();
    let text = text();
    assert_eq!(
        ask_answer(&single, &result(&single, r#"["dev"]"#)),
        Ok(raw(r#""dev""#))
    );
    assert_eq!(
        ask_answer(&multi, &result(&multi, r#"["main", "dev"]"#)),
        Ok(raw(r#"["main","dev"]"#))
    );
    assert_eq!(
        ask_answer(&text, &result(&text, r#"["fix bug"]"#)),
        Ok(raw(r#""fix bug""#))
    );
    assert_eq!(
        ask_answer(&confirm, &result(&confirm, r#"["yes"]"#)),
        Ok(Answer::Approve)
    );
    assert_eq!(
        ask_answer(&confirm, &result(&confirm, r#"["no"]"#)),
        Ok(Answer::Decline)
    );
}

#[tokio::test]
async fn well_formed_text_answer_reaches_the_tool() {
    let text = Box::pin(text_question_outcome(|question| {
        answers(question, r#"["bob"]"#)
    }))
    .await;
    assert_eq!(text, "answered");
}

#[tokio::test]
async fn text_answer_with_a_non_string_entry_is_no_answer() {
    let text = Box::pin(text_question_outcome(|question| {
        answers(question, r#"["bob", 7]"#)
    }))
    .await;
    assert_eq!(text, "default");
}

#[tokio::test]
async fn text_answer_with_more_than_one_entry_is_no_answer() {
    let text = Box::pin(text_question_outcome(|question| {
        answers(question, r#"["bob", "alice"]"#)
    }))
    .await;
    assert_eq!(text, "default");
}
