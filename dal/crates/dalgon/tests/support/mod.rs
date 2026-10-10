#![expect(
    dead_code,
    reason = "fixture helpers are shared across binary test crates"
)]

use std::{
    error::Error,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

pub(crate) struct CliFixture {
    root: TempDir,
    pub(crate) home: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) data: PathBuf,
}

impl CliFixture {
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        let root = TempDir::new()?;
        let home = root.path().join("home");
        let config = root.path().join("config");
        let data = root.path().join("data");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&config)?;
        fs::create_dir_all(&data)?;
        Ok(Self {
            root,
            home,
            config,
            data,
        })
    }

    /// The working directory the fixture launches the binary in.
    pub(crate) fn cwd(&self) -> &Path {
        self.root.path()
    }

    pub(crate) fn auth_file(&self) -> PathBuf {
        self.data.join("dal").join("auth.json")
    }

    pub(crate) fn write_config(&self, text: &str) -> io::Result<()> {
        let path = self.config.join("dal").join("dal.toml");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)
    }

    pub(crate) fn write_auth(&self, text: &str) -> io::Result<()> {
        let path = self.auth_file();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, text)?;
        set_private_mode(&path)
    }

    pub(crate) fn output(&self, args: &[&str]) -> io::Result<Output> {
        self.command(args).output()
    }

    pub(crate) fn output_with_stdin(&self, args: &[&str], input: &[u8]) -> io::Result<Output> {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "child stdin was not piped")
        })?;
        stdin.write_all(input)?;
        drop(stdin);
        child.wait_with_output()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test harness: the fixture launches the built dalgon binary and forwards its own environment"
    )]
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dalgon"));
        command.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .current_dir(self.root.path());
        command
    }
}

#[cfg_attr(
    not(unix),
    expect(
        clippy::unnecessary_wraps,
        reason = "the unix body chmods and can fail; other platforms have no mode bit to set"
    )
)]
fn set_private_mode(path: &std::path::Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
