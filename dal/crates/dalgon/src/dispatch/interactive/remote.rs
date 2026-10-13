//! Remote implementations of the terminal Host/Agent operation contract.

use std::sync::Arc;

use dal_agent::SessionRef;
use dal_core::{
    Answer, ClientId, Command, CommandSpec, ExtStatus, Gen, PageReq, Reply, RequestId, Seq,
    SessionId, View,
};
use dal_tui::TuiError;
use dal_tui::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use dal_wire::{RemoteAgent, RemoteDelivery, RemoteHost, RemoteSubscription};

/// The remote client wrapped in the same operations used by the local TUI.
pub(super) struct RemoteBackend(pub(super) RemoteHost);

/// One remote session opened by the client.
#[derive(Clone)]
pub(super) struct RemoteSession(RemoteAgent);

/// Remote deliveries, including a fresh view after replay was lost.
pub(super) struct RemoteUpdates(RemoteSubscription);

impl TuiHost for RemoteBackend {
    type Agent = RemoteSession;

    async fn open(
        &self,
        session: SessionRef,
        _client: ClientId,
    ) -> Result<RemoteSession, TuiError> {
        self.0
            .open(session)
            .await
            .map(RemoteSession)
            .map_err(|error| TuiError::Backend(Box::new(error)))
    }

    async fn commands(&self) -> Result<Arc<[CommandSpec]>, TuiError> {
        self.0
            .commands()
            .await
            .map_err(|error| TuiError::Backend(Box::new(error)))
    }

    async fn close(&self, id: SessionId) -> Result<(), TuiError> {
        self.0
            .close(id)
            .await
            .map_err(|error| TuiError::Backend(Box::new(error)))
    }
}

impl TuiAgent for RemoteSession {
    type Subscription = RemoteUpdates;

    async fn view(&self, page: PageReq) -> Result<View, TuiError> {
        self.0
            .view(page)
            .await
            .map_err(|error| TuiError::Backend(Box::new(error)))
    }

    async fn subscribe(&self, after: Option<(Gen, Seq)>) -> Result<RemoteUpdates, TuiError> {
        self.0
            .subscribe(after)
            .await
            .map(RemoteUpdates)
            .map_err(|error| TuiError::Backend(Box::new(error)))
    }

    async fn submit(&self, command: Command) -> Result<Reply, TuiError> {
        self.0
            .submit(command)
            .await
            .map_err(|error| TuiError::Backend(Box::new(error)))
    }

    async fn answer(&self, id: RequestId, answer: Answer) -> Result<(), TuiError> {
        self.0
            .answer(id, answer)
            .await
            .map_err(|error| TuiError::Backend(Box::new(error)))
    }

    fn ext_status(&self) -> Vec<ExtStatus> {
        Vec::new()
    }
}

impl TuiSubscription for RemoteUpdates {
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError> {
        let delivery = self
            .0
            .next()
            .await
            .map_err(|error| TuiError::Backend(Box::new(error)))?;
        Ok(Some(match delivery {
            RemoteDelivery::Update(update) => TuiDelivery::Update(update),
            RemoteDelivery::Resync(view) => TuiDelivery::Resync(Some(view)),
        }))
    }
}
