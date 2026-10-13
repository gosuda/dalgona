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
    if bytes > 103 {
        return Err(LocalSocketError::PathTooLong {
            path: path.to_owned(),
            bytes,
        });
    }
    prepare_unix_parent(path, default_data_root)?;
    remove_stale_socket(path).await?;
    let listener = UnixListener::bind(path).map_err(|error| os_error(path, &error))?;
    let _path_guard = SocketPathGuard::capture(path.to_owned())?;
    let mut active = FuturesUnordered::<ConnectionFuture>::new();

    loop {
        if active.len() >= 64 {
            log_connection_result(active.next().await);
            continue;
        }
        tokio::select! {
            result = active.next(), if !active.is_empty() => log_connection_result(result),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let (reader, writer) = stream.into_split();
                    active.push(on_connection(Transport::Local(LocalTransport::new(reader, writer))));
                }
                Err(error) if is_descriptor_exhaustion(&error) => {
                    tracing::warn!(%error, "local RPC accept hit the descriptor limit; retrying");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(os_error(path, &error)),
            }
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
