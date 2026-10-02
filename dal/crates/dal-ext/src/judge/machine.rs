use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::Duration;

use dal_agent::error::ServiceError;
use dal_agent::ext::{Caller, Services};
use dal_core::{
    ContextItem, Inference, ModelRequest, ModelRoute, ModelToolSpec, Notice, Part, Purpose,
    RequestParams, SessionId, StreamChannel, StreamEvent, TurnId,
};
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout_at};

use super::config::{GateSetting, JudgeConfig};
use super::format::{
    SUMMARY_SYSTEM_LINE, SYSTEM_LINE, parse_answers, render_envelope, render_summary, reply_detail,
};
use super::types::{Gate, JudgeError, JudgeQuestion, Verdict};
use super::{BATCH_MAX, SHARED_MAX, STREAK_NOTICE_PREFIX};
mod settle;
use settle::CallSettlement;

/// Inputs used to open the session's resolved judge handle.
pub struct JudgeOpen {
    /// The session identity used to share one live judge handle.
    pub session: SessionId,
    /// The validated judge configuration.
    pub config: JudgeConfig,
    /// Session-scoped extension services.
    pub services: Arc<dyn Services>,
    /// Host-minted authority of the opening consumer.
    pub caller: Caller,
    /// The session route used when no judge model override is configured.
    pub session_route: ModelRoute,
    /// The session's resolved model id.
    pub session_model_id: Box<str>,
}

impl fmt::Debug for JudgeOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JudgeOpen")
            .field("session", &self.session)
            .field("config", &self.config)
            .field("caller", &"<host-minted>")
            .field("session_route", &self.session_route)
            .field("session_model_id", &self.session_model_id)
            .finish_non_exhaustive()
    }
}

/// A cloneable handle to one session's bounded judge service.
#[must_use]
#[derive(Clone)]
pub struct Judge {
    inner: Arc<JudgeInner>,
}

impl fmt::Debug for Judge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Judge")
            .field("gate", &self.inner.gate)
            .finish_non_exhaustive()
    }
}

struct JudgeInner {
    config: JudgeConfig,
    gate: Gate,
    model: ModelRoute,
    model_id: Box<str>,
    services: Arc<dyn Services>,
    caller: Caller,
    permits: Semaphore,
    calls: Mutex<CallState>,
}

struct CallState {
    next_call: u64,
    window: Option<(Option<TurnId>, u32)>,
    failure_streak: u8,
}

impl Default for CallState {
    fn default() -> Self {
        Self {
            next_call: 1,
            window: None,
            failure_streak: 0,
        }
    }
}

impl Judge {
    /// Opens or retrieves the live judge for one session.
    ///
    /// The purpose-scoped inference probe resolves the model through the host;
    /// it does not make a provider request.
    ///
    /// # Errors
    /// Returns [`JudgeError::Unavailable`] when an explicitly enabled role has
    /// no usable credentials or model. Other probe failures return
    /// [`JudgeError::Provider`] so the caller can retry opening the handle.
    pub async fn open(open: JudgeOpen) -> Result<Self, JudgeError> {
        if let Some(inner) = lock(judge_sessions())
            .get(&open.session)
            .and_then(Weak::upgrade)
        {
            return Ok(Self { inner });
        }

        let JudgeOpen {
            session,
            config,
            services,
            caller,
            session_route,
            session_model_id: _,
        } = open;
        let model = if config.model.is_empty() {
            session_route
        } else {
            ModelRoute::from_id(&config.model)
        };
        let probe = ModelRequest {
            purpose: Purpose::Judge,
            model: model.clone(),
            system: Arc::from(""),
            tools: Arc::from(Vec::<ModelToolSpec>::new()),
            context: Arc::from(Vec::<ContextItem>::new()),
            params: RequestParams::default(),
            cache_key: None,
        };
        let probe_result = services.infer(&caller, probe).await;
        let (gate, model_id) = match probe_result {
            Ok(_) if config.gate == GateSetting::Off => (Gate::Off, Box::from("")),
            Ok(inference) => {
                let model_id = Box::<str>::from(inference_text(inference).trim());
                (
                    Gate::Ready {
                        model_id: model_id.clone(),
                    },
                    model_id,
                )
            }
            Err(ServiceError::Failed { message, .. })
                if message.as_ref() == "judge disabled by gate" =>
            {
                (Gate::Off, Box::from(""))
            }
            Err(ServiceError::Failed { message, .. }) if resolution_unavailable(&message) => {
                if config.gate == GateSetting::On {
                    tracing::warn!(reason = %message, "judge role is unavailable");
                    return Err(JudgeError::Unavailable);
                }
                tracing::debug!(reason = %message, "judge role resolved off");
                (Gate::Off, Box::from(""))
            }
            Err(error) => return Err(provider_error(&error)),
        };

        let inner = Arc::new(JudgeInner {
            // `u32` to `usize` is a same-signedness widening; `max_concurrent` is 1..=32.
            permits: Semaphore::new(config.max_concurrent as usize),
            config,
            gate,
            model,
            model_id,
            services,
            caller,
            calls: Mutex::new(CallState::default()),
        });
        let mut sessions = lock(judge_sessions());
        if let Some(existing) = sessions.get(&session).and_then(Weak::upgrade) {
            return Ok(Self { inner: existing });
        }
        sessions.retain(|_, entry| entry.strong_count() != 0);
        sessions.insert(session, Arc::downgrade(&inner));
        Ok(Self { inner })
    }

    /// Returns the gate resolved when this session handle opened.
    #[must_use]
    pub fn state(&self) -> Gate {
        self.inner.gate.clone()
    }

    /// Judges one typed question.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::judge_batch`] and returns
    /// [`JudgeError::Parse`] if a successful batch ever violates the one-answer
    /// invariant (unreachable: `parse_answers` enforces one answer per question).
    pub async fn judge(
        &self,
        feature: &str,
        shared: &str,
        question: JudgeQuestion,
        turn: Option<TurnId>,
        deadline_ms: Option<u64>,
    ) -> Result<Verdict, JudgeError> {
        let mut verdicts = self
            .judge_batch(feature, shared, vec![question], turn, deadline_ms)
            .await?;
        match verdicts.pop() {
            Some(verdict) if verdicts.is_empty() => Ok(verdict),
            _ => Err(JudgeError::Parse {
                detail: Box::from("single-question response did not contain one verdict"),
            }),
        }
    }

    /// Judges a non-empty batch with one provider request.
    ///
    /// # Errors
    /// Returns [`JudgeError::InvalidQuestion`], [`JudgeError::SharedTooLarge`],
    /// [`JudgeError::Unavailable`], [`JudgeError::BudgetExhausted`],
    /// [`JudgeError::Timeout`], [`JudgeError::Parse`], or
    /// [`JudgeError::Provider`] for the corresponding validation, gate,
    /// admission, deadline, response, or service failure.
    pub async fn judge_batch(
        &self,
        feature: &str,
        shared: &str,
        questions: Vec<JudgeQuestion>,
        turn: Option<TurnId>,
        deadline_ms: Option<u64>,
    ) -> Result<Vec<Verdict>, JudgeError> {
        for question in &questions {
            question.validate()?;
        }
        validate_shared(shared)?;
        if questions.len() > BATCH_MAX {
            return Err(JudgeError::InvalidQuestion {
                reason: Box::from(format!("a batch carries at most {BATCH_MAX} questions")),
            });
        }
        if questions.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_ready()?;

        let question_count = questions.len();
        let questions = Arc::new(questions);
        let render_questions = Arc::clone(&questions);
        self.call(
            feature,
            question_count,
            turn,
            deadline_ms,
            SYSTEM_LINE,
            move || render_envelope(shared, render_questions.as_slice()),
            move |reply| parse_answers(reply, questions.as_slice()),
        )
        .await
    }

    /// Returns a free-text summary through the same session judge role.
    ///
    /// # Errors
    /// Returns the gate, shared-context, budget, deadline, and service errors
    /// of a normal judge call. No typed verdict is used for summary text.
    pub async fn summarize(&self, shared: &str, prompt: &str) -> Result<String, JudgeError> {
        validate_shared(shared)?;
        self.ensure_ready()?;
        self.call(
            "history",
            1,
            None,
            None,
            SUMMARY_SYSTEM_LINE,
            || render_summary(shared, prompt),
            |reply| Ok(reply.to_owned()),
        )
        .await
    }

    /// Formats the load-time notice for judged rules when the gate is off.
    #[must_use]
    pub fn inert_rules_notice(count: usize) -> Box<str> {
        format!("judge: off; {count} judged rule(s) are inert").into_boxed_str()
    }

    fn ensure_ready(&self) -> Result<(), JudgeError> {
        if matches!(&self.inner.gate, Gate::Off) {
            return Err(JudgeError::Unavailable);
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one judge call carries the request shape end to end"
    )]
    async fn call<T, Render, Parse>(
        &self,
        feature: &str,
        questions: usize,
        turn: Option<TurnId>,
        deadline_ms: Option<u64>,
        system: &'static str,
        render: Render,
        parse: Parse,
    ) -> Result<T, JudgeError>
    where
        T: Send,
        Render: FnOnce() -> String + Send,
        Parse: FnOnce(&str) -> Result<T, JudgeError> + Send,
    {
        let (call, exhausted) = self.reserve_call(turn);
        let mut settlement =
            CallSettlement::new(Arc::clone(&self.inner), call, turn, feature, questions);
        if exhausted {
            let error = JudgeError::BudgetExhausted {
                max_per_turn: self.inner.config.max_per_turn,
            };
            let row = settlement.row("budget", &error.to_string(), 0, 0, 0);
            settlement.append(row).await?;
            return Err(error);
        }

        let timeout_ms = deadline_ms.map_or(self.inner.config.timeout_ms, |limit| {
            limit.min(self.inner.config.timeout_ms)
        });
        let start = Instant::now();
        let deadline = start + Duration::from_millis(timeout_ms);
        settlement.start_slot_wait();
        let slot = timeout_at(deadline, self.inner.permits.acquire()).await;
        settlement.finish_slot_wait();
        let (outcome, input_tokens, output_tokens) = match slot {
            Err(_) => (Err(JudgeError::Timeout { ms: timeout_ms }), 0, 0),
            Ok(Err(_)) => (
                // The semaphore is never closed; its failure is an internal
                // invariant violation, not a provider outcome.
                Err(JudgeError::Provider {
                    message: Box::from("judge admission semaphore closed"),
                }),
                0,
                0,
            ),
            Ok(Ok(permit)) => {
                let body = render();
                let request = ModelRequest {
                    purpose: Purpose::Judge,
                    model: self.inner.model.clone(),
                    system: Arc::from(system),
                    tools: Arc::from(Vec::<ModelToolSpec>::new()),
                    context: Arc::from(vec![ContextItem::User {
                        parts: vec![Part::Text { text: body.into() }],
                    }]),
                    params: RequestParams::default(),
                    cache_key: None,
                };
                let inference = timeout_at(
                    deadline,
                    self.inner.services.infer(&self.inner.caller, request),
                )
                .await;
                drop(permit);
                match inference {
                    Err(_) => (Err(JudgeError::Timeout { ms: timeout_ms }), 0, 0),
                    Ok(Err(error)) => (Err(provider_error(&error)), 0, 0),
                    Ok(Ok(inference)) => {
                        let (response, input_tokens, output_tokens) = split_inference(inference);
                        (parse(&response), input_tokens, output_tokens)
                    }
                }
            }
        };

        let status = outcome
            .as_ref()
            .map_or_else(|error| error_status(error), |_| "ok");
        let cause = outcome.as_ref().err().map(ledger_cause).unwrap_or_default();
        let row = settlement.row(
            status,
            &cause,
            elapsed_ms(start.elapsed()),
            input_tokens,
            output_tokens,
        );
        let append_result = settlement.append(row).await;
        match outcome {
            Ok(value) => match append_result {
                Ok(()) => {
                    self.reset_failure_streak();
                    Ok(value)
                }
                Err(error) => {
                    self.note_failure(&error, turn);
                    Err(error)
                }
            },
            Err(error) => {
                self.note_failure(&error, turn);
                append_result?;
                Err(error)
            }
        }
    }

    fn reserve_call(&self, turn: Option<TurnId>) -> (u64, bool) {
        let mut calls = lock(&self.inner.calls);
        let call = calls.next_call;
        calls.next_call = calls.next_call.saturating_add(1);
        let same_window = matches!(
            calls.window.as_ref(),
            Some((window_turn, _)) if window_turn == &turn
        );
        if !same_window {
            calls.window = Some((turn, 0));
        }
        let Some((_, count)) = calls.window.as_mut() else {
            return (call, true);
        };
        if *count >= self.inner.config.max_per_turn {
            return (call, true);
        }
        *count += 1;
        (call, false)
    }

    fn reset_failure_streak(&self) {
        lock(&self.inner.calls).failure_streak = 0;
    }

    fn note_failure(&self, error: &JudgeError, turn: Option<TurnId>) {
        if !matches!(
            error,
            JudgeError::Timeout { .. } | JudgeError::Parse { .. } | JudgeError::Provider { .. }
        ) {
            return;
        }
        let notify = {
            let mut calls = lock(&self.inner.calls);
            calls.failure_streak = calls.failure_streak.saturating_add(1);
            calls.failure_streak == 3
        };
        if notify {
            // Plan 4538: streak notice carries the display string capped at 200
            // bytes (transient status); the journal row stays redacted via
            // `ledger_cause`.
            let cause = reply_detail(&error.to_string());
            let text = format!("{STREAK_NOTICE_PREFIX}{cause}); judged answers are being skipped")
                .into_boxed_str();
            self.inner.services.notify(
                &self.inner.caller,
                Notice {
                    turn,
                    kind: Box::from("judge"),
                    text,
                },
            );
        }
    }
}

fn split_inference(inference: Inference) -> (String, u64, u64) {
    let mut text = String::new();
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    for event in inference.events {
        match event {
            StreamEvent::Delta {
                channel: StreamChannel::Text,
                text: delta,
            } => text.push_str(&delta),
            StreamEvent::Usage(usage) => {
                input_tokens = usage.input_tokens;
                output_tokens = usage.output_tokens;
            }
            _ => {}
        }
    }
    (text, input_tokens, output_tokens)
}

fn inference_text(inference: Inference) -> String {
    split_inference(inference).0
}

fn provider_error(error: &ServiceError) -> JudgeError {
    JudgeError::Provider {
        message: reply_detail(&error.to_string()),
    }
}

/// Redacted ledger and notice cause: never carries reply bytes, so a model
/// echo cannot persist shared input, prompts, or options in the journal.
fn ledger_cause(error: &JudgeError) -> String {
    match error {
        JudgeError::Parse { .. } => String::from("judge parse error"),
        _ => error.to_string(),
    }
}

fn error_status(error: &JudgeError) -> &'static str {
    match error {
        JudgeError::Timeout { .. } => "timeout",
        JudgeError::Parse { .. } => "parse",
        _ => "provider",
    }
}

fn resolution_unavailable(message: &str) -> bool {
    message == super::REASON_NO_CREDENTIALS || message.starts_with("unknown judge model \"")
}

fn validate_shared(shared: &str) -> Result<(), JudgeError> {
    if shared.len() > SHARED_MAX {
        return Err(JudgeError::SharedTooLarge { len: shared.len() });
    }
    Ok(())
}

fn elapsed_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

static JUDGE_SESSIONS: LazyLock<Mutex<HashMap<SessionId, Weak<JudgeInner>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn judge_sessions() -> &'static Mutex<HashMap<SessionId, Weak<JudgeInner>>> {
    &JUDGE_SESSIONS
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
