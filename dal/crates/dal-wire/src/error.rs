use std::{io, net::IpAddr};

use thiserror::Error;

/// An error at the dal transport boundary.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum WireError {
    /// A frame could not be encoded or violated the transport framing contract.
    #[error("invalid wire frame")]
    Frame,
    /// A peer sent a protocol error or the request could not be handled.
    #[error("protocol error {code}: {message}")]
    Protocol {
        /// The stable protocol error code.
        code: i32,
        /// The message safe to return to the client.
        message: String,
    },
    /// The transport closed or failed while reading or writing.
    #[error("transport error: {0}")]
    Transport(String),
    /// A caller did not present a valid authentication token.
    #[error("authentication failed")]
    Auth,
    /// The remote protocol version has no method for this host operation.
    #[error("{operation} is not supported by the version-1 remote protocol")]
    Unsupported {
        /// The host operation name.
        operation: &'static str,
    },
}

/// A failure while starting or running the HTTP listener.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ServeError {
    /// The requested address and port could not be bound.
    #[error("cannot bind {addr}:{port}: {source}")]
    Bind {
        /// The requested bind address.
        addr: IpAddr,
        /// The requested port.
        port: u16,
        /// The operating-system bind failure.
        #[source]
        source: io::Error,
    },
    /// The bind text named no resolvable address.
    #[error("cannot resolve bind address {bind}: {source}")]
    Resolve {
        /// The configured bind text.
        bind: Box<str>,
        /// The resolution failure.
        #[source]
        source: io::Error,
    },
    /// A non-loopback bind was requested without public mode.
    #[error("serve --bind {addr} is not a loopback address: it needs a token")]
    NotLoopback {
        /// The requested bind address.
        addr: IpAddr,
    },
    /// A configured alias shadows one of the harness mode ids.
    #[error("alias {alias} shadows the harness mode of the same name")]
    AliasShadowsMode {
        /// The shadowing alias name.
        alias: Box<str>,
    },
    /// A protocol or transport failure ended serving.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// A serve token could not be created or loaded.
    #[error(transparent)]
    Token(#[from] crate::token::TokenError),
}
