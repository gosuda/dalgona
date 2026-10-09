// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
//!
//! A fresh install asks once, through the terminal's own question dialog,
//! before the bundled orchestration battery uses its services. An approval is
//! stored and journaled, and `/goal` then works. A decline fails closed with
//! the denial text and stores nothing. The gate needs tmux on `PATH`.
#![cfg(unix)]

use std::time::{Duration, Instant};

use gates::support::{self, Pane};

const TITLE: &str = "Allow orchestration to use";
const ANSWERED: &str = "answered \"Allow orchestration";
const READY: &str = "Ask dal to change code.";
const WAIT: Duration = Duration::from_secs(60);

/// One fresh-install run of the real binary.
struct Run {
    pane: Pane,
    /// Keys sent to the question dialog so far.
    answers: usize,
}

/// Whether the dialog is on screen now: a row that opens with its title.
fn question_open(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with(TITLE))
}

impl Run {
    fn start() -> support::TestResult<Self> {
        let scratch = support::Scratch::new("orchestration-grant-question")?;
        let run = Self {
            pane: Pane::start(scratch)?,
            answers: 0,
        };
        run.until(|text| text.contains(READY))?;
        // Startup work (the first goal load, the first poll) settles within
        // moments; the state after this pause is stable.
        std::thread::sleep(Duration::from_secs(4));
        Ok(run)
    }

    fn until(&self, done: impl Fn(&str) -> bool) -> support::TestResult<()> {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.pane.text()?;
            if done(&text) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("the screen never matched:\n{text}").into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Presses `key` for the open question once its predecessor's answer is
    /// acknowledged on screen.
    fn answer(&mut self, key: &str) -> support::TestResult<()> {
        let text = self.pane.text()?;
        if question_open(&text) && text.matches(ANSWERED).count() == self.answers {
            self.pane.type_text(key)?;
            self.answers += 1;
        }
        Ok(())
    }

    /// Types and submits `line`, answering every question with `key` until
    /// `done` holds. A question already open is answered first, so no typed
    /// character reaches the dialog.
    fn submit(
        &mut self,
        line: &str,
        key: &str,
        done: impl Fn(&str) -> bool,
    ) -> support::TestResult<()> {
        self.answer(key)?;
        std::thread::sleep(Duration::from_millis(500));
        self.pane.type_text(line)?;
        self.pane.enter()?;
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.pane.text()?;
            if done(&text) {
                return Ok(());
            }
            self.answer(key)?;
            if Instant::now() > deadline {
                return Err(format!("the screen never matched:\n{text}").into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

#[test]
fn an_approved_grant_question_is_stored_journaled_and_unlocks_goal() -> support::TestResult<()> {
    let mut run = Run::start()?;
    run.submit("/goal write the parser", "y", |text| {
        text.contains("goal g1: active")
    })?;
    std::thread::sleep(Duration::from_secs(1));
    run.submit("/goal", "y", |text| {
        text.matches("objective: write the parser").count() >= 2
    })?;

    let text = run.pane.text()?;
    assert_eq!(
        text.matches(ANSWERED).count(),
        1,
        "the user is asked once:\n{text}"
    );
    let grants = std::fs::read_to_string(
        run.pane
            .scratch()
            .path()
            .join("xdg-data/dalgona/grants.toml"),
    )?;
    assert!(grants.contains("ext = \"orchestration\""), "{grants}");
    assert!(grants.contains("origin = \"bundled\""), "{grants}");
    assert!(grants.contains("by = \"dal-tui\""), "{grants}");
    let journal = support::session_journal(run.pane.scratch())?;
    let resolved = support::journal_records(&journal, "resolved");
    assert_eq!(resolved.len(), 1, "the answer is journaled:\n{journal}");
    assert!(
        resolved[0].contains(r#""answer":"approve""#) && resolved[0].contains("dal-tui"),
        "{}",
        resolved[0]
    );
    Ok(())
}

#[test]
fn a_declined_grant_question_fails_closed_and_stores_nothing() -> support::TestResult<()> {
    let mut run = Run::start()?;
    run.submit("/goal write the parser", "n", |text| {
        text.contains("the session store is not available")
    })?;
    std::thread::sleep(Duration::from_secs(3));

    let text = run.pane.text()?;
    assert!(
        !text.contains("goal g1: active"),
        "a declined grant creates no goal:\n{text}"
    );
    assert!(
        !question_open(&text) && text.matches(ANSWERED).count() == run.answers,
        "a decline is not asked again until the user acts:\n{text}"
    );
    assert!(
        !run.pane
            .scratch()
            .path()
            .join("xdg-data/dalgona/grants.toml")
            .exists(),
        "a decline stores nothing"
    );
    assert!(
        text.contains("the user declined the request"),
        "the denial names the decline:\n{text}"
    );
    Ok(())
}
