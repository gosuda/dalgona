//! Sandbox launcher setup: canonical roots, exact setup errors, and profiles.
//!
//! Plan 3737 fixes the canonical root order, 3747-3755 the exact setup
//! errors, and 3851 the fail-closed startup rule. The Linux `__sandbox`
//! helper itself and the macOS Seatbelt enforcement belong to the subagent
//! and sandbox part; this module only resolves roots, probes the helper,
//! and renders the profile text the launcher consumes.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt::{self, Display},
    path::{Path, PathBuf},
    process::Command,
};

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
    /// `sandbox_writable` entries in config order.
    pub sandbox_writable: &'a [Box<str>],
    /// dal/dalgona config and data roots a writable path must not overlap.
    pub protected_roots: &'a [PathBuf],
    /// The Linux helper path; `None` fails closed when sandboxing is on.
    /// macOS's Seatbelt path never reads it.
    #[cfg_attr(
        target_os = "macos",
        expect(
            dead_code,
            reason = "only the Linux Landlock probe reads the helper path"
        )
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

/// Resolves the session launcher, failing closed before any child starts.
pub(crate) fn resolve_launcher(inputs: &SandboxInputs<'_>) -> Result<Launcher, SandboxSetupError> {
    if !inputs.sandbox_on {
        return Ok(Launcher::Direct);
    }
    #[cfg(windows)]
    {
        return Err(SandboxSetupError::new(
            "sandbox = \"on\" is not supported on Windows. Set sandbox = \"off\" in config.toml, or run dalgon inside WSL 2.",
        ));
    }
    #[cfg(not(windows))]
    {
        let roots = resolve_roots(inputs)?;
        #[cfg(target_os = "macos")]
        {
            if !Path::new("/usr/bin/sandbox-exec").exists() {
                return Err(SandboxSetupError::new(
                    "sandbox = \"on\" needs /usr/bin/sandbox-exec, and it is missing. Set sandbox = \"off\" in config.toml.",
                ));
            }
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
        return Ok(Launcher::Direct);
    }
    let home = platform_home(vars).ok_or_else(|| {
        SandboxSetupError::new(
            "sandbox: HOME is not set, so the sandbox cannot resolve its writable roots. Set HOME, or set sandbox = \"off\" in config.toml.",
        )
    })?;
    let cache = platform_cache(vars, &home);
    resolve_launcher(&SandboxInputs {
        sandbox_on: true,
        workspace_root,
        home: &home,
        cache: &cache,
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
    let roots = resolve_roots(&SandboxInputs {
        sandbox_on: true,
        workspace_root,
        home: &home,
        cache: &cache,
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
    if let Ok(temp) = std::fs::canonicalize(std::env::temp_dir()) {
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
                "sandbox: the writable path {} contains the home directory {}, so the sandbox would protect nothing. Start dalgon in a project directory, or set sandbox = \"off\" in config.toml.",
                canonical.display(),
                home.display(),
            )));
        }
        for protected in inputs.protected_roots {
            if canonical == *protected
                || canonical.starts_with(protected)
                || protected.starts_with(&canonical)
            {
                return Err(SandboxSetupError::new(format!(
                    "sandbox: the writable path {} overlaps {}, where dalgon and dalgona keep their config and plugins. A sandboxed command could change them and escape the sandbox at the next start. Remove the path, start dalgon elsewhere, or set sandbox = \"off\" in config.toml.",
                    canonical.display(),
                    protected.display(),
                )));
            }
        }
        push_unique(canonical, &mut roots);
    }
    Ok(roots)
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
        vars.get(OsStr::new("USERPROFILE")).map(PathBuf::from)
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

/// Renders the Seatbelt profile with exactly the four plan forms in order,
/// one escaped root rule per canonical root.
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

/// Builds the sandboxed argv for one platform; macOS returns the profile
/// path the caller must keep until that child exits.
pub(crate) fn sandbox_argv(
    target: &OsString,
    target_args: &[OsString],
    job: &JobId,
    helper: Option<&Path>,
    roots: &[PathBuf],
) -> Result<(OsString, Vec<OsString>, Option<PathBuf>), ToolError> {
    #[cfg(windows)]
    {
        let _ = (target, target_args, job, helper, roots);
        return Err(ToolError::message(
            "sandbox = \"on\" is not supported on Windows. Set sandbox = \"off\" in config.toml, or run dalgon inside WSL 2.",
        ));
    }
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
    #[cfg(all(not(windows), not(target_os = "macos")))]
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
        let temp = canonical_or_raw(&std::env::temp_dir());
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
                "sandbox: the writable path {} contains the home directory {}, so the sandbox would protect nothing. Start dalgon in a project directory, or set sandbox = \"off\" in config.toml.",
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

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_profile_uses_exact_forms_with_escaped_roots() {
        let profile = seatbelt_profile(&[
            PathBuf::from("/work/project"),
            PathBuf::from("/tmp/we\"ird\\path"),
        ]);
        assert_eq!(
            profile,
            "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* (subpath \"/work/project\"))\n(allow file-write* (subpath \"/tmp/we\\\"ird\\\\path\"))\n",
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
}
