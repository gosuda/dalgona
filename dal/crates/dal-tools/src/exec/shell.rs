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
    path: Option<&OsStr>,
) -> Result<ResolvedShell, ExecError> {
    for program_files in [program_files, program_files_x86].into_iter().flatten() {
        let candidate = Path::new(program_files).join(r"Git\bin\bash.exe");
        if is_executable_file(&candidate) {
            return Ok(ResolvedShell { program: candidate });
        }
    }
    if let Some(path) = path {
        if let Some(program) = find_in_path("bash.exe", path) {
            return Ok(ResolvedShell { program });
        }
    }
    Err(ExecError::NoBash)
}

#[cfg(windows)]
fn find_in_path(executable: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|directory| directory.join(executable))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::Path};

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
        let error = super::default_windows_ladder(None, None, None)
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
