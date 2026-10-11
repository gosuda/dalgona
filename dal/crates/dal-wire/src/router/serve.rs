//! Router-driven listener for the HTTP router.
//!
//! `serve_router` serves the routes in [`RouterOptions`] on a loopback or
//! public listener: non-loopback binds need `public`, public binds require
//! the serve token, and A2A mounts only when enabled.

use std::net::{IpAddr, SocketAddr};

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::RouterOptions;
use crate::error::ServeError;

/// Serves the configured routes with the listener guards.
///
/// Public binds load and require the serve token from
/// `options.token_file`; non-loopback binds without `public` fail.
///
/// # Errors
///
/// Returns [`ServeError`] when the address cannot be bound, the bind is not
/// loopback without `public`, or the public token cannot be loaded.
pub async fn serve_router(
    host: dal_agent::Host,
    options: RouterOptions,
    stop: CancellationToken,
) -> Result<crate::serve::ServeHandle, ServeError> {
    let bind = resolve_bind(&options.bind, options.port).await?;
    if !options.public && !is_loopback(bind) {
        return Err(ServeError::NotLoopback { addr: bind });
    }
    if let Some(alias) = options
        .aliases
        .keys()
        .find(|alias| super::HarnessMode::parse(alias).is_some())
    {
        return Err(ServeError::AliasShadowsMode {
            alias: alias.clone(),
        });
    }
    let token = if options.public {
        Some(crate::token::load(&options.token_file).map_err(ServeError::Token)?)
    } else {
        None
    };
    let listener = TcpListener::bind((bind, options.port))
        .await
        .map_err(|source| ServeError::Bind {
            addr: bind,
            port: options.port,
            source,
        })?;
    let local_addr: SocketAddr = listener.local_addr().map_err(|source| ServeError::Bind {
        addr: bind,
        port: options.port,
        source,
    })?;
    let cfg = crate::serve::ListenerCfg {
        bind,
        port: local_addr.port(),
        public: options.public,
        token,
        a2a: options.a2a,
        origins: options.origins.clone(),
        approval: options.approval,
        aliases: options.aliases.clone(),
        workspace: options.workspace.clone(),
    };
    Ok(crate::serve::listen(host, cfg, listener, local_addr, stop))
}

/// Resolves the bind text to the first address it names.
async fn resolve_bind(bind: &str, port: u16) -> Result<IpAddr, ServeError> {
    if let Ok(addr) = bind.parse::<IpAddr>() {
        return Ok(addr);
    }
    let mut addrs = tokio::net::lookup_host((bind, port))
        .await
        .map_err(|source| ServeError::Resolve {
            bind: bind.into(),
            source,
        })?;
    addrs
        .next()
        .map(|addr| addr.ip())
        .ok_or_else(|| ServeError::Resolve {
            bind: bind.into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no address found"),
        })
}

/// Returns true for loopback bind addresses (`127.0.0.0/8` or `::1`).
fn is_loopback(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(address) => address.octets()[0] == 127,
        IpAddr::V6(address) => address.is_loopback(),
    }
}
