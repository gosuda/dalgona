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
    Answer, Choice, Name, Preview, Question, RawJson, RegistrationError, ServiceSet, ToolClass,
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
    "may attach a preview of at most 8192 bytes.\n\n",
    "Apply two filters before asking: if the evidence already collected can\n",
    "answer the question, explore instead; if the ideal state settles it, resolve\n",
    "to that instead. An owner decision always survives as a question.\n\n",
    "When no controller can answer, the ask resolves immediately to the\n",
    "fail-closed default: the tool reports that there was no answer and says to\n",
    "continue on best judgment without asking again this turn. Print, JSON, and\n",
    "child sessions are never answerers, so a question raised there fails closed\n",
    "the same way.\n",
);

const PARAMETERS: &str = r#"{"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"id":{"type":"string"},"header":{"type":"string"},"question":{"type":"string"},"kind":{"type":"string","enum":["single","multi","text"]},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"object","properties":{"label":{"type":"string"},"description":{"type":"string"}},"required":["label"],"additionalProperties":false}},"preview":{"type":"string"}},"required":["id","header","question","kind"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}"#;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    questions: Vec<QuestionArgs>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionArgs {
    id: String,
    header: String,
    question: String,
    kind: String,
    #[serde(default)]
    options: Vec<OptionArgs>,
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

#[derive(Debug, thiserror::Error)]
enum InvalidInput {
    #[error("invalid input")]
    Decode,
    #[error("questions must contain 1 to 4 items")]
    QuestionCount,
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
    #[error("kind must be one of single, multi, text")]
    BadKind,
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
            RawJson::parse(PARAMETERS).map_err(|_| RegistrationError::InvalidParameters)?;
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

fn validate(questions: &[QuestionArgs]) -> Result<(), InvalidInput> {
    if questions.is_empty() || questions.len() > 4 {
        return Err(InvalidInput::QuestionCount);
    }
    let mut ids = HashSet::new();
    let mut headers = HashSet::new();
    for question in questions {
        if !is_snake(&question.id) {
            return Err(InvalidInput::BadId);
        }
        if !ids.insert(question.id.as_str()) {
            return Err(InvalidInput::DuplicateId(question.id.clone()));
        }
        if question.header.is_empty()
            || question.header.len() > 48
            || question.header.chars().count() > 12
        {
            return Err(InvalidInput::HeaderLength);
        }
        if !headers.insert(question.header.as_str()) {
            return Err(InvalidInput::DuplicateHeader);
        }
        if question.question.trim().is_empty() {
            return Err(InvalidInput::EmptyQuestion);
        }
        validate_kind(question)?;
        validate_preview(question)?;
    }
    Ok(())
}

fn validate_kind(question: &QuestionArgs) -> Result<(), InvalidInput> {
    match question.kind.as_str() {
        "text" if question.options.is_empty() => Ok(()),
        "text" => Err(InvalidInput::TextOptions),
        "single" | "multi" => validate_options(&question.options),
        _ => Err(InvalidInput::BadKind),
    }
}

fn validate_options(options: &[OptionArgs]) -> Result<(), InvalidInput> {
    if options.is_empty() {
        return Err(InvalidInput::MissingOptions);
    }
    if options.len() < 2 || options.len() > 4 {
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
    if preview.is_empty() || preview.len() > 8192 {
        return Err(InvalidInput::PreviewLength);
    }
    Ok(())
}

fn reject(reason: &InvalidInput) -> ToolOutcome {
    ToolOutcome::Err(dal_agent::ToolError::message(format!("ask: {reason}")))
}

fn decode_answer(kind: &str, answer: &Answer) -> Option<String> {
    let Answer::Value(value) = answer else {
        return None;
    };
    match kind {
        "single" => {
            let label: &str = sonic_rs::from_str(value.as_str()).ok()?;
            (!label.is_empty()).then(|| label.to_owned())
        }
        "text" => {
            let label: &str = sonic_rs::from_str(value.as_str()).ok()?;
            let trimmed = label.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        _ => {
            let labels: Vec<&str> = sonic_rs::from_str(value.as_str()).ok()?;
            if labels.is_empty() || labels.len() > 4 || labels.contains(&"") {
                return None;
            }
            Some(labels.join(", "))
        }
    }
}

async fn raise(
    services: &dyn dal_agent::ext::Services,
    caller: &dal_agent::ext::Caller,
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
    Ok(answer
        .as_ref()
        .and_then(|answer| decode_answer(&question.kind, answer)))
}

async fn run(call: ToolCall, cx: ToolCx<'_>) -> ToolOutcome {
    let Ok(args) = sonic_rs::from_str::<Args>(call.args.as_str()) else {
        return reject(&InvalidInput::Decode);
    };
    if let Err(reason) = validate(&args.questions) {
        return reject(&reason);
    }
    let services = cx.services();
    let caller = cx.caller().clone();
    let mut lines: Vec<String> = Vec::new();
    for (index, question) in args.questions.iter().enumerate() {
        let answer = match raise(services.as_ref(), &caller, question).await {
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
