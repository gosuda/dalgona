//! Workspace-confined, no-replace publication of patch rename destinations.

use std::{ffi::OsString, io, path::{Path, PathBuf}};
#[cfg(unix)]
use std::path::Component;
use tokio::io::AsyncWriteExt as _;

use super::{PatchSession, commit::nonce_u128};
use super::super::{
    ir::{EngineError, ErrorClass},
    resolve::resolve_rename_destination,
};

pub(super) struct PreparedRename {
    pub(super) target: PathBuf,
    pub(super) temp: PathBuf,
    #[cfg(unix)]
    directory: rustix::fd::OwnedFd,
    #[cfg(unix)]
    temp_name: OsString,
    #[cfg(unix)]
    target_name: OsString,
    #[cfg(not(unix))]
    workspace: PathBuf,
}

impl PreparedRename {
    pub(super) async fn stage(
        session: &PatchSession,
        dest: &Path,
        after: &[u8],
    ) -> Result<Self, EngineError> {
        let relative = resolve_rename_destination(&session.workspace, dest)?;
        let target = session.workspace.join(&relative);
        let target_name = relative.file_name().ok_or_else(|| {
            EngineError::new(ErrorClass::Resolve, "patch: invalid rename destination.")
        })?;
        let temp_name = OsString::from(format!(
            ".dalgon-patch-{}-{:032x}.tmp",
            std::process::id(),
            nonce_u128()
        ));
        let parent = target.parent().ok_or_else(|| {
            EngineError::new(ErrorClass::Resolve, "patch: invalid rename parent.")
        })?;
        let temp = parent.join(&temp_name);

        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, Mode, OFlags};
            let directory = open_parent(&session.workspace, &relative)
                .map_err(|error| write_error(dest, error))?;
            match rustix::fs::statat(&directory, target_name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => return Err(destination_exists(dest)),
                Err(rustix::io::Errno::NOENT) => {}
                Err(error) => return Err(write_error(dest, io::Error::from(error))),
            }
            let handle = rustix::fs::openat(
                &directory,
                temp_name.as_os_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|error| write_error(dest, io::Error::from(error)))?;
            let mut file = tokio::fs::File::from_std(std::fs::File::from(handle));
            let result = async {
                file.write_all(after).await?;
                file.sync_all().await
            }.await;
            drop(file);
            if let Err(error) = result {
                let _ = rustix::fs::unlinkat(&directory, temp_name.as_os_str(), AtFlags::empty());
                return Err(write_error(dest, error));
            }
            Ok(Self { target, temp, directory, temp_name, target_name: target_name.to_os_string() })
        }

        #[cfg(not(unix))]
        {
            tokio::fs::create_dir_all(parent).await.map_err(|error| write_error(dest, error))?;
            match tokio::fs::symlink_metadata(&target).await {
                Ok(_) => return Err(destination_exists(dest)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(write_error(dest, error)),
            }
            let mut file = tokio::fs::OpenOptions::new()
                .write(true).create_new(true).open(&temp).await
                .map_err(|error| write_error(dest, error))?;
            let result = async {
                file.write_all(after).await?;
                file.sync_all().await
            }.await;
            drop(file);
            if let Err(error) = result {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(write_error(dest, error));
            }
            Ok(Self { target, temp, workspace: session.workspace.clone() })
        }
    }

    pub(super) async fn publish(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            rustix::fs::linkat(
                &self.directory, self.temp_name.as_os_str(),
                &self.directory, self.target_name.as_os_str(),
                rustix::fs::AtFlags::empty(),
            ).map_err(io::Error::from)
        }
        #[cfg(not(unix))]
        {
            let relative = self.target.strip_prefix(&self.workspace).map_err(io::Error::other)?;
            resolve_rename_destination(&self.workspace, relative).map_err(io::Error::other)?;
            tokio::fs::hard_link(&self.temp, &self.target).await
        }
    }

    pub(super) async fn cleanup(&self) {
        #[cfg(unix)]
        {
            let _ = rustix::fs::unlinkat(
                &self.directory, self.temp_name.as_os_str(), rustix::fs::AtFlags::empty(),
            );
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::fs::remove_file(&self.temp).await;
        }
    }
}

#[cfg(unix)]
fn open_parent(workspace: &Path, relative: &Path) -> io::Result<rustix::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = rustix::fs::open(workspace, flags, Mode::empty()).map_err(io::Error::from)?;
    if let Some(parent) = relative.parent() {
        for part in parent.components() {
            let Component::Normal(name) = part else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid rename parent"));
            };
            match rustix::fs::mkdirat(&directory, name, Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(io::Error::from(error)),
            }
            directory = rustix::fs::openat(&directory, name, flags, Mode::empty())
                .map_err(io::Error::from)?;
        }
    }
    Ok(directory)
}

fn destination_exists(dest: &Path) -> EngineError {
    EngineError::new(ErrorClass::File, format!("patch: rename destination {} already exists.", dest.display()))
}

fn write_error(dest: &Path, error: io::Error) -> EngineError {
    EngineError::new(ErrorClass::Io, format!("patch: cannot write {}: {error}. Nothing was written.", dest.display()))
}
