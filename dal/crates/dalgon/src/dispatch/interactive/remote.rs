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
use dal_wire::remote::{CancellableLogin, RemoteHostUpdate, RemoteLoginMethod};
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
        let pending = match self
            .0
            .login_cancellable(provider, wire_method)
            .await
            .map_err(wire)?
        {
            CancellableLogin::Ready => return Ok(outcome),
            CancellableLogin::Pending {
                login_id,
                url,
                user_code,
            } => {
                let shown = match user_code {
                    Some(code) => LoginProgress::ShowCode { url, code },
                    None => LoginProgress::OpenUrl { url },
                };
                if progress.try_send(shown).is_err() {
                    return Err(cancelled());
                }
                login_id
            }
        };
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
                login_id,
                ready,
                detail,
                ..
            } = update
            else {
                continue;
            };
            if login_id != pending {
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
                    expires_at: None,
                })
            })
            .collect())
    }
}

impl TuiAgent for RemoteSession {
    type Subscription = RemoteUpdates;

    fn session(&self) -> SessionId {
        self.0.session()
    }

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

#[cfg(all(test, unix))]
#[expect(clippy::expect_used, reason = "loopback login tests fail loudly")]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use dal_agent::login::{CredentialKind, LoginId, LoginIo, LoginOutcome, LoginProgress, Method};
    use dal_agent::login_fake::{FakeOAuth, TokenReply, USER_CODE, follow_authorize_url};
    use dal_agent::{Env, Host, HostError, Product};
    use dal_core::{Config, ConfigProduct};
    use dal_provider::ProviderError;
    use dal_tui::TuiError;
    use dal_tui::backend::TuiHost as _;
    use dal_wire::{RemoteEndpoint, RemoteHost, serve_local, serve_rpc};
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    use super::RemoteBackend;

    const WAIT: Duration = Duration::from_secs(30);

    struct Rig {
        dir: tempfile::TempDir,
        host: Host,
        fake: FakeOAuth,
        backend: RemoteBackend,
        connections: Arc<Mutex<Vec<CancellationToken>>>,
    }

    impl Rig {
        fn auth_json(&self) -> std::path::PathBuf {
            self.dir.path().join("data").join("auth.json")
        }

        fn drop_first_connection(&self) {
            self.connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[0]
                .cancel();
        }
    }

    async fn with_backend<F>(reply: TokenReply, body: F)
    where
        F: AsyncFnOnce(&Rig),
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).expect("data root");
        let socket_dir = dir.path().join("sock");
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&socket_dir)
                .expect("private socket directory");
        }
        let socket = socket_dir.join("rpc.sock");
        let config = Config::load(ConfigProduct::Dalgon, &data, "", None).expect("config");
        let product = Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: Vec::new(),
            bundled: Vec::new(),
        };
        let host = Host::start(product, config, Env::data_root(data))
            .await
            .expect("host");
        let fake = FakeOAuth::start(reply).await.expect("fake oauth server");
        host.set_login_endpoints(fake.endpoints(0, 0).expect("endpoints"))
            .expect("loopback endpoints");
        let connections = Arc::new(Mutex::new(Vec::new()));
        let serve_host = host.clone();
        let registry = Arc::clone(&connections);
        let server = serve_local(&socket, None, None, move |transport| {
            let host = serve_host.clone();
            let dropped = CancellationToken::new();
            registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(dropped.clone());
            Box::pin(async move {
                tokio::select! {
                    result = serve_rpc(host, transport) => result,
                    () = dropped.cancelled() => Ok(()),
                }
            })
        });
        let client = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while !socket.exists() {
                assert!(tokio::time::Instant::now() < deadline, "socket appears");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let remote = RemoteHost::connect(RemoteEndpoint::LocalSocket(socket.clone()))
                .await
                .expect("connect");
            let rig = Rig {
                dir,
                host,
                fake,
                backend: RemoteBackend(remote),
                connections,
            };
            body(&rig).await;
        };
        tokio::select! {
            outcome = server => panic!("the local server ended: {outcome:?}"),
            result = tokio::time::timeout(WAIT, client) => result.expect("the test finishes"),
        }
    }

    fn is_cancelled(error: &TuiError) -> bool {
        matches!(
            error,
            TuiError::Host(HostError::Provider(ProviderError::LoginCancelled))
        )
    }

    fn backend_message(error: TuiError) -> Option<String> {
        match error {
            TuiError::Backend(source) => Some(source.to_string()),
            _ => None,
        }
    }

    /// Publishes a failed `LoginFinished` for another attempt of the same
    /// provider and one for another provider.
    async fn finish_unrelated_attempts(host: &Host) {
        let (io, _events) = LoginIo::channel(None, CancellationToken::new());
        let same_provider = host
            .login_as(LoginId::new(9_000), "openai-codex", Method::ApiKey, io)
            .await;
        assert!(same_provider.is_err());
        let (io, _events) = LoginIo::channel(None, CancellationToken::new());
        let other_provider = host
            .login_as(LoginId::new(9_001), "openai", Method::Browser, io)
            .await;
        assert!(other_provider.is_err());
    }

    async fn first_progress(
        events: &mut tokio::sync::mpsc::Receiver<LoginProgress>,
    ) -> LoginProgress {
        tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("progress in time")
            .expect("progress")
    }

    #[tokio::test]
    async fn an_api_key_paste_is_stored_through_the_server() {
        with_backend(TokenReply::Issue, async |rig| {
            let (key, pasted) = oneshot::channel();
            key.send(String::from("sk-remote")).expect("send key");
            let (io, _events) = LoginIo::channel(Some(pasted), CancellationToken::new());
            let outcome = rig
                .backend
                .login("openai", Method::ApiKey, io)
                .await
                .expect("login");
            assert_eq!(
                outcome,
                LoginOutcome {
                    provider: "openai".into(),
                    method: Method::ApiKey,
                    account: None,
                }
            );
            let stored = rig.backend.stored_credentials().await.expect("credentials");
            assert_eq!(
                stored
                    .iter()
                    .map(|row| (&*row.provider, row.kind, row.expired))
                    .collect::<Vec<_>>(),
                [("openai", CredentialKind::ApiKey, false)]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn an_api_key_login_without_a_key_or_with_cancel_stores_nothing() {
        with_backend(TokenReply::Issue, async |rig| {
            let (io, _events) = LoginIo::channel(None, CancellationToken::new());
            let error = rig
                .backend
                .login("openai", Method::ApiKey, io)
                .await
                .expect_err("no paste channel");
            assert!(is_cancelled(&error), "{error}");

            let (key, pasted) = oneshot::channel::<String>();
            drop(key);
            let (io, _events) = LoginIo::channel(Some(pasted), CancellationToken::new());
            let error = rig
                .backend
                .login("openai", Method::ApiKey, io)
                .await
                .expect_err("closed paste channel");
            assert!(is_cancelled(&error), "{error}");

            let (_key, pasted) = oneshot::channel::<String>();
            let cancel = CancellationToken::new();
            cancel.cancel();
            let (io, _events) = LoginIo::channel(Some(pasted), cancel);
            let error = rig
                .backend
                .login("openai", Method::ApiKey, io)
                .await
                .expect_err("cancelled before the key");
            assert!(is_cancelled(&error), "{error}");
            assert!(!rig.auth_json().exists());
        })
        .await;
    }

    #[tokio::test]
    async fn a_browser_login_opens_the_url_and_ignores_other_attempts_finishing() {
        with_backend(TokenReply::Issue, async |rig| {
            let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
            let browser = async {
                let LoginProgress::OpenUrl { url } = first_progress(&mut events).await else {
                    panic!("a browser login opens a URL");
                };
                assert!(url.contains("/oauth/authorize"), "{url}");
                finish_unrelated_attempts(&rig.host).await;
                follow_authorize_url(&url, "auth-code")
                    .await
                    .expect("callback");
            };
            let (outcome, ()) = tokio::join!(
                rig.backend.login("openai-codex", Method::Browser, io),
                browser
            );
            let outcome = outcome.expect("the login outlives the unrelated updates");
            assert_eq!(&*outcome.provider, "openai-codex");
            assert_eq!(outcome.method, Method::Browser);
            assert!(rig.auth_json().is_file());
            assert_eq!(rig.fake.requests_to("/oauth/token").len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn a_device_login_shows_the_code_and_finishes_ready() {
        with_backend(TokenReply::Issue, async |rig| {
            let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
            let (outcome, shown) = tokio::join!(
                rig.backend.login("openai-codex", Method::Device, io),
                first_progress(&mut events)
            );
            outcome.expect("login");
            let LoginProgress::ShowCode { url, code } = shown else {
                panic!("a device login shows a code");
            };
            assert!(url.ends_with("/codex/device"), "{url}");
            assert_eq!(code, USER_CODE);
            assert!(rig.auth_json().is_file());
        })
        .await;
    }

    #[tokio::test]
    async fn a_rejected_exchange_surfaces_the_server_detail() {
        with_backend(TokenReply::Reject, async |rig| {
            let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
            let browser = async {
                let LoginProgress::OpenUrl { url } = first_progress(&mut events).await else {
                    panic!("a browser login opens a URL");
                };
                follow_authorize_url(&url, "auth-code")
                    .await
                    .expect("callback");
            };
            let (outcome, ()) = tokio::join!(
                rig.backend.login("openai-codex", Method::Browser, io),
                browser
            );
            let message = backend_message(outcome.expect_err("the exchange is rejected"))
                .expect("a backend error");
            assert!(
                message.starts_with("sign-in failed: the token endpoint returned 400"),
                "{message}"
            );
            assert!(!rig.auth_json().exists());
        })
        .await;
    }

    #[tokio::test]
    async fn cancelling_a_pending_login_ends_in_a_cancelled_error() {
        with_backend(TokenReply::Issue, async |rig| {
            let cancel = CancellationToken::new();
            let (io, mut events) = LoginIo::channel(None, cancel.clone());
            let user = async {
                let LoginProgress::OpenUrl { .. } = first_progress(&mut events).await else {
                    panic!("a browser login opens a URL");
                };
                cancel.cancel();
            };
            let (outcome, ()) =
                tokio::join!(rig.backend.login("openai-codex", Method::Browser, io), user);
            let error = outcome.expect_err("cancelled");
            assert!(is_cancelled(&error), "{error}");
            assert!(!rig.auth_json().exists());
        })
        .await;
    }

    #[tokio::test]
    async fn a_dropped_connection_ends_the_wait_with_a_backend_error() {
        with_backend(TokenReply::Issue, async |rig| {
            let (io, mut events) = LoginIo::channel(None, CancellationToken::new());
            let user = async {
                let LoginProgress::OpenUrl { .. } = first_progress(&mut events).await else {
                    panic!("a browser login opens a URL");
                };
                rig.drop_first_connection();
            };
            let (outcome, ()) =
                tokio::join!(rig.backend.login("openai-codex", Method::Browser, io), user);
            let message = backend_message(outcome.expect_err("the connection dropped"))
                .expect("a backend error");
            assert_eq!(message, "the connection dropped during sign-in.");
            assert!(!rig.auth_json().exists());
        })
        .await;
    }
}
