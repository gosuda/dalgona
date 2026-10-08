//! Provider transport helpers: error decoding, redaction, permits, and refresh.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use dal_core::Family;
use futures::{Stream, StreamExt, future::ready};
use serde::Deserialize;
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{
        credential::{Credential, OAuthCredential},
        refresh::{OAuthProvider, RefreshReason, Refresher},
    },
    error::{ProviderError, UsageCheckReason},
    family,
    stream::{EventStream, StreamEvent},
};

#[derive(Default, Deserialize)]
struct ErrorBody {
    error: Option<ErrorDetail>,
    message: Option<String>,
    detail: Option<String>,
    code: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Default, Deserialize)]
struct ErrorDetail {
    code: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

pub(crate) fn error_fields(body: &str) -> (Option<String>, String) {
    let parsed = sonic_rs::from_str::<ErrorBody>(body).ok();
    let code = parsed.as_ref().and_then(|body| {
        body.error
            .as_ref()
            .and_then(|error| error.code.clone().or_else(|| error.kind.clone()))
            .or_else(|| body.code.clone().or_else(|| body.kind.clone()))
    });
    let message = parsed
        .and_then(|body| {
            body.error
                .and_then(|error| error.message)
                .or(body.message)
                .or(body.detail)
        })
        .unwrap_or_else(|| body.lines().next().unwrap_or_default().to_owned());
    (code, first_line(&message, 300))
}

pub(crate) fn first_line(input: &str, max_bytes: usize) -> String {
    let line = input.lines().next().unwrap_or_default();
    let mut end = line.len().min(max_bytes);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

pub(crate) fn decode_response(
    response: reqwest::Response,
    family: Family,
    provider: &str,
    model: &str,
    oauth: bool,
    secrets: Vec<Box<str>>,
) -> EventStream {
    let read_failure: Arc<Mutex<Option<ProviderError>>> = Arc::default();
    let chunks = response
        .bytes_stream()
        .scan(Arc::clone(&read_failure), move |slot, chunk| {
            ready(match chunk {
                Ok(bytes) => Some(bytes.to_vec()),
                Err(error) => {
                    // An idle stall stays a cut, like the Codex HTTPS stream.
                    if !error.is_timeout()
                        && let Ok(mut failure) = slot.lock()
                    {
                        *failure = Some(crate::http::from_reqwest(family, error));
                    }
                    None
                }
            })
        });
    let events = crate::sse::decode_stream(chunks);
    let decoded: Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>> =
        match family {
            Family::Chat => Box::pin(family::chat::decode_events(
                events,
                family::chat::ChatDecoder::new(provider, model),
            )),
            Family::Responses | Family::Codex => Box::pin(family::responses::decode(
                events,
                family,
                Box::<str>::from(model),
            )),
            Family::Anthropic => Box::pin(family::anthropic::decode_stream(
                events,
                model.into(),
                oauth,
            )),
        };
    // A body read failure ends the byte source, so the decoder would report
    // it as a cut (or as a protocol error for a half-read frame); the transport
    // failure is the retryable truth.
    let safe_errors = decoded.map(move |result| {
        result.map_err(|error| {
            let failure = read_failure.lock().ok().and_then(|mut slot| slot.take());
            redact_provider_error(failure.unwrap_or(error), &secrets)
        })
    });
    EventStream::new(safe_errors, || {})
}

pub(crate) fn redact_provider_error(error: ProviderError, secrets: &[Box<str>]) -> ProviderError {
    let redact = |text: String| redact_text(text, secrets);
    match error {
        ProviderError::Transport { family, reason } => ProviderError::Transport {
            family,
            reason: redact(reason),
        },
        ProviderError::Status {
            family,
            status,
            message,
        } => ProviderError::Status {
            family,
            status,
            message: redact(message),
        },
        ProviderError::InvalidRequest { message } => ProviderError::InvalidRequest {
            message: redact(message),
        },
        ProviderError::ContextOverflow {
            family,
            code,
            message,
        } => ProviderError::ContextOverflow {
            family,
            code: redact(code),
            message: redact(message),
        },
        ProviderError::RateLimited {
            message,
            retry_after,
        } => ProviderError::RateLimited {
            message: redact(message),
            retry_after,
        },
        ProviderError::RetryAfterTooLong { seconds, message } => ProviderError::RetryAfterTooLong {
            seconds,
            message: redact(message),
        },
        ProviderError::Quota { message } => ProviderError::Quota {
            message: redact(message),
        },
        ProviderError::WsClosed { code } => ProviderError::WsClosed {
            code: code.map(|(code, reason)| (code, redact(reason))),
        },
        ProviderError::Protocol { family, detail } => ProviderError::Protocol {
            family,
            detail: redact(detail),
        },
        ProviderError::UsageLimit { model, message } => ProviderError::UsageLimit {
            model: redact(model),
            message: redact(message),
        },
        ProviderError::UsageNotIncluded { message } => ProviderError::UsageNotIncluded {
            message: redact(message),
        },
        ProviderError::ReserveUnavailable { status, message } => {
            ProviderError::ReserveUnavailable {
                status,
                message: redact(message),
            }
        }
        ProviderError::AuthFileInvalid { path, message } => ProviderError::AuthFileInvalid {
            path,
            message: redact(message),
        },
        ProviderError::AuthWrite { reason } => ProviderError::AuthWrite {
            reason: redact(reason),
        },
        ProviderError::CallbackBind { port, reason } => ProviderError::CallbackBind {
            port,
            reason: redact(reason),
        },
        ProviderError::TokenExchange { status, message } => ProviderError::TokenExchange {
            status,
            message: redact(message),
        },
        ProviderError::DeviceCode { status, message } => ProviderError::DeviceCode {
            status,
            message: redact(message),
        },
        ProviderError::UsageCheck { reason } => ProviderError::UsageCheck {
            reason: redact_usage_reason(reason, secrets),
        },
        other => other,
    }
}

pub(crate) fn redact_text(mut text: String, secrets: &[Box<str>]) -> String {
    for secret in secrets {
        if !secret.is_empty() && text.contains(secret.as_ref()) {
            text = text.replace(secret.as_ref(), "<redacted>");
        }
    }
    text
}

fn redact_usage_reason(reason: UsageCheckReason, secrets: &[Box<str>]) -> UsageCheckReason {
    let redact = |text: String| redact_text(text, secrets);
    match reason {
        UsageCheckReason::Status { status, message } => UsageCheckReason::Status {
            status,
            message: redact(message),
        },
        UsageCheckReason::Transport { reason } => UsageCheckReason::Transport {
            reason: redact(reason),
        },
        other => other,
    }
}
pub(crate) async fn acquire_provider_permit(
    owner: &super::set::ProviderSetInner,
    provider: &str,
    cancel: &CancellationToken,
) -> Result<OwnedSemaphorePermit, ProviderError> {
    {
        let slot = owner
            .providers
            .get(provider)
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: format!("provider {provider} is not configured"),
            })?;
        let family = slot.entry.family;
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ProviderError::Transport {
                family,
                reason: String::from("provider request cancelled"),
            }),
            permit = Arc::clone(&slot.permits).acquire_owned() => permit
                .map_err(|_| ProviderError::Transport {
                    family,
                    reason: String::from("provider request admission is closed"),
                }),
        }
    }
}

pub(crate) fn hold_permit(stream: EventStream, permit: OwnedSemaphorePermit) -> EventStream {
    let source = futures::stream::unfold(
        (stream, Some(permit)),
        |(mut stream, mut permit)| async move {
            match stream.next().await {
                Some(Ok(event @ StreamEvent::Stop { .. })) => {
                    drop(permit.take());
                    Some((Ok(event), (stream, permit)))
                }
                Some(item) => Some((item, (stream, permit))),
                None => None,
            }
        },
    );
    EventStream::new(source, || {})
}

pub(crate) async fn refresh_expiring(
    refresher: &Refresher,
    provider: &str,
    family: Family,
    credential: &Credential,
    cancel: &CancellationToken,
) -> Result<Credential, ProviderError> {
    let Credential::OAuth(held) = credential else {
        return Ok(credential.clone());
    };
    let Some(provider_kind) = OAuthProvider::from_id(provider) else {
        return Ok(credential.clone());
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProviderError::Transport {
            family,
            reason: String::from("provider request cancelled while refreshing credentials"),
        }),
        credential = refresher.refresh(provider_kind, held, RefreshReason::Expiring) => credential,
    }
}
pub(crate) async fn refresh_credential(
    refresher: Arc<Refresher>,
    provider: Box<str>,
    held: OAuthCredential,
) -> Result<Credential, ProviderError> {
    {
        let Some(provider_kind) = OAuthProvider::from_id(&provider) else {
            return Err(ProviderError::SignInExpired {
                provider: provider.to_string(),
            });
        };
        refresher
            .refresh(provider_kind, &held, RefreshReason::Rejected)
            .await
    }
}
