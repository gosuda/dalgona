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

    /// Base flags for directory descriptors. Where `O_PATH` exists, a walk
    /// needs only search permission, so a parent granting write and execute
    /// without read still accepts a staged rename, as the pathname writer did.
    /// `O_PATH` descriptors accept every `*at` operation used here but not
    /// `fsync`, so a separate readable descriptor is requested for syncing.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "emscripten",
        target_os = "freebsd",
        target_os = "fuchsia",
        target_os = "redox"
    ))]
    fn dir_flags() -> OFlags {
        OFlags::PATH | OFlags::DIRECTORY
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "emscripten",
        target_os = "freebsd",
        target_os = "fuchsia",
        target_os = "redox"
    )))]
    fn dir_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY
    }

    /// The workspace root is the approved boundary itself: a linked root
    /// resolves to the directory it names. No-follow checks apply below it.
    fn root_flags() -> OFlags {
        dir_flags() | OFlags::CLOEXEC
    }

    fn walk_flags() -> OFlags {
        dir_flags() | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    fn walk_segments(
        mut dir: OwnedFd,
        relative_parent: &Path,
        create_missing: bool,
    ) -> io::Result<OwnedFd> {
        for component in relative_parent.components() {
            let Component::Normal(segment) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid rename parent",
                ));
            };
            dir = match rustix::fs::openat(&dir, segment, walk_flags(), Mode::empty()) {
                Ok(opened) => opened,
                Err(Errno::NOENT) if create_missing => {
                    let mode = Mode::RWXU | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH;
                    match rustix::fs::mkdirat(&dir, segment, mode) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => return Err(io::Error::from(error)),
                    }
                    rustix::fs::openat(&dir, segment, walk_flags(), Mode::empty())
                        .map_err(io::Error::from)?
                }
                Err(error) => return Err(io::Error::from(error)),
            };
        }
        Ok(dir)
    }

    /// Maps saved permission bits onto an `openat` create mode; `None` is a
    /// new file's 0644. Built flag by flag because `RawMode` is not `u32` on
    /// every Unix.
    fn create_mode(saved: Option<u32>) -> Mode {
        const BITS: [(u32, Mode); 9] = [
            (0o400, Mode::RUSR),
            (0o200, Mode::WUSR),
            (0o100, Mode::XUSR),
            (0o040, Mode::RGRP),
            (0o020, Mode::WGRP),
            (0o010, Mode::XGRP),
            (0o004, Mode::ROTH),
            (0o002, Mode::WOTH),
            (0o001, Mode::XOTH),
        ];
        let bits = saved.unwrap_or(0o644);
        BITS.iter()
            .filter(|(bit, _)| bits & bit != 0)
            .fold(Mode::empty(), |mode, (_, flag)| mode | *flag)
    }

    pub(crate) struct RenameTemp {
        target: PathBuf,
        relative: PathBuf,
        workspace: OwnedFd,
        parent: OwnedFd,
        sync_parent: Option<OwnedFd>,
        name: OsString,
        temp: OsString,
    }

    impl RenameTemp {
        /// Open each parent below the workspace without following symlinks.
        /// Keep the final directory handle through publication. The temp is
        /// born with `saved_mode`, the permission bits it will publish with,
        /// or 0644 for `None`; the umask only narrows that.
        pub(crate) fn create(
            workspace: &Path,
            relative: &Path,
            saved_mode: Option<u32>,
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
            let workspace_fd = rustix::fs::open(workspace, root_flags(), Mode::empty())
                .map_err(io::Error::from)?;
            let root = rustix::fs::openat(&workspace_fd, ".", walk_flags(), Mode::empty())
                .map_err(io::Error::from)?;
            let parent = walk_segments(root, relative.parent().unwrap_or(Path::new("")), true)?;
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
            // Born with the mode it will publish with: a descriptor opened
            // while the temp was wider would survive the exact stamp that
            // commit.rs applies to the handle afterwards.
            let mode = create_mode(saved_mode);
            let mut staged = None;
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
                match rustix::fs::openat(&parent, temp.as_os_str(), flags, mode) {
                    Ok(handle) => {
                        staged = Some((temp, handle));
                        break;
                    }
                    Err(Errno::EXIST) => {}
                    Err(error) => return Err(io::Error::from(error)),
                }
            }
            let Some((temp, handle)) = staged else {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "unable to allocate a unique rename temporary file",
                ));
            };
            // `fsync` needs a readable descriptor; a search-only parent syncs
            // nothing it cannot expose and publication stays unsynced there.
            let sync_parent = rustix::fs::openat(
                &parent,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .ok();
            Ok((
                Self {
                    target: workspace.join(relative),
                    relative: relative.to_path_buf(),
                    workspace: workspace_fd,
                    parent,
                    sync_parent,
                    name,
                    temp,
                },
                handle.into(),
            ))
        }

        pub(crate) fn target(&self) -> &Path {
            &self.target
        }

        /// Re-resolve the destination parent beneath the workspace and prove
        /// it is still the staged directory, so a moved, removed, or swapped
        /// parent cannot redirect publication outside the approved root.
        fn verified_parent(&self) -> io::Result<OwnedFd> {
            let root = rustix::fs::openat(&self.workspace, ".", walk_flags(), Mode::empty())
                .map_err(io::Error::from)?;
            let dir = walk_segments(root, self.relative.parent().unwrap_or(Path::new("")), false)?;
            let staged = rustix::fs::fstat(&self.parent).map_err(io::Error::from)?;
            let current = rustix::fs::fstat(&dir).map_err(io::Error::from)?;
            if staged.st_dev != current.st_dev || staged.st_ino != current.st_ino {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "rename destination parent moved or was replaced",
                ));
            }
            Ok(dir)
        }

        /// A descriptor follows its directory when another actor moves it,
        /// even mid-publication. Prove the approved path still names the
        /// staged directory after the rename; when it does not, remove the
        /// published name wherever the moved directory landed so an escape
        /// never persists outside the workspace.
        fn confirm_landed(&self) -> io::Result<()> {
            match self.verified_parent() {
                Ok(_) => Ok(()),
                Err(error) => {
                    let _ =
                        rustix::fs::unlinkat(&self.parent, self.name.as_os_str(), AtFlags::empty());
                    Err(error)
                }
            }
        }

        /// The parent descriptor prevents symlink substitution after staging.
        /// Publication is atomic and refuses even dangling symlink destinations.
        pub(crate) fn publish(&self) -> io::Result<()> {
            let dir = self.verified_parent()?;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                rustix::fs::renameat_with(
                    &dir,
                    self.temp.as_os_str(),
                    &dir,
                    self.name.as_os_str(),
                    rustix::fs::RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from)?;
                self.confirm_landed()
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                rustix::fs::linkat(
                    &dir,
                    self.temp.as_os_str(),
                    &dir,
                    self.name.as_os_str(),
                    AtFlags::empty(),
                )
                .map_err(io::Error::from)?;
                if let Err(error) =
                    rustix::fs::unlinkat(&dir, self.temp.as_os_str(), AtFlags::empty())
                {
                    let _ = rustix::fs::unlinkat(&dir, self.name.as_os_str(), AtFlags::empty());
                    return Err(io::Error::from(error));
                }
                self.confirm_landed()
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
            if let Some(dir) = &self.sync_parent {
                let _ = rustix::fs::fsync(dir);
            }
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
    /// The handles pin directories only; access mode 0 requests no data
    /// access, so a parent the caller can write but not read still opens.
    pub(crate) struct RenameTemp {
        target: PathBuf,
        resolved_target: PathBuf,
        temp: PathBuf,
        _directories: Vec<File>,
    }

    fn open_directory(path: &Path) -> io::Result<File> {
        let handle = OpenOptions::new()
            .access_mode(0)
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
        /// Windows destinations inherit their ACLs from the parent directory,
        /// so the saved POSIX bits have nothing to apply to.
        pub(crate) fn create(
            workspace: &Path,
            relative: &Path,
            _saved_mode: Option<u32>,
        ) -> io::Result<(Self, File)> {
            if !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "rename destination must be workspace-relative",
                ));
            }
            let name = relative.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "empty rename destination")
            })?;
            // The workspace root is the approved boundary: resolve a linked
            // root once so a retargeted link cannot redirect the paths below.
            let mut parent = workspace.canonicalize()?;
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
            let resolved_target = parent.join(name);
            match fs::symlink_metadata(&resolved_target) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "rename destination already exists",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let mut staged = None;
            for _ in 0..16 {
                let temp = parent.join(format!(
                    ".dalgon-patch-{}-{:032x}.tmp",
                    std::process::id(),
                    super::super::commit::nonce_u128()
                ));
                match OpenOptions::new().write(true).create_new(true).open(&temp) {
                    Ok(handle) => {
                        staged = Some((temp, handle));
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            let Some((temp, handle)) = staged else {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "unable to allocate a unique rename temporary file",
                ));
            };
            Ok((
                Self {
                    target: workspace.join(relative),
                    resolved_target,
                    temp,
                    _directories: directories,
                },
                handle,
            ))
        }

        pub(crate) fn target(&self) -> &Path {
            &self.target
        }

        /// Creating a hard link is atomic and never replaces an existing name.
        pub(crate) fn publish(&self) -> io::Result<()> {
            fs::hard_link(&self.temp, &self.resolved_target)?;
            if let Err(error) = fs::remove_file(&self.temp) {
                let _ = fs::remove_file(&self.resolved_target);
                return Err(error);
            }
            Ok(())
        }

        pub(crate) fn cleanup(&self) {
            let _ = fs::remove_file(&self.temp);
        }

        pub(crate) fn rollback(&self) -> io::Result<()> {
            match fs::remove_file(&self.resolved_target) {
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
        pub(crate) fn create(
            workspace: &Path,
            relative: &Path,
            saved_mode: Option<u32>,
        ) -> io::Result<(Self, File)> {
            let _ = (workspace, relative, saved_mode);
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
            RenameTemp::create(workspace.path(), destination, None).expect("stage rename");
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
        let (pending, mut temp) = RenameTemp::create(&workspace, Path::new("sub/result.txt"), None)
            .expect("staged rename");
        temp.write_all(b"inside").expect("write");
        temp.sync_all().expect("sync");
        drop(temp);

        std::fs::rename(&original_parent, workspace.join("moved_sub"))
            .expect("move opened directory");
        std::os::unix::fs::symlink(&outside, &original_parent).expect("replace with symlink");
        // The approved path no longer names the staged directory, so
        // publication must refuse rather than follow the planted link or
        // write beneath the moved inode.
        pending
            .publish()
            .expect_err("publication must refuse a swapped parent");
        assert!(!outside.join("result.txt").exists());
        pending.cleanup();
        assert_eq!(
            std::fs::read_dir(workspace.join("moved_sub"))
                .expect("read moved parent")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn parent_moved_outside_workspace_refuses_publication() {
        let root = tempfile::tempdir().expect("test root");
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&outside).expect("outside");
        let original_parent = workspace.join("sub");
        std::fs::create_dir(&original_parent).expect("destination parent");
        let (pending, mut temp) = RenameTemp::create(&workspace, Path::new("sub/result.txt"), None)
            .expect("staged rename");
        temp.write_all(b"inside").expect("write");
        temp.sync_all().expect("sync");
        drop(temp);

        std::fs::rename(&original_parent, outside.join("sub")).expect("move outside workspace");
        let error = pending
            .publish()
            .expect_err("publication must refuse a parent outside the workspace");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!outside.join("sub/result.txt").exists());
        pending.cleanup();
        assert_eq!(
            std::fs::read_dir(outside.join("sub"))
                .expect("read moved parent")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_workspace_root_is_resolved_once() {
        let root = tempfile::tempdir().expect("test root");
        let real = root.path().join("real");
        std::fs::create_dir(&real).expect("real workspace");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("link workspace root");
        let (pending, mut temp) = RenameTemp::create(&link, Path::new("nested/result.txt"), None)
            .expect("a linked workspace root must stage");
        temp.write_all(b"inside").expect("write");
        temp.sync_all().expect("sync");
        drop(temp);
        pending.publish().expect("publish beneath resolved root");
        assert_eq!(
            std::fs::read(real.join("nested/result.txt")).expect("read"),
            b"inside"
        );
        pending.cleanup();
    }

    #[cfg(all(unix, target_os = "linux"))]
    #[test]
    fn search_only_parent_accepts_rename() {
        use std::os::unix::fs::PermissionsExt as _;
        let workspace = tempfile::tempdir().expect("workspace");
        let parent = workspace.path().join("private");
        std::fs::create_dir(&parent).expect("parent");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o300))
            .expect("write-and-search-only parent");
        let (pending, mut temp) =
            RenameTemp::create(workspace.path(), Path::new("private/result.txt"), None)
                .expect("a write-and-search-only parent must stage");
        temp.write_all(b"inside").expect("write");
        temp.sync_all().expect("sync");
        drop(temp);
        pending.publish().expect("publish into search-only parent");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
            .expect("restore permissions");
        assert_eq!(
            std::fs::read(parent.join("result.txt")).expect("read"),
            b"inside"
        );
        pending.cleanup();
    }

    #[cfg(windows)]
    #[test]
    fn linked_workspace_root_is_resolved_once() {
        let root = tempfile::tempdir().expect("test root");
        let real = root.path().join("real");
        std::fs::create_dir(&real).expect("real workspace");
        let link = root.path().join("link");
        // A junction links directories without requiring elevated privileges.
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .status()
            .expect("mklink junction");
        assert!(status.success());
        let (pending, mut temp) = RenameTemp::create(&link, Path::new("nested/result.txt"), None)
            .expect("a linked workspace root must stage");
        temp.write_all(b"inside").expect("write");
        temp.sync_all().expect("sync");
        drop(temp);
        pending.publish().expect("publish beneath resolved root");
        assert_eq!(
            std::fs::read(real.join("nested/result.txt")).expect("read"),
            b"inside"
        );
        pending.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_destination_symlink_is_not_replaced() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (pending, mut temp) =
            RenameTemp::create(workspace.path(), Path::new("target.txt"), None).expect("stage");
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
        assert!(RenameTemp::create(workspace.path(), Path::new("link/target.txt"), None).is_err());
        assert!(!outside.path().join("target.txt").exists());
    }

    /// A private destination must never sit in a wider temp, even briefly: a
    /// descriptor opened in that gap survives the later exact stamp. The
    /// umask only removes bits at creation, so no bit outside the saved mode
    /// may appear on the freshly created handle.
    #[cfg(unix)]
    #[test]
    fn temp_is_born_with_the_saved_mode() {
        use std::os::unix::fs::MetadataExt as _;

        let workspace = tempfile::tempdir().expect("workspace");
        let destination = Path::new("out.txt");
        for (saved, ceiling) in [(Some(0o600), 0o600), (Some(0o400), 0o400), (None, 0o644)] {
            let (pending, temp) =
                RenameTemp::create(workspace.path(), destination, saved).expect("stage rename");
            let born = temp.metadata().expect("temp metadata").mode() & 0o777;
            assert_eq!(born & !ceiling, 0, "{saved:?} born as {born:o}");
            drop(temp);
            pending.cleanup();
        }
    }
}
