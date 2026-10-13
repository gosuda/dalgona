use std::fmt::Display;
use std::sync::Arc;

use super::usage_of;
use dal_agent::error::ServiceError;
use dal_agent::ext::compact::SUMMARY_MAX_OUTPUT_TOKENS;
use dal_agent::ext::compact::{CompactError, CompactInput, Compaction, Compactor};
use dal_agent::ext::{BoxFuture, Services};
use dal_core::{ContextItem, Inference, ModelRequest, ModelRoute, Purpose, RequestParams, Stop};
use dal_core::{StreamChannel, StreamEvent};

pub(super) struct Summary;

impl Compactor for Summary {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        Box::pin(async move {
            let context = input.covered_context();
            let params = input.compact_params();
            let session = input.session;
            let caller = input.caller;
            let instructions = input.instructions;
            let from_entry = input.from_entry();
            let request = summary_request(
                input.model,
                context,
                params,
                session,
                from_entry,
                Some(instructions),
            );
            let inference = services.infer(caller, request).await?;
            match summary_result(&inference) {
                Ok(text) => Ok(Some(Compaction::text(
                    input.span,
                    text,
                    usage_of(&inference),
                ))),
                Err(SummaryError::Cancelled) => Err(CompactError::Service(ServiceError::Cancelled)),
                Err(SummaryError::Failed) => Err(CompactError::fail("summary completion failed")),
                Err(SummaryError::Empty) => Err(CompactError::fail(EMPTY_SUMMARY_MESSAGE)),
            }
        })
    }
}

/// Exact text when the model returns no summary text.
pub(super) const EMPTY_SUMMARY_MESSAGE: &str =
    "Compaction failed: the model returned no summary text.";

/// Why a summary completion cannot become a compaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SummaryError {
    /// The inference was cancelled; the caller stops with no entry.
    Cancelled,
    /// The inference failed; the caller surfaces the service error.
    Failed,
    /// The model returned no summary text.
    Empty,
}

impl Display for SummaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("compaction cancelled"),
            Self::Failed => f.write_str("compaction failed"),
            Self::Empty => f.write_str(EMPTY_SUMMARY_MESSAGE),
        }
    }
}

impl std::error::Error for SummaryError {}

/// Builds the single plain-completion summary request for the selected span.
///
/// The caller supplies the already-selected pre-cut context and parameters;
/// this helper never re-selects the span. The nonempty system forces the
/// provider to lower the request as a plain completion rather than the
/// native endpoint.
pub(super) fn summary_request(
    model: ModelRoute,
    context: Arc<[ContextItem]>,
    params: RequestParams,
    session: impl Display,
    from_entry: impl Display,
    instructions: Option<&str>,
) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Compact,
        model,
        system: Arc::from(local_prompt(session, from_entry, instructions)),
        tools: Arc::from([]),
        context,
        params: params.with_max_output_tokens(SUMMARY_MAX_OUTPUT_TOKENS),
        cache_key: None,
    }
}

/// Maps a summary completion to its text or to the reason it cannot apply.
///
/// Cancellation and failure stop with no compaction entry and no local retry;
/// only nonempty visible text becomes a replacement.
pub(super) fn summary_result(inference: &Inference) -> Result<Box<str>, SummaryError> {
    for event in &inference.events {
        if matches!(event, StreamEvent::Stop(Stop::Cancelled)) {
            return Err(SummaryError::Cancelled);
        }
        if matches!(event, StreamEvent::Stop(Stop::Failed)) {
            return Err(SummaryError::Failed);
        }
    }
    summary_text(&inference.events).ok_or(SummaryError::Empty)
}

const PROMPT_HEAD: &str = r#"The messages above are a conversation to summarize. Write a context checkpoint that another model will use to continue this work.

Use this exact format:

## Goal
[What is the user trying to accomplish?]

## Constraints
- [Constraints, preferences, or requirements; or "(none)"]

## Progress
### Done
- [Completed work]

### In progress
- [Current work]

### Blocked
- [Blockers, if any]

## Key decisions
- [Decision]: [why]

## Next steps
1. [What should happen next, in order]

## Critical context
- [Data, file paths, names, and commands needed to continue; or "(none)"]

Keep each section short. Keep exact file paths, names, and error messages.

"#;

const PROMPT_TAIL: &str = "End with exactly this line:\nJournal: earlier records exist in this session's journal from entry ";
const JOURNAL_MIDDLE: &str = ". Fetch them with read of session://";
const PROMPT_END: &str = ".\n";
const ID_TEXT_RESERVE: usize = 76;

fn local_prompt(
    session: impl Display,
    from_entry: impl Display,
    instructions: Option<&str>,
) -> String {
    let focus = instructions.filter(|instructions| !instructions.is_empty());
    let focus_len = focus.map_or(0, |focus| "Additional focus: ".len() + focus.len() + 2);
    let mut prompt = String::with_capacity(
        PROMPT_HEAD.len()
            + PROMPT_TAIL.len()
            + JOURNAL_MIDDLE.len()
            + PROMPT_END.len()
            + ID_TEXT_RESERVE
            + focus_len,
    );
    prompt.push_str(PROMPT_HEAD);
    if let Some(focus) = focus {
        prompt.push_str("Additional focus: ");
        prompt.push_str(focus);
        prompt.push_str("\n\n");
    }
    prompt.push_str(PROMPT_TAIL);
    prompt.push_str(&from_entry.to_string());
    prompt.push_str(JOURNAL_MIDDLE);
    prompt.push_str(&session.to_string());
    prompt.push('/');
    prompt.push_str(&from_entry.to_string());
    prompt.push_str(PROMPT_END);
    prompt
}

fn summary_text(events: &[StreamEvent]) -> Option<Box<str>> {
    let mut text = String::new();
    for event in events {
        if let StreamEvent::Delta {
            channel: StreamChannel::Text,
            text: delta,
        } = event
        {
            text.push_str(delta);
        }
    }
    (!text.is_empty()).then(|| text.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::{
        EMPTY_SUMMARY_MESSAGE, SummaryError, local_prompt, summary_request, summary_result,
        summary_text,
    };
    use dal_core::StreamEvent;
    use dal_core::{Family, Inference, ModelRoute, Purpose, RequestParams, StreamChannel};
    use std::sync::Arc;

    const PROMPT: &str = "The messages above are a conversation to summarize. Write a context checkpoint that another model will use to continue this work.\n\nUse this exact format:\n\n## Goal\n[What is the user trying to accomplish?]\n\n## Constraints\n- [Constraints, preferences, or requirements; or \"(none)\"]\n\n## Progress\n### Done\n- [Completed work]\n\n### In progress\n- [Current work]\n\n### Blocked\n- [Blockers, if any]\n\n## Key decisions\n- [Decision]: [why]\n\n## Next steps\n1. [What should happen next, in order]\n\n## Critical context\n- [Data, file paths, names, and commands needed to continue; or \"(none)\"]\n\nKeep each section short. Keep exact file paths, names, and error messages.\n\nEnd with exactly this line:\nJournal: earlier records exist in this session's journal from entry e1. Fetch them with read of session://s1/e1.\n";

    #[test]
    fn summary_text_joins_only_visible_text() {
        let events = [
            StreamEvent::Delta {
                channel: StreamChannel::Text,
                text: "check".into(),
            },
            StreamEvent::Delta {
                channel: StreamChannel::Thinking,
                text: "hidden".into(),
            },
            StreamEvent::Delta {
                channel: StreamChannel::Text,
                text: "point".into(),
            },
            StreamEvent::Stop(dal_core::Stop::EndTurn),
        ];
        assert_eq!(summary_text(&events).as_deref(), Some("checkpoint"));
    }

    #[test]
    fn summary_text_refuses_an_empty_completion() {
        assert_eq!(
            summary_text(&[StreamEvent::Stop(dal_core::Stop::EndTurn)]),
            None
        );
    }
    #[test]
    fn summary_request_uses_plain_completion_shape() {
        let route = ModelRoute::Api {
            family: Family::Chat,
            model: "test-model".into(),
        };
        let request = summary_request(
            route,
            Arc::from([]),
            RequestParams::default(),
            "s1",
            "e1",
            None,
        );
        assert_eq!(request.purpose, Purpose::Compact);
        assert_eq!(
            request.model,
            ModelRoute::Api {
                family: Family::Chat,
                model: "test-model".into(),
            }
        );
        assert_ne!(request.system.as_ref(), "");
        assert!(request.system.contains("session://s1/e1"));
        assert_eq!(request.tools.len(), 0);
        assert_eq!(request.params.max_output_tokens, Some(4096));
        assert_eq!(request.cache_key, None);
    }

    #[test]
    fn summary_request_appends_focus_before_journal_line() {
        let route = ModelRoute::Api {
            family: Family::Chat,
            model: "test-model".into(),
        };
        let request = summary_request(
            route,
            Arc::from([]),
            RequestParams::default(),
            "s1",
            "e1",
            Some("retain exact paths"),
        );
        let system = request.system.to_string();
        assert!(
            system.contains("Additional focus: retain exact paths\n\nEnd with exactly this line:")
        );
    }

    #[test]
    fn summary_result_accepts_nonempty_text() {
        let inference = Inference {
            events: vec![dal_core::StreamEvent::Delta {
                channel: dal_core::StreamChannel::Text,
                text: "checkpoint".into(),
            }],
        };
        assert_eq!(summary_result(&inference).as_deref(), Ok("checkpoint"));
    }

    #[test]
    fn summary_result_reports_empty_text_exactly() {
        let inference = Inference {
            events: vec![dal_core::StreamEvent::Stop(dal_core::Stop::EndTurn)],
        };
        assert_eq!(summary_result(&inference), Err(SummaryError::Empty));
        assert_eq!(SummaryError::Empty.to_string(), EMPTY_SUMMARY_MESSAGE);
        assert_eq!(
            EMPTY_SUMMARY_MESSAGE,
            "Compaction failed: the model returned no summary text."
        );
    }

    #[test]
    fn summary_result_stops_on_cancel_without_entry() {
        let inference = Inference {
            events: vec![
                dal_core::StreamEvent::Delta {
                    channel: dal_core::StreamChannel::Text,
                    text: "partial".into(),
                },
                dal_core::StreamEvent::Stop(dal_core::Stop::Cancelled),
            ],
        };
        assert_eq!(summary_result(&inference), Err(SummaryError::Cancelled));
    }

    #[test]
    fn summary_result_stops_on_failure_without_entry() {
        let inference = Inference {
            events: vec![dal_core::StreamEvent::Stop(dal_core::Stop::Failed)],
        };
        assert_eq!(summary_result(&inference), Err(SummaryError::Failed));
    }

    #[test]
    fn summary_prompt_keeps_journal_pointer_and_focus() {
        assert_eq!(local_prompt("s1", "e1", None), PROMPT);
        assert_eq!(local_prompt("s1", "e1", Some("")), PROMPT);
        let focused = local_prompt("s1", "e1", Some("retain exact paths"));
        assert_eq!(
            focused,
            PROMPT.replace(
                "End with exactly this line:",
                "Additional focus: retain exact paths\n\nEnd with exactly this line:",
            ),
        );
    }
}
