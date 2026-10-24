// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use dalgona_gates::support;

use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[test]
fn plugin_list_shows_every_bundled_battery() -> support::TestResult<()> {
    let scratch = support::Scratch::new("plugin-list-batteries")?;
    let output = support::run_command(
        Command::new(support::dalgona_binary()?)
            .args(["plugin", "list"])
            .env("XDG_DATA_HOME", scratch.path().join("data"))
            .env("XDG_CONFIG_HOME", scratch.path().join("config")),
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8(output.stdout)?;
    for name in support::BATTERIES {
        let row = format!("{name} (bundled): services:");
        assert!(
            listing.lines().any(|line| line.starts_with(&row)),
            "plugin list has no bundled row for {name}:\n{listing}"
        );
    }
    assert!(!listing.contains("(user)"), "{listing}");
    Ok(())
}

/// How long one wait for the screen may take.
#[cfg(unix)]
const WAIT: Duration = Duration::from_secs(60);
/// How often the screen is re-read.
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(100);
/// The bundled batteries this fixture lets ask for a startup grant. Any other
/// grant question fails the gate rather than being answered blindly.
#[cfg(unix)]
const PERMITTED_GRANTS: [&str; 2] = ["history", "orchestration"];
/// A grant question row opens with this text (`copy::ids::GRANT_TITLE`).
#[cfg(unix)]
const GRANT_PREFIX: &str = "Allow ";
/// The answer-key row an armed grant dialog shows (`DIALOG_ACTIONS_SHORT`).
#[cfg(unix)]
const ARMED_HINT: &str = "y allow · a session · n deny";
/// The idle composer's key hint; every width variant starts this way.
#[cfg(unix)]
const IDLE_HINT: &str = "enter send · ";

/// The extension named by the grant question open on screen, if any. Only a
/// row that opens with the title counts: the answered notice repeats it.
#[cfg(unix)]
fn open_grant(screen: &str) -> Option<String> {
    screen.lines().find_map(|line| {
        let title = line.trim_start().strip_prefix(GRANT_PREFIX)?;
        let (ext, _) = title.split_once(" to use ")?;
        Some(ext.to_owned())
    })
}

/// Whether the composer is idle and no grant dialog covers it.
#[cfg(unix)]
fn composer_idle(screen: &str) -> bool {
    open_grant(screen).is_none() && screen.lines().any(|line| line.contains(IDLE_HINT))
}

/// Approves the open grant for `ext` once its answer keys are armed, then
/// waits until that dialog is gone, so no second `y` reaches the composer.
/// A grant outside [`PERMITTED_GRANTS`] fails the gate.
#[cfg(unix)]
fn approve(
    pane: &support::Pane,
    ext: &str,
    screen: &str,
    deadline: Instant,
) -> support::TestResult<()> {
    if !PERMITTED_GRANTS.contains(&ext) {
        return Err(format!("unexpected startup grant for {ext:?}:\n{screen}").into());
    }
    let armed = screen
        .lines()
        .any(|line| line.trim_start().starts_with(ARMED_HINT));
    if !armed {
        return Ok(());
    }
    pane.type_text("y")?;
    loop {
        let after = pane.text()?;
        if open_grant(&after).as_deref() != Some(ext) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("the {ext} grant never closed:\n{after}").into());
        }
        std::thread::sleep(POLL);
    }
}

/// Polls until `done` holds on two consecutive reads, approving each
/// permitted grant on the way.
#[cfg(unix)]
fn settle(
    pane: &support::Pane,
    what: &str,
    done: impl Fn(&str) -> bool,
) -> support::TestResult<()> {
    let deadline = Instant::now() + WAIT;
    let mut held = false;
    loop {
        let screen = pane.text()?;
        if let Some(ext) = open_grant(&screen) {
            held = false;
            approve(pane, &ext, &screen, deadline)?;
        } else if done(&screen) {
            if held {
                return Ok(());
            }
            held = true;
        } else {
            held = false;
        }
        if Instant::now() >= deadline {
            return Err(format!("{what}: the screen never matched:\n{screen}").into());
        }
        std::thread::sleep(POLL);
    }
}

/// Types `line` into the settled idle composer and submits it.
#[cfg(unix)]
fn submit(pane: &support::Pane, line: &str) -> support::TestResult<()> {
    settle(pane, "idle composer", composer_idle)?;
    pane.type_text(line)?;
    pane.enter()
}

/// A bare interactive `dalgona` (no arguments, no battery config) answers
/// the work battery's `/plan` command, which plain dal does not register.
#[cfg(unix)]
#[test]
fn bare_dalgona_launch_runs_the_work_battery_plan_command() -> support::TestResult<()> {
    let pane = support::Pane::start(support::Scratch::new("bare-launch-plan")?)?;
    submit(&pane, "/plan on")?;
    settle(&pane, "plan on", |screen| {
        screen.contains("Plan mode is on.")
    })?;
    submit(&pane, "/plan off")?;
    settle(&pane, "plan off", |screen| {
        screen.contains("Plan mode is off.")
    })?;
    Ok(())
}
