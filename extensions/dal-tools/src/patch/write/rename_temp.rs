//! Workspace-anchored, no-replace publication for patch rename destinations.

#[cfg(unix)]
mod platform {
    use std::{
        ffi::OsString,
        io,
        os::fd::OwnedFd,
        path::{Component, Path, PathBuf},
    };

    use rustix::{
        fs::{AtFlags, Mode, OFlags},
        io::Errno,
    };

    pub(crate) struct RenameTemp {
        target: PathBuf,
        parent: OwnedFd,
        name: OsString,
        temp: OsString,
    }

    impl RenameTemp {
        /// Open each parent below the workspace without following symlinks.
        /// Keep the final directory handle through publication.
        pub(crate) fn create(
            workspace: &Path,
            relative: &Path,
        ) -> io::Result<(Self, std::fs::File)> {
            if !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "rename destination must be workspace-relative",
                ));
            }
            let name = relative
                .file_name()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "empty rename destination")
                })?
                .to_os_string();
            let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let mut parent =
                rustix::fs::open(workspace, dir_flags, Mode::empty()).map_err(io::Error::from)?;
            for component in relative.parent().into_iter().flat_map(Path::components) {
                let Component::Normal(segment) = component else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid rename parent",
                    ));
                };
                parent = match rustix::fs::openat(&parent, segment, dir_flags, Mode::empty()) {
                    Ok(opened) => opened,
                    Err(Errno::NOENT) => {
                        let mode = Mode::RWXU | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH;
                        match rustix::fs::mkdirat(&parent, segment, mode) {
                            Ok(()) | Err(Errno::EXIST) => {}
                            Err(error) => return Err(io::Error::from(error)),
                        }
                        rustix::fs::openat(&parent, segment, dir_flags, Mode::empty())
                            .map_err(io::Error::from)?
                    }
                    Err(error) => return Err(io::Error::from(error)),
                };
            }
            match rustix::fs::statat(&parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "rename destination already exists",
                    ));
                }
                Err(Errno::NOENT) => {}
                Err(error) => return Err(io::Error::from(error)),
            }
            for _ in 0..16 {
                let temp = OsString::from(format!(
                    ".dalgon-patch-{}-{:032x}.tmp",
                    std::process::id(),
                    super::super::commit::nonce_u128()
                ));
                let flags = OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC;
                // Match the existing patch writer's default mode (0666 & umask).
                let mode =
                    Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH;
                match rustix::fs::openat(&parent, temp.as_os_str(), flags, mode) {
                    Ok(handle) => {
                        return Ok((
                            Self {
                                target: workspace.join(relative),
                                parent,
                                name,
                                temp,
                            },
                            handle.into(),
                        ));
                    }
                    Err(Errno::EXIST) => {}
                    Err(error) => return Err(io::Error::from(error)),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "unable to allocate a unique rename temporary file",
            ))
        }

        pub(crate) fn target(&self) -> &Path {
            &self.target
        }

        /// The parent descriptor prevents symlink substitution after staging.
        /// Publication is atomic and refuses even dangling symlink destinations.
        pub(crate) fn publish(&self) -> io::Result<()> {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                rustix::fs::renameat_with(
                    &self.parent,
                    self.temp.as_os_str(),
                    &self.parent,
                    self.name.as_os_str(),
                    rustix::fs::RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from)
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                rustix::fs::linkat(
                    &self.parent,
                    self.temp.as_os_str(),
                    &self.parent,
                    self.name.as_os_str(),
                    AtFlags::empty(),
                )
                .map_err(io::Error::from)?;
                if let Err(error) =
                    rustix::fs::unlinkat(&self.parent, self.temp.as_os_str(), AtFlags::empty())
                {
                    let _ =
                        rustix::fs::unlinkat(&self.parent, self.name.as_os_str(), AtFlags::empty());
                    return Err(io::Error::from(error));
                }
                Ok(())
            }
        }

        pub(crate) fn cleanup(&self) {
            let _ = rustix::fs::unlinkat(&self.parent, self.temp.as_os_str(), AtFlags::empty());
        }

        pub(crate) fn rollback(&self) -> io::Result<()> {
            match rustix::fs::unlinkat(&self.parent, self.name.as_os_str(), AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => Ok(()),
                Err(error) => Err(io::Error::from(error)),
            }
        }

        pub(crate) fn sync_dir(&self) {
            let _ = rustix::fs::fsync(&self.parent);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        fs::{self, File, OpenOptions},
        io,
        os::windows::fs::{MetadataExt, OpenOptionsExt},
        path::{Component, Path, PathBuf},
    };

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    /// Holding all directory handles without FILE_SHARE_DELETE prevents a
    /// parent from being replaced between validation and publication.
    pub(crate) struct RenameTemp {
        target: PathBuf,
        temp: PathBuf,
        _directories: Vec<File>,
    }

    fn open_directory(path: &Path) -> io::Result<File> {
        let handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let metadata = handle.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "rename parent must be a directory without reparse points",
            ));
        }
        Ok(handle)
    }

    impl RenameTemp {
        pub(crate) fn create(workspace: &Path, relative: &Path) -> io::Result<(Self, File)> {
            if !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "rename destination must be workspace-relative",
                ));
            }
            let mut parent = workspace.to_path_buf();
            let mut directories = vec![open_directory(&parent)?];
            for component in relative.parent().into_iter().flat_map(Path::components) {
                let Component::Normal(segment) = component else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid rename parent",
                    ));
                };
                parent.push(segment);
                match fs::create_dir(&parent) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                directories.push(open_directory(&parent)?);
            }
            let target = workspace.join(relative);
            match fs::symlink_metadata(&target) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "rename destination already exists",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            for _ in 0..16 {
                let temp = parent.join(format!(
                    ".dalgon-patch-{}-{:032x}.tmp",
                    std::process::id(),
                    super::super::commit::nonce_u128()
                ));
                match OpenOptions::new().write(true).create_new(true).open(&temp) {
                    Ok(handle) => {
                        return Ok((
                            Self {
                                target,
                                temp,
                                _directories: directories,
                            },
                            handle,
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "unable to allocate a unique rename temporary file",
            ))
        }

        pub(crate) fn target(&self) -> &Path {
            &self.target
        }

        /// Creating a hard link is atomic and never replaces an existing name.
        pub(crate) fn publish(&self) -> io::Result<()> {
            fs::hard_link(&self.temp, &self.target)?;
            if let Err(error) = fs::remove_file(&self.temp) {
                let _ = fs::remove_file(&self.target);
                return Err(error);
            }
            Ok(())
        }

        pub(crate) fn cleanup(&self) {
            let _ = fs::remove_file(&self.temp);
        }

        pub(crate) fn rollback(&self) -> io::Result<()> {
            match fs::remove_file(&self.target) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        }

        pub(crate) fn sync_dir(&self) {}
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::{
        fs::File,
        io,
        path::{Path, PathBuf},
    };

    pub(crate) struct RenameTemp {
        target: PathBuf,
    }

    impl RenameTemp {
        pub(crate) fn create(workspace: &Path, relative: &Path) -> io::Result<(Self, File)> {
            let _ = (workspace, relative);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure rename is unsupported on this platform",
            ))
        }

        pub(crate) fn target(&self) -> &Path {
            &self.target
        }

        pub(crate) fn publish(&self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure rename is unsupported",
            ))
        }

        pub(crate) fn cleanup(&self) {}
        pub(crate) fn rollback(&self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "secure rename rollback is unsupported",
            ))
        }

        pub(crate) fn sync_dir(&self) {}
    }
}

pub(crate) use platform::RenameTemp;

#[cfg(test)]
mod tests {
    use std::{io::Write as _, path::Path};

    use super::RenameTemp;

    #[test]
    fn rename_publication_never_replaces_a_racing_destination() {
        let workspace = tempfile::tempdir().expect("workspace");
        let destination = Path::new("nested/target.txt");
        let (pending, mut temp) =
            RenameTemp::create(workspace.path(), destination).expect("stage rename");
        temp.write_all(b"edited").expect("write staged bytes");
        temp.sync_all().expect("sync staged bytes");
        drop(temp);

        let target = workspace.path().join(destination);
        std::fs::write(&target, b"competitor").expect("create destination after staging");
        let error = pending
            .publish()
            .expect_err("no-replace publication must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(target).expect("read"), b"competitor");
        pending.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn parent_swapped_after_staging_cannot_redirect_publication() {
        let root = tempfile::tempdir().expect("test root");
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&outside).expect("outside");
        let original_parent = workspace.join("sub");
        std::fs::create_dir(&original_parent).expect("destination parent");
        let (pending, mut temp) =
            RenameTemp::create(&workspace, Path::new("sub/result.txt")).expect("staged rename");
        temp.write_all(b"inside").expect("write");
        temp.sync_all().expect("sync");
        drop(temp);

        std::fs::rename(&original_parent, workspace.join("moved_sub"))
            .expect("move opened directory");
        std::os::unix::fs::symlink(&outside, &original_parent).expect("replace with symlink");
        pending.publish().expect("publish via anchored directory");
        assert_eq!(
            std::fs::read(workspace.join("moved_sub/result.txt")).expect("read original parent"),
            b"inside"
        );
        assert!(!outside.join("result.txt").exists());
        pending.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_destination_symlink_is_not_replaced() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (pending, mut temp) =
            RenameTemp::create(workspace.path(), Path::new("target.txt")).expect("stage");
        temp.write_all(b"inside").expect("write");
        drop(temp);
        std::os::unix::fs::symlink("missing.txt", workspace.path().join("target.txt"))
            .expect("insert dangling symlink");
        let error = pending.publish().expect_err("symlink must not be replaced");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(
            std::fs::symlink_metadata(workspace.path().join("target.txt"))
                .expect("symlink still exists")
                .file_type()
                .is_symlink()
        );
        pending.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn rename_parent_symlink_is_not_followed() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("link")).expect("symlink");
        assert!(RenameTemp::create(workspace.path(), Path::new("link/target.txt")).is_err());
        assert!(!outside.path().join("target.txt").exists());
    }
}
