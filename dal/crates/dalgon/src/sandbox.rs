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
        // An automatic root (temp, cache) may have vanished since startup;
        // omitting its rule denies it instead of failing the child.
        let Ok(fd) = PathFd::new(root) else {
            continue;
        };
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
/// grants its SID `(OI)(CI)` full access on the allowed roots and
/// read+execute on their ancestors, the launch directory, and `PATH` dirs
/// (system locations already grant `ALL APPLICATION PACKAGES`), then launches the
/// target under the container
/// inside its own kill-on-close job object. Grant lifetime is refcounted in a
/// shared state file so overlapping runs keep concurrent access and a killed
/// run's leftovers are reaped by the next edit. Reads match the Landlock and
/// Seatbelt models: open wherever the user's DACLs allow.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "process edge: no safe crate exposes AppContainer launch or DACL editing"
)]
mod win {
    use std::ffi::{OsStr, OsString};
    use std::fmt::Write as _;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
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
        ACL, DACL_SECURITY_INFORMATION, FreeSid, GetSecurityDescriptorDacl, PSID,
        SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
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
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateMutexW, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenMutexW,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        PROCESS_INFORMATION, ReleaseMutex, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    const GENERIC_READ_EXECUTE: u32 = 0xA000_0000; // GENERIC_READ | GENERIC_EXECUTE
    const GENERIC_ALL_ACCESS: u32 = 0x1000_0000; // GENERIC_ALL
    /// POSIX `+x` parity: traverse plus attribute reads, no listing or file
    /// contents — sibling names under an ancestor stay private.
    const TRAVERSE_ACCESS: u32 = 0xA0; // FILE_TRAVERSE | FILE_READ_ATTRIBUTES

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
        guid: String,
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

    /// Environment captured once at the process edge (`spawn`): ambient
    /// `std::env::var*` reads are disallowed, so the two variables the
    /// sandbox needs resolve here and thread down as a typed snapshot.
    struct Edge {
        user_key: OsString,
        state_root: Option<OsString>,
        path: Option<OsString>,
    }
    impl Edge {
        fn capture() -> Self {
            let vars: std::collections::HashMap<OsString, OsString> = std::env::vars_os().collect();
            let get = |name: &str| vars.get(OsStr::new(name)).cloned();
            Self {
                user_key: get("USERPROFILE")
                    .or_else(|| get("USERNAME"))
                    .unwrap_or_default(),
                state_root: get("LOCALAPPDATA"),
                path: get("PATH"),
            }
        }
    }

    /// A stable tag separating one user's kernel objects from another's:
    /// on a multi-session or Remote Desktop host a fixed `Global\` name lets
    /// any session squat the object and deny sandbox startup to everyone else.
    fn user_tag(edge: &Edge) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for unit in edge.user_key.encode_wide() {
            hash = (hash ^ u64::from(unit)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    /// Serializes one DACL edit plus its bookkeeping across sibling helpers.
    /// Held only for a load→edit→save transact — never across a child run —
    /// so unrelated jobs run concurrently. `Global\` scope so same-user
    /// sessions share it, user-tagged so other users cannot squat the name.
    struct DaclLock(OwnedHandle);
    impl DaclLock {
        fn take(edge: &Edge) -> Result<Self, String> {
            let name = wide(&format!("Global\\dalgon.sandbox.dacl.{}", user_tag(edge)));
            let handle = unsafe { CreateMutexW(ptr::null(), FALSE, name.as_ptr()) };
            let mutex = OwnedHandle::new(handle)?;
            if unsafe { WaitForSingleObject(mutex.0, INFINITE) } != WAIT_OBJECT_0 {
                return Err(last_error("lock the sandbox DACL mutex"));
            }
            Ok(Self(mutex))
        }
    }
    impl Drop for DaclLock {
        fn drop(&mut self) {
            unsafe { ReleaseMutex((self.0).0) };
        }
    }

    /// The liveness beacon for one run: a named mutex held for the whole
    /// plant→run→lift sequence. A helper killed mid-run releases it, so the
    /// next transact can reap that run's leftover grants by deriving its SID
    /// from its GUID-named profile.
    struct RunGuard(OwnedHandle);
    impl RunGuard {
        fn take(edge: &Edge, guid: &str) -> Result<Self, String> {
            let name = wide(&format!(
                "Global\\dalgon.sandbox.run.{guid}.{}",
                user_tag(edge)
            ));
            let handle = unsafe { CreateMutexW(ptr::null(), FALSE, name.as_ptr()) };
            let mutex = OwnedHandle::new(handle)?;
            if unsafe { WaitForSingleObject(mutex.0, INFINITE) } != WAIT_OBJECT_0 {
                return Err(last_error("take the sandbox run beacon"));
            }
            Ok(Self(mutex))
        }
    }
    impl Drop for RunGuard {
        fn drop(&mut self) {
            unsafe { ReleaseMutex((self.0).0) };
        }
    }

    /// A run beacon is alive while its mutex object exists: `OpenMutexW`
    /// fails with `ERROR_FILE_NOT_FOUND` once every handle closes — which is
    /// exactly when the last holder died. Any other failure means "alive".
    fn run_alive(edge: &Edge, guid: &str) -> bool {
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const ERROR_FILE_NOT_FOUND: u32 = 2;
        let name = wide(&format!(
            "Global\\dalgon.sandbox.run.{guid}.{}",
            user_tag(edge)
        ));
        let handle = unsafe { OpenMutexW(SYNCHRONIZE, FALSE, name.as_ptr()) };
        if !handle.is_null() {
            unsafe { CloseHandle(handle) };
            return true;
        }
        unsafe { GetLastError() != ERROR_FILE_NOT_FOUND }
    }

    /// Grant bookkeeping shared by every `__sandbox` helper of this user:
    /// `orig[path]` is the DACL-open flag observed by the path's first live
    /// planter; `holders[path]` is every live run's `(guid, access)` grant.
    /// The last lifter restores exactly what the first planter saw, so an
    /// open root returns to a null DACL and a deny-all root returns deny-all.
    #[derive(Default)]
    struct DaclState {
        orig: std::collections::BTreeMap<String, bool>,
        holders: std::collections::BTreeMap<String, Vec<(String, u32)>>,
    }

    /// Bookkeeping lives under `LOCALAPPDATA`, never inside a writable
    /// root: the temp root carries an inheritable full-control grant, so a
    /// sandboxed command could delete or forge holder records there and
    /// cleanup would retire ACEs that were never lifted.
    fn dacl_state_path(edge: &Edge) -> PathBuf {
        edge.state_root
            .as_ref()
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("dalgon")
            .join("sandbox-dacl.state")
    }

    fn path_key(path: &Path) -> String {
        let mut key = String::new();
        for unit in path.as_os_str().encode_wide() {
            let _ = write!(key, "{unit:04x}");
        }
        key
    }

    fn key_path(key: &str) -> Option<PathBuf> {
        let units: Vec<u16> = (0..key.len() / 4)
            .filter_map(|i| u16::from_str_radix(&key[i * 4..i * 4 + 4], 16).ok())
            .collect();
        if units.len() * 4 != key.len() {
            return None;
        }
        Some(PathBuf::from(OsString::from_wide(&units)))
    }

    impl DaclState {
        fn load(edge: &Edge) -> Self {
            let mut state = Self::default();
            let Ok(text) = std::fs::read_to_string(dacl_state_path(edge)) else {
                return state;
            };
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split('\t').collect();
                match fields.as_slice() {
                    ["O", path, open] => {
                        state.orig.insert((*path).to_string(), *open == "1");
                    }
                    ["H", path, guid, access] => {
                        if let Ok(access) = u32::from_str_radix(access, 16) {
                            state
                                .holders
                                .entry((*path).to_string())
                                .or_default()
                                .push(((*guid).to_string(), access));
                        }
                    }
                    _ => {}
                }
            }
            state
        }

        fn save(&self, edge: &Edge) -> Result<(), String> {
            let mut text = String::from("v1\n");
            for (path, open) in &self.orig {
                let _ = writeln!(text, "O\t{path}\t{}", u8::from(*open));
            }
            for (path, holders) in &self.holders {
                for (guid, access) in holders {
                    let _ = writeln!(text, "H\t{path}\t{guid}\t{access:x}");
                }
            }
            let path = dacl_state_path(edge);
            let tmp = path.with_extension("tmp");
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|source| {
                    format!("dalgon sandbox: create grant state dir: {source}")
                })?;
            }
            std::fs::write(&tmp, text)
                .and_then(|()| std::fs::rename(&tmp, &path))
                .map_err(|source| format!("dalgon sandbox: save grant state: {source}"))
        }

        fn add_holder(&mut self, path: &Path, guid: &str, access: u32) {
            let key = path_key(path);
            let holders = self.holders.entry(key).or_default();
            if !holders.iter().any(|(g, a)| g == guid && *a == access) {
                holders.push((guid.to_string(), access));
            }
        }

        fn remove_holder(&mut self, key: &str, guid: &str, access: u32) -> bool {
            let Some(holders) = self.holders.get_mut(key) else {
                return false;
            };
            let before = holders.len();
            holders.retain(|(g, a)| !(g == guid && *a == access));
            let removed = holders.len() != before;
            if holders.is_empty() {
                self.holders.remove(key);
            }
            removed
        }
    }

    /// Runs `edit` under the DACL mutex with a freshly loaded, reap-swept
    /// state, then persists it. Every grant mutation is one transact.
    fn transact<R>(
        edge: &Edge,
        edit: impl FnOnce(&mut DaclState) -> Result<R, String>,
    ) -> Result<R, String> {
        let _lock = DaclLock::take(edge)?;
        let mut state = DaclState::load(edge);
        reap_dead(edge, &mut state);
        let out = edit(&mut state)?;
        state.save(edge)?;
        Ok(out)
    }

    /// Derives a dead run's `AppContainer` SID from its profile name.
    fn sid_for(guid: &str) -> Option<OwnedSid> {
        let name = wide(&format!("dalgon.sandbox.{guid}"));
        let mut sid: PSID = ptr::null_mut();
        let ok = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &raw mut sid) };
        if ok < 0 || sid.is_null() {
            return None;
        }
        Some(OwnedSid(sid))
    }

    /// Lifts one `(path, guid, access)` grant: revokes that run's ACE, then
    /// removes the holder record. The record is dropped only after the edit
    /// succeeds, so a failed lift is retried by the next transact instead of
    /// leaving an untracked ACE behind. When the last holder leaves, the
    /// write restores the DACL state the first planter observed. A revoke
    /// failure propagates so callers can report the retained grant.
    fn lift(state: &mut DaclState, key: &str, guid: &str, access: u32) -> Result<(), String> {
        let has = state
            .holders
            .get(key)
            .is_some_and(|h| h.iter().any(|(g, a)| g == guid && *a == access));
        if !has {
            return Ok(());
        }
        let (Some(sid), Some(path)) = (sid_for(guid), key_path(key)) else {
            return Err(format!("cannot resolve the grant identity for {key}"));
        };
        let last = state.holders.get(key).is_some_and(|h| h.len() == 1);
        let was_open = if last {
            state.orig.get(key).copied().unwrap_or(false)
        } else {
            false
        };
        edit_dacl(&path, sid.0, access, REVOKE_ACCESS, last && was_open)?;
        state.remove_holder(key, guid, access);
        if last {
            state.orig.remove(key);
        }
        Ok(())
    }

    /// Drops every grant whose run beacon is gone — a helper killed mid-run
    /// leaves `H` records whose ACEs would otherwise pin the directory open
    /// or deny-all forever.
    fn reap_dead(edge: &Edge, state: &mut DaclState) {
        let dead: Vec<(String, String, u32)> = state
            .holders
            .iter()
            .flat_map(|(path, holders)| {
                holders
                    .iter()
                    .filter(|(guid, _)| !run_alive(edge, guid))
                    .map(|(guid, access)| (path.clone(), guid.clone(), *access))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (path, guid, access) in dead {
            // Best-effort: a failed lift keeps its holder record and is
            // retried by the next transact.
            let _ = lift(state, &path, &guid, access);
        }
    }

    /// Reads whether the DACL on `path` is unrestricted (absent or null).
    /// `SetEntriesInAclW` merges a one-entry ACL onto a null DACL, so a lift
    /// that wrote the merged ACL back would turn an open root into deny-all —
    /// the last lifter must restore this flag's value instead.
    fn dacl_open(path: &Path) -> Result<bool, String> {
        let wide_path = wide_path(path);
        let mut sd = ptr::null_mut();
        let mut old_dacl: *mut ACL = ptr::null_mut();
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
        let mut present = FALSE;
        let mut defaulted = FALSE;
        let mut stored_dacl: *mut ACL = ptr::null_mut();
        let described = unsafe {
            GetSecurityDescriptorDacl(
                sd,
                &raw mut present,
                &raw mut stored_dacl,
                &raw mut defaulted,
            )
        };
        if !sd.is_null() {
            unsafe { LocalFree(sd) };
        }
        if described == FALSE {
            return Err(last_error(&format!("describe ACL on {}", path.display())));
        }
        Ok(present == FALSE || stored_dacl.is_null())
    }

    /// Plants `(path, access)` for this run in two transacts: the intent
    /// (holder record plus the first live planter's DACL-open flag) is
    /// persisted BEFORE the DACL changes, so a kill in the gap leaves a
    /// stale intent the next reap retires — never an untracked live ACE.
    /// A failed grant then retires its intent so reaps never chase an ACE
    /// that does not exist.
    fn plant(edge: &Edge, path: &Path, sid: PSID, guid: &str, access: u32) -> Result<bool, String> {
        let key = path_key(path);
        let recorded = transact(edge, |state| {
            if state
                .holders
                .get(&key)
                .is_some_and(|h| h.iter().any(|(g, a)| g == guid && *a == access))
            {
                return Ok(false);
            }
            if !state.holders.contains_key(&key) {
                state.orig.insert(key.clone(), dacl_open(path)?);
            }
            state.add_holder(path, guid, access);
            Ok(true)
        })?;
        if !recorded {
            return Ok(false);
        }
        match transact(edge, |_state| {
            edit_dacl(path, sid, access, GRANT_ACCESS, false).map(|_| ())
        }) {
            Ok(()) => Ok(true),
            Err(error) => {
                let _ = transact(edge, |state| {
                    if state.holders.get(&key).is_some_and(|h| h.len() == 1) {
                        state.orig.remove(&key);
                    }
                    state.remove_holder(&key, guid, access);
                    Ok(())
                });
                Err(error)
            }
        }
    }

    /// Adds or removes the container SID's ACE on one path; `(OI)(CI)` covers
    /// the subtree. Callers must only edit DACLs the current user can write.
    /// Returns whether the original DACL was unrestricted (absent or null):
    /// `SetEntriesInAclW` builds a one-entry ACL from a null DACL, so a lift
    /// that wrote the merged ACL back would turn an open root into deny-all.
    /// `restore_open` writes a null DACL instead of the merged ACL, but only
    /// when the merged ACL is empty — ACEs planted on the same path by other
    /// live runs must survive this run's lift.
    fn edit_dacl(
        path: &Path,
        sid: PSID,
        access: u32,
        mode: i32,
        restore_open: bool,
    ) -> Result<bool, String> {
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
        let mut present = FALSE;
        let mut defaulted = FALSE;
        let mut stored_dacl: *mut ACL = ptr::null_mut();
        let described = unsafe {
            GetSecurityDescriptorDacl(
                sd,
                &raw mut present,
                &raw mut stored_dacl,
                &raw mut defaulted,
            )
        };
        if described == FALSE {
            if !sd.is_null() {
                unsafe { LocalFree(sd) };
            }
            return Err(last_error(&format!("describe ACL on {}", path.display())));
        }
        let was_open = present == FALSE || stored_dacl.is_null();
        let mut new_dacl: *mut ACL = ptr::null_mut();
        let merge = unsafe { SetEntriesInAclW(1, &raw const entry, old_dacl, &raw mut new_dacl) };
        if !sd.is_null() {
            unsafe { LocalFree(sd) };
        }
        if merge != 0 {
            return Err(last_error(&format!("build ACL for {}", path.display())));
        }
        let restore = restore_open && (new_dacl.is_null() || unsafe { (*new_dacl).AceCount == 0 });
        let write = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                if restore { ptr::null_mut() } else { new_dacl },
                ptr::null_mut(),
            )
        };
        unsafe { LocalFree(new_dacl.cast()) };
        if write != 0 {
            return Err(last_error(&format!("write ACL on {}", path.display())));
        }
        Ok(was_open)
    }

    /// The grant plan, matched to the POSIX read-open model: the container
    /// reads and executes wherever DACLs already let it — system locations
    /// grant `ALL APPLICATION PACKAGES` out of the box — plus RX ACEs on the
    /// launch directory and every `PATH` directory, and traverse-only ACEs
    /// on each writable root's ancestors (POSIX `+x` parity: the container
    /// can resolve through them but cannot list or read sibling files).
    /// Writable roots get full control. Drive roots are never touched: an
    /// `(OI)(CI)` ACE on a drive root propagates to every existing
    /// descendant on the volume. `PATH` entries tolerate grant failure —
    /// system dirs already carry `ALL APPLICATION PACKAGES` — while the
    /// launch dir, ancestors, and writable roots are fatal: a failed grant
    /// there means the promised access cannot exist, so the run must not
    /// start with the read policy only partially installed.
    fn grant_plan(edge: &Edge, roots: &[PathBuf], executable: &Path) -> Vec<(PathBuf, u32, bool)> {
        let mut plan: Vec<(PathBuf, u32, bool)> = Vec::new();
        let push_rx = |dir: PathBuf, optional: bool, plan: &mut Vec<(PathBuf, u32, bool)>| {
            if !dir.as_os_str().is_empty()
                && !plan
                    .iter()
                    .any(|(p, a, _)| *p == dir && *a == GENERIC_READ_EXECUTE)
            {
                plan.push((dir, GENERIC_READ_EXECUTE, optional));
            }
        };
        if let Some(dir) = executable
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
        {
            push_rx(dir.to_path_buf(), false, &mut plan);
        }
        if let Some(paths) = &edge.path {
            for dir in std::env::split_paths(&paths) {
                push_rx(dir, true, &mut plan);
            }
        }
        for root in roots {
            // Ancestor traverse stops before the drive/share root: an
            // inheritable ACE on a volume root rewrites every descendant's
            // DACL, and a normal user cannot write it anyway — the run
            // would fail for lack of WRITE_DAC. Traverse, not RX, so the
            // container cannot list or read sibling files under a private
            // parent (POSIX `+x` parity).
            for ancestor in root
                .ancestors()
                .skip(1)
                .take_while(|dir| dir.parent().is_some())
            {
                if !ancestor.as_os_str().is_empty() && !plan.iter().any(|(p, _, _)| *p == ancestor)
                {
                    plan.push((ancestor.to_path_buf(), TRAVERSE_ACCESS, false));
                }
            }
            plan.push((root.clone(), GENERIC_ALL_ACCESS, false));
        }
        plan
    }

    /// Plants the grant plan under per-edit transacts. A fatal (writable
    /// root) failure lifts what already planted and aborts; optional entries
    /// tolerate the edit failing.
    fn plant_grants(
        edge: &Edge,
        roots: &[PathBuf],
        executable: &Path,
        profile: &Profile,
    ) -> Result<Vec<(PathBuf, u32)>, String> {
        let mut planted: Vec<(PathBuf, u32)> = Vec::new();
        let result = (|| {
            for (dir, access, optional) in grant_plan(edge, roots, executable) {
                match plant(edge, &dir, profile.sid.0, &profile.guid, access) {
                    Ok(_) => planted.push((dir, access)),
                    Err(error) if !optional => return Err(error),
                    Err(_) => {}
                }
            }
            Ok(())
        })();
        if result.is_err() {
            lift_all(edge, &planted, profile);
        }
        result.map(|()| planted)
    }

    /// Lifts this run's planted grants. The state file decides each path's
    /// restore: the last live holder returns the DACL the first planter
    /// observed, so overlapping runs cannot erase each other and a killed
    /// run's leftovers are reaped by the next transact. Failures collect so
    /// the run reports grants it could not remove.
    fn lift_all(edge: &Edge, planted: &[(PathBuf, u32)], profile: &Profile) -> Option<String> {
        let mut failed = Vec::new();
        for (path, access) in planted {
            if let Err(error) = transact(edge, |state| {
                lift(state, &path_key(path), &profile.guid, *access)
            }) {
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
        let guid = run_id()?;
        let name = wide(&format!("dalgon.sandbox.{guid}"));
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
                guid,
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
        let edge = Edge::capture();
        let (profile, capabilities) = container()?;
        // The run beacon lives for the whole plant→run→lift sequence: if this
        // helper dies mid-run the next transact reaps this run's grants instead
        // of leaving ACEs pinned forever.
        let _run = RunGuard::take(&edge, &profile.guid)?;
        let planted = plant_grants(&edge, roots, Path::new(executable), &profile)?;

        let std_handles = [
            unsafe { GetStdHandle(STD_INPUT_HANDLE) },
            unsafe { GetStdHandle(STD_OUTPUT_HANDLE) },
            unsafe { GetStdHandle(STD_ERROR_HANDLE) },
        ];

        let attrs = attributes(&profile, &capabilities, &std_handles)
            .inspect_err(|_| drop(lift_all(&edge, &planted, &profile)))?;

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
            lift_all(&edge, &planted, &profile);
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
                lift_all(&edge, &planted, &profile);
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
        let revoke_error = lift_all(&edge, &planted, &profile);
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
