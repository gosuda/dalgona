use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

#[cfg(windows)]
use std::ffi::OsStr;

use super::ExecError;

#[derive(Debug)]
pub(crate) struct ResolvedShell {
    pub program: PathBuf,
}

/// Resolves a configured shell against captured paths and the platform ladder.
///
/// A configured shell is authoritative: failure to resolve it never falls back
/// to a different executable.
pub(crate) fn resolve(
    configured: Option<&str>,
    cwd: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<ResolvedShell, ExecError> {
    if let Some(value) = configured {
        // Relative configured shells resolve against the call working
        // directory (explicit and captured), never the process cwd.
        let path = Path::new(value);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        return if is_executable_file(&resolved) {
            Ok(ResolvedShell { program: resolved })
        } else {
            Err(ExecError::ShellMissing {
                path: value.to_owned(),
            })
        };
    }
    default_ladder(environment)
}

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the windows twin reports real errors; the signature must match"
)]
fn default_ladder(_environment: &BTreeMap<OsString, OsString>) -> Result<ResolvedShell, ExecError> {
    const CANDIDATES: [&str; 4] = [
        "/bin/bash",
        "/usr/bin/bash",
        "/usr/local/bin/bash",
        "/opt/homebrew/bin/bash",
    ];
    for candidate in CANDIDATES {
        let path = Path::new(candidate);
        if is_executable_file(path) {
            return Ok(ResolvedShell {
                program: PathBuf::from(candidate),
            });
        }
    }
    Ok(ResolvedShell {
        program: PathBuf::from("/bin/sh"),
    })
}

#[cfg(windows)]
fn default_ladder(environment: &BTreeMap<OsString, OsString>) -> Result<ResolvedShell, ExecError> {
    default_windows_ladder(
        env_value(environment, "ProgramFiles"),
        env_value(environment, "ProgramFiles(x86)"),
        env_value(environment, "LOCALAPPDATA"),
        env_value(environment, "SystemRoot"),
        env_value(environment, "PATH"),
    )
}

#[cfg(windows)]
fn env_value<'a>(environment: &'a BTreeMap<OsString, OsString>, name: &str) -> Option<&'a OsStr> {
    environment
        .get(OsStr::new(name))
        .or_else(|| {
            environment.iter().find_map(|(key, value)| {
                key.to_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case(name))
                    .then_some(value)
            })
        })
        .map(OsString::as_os_str)
}

#[cfg(windows)]
fn default_windows_ladder(
    program_files: Option<&OsStr>,
    program_files_x86: Option<&OsStr>,
    local_app_data: Option<&OsStr>,
    system_root: Option<&OsStr>,
    path: Option<&OsStr>,
) -> Result<ResolvedShell, ExecError> {
    for program_files in [program_files, program_files_x86].into_iter().flatten() {
        let candidate = Path::new(program_files).join(r"Git\bin\bash.exe");
        if is_executable_file(&candidate) {
            return Ok(ResolvedShell { program: candidate });
        }
    }
    // Per-user Git for Windows installs under LOCALAPPDATA\Programs.
    if let Some(local_app_data) = local_app_data {
        let candidate = Path::new(local_app_data).join(r"Programs\Git\bin\bash.exe");
        if is_executable_file(&candidate) {
            return Ok(ResolvedShell { program: candidate });
        }
    }
    if let Some(path) = path
        && let Some(program) = find_in_path("bash.exe", path, system_root)
    {
        return Ok(ResolvedShell { program });
    }
    Err(ExecError::NoBash)
}

/// Finds `executable` on PATH, skipping the WSL `bash.exe` stub in
/// System32 — it resolves first on a default PATH but cannot open the
/// `C:\` paths an exec call passes it.
#[cfg(windows)]
fn find_in_path(executable: &str, path: &OsStr, system_root: Option<&OsStr>) -> Option<PathBuf> {
    let wsl_stub = system_root.map(|root| Path::new(root).join(r"System32\bash.exe"));
    std::env::split_paths(path)
        .map(|directory| directory.join(executable))
        .find(|candidate| {
            is_executable_file(candidate)
                && !wsl_stub.as_ref().is_some_and(|stub| {
                    candidate.as_os_str().eq_ignore_ascii_case(stub.as_os_str())
                })
        })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::Path};

    #[cfg(windows)]
    use std::ffi::OsString;

    use super::resolve;

    #[test]
    fn shell_resolution_missing_bash() {
        let root = tempfile::tempdir().expect("create a temporary workspace");
        let missing = root.path().join("missing-shell");
        let missing_text = missing.to_string_lossy();
        let error = resolve(Some(&missing_text), root.path(), &BTreeMap::new())
            .expect_err("the generated shell path is absent");
        assert_eq!(
            error.to_string(),
            format!("exec: shell {} does not exist", missing.display())
        );
    }

    #[test]
    fn configured_shell_must_be_an_executable_file() {
        let root = tempfile::tempdir().expect("create a temporary workspace");
        let error = resolve(Some("."), root.path(), &BTreeMap::new())
            .expect_err("the workspace root is a directory, not a shell");
        assert_eq!(error.to_string(), "exec: shell . does not exist");

        let shell = root.path().join("shell");
        fs::write(&shell, b"shell").expect("write a temporary shell file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&shell, fs::Permissions::from_mode(0o700))
                .expect("make the temporary shell executable");
        }
        let resolved = resolve(Some("shell"), root.path(), &BTreeMap::new())
            .expect("resolve the executable relative to the captured cwd");
        assert_eq!(resolved.program, shell);
        assert!(Path::new(&resolved.program).is_absolute());
    }

    #[cfg(windows)]
    #[test]
    fn windows_without_git_bash_returns_the_named_error() {
        let error = super::default_windows_ladder(None, None, None, None, None)
            .expect_err("an empty captured environment must fail closed");
        assert_eq!(
            error.to_string(),
            "exec: no bash found. Install Git for Windows (https://git-scm.com/downloads/win), add bash.exe to PATH, or set shell in dal.toml"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_search_uses_the_captured_environment() {
        let root = tempfile::tempdir().expect("create a temporary workspace");
        let git = root.path().join(r"Git\bin");
        fs::create_dir_all(&git).expect("create Git's bash directory");
        let bash = git.join("bash.exe");
        fs::write(&bash, b"bash").expect("write the bash fixture");
        let mut environment = BTreeMap::new();
        environment.insert(
            OsString::from("programfiles"),
            root.path().as_os_str().to_owned(),
        );
        let resolved = resolve(None, root.path(), &environment)
            .expect("find bash using captured ProgramFiles");
        assert_eq!(resolved.program, bash);
    }
}
