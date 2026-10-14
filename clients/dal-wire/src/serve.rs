//! HTTP/1.1 serve listener: lifecycle, guards, and route dispatch.
//!
//! Hyper serves HTTP/1.1 only on a Tokio `TcpListener` with a 256-connection
//! cap. The pipeline is route, browser guard, authentication, content type,
//! bounded body, then the route handler. OpenAI routes run through the
//! router, A2A routes through `a2a`, and `/v1/ws` plus `/codex/ws` upgrade
//! to WebSockets. Loopback binds need no token; public binds require the
//! BLAKE3-checked serve token. Stopping ceases accepts, cancels handler
//! turns, and drains connections within two seconds.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::router::{DigestTable, RouterOptions};
use crate::token::SecretToken;
use bytes::Bytes;
use dal_agent::Host;
use dal_core::ApprovalMode;
use futures::{StreamExt, stream::FuturesUnordered};
use hyper::{Request, Response};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};
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
/// Maximum connections that have not passed their first request guard.
const PREAUTH_CAP: usize = 32;
/// Maximum time to receive one complete set of HTTP request headers.
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum lifetime of a connection before its first successful guard.
const PREAUTH_TIMEOUT: Duration = Duration::from_secs(10);
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

/// One live stream or upgrade future owned by the accept loop.
pub(crate) type Driver = futures::future::BoxFuture<'static, ()>;

/// Holds a scarce pre-authentication slot until the first request is accepted.
struct PreauthGuard {
    permit: Mutex<Option<OwnedSemaphorePermit>>,
    admitted: CancellationToken,
}

impl PreauthGuard {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        Self {
            permit: Mutex::new(Some(permit)),
            admitted: CancellationToken::new(),
        }
    }

    async fn admit(&self) {
        drop(self.permit.lock().await.take());
        self.admitted.cancel();
    }
}

/// Configures the same bounded HTTP/1 driver for every accepted socket.
fn http1_builder() -> hyper::server::conn::http1::Builder {
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .max_buf_size(HEADER_LIMIT)
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT);
    builder
}

/// Binds the accept loop for one listener and returns its handle.
pub(crate) fn listen(
    host: Host,
    cfg: ListenerCfg,
    listener: TcpListener,
    local_addr: SocketAddr,
    stop: CancellationToken,
) -> ServeHandle {
    let (driver_tx, mut driver_rx) = mpsc::channel::<Driver>(CONN_CAP);
    let preauth_slots = Arc::new(Semaphore::new(PREAUTH_CAP));
    let ctx = Arc::new(ServeCtx {
        host,
        cfg,
        digests: Arc::new(Mutex::new(DigestTable::new(4096))),
        a2a: Arc::new(Mutex::new(a2a::A2aState::new())),
        drivers: driver_tx,
        shutdown: stop.child_token(),
    });
    let shutdown = ctx.shutdown.clone();
    let done = async move {
        let mut conns: FuturesUnordered<Driver> = FuturesUnordered::new();
        let mut drivers: FuturesUnordered<Driver> = FuturesUnordered::new();
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                _ = conns.next(), if !conns.is_empty() => {}
                _ = drivers.next(), if !drivers.is_empty() => {}
                Some(driver) = driver_rx.recv() => drivers.push(driver),
                accepted = listener.accept(), if conns.len() < CONN_CAP => {
                    match accepted {
                        Ok((stream, _)) => {
                            if let Ok(permit) = Arc::clone(&preauth_slots).try_acquire_owned() {
                                let ctx = Arc::clone(&ctx);
                                conns.push(Box::pin(async move {
                                    serve_conn(ctx, stream, permit).await;
                                }));
                            }
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

/// Serves one HTTP/1.1 connection to completion.
async fn serve_conn(
    ctx: Arc<ServeCtx>,
    stream: tokio::net::TcpStream,
    permit: OwnedSemaphorePermit,
) {
    let preauth = Arc::new(PreauthGuard::new(permit));
    let io = hyper_util::rt::TokioIo::new(stream);
    let shutdown = ctx.shutdown.clone();
    let service_preauth = Arc::clone(&preauth);
    let service = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
        let ctx = Arc::clone(&ctx);
        let preauth = Arc::clone(&service_preauth);
        async move { Ok::<_, hyper::Error>(handle_request(ctx, req, preauth).await) }
    });
    let conn = http1_builder().serve_connection(io, service).with_upgrades();
    let mut conn = std::pin::pin!(conn);
    let deadline = tokio::time::sleep(PREAUTH_TIMEOUT);
    let mut deadline = std::pin::pin!(deadline);
    let mut admitted = false;
    let result = loop {
        tokio::select! {
            result = conn.as_mut() => break result,
            () = shutdown.cancelled() => {
                conn.as_mut().graceful_shutdown();
                break conn.await;
            }
            () = preauth.admitted.cancelled(), if !admitted => admitted = true,
            () = &mut deadline, if !admitted => {
                tracing::debug!("pre-authentication connection expired");
                return;
            }
        }
    };
    if let Err(error) = result {
        tracing::debug!(%error, "connection ended");
    }
}

/// Handles one HTTP request through guards to its route handler.
async fn handle_request(
    ctx: Arc<ServeCtx>,
    req: Request<hyper::body::Incoming>,
    preauth: Arc<PreauthGuard>,
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
        return into_response(Resp::text(403, "browser requests are not allowed"));
    }
    if !ctx.cfg.public
        && let Err(resp) = host_guard(&ctx, &req)
    {
        return into_response(resp);
    }
    if ctx.cfg.public
        && !is_card(&method, &path)
        && !upgrade_route
        && let Err(resp) = auth_guard(&ctx, &req)
    {
        return into_response(resp);
    }
    if upgrade_route {
        return upgrade_response(ctx, req, route, preauth).await;
    }
    if requires_json(&route)
        && let Err(resp) = content_guard(&req, &route)
    {
        return into_response(resp);
    }
    // The public agent card has no token requirement; keep it in the pre-auth budget.
    if !ctx.cfg.public || !is_card(&method, &path) {
        preauth.admit().await;
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
mod tests {
    use std::sync::Arc;

    use hyper::Response;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Semaphore;
    use tokio::task::JoinHandle;

    use super::{HEADER_TIMEOUT, PreauthGuard, ServeBody, http1_builder};

    async fn test_connection() -> (TcpStream, JoinHandle<Result<(), hyper::Error>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let service = service_fn(|_req| async {
                Ok::<_, hyper::Error>(Response::new(ServeBody::Full(Some(
                    bytes::Bytes::from_static(b"ok"),
                ))))
            });
            http1_builder()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await
        });
        let client = TcpStream::connect(addr).await.expect("connect");
        (client, server)
    }

    #[tokio::test(start_paused = true)]
    async fn silent_http_peer_expires_before_authentication() {
        let (_peer, server) = test_connection().await;
        tokio::time::timeout(HEADER_TIMEOUT + std::time::Duration::from_secs(1), server)
            .await
            .expect("silent peer must be evicted")
            .expect("connection driver must finish");
    }

    #[tokio::test(start_paused = true)]
    async fn slowloris_headers_expire_without_progress_reset() {
        let (mut peer, server) = test_connection().await;
        peer.write_all(b"GET / HTTP/1.1\r\nHost: incomplete")
            .await
            .expect("write partial request");
        for _ in 0..3 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            peer.write_all(b"x").await.expect("write trickled header");
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), server)
            .await
            .expect("trickled headers must not extend the deadline")
            .expect("connection driver must finish");
    }

    #[tokio::test(start_paused = true)]
    async fn idle_keepalive_peer_expires_after_response() {
        let (mut peer, server) = test_connection().await;
        peer.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write complete request");
        let mut response = [0u8; 512];
        let size = peer.read(&mut response).await.expect("read response");
        assert!(
            response[..size].starts_with(b"HTTP/1.1 200"),
            "server must respond before idle timeout"
        );
        tokio::time::timeout(HEADER_TIMEOUT + std::time::Duration::from_secs(1), server)
            .await
            .expect("idle keepalive must be evicted")
            .expect("connection driver must finish");
    }

    #[tokio::test]
    async fn authenticating_releases_the_preauth_budget() {
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&slots).try_acquire_owned().expect("available slot");
        let guard = PreauthGuard::new(permit);
        assert!(slots.try_acquire().is_err(), "unauthenticated peer holds the slot");
        guard.admit().await;
        assert!(guard.admitted.is_cancelled(), "admission lifts the deadline");
        assert!(slots.try_acquire().is_ok(), "authenticated peer releases the slot");
    }
}
