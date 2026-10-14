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

/// Request body timeout.
pub(super) const BODY_TIMEOUT: Duration = Duration::from_secs(60);

/// Aggregate listener budget: 64 MiB in 4 KiB permits.
pub(super) const BODY_BUDGET_PERMITS: usize = 16_384;
const BODY_BUDGET_UNIT: usize = 4_096;
const BUDGET_EXHAUSTED: &str = "listener request body budget exhausted";

/// A body whose memory reservation stays alive during request dispatch.
pub(super) struct BufferedBody {
    /// The body contents.
    pub(super) bytes: Bytes,
    /// Permits for its allocated buffer capacity.
    _capacity: Option<OwnedSemaphorePermit>,
}

impl BufferedBody {
    /// Creates an empty body for a route that does not read one.
    pub(super) fn empty() -> Self {
        Self {
            bytes: Bytes::new(),
            _capacity: None,
        }
    }
}

/// One HTTP response with its headers and body.
#[derive(Debug)]
pub(crate) struct Resp {
    /// The HTTP status.
    pub status: u16,
    /// The response headers.
    pub headers: Vec<(String, String)>,
    /// The response body.
    pub body: RespBody,
}

/// One HTTP response body.
#[derive(Debug)]
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

/// Reserves the entire next buffer allocation before any growth.
///
/// While resizing, the old and new capacities both hold permits. A failed
/// reservation rejects the request instead of waiting with a partial body.
fn reserve_capacity(
    budget: &Arc<Semaphore>,
    capacity: usize,
) -> Result<OwnedSemaphorePermit, Resp> {
    let permits = u32::try_from(capacity.div_ceil(BODY_BUDGET_UNIT))
        .map_err(|_| Resp::text(503, BUDGET_EXHAUSTED))?;
    Arc::clone(budget)
        .try_acquire_many_owned(permits)
        .map_err(|_| Resp::text(503, BUDGET_EXHAUSTED))
}

/// Reads one bounded request body into a single growable buffer.
///
/// The capacity permit is returned with the bytes so it remains held until
/// the route handler is finished using the body.
pub(super) async fn read_body(
    req: Request<hyper::body::Incoming>,
    budget: &Arc<Semaphore>,
) -> Result<BufferedBody, Resp> {
    let (_parts, mut incoming) = req.into_parts();
    let mut buffer = Vec::new();
    let mut capacity_permit = None;
    let read = async {
        while let Some(frame) =
            futures::future::poll_fn(|cx| std::pin::Pin::new(&mut incoming).poll_frame(cx)).await
        {
            let Ok(frame) = frame else {
                drain_body(&mut incoming).await;
                return Err(Resp::text(400, "request body is not valid"));
            };
            let Some(data) = frame.data_ref() else {
                continue;
            };
            if data.len() > BODY_LIMIT - buffer.len() {
                drain_body(&mut incoming).await;
                return Err(Resp::text(
                    413,
                    &format!("request body exceeds {BODY_LIMIT} bytes"),
                ));
            }
            let next_len = buffer.len() + data.len();
            if next_len > buffer.capacity() {
                let capacity = next_len.next_power_of_two().max(BODY_BUDGET_UNIT);
                let new_permit = match reserve_capacity(budget, capacity) {
                    Ok(permit) => permit,
                    Err(resp) => {
                        drain_body(&mut incoming).await;
                        return Err(resp);
                    }
                };
                if buffer.try_reserve_exact(capacity - buffer.len()).is_err() {
                    drain_body(&mut incoming).await;
                    return Err(Resp::text(503, "request body allocation failed"));
                }
                // Releasing the old permit is safe only after resizing completes.
                capacity_permit = Some(new_permit);
            }
            buffer.extend_from_slice(data);
        }
        Ok(())
    };
    match tokio::time::timeout(BODY_TIMEOUT, read).await {
        Ok(Ok(())) => {}
        Ok(Err(resp)) => return Err(resp),
        Err(_) => {
            drain_body(&mut incoming).await;
            return Err(Resp::text(408, "request body was not received within 60 s"));
        }
    }
    Ok(BufferedBody {
        bytes: Bytes::from(buffer),
        _capacity: capacity_permit,
    })
}

/// Empties an unread request body so the socket closes with no pending
/// input: closing mid-stream resets the connection on BSD-family systems
/// and the client loses the response entirely. The drain never allocates,
/// so it stays outside the shared allocation budget; an upload that never
/// finishes waits out the drain timeout and the response still goes out.
pub(super) async fn drain_body(body: &mut hyper::body::Incoming) {
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

    use tokio::sync::Semaphore;

    use super::{BODY_BUDGET_PERMITS, BODY_BUDGET_UNIT, reserve_capacity};

    #[test]
    fn concurrent_body_reservations_have_a_fixed_aggregate_ceiling() {
        let budget = Arc::new(Semaphore::new(BODY_BUDGET_PERMITS));
        let one_mib = 1024 * 1024;
        let count = BODY_BUDGET_PERMITS * BODY_BUDGET_UNIT / one_mib;
        let leases: Vec<_> = (0..count)
            .map(|_| reserve_capacity(&budget, one_mib).expect("budget admits 64 MiB"))
            .collect();
        assert_eq!(budget.available_permits(), 0);
        assert_eq!(
            reserve_capacity(&budget, BODY_BUDGET_UNIT)
                .err()
                .map(|resp| resp.status),
            Some(503)
        );
        drop(leases);
        assert_eq!(budget.available_permits(), BODY_BUDGET_PERMITS);
    }

    #[test]
    fn resize_counts_both_the_old_and_new_allocations() {
        let budget = Arc::new(Semaphore::new(BODY_BUDGET_PERMITS));
        let old = reserve_capacity(&budget, 32 * 1024 * 1024).expect("old buffer");
        let competing = reserve_capacity(&budget, 16 * 1024 * 1024).expect("competing buffer");
        assert_eq!(
            reserve_capacity(&budget, 32 * 1024 * 1024)
                .err()
                .map(|resp| resp.status),
            Some(503)
        );
        drop(competing);
        let replacement = reserve_capacity(&budget, 32 * 1024 * 1024)
            .expect("replacement fits with old allocation");
        assert_eq!(budget.available_permits(), 0);
        drop(old);
        drop(replacement);
        assert_eq!(budget.available_permits(), BODY_BUDGET_PERMITS);
    }
}
