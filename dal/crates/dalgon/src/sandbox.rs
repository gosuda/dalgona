use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;
use std::process::ExitCode;
use std::sync::Arc;

use dal_agent::ext::{BoxFuture, Extension, ExtensionBuilder, HookCx, HookError, ObserveHook};
use dal_core::ext::SessionStart;
use dal_core::{Notice, RegistrationError, ServiceSet};

#[cfg(all(test, target_os = "linux"))]
const DENIAL_NOTE: &str = "dalgon sandbox: a \"Permission denied\" or \"Operation not permitted\" error can come from the sandbox; if the path should be writable, add it to sandbox_writable in dal.toml.";
#[expect(dead_code, reason = "kept for the SDK embedder seam")]
const HELPER_ERROR: &str = "sandbox: no sandbox helper. SDK embedders must pass a helper path; the dalgon binary provides dalgon __sandbox.";
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
const WINDOWS_ERROR: &str =
    "sandbox = \"on\" is not supported on this platform. Set sandbox = \"off\" in dal.toml.";
const MALFORMED_ARGS: &str = "dalgon sandbox: malformed launcher arguments";

#[non_exhaustive]
#[derive(Debug)]
#[expect(dead_code, reason = "helper-path errors exist for SDK embedders")]
pub(crate) enum SandboxError {
    HelperPath(io::Error),
    Landlock(String),
    Seatbelt(String),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HelperPath(_) => formatter.write_str(HELPER_ERROR),
            Self::Landlock(message) | Self::Seatbelt(message) => {
                write!(formatter, "sandbox: {message}")
            }
        }
    }
}

impl std::error::Error for SandboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HelperPath(error) => Some(error),
            Self::Landlock(_) | Self::Seatbelt(_) => None,
        }
    }
}

#[expect(
    dead_code,
    reason = "SDK embedders resolve the helper; the binary passes dalgon __sandbox"
)]
pub(crate) fn helper_path() -> Result<PathBuf, SandboxError> {
    std::env::current_exe().map_err(SandboxError::HelperPath)
}

/// Builds the `sandbox` extension: an observe-only session-start hook that
/// publishes the canonical writable-roots notice when `sandbox = "on"`. The
/// launcher itself lives in the session backend, so removing this extension
/// never changes what commands may write.
///
/// # Errors
/// Returns [`RegistrationError`] when the builder rejects registration.
pub(crate) fn extension(
    on: bool,
    writable: Arc<[Box<str>]>,
    protected: Arc<[PathBuf]>,
) -> Result<Extension, RegistrationError> {
    ExtensionBuilder::new("sandbox", env!("CARGO_PKG_VERSION"), ServiceSet::EMPTY)?
        .on_session_start(NoticeHook {
            on,
            writable,
            protected,
        })
        .build()
}

/// Emits `Sandbox on. Commands can write only under: <roots>.` at session
/// start; silent when the sandbox is off or roots cannot resolve.
struct NoticeHook {
    on: bool,
    writable: Arc<[Box<str>]>,
    protected: Arc<[PathBuf]>,
}

impl ObserveHook<SessionStart> for NoticeHook {
    fn call(&self, input: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let on = self.on;
        let writable = Arc::clone(&self.writable);
        let protected = Arc::clone(&self.protected);
        Box::pin(async move {
            let env = cx.process_env();
            if let Some(text) = dal_agent::sandbox_notice(
                on,
                &env.vars,
                input.workspace.as_path(),
                &writable,
                &protected,
            ) {
                cx.services.notify(
                    &cx.caller,
                    Notice {
                        turn: None,
                        kind: "sandbox".into(),
                        text,
                    },
                );
            }
            Ok(())
        })
    }
}

pub(crate) fn run(argv: &[OsString]) -> ExitCode {
    if argv.get(1).map(OsString::as_os_str) != Some(OsStr::new("__sandbox")) {
        return malformed();
    }

    #[cfg(target_os = "linux")]
    {
        run_linux(argv)
    }

    #[cfg(target_os = "macos")]
    {
        run_macos(argv)
    }

    #[cfg(windows)]
    {
        run_windows(argv)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        write_error(WINDOWS_ERROR);
        ExitCode::from(126)
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn denial_note(output: &str) -> Option<&'static str> {
    (output.contains("Permission denied") || output.contains("Operation not permitted"))
        .then_some(DENIAL_NOTE)
}

fn malformed() -> ExitCode {
    write_error(MALFORMED_ARGS);
    ExitCode::from(126)
}

fn write_error(message: &str) {
    let _ = writeln!(io::stderr().lock(), "{message}");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exec_error(executable: &OsStr, error: &io::Error) -> ExitCode {
    let name = executable.to_string_lossy();
    write_error(&format!("dalgon sandbox: cannot run {name}: {error}"));
    if error.kind() == io::ErrorKind::PermissionDenied {
        ExitCode::from(126)
    } else {
        ExitCode::from(127)
    }
}

#[cfg(target_os = "linux")]
fn run_linux(argv: &[OsString]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    match argv.get(2).map(OsString::as_os_str) {
        Some(mode) if mode == OsStr::new("--probe") => {
            if argv.len() != 3 {
                return malformed();
            }
            let abi = linux_abi();
            if writeln!(io::stdout().lock(), "{abi}").is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Some(mode) if mode == OsStr::new("--allow") => {
            let Some((roots, executable, run_args)) = parse_allow_args(argv) else {
                return malformed();
            };
            let abi = linux_abi();
            if abi < 3 {
                write_error(&format!(
                    "dalgon sandbox: sandbox = \"on\" needs Landlock ABI 3 (Linux 6.1 or newer); this kernel reports ABI {abi}."
                ));
                return ExitCode::from(126);
            }
            if let Err(error) = apply_landlock(&roots) {
                write_error(&format!("dalgon sandbox: {error}"));
                return ExitCode::from(126);
            }

            // Replace the trampoline so the target retains the launcher's PID and process group.
            #[expect(
                clippy::disallowed_methods,
                reason = "R4 edge: the sandbox helper execs the target in place, replacing the trampoline process"
            )]
            let error = Command::new(executable).args(run_args).exec();
            exec_error(executable, &error)
        }
        _ => malformed(),
    }
}

#[cfg(any(target_os = "linux", windows))]
fn parse_allow_args(argv: &[OsString]) -> Option<(Vec<PathBuf>, &OsStr, &[OsString])> {
    let mut index = 3;
    let mut roots = Vec::new();
    while let Some(arg) = argv.get(index) {
        if arg == OsStr::new("--") {
            break;
        }
        roots.push(PathBuf::from(arg));
        index += 1;
    }
    if roots.is_empty()
        || roots.iter().any(|root| !root.is_absolute())
        || argv.get(index).map(OsString::as_os_str) != Some(OsStr::new("--"))
    {
        return None;
    }
    let executable = argv.get(index + 1)?;
    if executable.is_empty() {
        return None;
    }
    Some((roots, executable, &argv[index + 2..]))
}

#[cfg(target_os = "linux")]
fn linux_abi() -> u32 {
    use landlock::{ABI, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr};

    for (version, abi) in [
        (9, ABI::V9),
        (8, ABI::V8),
        (7, ABI::V7),
        (6, ABI::V6),
        (5, ABI::V5),
        (4, ABI::V4),
        (3, ABI::V3),
        (2, ABI::V2),
        (1, ABI::V1),
    ] {
        let probe = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_write(abi))
            .and_then(Ruleset::create);
        if probe.is_ok() {
            return version;
        }
    }
    0
}

#[cfg(target_os = "linux")]
fn apply_landlock(roots: &[PathBuf]) -> Result<(), String> {
    use landlock::{
        ABI, AccessFs, CompatLevel, Compatible, LandlockStatus, PathBeneath, PathFd, Ruleset,
        RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    };

    let access = AccessFs::from_write(ABI::V3);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(access)
        .and_then(Ruleset::create)
        .map_err(|error| error.to_string())?;

    for root in roots {
        let fd = PathFd::new(root).map_err(|error| error.to_string())?;
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, access))
            .map_err(|error| error.to_string())?;
    }

    let status = ruleset
        .no_new_privs(true)
        .restrict_self()
        .map_err(|error| error.to_string())?;
    if status.ruleset != RulesetStatus::FullyEnforced
        || !status.no_new_privs
        || !matches!(
            status.landlock,
            LandlockStatus::Available { effective_abi, .. } if effective_abi >= ABI::V3
        )
    {
        return Err("Landlock ruleset is not fully enforced".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_macos(argv: &[OsString]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    if argv.get(2).map(OsString::as_os_str) != Some(OsStr::new("--profile")) {
        return malformed();
    }
    let Some(profile) = argv.get(3) else {
        return malformed();
    };
    if argv.get(4).map(OsString::as_os_str) != Some(OsStr::new("--")) {
        return malformed();
    }
    let Some(executable) = argv.get(5) else {
        return malformed();
    };
    if executable.is_empty() {
        return malformed();
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "R4 edge: the sandbox helper execs through the macOS Seatbelt front end in place"
    )]
    let error = Command::new("/usr/bin/sandbox-exec")
        .arg("-f")
        .arg(profile)
        .arg("--")
        .arg(executable)
        .args(&argv[6..])
        .exec();
    exec_error(executable, &error)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn denial_note_matches_permission_denied() {
        assert_eq!(
            denial_note("write failed: Permission denied"),
            Some(DENIAL_NOTE)
        );
    }

    #[test]
    fn denial_note_matches_operation_not_permitted() {
        assert_eq!(denial_note("Operation not permitted"), Some(DENIAL_NOTE));
    }

    #[test]
    fn denial_note_ignores_unrelated_output() {
        assert_eq!(denial_note("permission mismatch"), None);
        assert_eq!(denial_note(""), None);
    }

    #[test]
    fn landlock_denies_a_sibling_path_and_allows_its_root() -> io::Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let executable = std::env::current_exe()?;
        let parent = executable
            .parent()
            .ok_or_else(|| io::Error::other("test executable has no parent"))?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let base = parent.join(format!("sandbox-test-{}-{nonce}", std::process::id()));
        let allowed = base.join("allowed");
        let denied = base.join("denied");
        let result = (|| {
            std::fs::create_dir_all(&allowed)?;
            std::fs::create_dir_all(&denied)?;
            let denied_file = denied.join("probe");
            std::fs::write(&denied_file, b"baseline")?;
            std::fs::remove_file(&denied_file)?;

            let allowed = std::fs::canonicalize(allowed)?;
            let thread = std::thread::spawn(move || -> io::Result<()> {
                apply_landlock(std::slice::from_ref(&allowed)).map_err(io::Error::other)?;

                let inside = allowed.join("inside");
                std::fs::write(&inside, b"allowed")?;
                if std::fs::read(&inside)?.as_slice() != b"allowed" {
                    return Err(io::Error::other("write under the Landlock root failed"));
                }

                match std::fs::write(&denied_file, b"blocked") {
                    Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(()),
                    Err(error) => Err(error),
                    Ok(()) => Err(io::Error::other(
                        "write outside the Landlock root succeeded",
                    )),
                }
            });
            match thread.join() {
                Ok(result) => result,
                Err(_) => Err(io::Error::other("Landlock test thread panicked")),
            }
        })();
        let cleanup = std::fs::remove_dir_all(base);
        result?;
        cleanup?;
        Ok(())
    }

    #[test]
    fn parse_allow_args_requires_root_delimiter_and_executable() {
        let argv = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(parse_allow_args(&argv(&["dalgon", "__sandbox", "--allow"])).is_none());
        assert!(parse_allow_args(&argv(&["dalgon", "__sandbox", "--allow", "/tmp"])).is_none());
        assert!(
            parse_allow_args(&argv(&["dalgon", "__sandbox", "--allow", "/tmp", "--"])).is_none()
        );
        assert!(
            parse_allow_args(&argv(&["dalgon", "__sandbox", "--allow", "/tmp", "--", ""]))
                .is_none()
        );
        assert!(
            parse_allow_args(&argv(&[
                "dalgon",
                "__sandbox",
                "--allow",
                "relative",
                "--",
                "/bin/true"
            ]))
            .is_none()
        );
    }
}

/// Windows backend: an AppContainer launch inside the kill-on-close job scope
/// the parent's process wrap already provides. The container profile grants
/// the network capabilities the plan leaves unrestricted; filesystem writes
/// are allowed only where this run planted an explicit DACL grant.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "R4 edge: the Windows sandbox owns the Win32 launch"
)]
mod win {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, REVOKE_ACCESS, SE_FILE_OBJECT,
        SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_NAME, TRUSTEE_IS_SID, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::DeriveCapabilitySidsFromName;
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeleteAppContainerProfile,
        DeriveAppContainerSidFromAppContainerName,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, FreeSid, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    };
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
    use windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED;
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
        STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };
    use windows_sys::core::PWSTR;

    const GENERIC_READ_EXECUTE: u32 = 0xA2000000; // GENERIC_READ | GENERIC_EXECUTE
    const GENERIC_ALL_ACCESS: u32 = 0x10000000; // GENERIC_ALL
    const PROFILE_NAME: &str = "dalgon.sandbox";
    const STILL_ACTIVE: u32 = 259;

    struct OwnedSid(PSID);
    impl Drop for OwnedSid {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { FreeSid(self.0) };
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text).encode_wide().chain(Some(0)).collect()
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn last_error(what: &str) -> String {
        format!("dalgon sandbox: {what} failed ({}).", unsafe {
            GetLastError()
        })
    }

    /// Adds or removes the container SID's ACE on one path; inheritance is
    /// `(OI)(CI)` so one ACE covers the subtree.
    fn edit_dacl(path: &Path, sid: PSID, access: u32, mode: i32) -> Result<(), String> {
        let wide_path = wide_path(path);
        let mut trustee: TRUSTEE_W = unsafe { std::mem::zeroed() };
        trustee.TrusteeForm = TRUSTEE_IS_SID;
        trustee.TrusteeType = TRUSTEE_IS_NAME;
        trustee.ptstrName = sid as PWSTR;
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: access,
            grfAccessMode: mode,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: trustee,
        };
        let mut old_dacl: *mut ACL = ptr::null_mut();
        let mut sd = ptr::null_mut();
        let read = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut old_dacl,
                ptr::null_mut(),
                &mut sd,
            )
        };
        if read != 0 {
            return Err(last_error(&format!("read ACL on {}", path.display())));
        }
        let mut new_dacl: *mut ACL = ptr::null_mut();
        let merge = unsafe { SetEntriesInAclW(1, &entry, old_dacl, &mut new_dacl) };
        if !sd.is_null() {
            unsafe { LocalFree(sd) };
        }
        if merge != 0 {
            return Err(last_error(&format!("build ACL for {}", path.display())));
        }
        let write = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                new_dacl,
                ptr::null_mut(),
            )
        };
        unsafe { LocalFree(new_dacl.cast()) };
        if write != 0 {
            return Err(last_error(&format!("write ACL on {}", path.display())));
        }
        Ok(())
    }

    /// Every mounted drive root so the container can read and exec system
    /// binaries; writes stay denied outside the allowed roots.
    fn drive_roots() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        let mask = unsafe { GetLogicalDrives() };
        for index in 0..26u32 {
            if mask & (1 << index) != 0 {
                roots.push(PathBuf::from(format!(
                    "{}:\\",
                    (b'A' + index as u8) as char
                )));
            }
        }
        roots
    }

    /// Plants the container grants: read+execute on every drive root, then
    /// full access on each allowed root. On failure every planted ACE is
    /// lifted before the error returns.
    fn plant_grants(roots: &[PathBuf], sid: PSID) -> Result<Vec<(PathBuf, u32)>, String> {
        let mut planted: Vec<(PathBuf, u32)> = Vec::new();
        let result = (|| {
            for root in drive_roots() {
                edit_dacl(&root, sid, GENERIC_READ_EXECUTE, GRANT_ACCESS)?;
                planted.push((root, GENERIC_READ_EXECUTE));
            }
            for root in roots {
                edit_dacl(root, sid, GENERIC_ALL_ACCESS, GRANT_ACCESS)?;
                planted.push((root.clone(), GENERIC_ALL_ACCESS));
            }
            Ok(())
        })();
        if result.is_err() {
            revoke_planted(&planted, sid);
        }
        result.map(|()| planted)
    }

    fn revoke_planted(planted: &[(PathBuf, u32)], sid: PSID) {
        for (path, access) in planted {
            let _ = edit_dacl(path, sid, *access, REVOKE_ACCESS);
        }
    }

    /// Resolves or registers the dalgon AppContainer profile and returns its
    /// SID plus the unrestricted network capability SIDs.
    fn container_sid() -> Result<(OwnedSid, Vec<SID_AND_ATTRIBUTES>), String> {
        let name = wide(PROFILE_NAME);
        let mut sid: PSID = ptr::null_mut();
        let profile = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                ptr::null(),
                0,
                &mut sid,
            )
        };
        if profile < 0 || sid.is_null() {
            let derive =
                unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
            if derive < 0 || sid.is_null() {
                return Err(format!(
                    "dalgon sandbox: cannot resolve the AppContainer profile (0x{profile:08x})."
                ));
            }
        }
        let mut capabilities = Vec::new();
        for capability in [
            "internetClient",
            "internetClientServer",
            "privateNetworkClientServer",
        ] {
            let capability = wide(capability);
            let mut sids: *mut PSID = ptr::null_mut();
            let mut count = 0u32;
            let mut group_sids: *mut PSID = ptr::null_mut();
            let mut group_count = 0u32;
            let ok = unsafe {
                DeriveCapabilitySidsFromName(
                    capability.as_ptr(),
                    &mut group_sids,
                    &mut group_count,
                    &mut sids,
                    &mut count,
                )
            };
            if ok == FALSE || count == 0 {
                return Err(
                    "dalgon sandbox: cannot resolve the network capability SIDs.".to_string(),
                );
            }
            for index in 0..count as isize {
                capabilities.push(SID_AND_ATTRIBUTES {
                    Sid: unsafe { *sids.offset(index) },
                    Attributes: SE_GROUP_ENABLED as u32,
                });
            }
        }
        Ok((OwnedSid(sid), capabilities))
    }

    /// Spawns the target inside the AppContainer and waits for it, returning
    /// the child's exit code. Every planted ACE is lifted on return.
    ///
    /// # Safety
    /// Win32 process launch; handles and the attribute list are closed on
    /// every exit path.
    pub(super) fn spawn(
        roots: &[PathBuf],
        executable: &OsStr,
        run_args: &[OsString],
    ) -> Result<ExitCode, String> {
        let (sid, capabilities) = container_sid()?;
        let planted = plant_grants(roots, sid.0)?;

        let mut list_size = 0usize;
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut list_size) };
        if list_size == 0 {
            revoke_planted(&planted, sid.0);
            return Err(last_error("size the attribute list"));
        }
        let mut buffer = vec![0u8; list_size];
        let list: LPPROC_THREAD_ATTRIBUTE_LIST = buffer.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut list_size) } == FALSE {
            revoke_planted(&planted, sid.0);
            return Err(last_error("initialize the attribute list"));
        }
        let mut security = SECURITY_CAPABILITIES {
            AppContainerSid: sid.0,
            Capabilities: capabilities.as_ptr() as *mut SID_AND_ATTRIBUTES,
            CapabilityCount: capabilities.len() as u32,
            Reserved: 0,
        };
        let attribute = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                ptr::addr_of_mut!(security).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if attribute == FALSE {
            unsafe { DeleteProcThreadAttributeList(list) };
            revoke_planted(&planted, sid.0);
            return Err(last_error("set the security capability attribute"));
        }

        let mut command = wide_path(Path::new(executable));
        command.pop();
        for arg in run_args {
            command.extend(OsStr::new(" ").encode_wide());
            command.extend(arg.encode_wide());
        }
        command.push(0);

        let mut info: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        info.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        info.lpAttributeList = list;
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let spawned = unsafe {
            CreateProcessW(
                ptr::null(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                FALSE,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                ptr::null(),
                ptr::null(),
                &mut info.StartupInfo,
                &mut process,
            )
        };
        unsafe { DeleteProcThreadAttributeList(list) };
        if spawned == FALSE {
            revoke_planted(&planted, sid.0);
            return Err(last_error("spawn the sandboxed process"));
        }

        unsafe { ResumeThread(process.hThread) };
        unsafe { WaitForSingleObject(process.hProcess, INFINITE) };
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(process.hProcess, &mut code) };
        if code == STILL_ACTIVE {
            unsafe { TerminateProcess(process.hProcess, 1) };
            code = 1;
        }
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        };
        revoke_planted(&planted, sid.0);
        let _ = unsafe { DeleteAppContainerProfile(wide(PROFILE_NAME).as_ptr()) };
        Ok(ExitCode::from((code & 0xFF) as u8))
    }
}

#[cfg(windows)]
fn run_windows(argv: &[OsString]) -> ExitCode {
    let Some((roots, executable, run_args)) = parse_allow_args(argv) else {
        return malformed();
    };
    match win::spawn(&roots, executable, run_args) {
        Ok(code) => code,
        Err(message) => {
            write_error(&message);
            ExitCode::from(126)
        }
    }
}
