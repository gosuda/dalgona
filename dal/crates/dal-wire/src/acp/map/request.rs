//! ACP server-initiated permission and elicitation requests and their answers.

use std::sync::Arc;

use dal_agent::Agent;
use dal_core::{Answer, CallGrant, Choice, Question, Request, SessionId};
use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};
use tokio::sync::{Mutex, oneshot};

use super::super::{AcpConn, AcpVersion, ServerAnswer, send_notice};
use crate::jsonrpc::Message;
use crate::transport::FrameWriter;

/// Outcome of driving one opened request to an answer.
pub(crate) enum PermissionEnd {
    /// The client answer was submitted to the core.
    Answered,
    /// The wait ended; the core default stands.
    Defaulted,
}

/// Drives one opened request: asks the client, maps the answer, and resolves.
///
/// Registers the client request id against the core id so a racing
/// resolution elsewhere cancels this wait. A client wait ends at the
/// request's own timeout: the wire sends `$/cancel_request` and the core
/// default stands. An unknown question sends nothing; the core default
/// resolves it.
pub(crate) async fn permission_flow(
    state: Arc<Mutex<AcpConn>>,
    writer: FrameWriter,
    agent: Agent,
    session: SessionId,
    version: AcpVersion,
    request: Request,
) -> PermissionEnd {
    let core_id = request.id;
    let timeout = request.timeout.max(std::time::Duration::from_secs(1));
    let Some((method, params)) = build_question(version, session, &request) else {
        return PermissionEnd::Defaulted;
    };
    let (client, receiver) = {
        let mut locked = state.lock().await;
        locked.next_req += 1;
        let client = format!("dal-req-{}", locked.next_req);
        let (sender, receiver) = oneshot::channel();
        locked.pending.insert(client.clone(), (session, sender));
        locked.outstanding.insert(core_id, client.clone());
        (client, receiver)
    };
    crate::rpc::send(
        &writer,
        &Message::Request {
            id: crate::jsonrpc::Id::String(client.clone()),
            method,
            params,
        },
    )
    .await;
    let answer = tokio::select! {
        biased;
        response = receiver => response.ok(),
        () = tokio::time::sleep(timeout) => None,
    };
    let still_open = {
        let mut locked = state.lock().await;
        locked.outstanding.remove(&core_id);
        locked.pending.remove(&client).is_some()
    };
    let Some(answer) = answer else {
        if still_open {
            cancel_one(&writer, &client).await;
        }
        return PermissionEnd::Defaulted;
    };
    let mapped = map_client_answer(&request.question, &answer);
    match agent.answer(core_id, mapped).await {
        Ok(()) | Err(dal_agent::AgentError::AlreadyResolved { .. }) => PermissionEnd::Answered,
        Err(_) => PermissionEnd::Defaulted,
    }
}

/// Sends one `$/cancel_request` for a client request id.
pub(crate) async fn cancel_one(writer: &FrameWriter, client: &str) {
    crate::rpc::send(
        writer,
        &Message::Notification {
            method: "$/cancel_request".to_owned(),
            params: sonic_rs::json!({"requestId": client}),
        },
    )
    .await;
}

/// What a permission request asks about.
enum Subject {
    /// A tool call.
    Tool,
    /// An extension capability grant.
    Command,
}

/// Builds the client request for one question: method plus params.
///
/// Returns `None` for an unknown question so the core default resolves it.
fn build_question(
    version: AcpVersion,
    session: SessionId,
    request: &Request,
) -> Option<(String, Value)> {
    let session_text = session.to_string();
    let (title, subject, options) = match &request.question {
        Question::Approval {
            tool,
            preview,
            grant,
        } => {
            let mut title = approval_title(tool, preview);
            if let Some(grant) = grant {
                title.push_str(&grant_clause(grant));
            }
            (title, Subject::Tool, permission_options())
        }
        Question::Grant {
            ext,
            capabilities,
            detail,
            ..
        } => (
            format!(
                "grant {ext}: {}{}",
                capabilities.join(", "),
                detail
                    .as_deref()
                    .map_or_else(String::new, |detail| format!("\n{detail}"))
            ),
            Subject::Command,
            permission_options(),
        ),
        Question::Select {
            options,
            multi: true,
            ..
        } => {
            return Some(elicitation_request(
                &session_text,
                request,
                &multi_schema(options),
            ));
        }
        Question::Select {
            prompt, options, ..
        } => (prompt.to_string(), Subject::Tool, select_options(options)),
        Question::Confirm { text } => (text.to_string(), Subject::Tool, confirm_options()),
        Question::Text { .. } => {
            return Some(elicitation_request(&session_text, request, &text_schema()));
        }
        _ => return None,
    };
    let tool_call = sonic_rs::json!({"toolCallId": request.id.to_string(), "title": title});
    let params = match version {
        AcpVersion::V1 => sonic_rs::json!({
            "sessionId": session_text,
            "toolCall": tool_call,
            "options": options,
        }),
        AcpVersion::V2 => {
            let subject = match subject {
                Subject::Tool => sonic_rs::json!({"type": "tool_call", "toolCall": tool_call}),
                Subject::Command => {
                    sonic_rs::json!({"type": "command", "command": title, "cwd": ""})
                }
            };
            sonic_rs::json!({
                "sessionId": session_text,
                "title": title,
                "subject": subject,
                "options": options,
            })
        }
    };
    Some(("session/request_permission".to_owned(), params))
}

/// Returns single-select options: one `allow_once` option per choice.
fn select_options(options: &[Choice]) -> Value {
    let choices: Vec<Value> = options
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            sonic_rs::json!({
                "optionId": format!("choice-{index}"),
                "name": choice_label(choice),
                "kind": "allow_once",
            })
        })
        .collect();
    sonic_rs::json!(choices)
}

/// Returns the yes and no confirmation options.
fn confirm_options() -> Value {
    sonic_rs::json!([
        {"optionId": "yes", "name": "Yes", "kind": "allow_once"},
        {"optionId": "no", "name": "No", "kind": "reject_once"},
    ])
}

/// Builds an `elicitation/create` request for text and multi-select.
fn elicitation_request(session: &str, request: &Request, schema: &Value) -> (String, Value) {
    let prompt = match &request.question {
        Question::Text { prompt, .. } | Question::Select { prompt, .. } => prompt.to_string(),
        _ => String::new(),
    };
    (
        "elicitation/create".to_owned(),
        sonic_rs::json!({
            "sessionId": session,
            "mode": "form",
            "message": prompt,
            "requestedSchema": schema,
        }),
    )
}

/// Returns the text-question form schema.
fn text_schema() -> Value {
    sonic_rs::json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
    })
}

/// Returns the multi-select form schema: one boolean per choice.
fn multi_schema(options: &[Choice]) -> Value {
    let mut properties = sonic_rs::json!({});
    for (index, choice) in options.iter().enumerate() {
        if let Some(object) = properties.as_object_mut() {
            object.insert(
                &format!("choice-{index}"),
                sonic_rs::json!({"type": "boolean", "title": choice_label(choice)}),
            );
        }
    }
    sonic_rs::json!({"type": "object", "properties": properties})
}

/// Returns the three approval options in order.
fn permission_options() -> Value {
    sonic_rs::json!([
        {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
        {"optionId": "allow_session", "name": "Allow for this session", "kind": "allow_always"},
        {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
    ])
}

/// Builds an approval title from the tool and its preview.
fn approval_title(tool: &str, preview: &dal_core::Preview) -> String {
    let first = preview.title.lines().next().unwrap_or("");
    if first.is_empty() {
        tool.to_owned()
    } else {
        format!("{tool} {first}")
    }
}

/// Builds the call-scoped grant clause appended to approval text.
fn grant_clause(grant: &CallGrant) -> String {
    let roots: Vec<String> = grant
        .roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    format!(
        " (also allows {} in {} until the job ends)",
        grant.argv_prefix,
        roots.join(", ")
    )
}

/// Returns one choice's display label.
fn choice_label(choice: &Choice) -> String {
    choice.label.to_string()
}

/// Maps one client answer to its core answer value.
fn map_client_answer(question: &Question, answer: &ServerAnswer) -> Answer {
    let result = &answer.result;
    let outcome = result
        .get("outcome")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match question {
        Question::Approval { .. } | Question::Grant { .. } => match outcome {
            "selected" => match result.get("optionId").and_then(|value| value.as_str()) {
                Some("allow") => Answer::Approve,
                Some("allow_session") => Answer::ApproveForSession,
                Some("deny") | None => Answer::Decline,
                Some(other) => {
                    tracing::warn!(option = %other, "unknown approval option; declining");
                    Answer::Decline
                }
            },
            "cancelled" => Answer::Cancel,
            _ => {
                tracing::warn!(outcome = %outcome, "unknown approval outcome; declining");
                Answer::Decline
            }
        },
        Question::Confirm { .. } => match outcome {
            "selected" => match result.get("optionId").and_then(|value| value.as_str()) {
                Some("yes") => Answer::Value(raw_bool(true)),
                Some("no") => Answer::Value(raw_bool(false)),
                _ => Answer::Decline,
            },
            "cancelled" => Answer::Cancel,
            _ => Answer::Decline,
        },
        Question::Select {
            options,
            multi: true,
            ..
        } => elicitation_answer(options, result),
        Question::Select { options, .. } => match outcome {
            "selected" => match result
                .get("optionId")
                .and_then(|value| value.as_str())
                .and_then(|id| id.strip_prefix("choice-"))
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| options.get(index))
            {
                Some(choice) => Answer::Value(raw_string(&choice_label(choice))),
                None => Answer::Decline,
            },
            "cancelled" => Answer::Cancel,
            _ => Answer::Decline,
        },
        Question::Text { .. } => elicitation_answer(&[], result),
        _ => Answer::Decline,
    }
}

/// Maps one elicitation response to its core answer.
fn elicitation_answer(options: &[Choice], result: &Value) -> Answer {
    let outcome = result
        .get("outcome")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match outcome {
        "accept" => {
            if options.is_empty() {
                let text = result
                    .get("answer")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                Answer::Value(raw_string(text))
            } else {
                let mut checked = Vec::new();
                for (index, choice) in options.iter().enumerate() {
                    let key = format!("choice-{index}");
                    if result
                        .get(&key)
                        .and_then(sonic_rs::JsonValueTrait::as_bool)
                        .unwrap_or(false)
                    {
                        checked.push(Value::from(choice_label(choice).as_str()));
                    }
                }
                Answer::Value(raw_value(&sonic_rs::json!(checked)))
            }
        }
        "cancel" => Answer::Cancel,
        _ => Answer::Decline,
    }
}

/// Builds a raw JSON string answer.
fn raw_string(text: &str) -> dal_core::RawJson {
    dal_core::RawJson::parse(&sonic_rs::to_string(&text).unwrap_or_else(|_| "\"\"".to_owned()))
        .unwrap_or_else(|_| dal_core::RawJson::null())
}

/// Builds a raw JSON boolean answer.
fn raw_bool(value: bool) -> dal_core::RawJson {
    dal_core::RawJson::parse(if value { "true" } else { "false" })
        .unwrap_or_else(|_| dal_core::RawJson::null())
}

/// Builds a raw JSON answer from a value.
fn raw_value(value: &Value) -> dal_core::RawJson {
    dal_core::RawJson::parse(&sonic_rs::to_string(value).unwrap_or_else(|_| "null".to_owned()))
        .unwrap_or_else(|_| dal_core::RawJson::null())
}

/// Emits the Cancel resolution path: resolves `Cancel` at once and warns.
pub(crate) async fn cancel_unaskable(
    writer: &FrameWriter,
    agent: &Agent,
    session: SessionId,
    request: &Request,
) {
    send_notice(
        writer,
        session,
        "warning",
        "dalgon cannot ask this question over ACP: the request was cancelled".to_owned(),
    )
    .await;
    let _ = agent.answer(request.id, Answer::Cancel).await;
}

/// Returns true when text/multi-select may use `elicitation/create`.
pub(crate) async fn may_elicit(state: &Arc<Mutex<AcpConn>>, version: AcpVersion) -> bool {
    version == AcpVersion::V1 && state.lock().await.elicitation_form
}

/// Handles `request_resolved` by another client: cancels the open wait.
pub(crate) async fn resolved_elsewhere(
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    id: dal_core::RequestId,
) {
    let open = {
        let mut locked = state.lock().await;
        locked
            .outstanding
            .remove(&id)
            .filter(|client| locked.pending.remove(client).is_some())
    };
    if let Some(client) = open {
        cancel_one(writer, &client).await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dal_core::{Answer, Owner, Question, Request, RequestId};
    use sonic_rs::JsonValueTrait;

    use super::{AcpVersion, build_question};

    #[test]
    fn grant_permission_request_includes_declared_server_details() {
        let request = Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Grant {
                ext: "web".into(),
                origin: "user".into(),
                capabilities: vec!["mcp".into()],
                detail: Some("search: URL https://mcp.example/search".into()),
            },
            timeout: Duration::from_secs(30),
            default: Answer::Decline,
        };
        for version in [AcpVersion::V1, AcpVersion::V2] {
            let (_, params) = build_question(version, dal_core::SessionId::new_v7(), &request)
                .expect("grant permission request");
            let title = if version == AcpVersion::V1 {
                params["toolCall"]["title"].as_str()
            } else {
                params["subject"]["command"].as_str()
            };
            assert_eq!(
                title,
                Some("grant web: mcp\nsearch: URL https://mcp.example/search")
            );
        }
    }
}
