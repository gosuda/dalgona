use std::{
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::fs::OpenOptions;

use thiserror::Error;
use uuid::Uuid;

/// A saved serve token represented only by its BLAKE3 digest.
#[derive(Clone)]
pub struct SecretToken {
    digest: [u8; 32],
}

impl SecretToken {
    fn from_token(token: &str) -> Self {
        Self {
            digest: *blake3::hash(token.as_bytes()).as_bytes(),
        }
    }

    /// Checks a presented token without comparing or logging token bytes.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        let candidate = blake3::hash(candidate.as_bytes());
        let mut difference = 0_u8;
        for (saved, presented) in self.digest.iter().zip(candidate.as_bytes()) {
            difference |= *saved ^ *presented;
        }
        difference == 0
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

/// A failure while creating, loading, or reading a serve token.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum TokenError {
    /// The token path is not absolute.
    #[error("token path must be absolute: {path}")]
    RelativePath {
        /// The rejected path.
        path: PathBuf,
    },
    /// The token file does not exist.
    #[error("token file does not exist: {path}")]
    Missing {
        /// The missing token path.
        path: PathBuf,
    },
    /// The token file is accessible by group or other users on POSIX.
    #[error("token file is open to other users: {path} has mode {mode:04o}")]
    OpenToOtherUsers {
        /// The token file path.
        path: PathBuf,
        /// The file mode observed on disk.
        mode: u32,
    },
    /// A Windows token file grants access beyond the current user and privileged accounts.
    #[cfg(windows)]
    #[error("token file has an unsafe Windows ACL: {path} must be owner-only and protected")]
    InsecureAcl {
        /// The rejected token file.
        path: PathBuf,
    },
    /// The token path is a symbolic link.
    #[error("token file must not be a symbolic link: {path}")]
    Symlink {
        /// The rejected token path.
        path: PathBuf,
    },
    /// The token path is not a regular file.
    #[error("token path is not a regular file: {path}")]
    NotRegularFile {
        /// The rejected token path.
        path: PathBuf,
    },
    /// The token file does not have the required mode.
    #[error("token file must have mode 0600: {path} has mode {mode:04o}")]
    ModeNot0600 {
        /// The token file path.
        path: PathBuf,
        /// The file mode observed on disk.
        mode: u32,
    },
    /// The first token line is empty.
    #[error("token file is empty: {path}")]
    Empty {
        /// The empty token path.
        path: PathBuf,
    },
    /// The first token line does not match the dal token format.
    #[error("token file has an invalid token: {path}")]
    Invalid {
        /// The invalid token path.
        path: PathBuf,
    },
    /// A token file already exists and replacement was not requested.
    #[error("token file already exists: {path}")]
    AlreadyExists {
        /// The existing token path.
        path: PathBuf,
    },
    /// A file operation failed at the token path.
    #[error("token file operation failed for {path}: {source}")]
    Io {
        /// The path involved in the failed operation.
        path: PathBuf,
        /// The underlying operating-system error.
        #[source]
        source: io::Error,
    },
}

/// Creates and durably writes a fresh token, returning it once to the caller.
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file already exists without `force`,
/// or any file write, sync, or rename step fails.
pub fn create(path: &Path, force: bool) -> Result<String, TokenError> {
    ensure_absolute(path)?;
    let token = format!("dal_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let bytes = format!("{token}\n");

    if !force {
        let mut file = open_exclusive(path).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                TokenError::AlreadyExists {
                    path: path.to_owned(),
                }
            } else {
                TokenError::Io {
                    path: path.to_owned(),
                    source,
                }
            }
        })?;
        if let Err(source) = write_sync(&mut file, bytes.as_bytes()) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(TokenError::Io {
                path: path.to_owned(),
                source,
            });
        }
        sync_parent(path)?;
        return Ok(token);
    }

    let temporary = append_suffix(path, ".tmp");
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(|source| TokenError::Io {
            path: temporary.clone(),
            source,
        })?;
    }
    let mut file = open_exclusive(&temporary).map_err(|source| TokenError::Io {
        path: temporary.clone(),
        source,
    })?;
    if let Err(source) = write_sync(&mut file, bytes.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(TokenError::Io {
            path: temporary,
            source,
        });
    }
    drop(file);
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(TokenError::Io {
            path: path.to_owned(),
            source,
        });
    }
    sync_parent(path)?;
    Ok(token)
}

/// Creates and durably writes a fresh token, returning it once to the caller.
///
/// Convenience alias for [`create`].
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file already exists without `force`,
/// or any file write, sync, or rename step fails.
pub fn create_token(path: &Path, force: bool) -> Result<String, TokenError> {
    create(path, force)
}

/// Loads a token file and retains only the digest used for authentication.
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file is missing, has unsafe
/// permissions, is empty, or does not match the expected token shape.
pub fn load(path: &Path) -> Result<SecretToken, TokenError> {
    let bytes = read_token_file(path)?;
    let (mut token, has_more_lines) = first_token_line(&bytes);
    if has_more_lines {
        tracing::warn!(path = %path.display(), "serve token file has multiple lines; using the first");
    }
    if token.last() == Some(&b'\r') {
        token = &token[..token.len() - 1];
    }
    if token.is_empty() {
        return Err(TokenError::Empty {
            path: path.to_owned(),
        });
    }
    if !valid_token(token) {
        return Err(TokenError::Invalid {
            path: path.to_owned(),
        });
    }
    let token = std::str::from_utf8(token).map_err(|_| TokenError::Invalid {
        path: path.to_owned(),
    })?;
    Ok(SecretToken::from_token(token))
}

/// Reads a token file for an authenticated client without hashing the token.
///
/// The returned string contains the first trimmed token line. Callers must treat it as a secret and
/// must not log or display it.
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file is missing, is not a regular
/// file or is a symbolic link, does not have mode 0600 on POSIX, is empty, or does not match the
/// expected token shape.
pub fn read_connect_token(path: &Path) -> Result<String, TokenError> {
    let bytes = read_token_file(path)?;
    let (token, _) = first_token_line(&bytes);
    let token = token.trim_ascii();
    if token.is_empty() {
        return Err(TokenError::Empty {
            path: path.to_owned(),
        });
    }
    if !valid_token(token) {
        return Err(TokenError::Invalid {
            path: path.to_owned(),
        });
    }
    std::str::from_utf8(token)
        .map(str::to_owned)
        .map_err(|_| TokenError::Invalid {
            path: path.to_owned(),
        })
}

fn read_token_file(path: &Path) -> Result<Vec<u8>, TokenError> {
    ensure_absolute(path)?;
    let mut file = open_token_file(path)?;
    let mut bytes = Vec::with_capacity(80);
    Read::by_ref(&mut file)
        .take(4096)
        .read_to_end(&mut bytes)
        .map_err(|source| TokenError::Io {
            path: path.to_owned(),
            source,
        })?;
    Ok(bytes)
}

fn first_token_line(bytes: &[u8]) -> (&[u8], bool) {
    let end = bytes.iter().position(|byte| *byte == b'\n');
    let has_more_lines = end.is_some_and(|end| end + 1 < bytes.len());
    let token = match end {
        Some(end) => &bytes[..end],
        None => bytes,
    };
    (token, has_more_lines)
}

/// Loads a token file and retains only the digest used for authentication.
///
/// Convenience alias for [`load`].
///
/// # Errors
///
/// Returns [`TokenError`] when the path is not absolute, the file is missing, has unsafe
/// permissions, is empty, or does not match the expected token shape.
pub fn load_token(path: &Path) -> Result<SecretToken, TokenError> {
    load(path)
}

fn open_token_file(path: &Path) -> Result<File, TokenError> {
    let link = fs::symlink_metadata(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            TokenError::Missing {
                path: path.to_owned(),
            }
        } else {
            TokenError::Io {
                path: path.to_owned(),
                source,
            }
        }
    })?;
    if link.file_type().is_symlink() {
        return Err(TokenError::Symlink {
            path: path.to_owned(),
        });
    }
    if !link.file_type().is_file() {
        return Err(TokenError::NotRegularFile {
            path: path.to_owned(),
        });
    }
    #[cfg(windows)]
    let file = win::open_for_read(path);
    #[cfg(not(windows))]
    let file = File::open(path);
    let file = file.map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            TokenError::Missing {
                path: path.to_owned(),
            }
        } else {
            TokenError::Io {
                path: path.to_owned(),
                source,
            }
        }
    })?;
    let opened = file.metadata().map_err(|source| TokenError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !opened.is_file() {
        return Err(TokenError::NotRegularFile {
            path: path.to_owned(),
        });
    }
    #[cfg(unix)]
    if !same_file(&link, &opened) {
        return Err(TokenError::Symlink {
            path: path.to_owned(),
        });
    }
    #[cfg(windows)]
    if !win::same_file(path, &file).map_err(|source| TokenError::Io {
        path: path.to_owned(),
        source,
    })? {
        return Err(TokenError::Symlink {
            path: path.to_owned(),
        });
    }
    #[cfg(unix)]
    check_connect_file_mode(path, &opened)?;
    #[cfg(windows)]
    if !win::owner_only(&file).map_err(|source| TokenError::Io {
        path: path.to_owned(),
        source,
    })? {
        return Err(TokenError::InsecureAcl {
            path: path.to_owned(),
        });
    }
    Ok(file)
}

#[cfg(unix)]
fn same_file(link: &fs::Metadata, opened: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    link.dev() == opened.dev() && link.ino() == opened.ino()
}

#[cfg(unix)]
fn check_connect_file_mode(path: &Path, metadata: &fs::Metadata) -> Result<(), TokenError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(TokenError::OpenToOtherUsers {
            path: path.to_owned(),
            mode,
        });
    }
    if mode != 0o600 {
        return Err(TokenError::ModeNot0600 {
            path: path.to_owned(),
            mode,
        });
    }
    Ok(())
}

fn ensure_absolute(path: &Path) -> Result<(), TokenError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(TokenError::RelativePath {
            path: path.to_owned(),
        })
    }
}

fn valid_token(token: &[u8]) -> bool {
    token.len() == 68
        && token.starts_with(b"dal_")
        && token[4..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn open_exclusive(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    let file = win::create_exclusive(path)?;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        options.mode(0o600);
        let file = options.open(path)?;
        if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
        file
    };
    Ok(file)
}

/// Windows carries no mode bits; the token file gets an explicit protected
/// DACL instead — the POSIX 0600 grant for the owner plus the privileged
/// system accounts, with inheritance cut so the grant set is exact. The
/// descriptor rides on the `CreateFile` call itself so the file never
/// exists with a wider inherited DACL.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "token file DACL requires Win32 security descriptor calls"
)]
mod win {
    use std::{
        ffi::c_void,
        fs::File,
        io,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle},
        },
        path::Path,
        ptr,
    };

    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
            LocalFree,
        },
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorControl,
            GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
            SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING,
        },
        System::{
            SystemServices::ACCESS_ALLOWED_ACE_TYPE,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The process user's SID as SDDL text — the identity a newly created
    /// file records as its owner, which is who the owner ACE must name.
    fn user_sid() -> io::Result<String> {
        let mut token: HANDLE = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        let mut needed = 0u32;
        unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &raw mut needed) };
        let mut buffer = vec![0u8; needed as usize];
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        };
        unsafe { CloseHandle(token) };
        if queried == FALSE {
            return Err(io::Error::last_os_error());
        }
        // The token buffer is only byte-aligned, so the header must be
        // read unaligned rather than reinterpreted as a `TOKEN_USER`.
        let sid: PSID = unsafe {
            buffer
                .as_ptr()
                .cast::<TOKEN_USER>()
                .read_unaligned()
                .User
                .Sid
        };
        sid_text(sid)
    }

    /// The Win32 API allocates SID text and security descriptors with LocalAlloc.
    /// Keep the allocation alive while its borrowed SID and ACE pointers are used.
    struct LocalAllocation(*mut c_void);

    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { LocalFree(self.0) };
            }
        }
    }

    fn sid_text(sid: PSID) -> io::Result<String> {
        let mut text = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        let _allocation = LocalAllocation(text.cast());
        let mut len = 0usize;
        while unsafe { *text.add(len) } != 0 {
            len += 1;
        }
        Ok(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, len)
        }))
    }

    /// Opens the token itself rather than following a replaced reparse point.
    /// The returned handle is also the handle checked for ACLs and read for the token.
    pub(super) fn open_for_read(path: &Path) -> io::Result<File> {
        let path_name = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>();
        let raw = unsafe {
            CreateFileW(
                path_name.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_handle(raw) })
    }

    /// Compares two independent non-following opens by stable file identity.
    /// A rename or reparse substitution between opens fails closed.
    pub(super) fn same_file(path: &Path, opened: &File) -> io::Result<bool> {
        let current = open_for_read(path)?;
        let current_info = file_information(&current)?;
        let opened_info = file_information(opened)?;
        Ok(current_info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && opened_info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && current_info.dwVolumeSerialNumber == opened_info.dwVolumeSerialNumber
            && current_info.nFileIndexHigh == opened_info.nFileIndexHigh
            && current_info.nFileIndexLow == opened_info.nFileIndexLow)
    }

    fn file_information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    }

    /// A token is safe only when its owner is the current user and its
    /// protected DACL grants access exclusively to that user, SYSTEM, or
    /// Administrators. Unknown and inherited ACEs fail closed.
    pub(super) fn owner_only(file: &File) -> io::Result<bool> {
        let mut owner: PSID = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let result = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut owner,
                ptr::null_mut(),
                &raw mut dacl,
                ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        let _allocation = LocalAllocation(descriptor.cast());
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        if owner.is_null() || dacl.is_null() || descriptor.is_null() {
            return Ok(false);
        }
        let mut control = 0;
        let mut revision = 0;
        if unsafe {
            GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision)
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        if control & SE_DACL_PROTECTED == 0 {
            return Ok(false);
        }
        let user = user_sid()?;
        if sid_text(owner)? != user {
            return Ok(false);
        }
        let mut info = ACL_SIZE_INFORMATION::default();
        if unsafe {
            GetAclInformation(
                dacl,
                (&raw mut info).cast(),
                u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(u32::MAX),
                AclSizeInformation,
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }

        let mut has_user_grant = false;
        for index in 0..info.AceCount {
            let mut raw_ace: *mut c_void = ptr::null_mut();
            if unsafe { GetAce(dacl, index, &raw mut raw_ace) } == FALSE {
                return Err(io::Error::last_os_error());
            }
            if raw_ace.is_null() {
                return Ok(false);
            }
            let header = unsafe { &*raw_ace.cast::<ACE_HEADER>() };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0 {
                return Ok(false);
            }
            let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
            let principal = sid_text((&raw const ace.SidStart).cast_mut().cast())?;
            if principal == user {
                has_user_grant = true;
            } else if principal != "S-1-5-18" && principal != "S-1-5-32-544" {
                return Ok(false);
            }
        }
        Ok(has_user_grant)
    }

    /// Creates `path` with `CREATE_NEW` semantics and the protected DACL
    /// attached, so the token file is born with the POSIX-0600-equivalent
    /// grant set instead of briefly carrying a wider inherited DACL. The
    /// descriptor must outlive the `CreateFileW` call, so it is created
    /// and dropped inside this function.
    pub(super) fn create_exclusive(path: &Path) -> io::Result<File> {
        let (attrs, _descriptor) = create_security_attributes()?;
        let wide_path = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>();
        let raw = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &raw const attrs,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_handle(raw) })
    }

    /// `SECURITY_ATTRIBUTES` carrying a protected DACL granting the creating
    /// user, SYSTEM, and Administrators — the Windows counterpart of
    /// `O_CREAT` with mode 0600. The returned descriptor backs the
    /// attributes and must outlive the open call that receives them.
    /// `P` marks the DACL protected so inherited ACEs cannot widen the
    /// grant; SY and BA are the SDDL aliases for SYSTEM and Administrators.
    fn create_security_attributes() -> io::Result<(SECURITY_ATTRIBUTES, LocalAllocation)> {
        let owner_sid = user_sid()?;
        let sddl = wide(&format!(
            "O:{owner_sid}D:P(A;;FA;;;{owner_sid})(A;;FA;;;SY)(A;;FA;;;BA)"
        ));
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        Ok((
            SECURITY_ATTRIBUTES {
                nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
                lpSecurityDescriptor: descriptor,
                bInheritHandle: FALSE,
            },
            LocalAllocation(descriptor.cast()),
        ))
    }
}

fn write_sync(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Windows has no directory-sync door; the file sync is the durable edge.
#[cfg_attr(
    windows,
    expect(
        clippy::unnecessary_wraps,
        reason = "directory sync fails only on POSIX"
    )
)]
fn sync_parent(path: &Path) -> Result<(), TokenError> {
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| TokenError::Io {
                path: parent.to_owned(),
                source,
            })
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::{
        fs::{self, File},
        process::Command,
    };

    use super::{TokenError, create, load, read_connect_token, win};

    fn icacls(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("icacls")
            .arg(path)
            .args(args)
            .output()
            .expect("run icacls");
        assert!(
            output.status.success(),
            "icacls failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn load_and_client_read_reject_acl_drift() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("serve.token");
        let token = create(&path, false).expect("create protected token");
        assert!(load(&path).expect("load protected token").matches(&token));
        assert_eq!(read_connect_token(&path).expect("read protected token"), token);

        icacls(&path, &["/grant", "*S-1-1-0:R"]);
        assert!(matches!(load(&path), Err(TokenError::InsecureAcl { .. })));
        assert!(matches!(read_connect_token(&path), Err(TokenError::InsecureAcl { .. })));
    }

    #[test]
    fn load_rejects_unprotected_acl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("serve.token");
        create(&path, false).expect("create protected token");
        icacls(&path, &["/inheritance:e"]);
        assert!(matches!(load(&path), Err(TokenError::InsecureAcl { .. })));
    }

    #[test]
    fn path_replacement_changes_file_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("serve.token");
        create(&path, false).expect("create original token");
        let original = win::open_for_read(&path).expect("open original");
        assert!(win::same_file(&path, &original).expect("same file"));

        fs::rename(&path, dir.path().join("old.token")).expect("rename token");
        create(&path, false).expect("create replacement");
        assert!(!win::same_file(&path, &original).expect("different file"));
    }
}
