// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::{future::Future, path::PathBuf, sync::Arc};

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
    CallDeadline, HttpCall, HttpTransport, StreamContext,
    auth::RefreshCoordinator,
    protocol::{PROTOCOL_VERSION, outbound_headers, request_body},
};
use crate::mcp::{Budgets, tools::Key};

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
        Arc::new(RefreshCoordinator::new()),
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
    let (response, (headers, sent_body)) = tokio::join!(
        transport.send_once(
            HttpCall {
                body: request.as_str(),
                method: Some("tools/call"),
                name: Some("echo"),
                extra: &extra,
                version: PROTOCOL_VERSION,
                token: None,
            },
            &cancel,
            &deadline,
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
    let initial_deadline = deadline.expires;
    let response = transport
        .read_event_stream(
            response,
            StreamContext {
                request_id: 9,
                original_id: 4,
                cancel: &CancellationToken::new(),
                deadline: &mut deadline,
                version: PROTOCOL_VERSION,
                token: None,
            },
        )
        .await
        .expect("SSE reply");
    assert!(response.as_str().contains("\"id\":4"));
    assert!(deadline.expires > initial_deadline);
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
        JobsReply, ModelRequest, Notice, Question, RawJson, RunOutput, RunRequest, SessionId,
        SidecarOp, TurnOp, TurnOpReply,
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
            ExchangeRequest, HttpTransport,
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

        fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
            self.asked.fetch_add(1, Ordering::SeqCst);
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
    }

    /// What the loopback server answers; shared with the test body.
    struct Server {
        origin: String,
        resource: String,
        accepted: Mutex<String>,
        token_replies: Mutex<VecDeque<(u16, String)>>,
        token_requests: AtomicUsize,
        step_up: AtomicBool,
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
            self.step_up.store(true, Ordering::SeqCst);
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

        fn respond(&self, headers: &str, body: &str) -> String {
            let line = headers.lines().next().unwrap_or_default();
            if line.starts_with("POST /token") {
                self.token_requests.fetch_add(1, Ordering::SeqCst);
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
                return reply(
                    200,
                    &format!(
                        "{{\"issuer\":\"{origin}/issuer\",\"authorization_endpoint\":\"{origin}/authorize\",\"token_endpoint\":\"{origin}/token\",\"code_challenge_methods_supported\":[\"S256\"]}}"
                    ),
                );
            }
            if !line.starts_with("POST /mcp") {
                return reply(404, "{}");
            }
            if self.step_up.load(Ordering::SeqCst) {
                return "HTTP/1.1 403 Forbidden\r\nWWW-Authenticate: Bearer error=\"insufficient_scope\", scope=\"write\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
            }
            let bearer = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().strip_prefix("Bearer ").map(str::to_owned))
                    .flatten()
            });
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
                step_up: AtomicBool::new(false),
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
            oauth::discover(
                &reqwest::Client::new(),
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
                transport
                    .exchange(ExchangeRequest {
                        id: 1,
                        ids: &ids,
                        method: "tools/call",
                        params: "{\"name\":\"echo\"}",
                        headers: &[],
                        arguments: None,
                        version: PROTOCOL_VERSION,
                        services: services.as_ref(),
                        who: &who,
                        cancel: &CancellationToken::new(),
                    })
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
}
