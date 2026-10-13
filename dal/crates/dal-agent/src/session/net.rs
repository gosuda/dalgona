//! Public-network admission for the extension Net service.
//!
//! The grant is not permission to reach the host's private network. Resolve and
//! validate every address before connecting, pin that answer to a single-hop
//! client, and keep proxy and redirect handling out of the transport.

use std::net::{IpAddr, SocketAddr};

use dal_core::{FetchMethod, FetchRequest, FetchResponse, Service};
use url::Host;

use crate::error::ServiceError;

fn denied(reason: impl Into<String>) -> ServiceError {
    ServiceError::failed(Some(Service::Net), reason.into())
}

fn target(url: &reqwest::Url) -> Result<(&str, u16), ServiceError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(denied("network URL must use http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(denied("network URL must not contain credentials"));
    }
    let host = url.host_str().ok_or_else(|| denied("network URL needs a host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| denied("network URL needs a port"))?;
    Ok((host, port))
}

/// Only globally routable unicast addresses may be extension Net destinations.
/// Fail closed on special-use ranges, including IPv4-mapped and transition IPv6.
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            !matches!(
                (a, b, c),
                (0 | 10 | 127 | 224..=255, _, _)
                    | (100, 64..=127, _)
                    | (169, 254, _)
                    | (172, 16..=31, _)
                    | (192, 0, 0 | 2)
                    | (192, 88, 99)
                    | (192, 168, _)
                    | (198, 18 | 19, _)
                    | (198, 51, 100)
                    | (203, 0, 113)
            )
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            (0x2000..=0x3fff).contains(&segments[0])
                && !(segments[0] == 0x2001
                    && (segments[1] <= 0x01ff || segments[1] == 0x0db8))
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] <= 0x0fff)
        }
    }
}

/// Refuse the entire DNS answer if even one address is not public. Filtering
/// only the private answers would hide a rebinding or split-horizon hostname.
fn checked_addresses(addresses: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, ServiceError> {
    if addresses.is_empty() || addresses.iter().any(|addr| !public_ip(addr.ip())) {
        return Err(denied("network destination must resolve only to public IP addresses"));
    }
    Ok(addresses)
}

async fn admitted_target(url: &reqwest::Url) -> Result<(String, Vec<SocketAddr>), ServiceError> {
    let (host, port) = target(url)?;
    let addresses = match url.host() {
        Some(Host::Ipv4(ip)) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Some(Host::Ipv6(ip)) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        Some(Host::Domain(name)) => tokio::net::lookup_host((name, port))
            .await
            .map_err(|error| denied(format!("network DNS lookup failed: {error}")))?
            .collect(),
        None => return Err(denied("network URL needs a host")),
    };
    Ok((host.to_owned(), checked_addresses(addresses)?))
}

/// A new client is constructed for each approved DNS answer, so a later DNS
/// change cannot move the TCP/TLS connection to a different address. The URL
/// host remains unchanged for HTTP authority, SNI and certificate validation.
fn single_hop_client(
    host: &str,
    addresses: &[SocketAddr],
) -> Result<reqwest::Client, ServiceError> {
    reqwest::Client::builder()
        .connect_timeout(dal_provider::CONNECT_TIMEOUT)
        .read_timeout(dal_provider::STREAM_IDLE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .retry(reqwest::retry::never())
        .no_proxy()
        .resolve_to_addrs(host, addresses)
        .build()
        .map_err(|error| denied(format!("network client initialization failed: {error}")))
}

/// Executes exactly one approved HTTP request. Redirects are returned to the
/// caller as 3xx responses so each subsequent hop must pass the same check.
pub(super) async fn send(req: FetchRequest) -> Result<FetchResponse, ServiceError> {
    let url = reqwest::Url::parse(req.url.as_ref())
        .map_err(|error| denied(format!("invalid network URL: {error}")))?;
    let (host, addresses) = admitted_target(&url).await?;
    let client = single_hop_client(&host, &addresses)?;

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
            // A caller-supplied Host header cannot override the checked URL authority.
            if name != reqwest::header::HOST {
                outgoing = outgoing.header(name, value);
            }
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

    use super::{checked_addresses, public_ip, single_hop_client, target};

    fn addr(raw: &str) -> IpAddr {
        raw.parse().expect("test IP address")
    }

    #[test]
    fn refuses_non_public_ipv4() {
        for raw in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.10",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!public_ip(addr(raw)), "{raw} must be denied");
        }
    }

    #[test]
    fn refuses_non_public_ipv6_and_tunnels() {
        for raw in [
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "64:ff9b::7f00:1",
            "2001::1",
            "2001:db8::1",
            "2002:c0a8:101::1",
            "3fff::1",
        ] {
            assert!(!public_ip(addr(raw)), "{raw} must be denied");
        }
    }

    #[test]
    fn admits_public_unicast_addresses() {
        for raw in ["1.1.1.1", "8.8.8.8", "2001:4860:4860::8888", "2606:4700:4700::1111"] {
            assert!(public_ip(addr(raw)), "{raw} must be admitted");
        }
    }

    #[test]
    fn fails_closed_on_empty_and_mixed_dns_answers() {
        let public = SocketAddr::new(addr("1.1.1.1"), 443);
        let private = SocketAddr::new(addr("10.0.0.1"), 443);
        assert!(checked_addresses(vec![]).is_err());
        assert!(checked_addresses(vec![private]).is_err());
        assert!(checked_addresses(vec![public, private]).is_err());
        assert_eq!(checked_addresses(vec![public]).expect("public DNS"), vec![public]);
        // A rebinding response is rejected even if an earlier answer was public.
        assert!(checked_addresses(vec![private]).is_err());
    }

    #[test]
    fn checks_urls_before_resolution() {
        for raw in ["ftp://example.test/", "http://user:pass@example.test/"] {
            let url = reqwest::Url::parse(raw).expect("test URL");
            assert!(target(&url).is_err(), "{raw} must be denied");
        }
        let url = reqwest::Url::parse("http://2130706433/").expect("numeric loopback");
        assert!(matches!(url.host(), Some(url::Host::Ipv4(_))));
        assert!(!public_ip(addr(url.host_str().expect("literal host"))));
    }

    #[tokio::test]
    async fn client_pins_dns_and_exposes_redirect_without_following_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let local = listener.local_addr().expect("bound port");
        let server = async move {
            let (mut connection, _) = listener.accept().await.expect("one connection");
            assert_eq!(
                connection.read_u8().await.expect("first HTTP request byte"),
                b'G'
            );
            connection
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write redirect");
        };

        // This helper tests the transport in isolation: public-IP admission is
        // covered above. The reserved .invalid domain cannot resolve normally.
        let client = single_hop_client("rebinding.invalid", &[local])
            .expect("client with pinned test address");
        let fetch = async {
            let response = client
                .get(format!("http://rebinding.invalid:{}/", local.port()))
                .send()
                .await
                .expect("pinned single-hop response");
            assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        };
        tokio::join!(server, fetch);
    }
}
