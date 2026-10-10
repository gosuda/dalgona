#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Verifies the inline transcript keeps one spacing for every turn: each
//! settled block is followed by exactly one blank row, so a prompt, its
//! reply, and the next prompt sit two rows apart on turn one and on every
//! later turn, a tool card included, and the live block starts one blank row
//! under the last block.

#[path = "support/pty.rs"]
#[expect(
    dead_code,
    reason = "PTY support helpers are shared across TUI gate targets"
)]
mod pty;
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;
#[path = "support/vt.rs"]
mod vt;

use std::{
    error::Error,
    process::Command,
    time::{Duration, Instant},
};

use pty::{PtyProcess, dalgon_command, dalgon_command_with_fixture};
use support::TestDir;
use vt::VtRecorder;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const COLUMNS: u16 = 100;
const ROWS: u16 = 30;

/// Answers the capability probe without confirming kitty keyboard encoding,
/// so the legacy key column is the working one.
fn probe_answers() -> &'static [u8] {
    b"\x1b[?2026;2$y\x1b[?2027;2$y\x1b]11;rgb:0000/0000/0000\x07\x1b[?1;2c"
}

/// Replays everything the child printed and returns the visible rows once the
/// recorder sits outside a synchronized update, trimmed of trailing blanks.
fn replayed_rows(terminal: &PtyProcess) -> (VtRecorder, Vec<String>) {
    let mut screen = VtRecorder::new(COLUMNS, ROWS);
    screen.feed(terminal.output());
    let rows = screen
        .screen_rows()
        .into_iter()
        .map(|row| row.trim_end().to_owned())
        .collect();
    (screen, rows)
}

/// Whether the last non-empty row, the status row, carries no spinner glyph
/// and no turn state word, which holds only after the turn left its running
/// state and the frame behind it was drawn.
fn status_row_is_idle(rows: &[String]) -> bool {
    use dal_tui::copy::ids;
    let Some(status) = rows.iter().rev().find(|row| !row.is_empty()) else {
        return false;
    };
    let spinner = status
        .chars()
        .any(|cell| ('\u{2800}'..='\u{28ff}').contains(&cell));
    let state_words = [
        ids::STATE_THINKING,
        ids::STATE_WORKING,
        ids::STATE_FETCHING,
        ids::STATE_COMPACTING,
        ids::STATE_RETRYING,
        ids::STATE_WAITING,
    ];
    !spinner && !state_words.iter().any(|word| status.contains(word))
}

/// Drains output until the replayed screen, outside any synchronized update,
/// satisfies `ready`; reports the last screen when the deadline passes.
fn wait_for_screen(
    terminal: &mut PtyProcess,
    what: &str,
    ready: impl Fn(&[String]) -> bool,
) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        terminal.collect_for(Duration::from_millis(20))?;
        let (screen, rows) = replayed_rows(terminal);
        if !screen.sync_is_open() && ready(&rows) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("screen never showed {what}: {rows:#?}").into());
        }
    }
}

/// Starts `command` inline, types each prompt, waits for the text that ends
/// its turn, and returns the settled screen as trimmed rows.
fn run_turns(
    command: &mut Command,
    turns: &[(&str, &str)],
) -> Result<(PtyProcess, Vec<String>), Box<dyn Error + Send + Sync>> {
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(command, COLUMNS, ROWS)?;
    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;
    terminal.write(probe_answers())?;
    terminal.collect_for(Duration::from_millis(20))?;
    for (prompt, reply) in turns {
        terminal.write(format!("{prompt}\r").as_bytes())?;
        let reply_row = format!("  {reply}");
        wait_for_screen(&mut terminal, "the reply settled", |rows| {
            rows.iter().any(|row| *row == reply_row) && status_row_is_idle(rows)
        })?;
    }
    let (screen, rows) = replayed_rows(&terminal);
    assert_eq!(screen.erase_display_sequences(), 0);
    assert!(!screen.sync_is_open());
    Ok((terminal, rows))
}

/// Asserts the screen ends with `blocks`, one blank row after each, then the
/// composer, hint, and status rows at the bottom.
fn assert_blocks_then_live_block(rows: &[String], blocks: &[&str]) -> TestResult {
    let first = rows
        .iter()
        .position(|row| row == blocks[0])
        .ok_or_else(|| format!("no row equals {:?} in {rows:#?}", blocks[0]))?;
    let expected: Vec<String> = blocks
        .iter()
        .flat_map(|block| [(*block).to_owned(), String::new()])
        .collect();
    assert_eq!(
        rows.get(first..first + expected.len()),
        Some(expected.as_slice()),
        "one blank row after every block: {rows:#?}"
    );
    let composer = rows
        .get(first + expected.len())
        .ok_or("the live block is missing under the transcript")?;
    assert_eq!(
        composer,
        &format!("> {}", dal_tui::copy::ids::COMPOSER_PLACEHOLDER),
        "the composer row follows the last blank row: {rows:#?}"
    );
    assert_eq!(
        first + expected.len() + 3,
        rows.len(),
        "the live block is the composer, hint, and status rows at the screen bottom: {rows:#?}"
    );
    Ok(())
}

fn quit(mut terminal: PtyProcess) -> TestResult {
    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    Ok(())
}

#[test]
fn tui_inline_turns_share_one_spacing() -> TestResult {
    let turns = [
        ("first inline prompt", "inline reply one"),
        ("second inline prompt", "inline reply two"),
        ("third inline prompt", "inline reply three"),
    ];
    let dir = TestDir::new()?;
    let replies = turns.map(|(_, reply)| reply);
    let mut command = dalgon_command(dir.path(), &replies)?;
    let (terminal, rows) = run_turns(&mut command, &turns)?;

    // Every turn: reply two rows under its prompt, next prompt two rows under
    // the reply, whichever frame each block settled in.
    let at = |wanted: String| {
        rows.iter()
            .position(|row| *row == wanted)
            .ok_or_else(|| format!("no row equals {wanted:?} in {rows:#?}"))
    };
    let mut gaps = Vec::new();
    let mut previous_reply = None;
    for (prompt, reply) in turns {
        let prompt_at = at(format!("> {prompt}"))?;
        let reply_at = at(format!("  {reply}"))?;
        gaps.push(reply_at - prompt_at);
        if let Some(previous) = previous_reply {
            gaps.push(prompt_at - previous);
        }
        previous_reply = Some(reply_at);
    }
    assert_eq!(
        gaps, [2; 5],
        "every prompt, reply, and next prompt sits two rows apart: {rows:#?}"
    );

    let blocks: Vec<String> = turns
        .iter()
        .flat_map(|(prompt, reply)| [format!("> {prompt}"), format!("  {reply}")])
        .collect();
    let blocks: Vec<&str> = blocks.iter().map(String::as_str).collect();
    assert_blocks_then_live_block(&rows, &blocks)?;
    quit(terminal)
}

/// A running tool grows the live block with activity rows; its settled card
/// then replaces them. The rows left behind must not show as extra blanks.
#[test]
fn tui_inline_tool_turn_keeps_the_same_spacing() -> TestResult {
    const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";
    let exec = "echo line1; sleep 1; echo line2; sleep 1; echo line3";
    let exec = sonic_rs::to_string(exec)?;
    let fixture = format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"call-exec\",\"name\":\"exec\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"call-exec\",\"name\":\"exec\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"command\":{exec},\"timeout_seconds\":30}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n\
         {{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"tool reply one\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n\
         {{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"plain reply two\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    );
    let dir = TestDir::new()?;
    let mut command = dalgon_command_with_fixture(dir.path(), &fixture)?;
    // Run the tool without an approval question.
    let config = dir.path().join(".config/dal/dal.toml");
    let text =
        std::fs::read_to_string(&config)?.replace("approval = \"ask\"", "approval = \"all\"");
    std::fs::write(&config, text)?;
    let (terminal, rows) = run_turns(
        &mut command,
        &[
            ("first tool prompt", "tool reply one"),
            ("second plain prompt", "plain reply two"),
        ],
    )?;
    let card = rows
        .iter()
        .find(|row| row.starts_with("  ok  exec"))
        .ok_or_else(|| format!("no settled exec card in {rows:#?}"))?
        .clone();
    assert_blocks_then_live_block(
        &rows,
        &[
            "> first tool prompt",
            &card,
            "  tool reply one",
            "> second plain prompt",
            "  plain reply two",
        ],
    )?;
    quit(terminal)
}

/// Opening the command popup grows the live block; closing it shrinks the
/// block again. The rows it frees go below the status row, so no blank row
/// comes between the transcript and the composer.
#[test]
fn tui_inline_popup_close_leaves_no_blank_rows_above_the_composer() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["inline reply one"])?;
    let (mut terminal, _) =
        run_turns(&mut command, &[("first inline prompt", "inline reply one")])?;
    terminal.write(b"/")?;
    terminal.wait_for(b"/bug \xc2\xb7 not in dalgon", Duration::from_secs(10))?;
    terminal.write(b"\x1b")?;
    terminal.collect_for(Duration::from_millis(300))?;
    terminal.write(b"\x7f")?;
    let placeholder = format!("> {}", dal_tui::copy::ids::COMPOSER_PLACEHOLDER);
    wait_for_screen(&mut terminal, "the emptied composer", |rows| {
        rows.iter().any(|row| *row == placeholder)
    })?;

    let (_, rows) = replayed_rows(&terminal);
    let first = rows
        .iter()
        .position(|row| row == "> first inline prompt")
        .ok_or_else(|| format!("no prompt row in {rows:#?}"))?;
    assert_eq!(
        rows.get(first..first + 5),
        Some(
            [
                "> first inline prompt",
                "",
                "  inline reply one",
                "",
                &format!("> {}", dal_tui::copy::ids::COMPOSER_PLACEHOLDER),
            ]
            .map(str::to_owned)
            .as_slice()
        ),
        "the composer follows the last blank row after the popup closed: {rows:#?}"
    );
    assert!(
        rows.get(first + 6)
            .is_some_and(|row| row.starts_with("openai-responses/")),
        "the status row follows the hint row: {rows:#?}"
    );
    quit(terminal)
}
