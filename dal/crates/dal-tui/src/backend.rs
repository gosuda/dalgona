//! The operations shared by in-process and remote terminal clients.

use std::sync::Arc;

use dal_agent::{Agent, Delivery, Host, SessionRef, Subscription};
use dal_core::{
    Answer, ClientId, Command, CommandSpec, ExtStatus, Gen, PageReq, Reply, RequestId, Seq,
    SessionId, Update, View,
};

use crate::TuiError;

/// A delivery from a local or reconnecting remote subscription.
#[derive(Debug)]
pub enum TuiDelivery {
    /// One sequenced session change.
    Update(Arc<Update>),
    /// Replay was lost; this view is the new baseline.
    Resync(Option<Box<View>>),
}

/// A live subscription to a terminal client's session.
#[expect(
    async_fn_in_trait,
    reason = "the terminal drives these futures on a blocking runtime handle"
)]
pub trait TuiSubscription: Send + 'static {
    /// Receives the next update, or `None` when the session closes.
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError>;
}

/// The session operations needed by the terminal interface.
#[expect(
    async_fn_in_trait,
    reason = "the terminal drives these futures on a blocking runtime handle"
)]
pub trait TuiAgent: Clone + Send + Sync + 'static {
    /// The session subscription implementation.
    type Subscription: TuiSubscription;

    /// Reads one bounded page of the session view.
    async fn view(&self, page: PageReq) -> Result<View, TuiError>;
    /// Subscribes after a view's generation and sequence.
    async fn subscribe(&self, after: Option<(Gen, Seq)>) -> Result<Self::Subscription, TuiError>;
    /// Runs one typed command.
    async fn submit(&self, command: Command) -> Result<Reply, TuiError>;
    /// Resolves exactly one open request.
    async fn answer(&self, id: RequestId, answer: Answer) -> Result<(), TuiError>;
    /// Returns busy extension status rows already published at attach time.
    fn ext_status(&self) -> Vec<ExtStatus>;
    /// Whether the session workspace lives on this machine's disk. A remote
    /// host's workspace path names the host's disk, so the default denies and
    /// no client reads a workspace it does not own; the in-process host allows.
    fn workspace_is_local(&self) -> bool {
        false
    }
}

/// A host that can open a session and expose its command registry.
#[expect(
    async_fn_in_trait,
    reason = "the terminal drives these futures on a blocking runtime handle"
)]
pub trait TuiHost: Send + 'static {
    /// The opened session handle.
    type Agent: TuiAgent;

    /// Opens a session for the terminal client.
    async fn open(&self, session: SessionRef, client: ClientId) -> Result<Self::Agent, TuiError>;
    /// Returns the registered slash commands.
    async fn commands(&self) -> Result<Arc<[CommandSpec]>, TuiError>;
    /// Releases the opened session after the terminal has been restored.
    async fn close(&self, id: SessionId) -> Result<(), TuiError>;
}

impl TuiHost for Host {
    type Agent = Agent;

    async fn open(&self, session: SessionRef, client: ClientId) -> Result<Agent, TuiError> {
        Ok(self.open(session, client).await?)
    }

    async fn commands(&self) -> Result<Arc<[CommandSpec]>, TuiError> {
        Ok(async { self.commands() }.await)
    }

    async fn close(&self, id: SessionId) -> Result<(), TuiError> {
        Ok(self.close(id).await?)
    }
}

impl TuiAgent for Agent {
    type Subscription = Subscription;

    async fn view(&self, page: PageReq) -> Result<View, TuiError> {
        async { Ok(self.view(page)?) }.await
    }

    async fn subscribe(&self, after: Option<(Gen, Seq)>) -> Result<Subscription, TuiError> {
        async { Ok(self.subscribe(after)?) }.await
    }

    async fn submit(&self, command: Command) -> Result<Reply, TuiError> {
        Ok(self.submit(command).await?)
    }

    async fn answer(&self, id: RequestId, answer: Answer) -> Result<(), TuiError> {
        Ok(self.answer(id, answer).await?)
    }

    fn ext_status(&self) -> Vec<ExtStatus> {
        self.ext_status()
    }

    fn workspace_is_local(&self) -> bool {
        true
    }
}

impl TuiSubscription for Subscription {
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError> {
        Ok(Subscription::next(self)
            .await
            .map(|delivery| match delivery {
                Delivery::Update(update) => TuiDelivery::Update(update),
                Delivery::Resync { .. } => TuiDelivery::Resync(None),
            }))
    }
}
