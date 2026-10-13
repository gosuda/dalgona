#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real product binaries"
)]

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use dal_agent::{Agent, Env, Host, Product, SessionRef};
use dal_core::Config;

static DALGON_BINARIES: OnceLock<Result<BTreeMap<String, PathBuf>, String>> = OnceLock::new();

pub(crate) fn dalgon_binary(name: &str) -> io::Result<&'static Path> {
    let binaries = DALGON_BINARIES
        .get_or_init(build_dalgon_binaries)
        .as_ref()
        .map_err(|message| io::Error::other(message.as_str()))?;
    binaries
        .get(name)
        .map(PathBuf::as_path)
        .ok_or_else(|| io::Error::other(format!("Cargo did not build the {name} binary")))
}

fn build_dalgon_binaries() -> Result<BTreeMap<String, PathBuf>, String> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "-p",
            "dalgon",
            "--bins",
            "--locked",
            "--message-format=json",
        ])
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("could not start Cargo: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Cargo could not build dalgon binaries: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let mut binaries = BTreeMap::new();
    for line in output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let text = std::str::from_utf8(line).map_err(|error| error.to_string())?;
        let message: CargoMessage = sonic_rs::from_str(text).map_err(|error| error.to_string())?;
        if message.reason.as_deref() != Some("compiler-artifact") {
            continue;
        }
        let (Some(target), Some(executable)) = (message.target, message.executable) else {
            continue;
        };
        if target.kind.iter().any(|kind| kind == "bin") {
            binaries.insert(target.name, executable);
        }
    }
    Ok(binaries)
}

#[derive(serde::Deserialize)]
struct CargoMessage {
    reason: Option<String>,
    target: Option<CargoTarget>,
    executable: Option<PathBuf>,
}

#[derive(serde::Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
}

static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct GateHarness {
    pub(crate) host: Host,
    pub(crate) agent: Agent,
}

pub(crate) async fn scripted_session(
    product: Product,
    config: Config,
    env: Env,
    session: SessionRef,
) -> Result<GateHarness, Box<dyn std::error::Error + Send + Sync>> {
    let host = Host::start(product, config, env).await?;
    let agent = host.open(session, dal_core::ClientId::new("core")).await?;
    Ok(GateHarness { host, agent })
}

/// Probes whether `pid` still runs an image named `name`, portable across
/// gate platforms. A bare liveness probe lies on Windows: PIDs recycle fast
/// enough that a killed target's number lands on an unrelated system process
/// (seen on windows-11-arm as a live svchost under the dead sleep's PID), so
/// the probe verifies the process identity, not just the number.
pub(crate) fn process_named_alive(pid: u32, name: &str) -> bool {
    #[cfg(target_os = "linux")]
    let alive = std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .is_ok_and(|comm| comm.trim().contains(name));
    #[cfg(target_os = "macos")]
    let alive = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(name));
    #[cfg(windows)]
    let alive = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {pid} -ErrorAction SilentlyContinue).ProcessName"),
        ])
        .output()
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .contains(name)
        });
    alive
}

/// The fixed environment snapshot for a test session. Windows console
/// tools read `SystemRoot`, TEMP, `COMSPEC`, and `PSModulePath` during
/// startup and the shell ladder needs the runner PATH to find Git Bash,
/// so the snapshot carries the whole runner environment on Windows; other
/// platforms keep an empty snapshot.
pub(crate) fn captured_shell_vars() -> BTreeMap<std::ffi::OsString, std::ffi::OsString> {
    #[cfg(windows)]
    {
        std::env::vars_os().collect()
    }
    #[cfg(not(windows))]
    {
        BTreeMap::default()
    }
}

pub(crate) struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub(crate) fn new() -> io::Result<Self> {
        Self::new_in(&std::env::temp_dir())
    }

    /// Creates the test directory under `root`: unix socket paths under the
    /// platform temp root can exceed `SUN_LEN` on BSD, so socket tests need
    /// a bounded base path.
    pub(crate) fn new_in(root: &Path) -> io::Result<Self> {
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("dalgon-gates-{}-{id}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
