//! Windows owner-only file and directory security.
//!
//! The descriptor is supplied to CreateFileW/CreateDirectoryW, before the
//! pathname becomes visible. Reads inspect the DACL and owner on the opened
//! handle. A widened, inherited, or unrecognized ACL fails closed.

use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
        ACCESS_ALLOWED_ACE, ACCESS_ALLOWED_ACE_TYPE, ACE_HEADER, ACL,
        DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorControl, GetTokenInformation,
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

use crate::util::FileMode;

/// Allocated by Advapi32, released through LocalFree on every exit path.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0.cast()) };
        }
    }
}

impl Descriptor {
    fn for_current_user() -> io::Result<Self> {
        let user = process_user_sid()?;
        // P prevents parent ACE inheritance. System and local Administrators
        // retain their customary recovery rights.
        let sddl = wide(&format!(
            "O:{user}D:P(A;;FA;;;{user})(A;;FA;;;SY)(A;;FA;;;BA)"
        ));
        let mut descriptor = ptr::null_mut();
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
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).expect("small ABI struct"),
            lpSecurityDescriptor: self.0,
            bInheritHandle: FALSE,
        }
    }
}

fn wide(path: &str) -> Vec<u16> {
    path.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn sid_text(sid: PSID) -> io::Result<String> {
    if sid.is_null() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "missing security SID"));
    }
    let mut text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0usize;
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    let result = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    unsafe { LocalFree(text.cast()) };
    Ok(result)
}

fn process_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut length = 0u32;
        unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &raw mut length) };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; length as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &raw mut length,
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        // TOKEN_USER is inside a byte buffer with no guaranteed alignment.
        let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read_unaligned() };
        sid_text(user.User.Sid)
    })();
    unsafe { CloseHandle(token) };
    result
}

pub(super) fn create_new(path: &Path, mode: FileMode) -> io::Result<File> {
    match mode {
        FileMode::Mode0600 => {}
    }
    let descriptor = Descriptor::for_current_user()?;
    let attrs = descriptor.attributes();
    let path = wide_path(path);
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &raw const attrs,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub(super) fn create_dir(path: &Path) -> io::Result<()> {
    let descriptor = Descriptor::for_current_user()?;
    let attrs = descriptor.attributes();
    let path = wide_path(path);
    if unsafe { CreateDirectoryW(path.as_ptr(), &raw const attrs) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn open_checked(path: &Path, write: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    if !file.metadata()?.is_file() || !is_private(&file)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file is not a regular owner-only file",
        ));
    }
    Ok(file)
}

pub(super) fn dir_is_private(path: &Path) -> io::Result<bool> {
    let path = wide_path(path);
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    Ok(file.metadata()?.is_dir() && is_private(&file)?)
}

/// Checks the effective security descriptor, not the pathname or parent ACL.
/// The only permitted ACEs are ordinary allow ACEs for the process user,
/// SYSTEM and BUILTIN\Administrators. Inherited and unknown ACEs are rejected.
pub(super) fn is_private(file: &File) -> io::Result<bool> {
    use std::os::windows::fs::MetadataExt;

    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(false);
    }
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut raw_descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut raw_descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ));
    }
    let descriptor = Descriptor(raw_descriptor);
    if owner.is_null() || dacl.is_null() {
        return Ok(false);
    }
    let user = process_user_sid()?;
    if sid_text(owner)? != user {
        return Ok(false);
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &raw mut control, &raw mut revision) }
        == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Ok(false);
    }
    let acl = unsafe { &*dacl };
    if acl.AceCount == 0 {
        return Ok(false);
    }
    for index in 0..u32::from(acl.AceCount) {
        let mut ace = ptr::null_mut();
        if unsafe { GetAce(dacl, index, &raw mut ace) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || header.AceFlags != 0
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Ok(false);
        }
        let entry = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = ptr::addr_of!(entry.SidStart).cast_mut().cast();
        let trustee = sid_text(sid)?;
        if trustee != user && trustee != "S-1-5-18" && trustee != "S-1-5-32-544" {
            return Ok(false);
        }
    }
    Ok(true)
}
