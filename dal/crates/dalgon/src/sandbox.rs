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

/// Windows backend. Each run registers a GUID-unique `AppContainer` profile,
/// grants its SID `(OI)(CI)` access on the allowed roots and execute-only on
/// the executable's directory and `PATH` dirs (system locations already grant
/// `ALL APPLICATION PACKAGES`), then launches the target under the container
/// inside its own kill-on-close job object. The planted ACEs are lifted on
/// return; residue left by an abrupt kill is inert because the SID is unique
/// to this run. Unlike the write-only Landlock model, an `AppContainer` also
/// denies reads outside the granted roots.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "process edge: no safe crate exposes AppContainer launch or DACL editing"
)]
mod win {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::ptr;

    use windows_sys::Win32::Foundation::{
        CloseHandle, FALSE, GetLastError, LocalFree, WAIT_OBJECT_0,
    };
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
    use windows_sys::Win32::System::Com::CoCreateGuid;
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED;
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };

    const GENERIC_EXECUTE_ONLY: u32 = 0x2000_0000; // GENERIC_EXECUTE (FILE_EXECUTE|FILE_TRAVERSE)
    const GENERIC_ALL_ACCESS: u32 = 0x1000_0000; // GENERIC_ALL

    struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);
    impl OwnedHandle {
        fn new(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<Self, String> {
            if handle.is_null() || handle == -1isize as _ {
                Err(last_error("open handle"))
            } else {
                Ok(Self(handle))
            }
        }
    }
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    struct OwnedSid(PSID);
    impl Drop for OwnedSid {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { FreeSid(self.0) };
            }
        }
    }

    /// The `AppContainer` profile for this run. Name-keyed derivation gives each
    /// run a unique SID, so planted grants can never cross runs.
    struct Profile {
        sid: OwnedSid,
        name: Vec<u16>,
    }
    impl Drop for Profile {
        fn drop(&mut self) {
            let _ = unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
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

    /// Appends one arg to the command line under `CommandLineToArgvW` rules:
    /// wrap in quotes when it is empty or holds space/tab/quote, double
    /// backslashes before a quote, escape inner quotes.
    fn push_quoted(command: &mut Vec<u16>, arg: &OsStr) {
        let mut encoded: Vec<u16> = arg.encode_wide().collect();
        if encoded.last() == Some(&0) {
            encoded.pop();
        }
        let needs_quotes = encoded.is_empty()
            || encoded.iter().any(|unit| {
                *unit == u16::from(b' ') || *unit == u16::from(b'\t') || *unit == u16::from(b'"')
            });
        if !needs_quotes {
            command.extend(encoded);
            return;
        }
        command.push(u16::from(b'"'));
        let mut backslashes = 0usize;
        for unit in encoded {
            if unit == u16::from(b'\\') {
                backslashes += 1;
            } else {
                backslashes = 0;
            }
            if unit == u16::from(b'"') {
                command.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
            }
            command.push(unit);
        }
        let tail: usize = command
            .iter()
            .rev()
            .take_while(|unit| **unit == u16::from(b'\\'))
            .count();
        let quote_tail = tail;
        for _ in 0..quote_tail {
            command.push(u16::from(b'\\'));
        }
        command.push(u16::from(b'"'));
    }

    /// Adds or removes the container SID's ACE on one path; `(OI)(CI)` covers
    /// the subtree. Callers must only edit DACLs the current user can write.
    fn edit_dacl(path: &Path, sid: PSID, access: u32, mode: i32) -> Result<(), String> {
        let wide_path = wide_path(path);
        let mut trustee: TRUSTEE_W = unsafe { std::mem::zeroed() };
        trustee.TrusteeForm = TRUSTEE_IS_SID;
        trustee.TrusteeType = TRUSTEE_IS_NAME;
        trustee.ptstrName = sid.cast();
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
                &raw mut old_dacl,
                ptr::null_mut(),
                &raw mut sd,
            )
        };
        if read != 0 {
            return Err(last_error(&format!("read ACL on {}", path.display())));
        }
        let mut new_dacl: *mut ACL = ptr::null_mut();
        let merge = unsafe { SetEntriesInAclW(1, &raw const entry, old_dacl, &raw mut new_dacl) };
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

    /// Grants the run-unique SID full access on each allowed root and
    /// execute-only on the executable's directory plus every PATH directory so
    /// the command runtime (outside the `ALL APPLICATION PACKAGES` grants)
    /// still loads. Runtime dirs grant `FILE_EXECUTE`, never `FILE_READ_DATA`:
    /// image loading needs execute rights only, so private files in those
    /// directories stay unreadable — consistent with the deny-read model.
    #[expect(
        clippy::disallowed_methods,
        reason = "the process edge owns environment reads; PATH decides the runtime dirs"
    )]
    fn plant_grants(
        roots: &[PathBuf],
        executable: &Path,
        sid: PSID,
    ) -> Result<Vec<(PathBuf, u32)>, String> {
        let mut planted: Vec<(PathBuf, u32)> = Vec::new();
        let result = (|| {
            if let Some(dir) = executable
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
            {
                // Execute on the launch directory only. System locations
                // already permit `ALL APPLICATION PACKAGES` and an unprivileged
                // user cannot edit them; a directory the container genuinely
                // cannot read fails at exec time with an access error, so a
                // failed grant here must not block launch.
                if edit_dacl(dir, sid, GENERIC_EXECUTE_ONLY, GRANT_ACCESS).is_ok() {
                    planted.push((dir.to_path_buf(), GENERIC_EXECUTE_ONLY));
                }
            }
            // Execute on every PATH directory so the shell can reach its
            // runtime (`cargo` under %USERPROFILE%\.cargo\bin, Git's usr\bin).
            // Same tolerate rule as the launch directory: the OS decides.
            if let Some(paths) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&paths) {
                    if dir.as_os_str().is_empty() {
                        continue;
                    }
                    if edit_dacl(&dir, sid, GENERIC_EXECUTE_ONLY, GRANT_ACCESS).is_ok() {
                        planted.push((dir, GENERIC_EXECUTE_ONLY));
                    }
                }
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

    /// Lifts every planted ACE; failures are collected instead of discarded so
    /// the run reports grants it could not remove.
    fn revoke_planted(planted: &[(PathBuf, u32)], sid: PSID) -> Option<String> {
        let mut failed = Vec::new();
        for (path, access) in planted {
            if let Err(error) = edit_dacl(path, sid, *access, REVOKE_ACCESS) {
                failed.push(error);
            }
        }
        if failed.is_empty() {
            None
        } else {
            Some(format!(
                "dalgon sandbox: could not lift {} ACE grant(s): {}",
                failed.len(),
                failed.join("; ")
            ))
        }
    }

    /// A run identity Windows cannot recycle: a fresh GUID, not a `PID` —
    /// killed helpers leave profiles and ACEs that a recycled PID would revive.
    /// Fails closed when the GUID cannot be minted; a weak identity would let
    /// two runs share planted grants.
    fn run_id() -> Result<String, String> {
        let mut guid = unsafe { std::mem::zeroed() };
        if unsafe { CoCreateGuid(&raw mut guid) } < 0 {
            return Err(last_error("mint the sandbox run identity"));
        }
        let guid: windows_sys::core::GUID = guid;
        Ok(format!(
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            guid.data1,
            guid.data2,
            guid.data3,
            guid.data4[0],
            guid.data4[1],
            guid.data4[2],
            guid.data4[3],
            guid.data4[4],
            guid.data4[5],
            guid.data4[6],
            guid.data4[7]
        ))
    }

    /// Resolves or registers this run's `AppContainer` profile and returns its
    /// SID plus the network capability SIDs the sandbox does not restrict.
    fn container() -> Result<(Profile, Vec<SID_AND_ATTRIBUTES>), String> {
        let name = wide(&format!("dalgon.sandbox.{}", run_id()?));
        let mut sid: PSID = ptr::null_mut();
        let profile = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                ptr::null(),
                0,
                &raw mut sid,
            )
        };
        if profile < 0 || sid.is_null() {
            let derive =
                unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &raw mut sid) };
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
                    &raw mut group_sids,
                    &raw mut group_count,
                    &raw mut sids,
                    &raw mut count,
                )
            };
            if ok == FALSE || count == 0 {
                return Err(
                    "dalgon sandbox: cannot resolve the network capability SIDs.".to_string(),
                );
            }
            for index in
                0..isize::try_from(count).map_err(|_| last_error("index the capability sids"))?
            {
                capabilities.push(SID_AND_ATTRIBUTES {
                    Sid: unsafe { *sids.offset(index) },
                    Attributes: SE_GROUP_ENABLED.cast_unsigned(),
                });
            }
        }
        Ok((
            Profile {
                sid: OwnedSid(sid),
                name,
            },
            capabilities,
        ))
    }

    /// A new kill-on-close job object; the child joins before it resumes, so
    /// any helper death — normal exit, cancel kill, crash — kills the child.
    fn kill_on_close_job() -> Result<OwnedHandle, String> {
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        let job = OwnedHandle::new(job)?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                ptr::addr_of_mut!(limits).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .map_err(|_| last_error("size the job limits"))?,
            )
        };
        if set == FALSE {
            return Err(last_error("set the job kill-on-close limit"));
        }
        Ok(job)
    }

    /// Spawns the target inside this run's `AppContainer` and returns its exit
    /// code. stdio is inherited through the three parent standard handles only.
    ///
    /// Owning proc-thread attribute list. `DeleteProcThreadAttributeList`
    /// runs on drop, so launch exits cannot leak the list.
    struct AttrList(Vec<u8>);

    impl AttrList {
        fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.0.as_ptr().cast_mut().cast()
        }
    }

    impl Drop for AttrList {
        fn drop(&mut self) {
            if !self.0.is_empty() {
                unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
            }
        }
    }

    /// Builds the proc-thread attribute list: the container security
    /// capabilities plus the three-handle inheritance bound.
    fn attributes(
        profile: &Profile,
        capabilities: &[SID_AND_ATTRIBUTES],
        std_handles: &[windows_sys::Win32::Foundation::HANDLE; 3],
    ) -> Result<AttrList, String> {
        let mut list_size = 0usize;
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &raw mut list_size) };
        if list_size == 0 {
            return Err(last_error("size the attribute list"));
        }
        let mut buffer = vec![0u8; list_size];
        let list: LPPROC_THREAD_ATTRIBUTE_LIST = buffer.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &raw mut list_size) } == FALSE {
            return Err(last_error("initialize the attribute list"));
        }
        let mut security = SECURITY_CAPABILITIES {
            AppContainerSid: profile.sid.0,
            Capabilities: capabilities.as_ptr().cast_mut(),
            CapabilityCount: u32::try_from(capabilities.len())
                .map_err(|_| last_error("count the capabilities"))?,
            Reserved: 0,
        };
        let security_set = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                usize::try_from(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES)
                    .map_err(|_| last_error("attribute number"))?,
                ptr::addr_of_mut!(security).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        let handles_set = security_set != FALSE
            && unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    usize::try_from(PROC_THREAD_ATTRIBUTE_HANDLE_LIST)
                        .map_err(|_| last_error("attribute number"))?,
                    std_handles.as_ptr().cast(),
                    std::mem::size_of_val(std_handles),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            } != FALSE;
        if !handles_set {
            unsafe { DeleteProcThreadAttributeList(list) };
            return Err(last_error("set the process attributes"));
        }
        Ok(AttrList(buffer))
    }

    ///
    /// # Safety
    /// Win32 process launch; handles and the attribute list are closed on
    /// every exit path, and planted ACEs are lifted on every exit path.
    pub(super) fn spawn(
        roots: &[PathBuf],
        executable: &OsStr,
        run_args: &[OsString],
    ) -> Result<ExitCode, String> {
        let (profile, capabilities) = container()?;
        let planted = plant_grants(roots, Path::new(executable), profile.sid.0)?;

        let std_handles = [
            unsafe { GetStdHandle(STD_INPUT_HANDLE) },
            unsafe { GetStdHandle(STD_OUTPUT_HANDLE) },
            unsafe { GetStdHandle(STD_ERROR_HANDLE) },
        ];

        let attrs = attributes(&profile, &capabilities, &std_handles)
            .inspect_err(|_| drop(revoke_planted(&planted, profile.sid.0)))?;

        let mut application = Path::new(executable)
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|_| wide_path(Path::new(executable)));
        let mut command: Vec<u16> = Vec::new();
        push_quoted(&mut command, executable);
        for arg in run_args {
            command.push(u16::from(b' '));
            push_quoted(&mut command, arg);
        }
        command.push(0);

        let mut info: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        info.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
            .map_err(|_| "STARTUPINFOEXW overflows u32".to_string())?;
        info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        info.StartupInfo.hStdInput = std_handles[0];
        info.StartupInfo.hStdOutput = std_handles[1];
        info.StartupInfo.hStdError = std_handles[2];
        info.lpAttributeList = attrs.as_ptr();
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let spawned = unsafe {
            CreateProcessW(
                application
                    .as_mut()
                    .map_or(ptr::null(), |name| name.as_mut_ptr()),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1, // bInheritHandles, bounded by PROC_THREAD_ATTRIBUTE_HANDLE_LIST
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                ptr::null(),
                ptr::null(),
                &raw const info.StartupInfo,
                &raw mut process,
            )
        };
        if let Some(name) = application.as_mut() {
            name.clear();
        }
        if spawned == FALSE {
            revoke_planted(&planted, profile.sid.0);
            return Err(last_error("spawn the sandboxed process"));
        }

        let job = kill_on_close_job().and_then(|job| {
            if unsafe { AssignProcessToJobObject(job.0, process.hProcess) } == FALSE {
                Err(last_error("bind the sandboxed process to its job"))
            } else {
                Ok(job)
            }
        });
        let _job = match job {
            Ok(job) => job,
            Err(error) => {
                unsafe {
                    TerminateProcess(process.hProcess, 126);
                    CloseHandle(process.hThread);
                    CloseHandle(process.hProcess);
                }
                revoke_planted(&planted, profile.sid.0);
                return Err(error);
            }
        };
        unsafe { ResumeThread(process.hThread) };
        let waited = unsafe { WaitForSingleObject(process.hProcess, INFINITE) };
        let mut code = 1u32;
        if waited == WAIT_OBJECT_0 {
            unsafe { GetExitCodeProcess(process.hProcess, &raw mut code) };
        }
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        };
        let revoke_error = revoke_planted(&planted, profile.sid.0);
        if let Some(error) = revoke_error {
            super::write_error(&error);
        }
        u8::try_from(code & 0xFF).map_or(Ok(ExitCode::from(126)), |c| Ok(ExitCode::from(c)))
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
