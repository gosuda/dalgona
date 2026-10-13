use std::net::SocketAddr;

use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::super::support::{
    HttpReply, WAIT, ask_probe_extension, denied_result_text, gate_step, host_header, http,
    parse_reply, result_text, rig, rig_with_extensions, router_options, sse_data, text_step,
    tool_step, with_serve,
};

async fn post(addr: SocketAddr, path: &str, extra: &str, body: &str) -> HttpReply {
    http(
        addr,
        &format!(
            "POST {path} HTTP/1.1\r\n{}\r\ncontent-type: application/json{extra}",
            host_header(addr)
        ),
        body,
    )
    .await
}

fn events(reply: &HttpReply) -> Vec<Value> {
    sse_data(&reply.body)
        .iter()
        .filter(|data| data.as_str() != "[DONE]")
        .map(|data| sonic_rs::from_str(data).expect("event json"))
        .collect()
}

fn session_of(response_id: &str) -> (&str, u64) {
    let rest = response_id
        .strip_prefix("resp_")
        .expect("responses id prefix");
    let (session, turn) = rest.split_once('.').expect("session.turn");
    (session, turn.parse().expect("turn number"))
}

#[tokio::test]
async fn golden_sse_streams() {
    let steps = [
        text_step(&["Hel", "lo"], 10, 5),
        text_step(&["Hel", "lo"], 10, 5),
        text_step(&["Hel", "lo"], 10, 5),
    ];
    let rig = rig(&steps).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        let chat = post(
            addr,
            "/v1/chat/completions",
            "",
            r#"{"model":"dalgon/normal","stream":true,"stream_options":{"include_usage":true},"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await;
        check_chat(&chat);

        let responses = post(
            addr,
            "/v1/responses",
            "",
            r#"{"model":"dalgon/normal","stream":true,"input":"hi"}"#,
        )
        .await;
        check_responses(&responses);

        let messages = post(
            addr,
            "/v1/messages",
            "",
            r#"{"model":"dalgon/normal","stream":true,"max_tokens":64,"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await;
        check_messages(&messages);
    })
    .await;
}

#[tokio::test]
async fn session_continuation_ids() {
    let steps: Vec<String> = (0..5).map(|_| text_step(&["Hello"], 1, 1)).collect();
    let rig = rig(&steps).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        let first = post(addr, "/v1/responses", "", r#"{"model":"dalgon/normal","input":"hi"}"#).await;
        assert_eq!(first.status, 200, "{first:?}");
        let first_id = first.json()["id"].as_str().expect("response id").to_owned();
        let (session, turn) = session_of(&first_id);

        let body = format!(
            r#"{{"model":"dalgon/normal","input":"again","previous_response_id":"{first_id}"}}"#
        );
        let second = post(addr, "/v1/responses", "", &body).await;
        assert_eq!(second.status, 200, "{second:?}");
        let second_json = second.json();
        let (continued, next_turn) = session_of(second_json["id"].as_str().expect("id"));
        assert_eq!(continued, session);
        assert!(next_turn > turn);

        let header = format!("\r\nx-dal-session: {session}");
        let third = post(addr, "/v1/responses", &header, r#"{"model":"dalgon/normal","input":"more"}"#).await;
        assert_eq!(third.status, 200, "{third:?}");
        let third_json = third.json();
        assert_eq!(session_of(third_json["id"].as_str().expect("id")).0, session);

        let chat = post(
            addr,
            "/v1/chat/completions",
            "",
            r#"{"model":"dalgon/normal","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await;
        assert_eq!(chat.status, 200, "{chat:?}");
        let chat_turn = chat.json()["id"].as_str().expect("chat id").to_owned();
        let digest = post(
            addr,
            "/v1/chat/completions",
            "",
            r#"{"model":"dalgon/normal","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"Hello"},{"role":"user","content":"more"}]}"#,
        )
        .await;
        assert_eq!(digest.status, 200, "{digest:?}");
        let digest_turn = digest.json()["id"].as_str().expect("chat id").to_owned();
        assert_ne!(digest_turn, chat_turn, "digest did not continue the session");

        let stranger = dal_core::SessionId::new_v7().to_string();
        let unknown = post(
            addr,
            "/v1/responses",
            &format!("\r\nx-dal-session: {stranger}"),
            r#"{"model":"dalgon/normal","input":"x"}"#,
        )
        .await;
        assert_eq!(unknown.status, 404);
        assert_eq!(unknown.json()["error"]["code"].as_str(), Some("session_not_found"));
        assert_eq!(
            unknown.json()["error"]["message"].as_str(),
            Some(format!(r#"session "{stranger}" was not found"#).as_str())
        );
        let previous = post(
            addr,
            "/v1/responses",
            "",
            r#"{"model":"dalgon/normal","input":"x","previous_response_id":"resp_nope"}"#,
        )
        .await;
        assert_eq!(previous.status, 404);
        assert_eq!(
            previous.json()["error"]["code"].as_str(),
            Some("previous_response_not_found")
        );
        assert_eq!(
            previous.json()["error"]["message"].as_str(),
            Some(r#"previous response "resp_nope" was not found"#)
        );
    })
    .await;
}

#[tokio::test]
async fn concurrent_request_conflict() {
    let steps = [
        text_step(&["Hello"], 1, 1),
        gate_step("c1"),
        text_step(&["done"], 1, 1),
    ];
    let rig = rig(&steps).await;
    let gate = rig.gate.clone();
    with_serve(&rig, router_options(&rig), async |addr| {
        let first = post(addr, "/v1/responses", "", r#"{"model":"dalgon/normal","input":"hi"}"#).await;
        let first_id = first.json()["id"].as_str().expect("response id").to_owned();
        let (session, turn) = session_of(&first_id);
        let header = format!("\r\nx-dal-session: {session}");

        let mut running = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let body = r#"{"model":"dalgon/normal","stream":true,"input":"wait"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\n{}\r\ncontent-type: application/json{header}\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
            host_header(addr),
            body.len()
        );
        running.write_all(request.as_bytes()).await.expect("stream request");
        let mut bytes = Vec::new();
        while !String::from_utf8_lossy(&bytes).contains("response.created") {
            let mut buffer = [0u8; 4096];
            let read = tokio::time::timeout(WAIT, running.read(&mut buffer))
                .await
                .expect("stream starts in time")
                .expect("stream read");
            assert!(read > 0, "stream closed early: {}", String::from_utf8_lossy(&bytes));
            bytes.extend_from_slice(&buffer[..read]);
        }

        let busy = post(addr, "/v1/responses", &header, r#"{"model":"dalgon/normal","input":"no"}"#).await;
        assert_eq!(busy.status, 409, "{busy:?}");
        assert_eq!(busy.json()["error"]["code"].as_str(), Some("conflict"));
        assert_eq!(
            busy.json()["error"]["message"].as_str(),
            Some(format!("session {session} is running turn {}: wait for it to end", turn + 1).as_str())
        );

        gate.add_permits(1);
        tokio::time::timeout(WAIT, running.read_to_end(&mut bytes))
            .await
            .expect("stream ends in time")
            .expect("stream read");
        let done = parse_reply(&bytes);
        assert_eq!(done.status, 200);
        let last = events(&done).pop().expect("terminal event");
        assert_eq!(last["type"].as_str(), Some("response.completed"), "{last}");
    })
    .await;
}

/// The router never declares the answer capability, so an approval raised in
/// its session is denied at once as a headless denial instead of opening a
/// request that waits for a person.
#[tokio::test]
async fn a_router_session_denies_an_approval_at_once() {
    let steps = [tool_step("c1", "ask"), text_step(&["done"], 1, 1)];
    let rig = rig(&steps).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        let reply = post(
            addr,
            "/v1/responses",
            "",
            r#"{"model":"dalgon/normal","input":"edit"}"#,
        )
        .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        let id = reply.json()["id"].as_str().expect("response id").to_owned();
        let (session, _) = session_of(&id);
        let denial = denied_result_text(&rig, session, "ask").await;
        assert_eq!(
            dal_core::parse_headless_denial(&denial).map(|(tool, _)| tool),
            Some("ask"),
            "{denial}"
        );
    })
    .await;
}

/// An extension question raised in a router session takes its default at
/// once: no client of the router can answer, so nothing waits for the
/// question's time limit.
#[tokio::test]
async fn a_router_session_defaults_an_extension_question_at_once() {
    let steps = [tool_step("c1", "probe_ask"), text_step(&["done"], 1, 1)];
    let rig = rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await;
    with_serve(&rig, router_options(&rig), async |addr| {
        let reply = post(
            addr,
            "/v1/responses",
            "",
            r#"{"model":"dalgon/normal","input":"ask"}"#,
        )
        .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        let id = reply.json()["id"].as_str().expect("response id").to_owned();
        let (session, _) = session_of(&id);
        assert_eq!(
            result_text(&rig, session, "probe_ask", false).await,
            "default"
        );
    })
    .await;
}

fn check_chat(chat: &HttpReply) {
    assert_eq!(chat.status, 200, "{chat:?}");
    assert_eq!(
        sse_data(&chat.body).last().map(String::as_str),
        Some("[DONE]")
    );
    let chunks = events(chat);
    let first = chunks.first().expect("role chunk");
    assert_eq!(
        first["choices"][0]["delta"]["role"].as_str(),
        Some("assistant"),
        "{first}"
    );
    assert!(
        chunks.iter().all(|chunk| chunk["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("chatcmpl-dal-"))),
        "{chunks:?}"
    );
    let text: String = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "Hello");
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk["choices"][0]["finish_reason"].as_str() == Some("stop")),
        "{chunks:?}"
    );
    let usage = chunks.last().expect("usage chunk");
    assert!(
        usage["choices"]
            .as_array()
            .is_some_and(sonic_rs::Array::is_empty),
        "{usage}"
    );
    assert_eq!(usage["usage"]["prompt_tokens"].as_u64(), Some(10));
    assert_eq!(usage["usage"]["completion_tokens"].as_u64(), Some(5));
}

fn check_responses(responses: &HttpReply) {
    assert_eq!(responses.status, 200, "{responses:?}");
    assert!(!responses.body.contains("[DONE]"));
    let stream = events(responses);
    let kinds: Vec<&str> = stream
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect();
    assert_eq!(
        kinds.first().copied(),
        Some("response.created"),
        "{kinds:?}"
    );
    assert_eq!(
        kinds.get(1).copied(),
        Some("response.in_progress"),
        "{kinds:?}"
    );
    assert_eq!(
        kinds.last().copied(),
        Some("response.completed"),
        "{kinds:?}"
    );
    let numbers: Vec<u64> = stream
        .iter()
        .map(|event| event["sequence_number"].as_u64().expect("sequence number"))
        .collect();
    assert_eq!(
        numbers,
        (1..=u64::try_from(stream.len()).expect("event count")).collect::<Vec<_>>()
    );
    let text: String = stream
        .iter()
        .filter(|event| event["type"].as_str() == Some("response.output_text.delta"))
        .filter_map(|event| event["delta"].as_str())
        .collect();
    assert_eq!(text, "Hello");
    let completed = stream.last().expect("completed");
    session_of(completed["response"]["id"].as_str().expect("response id"));
    assert_eq!(
        completed["response"]["usage"]["input_tokens"].as_u64(),
        Some(10)
    );
    assert_eq!(
        completed["response"]["usage"]["output_tokens"].as_u64(),
        Some(5)
    );
}

fn check_messages(messages: &HttpReply) {
    assert_eq!(messages.status, 200, "{messages:?}");
    let stream = events(messages);
    let kinds: Vec<&str> = stream
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect();
    assert_eq!(kinds.first().copied(), Some("message_start"), "{kinds:?}");
    assert_eq!(kinds.last().copied(), Some("message_stop"), "{kinds:?}");
    assert!(
        stream[0]["message"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("msg_dal_")),
        "{}",
        stream[0]
    );
    let order: Vec<&str> = kinds
        .iter()
        .copied()
        .filter(|kind| *kind != "content_block_delta")
        .collect();
    assert_eq!(
        order,
        [
            "message_start",
            "content_block_start",
            "content_block_stop",
            "message_delta",
            "message_stop"
        ]
    );
    let text: String = stream
        .iter()
        .filter_map(|event| event["delta"]["text"].as_str())
        .collect();
    assert_eq!(text, "Hello");
    let delta = stream
        .iter()
        .find(|event| event["type"].as_str() == Some("message_delta"))
        .expect("message_delta");
    assert_eq!(
        delta["delta"]["stop_reason"].as_str(),
        Some("end_turn"),
        "{delta}"
    );
    assert_eq!(delta["usage"]["output_tokens"].as_u64(), Some(5), "{delta}");
}
