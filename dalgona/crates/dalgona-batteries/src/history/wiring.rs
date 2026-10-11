// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The `history` battery constructor and session hooks.
//!
//! `history()` registers only the `history` compactor with the `jobs` and
//! `sidecar` services plus the `session_start`, `session_end`,
//! first-prompt `input`, and observe-only `settled` hooks. A disabled config
//! builds an empty extension; an invalid config installs the failing
//! compactor so startup continues and every compaction surfaces the exact
//! configuration warning.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dal_agent::ext::{
    BoxFuture, Extension, ExtensionBuilder, Hook, HookCx, HookError, ObserveHook,
};
use dal_core::{
    InputEvent, InputVerdict, Origin, RegistrationError, ServiceSet, SessionEnd, SessionId,
    SessionStart, Settled,
};

use super::compact::{FailingCompactor, HistoryCompactor};
use super::config::HistoryConfig;
use super::task::{self, TaskMsg};
use tokio::sync::mpsc;

/// One live dream session: the inbox of its session-owned task plus the task.
pub(crate) struct SessionHandle {
    tx: mpsc::Sender<TaskMsg>,
    task: tokio_util::task::AbortOnDropHandle<()>,
}

/// Per-session dream tasks for one `history` battery instance.
pub(crate) struct Shared {
    sessions: Mutex<HashMap<SessionId, SessionHandle>>,
}

impl Shared {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
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
}

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
                .compactor("history", Arc::new(HistoryCompactor::new(share)))
                .on_session_start(SessionStartHook {
                    shared: Arc::clone(&shared),
                })
                .on_session_end(SessionEndHook {
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

struct SessionStartHook {
    shared: Arc<Shared>,
}

impl ObserveHook<SessionStart> for SessionStartHook {
    fn call(&self, event: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            shared.remove(cx.session);
            let (tx, rx) = mpsc::channel(16);
            #[expect(
                clippy::disallowed_methods,
                reason = "task is kept alive by the SessionHandle held in Shared.sessions"
            )]
            let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(task::run(
                event.session,
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
            shared.send(cx.session, TaskMsg::End);
            shared.remove(cx.session);
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
