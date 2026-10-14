// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

use super::{AddressClass, Guard, Lookup, LookupFuture, classify_address, is_global};
use crate::web::WebError;

fn ip(text: &str) -> IpAddr {
    text.parse().expect("test address")
}

fn url(text: &str) -> reqwest::Url {
    reqwest::Url::parse(text).expect("test url")
}

#[test]
fn classifies_every_private_range_and_its_boundaries() {
    use AddressClass::{Global, LinkLocal, Loopback, Other, Private};
    let cases = [
        ("127.0.0.1", Loopback),
        ("127.255.255.255", Loopback),
        ("10.0.0.0", Private),
        ("10.255.255.255", Private),
        ("172.15.255.255", Global),
        ("172.16.0.0", Private),
        ("172.31.255.255", Private),
        ("172.32.0.0", Global),
        ("192.167.255.255", Global),
        ("192.168.0.0", Private),
        ("192.168.255.255", Private),
        ("192.169.0.0", Global),
        ("169.253.255.255", Global),
        ("169.254.0.0", LinkLocal),
        ("169.254.169.254", LinkLocal),
        ("169.255.0.0", Global),
        ("0.0.0.0", Other),
        ("100.63.255.255", Global),
        ("100.64.0.0", Other),
        ("100.127.255.255", Other),
        ("100.128.0.0", Global),
        ("198.17.255.255", Global),
        ("198.18.0.0", Other),
        ("198.19.255.255", Other),
        ("198.20.0.0", Global),
        ("192.0.2.1", Other),
        ("224.0.0.1", Other),
        ("240.0.0.1", Other),
        ("255.255.255.255", Other),
        ("1.1.1.1", Global),
        ("93.184.216.34", Global),
        ("::1", Loopback),
        ("::", Other),
        ("fe80::1", LinkLocal),
        ("febf::1", LinkLocal),
        ("fec0::1", Private),
        ("feff::1", Private),
        ("fc00::1", Private),
        ("fdff::1", Private),
        ("100::1", Other),
        ("0.1.2.3", Other),
        ("::a00:1", Private),
        ("::7f00:1", Loopback),
        ("::2", Other),
        ("::101:101", Global),
        ("64:ff9b::a00:1", Private),
        ("64:ff9b::a9fe:a9fe", LinkLocal),
        ("64:ff9b::101:101", Global),
        ("ff02::1", Other),
        ("2001:db8::1", Other),
        ("2001:db9::1", Global),
        ("::ffff:127.0.0.1", Loopback),
        ("::ffff:10.0.0.1", Private),
        ("::ffff:169.254.169.254", LinkLocal),
        ("::ffff:1.1.1.1", Global),
        ("2606:4700:4700::1111", Global),
    ];
    for (address, class) in cases {
        assert_eq!(classify_address(ip(address)), class, "{address}");
    }
}

#[tokio::test]
async fn refuses_literal_and_normalised_private_hosts_with_the_exact_text() {
    // The URL parser folds decimal, hex, short, and mapped spellings into the
    // address the socket would reach; the gate classifies that address.
    let cases = [
        ("http://127.0.0.1:8080/", "127.0.0.1"),
        ("http://[::1]/", "[::1]"),
        (
            "http://169.254.169.254/latest/meta-data/",
            "169.254.169.254",
        ),
        ("http://10.0.0.1/", "10.0.0.1"),
        ("http://172.16.5.4/", "172.16.5.4"),
        ("http://192.168.1.1/", "192.168.1.1"),
        ("http://0.0.0.0/", "0.0.0.0"),
        ("http://[::]/", "[::]"),
        ("http://[fc00::1]/", "[fc00::1]"),
        ("http://[fe80::1]/", "[fe80::1]"),
        ("http://[::ffff:10.0.0.1]/", "[::ffff:a00:1]"),
        ("http://2130706433/", "127.0.0.1"),
        ("http://0x7f.1/", "127.0.0.1"),
        ("http://127.1/", "127.0.0.1"),
        ("http://user:pw@127.0.0.1/", "127.0.0.1"),
        ("https://10.1.2.3/", "10.1.2.3"),
    ];
    let guard = Guard::system();
    for (target, host) in cases {
        let error = guard
            .pin(&url(target))
            .await
            .expect_err("a private literal must refuse");
        assert!(matches!(error, WebError::PrivateAddress { .. }), "{target}");
        assert_eq!(
            error.to_string(),
            format!("web_fetch: {host} is a private address; fetch public URLs only."),
            "{target}"
        );
    }
}

#[tokio::test]
async fn passes_global_literals_unchanged() {
    let guard = Guard::system();
    for target in [
        "http://1.1.1.1:8080/x?q=1",
        "https://93.184.216.34/",
        "http://[2606:4700:4700::1111]/",
    ] {
        let pinned = guard.pin(&url(target)).await.expect("global literal");
        assert_eq!(pinned.url.as_str(), url(target).as_str(), "{target}");
        assert!(pinned.host_header.is_none(), "{target}");
    }
}

#[tokio::test]
async fn refuses_a_name_the_platform_resolves_to_loopback() {
    // `localhost` resolves through the real platform resolver.
    let error = Guard::system()
        .pin(&url("http://localhost:1/"))
        .await
        .expect_err("localhost is loopback");
    assert_eq!(
        error.to_string(),
        "web_fetch: localhost is a private address; fetch public URLs only."
    );
}

/// A resolver with a fixed answer or failure.
struct FixedLookup(Result<Vec<IpAddr>, &'static str>);

impl Lookup for FixedLookup {
    fn lookup<'a>(&'a self, _host: &'a str) -> LookupFuture<'a> {
        let answer = self.0.clone().map_err(io::Error::other);
        Box::pin(async move { answer })
    }
}

fn fixed(answer: Result<Vec<IpAddr>, &'static str>) -> Guard {
    Guard {
        lookup: Arc::new(FixedLookup(answer)),
        public: is_global,
    }
}

#[tokio::test]
async fn one_private_address_among_public_ones_refuses_the_name() {
    for answer in [
        vec![ip("93.184.216.34"), ip("10.0.0.5")],
        vec![ip("10.0.0.5"), ip("93.184.216.34")],
        vec![ip("2606:4700::1"), ip("fc00::7")],
    ] {
        let error = fixed(Ok(answer))
            .pin(&url("http://mixed.test/"))
            .await
            .expect_err("a mixed answer refuses");
        assert_eq!(
            error.to_string(),
            "web_fetch: mixed.test is a private address; fetch public URLs only."
        );
    }
}

#[tokio::test]
async fn pins_an_http_hop_to_the_first_validated_address() {
    let guard = fixed(Ok(vec![ip("93.184.216.34"), ip("1.1.1.1")]));
    let pinned = guard
        .pin(&url("http://example.test:8080/path?q=1"))
        .await
        .expect("public answer");
    assert_eq!(pinned.url.as_str(), "http://93.184.216.34:8080/path?q=1");
    assert_eq!(pinned.host_header.as_deref(), Some("example.test:8080"));

    let pinned = guard
        .pin(&url("http://example.test/"))
        .await
        .expect("public answer");
    assert_eq!(pinned.url.as_str(), "http://93.184.216.34/");
    assert_eq!(pinned.host_header.as_deref(), Some("example.test"));
}

#[tokio::test]
async fn pins_an_http_hop_to_an_ipv6_address_in_brackets() {
    let pinned = fixed(Ok(vec![ip("2606:4700:4700::1111")]))
        .pin(&url("http://example.test/"))
        .await
        .expect("public answer");
    assert_eq!(pinned.url.as_str(), "http://[2606:4700:4700::1111]/");
}

#[tokio::test]
async fn keeps_the_hostname_on_an_https_hop_so_tls_checks_the_name() {
    let pinned = fixed(Ok(vec![ip("93.184.216.34")]))
        .pin(&url("https://example.test/a"))
        .await
        .expect("public answer");
    assert_eq!(pinned.url.as_str(), "https://example.test/a");
    assert!(pinned.host_header.is_none());
}

#[tokio::test]
async fn an_empty_or_failed_lookup_is_a_network_error_not_a_pass() {
    let empty = fixed(Ok(Vec::new()))
        .pin(&url("http://gone.test/"))
        .await
        .expect_err("empty answer");
    assert_eq!(
        empty.to_string(),
        "web_fetch failed: gone.test did not resolve to an address"
    );
    let failed = fixed(Err("no such host"))
        .pin(&url("http://gone.test/"))
        .await
        .expect_err("failed lookup");
    assert_eq!(failed.to_string(), "web_fetch failed: no such host");
}

/// A resolver that never answers.
struct Stalled;

impl Lookup for Stalled {
    fn lookup<'a>(&'a self, _host: &'a str) -> LookupFuture<'a> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn a_stalled_resolver_times_out_like_a_stalled_fetch() {
    let guard = Guard {
        lookup: Arc::new(Stalled),
        public: is_global,
    };
    let error = guard
        .pin_within(&url("http://slow.test/"), 3)
        .await
        .expect_err("the resolver never answers");
    assert_eq!(
        error.to_string(),
        "web_fetch timed out after 3s: http://slow.test/"
    );
}

// --- DNS rebinding, with a real DNS server and real listeners ---------------

/// Builds an A query for `host`.
fn dns_query(host: &str) -> Vec<u8> {
    let mut message = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in host.split('.') {
        message.push(u8::try_from(label.len()).expect("short label"));
        message.extend_from_slice(label.as_bytes());
    }
    message.extend_from_slice(&[0, 0, 1, 0, 1]);
    message
}

/// Answers a query with one A record for `address`.
fn dns_reply(query: &[u8], address: Ipv4Addr) -> Vec<u8> {
    let mut end = 12;
    while query[end] != 0 {
        end += usize::from(query[end]) + 1;
    }
    let question_end = end + 5;
    let mut reply = vec![query[0], query[1], 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0];
    reply.extend_from_slice(&query[12..question_end]);
    reply.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 0, 0, 4]);
    reply.extend_from_slice(&address.octets());
    reply
}

/// A resolver that asks a DNS server over UDP, one datagram each way.
struct UdpLookup {
    server: SocketAddr,
}

impl Lookup for UdpLookup {
    fn lookup<'a>(&'a self, host: &'a str) -> LookupFuture<'a> {
        Box::pin(async move {
            let socket = UdpSocket::bind("127.0.0.1:0").await?;
            socket.send_to(&dns_query(host), self.server).await?;
            let mut buffer = [0u8; 512];
            let (length, _) = socket.recv_from(&mut buffer).await?;
            let rdata: [u8; 4] = buffer[length - 4..length]
                .try_into()
                .map_err(io::Error::other)?;
            Ok(vec![IpAddr::V4(Ipv4Addr::from(rdata))])
        })
    }
}

/// Answers one query per entry of `answers`, in order, counting each query.
async fn serve_dns(socket: &UdpSocket, answers: &[Ipv4Addr], queries: &AtomicUsize) {
    for answer in answers {
        let mut buffer = [0u8; 512];
        let (length, peer) = socket.recv_from(&mut buffer).await.expect("dns query");
        queries.fetch_add(1, Ordering::SeqCst);
        socket
            .send_to(&dns_reply(&buffer[..length], *answer), peer)
            .await
            .expect("dns reply");
    }
}

/// Accepts one HTTP connection, answers `ok`, and returns the request head.
async fn serve_one(listener: &TcpListener) -> String {
    let (mut stream, _) = listener.accept().await.expect("accept");
    let mut head = Vec::new();
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let length = stream.read(&mut buffer).await.expect("read request");
        if length == 0 {
            break;
        }
        head.extend_from_slice(&buffer[..length]);
    }
    stream
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
        .await
        .expect("write reply");
    String::from_utf8_lossy(&head).into_owned()
}

/// Whether any connection reaches `listener` within `window`.
async fn connection_arrives(listener: &TcpListener, window: Duration) -> bool {
    tokio::time::timeout(window, listener.accept())
        .await
        .is_ok()
}

/// Binds one port on both `127.0.0.2` (the public stand-in) and `127.0.0.1`
/// (the internal service the rebinding answer points at).
async fn paired_listeners() -> (TcpListener, TcpListener, u16) {
    for _ in 0..64 {
        let public = TcpListener::bind("127.0.0.2:0").await.expect("bind public");
        let port = public.local_addr().expect("public port").port();
        if let Ok(internal) = TcpListener::bind(("127.0.0.1", port)).await {
            return (public, internal, port);
        }
    }
    panic!("no port is free on both loopback addresses");
}

#[tokio::test]
async fn a_rebinding_name_connects_only_to_the_validated_address() {
    // `rebind.test` answers 127.0.0.2 once, then 127.0.0.1: the first answer
    // is the "public" origin (the test trusts 127.0.0.2), the second is the
    // internal service an attacker wants the fetch to reach.
    let (public, internal, port) = paired_listeners().await;
    let dns = UdpSocket::bind("127.0.0.1:0").await.expect("bind dns");
    let server = dns.local_addr().expect("dns address");
    let queries = AtomicUsize::new(0);
    let guard = Guard {
        lookup: Arc::new(UdpLookup { server }),
        public: |address| address == ip("127.0.0.2") || is_global(address),
    };
    let target = url(&format!("http://rebind.test:{port}/secret"));

    let client = async {
        let pinned = guard
            .pin(&target)
            .await
            .expect("the first answer validates");
        assert_eq!(pinned.url.host_str(), Some("127.0.0.2"));
        let host_header = pinned.host_header.clone().expect("the original authority");
        assert_eq!(host_header, format!("rebind.test:{port}"));

        // The real connection: no name remains in the URL, so the HTTP client
        // performs no lookup of its own and the flip cannot reach it.
        let body = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client")
            .get(pinned.url.clone())
            .header("Host", &host_header)
            .send()
            .await
            .expect("the validated address answers")
            .text()
            .await
            .expect("body");
        let lookups_after_fetch = queries.load(Ordering::SeqCst);

        // A fresh resolution now yields the rebound answer and is refused.
        let rebound = guard.pin(&target).await.expect_err("the rebound answer");
        (body, lookups_after_fetch, rebound)
    };

    let answers = [Ipv4Addr::new(127, 0, 0, 2), Ipv4Addr::LOCALHOST];
    let ((), head, internal_reached, (body, lookups_after_fetch, rebound)) = tokio::join!(
        serve_dns(&dns, &answers, &queries),
        serve_one(&public),
        connection_arrives(&internal, Duration::from_millis(400)),
        client,
    );

    assert_eq!(body, "ok");
    assert!(
        head.to_ascii_lowercase()
            .contains(&format!("host: rebind.test:{port}")),
        "the origin must still see the requested virtual host: {head}"
    );
    assert_eq!(
        lookups_after_fetch, 1,
        "the connection must not resolve the name again"
    );
    assert!(
        !internal_reached,
        "the internal service must see no connection"
    );
    assert_eq!(
        rebound.to_string(),
        "web_fetch: rebind.test is a private address; fetch public URLs only."
    );
    assert_eq!(queries.load(Ordering::SeqCst), 2);
}
