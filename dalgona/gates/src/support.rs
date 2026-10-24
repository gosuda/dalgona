// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Shared helpers for the gate test binaries.
#![expect(
    clippy::disallowed_methods,
    reason = "gate support drives real binaries, scripts, and toolchain commands"
)]
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sonic_rs::{JsonContainerTrait, JsonValueTrait};

/// The gate-wide result type: any error fails the criterion.
pub type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// A unique temporary directory removed on drop.
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Creates a temp directory named `label-<uuid>`.
    ///
    /// # Errors
    /// Returns the directory-creation error.
    pub fn new(label: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("{label}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// The scratch directory path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The repository root two levels above this crate's manifest directory.
#[must_use]
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether the released-artifact gates run.
///
/// They install the published crate and the release assets of a tag, which
/// exist only after a release. The tag-only CI job sets
/// `DALGONA_RELEASED_ARTIFACTS`; every other run skips them.
#[must_use]
pub fn released_artifacts_requested() -> bool {
    std::env::var_os("DALGONA_RELEASED_ARTIFACTS").is_some_and(|value| !value.is_empty())
}

/// The `dalgona` package version from the workspace metadata.
///
/// # Errors
/// Returns the `cargo metadata` failure or a metadata document without the
/// `dalgona` package version.
pub fn dalgona_version() -> TestResult<String> {
    let output = run_command(
        std::process::Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version=1", "--locked"])
            .current_dir(repo_root()),
    )?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let metadata: sonic_rs::Value = sonic_rs::from_slice(&output.stdout)?;
    metadata
        .get("packages")
        .and_then(sonic_rs::Value::as_array)
        .and_then(|packages| {
            packages.iter().find(|package| {
                package.get("name").and_then(sonic_rs::Value::as_str) == Some("dalgona")
            })
        })
        .and_then(|package| package.get("version"))
        .and_then(sonic_rs::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Cargo metadata omitted the dalgona version".into())
}

/// Fails unless the release tag names this workspace's `dalgona` version.
///
/// Runs only when the released-artifact gates run and a ref name is present:
/// without the opt-in, or without `GITHUB_REF_NAME`, there is nothing to
/// compare and the check passes.
///
/// # Errors
/// Returns a mismatch naming the tag, the workspace version, and the expected
/// tag, so a diverged tag or version is reconciled on either side.
pub fn assert_release_tag_matches_version(version: &str) -> TestResult<()> {
    let Some(tag) = std::env::var_os("GITHUB_REF_NAME") else {
        return Ok(());
    };
    if !released_artifacts_requested() {
        return Ok(());
    }
    let tag = tag.to_string_lossy();
    let expected = format!("dalgona-v{version}");
    if tag == expected {
        Ok(())
    } else {
        Err(format!(
            "release tag `{tag}` does not match workspace dalgona version `{version}` \
             (expected tag `{expected}`): retag `{expected}` or update the workspace version to the tag"
        )
        .into())
    }
}

/// Runs `command` to completion and returns its captured output.
///
/// # Errors
/// Returns the spawn or wait error.
pub fn run_command(command: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    command.output()
}

/// The resident set of this process, in bytes, read per platform without a
/// library dependency: procfs on Linux, `ps` on macOS, `Get-Process` on
/// Windows.
///
/// # Errors
/// Returns the read, spawn, or parse failure of the platform counter.
pub fn resident_set_bytes() -> std::io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status")?;
        let value = status
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .ok_or_else(|| std::io::Error::other("VmRSS was not available"))?
            .parse::<u64>()
            .map_err(std::io::Error::other)?;
        Ok(value.saturating_mul(1024))
    }
    #[cfg(target_os = "macos")]
    {
        let output = run_command(std::process::Command::new("ps").args([
            "-o",
            "rss=",
            "-p",
            &std::process::id().to_string(),
        ]))?;
        let value = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u64>()
            .map_err(std::io::Error::other)?;
        Ok(value.saturating_mul(1024))
    }
    #[cfg(windows)]
    {
        let command = format!(
            "(Get-Process -Id {} | Select-Object -ExpandProperty WorkingSet64)",
            std::process::id()
        );
        let output = run_command(std::process::Command::new("powershell.exe").args([
            "-NoProfile",
            "-Command",
            &command,
        ]))?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u64>()
            .map_err(std::io::Error::other)
    }
}

/// The count of this process's open handles: file descriptors via
/// `/dev/fd` on Unix, kernel `HandleCount` via `Get-Process` on Windows.
///
/// # Errors
/// Returns the read, spawn, or parse failure of the platform counter.
pub fn open_handle_count() -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        Ok(std::fs::read_dir("/dev/fd")?.count())
    }
    #[cfg(windows)]
    {
        let command = format!(
            "(Get-Process -Id {} | Select-Object -ExpandProperty HandleCount)",
            std::process::id()
        );
        let output = run_command(std::process::Command::new("powershell.exe").args([
            "-NoProfile",
            "-Command",
            &command,
        ]))?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<usize>()
            .map_err(std::io::Error::other)
    }
}

/// The highest resident set and open-handle count a [`PeakSampler`] read.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourcePeak {
    /// The highest resident set read, in bytes.
    pub resident_bytes: u64,
    /// The highest open-handle count read.
    pub open_handles: usize,
    /// How many readings the peaks come from.
    pub samples: usize,
}

/// Reads this process's resident set and open-handle count on a fixed period
/// until it finishes, so a stress gate bounds the peak of the run instead of
/// only the quiet moment after it. Each reading uses the platform counters of
/// [`resident_set_bytes`] and [`open_handle_count`].
#[derive(Debug)]
pub struct PeakSampler {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<std::io::Result<ResourcePeak>>>,
}

impl PeakSampler {
    /// Starts the sampling thread; the first reading happens at once.
    #[must_use]
    pub fn start(period: std::time::Duration) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || -> std::io::Result<ResourcePeak> {
            let mut peak = ResourcePeak::default();
            loop {
                peak.resident_bytes = peak.resident_bytes.max(resident_set_bytes()?);
                peak.open_handles = peak.open_handles.max(open_handle_count()?);
                peak.samples += 1;
                if flag.load(std::sync::atomic::Ordering::Acquire) {
                    return Ok(peak);
                }
                std::thread::sleep(period);
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// Takes one last reading, stops the thread, and returns the peaks.
    ///
    /// This blocks for up to one sampling period; call it from a blocking
    /// task inside an async gate.
    ///
    /// # Errors
    /// Returns the platform counter failure of any reading, or a panic of the
    /// sampling thread.
    pub fn finish(mut self) -> TestResult<ResourcePeak> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let thread = self
            .thread
            .take()
            .ok_or("the resource sampler was already finished")?;
        let peak = thread
            .join()
            .map_err(|_| "the resource sampler thread panicked")??;
        Ok(peak)
    }
}

impl Drop for PeakSampler {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Runs `git` with `args` in `dir` and returns its standard output.
///
/// # Errors
/// Returns the spawn failure, or the arguments and standard error of a
/// command that exits unsuccessfully.
pub fn git_output(dir: &Path, args: &[&str]) -> TestResult<String> {
    let output = run_command(
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_OPTIONAL_LOCKS", "0"),
    )?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Creates `dir` as a real git checkout holding one committed file, so the
/// worktree isolation of a subagent run has a base commit to branch from.
///
/// # Errors
/// Returns the directory-creation, file-write, or git failure.
pub fn init_git_checkout(dir: &Path) -> TestResult<()> {
    std::fs::create_dir_all(dir)?;
    git_output(dir, &["init"])?;
    std::fs::write(dir.join("README.md"), "gate fixture\n")?;
    git_output(dir, &["add", "README.md"])?;
    git_output(
        dir,
        &[
            "-c",
            "user.email=gate@test",
            "-c",
            "user.name=gate",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "gate fixture",
        ],
    )?;
    Ok(())
}

/// Grants every bundled extension the services it declares for `workspace`,
/// as a person would on first use, so no grant question interrupts a run.
/// The grants are stored under `data` and name `client` as the granter.
///
/// # Errors
/// Returns the product-build or grant-store failure, or an extension without
/// a code identity.
pub async fn grant_bundled_extensions(
    data: &Path,
    workspace: &Path,
    user: &str,
    client: &str,
) -> TestResult<()> {
    let product = build_product(data.to_path_buf(), Some(user))?;
    for extension in &product.extensions {
        let services = extension.inject().capabilities();
        if services.is_empty() || extension.origin() == dal_core::Origin::Builtin {
            continue;
        }
        dal_agent::ext::grants::GrantStore::new(data.to_path_buf())
            .grant(
                dal_agent::ext::grants::GrantKey::for_extension(extension, workspace)
                    .ok_or("registered extension has no code identity")?,
                dal_core::ClientId::new(client),
                dal_core::Timestamp::now(),
            )
            .await?;
    }
    Ok(())
}

/// The battery extension names the product registers.
pub const BATTERIES: [&str; 12] = [
    "ask",
    "history",
    "judged",
    "mcp",
    "orchestration",
    "procs",
    "quality",
    "review",
    "skills",
    "ttsr-rules",
    "web",
    "work",
];

/// Builds the dalgona product against `root` with optional user config.
///
/// # Errors
/// Returns config-load or product-build failures.
pub fn build_product(root: PathBuf, user_toml: Option<&str>) -> TestResult<dal_agent::Product> {
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        user_toml,
    )?;
    let cx = dalgon::BuildCx {
        data_root: root,
        config: &config,
    };
    Ok(dalgona::build(&cx)?)
}

/// The names of `product`'s extensions that are registered batteries.
pub fn battery_names(product: &dal_agent::Product) -> std::collections::BTreeSet<&str> {
    product
        .extensions
        .iter()
        .map(dal_agent::ext::Extension::name)
        .filter(|name| BATTERIES.contains(name))
        .collect()
}

/// The compiled `dalgona` binary under Cargo's target directory.
///
/// The test executable lives under the build directory, so its profile
/// directory is not where `dalgona` lands. Canonical directories come from
/// `cargo metadata`: the profile path relative to the build directory joins
/// onto the target directory. One intended binary, no fallback.
///
/// Freshness is this resolver's own job, not the caller's: before the
/// `is_file` check it runs `cargo build -p dalgona --bin dalgona --locked`
/// once per test process, so a stale artifact from an older tree can never
/// satisfy the check. Cargo's own fingerprinting keeps the rebuild cheap
/// when the binary is already current.
///
/// # Errors
/// Returns metadata, layout, build, or missing-binary failures.
pub fn dalgona_binary() -> TestResult<PathBuf> {
    let exe = std::env::current_exe()?;
    let profile_dir = test_profile_dir(&exe)?;
    let (target_dir, build_dir) = cargo_directories()?;
    let profile_dir = std::fs::canonicalize(profile_dir)?;
    let build_dir = std::fs::canonicalize(&build_dir)?;
    let relative = profile_dir.strip_prefix(&build_dir).map_err(|_| {
        format!(
            "the test binary is not under Cargo's build directory {}",
            build_dir.display()
        )
    })?;
    let profile = relative
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .ok_or("the test profile directory has no name")?;
    ensure_dalgona_binary_built(profile)?;
    let binary = target_dir
        .join(relative)
        .join(format!("dalgona{}", std::env::consts::EXE_SUFFIX));
    if binary.is_file() {
        return Ok(binary);
    }
    Err(format!(
        "the Dalgona binary is still missing at {} after `cargo build -p dalgona --bin dalgona --locked`",
        binary.display()
    )
    .into())
}

/// Builds the `dalgona` binary once per test process, so every gate sees a
/// binary from the current tree instead of a stale artifact.
///
/// The build runs in the workspace root with this process's environment, so
/// a `CARGO_TARGET_DIR` override applies to the build exactly as it did to
/// the `cargo metadata` layout above. Standard error is inherited so build
/// failures surface in the gate output; standard output is captured and
/// discarded. A plain build in the test process is fine: gates run as test
/// executables, never inside a Cargo build script, so there is no nested
/// Cargo to deadlock against.
///
/// # Errors
/// Returns the spawn failure or the build's nonzero status; the first
/// result is cached and replayed to later callers in this process.
fn ensure_dalgona_binary_built(profile: &str) -> TestResult<()> {
    static BUILT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let owned = profile.to_owned();
    let cached = BUILT.get_or_init(|| {
        let mut command = std::process::Command::new(env!("CARGO"));
        command.args(["build", "-p", "dalgona", "--bin", "dalgona", "--locked"]);
        match owned.as_str() {
            "debug" => {}
            "release" => {
                command.arg("--release");
            }
            custom => {
                command.args(["--profile", custom]);
            }
        }
        command
            .current_dir(repo_root())
            .stderr(std::process::Stdio::inherit());
        match command.spawn() {
            Err(error) => Err(format!(
                "failed to run cargo build -p dalgona --bin dalgona --locked: {error}"
            )),
            Ok(mut child) => run_cargo_build_to_completion(&mut child),
        }
    });
    match cached {
        Ok(()) => Ok(()),
        Err(message) => Err(message.clone().into()),
    }
}

/// Upper bound on the gate's own binary build. A cold workspace build of
/// the binary takes minutes, not a quarter hour; past this the build is
/// hung, and surfacing it beats hanging every gate process.
const CARGO_BUILD_DEADLINE: std::time::Duration = std::time::Duration::from_mins(15);

/// Waits for the spawned build, killing it and failing the gate once the
/// deadline passes instead of stalling the test process forever.
fn run_cargo_build_to_completion(child: &mut std::process::Child) -> Result<(), String> {
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "cargo build -p dalgona --bin dalgona --locked failed with status {status}"
                ));
            }
            Ok(None) => {}
            Err(error) => {
                return Err(format!(
                    "failed waiting for cargo build -p dalgona --bin dalgona --locked: {error}"
                ));
            }
        }
        if started.elapsed() >= CARGO_BUILD_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "cargo build -p dalgona --bin dalgona --locked did not finish within 900 seconds"
                    .to_owned(),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Cargo's canonical `target_directory` and `build_directory` from
/// `cargo metadata`, so configured target directories and directory splits
/// resolve without guessing.
fn cargo_directories() -> TestResult<(PathBuf, PathBuf)> {
    let mut command = std::process::Command::new(env!("CARGO"));
    command
        .args(["metadata", "--no-deps", "--format-version=1", "--locked"])
        .current_dir(repo_root());
    let output = run_command(&mut command)?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let metadata: sonic_rs::Value = sonic_rs::from_slice(&output.stdout)?;
    let directory = |field: &str| {
        metadata
            .get(field)
            .and_then(sonic_rs::Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| format!("cargo metadata omitted {field}"))
    };
    Ok((
        directory("target_directory")?,
        directory("build_directory")?,
    ))
}

/// The profile directory holding the running test executable: `<profile>` in
/// `<root>/<profile>/deps/<test>`, tolerating a test binary placed directly
/// in the profile directory.
fn test_profile_dir(exe: &Path) -> TestResult<&Path> {
    let exe_dir = exe
        .parent()
        .ok_or("the test binary has no parent directory")?;
    if exe_dir.file_name().is_some_and(|name| name == "deps") {
        exe_dir
            .parent()
            .ok_or_else(|| "the test binary has no profile directory".into())
    } else {
        Ok(exe_dir)
    }
}

/// Loads the layered config a host start needs, from the same data root and
/// user TOML the product build used.
///
/// # Errors
/// Returns the config-load failure.
pub fn host_config(
    data: &Path,
    user: &str,
) -> Result<dal_core::Config, Box<dyn std::error::Error + Send + Sync>> {
    let factory = dalgona::product();
    Ok(dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        data,
        factory.defaults,
        Some(user),
    )?)
}

/// Starts a dalgona `Host` on `root` with default configuration.
///
/// # Errors
/// Returns config-load, product-build, or host-start failures.
pub async fn start_dalgona(root: PathBuf) -> TestResult<dal_agent::Host> {
    start_dalgona_with_config(root, None).await
}

/// Starts a dalgona `Host` on `root` with optional user config.
///
/// # Errors
/// Returns config-load, product-build, or host-start failures.
pub async fn start_dalgona_with_config(
    root: PathBuf,
    user_toml: Option<&str>,
) -> TestResult<dal_agent::Host> {
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        &root,
        factory.defaults,
        user_toml,
    )?;
    let cx = dalgon::BuildCx {
        data_root: root.clone(),
        config: &config,
    };
    let product = (factory.build)(&cx)?;
    let env = dal_agent::Env {
        vars: BTreeMap::new(),
        cwd: root.clone(),
        sandbox_helper: None,
    };
    Ok(dal_agent::Host::start(product, config, env).await?)
}

/// One scripted provider step that streams `text` and ends the turn.
#[must_use]
pub fn text_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":{}}},{{"type":"tool_calls_done","calls":[]}},{{"type":"usage","usage":{USAGE}}},{{"type":"stop","reason":"end_turn"}}]}}"#,
        json_string(text)
    )
}

/// One scripted provider step that calls a single tool with `args_json`.
#[must_use]
pub fn tool_step(id: &str, name: &str, args_json: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":{id},"name":{name}}},{{"type":"tool_calls_done","calls":[{{"id":{id},"name":{name},"args":{{"kind":"parsed","value":{args_json}}}}}]}},{{"type":"usage","usage":{USAGE}}},{{"type":"stop","reason":"tool_use"}}]}}"#,
        id = json_string(id),
        name = json_string(name),
    )
}

const USAGE: &str = r#"{"input_tokens":12,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}"#;

fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            control if control.is_control() => {
                let _ = write!(quoted, "\\u{:04x}", u32::from(control));
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// Runs `dalgona -p` against a scripted provider in a temporary data root and
/// returns the session journal, one record per line.
///
/// `top_level_toml` is appended to `dal.toml` before the provider table, so it
/// can hold top-level keys such as `disabled_batteries`.
///
/// # Errors
/// Returns setup failures, a failed `dalgona -p` run, or a journal count other
/// than one.
pub fn run_scripted_print(
    scratch: &Scratch,
    top_level_toml: &str,
    steps: &[String],
    prompt: &str,
) -> TestResult<String> {
    let config_home = scratch.path().join("xdg-config");
    let data_home = scratch.path().join("xdg-data");
    std::fs::create_dir_all(config_home.join("dalgona"))?;
    std::fs::create_dir_all(&data_home)?;
    let fixture = scratch.path().join("scripted.jsonl");
    std::fs::write(&fixture, steps.join("\n") + "\n")?;
    std::fs::write(
        config_home.join("dalgona/dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\n{top_level_toml}\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;
    let output = run_command(
        std::process::Command::new(dalgona_binary()?)
            .args(["-p", "--json", "--approval", "all", prompt])
            .current_dir(scratch.path())
            .env_clear()
            .env("HOME", scratch.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home),
    )?;
    if !output.status.success() {
        return Err(format!(
            "dalgona -p failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    read_one_journal(&data_home.join("dalgona/sessions"))
}

/// Reads the one `journal.jsonl` under `dir`.
///
/// # Errors
/// Returns a collection failure or a journal count other than one.
fn read_one_journal(dir: &Path) -> TestResult<String> {
    let mut journals = Vec::new();
    collect_journals(dir, &mut journals)?;
    match journals.as_slice() {
        [journal] => Ok(std::fs::read_to_string(journal)?),
        other => Err(format!("expected one session journal, found {}", other.len()).into()),
    }
}

fn collect_journals(dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_journals(&path, found)?;
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            found.push(path);
        }
    }
    Ok(())
}

/// The journal records of one kind, in order.
#[must_use]
pub fn journal_records<'a>(journal: &'a str, kind: &str) -> Vec<&'a str> {
    let marker = format!(r#""type":"{kind}""#);
    journal
        .lines()
        .filter(|line| line.contains(&marker))
        .collect()
}

/// The journal of the one session a [`Pane::start`] run created under the
/// scratch XDG data root.
///
/// # Errors
/// Returns a read failure or a journal count other than one.
pub fn session_journal(scratch: &Scratch) -> TestResult<String> {
    read_one_journal(&scratch.path().join("xdg-data/dalgona/sessions"))
}

/// A real `dalgona` running in a tmux pane on a fresh install, so a gate can
/// read the screen a person sees and press keys. tmux is a prerequisite of
/// the gate that uses it; its absence fails the gate with that name.
#[cfg(unix)]
pub struct Pane {
    socket: String,
    scratch: Scratch,
}

#[cfg(unix)]
impl Pane {
    /// Starts `dalgona --screen inline` in a 110 by 32 pane over a fresh
    /// config, data root, and workspace, with a scripted provider and
    /// approval mode `ask`.
    ///
    /// # Errors
    /// Returns setup failures, a missing binary, or a missing tmux.
    pub fn start(scratch: Scratch) -> TestResult<Self> {
        Self::start_with(scratch, "")
    }

    /// Like [`Pane::start`], with `top_level` config lines placed before the
    /// provider table, for a gate that isolates one battery's behavior.
    ///
    /// # Errors
    /// Returns setup failures, a missing binary, or a missing tmux.
    pub fn start_with(scratch: Scratch, top_level: &str) -> TestResult<Self> {
        let config = format!("model = \"openai-responses/gpt-6\"\napproval = \"ask\"\n{top_level}");
        Self::start_scripted(scratch, &config, &[text_step("reply")])
    }

    /// Starts the pane with the supplied config and provider responses.
    ///
    /// # Errors
    /// Returns setup failures, a missing binary, or a missing tmux.
    pub fn start_scripted(scratch: Scratch, config: &str, steps: &[String]) -> TestResult<Self> {
        let home = scratch.path().to_path_buf();
        let config_home = home.join("xdg-config");
        let data_home = home.join("xdg-data");
        let workspace = home.join("work");
        std::fs::create_dir_all(config_home.join("dalgona"))?;
        std::fs::create_dir_all(&data_home)?;
        std::fs::create_dir_all(&workspace)?;
        let fixture = home.join("scripted.jsonl");
        std::fs::write(&fixture, steps.join("\n") + "\n")?;
        std::fs::write(
            config_home.join("dalgona/dal.toml"),
            format!(
                "{config}[providers.scripted]\nfixture = {:?}\n",
                fixture.to_string_lossy()
            ),
        )?;
        let pane = Self {
            socket: format!("dalgona-gate-{}", uuid::Uuid::now_v7()),
            scratch,
        };
        let output = pane.tmux(&[
            "new-session",
            "-d",
            "-s",
            "s",
            "-x",
            "110",
            "-y",
            "32",
            "-c",
            &workspace.to_string_lossy(),
            "env",
            "-i",
            &format!("HOME={}", home.display()),
            &format!("XDG_CONFIG_HOME={}", config_home.display()),
            &format!("XDG_DATA_HOME={}", data_home.display()),
            "NO_COLOR=1",
            "DAL_NO_MOTION=1",
            "TERM=tmux-256color",
            &format!("PATH={}", std::env::var("PATH").unwrap_or_default()),
            &dalgona_binary()?.to_string_lossy(),
            "--screen",
            "inline",
        ])?;
        if !output.status.success() {
            return Err(format!(
                "tmux could not start the pane: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(pane)
    }

    /// The scratch root of this run.
    #[must_use]
    pub fn scratch(&self) -> &Scratch {
        &self.scratch
    }

    fn tmux(&self, args: &[&str]) -> TestResult<std::process::Output> {
        let mut command = std::process::Command::new("tmux");
        command.args(["-L", &self.socket]).args(args);
        run_command(&mut command).map_err(|error| {
            format!("this gate needs tmux on PATH, which failed to run: {error}").into()
        })
    }

    /// The pane text, scrollback included, one row per line.
    ///
    /// # Errors
    /// Returns tmux failures.
    pub fn text(&self) -> TestResult<String> {
        let output = self.tmux(&["capture-pane", "-p", "-J", "-S", "-", "-t", "s"])?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Types `text` literally, with no key names.
    ///
    /// # Errors
    /// Returns tmux failures.
    pub fn type_text(&self, text: &str) -> TestResult<()> {
        self.tmux(&["send-keys", "-t", "s", "-l", text])?;
        Ok(())
    }

    /// Presses Enter.
    ///
    /// # Errors
    /// Returns tmux failures.
    pub fn enter(&self) -> TestResult<()> {
        self.tmux(&["send-keys", "-t", "s", "Enter"])?;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

// === Stress-scenario completion: process census, report files, subscriber ledger ===
//
// The V10 process-group census proves real OS disappearance after cancel
// without a production PID API: each gate `exec` command self-reports its
// shell PID (and a background sibling sleep PID) into a temp report file,
// and the gate reads those files back and probes the OS process table.
// Unix uses the safe `rustix` process APIs (`test_kill_process`,
// `test_kill_process_group`, `getpgid`); only `ESRCH` proves absence while
// `EPERM` proves the process still exists. Windows uses in-box PowerShell
// `Get-Process`, which never throws for protected processes on id lookup.

/// Subdirectory of the gate workspace holding `*.pids` self-reports.
pub const STRESS_PIDS_DIR: &str = ".pids";

/// Whether the minutes-long stress scenarios are opted in.
#[must_use]
pub fn stress_enabled() -> bool {
    std::env::var_os("DALGONA_ORCH_STRESS").is_some()
}

/// Emits the machine-checkable completion marker of one stress scenario.
/// The CI workflow greps for every marker so no scenario can pass silently.
/// Lives in each test target (not the library) to keep `eprintln!` out of
/// library code.
#[must_use]
pub fn scenario_marker(marker: &str) -> String {
    format!("stress-scenario-ok: {marker}")
}

/// Renders `text` as one JSON string literal.
#[must_use]
pub fn json_text(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            control if control.is_control() => {
                let _ = std::fmt::Write::write_fmt(
                    &mut quoted,
                    format_args!("\\u{:04x}", u32::from(control)),
                );
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// Renders `path` as one POSIX shell word: forward slashes (also on
/// Windows, where Git Bash reads them) wrapped in single quotes.
#[must_use]
pub fn shell_word(path: &Path) -> String {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "'\\''");
    format!("'{text}'")
}

/// The `*.pids` report file of one labelled gate command.
#[must_use]
pub fn report_file(reports: &Path, label: &str) -> PathBuf {
    reports.join(format!("{label}.pids"))
}

/// One self-reporting gate command: backgrounds a sibling `sleep`, writes
/// the shell PID and the sibling PID atomically into `file`, then runs
/// `tail`. The caller chooses `tail` so the command keeps its authored
/// classifier shape (a trailing `sleep 30` still classifies as a sleep
/// wait; a `; exec sleep N` tail does not).
#[must_use]
pub fn tree_report_command(file: &Path, sibling_secs: u64, tail: &str) -> String {
    // The `true;` prefix keeps the sleep classifier from seeing a leading
    // sleep: only an intended trailing `sleep 30` may classify, never the
    // census sibling. Without it the rewrite would clamp a parked exec to
    // the 5 s sleep-wait window and the cancel would find nothing live.
    // On Windows the shell runs under Git Bash, where `$$` is the MSYS pid,
    // invisible to the Win32 table: `/proc/<msys>/winpid` translates it.
    #[cfg(windows)]
    {
        format!(
            "true; sleep {sibling_secs} & echo \"$(cat /proc/$$/winpid) $(cat /proc/$!/winpid)\" > {}; {tail}",
            shell_word(file)
        )
    }
    #[cfg(not(windows))]
    {
        format!(
            "true; sleep {sibling_secs} & echo \"$$ $!\" > {}; {tail}",
            shell_word(file)
        )
    }
}

/// One process tree a gate command self-reported.
#[derive(Clone, Debug)]
pub struct ReportedProcess {
    /// The report label (`held-03-1`, `cancel-02-4`, ...).
    pub label: String,
    /// The OS PIDs the shell wrote: itself, then its background sibling.
    pub pids: Vec<i32>,
    /// The shell's process group on Unix; `None` on Windows and when the
    /// group lookup raced a reaped shell.
    pub pgid: Option<i32>,
}

/// Reads every complete `*.pids` report in `reports`: sorted by label.
/// A file whose content lacks a terminating newline is still being written
/// and is skipped. A file with no parseable PID is a gate failure, never
/// silence.
///
/// # Errors
/// Returns the directory-read failure or the first malformed report.
pub fn read_process_reports(reports: &Path) -> TestResult<Vec<ReportedProcess>> {
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(reports) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "pids") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        if !text.ends_with('\n') {
            continue;
        }
        let pids: Vec<i32> = text
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<i32>, _>>()
            .map_err(|_| format!("{} holds no PIDs: {text:?}", path.display()))?;
        if pids.is_empty() {
            return Err(format!("{} holds no PIDs", path.display()).into());
        }
        let label = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let pgid = process_group_of(pids[0]);
        found.push(ReportedProcess { label, pids, pgid });
    }
    found.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(found)
}

/// The group of `pid` on Unix; `None` when the process is already reaped.
///
/// # Errors
/// Never fails: any lookup error other than absence maps to `None` because
/// the shell may exit between the report write and this read.
#[cfg(unix)]
fn process_group_of(pid: i32) -> Option<i32> {
    let target = rustix::process::Pid::from_raw(pid)?;
    rustix::process::getpgid(Some(target))
        .ok()
        .map(rustix::process::Pid::as_raw_pid)
}

/// Windows has no process-group identity for the census: every report
/// carries `None`, and liveness is per-PID only.
#[cfg(windows)]
fn process_group_of(_pid: i32) -> Option<i32> {
    None
}

/// Counts the complete `*.pids` reports in `reports` without parsing PIDs,
/// for handshake loops that must not park the runtime.
#[must_use]
pub fn count_process_reports(reports: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(reports) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pids"))
        .filter(|entry| {
            std::fs::read_to_string(entry.path())
                .is_ok_and(|text| text.ends_with('\n') && !text.trim().is_empty())
        })
        .count()
}

/// Blocks (on the calling thread) until `reports` holds `expected` complete
/// reports, then reads and returns them. This uses plain thread sleep so a
/// paused-tokio test never parks its runtime while waiting.
///
/// # Errors
/// Returns a timeout naming the directory and the count seen, or a
/// malformed report.
pub fn await_process_reports(
    reports: &Path,
    expected: usize,
    timeout: std::time::Duration,
) -> TestResult<Vec<ReportedProcess>> {
    let start = std::time::Instant::now();
    loop {
        if count_process_reports(reports) >= expected {
            let read = read_process_reports(reports)?;
            if read.len() == expected {
                return Ok(read);
            }
            return Err(format!(
                "{} holds {} reports, expected {expected}",
                reports.display(),
                read.len()
            )
            .into());
        }
        if start.elapsed() >= timeout {
            return Err(format!(
                "only {} of {expected} process reports arrived in {:?} under {}",
                count_process_reports(reports),
                timeout,
                reports.display()
            )
            .into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Whether one PID names a live process: `true` on `EPERM` (exists, no
/// permission), `false` only on `ESRCH`.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    let Some(target) = rustix::process::Pid::from_raw(pid) else {
        return false;
    };
    match rustix::process::test_kill_process(target) {
        // A zombie still answers kill but is already dead; on Linux the
        // /proc state says so.
        Ok(()) => !is_zombie(pid),
        Err(rustix::io::Errno::SRCH) => false,
        Err(_) => true,
    }
}

/// Whether `pid` is a zombie on Linux: present in the table but dead.
/// Always `false` off Linux, where no cheap state read exists.
#[cfg(unix)]
fn is_zombie(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let fields: Vec<&str> = text
            .rsplit(')')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        fields.first().is_some_and(|state| state.starts_with('Z'))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

/// Whether one process group still has a member: `true` on `EPERM`,
/// `false` only on `ESRCH`. Linux censuses groups through `/proc` instead.
#[cfg(all(unix, not(target_os = "linux")))]
fn group_alive(pgid: i32) -> bool {
    let Some(target) = rustix::process::Pid::from_raw(pgid) else {
        return false;
    };
    match rustix::process::test_kill_process_group(target) {
        Ok(()) => true,
        Err(rustix::io::Errno::SRCH) => false,
        Err(_) => true,
    }
}

/// The live process groups on Linux, read from `/proc` in one pass so a
/// 220-group census costs one scan, not 220. Zombies never count as live.
#[cfg(target_os = "linux")]
fn live_groups_linux() -> std::collections::BTreeSet<i32> {
    let mut live = std::collections::BTreeSet::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return live;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.bytes().any(|byte| !byte.is_ascii_digit()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(format!("/proc/{name}/stat")) else {
            continue;
        };
        // `comm` may hold spaces and parens; the state and friends follow
        // the last `)`.
        let after: Vec<&str> = text
            .rsplit(')')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        // Fields after comm: state, ppid, pgrp, ...
        if after.first().is_some_and(|state| state.starts_with('Z')) {
            continue;
        }
        // Fields after comm: state, ppid, pgrp, session, ...
        if let Some(pgrp) = after.get(2).and_then(|text| text.parse().ok()) {
            live.insert(pgrp);
        }
    }
    live
}

/// Every reported PID and process group still alive, in one OS pass.
#[derive(Clone, Debug, Default)]
pub struct Survivors {
    /// Reported PIDs still alive.
    pub pids: Vec<i32>,
    /// Reported process groups (Unix) still holding a member.
    pub groups: Vec<i32>,
}

impl Survivors {
    /// Whether no reported PID and no reported group survives.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pids.is_empty() && self.groups.is_empty()
    }

    /// How many reported identities survive in total.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pids.len() + self.groups.len()
    }
}

/// Reads which reported processes and groups are still alive, in one OS
/// pass. `EPERM` counts as alive; only `ESRCH` (Unix) or an empty
/// `Get-Process` hit (Windows) counts as gone.
///
/// # Errors
/// Returns the PowerShell spawn/wait failure on Windows; Unix never fails.
pub fn survivors(reported: &[ReportedProcess]) -> TestResult<Survivors> {
    #[cfg(unix)]
    {
        let mut out = Survivors::default();
        #[cfg(target_os = "linux")]
        let live_groups = live_groups_linux();
        let groups: std::collections::BTreeSet<i32> =
            reported.iter().filter_map(|report| report.pgid).collect();
        #[cfg(target_os = "linux")]
        {
            for group in &groups {
                if live_groups.contains(group) {
                    out.groups.push(*group);
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            for group in &groups {
                if group_alive(*group) {
                    out.groups.push(*group);
                }
            }
        }
        for report in reported {
            for pid in &report.pids {
                if pid_alive(*pid) {
                    out.pids.push(*pid);
                }
            }
        }
        Ok(out)
    }
    #[cfg(windows)]
    {
        let mut ids: Vec<i32> = Vec::new();
        for report in reported {
            ids.extend(report.pids.iter().copied());
        }
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Ok(Survivors::default());
        }
        let list = ids.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
        // One PowerShell process per census: `Get-Process -Id` lists by id
        // and never throws for protected processes on a bare id lookup.
        // Only a missing id raises, caught below per id.
        let script = format!(
            "$missing = @(); foreach ($id in {list}) {{ try {{ $null = Get-Process -Id $id -ErrorAction Stop }} catch [Microsoft.PowerShell.Commands.ProcessCommandException] {{ $missing += $id }} }}; $missing -join ','"
        );
        let output = run_command(std::process::Command::new("powershell.exe").args([
            "-NoProfile",
            "-Command",
            &script,
        ]))?;
        if !output.status.success() {
            return Err(format!(
                "Get-Process census failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        let missing: std::collections::BTreeSet<i32> = String::from_utf8_lossy(&output.stdout)
            .split([',', '\r', '\n', ' '])
            .filter_map(|token| token.trim().parse().ok())
            .collect();
        let mut out = Survivors::default();
        for id in ids {
            if !missing.contains(&id) {
                out.pids.push(id);
            }
        }
        Ok(out)
    }
}

/// Fails unless every reported PID and (on Unix) every reported group is
/// alive: the nonvacuous gate before a cancel, so a later disappearance
/// proof means something.
///
/// # Errors
/// Returns the census failure or the survivor shortfall.
pub fn assert_all_alive(reported: &[ReportedProcess]) -> TestResult<Survivors> {
    let live = survivors(reported)?;
    let want_pids: usize = reported.iter().map(|report| report.pids.len()).sum();
    let want_groups: usize = {
        let mut groups = std::collections::BTreeSet::new();
        for report in reported {
            if let Some(pgid) = report.pgid {
                groups.insert(pgid);
            }
        }
        groups.len()
    };
    // Windows has no process-group identity to assert.
    let want_groups = {
        #[cfg(unix)]
        {
            want_groups
        }
        #[cfg(windows)]
        {
            let _ = want_groups;
            0
        }
    };
    if live.pids.len() != want_pids || live.groups.len() != want_groups {
        return Err(format!(
            "only {}/{} PIDs and {}/{} groups alive before cancel; missing proves nothing",
            live.pids.len(),
            want_pids,
            live.groups.len(),
            want_groups
        )
        .into());
    }
    Ok(live)
}

/// Polls [`survivors`] until nothing reported survives, or `grace` after
/// `from` passes. Returns when the census that found nothing *ended*.
/// Census latency is measurement resolution, not budget: the caller asserts
/// the returned end time against its own deadline.
///
/// # Errors
/// Returns the census failure, or the identities still alive past the grace.
pub fn wait_until_gone(
    reported: &[ReportedProcess],
    from: std::time::Instant,
    grace: std::time::Duration,
) -> TestResult<std::time::Instant> {
    loop {
        let live = survivors(reported)?;
        let ended = std::time::Instant::now();
        if live.is_empty() {
            return Ok(ended);
        }
        if ended.duration_since(from) > grace {
            let mut pids = live.pids;
            pids.sort_unstable();
            let mut groups = live.groups;
            groups.sort_unstable();
            return Err(format!(
                "{} PIDs and {} groups still alive {:?} after cancel: pids {pids:?}, groups {groups:?}",
                pids.len(),
                groups.len(),
                ended.duration_since(from)
            )
            .into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// How often a slow subscriber test drains: the 250 ms of V9 T9.
pub const SUBSCRIBER_PERIOD: std::time::Duration = std::time::Duration::from_millis(250);
/// The product queue bound a slow subscriber must never overflow.
pub const SUBSCRIBER_QUEUE_CAP: usize = 1024;
/// The V9 T9 per-job update rate bound, per job per second.
pub const MAX_JOB_UPDATES_PER_SEC: usize = 2;

/// The synchronous accounting of one slow session subscriber: every 250 ms
/// the test drains everything queued, records the largest single drain,
/// counts `Resync` deliveries, and tracks the worst per-job update rate in
/// any sliding one-second window. The async drain loop lives in the test;
/// this ledger only records what it saw.
#[derive(Debug, Default)]
pub struct SubscriberLedger {
    /// How many 250 ms drains ran.
    pub drains: usize,
    /// How many deliveries were drained in total.
    pub deliveries: usize,
    /// How many drained deliveries the pump site classified as
    /// workload-relevant: job start/settle updates and detached-`exec`
    /// results on the fast200 reader, tool settle/turn end on the scale500
    /// and v9 readers.
    pub relevant: usize,
    /// The largest single drain.
    pub largest_drain: usize,
    /// How many `Resync` deliveries arrived; must stay zero.
    pub resyncs: usize,
    /// Whether the stream ended before the test stopped it.
    pub ended_early: bool,
    /// Per-job update instants, for the sliding-window rate.
    updates_by_job:
        std::collections::HashMap<String, std::collections::VecDeque<std::time::Instant>>,
    /// The worst per-job updates seen in any one-second window.
    pub worst_job_rate: usize,
}

impl SubscriberLedger {
    /// Records one drain of `count` deliveries, of which `resync` were
    /// resyncs, `jobs` name updated jobs, and `relevant` carried the
    /// pump site's workload-relevant event.
    pub fn record_drain(
        &mut self,
        count: usize,
        resync: usize,
        jobs: &[String],
        relevant: usize,
        at: std::time::Instant,
    ) {
        self.drains += 1;
        self.deliveries += count;
        self.relevant += relevant;
        self.largest_drain = self.largest_drain.max(count);
        self.resyncs += resync;
        for job in jobs {
            let window = self.updates_by_job.entry(job.clone()).or_default();
            window.push_back(at);
            while window
                .front()
                .is_some_and(|first| at.duration_since(*first).as_secs() >= 1)
            {
                window.pop_front();
            }
            self.worst_job_rate = self.worst_job_rate.max(window.len());
        }
    }

    /// Records that the subscription stream ended before the stop flag.
    pub fn record_early_end(&mut self) {
        self.ended_early = true;
    }

    /// One-line summary for the CI log.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "drains={} deliveries={} relevant={} largest_drain={} resyncs={} worst_job_rate={}/s ended_early={}",
            self.drains,
            self.deliveries,
            self.relevant,
            self.largest_drain,
            self.resyncs,
            self.worst_job_rate,
            self.ended_early
        )
    }

    /// Fails on any `Resync`, an early stream end, zero observed deliveries,
    /// zero workload-relevant deliveries, or a per-job rate above the V9
    /// T9 bound. `deliveries` counts every `Update` (including turn-startup
    /// noise), so `relevant` — classified per pump site (job updates and
    /// detached-`exec` results on the fast200 reader; tool settle/turn end
    /// on the scale500 and v9 readers) — is the load proof: the drain loop
    /// records a final
    /// (possibly empty) drain on the stop flag, so `drains > 0` alone
    /// cannot tell full pressure from no flow, and one startup update
    /// cannot stand in for workload. The drain-size bound is reported, not
    /// asserted: a drain can legally exceed the queue cap when production
    /// outpaces the 250 ms poll, and only a `Resync` proves the queue
    /// overflowed.
    ///
    /// For `exec` pumps, the rate window names only `JobStarted`/`JobSettled`
    /// updates, which those workloads never emit; `updates_by_job` is empty and
    /// `worst_job_rate == 0`. The bound is vacuous there; other gates carry the
    /// load proof.
    ///
    /// # Errors
    /// Returns the violated bound.
    pub fn verdict(&self) -> TestResult<()> {
        if self.drains == 0 {
            return Err("the slow subscriber never drained".into());
        }
        if self.deliveries == 0 {
            return Err("the slow subscriber saw no deliveries at all".into());
        }
        if self.relevant == 0 {
            return Err("the slow subscriber saw no workload-relevant deliveries".into());
        }
        if self.resyncs > 0 {
            return Err(format!("the slow subscriber took {} Resync", self.resyncs).into());
        }
        if self.ended_early {
            return Err("the slow subscriber stream ended before the stop flag".into());
        }
        if self.worst_job_rate > MAX_JOB_UPDATES_PER_SEC {
            return Err(format!(
                "one job took {} updates in a second, above {MAX_JOB_UPDATES_PER_SEC}",
                self.worst_job_rate
            )
            .into());
        }
        Ok(())
    }
}

/// The directory holding one session's `journal.jsonl`: the child session
/// keeps its own journal beside the parent's, found by session id.
///
/// # Errors
/// Never fails; returns `None` when no journal matches.
#[must_use]
pub fn session_dir(data: &Path, session: &str) -> Option<PathBuf> {
    let mut journals = Vec::new();
    collect_session_journals(&data.join("sessions"), &mut journals);
    journals.into_iter().find_map(|path| {
        if path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == session)
        {
            path.parent().map(Path::to_path_buf)
        } else {
            None
        }
    })
}

/// The `jobs/reports.jsonl` ledger beside one session's journal.
///
/// # Errors
/// Never fails; returns `None` when the session directory is unknown.
#[must_use]
pub fn session_ledger(data: &Path, session: &str) -> Option<PathBuf> {
    session_dir(data, session).map(|dir| dir.join("jobs").join("reports.jsonl"))
}

fn collect_session_journals(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_session_journals(&path, found);
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            found.push(path);
        }
    }
}
