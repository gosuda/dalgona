//! `dalgon rpc`, `dalgon acp`, and `dalgon app-server` over the local host.
//!
//! Each command starts one host in this process, then hands a line-framed
//! transport to the matching `dal-wire` server.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use dal_agent::{Host, Product};
use dal_wire::{
    ConnectionFuture, LocalSocketError, StdioTransport, Transport, WireError, default_rpc_path,
    serve_acp, serve_codex, serve_local, serve_rpc,
};
use tokio_util::sync::CancellationToken;

use super::start_host;
use crate::cli;
use crate::edge;
use crate::exit;
use crate::{Startup, two_lines};

const HOST_GRACE: Duration = Duration::from_secs(3);

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
        data_root,
        helper,
        ..
    } = startup;
    let host = match start_host(product, &config, vars, cwd, helper, &data_root).await {
        Ok(host) => host,
        Err(code) => return code,
    };
    if let Some(notice) = wire.notice()
        && edge::terminal_snapshot().stderr_tty
    {
        let _ = writeln!(std::io::stderr().lock(), "{notice}");
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "R4 edge: the stdio wire owns standard input"
    )]
    let reader = tokio::io::stdin();
    let transport = Transport::Stdio(StdioTransport::new(reader, tokio::io::stdout()));
    let stop = CancellationToken::new();
    let outcome = super::drive(&stop, async {
        let result = tokio::select! {
            biased;
            () = stop.cancelled() => Ok(()),
            result = wire.serve(host.clone(), transport) => result,
        };
        let _ = host.shutdown(HOST_GRACE).await;
        result
    })
    .await;
    match outcome {
        Ok(Ok(())) => exit::code(exit::ExitKind::Success),
        Ok(Err(error)) => wire_failure(wire, &error),
        Err(code) => ExitCode::from(code),
    }
}

async fn local_socket(socket: Option<PathBuf>, startup: Startup, product: Product) -> ExitCode {
    let Startup {
        vars,
        cwd,
        config,
        data_root,
        helper,
        ..
    } = startup;
    let default_path = default_rpc_path(&data_root);
    let (path, default_root) = match socket {
        Some(path) => (cwd.join(path), None),
        None => (default_path, Some(data_root.as_path())),
    };
    let sid = match edge::current_user_sid(&vars) {
        Ok(sid) => sid,
        Err(error) => {
            return two_lines(
                cli::texts::rpc_socket_os(&path, &error.to_string()),
                exit::ExitKind::RequestedFailure,
            );
        }
    };
    let host = match start_host(product, &config, vars, cwd, helper, &data_root).await {
        Ok(host) => host,
        Err(code) => return code,
    };
    let listener_host = host.clone();
    let stop = CancellationToken::new();
    let outcome = super::drive(&stop, async {
        let result = tokio::select! {
            biased;
            () = stop.cancelled() => Ok(()),
            result = serve_local(
                &path,
                default_root,
                sid.as_ref().map(edge::CurrentUserSid::as_str),
                move |transport| connection(listener_host.clone(), transport),
            ) => result,
        };
        let _ = host.shutdown(HOST_GRACE).await;
        result
    })
    .await;
    match outcome {
        Ok(Ok(())) => exit::code(exit::ExitKind::Success),
        Ok(Err(error)) => socket_failure(&path, &data_root, &error),
        Err(code) => ExitCode::from(code),
    }
}

fn connection(host: Host, transport: Transport) -> ConnectionFuture {
    Box::pin(serve_rpc(host, transport))
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
