#![expect(
    clippy::disallowed_methods,
    reason = "release support runs the publish and semver shell scripts"
)]
use std::{
    error::Error,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) type Run = Result<(i32, String, String), Box<dyn Error>>;

pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").clone()
}

/// The bash that runs the release scripts.
///
/// On Windows a bare `bash` resolves `C:\Windows\System32\bash.exe` — the
/// WSL launcher — before PATH, and with no distro registered it exits 1
/// without starting the script. Prefer the Git for Windows bash under
/// Program Files, the same ladder the exec tool follows.
fn shell() -> Command {
    #[cfg(windows)]
    {
        for key in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(dir) = env::var_os(key) {
                let candidate = Path::new(&dir).join(r"Git\bin\bash.exe");
                if candidate.is_file() {
                    return Command::new(candidate);
                }
            }
        }
        // Per-user Git for Windows installs under LOCALAPPDATA\Programs.
        if let Some(dir) = env::var_os("LOCALAPPDATA") {
            let candidate = Path::new(&dir).join(r"Programs\Git\bin\bash.exe");
            if candidate.is_file() {
                return Command::new(candidate);
            }
        }
        if let Some(paths) = env::var_os("PATH") {
            let stub =
                env::var_os("SystemRoot").map(|root| Path::new(&root).join(r"System32\bash.exe"));
            for candidate in env::split_paths(&paths).map(|dir| dir.join("bash.exe")) {
                if !candidate.is_file() {
                    continue;
                }
                // A WSL `bash.exe` wins on PATH but cannot open the `C:\`
                // script paths the guards pass.
                if stub.as_ref().is_some_and(|stub| {
                    candidate.as_os_str().eq_ignore_ascii_case(stub.as_os_str())
                }) {
                    continue;
                }
                return Command::new(candidate);
            }
        }
    }
    Command::new("bash")
}

fn captured(command: &mut Command, input: Option<&str>) -> Run {
    let output = if let Some(input) = input {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("child stdin was not piped"))?
            .write_all(input.as_bytes())?;
        child.wait_with_output()?
    } else {
        command.output()?
    };
    let code = output
        .status
        .code()
        .ok_or_else(|| io::Error::other("child process terminated without an exit code"))?;
    Ok((
        code,
        String::from_utf8(output.stdout)?,
        String::from_utf8(output.stderr)?,
    ))
}

pub(crate) fn run_publish_script(root: &Path, args: &[&str]) -> Run {
    let script = repo_root().join("scripts/publish-crates.sh");
    let mut command = shell();
    command.arg(script).args(args).current_dir(root);
    captured(&mut command, None)
}

pub(crate) fn run_gate_dep(name: &str, req: &str, stdin_json: &str) -> Run {
    let script = repo_root().join("scripts/publish-crates.sh");
    let mut command = shell();
    command
        .arg(script)
        .args(["--check-dep-only", name, req, "dal"])
        .current_dir(repo_root());
    captured(&mut command, Some(stdin_json))
}

pub(crate) fn run_semver_gate(current: &Path, baseline: &Path) -> Run {
    let script = repo_root().join("scripts/semver-gate.sh");
    let mut command = shell();
    command
        .arg(script)
        .arg("--baseline-root")
        .arg(baseline)
        .arg(current)
        .current_dir(repo_root());
    captured(&mut command, None)
}
