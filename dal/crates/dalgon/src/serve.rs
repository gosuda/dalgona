//! Local HTTP, WebSocket, and Codex serving commands.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    process::ExitCode,
};

use dal_agent::Host;
use dal_core::ApprovalMode;
use dal_store::{FileMode, write_atomic};
use dal_wire::{router::RouterOptions, token};
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;
use tokio_util::sync::CancellationToken;

/// Options supplied directly to `dalgon serve`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServeArgs {
    /// Whether the listener is exposed beyond loopback and requires a token.
    pub public: bool,
    /// Whether to mount the A2A protocol surface.
    pub a2a: bool,
    /// An optional command-line bind address.
    pub bind: Option<String>,
    /// An optional command-line TCP port.
    pub port: Option<u16>,
    /// An optional command-line token-file path.
    pub token_file: Option<PathBuf>,
}

/// Effective serve settings after configuration and CLI precedence are applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeConfig {
    /// The configured bind address.
    pub bind: String,
    /// Whether the configured bind address came from an explicit user key.
    pub bind_was_explicit: bool,
    /// The configured TCP port.
    pub port: u16,
    /// The effective token-file path.
    pub token_file: PathBuf,
    /// The approval policy for actions received through the server.
    pub approval: ApprovalMode,
    /// Allowed browser origins.
    pub origins: Vec<String>,
    /// Model aliases resolved for this process.
    pub aliases: BTreeMap<Box<str>, Box<str>>,
    /// The captured absolute workspace root.
    pub workspace: PathBuf,
}

/// A serve command failed while preparing or running its listener.
#[derive(Debug, ThisError)]
pub enum ServeCommandError {
    /// A local file operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The listener refused its bind, address, alias, or token inputs.
    #[error(transparent)]
    Serve(#[from] dal_wire::ServeError),
    /// A connection failed while the listener drained.
    #[error(transparent)]
    Wire(#[from] dal_wire::WireError),
    /// A serve token could not be created outside the public-token path.
    #[error(transparent)]
    Token(#[from] dal_wire::token::TokenError),
}

/// Creates or atomically replaces the serve bearer token.
///
/// # Errors
/// Returns an I/O or wire-token error when the token cannot be written or
/// either output stream fails.
pub fn create_token(
    token_file: &Path,
    force: bool,
    stderr_is_tty: bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<ExitCode, ServeCommandError> {
    if !force && token_file.try_exists()? {
        return write_failure(
            stderr,
            &crate::cli::texts::serve_token_already_exists(token_file),
        );
    }

    if token_file.is_absolute()
        && let Some(parent) = token_file.parent()
    {
        dal_store::create_private_dir_all(parent).map_err(|source| {
            ServeCommandError::Token(dal_wire::token::TokenError::Io {
                path: parent.to_path_buf(),
                source,
            })
        })?;
    }

    let token = match token::create(token_file, force) {
        Ok(token) => token,
        Err(_error) if !force && token_file.try_exists()? => {
            return write_failure(
                stderr,
                &crate::cli::texts::serve_token_already_exists(token_file),
            );
        }
        Err(error) => return Err(ServeCommandError::Token(error)),
    };

    writeln!(stdout, "{token}")?;
    if stderr_is_tty {
        writeln!(
            stderr,
            "{}",
            crate::cli::texts::serve_stored_token(token_file)
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolved inputs for one serve run, ahead of its stream handles.
pub struct ServeRun {
    /// The started host serving the three protocol surfaces.
    pub host: Host,
    /// Parsed serve flags.
    pub args: ServeArgs,
    /// Layered serve configuration.
    pub config: ServeConfig,
    /// Product data root for the process-lock advertisement.
    pub data_root: PathBuf,
    /// Cooperative shutdown for the whole listener.
    pub stop: CancellationToken,
}

/// Runs the single-listener dal server and its three protocol surfaces.
///
/// # Errors
/// Returns a typed wire, store, or stream-I/O error. User-correctable startup
/// refusals are written to `stderr` and return exit code 1.
pub async fn run(
    serve: ServeRun,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    _stderr_is_tty: bool,
    stdout_is_tty: bool,
) -> Result<ExitCode, ServeCommandError> {
    let ServeRun {
        host,
        args,
        config,
        data_root,
        stop,
    } = serve;
    let bind = resolve_bind(&args, &config);
    let port = args.port.unwrap_or(config.port);
    let token_file = args.token_file.unwrap_or(config.token_file);

    if let Some(alias) = config.aliases.keys().find(|alias| is_mode_alias(alias)) {
        return write_failure(stderr, &crate::cli::texts::serve_alias_shadows_mode(alias));
    }
    if !args.public {
        match all_resolved_addresses_are_loopback(&bind, port) {
            Ok(true) => {}
            Ok(false) => {
                return write_failure(stderr, &crate::cli::texts::serve_non_loopback_bind(&bind));
            }
            Err(error) => {
                let address = format!("{bind}:{port}");
                return write_failure(
                    stderr,
                    &crate::cli::texts::serve_bind_failed(&address, &error.to_string()),
                );
            }
        }
    }
    if args.public
        && let Err(error) = validate_public_token(&token_file)
    {
        let diagnostic = match error {
            TokenFileError::Missing => crate::cli::texts::serve_public_token_missing(&token_file),
            TokenFileError::UnsafeMode(mode) => {
                crate::cli::texts::serve_public_token_unsafe(&token_file, &format!("{mode:o}"))
            }
            TokenFileError::Empty => crate::cli::texts::serve_public_token_empty(&token_file),
            TokenFileError::Invalid => crate::cli::texts::serve_public_token_invalid(&token_file),
            TokenFileError::Other(source) => return Err(ServeCommandError::Io(source)),
        };
        return write_failure(stderr, &diagnostic);
    }

    let options = RouterOptions {
        bind: bind.clone(),
        port,
        public: args.public,
        a2a: args.a2a,
        token_file: token_file.clone(),
        approval: config.approval,
        origins: config.origins,
        aliases: config.aliases,
        workspace: config.workspace,
    };
    let handle = match dal_wire::serve_router(host, options, stop.clone()).await {
        Ok(handle) => handle,
        Err(error) => match crate::cli::texts::serve_router_refusal(&error) {
            Some(lines) => return write_failure(stderr, &lines),
            None => return Err(ServeCommandError::Serve(error)),
        },
    };
    let local_addr = handle.local_addr();
    let lease = match publish_advertisement(&data_root, local_addr) {
        Ok(lease) => lease,
        Err(error) => {
            stop.cancel();
            let _ = handle.wait().await;
            let path = advertisement_path(&data_root, std::process::id());
            return write_failure(
                stderr,
                &crate::cli::texts::serve_advertisement_failed(&path, &error.to_string()),
            );
        }
    };

    if args.public {
        writeln!(stderr, "{}", crate::cli::texts::SERVE_PUBLIC_WARNING)?;
    }
    let url = format!("http://{local_addr}");
    let details = listen_details(args.public, &token_file, local_addr);
    let listen_line = crate::cli::texts::serve_listen_line(&url, &details, args.a2a);
    if let Err(error) = writeln!(stdout, "{listen_line}") {
        stop.cancel();
        let _ = handle.wait().await;
        drop(lease);
        return Err(ServeCommandError::Io(error));
    }

    handle.wait().await?;
    drop(lease);
    if stdout_is_tty {
        writeln!(stdout, "{}", crate::cli::texts::SERVE_STOPPED)?;
    }

    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServeAdvertisement {
    pub version: u8,
    pub pid: u32,
    pub bind: String,
    pub port: u16,
    pub http: String,
    pub websocket: String,
    pub codex: String,
}

/// Returns a live serve advertisement only while its process lock is held.
#[must_use]
pub(crate) fn active_advertisement(data_root: &Path, pid: u32) -> Option<ServeAdvertisement> {
    let lock_path = advertisement_lock_path(data_root, pid);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .ok()?;
    match lock.try_lock() {
        Ok(()) => {
            let _ = lock.unlock();
            return None;
        }
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(std::fs::TryLockError::Error(_)) => return None,
    }

    let bytes = fs::read(advertisement_path(data_root, pid)).ok()?;
    let advertisement: ServeAdvertisement = sonic_rs::from_slice(&bytes).ok()?;
    (advertisement.version == 1 && advertisement.pid == pid).then_some(advertisement)
}

struct ServingLease {
    _lock: File,
    record_path: PathBuf,
    lock_path: PathBuf,
}

impl Drop for ServingLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.record_path);
        let _ = fs::remove_file(&self.lock_path);
    }
}

#[derive(Debug)]
enum TokenFileError {
    Missing,
    UnsafeMode(u32),
    Empty,
    Invalid,
    Other(io::Error),
}

fn validate_public_token(path: &Path) -> Result<(), TokenFileError> {
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            TokenFileError::Missing
        } else {
            TokenFileError::Other(error)
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(TokenFileError::UnsafeMode(mode));
        }
    }
    if metadata.len() == 0 {
        return Err(TokenFileError::Empty);
    }
    token::load(path).map_err(|error| match error {
        dal_wire::token::TokenError::Missing { .. } => TokenFileError::Missing,
        dal_wire::token::TokenError::OpenToOtherUsers { mode, .. } => {
            TokenFileError::UnsafeMode(mode)
        }
        dal_wire::token::TokenError::Empty { .. } => TokenFileError::Empty,
        dal_wire::token::TokenError::Invalid { .. } => TokenFileError::Invalid,
        error => TokenFileError::Other(io::Error::other(error)),
    })?;
    Ok(())
}

fn publish_advertisement(
    data_root: &Path,
    local_addr: SocketAddr,
) -> Result<ServingLease, io::Error> {
    let pid = std::process::id();
    let directory = data_root.join("run/serve");
    fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }

    let lock_path = advertisement_lock_path(data_root, pid);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(&lock_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;
    }
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the serving PID lock is already held",
            ));
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error),
    }

    let record_path = advertisement_path(data_root, pid);
    let lease = ServingLease {
        _lock: lock,
        record_path: record_path.clone(),
        lock_path,
    };
    let host = connect_host(local_addr.ip());
    let port = local_addr.port();
    let advertisement = ServeAdvertisement {
        version: 1,
        pid,
        bind: local_addr.ip().to_string(),
        port,
        http: format!("http://{host}:{port}"),
        websocket: format!("ws://{host}:{port}/v1/ws"),
        codex: format!("ws://{host}:{port}/codex/ws"),
    };
    let bytes = sonic_rs::to_vec(&advertisement).map_err(io::Error::other)?;
    write_atomic(&record_path, &bytes, FileMode::Mode0600).map_err(io::Error::other)?;
    Ok(lease)
}

fn advertisement_path(data_root: &Path, pid: u32) -> PathBuf {
    data_root.join(format!("run/serve/{pid}.json"))
}

fn advertisement_lock_path(data_root: &Path, pid: u32) -> PathBuf {
    data_root.join(format!("run/serve/{pid}.lock"))
}

fn connect_host(address: IpAddr) -> String {
    if address.is_unspecified() {
        return match address {
            IpAddr::V4(_) => "127.0.0.1".to_owned(),
            IpAddr::V6(_) => "[::1]".to_owned(),
        };
    }
    match address {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    }
}

fn is_mode_alias(alias: &str) -> bool {
    matches!(
        alias,
        "dalgon/normal" | "dalgon/eval-first" | "dalgon/eval-only"
    )
}

fn resolve_bind(args: &ServeArgs, config: &ServeConfig) -> String {
    if let Some(bind) = &args.bind {
        return bind.clone();
    }
    if config.bind_was_explicit {
        return config.bind.clone();
    }
    if args.public {
        return "0.0.0.0".to_owned();
    }
    "127.0.0.1".to_owned()
}

fn all_resolved_addresses_are_loopback(bind: &str, port: u16) -> io::Result<bool> {
    let addresses = (bind, port).to_socket_addrs()?;
    let mut found = false;
    for address in addresses {
        found = true;
        if !address.ip().is_loopback() {
            return Ok(false);
        }
    }
    if !found {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "bind address resolved to no endpoints",
        ));
    }
    Ok(true)
}

fn listen_details(public: bool, token_file: &Path, local_addr: SocketAddr) -> String {
    if !public {
        return "loopback, no token".to_owned();
    }
    if local_addr.ip().is_unspecified() {
        return format!("all interfaces, token from {}", token_file.display());
    }
    if local_addr.ip().is_loopback() {
        return format!("loopback, token from {}", token_file.display());
    }
    format!("token required, token from {}", token_file.display())
}

fn write_failure(
    stderr: &mut impl Write,
    lines: &[String; 2],
) -> Result<ExitCode, ServeCommandError> {
    writeln!(stderr, "{}", lines[0])?;
    writeln!(stderr, "{}", lines[1])?;
    Ok(ExitCode::FAILURE)
}
#[cfg(test)]
mod tests {
    use super::*;
    use dal_core::ApprovalMode;

    fn config(bind: &str, bind_was_explicit: bool) -> ServeConfig {
        ServeConfig {
            bind: bind.to_owned(),
            bind_was_explicit,
            port: 7437,
            token_file: PathBuf::from("serve.token"),
            approval: ApprovalMode::Ask,
            origins: Vec::new(),
            aliases: BTreeMap::new(),
            workspace: PathBuf::from("/workspace"),
        }
    }

    #[test]
    fn public_bind_precedence_and_loopback_refusal() {
        let defaults = config("127.0.0.1", false);
        assert_eq!(resolve_bind(&ServeArgs::default(), &defaults), "127.0.0.1");

        let public_default = ServeArgs {
            public: true,
            ..ServeArgs::default()
        };
        assert_eq!(resolve_bind(&public_default, &defaults), "0.0.0.0");

        let cli_bind = ServeArgs {
            public: true,
            bind: Some("192.0.2.10".to_owned()),
            ..ServeArgs::default()
        };
        assert_eq!(resolve_bind(&cli_bind, &defaults), "192.0.2.10");

        let configured = config("localhost", true);
        assert_eq!(
            resolve_bind(&ServeArgs::default(), &configured),
            "localhost"
        );
        assert!(all_resolved_addresses_are_loopback("localhost", 7437).unwrap());
        assert!(!all_resolved_addresses_are_loopback("192.0.2.10", 7437).unwrap());

        let public_loopback = ServeArgs {
            public: true,
            bind: Some("127.0.0.1".to_owned()),
            ..ServeArgs::default()
        };
        assert_eq!(resolve_bind(&public_loopback, &defaults), "127.0.0.1");
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            validate_public_token(&dir.path().join("missing-token")),
            Err(TokenFileError::Missing)
        ));
    }

    #[test]
    fn public_token_rejects_invalid_content_with_user_text() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("serve.token");
        fs::write(&token_file, b"not-a-token\n").unwrap();
        assert!(matches!(
            validate_public_token(&token_file),
            Err(TokenFileError::Invalid)
        ));
        let lines = crate::cli::texts::serve_public_token_invalid(&token_file);
        assert_eq!(
            lines,
            [
                format!("dalgon: serve.token is invalid: {}", token_file.display()),
                "Run dalgon serve token --force to write a new token.".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn token_command_keeps_stdout_and_tty_note_exact() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("serve.token");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let created = create_token(&token_file, false, false, &mut stdout, &mut stderr).unwrap();
        assert_eq!(created, ExitCode::SUCCESS);
        assert!(stderr.is_empty());
        let token = String::from_utf8(stdout.clone()).unwrap();
        let token = token.strip_suffix('\n').unwrap();
        let payload = token.strip_prefix("dal_").unwrap();
        assert_eq!(payload.len(), 64);
        assert!(
            payload
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(fs::read(&token_file).unwrap(), stdout);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&token_file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        stdout.clear();
        let refused = create_token(&token_file, false, false, &mut stdout, &mut stderr).unwrap();
        assert_eq!(refused, ExitCode::FAILURE);
        assert!(stdout.is_empty());
        assert_eq!(
            stderr,
            format!(
                "dalgon: serve.token already exists: {}\nPass --force to replace it. Every client must then use the new token.\n",
                token_file.display()
            )
            .as_bytes()
        );

        stderr.clear();
        let replaced = create_token(&token_file, true, true, &mut stdout, &mut stderr).unwrap();
        assert_eq!(replaced, ExitCode::SUCCESS);
        let new_token = String::from_utf8(stdout.clone()).unwrap();
        assert_ne!(new_token, format!("{token}\n"));
        assert_eq!(fs::read(&token_file).unwrap(), stdout);
        assert_eq!(
            stderr,
            format!("dalgon: stored at {}\n", token_file.display()).as_bytes()
        );
        assert!(
            !stderr
                .windows(64)
                .any(|window| window == payload.as_bytes())
        );
    }

    #[test]
    fn advertisement_is_live_locked_and_secret_free() {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let local_addr = listener.local_addr().unwrap();
        let secret = b"dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";
        fs::write(dir.path().join("serve.token"), secret).unwrap();

        let lease = publish_advertisement(dir.path(), local_addr).unwrap();
        let pid = std::process::id();
        let advertisement = active_advertisement(dir.path(), pid).unwrap();
        assert_eq!(advertisement.version, 1);
        assert_eq!(advertisement.pid, pid);
        assert_eq!(advertisement.bind, "127.0.0.1");
        assert_eq!(advertisement.port, local_addr.port());
        assert_eq!(
            advertisement.http,
            format!("http://127.0.0.1:{}", local_addr.port())
        );
        assert_eq!(
            advertisement.websocket,
            format!("ws://127.0.0.1:{}/v1/ws", local_addr.port())
        );
        assert_eq!(
            advertisement.codex,
            format!("ws://127.0.0.1:{}/codex/ws", local_addr.port())
        );
        let bytes = fs::read(advertisement_path(dir.path(), pid)).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let version = text.find("\"version\"").unwrap();
        let process = text.find("\"pid\"").unwrap();
        let bind = text.find("\"bind\"").unwrap();
        let port = text.find("\"port\"").unwrap();
        let http = text.find("\"http\"").unwrap();
        let websocket = text.find("\"websocket\"").unwrap();
        let codex = text.find("\"codex\"").unwrap();
        assert!(version < process && process < bind && bind < port && port < http);
        assert!(http < websocket && websocket < codex);
        assert!(
            !bytes
                .windows(secret.len() - 1)
                .any(|window| window == &secret[..secret.len() - 1])
        );
        assert!(!text.contains("Authorization"));

        drop(lease);
        assert!(active_advertisement(dir.path(), pid).is_none());
    }
}
