use std::sync::Arc;

use super::usage_of;
use dal_agent::ext::compact::{CompactError, CompactInput, Compaction, Compactor};
use dal_agent::ext::{BoxFuture, Services};
use dal_core::{CompactOutcome, ContextItem, Inference, ModelRequest, ModelRoute, Purpose};
use dal_core::{RequestParams, StreamEvent};

pub(super) struct Remote;

impl Compactor for Remote {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        Box::pin(async move {
            let context = input.covered_context();
            let params = input.compact_params();
            // The route travels in the request and stays behind for the
            // binding check below; the name is genuinely needed twice.
            let expected = match &input.model {
                ModelRoute::Api { family, model } => Some((*family, model.clone())),
                _ => None,
            };
            let span = input.span;
            let caller = input.caller;
            let request = remote_request(input.model, context, params);
            let inference = services.infer(caller, request).await?;
            let usage = usage_of(&inference);
            match native_outcome(inference) {
                Ok(CompactOutcome::Compacted(history)) if matches!(&expected, Some((family, model)) if history.family == *family && history.model.as_ref() == model.as_ref()) => {
                    Ok(Some(Compaction::native(span, history, usage)))
                }
                Ok(CompactOutcome::Compacted(_)) => Err(CompactError::fail(
                    "remote compaction returned history for another route; refusing it.",
                )),
                Ok(_) => Ok(None),
                Err(_) => Err(CompactError::fail(
                    "remote compaction returned an invalid result.",
                )),
            }
        })
    }
}

/// Builds the single native compaction request for the selected span.
///
/// The caller supplies the already-selected compactable context and request
/// parameters; this helper never re-runs the cut algorithm. An empty system
/// selects the provider-native endpoint; a nonempty system would lower as a
/// plain completion instead.
pub(super) fn remote_request(
    model: ModelRoute,
    context: Arc<[ContextItem]>,
    params: RequestParams,
) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Compact,
        model,
        system: Arc::from(""),
        tools: Arc::from([]),
        context,
        params,
        cache_key: None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeOutcomeError {
    /// No compaction event in a native-compaction response.
    Missing,
    /// An event follows the terminal compaction event.
    NotTerminal,
}

fn native_outcome(inference: Inference) -> Result<CompactOutcome, NativeOutcomeError> {
    if !matches!(
        inference.events.last(),
        Some(StreamEvent::Compaction { .. })
    ) {
        let has = inference
            .events
            .iter()
            .any(|event| matches!(event, StreamEvent::Compaction { .. }));
        return Err(if has {
            NativeOutcomeError::NotTerminal
        } else {
            NativeOutcomeError::Missing
        });
    }
    match inference.events.into_iter().last() {
        Some(StreamEvent::Compaction { outcome }) => Ok(outcome),
        _ => Err(NativeOutcomeError::Missing),
    }
}

#[cfg(test)]
mod tests {
    use super::{native_outcome, remote_request};
    use dal_core::{
        CompactOutcome, CompactedHistory, Family, Inference, ModelRoute, Purpose, RawJson,
        RequestParams, StreamEvent,
    };
    use std::sync::Arc;

    #[test]
    fn native_outcome_preserves_provider_history() {
        let item = RawJson::parse(r#"{"type":"compaction","payload":"opaque"}"#)
            .expect("fixture is valid JSON");
        let history = CompactedHistory {
            family: Family::Responses,
            model: "gpt-test".into(),
            items: vec![item],
        };
        let inference = Inference {
            events: vec![StreamEvent::Compaction {
                outcome: CompactOutcome::Compacted(history.clone()),
            }],
        };

        assert_eq!(
            native_outcome(inference),
            Ok(CompactOutcome::Compacted(history))
        );
    }

    #[test]
    fn native_outcome_preserves_unsupported_result() {
        let inference = Inference {
            events: vec![StreamEvent::Compaction {
                outcome: CompactOutcome::Unsupported,
            }],
        };

        assert_eq!(native_outcome(inference), Ok(CompactOutcome::Unsupported));
    }

    #[test]
    fn native_outcome_rejects_a_missing_compaction_event() {
        let inference = Inference { events: Vec::new() };

        assert_eq!(
            native_outcome(inference),
            Err(super::NativeOutcomeError::Missing)
        );
    }
    #[test]
    fn native_outcome_rejects_events_after_compaction() {
        let inference = Inference {
            events: vec![
                StreamEvent::Compaction {
                    outcome: CompactOutcome::Unsupported,
                },
                StreamEvent::Usage(dal_core::Usage {
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                }),
            ],
        };

        assert_eq!(
            native_outcome(inference),
            Err(super::NativeOutcomeError::NotTerminal)
        );
    }

    #[test]
    fn remote_request_uses_native_call_shape() {
        let route = ModelRoute::Api {
            family: Family::Chat,
            model: "chat-model".into(),
        };
        let request = remote_request(route, Arc::from([]), RequestParams::default());
        assert_eq!(request.purpose, Purpose::Compact);
        assert_eq!(
            request.model,
            ModelRoute::Api {
                family: Family::Chat,
                model: "chat-model".into(),
            }
        );
        assert_eq!(request.system.as_ref(), "");
        assert!(request.tools.is_empty());
        assert!(request.context.is_empty());
        assert_eq!(request.params, RequestParams::default());
        assert_eq!(request.cache_key, None);
    }
}
