//! Public-destination policy for the extension network service.
//!
//! Validate URLs and every DNS answer before connecting. Pin the checked
//! addresses to this one request, bypass ambient proxies, and do not follow
//! redirects: a caller must submit each new hop through this same policy.

use std::net::{IpAddr, SocketAddr};

use dal_core::{FetchMethod, FetchRequest, FetchResponse, Service};
use url::Host;

use crate::error::ServiceError;

const INVALID_URL: &str = "network requests require an HTTP(S) URL without credentials";
const PRIVATE_DESTINATION: &str = "network destination must resolve only to public addresses";

fn rejected(message: &'static str) -> ServiceError {
    ServiceError::failed(Some(Service::Net), message)
}

/// Accept only addresses that are publicly routable, not merely non-loopback.
/// This is deliberately conservative for special-purpose address blocks.
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            !(matches!(a, 0 | 10 | 127 | 224..=255)
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99)))
                || (a == 198 && ((18..=19).contains(&b) || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(address) => {
            let [first, second, ..] = address.segments();
            address.to_ipv4_mapped().is_none()
                && (first & 0xe000) == 0x2000
                && first != 0x2002 // 6to4 can tunnel to internal IPv4 addresses.
                && !(first == 0x2001 && (second <= 0x01ff || second == 0x0db8))
        }
    }
}

fn check_addresses(addresses: &[SocketAddr]) -> Result<(), ServiceError> {
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err(rejected(PRIVATE_DESTINATION));
    }
    Ok(())
}

/// Returns pinned DNS answers for a domain, or None for a checked IP literal.
/// Refuse mixed public/private answers rather than let a client choose one.
async fn public_destination(
    url: &reqwest::Url,
) -> Result<Option<(String, Vec<SocketAddr>)>, ServiceError> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(rejected(INVALID_URL));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| rejected(INVALID_URL))?;
    match url.host().ok_or_else(|| rejected(INVALID_URL))? {
        Host::Ipv4(address) => {
            check_addresses(&[SocketAddr::new(IpAddr::V4(address), port)])?;
            Ok(None)
        }
        Host::Ipv6(address) => {
            check_addresses(&[SocketAddr::new(IpAddr::V6(address), port)])?;
            Ok(None)
        }
        Host::Domain(name) => {
            if name == "localhost"
                || name.ends_with(".localhost")
                || name.ends_with(".local")
                || name.ends_with(".internal")
            {
                return Err(rejected(PRIVATE_DESTINATION));
            }
            let addresses: Vec<_> = tokio::net::lookup_host((name, port))
                .await
                .map_err(|_| rejected("could not resolve network destination"))?
                .collect();
            check_addresses(&addresses)?;
            Ok(Some((name.to_owned(), addresses)))
        }
    }
}

/// Only a checked address can reach the socket. TLS keeps the URL hostname,
/// including certificate validation, when the pinned IP differs from DNS.
fn client(pinned: Option<(&str, &[SocketAddr])>) -> Result<reqwest::Client, ServiceError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(dal_provider::CONNECT_TIMEOUT)
        .read_timeout(dal_provider::STREAM_IDLE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .referer(false)
        .no_proxy();
    if let Some((name, addresses)) = pinned {
        builder = builder.resolve_to_addrs(name, addresses);
    }
    builder
        .build()
        .map_err(|_| rejected("could not initialize network client"))
}

/// Perform one network exchange without redirects or unchecked DNS lookups.
pub(super) async fn fetch(req: FetchRequest) -> Result<FetchResponse, ServiceError> {
    let url = reqwest::Url::parse(req.url.as_ref()).map_err(|_| rejected(INVALID_URL))?;
    let pinned = public_destination(&url).await?;
    let client = client(
        pinned
            .as_ref()
            .map(|(name, addresses)| (name.as_str(), addresses.as_slice())),
    )?;
    let method = match req.method {
        FetchMethod::Post => reqwest::Method::POST,
        FetchMethod::Put => reqwest::Method::PUT,
        FetchMethod::Delete => reqwest::Method::DELETE,
        FetchMethod::Head => reqwest::Method::HEAD,
        FetchMethod::Options => reqwest::Method::OPTIONS,
        FetchMethod::Patch => reqwest::Method::PATCH,
        _ => reqwest::Method::GET,
    };
    let mut outgoing = client.request(method, url).body(req.body);
    for (name, value) in req.headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(&value),
        ) {
            outgoing = outgoing.header(name, value);
        }
    }
    let failure = FetchResponse {
        status: 0,
        headers: Vec::new(),
        body: Vec::new(),
    };
    let Ok(mut response) = outgoing.send().await else {
        return Ok(failure);
    };
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().into(), value.to_str().unwrap_or("").into()))
        .collect();
    let mut body = Vec::new();
    while body.len() < dal_provider::BODY_LIMIT
        && let Ok(Some(chunk)) = response.chunk().await
    {
        let room = dal_provider::BODY_LIMIT - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    Ok(FetchResponse {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, SocketAddr};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{check_addresses, client, public_destination, public_ip};

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn refuses_non_public_ipv4_and_ipv6() -> TestResult {
        for text in [
            "0.0.0.0",
            "10.2.3.4",
            "100.100.100.200",
            "127.0.0.1",
            "169.254.169.254",
            "172.31.4.2",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.2",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "2002::1",
            "64:ff9b::7f00:1",
        ] {
            let address: IpAddr = text.parse()?;
            assert!(!public_ip(address), "accepted non-public address {text}");
        }
        for text in [
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "2001:4860::8888",
        ] {
            let address: IpAddr = text.parse()?;
            assert!(public_ip(address), "refused public address {text}");
        }
        Ok(())
    }

    #[test]
    fn refuses_mixed_dns_results_and_empty_answers() -> TestResult {
        let public: SocketAddr = "1.1.1.1:443".parse()?;
        let private: SocketAddr = "10.0.0.3:443".parse()?;
        assert!(check_addresses(&[]).is_err());
        assert!(check_addresses(&[public, private]).is_err());
        assert!(check_addresses(&[public]).is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn refuses_internal_literal_urls_before_connecting() -> TestResult {
        for raw in [
            "http://127.1/",
            "https://169.254.169.254/latest/meta-data/",
            "https://[::1]/",
            "http://[::ffff:10.0.0.1]/",
            "http://localhost/",
            "http://example.local/",
            "http://example.internal/",
            "ftp://1.1.1.1/",
            "https://user:pass@1.1.1.1/",
        ] {
            let url = reqwest::Url::parse(raw)?;
            assert!(
                public_destination(&url).await.is_err(),
                "accepted internal or invalid URL {raw}"
            );
        }
        let public = reqwest::Url::parse("https://1.1.1.1/")?;
        assert!(public_destination(&public).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn the_client_returns_redirects_without_following_them() -> TestResult {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut request = [0_u8; 1024];
            stream.read(&mut request).await?;
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
        });
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        // The test-only pin reaches a local fixture; the production caller
        // checks public_ip before it can construct this client.
        let client = client(Some(("example.test", &[address])))?;
        let response = client
            .get(format!("http://example.test:{port}/start"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server.await??;
        Ok(())
    }
}
