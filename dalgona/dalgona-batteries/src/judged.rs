// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Judged battery: judge-fed reminders deduplicated through one admission path.

mod anchor;
mod claim;
mod config;
mod dedup;
mod docs;
mod rerank;
mod state;
mod thinking;

pub use config::{JudgedConfig, JudgedConfigError};
pub use dedup::Admission;
pub use docs::JUDGED_DOC;
pub use rerank::JudgedRerank;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dal_agent::ext::{
    BoxFuture, Extension, ExtensionBuilder, Hook, HookCx, HookError, ObserveHook, SessionCx,
    SessionInstalls, SessionSetup, StreamWatch, TurnInfo, WatchFactory,
};
use dal_core::ext::{BeforeRequest, BeforeTurn, InputVerdict, ToolCallVerdict};
use dal_core::{
    Channel, InputEvent, Origin, Part, RegistrationError, RequestParams, ServiceSet, SessionEnd,
    SessionStart, Settled, StreamVerdict, ToolCallEvent, ToolResultEvent, TurnId,
};
use dal_ext::judge::{Judge, JudgeConfig, JudgeOpen};

use self::state::SessionState;

/// The compiled judged extension and the reranker installed into search tools.
pub struct JudgedParts {
    /// The hooks and stream watcher registered with dal's extension runtime.
    pub extension: Extension,
    /// The callback to install in [`dal_tools::ToolsConfig`].
    pub rerank: Arc<JudgedRerank>,
    /// The shared battery handle: other dalgona batteries that inject reminders
    /// call [`Battery::admit`] on it so the dedup window covers every reminder.
    pub battery: Arc<Battery>,
}

impl std::fmt::Debug for JudgedParts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JudgedParts")
            .field("rerank", &self.rerank)
            .finish_non_exhaustive()
    }
}

/// Shared behavior and bounded state for the judged battery.
pub struct Battery {
    pub(super) cfg: JudgedConfig,
    sessions: Mutex<HashMap<dal_core::SessionId, SessionState>>,
}

impl Battery {
    /// Creates an empty per-session state map for one judged battery instance.
    #[must_use]
    pub fn new(cfg: JudgedConfig) -> Self {
        Self {
            cfg,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn with_session<R>(
        &self,
        session: dal_core::SessionId,
        operation: impl FnOnce(&mut SessionState) -> R,
    ) -> R {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(sessions.entry(session).or_default())
    }

    fn remove_session(&self, session: dal_core::SessionId) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session);
    }

    /// Returns the stored judge handle, if the session has opened one.
    pub fn judge(&self, cx: &HookCx) -> Option<Judge> {
        self.with_session(cx.session, |state| state.judge.clone())
    }

    /// Returns the stored judge handle for search's session-scoped reranker.
    pub fn judge_session(&self, session: dal_core::SessionId) -> Option<Judge> {
        self.with_session(session, |state| state.judge.clone())
    }

    /// Opens or retrieves the session's one dal judge handle.
    pub async fn ensure_judge(&self, cx: &HookCx) -> Option<Judge> {
        if let Some(judge) = self.judge(cx) {
            return Some(judge);
        }
        let (config, session_route, session_model_id) = self.with_session(cx.session, |state| {
            (
                state.judge_config.clone(),
                state.session_route.clone(),
                state.session_model_id.clone(),
            )
        });
        let open = JudgeOpen {
            session: cx.session,
            config,
            services: Arc::clone(&cx.services),
            caller: cx.caller.clone(),
            session_route,
            session_model_id,
        };
        let Ok(judge) = Judge::open(open).await else {
            return None;
        };
        Some(self.with_session(cx.session, |state| {
            if let Some(existing) = &state.judge {
                return existing.clone();
            }
            state.judge = Some(judge.clone());
            judge
        }))
    }

    /// Records the latest text-bearing user input observed by the public hook.
    pub fn observe_user(&self, session: dal_core::SessionId, content: &[Part]) {
        self.with_session(session, |state| state.digest.observe_user(content));
    }

    /// Records one bounded tool-result preview in recent context.
    pub fn observe_preview(&self, session: dal_core::SessionId, preview: &str) {
        self.with_session(session, |state| state.digest.observe_preview(preview));
    }

    /// Adds one assistant text delta to the bounded rolling digest.
    pub fn observe_assistant_delta(&self, session: dal_core::SessionId, delta: &str) {
        self.with_session(session, |state| {
            state.digest.observe_assistant_delta(delta);
        });
    }

    /// Starts the assistant digest of `turn`; later requests of the same turn
    /// extend it, so a settled turn's text covers every tool round.
    pub fn begin_assistant_turn(&self, session: dal_core::SessionId, turn: TurnId) {
        self.with_session(session, |state| state.digest.begin_assistant_turn(turn));
    }
}

impl std::fmt::Debug for Battery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Battery").finish_non_exhaustive()
    }
}

/// Captures the session's judge identity at session setup: the merged
/// `[judge]` configuration, the session's model route, and its resolved
/// model id. The host runs setup before it observes the session, so every
/// later `ensure_judge` opens against the session's real model instead of a
/// missing identity.
struct SessionIdentity {
    battery: Arc<Battery>,
}

impl SessionSetup for SessionIdentity {
    fn open(&self, cx: SessionCx) -> BoxFuture<'static, Result<SessionInstalls, HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move {
            let config =
                JudgeConfig::from_config(&cx.config).map_err(|error| HookError::Failed {
                    message: format!("dal.toml: {error}").into(),
                })?;
            battery.with_session(cx.session, |state| {
                state.judge_config = config;
                state.session_route.clone_from(&cx.model);
                state.session_model_id = cx.model_id.clone();
            });
            Ok(SessionInstalls::default())
        })
    }
}

struct DigestWatcherFactory(Arc<Battery>);

impl WatchFactory for DigestWatcherFactory {
    fn start(&self, turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        self.0.begin_assistant_turn(turn.session, turn.turn);
        Some(Box::new(DigestWatch {
            battery: Arc::clone(&self.0),
            session: turn.session,
        }))
    }
}

struct DigestWatch {
    battery: Arc<Battery>,
    session: dal_core::SessionId,
}

impl StreamWatch for DigestWatch {
    fn feed(&mut self, channel: Channel, delta: &str) -> StreamVerdict {
        if matches!(channel, Channel::Text) {
            self.battery.observe_assistant_delta(self.session, delta);
        }
        StreamVerdict::Continue
    }

    fn finish(&mut self) -> StreamVerdict {
        StreamVerdict::Continue
    }
}

/// Builds the one compiled battery registration and its search reranker.
///
/// # Errors
/// Returns the extension runtime's registration error if the battery's fixed
/// declaration cannot be registered.
pub fn judged(cfg: JudgedConfig) -> Result<JudgedParts, RegistrationError> {
    let battery = Arc::new(Battery::new(cfg));
    let rerank = Arc::new(JudgedRerank::new(Arc::clone(&battery)));

    let extension = ExtensionBuilder::new("judged", "0.1.0", ServiceSet::EMPTY)?
        .with_origin(Origin::Bundled, None)
        .session_setup(SessionIdentity {
            battery: Arc::clone(&battery),
        })
        .on_session_start_lossless(SessionStartHook {
            battery: Arc::clone(&battery),
        })
        .on_session_end_lossless(SessionEndHook {
            battery: Arc::clone(&battery),
        })
        .on_input(InputHook {
            battery: Arc::clone(&battery),
        })
        .on_before_request(BeforeRequestHook {
            battery: Arc::clone(&battery),
        })
        .on_tool_call(ToolCallHook {
            battery: Arc::clone(&battery),
        })
        .on_tool_result(ToolResultHook {
            battery: Arc::clone(&battery),
        })
        .on_before_turn(BeforeTurnHook {
            battery: Arc::clone(&battery),
        })
        .on_settled_lossless(SettledHook {
            battery: Arc::clone(&battery),
        })
        .output_stream(Arc::new(DigestWatcherFactory(Arc::clone(&battery))))
        .build()?;

    Ok(JudgedParts {
        extension,
        rerank,
        battery,
    })
}

struct SessionStartHook {
    battery: Arc<Battery>,
}

impl ObserveHook<SessionStart> for SessionStartHook {
    fn call(&self, _event: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move {
            battery.with_session(cx.session, |state| {
                state.is_child = cx.parent.is_some();
            });
            Ok(())
        })
    }
}

struct SessionEndHook {
    battery: Arc<Battery>,
}

impl ObserveHook<SessionEnd> for SessionEndHook {
    fn call(&self, _event: SessionEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move {
            battery.remove_session(cx.session);
            Ok(())
        })
    }
}

struct InputHook {
    battery: Arc<Battery>,
}

impl Hook<InputEvent, InputVerdict> for InputHook {
    fn call(
        &self,
        event: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move {
            battery.observe_user(cx.session, &event.content);
            Ok(InputVerdict::Continue)
        })
    }
}

struct BeforeRequestHook {
    battery: Arc<Battery>,
}

impl Hook<BeforeRequest, Option<RequestParams>> for BeforeRequestHook {
    fn call(
        &self,
        event: BeforeRequest,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<RequestParams>, HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move { Ok(thinking::classify(&battery, &cx, &event).await) })
    }
}

struct ToolCallHook {
    battery: Arc<Battery>,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for ToolCallHook {
    fn call(
        &self,
        event: ToolCallEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move { Ok(anchor::classify_tool_call(&battery, &cx, &event).await) })
    }
}

struct ToolResultHook {
    battery: Arc<Battery>,
}

impl ObserveHook<ToolResultEvent> for ToolResultHook {
    fn call(
        &self,
        event: ToolResultEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move {
            battery.observe_preview(cx.session, &event.preview);
            Ok(())
        })
    }
}

struct BeforeTurnHook {
    battery: Arc<Battery>,
}

impl Hook<BeforeTurn, Option<String>> for BeforeTurnHook {
    fn call(
        &self,
        event: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move { Ok(claim::on_before_turn(&battery, &cx, &event).await) })
    }
}

struct SettledHook {
    battery: Arc<Battery>,
}

impl ObserveHook<Settled> for SettledHook {
    fn call(&self, event: Settled, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let battery = Arc::clone(&self.battery);
        Box::pin(async move {
            claim::on_settled(&battery, &cx, &event).await;
            Ok(())
        })
    }
}
