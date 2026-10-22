//! Process-edge proofs: configuration diagnostics, `DAL_LOG` handling, serve
//! signal behavior, and serve-token auth boundaries. Every proof runs the
//! real built binary with synthetic inputs only: no network, temp dirs for
//! config and data roots.

#[expect(
    dead_code,
    reason = "fixture helpers are shared across binary test crates"
)]
mod support;

use std::{
    error::Error,
    fs,
    io::{self, Read},
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use support::CliFixture;

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Spawns the built binary like the fixture does, plus extra environment
/// variables (the fixture clears the environment, so `DAL_LOG` needs an
/// explicit opt-in per case).
#[expect(
    clippy::disallowed_methods,
    reason = "test harness: the fixture launches the built dalgon binary and forwards its own PATH"
)]
fn spawn_with_env(
    fixture: &CliFixture,
    args: &[&str],
    extra: &[(&str, &str)],
) -> io::Result<Child> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dalgon"));
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command
        .args(args)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", &fixture.config)
        .env("XDG_DATA_HOME", &fixture.data)
        .current_dir(fixture.cwd());
    for (key, value) in extra {
        command.env(key, value);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn output_with_env(
    fixture: &CliFixture,
    args: &[&str],
    extra: &[(&str, &str)],
) -> io::Result<Output> {
    spawn_with_env(fixture, args, extra)?.wait_with_output()
}

#[expect(
    clippy::disallowed_methods,
    reason = "test harness: signals the spawned binary under test"
)]
fn send_signal(pid: u32, sig: &str) {
    let _ = Command::new("kill")
        .args([sig, &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// --- Configuration ownership: the CLI edge owns diagnostics ---------------

#[test]
fn config_unknown_key_suggests_closest_key_with_two_lines() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_config("modle = \"normal\"\n")?;

    let output = fixture.output(&["models"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        stderr_text(&output),
        "dalgon: dal.toml: unknown key \"modle\"\nDid you mean \"mode\"? See dalgon docs dal://config.\n",
    );
    Ok(())
}

#[test]
fn config_nested_unknown_key_carries_dotted_path() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_config("[rules]\nwach = true\n")?;

    let output = fixture.output(&["docs"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        stderr_text(&output),
        "dalgon: dal.toml: unknown key \"rules.wach\"\nDid you mean \"rules.watch\"? See dalgon docs dal://config.\n",
    );
    Ok(())
}

#[test]
fn config_invalid_value_names_key_and_valid_set() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_config("mode = \"turbo\"\n")?;

    let output = fixture.output(&["plugin", "list"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        stderr_text(&output),
        "dalgon: invalid config value turbo for mode: normal, eval-first, eval-only\nFix the file, or move it aside and run dalgon to start fresh.\n",
    );
    Ok(())
}

#[test]
fn config_duplicate_keys_fail_closed_with_fix_line() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_config("mode = \"normal\"\nmode = \"eval-only\"\n")?;

    let output = fixture.output(&["rules"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
    let stderr = stderr_text(&output);
    let (first, second) = stderr
        .split_once('\n')
        .ok_or("the diagnostic has a first line")?;
    assert!(
        first.starts_with("dalgon: dal.toml:2: ") && first.len() > "dalgon: dal.toml:2: ".len(),
        "the first line names the file, the duplicate's line, and the parser message: {first}"
    );
    assert_eq!(
        second,
        "Fix the file, or move it aside and run dalgon to start fresh.\n"
    );
    Ok(())
}

#[test]
fn config_unreadable_names_the_file_and_the_permission_fix() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    // A directory where the file belongs cannot be read as text on any platform.
    let config = fixture.config.join("dal").join("dal.toml");
    fs::create_dir_all(&config)?;

    let output = fixture.output(&["docs"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
    let stderr = stderr_text(&output);
    assert!(
        stderr.starts_with(&format!("dalgon: cannot read {}: ", config.display())),
        "the first line names the operation and the absolute path: {stderr}"
    );
    assert!(
        stderr.ends_with(&format!(
            "Fix the permissions on {}, or point the XDG variables at a writable location.\n",
            config.display()
        )),
        "the second line names the fix: {stderr}"
    );
    Ok(())
}

// --- Required logging: DAL_LOG level + file sink, never the terminal ------

#[test]
fn dal_log_unknown_value_fails_with_two_line_error() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = output_with_env(&fixture, &["docs"], &[("DAL_LOG", "verbose")])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        stderr_text(&output),
        "dalgon: DAL_LOG \"verbose\" is invalid\nUse one of error, warning, info, or debug.\n",
    );
    Ok(())
}

#[test]
fn debug_logging_reaches_the_cache_file_not_stdout() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let debug = output_with_env(&fixture, &["docs"], &[("DAL_LOG", "debug")])?;
    assert_eq!(
        debug.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&debug)
    );
    let log = fixture.data.join("dal").join("cache").join("dal.log");
    assert!(
        log.is_file(),
        "expected a family log file at {}",
        log.display()
    );
    let plain = fixture.output(&["docs"])?;
    assert_eq!(
        plain.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&plain)
    );
    // The log sink never writes to the terminal: debug output is byte-equal.
    assert_eq!(debug.stdout, plain.stdout);
    assert_eq!(debug.stderr, plain.stderr);
    Ok(())
}

// --- Signals: serve stops cleanly, print races keep the ladder ------------

#[test]
#[cfg_attr(not(unix), ignore = "POSIX SIGTERM delivery only")]
fn sigterm_during_serve_stops_cleanly_with_zero() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let mut child = spawn_with_env(&fixture, &["serve", "--port", "0"], &[])?;
    let pid = child.id();
    // Never hang the suite if the stop path wedges: the watchdog SIGKILLs
    // the child unless the test finishes first.
    let _watchdog = watchdog(pid);
    // Wait for the listen line before signalling: the server is up.
    let listen = read_listen_line(&mut child)?;
    assert!(
        listen.starts_with("dalgon serve listening on http://127.0.0.1:"),
        "listen line: {listen}"
    );
    send_signal(pid, "-TERM");
    let output = child.wait_with_output()?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Kills `pid` after one minute unless the guard is dropped first.
///
/// The guard owns the only sender, so dropping it disconnects the thread and
/// disarms the kill: a pid the OS has since reused is never signalled.
struct Watchdog {
    _disarm: mpsc::Sender<()>,
}

fn watchdog(pid: u32) -> Watchdog {
    let (disarm, armed) = mpsc::channel::<()>();
    thread::spawn(move || {
        if armed.recv_timeout(Duration::from_secs(60)) == Err(mpsc::RecvTimeoutError::Timeout) {
            send_signal(pid, "-KILL");
        }
    });
    Watchdog { _disarm: disarm }
}

/// Reads the one-line serve startup message from a spawned `serve` child.
///
/// Blocks until the child prints a line or closes its output, so callers hold
/// a [`Watchdog`] that kills a wedged child and ends the read.
fn read_listen_line(child: &mut Child) -> io::Result<String> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "serve stdout was not piped"))?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stdout.read_exact(&mut byte)?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
    }
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "only Linux routes all of 127.0.0.0/8 to loopback without setup"
)]
fn configured_serve_bind_outranks_the_loopback_default() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_config("[serve]\nbind = \"127.0.0.2\"\n")?;
    let mut child = spawn_with_env(&fixture, &["serve", "--port", "0"], &[])?;
    let pid = child.id();
    // A wedged listener must not hang the suite: the read below blocks
    // until the child prints or the watchdog kills it.
    let _watchdog = watchdog(pid);
    let line = read_listen_line(&mut child);
    send_signal(pid, "-TERM");
    let output = child.wait_with_output()?;
    let line = line?;
    assert!(
        line.starts_with("dalgon serve listening on http://127.0.0.2:"),
        "the configured bind address is the listener address: {line}"
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    Ok(())
}

// --- Authentication boundaries: serve token lifecycle ---------------------

#[test]
fn serve_public_without_token_fails_with_two_line_error() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["serve", "--public", "--port", "0"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
    let token_path = fixture.data.join("dal").join("serve.token");
    assert_eq!(
        stderr_text(&output),
        format!(
            "dalgon: serve --public needs a token: {} does not exist\nRun dalgon serve token to create it. The token is printed once.\n",
            token_path.display()
        ),
    );
    Ok(())
}

#[test]
fn serve_token_create_is_private_and_refuses_without_force() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let token_path = fixture.data.join("dal").join("serve.token");

    let created = fixture.output(&["serve", "token"])?;
    assert_eq!(
        created.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&created)
    );
    assert_eq!(created.stderr, [] as [u8; 0]);
    let token = stdout_text(&created);
    assert!(
        token.starts_with("dal_") && token.len() == 68 + 1 && token.ends_with('\n'),
        "token is dal_ plus 64 hex plus LF: {token:?}"
    );
    let body = fs::read(&token_path)?;
    assert_eq!(
        body,
        token.as_bytes(),
        "the file holds the printed token plus LF"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&token_path)?.permissions().mode() & 0o777,
            0o600,
            "the token file is owner-only"
        );
    }

    let again = fixture.output(&["serve", "token"])?;
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(again.stdout, [] as [u8; 0]);
    assert_eq!(
        stderr_text(&again),
        format!(
            "dalgon: serve.token already exists: {}\nPass --force to replace it. Every client must then use the new token.\n",
            token_path.display()
        ),
    );
    assert_eq!(
        fs::read(&token_path)?,
        body,
        "refusal leaves the file unchanged"
    );

    let forced = fixture.output(&["serve", "token", "--force"])?;
    assert_eq!(
        forced.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&forced)
    );
    assert_ne!(stdout_text(&forced), token, "force mints a fresh token");
    Ok(())
}
