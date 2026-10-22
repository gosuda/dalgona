//! Maps core requests to Codex server requests and client results to answers.
//!
//! Approvals of `patch` become `item/fileChange/requestApproval`; every other
//! tool approval becomes `item/commandExecution/requestApproval`, and each
//! reply is parsed with the decision table of the method that asked. Extension
//! capability grants become `item/permissions/requestApproval`. Select,
//! confirm, and text questions become `item/tool/requestUserInput`. Any
//! unknown, malformed, or missing client value never approves.
//!
//! A user-input result decodes through the rich-ask contract the local ask
//! path enforces: each question takes the shape its kind fixes, and a result
//! outside it fails with [`AnswerError`] instead of being trimmed down to a
//! shape that fits. The adapter then resolves the question as dismissed.

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
            ..
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

/// A user-input result that is not a valid answer to its rich-ask question.
///
/// The rules are the ones the local ask path enforces: a single select takes
/// exactly one offered label, a multi select takes distinct offered labels, a
/// text question takes exactly one string, and a confirmation takes `yes` or
/// `no`. An empty answer list is a dismissal, not an error.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum AnswerError {
    /// The result has no `answers` object.
    #[error("the result has no `answers` object")]
    MissingAnswers,
    /// The entry for the question has no `answers` array.
    #[error("the entry for question {question} has no `answers` array")]
    NotAList { question: String },
    /// One answer is not a string.
    #[error("answer {index} is not a string")]
    NotString { index: usize },
    /// A single-answer question got several answers.
    #[error("the question takes exactly one answer, got {count}")]
    Arity { count: usize },
    /// An answer names a choice the question did not offer.
    #[error("`{label}` is not an offered choice")]
    UnknownLabel { label: String },
    /// A multi select names one choice twice.
    #[error("`{label}` is selected more than once")]
    DuplicateLabel { label: String },
    /// The accepted answer did not encode as JSON.
    #[error("the answer does not encode as JSON")]
    Encode,
}

/// Maps one client result to its core answer; `None` means the client
/// returned an error or no result. A malformed result fails closed like a
/// dismissal and is logged with its [`AnswerError`].
pub(crate) fn client_answer(request: &Request, result: Option<&Value>) -> Answer {
    let fallback = dismissal(&request.question);
    let Some(result) = result else {
        return fallback;
    };
    match &request.question {
        Question::Approval { tool, .. } => approval_answer(tool, result.get("decision")),
        Question::Grant { capabilities, .. } => permissions_answer(capabilities, result),
        _ => ask_answer(request, result).unwrap_or_else(|error| {
            tracing::warn!(%error, request = %request.id, "codex answer is malformed; failing closed");
            fallback
        }),
    }
}

/// Returns the answer a dismissed or malformed result resolves to: approvals,
/// grants, and confirmations decline; every other question cancels.
fn dismissal(question: &Question) -> Answer {
    match question {
        Question::Approval { .. } | Question::Grant { .. } | Question::Confirm { .. } => {
            Answer::Decline
        }
        _ => Answer::Cancel,
    }
}

/// Decodes one `item/tool/requestUserInput` result into the answer to the
/// request's select, confirm, or text question.
///
/// # Errors
/// Returns [`AnswerError`] when the result is not a valid answer to the
/// question.
pub(crate) fn ask_answer(request: &Request, result: &Value) -> Result<Answer, AnswerError> {
    let Some(labels) = user_answers(result, &request.id.to_string())? else {
        return Ok(dismissal(&request.question));
    };
    match &request.question {
        Question::Select { options, multi, .. } => select_answer(options, *multi, &labels),
        Question::Confirm { .. } => match exactly_one(&labels)? {
            "yes" => Ok(Answer::Approve),
            "no" => Ok(Answer::Decline),
            other => Err(AnswerError::UnknownLabel {
                label: other.to_owned(),
            }),
        },
        Question::Text { .. } => raw_value(&exactly_one(&labels)?).map(Answer::Value),
        _ => Ok(dismissal(&request.question)),
    }
}

/// Routes an approval reply to the decision table of the method that asked:
/// `server_request` sends patch approvals as file-change requests and every
/// other tool approval as command requests.
fn approval_answer(tool: &str, decision: Option<&Value>) -> Answer {
    if tool == "patch" {
        return file_change_decision(decision);
    }
    command_decision(decision)
}

/// Maps a command-execution approval decision; unknown or malformed values decline.
///
/// Dal has no command-prefix- or host-scoped allowance, so an accepted
/// amendment approves only this call (with its call-scoped grant): the
/// session persistence the client agreed to is not invented here.
pub(crate) fn command_decision(decision: Option<&Value>) -> Answer {
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
        .and_then(|amendment| amendment.get("execpolicy_amendment"))
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty() && items.iter().all(|item| item.as_str().is_some()))
    {
        return Answer::Approve;
    }
    let action = decision
        .get("applyNetworkPolicyAmendment")
        .and_then(|amendment| amendment.get("network_policy_amendment"))
        .and_then(|amendment| amendment.get("action"))
        .and_then(|value| value.as_str());
    match action {
        Some("allow") => Answer::Approve,
        _ => Answer::Decline,
    }
}

/// Maps a file-change approval decision; only the four pinned strings apply.
///
/// The amendment objects belong to command-execution approvals, so any
/// object declines here.
pub(crate) fn file_change_decision(decision: Option<&Value>) -> Answer {
    match decision.and_then(|value| value.as_str()) {
        Some("accept") => Answer::Approve,
        Some("acceptForSession") => Answer::ApproveForSession,
        Some("cancel") => Answer::Cancel,
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

/// Maps selected labels: a single select takes exactly one offered label and
/// answers with a JSON string; a multi select takes distinct offered labels
/// and answers with a JSON array of strings.
fn select_answer(options: &[Choice], multi: bool, labels: &[&str]) -> Result<Answer, AnswerError> {
    if !multi {
        exactly_one(labels)?;
    }
    for (index, label) in labels.iter().enumerate() {
        if !options.iter().any(|choice| choice.label.as_ref() == *label) {
            return Err(AnswerError::UnknownLabel {
                label: (*label).to_owned(),
            });
        }
        if labels.iter().take(index).any(|earlier| earlier == label) {
            return Err(AnswerError::DuplicateLabel {
                label: (*label).to_owned(),
            });
        }
    }
    let value = if multi {
        raw_value(labels)?
    } else {
        raw_value(&exactly_one(labels)?)?
    };
    Ok(Answer::Value(value))
}

/// Returns the one answer a single-answer question takes.
fn exactly_one<'a>(labels: &[&'a str]) -> Result<&'a str, AnswerError> {
    match labels {
        [one] => Ok(*one),
        _ => Err(AnswerError::Arity {
            count: labels.len(),
        }),
    }
}

/// Reads the answer list for one question id from a user-input result.
///
/// `Ok(None)` is a dismissal: the client sent no entry for the question or an
/// empty list. Anything else must be a list of strings.
fn user_answers<'a>(
    result: &'a Value,
    question: &str,
) -> Result<Option<Vec<&'a str>>, AnswerError> {
    let answers = match result.get("answers") {
        Some(answers) if answers.is_object() => answers,
        _ => return Err(AnswerError::MissingAnswers),
    };
    let Some(entry) = answers.get(question) else {
        return Ok(None);
    };
    let list = entry
        .get("answers")
        .and_then(|list| list.as_array())
        .ok_or_else(|| AnswerError::NotAList {
            question: question.to_owned(),
        })?;
    let labels = list
        .iter()
        .enumerate()
        .map(|(index, item)| item.as_str().ok_or(AnswerError::NotString { index }))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((!labels.is_empty()).then_some(labels))
}

/// Encodes one serializable value as a raw JSON answer.
fn raw_value<T: serde::Serialize + ?Sized>(value: &T) -> Result<RawJson, AnswerError> {
    let text = sonic_rs::to_string(value).map_err(|_| AnswerError::Encode)?;
    RawJson::parse(&text).map_err(|_| AnswerError::Encode)
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
