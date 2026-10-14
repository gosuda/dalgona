// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The connect-time address gate for `web_fetch`.
//!
//! The host `net` service resolves and connects on its own, so a name checked
//! in one lookup and fetched through another could change its answer in
//! between (DNS rebinding). The gate therefore resolves a hostname exactly
//! once, classifies every address of the answer, and hands the request the
//! validated address: an `http` hop connects to that IP with the original
//! `Host` header, so no second lookup exists to flip. An `https` hop keeps the
//! hostname, because the TLS name check must see it; there the certificate
//! binds the connection to the checked name.
//!
//! Loopback, link-local, RFC 1918, unique-local, and unspecified ranges
//! refuse, as do the other non-global blocks listed in [`AddressClass`].

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::{ToolLabel, WebError};

/// The address classes the web battery distinguishes. Only `Global` passes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AddressClass {
    Global,
    Loopback,
    LinkLocal,
    Private,
    Other,
}

/// Classifies one resolved address. IPv4-mapped IPv6 addresses take the class
/// of their embedded IPv4 form, so `::ffff:10.0.0.1` is private like `10.0.0.1`.
fn classify_address(address: IpAddr) -> AddressClass {
    match address {
        IpAddr::V4(address) => classify_ipv4(address),
        IpAddr::V6(address) => classify_ipv6(address),
    }
}

fn classify_ipv4(address: Ipv4Addr) -> AddressClass {
    let octets = address.octets();
    if address.is_loopback() {
        AddressClass::Loopback
    } else if octets[0] == 169 && octets[1] == 254 {
        AddressClass::LinkLocal
    } else if address.is_private() {
        AddressClass::Private
    } else if octets[0] == 0
        || address.is_broadcast()
        || address.is_multicast()
        // Shared 100.64.0.0/10, IETF 192.0.0.0/24, benchmarking 198.18.0.0/15,
        // the three documentation blocks, and reserved 240.0.0.0/4 never name a
        // public origin.
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && (octets[2] == 0 || octets[2] == 2))
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        || octets[0] >= 240
    {
        AddressClass::Other
    } else {
        AddressClass::Global
    }
}

fn classify_ipv6(address: Ipv6Addr) -> AddressClass {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return classify_ipv4(mapped);
    }
    let segments = address.segments();
    if address.is_loopback() {
        return AddressClass::Loopback;
    }
    if address.is_unspecified() {
        return AddressClass::Other;
    }
    // The IPv4-compatible `::/96` and the NAT64 `64:ff9b::/96` forms carry an
    // IPv4 target in their low 32 bits, which a translator would reach.
    if segments[..6] == [0, 0, 0, 0, 0, 0] || segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let embedded = (u32::from(segments[6]) << 16) | u32::from(segments[7]);
        return classify_ipv4(Ipv4Addr::from(embedded));
    }
    if segments[0] & 0xffc0 == 0xfe80 {
        AddressClass::LinkLocal
    } else if segments[0] & 0xfe00 == 0xfc00 || segments[0] & 0xffc0 == 0xfec0 {
        // Unique-local fc00::/7 and the deprecated site-local fec0::/10.
        AddressClass::Private
    } else if address.is_multicast()
        // Documentation 2001:db8::/32 and the discard prefix 100::/64.
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x0100 && segments[1..4] == [0, 0, 0])
    {
        AddressClass::Other
    } else {
        AddressClass::Global
    }
}

/// The future of one name lookup.
pub(crate) type LookupFuture<'a> =
    Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>>;

/// Resolves a hostname to its addresses. The product resolves through the
/// platform stack ([`SystemLookup`]); a test substitutes a resolver of its own.
pub(crate) trait Lookup: Send + Sync {
    /// Resolves `host`; an empty answer is the caller's error to report.
    fn lookup<'a>(&'a self, host: &'a str) -> LookupFuture<'a>;
}

/// The platform resolver.
pub(crate) struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup<'a>(&'a self, host: &'a str) -> LookupFuture<'a> {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host, 0)).await?;
            Ok(addresses.map(|address| address.ip()).collect())
        })
    }
}

/// One request target after the gate: the URL to send and the `Host` header
/// that keeps virtual hosting intact when the URL names an address.
#[derive(Debug)]
pub(crate) struct Pinned {
    pub(crate) url: reqwest::Url,
    pub(crate) host_header: Option<String>,
}

/// The connect-time address gate of one `web_fetch` tool.
pub(crate) struct Guard {
    lookup: Arc<dyn Lookup>,
    /// Whether an address may be fetched. The product gate admits only
    /// global addresses; a test swaps in a predicate that also admits a real
    /// loopback listener standing in for a public origin.
    public: fn(IpAddr) -> bool,
}

fn is_global(address: IpAddr) -> bool {
    classify_address(address) == AddressClass::Global
}

impl Guard {
    /// The product gate: platform resolution, every non-global address refused.
    pub(crate) fn system() -> Self {
        Self {
            lookup: Arc::new(SystemLookup),
            public: is_global,
        }
    }

    /// Gates one hop under the per-request deadline, so a stalled resolver
    /// times out like a stalled fetch.
    pub(crate) async fn pin_within(
        &self,
        url: &reqwest::Url,
        timeout_secs: u32,
    ) -> Result<Pinned, WebError> {
        let deadline = Duration::from_secs(u64::from(timeout_secs));
        match tokio::time::timeout(deadline, self.pin(url)).await {
            Ok(pinned) => pinned,
            Err(_elapsed) => Err(WebError::Timeout {
                tool: ToolLabel::Fetch,
                secs: timeout_secs,
                url: url.as_str().to_owned(),
            }),
        }
    }

    /// Checks one hop's target and returns the request target to use.
    ///
    /// A literal IP classifies directly. A name resolves once; any
    /// non-global address in the answer refuses the hop, and an all-global
    /// answer pins an `http` hop to its first address.
    pub(crate) async fn pin(&self, url: &reqwest::Url) -> Result<Pinned, WebError> {
        let Some(host) = url.host_str() else {
            return Err(WebError::BadUrl {
                reason: "URL has no host".into(),
            });
        };
        let bare = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if let Ok(address) = bare.parse::<IpAddr>() {
            return self.literal(url, host, address);
        }
        let addresses = self
            .lookup
            .lookup(host)
            .await
            .map_err(|error| network(&error.to_string()))?;
        let Some(first) = addresses.first().copied() else {
            return Err(network(&format!("{host} did not resolve to an address")));
        };
        if addresses.iter().any(|address| !(self.public)(*address)) {
            return Err(private(host));
        }
        if url.scheme() != "http" {
            return Ok(Pinned {
                url: url.clone(),
                host_header: None,
            });
        }
        let mut pinned = url.clone();
        if pinned.set_ip_host(first).is_err() {
            return Err(network(&format!("cannot pin {host} to {first}")));
        }
        let host_header = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        Ok(Pinned {
            url: pinned,
            host_header: Some(host_header),
        })
    }

    fn literal(&self, url: &reqwest::Url, host: &str, address: IpAddr) -> Result<Pinned, WebError> {
        if !(self.public)(address) {
            return Err(private(host));
        }
        Ok(Pinned {
            url: url.clone(),
            host_header: None,
        })
    }
}

fn private(host: &str) -> WebError {
    WebError::PrivateAddress {
        host: host.to_owned(),
    }
}

fn network(cause: &str) -> WebError {
    WebError::Network {
        tool: ToolLabel::Fetch,
        cause: crate::text::byte_prefix(cause, 200).to_owned(),
    }
}

#[cfg(test)]
mod tests;
