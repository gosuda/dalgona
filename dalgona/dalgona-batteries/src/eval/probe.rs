// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The extension an eval run adds to its product: it opens the judge for a
//! session on request and refuses tool calls outside a case's tool list.
//!
//! The extension is added by `dalgona eval` to the hosts it starts; it is not
//! one of the product's batteries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dal_agent::ext::{
    BoxFuture, Caller, Extension, ExtensionBuilder, Hook, HookCx, HookError, ObserveHook, Services,
    SessionCx, SessionInstalls, SessionSetup,
};
use dal_core::ext::{SessionEnd, ToolCallEvent, ToolCallVerdict};
use dal_core::{ModelRoute, Origin, RegistrationError, ServiceSet, SessionId, TurnId};
use dal_ext::judge::{Gate, GateSetting, Judge, JudgeConfig, JudgeOpen, JudgeQuestion, Verdict};

use super::pack::ToolCall;
use super::run::{JudgeResult, JudgeSkip};
use crate::text::byte_prefix;

/// The registration name of the probe extension.
const EXTENSION_NAME: &str = "eval-run";

/// Whether the judge can score a session.
pub enum JudgeAccess {
    /// The gate resolved on; the handle answers questions.
    Ready(Verdicts),
    /// The gate did not resolve on.
    Skip(JudgeSkip),
}

/// What the judge sees of one finished run.
#[derive(Clone, Copy, Debug)]
pub struct Transcript<'a> {
    /// The prompt the session ran.
    pub prompt: &'a str,
    /// The assistant text.
    pub text: &'a str,
    /// Every tool call, in order.
    pub tool_calls: &'a [ToolCall],
}

/// A ready judge handle that scores runs with bool questions.
pub struct Verdicts(Judge);

/// The judge feature label of eval verdicts.
const JUDGE_FEATURE: &str = "eval";
/// The judge's shared-context limit is 16384 bytes; this leaves room for headings.
const SHARED_BUDGET: usize = 15_000;
/// The longest tool-call argument text the judge sees, in bytes.
const ARGS_BUDGET: usize = 500;

fn shared_context(transcript: &Transcript<'_>) -> String {
    let mut shared = format!(
        "Task prompt:\n{}\n\n",
        byte_prefix(transcript.prompt, 4_000)
    );
    shared.push_str("Assistant text:\n");
    shared.push_str(byte_prefix(transcript.text, 6_000));
    shared.push_str("\n\nTool calls:\n");
    for call in transcript.tool_calls {
        let line = format!("{} {}\n", call.name, byte_prefix(&call.args, ARGS_BUDGET));
        if shared.len() + line.len() > SHARED_BUDGET {
            break;
        }
        shared.push_str(&line);
    }
    shared
}

impl Verdicts {
    /// Asks the judge one yes/no question about a run. A failed call is
    /// reported as [`JudgeResult::Error`], never raised.
    pub async fn ask(
        &self,
        question: &str,
        transcript: &Transcript<'_>,
        turn: TurnId,
    ) -> JudgeResult {
        let question = match JudgeQuestion::bool(question) {
            Ok(question) => question,
            Err(error) => return JudgeResult::Error(error.to_string().into()),
        };
        let shared = shared_context(transcript);
        match self
            .0
            .judge(JUDGE_FEATURE, &shared, question, Some(turn), None)
            .await
        {
            Ok(Verdict::Bool(verdict)) => JudgeResult::Verdict(verdict),
            Ok(_) => JudgeResult::Error("the judge answered with a non-bool verdict".into()),
            Err(error) => JudgeResult::Error(error.to_string().into()),
        }
    }
}

/// What the judge needs to open, captured at session setup.
#[derive(Clone)]
struct JudgeParts {
    config: JudgeConfig,
    services: Arc<dyn Services>,
    caller: Caller,
    route: Option<ModelRoute>,
    model_id: Box<str>,
}

#[derive(Default)]
struct SessionState {
    judge: Option<JudgeParts>,
    tools: Option<Vec<String>>,
}

/// The per-session state shared between an executor and the probe's hooks.
#[derive(Default)]
pub struct Probe {
    sessions: Mutex<HashMap<SessionId, SessionState>>,
}

impl Probe {
    /// Creates an empty probe.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, SessionState>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Builds the extension to add to the product before the host starts.
    ///
    /// # Errors
    /// Returns the registration error for the fixed declarations.
    pub fn extension(self: &Arc<Self>) -> Result<Extension, RegistrationError> {
        ExtensionBuilder::new(EXTENSION_NAME, env!("CARGO_PKG_VERSION"), ServiceSet::EMPTY)?
            .with_origin(Origin::Bundled, None)
            .session_setup(Setup {
                probe: Arc::clone(self),
            })
            .on_tool_call(ToolGate {
                probe: Arc::clone(self),
            })
            .on_session_end_lossless(Forget {
                probe: Arc::clone(self),
            })
            .build()
    }

    /// Restricts `session` to the named tools; `None` lifts the restriction.
    pub fn allow_tools(&self, session: SessionId, tools: Option<&[String]>) {
        self.lock().entry(session).or_default().tools = tools.map(<[String]>::to_vec);
    }

    fn verdict(&self, session: SessionId, tool: &str) -> ToolCallVerdict {
        let sessions = self.lock();
        let Some(allowed) = sessions
            .get(&session)
            .and_then(|state| state.tools.as_ref())
        else {
            return ToolCallVerdict::Allow;
        };
        if allowed.iter().any(|name| name == tool) {
            return ToolCallVerdict::Allow;
        }
        let list = if allowed.is_empty() {
            "none".to_owned()
        } else {
            allowed.join(", ")
        };
        ToolCallVerdict::Block {
            reason: format!("{tool} is not in this eval case's tool list ({list})").into(),
        }
    }

    /// Resolves the judge gate for `session`.
    ///
    /// A gate configured `off` skips with [`JudgeSkip::Off`]. Any other gate
    /// that does not resolve to a ready judge skips with
    /// [`JudgeSkip::Unresolved`].
    pub async fn judge(&self, session: SessionId) -> JudgeAccess {
        let Some(parts) = self
            .lock()
            .get(&session)
            .and_then(|state| state.judge.clone())
        else {
            return JudgeAccess::Skip(JudgeSkip::Unresolved);
        };
        let gate = parts.config.gate;
        let open = JudgeOpen {
            session,
            config: parts.config,
            services: parts.services,
            caller: parts.caller,
            session_route: parts.route,
            session_model_id: parts.model_id,
        };
        match Judge::open(open).await {
            Ok(judge) if matches!(judge.state(), Gate::Ready { .. }) => {
                JudgeAccess::Ready(Verdicts(judge))
            }
            Ok(_) | Err(_) if gate == GateSetting::Off => JudgeAccess::Skip(JudgeSkip::Off),
            Ok(_) | Err(_) => JudgeAccess::Skip(JudgeSkip::Unresolved),
        }
    }
}

struct Setup {
    probe: Arc<Probe>,
}

impl SessionSetup for Setup {
    fn open(&self, cx: SessionCx) -> BoxFuture<'static, Result<SessionInstalls, HookError>> {
        let probe = Arc::clone(&self.probe);
        Box::pin(async move {
            let config =
                JudgeConfig::from_config(&cx.config).map_err(|error| HookError::Failed {
                    message: format!("dal.toml: {error}").into(),
                })?;
            probe.lock().entry(cx.session).or_default().judge = Some(JudgeParts {
                config,
                services: Arc::clone(&cx.services),
                caller: cx.caller.clone(),
                route: cx.model.clone(),
                model_id: cx.model_id.clone(),
            });
            Ok(SessionInstalls::default())
        })
    }
}

struct ToolGate {
    probe: Arc<Probe>,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for ToolGate {
    fn call(
        &self,
        event: ToolCallEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let verdict = self.probe.verdict(cx.session, event.tool.as_str());
        Box::pin(async move { Ok(verdict) })
    }
}

struct Forget {
    probe: Arc<Probe>,
}

impl ObserveHook<SessionEnd> for Forget {
    fn call(&self, _event: SessionEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.probe.lock().remove(&cx.session);
        Box::pin(async { Ok(()) })
    }
}
