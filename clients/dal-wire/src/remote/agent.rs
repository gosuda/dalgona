//! Remote agent operations and session subscriptions.

use std::collections::VecDeque;
use std::sync::Arc;

use base64::Engine as _;
use dal_core::{Answer, BlobId, Command, Gen, PageReq, Reply, RequestId, Seq, SessionId, View};
use sonic_rs::{JsonValueMutTrait, Value};

use super::RemoteDelivery;
use super::conn::{Shared, SubState, subscribe_params};
use super::decode::{decode, encode, string};
use crate::error::WireError;

/// A remote agent bound to one session.
#[derive(Clone)]
pub struct RemoteAgent {
    /// The shared connection.
    pub(super) shared: Arc<Shared>,
    /// The bound session.
    pub(super) session: SessionId,
}

/// One remote session subscription.
///
/// Dropping it stops local delivery only; the server keeps running every turn.
pub struct RemoteSubscription {
    /// The shared connection.
    shared: Arc<Shared>,
    /// The subscribed session.
    session: SessionId,
    /// Identifies this handle's subscription slot.
    fence: u64,
}

impl RemoteAgent {
    /// Returns the bound session id.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Views one page through `session/view`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails or the view does not decode.
    pub async fn view(&self, page: PageReq) -> Result<View, WireError> {
        let mut params = sonic_rs::json!({
            "sessionId": self.session.to_string(),
            "limit": page.limit.get(),
        });
        if let Some(before) = page.before
            && let Some(object) = params.as_object_mut()
        {
            object.insert("before", Value::from(before.to_string().as_str()));
        }
        let result = self.shared.call("session/view", params).await?;
        decode(&result, "view")
    }

    /// Subscribes after one cursor through `session/subscribe`, replacing any
    /// earlier subscription to this session on this connection.
    ///
    /// `None` streams updates after the current head.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn subscribe(
        &self,
        after: Option<(Gen, Seq)>,
    ) -> Result<RemoteSubscription, WireError> {
        let core = &self.shared.core;
        let fence = {
            let mut state = core.state();
            let fence = state.next_fence();
            state.subs.insert(
                self.session,
                SubState {
                    fence,
                    last: after,
                    queue: VecDeque::new(),
                    resync: false,
                },
            );
            fence
        };
        let subscription = RemoteSubscription {
            shared: Arc::clone(&self.shared),
            session: self.session,
            fence,
        };
        let result = self
            .shared
            .call("session/subscribe", subscribe_params(self.session, after))
            .await?;
        core.subscribed(self.session, fence, &result)?;
        Ok(subscription)
    }

    /// Submits one command through `session/submit`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails or the reply does not decode.
    pub async fn submit(&self, command: Command) -> Result<Reply, WireError> {
        let params = sonic_rs::json!({
            "sessionId": self.session.to_string(),
            "command": encode(&command)?,
        });
        let result = self.shared.call("session/submit", params).await?;
        decode(&result, "reply")
    }

    /// Answers one request through `session/answer`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn answer(&self, id: RequestId, answer: Answer) -> Result<(), WireError> {
        let params = sonic_rs::json!({
            "sessionId": self.session.to_string(),
            "requestId": id.to_string(),
            "answer": encode(&answer)?,
        });
        self.shared.call("session/answer", params).await?;
        Ok(())
    }

    /// Reads one blob through `blob/read`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails or the payload does not decode.
    pub async fn blob(&self, id: &BlobId) -> Result<Arc<[u8]>, WireError> {
        let params = sonic_rs::json!({
            "sessionId": self.session.to_string(),
            "blobId": id.to_string(),
        });
        let result = self.shared.call("blob/read", params).await?;
        base64::engine::general_purpose::STANDARD
            .decode(string(&result, "base64")?)
            .map(Arc::from)
            .map_err(|error| WireError::Protocol {
                code: -32700,
                message: format!("remote frame has an invalid base64: {error}"),
            })
    }
}

/// What the next delivery requires.
enum Next {
    /// A queued update.
    Update(Arc<dal_core::Update>),
    /// A server `resync` needs a view repaint.
    Repaint,
}

impl RemoteSubscription {
    /// Returns the subscribed session id.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Returns the next delivery, reconnecting and resubscribing as needed.
    ///
    /// Each `(gen, seq)` is delivered at most once. A server `resync` fetches
    /// `session/view`, resubscribes at its `(gen, seq)`, and delivers
    /// [`RemoteDelivery::Resync`]. Cancel safe.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the connection fails with a non-transport
    /// error, the repaint fails, or a newer subscription replaced this one.
    pub async fn next(&mut self) -> Result<RemoteDelivery, WireError> {
        let (session, fence) = (self.session, self.fence);
        let next = self
            .shared
            .wait(|state| {
                let Some(sub) = state
                    .subs
                    .get_mut(&session)
                    .filter(|sub| sub.fence == fence)
                else {
                    return Some(Err(WireError::Transport(format!(
                        "subscription to session {session} was replaced"
                    ))));
                };
                if sub.resync {
                    sub.resync = false;
                    return Some(Ok(Next::Repaint));
                }
                sub.queue.pop_front().map(|update| Ok(Next::Update(update)))
            })
            .await?;
        match next {
            Next::Update(update) => Ok(RemoteDelivery::Update(update)),
            Next::Repaint => {
                let guard = RepaintGuard {
                    subscription: self,
                    done: false,
                };
                guard.repaint().await
            }
        }
    }

    /// Stops server-side delivery through `session/unsubscribe`.
    ///
    /// This cancels no turn.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn unsubscribe(self) -> Result<(), WireError> {
        self.shared
            .call(
                "session/unsubscribe",
                sonic_rs::json!({"sessionId": self.session.to_string()}),
            )
            .await?;
        Ok(())
    }
}

impl Drop for RemoteSubscription {
    fn drop(&mut self) {
        let mut state = self.shared.core.state();
        if state
            .subs
            .get(&self.session)
            .is_some_and(|sub| sub.fence == self.fence)
        {
            state.subs.remove(&self.session);
        }
    }
}

/// Re-arms the resync flag unless the repaint completes.
struct RepaintGuard<'a> {
    subscription: &'a RemoteSubscription,
    done: bool,
}

impl RepaintGuard<'_> {
    /// Fetches the head view and resubscribes at its `(gen, seq)`.
    async fn repaint(mut self) -> Result<RemoteDelivery, WireError> {
        let subscription = self.subscription;
        let agent = RemoteAgent {
            shared: Arc::clone(&subscription.shared),
            session: subscription.session,
        };
        let view = agent.view(PageReq::default()).await?;
        let head = (view.r#gen, view.seq);
        subscription
            .shared
            .call(
                "session/subscribe",
                subscribe_params(subscription.session, Some(head)),
            )
            .await?;
        self.done = true;
        let mut state = subscription.shared.core.state();
        if let Some(sub) = state
            .subs
            .get_mut(&subscription.session)
            .filter(|sub| sub.fence == subscription.fence)
        {
            sub.queue.retain(|update| (update.r#gen, update.seq) > head);
            sub.last = sub.last.max(Some(head));
        }
        Ok(RemoteDelivery::Resync(Box::new(view)))
    }
}

impl Drop for RepaintGuard<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let subscription = self.subscription;
        if let Some(sub) = subscription
            .shared
            .core
            .state()
            .subs
            .get_mut(&subscription.session)
            .filter(|sub| sub.fence == subscription.fence)
        {
            sub.resync = true;
        }
        subscription.shared.core.wake();
    }
}
