use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::PathBuf;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;
use std::process::ExitCode;

const MALFORMED_ARGS: &str = "dalgon sandbox: malformed launcher arguments";

#[cfg(any(windows, not(any(target_os = "linux", target_os = "macos", windows))))]
const WINDOWS_ERROR: &str = "sandbox = \"on\" is not supported on Windows. Set sandbox = \"off\" in dal.toml, or run dalgon inside WSL 2.";

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
        write_error(WINDOWS_ERROR);
        ExitCode::from(126)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        write_error(WINDOWS_ERROR);
        ExitCode::from(126)
    }
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
            let Some((roots, executable, target_args)) = parse_allow_args(argv) else {
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
            let error = Command::new(executable).args(target_args).exec();
            exec_error(executable, &error)
        }
        _ => malformed(),
    }
}

#[cfg(target_os = "linux")]
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
