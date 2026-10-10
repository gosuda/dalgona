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
/// How often the screen and disk are re-read.
const POLL: Duration = Duration::from_millis(100);
/// Four of the orchestration runtime's 250 ms delivery ticks: a refused grant
/// that the poll would raise again shows within one tick.
const QUIET: Duration = Duration::from_secs(1);
const ARMED_HINT: &str = "y allow · a session · n deny";
const LOCKED_HINT: &str = "esc denies · answer keys ready in a moment";

/// One fresh-install run of the real binary.
struct Run {
    pane: Pane,
}

/// Whether the dialog is on screen now: a row that opens with its title.
fn question_open(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with(TITLE))
}

/// Whether the dialog lists its answer keys, which only an armed dialog does.
fn question_armed(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with(ARMED_HINT))
}

/// Whether the dialog is on screen and still ignores answer keys.
fn question_unarmed(text: &str) -> bool {
    question_open(text)
        && text
            .lines()
            .any(|line| line.trim_start().starts_with(LOCKED_HINT))
}

impl Run {
    fn start() -> support::TestResult<Self> {
        let scratch = support::Scratch::new("orchestration-grant-question")?;
        let run = Self {
            pane: Pane::start(scratch)?,
        };
        // The session-start goal load can ask for the grant before the
        // composer first paints, so an open question also means interactive.
        run.until("startup", |text| {
            text.contains(READY) || question_open(text)
        })?;
        Ok(run)
    }

    /// Whether the session's goal sidecar is on disk.
    fn goal_saved(&self) -> bool {
        std::fs::read_dir(self.pane.scratch().path().join("xdg-data/dalgona/sessions"))
            .into_iter()
            .flatten()
            .flatten()
            .flat_map(|workspace| std::fs::read_dir(workspace.path()).into_iter().flatten())
            .flatten()
            .any(|session| {
                session
                    .path()
                    .join("sidecar/orchestration/goal.json")
                    .is_file()
            })
    }

    fn until_goal_saved(&self) -> support::TestResult<()> {
        let deadline = Instant::now() + WAIT;
        while !self.goal_saved() {
            if Instant::now() > deadline {
                let text = self.pane.text()?;
                return Err(format!("the goal sidecar was never saved:\n{text}").into());
            }
            std::thread::sleep(POLL);
        }
        Ok(())
    }

    /// Fails as soon as `holds` stops being true within `window`.
    fn stays(&self, window: Duration, holds: impl Fn(&str) -> bool) -> support::TestResult<()> {
        let deadline = Instant::now() + window;
        loop {
            let text = self.pane.text()?;
            if !holds(&text) {
                return Err(format!("the screen changed within {window:?}:\n{text}").into());
            }
            if Instant::now() > deadline {
                return Ok(());
            }
            std::thread::sleep(POLL);
        }
    }

    /// Polls until `done` holds and returns the screen that proved it.
    fn until(&self, what: &str, done: impl Fn(&str) -> bool) -> support::TestResult<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.pane.text()?;
            if done(&text) {
                return Ok(text);
            }
            if Instant::now() > deadline {
                return Err(format!("{what}: the screen never matched:\n{text}").into());
            }
            std::thread::sleep(POLL);
        }
    }

    /// Presses `key` for the open question once the question lists its answer
    /// keys, then waits until the dialog has closed. The acknowledged-answer
    /// note is not awaited: a queue resync can drop it while the answer
    /// itself still resolves.
    fn answer(&mut self, key: &str) -> support::TestResult<()> {
        if !question_open(&self.pane.text()?) {
            return Ok(());
        }
        self.until("armed", question_armed)?;
        self.pane.type_text(key)?;
        self.until("closed", |text| !question_open(text))?;
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
                return Err(format!("submitted: the screen never matched:\n{text}").into());
            }
            std::thread::sleep(POLL);
        }
    }
}

#[test]
fn an_approved_grant_question_is_stored_journaled_and_unlocks_goal() -> support::TestResult<()> {
    let mut run = Run::start()?;
    run.submit("/goal write the parser", "y", |text| {
        text.contains("goal g1: active")
    })?;
    run.until_goal_saved()?;
    run.submit("/goal", "y", |text| {
        text.matches("objective: write the parser").count() >= 2
    })?;

    // Exactly one answer is proven by the single journaled `resolved` record
    // below; the on-screen acknowledgment note is not reliable.
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
            && text.contains("the user declined the request")
    })?;
    run.stays(QUIET, |text| {
        !text.contains("goal g1: active") && !question_open(text)
    })?;

    let text = run.pane.text()?;
    assert!(
        !text.contains("goal g1: active"),
        "a declined grant creates no goal:\n{text}"
    );
    assert!(
        !question_open(&text),
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

#[test]
fn keys_typed_as_the_grant_question_opens_answer_nothing() -> support::TestResult<()> {
    // The burst below must reach the app while the dialog is still locked.
    // Two load races lose that window: the dialog can arm before the burst
    // lands, and the session-start load can ask before the burst is even
    // staged. Either restarts on a fresh install. A dialog that answers
    // before ever showing locked keys fails every attempt.
    for _ in 0..3 {
        if locked_typeahead_is_dropped()? {
            return Ok(());
        }
    }
    Err("the type-ahead burst never landed while locked in three installs; rerun the gate".into())
}

/// Types `a` — the session-approval key — at the grant question while it is
/// still locked, and reports whether every key was dropped.
///
/// Returns `Ok(false)` when the burst reached the app after arming and
/// answered it, or when the session-start load asked first; answering after
/// the answer keys list is correct behavior either way. Only one burst is
/// ever in flight, so a slow runner delays the burst instead of typing
/// through the armed dialog.
fn locked_typeahead_is_dropped() -> support::TestResult<bool> {
    let run = Run::start()?;
    if question_open(&run.pane.text()?) {
        // A dialog is already open, so the goal command cannot be staged:
        // typed keys would land in the dialog, and answering it now would
        // consume the scenario this install can ask about.
        return Ok(false);
    }
    run.pane.type_text("/goal write the parser")?;
    run.pane.enter()?;
    // This question is ours, or the session-start load's when it swallowed
    // our Enter; either way nothing has been answered yet. The burst goes
    // out only against the screen that still shows locked keys.
    let opened = run.until("opened", question_open)?;
    if !question_unarmed(&opened) {
        return Ok(false);
    }
    run.pane.type_text("aaa")?;
    let deadline = Instant::now() + WAIT;
    loop {
        let text = run.pane.text()?;
        if text.matches(ANSWERED).count() > 0 || grants_file(&run).exists() {
            return Ok(false);
        }
        if question_armed(&text) {
            break;
        }
        if Instant::now() > deadline {
            return Err(format!("the grant question never armed:\n{text}").into());
        }
        std::thread::sleep(POLL);
    }
    assert!(!grants_file(&run).exists(), "type-ahead stored a grant");

    // Keys arrive in order, so this `y` is processed after the burst: a
    // burst that answered first journals `approve_for_session`, while ours
    // journals `approve`. Either way the answer is attributed with no timing.
    run.pane.type_text("y")?;
    run.until("closed", |text| !question_open(text))?;
    if burst_answered_first(&run)? {
        return Ok(false);
    }

    // An Enter swallowed by the session-start load's dialog leaves the goal
    // command in the composer; an empty submit is a no-op otherwise.
    run.pane.enter()?;
    run.until("goal-active", |text| text.contains("goal g1: active"))?;
    let stored = std::fs::read_to_string(grants_file(&run))?;
    assert!(stored.contains("ext = \"orchestration\""), "{stored}");
    Ok(true)
}

fn grants_file(run: &Run) -> std::path::PathBuf {
    run.pane
        .scratch()
        .path()
        .join("xdg-data/dalgona/grants.toml")
}

/// Whether the install's journaled answer approved the whole session, which
/// only the type-ahead burst's `a` key requests; our deliberate `y` approves
/// once.
fn burst_answered_first(run: &Run) -> support::TestResult<bool> {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(journal) = support::session_journal(run.pane.scratch()) {
            let resolved = support::journal_records(&journal, "resolved");
            if resolved.len() == 1 {
                let session = resolved[0].contains(r#""answer":"approve_for_session""#);
                if !session && !resolved[0].contains(r#""answer":"approve""#) {
                    return Err(format!("unrecognized answer journal: {}", resolved[0]).into());
                }
                return Ok(session);
            }
            if resolved.len() > 1 {
                return Err("one question deserves one answer".into());
            }
        }
        if Instant::now() > deadline {
            let text = run.pane.text()?;
            return Err(format!("the answer was never journaled:\n{text}").into());
        }
        std::thread::sleep(POLL);
    }
}
