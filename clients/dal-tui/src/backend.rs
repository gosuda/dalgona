//! The operations shared by in-process and remote terminal clients.

use std::sync::Arc;

use dal_agent::login::{LoginIo, LoginOutcome, Method, StoredCredential};
use dal_agent::{Agent, Delivery, Host, SessionRef, Subscription};
use dal_core::{
    Answer, BlobId, ClientId, Command, CommandSpec, ExtStatus, Gen, PageReq, Reply, RequestId, Seq,
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

    /// The identity of the opened session, known before any view is read.
    fn session(&self) -> SessionId;

    /// Reads one bounded page of the session view.
    async fn view(&self, page: PageReq) -> Result<View, TuiError>;
    /// Subscribes after a view's generation and sequence.
    async fn subscribe(&self, after: Option<(Gen, Seq)>) -> Result<Self::Subscription, TuiError>;
    /// Runs one typed command.
    async fn submit(&self, command: Command) -> Result<Reply, TuiError>;
    /// Resolves exactly one open request.
    async fn answer(&self, id: RequestId, answer: Answer) -> Result<(), TuiError>;
    /// Reads one stored session blob, the same read `Agent::blob` performs.
    ///
    /// The default denies: a client without a blob channel (a test agent, a
    /// backend that never named one) reports the gap instead of the caller
    /// reading local paths a remote workspace does not share.
    async fn blob(&self, _id: BlobId) -> Result<Arc<[u8]>, TuiError> {
        Err(TuiError::Terminal(
            "dal-tui: this host cannot read session blobs".to_owned(),
        ))
    }
    /// Returns busy extension status rows already published at attach time.
    fn ext_status(&self) -> Vec<ExtStatus>;
    /// Returns the session committed scope for the scoped-models picker.
    fn committed_scope(&self) -> Vec<Box<str>> {
        Vec::new()
    }
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
pub trait TuiHost: Clone + Send + Sync + 'static {
    /// The opened session handle.
    type Agent: TuiAgent;

    /// Opens a session for the terminal client.
    async fn open(&self, session: SessionRef, client: ClientId) -> Result<Self::Agent, TuiError>;
    /// Opens the session that takes over from `leaving`, which the terminal
    /// closes once the opened session is attached; the contract of
    /// [`Host::open_replacing`]. The host refuses a target the close of
    /// `leaving` would end, before the terminal cancels or closes anything.
    /// The default denies like `sessions`: a host that cannot say whether
    /// the target survives that close never resumes into a session it may
    /// take down with the one on screen.
    async fn open_replacing(
        &self,
        _leaving: SessionId,
        _session: SessionRef,
        _client: ClientId,
    ) -> Result<Self::Agent, TuiError> {
        Err(TuiError::Terminal(
            "dal-tui: this host cannot open a session in place of another".to_owned(),
        ))
    }
    /// Returns the registered slash commands.
    async fn commands(&self) -> Result<Arc<[CommandSpec]>, TuiError>;
    /// Releases the opened session after the terminal has been restored.
    async fn close(&self, id: SessionId) -> Result<(), TuiError>;
    /// Signs in to one provider; the contract of [`Host::login`]. A remote
    /// host answers with the URL, then waits for its `login_finished` update.
    async fn login(
        &self,
        provider: &str,
        method: Method,
        io: LoginIo,
    ) -> Result<LoginOutcome, TuiError>;
    /// Removes one provider's stored credential, or every stored credential
    /// when `provider` is `None`, and returns the providers that had one.
    async fn logout(&self, provider: Option<&str>) -> Result<Vec<Box<str>>, TuiError>;
    /// Lists the stored credentials without their secrets.
    async fn stored_credentials(&self) -> Result<Vec<StoredCredential>, TuiError>;
    /// Lists sessions for the resume picker. The default denies so remote
    /// or test hosts without a listing channel report the gap instead of
    /// guessing; the in-process host serves the real workspace listing.
    async fn sessions(
        &self,
        _query: dal_core::ListQuery,
    ) -> Result<dal_core::Page<dal_core::SessionInfo, Box<str>>, TuiError> {
        Err(TuiError::Terminal(
            "dal-tui: this host cannot list sessions".to_owned(),
        ))
    }
    /// Resolves one session reference (a full id, an exact name, or an id
    /// prefix) to its listing row with no listing cap; the row names the
    /// workspace the session lives in. The default denies like `sessions`,
    /// so a host without the seam never has a workspace guessed for it.
    async fn find_session(&self, _reference: &str) -> Result<dal_core::SessionInfo, TuiError> {
        Err(TuiError::Terminal(
            "dal-tui: this host cannot look up a session".to_owned(),
        ))
    }
    /// Deletes one session tree through existing store ownership. Refuses
    /// live/owned sessions without touching the tree; the store lock is the
    /// second refusal. The default denies like `sessions`.
    async fn delete_session(&self, _id: SessionId) -> Result<(), TuiError> {
        Err(TuiError::Terminal(
            "dal-tui: this host cannot delete sessions".to_owned(),
        ))
    }
}

impl TuiHost for Host {
    type Agent = Agent;

    async fn open(&self, session: SessionRef, client: ClientId) -> Result<Agent, TuiError> {
        Ok(self.open(session, client).await?)
    }

    async fn open_replacing(
        &self,
        leaving: SessionId,
        session: SessionRef,
        client: ClientId,
    ) -> Result<Agent, TuiError> {
        Ok(Host::open_replacing(self, leaving, session, client).await?)
    }

    async fn commands(&self) -> Result<Arc<[CommandSpec]>, TuiError> {
        Ok(async { self.commands() }.await)
    }

    async fn close(&self, id: SessionId) -> Result<(), TuiError> {
        Ok(self.close(id).await?)
    }

    async fn login(
        &self,
        provider: &str,
        method: Method,
        io: LoginIo,
    ) -> Result<LoginOutcome, TuiError> {
        Ok(self.login(provider, method, io).await?)
    }

    async fn logout(&self, provider: Option<&str>) -> Result<Vec<Box<str>>, TuiError> {
        Ok(self.logout(provider).await?)
    }

    async fn stored_credentials(&self) -> Result<Vec<StoredCredential>, TuiError> {
        Ok(self.stored_credentials().await?)
    }

    async fn sessions(
        &self,
        query: dal_core::ListQuery,
    ) -> Result<dal_core::Page<dal_core::SessionInfo, Box<str>>, TuiError> {
        async { Ok(Host::sessions(self, query)?) }.await
    }

    async fn find_session(&self, reference: &str) -> Result<dal_core::SessionInfo, TuiError> {
        async { Ok(Host::find_session(self, reference)?) }.await
    }

    async fn delete_session(&self, id: SessionId) -> Result<(), TuiError> {
        async { Ok(Host::delete_session(self, id)?) }.await
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

    async fn blob(&self, id: BlobId) -> Result<Arc<[u8]>, TuiError> {
        Ok(self.blob(id).await?.into())
    }

    fn ext_status(&self) -> Vec<ExtStatus> {
        self.ext_status()
    }

    fn committed_scope(&self) -> Vec<Box<str>> {
        self.scoped_models()
    }

    fn workspace_is_local(&self) -> bool {
        true
    }

    fn session(&self) -> SessionId {
        self.session()
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
