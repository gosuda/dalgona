//! Windows owner-only files and directories. Every newly created object receives
//! its DACL in the create call, before its name becomes visible.

use std::{
    ffi::c_void,
    fs::{self, File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    ptr,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, GetTokenInformation, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_FILE_OBJECT, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);

    let mut needed = 0u32;
    unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // The returned TOKEN_USER contains a pointer into this allocation. Keep
    // the storage live until the SID is converted into owned text.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    } == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read() };
    sid_text(user.User.Sid)
}

fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    let text_guard = LocalMemory(text.cast());
    let mut length = 0usize;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let output = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    drop(text_guard);
    output
}

fn descriptor(directory: bool) -> io::Result<LocalMemory> {
    let user = user_sid()?;
    let inherit = if directory { "OICI" } else { "" };
    // No inherited ACEs. A protected DACL allows only the user, SYSTEM, and
    // Administrators, matching the existing serve-token Windows policy.
    let sddl = format!(
        "D:P(A;{inherit};FA;;;{user})(A;{inherit};FA;;;SY)(A;{inherit};FA;;;BA)"
    );
    let string = sddl.encode_utf16().chain(std::iter::once(0)).collect::<Vec<_>>();
    let mut raw: PSECURITY_DESCRIPTOR = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string.as_ptr(),
            1,
            &raw mut raw,
            ptr::null_mut(),
        )
    } == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalMemory(raw))
}

fn security_attributes(descriptor: &LocalMemory) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: FALSE,
    }
}

pub(super) fn create_new(path: &Path) -> io::Result<File> {
    let descriptor = descriptor(false)?;
    let attributes = security_attributes(&descriptor);
    let raw = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &raw const attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(raw) })
}

/// Creates the lock file securely, or validates its existing ACL before reuse.
pub(super) fn open_lock(path: &Path) -> io::Result<File> {
    let descriptor = descriptor(false)?;
    let attributes = security_attributes(&descriptor);
    let raw = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &raw const attributes,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(raw) };
    super::require_private(&file)?;
    Ok(file)
}

pub(super) fn open(path: &Path, write: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    super::require_private(&file)?;
    Ok(file)
}

fn dacl_sddl(descriptor: PSECURITY_DESCRIPTOR) -> io::Result<String> {
    let mut text = ptr::null_mut();
    let mut length = 0u32;
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            1,
            DACL_SECURITY_INFORMATION,
            &raw mut text,
            &raw mut length,
        )
    } == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    let text_guard = LocalMemory(text.cast());
    let output = String::from_utf16(unsafe {
        std::slice::from_raw_parts(text, length as usize)
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    drop(text_guard);
    output
}

/// Validates the opened handle rather than a path which may be replaced.
/// Rejects permissive, inheritable, unexpected, and reparse-point ACLs.
fn check_dacl(file: &File, directory: bool) -> io::Result<bool> {
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || metadata.is_dir() != directory
    {
        return Ok(false);
    }

    let expected = descriptor(directory)?;
    let mut owner: PSID = ptr::null_mut();
    let mut actual: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut actual,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let actual_guard = LocalMemory(actual);
    // DACL ownership matters: a third-party owner could change a currently
    // private DACL after the check. Elevated token owners may be BA or SY.
    if owner.is_null() {
        return Ok(false);
    }
    let owner_text = sid_text(owner)?;
    let current_user = user_sid()?;
    if owner_text != current_user
        && owner_text != "S-1-5-18"
        && owner_text != "S-1-5-32-544"
    {
        return Ok(false);
    }
    let same = dacl_sddl(expected.0)? == dacl_sddl(actual_guard.0)?;
    Ok(same)
}

pub(super) fn file_is_private(file: &File) -> io::Result<bool> {
    check_dacl(file, false)
}

fn directory_is_private(path: &Path) -> io::Result<bool> {
    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let directory = options.open(path)?;
    check_dacl(&directory, true)
}

fn create_directory(path: &Path) -> io::Result<()> {
    let descriptor = descriptor(true)?;
    let attributes = security_attributes(&descriptor);
    if unsafe { CreateDirectoryW(wide(path).as_ptr(), &raw const attributes) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut missing = Vec::<PathBuf>::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) => {
                if !metadata.is_dir()
                    || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "private directory path contains a reparse point or non-directory",
                    ));
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_owned());
                current = current.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "directory has no parent")
                })?;
            }
            Err(error) => return Err(error),
        }
    }
    for directory in missing.into_iter().rev() {
        match create_directory(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if !directory_is_private(&directory)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private directory has an unsafe Windows ACL",
            ));
        }
    }
    // An existing target is not silently repaired. Changing its ACL should
    // be a distinct, deliberate migration, never a side effect of a read.
    if !directory_is_private(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private directory has an unsafe Windows ACL",
        ));
    }
    Ok(())
}
