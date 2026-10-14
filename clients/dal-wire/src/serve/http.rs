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
    let permits = u32::try_from(size).expect("frame fits within BODY_LIMIT");
    Arc::clone(budget)
        .try_acquire_many_owned(permits)
        .map_err(|_| Resp::text(503, "serve request body budget exhausted"))
}

/// Reads one bounded request body into a single buffer with its timeout.
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
                return Err(Resp::text(400, "request body is not valid"));
            };
            if let Some(data) = frame.data_ref() {
                if data.len() > BODY_LIMIT - out.len() {
                    // Release already-buffered bytes before draining the invalid body.
                    drop(out);
                    drop(held);
                    drain_body(&mut body).await;
                    return Err(Resp::text(
                        413,
                        &format!("request body exceeds {BODY_LIMIT} bytes"),
                    ));
                }
                if data.is_empty() {
                    continue;
                }
                let permit = reserve_bytes(budget, data.len())?;
                if out.try_reserve(data.len()).is_err() {
                    return Err(Resp::text(503, "request body allocation failed"));
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
        Ok(result) => result,
        Err(_) => Err(Resp::text(408, "request body was not received within 60 s")),
    }
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
    use std::sync::Arc;

    use tokio::sync::{Barrier, Semaphore};

    use super::reserve_bytes;

    #[tokio::test]
    async fn concurrent_body_admission_has_a_fixed_aggregate_ceiling() {
        const CLIENTS: usize = 256;
        const PER_REQUEST: usize = 1024;
        const BUDGET: usize = 32 * PER_REQUEST;
        let budget = Arc::new(Semaphore::new(BUDGET));
        let start = Arc::new(Barrier::new(CLIENTS + 1));
        let finish = Arc::new(Barrier::new(CLIENTS + 1));
        let mut tasks = Vec::with_capacity(CLIENTS);
        for _ in 0..CLIENTS {
            let budget = Arc::clone(&budget);
            let start = Arc::clone(&start);
            let finish = Arc::clone(&finish);
            tasks.push(tokio::spawn(async move {
                start.wait().await;
                let held = reserve_bytes(&budget, PER_REQUEST).ok();
                finish.wait().await;
                held
            }));
        }
        start.wait().await;
        finish.wait().await;

        let mut admitted = Vec::new();
        for task in tasks {
            admitted.push(task.await.expect("admission task finished"));
        }
        assert_eq!(admitted.iter().filter(|permit| permit.is_some()).count(), 32);
        assert_eq!(budget.available_permits(), 0);
        drop(admitted);
        assert_eq!(budget.available_permits(), BUDGET);
    }
}
