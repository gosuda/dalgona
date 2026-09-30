//! Maps core requests to Codex server requests and client results to answers.
//!
//! Approvals of `patch` become `item/fileChange/requestApproval`; every other
//! tool approval becomes `item/commandExecution/requestApproval`. Extension
//! capability grants become `item/permissions/requestApproval`. Select,
//! confirm, and text questions become `item/tool/requestUserInput`. Any
//! unknown, malformed, or missing client value never approves.

use std::path::Path;

use dal_core::{Answer, CallGrant, Choice, Preview, Question, RawJson, Request, TurnId};
use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Value};

use super::items::now_ms;

/// The shared question header shown by Codex clients.
const HEADER: &str = "dal";

/// Builds the Codex server request for one core request.
///
/// Returns `None` for question kinds with no Codex counterpart; the core
/// default then stands.
pub(crate) fn server_request(
    thread: &str,
    turn: TurnId,
    cwd: &Path,
    request: &Request,
) -> Option<(&'static str, Value)> {
    let turn = request.turn.unwrap_or(turn).to_string();
    let item = request.id.to_string();
    let cwd = cwd.display().to_string();
    let mut params = sonic_rs::json!({
        "threadId": thread,
        "turnId": turn,
        "itemId": item.as_str(),
    });
    let object = params.as_object_mut()?;
    let method = match &request.question {
        Question::Approval {
            tool,
            preview,
            grant,
        } => {
            let mut reason = approval_title(tool, preview);
            if let Some(grant) = grant {
                reason.push_str(&grant_clause(grant));
            }
            object.insert("startedAtMs", now_ms());
            object.insert("reason", reason.as_str());
            if tool.as_ref() == "patch" {
                "item/fileChange/requestApproval"
            } else {
                let command = if preview.body.is_empty() {
                    tool.as_ref()
                } else {
                    preview.body.as_ref()
                };
                object.insert("command", command);
                object.insert("cwd", cwd.as_str());
                "item/commandExecution/requestApproval"
            }
        }
        Question::Grant {
            ext,
            capabilities,
            detail,
            ..
        } => {
            object.insert("startedAtMs", now_ms());
            object.insert(
                "reason",
                format!(
                    "grant {ext}: {}{}",
                    capabilities.join(", "),
                    detail
                        .as_deref()
                        .map_or_else(String::new, |detail| format!("\n{detail}"))
                )
                .as_str(),
            );
            object.insert("permissions", permission_profile(capabilities, &cwd));
            object.insert("cwd", cwd.as_str());
            "item/permissions/requestApproval"
        }
        Question::Select {
            prompt, options, ..
        } => {
            let choices: Vec<Value> = options.iter().map(choice_option).collect();
            object.insert("isBlocking", true);
            object.insert("questions", vec![question(&item, prompt, Some(choices))]);
            "item/tool/requestUserInput"
        }
        Question::Confirm { text } => {
            let choices = vec![
                sonic_rs::json!({"label": "yes", "description": "Confirm"}),
                sonic_rs::json!({"label": "no", "description": "Refuse"}),
            ];
            object.insert("isBlocking", true);
            object.insert("questions", vec![question(&item, text, Some(choices))]);
            "item/tool/requestUserInput"
        }
        Question::Text { prompt, .. } => {
            object.insert("isBlocking", true);
            object.insert("questions", vec![question(&item, prompt, None)]);
            "item/tool/requestUserInput"
        }
        _ => return None,
    };
    Some((method, params))
}

/// Maps one client result to its core answer; `None` means the client
/// returned an error or no result.
pub(crate) fn client_answer(request: &Request, result: Option<&Value>) -> Answer {
    let fallback = match &request.question {
        Question::Approval { .. } | Question::Grant { .. } | Question::Confirm { .. } => {
            Answer::Decline
        }
        _ => Answer::Cancel,
    };
    let Some(result) = result else {
        return fallback;
    };
    match &request.question {
        Question::Approval { .. } => decision_answer(result.get("decision")),
        Question::Grant { capabilities, .. } => permissions_answer(capabilities, result),
        Question::Select { options, multi, .. } => {
            let answers = user_answers(result, &request.id.to_string());
            select_answer(options, *multi, &answers).unwrap_or(fallback)
        }
        Question::Confirm { .. } => match user_answers(result, &request.id.to_string()).first() {
            Some(answer) if answer == "yes" => Answer::Approve,
            _ => Answer::Decline,
        },
        Question::Text { .. } => user_answers(result, &request.id.to_string())
            .first()
            .and_then(|text| raw_value(&text.as_str()))
            .map_or(fallback, Answer::Value),
        _ => fallback,
    }
}

/// Maps a command or file-change approval decision; unknown values decline.
pub(crate) fn decision_answer(decision: Option<&Value>) -> Answer {
    let Some(decision) = decision else {
        return Answer::Decline;
    };
    if let Some(text) = decision.as_str() {
        return match text {
            "accept" => Answer::Approve,
            "acceptForSession" => Answer::ApproveForSession,
            "cancel" => Answer::Cancel,
            _ => Answer::Decline,
        };
    }
    if decision
        .get("acceptWithExecpolicyAmendment")
        .is_some_and(sonic_rs::JsonValueTrait::is_object)
    {
        return Answer::ApproveForSession;
    }
    let action = decision
        .get("applyNetworkPolicyAmendment")
        .and_then(|amendment| amendment.get("network_policy_amendment"))
        .and_then(|amendment| amendment.get("action"))
        .and_then(|value| value.as_str());
    match action {
        Some("allow") => Answer::ApproveForSession,
        _ => Answer::Decline,
    }
}

/// Approves a capability grant only when the client granted every requested
/// capability; capabilities with no Codex permission (such as `run`) never approve.
fn permissions_answer(capabilities: &[Box<str>], result: &Value) -> Answer {
    let Some(profile) = result.get("permissions") else {
        return Answer::Decline;
    };
    let paths = |access: &str| {
        profile
            .get("fileSystem")
            .and_then(|file_system| file_system.get(access))
            .and_then(|value| value.as_array())
            .is_some_and(|paths| !paths.is_empty())
    };
    let granted = |capability: &str| match capability {
        "fs.read" => paths("read"),
        "fs.write" => paths("write"),
        "net" => {
            profile
                .get("network")
                .and_then(|network| network.get("enabled"))
                .and_then(sonic_rs::JsonValueTrait::as_bool)
                == Some(true)
        }
        _ => false,
    };
    if !capabilities.is_empty() && capabilities.iter().all(|capability| granted(capability)) {
        Answer::ApproveForSession
    } else {
        Answer::Decline
    }
}

/// Maps selected labels; any unknown label or an empty answer yields `None`.
fn select_answer(options: &[Choice], multi: bool, answers: &[String]) -> Option<Answer> {
    if answers.is_empty() || (!multi && answers.len() > 1) {
        return None;
    }
    let known = |answer: &String| options.iter().any(|choice| *choice.label == **answer);
    if !answers.iter().all(known) {
        return None;
    }
    let value = if multi {
        raw_value(&answers)?
    } else {
        raw_value(&answers[0].as_str())?
    };
    Some(Answer::Value(value))
}

/// Reads the answer list for one question id from a user-input result.
fn user_answers(result: &Value, question: &str) -> Vec<String> {
    result
        .get("answers")
        .and_then(|answers| answers.get(question))
        .and_then(|answer| answer.get("answers"))
        .and_then(|value| value.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Encodes one serializable value as a raw JSON answer.
fn raw_value<T: serde::Serialize + ?Sized>(value: &T) -> Option<RawJson> {
    let text = sonic_rs::to_string(value).ok()?;
    RawJson::parse(&text).ok()
}

/// Builds one `ToolRequestUserInputQuestion`.
fn question(id: &str, prompt: &str, options: Option<Vec<Value>>) -> Value {
    let mut question = sonic_rs::json!({"id": id, "header": HEADER, "question": prompt});
    if let (Some(options), Some(object)) = (options, question.as_object_mut()) {
        object.insert("options", options);
    }
    question
}

/// Builds one `ToolRequestUserInputOption`.
fn choice_option(choice: &Choice) -> Value {
    sonic_rs::json!({
        "label": choice.label.as_ref(),
        "description": choice.description.as_deref().unwrap_or(""),
    })
}

/// Builds the requested permission profile for extension capabilities.
fn permission_profile(capabilities: &[Box<str>], cwd: &str) -> Value {
    let has = |name: &str| capabilities.iter().any(|capability| **capability == *name);
    let mut profile = sonic_rs::json!({});
    let Some(object) = profile.as_object_mut() else {
        return profile;
    };
    let mut file_system = sonic_rs::json!({});
    if let Some(members) = file_system.as_object_mut() {
        if has("fs.read") {
            members.insert("read", vec![cwd]);
        }
        if has("fs.write") {
            members.insert("write", vec![cwd]);
        }
    }
    if file_system
        .as_object()
        .is_some_and(|members| !members.is_empty())
    {
        object.insert("fileSystem", file_system);
    }
    if has("net") {
        object.insert("network", sonic_rs::json!({"enabled": true}));
    }
    profile
}

/// Builds an approval title from the tool and its preview's first line.
fn approval_title(tool: &str, preview: &Preview) -> String {
    match preview.title.lines().next() {
        Some(first) if !first.is_empty() => format!("{tool} {first}"),
        _ => tool.to_owned(),
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
