// Copyright (c) Cognition Inc. and other dal contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! End-to-end terminal scenarios on a real pseudo-terminal.
//!
//! Each test starts the real `dalgon` binary with a scripted provider, drives
//! it with keystrokes, and reads the rendered screen through a terminal
//! emulator. A claim here is about rows a person would see, never about exit
//! codes alone: startup, streamed replies, approvals, slash commands, queued
//! follow-ups, interrupts, resizes, multiline paste, and resume.
//!
//! Records a real PTY session as a transcript artifact and diffs its replayed
//! screen against the committed snapshot.

#[expect(
    dead_code,
    reason = "PTY support includes helpers used by other gate targets"
)]
#[path = "support/pty.rs"]
mod pty;
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;
#[path = "support/transcript.rs"]
mod transcript;
#[path = "support/vt.rs"]
mod vt;

use std::{
    error::Error,
    fmt::Write as _,
    path::Path,
    time::{Duration, Instant},
};

use pty::{PtyProcess, dalgon_command, dalgon_command_with_fixture};
use support::TestDir;
use transcript::{Transcript, assert_snapshot};
use vt::VtRecorder;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type FixtureResult = Result<String, Box<dyn Error + Send + Sync>>;

const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":1200,"cached_input_tokens":0,"output_tokens":340,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":0.0123}}"#;
const WAIT: Duration = Duration::from_secs(15);

/// One scripted provider step that streams `deltas` and ends the turn.
fn text_step(deltas: &[&str]) -> FixtureResult {
    let mut events = String::new();
    for delta in deltas {
        let text = sonic_rs::to_string(delta)?;
        write!(events, "{{\"type\":\"text_delta\",\"text\":{text}}},")?;
    }
    Ok(format!(
        "{{\"kind\":\"events\",\"events\":[{events}{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    ))
}

/// One scripted provider step that calls `name` with `args` and waits for the result.
fn tool_step(call: &str, name: &str, args: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"{call}\",\"name\":\"{name}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"{call}\",\"name\":\"{name}\",\"args\":{{\"kind\":\"parsed\",\"value\":{args}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

fn exec_step(call: &str, command: &str) -> FixtureResult {
    let command = sonic_rs::to_string(command)?;
    Ok(tool_step(
        call,
        "exec",
        &format!("{{\"command\":{command},\"timeout_seconds\":30}}"),
    ))
}

/// A running `dalgon` plus the emulated screen it paints.
struct Screen {
    term: PtyProcess,
    vt: VtRecorder,
    fed: usize,
}

impl Screen {
    fn start(
        home: &Path,
        fixture: &str,
        columns: u16,
        rows: u16,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Self::start_with(home, fixture, columns, rows, &[])
    }

    fn start_with(
        home: &Path,
        fixture: &str,
        columns: u16,
        rows: u16,
        extra: &[&str],
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut command = dalgon_command_with_fixture(home, fixture)?;
        command.args(["--screen", "inline"]);
        command.args(extra);
        let term = PtyProcess::spawn(&mut command, columns, rows)?;
        let mut screen = Self {
            term,
            vt: VtRecorder::new(columns, rows),
            fed: 0,
        };
        screen.until(|text| text.contains(dal_tui::copy::ids::COMPOSER_PLACEHOLDER))?;
        Ok(screen)
    }

    fn pump(&mut self, millis: u64) -> TestResult {
        self.term.collect_for(Duration::from_millis(millis))?;
        let output = self.term.output();
        self.vt.feed(&output[self.fed..]);
        self.fed = output.len();
        Ok(())
    }

    /// Scrollback plus the visible screen, one row per line.
    fn text(&self) -> String {
        self.vt
            .scrollback_rows()
            .into_iter()
            .chain(self.vt.screen_rows())
            .map(|row| row.trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The visible screen only, one row per line.
    fn visible(&self) -> Vec<String> {
        self.vt
            .screen_rows()
            .into_iter()
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn until(&mut self, predicate: impl Fn(&str) -> bool) -> TestResult {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump(25)?;
            if predicate(&self.text()) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("the screen never matched:\n{}", self.dump()).into());
            }
        }
    }

    fn dump(&self) -> String {
        let mut dump = String::new();
        for row in self.vt.scrollback_rows() {
            let _ = writeln!(dump, "S|{}", row.trim_end());
        }
        for row in self.visible() {
            let _ = writeln!(dump, " |{row}");
        }
        dump
    }

    /// Sends one key sequence and lets the frame settle.
    fn send(&mut self, bytes: &[u8]) -> TestResult {
        self.term.write(bytes)?;
        self.pump(250)
    }

    /// Deletes any draft with backspaces, then quits with Ctrl-D.
    fn quit_after_clearing(mut self) -> TestResult {
        self.term.write(b"\x1b[F")?;
        self.term.write(&[0x7f; 200])?;
        self.pump(300)?;
        self.quit()
    }

    fn quit(mut self) -> TestResult {
        for _ in 0..6 {
            self.term.write(b"\x04")?;
            match self.term.wait_for_exit(Duration::from_secs(3)) {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err("dalgon stayed alive after repeated Ctrl-D".into())
    }
}

fn contains_line(text: &str, needle: &str) -> bool {
    text.lines().any(|line| line.contains(needle))
}

#[test]
fn startup_names_the_configured_model_and_skips_the_sign_in_banner() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["unused"])?, 100, 30)?;
    screen.pump(300)?;
    let rows = screen.visible();
    let status = rows.last().expect("the screen has rows");
    assert!(
        status.contains("openai-responses/gpt-6"),
        "the status line names the configured model, got {status:?}\n{}",
        screen.dump()
    );
    assert!(
        !screen.text().contains("No model selected yet"),
        "a configured model must not trigger the sign-in banner\n{}",
        screen.dump()
    );
    assert!(
        status.contains('~'),
        "the status path is written with ~, got {status:?}"
    );
    screen.quit()
}

#[test]
fn streamed_reply_reads_as_one_paragraph_and_status_shows_usage() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = text_step(&["Hello ", "streamed ", "world.\n\n", "Second paragraph."])?;
    let mut screen = Screen::start(dir.path(), &fixture, 120, 30)?;
    screen.send(b"say hello\r")?;
    screen.until(|text| text.contains("Second paragraph."))?;
    screen.pump(300)?;
    let text = screen.text();
    assert!(
        contains_line(&text, "Hello streamed world."),
        "deltas of one reply must join into one row\n{}",
        screen.dump()
    );
    assert!(contains_line(&text, "> say hello"));
    assert!(
        text.lines().any(|line| line == "  Hello streamed world."),
        "assistant rows keep the 2-cell left gutter\n{}",
        screen.dump()
    );
    let rows = screen.visible();
    let status = rows.last().expect("the screen has rows");
    assert!(
        status.contains("in 1k out 340"),
        "status shows token usage, got {status:?}"
    );
    assert!(
        status.contains("$0.012"),
        "status shows cost, got {status:?}"
    );
    screen.quit()
}

#[test]
fn f1_lists_every_key_by_name() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["unused"])?, 100, 40)?;
    screen.send(b"\x1bOP")?;
    screen.until(|text| text.contains("Copy selection") || text.contains("no command"))?;
    let text = screen.text();
    assert!(
        !text.contains("no command registered"),
        "F1 must open the key list\n{}",
        screen.dump()
    );
    assert!(contains_line(&text, "ctrl+d Quit"));
    assert!(
        !text.lines().any(|line| line.starts_with("key ")),
        "every key must show its name, never the word key\n{}",
        screen.dump()
    );
    screen.quit()
}

#[test]
fn copy_command_sets_the_clipboard_and_counts_characters() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["reply to copy"])?, 100, 30)?;
    screen.send(b"hello\r")?;
    screen.until(|text| text.contains("reply to copy"))?;
    let before = screen.term.output().len();
    screen.send(b"/copy\r")?;
    screen.until(|text| text.contains("copied") || text.contains("CopyReply"))?;
    let text = screen.text();
    assert!(
        !text.contains("CopyReply"),
        "copy must not print debug text\n{}",
        screen.dump()
    );
    assert!(
        contains_line(&text, "copied 13 characters"),
        "copy reports the count honestly\n{}",
        screen.dump()
    );
    let written = String::from_utf8_lossy(&screen.term.output()[before..]).into_owned();
    assert!(
        written.contains("\x1b]52;c;cmVwbHkgdG8gY29weQ=="),
        "copy writes an OSC 52 clipboard sequence"
    );
    screen.quit()
}

#[test]
fn approving_for_the_session_skips_the_next_dialog_and_cards_settle_once() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = format!(
        "{}{}{}",
        exec_step("call-a", "echo ran-one")?,
        exec_step("call-b", "echo ran-two")?,
        text_step(&["both commands done"])?
    );
    let mut screen = Screen::start(dir.path(), &fixture, 100, 30)?;
    screen.send(b"run both\r")?;
    screen.until(|text| text.contains(dal_tui::copy::ids::DIALOG_ACTIONS_SHORT))?;
    let rows = screen.visible();
    let status = rows.last().expect("the screen has rows");
    assert!(
        status.contains("waiting for you"),
        "the status names the wait in full, got {status:?}"
    );
    screen.send(b"a")?;
    screen.until(|text| text.contains("both commands done") || text.contains("echo ran-two"))?;
    screen.pump(300)?;
    let text = screen.text();
    assert!(
        !text.contains("echo ran-two"),
        "a session grant must cover the second command\n{}",
        screen.dump()
    );
    assert!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("working  exec")),
        "settled commands must not keep a working row\n{}",
        screen.dump()
    );
    let cards: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("  ok  exec"))
        .collect();
    assert_eq!(
        cards.len(),
        2,
        "one settled card per command\n{}",
        screen.dump()
    );
    for card in cards {
        let duration = card.rsplit(" · ").next().unwrap_or_default();
        assert!(
            duration.ends_with(" ms") || duration.ends_with('s'),
            "settled card ends with its duration, got {card:?}"
        );
    }
    screen.quit()
}

#[test]
fn denying_a_command_keeps_it_from_running_and_says_so() -> TestResult {
    let dir = TestDir::new()?;
    let marker = dir.path().join("must-not-exist");
    let fixture = format!(
        "{}{}",
        exec_step("call-a", &format!("touch {}", marker.display()))?,
        text_step(&["denied and moved on"])?
    );
    let mut screen = Screen::start(dir.path(), &fixture, 100, 30)?;
    screen.send(b"touch it\r")?;
    screen.until(|text| text.contains(dal_tui::copy::ids::DIALOG_ACTIONS_SHORT))?;
    screen.send(b"n")?;
    screen.until(|text| text.contains("denied and moved on"))?;
    let text = screen.text();
    assert!(contains_line(&text, "failed  exec"), "{}", screen.dump());
    assert!(!marker.exists(), "a denied command must not run");
    screen.quit()
}

#[test]
fn pickers_open_with_slash_commands_and_close_with_escape() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["a reply"])?, 100, 30)?;
    screen.send(b"hello\r")?;
    screen.until(|text| text.contains("a reply"))?;
    for (command, title) in [
        ("/settings\r", "Settings"),
        ("/tree\r", "Session tree"),
        ("/fork\r", "Pick a user message to fork from"),
        ("/model\r", "Pick a model"),
    ] {
        screen.send(command.as_bytes())?;
        let rows = screen.visible();
        assert!(
            rows.iter().any(|row| row.contains(title)),
            "{command:?} opens {title:?}\n{}",
            screen.dump()
        );
        screen.send(b"\x1b")?;
        let rows = screen.visible();
        assert!(
            rows.iter()
                .any(|row| row.contains(dal_tui::copy::ids::COMPOSER_PLACEHOLDER)),
            "Esc closes the picker for {command:?}\n{}",
            screen.dump()
        );
    }
    screen.quit()
}

#[test]
fn typing_while_busy_queues_a_steer_and_the_turn_can_still_be_interrupted() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = format!(
        "{}{}",
        exec_step("call-a", "sleep 30")?,
        text_step(&["unused"])?
    );
    let mut screen = Screen::start(dir.path(), &fixture, 100, 30)?;
    screen.send(b"start\r")?;
    screen.until(|text| text.contains(dal_tui::copy::ids::DIALOG_ACTIONS_SHORT))?;
    screen.send(b"y")?;
    screen.until(|text| text.contains("working"))?;
    screen.send(b"steer later\r")?;
    screen.until(|text| {
        text.contains("1 message queued for the next reply") || text.contains("turn mismatch")
    })?;
    let text = screen.text();
    assert!(
        !text.contains("turn mismatch"),
        "typing while a turn runs must not be refused\n{}",
        screen.dump()
    );
    assert!(
        screen
            .visible()
            .iter()
            .any(|row| row.starts_with("> ") && !row.contains("steer later")),
        "the composer is empty again after the message is queued\n{}",
        screen.dump()
    );
    screen.send(b"\x03")?;
    screen.until(|text| text.contains(dal_tui::copy::ids::TURN_CANCELLED))?;
    screen.pump(400)?;
    assert!(
        !screen
            .visible()
            .iter()
            .any(|row| row.trim_start().starts_with("working  exec")),
        "a cancelled tool call must not keep a working row\n{}",
        screen.dump()
    );
    screen.quit()
}

#[test]
fn a_steer_typed_during_a_command_reaches_the_model_at_the_next_step() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = format!(
        "{}{}",
        exec_step("call-a", "sleep 2")?,
        text_step(&["steer answered"])?
    );
    let mut screen = Screen::start(dir.path(), &fixture, 100, 30)?;
    screen.send(b"start\r")?;
    screen.until(|text| text.contains(dal_tui::copy::ids::DIALOG_ACTIONS_SHORT))?;
    screen.send(b"y")?;
    screen.until(|text| text.contains("working"))?;
    screen.send(b"and then this\r")?;
    screen.until(|text| text.contains("steer answered") || text.contains("turn mismatch"))?;
    let text = screen.text();
    assert!(contains_line(&text, "> and then this"), "{}", screen.dump());
    assert!(contains_line(&text, "steer answered"), "{}", screen.dump());
    screen.quit()
}

#[test]
fn resizing_mid_turn_keeps_status_and_composer_on_the_last_rows() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = format!(
        "{}{}",
        exec_step("call-a", "sleep 30")?,
        text_step(&["unused"])?
    );
    let mut screen = Screen::start(dir.path(), &fixture, 100, 30)?;
    screen.send(b"start\r")?;
    screen.until(|text| text.contains(dal_tui::copy::ids::DIALOG_ACTIONS_SHORT))?;
    screen.send(b"y")?;
    screen.until(|text| text.contains("working"))?;
    for (columns, rows) in [(60_u16, 20_u16), (120, 40), (100, 30)] {
        screen.term.resize(columns, rows)?;
        screen.vt.resize(columns, rows);
        screen.pump(500)?;
        let visible = screen.visible();
        let last = visible.last().expect("the screen has rows");
        assert!(
            last.contains("working"),
            "the status stays on row {rows} at {columns}x{rows}\n{}",
            screen.dump()
        );
        assert!(
            visible[visible.len().saturating_sub(4)..]
                .iter()
                .any(|row| row.starts_with("> ")),
            "the composer stays in the last rows at {columns}x{rows}\n{}",
            screen.dump()
        );
    }
    assert_eq!(
        screen.vt.erase_display_sequences(),
        0,
        "a resize never clears the whole screen"
    );
    screen.send(b"\x03")?;
    screen.until(|text| text.contains(dal_tui::copy::ids::TURN_CANCELLED))?;
    screen.quit()
}

#[test]
fn multiline_paste_shows_every_line_and_sends_one_message() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["paste received"])?, 80, 24)?;
    screen
        .term
        .write(b"\x1b[200~line one\nline two\nline three\x1b[201~")?;
    let started = Instant::now();
    screen.until(|text| text.contains("line three"))?;
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "pasted text must appear promptly, took {:?}",
        started.elapsed()
    );
    let rows = screen.visible();
    assert!(
        rows.iter().any(|row| row.contains("> line one")),
        "{}",
        screen.dump()
    );
    assert!(
        rows.iter().any(|row| row.trim() == "line two"),
        "each pasted line gets its own composer row\n{}",
        screen.dump()
    );
    assert!(!screen.text().contains("\\n"), "{}", screen.dump());
    screen.send(b"\r")?;
    screen.until(|text| text.contains("paste received"))?;
    let text = screen.text();
    for line in ["> line one", "> line two", "> line three"] {
        assert!(
            contains_line(&text, line),
            "the sent message keeps every pasted line\n{}",
            screen.dump()
        );
    }
    screen.quit()
}

#[test]
fn the_saved_session_line_names_a_session_that_resumes() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = format!(
        "{}{}",
        text_step(&["first reply"])?,
        text_step(&["second reply"])?
    );
    let mut screen = Screen::start(dir.path(), &fixture, 100, 30)?;
    screen.send(b"first question\r")?;
    screen.until(|text| text.contains("first reply"))?;
    screen.pump(300)?;
    let before = screen.term.output().len();
    screen.term.write(b"\x04")?;
    screen.term.wait_for_exit(Duration::from_secs(5))?;
    let exit = String::from_utf8_lossy(&screen.term.output()[before..]).into_owned();
    let hint = exit
        .split("run dalgon -r ")
        .nth(1)
        .and_then(|rest| rest.split(" to resume").next())
        .ok_or("the exit line names how to resume")?
        .to_owned();

    let mut resumed = Screen::start_with(dir.path(), &fixture, 100, 30, &["-r", &hint])?;
    resumed.pump(300)?;
    assert!(
        contains_line(&resumed.text(), "first reply"),
        "resuming {hint:?} shows the saved transcript\n{}",
        resumed.dump()
    );
    resumed.quit()
}

#[test]
fn file_links_are_underlined_hyperlinks() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = text_step(&["See [the note](file:///tmp/t30-note.txt) for details."])?;
    let mut screen = Screen::start(dir.path(), &fixture, 100, 30)?;
    let before = screen.term.output().len();
    screen.send(b"link please\r")?;
    screen.until(|text| text.contains("for details."))?;
    screen.pump(300)?;
    let text = screen.text();
    assert!(
        contains_line(&text, "See the note for details."),
        "the link shows its label, not its markup\n{}",
        screen.dump()
    );
    let written = String::from_utf8_lossy(&screen.term.output()[before..]).into_owned();
    assert!(
        written.contains("\x1b]8;;file:///tmp/t30-note.txt"),
        "the label carries an OSC 8 destination"
    );
    assert!(
        written.contains("\x1b[4m"),
        "a link is underlined and nothing else is"
    );
    screen.quit()
}

#[test]
fn replies_render_markdown_instead_of_showing_its_markers() -> TestResult {
    let dir = TestDir::new()?;
    let reply = "# Plan\n\nUse **bold** and `code` here.\n\n- first\n* second\n- [x] done\n- [ ] todo\n\n```rust\nfn main() {}\n```\n\nAfter the fence.";
    let mut screen = Screen::start(dir.path(), &text_step(&[reply])?, 100, 40)?;
    let before = screen.term.output().len();
    screen.send(b"show markdown\r")?;
    screen.until(|text| text.contains("After the fence."))?;
    screen.pump(300)?;
    let text = screen.text();
    for line in [
        "  Plan",
        "  Use bold and code here.",
        "  - first",
        "  - second",
        "  [x] done",
        "  [ ] todo",
        "  rust",
        "  | fn main() {}",
    ] {
        assert!(
            text.lines().any(|row| row == line),
            "expected the row {line:?}\n{}",
            screen.dump()
        );
    }
    assert!(
        !text.contains("**") && !text.contains('`'),
        "markdown markers must not reach the screen\n{}",
        screen.dump()
    );
    let written = String::from_utf8_lossy(&screen.term.output()[before..]).into_owned();
    assert!(
        written.contains("\x1b[1mbold") && written.contains("\x1b[1mPlan"),
        "strong text and headings are bold, which survives NO_COLOR"
    );
    assert!(!written.contains("\x1b[3m"), "nothing renders in italics");
    screen.quit()
}

#[test]
fn status_names_the_git_branch_beside_the_path() -> TestResult {
    let dir = TestDir::new()?;
    std::fs::create_dir_all(dir.path().join(".git"))?;
    std::fs::write(
        dir.path().join(".git/HEAD"),
        "ref: refs/heads/feature/status\n",
    )?;
    let mut screen = Screen::start(dir.path(), &text_step(&["unused"])?, 100, 30)?;
    screen.pump(300)?;
    let rows = screen.visible();
    let status = rows.last().expect("the screen has rows");
    assert!(
        status.contains("~ (feature/status)"),
        "the path slot reads `~ (branch)`, got {status:?}\n{}",
        screen.dump()
    );
    screen.quit()
}

#[test]
fn medium_width_status_compacts_tokens_to_in_slash_out() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["counted"])?, 70, 24)?;
    screen.send(b"count\r")?;
    screen.until(|text| text.contains("counted"))?;
    screen.pump(300)?;
    let rows = screen.visible();
    let status = rows.last().expect("the screen has rows");
    assert!(
        status.contains("1k/340"),
        "60-79 columns show `{{in}}/{{out}}`, got {status:?}"
    );
    assert!(!status.contains("in/"), "got {status:?}");
    screen.quit()
}

#[test]
fn the_composer_edits_at_the_caret_and_the_terminal_caret_follows() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["unused"])?, 100, 30)?;
    screen.send(b"abcdef")?;
    screen.send(b"\x1b[D\x1b[D")?;
    screen.send(b"X")?;
    let rows = screen.visible();
    let composer = rows
        .iter()
        .position(|row| row == "> abcdXef")
        .ok_or_else(|| format!("typing at the caret inserts there\n{}", screen.dump()))?;
    assert_eq!(
        screen.vt.cursor(),
        (2 + 5, composer),
        "the terminal caret sits after the typed X\n{}",
        screen.dump()
    );
    screen.send(b"\x1b[H\x1b[3~")?;
    let rows = screen.visible();
    let composer = rows
        .iter()
        .position(|row| row == "> bcdXef")
        .ok_or_else(|| {
            format!(
                "Home then Delete removes the first letter\n{}",
                screen.dump()
            )
        })?;
    assert_eq!(screen.vt.cursor(), (2, composer), "{}", screen.dump());
    screen.quit_after_clearing()
}

#[test]
fn a_long_draft_wraps_inside_the_composer_instead_of_clipping() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["unused"])?, 44, 24)?;
    let draft = "word ".repeat(12).trim_end().to_owned();
    screen.send(draft.as_bytes())?;
    // Wrapping breaks at the cell, so the draft's rows read back as the draft.
    let rows = screen.visible();
    let first = rows
        .iter()
        .position(|row| row.starts_with("> word"))
        .ok_or_else(|| format!("the draft starts the composer\n{}", screen.dump()))?;
    let mut read_back = String::new();
    for row in &rows[first..rows.len() - 2] {
        read_back.push_str(
            row.strip_prefix("> ")
                .or_else(|| row.strip_prefix("  "))
                .unwrap_or(row),
        );
    }
    assert_eq!(
        read_back,
        draft,
        "every word of a draft stays visible, wrapped inside the composer\n{}",
        screen.dump()
    );
    assert!(
        rows.len() - first > 3,
        "the draft wraps onto a second row\n{}",
        screen.dump()
    );
    screen.quit_after_clearing()
}

#[test]
fn leaving_the_transcript_overlay_leaves_no_chrome_copies_in_scrollback() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["unused"])?, 80, 24)?;
    screen.send(b"\x14")?;
    screen.until(|text| text.contains("dal · "))?;
    screen.send(b"\x1b")?;
    // The next commit scrolls the screen; any duplicate chrome rows would
    // land in the scrollback the moment the transcript moves.
    screen.send(b"hi\r")?;
    screen.until(|text| text.contains("unused"))?;
    let scrollback = screen.vt.scrollback_rows().join("\n");
    assert!(
        !scrollback.contains(dal_tui::copy::ids::COMPOSER_PLACEHOLDER),
        "the composer row never lands in scrollback\n{}",
        screen.dump()
    );
    assert!(
        !scrollback.contains("enter send"),
        "the hint row never lands in scrollback\n{}",
        screen.dump()
    );
    assert!(
        !scrollback.contains("openai-responses/gpt-6"),
        "the status row never lands in scrollback\n{}",
        screen.dump()
    );
    screen.quit()
}

#[test]
fn an_unsent_draft_asks_before_quit_and_keeps_editing_on_n() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["unused"])?, 100, 30)?;
    screen.send(b"/quit\rx")?;
    screen.until(|text| text.contains("Discard the draft and quit"))?;
    assert!(
        contains_line(
            &screen.text(),
            "[y] Discard the draft and quit   [n] Keep editing"
        ),
        "{}",
        screen.dump()
    );
    screen.send(b"n")?;
    assert!(
        screen.visible().iter().any(|row| row == "> x"),
        "keeping the draft returns to the composer with the draft intact\n{}",
        screen.dump()
    );
    screen.quit_after_clearing()
}

#[test]
fn answering_y_to_the_draft_question_quits() -> TestResult {
    let dir = TestDir::new()?;
    let mut screen = Screen::start(dir.path(), &text_step(&["unused"])?, 100, 30)?;
    screen.send(b"/quit\rx")?;
    screen.until(|text| text.contains("Discard the draft and quit"))?;
    screen.term.write(b"y")?;
    let status = screen.term.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "{status}");
    Ok(())
}

#[test]
fn pty_transcript_replays_to_the_committed_snapshot() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["snapshot reply text"])?;
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    let mut record = Transcript::new();

    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;
    record.absorb(terminal.output());

    terminal.write(b"snapshot prompt\r")?;
    record.input(b"snapshot prompt\r");
    terminal.wait_for(b"snapshot reply text", Duration::from_secs(10))?;
    // Let the commit burst settle so the captured frame is the final state.
    terminal.collect_for(Duration::from_millis(150))?;
    record.absorb(terminal.output());

    let path = dir.path().join("session.transcript");
    record.save(&path)?;

    // The committed transcript itself must round-trip through the parser.
    let loaded = Transcript::load(&path)?;
    let replayed = loaded.replay(80, 24);
    // The live workspace row ends in the run's random tempdir; normalize
    // everything from the tempdir marker onward or every run drifts there.
    // The temp root itself is spelled per-platform (`/tmp` on Linux,
    // `$TMPDIR`/`private` forms on macOS), so fold it to a stable `/tmp`.
    let temp = std::env::temp_dir();
    let temp_canonical = temp.canonicalize().unwrap_or_else(|_| temp.clone());
    let rows: Vec<String> = replayed
        .screen_rows()
        .into_iter()
        .chain(replayed.scrollback_rows())
        .map(|row| {
            row.replace(temp_canonical.to_string_lossy().as_ref(), "/tmp")
                .replace(temp.to_string_lossy().as_ref(), "/tmp")
        })
        .map(|row| match row.find("dalgon-gates-") {
            Some(at) => format!("{}<workspace>", &row[..at]),
            None => row,
        })
        .filter(|row| !row.trim().is_empty())
        .collect();
    assert!(
        rows.iter().any(|row| row.contains("snapshot reply text")),
        "replayed transcript shows the scripted reply"
    );

    assert_snapshot("loop-tui-12", "settled-screen", &rows)?;

    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    Ok(())
}
// weave: run 'weave explain dal/gates/tests/loop-tui-12.rs' for per-hunk detail, 'weave check' to verify your resolution
