//! Live blocks fed by sequenced updates; settled rows commit exactly once.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_core::{BlobId, ExtStatus, Stop, StreamChannel, Update, UpdateKind};

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
    /// Bytes frozen; never repainted. No live tool card holds this state: a
    /// turn end releases its cards instead of keeping them settled.
    Settled,
}

/// Live region owned by the paint thread.
#[derive(Debug, Default)]
pub struct Live {
    product: crate::copy::Identity,
    assistant_text: String,
    stream: RefCell<crate::markdown::MarkdownStream>,
    assistant_open: bool,
    tool_cards: HashMap<String, ToolCard>,
    tool_order: Vec<String>,
    notices: Vec<NoticeRow>,
    ext_busy: BTreeMap<Box<str>, ExtStatus>,
    jobs: HashSet<dal_core::JobId>,
    /// The card the expand view holds open, with its scroll offset.
    expand: Option<ExpandedView>,
    spinner_frame: usize,
    spinner_last: Option<Instant>,
}

/// One live-block notice: text plus the role its severity chose at push
/// time, so an error notice paints in the error role instead of flattening
/// into dim text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeRow {
    /// The rendered notice text, including any `note:` prefix.
    pub text: String,
    /// The semantic role the row paints in.
    pub role: crate::theme::Role,
}

/// One card the user expanded with the newest-card action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpandedCard {
    /// The reasoning blocks of an assistant entry.
    Reasoning { entry: dal_core::EntryId },
    /// The full stored output of a settled tool result entry.
    Tool { entry: dal_core::EntryId },
    /// The source of the fence at `fence` inside the `block` text block of an
    /// assistant entry.
    Diagram {
        /// The entry holding the fenced source block.
        entry: dal_core::EntryId,
        /// Index into the entry's content blocks.
        block: usize,
        /// Index into `markdown::fence_spans` over that block's text: the
        /// ordinal is stable under append because the scan is deterministic
        /// and committed text is immutable.
        fence: usize,
    },
}

/// Open expand view: which card plus its window geometry and blob reads.
///
/// `scroll` is the first visible content row, `window` the last observed
/// window height, and `content` the last measured row count; the cells update
/// at render time so scroll keys never race the frame that measured them.
/// `blobs` records one read per `TextBlob` part the card names, filled by the
/// worker the runtime starts when the view opens; the worker holds a `Weak`,
/// so closing or retargeting the view cancels the read for it.
#[derive(Debug)]
pub(crate) struct ExpandedView {
    /// The expanded card.
    pub(crate) card: ExpandedCard,
    /// One `Pending` slot per `TextBlob` part, resolved once by the read.
    blobs: Arc<Mutex<HashMap<BlobId, BlobRead>>>,
    scroll: Cell<usize>,
    window: Cell<usize>,
    content: Cell<usize>,
}

/// The one read of a card's stored blob: in flight, decoded text, or failed.
#[derive(Debug, Clone)]
pub(crate) enum BlobRead {
    /// The runtime's worker has not answered yet.
    Pending,
    /// The stored bytes decoded as the UTF-8 text a `TextBlob` always holds.
    Text(Arc<str>),
    /// The read failed; the message is the agent's own error text.
    Failed(Arc<str>),
}

impl ExpandedView {
    /// Opens the view pinned to the card's first row, registering one pending
    /// read per `TextBlob` digest the card names.
    pub(crate) fn new(card: ExpandedCard, blob_ids: Vec<BlobId>) -> Self {
        Self {
            card,
            blobs: Arc::new(Mutex::new(
                blob_ids
                    .into_iter()
                    .map(|id| (id, BlobRead::Pending))
                    .collect(),
            )),
            scroll: Cell::new(0),
            window: Cell::new(0),
            content: Cell::new(0),
        }
    }

    /// The weak end of the shared slot map the runtime's read worker fills:
    /// the worker upgrades per write, so dropping this view cancels the read.
    pub(crate) fn blob_sink(&self) -> std::sync::Weak<Mutex<HashMap<BlobId, BlobRead>>> {
        Arc::downgrade(&self.blobs)
    }

    /// The recorded read of one blob, cloning nothing but the Arc inside.
    pub(crate) fn blob(&self, id: &BlobId) -> Option<BlobRead> {
        self.blobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// The row count the last painted frame produced, when one did. A view
    /// that has not painted yet reports `None` so the allocator grants the
    /// full overlay allowance rather than guessing a smaller natural height.
    pub(crate) fn measured_rows(&self) -> Option<usize> {
        (self.content.get() > 0).then(|| self.content.get())
    }

    /// Records the window height and row count one frame just painted, then
    /// re-pins the scroll inside them.
    pub(crate) fn observe(&self, window: usize, content: usize) {
        self.window.set(window);
        self.content.set(content);
        self.clamp();
    }

    /// Scrolls one row up or down.
    pub(crate) fn scroll_up(&self) {
        self.scroll.set(self.scroll.get().saturating_sub(1));
    }

    /// Scrolls one row down, stopping at the last page.
    pub(crate) fn scroll_down(&self) {
        self.scroll.set(self.scroll.get().saturating_add(1));
        self.clamp();
    }

    /// Scrolls up one observed window.
    pub(crate) fn page_up(&self) {
        let page = self.window.get().max(1);
        self.scroll.set(self.scroll.get().saturating_sub(page));
    }

    /// Scrolls down one observed window, stopping at the last page.
    pub(crate) fn page_down(&self) {
        let page = self.window.get().max(1);
        self.scroll.set(self.scroll.get().saturating_add(page));
        self.clamp();
    }

    /// Jumps to the last page of content.
    pub(crate) fn jump_latest(&self) {
        self.scroll.set(usize::MAX);
        self.clamp();
    }

    /// Returns the first visible content row, clamped to the last window.
    #[must_use]
    pub(crate) fn top(&self) -> usize {
        self.clamp();
        self.scroll.get()
    }

    fn clamp(&self) {
        let max = self.content.get().saturating_sub(self.window.get().max(1));
        self.scroll.set(self.scroll.get().min(max));
    }
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
    /// Builds an empty live region whose turn-end notices name `binary`'s
    /// product family.
    #[must_use]
    pub(crate) fn for_binary(binary: &str) -> Self {
        Self {
            product: crate::copy::identity(binary),
            ..Self::default()
        }
    }

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
                // The kind names the event, not a severity — only the kinds
                // that end in "failed" are failures ("turn.failed",
                // "compact.manual_failed"); anything else, including the
                // overloaded "compaction_ended", stays dim. Local producers
                // name a severity directly through `notice_as`.
                let kind = notice.kind.as_ref();
                let role = if kind.ends_with(".failed") || kind.ends_with("_failed") {
                    crate::theme::Role::Error
                } else {
                    crate::theme::Role::Dim
                };
                self.push_notice(role, format!("note: {}", notice.text));
                true
            }
            UpdateKind::RuleFired { rule, .. } => {
                self.push_notice(crate::theme::Role::Dim, format!("rule {rule} fired."));
                true
            }
            UpdateKind::TurnEnded { stop, .. } => {
                if *stop == Stop::Cancelled {
                    self.push_notice(
                        crate::theme::Role::Dim,
                        crate::copy::ids::TURN_CANCELLED.to_owned(),
                    );
                } else if let Some(line) = turn_end_line(*stop, self.product) {
                    self.push_notice(crate::theme::Role::Dim, line);
                }
                self.assistant_open = false;
                self.tool_cards.clear();
                self.tool_order.clear();
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
        self.stream.get_mut().reset();
        std::mem::take(&mut self.assistant_text)
    }

    /// The last `limit` rows of the assistant text as markdown, rendering only the
    /// lines completed since the previous call. `wire` offers completed
    /// routable fences to the diagram engine; `None` renders them as code.
    pub(crate) fn assistant_rows(
        &self,
        cap: usize,
        full: usize,
        mode: crate::width::WidthMode,
        limit: usize,
        wire: Option<(
            &crate::diagram::DiagramSettings,
            &crate::diagram::RenderCache,
        )>,
    ) -> Vec<crate::render::RenderRow> {
        let mut rows =
            self.stream
                .borrow_mut()
                .rows(&self.assistant_text, cap, full, mode, limit, wire);
        if self.assistant_text.ends_with('\n')
            && rows.len() > 1
            && rows.last().is_some_and(|row| row.text.is_empty())
        {
            rows.pop();
        }
        rows
    }

    /// Returns how long the tool call `call` has run: the settled duration, or
    /// the time since it started when its settle update has not arrived yet.
    /// Reading leaves the card in place; the crate's `release_tool` takes it.
    #[must_use]
    pub fn tool_elapsed(&self, call: &str) -> Option<Duration> {
        let card = self.tool_cards.get(call)?;
        Some(card.duration.unwrap_or_else(|| card.started.elapsed()))
    }

    /// Releases the card of the tool call `call` once its result entry is
    /// committed, returning how long the call ran: the settled duration, or the
    /// time since it started when its settle update has not arrived yet.
    ///
    /// The card is gone once this returns, so the caller keeps the duration in
    /// the transcript in the same step. A settle or progress update that lands
    /// after the release finds no card and changes nothing. `None` means no
    /// card was live for the call.
    #[must_use]
    pub(crate) fn release_tool(&mut self, call: &str) -> Option<Duration> {
        let card = self.tool_cards.remove(call)?;
        self.tool_order.retain(|id| id != call);
        Some(card.duration.unwrap_or_else(|| card.started.elapsed()))
    }

    /// How many tool calls still hold a live card, and how many hold a slot in
    /// the row order; the two agree whenever the live region is consistent.
    #[cfg(test)]
    pub(crate) fn tracked_tools(&self) -> (usize, usize) {
        (self.tool_cards.len(), self.tool_order.len())
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

    /// How many background jobs the host reported started and not yet settled.
    #[must_use]
    pub fn running_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// Borrows notices in arrival order for the live block.
    #[must_use]
    pub fn notices(&self) -> &[NoticeRow] {
        &self.notices
    }

    /// Adds a local command or transport notice, subject to the same three-row cap.
    pub fn notice(&mut self, message: impl Into<String>) {
        self.notice_as(crate::theme::Role::Dim, message);
    }

    /// Adds a notice in a chosen role, for producers whose outcome has a
    /// severity: errors keep the error role instead of dimming away.
    pub fn notice_as(&mut self, role: crate::theme::Role, message: impl Into<String>) {
        self.push_notice(role, message.into());
    }

    /// Pushes one notice row, keeping at most the newest three.
    fn push_notice(&mut self, role: crate::theme::Role, text: String) {
        self.notices.push(NoticeRow { text, role });
        if self.notices.len() > 3 {
            self.notices.remove(0);
        }
    }

    /// The card the expand view holds open, when one does.
    #[must_use]
    pub(crate) fn expanded(&self) -> Option<&ExpandedView> {
        self.expand.as_ref()
    }

    /// Opens or retargets the expand view; `blob_ids` names the `TextBlob`
    /// parts the card resolves through the runtime's read worker.
    pub(crate) fn open_expand(&mut self, card: ExpandedCard, blob_ids: Vec<BlobId>) {
        self.expand = Some(ExpandedView::new(card, blob_ids));
    }

    /// Closes the expand view; returns whether one was open.
    pub(crate) fn close_expand(&mut self) -> bool {
        self.expand.take().is_some()
    }

    /// Rebuilds transient state from a fresh view after replay was lost.
    pub fn reset_after_resync(&mut self) {
        self.assistant_text.clear();
        self.stream.get_mut().reset();
        self.assistant_open = false;
        self.tool_cards.clear();
        self.tool_order.clear();
        self.jobs.clear();
        // A resync swaps the branch; a card the view no longer holds cannot
        // stay expanded. Dropping the view also drops its blob map, which
        // ends the read worker the moment it finishes its current wait.
        self.expand = None;
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

fn turn_end_line(stop: Stop, product: crate::copy::Identity) -> Option<String> {
    match stop {
        Stop::EndTurn | Stop::Cancelled | Stop::MaxSteps | Stop::Failed => None,
        Stop::Length => Some(product.turn_length.to_owned()),
        Stop::Filter => Some(crate::copy::ids::TURN_FILTER.to_owned()),
    }
}

fn one_line(text: &str) -> String {
    crate::width::escape(&text.replace('\n', " "))
}

#[cfg(test)]
mod tests {
    use super::{Live, SPINNER_FRAMES};
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
        assert_eq!(live.ext_lines(3), [] as [String; 0]);
    }

    #[test]
    fn turn_end_line_maps_every_stop() {
        use dal_core::Stop;
        let dal = crate::copy::identity("dal");
        let dalgona = crate::copy::identity("dalgona");
        for stop in [Stop::EndTurn, Stop::Cancelled, Stop::MaxSteps, Stop::Failed] {
            assert_eq!(
                super::turn_end_line(stop, dal),
                None,
                "{stop:?} leaves no row"
            );
        }
        assert_eq!(
            super::turn_end_line(Stop::Length, dal),
            Some(crate::copy::ids::TURN_LENGTH.to_owned())
        );
        assert_eq!(
            super::turn_end_line(Stop::Length, dalgona),
            Some("The reply hit the length limit. Ask dalgona to continue.".to_owned())
        );
        assert_eq!(
            super::turn_end_line(Stop::Filter, dal),
            Some(crate::copy::ids::TURN_FILTER.to_owned())
        );
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
    fn reading_a_tools_elapsed_time_leaves_its_card_in_place() {
        let mut live = Live::default();
        assert_eq!(live.tool_elapsed("call-1"), None);
        assert!(live.apply_update(&tool_started(1, "call-1", "exec")));
        assert!(live.tool_elapsed("call-1").is_some());
        assert_eq!(live.tracked_tools(), (1, 1), "a read takes nothing");
        assert!(live.apply_update(&tool_settled(2, "call-1", Some(1500))));
        assert_eq!(
            live.tool_elapsed("call-1"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(live.tracked_tools(), (1, 1));
        assert_eq!(
            live.release_tool("call-1"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(live.tool_elapsed("call-1"), None, "nothing left to read");
    }

    fn tool_update(seq: u64, kind: UpdateKind) -> Update {
        Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: Seq::new(std::num::NonZeroU64::new(seq).unwrap_or(std::num::NonZeroU64::MIN)),
            kind,
        }
    }

    fn tool_started(seq: u64, call: &str, tool: &str) -> Update {
        tool_update(
            seq,
            UpdateKind::ToolStarted {
                call: dal_core::CallId::new(call),
                tool: tool.into(),
                args: dal_core::RawJson::null(),
            },
        )
    }

    fn tool_settled(seq: u64, call: &str, elapsed_ms: Option<u64>) -> Update {
        tool_update(
            seq,
            UpdateKind::ToolSettled {
                call: dal_core::CallId::new(call),
                outcome: dal_core::ToolOutcomeView {
                    is_error: false,
                    text: "done".into(),
                    images: Vec::new(),
                    elapsed_ms,
                },
            },
        )
    }

    fn turn_ended(seq: u64, stop: dal_core::Stop) -> Update {
        tool_update(
            seq,
            UpdateKind::TurnEnded {
                turn: dal_core::TurnId::new(std::num::NonZeroU64::MIN),
                stop,
            },
        )
    }

    #[test]
    fn a_released_tool_hands_back_its_settled_timing_and_no_late_update_revives_it() {
        let mut live = Live::default();
        assert!(live.apply_update(&tool_started(1, "call-1", "exec")));
        assert_eq!(live.tracked_tools(), (1, 1));
        assert_eq!(live.running_tool_rows(), ["working  exec"]);

        // Settled before its result entry lands: the card waits for the
        // result, no longer working, holding the engine's own duration.
        assert!(live.apply_update(&tool_settled(2, "call-1", Some(1500))));
        assert_eq!(live.tracked_tools(), (1, 1));
        assert_eq!(live.running_tool_rows().len(), 0);
        assert_eq!(live.activity(), None);

        assert_eq!(
            live.release_tool("call-1"),
            Some(Duration::from_millis(1500)),
            "the settled duration travels to the transcript with the release"
        );
        assert_eq!(live.tracked_tools(), (0, 0));
        assert_eq!(live.release_tool("call-1"), None, "a release happens once");

        let progress = tool_update(
            3,
            UpdateKind::ToolProgress {
                call: dal_core::CallId::new("call-1"),
                tail: "late".into(),
            },
        );
        assert!(
            !live.apply_update(&progress),
            "progress for a released card"
        );
        assert!(
            !live.apply_update(&tool_settled(4, "call-1", Some(9))),
            "a settle that trails its result finds no card"
        );
        assert_eq!(live.tracked_tools(), (0, 0));
        assert_eq!(live.running_tool_rows().len(), 0);
        assert_eq!(live.activity(), None);
    }

    #[test]
    fn a_result_before_its_settle_releases_the_card_with_the_elapsed_since_start() {
        let mut live = Live::default();
        assert!(live.apply_update(&tool_started(1, "call-1", "exec")));
        let elapsed = live
            .release_tool("call-1")
            .expect("a running card has an elapsed time");
        // The trailing settle names a far larger engine duration; it must
        // neither revive the card nor change what the release handed back.
        let engine = Duration::from_secs(3600);
        assert!(elapsed < engine);
        assert!(!live.apply_update(&tool_settled(2, "call-1", Some(3_600_000))));
        assert_eq!(live.tracked_tools(), (0, 0));
        assert_eq!(live.release_tool("call-1"), None);
    }

    #[test]
    fn releasing_one_tool_leaves_its_active_siblings_running_in_order() {
        let mut live = Live::default();
        for (seq, call, tool) in [(1, "a", "read"), (2, "b", "exec"), (3, "c", "grep")] {
            assert!(live.apply_update(&tool_started(seq, call, tool)));
        }
        assert!(live.release_tool("b").is_some());
        assert_eq!(live.tracked_tools(), (2, 2));
        assert_eq!(live.running_tool_rows(), ["working  read", "working  grep"]);
        assert_eq!(live.activity(), Some(crate::copy::ids::STATE_WORKING));

        let progress = tool_update(
            4,
            UpdateKind::ToolProgress {
                call: dal_core::CallId::new("c"),
                tail: "scanning".into(),
            },
        );
        assert!(live.apply_update(&progress));
        assert_eq!(
            live.running_tool_rows(),
            ["working  read", "working  grep · scanning"]
        );

        assert!(live.release_tool("a").is_some());
        assert_eq!(live.running_tool_rows(), ["working  grep · scanning"]);
        assert!(live.release_tool("c").is_some());
        assert_eq!(live.tracked_tools(), (0, 0));
        assert_eq!(live.activity(), None);
    }

    #[test]
    fn a_restarted_call_id_after_its_release_is_a_fresh_card() {
        let mut live = Live::default();
        assert!(live.apply_update(&tool_started(1, "call-1", "read")));
        assert!(live.release_tool("call-1").is_some());
        assert!(live.apply_update(&tool_started(2, "call-1", "exec")));
        assert_eq!(live.tracked_tools(), (1, 1));
        assert_eq!(live.running_tool_rows(), ["working  exec"]);
    }

    #[test]
    fn a_turn_end_releases_every_card_whose_result_never_came() {
        let mut live = Live::default();
        assert!(live.apply_update(&tool_started(1, "settled", "read")));
        assert!(live.apply_update(&tool_settled(2, "settled", Some(5))));
        assert!(live.apply_update(&tool_started(3, "running", "exec")));
        assert_eq!(live.tracked_tools(), (2, 2));

        assert!(live.apply_update(&turn_ended(4, dal_core::Stop::Cancelled)));
        assert_eq!(live.tracked_tools(), (0, 0));
        assert_eq!(live.running_tool_rows().len(), 0);
        assert_eq!(live.activity(), None);
        assert_eq!(live.release_tool("running"), None);
        assert!(
            live.notices()
                .iter()
                .any(|notice| notice.text == crate::copy::ids::TURN_CANCELLED),
            "releasing cards keeps the turn-end notice"
        );

        // The next turn starts from an empty region, whatever the stop was.
        assert!(live.apply_update(&tool_started(5, "next", "exec")));
        assert_eq!(live.tracked_tools(), (1, 1));
        assert!(live.apply_update(&turn_ended(6, dal_core::Stop::Failed)));
        assert_eq!(live.tracked_tools(), (0, 0));
    }

    #[test]
    fn notice_rows_keep_the_severity_their_kind_or_producer_chose() {
        use crate::theme::Role;
        use dal_core::{Notice, TurnId};

        let mut live = Live::default();
        let notice = |seq: u64, kind: &str| Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: Seq::new(std::num::NonZeroU64::new(seq).unwrap()),
            kind: UpdateKind::Notice(Notice {
                turn: Some(TurnId::new(std::num::NonZeroU64::MIN)),
                kind: kind.into(),
                text: "went wrong".into(),
            }),
        };
        assert!(live.apply_update(&notice(1, "turn.failed")));
        assert!(live.apply_update(&notice(2, "compact.manual_failed")));
        assert!(live.apply_update(&notice(3, "compaction_ended")));
        let roles: Vec<Role> = live.notices().iter().map(|row| row.role).collect();
        assert_eq!(
            roles,
            [Role::Error, Role::Error, Role::Dim],
            "failure kinds keep the error role, a plain event stays dim: {:?}",
            live.notices()
        );
        assert!(
            live.notices()
                .iter()
                .all(|row| row.text.starts_with("note: ")),
            "the upstream notice keeps its note prefix"
        );

        // A producer can name a severity directly through `notice_as`.
        live.notice_as(Role::Error, "note: write failed");
        assert_eq!(live.notices().last().map(|row| row.role), Some(Role::Error));
        live.notice("local note");
        assert_eq!(live.notices().last().map(|row| row.role), Some(Role::Dim));
    }

    #[test]
    fn live_tool_state_returns_to_empty_after_every_one_of_many_turns() {
        let mut live = Live::default();
        let mut seq = 0;
        let mut next = || {
            seq += 1;
            seq
        };
        for turn in 0..300_u64 {
            let first = format!("t{turn}-a");
            let second = format!("t{turn}-b");
            let lost = format!("t{turn}-lost");
            // Every third turn streams a call that is cancelled before any
            // result entry exists for it.
            let cancelled = turn.is_multiple_of(3);
            assert!(live.apply_update(&tool_started(next(), &first, "read")));
            assert!(live.apply_update(&tool_started(next(), &second, "exec")));
            if cancelled {
                assert!(live.apply_update(&tool_started(next(), &lost, "grep")));
            }
            // Alternate which of the two orders the engine's updates can take
            // for the same call: result then settle, or settle then result.
            if turn.is_multiple_of(2) {
                assert!(live.release_tool(&first).is_some());
                assert!(!live.apply_update(&tool_settled(next(), &first, Some(7))));
            } else {
                assert!(live.apply_update(&tool_settled(next(), &first, Some(7))));
                assert_eq!(live.release_tool(&first), Some(Duration::from_millis(7)));
            }
            // The siblings keep their cards whichever call just went.
            let siblings = if cancelled { 2 } else { 1 };
            assert_eq!(live.tracked_tools(), (siblings, siblings));
            assert_eq!(
                live.running_tool_rows().first().map(String::as_str),
                Some("working  exec")
            );
            assert!(live.release_tool(&second).is_some());

            let stop = if cancelled {
                dal_core::Stop::Cancelled
            } else {
                dal_core::Stop::EndTurn
            };
            assert!(live.apply_update(&turn_ended(next(), stop)));
            assert_eq!(
                live.tracked_tools(),
                (0, 0),
                "turn {turn} left live tool state behind"
            );
            assert_eq!(live.activity(), None);
        }
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
        assert_eq!(live.running_tool_rows().len(), 0);
        assert_eq!(live.activity(), None);
        assert_eq!(
            live.tracked_tools(),
            (0, 0),
            "the cancelled call is released"
        );
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

    #[test]
    fn streamed_rows_follow_deltas_and_start_over_after_the_text_is_taken() {
        use crate::width::WidthMode;
        let mut live = super::Live::default();
        let delta = |seq: u64, text: &str| Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: Seq::new(std::num::NonZeroU64::new(seq).unwrap_or(std::num::NonZeroU64::MIN)),
            kind: UpdateKind::Delta {
                turn: dal_core::TurnId::new(std::num::NonZeroU64::MIN),
                channel: dal_core::StreamChannel::Text,
                text: text.into(),
            },
        };
        let rows = |live: &Live| -> Vec<String> {
            live.assistant_rows(40, 40, WidthMode::Narrow, usize::MAX, None)
                .into_iter()
                .map(|row| row.text)
                .collect()
        };
        live.apply_update(&delta(1, "first li"));
        assert_eq!(rows(&live), ["first li"]);
        live.apply_update(&delta(2, "ne\nsecond"));
        assert_eq!(rows(&live), ["first line", "second"]);
        live.take_assistant_text();
        live.apply_update(&delta(3, "fresh"));
        assert_eq!(rows(&live), ["fresh"]);
    }

    #[test]
    fn an_embedded_diagram_fence_reaches_the_live_stream() {
        use crate::diagram::{DiagramSettings, RenderCache};
        use crate::width::WidthMode;
        let mut live = super::Live::default();
        let delta = |seq: u64, text: &str| Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: Seq::new(std::num::NonZeroU64::new(seq).unwrap_or(std::num::NonZeroU64::MIN)),
            kind: UpdateKind::Delta {
                turn: dal_core::TurnId::new(std::num::NonZeroU64::MIN),
                channel: dal_core::StreamChannel::Text,
                text: text.into(),
            },
        };
        live.apply_update(&delta(1, "prose\n```mermaid\ngraph TD; A-->B\n```\ntail\n"));
        let settings = DiagramSettings { enabled: true };
        let cache = RenderCache::default();
        let texts: Vec<String> = live
            .assistant_rows(
                40,
                40,
                WidthMode::Narrow,
                usize::MAX,
                Some((&settings, &cache)),
            )
            .into_iter()
            .map(|row| row.text)
            .collect();
        // The completed fence materializes its card inline: the source never
        // appears as code rows, while the surrounding prose survives in order.
        assert!(texts.first().is_some_and(|row| row.contains("prose")));
        assert!(texts.last().is_some_and(|row| row.contains("tail")));
        assert!(
            !texts
                .iter()
                .any(|row| row.contains("graph TD") || row.contains("```")),
            "{texts:?}"
        );
        assert_eq!(cache.renders(), 1);
    }

    #[test]
    fn a_resync_forgets_jobs_the_lost_replay_may_have_settled() {
        let mut live = super::Live::default();
        let started = Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq: Seq::new(std::num::NonZeroU64::MIN),
            kind: UpdateKind::JobStarted {
                job: dal_core::JobId::new_v7(),
            },
        };
        live.apply_update(&started);
        assert_eq!(live.running_jobs(), 1);
        live.reset_after_resync();
        assert_eq!(live.running_jobs(), 0);
    }
}
