// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The `history` battery constructor and session hooks.
//!
//! `history()` registers the `history` compactor with the `jobs` and `sidecar`
//! services, captures the session judge identity during setup, and installs
//! the `session_start`, `session_end`, first-prompt `input`, and observe-only
//! `settled` hooks. A disabled config builds an empty extension; an invalid
//! config installs the failing compactor so startup continues and every
//! compaction surfaces the exact configuration warning.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, Extension, ExtensionBuilder, Hook, HookCx, HookError, ObserveHook, SessionCx,
    SessionInstalls, SessionSetup,
};
use dal_core::{
    InputEvent, InputVerdict, ModelRoute, Origin, RegistrationError, ServiceSet, SessionEnd,
    SessionId, SessionStart, Settled,
};
use dal_ext::judge::JudgeConfig;

use super::compact::{FailingCompactor, HistoryCompactor};
use super::config::HistoryConfig;
use super::task::{self, TaskMsg};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout_at};

/// One live dream session: the inbox of its session-owned task plus the task.
pub(crate) struct SessionHandle {
    tx: mpsc::Sender<TaskMsg>,
    task: tokio_util::task::AbortOnDropHandle<()>,
}

/// The host-resolved judge inputs retained before lifecycle hooks run.
#[derive(Clone)]
struct JudgeIdentity {
    config: JudgeConfig,
    route: Option<ModelRoute>,
    model_id: Box<str>,
}

/// Per-session dream tasks for one `history` battery instance.
pub(crate) struct Shared {
    sessions: Mutex<HashMap<SessionId, SessionHandle>>,
    identities: Mutex<HashMap<SessionId, JudgeIdentity>>,
}

impl Shared {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            identities: Mutex::new(HashMap::new()),
        }
    }

    fn set_identity(&self, session: SessionId, identity: JudgeIdentity) {
        self.identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, identity);
    }

    fn take_identity(&self, session: SessionId) -> Option<JudgeIdentity> {
        self.identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session)
    }

    fn send(&self, session: SessionId, message: TaskMsg) {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handle) = sessions.get(&session) {
            let _ = handle.tx.try_send(message);
        }
    }

    fn remove(&self, session: SessionId) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handle) = sessions.remove(&session) {
            handle.task.abort();
        }
    }

    async fn end_session(&self, session: SessionId) {
        let handle = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session);
        let Some(handle) = handle else {
            return;
        };
        let deadline = Instant::now() + SESSION_END_GRACE;
        let _ = timeout_at(deadline, handle.tx.send(TaskMsg::End)).await;
        let _ = timeout_at(deadline, handle.tx.closed()).await;
        if !handle.task.is_finished() {
            handle.task.abort();
        }
    }
}

/// Session-end delivery bound: `End` is queued with backpressure and the
/// task is given until this deadline to drain and exit before the abort
/// fallback fires, so session close never hangs on a wedged task.
const SESSION_END_GRACE: Duration = Duration::from_secs(2);

/// Builds the `history` extension from a decoded `[plugin.history]` section.
///
/// # Errors
/// Returns `RegistrationError` for an unknown service name or a duplicate
/// compactor registration; the service list is fixed so this cannot happen.
pub fn history(config: HistoryConfig) -> Result<Extension, RegistrationError> {
    let inject = ServiceSet::from_names(["jobs", "sidecar"])?;
    let builder =
        ExtensionBuilder::new("history", "0.1.0", inject)?.with_origin(Origin::Bundled, None);
    match config {
        HistoryConfig::Disabled => builder.build(),
        HistoryConfig::Invalid { message } => builder
            .compactor("history", Arc::new(FailingCompactor::new(message)))
            .build(),
        HistoryConfig::Enabled { share } => {
            let shared = Arc::new(Shared::new());
            builder
                .session_setup(SessionIdentity {
                    shared: Arc::clone(&shared),
                })
                .compactor("history", Arc::new(HistoryCompactor::new(share)))
                .on_session_start(SessionStartHook {
                    shared: Arc::clone(&shared),
                })
                .on_session_end_lossless(SessionEndHook {
                    shared: Arc::clone(&shared),
                })
                .on_input(InputHook {
                    shared: Arc::clone(&shared),
                })
                .on_settled(SettledHook {
                    shared: Arc::clone(&shared),
                })
                .build()
        }
    }
}

/// Captures configured judge inputs before the history session hook starts.
struct SessionIdentity {
    shared: Arc<Shared>,
}

impl SessionSetup for SessionIdentity {
    fn open(&self, cx: SessionCx) -> BoxFuture<'static, Result<SessionInstalls, HookError>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            let config =
                JudgeConfig::from_config(&cx.config).map_err(|error| HookError::Failed {
                    message: format!("dal.toml: {error}").into(),
                })?;
            shared.set_identity(
                cx.session,
                JudgeIdentity {
                    config,
                    route: cx.model,
                    model_id: cx.model_id,
                },
            );
            Ok(SessionInstalls::default())
        })
    }
}

struct SessionStartHook {
    shared: Arc<Shared>,
}

impl ObserveHook<SessionStart> for SessionStartHook {
    fn call(&self, event: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            let Some(identity) = shared.take_identity(cx.session) else {
                return Err(HookError::Failed {
                    message: "history session identity is unavailable.".into(),
                });
            };
            shared.remove(cx.session);
            let (tx, rx) = mpsc::channel(16);
            #[expect(
                clippy::disallowed_methods,
                reason = "task is kept alive by the SessionHandle held in Shared.sessions"
            )]
            let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(task::run(
                event.session,
                identity.config,
                identity.route,
                identity.model_id,
                cx.services,
                cx.caller,
                rx,
            )));
            shared
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(cx.session, SessionHandle { tx, task });
            Ok(())
        })
    }
}

struct SessionEndHook {
    shared: Arc<Shared>,
}

impl ObserveHook<SessionEnd> for SessionEndHook {
    fn call(&self, _event: SessionEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            shared.end_session(cx.session).await;
            Ok(())
        })
    }
}

struct InputHook {
    shared: Arc<Shared>,
}

impl Hook<InputEvent, InputVerdict> for InputHook {
    fn call(
        &self,
        _event: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            shared.send(cx.session, TaskMsg::Input);
            Ok(InputVerdict::Continue)
        })
    }
}

struct SettledHook {
    shared: Arc<Shared>,
}

impl ObserveHook<Settled> for SettledHook {
    fn call(&self, _event: Settled, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            shared.send(cx.session, TaskMsg::Settled);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{SESSION_END_GRACE, SessionEndHook, SessionHandle, Shared, TaskMsg};
    use crate::work::support::FakeServices;
    use dal_agent::ext::{HookCx, ObserveHook, Services};
    use dal_core::{SessionEnd, SessionId};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio_util::task::AbortOnDropHandle;

    fn lock_sessions(
        shared: &Shared,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<SessionId, SessionHandle>> {
        shared
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[tokio::test]
    async fn session_end_reaches_a_full_inbox_before_the_abort_fallback() {
        let shared = Arc::new(Shared::new());
        let session = SessionId::new_v7();
        let (tx, mut rx) = mpsc::channel(16);
        for _ in 0..16 {
            tx.try_send(TaskMsg::Input).expect("fresh channel has room");
        }
        assert!(tx.try_send(TaskMsg::Settled).is_err());
        let end_received = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&end_received);
        #[expect(
            clippy::disallowed_methods,
            reason = "test owns the spawned run through the SessionHandle under test"
        )]
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if matches!(message, TaskMsg::End) {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
            }
        }));
        lock_sessions(&shared).insert(session, SessionHandle { tx, task });
        let services: Arc<dyn Services> = Arc::new(FakeServices::default());
        let hook = SessionEndHook {
            shared: Arc::clone(&shared),
        };
        let start = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(3),
            hook.call(
                SessionEnd {
                    session,
                    reason: "close".into(),
                },
                HookCx::for_test(services, session, None),
            ),
        )
        .await;
        assert!(matches!(outcome, Ok(Ok(()))));
        assert!(start.elapsed() < SESSION_END_GRACE + Duration::from_secs(1));
        assert!(
            end_received.load(std::sync::atomic::Ordering::SeqCst),
            "the task drained the full inbox and observed End before removal"
        );
        assert!(lock_sessions(&shared).is_empty());
    }

    #[tokio::test]
    async fn session_end_never_hangs_on_a_wedged_task() {
        let shared = Arc::new(Shared::new());
        let session = SessionId::new_v7();
        let (tx, rx) = mpsc::channel(1);
        tx.try_send(TaskMsg::Input).expect("fresh channel has room");
        #[expect(
            clippy::disallowed_methods,
            reason = "test owns the spawned run through the SessionHandle under test"
        )]
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let _held = rx;
            tokio::time::sleep(Duration::from_secs(3_600)).await;
        }));
        lock_sessions(&shared).insert(session, SessionHandle { tx, task });
        let services: Arc<dyn Services> = Arc::new(FakeServices::default());
        let hook = SessionEndHook {
            shared: Arc::clone(&shared),
        };
        let start = tokio::time::Instant::now();
        let outcome = hook
            .call(
                SessionEnd {
                    session,
                    reason: "close".into(),
                },
                HookCx::for_test(services, session, None),
            )
            .await;
        assert!(outcome.is_ok());
        assert!(start.elapsed() < SESSION_END_GRACE + Duration::from_secs(3));
        assert!(lock_sessions(&shared).is_empty());
    }
}
