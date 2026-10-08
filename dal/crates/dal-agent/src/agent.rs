//! Public operations on one live session.
//!
//! `view` and `subscribe` read the shared snapshot synchronously; `submit`,
//! `answer`, and `blob` cross the bounded actor channel asynchronously.

use std::sync::Arc;

use dal_core::{
    Answer, BlobId, CancelScope, ClientId, Command, ExtStatus, Gen, PageReq, Reply, RequestId, Seq,
    SessionId, Timestamp, View, Workspace,
};

use crate::broker::Broker;
use crate::error::AgentError;
use crate::session::projection::SnapshotArgs;
use crate::session::shared::Shared;
use crate::session::subscriber::Subscriber;
use crate::session::{SessionHandle, SubscriberPort};
/// A handle to one live session.
#[derive(Clone)]
pub struct Agent {
    pub(crate) inner: Arc<AgentInner>,
}

/// One update delivery or a cursor that requires the client to resynchronize.
#[derive(Clone, Debug)]
pub enum Delivery {
    /// A session update. Durable updates follow their journal receipt;
    /// transient updates such as extension status are not replayed.
    Update(Arc<dal_core::Update>),
    /// The client cursor fell outside the retained replay suffix.
    Resync {
        /// The current session generation.
        generation: dal_core::Gen,
        /// The latest sequence in that generation.
        seq: dal_core::Seq,
    },
}

/// A live subscription to one session's update stream.
pub struct Subscription {
    port: SubscriberPort,
}

/// Which request kinds one subscriber may answer.
///
/// The two roles come from the front end's declarations: the TUI holds
/// both, a wire client holds the kinds it named in `initialize`, and a
/// listen-only subscriber holds neither. A role without its declaration
/// resolves at once while the other still waits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnswerScope {
    /// The subscriber may resolve approval requests.
    pub approval: bool,
    /// The subscriber may answer extension questions.
    pub ask: bool,
}

pub(crate) struct AgentInner {
    pub(crate) session: SessionId,
    pub(crate) client: ClientId,
    pub(crate) handle: SessionHandle,
    pub(crate) shared: Arc<Shared>,
    pub(crate) broker: Arc<Broker>,
    pub(crate) workspace: Workspace,
    pub(crate) generation: Gen,
    pub(crate) control: Arc<std::sync::Mutex<crate::session::control::ControlCell>>,
    pub(crate) created_at: Option<Timestamp>,
    pub(crate) archived: Option<bool>,
}

impl Agent {
    /// Snapshots the materialized view from the shared snapshot.
    ///
    /// # Errors
    /// This operation currently has no error cases and always returns `Ok`.
    pub fn view(&self, page: PageReq) -> Result<View, AgentError> {
        Ok(self.inner.shared.snapshot(SnapshotArgs {
            generation: self.inner.generation,
            id: self.inner.session,
            workspace: self.inner.workspace.clone(),
            open: self.inner.broker.open_requests(),
            updated_at: Timestamp::now(),
            created_at: self.inner.created_at,
            archived: self.inner.archived,
            page,
        }))
    }

    /// Returns the current status of every extension that is busy or has
    /// status text, ordered by extension name.
    ///
    /// A client attaching after updates were published seeds its status rows
    /// from this snapshot, then follows `ext_status` updates.
    #[must_use]
    pub fn ext_status(&self) -> Vec<ExtStatus> {
        self.inner.shared.ext_statuses().into_values().collect()
    }

    /// Polls every extension status kind now and returns the statuses that
    /// are busy or carry text, ordered by extension name.
    ///
    /// Unlike [`Agent::ext_status`], which reads the last published states,
    /// this waits for the session actor to sweep each kind, so a caller that
    /// just saw a turn end cannot read a state older than that turn.
    ///
    /// # Errors
    /// Returns [`AgentError::SessionClosed`] when the session is closed.
    pub async fn poll_status(&self) -> Result<Vec<ExtStatus>, AgentError> {
        self.inner.handle.poll_status().await
    }

    /// Polls the session actor and reports whether every status kind is quiet.
    ///
    /// # Errors
    /// Returns [`AgentError::SessionClosed`] when the session actor is closed.
    pub async fn is_quiet(&self) -> Result<bool, AgentError> {
        Ok(self.poll_status().await?.iter().all(ExtStatus::is_quiet))
    }

    /// Registers a subscriber at the given cursor on the shared snapshot.
    /// The subscriber counts as an attached answerer for approvals and
    /// asks: approval requests wait for it before falling back to headless
    /// denial, and extension questions wait for it before taking the
    /// fail-closed default.
    ///
    /// # Errors
    /// This operation currently has no error cases and always returns `Ok`.
    pub fn subscribe(&self, after: Option<(Gen, Seq)>) -> Result<Subscription, AgentError> {
        self.subscribe_scoped(
            after,
            AnswerScope {
                approval: true,
                ask: true,
            },
        )
    }

    /// Registers a subscriber with one answerer role per request kind.
    /// The delivery stream is the same either way; only the raise-time
    /// default changes per kind.
    ///
    /// # Errors
    /// This operation currently has no error cases and always returns `Ok`.
    pub fn subscribe_scoped(
        &self,
        after: Option<(Gen, Seq)>,
        scope: AnswerScope,
    ) -> Result<Subscription, AgentError> {
        Ok(Subscription {
            port: self
                .inner
                .shared
                .subscribe(after, scope.approval, scope.ask),
        })
    }

    /// Registers a listen-only subscriber at the given cursor. It receives the
    /// same delivery stream but never counts as an attached answerer, so a
    /// run with only listeners denies approval asks instead of waiting out
    /// the request timeout.
    ///
    /// # Errors
    /// This operation currently has no error cases and always returns `Ok`.
    pub fn subscribe_listen(&self, after: Option<(Gen, Seq)>) -> Result<Subscription, AgentError> {
        self.subscribe_scoped(
            after,
            AnswerScope {
                approval: false,
                ask: false,
            },
        )
    }

    /// Submits a command through the fold; full channels apply backpressure.
    ///
    /// A `Cancel` fires the turn token before the command crosses the
    /// channel, so it preempts in-flight opening hooks and live streams
    /// instead of waiting behind them.
    ///
    /// # Errors
    /// Returns [`AgentError::Invalid`] for a rejected command, [`AgentError::WrongTurn`]
    /// when its turn state does not match, [`AgentError::SteerFull`] when the steer
    /// queue is full, or [`AgentError::SessionClosed`] when the session is closed.
    pub async fn submit(&self, command: Command) -> Result<Reply, AgentError> {
        if let Command::Cancel {
            scope: CancelScope::Turn(turn),
        } = &command
        {
            let _ = self
                .inner
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cancel(*turn);
        }
        self.inner
            .handle
            .submit(command, self.inner.client.clone())
            .await
    }

    /// Answers one open request through the broker.
    ///
    /// # Errors
    /// Returns [`AgentError::Invalid`] when the request is unknown or the answer
    /// does not match its question, [`AgentError::AlreadyResolved`] when another
    /// client already answered it, or [`AgentError::SessionClosed`] when the
    /// session is closed.
    pub async fn answer(&self, id: RequestId, answer: Answer) -> Result<(), AgentError> {
        self.inner
            .handle
            .answer(id, answer, self.inner.client.clone())
            .await
    }

    /// Reads one session blob through the journal.
    ///
    /// # Errors
    /// Returns [`AgentError::BlobNotFound`] when the digest is absent,
    /// [`AgentError::SessionGone`] when the session directory is gone, or
    /// [`AgentError::SessionClosed`] when the session is closed.
    pub async fn blob(&self, id: BlobId) -> Result<Vec<u8>, AgentError> {
        self.inner.handle.blob(id).await
    }
}

impl Subscription {
    /// Returns the next delivery, or `None` after drain on close or lag.
    pub async fn next(&mut self) -> Option<Delivery> {
        Subscriber::next(&self.port).await
    }
}
