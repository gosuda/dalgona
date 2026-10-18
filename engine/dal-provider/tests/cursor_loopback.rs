#![expect(clippy::expect_used, reason = "loopback proof fails loudly")]
#![expect(clippy::panic, reason = "loopback proof asserts duplex ordering")]
#![expect(
    clippy::disallowed_methods,
    reason = "loopback proof drives real listeners on background tasks"
)]
//! Bidirectional HTTP/2 loopback proof for the provider HTTP transport.
//!
//! Exercises bidirectional HTTP/2 and cancellation through [`send`] and a
//! loopback hyper server. The request remains open while response frames
//! arrive; cancellation resets the stream. Cleartext uses
//! [`build_client_http2_prior_knowledge`]. The stock [`build_client`]
//! control must fail before a handler runs. [`Family::Chat`] is only the
//! transport carrier. These tests do not cover Cursor Connect framing,
//! adapter mapping, or durable replay.

use std::{
    convert::Infallible,
    future::poll_fn,
    io,
    net::{Ipv4Addr, SocketAddr},
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use dal_core::Family;
use dal_provider::{Exchange, build_client, build_client_http2_prior_knowledge, send};
use futures::SinkExt as _;
use hyper::{
    Method, Request, Response, StatusCode, Version,
    body::{Body, Frame, Incoming},
    header::{CONTENT_TYPE, HeaderValue},
    server::conn::http2,
    service::service_fn,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::{
    net::TcpListener,
    sync::mpsc,
    task::JoinSet,
    time::{sleep, timeout},
};

/// Bound on every wait the happy path makes; a healthy run needs milliseconds.
const PROMPT: Duration = Duration::from_secs(5);

/// Shared loopback state: an append-only event log, the byte count of every
/// upload the server read, and the two cancel-observation flags for the
/// `/stall` case.
#[derive(Default)]
struct Shared {
    log: Mutex<Vec<String>>,
    uploaded: AtomicUsize,
    stall_request_closed: AtomicBool,
    stall_response_dropped: AtomicBool,
}

fn record(shared: &Shared, line: &str) {
    shared
        .log
        .lock()
        .expect("loopback log is never poisoned")
        .push(line.to_owned());
}

/// How the request direction of one stream ended, as the server saw it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UploadEnd {
    /// The client half-closed the stream: a clean `END_STREAM`.
    Eos,
    /// The stream was reset (`RST_STREAM`) before the client half-closed it.
    Reset,
}

/// A response body fed by the handler task: each send is one flushed chunk,
/// dropping the sender ends the stream. The queue is bounded (8): the scripts
/// send at most three frames, so sends never apply backpressure, and any
/// fuller queue would await rather than grow memory.
struct DuplexBody {
    rx: mpsc::Receiver<Bytes>,
}

impl Body for DuplexBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        match self.get_mut().rx.poll_recv(cx) {
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn ended_body() -> DuplexBody {
    let (_, rx) = mpsc::channel::<Bytes>(8);
    DuplexBody { rx }
}

async fn route(
    request: Request<Incoming>,
    shared: Arc<Shared>,
) -> Result<Response<DuplexBody>, Infallible> {
    record(&shared, &format!("protocol:{:?}", request.version()));
    match (request.method(), request.uri().path()) {
        (&Method::POST, "/duplex") => duplex(request, shared).await,
        (&Method::POST, "/stall") => stall(request, shared).await,
        _ => {
            let mut response = Response::new(ended_body());
            *response.status_mut() = StatusCode::NOT_FOUND;
            Ok(response)
        }
    }
}

/// Full-duplex case: `delta-1` goes out before the request body is touched,
/// `delta-2` plus the single terminal frame follow only after the upload
/// ends. The send order in the log is the interleaving proof.
async fn duplex(
    request: Request<Incoming>,
    shared: Arc<Shared>,
) -> Result<Response<DuplexBody>, Infallible> {
    let (tx, rx) = mpsc::channel::<Bytes>(8);
    tx.send(Bytes::from_static(b"delta-1\n"))
        .await
        .expect("loopback receiver is alive");
    record(&shared, "sent:delta-1");
    let (_parts, body) = request.into_parts();
    tokio::spawn(async move {
        let end = read_upload(body, &shared, "got").await;
        record(&shared, &format!("request-end:{end:?}"));
        if tx.send(Bytes::from_static(b"delta-2\n")).await.is_err() {
            record(&shared, "response-dropped");
            return;
        }
        record(&shared, "sent:delta-2");
        if tx
            .send(Bytes::from_static(b"terminal: end\n"))
            .await
            .is_err()
        {
            record(&shared, "response-dropped");
            return;
        }
        record(&shared, "sent:terminal");
    });
    Ok(event_stream(rx))
}

/// Cancel case: `delta-1` goes out, then the handler only watches. When the
/// client cancels, the request-body read ends and the response receiver
/// drops; each side is recorded exactly once, and the request side records how
/// it ended so a clean half-close cannot pass for a cancel.
async fn stall(
    request: Request<Incoming>,
    shared: Arc<Shared>,
) -> Result<Response<DuplexBody>, Infallible> {
    let (tx, rx) = mpsc::channel::<Bytes>(8);
    tx.send(Bytes::from_static(b"delta-1\n"))
        .await
        .expect("loopback receiver is alive");
    record(&shared, "stall-sent:delta-1");
    let (_parts, body) = request.into_parts();
    tokio::spawn(async move {
        let end = read_upload(body, &shared, "stall-got").await;
        record(&shared, &format!("stall-request-closed:{end:?}"));
        shared.stall_request_closed.store(true, Ordering::SeqCst);
        if timeout(PROMPT, tx.closed()).await.is_ok() {
            record(&shared, "stall-response-dropped");
            shared.stall_response_dropped.store(true, Ordering::SeqCst);
        } else {
            record(&shared, "stall-response-still-open");
        }
    });
    Ok(event_stream(rx))
}

fn event_stream(rx: mpsc::Receiver<Bytes>) -> Response<DuplexBody> {
    let mut response = Response::new(DuplexBody { rx });
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
}

/// Reads one upload until it ends, logging each data frame under `tag`. A
/// frame error is a reset stream: nothing else fails a loopback read.
async fn read_upload(body: Incoming, shared: &Arc<Shared>, tag: &str) -> UploadEnd {
    let mut body = pin!(body);
    while let Some(frame) = poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
        let Ok(frame) = frame else {
            return UploadEnd::Reset;
        };
        if let Some(data) = frame.data_ref() {
            shared.uploaded.fetch_add(data.len(), Ordering::SeqCst);
            record(shared, &format!("{tag}:{}-bytes", data.len()));
        }
    }
    UploadEnd::Eos
}

/// Serves one accepted connection as HTTP/2 and nothing else: a peer that does
/// not open with the HTTP/2 connection preface is dropped before any handler
/// runs.
async fn serve_stream(stream: tokio::net::TcpStream, shared: Arc<Shared>) {
    let service = service_fn(move |request| {
        let shared = Arc::clone(&shared);
        async move { route(request, shared).await }
    });
    let _ = http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

/// Ephemeral loopback server: `hyper` HTTP/2 connections over `hyper-util`
/// `TokioIo`, mirroring the replay server's shape. Dropping it aborts the
/// accept loop and every connection it spawned.
struct Loopback {
    addr: SocketAddr,
    tasks: JoinSet<()>,
    shared: Arc<Shared>,
}

impl Loopback {
    async fn start() -> Self {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("loopback listener binds");
        let addr = listener
            .local_addr()
            .expect("loopback listener has an address");
        assert!(addr.ip().is_loopback(), "proof stays on loopback: {addr}");
        let shared = Arc::new(Shared::default());
        let mut tasks = JoinSet::new();
        let serving = Arc::clone(&shared);
        tasks.spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((stream, _)) = listener.accept().await {
                connections.spawn(serve_stream(stream, Arc::clone(&serving)));
            }
        });
        Self {
            addr,
            tasks,
            shared,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn snapshot(&self) -> Vec<String> {
        self.shared
            .log
            .lock()
            .expect("loopback log is never poisoned")
            .clone()
    }

    async fn shutdown(mut self) {
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }
}

fn count(log: &[String], marker: &str) -> usize {
    log.iter().filter(|line| line.as_str() == marker).count()
}

fn position(log: &[String], marker: &str) -> usize {
    log.iter()
        .position(|line| line == marker)
        .unwrap_or_else(|| panic!("server logged {marker}: {log:?}"))
}

/// The server saw exactly one request and it arrived as HTTP/2.
fn assert_server_saw_h2(log: &[String]) {
    let h2 = format!("protocol:{:?}", Version::HTTP_2);
    assert_eq!(
        count(log, &h2),
        1,
        "the one request reached the handler over HTTP/2: {log:?}"
    );
    assert_eq!(
        log.iter()
            .filter(|line| line.starts_with("protocol:"))
            .count(),
        1,
        "the server saw no other request: {log:?}"
    );
}

/// Reads response chunks until `needle` has arrived whole, appending every
/// byte to `raw`. Chunk splits must not matter, so the search runs over the
/// accumulated bytes. Panics when the stream ends first: ending early would
/// prove the server half-closed while the upload was open.
async fn read_until(
    response: &mut reqwest::Response,
    needle: &[u8],
    what: &str,
    raw: &mut Vec<u8>,
) {
    loop {
        let chunk = timeout(PROMPT, response.chunk())
            .await
            .expect("loopback frame arrives promptly")
            .expect("loopback chunk reads cleanly");
        match chunk {
            Some(chunk) => {
                raw.extend_from_slice(&chunk);
                if raw.windows(needle.len()).any(|window| window == needle) {
                    return;
                }
            }
            None => panic!("response ended before {what}"),
        }
    }
}

/// Reads the rest of the response until the server ends it.
async fn drain(response: &mut reqwest::Response, raw: &mut Vec<u8>) {
    while let Some(chunk) = timeout(PROMPT, response.chunk())
        .await
        .expect("remaining loopback frames arrive promptly")
        .expect("loopback chunk reads cleanly")
    {
        raw.extend_from_slice(&chunk);
    }
}

/// Opens one exchange through [`send`] over HTTP/2 and returns the upload
/// sender plus the response, whose head arrived while the upload was still
/// open. Only the first chunk is queued before the head is awaited, so
/// anything received before the later sends left while the upload was open.
async fn duplex_response(
    url: &str,
) -> (
    futures::channel::mpsc::Sender<Result<Bytes, io::Error>>,
    reqwest::Response,
) {
    let (mut req_tx, req_rx) = futures::channel::mpsc::channel::<Result<Bytes, io::Error>>(8);
    req_tx
        .send(Ok(Bytes::from_static(b"req-a")))
        .await
        .expect("buffer the first request chunk");
    let response = timeout(
        PROMPT,
        send(
            Family::Chat,
            build_client_http2_prior_knowledge()
                .post(url)
                .body(reqwest::Body::wrap_stream(req_rx)),
            "dalgon/0-test",
            Exchange::Stream,
            sleep,
        ),
    )
    .await
    .expect("response headers arrive promptly")
    .expect("response headers arrive while the upload is still open");
    assert_eq!(
        response.version(),
        reqwest::Version::HTTP_2,
        "the client negotiated HTTP/2"
    );
    assert!(
        response.status().is_success(),
        "loopback route answered: {}",
        response.status()
    );
    (req_tx, response)
}

#[tokio::test]
async fn h2_duplex_request_open_while_responses_flow() {
    let server = Loopback::start().await;
    let (mut req_tx, mut response) = duplex_response(&server.url("/duplex")).await;

    // delta-1 must arrive while the upload is still open: the remaining
    // chunks are queued only after it lands, and queueing on a closed stream
    // fails instead of passing silently.
    let mut raw = Vec::new();
    read_until(&mut response, b"delta-1\n", "delta-1 arrives", &mut raw).await;
    req_tx
        .send(Ok(Bytes::from_static(b"req-b")))
        .await
        .expect("upload still open while responses flow");
    req_tx
        .send(Ok(Bytes::from_static(b"req-c")))
        .await
        .expect("upload still open while responses flow");
    drop(req_tx);
    drain(&mut response, &mut raw).await;

    let text = String::from_utf8(raw).expect("loopback frames are UTF-8");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        vec!["delta-1", "delta-2", "terminal: end"],
        "interleaved frames keep their order"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("terminal:"))
            .count(),
        1,
        "exactly one terminal outcome"
    );

    let log = server.snapshot();
    assert_server_saw_h2(&log);
    let first_sent = position(&log, "sent:delta-1");
    let request_end = position(&log, "request-end:Eos");
    let second_sent = position(&log, "sent:delta-2");
    let terminal_sent = position(&log, "sent:terminal");
    assert!(
        first_sent < request_end,
        "response flowed while the request stream was still open: {log:?}"
    );
    assert!(
        request_end < second_sent && second_sent < terminal_sent,
        "the rest of the response followed the end of the upload: {log:?}"
    );
    assert_eq!(
        server.shared.uploaded.load(Ordering::SeqCst),
        "req-areq-breq-c".len(),
        "every upload byte reached the server: {log:?}"
    );
    for marker in ["request-end:Eos", "sent:delta-2", "sent:terminal"] {
        assert_eq!(count(&log, marker), 1, "{marker} happened once: {log:?}");
    }
    assert!(
        !log.iter().any(|line| line == "response-dropped"),
        "no direction closed early: {log:?}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn h2_cancel_closes_both_directions_with_single_terminal() {
    let server = Loopback::start().await;
    let (mut req_tx, mut response) = duplex_response(&server.url("/stall")).await;

    let mut raw = Vec::new();
    read_until(&mut response, b"delta-1\n", "delta-1 arrives", &mut raw).await;
    let text = String::from_utf8(raw).expect("loopback frames are UTF-8");
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("terminal:"))
            .count(),
        0,
        "cancel happens before any terminal frame: {text:?}"
    );

    // Cancel while the upload is still open: the client aborts it with an
    // error instead of half-closing, which resets the stream. The upload is
    // never ended cleanly, so a request direction that closes on the server
    // can only have closed because of the cancel. No terminal frame was
    // delivered and none may follow a reset stream, so the cancellation
    // itself is the single terminal outcome.
    req_tx
        .send(Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "client cancelled",
        )))
        .await
        .expect("upload still open at cancel");
    let after = timeout(PROMPT, response.chunk())
        .await
        .expect("cancel reaches the response reader promptly");
    assert!(
        !matches!(after, Ok(Some(_))),
        "no frame follows a cancel: {after:?}"
    );
    drop(response);
    drop(req_tx);

    let closed = timeout(PROMPT * 2, async {
        while !(server.shared.stall_request_closed.load(Ordering::SeqCst)
            && server.shared.stall_response_dropped.load(Ordering::SeqCst))
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "cancel closes both directions: {:?}",
        server.snapshot()
    );
    let log = server.snapshot();
    assert_server_saw_h2(&log);
    assert_eq!(
        count(&log, "stall-request-closed:Reset"),
        1,
        "the request direction closed once, by reset: {log:?}"
    );
    assert_eq!(
        count(&log, "stall-response-dropped"),
        1,
        "the response direction closed once: {log:?}"
    );
    assert!(
        !log.iter().any(|line| {
            line == "stall-request-closed:Eos"
                || line == "stall-response-still-open"
                || line.contains("terminal")
        }),
        "cancel was the only terminal outcome: {log:?}"
    );

    server.shutdown().await;
}

/// Control: the stock [`build_client`] client on cleartext has no ALPN, so it
/// speaks HTTP/1.1, and the HTTP/2-only server refuses it before any handler
/// runs. Together with the two tests above this shows the HTTP/2 path is
/// negotiated HTTP/2 and not a fallback.
#[tokio::test]
async fn stock_cleartext_client_cannot_reach_the_h2_only_server() {
    let server = Loopback::start().await;
    let outcome = timeout(
        PROMPT,
        send(
            Family::Chat,
            build_client().post(server.url("/duplex")).body("req-a"),
            "dalgon/0-test",
            Exchange::Stream,
            sleep,
        ),
    )
    .await
    .expect("the refused exchange fails promptly");
    let error = outcome.expect_err("an HTTP/1.1 client cannot talk to an HTTP/2-only server");
    assert!(
        server.snapshot().is_empty(),
        "no handler ran for the refused client ({error}): {:?}",
        server.snapshot()
    );

    server.shutdown().await;
}
