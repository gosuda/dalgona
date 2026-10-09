//! Remote implementations of the terminal Host/Agent operation contract.

use std::sync::Arc;

use dal_agent::SessionRef;
use dal_agent::login::{
    CredentialKind, LoginIo, LoginOutcome, LoginProgress, Method, StoredCredential,
};
use dal_core::{
    Answer, ClientId, Command, CommandSpec, ExtStatus, Gen, PageReq, Reply, RequestId, Seq,
    SessionId, View,
};
use dal_tui::TuiError;
use dal_tui::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use dal_wire::remote::{RemoteHostUpdate, RemoteLogin, RemoteLoginMethod};
use dal_wire::{RemoteAgent, RemoteDelivery, RemoteHost, RemoteSubscription};

/// The remote client wrapped in the same operations used by the local TUI.
#[derive(Clone)]
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

    /// Signs in through `auth/login`. A browser or device login answers
    /// with its URL, then ends with the server's `login_finished` update.
    /// The wire has no cancel or paste method: cancelling stops the wait,
    /// and the server flow runs until its own deadline or the connection
    /// closes.
    async fn login(
        &self,
        provider: &str,
        method: Method,
        io: LoginIo,
    ) -> Result<LoginOutcome, TuiError> {
        let LoginIo {
            progress,
            paste,
            cancel,
        } = io;
        let outcome = LoginOutcome {
            provider: provider.into(),
            method,
            account: None,
        };
        let wire = |error| TuiError::Backend(Box::new(error));
        let cancelled = || {
            TuiError::Host(dal_agent::HostError::Provider(
                dal_provider::ProviderError::LoginCancelled,
            ))
        };
        if method == Method::ApiKey {
            let key = match paste {
                Some(paste) => tokio::select! {
                    biased;
                    () = cancel.cancelled() => return Err(cancelled()),
                    key = paste => key.map_err(|_| cancelled())?,
                },
                None => return Err(cancelled()),
            };
            self.0
                .login(provider, RemoteLoginMethod::ApiKey(key))
                .await
                .map_err(wire)?;
            return Ok(outcome);
        }
        let mut updates = self.0.subscribe().await.map_err(wire)?;
        let wire_method = if method == Method::Device {
            RemoteLoginMethod::Device
        } else {
            RemoteLoginMethod::Browser
        };
        match self.0.login(provider, wire_method).await.map_err(wire)? {
            RemoteLogin::Ready => return Ok(outcome),
            RemoteLogin::Pending { url, user_code } => {
                let shown = match user_code {
                    Some(code) => LoginProgress::ShowCode { url, code },
                    None => LoginProgress::OpenUrl { url },
                };
                if progress.try_send(shown).is_err() {
                    return Err(cancelled());
                }
            }
        }
        loop {
            let update = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(cancelled()),
                update = updates.next() => update.map_err(wire)?,
            };
            if matches!(update, RemoteHostUpdate::Reconnected) {
                return Err(TuiError::Backend(Box::new(std::io::Error::other(
                    "the connection dropped during sign-in.",
                ))));
            }
            let RemoteHostUpdate::LoginFinished {
                provider: finished,
                ready,
                detail,
            } = update
            else {
                continue;
            };
            if finished != provider {
                continue;
            }
            return if ready {
                Ok(outcome)
            } else {
                Err(TuiError::Backend(Box::new(std::io::Error::other(
                    detail.unwrap_or_else(|| String::from("sign-in failed.")),
                ))))
            };
        }
    }

    async fn logout(&self, provider: Option<&str>) -> Result<Vec<Box<str>>, TuiError> {
        self.0
            .logout(provider)
            .await
            .map(|removed| removed.into_iter().map(String::into_boxed_str).collect())
            .map_err(|error| TuiError::Backend(Box::new(error)))
    }

    async fn stored_credentials(&self) -> Result<Vec<StoredCredential>, TuiError> {
        let rows = self
            .0
            .auth_status()
            .await
            .map_err(|error| TuiError::Backend(Box::new(error)))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let kind = match row.detail.as_deref()? {
                    "api_key" => CredentialKind::ApiKey,
                    "oauth" => CredentialKind::OAuth,
                    _ => return None,
                };
                Some(StoredCredential {
                    provider: row.provider.into_boxed_str(),
                    kind,
                    expired: row.state == "expired",
                })
            })
            .collect())
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
