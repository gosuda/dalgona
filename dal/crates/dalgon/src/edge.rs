//! Process-edge snapshots, lexical paths, and platform identity.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub(crate) enum EdgeError {
    #[error("home directory is unavailable")]
    HomeMissing,
    #[error("workspace is not a directory: {0}")]
    Workspace(PathBuf),
    #[error("workspace path must be absolute")]
    WorkspaceRelative,
    #[error("{operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid product identity")]
    InvalidProduct,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RootPaths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalSnapshot {
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    pub stderr_tty: bool,
    pub width: usize,
}

/// Typed failures of the Windows current-user SID probe.
#[cfg(any(windows, test))]
#[derive(Debug, thiserror::Error)]
pub(crate) enum SidError {
    #[error("SystemRoot is not set to an absolute directory")]
    SystemRoot,
    #[error("cannot start whoami.exe: {0}")]
    Spawn(#[source] io::Error),
    #[error("whoami.exe exited with status {0:?}")]
    Failed(Option<i32>),
    #[error("whoami.exe returned non-UTF-8 output")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("whoami.exe returned malformed CSV")]
    Csv,
    #[error("whoami.exe did not return a valid current-user SID")]
    InvalidSid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CurrentUserSid(Box<str>);

impl CurrentUserSid {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Log verbosity selected once from `DAL_LOG`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogLevel {
    Error,
    Warning,
    Info,
    Debug,
}

/// Parses `DAL_LOG` (`error|warning|info|debug`, default `warning`).
///
/// Returns `Ok(None)` when absent and `Err(())` when present but unknown
/// (edge maps unknown to E7 without logging secrets).
pub(crate) fn parse_log_level(vars: &BTreeMap<OsString, OsString>) -> Result<Option<LogLevel>, ()> {
    let Some(value) = env_value(vars, "DAL_LOG") else {
        return Ok(None);
    };
    match value.to_str() {
        Some("error") => Ok(Some(LogLevel::Error)),
        Some("warning") => Ok(Some(LogLevel::Warning)),
        Some("info") => Ok(Some(LogLevel::Info)),
        Some("debug") => Ok(Some(LogLevel::Debug)),
        _ => Err(()),
    }
}

pub(crate) fn snapshot_environment() -> BTreeMap<OsString, OsString> {
    std::env::vars_os().collect()
}

pub(crate) fn process_cwd() -> io::Result<PathBuf> {
    #[expect(clippy::disallowed_methods, reason = "R4 edge")]
    std::env::current_dir()
}

/// Captures the current executable path for the `__sandbox` helper launch.
///
/// Returns `None` when the path is unresolvable; the sandbox backend fails
/// closed in that case instead of running direct.
pub(crate) fn current_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

pub(crate) fn terminal_snapshot() -> TerminalSnapshot {
    use std::io::IsTerminal as _;

    #[expect(clippy::disallowed_methods, reason = "R4 edge")]
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    let stderr_tty = std::io::stderr().is_terminal();
    let width = if stdout_tty {
        crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns))
    } else {
        80
    }
    .clamp(40, 100);
    TerminalSnapshot {
        stdin_tty,
        stdout_tty,
        stderr_tty,
        width,
    }
}

/// Waits for one shutdown signal and returns its `128 + signo` exit code.
///
/// POSIX watches SIGINT, SIGTERM, and SIGHUP; other platforms watch the
/// console break event. A signal whose registration fails is skipped.
#[expect(
    clippy::disallowed_methods,
    reason = "R4 edge: the process edge owns its shutdown signal"
)]
pub(crate) async fn shutdown_signal() -> u8 {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut hangup = signal(SignalKind::hangup()).ok();
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => 130,
            () = wait_for_signal(&mut terminate) => 143,
            () = wait_for_signal(&mut hangup) => 129,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        130
    }
}

#[cfg(unix)]
async fn wait_for_signal(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// Maps a binary alias to its product-family directory name.
/// `dalgon`, `dal`, and `dl` share the `dal` family; `dalgona` uses its own.
/// Returns `None` for unknown binaries, which the edge rejects before any
/// path work.
#[must_use]
pub(crate) fn family_for_binary(binary: &str) -> Option<&'static str> {
    match binary {
        "dalgon" | "dal" | "dl" => Some("dal"),
        "dalgona" => Some("dalgona"),
        _ => None,
    }
}

/// The user configuration file name inside the family config directory.
pub(crate) const CONFIG_FILE_NAME: &str = "dal.toml";

pub(crate) fn log_file_path(vars: &BTreeMap<OsString, OsString>, binary: &str) -> PathBuf {
    let family = family_for_binary(binary).unwrap_or("dal");
    let home = select_home(vars).unwrap_or_else(|_| PathBuf::from("/"));
    let data_base = absolute_env_path(vars, "XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local").join("share"));
    data_base
        .join(family)
        .join("cache")
        .join(format!("{family}.log"))
}

pub(crate) fn resolve_roots(
    vars: &BTreeMap<OsString, OsString>,
    binary: &str,
) -> Result<RootPaths, EdgeError> {
    let Some(family) = family_for_binary(binary) else {
        return Err(EdgeError::InvalidProduct);
    };
    let home = select_home(vars)?;
    let config_base =
        absolute_env_path(vars, "XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    let data_base = absolute_env_path(vars, "XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local").join("share"));
    let config = append_product(config_base, family)?;
    let data = append_product(data_base, family)?;
    Ok(RootPaths {
        home,
        cache: data.join("cache"),
        config,
        data,
    })
}

/// Returns the user configuration file path inside the resolved config root.
#[must_use]
pub(crate) fn config_file_path(roots: &RootPaths) -> PathBuf {
    roots.config.join(CONFIG_FILE_NAME)
}

/// Reads the user configuration file, if it exists.
/// A missing file is valid configuration and returns `None`; any other
/// read failure is a typed startup error naming the absolute path.
pub(crate) fn read_user_config(path: &Path) -> Result<Option<String>, EdgeError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(EdgeError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn select_home(vars: &BTreeMap<OsString, OsString>) -> Result<PathBuf, EdgeError> {
    #[cfg(windows)]
    {
        for name in ["HOME", "USERPROFILE"] {
            if let Some(path) = absolute_env_path(vars, name) {
                return Ok(path);
            }
        }
        if let (Some(drive), Some(path)) =
            (env_value(vars, "HOMEDRIVE"), env_value(vars, "HOMEPATH"))
        {
            let mut combined = drive.clone();
            combined.push(path);
            let home = PathBuf::from(combined);
            if home.is_absolute() {
                return Ok(home);
            }
        }
        Err(EdgeError::HomeMissing)
    }
    #[cfg(not(windows))]
    {
        absolute_env_path(vars, "HOME").ok_or(EdgeError::HomeMissing)
    }
}

fn env_value<'a>(vars: &'a BTreeMap<OsString, OsString>, name: &str) -> Option<&'a OsString> {
    vars.get(OsStr::new(name))
}

fn absolute_env_path(vars: &BTreeMap<OsString, OsString>, name: &str) -> Option<PathBuf> {
    let value = env_value(vars, name)?;
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

fn append_product(mut base: PathBuf, binary: &str) -> Result<PathBuf, EdgeError> {
    base.push(binary);
    if !base.is_absolute() {
        return Err(EdgeError::InvalidProduct);
    }
    Ok(base)
}

pub(crate) fn resolve_workspace_path(cwd: &Path, requested: Option<&Path>) -> PathBuf {
    let path = match requested {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => cwd.join(path),
        None => cwd.to_path_buf(),
    };
    lexical_normalize(&path)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if result.file_name().is_some() {
                    result.pop();
                } else if !result.is_absolute() {
                    result.push(component.as_os_str());
                }
            }
            Component::Normal(value) => result.push(value),
            Component::Prefix(_) | Component::RootDir => result.push(component.as_os_str()),
        }
    }
    result
}

pub(crate) fn validate_workspace(path: PathBuf) -> Result<dal_core::Workspace, EdgeError> {
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => {
            dal_core::Workspace::new(path).map_err(|_| EdgeError::WorkspaceRelative)
        }
        Ok(_) => Err(EdgeError::Workspace(path)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(EdgeError::Workspace(path)),
        Err(source) => Err(EdgeError::Io {
            operation: "read",
            path,
            source,
        }),
    }
}

/// Resolves the effective color choice for the interactive UI.
#[cfg(feature = "tui")]
pub(crate) fn resolve_color(
    requested: Option<crate::cli::ColorArg>,
    vars: &BTreeMap<OsString, OsString>,
    output_is_tty: bool,
) -> crate::cli::ColorArg {
    use crate::cli::ColorArg;

    match requested {
        Some(ColorArg::Always) => ColorArg::Always,
        Some(ColorArg::Never) => ColorArg::Never,
        Some(ColorArg::Auto) | None if env_value(vars, "NO_COLOR").is_some() => ColorArg::Never,
        Some(ColorArg::Auto) | None if env_value(vars, "FORCE_COLOR").is_some() => ColorArg::Always,
        Some(ColorArg::Auto) | None => {
            let term = env_value(vars, "TERM").and_then(|value| value.to_str());
            if term.is_none_or(|term| term == "dumb") || !output_is_tty {
                ColorArg::Never
            } else {
                ColorArg::Always
            }
        }
    }
}

/// Returns the current user's SID for naming the local RPC socket.
///
/// Windows probes `whoami.exe` and reports typed failures; every other
/// platform has no SID and always yields `None`.
#[cfg(windows)]
pub(crate) fn current_user_sid(
    vars: &BTreeMap<OsString, OsString>,
) -> Result<Option<CurrentUserSid>, SidError> {
    let system_root = env_value(vars, "SystemRoot").map(PathBuf::from);
    let Some(system_root) = system_root.filter(|path| path.is_absolute()) else {
        return Err(SidError::SystemRoot);
    };
    let executable = system_root.join("System32").join("whoami.exe");
    let output = run_whoami(&executable).map_err(SidError::Spawn)?;
    if !output.status.success() {
        return Err(SidError::Failed(output.status.code()));
    }
    let stdout = String::from_utf8(output.stdout)?;
    let mut lines = stdout.lines();
    let Some(line) = lines.next() else {
        return Err(SidError::Csv);
    };
    if lines.next().is_some() {
        return Err(SidError::Csv);
    }
    let fields = parse_csv_row(line)?;
    let sid = fields.get(1).ok_or(SidError::Csv)?;
    parse_sid(sid).map(Some)
}

#[cfg(not(windows))]
pub(crate) fn current_user_sid(vars: &BTreeMap<OsString, OsString>) -> Option<CurrentUserSid> {
    let _ = vars;
    None
}

#[cfg(windows)]
#[expect(clippy::disallowed_methods, reason = "R4 edge")]
fn run_whoami(executable: &Path) -> io::Result<std::process::Output> {
    std::process::Command::new(executable)
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
}

#[cfg(any(windows, test))]
fn parse_csv_row(row: &str) -> Result<Vec<String>, SidError> {
    let mut fields = Vec::with_capacity(2);
    let mut field = String::new();
    let mut chars = row.chars().peekable();
    let mut quoted = false;
    let mut closed_quote = false;
    while let Some(character) = chars.next() {
        match (quoted, closed_quote, character) {
            (true, _, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, _, '"') => {
                quoted = false;
                closed_quote = true;
            }
            (false, false, '"') if field.is_empty() => quoted = true,
            (false, false, ',') => {
                fields.push(std::mem::take(&mut field));
            }
            (false, true, ',') => {
                fields.push(std::mem::take(&mut field));
                closed_quote = false;
            }
            (true, _, value) | (false, false, value) => field.push(value),
            (false, true, _) => return Err(SidError::Csv),
        }
    }
    if quoted || fields.len() != 1 {
        return Err(SidError::Csv);
    }
    fields.push(field);
    Ok(fields)
}

#[cfg(any(windows, test))]
fn parse_sid(value: &str) -> Result<CurrentUserSid, SidError> {
    let Some(tail) = value.strip_prefix("S-1-") else {
        return Err(SidError::InvalidSid);
    };
    let mut fields = tail.split('-');
    let Some(authority) = fields.next() else {
        return Err(SidError::InvalidSid);
    };
    if !decimal_field(authority) {
        return Err(SidError::InvalidSid);
    }
    let mut has_subauthority = false;
    for field in fields {
        if !decimal_field(field) {
            return Err(SidError::InvalidSid);
        }
        has_subauthority = true;
    }
    if !has_subauthority {
        return Err(SidError::InvalidSid);
    }
    Ok(CurrentUserSid(value.into()))
}

#[cfg(any(windows, test))]
fn decimal_field(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{
        EdgeError, RootPaths, SidError, decimal_field, parse_csv_row, parse_sid, resolve_roots,
        resolve_workspace_path,
    };
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::path::{Path, PathBuf};

    #[test]
    fn posix_home_uses_product_scoped_dotfolders() {
        #[cfg(not(windows))]
        {
            let vars = env(&[("HOME", "/t/home")]);
            let roots = resolve_roots(&vars, "dalgon").unwrap();
            assert_eq!(roots.config, PathBuf::from("/t/home/.config/dal"));
            assert_eq!(roots.data, PathBuf::from("/t/home/.local/share/dal"));
        }
    }

    #[cfg(windows)]
    const TEST_HOME: &str = "C:\\t\\home";
    #[cfg(not(windows))]
    const TEST_HOME: &str = "/t/home";
    #[cfg(windows)]
    const TEST_DATA: &str = "C:\\t\\data\\";
    #[cfg(not(windows))]
    const TEST_DATA: &str = "/t/data/";
    #[cfg(windows)]
    const TEST_FAMILY_HOME: &str = "C:\\home\\dalgona";
    #[cfg(not(windows))]
    const TEST_FAMILY_HOME: &str = "/home/dalgona";

    #[test]
    fn relative_xdg_falls_back_and_absolute_xdg_appends_product_once() {
        let vars = env(&[
            ("HOME", TEST_HOME),
            ("XDG_CONFIG_HOME", "relative"),
            ("XDG_DATA_HOME", TEST_DATA),
        ]);
        let roots = resolve_roots(&vars, "dalgon").unwrap();
        assert_eq!(roots.config, PathBuf::from(TEST_HOME).join(".config/dal"));
        assert_eq!(roots.data, PathBuf::from(TEST_DATA).join("dal"));
    }

    /// `HOME` stays primary on Windows; `USERPROFILE` and the
    /// `HOMEDRIVE`/`HOMEPATH` pair are the documented fallbacks.
    #[cfg(windows)]
    #[test]
    fn userprofile_and_homedrive_pair_resolve_home_without_home() {
        let vars = env(&[("USERPROFILE", "C:\\Users\\alice")]);
        assert_eq!(
            super::select_home(&vars).unwrap(),
            PathBuf::from("C:\\Users\\alice")
        );
        let vars = env(&[("HOME", "relative"), ("USERPROFILE", "C:\\Users\\bob")]);
        assert_eq!(
            super::select_home(&vars).unwrap(),
            PathBuf::from("C:\\Users\\bob")
        );
        let vars = env(&[("HOMEDRIVE", "C:"), ("HOMEPATH", "\\Users\\carol")]);
        assert_eq!(
            super::select_home(&vars).unwrap(),
            PathBuf::from("C:\\Users\\carol")
        );
    }

    #[test]
    fn product_roots_use_the_family_identity_not_home_basename() {
        let vars = env(&[("HOME", TEST_FAMILY_HOME)]);
        let dalgon = resolve_roots(&vars, "dalgon").unwrap();
        let dal = resolve_roots(&vars, "dal").unwrap();
        let dl = resolve_roots(&vars, "dl").unwrap();
        let dalgona = resolve_roots(&vars, "dalgona").unwrap();
        assert_eq!(dalgon.data.file_name(), Some(OsStr::new("dal")));
        assert_eq!(dal.data, dalgon.data);
        assert_eq!(dl.data, dalgon.data);
        assert_eq!(dalgona.data.file_name(), Some(OsStr::new("dalgona")));
        assert_ne!(dalgon.data, dalgona.data);
        assert_eq!(
            super::config_file_path(&dalgon),
            PathBuf::from(TEST_FAMILY_HOME).join(".config/dal/dal.toml")
        );
        assert!(matches!(
            resolve_roots(&vars, "unknown"),
            Err(EdgeError::InvalidProduct)
        ));
    }

    #[test]
    fn no_absolute_home_returns_the_typed_startup_error() {
        let vars = env(&[("HOME", "relative")]);
        assert!(matches!(
            resolve_roots(&vars, "dalgon"),
            Err(EdgeError::HomeMissing)
        ));
    }

    #[test]
    fn workspace_join_is_lexical_and_keeps_symlink_components() {
        let cwd = Path::new("/tmp/current");
        assert_eq!(
            resolve_workspace_path(cwd, Some(Path::new("../link/./workspace"))),
            PathBuf::from("/tmp/link/workspace")
        );
    }

    #[cfg(feature = "tui")]
    #[test]
    fn color_precedence_uses_environment_presence_not_values() {
        use super::resolve_color;
        use crate::cli::ColorArg;

        let vars = env(&[("NO_COLOR", ""), ("FORCE_COLOR", "0"), ("TERM", "xterm")]);
        assert_eq!(resolve_color(None, &vars, true), ColorArg::Never);
        let vars = env(&[("FORCE_COLOR", "0"), ("TERM", "xterm")]);
        assert_eq!(resolve_color(None, &vars, false), ColorArg::Always);
        assert_eq!(
            resolve_color(Some(ColorArg::Never), &vars, true),
            ColorArg::Never
        );
    }

    #[test]
    fn whoami_csv_accepts_quoted_fields_and_validates_sid_decimal_fields() {
        let fields = parse_csv_row("\"DOMAIN\\\\alice\",\"S-1-5-21-42\"").unwrap();
        assert_eq!(fields[1], "S-1-5-21-42");
        assert_eq!(parse_sid(&fields[1]).unwrap().as_str(), "S-1-5-21-42");
        assert!(parse_csv_row("\"DOMAIN\\\\alice\",\"S-1-5-21-42\",").is_err());
        assert!(parse_csv_row("\"DOMAIN\\\\alice\",\"S-1-5-21-42\",\"extra\"").is_err());
        assert!(parse_csv_row("\"DOMAIN\\\\alice\"").is_err());
        assert!(parse_csv_row("").is_err());
        let windows = "\"DOMAIN\\\\alice\",\"S-1-5-21-42\"\r\n";
        let mut rows = windows.lines();
        assert_eq!(
            parse_csv_row(rows.next().unwrap()).unwrap()[1],
            "S-1-5-21-42"
        );
        assert!(rows.next().is_none());
        assert!(parse_sid("S-1-5").is_err());
        assert!(parse_sid("S-1-5-x").is_err());
        assert!(!decimal_field(""));
    }

    #[test]
    fn sid_probe_failures_render_their_operational_messages() {
        assert_eq!(
            SidError::SystemRoot.to_string(),
            "SystemRoot is not set to an absolute directory"
        );
        assert_eq!(
            SidError::Spawn(io::Error::other("access is denied")).to_string(),
            "cannot start whoami.exe: access is denied"
        );
        assert_eq!(
            SidError::Failed(None).to_string(),
            "whoami.exe exited with status None"
        );
        assert_eq!(
            SidError::Failed(Some(1)).to_string(),
            "whoami.exe exited with status Some(1)"
        );
        assert_eq!(
            SidError::Csv.to_string(),
            "whoami.exe returned malformed CSV"
        );
        assert_eq!(
            SidError::InvalidSid.to_string(),
            "whoami.exe did not return a valid current-user SID"
        );
    }

    #[test]
    fn dal_log_accepts_four_levels_and_rejects_unknown() {
        use super::{LogLevel, parse_log_level};
        assert_eq!(parse_log_level(&env(&[])), Ok(None));
        assert_eq!(
            parse_log_level(&env(&[("DAL_LOG", "debug")])),
            Ok(Some(LogLevel::Debug))
        );
        assert!(parse_log_level(&env(&[("DAL_LOG", "verbose")])).is_err());
    }

    fn env(values: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
        values
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    }

    #[test]
    fn root_paths_keep_distinct_config_and_cache_roots() {
        let home = if cfg!(windows) { "C:\\h" } else { "/h" };
        let roots = resolve_roots(&env(&[("HOME", home)]), "dalgon").unwrap();
        let base = PathBuf::from(home);
        assert_eq!(
            roots,
            RootPaths {
                home: base.clone(),
                config: base.join(".config/dal"),
                data: base.join(".local/share/dal"),
                cache: base.join(".local/share/dal/cache"),
            }
        );
    }

    #[test]
    fn family_table_maps_aliases_and_rejects_unknown_binaries() {
        assert_eq!(super::family_for_binary("dalgon"), Some("dal"));
        assert_eq!(super::family_for_binary("dal"), Some("dal"));
        assert_eq!(super::family_for_binary("dl"), Some("dal"));
        assert_eq!(super::family_for_binary("dalgona"), Some("dalgona"));
        assert_eq!(super::family_for_binary(""), None);
        assert_eq!(super::family_for_binary("dalgon/foo"), None);
        assert_eq!(super::CONFIG_FILE_NAME, "dal.toml");
    }
}
