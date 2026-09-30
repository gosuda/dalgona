#![cfg(unix)]
#![expect(
    unreachable_pub,
    reason = "PTY helpers are public only within private test modules"
)]

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt},
    termios::{Winsize, tcsetwinsize},
};

/// A real child process attached to a native POSIX pseudoterminal.
#[must_use]
pub struct PtyProcess {
    child: Child,
    master: File,
    output: Vec<u8>,
}

impl PtyProcess {
    /// Starts `command` with stdin, stdout, and stderr attached to one PTY.
    pub fn spawn(command: &mut Command, columns: u16, rows: u16) -> io::Result<Self> {
        let master_fd = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).map_err(io::Error::from)?;
        grantpt(&master_fd).map_err(io::Error::from)?;
        unlockpt(&master_fd).map_err(io::Error::from)?;
        let slave_name = ptsname(&master_fd, Vec::new()).map_err(io::Error::from)?;
        let slave_path = Path::new(slave_name.to_str().map_err(io::Error::other)?);
        let slave = OpenOptions::new().read(true).write(true).open(slave_path)?;
        tcsetwinsize(
            &slave,
            Winsize {
                ws_row: rows,
                ws_col: columns,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .map_err(io::Error::from)?;

        let master = File::from(master_fd);
        let flags = fcntl_getfl(&master).map_err(io::Error::from)?;
        fcntl_setfl(&master, flags | OFlags::NONBLOCK).map_err(io::Error::from)?;
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?));
        let child = command.spawn()?;
        Ok(Self {
            child,
            master,
            output: Vec::new(),
        })
    }

    /// Writes key or paste bytes to the real terminal input stream.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.master.write_all(bytes)
    }

    /// Captures available output for at most `duration`.
    pub fn collect_for(&mut self, duration: Duration) -> io::Result<()> {
        let deadline = Instant::now() + duration;
        loop {
            let read = self.read_available()?;
            if Instant::now() >= deadline {
                return Ok(());
            }
            if read == 0 {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Captures output until it contains `needle` or the deadline expires.
    pub fn wait_for(&mut self, needle: &[u8], timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if contains(&self.output, needle) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                self.read_available()?;
                return Err(io::Error::other(format!(
                    "PTY child exited with {status} before output {:?}",
                    String::from_utf8_lossy(needle)
                )));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "PTY output did not contain {:?}",
                        String::from_utf8_lossy(needle)
                    ),
                ));
            }
            if self.read_available()? == 0 {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Waits until `needle` has appeared at least `count` times in terminal output.
    pub fn wait_for_count(
        &mut self,
        needle: &[u8],
        count: usize,
        timeout: Duration,
    ) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if occurrences(&self.output, needle) >= count {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                self.read_available()?;
                return Err(io::Error::other(format!(
                    "PTY child exited with {status} before {count} occurrences of {:?}",
                    String::from_utf8_lossy(needle)
                )));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "PTY output did not contain {count} occurrences of {:?}",
                        String::from_utf8_lossy(needle)
                    ),
                ));
            }
            if self.read_available()? == 0 {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Returns every byte read from the pseudoterminal master so far.
    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Waits for child termination while continuing to drain PTY output.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> io::Result<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            self.read_available()?;
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "PTY child did not exit",
                ));
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn read_available(&mut self) -> io::Result<usize> {
        let mut buffer = [0_u8; 8192];
        match self.master.read(&mut buffer) {
            Ok(0) => Ok(0),
            Ok(length) => {
                self.output.extend_from_slice(&buffer[..length]);
                Ok(length)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(0),
            Err(_error) if self.child.try_wait()?.is_some() => Ok(0),
            Err(error) => Err(error),
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Builds an isolated dalgon command configured for deterministic scripted replies.
pub fn dalgon_command(home: &Path, replies: &[&str]) -> io::Result<Command> {
    let mut contents = String::new();
    for reply in replies {
        contents.push_str(&format!(
            "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":{}}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":12,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n",
            sonic_rs::to_string(reply).map_err(io::Error::other)?
        ));
    }
    dalgon_command_with_fixture(home, &contents)
}

/// Builds an isolated dalgon command from an exact provider replay fixture.
pub fn dalgon_command_with_fixture(home: &Path, contents: &str) -> io::Result<Command> {
    use crate::support::dalgon_binary;
    let config_dir = home.join(".config/dal");
    let data_dir = home.join(".local/share/dal");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&data_dir)?;
    let fixture = home.join("scripted.jsonl");
    std::fs::write(&fixture, contents)?;
    let fixture = fixture.to_string_lossy();
    std::fs::write(
        config_dir.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\napproval = \"ask\"\n[providers.scripted]\nfixture = {fixture:?}\n"
        ),
    )?;

    let mut command = Command::new(dalgon_binary("dalgon")?);
    command
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("DAL_NO_MOTION", "1")
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    Ok(command)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}
