//! A2A part decoding, answer parsing, and question rendering.

use base64::Engine as _;
use dal_core::{Answer, Part, Question, RawJson, Request};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::error::Fail;

/// The image media types dal accepts.
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// The refusal for an unreadable approval answer.
pub(crate) const APPROVAL_HINT: &str = "answer with approve, approve_for_session, or decline";
/// The refusal for a question answer without a data part.
pub(crate) const DATA_HINT: &str = "answer with a data part {\"answer\": <value>}";

/// Converts A2A 1.0 message parts into prompt content.
///
/// Text parts stay text, `raw` images become images, and `data` parts
/// become compact JSON text.
///
/// # Errors
///
/// URL parts and malformed parts are `-32602`; non-image media is `-32005`.
pub(crate) fn prompt_parts(parts: &Value) -> Result<Vec<Part>, Fail> {
    let items = parts
        .as_array()
        .ok_or_else(|| Fail::invalid("message.parts must be an array"))?;
    if items.is_empty() {
        return Err(Fail::invalid("message.parts is empty"));
    }
    items.iter().map(prompt_part).collect()
}

/// Converts one A2A part.
fn prompt_part(part: &Value) -> Result<Part, Fail> {
    if let Some(text) = part.get("text").and_then(|text| text.as_str()) {
        return Ok(Part::Text { text: text.into() });
    }
    if part.get("url").is_some() {
        return Err(Fail::invalid("dalgon does not fetch part urls"));
    }
    let media = part
        .get("mediaType")
        .and_then(|media| media.as_str())
        .unwrap_or("");
    if let Some(raw) = part.get("raw").and_then(|raw| raw.as_str()) {
        if !IMAGE_TYPES.contains(&media) {
            return Err(unsupported_media(media));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|_| Fail::invalid("part raw data is not base64"))?;
        return Ok(Part::Image {
            mime: media.into(),
            bytes: bytes.into(),
        });
    }
    if let Some(data) = part.get("data") {
        let text = sonic_rs::to_string(data).map_err(|error| Fail::internal(error.to_string()))?;
        return Ok(Part::Text { text: text.into() });
    }
    Err(Fail::invalid("each part needs text, raw, url, or data"))
}

/// Builds the unsupported-media failure.
fn unsupported_media(media: &str) -> Fail {
    Fail::new(
        "CONTENT_TYPE_NOT_SUPPORTED",
        format!("dalgon accepts text and images: {media} is not supported"),
    )
}

/// Returns the plain prompt text of content parts, joined by blank lines.
pub(crate) fn prompt_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Parses one answer to an open question from message parts.
///
/// Approval and grant questions accept only `approve`,
/// `approve_for_session`, or `decline` as text or `{"answer": ...}` data;
/// every other question requires a `{"answer": <json>}` data part.
///
/// # Errors
///
/// Returns the exact `-32602` hint when the parts do not answer the question.
pub(crate) fn answer_for(question: &Question, parts: &Value) -> Result<Answer, Fail> {
    let items = parts
        .as_array()
        .map(|items| items.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let data = items
        .iter()
        .find_map(|part| part.get("data").and_then(|data| data.get("answer")));
    match question {
        Question::Approval { .. } | Question::Grant { .. } => {
            let text = items
                .iter()
                .find_map(|part| part.get("text").and_then(|text| text.as_str()))
                .or_else(|| data.and_then(|answer| answer.as_str()));
            match text.map(str::trim) {
                Some("approve") => Ok(Answer::Approve),
                Some("approve_for_session") => Ok(Answer::ApproveForSession),
                Some("decline") => Ok(Answer::Decline),
                _ => Err(Fail::invalid(APPROVAL_HINT)),
            }
        }
        _ => {
            let answer = data.ok_or_else(|| Fail::invalid(DATA_HINT))?;
            let text =
                sonic_rs::to_string(answer).map_err(|error| Fail::internal(error.to_string()))?;
            RawJson::parse(&text)
                .map(Answer::Value)
                .map_err(|error| Fail::internal(error.to_string()))
        }
    }
}

/// Renders the person-facing text of one question, grant clause included.
pub(crate) fn question_text(question: &Question) -> String {
    match question {
        Question::Approval {
            tool,
            preview,
            grant,
        } => {
            let clause = grant.as_ref().map_or_else(String::new, |grant| {
                let roots = grant
                    .roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    " (also allows {} in {roots} until the job ends)",
                    grant.argv_prefix
                )
            });
            format!("Allow {tool}? {}{clause}", preview.title)
        }
        Question::Grant {
            ext,
            capabilities,
            detail,
            ..
        } => format!(
            "Grant {ext}: {}{}",
            capabilities.join(", "),
            detail
                .as_deref()
                .map_or_else(String::new, |detail| format!("\n{detail}"))
        ),
        Question::Select { prompt, .. } | Question::Text { prompt, .. } => prompt.to_string(),
        Question::Confirm { text } => text.to_string(),
        _ => "dal asks a question".to_owned(),
    }
}

/// Builds the `INPUT_REQUIRED` agent message parts: question text plus its data.
///
/// # Errors
///
/// Returns an internal failure when the question cannot be encoded.
pub(crate) fn question_parts(request: &Request) -> Result<Value, Fail> {
    let question =
        sonic_rs::to_value(&request.question).map_err(|error| Fail::internal(error.to_string()))?;
    let default =
        sonic_rs::to_value(&request.default).map_err(|error| Fail::internal(error.to_string()))?;
    Ok(sonic_rs::json!([
        {"text": question_text(&request.question)},
        {"data": {
            "requestId": request.id.to_string(),
            "question": question,
            "default": default,
        }},
    ]))
}
