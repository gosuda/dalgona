//! HTTP response values, the Hyper body, and bounded request body reads.

use bytes::Bytes;
use hyper::body::{Body, Frame};
use hyper::{Request, Response, StatusCode};
use sonic_rs::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

/// Maximum request body size in bytes.
pub(super) const BODY_LIMIT: usize = 33_554_432;

/// Maximum total retained body bytes across a listener.
pub(super) const BODY_BUDGET: usize = 64 * 1024 * 1024;

/// A buffered body retains its byte reservation until dispatch finishes.
pub(super) struct BufferedBody {
    pub bytes: Bytes,
    _permit: Option<OwnedSemaphorePermit>,
}

/// Request body timeout.
pub(super) const BODY_TIMEOUT: Duration = Duration::from_secs(60);

/// One HTTP response with its headers and body.
pub(crate) struct Resp {
    /// The HTTP status.
    pub status: u16,
    /// The response headers.
    pub headers: Vec<(String, String)>,
    /// The response body.
    pub body: RespBody,
}

/// One HTTP response body.
pub(crate) enum RespBody {
    /// A complete body.
    Full(Bytes),
    /// A live event stream; `None` ends the stream.
    Stream(mpsc::Receiver<Option<String>>),
}

impl Resp {
    /// Builds a JSON response with no extra headers.
    pub(crate) fn json(status: u16, value: &Value) -> Self {
        let text = sonic_rs::to_string(value).unwrap_or_else(|_| "null".to_owned());
        Self {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: RespBody::Full(Bytes::from(text.into_bytes())),
        }
    }

    /// Builds a plain-text response.
    pub(crate) fn text(status: u16, text: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
            body: RespBody::Full(Bytes::from(text.as_bytes().to_vec())),
        }
    }

    /// Builds a router error response with its wire code.
    pub(crate) fn router_error(status: u16, code: &str, message: &str) -> Self {
        Self::json(
            status,
            &sonic_rs::json!({"error": {"code": code, "message": message}}),
        )
    }

    /// Builds a live event-stream response.
    pub(crate) fn stream(receiver: mpsc::Receiver<Option<String>>, content_type: &str) -> Self {
        Self {
            status: 200,
            headers: vec![
                ("content-type".to_owned(), content_type.to_owned()),
                ("cache-control".to_owned(), "no-cache".to_owned()),
            ],
            body: RespBody::Stream(receiver),
        }
    }
}

/// How long an early-rejected body is drained before the socket closes.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Bytes a rejected body may still consume while draining: enough for the
/// rest of any body at the declared limit. A still-sending client keeps
/// writing after this point and the close resets instead — best effort.
const DRAIN_LIMIT: usize = BODY_LIMIT;

/// Reserves the bytes of one frame before copying them into the body buffer.
fn reserve_bytes(budget: &Arc<Semaphore>, size: usize) -> Result<OwnedSemaphorePermit, Resp> {
    let permits = u32::try_from(size)
        .map_err(|_| Resp::text(413, "request body exceeds per-request limit"))?;
    Arc::clone(budget)
        .try_acquire_many_owned(permits)
        .map_err(|_| Resp::text(503, "serve request body budget exhausted"))
}

/// Reads one bounded request body into a single buffer with its timeout.
///
/// `BODY_TIMEOUT` bounds only the read. Every rejection releases the buffered
/// bytes and their byte permits, then drains the rest of the body under the
/// drain's own bounded deadline — outside the read deadline, so a decision
/// reached just before it keeps its response instead of collapsing into 408.
/// That path is the single [`reject_body`] funnel: an early return must not
/// skip it.
pub(super) async fn read_body(
    req: Request<hyper::body::Incoming>,
    budget: &Arc<Semaphore>,
) -> Result<BufferedBody, Resp> {
    let (_parts, mut body) = req.into_parts();
    let read = async {
        let mut out = Vec::new();
        let mut held: Option<OwnedSemaphorePermit> = None;
        while let Some(frame) =
            futures::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
        {
            let Ok(frame) = frame else {
                return Err((out, held, Resp::text(400, "request body is not valid")));
            };
            if let Some(data) = frame.data_ref() {
                if data.len() > BODY_LIMIT - out.len() {
                    let resp = Resp::text(413, &format!("request body exceeds {BODY_LIMIT} bytes"));
                    return Err((out, held, resp));
                }
                if data.is_empty() {
                    continue;
                }
                let permit = match reserve_bytes(budget, data.len()) {
                    Ok(permit) => permit,
                    Err(resp) => return Err((out, held, resp)),
                };
                if out.try_reserve(data.len()).is_err() {
                    let resp = Resp::text(503, "request body allocation failed");
                    return Err((out, held, resp));
                }
                out.extend_from_slice(data);
                if let Some(previous) = &mut held {
                    previous.merge(permit);
                } else {
                    held = Some(permit);
                }
            }
        }
        Ok(BufferedBody {
            bytes: Bytes::from(out),
            _permit: held,
        })
    };
    match tokio::time::timeout(BODY_TIMEOUT, read).await {
        Ok(Ok(buffered)) => Ok(buffered),
        Ok(Err((out, held, resp))) => Err(reject_body(out, held, &mut body, resp).await),
        Err(_) => {
            drain_body(&mut body).await;
            Err(Resp::text(408, "request body was not received within 60 s"))
        }
    }
}

/// Releases a rejected body's buffered bytes and byte permits before the
/// bounded drain, so the budget is not held while the peer finishes sending.
async fn reject_body(
    out: Vec<u8>,
    held: Option<OwnedSemaphorePermit>,
    body: &mut hyper::body::Incoming,
    resp: Resp,
) -> Resp {
    drop(out);
    drop(held);
    drain_body(body).await;
    resp
}

/// Empties a rejected request body so the socket closes with no unread
/// input: closing mid-stream resets the connection on BSD-family systems
/// and the client loses the rejection response entirely.
async fn drain_body(body: &mut hyper::body::Incoming) {
    let mut remaining = DRAIN_LIMIT;
    let drain = async {
        while remaining > 0 {
            let frame =
                futures::future::poll_fn(|cx| std::pin::Pin::new(&mut *body).poll_frame(cx)).await;
            match frame {
                Some(Ok(frame)) => {
                    if let Some(data) = frame.data_ref() {
                        remaining = remaining.saturating_sub(data.len());
                    }
                }
                Some(Err(_)) | None => break,
            }
        }
    };
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, drain).await;
}

/// Converts one wire response into its hyper response.
pub(super) fn into_response(resp: Resp) -> Response<ServeBody> {
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    for (name, value) in &resp.headers {
        builder = builder.header(name.clone(), value.clone());
    }
    let body = match resp.body {
        RespBody::Full(bytes) => ServeBody::Full(Some(bytes)),
        RespBody::Stream(receiver) => ServeBody::Stream(receiver),
    };
    builder.body(body).unwrap_or_else(|_| {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(ServeBody::Full(None))
            .unwrap_or_else(|_| Response::new(ServeBody::Full(None)))
    })
}

/// One hyper response body: complete bytes or a live event stream.
pub(crate) enum ServeBody {
    /// Complete bytes (`None` for empty).
    Full(Option<Bytes>),
    /// A live event stream; `None` ends the stream.
    Stream(mpsc::Receiver<Option<String>>),
}

impl Body for ServeBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut() {
            Self::Full(bytes) => {
                let bytes = bytes.take();
                std::task::Poll::Ready(bytes.map(|data| Ok(Frame::data(data))))
            }
            Self::Stream(receiver) => match receiver.poll_recv(cx) {
                std::task::Poll::Ready(Some(Some(line))) => {
                    std::task::Poll::Ready(Some(Ok(Frame::data(Bytes::from(line.into_bytes())))))
                }
                std::task::Poll::Ready(Some(None) | None) => std::task::Poll::Ready(None),
                std::task::Poll::Pending => std::task::Poll::Pending,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{Barrier, Semaphore};

    use super::{BODY_BUDGET, BufferedBody, Resp, into_response, read_body, reserve_bytes};

    /// How long a test waits for a write or a reply on loopback.
    const WAIT: Duration = Duration::from_secs(30);

    /// Binds a loopback HTTP/1.1 listener that runs `read_body` for each
    /// request on the shared `budget` and answers with `finish` on success.
    /// Returns the accept-loop future and the bound address so each test
    /// drives the server concurrently with its client, the same way
    /// `with_serve` drives the real listener: real sockets, real `Incoming`
    /// bodies, no mock.
    async fn serve_read<F>(
        budget: &Arc<Semaphore>,
        finish: F,
    ) -> (impl Future<Output = ()>, SocketAddr)
    where
        F: Fn(BufferedBody) -> Resp + Clone + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let addr = listener.local_addr().expect("listener address");
        let budget = Arc::clone(budget);
        let server = async move {
            while let Ok((stream, _)) = listener.accept().await {
                let io = hyper_util::rt::TokioIo::new(stream);
                let budget = Arc::clone(&budget);
                let finish = finish.clone();
                let service = hyper::service::service_fn(move |req| {
                    let budget = Arc::clone(&budget);
                    let finish = finish.clone();
                    async move {
                        let resp = match read_body(req, &budget).await {
                            Ok(body) => finish(body),
                            Err(resp) => resp,
                        };
                        Ok::<_, hyper::Error>(into_response(resp))
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            }
        };
        (server, addr)
    }

    /// Writes `head` then `body` on a fresh connection and reads the reply
    /// to EOF.
    async fn exchange(addr: SocketAddr, head: &str, body: &[u8]) -> String {
        let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        client.write_all(head.as_bytes()).await.expect("head write");
        client.write_all(body).await.expect("body write");
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.expect("response read");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn a_buffered_body_holds_its_admission_until_dropped() {
        let budget = Arc::new(Semaphore::new(BODY_BUDGET));
        let seen = Arc::clone(&budget);
        let (server, addr) = serve_read(&budget, move |body| {
            // The byte admission lives as long as the buffered body: the
            // budget refills only when the buffer drops at dispatch end.
            assert_eq!(seen.available_permits(), BODY_BUDGET - body.bytes.len());
            drop(body);
            assert_eq!(seen.available_permits(), BODY_BUDGET);
            Resp::text(200, "read")
        })
        .await;
        let text = tokio::select! {
            () = server => panic!("server stopped early"),
            text = exchange(
                addr,
                "POST /x HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\ncontent-length: 4096\r\n\r\n",
                &[b'x'; 4096],
            ) => text,
        };
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    }

    /// A mid-body admission failure must still drain the rest of the body:
    /// closing the socket with unread input resets it and the client loses
    /// the 503 entirely. The whole declared body must land and the response
    /// must still arrive.
    #[tokio::test]
    async fn a_budget_rejection_drains_so_the_response_arrives() {
        const TOTAL: usize = 16 * 1024 * 1024;
        let budget = Arc::new(Semaphore::new(BODY_BUDGET));
        // A concurrent holder leaves only ~100 KiB free: some early frame
        // admits, then admission fails while the client is still sending.
        let blocker = Arc::clone(&budget)
            .try_acquire_many_owned(u32::try_from(BODY_BUDGET - 100_000).expect("fits u32"))
            .expect("budget blocker");
        let (server, addr) = serve_read(&budget, |_| Resp::text(200, "read")).await;
        let text = tokio::select! {
            () = server => panic!("server stopped early"),
            text = tokio::time::timeout(WAIT, async {
                exchange(
                    addr,
                    &format!("POST /x HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\ncontent-length: {TOTAL}\r\n\r\n"),
                    &vec![b'x'; TOTAL],
                )
                .await
            }) => text.expect("exchange completes in time"),
        };
        assert!(text.starts_with("HTTP/1.1 503"), "{text}");
        assert!(
            text.contains("serve request body budget exhausted"),
            "{text}"
        );
        // The rejected request released the admission it held.
        assert_eq!(budget.available_permits(), 100_000);
        drop(blocker);
        assert_eq!(budget.available_permits(), BODY_BUDGET);
    }

    /// Admission failure just before the read deadline keeps its 503: the
    /// drain runs outside `BODY_TIMEOUT`, so the deadline cannot cancel the
    /// rejection into a 408.
    #[tokio::test]
    async fn a_late_budget_rejection_keeps_its_response() {
        let budget = Arc::new(Semaphore::new(BODY_BUDGET));
        let blocker = Arc::clone(&budget)
            .try_acquire_many_owned(u32::try_from(BODY_BUDGET - 100_000).expect("fits u32"))
            .expect("budget blocker");
        let (server, addr) = serve_read(&budget, |_| Resp::text(200, "read")).await;
        tokio::time::pause();
        let text = tokio::select! {
            () = server => panic!("server stopped early"),
            text = async {
                let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
                client
                    .write_all(
                        "POST /x HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\ncontent-length: 400000\r\n\r\n"
                            .as_bytes(),
                    )
                    .await
                    .expect("head write");
                client.write_all(&[b'x'; 64]).await.expect("first frame");
                // The budget fails on a frame that lands one second before
                // the 60 s read deadline.
                tokio::time::sleep(Duration::from_secs(59)).await;
                client
                    .write_all(&vec![b'x'; 200_000])
                    .await
                    .expect("late frame");
                let mut bytes = Vec::new();
                client
                    .read_to_end(&mut bytes)
                    .await
                    .expect("503 response read");
                String::from_utf8_lossy(&bytes).into_owned()
            } => text,
        };
        tokio::time::resume();
        assert!(text.starts_with("HTTP/1.1 503"), "{text}");
        // The rejected request released the admission it held.
        assert_eq!(budget.available_permits(), 100_000);
        drop(blocker);
        assert_eq!(budget.available_permits(), BODY_BUDGET);
    }

    /// A body abandoned by its client releases its admission when the read
    /// times out.
    #[tokio::test]
    async fn a_stalled_body_releases_its_admission_on_timeout() {
        let budget = Arc::new(Semaphore::new(BODY_BUDGET));
        let (server, addr) = serve_read(&budget, |_| Resp::text(200, "read")).await;
        let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        client
            .write_all(
                "POST /x HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\ncontent-length: 4096\r\n\r\n"
                    .as_bytes(),
            )
            .await
            .expect("request write");
        client.write_all(&[b'x'; 64]).await.expect("partial body");
        let mut bytes = Vec::new();
        tokio::time::pause();
        tokio::select! {
            () = server => panic!("server stopped early"),
            read = client.read_to_end(&mut bytes) => read.expect("408 response read"),
        };
        tokio::time::resume();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(text.starts_with("HTTP/1.1 408"), "{text}");
        assert_eq!(budget.available_permits(), BODY_BUDGET);
    }

    #[tokio::test]
    async fn concurrent_body_admission_has_a_fixed_aggregate_ceiling() {
        const CLIENTS: usize = 256;
        const PER_REQUEST: usize = 1024;
        const BUDGET: usize = 32 * PER_REQUEST;
        let budget = Arc::new(Semaphore::new(BUDGET));
        let start = Arc::new(Barrier::new(CLIENTS + 1));
        let finish = Arc::new(Barrier::new(CLIENTS + 1));
        let clients = (0..CLIENTS).map(|_| {
            let budget = Arc::clone(&budget);
            let start = Arc::clone(&start);
            let finish = Arc::clone(&finish);
            async move {
                start.wait().await;
                let held = reserve_bytes(&budget, PER_REQUEST).ok();
                finish.wait().await;
                held
            }
        });
        let run = futures::future::join_all(clients);
        let arms = async {
            start.wait().await;
            finish.wait().await;
        };
        let (admitted, ()) = tokio::join!(run, arms);
        assert_eq!(
            admitted.iter().filter(|permit| permit.is_some()).count(),
            32
        );
        assert_eq!(budget.available_permits(), 0);
        drop(admitted);
        assert_eq!(budget.available_permits(), BUDGET);
    }
}
