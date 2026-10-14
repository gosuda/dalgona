//! HTTP/1.1 serve listener: lifecycle, guards, and route dispatch.
//!
//! Hyper serves HTTP/1.1 only on a Tokio `TcpListener` with a 256-connection
//! cap. The pipeline is route, browser guard, authentication, content type,
//! bounded body, then the route handler. OpenAI routes run through the
//! router, A2A routes through `a2a`, and `/v1/ws` plus `/codex/ws` upgrade
//! to WebSockets. Loopback binds need no token; public binds require the
//! BLAKE3-checked serve token. Stopping ceases accepts, cancels handler
//! turns, and drains connections within two seconds.

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::router::{DigestTable, RouterOptions};
use crate::token::SecretToken;
use bytes::Bytes;
use dal_agent::Host;
use dal_core::ApprovalMode;
use futures::{StreamExt, stream::FuturesUnordered};
use hyper::{Request, Response};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::error::WireError;

pub(crate) mod a2a;
mod guard;
mod http;
mod upgrade;

use guard::{auth_guard, content_guard, host_guard, is_card, is_upgrade, requires_json};
pub(crate) use http::{Resp, RespBody, ServeBody};
use http::{into_response, read_body};
use upgrade::upgrade_response;

/// A bound serve listener: its address plus its draining wait.
pub struct ServeHandle {
    /// The bound socket address.
    local_addr: SocketAddr,
    /// The accept-and-drain future.
    done: futures::future::BoxFuture<'static, Result<(), WireError>>,
}

impl ServeHandle {
    /// Returns the bound socket address.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Waits for the stop signal, cancels handler turns, and drains.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when a connection fails during the drain.
    pub async fn wait(self) -> Result<(), WireError> {
        self.done.await
    }
}

/// Maximum concurrent HTTP connections.
const CONN_CAP: usize = 256;
/// Maximum retained connections waiting to pass their first request guards.
const PRE_AUTH_CAP: usize = 32;
/// Absolute deadline for each HTTP request header, including idle keep-alive.
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum request header buffer size in bytes.
const HEADER_LIMIT: usize = 1_048_576;
/// Stop drain grace.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// The resolved listener configuration for one bound socket.
#[derive(Clone)]
pub(crate) struct ListenerCfg {
    /// The bind address.
    pub bind: IpAddr,
    /// The bound port.
    pub port: u16,
    /// Whether public token authentication is required.
    pub public: bool,
    /// The loaded serve token, when public.
    pub token: Option<SecretToken>,
    /// Whether A2A routes are mounted.
    pub a2a: bool,
    /// The allowed browser origins for WebSocket upgrades.
    pub origins: Vec<String>,
    /// The serve approval mode.
    pub approval: ApprovalMode,
    /// One-level model aliases.
    pub aliases: BTreeMap<Box<str>, Box<str>>,
    /// The serve working directory captured once at the edge.
    pub workspace: PathBuf,
}

/// Shared listener state for one bound socket.
pub(crate) struct ServeCtx {
    /// The serving host.
    pub host: Host,
    /// The listener configuration.
    pub cfg: ListenerCfg,
    /// The history-digest continuation table.
    pub digests: Arc<Mutex<DigestTable>>,
    /// The A2A context and task tables.
    pub a2a: Arc<Mutex<a2a::A2aState>>,
    /// Live stream drivers polled by the accept loop (no spawned tasks).
    pub drivers: mpsc::Sender<Driver>,
    /// Fires when handlers must cancel their turns.
    pub shutdown: CancellationToken,
}

impl ServeCtx {
    /// Hands one live stream or upgrade driver to the accept loop, which polls it.
    pub(crate) async fn drive(&self, driver: Driver) {
        if self.drivers.send(driver).await.is_err() {
            tracing::debug!("listener stopped before a driver started");
        }
    }
}

/// One unverified connection, which can be evicted to admit a new peer.
struct PendingConnection {
    cancel: CancellationToken,
}

/// Single-owner admission queue. Validated connections leave this queue
/// through their weak reference without reducing the normal connection cap.
#[derive(Default)]
struct PreAuthBudget {
    slots: VecDeque<Weak<PendingConnection>>,
}

impl PreAuthBudget {
    fn admit(&mut self) -> Arc<PendingConnection> {
        self.slots.retain(|slot| slot.strong_count() > 0);
        if self.slots.len() >= PRE_AUTH_CAP
            && let Some(oldest) = self.slots.pop_front().and_then(|slot| slot.upgrade())
        {
            oldest.cancel.cancel();
        }
        let pending = Arc::new(PendingConnection {
            cancel: CancellationToken::new(),
        });
        self.slots.push_back(Arc::downgrade(&pending));
        pending
    }
}

/// One live stream or upgrade future owned by the accept loop.
pub(crate) type Driver = futures::future::BoxFuture<'static, ()>;
/// Binds the accept loop for one listener and returns its handle.
pub(crate) fn listen(
    host: Host,
    cfg: ListenerCfg,
    listener: TcpListener,
    local_addr: SocketAddr,
    stop: CancellationToken,
) -> ServeHandle {
    let (driver_tx, mut driver_rx) = mpsc::channel::<Driver>(CONN_CAP);
    let ctx = Arc::new(ServeCtx {
        host,
        cfg,
        digests: Arc::new(Mutex::new(DigestTable::new(4096))),
        a2a: Arc::new(Mutex::new(a2a::A2aState::new())),
        drivers: driver_tx,
        shutdown: stop.child_token(),
    });
    let shutdown = ctx.shutdown.clone();
    let mut pre_auth = PreAuthBudget::default();
    let done = async move {
        let mut conns: FuturesUnordered<Driver> = FuturesUnordered::new();
        let mut drivers: FuturesUnordered<Driver> = FuturesUnordered::new();
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                // Prioritize reaping cancelled or completed drivers over new accepts.
                _ = conns.next(), if !conns.is_empty() => {}
                _ = drivers.next(), if !drivers.is_empty() => {}
                Some(driver) = driver_rx.recv() => drivers.push(driver),
                accepted = listener.accept(), if conns.len() < CONN_CAP => {
                    match accepted {
                        Ok((stream, _)) => {
                            let pending = pre_auth.admit();
                            let ctx = Arc::clone(&ctx);
                            conns.push(Box::pin(async move {
                                serve_conn(ctx, stream, pending).await;
                            }));
                        }
                        Err(error) => {
                            if is_capacity_error(&error) {
                                tracing::warn!(%error, "accept capacity exhausted");
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                continue;
                            }
                            tracing::warn!(%error, "accept failed");
                            break;
                        }
                    }
                }
            }
        }
        shutdown.cancel();
        drop(ctx);
        let _ = tokio::time::timeout(STOP_GRACE, async {
            loop {
                tokio::select! {
                    Some(driver) = driver_rx.recv() => drivers.push(driver),
                    Some(()) = conns.next() => {}
                    Some(()) = drivers.next() => {}
                    else => break,
                }
            }
        })
        .await;
        Ok(())
    };
    ServeHandle {
        local_addr,
        done: Box::pin(done),
    }
}

/// Returns true for EMFILE/ENFILE accept failures.
fn is_capacity_error(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code) if code == libc_emfile() || code == libc_enfile()
    )
}

/// The EMFILE errno without a libc dependency.
fn libc_emfile() -> i32 {
    24
}

/// The ENFILE errno without a libc dependency.
fn libc_enfile() -> i32 {
    23
}

/// Builds an HTTP/1.1 parser with bounded time to finish every request header.
fn http1_builder() -> hyper::server::conn::http1::Builder {
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .max_buf_size(HEADER_LIMIT)
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT);
    builder
}

/// Serves one HTTP/1.1 connection to completion.
async fn serve_conn(
    ctx: Arc<ServeCtx>,
    stream: tokio::net::TcpStream,
    pending: Arc<PendingConnection>,
) {
    let io = hyper_util::rt::TokioIo::new(stream);
    let shutdown = ctx.shutdown.clone();
    let evict = pending.cancel.clone();
    // A validated request drops the last strong reference in the pending
    // queue. Its HTTP connection can still run under the normal cap.
    let pre_auth = Arc::new(Mutex::new(Some(pending)));
    let service = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
        let ctx = Arc::clone(&ctx);
        let pre_auth = Arc::clone(&pre_auth);
        async move { Ok::<_, hyper::Error>(handle_request(ctx, req, &pre_auth).await) }
    });
    let conn = http1_builder()
        .serve_connection(io, service)
        .with_upgrades();
    let mut conn = std::pin::pin!(conn);
    let result = tokio::select! {
        result = conn.as_mut() => result,
        () = evict.cancelled() => {
            tracing::debug!("evicted unverified HTTP connection");
            return;
        }
        () = shutdown.cancelled() => {
            conn.as_mut().graceful_shutdown();
            conn.await
        }
    };
    if let Err(error) = result {
        tracing::debug!(%error, "connection ended");
    }
}

/// Closes rejected or anonymous HTTP connections after the response.
fn close_untrusted(mut response: Response<ServeBody>) -> Response<ServeBody> {
    response.headers_mut().insert(
        hyper::header::CONNECTION,
        hyper::header::HeaderValue::from_static("close"),
    );
    response
}

/// Handles one HTTP request through guards to its route handler.
async fn handle_request(
    ctx: Arc<ServeCtx>,
    req: Request<hyper::body::Incoming>,
    pre_auth: &Mutex<Option<Arc<PendingConnection>>>,
) -> Response<ServeBody> {
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    let query = req.uri().query().unwrap_or("").to_owned();
    let route = super::router::match_serve_route(&method, &path, ctx.cfg.a2a);
    let upgrade_route = matches!(
        route,
        super::router::ServeRoute::WebSocket | super::router::ServeRoute::CodexWs
    );
    if req.headers().contains_key("origin") && !is_upgrade(&req) {
        return close_untrusted(into_response(Resp::text(
            403,
            "browser requests are not allowed",
        )));
    }
    if !ctx.cfg.public
        && let Err(resp) = host_guard(&ctx, &req)
    {
        return close_untrusted(into_response(resp));
    }
    if ctx.cfg.public
        && !is_card(&method, &path)
        && !upgrade_route
        && let Err(resp) = auth_guard(&ctx, &req)
    {
        return close_untrusted(into_response(resp));
    }
    if upgrade_route {
        let response = upgrade_response(ctx, req, route).await;
        if response.status() == hyper::StatusCode::SWITCHING_PROTOCOLS {
            drop(pre_auth.lock().await.take());
            return response;
        }
        return close_untrusted(response);
    }
    if ctx.cfg.public && is_card(&method, &path) {
        // The public card needs no token. Do not let its body or keep-alive
        // occupy an unauthenticated slot after the response.
        let host = req
            .headers()
            .get("host")
            .and_then(|value| value.to_str().ok());
        return close_untrusted(into_response(a2a::card(&ctx, host)));
    }
    // Request guards passed. Later body reads, handlers, and streams belong
    // to the normal connection pool rather than the pre-auth budget.
    drop(pre_auth.lock().await.take());
    if requires_json(&route)
        && let Err(resp) = content_guard(&req, &route)
    {
        return into_response(resp);
    }
    let version = req
        .headers()
        .get("a2a-version")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let anthropic = req.headers().contains_key("anthropic-version");
    let host_header = req
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let session = req
        .headers()
        .get("x-dal-session")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return into_response(resp),
    };
    let request = RouteRequest {
        session: session.as_deref(),
        query: &query,
        version: version.as_deref(),
        host: host_header.as_deref(),
        anthropic,
    };
    dispatch(ctx, &request, route, body).await
}

/// The request header facts a route handler needs after the body is read.
struct RouteRequest<'a> {
    session: Option<&'a str>,
    query: &'a str,
    version: Option<&'a str>,
    host: Option<&'a str>,
    anthropic: bool,
}

/// Dispatches one guarded request to its route handler.
async fn dispatch(
    ctx: Arc<ServeCtx>,
    request: &RouteRequest<'_>,
    route: super::router::ServeRoute,
    body: Bytes,
) -> Response<ServeBody> {
    let RouteRequest {
        session,
        query,
        version,
        host,
        anthropic,
    } = *request;
    let http = || super::router::harness::HttpParts {
        session_header: session.map(str::to_owned),
    };
    let resp = match route {
        super::router::ServeRoute::Models if anthropic => {
            super::router::handlers::anthropic_models(&ctx).await
        }
        super::router::ServeRoute::Models => super::router::handlers::models(&ctx).await,
        super::router::ServeRoute::Chat => super::router::handlers::chat(&ctx, &body, http()).await,
        super::router::ServeRoute::Responses => {
            super::router::handlers::responses(&ctx, &body, http()).await
        }
        super::router::ServeRoute::Messages => {
            super::router::handlers::messages(&ctx, &body, http()).await
        }
        super::router::ServeRoute::A2aCard => a2a::card(&ctx, host),
        super::router::ServeRoute::A2aJsonRpc => a2a::json_rpc(&ctx, &body, query, version).await,
        super::router::ServeRoute::A2aRest(kind, id) => {
            a2a::rest(&ctx, kind, id, &body, query, version).await
        }
        super::router::ServeRoute::WebSocket | super::router::ServeRoute::CodexWs => {
            Resp::text(426, "upgrade required")
        }
        super::router::ServeRoute::NotFound { method, path } => {
            Resp::text(404, &format!("no route for {method} {path}"))
        }
        super::router::ServeRoute::MethodNotAllowed {
            method,
            path,
            allow,
        } => Resp {
            status: 405,
            headers: vec![("allow".to_owned(), allow.clone())],
            body: RespBody::Full(Bytes::from(
                format!("{method} {path} is not allowed: use {allow}").into_bytes(),
            )),
        },
    };
    into_response(resp)
}

/// Builds router options from a listener config.
pub(crate) fn router_options(cfg: &ListenerCfg) -> RouterOptions {
    RouterOptions {
        bind: cfg.bind.to_string(),
        port: cfg.port,
        public: cfg.public,
        a2a: cfg.a2a,
        token_file: std::path::PathBuf::new(),
        approval: cfg.approval,
        origins: cfg.origins.clone(),
        aliases: cfg.aliases.clone(),
        workspace: cfg.workspace.clone(),
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::{HEADER_TIMEOUT, PRE_AUTH_CAP, PreAuthBudget, ServeBody, http1_builder};
    use std::{error::Error, io, time::Duration};

    use hyper::{Request, Response};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinSet,
        time::Instant,
    };

    type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

    async fn socket_pair() -> io::Result<(TcpStream, JoinSet<io::Result<()>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        // `tokio::spawn` is workspace-banned; JoinSet is the tracked spawn.
        let mut server = JoinSet::new();
        server.spawn(async move {
            let (socket, _) = listener.accept().await?;
            let service =
                hyper::service::service_fn(|_req: Request<hyper::body::Incoming>| async {
                    Ok::<_, hyper::Error>(Response::new(ServeBody::Full(None)))
                });
            let _ = http1_builder()
                .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                .await;
            Ok(())
        });
        Ok((TcpStream::connect(address).await?, server))
    }

    async fn assert_server_closed(
        server: &mut JoinSet<io::Result<()>>,
        deadline: Instant,
    ) -> TestResult {
        tokio::time::timeout_at(deadline, server.join_next())
            .await?
            .expect("server task joined")??;
        Ok(())
    }

    #[tokio::test]
    async fn silent_peer_cannot_hold_a_header_slot() -> TestResult {
        let (client, mut server) = socket_pair().await?;
        let deadline = Instant::now() + HEADER_TIMEOUT + Duration::from_secs(2);
        assert_server_closed(&mut server, deadline).await?;
        drop(client);
        Ok(())
    }

    #[tokio::test]
    async fn partial_headers_cannot_extend_the_absolute_deadline() -> TestResult {
        let (mut client, mut server) = socket_pair().await?;
        let deadline = Instant::now() + HEADER_TIMEOUT + Duration::from_secs(2);
        client.write_all(b"GET / HTTP/1.1\r\n").await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        client.write_all(b"host: localhost").await?;
        assert_server_closed(&mut server, deadline).await?;
        Ok(())
    }

    #[tokio::test]
    async fn idle_keep_alive_closes_after_a_valid_response() -> TestResult {
        let (mut client, mut server) = socket_pair().await?;
        client
            .write_all(b"GET / HTTP/1.1\r\nhost: localhost\r\n\r\n")
            .await?;
        let mut response = [0; 512];
        let count =
            tokio::time::timeout(Duration::from_secs(2), client.read(&mut response)).await??;
        assert!(
            String::from_utf8_lossy(&response[..count]).starts_with("HTTP/1.1 200"),
            "the first request must complete before the keep-alive deadline"
        );
        assert_server_closed(
            &mut server,
            Instant::now() + HEADER_TIMEOUT + Duration::from_secs(2),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn pre_auth_admission_evicts_the_oldest_unverified_peer() -> TestResult {
        let mut budget = PreAuthBudget::default();
        let first = budget.admit();
        let mut waiting = Vec::with_capacity(PRE_AUTH_CAP - 1);
        for _ in 1..PRE_AUTH_CAP {
            waiting.push(budget.admit());
        }
        let fresh = budget.admit();
        assert!(
            first.cancel.is_cancelled(),
            "new peers must evict old unverified sockets at capacity"
        );
        assert!(!fresh.cancel.is_cancelled());
        assert_eq!(budget.slots.len(), PRE_AUTH_CAP);

        let validated = tokio::sync::Mutex::new(waiting.pop());
        drop(validated.lock().await.take());
        let next = budget.admit();
        assert!(!next.cancel.is_cancelled());
        assert!(!fresh.cancel.is_cancelled());
        assert_eq!(
            budget.slots.len(),
            PRE_AUTH_CAP,
            "validated connections must free pre-auth capacity"
        );
        Ok(())
    }
}
