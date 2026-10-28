//! `dalgon rpc`, `dalgon acp`, and `dalgon app-server` over the local host.
//!
//! Each command starts one host in this process, then hands a line-framed
//! transport to the matching `dal-wire` server.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dal_agent::{Env, Host, Product};
use dal_wire::{
    LocalSocketError, StdioTransport, Transport, WireError, default_rpc_path, serve_acp,
    serve_codex, serve_local, serve_rpc,
};
use tokio_util::sync::CancellationToken;

use super::start_host;
use crate::cli;
use crate::edge;
use crate::exit;
use crate::{Startup, two_lines};

#[derive(Clone, Copy)]
enum Wire {
    Rpc,
    Acp,
    Codex,
}

impl Wire {
    async fn serve(self, host: Host, transport: Transport) -> Result<(), WireError> {
        match self {
            Self::Rpc => serve_rpc(host, transport).await,
            Self::Acp => serve_acp(host, transport).await,
            Self::Codex => serve_codex(host, transport).await,
        }
    }

    const fn notice(self) -> Option<&'static str> {
        match self {
            Self::Rpc => Some(cli::texts::RPC_ONE_JSON_PER_LINE),
            Self::Acp => Some(cli::texts::ACP_ONE_MESSAGE_PER_LINE),
            Self::Codex => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Acp => "acp",
            Self::Codex => "app-server",
        }
    }
}

/// Runs `dalgon rpc [--socket [FILE]]`.
pub(crate) async fn rpc(args: cli::RpcArgs, startup: Startup, product: Product) -> ExitCode {
    let Some(socket) = args.socket else {
        return stdio(Wire::Rpc, startup, product).await;
    };
    let socket = match socket {
        cli::RpcSocket::Auto => None,
        cli::RpcSocket::Path(path) => Some(path),
    };
    local_socket(socket, startup, product).await
}

/// Runs `dalgon acp` over standard input and output.
pub(crate) async fn acp(startup: Startup, product: Product) -> ExitCode {
    stdio(Wire::Acp, startup, product).await
}

/// Runs `dalgon app-server` over standard input and output.
pub(crate) async fn app_server(startup: Startup, product: Product) -> ExitCode {
    stdio(Wire::Codex, startup, product).await
}

async fn stdio(wire: Wire, startup: Startup, product: Product) -> ExitCode {
    let Startup {
        vars,
        cwd,
        config,
        config_path,
        data_root,
        helper,
        ..
    } = startup;
    // Take the protocol streams before the host starts, so extension and tool
    // children never inherit them.
    let transport = match wire_transport() {
        Ok(transport) => Transport::Stdio(transport),
        Err(error) => return wire_streams_failure(wire, &error),
    };
    let host = match start_host(
        product,
        config,
        Env {
            vars,
            cwd,
            sandbox_helper: helper,
        },
        &data_root,
        &config_path,
    )
    .await
    {
        Ok(host) => host,
        Err(code) => return code,
    };
    if let Some(notice) = wire.notice()
        && edge::terminal_snapshot().stderr_tty
    {
        let _ = writeln!(std::io::stderr().lock(), "{notice}");
    }
    let stop = CancellationToken::new();
    let mut signals = edge::ShutdownSignals::register();
    let outcome = super::drive(&stop, &mut signals, async {
        tokio::select! {
            biased;
            () = stop.cancelled() => Ok(()),
            result = wire.serve(host.clone(), transport) => result,
        }
    })
    .await;
    let result = match outcome {
        Ok(result) => result,
        Err(code) => {
            let _closed = exit::Ladder::begin()
                .close_host_after_quiet_wait(host)
                .await;
            return ExitCode::from(code);
        }
    };
    let closed = exit::Ladder::begin().close_host(host, &mut signals).await;
    if let Some(code) = closed.skipped_by {
        return ExitCode::from(code);
    }
    match result {
        Ok(()) => exit::code(exit::ExitKind::Success),
        Err(error) => wire_failure(wire, &error),
    }
}

async fn local_socket(socket: Option<PathBuf>, startup: Startup, product: Product) -> ExitCode {
    let Startup {
        vars,
        cwd,
        config,
        config_path,
        data_root,
        helper,
        ..
    } = startup;
    let default_path = default_rpc_path(&data_root);
    let (path, default_root) = match socket {
        Some(path) => (cwd.join(path), None),
        None => (default_path, Some(data_root.as_path())),
    };
    #[cfg(windows)]
    let sid = match edge::current_user_sid(&vars) {
        Ok(sid) => sid,
        Err(error) => {
            return two_lines(
                cli::texts::rpc_socket_os(&path, &error.to_string()),
                exit::ExitKind::RequestedFailure,
            );
        }
    };
    #[cfg(not(windows))]
    let sid = edge::current_user_sid(&vars);
    let host = match start_host(
        product,
        config,
        Env {
            vars,
            cwd,
            sandbox_helper: helper,
        },
        &data_root,
        &config_path,
    )
    .await
    {
        Ok(host) => host,
        Err(code) => return code,
    };
    let listener_host = host.clone();
    let stop = CancellationToken::new();
    let mut signals = edge::ShutdownSignals::register();
    let outcome = super::drive(&stop, &mut signals, async {
        tokio::select! {
            biased;
            () = stop.cancelled() => Ok(()),
            result = serve_local(
                &path,
                default_root,
                sid.as_ref().map(edge::CurrentUserSid::as_str),
                move |transport| Box::pin(serve_rpc(listener_host.clone(), transport)),
            ) => result,
        }
    })
    .await;
    let result = match outcome {
        Ok(result) => result,
        Err(code) => {
            let _closed = exit::Ladder::begin()
                .close_host_after_quiet_wait(host)
                .await;
            return ExitCode::from(code);
        }
    };
    let closed = exit::Ladder::begin().close_host(host, &mut signals).await;
    if let Some(code) = closed.skipped_by {
        return ExitCode::from(code);
    }
    match result {
        Ok(()) => exit::code(exit::ExitKind::Success),
        Err(error) => socket_failure(&path, &data_root, &error),
    }
}

fn socket_failure(path: &Path, data_root: &Path, error: &LocalSocketError) -> ExitCode {
    let lines = match error {
        LocalSocketError::DirNotPrivate { path: dir, mode } => {
            cli::texts::rpc_socket_dir_open(path, dir, *mode, data_root)
        }
        LocalSocketError::PathTooLong { bytes, .. } => {
            cli::texts::rpc_socket_path_long(path, *bytes)
        }
        LocalSocketError::InUse { .. } => cli::texts::rpc_socket_in_use(path),
        LocalSocketError::Os { message, .. } => cli::texts::rpc_socket_os(path, message),
        other => cli::texts::rpc_socket_os(path, &other.to_string()),
    };
    two_lines(lines, exit::ExitKind::RequestedFailure)
}

fn wire_failure(wire: Wire, error: &WireError) -> ExitCode {
    two_lines(
        cli::texts::internal_error(wire.name(), &error.to_string()),
        exit::ExitKind::Internal,
    )
}

/// Builds the stdio transport on private copies of the standard streams, then
/// points the shared streams away from the protocol: input at the null device
/// and output at standard error.
#[cfg(unix)]
fn wire_transport() -> std::io::Result<StdioTransport> {
    let streams = edge::isolate_wire_streams()?;
    Ok(StdioTransport::from_files(streams.reader, streams.writer))
}

/// Captures the process standard streams for the stdio wire.
#[cfg(not(unix))]
fn process_stdio() -> (tokio::io::Stdin, tokio::io::Stdout) {
    #[expect(
        clippy::disallowed_methods,
        reason = "process edge: the stdio wire owns standard input"
    )]
    let reader = tokio::io::stdin();
    (reader, tokio::io::stdout())
}

/// Builds the stdio transport on the process standard streams.
#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the unix variant can fail isolating wire streams; the signature is shared"
)]
fn wire_transport() -> std::io::Result<StdioTransport> {
    let (reader, writer) = process_stdio();
    Ok(StdioTransport::new(reader, writer))
}

/// Owned input and output files for a native helper.
#[cfg(all(feature = "test-support", any(unix, windows)))]
pub struct RawWireStreams {
    /// Standard-input file.
    pub reader: std::fs::File,
    /// Standard-output file.
    pub writer: std::fs::File,
}

/// Captures protocol streams for a native helper. Call once at process startup.
///
/// Unix isolates the wire streams onto private copies and points the shared
/// streams away from the protocol.
///
/// # Errors
///
/// Returns the operating-system error when the streams cannot be isolated.
#[cfg(all(feature = "test-support", unix))]
pub fn raw_wire_streams() -> std::io::Result<RawWireStreams> {
    let streams = edge::isolate_wire_streams()?;
    Ok(RawWireStreams {
        reader: streams.reader,
        writer: streams.writer,
    })
}

/// Captures protocol streams for a native helper. Call once at process startup.
///
/// Windows duplicates owned handles for the standard input and output streams.
///
/// # Errors
///
/// Returns the operating-system error when the handles cannot be duplicated.
#[cfg(all(feature = "test-support", windows))]
pub fn raw_wire_streams() -> std::io::Result<RawWireStreams> {
    use std::os::windows::io::AsHandle;

    let (reader, writer) = process_stdio();
    let reader = std::fs::File::from(reader.as_handle().try_clone_to_owned()?);
    let writer = std::fs::File::from(writer.as_handle().try_clone_to_owned()?);
    Ok(RawWireStreams { reader, writer })
}

fn wire_streams_failure(wire: Wire, error: &std::io::Error) -> ExitCode {
    two_lines(
        cli::texts::wire_streams(wire.name(), &error.to_string()),
        exit::ExitKind::RequestedFailure,
    )
}
