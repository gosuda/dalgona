//! Sandbox launcher setup: canonical roots, exact setup errors, and profiles.
//!
//! Canonical roots use a fixed order, setup errors are rendered exactly, and
//! startup fails closed. The Linux `__sandbox`
//! helper itself and the macOS Seatbelt enforcement belong to the subagent
//! and sandbox part; this module only resolves roots, probes the helper,
//! and renders the profile text the launcher consumes.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt::{self, Display},
    path::{Path, PathBuf},
};

#[cfg(all(not(windows), not(target_os = "macos")))]
use std::process::Command;

use dal_core::JobId;

use super::Launcher;
use crate::error::ToolError;

/// Setup inputs; every path decision stays inside this module so callers
/// pass values instead of reconstructing the session layout with joins.
pub(crate) struct SandboxInputs<'a> {
    /// Whether `sandbox = "on"` was configured.
    pub sandbox_on: bool,
    /// The canonical session workspace root.
    pub workspace_root: &'a Path,
    /// The user's home directory.
    pub home: &'a Path,
    /// The platform cache directory.
    pub cache: &'a Path,
    /// The process temporary directory captured for this launch.
    pub temp: PathBuf,
    /// `sandbox_writable` entries in config order.
    pub sandbox_writable: &'a [Box<str>],
    /// dal/dalgona config and data roots a writable path must not overlap.
    pub protected_roots: &'a [PathBuf],
    /// The sandbox helper path; `None` fails closed when sandboxing is on.
    /// macOS's Seatbelt path never reads it.
    #[cfg_attr(
        target_os = "macos",
        expect(dead_code, reason = "only the probe paths read the helper path")
    )]
    pub helper: Option<&'a Path>,
}

/// A sandbox setup failure carrying the exact plan text.
#[derive(Clone, Debug)]
pub(crate) struct SandboxSetupError {
    message: Box<str>,
}

impl SandboxSetupError {
    fn new(message: impl Into<Box<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Converts the exact setup text into the tool error surface.
    pub(crate) fn tool_error(&self) -> ToolError {
        ToolError::message(self.message.clone())
    }
}

impl Display for SandboxSetupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SandboxSetupError {}

/// Fails closed when the macOS Seatbelt utility is absent.
///
/// When the sandbox is enabled, commands run under macOS Seatbelt, which
/// requires the system utility `/usr/bin/sandbox-exec`.
#[cfg(target_os = "macos")]
fn require_seatbelt() -> Result<(), SandboxSetupError> {
    require_seatbelt_at(Path::new("/usr/bin/sandbox-exec"))
}

/// The existence check behind [`require_seatbelt`], kept separate so tests
/// can exercise the contract against a path the host controls.
#[cfg(target_os = "macos")]
fn require_seatbelt_at(seatbelt: &Path) -> Result<(), SandboxSetupError> {
    if !seatbelt.exists() {
        return Err(SandboxSetupError::new(
            "sandbox: command launches need /usr/bin/sandbox-exec, and it is missing.",
        ));
    }
    Ok(())
}

/// Resolves the session launcher, failing closed before any child starts.
pub(crate) fn resolve_launcher(inputs: &SandboxInputs<'_>) -> Result<Launcher, SandboxSetupError> {
    if !inputs.sandbox_on {
        // Direct launches stay under the mandatory lifetime policy on
        // macOS, so the Seatbelt utility is required here as well.
        #[cfg(target_os = "macos")]
        require_seatbelt()?;
        return Ok(Launcher::Direct);
    }
    let roots = resolve_roots(inputs)?;
    // The overlap refusal above precedes platform availability, so a
    // protected root under an automatic grant reports the overlap on every
    // platform even where the Seatbelt utility is absent.
    #[cfg(target_os = "macos")]
    require_seatbelt()?;
    #[cfg(windows)]
    {
        let helper = inputs.helper.ok_or_else(|| {
            SandboxSetupError::new(
                "sandbox: no sandbox helper. SDK embedders must pass a helper path; the dalgon binary provides dalgon __sandbox.",
            )
        })?;
        Ok(Launcher::Sandbox {
            helper: Some(helper.to_path_buf()),
            roots: roots.into_boxed_slice(),
        })
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "macos")]
        {
            require_seatbelt()?;
            Ok(Launcher::Sandbox {
                helper: None,
                roots: roots.into_boxed_slice(),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let helper = inputs.helper.ok_or_else(|| {
                SandboxSetupError::new(
                    "sandbox: no sandbox helper. SDK embedders must pass a helper path; the dalgon binary provides dalgon __sandbox.",
                )
            })?;
            let abi = probe_landlock_abi(helper)?;
            if abi < 3 {
                return Err(abi_error(abi));
            }
            Ok(Launcher::Sandbox {
                helper: Some(helper.to_path_buf()),
                roots: roots.into_boxed_slice(),
            })
        }
    }
}

/// Resolves the launcher for one session from the host snapshot, failing
/// closed: sandbox off runs directly, an unresolvable setup refuses every
/// spawn with the typed setup error instead of ever running unsandboxed.
pub(crate) fn session_launcher(
    config: &dal_core::Config,
    vars: &BTreeMap<OsString, OsString>,
    workspace_root: &Path,
    protected_roots: &[PathBuf],
    helper: Option<&Path>,
) -> Result<Launcher, SandboxSetupError> {
    if !config.sandbox() {
        // Direct launches stay under the mandatory lifetime policy on
        // macOS, so the Seatbelt utility is required here as well.
        #[cfg(target_os = "macos")]
        require_seatbelt()?;
        return Ok(Launcher::Direct);
    }
    let home = platform_home(vars).ok_or_else(|| {
        SandboxSetupError::new(
            "sandbox: HOME is not set, so the sandbox cannot resolve its writable roots. Set HOME, or set sandbox = \"off\" in dal.toml.",
        )
    })?;
    let cache = platform_cache(vars, &home);
    let temp = platform_temp(vars, &home);
    resolve_launcher(&SandboxInputs {
        sandbox_on: true,
        workspace_root,
        home: &home,
        cache: &cache,
        temp,
        sandbox_writable: config.sandbox_writable(),
        protected_roots,
        helper,
    })
}

/// Resolves this session's canonical writable roots and renders the
/// structured session-start notice; `None` when the sandbox is off or the
/// roots cannot resolve — the launcher reports the same failure on spawn.
#[must_use]
pub fn sandbox_notice(
    on: bool,
    vars: &BTreeMap<OsString, OsString>,
    workspace_root: &Path,
    writable: &[Box<str>],
    protected_roots: &[PathBuf],
) -> Option<Box<str>> {
    if !on {
        return None;
    }
    let home = platform_home(vars)?;
    let cache = platform_cache(vars, &home);
    let temp = platform_temp(vars, &home);
    let roots = resolve_roots(&SandboxInputs {
        sandbox_on: true,
        workspace_root,
        home: &home,
        cache: &cache,
        temp,
        sandbox_writable: writable,
        protected_roots,
        helper: None,
    })
    .ok()?;
    let mut text = String::from("Sandbox on. Commands can write only under: ");
    for (index, root) in roots.iter().enumerate() {
        if index > 0 {
            text.push_str(", ");
        }
        text.push_str(&root.display().to_string());
    }
    text.push('.');
    Some(text.into())
}

/// Resolves canonical roots in plan order: workspace, temp, cache, then
/// `sandbox_writable` entries. Duplicates keep the first occurrence; a
/// missing path is never created.
pub(crate) fn resolve_roots(inputs: &SandboxInputs<'_>) -> Result<Vec<PathBuf>, SandboxSetupError> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let push_unique = |path: PathBuf, roots: &mut Vec<PathBuf>| {
        if !roots.contains(&path) {
            roots.push(path);
        }
    };
    let workspace = std::fs::canonicalize(inputs.workspace_root)
        .map_err(|source| cannot_open(&inputs.workspace_root.to_string_lossy(), &source))?;
    push_unique(workspace, &mut roots);
    // Automatic roots that do not exist are skipped: a missing path is
    // never created, and omitting it denies it like a raw path would while
    // the Linux helper can still open every root it is handed.
    if let Ok(temp) = std::fs::canonicalize(&inputs.temp) {
        push_unique(temp, &mut roots);
    }
    if let Ok(cache) = std::fs::canonicalize(inputs.cache) {
        push_unique(cache, &mut roots);
    }
    let home = canonical_or_raw(inputs.home);
    for raw in inputs.sandbox_writable {
        let expanded = expand_writable(raw, &home)?;
        let canonical =
            std::fs::canonicalize(&expanded).map_err(|source| cannot_open(raw, &source))?;
        if home.starts_with(&canonical) {
            return Err(SandboxSetupError::new(format!(
                "sandbox: the writable path {} contains the home directory {}, so the sandbox would protect nothing. Start dalgon in a project directory, or set sandbox = \"off\" in dal.toml.",
                canonical.display(),
                home.display(),
            )));
        }
        push_unique(canonical, &mut roots);
    }
    // No launcher may receive an automatic or configured writable root
    // that contains protected state, or that lies inside protected state.
    let protected = inputs
        .protected_roots
        .iter()
        .map(|root| canonicalize_protected_root(root))
        .collect::<Result<Vec<_>, _>>()?;
    for root in &roots {
        for guarded in &protected {
            if root == guarded || root.starts_with(guarded) || guarded.starts_with(root) {
                return Err(SandboxSetupError::new(format!(
                    "sandbox: the writable path {} overlaps {}, where dalgon and dalgona keep their config and plugins. A sandboxed command could change them and escape the sandbox at the next start. Change the workspace, temporary directory, cache, or sandbox_writable entry, or set sandbox = \"off\" in dal.toml.",
                    root.display(),
                    guarded.display(),
                )));
            }
        }
    }
    Ok(roots)
}

/// Resolves an existing protected root or its deepest existing ancestor.
/// A missing tail is preserved, so a not-yet-created data directory cannot
/// escape overlap validation through a symlinked parent.
fn canonicalize_protected_root(root: &Path) -> Result<PathBuf, SandboxSetupError> {
    if !root.is_absolute() {
        return Err(SandboxSetupError::new(format!(
            "sandbox: protected root {} must be absolute",
            root.display(),
        )));
    }
    let mut current = root;
    let mut missing: Vec<&OsStr> = Vec::new();
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut canonical) => {
                if !missing.is_empty() && !canonical.is_dir() {
                    return Err(SandboxSetupError::new(format!(
                        "sandbox: protected root {} has a non-directory parent",
                        root.display(),
                    )));
                }
                canonical.extend(missing.into_iter().rev());
                return Ok(canonical);
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(current).is_ok() {
                    return Err(cannot_open(&root.to_string_lossy(), &source));
                }
                let Some(name) = current.file_name() else {
                    return Err(cannot_open(&root.to_string_lossy(), &source));
                };
                let Some(parent) = current.parent() else {
                    return Err(cannot_open(&root.to_string_lossy(), &source));
                };
                missing.push(name);
                current = parent;
            }
            Err(source) => return Err(cannot_open(&root.to_string_lossy(), &source)),
        }
    }
}

/// Expands one writable entry: absolute stays, a leading `~/` resolves
/// against home, anything else is rejected with its exact error.
fn expand_writable(raw: &str, home: &Path) -> Result<PathBuf, SandboxSetupError> {
    if let Some(rest) = raw.strip_prefix("~/") {
        return Ok(home.join(rest));
    }
    if Path::new(raw).is_absolute() {
        return Ok(PathBuf::from(raw));
    }
    Err(SandboxSetupError::new(format!(
        "sandbox_writable: \"{raw}\" must be an absolute path or start with \"~/\".",
    )))
}

/// Canonicalizes an existing path, keeping the raw form when the
/// filesystem cannot be read; automatic roots fail open to a denial
/// (a raw root never prefix-matches a canonical cwd) rather than failing
/// the whole session start.
fn canonical_or_raw(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn cannot_open(path: &str, source: &std::io::Error) -> SandboxSetupError {
    SandboxSetupError::new(format!("sandbox: cannot open root {path}: {source}"))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn abi_error(number: impl Display) -> SandboxSetupError {
    SandboxSetupError::new(format!(
        "sandbox = \"on\" needs Landlock ABI 3 (Linux 6.1 or newer); this kernel reports ABI {number}."
    ))
}

/// Probes the helper; it must print one decimal ABI followed by `\n`.
/// One transient spawn failure is retried once; a second failure still
/// refuses with the unknown-ABI text, so the probe keeps failing closed.
#[cfg(all(not(windows), not(target_os = "macos")))]
#[expect(
    clippy::disallowed_methods,
    reason = "the ABI probe is a synchronous startup diagnostic of the host-provided helper binary, not a tool child; the checked async spawn door cannot serve it"
)]
fn probe_landlock_abi(helper: &Path) -> Result<u32, SandboxSetupError> {
    let output = match Command::new(helper)
        .arg("__sandbox")
        .arg("--probe")
        .output()
    {
        Ok(output) => output,
        Err(_) => Command::new(helper)
            .arg("__sandbox")
            .arg("--probe")
            .output()
            .map_err(|_| abi_error("unknown"))?,
    };
    if !output.status.success() {
        return Err(abi_error("unknown"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(token) = stdout.strip_suffix('\n') else {
        return Err(abi_error("unknown"));
    };
    if token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_digit()) {
        let shown = if token.is_empty() { "unknown" } else { token };
        return Err(abi_error(shown));
    }
    token.parse::<u32>().map_err(|_| abi_error(token))
}

/// Reads the home directory from the captured variables, never the ambient
/// process environment.
pub(crate) fn platform_home(vars: &BTreeMap<OsString, OsString>) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let absolute = |name: &str| {
            vars.get(OsStr::new(name))
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        for name in ["HOME", "USERPROFILE"] {
            if let Some(path) = absolute(name) {
                return Some(path);
            }
        }
        if let (Some(drive), Some(path)) = (
            vars.get(OsStr::new("HOMEDRIVE")),
            vars.get(OsStr::new("HOMEPATH")),
        ) {
            let mut combined = drive.clone();
            combined.push(path);
            let combined = PathBuf::from(combined);
            if combined.is_absolute() {
                return Some(combined);
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        vars.get(OsStr::new("HOME")).map(PathBuf::from)
    }
}

/// Resolves the platform cache directory from the captured variables:
/// `XDG_CACHE_HOME` only when absolute and non-empty, else `~/.cache`
/// (macOS `~/Library/Caches`).
pub(crate) fn platform_cache(vars: &BTreeMap<OsString, OsString>, home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let _ = vars;
        home.join("Library/Caches")
    }
    #[cfg(not(target_os = "macos"))]
    {
        match vars.get(OsStr::new("XDG_CACHE_HOME")) {
            Some(dir) if !dir.is_empty() && Path::new(dir).is_absolute() => PathBuf::from(dir),
            _ => home.join(".cache"),
        }
    }
}

/// Resolves the temporary directory from the captured variables, never the
/// ambient process environment: `TMPDIR` then `/tmp` on Unix, `TMP` then
/// `TEMP` then the captured home's `AppData/Local/Temp` on Windows. Only
/// absolute non-empty entries are honored.
pub(crate) fn platform_temp(vars: &BTreeMap<OsString, OsString>, home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        for name in ["TMP", "TEMP"] {
            if let Some(dir) = vars.get(OsStr::new(name)) {
                if !dir.is_empty() && Path::new(dir).is_absolute() {
                    return PathBuf::from(dir);
                }
            }
        }
        home.join("AppData").join("Local").join("Temp")
    }
    #[cfg(not(windows))]
    {
        let _ = home;
        if let Some(dir) = vars.get(OsStr::new("TMPDIR"))
            && !dir.is_empty()
            && Path::new(dir).is_absolute()
        {
            return PathBuf::from(dir);
        }
        PathBuf::from("/tmp")
    }
}

/// Kernel-enforced restrictions inherited by every macOS child, including
/// processes launched when the filesystem sandbox is disabled.
/// A process may not leave the process group that the host owns.
#[cfg(target_os = "macos")]
const MACOS_LIFETIME_RULES: &str = concat!(
    "(deny syscall-unix (syscall-number SYS_setsid))\n",
    "(deny syscall-unix (syscall-number SYS_setpgid))\n",
);

/// Renders the Seatbelt filesystem profile and mandatory lifetime rules.
#[cfg(target_os = "macos")]
pub(crate) fn seatbelt_profile(roots: &[PathBuf]) -> String {
    use std::fmt::Write as _;
    let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    for root in roots {
        let _ = writeln!(
            profile,
            "(allow file-write* (subpath \"{}\"))",
            escape_sbpl(root),
        );
    }
    profile.push_str(MACOS_LIFETIME_RULES);
    profile
}

/// Escapes one SBPL string path; raw path bytes are never interpolated.
#[cfg(target_os = "macos")]
fn escape_sbpl(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Builds the exact-text error for a Seatbelt profile write failure.
#[cfg(target_os = "macos")]
fn seatbelt_write_error(path: &Path, source: &std::io::Error) -> ToolError {
    ToolError::Failed(Box::new(std::io::Error::new(
        source.kind(),
        format!(
            "sandbox: cannot write Seatbelt profile {}: {source}",
            path.display()
        ),
    )))
}

/// Writes a unique mode-0600 Seatbelt profile for one child; the caller
/// keeps the path until that child exits and removes it on every exit path.
#[cfg(target_os = "macos")]
pub(crate) fn write_seatbelt_profile(job: &JobId, roots: &[PathBuf]) -> Result<PathBuf, ToolError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let path = std::env::temp_dir().join(format!("dalgon-seatbelt-{job}.sb"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|source| seatbelt_write_error(&path, &source))?;
    file.write_all(seatbelt_profile(roots).as_bytes())
        .map_err(|source| seatbelt_write_error(&path, &source))?;
    Ok(path)
}

/// Wraps a macOS command in lifetime-only Seatbelt restrictions.
///
/// The normal filesystem sandbox remains disabled for direct launches, but
/// detaching from the host-owned process group is never permitted.
#[cfg(target_os = "macos")]
pub(crate) fn lifetime_argv(
    target: &OsString,
    target_args: &[OsString],
) -> (OsString, Vec<OsString>, Option<PathBuf>) {
    let mut args = vec![
        OsString::from("-p"),
        OsString::from(format!(
            "(version 1)\n(allow default)\n{MACOS_LIFETIME_RULES}"
        )),
        OsString::from("--"),
        target.clone(),
    ];
    args.extend(target_args.iter().cloned());
    (OsString::from("/usr/bin/sandbox-exec"), args, None)
}

/// Builds the sandboxed argv for one platform; macOS returns the profile
/// path the caller must keep until that child exits.
pub(crate) fn sandbox_argv(
    target: &OsString,
    target_args: &[OsString],
    job: &JobId,
    helper: Option<&Path>,
    roots: &[PathBuf],
) -> Result<(OsString, Vec<OsString>, Option<PathBuf>), ToolError> {
    #[cfg(target_os = "macos")]
    {
        let _ = helper;
        let profile = write_seatbelt_profile(job, roots)?;
        let mut args = vec![
            OsString::from("-f"),
            profile.as_os_str().to_owned(),
            OsString::from("--"),
            target.clone(),
        ];
        args.extend(target_args.iter().cloned());
        Ok((OsString::from("/usr/bin/sandbox-exec"), args, Some(profile)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = job;
        let helper = helper.ok_or_else(|| {
            ToolError::message(
                "sandbox: no sandbox helper. SDK embedders must pass a helper path; the dalgon binary provides dalgon __sandbox.",
            )
        })?;
        let mut args = vec![OsString::from("__sandbox"), OsString::from("--allow")];
        args.extend(roots.iter().map(|root| root.as_os_str().to_owned()));
        args.push(OsString::from("--"));
        args.push(target.clone());
        args.extend(target_args.iter().cloned());
        Ok((OsString::from(helper), args, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(
        workspace: &'a Path,
        home: &'a Path,
        cache: &'a Path,
        writable: &'a [Box<str>],
        protected: &'a [PathBuf],
        helper: Option<&'a Path>,
    ) -> SandboxInputs<'a> {
        SandboxInputs {
            sandbox_on: true,
            workspace_root: workspace,
            home,
            cache,
            temp: home.join("temp"),
            sandbox_writable: writable,
            protected_roots: protected,
            helper,
        }
    }

    #[test]
    fn sandbox_off_selects_direct_without_touching_paths() {
        let inputs = SandboxInputs {
            sandbox_on: false,
            workspace_root: Path::new("/nonexistent-workspace"),
            home: Path::new("/nonexistent-home"),
            cache: Path::new("/nonexistent-cache"),
            temp: PathBuf::from("/nonexistent-temp"),
            sandbox_writable: &[],
            protected_roots: &[],
            helper: None,
        };
        assert!(matches!(resolve_launcher(&inputs), Ok(Launcher::Direct)));
    }

    #[test]
    fn roots_follow_plan_order_and_drop_duplicates() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let cache = home.path().join("cache");
        std::fs::create_dir(&cache).expect("cache dir");
        let temp = home.path().join("temp");
        std::fs::create_dir(&temp).expect("temp dir");
        let extra = tempfile::tempdir().expect("extra writable");
        let writable: Vec<Box<str>> = vec![
            "~/extra".into(),
            extra.path().to_string_lossy().into(),
            extra.path().to_string_lossy().into(),
        ];
        std::fs::create_dir(home.path().join("extra")).expect("tilde extra dir");
        let inputs = inputs(workspace.path(), home.path(), &cache, &writable, &[], None);
        let roots = resolve_roots(&inputs).expect("roots resolve");
        let workspace_root = std::fs::canonicalize(workspace.path()).expect("canonical");
        let temp = std::fs::canonicalize(&temp).expect("canonical temp");
        let cache_root = std::fs::canonicalize(&cache).expect("canonical cache");
        let extra_root = std::fs::canonicalize(extra.path()).expect("canonical extra");
        assert_eq!(roots[0], workspace_root);
        assert!(roots.contains(&temp));
        assert!(roots.contains(&cache_root));
        assert_eq!(roots.iter().filter(|root| *root == &extra_root).count(), 1);
        assert!(
            roots
                .iter()
                .position(|root| *root == workspace_root)
                .expect("order")
                < roots
                    .iter()
                    .position(|root| *root == extra_root)
                    .expect("order")
        );
    }

    #[test]
    fn relative_writable_is_rejected_with_exact_text() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let writable: Vec<Box<str>> = vec!["relative/path".into()];
        let inputs = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &writable,
            &[],
            None,
        );
        let error = resolve_roots(&inputs).expect_err("relative root rejected");
        assert_eq!(
            error.to_string(),
            "sandbox_writable: \"relative/path\" must be an absolute path or start with \"~/\".",
        );
    }

    #[test]
    fn missing_writable_reports_cannot_open() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let missing = home.path().join("missing");
        let writable: Vec<Box<str>> = vec![missing.to_string_lossy().into()];
        let inputs = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &writable,
            &[],
            None,
        );
        let error = resolve_roots(&inputs).expect_err("missing root rejected");
        assert!(
            error.to_string().starts_with(&format!(
                "sandbox: cannot open root {}: ",
                missing.to_string_lossy(),
            )),
            "unexpected: {error}",
        );
    }

    #[test]
    fn writable_covering_home_is_rejected_with_exact_text() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let writable: Vec<Box<str>> = vec![home.path().to_string_lossy().into()];
        let inputs = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &writable,
            &[],
            None,
        );
        let error = resolve_roots(&inputs).expect_err("home-covering root rejected");
        let home_root = std::fs::canonicalize(home.path()).expect("canonical home");
        assert_eq!(
            error.to_string(),
            format!(
                "sandbox: the writable path {} contains the home directory {}, so the sandbox would protect nothing. Start dalgon in a project directory, or set sandbox = \"off\" in dal.toml.",
                home_root.display(),
                home_root.display(),
            ),
        );
    }

    #[test]
    fn writable_overlapping_protected_is_rejected_both_directions() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let protected = tempfile::tempdir().expect("protected");
        let protected_root = std::fs::canonicalize(protected.path()).expect("canonical");
        let guarded = [protected_root.clone()];
        // Writable root containing the protected root.
        let writable: Vec<Box<str>> = vec![protected.path().to_string_lossy().into()];
        let setup = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &writable,
            &guarded,
            None,
        );
        let error = resolve_roots(&setup).expect_err("overlap rejected");
        assert!(
            error.to_string().starts_with(&format!(
                "sandbox: the writable path {} overlaps {}",
                protected_root.display(),
                protected_root.display(),
            )),
            "unexpected: {error}",
        );
        // Writable root inside the protected root.
        let inside = protected.path().join("plugins");
        std::fs::create_dir(&inside).expect("inside dir");
        let writable: Vec<Box<str>> = vec![inside.to_string_lossy().into()];
        let inner = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &writable,
            &guarded,
            None,
        );
        let error = resolve_roots(&inner).expect_err("inner overlap rejected");
        assert!(
            error.to_string().contains("overlaps"),
            "unexpected: {error}"
        );
    }

    fn assert_overlap_rejected(setup: &SandboxInputs<'_>, writable: &Path, guarded: &Path) {
        let error = resolve_launcher(setup).expect_err("overlapping root must block launch");
        let writable = std::fs::canonicalize(writable).expect("canonical writable");
        let guarded = std::fs::canonicalize(guarded).expect("canonical protected");
        assert!(
            error.to_string().starts_with(&format!(
                "sandbox: the writable path {} overlaps {}",
                writable.display(),
                guarded.display(),
            )),
            "unexpected: {error}",
        );
    }

    #[test]
    fn automatic_workspace_equal_parent_and_child_of_protected_are_rejected() {
        let tree = tempfile::tempdir().expect("tree");
        let home = tempfile::tempdir().expect("home");
        let protected = tree.path().join("data");
        let child = protected.join("plugins");
        std::fs::create_dir_all(&child).expect("protected tree");
        let guarded = [protected.clone()];

        for workspace in [protected.as_path(), tree.path(), child.as_path()] {
            let setup = inputs(workspace, home.path(), home.path(), &[], &guarded, None);
            assert_overlap_rejected(&setup, workspace, &protected);
        }
    }

    #[test]
    fn automatic_temp_and_cache_containing_protected_are_rejected() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let temp = tempfile::tempdir().expect("temp");
        let cache = tempfile::tempdir().expect("cache");

        for automatic in [temp.path(), cache.path()] {
            let protected = automatic.join("credentials");
            std::fs::create_dir(&protected).expect("protected directory");
            let guarded = [protected.clone()];
            let mut setup = inputs(
                workspace.path(),
                home.path(),
                cache.path(),
                &[],
                &guarded,
                None,
            );
            setup.temp = temp.path().to_path_buf();
            assert_overlap_rejected(&setup, automatic, &protected);
        }
    }

    #[test]
    fn platform_temp_prefers_captured_vars_over_ambient() {
        let home = tempfile::tempdir().expect("home");
        let captured = tempfile::tempdir().expect("captured temp");
        let vars = BTreeMap::from([
            (
                OsString::from("TMPDIR"),
                captured.path().as_os_str().to_owned(),
            ),
            (
                OsString::from("TMP"),
                captured.path().as_os_str().to_owned(),
            ),
            (
                OsString::from("TEMP"),
                captured.path().as_os_str().to_owned(),
            ),
        ]);
        assert_eq!(platform_temp(&vars, home.path()), captured.path());
    }

    #[cfg(not(windows))]
    #[test]
    fn platform_temp_on_unix_uses_tmpdir_then_tmp() {
        let home = tempfile::tempdir().expect("home");
        let captured = tempfile::tempdir().expect("captured temp");
        assert_eq!(
            platform_temp(&BTreeMap::new(), home.path()),
            PathBuf::from("/tmp"),
        );
        for invalid in [OsString::from(""), OsString::from("relative/tmp")] {
            let vars = BTreeMap::from([(OsString::from("TMPDIR"), invalid)]);
            assert_eq!(platform_temp(&vars, home.path()), PathBuf::from("/tmp"));
        }
        let vars = BTreeMap::from([
            (
                OsString::from("TMP"),
                captured.path().as_os_str().to_owned(),
            ),
            (
                OsString::from("TEMP"),
                captured.path().as_os_str().to_owned(),
            ),
        ]);
        assert_eq!(platform_temp(&vars, home.path()), PathBuf::from("/tmp"));
        let vars = BTreeMap::from([(
            OsString::from("TMPDIR"),
            captured.path().as_os_str().to_owned(),
        )]);
        assert_eq!(platform_temp(&vars, home.path()), captured.path());
    }

    #[cfg(windows)]
    #[test]
    fn platform_temp_on_windows_uses_tmp_then_temp_then_home() {
        let home = tempfile::tempdir().expect("home");
        let first = tempfile::tempdir().expect("first temp");
        let second = tempfile::tempdir().expect("second temp");
        let fallback = home.path().join("AppData").join("Local").join("Temp");
        assert_eq!(platform_temp(&BTreeMap::new(), home.path()), fallback);
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let vars = BTreeMap::from([(
            OsString::from("TMPDIR"),
            elsewhere.path().as_os_str().to_owned(),
        )]);
        assert_eq!(platform_temp(&vars, home.path()), fallback);
        let vars = BTreeMap::from([
            (OsString::from("TMP"), OsString::from("relative/tmp")),
            (OsString::from("TEMP"), second.path().as_os_str().to_owned()),
        ]);
        assert_eq!(platform_temp(&vars, home.path()), second.path());
        let vars = BTreeMap::from([
            (OsString::from("TMP"), first.path().as_os_str().to_owned()),
            (OsString::from("TEMP"), second.path().as_os_str().to_owned()),
        ]);
        assert_eq!(platform_temp(&vars, home.path()), first.path());
    }

    #[cfg(unix)]
    #[test]
    fn platform_temp_accepts_non_unicode_tmpdir() {
        use std::os::unix::ffi::OsStringExt as _;

        let home = tempfile::tempdir().expect("home");
        let mut bytes = home.path().as_os_str().as_encoded_bytes().to_vec();
        bytes.extend_from_slice(b"/tmp-\xff");
        let dir = OsString::from_vec(bytes);
        assert!(Path::new(&dir).is_absolute());
        let expected = PathBuf::from(&dir);
        let vars = BTreeMap::from([(OsString::from("TMPDIR"), dir)]);
        assert_eq!(platform_temp(&vars, home.path()), expected);
    }

    #[cfg(unix)]
    #[test]
    fn missing_protected_tail_beneath_symlink_is_still_guarded() {
        let tree = tempfile::tempdir().expect("tree");
        let home = tempfile::tempdir().expect("home");
        let actual = tree.path().join("real");
        let alias = tree.path().join("alias");
        std::fs::create_dir(&actual).expect("actual dir");
        std::os::unix::fs::symlink(&actual, &alias).expect("alias");
        let guarded = [alias.join("missing/data")];
        let setup = inputs(&actual, home.path(), home.path(), &[], &guarded, None);
        let error = resolve_roots(&setup).expect_err("symlinked protected root must block");
        let canonical = std::fs::canonicalize(&actual).expect("canonical actual");
        assert!(
            error.to_string().contains(&format!(
                "overlaps {}",
                canonical.join("missing/data").display(),
            )),
            "unexpected: {error}",
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn sandbox_argv_routes_through_helper_or_refuses_without_one() {
        let job = JobId::new_v7();
        let target = OsString::from("sh");
        let target_args = [OsString::from("-c"), OsString::from("true")];
        let roots = [PathBuf::from("/work/project")];
        let refused = sandbox_argv(&target, &target_args, &job, None, &roots)
            .expect_err("a missing helper refuses the spawn");
        assert_eq!(
            refused.to_string(),
            "sandbox: no sandbox helper. SDK embedders must pass a helper path; the dalgon binary provides dalgon __sandbox.",
        );
        let helper = Path::new("/opt/dalgon/bin/dalgon");
        let (program, args, profile) =
            sandbox_argv(&target, &target_args, &job, Some(helper), &roots)
                .expect("a helper resolves the sandboxed argv");
        assert_eq!(program, OsString::from("/opt/dalgon/bin/dalgon"));
        assert_eq!(profile, None);
        let text: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            text,
            vec![
                "__sandbox".to_string(),
                "--allow".to_string(),
                "/work/project".to_string(),
                "--".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ],
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn missing_seatbelt_fails_closed_for_both_launch_modes() {
        let error = require_seatbelt_at(Path::new("/dal-probe/missing-seatbelt"))
            .expect_err("a missing utility is rejected");
        assert_eq!(
            error.to_string(),
            "sandbox: command launches need /usr/bin/sandbox-exec, and it is missing.",
        );
        require_seatbelt_at(Path::new("/")).expect("an existing path resolves");
    }

    #[test]
    fn session_launcher_sandbox_off_returns_direct_without_seatbelt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = dal_core::Config::load(
            dal_core::ConfigProduct::Dalgon,
            temp.path(),
            "",
            Some("sandbox = false"),
        )
        .expect("config");
        let vars = BTreeMap::new();
        let launcher = session_launcher(
            &config,
            &vars,
            Path::new("/nonexistent-workspace"),
            &[],
            None,
        );
        assert!(matches!(launcher, Ok(Launcher::Direct)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_profile_uses_exact_forms_with_escaped_roots() {
        let profile = seatbelt_profile(&[
            PathBuf::from("/work/project"),
            PathBuf::from("/tmp/we\"ird\\path"),
        ]);
        assert_eq!(
            profile,
            "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* (subpath \"/work/project\"))\n(allow file-write* (subpath \"/tmp/we\\\"ird\\\\path\"))\n(deny syscall-unix (syscall-number SYS_setsid))\n(deny syscall-unix (syscall-number SYS_setpgid))\n",
        );
    }

    #[cfg(target_os = "linux")]
    fn probe_helper_script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join("fake-sandbox");
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).expect("helper script");
        let mut permissions = std::fs::metadata(&path).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");
        path
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_accepts_abi_3_and_reports_lower() {
        let dir = tempfile::tempdir().expect("helpers");
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let empty: Vec<Box<str>> = vec![];
        let good = probe_helper_script(dir.path(), "printf '3\\n'");
        assert_eq!(probe_landlock_abi(&good).expect("abi 3"), 3);
        let stale_dir = tempfile::tempdir().expect("old helpers");
        let old = probe_helper_script(stale_dir.path(), "printf '1\\n'");
        assert_eq!(probe_landlock_abi(&old).expect("abi 1"), 1);
        let stale = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &empty,
            &[],
            Some(&old),
        );
        let error = resolve_launcher(&stale).expect_err("abi 1 fails closed");
        assert_eq!(
            error.to_string(),
            "sandbox = \"on\" needs Landlock ABI 3 (Linux 6.1 or newer); this kernel reports ABI 1.",
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_on_probes_helper_and_fails_closed_without_one() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let empty: Vec<Box<str>> = vec![];
        let no_helper = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &empty,
            &[],
            None,
        );
        let error = resolve_launcher(&no_helper).expect_err("missing helper fails closed");
        assert_eq!(
            error.to_string(),
            "sandbox: no sandbox helper. SDK embedders must pass a helper path; the dalgon binary provides dalgon __sandbox.",
        );
        let dir = tempfile::tempdir().expect("helpers");
        let helper = probe_helper_script(dir.path(), "printf '3\\n'");
        let with_helper = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &empty,
            &[],
            Some(&helper),
        );
        let launcher = resolve_launcher(&with_helper).expect("abi 3 resolves");
        assert!(matches!(launcher, Launcher::Sandbox { .. }));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_rejects_malformed_outputs_with_exact_fail_closed_text() {
        const UNKNOWN: &str = "sandbox = \"on\" needs Landlock ABI 3 (Linux 6.1 or newer); this kernel reports ABI unknown.";
        // A nonzero exit never admits the launcher.
        let failing_dir = tempfile::tempdir().expect("failing helper");
        let failing = probe_helper_script(failing_dir.path(), "exit 1");
        assert_eq!(
            probe_landlock_abi(&failing)
                .expect_err("nonzero exit fails closed")
                .to_string(),
            UNKNOWN,
        );
        // Output without the trailing newline is not one decimal line.
        let bare_dir = tempfile::tempdir().expect("bare helper");
        let bare = probe_helper_script(bare_dir.path(), "printf '3'");
        assert_eq!(
            probe_landlock_abi(&bare)
                .expect_err("missing newline fails closed")
                .to_string(),
            UNKNOWN,
        );
        // A non-decimal token reports itself, exactly.
        let text_dir = tempfile::tempdir().expect("text helper");
        let text = probe_helper_script(text_dir.path(), "printf 'v3\\n'");
        assert_eq!(
            probe_landlock_abi(&text)
                .expect_err("non-decimal output fails closed")
                .to_string(),
            "sandbox = \"on\" needs Landlock ABI 3 (Linux 6.1 or newer); this kernel reports ABI v3.",
        );
        // An empty line reports the unknown ABI, not an empty one.
        let empty_dir = tempfile::tempdir().expect("empty helper");
        let empty = probe_helper_script(empty_dir.path(), "printf '\\n'");
        assert_eq!(
            probe_landlock_abi(&empty)
                .expect_err("empty output fails closed")
                .to_string(),
            UNKNOWN,
        );
        // A decimal that overflows u32 reports itself, exactly.
        let wide_dir = tempfile::tempdir().expect("wide helper");
        let wide = probe_helper_script(wide_dir.path(), "printf '4294967296\\n'");
        assert_eq!(
            probe_landlock_abi(&wide)
                .expect_err("u32 overflow fails closed")
                .to_string(),
            "sandbox = \"on\" needs Landlock ABI 3 (Linux 6.1 or newer); this kernel reports ABI 4294967296.",
        );
        // An unspawnable helper exhausts its one retry, then fails closed.
        let missing = failing_dir.path().join("no-such-helper");
        assert_eq!(
            probe_landlock_abi(&missing)
                .expect_err("unspawnable helper fails closed")
                .to_string(),
            UNKNOWN,
        );
    }

    #[test]
    fn session_launcher_without_home_fails_closed_with_exact_text() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = dal_core::Config::load(
            dal_core::ConfigProduct::Dalgon,
            temp.path(),
            "",
            Some("sandbox = true"),
        )
        .expect("config");
        let error = session_launcher(&config, &BTreeMap::new(), temp.path(), &[], None)
            .expect_err("sandbox on without HOME fails closed");
        assert_eq!(
            error.to_string(),
            "sandbox: HOME is not set, so the sandbox cannot resolve its writable roots. Set HOME, or set sandbox = \"off\" in dal.toml.",
        );
    }

    #[test]
    fn sandbox_notice_reports_roots_when_on_and_none_when_off_or_unresolvable() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let cache = tempfile::tempdir().expect("cache");
        let temp = tempfile::tempdir().expect("temp");
        let vars = BTreeMap::from([
            (OsString::from("HOME"), home.path().as_os_str().to_owned()),
            (
                OsString::from("XDG_CACHE_HOME"),
                cache.path().as_os_str().to_owned(),
            ),
            (OsString::from("TMPDIR"), temp.path().as_os_str().to_owned()),
            (OsString::from("TMP"), temp.path().as_os_str().to_owned()),
            (OsString::from("TEMP"), temp.path().as_os_str().to_owned()),
        ]);
        let empty: Vec<Box<str>> = vec![];
        let notice = sandbox_notice(true, &vars, workspace.path(), &empty, &[])
            .expect("resolvable roots render a notice");
        assert!(
            notice.starts_with("Sandbox on. Commands can write only under: "),
            "unexpected notice: {notice}",
        );
        let canonical = std::fs::canonicalize(workspace.path()).expect("canonical workspace");
        assert!(
            notice.contains(canonical.to_string_lossy().as_ref()),
            "the notice names the workspace root: {notice}",
        );
        let temp_canonical = std::fs::canonicalize(temp.path()).expect("canonical temp");
        assert!(
            notice.contains(temp_canonical.to_string_lossy().as_ref()),
            "the notice names the captured temp root: {notice}",
        );
        assert!(sandbox_notice(false, &vars, workspace.path(), &empty, &[]).is_none());
        assert!(sandbox_notice(true, &BTreeMap::new(), workspace.path(), &empty, &[]).is_none());
        assert!(
            sandbox_notice(
                true,
                &vars,
                Path::new("/nonexistent-workspace"),
                &empty,
                &[]
            )
            .is_none()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn platform_home_and_cache_follow_captured_vars() {
        let home = tempfile::tempdir().expect("home");
        let vars = BTreeMap::from([(OsString::from("HOME"), home.path().as_os_str().to_owned())]);
        assert_eq!(platform_home(&vars), Some(home.path().to_path_buf()));
        assert_eq!(platform_home(&BTreeMap::new()), None);
        let cache = tempfile::tempdir().expect("cache");
        let cached = BTreeMap::from([
            (OsString::from("HOME"), home.path().as_os_str().to_owned()),
            (
                OsString::from("XDG_CACHE_HOME"),
                cache.path().as_os_str().to_owned(),
            ),
        ]);
        assert_eq!(platform_cache(&cached, home.path()), cache.path());
        for invalid in [OsString::from(""), OsString::from("relative/cache")] {
            let vars = BTreeMap::from([(OsString::from("XDG_CACHE_HOME"), invalid)]);
            assert_eq!(
                platform_cache(&vars, home.path()),
                home.path().join(".cache"),
            );
        }
    }

    #[test]
    fn relative_protected_root_is_rejected_with_exact_text() {
        let workspace = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        let empty: Vec<Box<str>> = vec![];
        let guarded = [PathBuf::from("relative/root")];
        let setup = inputs(
            workspace.path(),
            home.path(),
            home.path(),
            &empty,
            &guarded,
            None,
        );
        let error = resolve_roots(&setup).expect_err("relative protected root rejected");
        assert_eq!(
            error.to_string(),
            "sandbox: protected root relative/root must be absolute",
        );
    }
}
