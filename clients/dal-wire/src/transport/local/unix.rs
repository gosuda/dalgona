use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use futures::{StreamExt, stream::FuturesUnordered};
use tokio::net::UnixListener;

use crate::error::WireError;

use super::{
    ConnectionFuture, LocalSocketError, LocalTransport, Transport, default_rpc_path, path::os_error,
};

/// Most connections served at once; the next accept waits for a free slot.
const MAX_CONNECTIONS: usize = 64;

/// How long accepting pauses after the process runs out of descriptors.
const DESCRIPTOR_PAUSE: Duration = Duration::from_millis(100);

/// The longest socket path in bytes: the smallest `sun_path` among the
/// supported platforms, less its terminating NUL.
const MAX_PATH_BYTES: usize = 103;

/// One source of accepted connections.
///
/// The bound socket implements it. Tests implement it to inject accept
/// failures that a real socket only produces at the process descriptor limit.
trait Accept {
    /// Waits for the next connection or accept failure.
    fn next_connection(&self) -> impl Future<Output = io::Result<Transport>> + Send;
}

impl Accept for UnixListener {
    async fn next_connection(&self) -> io::Result<Transport> {
        let (stream, _) = self.accept().await?;
        let (reader, writer) = stream.into_split();
        Ok(Transport::Local(LocalTransport::new(reader, writer)))
    }
}

pub(super) async fn serve_unix<F>(
    path: &Path,
    default_data_root: Option<&Path>,
    on_connection: F,
) -> Result<(), LocalSocketError>
where
    F: Fn(Transport) -> ConnectionFuture + Send + Sync + 'static,
{
    use std::os::unix::ffi::OsStrExt;

    let bytes = path.as_os_str().as_bytes().len();
    if bytes > MAX_PATH_BYTES {
        return Err(LocalSocketError::PathTooLong {
            path: path.to_owned(),
            bytes,
        });
    }
    prepare_unix_parent(path, default_data_root)?;
    remove_stale_socket(path).await?;
    let listener = UnixListener::bind(path).map_err(|error| bind_error(path, &error))?;
    let _path_guard = SocketPathGuard::capture(path.to_owned())?;
    accept_loop(&listener, path, on_connection).await
}

/// Maps a bind failure. A path taken between the stale check and the bind
/// belongs to another listener that won the race, which is `InUse`.
fn bind_error(path: &Path, error: &io::Error) -> LocalSocketError {
    if error.kind() == io::ErrorKind::AddrInUse {
        return LocalSocketError::InUse {
            path: path.to_owned(),
        };
    }
    os_error(path, error)
}

/// Serves the connections of `listener` until accepting fails.
///
/// Every connection runs inside this future, so nothing else polls them. At
/// most [`MAX_CONNECTIONS`] run at once, and no accept starts while they do.
/// Running out of descriptors is not fatal: it is logged and accepting
/// pauses, with the running connections still polled, so they can finish and
/// free descriptors.
async fn accept_loop<A, F>(
    listener: &A,
    path: &Path,
    on_connection: F,
) -> Result<(), LocalSocketError>
where
    A: Accept,
    F: Fn(Transport) -> ConnectionFuture,
{
    let mut active = FuturesUnordered::<ConnectionFuture>::new();
    loop {
        if active.len() >= MAX_CONNECTIONS {
            log_connection_result(active.next().await);
            continue;
        }
        tokio::select! {
            result = active.next(), if !active.is_empty() => log_connection_result(result),
            accepted = listener.next_connection() => {
                admit(accepted, path, &on_connection, &mut active).await?;
            }
        }
    }
}

/// Applies one accept outcome: starts the connection, pauses after descriptor
/// exhaustion, skips an interrupted accept, and fails on anything else.
async fn admit<F>(
    accepted: io::Result<Transport>,
    path: &Path,
    on_connection: &F,
    active: &mut FuturesUnordered<ConnectionFuture>,
) -> Result<(), LocalSocketError>
where
    F: Fn(Transport) -> ConnectionFuture,
{
    match accepted {
        Ok(transport) => active.push(on_connection(transport)),
        Err(error) if is_descriptor_exhaustion(&error) => {
            tracing::warn!(%error, "local RPC accept hit the descriptor limit; retrying");
            pause_accepting(active).await;
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
        Err(error) => return Err(os_error(path, &error)),
    }
    Ok(())
}

/// Waits out [`DESCRIPTOR_PAUSE`] while polling the running connections.
async fn pause_accepting(active: &mut FuturesUnordered<ConnectionFuture>) {
    let pause = tokio::time::sleep(DESCRIPTOR_PAUSE);
    tokio::pin!(pause);
    loop {
        tokio::select! {
            () = &mut pause => return,
            result = active.next(), if !active.is_empty() => log_connection_result(result),
        }
    }
}

fn prepare_unix_parent(
    path: &Path,
    default_data_root: Option<&Path>,
) -> Result<(), LocalSocketError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Some(parent) = path.parent() else {
        return Err(os_error(
            path,
            &io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"),
        ));
    };
    if let Some(data_root) = default_data_root {
        if path != default_rpc_path(data_root) {
            return Err(os_error(
                path,
                &io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path is not the default RPC socket",
                ),
            ));
        }
        std::fs::create_dir_all(parent).map_err(|error| os_error(parent, &error))?;
        let parent_metadata =
            std::fs::symlink_metadata(parent).map_err(|error| os_error(parent, &error))?;
        let data_metadata =
            std::fs::metadata(data_root).map_err(|error| os_error(data_root, &error))?;
        if !parent_metadata.file_type().is_dir() || parent_metadata.uid() != data_metadata.uid() {
            return Err(LocalSocketError::DirNotPrivate {
                path: parent.to_owned(),
                mode: parent_metadata.mode() & 0o777,
            });
        }
        if parent_metadata.mode() & 0o077 != 0 {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| os_error(parent, &error))?;
        }
        return Ok(());
    }

    if std::fs::symlink_metadata(parent).is_err() {
        std::fs::create_dir_all(parent).map_err(|error| os_error(parent, &error))?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| os_error(parent, &error))?;
    }
    let metadata = std::fs::symlink_metadata(parent).map_err(|error| os_error(parent, &error))?;
    if !metadata.file_type().is_dir() {
        return Err(os_error(
            parent,
            &io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket parent is not a directory",
            ),
        ));
    }
    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(LocalSocketError::DirNotPrivate {
            path: parent.to_owned(),
            mode,
        });
    }
    Ok(())
}

async fn remove_stale_socket(path: &Path) -> Result<(), LocalSocketError> {
    use std::os::unix::fs::FileTypeExt;
    use tokio::{net::UnixStream, time::timeout};

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(os_error(path, &error)),
    };
    if !metadata.file_type().is_socket() {
        return Err(os_error(
            path,
            &io::Error::new(io::ErrorKind::AlreadyExists, "endpoint is not a socket"),
        ));
    }
    match timeout(Duration::from_secs(1), UnixStream::connect(path)).await {
        Ok(Ok(_stream)) => Err(LocalSocketError::InUse {
            path: path.to_owned(),
        }),
        Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => {
            remove_stale_path(path)
        }
        Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(Err(error)) => Err(os_error(path, &error)),
        Err(_) => Err(LocalSocketError::InUse {
            path: path.to_owned(),
        }),
    }
}

fn remove_stale_path(path: &Path) -> Result<(), LocalSocketError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(os_error(path, &error)),
    }
}

fn is_descriptor_exhaustion(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(23 | 24))
}

struct SocketPathGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketPathGuard {
    fn capture(path: PathBuf) -> Result<Self, LocalSocketError> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| os_error(&path, &error))?;
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        let current = std::fs::symlink_metadata(&self.path)
            .ok()
            .filter(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode);
        if current.is_none() {
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::debug!(path = %self.path.display(), %error, "failed to remove local RPC socket");
        }
    }
}

fn log_connection_result(result: Option<Result<(), WireError>>) {
    if let Some(Err(error)) = result {
        tracing::debug!(%error, "local RPC connection ended with an error");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use tokio::sync::Semaphore;

    use super::{
        Accept, ConnectionFuture, LocalSocketError, LocalTransport, Transport, accept_loop,
        bind_error,
    };

    const SOCKET: &str = "/run/user/1000/dal/rpc.sock";
    const ENFILE: i32 = 23;
    const EMFILE: i32 = 24;

    /// Accept outcomes in order; once they run out, accepting waits forever.
    struct Scripted(Mutex<VecDeque<io::Result<()>>>);

    impl Scripted {
        fn new(steps: impl IntoIterator<Item = io::Result<()>>) -> Self {
            Self(Mutex::new(steps.into_iter().collect()))
        }
    }

    impl Accept for Scripted {
        async fn next_connection(&self) -> io::Result<Transport> {
            let step = self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            match step {
                Some(Ok(())) => {
                    let (near, _far) = tokio::io::duplex(64);
                    let (reader, writer) = tokio::io::split(near);
                    Ok(Transport::Local(LocalTransport::new(reader, writer)))
                }
                Some(Err(error)) => Err(error),
                None => std::future::pending().await,
            }
        }
    }

    /// Counts connections as they start and finish. A connection finishes
    /// once it takes a permit from the gate.
    struct Probe {
        started: AtomicUsize,
        finished: AtomicUsize,
        gate: Semaphore,
    }

    impl Probe {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                started: AtomicUsize::new(0),
                finished: AtomicUsize::new(0),
                gate: Semaphore::new(0),
            })
        }

        fn started(&self) -> usize {
            self.started.load(Ordering::SeqCst)
        }

        fn finished(&self) -> usize {
            self.finished.load(Ordering::SeqCst)
        }
    }

    fn counting(probe: Arc<Probe>) -> impl Fn(Transport) -> ConnectionFuture {
        move |transport| -> ConnectionFuture {
            let probe = Arc::clone(&probe);
            Box::pin(async move {
                probe.started.fetch_add(1, Ordering::SeqCst);
                let permit = probe.gate.acquire().await.expect("gate is open");
                permit.forget();
                probe.finished.fetch_add(1, Ordering::SeqCst);
                drop(transport);
                Ok(())
            })
        }
    }

    fn exhausted(errno: i32) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(errno))
    }

    /// Runs every other task until all of them wait on the paused clock.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    /// The cap admits 64 connections at once. The 65th is not accepted until
    /// one of them finishes.
    #[tokio::test(start_paused = true)]
    async fn the_next_connection_waits_for_a_free_slot_at_the_cap() {
        let listener = Scripted::new((0..66).map(|_| Ok(())));
        let probe = Probe::new();
        let serve = accept_loop(&listener, Path::new(SOCKET), counting(Arc::clone(&probe)));
        let check = async {
            settle().await;
            assert_eq!(probe.started(), 64, "the cap admits 64 connections");
            probe.gate.add_permits(1);
            settle().await;
            assert_eq!(probe.started(), 65, "a freed slot admits one more");
            probe.gate.add_permits(2);
            settle().await;
            assert_eq!(probe.started(), 66, "two freed slots admit two more");
        };
        tokio::select! {
            result = serve => panic!("the accept loop ended: {result:?}"),
            () = check => {}
        }
    }

    /// Three descriptor-exhaustion failures leave the loop alive. Each one
    /// pauses accepting for 100 ms, and the fourth connection is served.
    #[tokio::test(start_paused = true)]
    async fn the_listener_survives_descriptor_exhaustion() {
        let listener = Scripted::new([
            exhausted(EMFILE),
            exhausted(ENFILE),
            exhausted(EMFILE),
            Ok(()),
        ]);
        let probe = Probe::new();
        let serve = accept_loop(&listener, Path::new(SOCKET), counting(Arc::clone(&probe)));
        let check = async {
            tokio::time::sleep(Duration::from_millis(250)).await;
            assert_eq!(probe.started(), 0, "each failure pauses accepting");
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(probe.started(), 1, "the fourth accept is served");
        };
        tokio::select! {
            result = serve => panic!("the accept loop ended: {result:?}"),
            () = check => {}
        }
    }

    /// Running connections are the ones that free descriptors, so the pause
    /// after an exhausted accept must keep polling them.
    #[tokio::test(start_paused = true)]
    async fn running_connections_progress_while_accepting_pauses() {
        let listener = Scripted::new([Ok(()), exhausted(EMFILE)]);
        let probe = Probe::new();
        let serve = accept_loop(&listener, Path::new(SOCKET), counting(Arc::clone(&probe)));
        let check = async {
            settle().await;
            assert_eq!(probe.started(), 1);
            probe.gate.add_permits(1);
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert_eq!(probe.finished(), 1, "it finishes during the pause");
        };
        tokio::select! {
            result = serve => panic!("the accept loop ended: {result:?}"),
            () = check => {}
        }
    }

    /// A listener that wins the bind race for a path leaves the loser `InUse`,
    /// not an OS message.
    #[test]
    fn a_taken_path_at_bind_is_in_use() {
        let taken = bind_error(
            Path::new(SOCKET),
            &io::Error::from(io::ErrorKind::AddrInUse),
        );
        assert!(
            matches!(taken, LocalSocketError::InUse { ref path } if path == Path::new(SOCKET)),
            "got {taken:?}"
        );
        let other = bind_error(
            Path::new(SOCKET),
            &io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert!(
            matches!(other, LocalSocketError::Os { .. }),
            "got {other:?}"
        );
    }

    /// An interrupted accept is retried at once; any other failure ends the
    /// listener and names the socket.
    #[tokio::test(start_paused = true)]
    async fn an_unexpected_accept_failure_ends_the_listener() {
        let listener = Scripted::new([
            Err(io::Error::from(io::ErrorKind::Interrupted)),
            Err(io::Error::from(io::ErrorKind::PermissionDenied)),
        ]);
        let result = accept_loop(&listener, Path::new(SOCKET), counting(Probe::new())).await;
        match result {
            Err(LocalSocketError::Os { path, message }) => {
                assert_eq!(path, Path::new(SOCKET));
                assert!(!message.is_empty(), "the failure carries the OS message");
            }
            other => panic!("expected an OS failure naming the socket, got {other:?}"),
        }
    }
}
