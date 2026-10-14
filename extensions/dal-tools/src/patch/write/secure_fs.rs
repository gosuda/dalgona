//! Workspace-confined filesystem writes for patch commits.
//!
//! Every operation is rooted in open directory handles. Unix uses *at
//! operations and never follows symlinks. Windows holds each ancestor directory
//! open without FILE_SHARE_DELETE, preventing a checked parent from being
//! replaced while pathname-based Windows operations run.

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[cfg(windows)]
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

/// Pins every traversed directory for the lifetime of one commit.
pub(super) struct SecureFs {
    root: PathBuf,
    directories: HashMap<PathBuf, Arc<File>>,
}

impl SecureFs {
    pub(super) fn new(workspace: &Path) -> io::Result<Self> {
        let root = std::fs::canonicalize(workspace)?;
        let handle = open_root(&root)?;
        let mut directories = HashMap::new();
        directories.insert(root.clone(), Arc::new(handle));
        Ok(Self { root, directories })
    }

    /// Returns the verified parent handle and one final filename component.
    ///
    /// Only missing directories may be created, relative to an already pinned
    /// parent. No caller-supplied absolute path is used as filesystem authority.
    fn parent(&mut self, path: &Path, create: bool) -> io::Result<(Arc<File>, OsString)> {
        let bytes = path.as_os_str().as_encoded_bytes();
        if bytes.ends_with(b"/")
            || bytes.ends_with(b"\\")
            || bytes.ends_with(b"/.")
            || bytes.ends_with(b"\\.")
        {
            return Err(invalid_path());
        }
        let relative = path.strip_prefix(&self.root).map_err(|_| invalid_path())?;
        let mut components = relative.components().peekable();
        let mut current = self.root.clone();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(invalid_path());
            };
            if components.peek().is_none() {
                return Ok((
                    Arc::clone(self.directories.get(&current).ok_or_else(invalid_path)?),
                    name.to_os_string(),
                ));
            }
            let child_path = current.join(name);
            if !self.directories.contains_key(&child_path) {
                let parent = self.directories.get(&current).ok_or_else(invalid_path)?;
                let opened = open_child_dir(parent, &child_path, name);
                let handle = match opened {
                    Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                        match create_child_dir(parent, &child_path, name) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                            Err(error) => return Err(error),
                        }
                        open_child_dir(parent, &child_path, name)?
                    }
                    result => result?,
                };
                self.directories.insert(child_path.clone(), Arc::new(handle));
            }
            current = child_path;
        }
        Err(invalid_path())
    }

    pub(super) fn read_optional(&mut self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let (parent, name) = match self.parent(path, false) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut file = match open_regular(&parent, path, &name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    /// Atomically reserves the temporary name under the pinned parent, then
    /// writes and syncs the returned file handle.
    pub(super) fn write_new(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let (parent, name) = self.parent(path, true)?;
        let mut file = create_exclusive(&parent, path, &name)?;
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if result.is_err() {
            let _ = self.remove_file(path);
        }
        result
    }

    pub(super) fn move_file(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        let (from_dir, from_name) = self.parent(from, false)?;
        let (to_dir, to_name) = self.parent(to, false)?;
        move_entry(&from_dir, from, &from_name, &to_dir, to, &to_name)
    }

    pub(super) fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        let (parent, name) = self.parent(path, false)?;
        remove_entry(&parent, path, &name)
    }

    pub(super) fn sync_parent(&self, path: &Path) {
        if let Some(handle) = self.directories.get(path) {
            let _ = handle.sync_all();
        }
    }
}

fn invalid_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "patch path is not a regular workspace-relative file",
    )
}

#[cfg(unix)]
fn open_root(root: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

#[cfg(unix)]
fn open_child_dir(parent: &File, _path: &Path, name: &OsStr) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

#[cfg(unix)]
fn create_child_dir(parent: &File, _path: &Path, name: &OsStr) -> io::Result<()> {
    rustix::fs::mkdirat(parent, name, rustix::fs::Mode::RWXU).map_err(Into::into)
}

#[cfg(unix)]
fn open_regular(parent: &File, _path: &Path, name: &OsStr) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    let file = File::from(
        openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a regular file"));
    }
    Ok(file)
}

#[cfg(unix)]
fn create_exclusive(parent: &File, _path: &Path, name: &OsStr) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    openat(
        parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
    .map_err(Into::into)
}

#[cfg(unix)]
fn move_entry(
    from_dir: &File,
    _from: &Path,
    from_name: &OsStr,
    to_dir: &File,
    _to: &Path,
    to_name: &OsStr,
) -> io::Result<()> {
    rustix::fs::renameat(from_dir, from_name, to_dir, to_name).map_err(Into::into)
}

#[cfg(unix)]
fn remove_entry(parent: &File, _path: &Path, name: &OsStr) -> io::Result<()> {
    rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::empty()).map_err(Into::into)
}

#[cfg(windows)]
fn open_root(root: &Path) -> io::Result<File> {
    open_windows_dir(root)
}

#[cfg(windows)]
fn open_child_dir(_parent: &File, path: &Path, _name: &OsStr) -> io::Result<File> {
    open_windows_dir(path)
}

#[cfg(windows)]
fn create_child_dir(_parent: &File, path: &Path, _name: &OsStr) -> io::Result<()> {
    std::fs::create_dir(path)
}

#[cfg(windows)]
fn open_windows_dir(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_path());
    }
    Ok(file)
}

#[cfg(windows)]
fn open_regular(_parent: &File, path: &Path, _name: &OsStr) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a regular file"));
    }
    Ok(file)
}

#[cfg(windows)]
fn create_exclusive(_parent: &File, path: &Path, _name: &OsStr) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(windows)]
fn move_entry(
    _from_dir: &File,
    from: &Path,
    _from_name: &OsStr,
    _to_dir: &File,
    to: &Path,
    _to_name: &OsStr,
) -> io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn remove_entry(_parent: &File, path: &Path, _name: &OsStr) -> io::Result<()> {
    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::SecureFs;

    #[cfg(unix)]
    #[test]
    fn swapped_parent_never_redirects_temp_or_rename() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let parent = workspace.path().join("parent");
        std::fs::create_dir(&parent).expect("parent");
        let target = parent.join("target.txt");
        let temp = parent.join(".patch.tmp");
        let mut fs = SecureFs::new(workspace.path()).expect("secure filesystem");
        fs.parent(&target, false).expect("pin original parent");

        std::fs::rename(&parent, workspace.path().join("moved")).expect("move parent");
        std::os::unix::fs::symlink(outside.path(), &parent).expect("replace with symlink");

        fs.write_new(&temp, b"inside").expect("stage through pinned directory");
        fs.move_file(&temp, &target).expect("install through pinned directory");
        assert_eq!(
            std::fs::read(workspace.path().join("moved/target.txt")).expect("inside file"),
            b"inside"
        );
        assert!(!outside.path().join("target.txt").exists());
        assert!(!outside.path().join(".patch.tmp").exists());
    }

    #[cfg(windows)]
    #[test]
    fn pinned_parent_cannot_be_renamed() {
        let workspace = tempfile::tempdir().expect("workspace");
        let parent = workspace.path().join("parent");
        std::fs::create_dir(&parent).expect("parent");
        let target = parent.join("target.txt");
        let mut fs = SecureFs::new(workspace.path()).expect("secure filesystem");
        fs.parent(&target, false).expect("pin original parent");
        assert!(
            std::fs::rename(&parent, workspace.path().join("moved")).is_err(),
            "open parent handle must prevent replacement"
        );
        fs.write_new(&target, b"inside").expect("write under pinned parent");
        assert_eq!(std::fs::read(&target).expect("target"), b"inside");
    }
}
