//! Live blocks fed by sequenced updates; settled rows commit exactly once.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use dal_core::{ExtStatus, Stop, StreamChannel, Update, UpdateKind};

const EXT_VISIBLE_ROWS: usize = 3;

/// Braille spinner frames; chrome is ASCII or braille per the design contract.
pub(crate) const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The interval between spinner frames; coalescing caps the loop at 60 Hz and
/// the spinner animates well under it.
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);

/// Lifecycle of one live block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    /// Receiving deltas.
    Open,
    /// Content final; renders at full height before commit.
    Settling,
    /// Bytes frozen; never repainted.
    Settled,
}

/// One row in an aggregation view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRow {
    /// Display name.
    pub name: String,
    /// State word: `failed`, `running`, `done`, `blocked`, `cancelled`, or `lost`.
    pub state: &'static str,
    /// Dispatch order for running rows.
    pub order: usize,
    /// Duration rank for finished rows (lower is faster).
    pub duration_rank: usize,
}

/// Live region owned by the paint thread.
#[derive(Debug, Default)]
pub struct Live {
    assistant_text: String,
    assistant_open: bool,
    tool_cards: HashMap<String, ToolCard>,
    tool_order: Vec<String>,
    emitted_tools: HashSet<String>,
    notices: Vec<String>,
    committed: HashSet<String>,
    ext_busy: BTreeMap<Box<str>, ExtStatus>,
    jobs: HashSet<dal_core::JobId>,
    spinner_frame: usize,
    spinner_last: Option<Instant>,
}

/// One tool card from started through settled.
#[derive(Debug, Clone)]
struct ToolCard {
    name: String,
    state: BlockState,
    tail: String,
    settled: Option<bool>,
    started: Instant,
    duration: Option<Duration>,
}

impl Live {
    /// Advances the braille spinner one frame per tick interval; the loop
    /// drives it, and `DAL_NO_MOTION` simply stops calling it.
    pub fn spin(&mut self, now: Instant) {
        let last = self.spinner_last.get_or_insert(now);
        if now.saturating_duration_since(*last) < SPINNER_INTERVAL {
            return;
        }
        *last = now;
        self.spinner_frame = self.spinner_frame.wrapping_add(1) % SPINNER_FRAMES.len();
    }

    /// The braille spinner cell for this tick.
    #[must_use]
    pub fn spinner_cell(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner_frame]
    }

    /// Applies one sequenced update; unknown kinds change nothing and report false.
    pub fn apply_update(&mut self, update: &Update) -> bool {
        match &update.kind {
            UpdateKind::Delta { channel, text, .. } => {
                if !matches!(channel, StreamChannel::Text) {
                    return false;
                }
                self.assistant_open = true;
                self.assistant_text.push_str(text);
                true
            }
            UpdateKind::ToolStarted { call, tool, .. } => {
                let id = call.as_str().to_owned();
                if !self.tool_cards.contains_key(&id) {
                    self.tool_order.push(id.clone());
                }
                self.tool_cards.insert(
                    call.as_str().to_owned(),
                    ToolCard {
                        name: tool.to_string(),
                        state: BlockState::Open,
                        tail: String::new(),
                        settled: None,
                        started: Instant::now(),
                        duration: None,
                    },
                );
                true
            }
            UpdateKind::ToolProgress { call, tail } => {
                if let Some(card) = self.tool_cards.get_mut(call.as_str()) {
                    card.tail = tail.to_string();
                    true
                } else {
                    false
                }
            }
            UpdateKind::ToolSettled { call, outcome } => {
                if let Some(card) = self.tool_cards.get_mut(call.as_str()) {
                    card.state = BlockState::Settling;
                    card.settled = Some(!outcome.is_error);
                    card.duration = outcome.elapsed_ms.map(Duration::from_millis);
                    card.tail = outcome.text.to_string();
                    true
                } else {
                    false
                }
            }
            UpdateKind::Notice(notice) => {
                self.notices.push(format!("note: {}", notice.text));
                if self.notices.len() > 3 {
                    self.notices.remove(0);
                }
                true
            }
            UpdateKind::RuleFired { rule, .. } => {
                self.notices.push(format!("rule {rule} fired."));
                if self.notices.len() > 3 {
                    self.notices.remove(0);
                }
                true
            }
            UpdateKind::TurnEnded { stop, .. } => {
                if *stop == Stop::Cancelled {
                    self.notices
                        .push(crate::copy::ids::TURN_CANCELLED.to_owned());
                } else if let Some(line) = turn_end_line(*stop) {
                    self.notices.push(line);
                }
                self.assistant_open = false;
                // No tool outlives its turn: a card whose settle update never came
                // (a cancelled call) must not keep a `working` row.
                for card in self.tool_cards.values_mut() {
                    card.state = BlockState::Settled;
                }
                true
            }
            UpdateKind::JobStarted { job } => self.jobs.insert(*job),
            UpdateKind::JobSettled { job } => self.jobs.remove(job),
            UpdateKind::ExtStatus(status) => {
                self.set_ext_status(status);
                true
            }
            UpdateKind::Unknown => false,
            _ => true,
        }
    }

    fn set_ext_status(&mut self, status: &ExtStatus) {
        if status.is_quiet() {
            self.ext_busy.remove(&status.ext);
        } else {
            self.ext_busy.insert(status.ext.clone(), status.clone());
        }
    }

    /// Replaces the busy set with a host snapshot after attach or resync.
    pub fn seed_ext_status(&mut self, statuses: &[ExtStatus]) {
        self.ext_busy.clear();
        for status in statuses {
            self.set_ext_status(status);
        }
    }

    /// Renders busy extensions within the total row allowance, including the fold row.
    #[must_use]
    pub fn ext_lines(&self, cap: usize) -> Vec<String> {
        let total = self.ext_busy.len();
        let max_names = cap.min(EXT_VISIBLE_ROWS);
        let shown = if total > max_names {
            max_names.min(cap.saturating_sub(1))
        } else {
            total
        };
        let mut lines: Vec<String> = self
            .ext_busy
            .values()
            .take(shown)
            .map(|status| {
                let ext = crate::width::escape(&status.ext);
                if let Some(text) = &status.text {
                    let text = crate::width::escape(&one_line(text));
                    crate::copy::render(
                        crate::copy::ids::EXT_ROW,
                        &[("ext", ext.as_str()), ("text", text.as_str())],
                        1,
                    )
                } else {
                    crate::copy::render(crate::copy::ids::EXT_BUSY, &[("ext", ext.as_str())], 1)
                }
            })
            .collect();
        let hidden = total - shown;
        if hidden > 0 && cap > 0 {
            let count = u64::try_from(hidden).unwrap_or(u64::MAX);
            lines.push(crate::copy::render(
                crate::copy::ids::EXT_MORE,
                &[("n", count.to_string().as_str())],
                count,
            ));
        }
        lines
    }

    /// Returns the current assistant text for the live region.
    #[must_use]
    pub fn assistant_text(&self) -> &str {
        &self.assistant_text
    }

    /// Drains completed assistant prose after its entry has committed.
    pub fn take_assistant_text(&mut self) -> String {
        self.assistant_open = false;
        std::mem::take(&mut self.assistant_text)
    }

    /// Returns how long the tool call `call` has run: the settled duration, or
    /// the time since it started when its settle update has not arrived yet.
    #[must_use]
    pub fn tool_elapsed(&self, call: &str) -> Option<Duration> {
        let card = self.tool_cards.get(call)?;
        Some(card.duration.unwrap_or_else(|| card.started.elapsed()))
    }

    /// Returns running tool cards and their latest bounded progress.
    #[must_use]
    pub fn running_tool_rows(&self) -> Vec<String> {
        self.tool_order
            .iter()
            .filter_map(|id| {
                let card = self.tool_cards.get(id)?;
                (card.state == BlockState::Open).then(|| {
                    let name = crate::width::escape(&card.name);
                    let tail = one_line(&card.tail);
                    if tail.is_empty() {
                        format!("working  {name}")
                    } else {
                        format!("working  {name} · {tail}")
                    }
                })
            })
            .collect()
    }

    /// Returns final tool summaries once, in their original call order.
    pub fn take_settled_tool_rows(&mut self) -> Vec<(String, String)> {
        let mut settled = Vec::new();
        for id in &self.tool_order {
            let Some(card) = self.tool_cards.get(id) else {
                continue;
            };
            if card.state == BlockState::Open || !self.emitted_tools.insert(id.clone()) {
                continue;
            }
            let duration = card.duration.map_or_else(String::new, crate::copy::dur);
            let name = crate::width::escape(&card.name);
            let summary = one_line(&card.tail);
            let (template, field) = if card.settled == Some(true) {
                (crate::copy::ids::TOOL_OK, "summary")
            } else {
                (crate::copy::ids::TOOL_FAILED, "reason")
            };
            let line = crate::copy::render(
                template,
                &[("name", &name), (field, &summary), ("dur", &duration)],
                1,
            );
            settled.push((id.clone(), line));
        }
        settled
    }

    /// How many background jobs the host reported started and not yet settled.
    #[must_use]
    pub fn running_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// Borrows notices in arrival order for the live block.
    #[must_use]
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    /// Clears transient notices once a frame has painted them.
    pub fn clear_notices(&mut self) {
        self.notices.clear();
    }

    /// Adds a local command or transport notice, subject to the same three-row cap.
    pub fn notice(&mut self, message: impl Into<String>) {
        self.notices.push(message.into());
        if self.notices.len() > 3 {
            self.notices.remove(0);
        }
    }

    /// Rebuilds transient state from a fresh view after replay was lost.
    pub fn reset_after_resync(&mut self) {
        self.assistant_text.clear();
        self.assistant_open = false;
        self.tool_cards.clear();
        self.tool_order.clear();
        self.emitted_tools.clear();
    }

    /// Commits settled rows exactly once per entry id.
    pub fn commit(&mut self, entry_id: &str, rows: &[String]) -> Vec<String> {
        if !self.committed.insert(entry_id.to_owned()) {
            return Vec::new();
        }
        rows.to_vec()
    }

    /// Renders settled tool headers using the copy deck.
    #[must_use]
    pub fn tool_headers(&self) -> Vec<String> {
        let mut headers: Vec<String> = self
            .tool_cards
            .values()
            .filter(|card| card.state == BlockState::Settled)
            .map(|card| match card.settled {
                Some(true) => format!("ok  {} {} · {}", card.name, one_line(&card.tail), "0.0s"),
                _ => format!(
                    "failed  {} · {} · {}",
                    card.name,
                    one_line(&card.tail),
                    "0.0s"
                ),
            })
            .collect();
        headers.sort();
        headers
    }
    /// One-word activity for the status row, if anything is still open.
    #[must_use]
    pub fn activity(&self) -> Option<&'static str> {
        if self
            .tool_cards
            .values()
            .any(|card| card.state == BlockState::Open)
        {
            Some(crate::copy::ids::STATE_WORKING)
        } else if self.assistant_open {
            Some(crate::copy::ids::STATE_THINKING)
        } else {
            None
        }
    }
}

fn turn_end_line(stop: Stop) -> Option<String> {
    match stop {
        Stop::EndTurn | Stop::Cancelled | Stop::MaxSteps | Stop::Failed => None,
        Stop::Length => Some(crate::copy::ids::TURN_LENGTH.to_owned()),
        Stop::Filter => Some(crate::copy::ids::TURN_FILTER.to_owned()),
    }
}

fn one_line(text: &str) -> String {
    crate::width::escape(&text.replace('\n', " "))
}

/// Renders an aggregation header and bounded rows: failed first, running in
/// dispatch order, fastest finished; settled rows never move.
#[must_use]
pub fn aggregate_children(
    running: usize,
    failed: usize,
    done: usize,
    mut rows: Vec<ChildRow>,
    cap: usize,
) -> Vec<String> {
    rows.sort_by(|left, right| {
        rank(left.state)
            .cmp(&rank(right.state))
            .then_with(|| left.order.cmp(&right.order))
            .then_with(|| left.duration_rank.cmp(&right.duration_rank))
    });
    let mut lines = vec![format!("{running} running · {failed} failed · {done} done")];
    let shown = rows.len().min(cap);
    for row in rows.iter().take(shown) {
        lines.push(format!("{} · {}", row.name, row.state));
    }
    if rows.len() > shown {
        let rest = rows.len() - shown;
        let breakdown = summarize(&rows[shown..]);
        lines.push(format!("({rest} more: {breakdown})"));
    }
    lines
}

fn rank(state: &str) -> u8 {
    match state {
        "failed" => 0,
        "running" => 1,
        _ => 2,
    }
}

fn summarize(rows: &[ChildRow]) -> String {
    let mut running = 0;
    let mut failed = 0;
    let mut done = 0;
    for row in rows {
        match row.state {
            "running" => running += 1,
            "failed" => failed += 1,
            _ => done += 1,
        }
    }
    format!("{running} running · {failed} failed · {done} done")
}

#[cfg(test)]
mod tests {
    use super::{ChildRow, Live, SPINNER_FRAMES, aggregate_children};
    use dal_core::{ExtState, ExtStatus, Gen, Seq, Update, UpdateKind};
    use std::time::{Duration, Instant};

    #[test]
    fn the_spinner_advances_braille_frames_from_ticks() {
        let start = Instant::now();
        let mut live = Live::default();
        assert_eq!(live.spinner_cell(), SPINNER_FRAMES[0]);
        // A tick inside the interval keeps the frame the loop started on.
        live.spin(start);
        assert_eq!(live.spinner_cell(), SPINNER_FRAMES[0]);
        for step in 1..SPINNER_FRAMES.len() * 2 {
            let at = start + Duration::from_millis(100 * u64::try_from(step).unwrap_or(u64::MAX));
            live.spin(at);
            assert_eq!(
                live.spinner_cell(),
                SPINNER_FRAMES[step % SPINNER_FRAMES.len()],
                "tick {step} advances the braille run and wraps it"
            );
        }
    }

    fn status_update(seq: u64, ext: &str, state: ExtState, text: Option<&str>) -> Update {
        let seq = Seq::new(std::num::NonZeroU64::new(seq).unwrap_or(std::num::NonZeroU64::MIN));
        Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq,
            kind: UpdateKind::ExtStatus(ExtStatus {
                ext: ext.into(),
                state,
                text: text.map(Into::into),
            }),
        }
    }

    #[test]
    fn running_tool_row_has_no_dangling_separator_before_any_progress() {
        let mut live = super::Live::default();
        let seq = |value: u64| {
            Seq::new(std::num::NonZeroU64::new(value).unwrap_or(std::num::NonZeroU64::MIN))
        };
        let started = Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: seq(1),
            kind: UpdateKind::ToolStarted {
                call: dal_core::CallId::new("call-1"),
                tool: "exec".into(),
                args: dal_core::RawJson::null(),
            },
        };
        assert!(live.apply_update(&started));
        assert_eq!(live.running_tool_rows(), ["working  exec"]);
        let progress = Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: seq(2),
            kind: UpdateKind::ToolProgress {
                call: dal_core::CallId::new("call-1"),
                tail: "compiling".into(),
            },
        };
        assert!(live.apply_update(&progress));
        assert_eq!(live.running_tool_rows(), ["working  exec · compiling"]);
    }

    #[test]
    fn ext_status_fold() {
        let mut live = super::Live::default();
        for (seq, ext) in [
            (1, "alpha"),
            (2, "bravo"),
            (3, "charlie"),
            (4, "delta"),
            (5, "echo"),
        ] {
            let text = (ext != "bravo").then_some("indexing");
            assert!(live.apply_update(&status_update(seq, ext, ExtState::Busy, text)));
        }
        assert!(live.apply_update(&status_update(6, "zulu", ExtState::Busy, Some("gone"))));
        assert!(live.apply_update(&status_update(7, "zulu", ExtState::Quiet, None)));
        assert_eq!(
            live.ext_lines(4),
            [
                "alpha: indexing",
                "bravo: busy",
                "charlie: indexing",
                "(2 more extensions busy)"
            ]
        );
        assert!(live.ext_lines(4).iter().all(|line| !line.contains("zulu")));
        assert_eq!(live.ext_lines(4).len(), 4);
        assert_eq!(
            live.ext_lines(2),
            ["alpha: indexing", "(4 more extensions busy)"]
        );
        assert!(live.apply_update(&status_update(8, "delta", ExtState::Quiet, None)));
        assert_eq!(
            live.ext_lines(4).last().map(String::as_str),
            Some("(1 more extension busy)")
        );
        assert!(live.apply_update(&status_update(9, "echo", ExtState::Quiet, None)));
        assert_eq!(
            live.ext_lines(4).last().map(String::as_str),
            Some("charlie: indexing")
        );
    }

    #[test]
    fn ext_status_fold_singular_and_seed() {
        let mut live = super::Live::default();
        live.seed_ext_status(&[
            ExtStatus {
                ext: "a".into(),
                state: ExtState::Busy,
                text: None,
            },
            ExtStatus {
                ext: "b".into(),
                state: ExtState::Busy,
                text: None,
            },
            ExtStatus {
                ext: "c".into(),
                state: ExtState::Busy,
                text: None,
            },
            ExtStatus {
                ext: "d".into(),
                state: ExtState::Busy,
                text: None,
            },
            ExtStatus {
                ext: "e".into(),
                state: ExtState::Quiet,
                text: Some("idle".into()),
            },
        ]);
        assert_eq!(
            live.ext_lines(4),
            ["a: busy", "b: busy", "c: busy", "(1 more extension busy)"]
        );
        live.seed_ext_status(&[]);
        assert!(live.ext_lines(3).is_empty());
    }

    #[test]
    fn aggregation_orders_failed_running_finished_and_folds() {
        let mut rows = Vec::new();
        for index in 0..487 {
            rows.push(ChildRow {
                name: format!("child-{index:03}"),
                state: "running",
                order: index,
                duration_rank: 0,
            });
        }
        for index in 0..3 {
            rows.push(ChildRow {
                name: format!("fail-{index}"),
                state: "failed",
                order: 1_000 + index,
                duration_rank: 0,
            });
        }
        for index in 0..10 {
            rows.push(ChildRow {
                name: format!("done-{index:02}"),
                state: "done",
                order: 2_000 + index,
                duration_rank: index,
            });
        }
        let lines = aggregate_children(487, 3, 10, rows, 8);
        assert_eq!(lines[0], "487 running · 3 failed · 10 done");
        assert!(lines[1].contains("fail-0"));
        assert!(lines[3].contains("fail-2"));
        assert!(lines[4].contains("child-000"));
        assert_eq!(lines.len(), 10);
        assert!(lines[9].starts_with("(492 more:"));
    }

    #[test]
    fn cancellation_does_not_become_a_persisted_turn_end_row() {
        assert_eq!(super::turn_end_line(dal_core::Stop::Cancelled), None);
    }

    #[test]
    fn activity_reports_open_assistant_and_tools() {
        use super::{BlockState, Live, ToolCard};
        let mut live = Live::default();
        assert_eq!(live.activity(), None);
        live.assistant_open = true;
        assert_eq!(live.activity(), Some(crate::copy::ids::STATE_THINKING));
        live.tool_cards.insert(
            "call-1".to_owned(),
            ToolCard {
                name: "read".to_owned(),
                state: BlockState::Open,
                tail: String::new(),
                settled: None,
                started: std::time::Instant::now(),
                duration: None,
            },
        );
        assert_eq!(live.activity(), Some(crate::copy::ids::STATE_WORKING));
        live.assistant_open = false;
        if let Some(card) = live.tool_cards.get_mut("call-1") {
            card.state = BlockState::Settled;
        }
        assert_eq!(live.activity(), None);
    }

    #[test]
    fn a_cancelled_turn_leaves_no_working_row() {
        let mut live = super::Live::default();
        let update = |seq: u64, kind| Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: Seq::new(std::num::NonZeroU64::new(seq).unwrap_or(std::num::NonZeroU64::MIN)),
            kind,
        };
        assert!(live.apply_update(&update(
            1,
            UpdateKind::ToolStarted {
                call: dal_core::CallId::new("call-1"),
                tool: "exec".into(),
                args: dal_core::RawJson::null(),
            },
        )));
        assert_eq!(live.running_tool_rows(), ["working  exec"]);
        assert!(live.apply_update(&update(
            2,
            UpdateKind::TurnEnded {
                turn: dal_core::TurnId::new(std::num::NonZeroU64::MIN),
                stop: dal_core::Stop::Cancelled,
            },
        )));
        assert!(live.running_tool_rows().is_empty());
        assert_eq!(live.activity(), None);
    }

    #[test]
    fn started_and_settled_job_updates_drive_the_running_count() {
        let mut live = super::Live::default();
        let update = |seq: u64, kind| Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: Seq::new(std::num::NonZeroU64::new(seq).unwrap_or(std::num::NonZeroU64::MIN)),
            kind,
        };
        let first = dal_core::JobId::new_v7();
        let second = dal_core::JobId::new_v7();
        live.apply_update(&update(1, UpdateKind::JobStarted { job: first }));
        live.apply_update(&update(2, UpdateKind::JobStarted { job: second }));
        live.apply_update(&update(3, UpdateKind::JobStarted { job: second }));
        assert_eq!(live.running_jobs(), 2);
        live.apply_update(&update(4, UpdateKind::JobSettled { job: first }));
        assert_eq!(live.running_jobs(), 1);
    }
}
