//! P4: the Anthropic Override wires of the three P4 rows — `bai`
//! (`WireBase::Override("https://api.b.ai")`), `opencode` (row default
//! `https://opencode.ai/zen`, `x-api-key`, `x-opencode-session`), and
//! `cloudflare-ai-gateway` (template base
//! `https://gateway.ai.cloudflare.com/v1/{account}/{gateway}/anthropic`,
//! `cf-aig-authorization`) — complete OFFLINE over the real transport:
//! `ProviderSet::new` -> `set.provider` -> `provider.open` ->
//! `http::LazyClient::get` -> `http::build_client` -> reqwest. The
//! operator hosts are never rewritten and never contacted.
//!
//! # Environment scoping: a child process, never the suite process
//!
//! The two transport inputs reqwest reads are ambient variables, and both
//! are read exactly once, at client build:
//!
//! - Proxy: `http::client_builder()` sets no proxy policy
//!   (`src/http.rs:106-116`), so the client keeps reqwest 0.13.5's default
//!   `auto_sys_proxy: true` (`async_impl/client.rs:309`), pushed as
//!   `ProxyMatcher::system()` in `build()` (`:416-420`). hyper-util 0.1.21
//!   reads `ALL_PROXY|HTTPS_PROXY|HTTP_PROXY|NO_PROXY` once at matcher
//!   construction (`proxy/matcher.rs:228-237`, `from_system` `:109-114`),
//!   routes every `https` destination to the `HTTPS_PROXY` value
//!   (`:127-135`), and dials it with `CONNECT {host}:{port} HTTP/1.1`
//!   (`legacy/connect/proxy/tunnel.rs:172`).
//! - Trust: with the `rustls` feature and no custom roots, reqwest verifies
//!   with `rustls_platform_verifier::Verifier::new` (client.rs:759); on
//!   Linux that loads exactly `rustls_native_certs::load_native_certs()`
//!   (rustls-platform-verifier 0.7.1, `verification/others.rs:92`), and
//!   rustls-native-certs 0.8.5 takes `SSL_CERT_FILE` as the whole root
//!   store (its `lib.rs:8,52,401`).
//!
//! `ProviderSet` has no client-injection seam (`LazyClient` is `pub(crate)`
//! with the fixed builder; `ProviderSet::new` takes only config, identity,
//! env snapshot, and directories), so ambient variables are the one
//! faithful capture point. This suite therefore never mutates its own
//! process environment: the orchestrating test re-executes the existing
//! test binary as a child (`std::env::current_exe`) with
//! `Command::env_clear()` plus exactly the variables the child needs — set
//! by the OS before the child's runtime or client ever exists. No `unsafe`,
//! no global state, no suite interference: the parent's environment is
//! untouched, and every other test keeps running against the pristine
//! environment.
//!
//! # What may and may not be contacted
//!
//! The CONNECT fixture allows only the three wire authorities and refuses
//! every other host with 403 before any tunnel opens; allowed tunnels
//! bridge only to the loopback TLS origin, which presents one fixture leaf
//! per wire host, signed by the fixture root (test-only key material) that
//! `SSL_CERT_FILE` names. No hostname is ever resolved and no byte leaves
//! the process pair.
//!
//! # Ownership and bounds
//!
//! The orchestrator owns every fixture thread and joins it normally (no
//! detached task), publishes the stop flag on every exit path, bounds every
//! socket operation and wait with a timeout, and awaits the child to
//! completion. Shared cells are one `Arc<Mutex>` log, a release/acquire
//! atomic, and bounded mpsc hand-offs.

use super::*;
use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener as StdListener, TcpStream as StdStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::{self, JoinHandle};

use rustls::pki_types::pem::PemObject as _;

/// Marks a child process as the offline-proof runner; without it the child
/// entry test is a no-op, so the suite never re-executes anything.
const CHILD_FLAG: &str = "DAL_WIRE_OFFLINE_CHILD";

/// The twelve explicit `SessionId`s (comma-separated) the child must use,
/// so the parent can assert the opencode session header verbatim.
const SESSIONS_VAR: &str = "DAL_WIRE_OFFLINE_SESSIONS";

/// The exact test path the child re-executes.
const CHILD_TEST: &str =
    "provider::tests::wire_offline_anthropic::served_anthropic_override_rows_offline";

/// Socket bound for every fixture read and write; the cancel leg waits
/// [`STEP_TIMEOUT`] instead so its deadline strictly exceeds the child's
/// settle-plus-abort window.
const LEG_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the child waits for a cancelled held drain to stay pending
/// before aborting it; the fixture close deadline ([`STEP_TIMEOUT`])
/// strictly exceeds settle-plus-abort, while every parent wait stays bounded.
const CLOSE_WAIT: Duration = Duration::from_secs(3);

/// How long one bounded wait (fixture capture, child step) may take; also
/// the fixture close deadline, so a timeout reports `false`, never a close.
const STEP_TIMEOUT: Duration = Duration::from_secs(15);

/// How many tunnels one leg may absorb with no TLS bytes before the fixture
/// treats the run as broken. A client that abandons a tunnel after its
/// CONNECT was counted costs one such tunnel; more than this means the
/// fixture, not the client, is misbehaving.
const ABANDONED_TUNNEL_LIMIT: usize = 4;

/// How long the parent waits for the child before killing and reaping it;
/// covers nine streaming turns plus three settle-plus-abort held turns.
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

/// The fixture trust root: the one certificate `SSL_CERT_FILE` names, so
/// the child's platform verifier trusts exactly the fixture origins.
const ROOT_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBozCCAUigAwIBAgIUQPTMLRoVQbkwXEEavmJOMxC4oDIwCgYIKoZIzj0EAwIw
LzEtMCsGA1UEAwwkZGFsLXByb3ZpZGVyIHdpcmUgb2ZmbGluZSBwcm9vZiByb290
MB4XDTI2MTAyNjAxMTg1NVoXDTM2MTAyMzAxMTg1NVowLzEtMCsGA1UEAwwkZGFs
LXByb3ZpZGVyIHdpcmUgb2ZmbGluZSBwcm9vZiByb290MFkwEwYHKoZIzj0CAQYI
KoZIzj0DAQcDQgAEvzn/Gpi8JkLdP46Pm0MMO6LzOnWwjwv7mLIlHchqedGZpUdk
ySLggW9hsBQcLc/fJwA+yf9HPyDTI5DD3l9dxaNCMEAwHQYDVR0OBBYEFIhcOImh
P1WmLFkVmvgA3UwHj6cPMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgEG
MAoGCCqGSM49BAMCA0kAMEYCIQD1XMCLVmN9EGAMBhUJVx38tAV0TmM8wTrpwOpb
dMH6bQIhAJKHXAVnwFdeLGygIXYk5baGQg7KgICCrptK6+YVLF9G
-----END CERTIFICATE-----
";

/// The `api.b.ai` leaf (SAN `DNS:api.b.ai`, serverAuth, 2026-2036).
const API_B_AI_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBgDCCASagAwIBAgIUJF0FbVdgZrcboPOG0oXhgIk/P4YwCgYIKoZIzj0EAwIw
LzEtMCsGA1UEAwwkZGFsLXByb3ZpZGVyIHdpcmUgb2ZmbGluZSBwcm9vZiByb290
MB4XDTI2MTAyNjAxMTg1NVoXDTM2MTAyMzAxMTg1NVowEzERMA8GA1UEAwwIYXBp
LmIuYWkwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAARx9Q0UEopIipPoxUKCWC7A
pBUZJ0+hDPetntmDkDK2cWjiOdLzUSBTDIpF+SQyi9qyvOwzTV4REmCVHvv/3rAg
ozwwOjATBgNVHREEDDAKgghhcGkuYi5haTATBgNVHSUEDDAKBggrBgEFBQcDATAO
BgNVHQ8BAf8EBAMCB4AwCgYIKoZIzj0EAwIDSAAwRQIgHmA5AOiwHleZWy1CBFEv
nLXj+ZnbQSDGBpljPC3UProCIQDyZMdSzxwW4GB8pD/mGg8+bjF7XsaK6A3sNnSa
Df13jw==
-----END CERTIFICATE-----
";

/// The `opencode.ai` leaf (SAN `DNS:opencode.ai`, serverAuth, 2026-2036).
const OPENCODE_AI_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBhjCCASygAwIBAgIUJF0FbVdgZrcboPOG0oXhgIk/P4cwCgYIKoZIzj0EAwIw
LzEtMCsGA1UEAwwkZGFsLXByb3ZpZGVyIHdpcmUgb2ZmbGluZSBwcm9vZiByb290
MB4XDTI2MTAyNjAxMTg1NVoXDTM2MTAyMzAxMTg1NVowFjEUMBIGA1UEAwwLb3Bl
bmNvZGUuYWkwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAARTcUj8mqaMBr/dof4x
v9XEBSz3DU4yWVVfo5k94LRPitzqzAWmNA3bXCYja7Totkgb33johfejbyXtismC
J3Peoz8wPTAWBgNVHREEDzANggtvcGVuY29kZS5haTATBgNVHSUEDDAKBggrBgEF
BQcDATAOBgNVHQ8BAf8EBAMCB4AwCgYIKoZIzj0EAwIDSAAwRQIgSRldYxLPgjB/
YEGeG+WqNUdtCBoqMUTRZ1xsZiUnbz0CIQDVRScnhvJwcwUTZhrsl7u7KjEwcpD1
riPvdULCz5cU4A==
-----END CERTIFICATE-----
";

/// The `gateway.ai.cloudflare.com` leaf (SAN
/// `DNS:gateway.ai.cloudflare.com`, serverAuth, 2026-2036).
const GATEWAY_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBojCCAUigAwIBAgIUJF0FbVdgZrcboPOG0oXhgIk/P4gwCgYIKoZIzj0EAwIw
LzEtMCsGA1UEAwwkZGFsLXByb3ZpZGVyIHdpcmUgb2ZmbGluZSBwcm9vZiByb290
MB4XDTI2MTAyNjAxMTg1NVoXDTM2MTAyMzAxMTg1NVowJDEiMCAGA1UEAwwZZ2F0
ZXdheS5haS5jbG91ZGZsYXJlLmNvbTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IA
BGtUnSorOgYodFouiQCrdCNMVe2qF1B6d4qWKrK0BbctqTIV4PlJikE88rsp3RTo
S22Pez0K6esJ0ipwAKZj99mjTTBLMCQGA1UdEQQdMBuCGWdhdGV3YXkuYWkuY2xv
dWRmbGFyZS5jb20wEwYDVR0lBAwwCgYIKwYBBQUHAwEwDgYDVR0PAQH/BAQDAgeA
MAoGCCqGSM49BAMCA0gAMEUCIQD4VM7YUTgt+JjW+RhLbOvDZNzoUuxSA1u/Wy6P
CCiR0gIgeDptBJbURlI1Mk387AZZZMJuD0DBN4pqccRPcr52xcc=
-----END CERTIFICATE-----
";

/// The `api.b.ai` leaf key (PKCS#8, test-only material).
const API_B_AI_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgeQjzSGveo9Cr026e
Gy0xF9TK4OjjwXdF/dS9rVaSgoChRANCAARx9Q0UEopIipPoxUKCWC7ApBUZJ0+h
DPetntmDkDK2cWjiOdLzUSBTDIpF+SQyi9qyvOwzTV4REmCVHvv/3rAg
-----END PRIVATE KEY-----";

/// The `opencode.ai` leaf key (PKCS#8, test-only material).
const OPENCODE_AI_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgF5cjbn6MNVvqU4j6
HuQTqsomsUR4cQMG69dNo2p9tKOhRANCAARTcUj8mqaMBr/dof4xv9XEBSz3DU4y
WVVfo5k94LRPitzqzAWmNA3bXCYja7Totkgb33johfejbyXtismCJ3Pe
-----END PRIVATE KEY-----";

/// The `gateway.ai.cloudflare.com` leaf key (PKCS#8, test-only material).
const GATEWAY_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgpu/VTbQdHGxcjPey
zj0bwRxt0/Usun3fPmhmZ7ob8pihRANCAARrVJ0qKzoGKHRaLokAq3QjTFXtqhdQ
eneKliqytAW3LakyFeD5SYpBPPK7Kd0U6Ettj3s9CunrCdIqcACmY/fZ
-----END PRIVATE KEY-----";

/// Everything one row's offline proof fixes: the wire identity the route
/// must select, the origin it must reach, and the exact header policy.
#[derive(Clone)]
struct RowProfile {
    id: &'static str,
    model: &'static str,
    host: &'static str,
    authority: &'static str,
    path: &'static str,
    env: &'static [(&'static str, &'static str)],
    auth_header: &'static str,
    headers_absent: &'static [&'static str],
    session_header: Option<&'static str>,
    leaf_cert: &'static str,
    leaf_key: &'static str,
}

/// The three P4 Anthropic Override rows, in drive order.
fn rows() -> Vec<RowProfile> {
    vec![
        RowProfile {
            id: "bai",
            model: "claude-fable-5.1",
            host: "api.b.ai",
            authority: "api.b.ai:443",
            path: "/v1/messages",
            env: &[("BAI_API_KEY", "bai-secret")],
            auth_header: "authorization: bearer bai-secret",
            headers_absent: &[
                "\nx-api-key:",
                "\ncf-aig-authorization:",
                "\nx-opencode-session:",
            ],
            session_header: None,
            leaf_cert: API_B_AI_CERT,
            leaf_key: API_B_AI_KEY,
        },
        RowProfile {
            id: "opencode",
            model: "claude-opus-5",
            host: "opencode.ai",
            authority: "opencode.ai:443",
            path: "/zen/v1/messages",
            env: &[("OPENCODE_API_KEY", "opencode-secret")],
            auth_header: "x-api-key: opencode-secret",
            headers_absent: &["\nauthorization:", "\ncf-aig-authorization:"],
            session_header: Some("x-opencode-session"),
            leaf_cert: OPENCODE_AI_CERT,
            leaf_key: OPENCODE_AI_KEY,
        },
        RowProfile {
            id: "cloudflare-ai-gateway",
            model: "anthropic/claude-opus-5",
            host: "gateway.ai.cloudflare.com",
            authority: "gateway.ai.cloudflare.com:443",
            path: "/v1/acct-one/gw-one/anthropic/v1/messages",
            env: &[
                ("CLOUDFLARE_ACCOUNT_ID", "acct-one"),
                ("CLOUDFLARE_GATEWAY_ID", "gw-one"),
                ("CLOUDFLARE_AI_GATEWAY_API_KEY", "gate-key"),
            ],
            auth_header: "cf-aig-authorization: bearer gate-key",
            headers_absent: &["\nauthorization:", "\nx-api-key:", "\nx-opencode-session:"],
            session_header: None,
            leaf_cert: GATEWAY_CERT,
            leaf_key: GATEWAY_KEY,
        },
    ]
}

/// One scripted origin leg: capture the tunneled request, then act.
#[derive(Clone, Copy)]
enum Leg {
    /// Reply with an Anthropic text SSE body and close.
    Text,
    /// Reply with an Anthropic tool-use SSE body and close.
    Tool,
    /// Reply with an Anthropic text SSE body and close (tool-result turn).
    TextAfterTool,
    /// Reply with nothing; wait for the peer to close first.
    HoldForClose,
}

/// A recorded CONNECT: the authority the client asked for, and whether the
/// fixture bridged it (`true`) or refused it before any tunnel (`false`).
#[derive(Clone, Debug, Eq, PartialEq)]
struct Connect {
    authority: String,
    allowed: bool,
    /// Whether an admitted CONNECT opened the loopback tunnel. A denied
    /// probe, and a client that writes a plain request onto a tunnel the
    /// proxy already closed, both stay `false`: neither reaches the origin,
    /// so neither serves a leg.
    tunnelled: bool,
}

/// Sets the shared stop flag when dropped, so the fixture threads exit even
/// when an assert panics mid-test.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The `id` row exactly as the built-in definition admits it: the row keeps
/// its default base and `Origins::default()`, so the row's Anthropic wire
/// (Override, default, or template suffix) alone decides the request origin.
fn wired_row(id: &str) -> ProviderEntry {
    let def = crate::find(id).unwrap_or_else(|| panic!("{id} is a built-in row"));
    ProviderEntry {
        id: def.id.into(),
        def: Some(def),
        family: def.family,
        base_url: def.base_url.into(),
        transport: Transport::Https,
        key_env: None,
        auth: def.key.map_or(AuthStyle::None, |key| key.style),
        max_concurrent_requests: 1,
        origins: Origins::default(),
        settings: ProviderSettings::default(),
    }
}

/// The anthropic-route resolution `provider.open` needs: the shared
/// `resolved` helper with the family's adaptive-thinking shape.
fn claude_route(row: &RowProfile) -> ResolvedModel {
    let mut route = resolved(row.id, Family::Anthropic, row.model);
    route.entry.thinking = ThinkingSupport::UnknownAdaptive;
    route
}

fn sse_head(body: &[u8]) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: \
         {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes()
}

fn sse_body(leg: Leg) -> Option<String> {
    match leg {
        Leg::Text | Leg::TextAfterTool => Some(anthropic_reply()),
        Leg::Tool => Some(tool_reply(Family::Anthropic, "lookup_docs")),
        Leg::HoldForClose => None,
    }
}

/// Serves the legs in order over fixture TLS (one leaf per leg's host) and
/// reports every captured request through `requests`; cancel legs report
/// whether the peer closed. Owned by the orchestrator; exits when `stop`
/// is published.
fn origin_serve(
    listener: StdListener,
    plans: Vec<(Arc<rustls::ServerConfig>, Leg)>,
    requests: std_mpsc::Sender<Vec<u8>>,
    closed: std_mpsc::Sender<bool>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("origin listener nonblocking");
        for (tls, leg) in plans {
            // A tunnel can reach the fixture with no TLS bytes: a client that
            // abandoned the attempt after its CONNECT was counted. Such a
            // socket serves no leg, so it must not spend a plan; the fixture
            // accepts the next tunnel for the same leg instead of dying and
            // starving every later leg. The proxy ledger keeps an exact
            // record of every tunnel, so an abandoned one stays visible.
            let mut abandoned = 0_usize;
            loop {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let Ok(socket) = accept_bounded(&listener, &stop) else {
                    return;
                };
                match serve_leg(socket, &tls, leg, &requests, &closed) {
                    LegOutcome::Served => break,
                    LegOutcome::Abandoned => {
                        abandoned += 1;
                        assert!(
                            abandoned <= ABANDONED_TUNNEL_LIMIT,
                            "the fixture accepted {abandoned} tunnels with no TLS bytes before this leg"
                        );
                    }
                }
            }
        }
    })
}

/// Accepts one socket while a 5 ms poll watches the shared stop flag.
fn accept_bounded(listener: &StdListener, stop: &Arc<AtomicBool>) -> std::io::Result<StdStream> {
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "fixture stopped",
            ));
        }
        match listener.accept() {
            Ok((socket, _)) => return Ok(socket),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Handshakes one TLS connection, reads the whole request, and runs the leg.
/// Why one accepted connection produced no leg.
///
/// A client that abandons a tunnel leaves a socket with no TLS bytes, and a
/// client that retries leaves one whose handshake the fixture never sees.
/// Such a connection served no request, so the plan stays unspent; the
/// orchestrator accepts the next one instead of dying and starving every
/// later leg.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegOutcome {
    /// The request was read and reported, so the plan is spent.
    Served,
    /// The connection carried no leg: no request was read.
    Abandoned,
}

fn serve_leg(
    socket: StdStream,
    tls: &Arc<rustls::ServerConfig>,
    leg: Leg,
    requests: &std_mpsc::Sender<Vec<u8>>,
    closed: &std_mpsc::Sender<bool>,
) -> LegOutcome {
    socket
        .set_read_timeout(Some(LEG_TIMEOUT))
        .expect("origin read timeout");
    socket
        .set_write_timeout(Some(LEG_TIMEOUT))
        .expect("origin write timeout");
    let connection =
        rustls::ServerConnection::new(Arc::clone(tls)).expect("fixture server connection");
    let mut tls_stream = rustls::StreamOwned::new(connection, socket);
    while tls_stream.conn.is_handshaking() {
        if tls_stream.conn.complete_io(&mut tls_stream.sock).is_err() {
            return LegOutcome::Abandoned;
        }
    }
    let Some(request) = read_whole_request(&mut tls_stream) else {
        return LegOutcome::Abandoned;
    };
    requests.send(request).expect("origin reports request");
    if let Some(body) = sse_body(leg) {
        tls_stream
            .write_all(&sse_head(body.as_bytes()))
            .expect("origin writes head");
        tls_stream
            .write_all(body.as_bytes())
            .expect("origin writes body");
        tls_stream.flush().expect("origin flushes");
        return LegOutcome::Served;
    }
    // The close deadline strictly exceeds the child's settle (500 ms) plus
    // post-cancel wait (`CLOSE_WAIT`) before `drain.abort()` drops the held
    // stream, so a `false` here means the peer never closed, not a race.
    tls_stream
        .sock
        .set_read_timeout(Some(STEP_TIMEOUT))
        .expect("cancel-leg read timeout");
    let mut byte = [0_u8; 1];
    let peer_closed = match tls_stream.read(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error)
            if error.kind() == std::io::ErrorKind::UnexpectedEof
                || error.kind() == std::io::ErrorKind::ConnectionReset =>
        {
            true
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::TimedOut
                || error.kind() == std::io::ErrorKind::WouldBlock =>
        {
            false
        }
        Err(error) => panic!("cancel leg read failed: {error:?}"),
    };
    closed.send(peer_closed).expect("origin reports closure");
    LegOutcome::Served
    // Dropping closes the TLS stream and the socket: every leg is
    // `connection: close`, so the client never reuses the tunnel.
}

/// Reads one HTTP/1.1 request head plus its `content-length` body.
fn read_whole_request(
    stream: &mut rustls::StreamOwned<rustls::ServerConnection, StdStream>,
) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 2048];
    loop {
        if let Some(head_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..head_end]);
            let body_len = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= head_end + 4 + body_len {
                return Some(bytes);
            }
        }
        let count = match stream.read(&mut chunk) {
            Ok(0) | Err(_) if bytes.is_empty() => return None,
            Ok(count) => count,
            Err(error) => panic!("origin reads request: {error:?}"),
        };
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

/// The CONNECT proxy: refuses every authority outside `allowed` before any
/// tunnel, bridges allowed tunnels only to the loopback origin, records
/// every CONNECT under `log`, and exits when `stop` is published.
fn proxy_serve(
    listener: StdListener,
    origin: std::net::SocketAddr,
    allowed: Arc<HashSet<String>>,
    log: Arc<StdMutex<Vec<Connect>>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("proxy listener nonblocking");
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let Ok(socket) = accept_bounded(&listener, &stop) else {
                return;
            };
            handle_connect(socket, origin, &allowed, &log);
        }
    })
}

/// Runs one CONNECT: refuse a foreign authority before any tunnel, or open
/// the loopback tunnel and pump both directions until either side closes.
/// The socket timeouts bound every copy, so this cannot hang the loop.
fn handle_connect(
    mut client: StdStream,
    origin: std::net::SocketAddr,
    allowed: &Arc<HashSet<String>>,
    log: &Arc<StdMutex<Vec<Connect>>>,
) {
    client
        .set_read_timeout(Some(LEG_TIMEOUT))
        .expect("proxy read timeout");
    client
        .set_write_timeout(Some(LEG_TIMEOUT))
        .expect("proxy write timeout");
    let head = read_head(&mut client);
    let first_line = head.lines().next().unwrap_or_default().to_owned();
    let authority = first_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let admitted = allowed.contains(&authority);
    if !admitted {
        // Deny the unrecognized host, and any plain request a client writes
        // onto a tunnel the proxy already closed, before any tunnel opens.
        log.lock().expect("connect log").push(Connect {
            authority: authority.clone(),
            allowed: false,
            tunnelled: false,
        });
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\nconnection: close\r\n\r\n");
        let _ = client.flush();
        return;
    }
    let _ = client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
    let _ = client.flush();
    let origin_stream = StdStream::connect(origin);
    log.lock().expect("connect log").push(Connect {
        authority: authority.clone(),
        allowed: true,
        tunnelled: origin_stream.is_ok(),
    });
    let mut upstream = origin_stream.expect("proxy reaches the origin fixture");
    upstream
        .set_read_timeout(Some(LEG_TIMEOUT))
        .expect("origin-side read timeout");
    upstream
        .set_write_timeout(Some(LEG_TIMEOUT))
        .expect("origin-side write timeout");
    let mut client_to_origin_rx = client.try_clone().expect("clone client for relay");
    let mut upstream_to_client_rx = upstream.try_clone().expect("clone upstream for relay");
    let client_to_origin = thread::spawn(move || {
        let _ = std::io::copy(&mut client_to_origin_rx, &mut upstream);
    });
    let origin_to_client = thread::spawn(move || {
        let _ = std::io::copy(&mut upstream_to_client_rx, &mut client);
    });
    // Both directions end when either side closes; both are joined here.
    let _ = client_to_origin.join();
    let _ = origin_to_client.join();
}

fn read_head(stream: &mut StdStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
        let count = stream
            .read(&mut chunk)
            .expect("proxy reads the CONNECT head");
        assert!(count > 0, "peer closed before the CONNECT head completed");
        bytes.extend_from_slice(&chunk[..count]);
    }
}

/// One fixture TLS configuration presenting `cert` for its host.
fn fixture_tls(cert: &'static str, key: &'static str) -> Arc<rustls::ServerConfig> {
    Arc::new(
        rustls::ServerConfig::builder_with_provider(
            rustls::crypto::aws_lc_rs::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            rustls::pki_types::CertificateDer::from_pem_slice(cert.as_bytes())
                .map(|leaf| vec![leaf])
                .expect("fixture leaf parses"),
            rustls::pki_types::PrivateKeyDer::from_pem_slice(key.as_bytes())
                .expect("fixture key parses"),
        )
        .expect("fixture server config"),
    )
}

fn tool_turn_request(model: &str) -> ModelRequest {
    let mut turn = request(Family::Anthropic, model, vec![user_text("Call the tool.")]);
    turn.tools = Arc::from([ModelToolSpec {
        name: "lookup_docs".into(),
        description: "Look up the docs.".into(),
        parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#).expect("tool params"),
        grammar: None,
        defer_loading: false,
    }]);
    turn
}

/// The follow-up turn: the assistant tool call the fixture produced, then
/// its result — exactly the shape the Anthropic context projection needs.
fn tool_result_request(model: &str) -> ModelRequest {
    use dal_core::{AssistantPart, CallId, ReplaySource};
    request(
        Family::Anthropic,
        model,
        vec![
            user_text("Call the tool."),
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Anthropic,
                    model: model.into(),
                },
                parts: vec![AssistantPart::ToolCall {
                    call: CallId::new("call-1"),
                    name: "lookup_docs".into(),
                    args: RawJson::parse("{}").expect("call args"),
                    replay: None,
                }],
                replay: None,
            },
            ContextItem::ToolResult {
                call: CallId::new("call-1"),
                name: "lookup_docs".into(),
                is_error: false,
                parts: vec![Part::Text {
                    text: "the docs say hello".into(),
                }],
            },
        ],
    )
}

/// Every turn reports its usage once and ends on exactly one terminal stop.
fn assert_usage_and_single_stop(events: &[StreamEvent], label: &str) {
    assert!(
        matches!(
            events.iter().find_map(|event| match event {
                StreamEvent::Usage { usage } => Some(usage),
                _ => None,
            }),
            Some(usage) if usage.input_tokens == 1 && usage.output_tokens == 1
        ),
        "{label} reported no usage: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Stop { .. }))
            .count(),
        1,
        "{label} stopped more than once: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::Stop { .. })),
        "{label} did not end on its terminal stop: {events:?}"
    );
}

fn assert_hello(events: &[StreamEvent], label: &str) {
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello")),
        "{label} streamed no text: {events:?}"
    );
}

fn assert_tool_call(events: &[StreamEvent], label: &str) {
    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::ToolCallStarted { name, .. } if name == "lookup_docs"
        )),
        "{label} never started the tool call: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::ToolCallsDone { calls }
                if calls.len() == 1 && calls[0].name == "lookup_docs"
        )),
        "{label} never delivered the tool call: {events:?}"
    );
}

fn lower(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_ascii_lowercase()
}

/// The header policy of one captured turn: the row's one auth header, the
/// family version, the streaming accepts, and no foreign credential style.
fn assert_turn_head(captured: &[u8], row: &RowProfile, session: Option<&str>, label: &str) {
    let head = lower(captured);
    assert!(
        head.starts_with(&format!("post {} ", row.path)),
        "{label}: wrong endpoint: {head}"
    );
    assert!(
        head.contains(&format!("host: {}\r\n", row.host)),
        "{label}: wrong host: {head}"
    );
    assert!(
        head.contains(row.auth_header),
        "{label}: the wire auth header is missing: {head}"
    );
    assert!(
        head.contains("anthropic-version: 2023-06-01"),
        "{label}: the family version is missing: {head}"
    );
    assert!(
        head.contains("accept: text/event-stream"),
        "{label}: the stream accept is missing: {head}"
    );
    assert!(
        head.contains("content-type: application/json"),
        "{label}: the json content type is missing: {head}"
    );
    for absent in row.headers_absent {
        assert!(
            !head.contains(absent),
            "{label}: a foreign header leaked: {head}"
        );
    }
    match (row.session_header, session) {
        (Some(name), Some(value)) => assert!(
            head.contains(&format!("{name}: {value}\r\n")),
            "{label}: the explicit session header is missing: {head}"
        ),
        (None, None) => {}
        _ => panic!("{label}: session-header policy mismatch"),
    }
}

/// Bounded capture of one tunneled request.
fn recv_request(requests: &std_mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    requests
        .recv_timeout(STEP_TIMEOUT)
        .expect("tunneled request captured")
}

/// Asserts every tunneled turn row-major: text, tool, tool result, held.
/// Keeps the `opencode` session header verbatim per turn.
fn assert_offline_tunneled_turns(
    row_list: &[RowProfile],
    sessions: &[String],
    requests: &std_mpsc::Receiver<Vec<u8>>,
) {
    for (index, row) in row_list.iter().enumerate() {
        let base = index * 4;
        let id = row.id;
        let session_of = |leg: usize| row.session_header.map(|_| sessions[base + leg].as_str());

        let captured = recv_request(requests);
        assert_turn_head(&captured, row, session_of(0), &format!("{id} text turn"));
        assert!(
            lower(request_body(&captured)).contains(&format!(r#""{}""#, row.model)),
            "{id}: the queried model spelling is missing"
        );

        let captured = recv_request(requests);
        assert_turn_head(&captured, row, session_of(1), &format!("{id} tool turn"));
        assert!(
            lower(request_body(&captured)).contains(r#""lookup_docs""#),
            "{id}: the request carries no tool"
        );

        let captured = recv_request(requests);
        assert_turn_head(
            &captured,
            row,
            session_of(2),
            &format!("{id} tool-result turn"),
        );
        let body = lower(request_body(&captured));
        assert!(
            body.contains(r#""type":"tool_result""#)
                && body.contains(r#""tool_use_id":"call-1""#)
                && body.contains(r#""type":"tool_use""#)
                && body.contains("the docs say hello"),
            "{id}: the tool result round-trip is incomplete: {body}"
        );

        let captured = recv_request(requests);
        assert_turn_head(&captured, row, session_of(3), &format!("{id} held turn"));
        let body = lower(request_body(&captured));
        assert!(
            body.contains(&format!(r#""{}""#, row.model)),
            "{id}: the held turn carries the wrong model: {body}"
        );
        assert!(
            body.contains("hold."),
            "{id}: the held turn carries the wrong prompt: {body}"
        );
    }
}

/// Asserts the three held turns closed on cancel, drains the fixtures off
/// the runtime, and checks the CONNECT ledger exactly.
async fn assert_offline_cancel_and_ledger(
    row_list: &[RowProfile],
    closed: &std_mpsc::Receiver<bool>,
    connect_log: Arc<StdMutex<Vec<Connect>>>,
    stop_guard: StopOnDrop,
    proxy_thread: JoinHandle<()>,
    origin_thread: JoinHandle<()>,
) {
    for _ in 0..3 {
        let peer_closed = closed
            .recv_timeout(STEP_TIMEOUT)
            .expect("cancel leg observed");
        assert!(peer_closed, "cancelling must close the fixture connection");
    }

    drop(stop_guard);
    let log_cell = Arc::clone(&connect_log);
    let drained = tokio::task::spawn_blocking(move || {
        proxy_thread.join().expect("proxy drains");
        origin_thread.join().expect("origin drains");
        std::mem::take(&mut *log_cell.lock().expect("connect log"))
    })
    .await
    .expect("drain completes");

    // The tunnelled ledger is exact: four real tunnels per row in row-major
    // order, each reaching the loopback origin. Entries that opened no
    // tunnel are refused requests: the foreign probe, and a plain request a
    // client writes onto a tunnel the proxy already closed after the
    // cancelled held leg. They serve no leg, so they are counted apart and
    // bounded instead of being folded into the leg proof.
    let mut expected = Vec::with_capacity(row_list.len() * 4);
    for row in row_list {
        for _ in 0..4 {
            expected.push(Connect {
                authority: row.authority.to_owned(),
                allowed: true,
                tunnelled: true,
            });
        }
    }
    let tunnelled: Vec<Connect> = drained
        .iter()
        .filter(|entry| entry.tunnelled)
        .cloned()
        .collect();
    assert_eq!(
        tunnelled, expected,
        "every leg opens one tunnel, row-major: {drained:?}"
    );
    let refused: Vec<&Connect> = drained.iter().filter(|entry| !entry.tunnelled).collect();
    assert!(
        !refused.is_empty() && refused[0].authority == "evil.example:443" && !refused[0].allowed,
        "the foreign probe is refused first: {refused:?}"
    );
    let stray = refused.len() - 1;
    assert!(
        stray <= row_list.len(),
        "at most one stray plain request per row is tolerated: {stray} in {drained:?}"
    );
}

/// Twelve fixture legs, row-major: text, tool, tool result, held-for-cancel.
/// Builds one TLS identity per wire host and serves the leaf matching each
/// leg's host, so every row's hostname verification stays real.
fn offline_origin_plans(row_list: &[RowProfile]) -> Vec<(Arc<rustls::ServerConfig>, Leg)> {
    let tls_by_host: HashMap<String, Arc<rustls::ServerConfig>> = row_list
        .iter()
        .map(|row| {
            (
                row.host.to_owned(),
                fixture_tls(row.leaf_cert, row.leaf_key),
            )
        })
        .collect();
    let mut plans = Vec::new();
    for row in row_list {
        let tls = Arc::clone(&tls_by_host[row.host]);
        for leg in [Leg::Text, Leg::Tool, Leg::TextAfterTool, Leg::HoldForClose] {
            plans.push((Arc::clone(&tls), leg));
        }
    }
    plans
}

/// Runs the child half of the proof to completion: re-executes the test
/// binary with a cleared environment plus exactly the proxy/cert/session
/// inputs, drains both pipes while awaiting exit, and bounds the whole wait
/// with [`CHILD_TIMEOUT`]. On timeout kill and reap are each bounded by
/// [`STEP_TIMEOUT`] and their outcomes are reported in the panic;
/// `kill_on_drop(true)` re-sends kill on drop, but a timed-out reap can
/// still leave a zombie until the test process exits.
async fn run_offline_child(
    proxy_addr: std::net::SocketAddr,
    root_file: &std::path::Path,
    sessions: &[String],
) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let proxy_url = format!("http://{proxy_addr}");
    #[expect(
        clippy::disallowed_methods,
        reason = "offline proof re-executes the test binary with env_clear plus proxy/cert so reqwest reads controlled ambient without mutating the suite process; no injection seam exists"
    )]
    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
        .env_clear()
        .env(CHILD_FLAG, "1")
        .env(SESSIONS_VAR, sessions.join(","))
        .env("HTTPS_PROXY", &proxy_url)
        .env("https_proxy", &proxy_url)
        .env("SSL_CERT_FILE", root_file)
        .env("RUST_BACKTRACE", "1")
        .args(["--exact", CHILD_TEST, "--test-threads=1", "--nocapture"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("child spawns");
    let mut stdout_pipe = child.stdout.take().expect("child stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("child stderr piped");
    let child_output = async {
        use tokio::io::AsyncReadExt as _;
        let (status, stdout_read, stderr_read) = tokio::join!(
            child.wait(),
            async {
                let mut buf = Vec::new();
                stdout_pipe.read_to_end(&mut buf).await.map(|_| buf)
            },
            async {
                let mut buf = Vec::new();
                stderr_pipe.read_to_end(&mut buf).await.map(|_| buf)
            },
        );
        let status = status.expect("child waits");
        let stdout = stdout_read.expect("child stdout drained");
        let stderr = stderr_read.expect("child stderr drained");
        (status, stdout, stderr)
    };
    let Ok((status, stdout, stderr)) = tokio::time::timeout(CHILD_TIMEOUT, child_output).await
    else {
        let kill_outcome = match tokio::time::timeout(STEP_TIMEOUT, child.kill()).await {
            Ok(Ok(())) => String::from("ok"),
            Ok(Err(err)) => format!("error {err}"),
            Err(_) => format!("timed out after {STEP_TIMEOUT:?}"),
        };
        let wait_outcome = match tokio::time::timeout(STEP_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => format!("reaped {status}"),
            Ok(Err(err)) => format!("error {err}"),
            Err(_) => format!("timed out after {STEP_TIMEOUT:?}"),
        };
        panic!("the offline child timed out; kill: {kill_outcome}; reap: {wait_outcome}");
    };
    (status, stdout, stderr)
}

/// The orchestrating test. It owns the proxy, the TLS origin, and the child
/// lifecycle; it never touches its own process environment. The child does
/// every `provider.open`; the parent asserts every captured byte, the
/// cancel closures, and the CONNECT ledger, then joins its fixtures.
#[tokio::test]
async fn anthropic_override_rows_complete_offline_through_a_child_client() {
    let dir = TestDir::new().expect("temp dir");
    let root_file = dir.path().join("fixture-root.pem");
    std::fs::write(&root_file, ROOT_PEM).expect("fixture root written");

    let row_list = rows();
    let plans = offline_origin_plans(&row_list);

    let origin_listener = StdListener::bind("127.0.0.1:0").expect("origin listener");
    let origin_addr = origin_listener.local_addr().expect("origin address");
    let proxy_listener = StdListener::bind("127.0.0.1:0").expect("proxy listener");
    let proxy_addr = proxy_listener.local_addr().expect("proxy address");

    let (requests_tx, requests_rx) = std_mpsc::channel::<Vec<u8>>();
    let (closed_tx, closed_rx) = std_mpsc::channel::<bool>();
    let connect_log = Arc::new(StdMutex::new(Vec::<Connect>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    // Every exit path (return, panic, normal end) publishes the flag.
    let stop_guard = StopOnDrop(Arc::clone(&stop));

    let allowed: Arc<HashSet<String>> = Arc::new(
        row_list
            .iter()
            .map(|row| row.authority.to_owned())
            .collect(),
    );
    let proxy_thread = proxy_serve(
        proxy_listener,
        origin_addr,
        Arc::clone(&allowed),
        Arc::clone(&connect_log),
        Arc::clone(&stop),
    );
    let origin_thread = origin_serve(
        origin_listener,
        plans,
        requests_tx,
        closed_tx,
        Arc::clone(&stop),
    );

    // A foreign authority is denied before any tunnel is opened; the probe
    // is a raw socket, so no provider code can be involved.
    {
        let mut probe = StdStream::connect(proxy_addr).expect("probe connects");
        probe
            .set_read_timeout(Some(STEP_TIMEOUT))
            .expect("probe read timeout");
        probe
            .set_write_timeout(Some(STEP_TIMEOUT))
            .expect("probe write timeout");
        probe
            .write_all(b"CONNECT evil.example:443 HTTP/1.1\r\nHost: evil.example:443\r\n\r\n")
            .expect("probe writes");
        let verdict = read_head(&mut probe);
        assert!(
            verdict.starts_with("HTTP/1.1 403 "),
            "foreign CONNECT not denied: {verdict}"
        );
    }

    // Twelve explicit sessions, passed to the child so the parent can
    // assert the opencode session header verbatim.
    let sessions: Vec<String> = (0..12).map(|_| SessionId::new_v7().to_string()).collect();
    let (status, stdout, stderr) = run_offline_child(proxy_addr, &root_file, &sessions).await;
    assert!(
        status.success(),
        "the offline child failed: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        status,
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );

    assert_offline_tunneled_turns(&row_list, &sessions, &requests_rx);
    assert_offline_cancel_and_ledger(
        &row_list,
        &closed_rx,
        connect_log,
        stop_guard,
        proxy_thread,
        origin_thread,
    )
    .await;
}

/// The child half of the proof: three rows, four real `provider.open` turns
/// each, against the orchestrator's fixtures, with the environment the OS
/// gave this process at spawn. Without [`CHILD_FLAG`] this is a no-op, so a
/// plain suite run never re-executes anything.
#[tokio::test]
async fn served_anthropic_override_rows_offline() -> TestResult {
    let snapshot = EnvSnapshot::capture();
    if snapshot.get(CHILD_FLAG) != Some("1") {
        return Ok(());
    }
    let session_text = snapshot.get(SESSIONS_VAR).expect("session list present");
    let sessions: Vec<SessionId> = session_text
        .split(',')
        .map(SessionId::parse)
        .collect::<Result<_, _>>()
        .expect("twelve parseable session ids");
    for (index, row) in rows().into_iter().enumerate() {
        let base = index * 4;
        let dir = TestDir::new().expect("child temp dir");
        let set = ProviderSet::new(
            &config(vec![wired_row(row.id)], 0),
            identity(),
            EnvSnapshot::test(row.env),
            dir.path(),
            dir.path(),
        )?;
        let provider = set
            .provider(claude_route(&row))
            .map_err(|error| format!("{}: provider resolve: {error}", row.id))?;

        let label = format!("{} text turn", row.id);
        let events = collect(
            provider
                .open(
                    sessions[base],
                    &request(Family::Anthropic, row.model, vec![user_text("Say hello.")]),
                    &[],
                    notice_sink(),
                    &CancellationToken::new(),
                )
                .await
                .map_err(|error| format!("{label}: open: {error}"))?,
        )
        .await
        .map_err(|error| format!("{label}: stream: {error}"))?;
        assert_hello(&events, &label);
        assert_usage_and_single_stop(&events, &label);

        let label = format!("{} tool turn", row.id);
        let events = collect(
            provider
                .open(
                    sessions[base + 1],
                    &tool_turn_request(row.model),
                    &[],
                    notice_sink(),
                    &CancellationToken::new(),
                )
                .await
                .map_err(|error| format!("{label}: open: {error}"))?,
        )
        .await
        .map_err(|error| format!("{label}: stream: {error}"))?;
        assert_tool_call(&events, &label);
        assert_usage_and_single_stop(&events, &label);

        let label = format!("{} tool-result turn", row.id);
        let events = collect(
            provider
                .open(
                    sessions[base + 2],
                    &tool_result_request(row.model),
                    &[],
                    notice_sink(),
                    &CancellationToken::new(),
                )
                .await
                .map_err(|error| format!("{label}: open: {error}"))?,
        )
        .await
        .map_err(|error| format!("{label}: stream: {error}"))?;
        assert_hello(&events, &label);
        assert_usage_and_single_stop(&events, &label);

        held_turn_cancels_silently(&provider, sessions[base + 3], row.id, row.model).await?;
    }
    Ok(())
}

/// The held turn: `open` hands back a lazy stream, so driving it is what
/// puts the request on the wire. The fixture answers nothing on the held
/// leg, so the drain must still be pending after the settle window;
/// cancelling then silences the stream by contract — no event, above all no
/// invented stop — until the caller drops it, the one bounded end a
/// cancelled turn has.
async fn held_turn_cancels_silently(
    provider: &Provider,
    session: SessionId,
    id: &str,
    model: &str,
) -> TestResult {
    let cancel = CancellationToken::new();
    let held_request = request(Family::Anthropic, model, vec![user_text("Hold.")]);
    let held = provider
        .open(session, &held_request, &[], notice_sink(), &cancel)
        .await?;
    #[expect(
        clippy::disallowed_methods,
        reason = "test edge: the held turn must be driven concurrently with its cancel"
    )]
    let mut drain = tokio::spawn(collect(held));
    if let Ok(finished) = tokio::time::timeout(Duration::from_millis(500), &mut drain).await {
        match finished {
            Ok(Ok(events)) => panic!(
                "{id}: the held turn ended with {} events before cancellation",
                events.len()
            ),
            Ok(Err(err)) => panic!("{id}: the held turn failed before cancellation: {err:?}"),
            Err(join) => panic!("{id}: the held drain task failed: {join}"),
        }
    }
    cancel.cancel();
    if let Ok(finished) = tokio::time::timeout(CLOSE_WAIT, &mut drain).await {
        match finished {
            Ok(Ok(events)) => {
                let stops = events
                    .iter()
                    .filter(|event| matches!(event, StreamEvent::Stop { .. }))
                    .count();
                panic!(
                    "{id}: a cancelled turn invented {} events and {stops} stops: {events:?}",
                    events.len()
                )
            }
            Ok(Err(err)) => panic!("{id}: a cancelled turn failed after cancellation: {err:?}"),
            Err(join) => panic!("{id}: the held drain task failed: {join}"),
        }
    }
    drain.abort();
    let _ = drain.await;
    Ok(())
}
