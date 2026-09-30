//! HTTP response values, the Hyper body, and bounded request body reads.

use bytes::Bytes;
use hyper::body::{Body, Frame};
use hyper::{Request, Response, StatusCode};
use sonic_rs::Value;
use std::time::Duration;
use tokio::sync::mpsc;

/// Maximum request body size in bytes.
pub(super) const BODY_LIMIT: usize = 33_554_432;

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

/// Reads one bounded request body with its timeout.
pub(super) async fn read_body(req: Request<hyper::body::Incoming>) -> Result<Bytes, Resp> {
    let (_parts, mut body) = req.into_parts();
    let mut collected = 0usize;
    let mut chunks = Vec::new();
    let read = async {
        while let Some(frame) =
            futures::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
        {
            let frame = frame.map_err(|_| Resp::text(400, "request body is not valid"))?;
            if let Some(data) = frame.data_ref() {
                collected += data.len();
                if collected > BODY_LIMIT {
                    return Err(Resp::text(
                        413,
                        &format!("request body exceeds {BODY_LIMIT} bytes"),
                    ));
                }
                chunks.push(data.clone());
            }
        }
        Ok(chunks)
    };
    let chunks: Vec<Bytes> = match tokio::time::timeout(BODY_TIMEOUT, read).await {
        Ok(Ok(chunks)) => chunks,
        Ok(Err(resp)) => return Err(resp),
        Err(_) => {
            return Err(Resp::text(408, "request body was not received within 60 s"));
        }
    };
    let mut out = Vec::with_capacity(collected);
    for chunk in chunks {
        out.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(out))
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
