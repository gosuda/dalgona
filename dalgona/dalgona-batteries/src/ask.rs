// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The bundled ask battery: one tool that asks the user one to four typed
//! questions and waits for the answers.

use std::collections::HashSet;
use std::sync::Arc;

use dal_agent::ext::{
    ArgError, BoxFuture, Extension, ExtensionBuilder, PromptOrder, PromptSection, RawValue, Tool,
    ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_core::ext::Visibility;
use dal_core::{
    Answer, Choice, Name, Notice, Preview, Question, RegistrationError, ServiceSet, ToolClass,
    ToolSpec, Workspace,
};

const EXTENSION_NAME: &str = "ask";
const TOOL_NAME: &str = "ask";

const DESCRIPTION: &str = "Ask the user one to four short questions and wait for the answers. Each question is\nsingle-select, multi-select, or free text; add a preview only when seeing a plan or a\ndiff would change the choice. Apply two filters before asking: if the evidence already\ncollected can answer the question, explore instead; if the ideal state settles it,\nresolve to that instead. An owner decision always survives as a question: irreversible,\ndestructive, safety-critical, or cross-cutting product choices. If the call returns no\nanswer, continue on best judgment and do not ask again this turn. Never use it for\npermission requests; permission goes through approval.";

const SECTION: &str = "When you consider asking the user a question, apply two filters. First: could the\nevidence already collected answer it? Then explore instead of asking. Second: does the\nideal state settle it? Then resolve to that state instead of asking. An owner decision\nalways survives as a question: irreversible or destructive actions, safety-critical\nchoices, and cross-cutting product choices. Explore to sufficiency, then stop.";

const NO_ANSWER: &str = "No answer (dismissed, timed out, or no user attached). Continue on best judgment; do not ask again this turn.";

/// The text of the `dalgona://ask` manual page.
pub const ASK_DOC: &str = concat!(
    "# ask\n\n",
    "The ask tool raises one to four typed questions through the shared ask\n",
    "service and waits for exactly one answer. A question is single-select,\n",
    "multi-select, or free text, carries a header of at most 12 characters, and\n",
    "may attach a preview of at most 8192 bytes. Question ids are snake_case\n",
    "and unique per call; select kinds carry two to four options whose labels\n",
    "and descriptions are non-empty and free of control characters.\n\n",
    "Select and text answers arrive as {\"selected\": [label, ...]} and\n",
    "{\"text\": answer} objects; an empty submission is a dismiss. Add\n",
    "\"explicit\": true only when the user asked for this question directly.\n\n",
    "Apply two filters before asking: if the evidence already collected can\n",
    "answer the question, explore instead; if the ideal state settles it, resolve\n",
    "to that instead. An owner decision always survives as a question.\n\n",
    "The open-question timeout is `[ask] timeout` in dal.toml; it defaults to\n",
    "30 minutes and clamps into 1 to 120 minutes at load, with one startup\n",
    "notice when clamped. Each raised question keeps one absolute deadline from\n",
    "its raise; dismissal, timeout, and a missing controller all reach the tool\n",
    "as one merged no answer: continue on best judgment and do not ask again\n",
    "this turn. Print, JSON, and child sessions are never answerers, so a\n",
    "question raised there fails closed the same way.\n\n",
    "The TUI shows one question at a time with a count for the queued rest,\n",
    "each select offering its options plus an always-present Other free-text\n",
    "row. Wire clients with the question capability receive the whole request\n",
    "and answer `session/answer`; a losing answer is refused after the first\n",
    "resolution, and a client that attaches late reads the open requests in\n",
    "the session view.\n",
);

const PARAMETERS: &str = r#"{"type":"object","additionalProperties":false,"required":["questions"],"properties":{"explicit":{"type":"boolean","default":false,"description":"true when the user asked for this question directly"},"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","additionalProperties":false,"required":["id","header","question","kind"],"properties":{"id":{"type":"string","pattern":"^[a-z][a-z0-9]*(_[a-z0-9]+)*$"},"header":{"type":"string","minLength":1,"maxLength":12},"question":{"type":"string","minLength":1},"kind":{"type":"string","enum":["single","multi","text"]},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"object","additionalProperties":false,"required":["label"],"properties":{"label":{"type":"string","minLength":1},"description":{"type":"string","minLength":1}}}},"preview":{"type":"string","minLength":1,"maxLength":8192}}}}}}"#;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    questions: Vec<QuestionArgs>,
    #[serde(default, rename = "explicit")]
    _explicit: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionArgs {
    id: String,
    header: String,
    question: String,
    kind: String,
    #[serde(default)]
    options: Option<Vec<OptionArgs>>,
    #[serde(default)]
    preview: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionArgs {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

/// The strict `select` answer object: `{"selected": [<label>, ...]}`. The
/// terminal dialog sends it; the bare string and array spellings are the
/// other accepted form.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedAnswer {
    selected: Vec<String>,
}

/// The strict `text` answer object: `{"text": <answer>}`. The terminal dialog
/// sends it; the bare string is the other accepted form.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TextAnswer {
    text: String,
}

#[derive(Debug, thiserror::Error)]
enum InvalidInput {
    #[error("{0}")]
    Decode(String),
    #[error("questions must contain 1 to 4 items")]
    QuestionCount,
    #[error("kind must be one of single, multi, text")]
    BadKind,
    #[error("id must be snake_case (a-z, 0-9, underscores, starting with a letter)")]
    BadId,
    #[error("question ids must be unique; \"{0}\" repeats")]
    DuplicateId(String),
    #[error("header must be 1 to 12 characters")]
    HeaderLength,
    #[error("header must be unique within one call")]
    DuplicateHeader,
    #[error("question must be non-empty")]
    EmptyQuestion,
    #[error("a text question takes no options")]
    TextOptions,
    #[error("a single or multi question needs options")]
    MissingOptions,
    #[error("options must contain 2 to 4 items")]
    OptionCount,
    #[error("option label must be non-empty")]
    EmptyLabel,
    #[error("option label must not contain control characters")]
    LabelControl,
    #[error("option description must be non-empty when present")]
    EmptyDescription,
    #[error("option description must not contain control characters")]
    DescriptionControl,
    #[error("preview is allowed only on single and multi questions")]
    TextPreview,
    #[error("preview must be 1 to 8192 bytes")]
    PreviewLength,
}

struct AskTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl AskTool {
    fn new() -> Result<Self, RegistrationError> {
        let name = Name::parse(TOOL_NAME)?;
        let parameters =
            RawValue::parse(PARAMETERS).map_err(|_| RegistrationError::InvalidParameters)?;
        if !dal_core::valid_tool_parameters(&parameters) {
            return Err(RegistrationError::InvalidParameters);
        }
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: DESCRIPTION.into(),
            parameters,
            grammar: None,
        });
        Ok(Self { name, spec })
    }
}

impl Tool for AskTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(run(call, cx))
    }
}

fn is_snake(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
        && !id.contains("__")
        && !id.ends_with('_')
}

fn has_control(value: &str) -> bool {
    value.chars().any(|character| character < '\u{20}')
}

/// Validates one call before any raise, in the order the contract fixes:
/// the question count, then per question the kind, id, header, question
/// text, options, and preview.
fn validate(questions: &[QuestionArgs]) -> Result<(), InvalidInput> {
    if questions.is_empty() || questions.len() > 4 {
        return Err(InvalidInput::QuestionCount);
    }
    let mut ids = HashSet::new();
    let mut headers = HashSet::new();
    for question in questions {
        validate_kind(question)?;
        validate_identity(question, &mut ids)?;
        validate_header(question, &mut headers)?;
        if question.question.trim().is_empty() {
            return Err(InvalidInput::EmptyQuestion);
        }
        validate_options(question)?;
        validate_preview(question)?;
    }
    Ok(())
}

fn validate_kind(question: &QuestionArgs) -> Result<(), InvalidInput> {
    match question.kind.as_str() {
        "single" | "multi" | "text" => Ok(()),
        _ => Err(InvalidInput::BadKind),
    }
}

fn validate_identity(
    question: &QuestionArgs,
    ids: &mut HashSet<String>,
) -> Result<(), InvalidInput> {
    if !is_snake(&question.id) {
        return Err(InvalidInput::BadId);
    }
    if !ids.insert(question.id.clone()) {
        return Err(InvalidInput::DuplicateId(question.id.clone()));
    }
    Ok(())
}

fn validate_header(
    question: &QuestionArgs,
    headers: &mut HashSet<String>,
) -> Result<(), InvalidInput> {
    let characters = question.header.chars().count();
    if !(1..=12).contains(&characters) {
        return Err(InvalidInput::HeaderLength);
    }
    if !headers.insert(question.header.clone()) {
        return Err(InvalidInput::DuplicateHeader);
    }
    Ok(())
}

/// Options are present exactly on `single` and `multi` questions, two to
/// four of them, each with a non-empty label and an optional non-empty
/// description, both free of control characters.
fn validate_options(question: &QuestionArgs) -> Result<(), InvalidInput> {
    let selects = question.kind != "text";
    let options = match (selects, question.options.as_deref()) {
        (true, None) => return Err(InvalidInput::MissingOptions),
        (false, Some(_)) => return Err(InvalidInput::TextOptions),
        (false, None) => return Ok(()),
        (true, Some(options)) => options,
    };
    if !(2..=4).contains(&options.len()) {
        return Err(InvalidInput::OptionCount);
    }
    options.iter().try_for_each(validate_option)
}

fn validate_option(option: &OptionArgs) -> Result<(), InvalidInput> {
    if option.label.trim().is_empty() {
        return Err(InvalidInput::EmptyLabel);
    }
    if has_control(&option.label) {
        return Err(InvalidInput::LabelControl);
    }
    match &option.description {
        Some(description) if description.trim().is_empty() => Err(InvalidInput::EmptyDescription),
        Some(description) if has_control(description) => Err(InvalidInput::DescriptionControl),
        _ => Ok(()),
    }
}

fn validate_preview(question: &QuestionArgs) -> Result<(), InvalidInput> {
    let Some(preview) = &question.preview else {
        return Ok(());
    };
    if question.kind == "text" {
        return Err(InvalidInput::TextPreview);
    }
    if !(1..=8192).contains(&preview.len()) {
        return Err(InvalidInput::PreviewLength);
    }
    Ok(())
}

fn reject(reason: &InvalidInput) -> ToolOutcome {
    ToolOutcome::Err(dal_agent::ToolError::message(format!("ask: {reason}")))
}
/// One decoded answer: an answered line, a blank submission that dismisses
/// the question, or a payload of the wrong shape.
enum Decoded {
    Answered(String),
    Blank,
    Malformed,
}

/// Decodes one strict answer payload in one of its two accepted spellings:
/// the bare value (a string for `single` and `text`, an array of strings for
/// `multi`) or the strict object (`{"text": ...}`, `{"selected": [...]}`).
///
/// The opening byte picks the spelling, so an object that fails its strict
/// decode (an unknown member, a wrong member) is malformed and never falls
/// through to a bare-value decode. Text trims to its answer; blank text and
/// an empty selection dismiss. Anything of the wrong shape is malformed.
fn decode_answer(kind: &str, answer: &Answer) -> Decoded {
    let Answer::Value(value) = answer else {
        return Decoded::Blank;
    };
    let raw = value.as_str();
    let Some(opening) = raw.trim_start().bytes().next() else {
        return Decoded::Malformed;
    };
    match (kind, opening) {
        ("text", b'{') => sonic_rs::from_str::<TextAnswer>(raw)
            .map_or(Decoded::Malformed, |payload| decode_text(&payload.text)),
        ("text", b'"') => {
            sonic_rs::from_str::<String>(raw).map_or(Decoded::Malformed, |text| decode_text(&text))
        }
        ("single" | "multi", b'{') => sonic_rs::from_str::<SelectedAnswer>(raw)
            .map_or(Decoded::Malformed, |payload| {
                decode_selection(kind, payload.selected)
            }),
        ("single", b'"') => match sonic_rs::from_str::<String>(raw) {
            Ok(label) if label.trim().is_empty() => Decoded::Blank,
            Ok(label) => Decoded::Answered(label),
            Err(_) => Decoded::Malformed,
        },
        ("multi", b'[') => sonic_rs::from_str::<Vec<String>>(raw)
            .map_or(Decoded::Malformed, |labels| decode_selection(kind, labels)),
        _ => Decoded::Malformed,
    }
}

/// A text answer trims to its answer; blank text dismisses.
fn decode_text(text: &str) -> Decoded {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Decoded::Blank
    } else {
        Decoded::Answered(trimmed.to_owned())
    }
}

/// A selection holds exactly one non-blank label for `single` and one to
/// four for `multi`; an empty selection dismisses.
fn decode_selection(kind: &str, mut labels: Vec<String>) -> Decoded {
    match (kind, labels.as_mut_slice()) {
        ("single", [one]) if !one.trim().is_empty() => Decoded::Answered(std::mem::take(one)),
        ("multi", selected)
            if (1..=4).contains(&selected.len())
                && selected.iter().all(|label| !label.trim().is_empty()) =>
        {
            Decoded::Answered(selected.join(", "))
        }
        ("single" | "multi", []) => Decoded::Blank,
        _ => Decoded::Malformed,
    }
}

/// The notice text that names a malformed answer for one question.
fn malformed_notice(header: &str, answer: &Answer) -> String {
    let raw = match answer {
        Answer::Value(value) => value.as_str(),
        _ => "no value",
    };
    let body: String = raw.chars().take(200).collect();
    format!("ask: malformed answer for question \"{header}\": {body}")
}

async fn raise(
    services: &dyn dal_agent::ext::Services,
    caller: &dal_agent::ext::Caller,
    turn: Option<dal_core::TurnId>,
    question: &QuestionArgs,
) -> Result<Option<String>, dal_agent::error::ServiceError> {
    let prompt = format!("{}: {}", question.header, question.question);
    let asked = if question.kind == "text" {
        services.ask(
            caller,
            Question::Text {
                prompt: prompt.into(),
                placeholder: None,
            },
        )
    } else {
        let options = question
            .options
            .iter()
            .flatten()
            .map(|option| Choice {
                label: option.label.as_str().into(),
                description: option.description.as_deref().map(Box::from),
            })
            .collect();
        let preview = question.preview.as_ref().map(|body| Preview {
            title: question.header.as_str().into(),
            digest: None,
            body: body.as_str().into(),
        });
        services.ask(
            caller,
            Question::Select {
                prompt: prompt.into(),
                options,
                multi: question.kind == "multi",
                preview,
            },
        )
    };
    let answer = asked.await?;
    let Some(value) = answer.as_ref() else {
        return Ok(None);
    };
    match decode_answer(&question.kind, value) {
        Decoded::Answered(text) => Ok(Some(text)),
        Decoded::Blank => Ok(None),
        Decoded::Malformed => {
            services.notify(
                caller,
                Notice {
                    turn,
                    kind: "ask".into(),
                    text: malformed_notice(&question.header, value).into(),
                },
            );
            Ok(None)
        }
    }
}

async fn run(call: ToolCall, cx: ToolCx<'_>) -> ToolOutcome {
    let args = match sonic_rs::from_str::<Args>(call.args.as_str()) {
        Ok(args) => args,
        Err(error) => {
            let message = error.to_string();
            let first = message.lines().next().unwrap_or("invalid input");
            return reject(&InvalidInput::Decode(first.to_owned()));
        }
    };
    if let Err(reason) = validate(&args.questions) {
        return reject(&reason);
    }
    let services = cx.services();
    let caller = cx.caller().clone();
    let turn = cx.turn();
    let mut lines: Vec<String> = Vec::new();
    for (index, question) in args.questions.iter().enumerate() {
        let answer = match raise(services.as_ref(), &caller, turn, question).await {
            Ok(answer) => answer,
            Err(dal_agent::error::ServiceError::Cancelled) if cx.cancel().is_cancelled() => {
                return ToolOutcome::Interrupted;
            }
            Err(_) => None,
        };
        let Some(answer) = answer else {
            if lines.is_empty() {
                return ToolOutcome::Ok(Box::new(ToolOutput::from_text(NO_ANSWER)));
            }
            let rest: Vec<&str> = args.questions[index..]
                .iter()
                .map(|question| question.header.as_str())
                .collect();
            lines.push(format!("Unanswered: {}", rest.join(", ")));
            return ToolOutcome::Ok(Box::new(ToolOutput::from_text(lines.join("\n"))));
        };
        lines.push(format!("{}: {}", question.header, answer));
    }
    ToolOutcome::Ok(Box::new(ToolOutput::from_text(lines.join("\n"))))
}

/// Builds the bundled ask extension. Registration performs no I/O.
///
/// # Errors
/// Returns [`RegistrationError`] when the fixed identity or tool
/// declaration is rejected.
pub fn ask() -> Result<Extension, RegistrationError> {
    let tool = AskTool::new()?;
    let inject = ServiceSet::from_names(["ask"])?;
    ExtensionBuilder::new(EXTENSION_NAME, env!("CARGO_PKG_VERSION"), inject)?
        .with_origin(dal_core::Origin::Bundled, None)
        .tool(Arc::new(tool), Visibility::Model)
        .prompt_section(PromptSection::Static {
            order: PromptOrder::Plugins,
            visibility: Visibility::Model,
            text: SECTION.into(),
        })
        .build()
}
