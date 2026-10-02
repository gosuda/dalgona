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
    let mut command = Command::new("bash");
    command.arg(script).args(args).current_dir(root);
    captured(&mut command, None)
}

pub(crate) fn run_gate_dep(name: &str, req: &str, stdin_json: &str) -> Run {
    let script = repo_root().join("scripts/publish-crates.sh");
    let mut command = Command::new("bash");
    command
        .arg(script)
        .args(["--check-dep-only", name, req, "dal"])
        .current_dir(repo_root());
    captured(&mut command, Some(stdin_json))
}

pub(crate) fn run_semver_gate(current: &Path, baseline: &Path) -> Run {
    let script = repo_root().join("scripts/semver-gate.sh");
    let mut command = Command::new("bash");
    command
        .arg(script)
        .arg("--baseline-root")
        .arg(baseline)
        .arg(current)
        .current_dir(repo_root());
    captured(&mut command, None)
}
