// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::{
    future::Future,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
};

use reqwest::{
    Url,
    header::{HeaderName, HeaderValue},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

use super::{
    CallDeadline, HttpTransport, auth as token_auth,
    protocol::{PROTOCOL_VERSION, outbound_headers, request_body},
};
use crate::{
    mcp::{Budgets, tools::Key},
    work::support::FakeServices,
};

fn test_key() -> Key {
    Key {
        session: dal_core::SessionId::new_v7(),
        skill: "test-skill".to_owned(),
        server: "test-server".to_owned(),
    }
}

async fn request_parts(stream: TcpStream) -> (String, String, BufReader<TcpStream>) {
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let count = reader.read_line(&mut line).await.expect("request headers");
        if count == 0 {
            break;
        }
        headers.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or_default();
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.expect("request body");
    (
        headers,
        String::from_utf8(body).expect("UTF-8 request body"),
        reader,
    )
}

async fn fixture_response(response: String) -> (Url, impl Future<Output = (String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let url = Url::parse(&format!("http://{address}/mcp")).expect("fixture URL");
    let server = async move {
        let (stream, _) = listener.accept().await.expect("fixture request");
        let (headers, body, mut reader) = request_parts(stream).await;
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .expect("fixture response");
        (headers, body)
    };
    (url, server)
}

fn test_transport(url: Url) -> HttpTransport {
    HttpTransport::new(
        test_key(),
        url,
        PathBuf::from("/tmp/dalgona-http-fixture-tokens.json"),
        "0.1.0".to_owned(),
        &Budgets::default(),
        Arc::new(token_auth::RefreshCoordinator::new()),
    )
    .expect("transport")
}

#[tokio::test]
async fn posts_mcp_headers_and_correlates_a_retried_id() {
    let body = "{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"ok\":true}}";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: fixture-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, server) = fixture_response(response).await;
    let transport = test_transport(url);
    let request = request_body(
        9,
        "tools/call",
        "{\"name\":\"echo\"}",
        PROTOCOL_VERSION,
        "0.1.0",
    )
    .expect("request envelope");
    let extra = [(
        HeaderName::from_static("mcp-param-x-trace"),
        HeaderValue::from_static("trace-1"),
    )];
    let request_headers = outbound_headers(
        PROTOCOL_VERSION,
        Some("tools/call"),
        Some("echo"),
        &extra,
        None,
    )
    .expect("MCP headers");
    let deadline = CallDeadline::new(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(5),
    );
    let cancel = CancellationToken::new();
    let send_cx = super::SendCx {
        version: PROTOCOL_VERSION,
        token: None,
        cancel: &cancel,
        call_cancel: None,
        deadline: &deadline,
    };
    let (response, (headers, sent_body)) = tokio::join!(
        transport.send_once(
            request.as_str(),
            Some("tools/call"),
            Some("echo"),
            &extra,
            &send_cx,
        ),
        server,
    );
    let response = response.expect("HTTP response");
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("mcp-method: tools/call")
    );
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("mcp-protocol-version: 2026-07-28")
    );
    assert!(headers.to_ascii_lowercase().contains("mcp-name: echo"));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("mcp-param-x-trace: trace-1")
    );
    assert!(sent_body.contains("\"progressToken\":\"t-9\""));
    assert!(request_headers.contains_key("mcp-protocol-version"));
    transport
        .capture_session(&response)
        .await
        .expect("session header");
    let bytes = transport
        .read_body(
            response,
            super::RESPONSE_MAX,
            &CancellationToken::new(),
            None,
            &deadline,
        )
        .await
        .expect("bounded JSON response");
    let text = std::str::from_utf8(&bytes).expect("JSON UTF-8");
    let normalized = super::response_for_id(text, 9, 4).expect("correlated response");
    assert!(normalized.as_str().contains("\"id\":4"));
    assert_eq!(
        transport.session_id.lock().await.as_deref(),
        Some("fixture-session")
    );
}

#[tokio::test]
async fn consumes_request_scoped_sse_incrementally_and_extends_on_progress() {
    let body = concat!(
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"_meta\":{\"progressToken\":\"t-9\"}}}\n\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"ok\":true}}\n\n"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, server) = fixture_response(response).await;
    let transport = test_transport(url);
    let (response, _) = tokio::join!(
        async {
            let headers = outbound_headers(PROTOCOL_VERSION, Some("tools/call"), None, &[], None)
                .expect("headers");
            transport
                .client
                .post(transport.url.clone())
                .headers(headers)
                .body("{}".to_owned())
                .send()
                .await
                .expect("SSE response")
        },
        server,
    );
    let mut deadline = CallDeadline::new(
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(4),
    );
    let cancel = CancellationToken::new();
    let initial_deadline = deadline.expires;
    let response = transport
        .read_event_stream(
            response,
            &mut super::StreamCx {
                request_id: 9,
                original_id: 4,
                version: PROTOCOL_VERSION,
                call_cancel: None,
                token: None,
                cancel: &cancel,
                deadline: &mut deadline,
            },
        )
        .await
        .expect("SSE reply");
    assert!(response.as_str().contains("\"id\":4"));
    assert!(deadline.expires > initial_deadline);
}

/// A server that answers an in-stream request's POST with silence must not
/// hold the call past its deadline.
#[tokio::test]
async fn a_stalled_answer_to_a_server_request_ends_at_the_call_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let url = Url::parse(&format!("http://{address}/mcp")).expect("fixture URL");
    let transport = test_transport(url);
    let event = "data: {\"jsonrpc\":\"2.0\",\"id\":\"srv-1\",\"method\":\"sampling/createMessage\",\"params\":{}}\n\n";
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{event}"
    );
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let server = async {
        let (stream, _) = listener.accept().await.expect("stream request");
        let (_headers, _body, mut stream_reader) = request_parts(stream).await;
        stream_reader
            .get_mut()
            .write_all(head.as_bytes())
            .await
            .expect("stream head and event");
        // The client answers the in-stream request on a second connection;
        // the fixture reads that answer and never replies to it.
        let (answer, _) = listener.accept().await.expect("answer request");
        let (_headers, _body, answer_reader) = request_parts(answer).await;
        let _ = released.await;
        drop((stream_reader, answer_reader));
    };
    let client = async {
        let headers = outbound_headers(PROTOCOL_VERSION, Some("tools/call"), None, &[], None)
            .expect("headers");
        let response = transport
            .client
            .post(transport.url.clone())
            .headers(headers)
            .body("{}".to_owned())
            .send()
            .await
            .expect("SSE response");
        let mut deadline = CallDeadline::new(
            std::time::Duration::from_millis(300),
            std::time::Duration::from_millis(300),
        );
        let cancel = CancellationToken::new();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            transport.read_event_stream(
                response,
                &mut super::StreamCx {
                    request_id: 9,
                    original_id: 4,
                    version: PROTOCOL_VERSION,
                    token: None,
                    cancel: &cancel,
                    call_cancel: None,
                    deadline: &mut deadline,
                },
            ),
        )
        .await;
        let _ = release.send(());
        outcome
    };
    let (outcome, ()) = tokio::join!(client, server);
    let error = outcome
        .expect("the stalled answer held the call past its deadline")
        .expect_err("a call that never answers fails");
    assert!(
        matches!(
            error,
            crate::mcp::TransportError::Mcp(crate::mcp::McpError::Timeout { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn validates_http_loopback_policy() {
    let loopback = Url::parse("http://127.0.0.1:9000/mcp").expect("loopback URL");
    let remote = Url::parse("http://example.com/mcp").expect("remote URL");
    assert!(super::validate_endpoint(&loopback).is_ok());
    assert!(super::validate_endpoint(&remote).is_err());
    assert!(
        super::validate_endpoint(&Url::parse("https://example.com/mcp").expect("HTTPS URL"))
            .is_ok()
    );
}

/// Boundary tests for 401 recovery: a real `HttpTransport` talks to one
/// loopback server that is the MCP endpoint, the OAuth metadata host, and the
/// token endpoint.
mod unauthorized_recovery {
    use std::{
        collections::VecDeque,
        path::PathBuf,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use dal_agent::{
        error::ServiceError,
        ext::{
            Caller, Doc, EventStream, HookCx, RawValue, Services, Tool, ToolOutcome,
            services::ServiceFuture,
        },
    };
    use dal_core::{
        AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
        JobsReply, ModelRequest, Notice, PrivateEffect, Question, RawJson, RunOutput, RunRequest,
        SessionId, SidecarOp, TurnOp, TurnOpReply,
        ext::{
            McpDeclaration, McpRequest, McpResponse, StateError, StateOp, StateRecord, Visibility,
        },
    };
    use reqwest::Url;
    use sonic_rs::{JsonValueTrait, Value};
    use tokio::{io::AsyncWriteExt, net::TcpListener, task::JoinSet};
    use tokio_util::sync::CancellationToken;

    use super::{request_parts, test_key};
    use crate::mcp::{
        Budgets, McpError, TransportError,
        http::{
            CallCx, HttpTransport,
            auth::{self, RefreshCoordinator, TokenRecord},
            oauth::{self, Challenge, Discovery},
            protocol::PROTOCOL_VERSION,
        },
    };

    const STALE: &str = "stale-access";

    /// A host whose only observable behavior is counting questions.
    #[derive(Default)]
    struct CountingServices {
        asked: AtomicUsize,
        answer: Mutex<Option<Answer>>,
        questions: Mutex<Vec<String>>,
    }

    fn unavailable<T: Send + 'static>() -> ServiceFuture<'static, T> {
        Box::pin(async {
            Err(ServiceError::failed(
                None,
                "unavailable in the OAuth recovery fixture",
            ))
        })
    }

    impl Services for CountingServices {
        fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
            unavailable()
        }

        fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
            unavailable()
        }

        fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
            unavailable()
        }

        fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
            unavailable()
        }

        fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
            unavailable()
        }

        fn ask(&self, _who: &Caller, question: Question) -> ServiceFuture<'_, Option<Answer>> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            if let Question::Confirm { text, .. } = &question {
                self.questions
                    .lock()
                    .expect("questions")
                    .push(text.to_string());
            }
            let answer = self.answer.lock().expect("answer").clone();
            Box::pin(async move { Ok(answer) })
        }

        fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
            unavailable()
        }

        fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
            unavailable()
        }

        fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
            unavailable()
        }

        fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
            unavailable()
        }

        fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
            unavailable()
        }

        fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
            unavailable()
        }

        fn add_session_tools(
            &self,
            _who: &Caller,
            _tools: Vec<(Arc<dyn Tool>, Visibility)>,
        ) -> ServiceFuture<'_, ()> {
            unavailable()
        }

        fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
            unavailable()
        }

        fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
            unavailable()
        }

        fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
            unavailable()
        }

        fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
            unavailable()
        }

        fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
            unavailable()
        }

        fn state(
            &self,
            _who: &Caller,
            _op: StateOp,
        ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
            unavailable()
        }

        fn model_ready(
            &self,
            _who: &Caller,
            _model: dal_core::ModelRoute,
        ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
            unavailable()
        }

        fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
            unavailable()
        }

        fn infer_stream(
            &self,
            _who: &Caller,
            _req: ModelRequest,
        ) -> ServiceFuture<'_, EventStream> {
            unavailable()
        }

        fn call_tool(
            &self,
            _who: &Caller,
            _name: &str,
            _args: Box<RawValue>,
        ) -> ServiceFuture<'_, ToolOutcome> {
            unavailable()
        }

        fn notify(&self, _who: &Caller, _notice: Notice) {}

        fn append_record(
            &self,
            _who: &Caller,
            _kind: &str,
            _body: Box<RawValue>,
        ) -> ServiceFuture<'_, EntryId> {
            unavailable()
        }

        fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
            unavailable()
        }

        fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
            unavailable()
        }
    }

    /// What the loopback server answers; shared with the test body.
    struct Server {
        origin: String,
        resource: String,
        accepted: Mutex<String>,
        token_replies: Mutex<VecDeque<(u16, String)>>,
        token_requests: AtomicUsize,
        step_up_remaining: AtomicUsize,
        registration: AtomicBool,
        authorize_iss: Mutex<Option<String>>,
        seen_paths: Mutex<Vec<String>>,
        seen_bearers: Mutex<Vec<String>>,
        dcr_bodies: Mutex<Vec<String>>,
        token_bodies: Mutex<Vec<String>>,
        auth_queries: Mutex<Vec<String>>,
    }

    fn reply(status: u16, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn tokens_reply(access: &str, refresh: &str) -> (u16, String) {
        (
            200,
            format!(
                "{{\"access_token\":\"{access}\",\"refresh_token\":\"{refresh}\",\"token_type\":\"Bearer\"}}"
            ),
        )
    }

    impl Server {
        fn token_requests(&self) -> usize {
            self.token_requests.load(Ordering::SeqCst)
        }

        fn require_step_up(&self) {
            self.step_up_remaining.store(usize::MAX, Ordering::SeqCst);
        }

        /// Demands scope elevation for exactly `times` more bearer calls.
        fn require_step_up_times(&self, times: usize) {
            self.step_up_remaining.store(times, Ordering::SeqCst);
        }

        /// Advertises a registration endpoint in the authorization metadata.
        fn enable_registration(&self) {
            self.registration.store(true, Ordering::SeqCst);
        }

        /// Answers authorization redirects with `iss` instead of the real issuer.
        fn spoof_authorize_iss(&self, iss: String) {
            *self.authorize_iss.lock().expect("authorize iss") = Some(iss);
        }

        fn accept_only(&self, token: &str) {
            *self.accepted.lock().expect("accepted token") = token.to_owned();
        }

        /// The last scripted token reply repeats once the script runs out.
        fn next_token_reply(&self) -> (u16, String) {
            let mut replies = self.token_replies.lock().expect("token replies");
            if replies.len() > 1 {
                replies.pop_front()
            } else {
                replies.front().cloned()
            }
            .unwrap_or((500, "{}".to_owned()))
        }

        #[expect(
            clippy::too_many_lines,
            reason = "one fixture answers every route in order"
        )]
        fn respond(&self, headers: &str, body: &str) -> String {
            let line = headers.lines().next().unwrap_or_default();
            let mut fields = line.split_whitespace();
            let method = fields.next().unwrap_or_default();
            let target = fields.next().unwrap_or_default();
            self.seen_paths
                .lock()
                .expect("seen paths")
                .push(format!("{method} {target}"));
            if method == "POST" && target == "/register" {
                self.dcr_bodies
                    .lock()
                    .expect("DCR bodies")
                    .push(body.to_owned());
                return reply(200, "{\"client_id\":\"fixture-client\"}");
            }
            if method == "GET" && target.starts_with("/authorize") {
                let query = target
                    .split_once('?')
                    .map(|(_, query)| query)
                    .unwrap_or_default();
                self.auth_queries
                    .lock()
                    .expect("authorization queries")
                    .push(query.to_owned());
                let callback =
                    Url::parse(&format!("http://127.0.0.1{target}")).expect("authorization target");
                let pairs: std::collections::HashMap<String, String> =
                    callback.query_pairs().into_owned().collect();
                let redirect = pairs.get("redirect_uri").cloned().unwrap_or_default();
                let state = pairs.get("state").cloned().unwrap_or_default();
                let iss = self
                    .authorize_iss
                    .lock()
                    .expect("authorize iss")
                    .clone()
                    .unwrap_or_else(|| format!("{}/issuer", self.origin));
                let separator = if redirect.contains('?') { '&' } else { '?' };
                let location =
                    format!("{redirect}{separator}code=fixture-code&state={state}&iss={iss}");
                return format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
            if line.starts_with("POST /token") {
                self.token_requests.fetch_add(1, Ordering::SeqCst);
                self.token_bodies
                    .lock()
                    .expect("token bodies")
                    .push(body.to_owned());
                let (status, body) = self.next_token_reply();
                return reply(status, &body);
            }
            if line.contains("/.well-known/oauth-protected-resource/mcp") {
                return reply(
                    200,
                    &format!(
                        "{{\"resource\":\"{}\",\"authorization_servers\":[\"{}/issuer\"]}}",
                        self.resource, self.origin
                    ),
                );
            }
            if line.contains("/.well-known/oauth-authorization-server/issuer") {
                let origin = &self.origin;
                let registration = if self.registration.load(Ordering::SeqCst) {
                    format!("\"registration_endpoint\":\"{origin}/register\",")
                } else {
                    String::new()
                };
                return reply(
                    200,
                    &format!(
                        "{{\"issuer\":\"{origin}/issuer\",{registration}\"authorization_endpoint\":\"{origin}/authorize\",\"token_endpoint\":\"{origin}/token\",\"code_challenge_methods_supported\":[\"S256\"]}}"
                    ),
                );
            }
            if !line.starts_with("POST /mcp") {
                return reply(404, "{}");
            }
            let remaining = self.step_up_remaining.load(Ordering::SeqCst);
            if remaining > 0 {
                self.step_up_remaining
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                        left.checked_sub(1)
                    })
                    .expect("step-up budget never grows here");
                return "HTTP/1.1 403 Forbidden\r\nWWW-Authenticate: Bearer error=\"insufficient_scope\", scope=\"files:write\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
            }
            let bearer = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().strip_prefix("Bearer ").map(str::to_owned))
                    .flatten()
            });
            self.seen_bearers
                .lock()
                .expect("seen bearers")
                .push(bearer.clone().unwrap_or_default());
            let accepted = self.accepted.lock().expect("accepted token").clone();
            if bearer.as_deref() != Some(accepted.as_str()) {
                return "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
            }
            let id = sonic_rs::from_str::<Value>(body)
                .ok()
                .and_then(|request| request.get("id").and_then(JsonValueTrait::as_u64))
                .expect("request id");
            reply(
                200,
                &format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"bearer\":\"{accepted}\"}}}}"
                ),
            )
        }
    }

    async fn serve(listener: TcpListener, server: Arc<Server>) -> std::convert::Infallible {
        loop {
            let (stream, _) = listener.accept().await.expect("fixture connection");
            let (headers, body, mut reader) = request_parts(stream).await;
            reader
                .get_mut()
                .write_all(server.respond(&headers, &body).as_bytes())
                .await
                .expect("fixture response");
        }
    }

    /// Runs `work` while the loopback server answers, within a fixed budget.
    async fn with_server<T>(
        listener: TcpListener,
        server: &Arc<Server>,
        work: impl Future<Output = T>,
    ) -> T {
        let budgeted = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::select! {
                never = serve(listener, Arc::clone(server)) => match never {},
                result = work => result,
            }
        });
        budgeted.await.expect("scenario finishes within its budget")
    }

    struct Fixture {
        server: Arc<Server>,
        url: Url,
        issuer: String,
        dir: PathBuf,
        refreshes: Arc<RefreshCoordinator>,
        services: Arc<CountingServices>,
        who: Caller,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Best effort: the directory only holds this scenario's tokens.
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    impl Fixture {
        /// Starts the server with a stale stored credential.
        async fn start(accepted: &str, replies: Vec<(u16, String)>) -> (Self, TcpListener) {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback listener");
            let address = listener.local_addr().expect("listener address");
            let origin = format!("http://{address}");
            let url = Url::parse(&format!("{origin}/mcp")).expect("MCP URL");
            let resource = auth::canonical_resource(&url);
            let issuer = format!("{origin}/issuer");
            let dir = std::env::temp_dir().join(format!(
                "dalgona-unauthorized-recovery-{}",
                uuid::Uuid::new_v4()
            ));
            let stale = TokenRecord {
                client_id: "client".to_owned(),
                access_token: STALE.to_owned(),
                refresh_token: Some("stale-refresh".to_owned()),
                scopes: Vec::new(),
            };
            auth::persist_token(&dir.join("tokens.json"), &issuer, &resource, stale)
                .expect("seed stale credential");
            let services = Arc::new(CountingServices::default());
            let host: Arc<dyn Services> = services.clone();
            let who = HookCx::for_test(host, SessionId::new_v7(), None).caller;
            let server = Arc::new(Server {
                origin,
                resource,
                accepted: Mutex::new(accepted.to_owned()),
                token_replies: Mutex::new(replies.into()),
                token_requests: AtomicUsize::new(0),
                step_up_remaining: AtomicUsize::new(0),
                registration: AtomicBool::new(false),
                authorize_iss: Mutex::new(None),
                seen_paths: Mutex::new(Vec::new()),
                seen_bearers: Mutex::new(Vec::new()),
                dcr_bodies: Mutex::new(Vec::new()),
                token_bodies: Mutex::new(Vec::new()),
                auth_queries: Mutex::new(Vec::new()),
            });
            let fixture = Self {
                server,
                url,
                issuer,
                dir,
                refreshes: Arc::new(RefreshCoordinator::new()),
                services,
                who,
            };
            (fixture, listener)
        }

        /// One transport per MCP session; all share the client's coordinator.
        fn transport(&self) -> Arc<HttpTransport> {
            let transport = HttpTransport::new(
                test_key(),
                self.url.clone(),
                self.dir.join("tokens.json"),
                "0.1.0".to_owned(),
                &Budgets::default(),
                Arc::clone(&self.refreshes),
            )
            .expect("transport");
            Arc::new(transport)
        }

        /// Discovers the fixture's metadata through the real discovery path.
        async fn discovery(&self) -> Discovery {
            let policy = oauth::NetworkPolicy::for_target(&self.url);
            let oauth = oauth::OAuthClient {
                client: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .no_proxy()
                    .dns_resolver(policy.resolver())
                    .build()
                    .expect("fixture OAuth client"),
                policy,
            };
            oauth::discover(
                &oauth,
                &self.url,
                &Challenge::default(),
                Duration::from_secs(5),
                &CancellationToken::new(),
            )
            .await
            .expect("fixture metadata")
        }

        fn asked(&self) -> usize {
            self.services.asked.load(Ordering::SeqCst)
        }

        fn answer(&self, answer: Answer) {
            *self.services.answer.lock().expect("answer") = Some(answer);
        }

        /// Pops the newest authorization URL the client asked the user to open.
        fn take_confirm_url(&self) -> Option<String> {
            self.services
                .questions
                .lock()
                .expect("questions")
                .pop()
                .and_then(|text| {
                    text.find("http")
                        .map(|start| text[start..].trim_end().to_owned())
                })
        }

        /// Issues one tool call through the real exchange path.
        fn call(
            &self,
            transport: &Arc<HttpTransport>,
        ) -> impl Future<Output = Result<RawJson, TransportError>> + Send + 'static {
            let transport = Arc::clone(transport);
            let services = Arc::clone(&self.services);
            let who = self.who.clone();
            async move {
                let ids = AtomicU64::new(1000);
                let cancel = CancellationToken::new();
                let call = CallCx {
                    method: "tools/call",
                    version: PROTOCOL_VERSION,
                    services: services.as_ref(),
                    who: &who,
                    cancel: &cancel,
                    call_cancel: None,
                };
                transport
                    .exchange(1, &ids, "{\"name\":\"echo\"}", &[], None, &call)
                    .await
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_calls_with_an_expired_token_send_one_token_request() {
        let replies = vec![tokens_reply("fresh-access", "fresh-refresh")];
        let (fixture, listener) = Fixture::start("fresh-access", replies).await;
        let mut calls = JoinSet::new();
        for _ in 0..6 {
            calls.spawn(fixture.call(&fixture.transport()));
        }
        let drain = async {
            let mut succeeded = 0_usize;
            while let Some(joined) = calls.join_next().await {
                let response = joined.expect("call task").expect("tool call succeeds");
                assert!(response.as_str().contains("fresh-access"));
                succeeded += 1;
            }
            succeeded
        };
        let succeeded = with_server(listener, &fixture.server, drain).await;
        assert_eq!(succeeded, 6);
        assert_eq!(fixture.server.token_requests(), 1);
        assert_eq!(fixture.asked(), 0);
    }

    #[tokio::test]
    async fn a_transient_refresh_failure_does_not_block_a_later_refresh() {
        let replies = vec![
            (304, "{}".to_owned()),
            tokens_reply("fresh-access", "fresh-refresh"),
        ];
        let (fixture, listener) = Fixture::start("fresh-access", replies).await;
        let transport = fixture.transport();
        let scenario = async {
            let first = fixture.call(&transport).await;
            assert!(
                matches!(first, Err(TransportError::Mcp(McpError::Auth { .. }))),
                "a 304 from the token endpoint is an error, not a refusal: {first:?}"
            );
            assert_eq!(fixture.server.token_requests(), 1);
            assert_eq!(fixture.asked(), 0, "a transient failure must not prompt");
            let second = fixture
                .call(&transport)
                .await
                .expect("a later 401 refreshes again");
            assert!(second.as_str().contains("fresh-access"));
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(fixture.server.token_requests(), 2);
        assert_eq!(fixture.asked(), 0);
    }

    #[tokio::test]
    async fn a_rate_limited_refresh_neither_prompts_nor_blocks_a_later_refresh() {
        let replies = vec![
            (429, "{}".to_owned()),
            tokens_reply("fresh-access", "fresh-refresh"),
        ];
        let (fixture, listener) = Fixture::start("fresh-access", replies).await;
        let transport = fixture.transport();
        let scenario = async {
            let first = fixture.call(&transport).await;
            assert!(
                matches!(first, Err(TransportError::Mcp(McpError::Auth { .. }))),
                "a 429 from the token endpoint is transient, not a refusal: {first:?}"
            );
            assert_eq!(fixture.asked(), 0, "a rate limit must not prompt");
            let second = fixture
                .call(&transport)
                .await
                .expect("a later 401 refreshes again");
            assert!(second.as_str().contains("fresh-access"));
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(fixture.server.token_requests(), 2);
        assert_eq!(fixture.asked(), 0);
    }

    #[tokio::test]
    async fn a_refused_refresh_latches_until_the_user_authorizes_again() {
        let replies = vec![(400, "{\"error\":\"invalid_grant\"}".to_owned())];
        let (fixture, listener) = Fixture::start("fresh-access", replies).await;
        let transport = fixture.transport();
        let scenario = async {
            let first = fixture.call(&transport).await;
            assert!(matches!(
                first,
                Err(TransportError::Mcp(McpError::Auth { .. }))
            ));
            assert_eq!(fixture.asked(), 1, "a refusal falls back to the prompt");
            let second = fixture.call(&transport).await;
            assert!(matches!(
                second,
                Err(TransportError::Mcp(McpError::Auth { .. }))
            ));
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(
            fixture.server.token_requests(),
            1,
            "a refused refresh token is not sent again"
        );
        assert_eq!(fixture.asked(), 2);
    }

    #[tokio::test]
    async fn an_interactive_token_replaces_a_cached_refresh_result() {
        let replies = vec![tokens_reply("refreshed-access", "refreshed-refresh")];
        let (fixture, listener) = Fixture::start("refreshed-access", replies).await;
        let holder = fixture.transport();
        let scenario = async {
            // This transport caches the stale credential before anyone refreshes.
            holder.set_issuer(&fixture.issuer).await;
            holder.load_tokens().await;
            fixture
                .call(&fixture.transport())
                .await
                .expect("the first transport refreshes");
            assert_eq!(fixture.server.token_requests(), 1);
            // The user then authorizes again; the server drops older tokens.
            fixture.server.accept_only("interactive-access");
            let interactive = TokenRecord {
                client_id: "client".to_owned(),
                access_token: "interactive-access".to_owned(),
                refresh_token: Some("interactive-refresh".to_owned()),
                scopes: Vec::new(),
            };
            let discovery = fixture.discovery().await;
            fixture
                .transport()
                .persist(&discovery, interactive)
                .await
                .expect("interactive record persists");
            fixture
                .call(&holder)
                .await
                .expect("the stale holder adopts the interactive token")
        };
        let response = with_server(listener, &fixture.server, scenario).await;
        assert!(response.as_str().contains("interactive-access"));
        assert_eq!(fixture.server.token_requests(), 1);
    }

    #[tokio::test]
    async fn a_cancelled_authorization_latches_later_401s_without_another_prompt() {
        let replies = vec![(400, "{\"error\":\"invalid_grant\"}".to_owned())];
        let (fixture, listener) = Fixture::start("fresh-access", replies).await;
        fixture.answer(Answer::Cancel);
        let transport = fixture.transport();
        let scenario = async {
            let first = fixture.call(&transport).await;
            assert!(matches!(
                first,
                Err(TransportError::Mcp(McpError::NoAskFrontEnd))
            ));
            let second = fixture.call(&transport).await;
            assert!(matches!(
                second,
                Err(TransportError::Mcp(McpError::NoAskFrontEnd))
            ));
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(fixture.server.token_requests(), 1);
        assert_eq!(fixture.asked(), 1);
    }

    fn interactive_record() -> TokenRecord {
        TokenRecord {
            client_id: "client".to_owned(),
            access_token: "interactive-access".to_owned(),
            refresh_token: Some("interactive-refresh".to_owned()),
            scopes: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_declined_transport_adopts_a_token_another_transport_published() {
        let replies = vec![(400, "{\"error\":\"invalid_grant\"}".to_owned())];
        let (fixture, listener) = Fixture::start("interactive-access", replies).await;
        fixture.answer(Answer::Cancel);
        let declined = fixture.transport();
        let scenario = async {
            let first = fixture.call(&declined).await;
            assert!(matches!(
                first,
                Err(TransportError::Mcp(McpError::NoAskFrontEnd))
            ));
            let discovery = fixture.discovery().await;
            fixture
                .transport()
                .persist(&discovery, interactive_record())
                .await
                .expect("another transport authorizes");
            fixture
                .call(&declined)
                .await
                .expect("the declined transport adopts the published token")
        };
        let response = with_server(listener, &fixture.server, scenario).await;
        assert!(response.as_str().contains("interactive-access"));
        assert_eq!(fixture.asked(), 1);
    }

    #[tokio::test]
    async fn a_decline_latch_does_not_outlive_the_credential_it_refused() {
        let replies = vec![(400, "{\"error\":\"invalid_grant\"}".to_owned())];
        let (fixture, listener) = Fixture::start("never-accepted", replies).await;
        fixture.answer(Answer::Cancel);
        let declined = fixture.transport();
        let scenario = async {
            let first = fixture.call(&declined).await;
            assert!(matches!(
                first,
                Err(TransportError::Mcp(McpError::NoAskFrontEnd))
            ));
            let discovery = fixture.discovery().await;
            fixture
                .transport()
                .persist(&discovery, interactive_record())
                .await
                .expect("another transport authorizes");
            let second = fixture.call(&declined).await;
            assert!(matches!(
                second,
                Err(TransportError::Mcp(McpError::NoAskFrontEnd))
            ));
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(
            fixture.asked(),
            2,
            "a rejected new credential prompts again instead of inheriting the old decline"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_step_up_cancellation_prompts_once_and_latches() {
        let (fixture, listener) = Fixture::start("fresh-access", Vec::new()).await;
        fixture.server.require_step_up();
        fixture.answer(Answer::Cancel);
        let mut calls = JoinSet::new();
        for _ in 0..2 {
            calls.spawn(fixture.call(&fixture.transport()));
        }
        let drain = async {
            while let Some(joined) = calls.join_next().await {
                let result = joined.expect("step-up task");
                assert!(matches!(
                    result,
                    Err(TransportError::Mcp(McpError::NoAskFrontEnd))
                ));
            }
        };
        with_server(listener, &fixture.server, drain).await;
        assert_eq!(fixture.asked(), 1);
    }

    /// Plays the system browser: follows the fixture authorization redirect
    /// into the loopback callback and returns the callback status.
    async fn complete_browser_authorization(url: &str) -> reqwest::StatusCode {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("browser client")
            .get(url)
            .send()
            .await
            .expect("authorization redirect reaches the callback")
            .status()
    }

    /// Drives one interactive authorization: approves the consent question and
    /// completes every browser redirect the call opens, expecting `authorizations`
    /// callbacks in total. Each callback has its own bound so a missing
    /// question fails fast with a cause instead of burning the fixture budget.
    async fn drive_browser_authorizations(fixture: &Fixture, authorizations: usize) {
        for _ in 0..authorizations {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Some(url) = fixture.take_confirm_url() {
                        let status = complete_browser_authorization(&url).await;
                        assert!(
                            status.is_success(),
                            "the callback answers the browser redirect"
                        );
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the consent question opens for the browser");
        }
    }

    /// Seeds an access-only credential: no refresh token, so the first 401
    /// must take the interactive path instead of refreshing.
    fn seed_access_only(fixture: &Fixture, access: &str, scopes: Vec<String>) {
        auth::persist_token(
            &fixture.dir.join("tokens.json"),
            &fixture.issuer,
            &fixture.server.resource,
            TokenRecord {
                client_id: "client".to_owned(),
                access_token: access.to_owned(),
                refresh_token: None,
                scopes,
            },
        )
        .expect("seed access-only credential");
    }

    /// Full OAuth loop: discovery order, dynamic registration with a native
    /// application type, PKCE with a resource-bound challenge and exchange,
    /// then the bearer on every later POST.
    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "the full OAuth loop asserts every step in one walk"
    )]
    async fn oauth_happy_path() {
        let (fixture, listener) = Fixture::start(
            "happy-access",
            vec![tokens_reply("happy-access", "happy-refresh")],
        )
        .await;
        fixture.server.enable_registration();
        seed_access_only(&fixture, "expired-access", vec!["read".to_owned()]);
        fixture.answer(Answer::Approve);
        let transport = fixture.transport();
        let scenario = async {
            let (result, ()) = tokio::join!(
                fixture.call(&transport),
                drive_browser_authorizations(&fixture, 1)
            );
            let response = result.expect("interactive authorization succeeds");
            assert!(
                response.as_str().contains("happy-access"),
                "the retried call carries the new bearer"
            );
            let again = fixture
                .call(&transport)
                .await
                .expect("a later call reuses the bearer");
            assert!(again.as_str().contains("happy-access"));
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(
            fixture.asked(),
            1,
            "one interactive authorization, then the bearer persists"
        );
        let paths = fixture.server.seen_paths.lock().expect("seen paths");
        let position = |needle: &str| {
            paths
                .iter()
                .position(|path| path.contains(needle))
                .unwrap_or_else(|| panic!("the server saw no {needle}: {paths:?}"))
        };
        assert!(
            position("oauth-protected-resource") < position("oauth-authorization-server"),
            "resource metadata precedes authorization metadata: {paths:?}"
        );
        assert!(
            position("oauth-authorization-server") < position("POST /register"),
            "registration follows discovery: {paths:?}"
        );
        assert!(
            position("POST /register") < position("GET /authorize"),
            "the browser opens after registration: {paths:?}"
        );
        assert!(
            position("GET /authorize") < position("POST /token"),
            "the code exchanges after the callback: {paths:?}"
        );
        let dcr = fixture.server.dcr_bodies.lock().expect("DCR bodies");
        assert_eq!(dcr.len(), 1, "exactly one dynamic registration: {dcr:?}");
        assert!(
            dcr[0].contains("\"application_type\":\"native\""),
            "the DCR body registers a native client: {}",
            dcr[0]
        );
        let queries = fixture.server.auth_queries.lock().expect("auth queries");
        assert_eq!(queries.len(), 1, "exactly one authorization request");
        for field in [
            "code_challenge=",
            "code_challenge_method=S256",
            "resource=",
            "client_id=fixture-client",
        ] {
            assert!(
                queries[0].contains(field),
                "the authorization request carries {field}: {}",
                queries[0]
            );
        }
        let exchanges = fixture.server.token_bodies.lock().expect("token bodies");
        assert_eq!(exchanges.len(), 1, "exactly one token exchange");
        for field in [
            "grant_type=authorization_code",
            "code=fixture-code",
            "code_verifier=",
            "resource=",
            "client_id=fixture-client",
        ] {
            assert!(
                exchanges[0].contains(field),
                "the token exchange carries {field}: {}",
                exchanges[0]
            );
        }
        let bearers = fixture.server.seen_bearers.lock().expect("seen bearers");
        assert!(
            bearers.len() >= 4,
            "the unauthenticated probe, the stored token, the retried call, and the later call: {bearers:?}"
        );
        assert_eq!(
            bearers[0], "",
            "the first POST is unauthenticated; authorization begins on 401"
        );
        assert_eq!(bearers[1], "expired-access");
        assert!(
            bearers[2..].iter().all(|bearer| bearer == "happy-access"),
            "every later POST carries the new bearer: {bearers:?}"
        );
    }

    /// A callback whose `iss` differs from discovery is rejected before any
    /// token request leaves the client.
    #[tokio::test]
    async fn oauth_issuer_mismatch() {
        let (fixture, listener) = Fixture::start(
            "mismatch-access",
            vec![tokens_reply("mismatch-access", "mismatch-refresh")],
        )
        .await;
        seed_access_only(&fixture, "expired-access", Vec::new());
        fixture
            .server
            .spoof_authorize_iss(format!("{}/attacker", fixture.server.origin));
        fixture.answer(Answer::Approve);
        let transport = fixture.transport();
        let scenario = async {
            let driver = async {
                tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        if let Some(url) = fixture.take_confirm_url() {
                            let status = complete_browser_authorization(&url).await;
                            assert_eq!(
                                status.as_u16(),
                                400,
                                "the callback rejects the mismatched issuer"
                            );
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("the consent question opens for the browser");
            };
            let (result, ()) = tokio::join!(fixture.call(&transport), driver);
            let error = result.expect_err("the issuer mismatch fails the call");
            let text = match &error {
                TransportError::Mcp(inner) => {
                    assert!(
                        matches!(inner, McpError::IssuerMismatch),
                        "the mismatch surfaces as IssuerMismatch: {error:?}"
                    );
                    inner.to_string()
                }
                TransportError::Cancelled => {
                    panic!("the mismatch stays an Mcp error: {error:?}")
                }
            };
            assert_eq!(text, "mcp authorization failed: issuer mismatch");
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(
            fixture.server.token_requests(),
            0,
            "no token request is sent after an issuer mismatch"
        );
        assert_eq!(fixture.asked(), 1);
    }

    /// One 403 unions the challenged scope with the stored scopes,
    /// re-authorizes once, and the retried call succeeds.
    #[tokio::test]
    async fn oauth_step_up_scope() {
        let (fixture, listener) = Fixture::start(
            "stepup-access",
            vec![tokens_reply("stepup-access", "stepup-refresh")],
        )
        .await;
        fixture.server.require_step_up_times(1);
        seed_access_only(&fixture, "stepup-access", vec!["read".to_owned()]);
        fixture.answer(Answer::Approve);
        let transport = fixture.transport();
        let scenario = async {
            let (result, ()) = tokio::join!(
                fixture.call(&transport),
                drive_browser_authorizations(&fixture, 1)
            );
            let response = result.expect("step-up re-authorization succeeds");
            assert!(response.as_str().contains("stepup-access"));
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(
            fixture.server.token_requests(),
            1,
            "the scope challenge re-authorizes exactly once"
        );
        assert_eq!(fixture.asked(), 1);
        let file = auth::read_tokens(&fixture.dir.join("tokens.json"));
        let record = auth::record_for(&file, &fixture.issuer, &fixture.server.resource)
            .expect("the step-up persists a record");
        assert_eq!(record.access_token, "stepup-access");
        assert_eq!(record.scopes, vec!["files:write", "read"]);
    }

    /// Persistent 403s stop after the maximum: the call fails with the exact
    /// step-up limit string instead of re-authorizing forever.
    #[tokio::test]
    async fn oauth_step_up_limit() {
        let (fixture, listener) = Fixture::start(
            "limited-access",
            vec![
                tokens_reply("limited-access", "limited-refresh"),
                tokens_reply("limited-access", "limited-refresh"),
                tokens_reply("limited-access", "limited-refresh"),
            ],
        )
        .await;
        fixture.server.require_step_up();
        seed_access_only(&fixture, "limited-access", Vec::new());
        fixture.answer(Answer::Approve);
        let transport = fixture.transport();
        let scenario = async {
            let (result, ()) = tokio::join!(
                fixture.call(&transport),
                drive_browser_authorizations(&fixture, 2)
            );
            let error = result.expect_err("persistent 403s hit the limit");
            let text = match &error {
                TransportError::Mcp(inner) => {
                    assert!(
                        matches!(inner, McpError::StepUpLimit),
                        "the cap surfaces as StepUpLimit: {error:?}"
                    );
                    inner.to_string()
                }
                TransportError::Cancelled => {
                    panic!("the cap stays an Mcp error: {error:?}")
                }
            };
            assert_eq!(text, "mcp authorization failed: step-up limit reached");
        };
        with_server(listener, &fixture.server, scenario).await;
        assert_eq!(
            fixture.server.token_requests(),
            2,
            "re-authorization stops at the cap"
        );
        assert_eq!(fixture.asked(), 2);
    }

    /// Issues one tool call on `transport` with `services` as the host and
    /// `who` as the caller, mirroring the fixture call path.
    fn direct_call<'a>(
        transport: &'a Arc<HttpTransport>,
        services: &'a CountingServices,
        who: &'a Caller,
    ) -> impl Future<Output = Result<RawJson, TransportError>> + Send + 'a {
        let transport = Arc::clone(transport);
        async move {
            let ids = AtomicU64::new(1000);
            let cancel = CancellationToken::new();
            let call = CallCx {
                method: "tools/call",
                version: PROTOCOL_VERSION,
                services,
                who,
                cancel: &cancel,
                call_cancel: None,
            };
            transport
                .exchange(1, &ids, "{\"name\":\"echo\"}", &[], None, &call)
                .await
        }
    }

    /// Two servers share one issuer under different canonical resources: each
    /// holds its own token, and neither ever receives the other's bearer.
    /// Bearer selection is per request from the verified issuer plus the
    /// transport URL's canonical resource, so isolation holds for every call
    /// on either transport with no prompt and no token request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn token_binding_per_resource() {
        let (fixture_a, listener_a) = Fixture::start("token-a", Vec::new()).await;
        let (fixture_b, listener_b) = Fixture::start("token-b", Vec::new()).await;
        let resource_a = auth::canonical_resource(&fixture_a.url);
        let resource_b = auth::canonical_resource(&fixture_b.url);
        assert_ne!(
            resource_a, resource_b,
            "the two loopback servers are different resources"
        );
        let issuer = "https://shared-issuer.example";
        let tokens_path = fixture_a.dir.join("tokens.json");
        for (resource, token) in [(&resource_a, "token-a"), (&resource_b, "token-b")] {
            auth::persist_token(
                &tokens_path,
                issuer,
                resource,
                TokenRecord {
                    client_id: "client".to_owned(),
                    access_token: token.to_owned(),
                    refresh_token: None,
                    scopes: Vec::new(),
                },
            )
            .expect("seed per-resource credential");
        }
        let coordinator = Arc::new(RefreshCoordinator::new());
        let build = |url: Url| {
            HttpTransport::new(
                test_key(),
                url,
                tokens_path.clone(),
                "0.1.0".to_owned(),
                &Budgets::default(),
                Arc::clone(&coordinator),
            )
            .expect("per-resource transport")
        };
        let transport_a = Arc::new(build(fixture_a.url.clone()));
        let transport_b = Arc::new(build(fixture_b.url.clone()));
        transport_a.set_issuer(issuer).await;
        transport_b.set_issuer(issuer).await;
        let scenario = async {
            let (first, second) = tokio::join!(
                direct_call(&transport_a, fixture_a.services.as_ref(), &fixture_a.who),
                direct_call(&transport_b, fixture_b.services.as_ref(), &fixture_b.who)
            );
            let first = first.expect("resource A answers with its own bearer");
            let second = second.expect("resource B answers with its own bearer");
            assert!(first.as_str().contains("token-a"));
            assert!(second.as_str().contains("token-b"));
        };
        Box::pin(with_server(listener_a, &fixture_a.server, async {
            Box::pin(with_server(listener_b, &fixture_b.server, scenario)).await;
        }))
        .await;
        assert_eq!(fixture_a.asked(), 0, "no prompt on either transport");
        assert_eq!(fixture_b.asked(), 0, "no prompt on either transport");
        assert_eq!(
            fixture_a.server.token_requests(),
            0,
            "no token request on either transport"
        );
        assert_eq!(
            fixture_b.server.token_requests(),
            0,
            "no token request on either transport"
        );
        let bearers_a = fixture_a.server.seen_bearers.lock().expect("bearers A");
        let bearers_b = fixture_b.server.seen_bearers.lock().expect("bearers B");
        assert!(
            !bearers_a.is_empty() && bearers_a.iter().all(|bearer| bearer == "token-a"),
            "server A only ever saw its own bearer: {bearers_a:?}"
        );
        assert!(
            !bearers_b.is_empty() && bearers_b.iter().all(|bearer| bearer == "token-b"),
            "server B only ever saw its own bearer: {bearers_b:?}"
        );
    }
}

fn json_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// One seen request: the request line, the Authorization header, and the body.
struct SeenRequest {
    line: String,
    authorization: Option<String>,
    body: String,
}

/// Serves the OAuth discovery documents on `issuer` and replays `mcp_replies`
/// to `POST /mcp` in order. The future finishes once the last scripted reply is
/// answered; every request seen is returned, in order.
async fn auth_server(
    listener: TcpListener,
    issuer: String,
    resource: String,
    mcp_replies: Vec<String>,
) -> Vec<SeenRequest> {
    let mut seen = Vec::new();
    let mut mcp_replies = mcp_replies.into_iter().peekable();
    loop {
        let (stream, _) = listener.accept().await.expect("scripted request");
        let (headers, body, mut reader) = request_parts(stream).await;
        let line = headers.lines().next().unwrap_or_default().to_owned();
        let authorization = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.trim().to_owned());
        seen.push(SeenRequest {
            line: line.clone(),
            authorization,
            body,
        });
        let response = if line.starts_with("POST /mcp") {
            mcp_replies.next().expect("scripted /mcp reply")
        } else if line.starts_with("GET /.well-known/oauth-protected-resource") {
            json_response(&format!(
                "{{\"resource\":\"{resource}\",\"authorization_servers\":[\"{issuer}\"]}}"
            ))
        } else if line.starts_with("GET /.well-known/oauth-authorization-server") {
            json_response(&format!(
                "{{\"issuer\":\"{issuer}\",\"authorization_endpoint\":\"{issuer}/authorize\",\"token_endpoint\":\"{issuer}/token\"}}"
            ))
        } else if line.starts_with("POST /token") {
            json_response(
                "{\"access_token\":\"fresh-token\",\"token_type\":\"Bearer\",\"refresh_token\":\"rt-2\"}",
            )
        } else {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        };
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .expect("scripted response");
        if line.starts_with("POST /mcp") && mcp_replies.peek().is_none() {
            return seen;
        }
    }
}

fn notify_transport(url: Url, tokens_path: PathBuf) -> HttpTransport {
    HttpTransport::new(
        test_key(),
        url,
        tokens_path,
        "0.1.0".to_owned(),
        &Budgets::default(),
        Arc::new(token_auth::RefreshCoordinator::new()),
    )
    .expect("transport")
}

fn notify_call<'a>(
    cx: &'a dal_agent::ext::HookCx,
    cancel: &'a CancellationToken,
) -> super::CallCx<'a> {
    super::CallCx {
        method: "notifications/initialized",
        version: PROTOCOL_VERSION,
        services: cx.services.as_ref(),
        who: &cx.caller,
        cancel,
        call_cancel: None,
    }
}

async fn notify_fixture(
    record: token_auth::TokenRecord,
    mcp_replies: Vec<String>,
) -> (
    HttpTransport,
    impl Future<Output = Vec<SeenRequest>>,
    String,
    PathBuf,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let issuer = format!("http://{}", listener.local_addr().expect("address"));
    let url = Url::parse(&format!("{issuer}/mcp")).expect("fixture URL");
    let resource = token_auth::canonical_resource(&url);
    let token_dir = std::env::temp_dir().join(format!(
        "dalgona-mcp-notify-tokens-{}",
        uuid::Uuid::new_v4()
    ));
    let tokens_path = token_dir.join("tokens.json");
    token_auth::persist_token(&tokens_path, &issuer, &resource, record).expect("seed tokens");
    let server = auth_server(listener, issuer.clone(), resource, mcp_replies);
    (
        notify_transport(url, tokens_path.clone()),
        server,
        issuer,
        tokens_path,
    )
}

fn http_response(status: u16, reason: &str, headers: &str) -> String {
    format!("HTTP/1.1 {status} {reason}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n")
}

#[tokio::test]
async fn notify_retries_once_with_the_stored_token() {
    let (transport, server, _issuer, tokens_path) = notify_fixture(
        token_auth::TokenRecord {
            client_id: "fixture".to_owned(),
            access_token: "stored-token".to_owned(),
            refresh_token: None,
            scopes: Vec::new(),
        },
        vec![
            http_response(401, "Unauthorized", "WWW-Authenticate: Bearer\r\n"),
            http_response(202, "Accepted", ""),
        ],
    )
    .await;
    let cx = dal_agent::ext::HookCx::for_test(
        Arc::new(FakeServices::default()),
        dal_core::SessionId::new_v7(),
        None,
    );
    let cancel = CancellationToken::new();
    let call = notify_call(&cx, &cancel);
    let ids = AtomicU64::new(9);
    let (result, seen) = tokio::join!(transport.notify(&ids, &call), server);
    result.expect("notify retried with the stored token");
    let mcp_posts: Vec<&SeenRequest> = seen
        .iter()
        .filter(|request| request.line.starts_with("POST /mcp"))
        .collect();
    assert_eq!(mcp_posts.len(), 2);
    assert_eq!(mcp_posts[0].authorization, None);
    assert_eq!(
        mcp_posts[1].authorization.as_deref(),
        Some("Bearer stored-token")
    );
    assert!(seen.iter().any(|request| {
        request
            .line
            .starts_with("GET /.well-known/oauth-protected-resource")
    }));
    assert!(seen.iter().any(|request| {
        request
            .line
            .starts_with("GET /.well-known/oauth-authorization-server")
    }));
    if let Some(directory) = tokens_path.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[tokio::test]
async fn notify_refreshes_a_rejected_token_and_persists() {
    let (transport, server, issuer, tokens_path) = notify_fixture(
        token_auth::TokenRecord {
            client_id: "fixture".to_owned(),
            access_token: "stale-token".to_owned(),
            refresh_token: Some("rt-1".to_owned()),
            scopes: Vec::new(),
        },
        vec![
            http_response(401, "Unauthorized", ""),
            http_response(401, "Unauthorized", ""),
            http_response(202, "Accepted", ""),
        ],
    )
    .await;
    let cx = dal_agent::ext::HookCx::for_test(
        Arc::new(FakeServices::default()),
        dal_core::SessionId::new_v7(),
        None,
    );
    let cancel = CancellationToken::new();
    let call = notify_call(&cx, &cancel);
    let ids = AtomicU64::new(9);
    let (result, seen) = tokio::join!(transport.notify(&ids, &call), server);
    result.expect("notify refreshed the rejected token");
    let mcp_posts: Vec<&SeenRequest> = seen
        .iter()
        .filter(|request| request.line.starts_with("POST /mcp"))
        .collect();
    assert_eq!(mcp_posts.len(), 3);
    assert_eq!(
        mcp_posts[1].authorization.as_deref(),
        Some("Bearer stale-token")
    );
    assert_eq!(
        mcp_posts[2].authorization.as_deref(),
        Some("Bearer fresh-token")
    );
    let refresh = seen
        .iter()
        .find(|request| request.line.starts_with("POST /token"))
        .expect("refresh request");
    assert!(refresh.body.contains("grant_type=refresh_token"));
    assert!(refresh.body.contains("refresh_token=rt-1"));
    let tokens = token_auth::read_tokens(&tokens_path);
    let stored = token_auth::record_for(
        &tokens,
        &issuer,
        &token_auth::canonical_resource(&transport.url),
    )
    .expect("persisted record");
    assert_eq!(stored.access_token, "fresh-token");
    assert_eq!(stored.refresh_token.as_deref(), Some("rt-2"));
    if let Some(directory) = tokens_path.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[tokio::test]
async fn notify_fails_closed_on_a_plain_forbidden() {
    let (transport, server, _issuer, tokens_path) = notify_fixture(
        token_auth::TokenRecord {
            client_id: "fixture".to_owned(),
            access_token: "stored-token".to_owned(),
            refresh_token: None,
            scopes: Vec::new(),
        },
        vec![http_response(403, "Forbidden", "")],
    )
    .await;
    let cx = dal_agent::ext::HookCx::for_test(
        Arc::new(FakeServices::default()),
        dal_core::SessionId::new_v7(),
        None,
    );
    let cancel = CancellationToken::new();
    let call = notify_call(&cx, &cancel);
    let ids = AtomicU64::new(9);
    let (result, seen) = tokio::join!(transport.notify(&ids, &call), server);
    match result {
        Err(super::TransportError::Mcp(crate::mcp::McpError::HttpAuth { code, .. })) => {
            assert_eq!(code, 403);
        }
        other => panic!("a plain 403 must fail closed, got {other:?}"),
    }
    assert!(
        !seen
            .iter()
            .any(|request| request.line.starts_with("POST /token"))
    );
    if let Some(directory) = tokens_path.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }
}
