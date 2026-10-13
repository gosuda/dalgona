//! Pure task views and stream responses folded from core updates.
//!
//! A stream starts with the task, then carries text-append artifact
//! updates, state updates, `dal.notice` and `dal.status` metadata, then a
//! final empty artifact chunk with `lastChunk:true`, then the final status.

use dal_core::{RequestId, SessionId, Stop, StreamChannel, TurnId, UpdateKind};
use sonic_rs::{JsonValueMutTrait, Value};

use super::parts::question_parts;
use super::table::{Framing, TaskRec};
use crate::a2a::{TaskEvent, TaskState};

/// The artifact id carrying a task's assistant text.
pub(crate) const ARTIFACT_ID: &str = "response";

/// One task-relevant step extracted from a core update.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    /// Assistant text arrived.
    Text(String),
    /// A question opened for this turn.
    Ask(Box<dal_core::Request>),
    /// The open question resolved by answer or default.
    Resolved,
    /// A notice for the task; `status` selects `dal.status` metadata.
    Notice {
        /// Whether this is an extension status notice.
        status: bool,
        /// The notice text.
        text: String,
    },
    /// The turn ended with this event.
    End(TaskEvent),
}

/// Maps one core stop to its task event.
pub(crate) const fn stop_event(stop: Stop) -> TaskEvent {
    match stop {
        Stop::EndTurn | Stop::Length | Stop::MaxSteps => TaskEvent::Complete,
        Stop::Filter => TaskEvent::Refuse,
        Stop::Cancelled => TaskEvent::Cancel,
        Stop::Failed => TaskEvent::Fail,
    }
}

/// Extracts the step one update means for one task, if any.
pub(crate) fn step(turn: TurnId, pending: Option<RequestId>, kind: &UpdateKind) -> Option<Step> {
    match kind {
        UpdateKind::Delta {
            turn: at,
            channel: StreamChannel::Text,
            text,
        } if *at == turn => Some(Step::Text(text.to_string())),
        UpdateKind::RequestOpened(request) if request.turn == Some(turn) => {
            Some(Step::Ask(Box::new(request.clone())))
        }
        UpdateKind::RequestResolved { id, .. } if pending == Some(*id) => Some(Step::Resolved),
        UpdateKind::RuleFired { turn: at, rule } if *at == turn => Some(Step::Notice {
            status: false,
            text: format!("rule {rule} fired"),
        }),
        UpdateKind::Notice(notice) if notice.turn.is_none_or(|at| at == turn) => {
            Some(Step::Notice {
                status: notice.kind.as_ref() == "status",
                text: notice.text.to_string(),
            })
        }
        UpdateKind::ExtStatus(status) => Some(Step::Notice {
            status: true,
            text: status.to_string(),
        }),
        UpdateKind::TurnEnded { turn: at, stop } if *at == turn => {
            Some(Step::End(stop_event(*stop)))
        }
        _ => None,
    }
}

/// Applies one step to a task and returns the stream responses it emits.
pub(crate) fn apply(task: &mut TaskRec, step: Step) -> Vec<Value> {
    match step {
        Step::Text(text) => {
            let append = task.chunks > 0;
            task.chunks += 1;
            task.text.push_str(&text);
            vec![artifact_update(task, &text, append, false)]
        }
        Step::Ask(request) => {
            task.request = Some(*request);
            if task.advance(TaskEvent::RequestInput) {
                vec![status_update(task, None)]
            } else {
                task.request = None;
                Vec::new()
            }
        }
        Step::Resolved => {
            if task.advance(TaskEvent::Answer) {
                vec![status_update(task, None)]
            } else {
                Vec::new()
            }
        }
        Step::Notice { status, text } => {
            let name = if status { "dal.status" } else { "dal.notice" };
            vec![status_update(task, Some((name, text)))]
        }
        Step::End(event) => {
            if !task.advance(event) && task.state == TaskState::InputRequired {
                task.advance(TaskEvent::Answer);
                task.advance(event);
            }
            vec![
                artifact_update(task, "", true, true),
                status_update(task, None),
            ]
        }
    }
}

/// Renders one task status, with the question message while input is required.
pub(crate) fn status_json(task: &TaskRec) -> Value {
    let mut status = sonic_rs::json!({"state": task.state.wire_name()});
    let question = task
        .request
        .as_ref()
        .filter(|_| task.state == TaskState::InputRequired)
        .and_then(|request| question_parts(request).ok());
    if let (Some(parts), Some(object)) = (question, status.as_object_mut()) {
        object.insert(
            "message",
            sonic_rs::json!({
                "messageId": format!("{}-input", task.key),
                "contextId": task.key.session.to_string(),
                "taskId": task.key.to_string(),
                "role": "ROLE_AGENT",
                "parts": parts,
            }),
        );
    }
    status
}

/// Renders one task object with history and collected artifacts.
pub(crate) fn task_json(task: &TaskRec) -> Value {
    let id = task.key.to_string();
    let context = task.key.session.to_string();
    let mut history = vec![sonic_rs::json!({
        "messageId": format!("{id}-user"),
        "contextId": context.as_str(),
        "taskId": id.as_str(),
        "role": "ROLE_USER",
        "parts": [{"text": task.prompt.as_str()}],
    })];
    let mut artifacts = Vec::new();
    if !task.text.is_empty() {
        artifacts.push(sonic_rs::json!({
            "artifactId": ARTIFACT_ID,
            "parts": [{"text": task.text.as_str()}],
        }));
        if task.state.is_terminal() {
            history.push(sonic_rs::json!({
                "messageId": format!("{id}-agent"),
                "contextId": context.as_str(),
                "taskId": id.as_str(),
                "role": "ROLE_AGENT",
                "parts": [{"text": task.text.as_str()}],
            }));
        }
    }
    sonic_rs::json!({
        "id": id,
        "contextId": context,
        "status": status_json(task),
        "artifacts": artifacts,
        "history": history,
    })
}

/// Builds one artifact update stream response.
fn artifact_update(task: &TaskRec, text: &str, append: bool, last: bool) -> Value {
    sonic_rs::json!({"artifactUpdate": {
        "taskId": task.key.to_string(),
        "contextId": task.key.session.to_string(),
        "artifact": {"artifactId": ARTIFACT_ID, "parts": [{"text": text}]},
        "append": append,
        "lastChunk": last,
    }})
}

/// Builds one status update stream response with optional metadata.
fn status_update(task: &TaskRec, metadata: Option<(&str, String)>) -> Value {
    let mut update = sonic_rs::json!({
        "taskId": task.key.to_string(),
        "contextId": task.key.session.to_string(),
        "status": status_json(task),
    });
    if let (Some((name, text)), Some(object)) = (metadata, update.as_object_mut()) {
        let mut meta = sonic_rs::Object::new();
        meta.insert(name, text.as_str());
        object.insert("metadata", meta);
    }
    sonic_rs::json!({"statusUpdate": update})
}

/// Wraps one task in its stream response.
pub(crate) fn task_response(task: &TaskRec) -> Value {
    sonic_rs::json!({"task": task_json(task)})
}

/// Frames one stream response as one SSE event.
pub(crate) fn frame(framing: &Framing, response: Value) -> String {
    let body = match framing {
        Framing::Rest => response,
        Framing::JsonRpc(id) => {
            sonic_rs::json!({"jsonrpc": "2.0", "id": id.clone(), "result": response})
        }
    };
    let text = sonic_rs::to_string(&body).unwrap_or_else(|_| "null".to_owned());
    format!("data: {text}\n\n")
}

/// Renders the FAILED task answered when a prompt submit starts no turn.
///
/// The id is `<contextId>.failed-<uuid>`: no turn exists to name it, so
/// the task is not retained and `GetTask` reports it as not found.
pub(crate) fn failed_task_json(session: SessionId, prompt: &str, message: &str) -> Value {
    let context = session.to_string();
    let id = format!("{context}.failed-{}", SessionId::new_v7());
    sonic_rs::json!({
        "id": id.as_str(),
        "contextId": context.as_str(),
        "status": {
            "state": TaskState::Failed.wire_name(),
            "message": {
                "messageId": format!("{id}-error"),
                "contextId": context.as_str(),
                "taskId": id.as_str(),
                "role": "ROLE_AGENT",
                "parts": [{"text": message}],
            },
        },
        "artifacts": [],
        "history": [{
            "messageId": format!("{id}-user"),
            "contextId": context.as_str(),
            "taskId": id.as_str(),
            "role": "ROLE_USER",
            "parts": [{"text": prompt}],
        }],
    })
}
