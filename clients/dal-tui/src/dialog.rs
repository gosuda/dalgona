//! One-dialog-at-a-time request queue with exactly-once answers.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use dal_core::{Answer, CallGrant, Question, RawJson, Request};

use crate::diagram::{DiagramSettings, RenderCache};
use crate::render::{Prose, RenderRow, prose_rows, text_rows};
use crate::theme::Role;

/// How many locally answered titles the resync cache retains at most.
///
/// A live queue only ever has a couple of answers waiting for their
/// resolution updates; the cap only bounds a host that stops resolving
/// altogether. Eviction drops the oldest answer, whose dialog is long gone.
const ANSWERED_TITLE_CAP: usize = 32;

/// Pending requests in open order with sent-answer tracking.
#[derive(Debug, Default)]
pub struct RequestQueue {
    open: Vec<Request>,
    answered: HashSet<dal_core::RequestId>,
    keys_disabled: bool,
    /// Titles of locally answered requests, retained so a snapshot resync
    /// that drops a request before its `RequestResolved` update arrives can
    /// still announce the outcome. Entries are removed when the resolution
    /// is announced, and the cap keeps the cache from tracking the session.
    answered_titles: Vec<(dal_core::RequestId, String)>,
}

impl RequestQueue {
    /// Pushes an opened request; the head owns the dialog.
    pub fn opened(&mut self, request: Request) {
        if self.open.iter().any(|current| current.id == request.id) {
            return;
        }
        self.open.push(request);
    }

    /// Resolves a request by id, returning its title for the outcome notice.
    ///
    /// A request the user answered may already have left `open` through a
    /// snapshot resync; its retained title keeps the resolution notice honest.
    pub fn resolved(&mut self, id: &str) -> Option<String> {
        if let Some(position) = self
            .open
            .iter()
            .position(|request| request.id.to_string() == id)
        {
            let request = self.open.remove(position);
            self.sync_disabled();
            self.answered_titles
                .retain(|(answered, _)| *answered != request.id);
            return Some(dialog_title(&request.question));
        }
        let position = self
            .answered_titles
            .iter()
            .position(|(answered, _)| answered.to_string() == id)?;
        Some(self.answered_titles.remove(position).1)
    }

    /// Rebuilds the queue from a `Resync` view.
    ///
    /// A vanished id is dropped silently, except that an id this client
    /// answered keeps its retained title so its still-pending resolution
    /// update can announce the outcome here.
    pub fn resync(&mut self, open: Vec<Request>) {
        self.open = open;
        self.sync_disabled();
    }

    fn sync_disabled(&mut self) {
        self.keys_disabled = self
            .open
            .first()
            .is_some_and(|head| self.answered.contains(&head.id));
    }

    /// Returns the shown request and the queued count behind it.
    #[must_use]
    pub fn shown(&self) -> Option<(&Request, usize)> {
        self.open
            .first()
            .map(|request| (request, self.open.len().saturating_sub(1)))
    }

    /// Marks an answer sent; returns false when the id already answered.
    ///
    /// The request title is retained until its resolution is announced, so a
    /// snapshot resync cannot orphan the pending outcome notice.
    pub fn mark_answered(&mut self, id: dal_core::RequestId) -> bool {
        if !self.answered.insert(id) {
            return false;
        }
        if let Some(request) = self.open.iter().find(|request| request.id == id) {
            self.answered_titles
                .insert(0, (id, dialog_title(&request.question)));
            if self.answered_titles.len() > ANSWERED_TITLE_CAP {
                self.answered_titles.pop();
            }
        }
        self.keys_disabled = true;
        true
    }

    /// Returns whether keys stay disabled until the request leaves the queue.
    #[must_use]
    pub const fn keys_disabled(&self) -> bool {
        self.keys_disabled
    }
}

/// Maps an approval key to its answer: y approve, a session, n/Esc decline.
#[must_use]
pub fn approval_answer(choice: char) -> Answer {
    match choice {
        'y' | 'Y' => Answer::Approve,
        'a' | 'A' => Answer::ApproveForSession,
        _ => Answer::Decline,
    }
}

/// Maps a confirm key to a boolean value answer.
#[must_use]
pub fn confirm_answer(choice: char) -> Answer {
    finish_payload(if matches!(choice, 'y' | 'Y') {
        "true"
    } else {
        "false"
    })
}

/// The verb a draft-confirm dialog lands on.
///
/// `Keep` is the conservative choice — the one that loses nothing — so it
/// is where the focus starts and where Esc stays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DraftChoice {
    /// Drop the draft and leave.
    Discard,
    /// Keep the draft and stay.
    Keep,
}

/// The two-verb confirm for discarding a held draft.
///
/// Focus starts on `Keep`: an Enter typed while the dialog was still
/// forming can never throw work away. Left, Right, Tab, and Shift+Tab move
/// the focused verb in both directions through the dialog binding table;
/// the letters answer directly.
#[derive(Debug)]
pub struct DraftConfirm {
    focused: DraftChoice,
}

impl DraftConfirm {
    /// The verb Enter activates right now.
    #[must_use]
    pub fn focused(&self) -> DraftChoice {
        self.focused
    }

    /// One key on the confirm: letters answer directly, focus keys move,
    /// Enter activates the focused verb. Anything else is ignored.
    pub fn key(&mut self, key: crate::keys::Key) -> Option<DraftChoice> {
        use crate::keys::{Action, Owner, resolve_in};
        use crossterm::event::{KeyCode, KeyModifiers};
        match (key.code, key.modifiers) {
            // Plain text reaches the decoder as the character itself, an
            // uppercase letter included and with no modifier; a terminal that
            // reports Shift as well is accepted too, so `y`, `Y`, `n`, and `N`
            // answer exactly as the committed quit confirmation did.
            (KeyCode::Char('y'), KeyModifiers::NONE)
            | (KeyCode::Char('Y'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.focused = DraftChoice::Discard;
                Some(DraftChoice::Discard)
            }
            (KeyCode::Esc | KeyCode::Char('n'), KeyModifiers::NONE)
            | (KeyCode::Char('N'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.focused = DraftChoice::Keep;
                Some(DraftChoice::Keep)
            }
            (KeyCode::BackTab, _) => {
                let plain = crate::keys::Key::new(KeyCode::BackTab, KeyModifiers::NONE);
                match resolve_in(plain, false, &[Owner::Dialog]) {
                    Some(Action::DialogFocusLeft | Action::DialogFocusRight) => {
                        self.focused = match self.focused {
                            DraftChoice::Discard => DraftChoice::Keep,
                            DraftChoice::Keep => DraftChoice::Discard,
                        };
                        None
                    }
                    _ => None,
                }
            }
            _ => match resolve_in(key, false, &[Owner::Dialog]) {
                Some(Action::DialogFocusLeft | Action::DialogFocusRight) => {
                    self.focused = match self.focused {
                        DraftChoice::Discard => DraftChoice::Keep,
                        DraftChoice::Keep => DraftChoice::Discard,
                    };
                    None
                }
                Some(Action::DialogActivate | Action::AskSubmit) => Some(self.focused),
                _ => None,
            },
        }
    }
}

impl Default for DraftConfirm {
    fn default() -> Self {
        Self {
            focused: DraftChoice::Keep,
        }
    }
}

/// Builds a single-select value answer from the focused option label.
#[must_use]
pub fn select_single(label: &str) -> Answer {
    object_value("selected", &[label])
}

/// Builds a multi-select value answer in option order.
///
/// An empty selection is an empty submission: it dismisses.
#[must_use]
pub fn select_multi(labels: &[&str]) -> Answer {
    if labels.is_empty() {
        return Answer::Cancel;
    }
    object_value("selected", labels)
}

/// Builds a free-text value answer; outer whitespace is trimmed here and a
/// blank submission is an empty submission: it dismisses.
#[must_use]
pub fn text_answer(text: &str) -> Answer {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Answer::Cancel;
    }
    string_value("text", trimmed)
}

/// Builds the Other-escape answer for a select question: the typed text as
/// one off-list `selected` label; a blank submission travels as a dismiss.
#[must_use]
pub fn other_answer(text: &str) -> Answer {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Answer::Cancel;
    }
    select_single(trimmed)
}

/// One step of the shared free-text row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FreeText {
    /// Plain Enter: the caller submits the buffer.
    Submitted,
    /// Esc: the caller dismisses the shown question.
    Dismissed,
    /// The key edited the buffer.
    Editing,
    /// The key is not a free-text key.
    Ignored,
}

/// Builds the one-member object payload `{ "<key>": [<values>, ...] }`.
fn object_value(key: &str, values: &[&str]) -> Answer {
    let mut body = String::from("{\"");
    body.push_str(key);
    body.push_str("\":[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        append_json_string(&mut body, value);
    }
    body.push_str("]}");
    finish_payload(&body)
}

/// Builds the one-member object payload `{ "<key>": "<value>" }`.
fn string_value(key: &str, value: &str) -> Answer {
    let mut body = String::from("{\"");
    body.push_str(key);
    body.push_str("\":");
    append_json_string(&mut body, value);
    body.push('}');
    finish_payload(&body)
}

/// Parses one payload into its value answer; unparseable input dismisses.
fn finish_payload(body: &str) -> Answer {
    RawJson::parse(body).map_or(Answer::Cancel, Answer::Value)
}

/// Renders the call-scoped grant clause above approval actions.
#[must_use]
pub fn grant_clause(grant: &CallGrant) -> String {
    let roots = grant
        .roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "also allows {} in {roots} until the job ends",
        grant.argv_prefix
    )
}

/// Returns the dialog title for a question.
#[must_use]
pub fn dialog_title(question: &Question) -> String {
    match question {
        Question::Approval { tool, preview, .. } => match tool.as_ref() {
            "exec" | "run" => crate::copy::ids::APPROVAL_TITLE_COMMAND.to_owned(),
            "patch" => crate::copy::render(
                crate::copy::ids::APPROVAL_TITLE_PATCH,
                &[("path", &preview.title)],
                1,
            ),
            "eval" => crate::copy::render(
                crate::copy::ids::APPROVAL_TITLE_EVAL,
                &[("services", &preview.title)],
                1,
            ),
            _ => crate::copy::render(crate::copy::ids::APPROVAL_TITLE_TOOL, &[("tool", tool)], 1),
        },
        Question::Grant {
            ext, capabilities, ..
        } => crate::copy::render(
            crate::copy::ids::GRANT_TITLE,
            &[
                ("ext", ext),
                (
                    "services",
                    &capabilities
                        .iter()
                        .map(AsRef::as_ref)
                        .collect::<Vec<&str>>()
                        .join(", "),
                ),
            ],
            1,
        ),
        Question::Select { prompt, .. } | Question::Text { prompt, .. } => prompt.to_string(),
        Question::Confirm { text } => text.to_string(),
        _ => "This question type is not supported here. Answer it from another client.".to_owned(),
    }
}

fn append_json_string(output: &mut String, text: &str) {
    use std::fmt::Write as _;
    output.push('"');
    for ch in text.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{2028}' | '\u{2029}' => {
                let _ = write!(output, "\\u{:04x}", u32::from(ch));
            }
            ch if ch.is_control() => {
                let _ = write!(output, "\\u{:04x}", u32::from(ch));
            }
            ch => output.push(ch),
        }
    }
    output.push('"');
}

/// How long a freshly shown dialog ignores every key but Esc.
///
/// Browsers hold the same line for dialogs that grant something: Firefox delays
/// its install and permission buttons by 1000 ms (`security.dialog_enable_delay`).
/// Keys already on their way when a dialog opens arrive within the gap between
/// two strokes of a typist, under 200 ms at 60 words a minute, so 500 ms swallows
/// that type-ahead and still passes a user who reads the question and answers.
pub const ARM_DELAY: Duration = Duration::from_millis(500);

/// Whether a dialog accepts answer keys yet.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Arming {
    /// No frame has shown the current dialog.
    #[default]
    Fresh,
    /// The dialog has been shown since this instant and still ignores answer keys.
    Shown(Instant),
    /// The hint row lists the answer keys and they act.
    Armed,
}

/// Keyboard and rendering state for the request currently at the queue head.
#[derive(Debug, Default)]
pub struct DialogUi {
    /// Open requests, in arrival order.
    pub queue: RequestQueue,
    focused: usize,
    checked: std::collections::BTreeSet<usize>,
    input: String,
    /// Whether the shown select question has its Other text row open.
    text_mode: bool,
    expanded: bool,
    scroll: usize,
    /// Focused answer verb on a button dialog (approval, grant, confirm).
    /// `None` until the kind is known so the conservative verb takes focus:
    /// deny for approvals and grants, no for confirmations.
    action_focus: Option<usize>,
    active_id: Option<String>,
    arming: Arming,
    /// When the shown request arrived; the countdown starts here.
    opened_at: Option<Instant>,
    /// The most recent frame clock reading, from [`DialogUi::tick`].
    now: Option<Instant>,
}

impl DialogUi {
    /// Opens one request and presents it when no earlier request is visible.
    pub fn opened(&mut self, request: Request) {
        self.queue.opened(request);
        self.reset_on_change();
    }

    /// Removes a resolved request and advances to the next open request.
    pub fn resolved(&mut self, id: &str) -> Option<String> {
        let title = self.queue.resolved(id);
        self.reset_on_change();
        title
    }

    /// Rebuilds the queue after a replay gap without treating vanished requests as new.
    pub fn resync(&mut self, open: Vec<Request>) {
        self.queue.resync(open);
        self.reset_on_change();
    }

    /// Returns whether a dialog currently owns keyboard input.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.queue.shown().is_some()
    }

    /// True while an answered request waits for the host to resolve it.
    #[must_use]
    pub const fn awaiting_resolution(&self) -> bool {
        self.queue.keys_disabled
    }

    /// Advances the arming clock; call once per frame just before painting,
    /// and only when the previous frame landed with this dialog on screen.
    ///
    /// The first call after a dialog appears starts the delay, and the call that
    /// finds [`ARM_DELAY`] elapsed arms it, so the frame painted right after
    /// shows the answer keys. A key counts only when the frame that lists the
    /// keys has already been painted.
    pub fn tick(&mut self, now: Instant) {
        self.now = Some(now);
        match self.arming {
            Arming::Fresh if self.is_open() => self.arming = Arming::Shown(now),
            Arming::Shown(since) if now.saturating_duration_since(since) >= ARM_DELAY => {
                self.arming = Arming::Armed;
            }
            _ => {}
        }
    }

    /// Advances the arming clock after a frame that did not land with the
    /// dialog on screen: one held while the caret measure went unanswered,
    /// discarded on a resize, or below the usable floor. The question is not
    /// on screen, so arming resets and answer keys stay closed no matter how
    /// long the request has been open.
    pub fn tick_without_dialog(&mut self, now: Instant) {
        self.arming = Arming::Fresh;
        self.now = Some(now);
    }

    /// Whole minutes left on the shown request, from its raise instant.
    ///
    /// The dialog never dismisses itself; the count only feeds the title
    /// countdown slot at the 1 s cadence the frame loop already drives.
    fn minutes_left(&self, request: &Request) -> Option<u64> {
        let seen = self.opened_at?;
        let now = self.now.unwrap_or(Instant::now());
        let elapsed = now.saturating_duration_since(seen);
        let left = request.timeout.saturating_sub(elapsed);
        Some(left.as_secs() / 60)
    }

    /// True once the shown dialog accepts answer keys.
    #[must_use]
    pub fn armed(&self) -> bool {
        self.arming == Arming::Armed
    }

    /// Inserts pasted content only into a live free-text row.
    pub fn paste(&mut self, bytes: &[u8]) {
        let Some((request, _)) = self.queue.shown() else {
            return;
        };
        let accepts_text = matches!(request.question, Question::Text { .. }) || self.text_mode;
        let live = self.armed() && !self.queue.keys_disabled();
        if live && accepts_text {
            self.input.push_str(&String::from_utf8_lossy(bytes));
        }
    }

    /// Maps a key into an answer; an answered request disables its keys until resolution.
    ///
    /// Until the dialog is armed only an unmodified Esc acts: it is the one answer
    /// that grants nothing, so a key typed before the dialog was visible can never
    /// consent. Every other key is dropped, not queued.
    pub fn key(&mut self, key: crate::keys::Key) -> Option<(dal_core::RequestId, Answer)> {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (request, _) = self.queue.shown()?;
        if self.queue.keys_disabled() {
            return None;
        }
        let escape = key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE;
        if !self.armed() && !escape {
            return None;
        }
        let id = request.id;
        let control_shortcut = key.modifiers == KeyModifiers::CONTROL
            && match (&request.question, key.code) {
                (Question::Select { .. }, KeyCode::Char('j')) => self.text_mode,
                (Question::Select { .. }, KeyCode::Char('o'))
                | (Question::Text { .. }, KeyCode::Char('j')) => true,
                _ => false,
            };
        if key.modifiers != KeyModifiers::NONE
            && key.modifiers != KeyModifiers::SHIFT
            && !control_shortcut
        {
            return None;
        }
        let question = request.question.clone();
        let answer = self.question_answer(&question, key);
        let answer = answer?;
        self.queue.mark_answered(id).then_some((id, answer))
    }

    /// Normalizes a key for the binding table: an uppercase ASCII letter
    /// resolves as the unmodified letter so `Y` answers like `y`, whether the
    /// terminal reports Shift (kitty) or only the uppercase character (the
    /// legacy decoder, or Caps Lock); Shift+Tab arriving as kitty `BackTab`
    /// resolves like the legacy unmodified `CSI Z`, and the dialog owner
    /// alone sees the key.
    fn resolve(key: crate::keys::Key) -> Option<crate::keys::Action> {
        use crossterm::event::{KeyCode, KeyModifiers};
        if let KeyCode::Char(ch) = key.code
            && ch.is_ascii_uppercase()
            && (key.modifiers == KeyModifiers::SHIFT || key.modifiers == KeyModifiers::NONE)
        {
            let plain =
                crate::keys::Key::new(KeyCode::Char(ch.to_ascii_lowercase()), KeyModifiers::NONE);
            return crate::keys::resolve_in(plain, false, &[crate::keys::Owner::Dialog]);
        }
        if key.code == KeyCode::BackTab {
            let plain = crate::keys::Key::new(KeyCode::BackTab, KeyModifiers::NONE);
            return crate::keys::resolve_in(plain, false, &[crate::keys::Owner::Dialog]);
        }
        crate::keys::resolve_in(key, false, &[crate::keys::Owner::Dialog])
    }

    /// Focus moves one step toward the first verb on a button row, wrapping.
    fn move_focus_prev(&mut self, count: usize) {
        self.step_focus(count, false);
    }

    /// Focus moves one step toward the last verb on a button row, wrapping.
    fn move_focus_next(&mut self, count: usize) {
        self.step_focus(count, true);
    }

    /// Focus moves one step on a button row, wrapping in both directions.
    fn step_focus(&mut self, count: usize, forward: bool) {
        let focused = self.action_focus.unwrap_or(0);
        self.action_focus = Some(if forward {
            (focused + 1) % count
        } else {
            (focused + count - 1) % count
        });
    }

    /// One key on the free-text row of a `text` question. Enter submits; a
    /// blank submission is an empty submission and travels as a dismiss.
    fn text_row_key(&mut self, key: crate::keys::Key) -> Option<Answer> {
        match self.step_free_text(key) {
            FreeText::Submitted => {
                self.text_mode = false;
                Some(text_answer(&self.input))
            }
            FreeText::Dismissed => Some(Answer::Cancel),
            FreeText::Editing | FreeText::Ignored => None,
        }
    }

    /// One key on the open Other escape of a select question. Enter submits
    /// the typed text as one off-list `selected` label, the implicit
    /// free-text escape; a blank submission travels as a dismiss.
    fn other_row_key(&mut self, key: crate::keys::Key) -> Option<Answer> {
        match self.step_free_text(key) {
            FreeText::Submitted => {
                self.text_mode = false;
                Some(other_answer(&self.input))
            }
            FreeText::Dismissed => Some(Answer::Cancel),
            FreeText::Editing | FreeText::Ignored => None,
        }
    }

    /// Advances the shared free-text buffer without submitting.
    fn step_free_text(&mut self, key: crate::keys::Key) -> FreeText {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Esc => FreeText::Dismissed,
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.push('\n');
                FreeText::Editing
            }
            KeyCode::Enter => FreeText::Submitted,
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push('\n');
                FreeText::Editing
            }
            KeyCode::Backspace => {
                crate::composer::pop_grapheme(&mut self.input);
                FreeText::Editing
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !ch.is_control() =>
            {
                self.input.push(ch);
                FreeText::Editing
            }
            _ => FreeText::Ignored,
        }
    }

    /// Maps one key to an answer for the shown question kind, mutating only
    /// dialog-local focus, checked set, scroll, and input buffer.
    ///
    /// Esc is matched by key code first because the table's Esc row is the
    /// approval `n deny` alias: the same physical key declines an approval
    /// but dismisses an ask. Every other bound key resolves through the
    /// dialog owner of the binding table, so the documented Left, Right,
    /// Tab, Shift+Tab, and Enter-activate paths reach every kind with a
    /// focusable row.
    fn question_answer(&mut self, question: &Question, key: crate::keys::Key) -> Option<Answer> {
        use crossterm::event::KeyCode;
        match question {
            Question::Approval { .. } | Question::Grant { .. } => {
                if key.code == KeyCode::Esc {
                    return Some(Answer::Decline);
                }
                self.approval_answer(key)
            }
            Question::Select { options, multi, .. } => self.select_answer(options, *multi, key),
            Question::Text { .. } => self.text_row_key(key),
            Question::Confirm { .. } => self.confirm_answer_row(key),
            _ => (key.code == KeyCode::Esc).then_some(Answer::Cancel),
        }
    }

    /// One key on an approval or grant button row. Esc already declined
    /// above; every other bound key resolves through the dialog owner.
    fn approval_answer(&mut self, key: crate::keys::Key) -> Option<Answer> {
        use crate::keys::Action;
        const VERBS: usize = 3; // allow once, allow session, deny
        self.action_focus.get_or_insert(VERBS - 1);
        match Self::resolve(key) {
            Some(Action::ApprovalAllowOnce) => Some(Answer::Approve),
            Some(Action::ApprovalAllowSession) => Some(Answer::ApproveForSession),
            Some(Action::ApprovalDeny) => Some(Answer::Decline),
            Some(Action::ApprovalView) => {
                self.expanded = !self.expanded;
                None
            }
            Some(Action::DialogFocusLeft | Action::AskMoveUp) => {
                self.move_focus_prev(VERBS);
                None
            }
            Some(Action::DialogFocusRight | Action::AskMoveDown) => {
                self.move_focus_next(VERBS);
                None
            }
            Some(Action::AskSubmit | Action::DialogActivate) => {
                Some(match self.action_focus.unwrap_or(VERBS - 1) {
                    0 => Answer::Approve,
                    1 => Answer::ApproveForSession,
                    _ => Answer::Decline,
                })
            }
            Some(Action::DialogScrollDown) => {
                self.scroll = self.scroll.saturating_add(1);
                None
            }
            Some(Action::DialogScrollUp) => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
            _ => None,
        }
    }

    /// One key on a select-question option row. The Other escape opens the
    /// in-dialog text row; focus reaches every offered option plus that row.
    fn select_answer(
        &mut self,
        options: &[dal_core::Choice],
        multi: bool,
        key: crate::keys::Key,
    ) -> Option<Answer> {
        use crate::keys::Action;
        use crossterm::event::{KeyCode, KeyModifiers};
        if self.text_mode {
            return self.other_row_key(key);
        }
        // Focus reaches every offered option plus the always-present
        // Other row; arrows and tab move with wrap in both
        // directions through the binding table.
        let rows = options.len() + 1;
        if key.code == KeyCode::Esc {
            return Some(Answer::Cancel);
        }
        match Self::resolve(key) {
            Some(Action::AskMoveUp | Action::DialogFocusLeft) => {
                self.focused = (self.focused + rows - 1) % rows;
                None
            }
            Some(Action::AskMoveDown | Action::DialogFocusRight) => {
                self.focused = (self.focused + 1) % rows;
                None
            }
            Some(Action::AskToggle) if self.focused < options.len() => {
                if multi {
                    if !self.checked.insert(self.focused) {
                        self.checked.remove(&self.focused);
                    }
                    None
                } else {
                    options
                        .get(self.focused)
                        .map(|option| select_single(&option.label))
                }
            }
            Some(Action::AskSubmit | Action::DialogActivate) if self.focused == options.len() => {
                // The Other escape opens the in-dialog text row.
                self.text_mode = true;
                None
            }
            Some(Action::AskSubmit | Action::DialogActivate) => {
                if multi {
                    let labels: Vec<&str> = self
                        .checked
                        .iter()
                        .filter_map(|index| options.get(*index))
                        .map(|option| option.label.as_ref())
                        .collect();
                    Some(select_multi(&labels))
                } else {
                    options
                        .get(self.focused)
                        .map(|option| select_single(&option.label))
                }
            }
            Some(Action::DialogScrollDown) => {
                self.expanded = true;
                self.scroll = self.scroll.saturating_add(1);
                None
            }
            Some(Action::DialogScrollUp) => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
            _ => {
                // Ctrl+O toggles the preview; it is a CONTROL
                // shortcut outside the plain-key table.
                if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.expanded = !self.expanded;
                }
                None
            }
        }
    }

    /// One key on a yes-or-no confirmation row. Esc already canceled above;
    /// the `const` sits before the statements it scopes over.
    fn confirm_answer_row(&mut self, key: crate::keys::Key) -> Option<Answer> {
        use crate::keys::Action;
        use crossterm::event::KeyCode;
        const VERBS: usize = 2; // yes, no
        if key.code == KeyCode::Esc {
            return Some(Answer::Cancel);
        }
        self.action_focus.get_or_insert(VERBS - 1);
        match Self::resolve(key) {
            Some(Action::ApprovalAllowOnce) => Some(confirm_answer('y')),
            Some(Action::ApprovalDeny) => Some(confirm_answer('n')),
            Some(Action::DialogFocusLeft | Action::AskMoveUp) => {
                self.move_focus_prev(VERBS);
                None
            }
            Some(Action::DialogFocusRight | Action::AskMoveDown) => {
                self.move_focus_next(VERBS);
                None
            }
            Some(Action::AskSubmit | Action::DialogActivate) => Some(confirm_answer(
                if self.action_focus.unwrap_or(VERBS - 1) == 0 {
                    'y'
                } else {
                    'n'
                },
            )),
            _ => None,
        }
    }

    /// Renders the active dialog with a title, scrollable body, and fail-closed actions.
    #[must_use]
    pub fn rows(&self, width: usize, height: usize, mode: crate::WidthMode) -> Vec<String> {
        self.rendered_rows(
            width,
            height,
            mode,
            DiagramSettings::default(),
            &RenderCache::default(),
            true,
        )
        .into_iter()
        .map(|row| row.text)
        .collect()
    }

    /// `kitty` is the terminal's keyboard encoding: hints name shift+enter
    /// when it is active and ctrl+j otherwise, matching what each terminal
    /// actually delivers.
    pub(crate) fn rendered_rows(
        &self,
        width: usize,
        height: usize,
        mode: crate::WidthMode,
        settings: DiagramSettings,
        cache: &RenderCache,
        kitty: bool,
    ) -> Vec<RenderRow> {
        let Some((request, waiting)) = self.queue.shown() else {
            return Vec::new();
        };
        let mut title = dialog_title(&request.question);
        if waiting > 0 {
            use std::fmt::Write as _;
            let count = u64::try_from(waiting).unwrap_or(u64::MAX);
            let _ = write!(
                title,
                " · {}",
                crate::copy::render(
                    crate::copy::ids::REQUEST_MORE,
                    &[("n", &count.to_string())],
                    count,
                )
            );
        }
        // The countdown slot at the 1 s cadence the frame loop drives; the
        // dialog never dismisses itself, resolution closes it. A question
        // under a minute from its deadline shows no slot.
        if let Some(minutes) = self.minutes_left(request)
            && minutes >= 1
        {
            use std::fmt::Write as _;
            let _ = write!(title, " · {minutes}m left");
        }
        let mut rows = prose_rows(&title, width, mode)
            .into_iter()
            .map(|mut row| {
                row.role = Role::Accent;
                row
            })
            .collect::<Vec<_>>();
        rows.truncate(height.saturating_sub(2).max(1));
        let (mut body, mut pinned) =
            self.body_parts(&request.question, width, mode, settings, cache);
        let actions_row = self.actions_row(&request.question, kitty, width, mode);
        let visible = height.saturating_sub(rows.len() + 1).max(1);
        // Pinned rows (the choices of a selection) stay on screen; only the
        // rows above them scroll. A select with more choices than rows
        // keeps a window centered on the focused choice; the last pinned
        // row is the always-present Other action, so it is popped before
        // the window and restored after, never counted off-screen.
        let mut other = pinned.pop();
        // One slot for the Other row, and one for the scrollable body when
        // it has rows (a preview or the open Other text row), come off the
        // option window's budget. A select with neither has no body to
        // reserve for, so its choices take every row left.
        let body_slot = usize::from(!body.is_empty());
        if self.text_mode && visible == 1 {
            // The typed text is what the user acts on: with one row left it
            // keeps that row, not the Other label.
            other = None;
        }
        let mut option_cap = visible.saturating_sub(usize::from(other.is_some()) + body_slot);
        if option_cap == 0 && self.focused < pinned.len() {
            // No row is left for a choice. Enter answers the focused one, so
            // it takes Other's row rather than painting nowhere; Other
            // paints again as soon as focus reaches it.
            other = None;
            option_cap = 1;
        }
        let mut pinned = window_pinned(pinned, self.focused, option_cap);
        if let Some(other) = other {
            pinned.push(other);
        }
        // Pinned rows outrank the body: with no row left over, the preview
        // paints none rather than pushing the block past its height.
        let room = visible.saturating_sub(pinned.len());
        let hidden = body.len().saturating_sub(room);
        let scroll = if self.expanded {
            self.scroll.min(hidden)
        } else {
            self.scroll
        };
        if hidden > 0 && !self.expanded {
            let shown = room.saturating_sub(1);
            body.truncate(shown);
            let count = u64::try_from(hidden + 1).unwrap_or(u64::MAX);
            body.push(RenderRow::new(
                crate::copy::render(
                    crate::copy::ids::DIALOG_BODY_MORE,
                    &[("n", &count.to_string())],
                    count,
                ),
                Role::Dim,
            ));
        }
        rows.extend(body.into_iter().skip(scroll).take(room));
        rows.extend(pinned);
        rows.push(actions_row);
        rows.into_iter()
            .map(|row| row.clipped(width, mode))
            .collect()
    }

    /// The scrollable rows of a select question: the preview, or the
    /// in-dialog text row while the Other escape is open.
    fn select_body(
        &self,
        preview: Option<&dal_core::Preview>,
        width: usize,
        mode: crate::WidthMode,
        settings: DiagramSettings,
        cache: &RenderCache,
    ) -> Vec<RenderRow> {
        if self.text_mode {
            return input_rows(&self.input, None);
        }
        preview
            .iter()
            .flat_map(|preview| {
                text_rows(
                    &preview.body,
                    width,
                    mode,
                    Prose::Markdown { full: width },
                    settings,
                    cache,
                )
            })
            .collect()
    }

    /// The pinned rows of a select question: one per option plus the
    /// always-present Other action. The focused row paints in the theme's
    /// selection reverse video on top of its `>` marker.
    fn select_pinned(&self, options: &[dal_core::Choice], multi: bool) -> Vec<RenderRow> {
        let mut pinned: Vec<RenderRow> = options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let checked = self.checked.contains(&index) || (!multi && index == self.focused);
                let marker =
                    ["( )", "(*)", "[ ]", "[x]"][usize::from(multi) * 2 + usize::from(checked)];
                let description = option
                    .description
                    .as_deref()
                    .map(|text| format!(" - {}", crate::width::escape(text)))
                    .unwrap_or_default();
                let mut row = RenderRow::new(
                    format!(
                        "{} {marker} {}{description}",
                        if index == self.focused { ">" } else { " " },
                        crate::width::escape(&option.label)
                    ),
                    Role::Text,
                );
                if index == self.focused && !self.text_mode {
                    row.selected.push(0..row.text.len());
                }
                row
            })
            .collect();
        let marker = if multi { "[ ]" } else { "( )" };
        let mut other = RenderRow::new(
            format!(
                "{} {marker} {}",
                if self.focused == options.len() && !self.text_mode {
                    ">"
                } else {
                    " "
                },
                crate::copy::ids::ASK_OTHER_ACTION
            ),
            Role::Text,
        );
        if self.focused == options.len() && !self.text_mode {
            other.selected.push(0..other.text.len());
        }
        pinned.push(other);
        pinned
    }

    /// Splits the dialog body into scrollable rows and rows pinned below them.
    fn body_parts(
        &self,
        question: &Question,
        width: usize,
        mode: crate::WidthMode,
        settings: DiagramSettings,
        cache: &RenderCache,
    ) -> (Vec<RenderRow>, Vec<RenderRow>) {
        let mut pinned = Vec::new();
        let rows_of = |body: &str, prose| text_rows(body, width, mode, prose, settings, cache);
        let mut rows = match question {
            Question::Approval { preview, grant, .. } => {
                let mut rows = Vec::new();
                if let Some(grant) = grant {
                    rows.push(RenderRow::new(grant_clause(grant), Role::Text));
                }
                rows.extend(rows_of(&preview.body, Prose::Verbatim));
                rows
            }
            Question::Grant {
                origin,
                capabilities,
                detail,
                ..
            } => {
                let mut rows = vec![RenderRow::new(
                    format!(
                        "{origin} requests: {}",
                        capabilities
                            .iter()
                            .map(AsRef::as_ref)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Role::Text,
                )];
                if let Some(detail) = detail {
                    rows.extend(
                        detail
                            .lines()
                            .map(|line| RenderRow::new(crate::width::escape(line), Role::Text)),
                    );
                }
                rows
            }
            Question::Select {
                options,
                multi,
                preview,
                ..
            } => {
                pinned.extend(self.select_pinned(options, *multi));
                self.select_body(preview.as_ref(), width, mode, settings, cache)
            }
            Question::Text { placeholder, .. } => input_rows(&self.input, placeholder.as_deref()),
            Question::Confirm { .. } => vec![RenderRow::new("[y] Yes   [n] No", Role::Text)],
            _ => vec![RenderRow::new(
                "This question type is not supported here. Answer it from another client.",
                Role::Text,
            )],
        };
        if rows.is_empty() && pinned.is_empty() {
            rows.push(RenderRow::new(String::new(), Role::Text));
        }
        (rows, pinned)
    }

    /// The fail-closed actions row, folded to the columns it gets.
    ///
    /// Segments drop in priority order — preview and courtesy affordances
    /// first — and the deny or dismiss cue carries the lowest number, so a
    /// narrow approval still shows how to say no. On a dialog with answer
    /// verbs the focused verb paints the selection range; the focused verb
    /// defaults to the conservative answer (deny, no).
    fn actions_row(
        &self,
        question: &Question,
        kitty: bool,
        width: usize,
        mode: crate::WidthMode,
    ) -> RenderRow {
        use crate::copy::ids;
        let approving = matches!(question, Question::Approval { .. } | Question::Grant { .. });
        if self.queue.keys_disabled() {
            return RenderRow::new(
                "Waiting for the request to settle...".to_owned(),
                Role::Text,
            );
        }
        if !self.armed() {
            let esc = if approving {
                ids::APPROVAL_ESC_DENIES
            } else {
                "esc dismiss"
            };
            let (text, _) = fold_hint_ranges(
                [esc, ids::DIALOG_ARMING]
                    .into_iter()
                    .zip([0_i16, 1])
                    .collect(),
                width,
                mode,
            );
            return RenderRow::new(text, Role::Text);
        }
        // `focus` counts only focusable verbs: the leading segments of a
        // button row. The focused verb folds last — below even the esc
        // cue — so Enter never activates a verb the row does not show.
        let (segments, focusable, focus): (Vec<(&str, i16)>, usize, usize) = match question {
            Question::Approval { .. } | Question::Grant { .. } => (
                ids::DIALOG_ACTIONS_SHORT
                    .split(" · ")
                    .chain(["v view", ids::APPROVAL_ESC_DENIES])
                    .zip([3_i16, 2, 1, 4, 0])
                    .collect(),
                3_usize,
                self.action_focus.unwrap_or(2),
            ),
            Question::Select { .. } if self.text_mode => {
                return RenderRow::new(Self::text_hint(kitty, width, mode), Role::Text);
            }
            Question::Select { multi: true, .. } => (
                ids::ASK_HINT_MULTI
                    .split(" · ")
                    .zip([3_i16, 2, 1, 0])
                    .collect(),
                0,
                0,
            ),
            Question::Select { .. } => (
                ids::ASK_HINT_SINGLE
                    .split(" · ")
                    .zip([2_i16, 1, 0])
                    .collect(),
                0,
                0,
            ),
            Question::Text { .. } => {
                return RenderRow::new(Self::text_hint(kitty, width, mode), Role::Text);
            }
            Question::Confirm { .. } => (
                ids::ASK_HINT_CONFIRM
                    .split(" · ")
                    .zip([2_i16, 1, 0])
                    .collect(),
                2_usize,
                self.action_focus.unwrap_or(1),
            ),
            _ => return RenderRow::new("esc dismiss".to_owned(), Role::Text),
        };
        // Demote only a real focused verb: a row without focusable verbs
        // keeps every segment's listed priority, so esc dismiss still
        // outlives the move cues.
        let focus = focus.min(focusable.saturating_sub(1));
        let segments: Vec<(&str, i16)> = segments
            .into_iter()
            .enumerate()
            .map(|(index, (segment, priority))| {
                (
                    segment,
                    if focusable > 0 && index == focus {
                        -1
                    } else {
                        priority
                    },
                )
            })
            .collect();
        let (text, ranges) = fold_hint_ranges(segments, width, mode);
        let mut row = RenderRow::new(text, Role::Text);
        if focusable > 0
            && let Some((_, range)) = ranges.iter().find(|(index, _)| *index == focus)
        {
            row.selected.push(range.clone());
        }
        row
    }

    /// The free-text actions row. The newline segment names the chord the
    /// terminal's keyboard encoding actually delivers: shift+enter under
    /// kitty keys, ctrl+j elsewhere.
    fn text_hint(kitty: bool, width: usize, mode: crate::WidthMode) -> String {
        use crate::copy::ids;
        let newline = if kitty {
            ids::HINT_NEWLINE
        } else {
            ids::HINT_NEWLINE_LEGACY
        };
        fold_hint_ranges(
            ["enter submit", newline, "esc dismiss"]
                .into_iter()
                .zip([1_i16, 2, 0])
                .collect(),
            width,
            mode,
        )
        .0
    }

    fn reset_on_change(&mut self) {
        let current = self
            .queue
            .shown()
            .map(|(request, _)| request.id.to_string());
        if self.active_id == current {
            return;
        }
        self.active_id = current;
        self.focused = 0;
        self.checked.clear();
        self.input.clear();
        self.text_mode = false;
        self.expanded = false;
        self.scroll = 0;
        self.action_focus = None;
        self.arming = Arming::Fresh;
        self.opened_at = self.now.or_else(|| Some(Instant::now()));
    }
}

/// Joins `segments` with the ` · ` separator, dropping the
/// highest-priority-numbered entries first until the row fits `width`
/// cells, and reports each surviving segment's byte range keyed by its
/// original position so a focused verb can paint its selection. The lowest
/// number folds last; the deny or dismiss cue (and the focused verb, which
/// the caller demotes below it) survives to the end, and the 12-cell
/// minimum floor always leaves room for it.
fn fold_hint_ranges(
    segments: Vec<(&str, i16)>,
    width: usize,
    mode: crate::WidthMode,
) -> (String, Vec<(usize, std::ops::Range<usize>)>) {
    let mut kept: Vec<(usize, &str, i16)> = segments
        .into_iter()
        .enumerate()
        .map(|(index, (segment, priority))| (index, segment, priority))
        .collect();
    let fits = |kept: &[(usize, &str, i16)]| {
        crate::width::width(
            &kept
                .iter()
                .map(|(_, segment, _)| *segment)
                .collect::<Vec<_>>()
                .join(" · "),
            mode,
        ) <= width
    };
    while kept.len() > 1 && !fits(&kept) {
        let index = kept
            .iter()
            .enumerate()
            .rev()
            .max_by_key(|(_, (_, _, priority))| *priority)
            .map_or(0, |(index, _)| index);
        kept.remove(index);
    }
    let mut text = String::new();
    let mut ranges = Vec::with_capacity(kept.len());
    for (index, segment, _) in &kept {
        if !text.is_empty() {
            text.push_str(" · ");
        }
        ranges.push((*index, text.len()..text.len() + segment.len()));
        text.push_str(segment);
    }
    (text, ranges)
}

/// Keeps at most `cap` of the pinned choice rows, centered on the
/// `focused` row, which is never scrolled out. Hidden options keep their
/// keys — arrows still reach them — and one `... n more` slot at either
/// end counts what the window hides.
fn window_pinned(mut pinned: Vec<RenderRow>, focused: usize, cap: usize) -> Vec<RenderRow> {
    if pinned.len() <= cap {
        return pinned;
    }
    let focused = focused.min(pinned.len() - 1);
    if cap < 3 {
        // No room for count rows: a tight window on the focused row.
        let start = focused.saturating_sub(cap / 2).min(pinned.len() - cap);
        return pinned.drain(start..start + cap).collect();
    }
    // Center the focus, then trade one slot per hidden side for the row
    // that counts it.
    let mut start = focused.saturating_sub(cap / 2);
    let mut end = (start + cap).min(pinned.len());
    start = end.saturating_sub(cap);
    if start > 0 {
        start += 1;
    }
    if end < pinned.len() {
        end -= 1;
    }
    end = end.max(focused + 1);
    start = start.min(focused);
    let above = start;
    let below = pinned.len() - end;
    let mut out: Vec<RenderRow> = Vec::with_capacity(cap);
    if above > 0 {
        out.push(RenderRow::new(format!("... {above} more"), Role::Dim));
    }
    out.extend(pinned.drain(start..end));
    if below > 0 {
        out.push(RenderRow::new(format!("... {below} more"), Role::Dim));
    }
    out
}

/// The rows of the shared free-text buffer: one `> ` input row per line,
/// continuation lines indented under it. Control characters are escaped so
/// pasted bytes can never write terminal commands into the live block; the
/// buffer itself keeps the raw text that the answer submits.
fn input_rows(input: &str, placeholder: Option<&str>) -> Vec<RenderRow> {
    if input.is_empty() {
        return vec![RenderRow::new(
            format!("> {}", crate::width::escape(placeholder.unwrap_or(""))),
            Role::Text,
        )];
    }
    input
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            RenderRow::new(
                format!(
                    "{}{}",
                    if index == 0 { "> " } else { "  " },
                    crate::width::escape(line)
                ),
                Role::Text,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        RequestQueue, approval_answer, confirm_answer, select_multi, select_single, text_answer,
    };
    use crate::diagram::{DiagramSettings, RenderCache};
    use crate::theme::Role;
    use dal_core::Answer;
    use std::time::Instant;

    fn arm(dialog: &mut super::DialogUi) {
        let start = Instant::now();
        dialog.tick(start);
        dialog.tick(start + super::ARM_DELAY);
        assert!(dialog.armed());
    }

    fn press(
        dialog: &mut super::DialogUi,
        code: crossterm::event::KeyCode,
    ) -> Option<(dal_core::RequestId, Answer)> {
        dialog.key(crate::keys::Key::new(
            code,
            crossterm::event::KeyModifiers::NONE,
        ))
    }

    fn approval(id: dal_core::RequestId) -> dal_core::Request {
        dal_core::Request {
            id,
            turn: None,
            owner: dal_core::Owner::Core,
            question: dal_core::Question::Approval {
                tool: "exec".into(),
                preview: dal_core::Preview {
                    title: "command".into(),
                    body: "echo hi".into(),
                    digest: None,
                },
                grant: None,
                call: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Decline,
        }
    }

    fn hint(dialog: &super::DialogUi) -> String {
        dialog
            .rendered_rows(
                80,
                12,
                crate::WidthMode::Narrow,
                DiagramSettings::default(),
                &RenderCache::default(),
                true,
            )
            .into_iter()
            .last()
            .map(|row| row.text)
            .unwrap_or_default()
    }

    #[test]
    fn approval_keys_map_once_and_lock_until_resolved() {
        assert!(matches!(approval_answer('y'), Answer::Approve));
        assert!(matches!(approval_answer('a'), Answer::ApproveForSession));
        assert!(matches!(approval_answer('n'), Answer::Decline));
        assert!(matches!(approval_answer('\u{1b}'), Answer::Decline));
        let mut queue = RequestQueue::default();
        let id = dal_core::RequestId::new_v7();
        assert!(queue.mark_answered(id));
        assert!(!queue.mark_answered(id));
        assert!(queue.keys_disabled());
    }

    #[test]
    fn a_snapshot_that_drops_an_answered_request_still_announces_it_once() {
        let mut queue = RequestQueue::default();
        let id = dal_core::RequestId::new_v7();
        let request = approval(id);
        let title = super::dialog_title(&request.question);
        queue.opened(request);
        assert!(queue.mark_answered(id));

        // Event order of the saved-evidence race: the snapshot poll lands
        // before the pending `RequestResolved` update.
        queue.resync(Vec::new());
        assert!(
            !queue.keys_disabled(),
            "an empty snapshot frees the keyboard"
        );

        assert_eq!(queue.resolved(&id.to_string()), Some(title));
        assert_eq!(
            queue.resolved(&id.to_string()),
            None,
            "a repeated resolution must not announce twice"
        );
    }

    #[test]
    fn an_answered_request_resolved_while_still_open_announces_once() {
        let mut queue = RequestQueue::default();
        let id = dal_core::RequestId::new_v7();
        let request = approval(id);
        let title = super::dialog_title(&request.question);
        queue.opened(request);
        assert!(queue.mark_answered(id));

        assert_eq!(queue.resolved(&id.to_string()), Some(title));
        assert_eq!(
            queue.resolved(&id.to_string()),
            None,
            "the open path must not leave a retained title behind"
        );
    }

    #[test]
    fn a_request_never_answered_here_resolves_silently_after_a_snapshot() {
        let mut queue = RequestQueue::default();
        let visible = dal_core::RequestId::new_v7();
        let queued = dal_core::RequestId::new_v7();
        queue.opened(approval(visible));
        queue.opened(approval(queued));
        assert!(queue.mark_answered(visible));

        queue.resync(Vec::new());

        assert_eq!(
            queue.resolved(&queued.to_string()),
            None,
            "a request this client never answered has no title to announce"
        );
    }

    #[test]
    fn retained_answered_titles_stay_bounded() {
        let mut queue = RequestQueue::default();
        let ids: Vec<_> = (0..super::ANSWERED_TITLE_CAP + 8)
            .map(|_| dal_core::RequestId::new_v7())
            .collect();
        for id in &ids {
            queue.opened(approval(*id));
            assert!(queue.mark_answered(*id));
            queue.resync(Vec::new());
        }

        assert_eq!(queue.answered_titles.len(), super::ANSWERED_TITLE_CAP);
        let oldest = ids.first().copied().unwrap();
        let newest = ids.last().copied().unwrap();
        assert_eq!(
            queue.resolved(&oldest.to_string()),
            None,
            "the oldest answer is evicted once the cap is reached"
        );
        assert!(queue.resolved(&newest.to_string()).is_some());
    }

    #[test]
    fn select_and_text_answers_follow_the_mapping() {
        assert!(matches!(select_single("b"), Answer::Value(_)));
        assert!(matches!(select_multi(&["a", "c"]), Answer::Value(_)));
        assert!(matches!(select_multi(&[]), Answer::Cancel));
        assert!(matches!(text_answer(""), Answer::Cancel));
        assert!(matches!(text_answer("   "), Answer::Cancel));
        assert!(matches!(text_answer("x"), Answer::Value(_)));
        assert!(matches!(confirm_answer('n'), Answer::Value(_)));
    }

    /// The strict payload shapes the ask battery decodes, pinned byte for
    /// byte: `{"selected":[...]}` and `{"text":...}`.
    #[test]
    fn answers_carry_the_battery_payload_objects() {
        let Answer::Value(value) = select_single("PostgreSQL") else {
            panic!("single answer must be a value");
        };
        assert_eq!(value.as_str(), r#"{"selected":["PostgreSQL"]}"#);

        let Answer::Value(value) = text_answer("  use sqlite  ") else {
            panic!("text answer must be a value");
        };
        assert_eq!(value.as_str(), r#"{"text":"use sqlite"}"#);

        let Answer::Value(value) = select_multi(&["a", "b"]) else {
            panic!("multi answer must be a value");
        };
        assert_eq!(value.as_str(), r#"{"selected":["a","b"]}"#);
    }

    #[test]
    fn text_and_select_values_preserve_json_controls() {
        let text = "C:\\tmp\n\"x\"\ty\u{2028}z";
        let Answer::Value(value) = text_answer(text) else {
            panic!("text answer must retain the value");
        };
        assert_eq!(
            value.as_str(),
            "{\"text\":\"C:\\\\tmp\\n\\\"x\\\"\\ty\\u2028z\"}"
        );

        let Answer::Value(value) = select_multi(&["a\\b", "c\n\"d"]) else {
            panic!("select answer must retain the values");
        };
        assert_eq!(value.as_str(), "{\"selected\":[\"a\\\\b\",\"c\\n\\\"d\"]}");
    }

    #[test]
    fn grant_dialog_shows_the_mcp_server_detail() {
        use dal_core::{Answer, Owner, Question, Request, RequestId};
        use std::time::Duration;

        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Grant {
                ext: "web".into(),
                origin: "user".into(),
                capabilities: vec!["mcp".into()],
                detail: Some("search: https://mcp.example/search".into()),
            },
            timeout: Duration::from_secs(30),
            default: Answer::Decline,
        });
        let rows = dialog.rows(80, 20, crate::WidthMode::Narrow);
        assert!(
            rows.iter()
                .any(|row| row.contains("search: https://mcp.example/search"))
        );
    }

    #[test]
    fn diagram_approval_preview_renders_art_only_when_enabled() {
        use dal_core::{Answer, Owner, Preview, Question, Request, RequestId};
        use std::time::Duration;

        let source = "```mermaid\ngraph TD; A-->B\n```";
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Approval {
                tool: "patch".into(),
                preview: Preview {
                    title: "diagram".into(),
                    body: source.into(),
                    digest: None,
                },
                grant: None,
                call: None,
            },
            timeout: Duration::from_secs(30),
            default: Answer::Decline,
        });
        let cache = RenderCache::default();
        let off = dialog.rendered_rows(
            80,
            20,
            crate::WidthMode::Narrow,
            DiagramSettings::default(),
            &cache,
            true,
        );
        assert!(off.iter().any(|row| row.text.contains("graph TD")));
        assert_eq!(cache.renders(), 0);

        let cache = RenderCache::default();
        let generation = cache.generation();
        let _ = dialog.rendered_rows(
            80,
            20,
            crate::WidthMode::Narrow,
            DiagramSettings { enabled: true },
            &cache,
            true,
        );
        for _ in 0..200 {
            if cache.generation() > generation {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            cache.generation() > generation,
            "background renderer did not finish"
        );
        let on = dialog.rendered_rows(
            80,
            20,
            crate::WidthMode::Narrow,
            DiagramSettings { enabled: true },
            &cache,
            true,
        );
        assert!(
            on.iter()
                .any(|row| row.spans.iter().any(|span| span.role == Role::Accent))
        );
        assert!(
            !on.iter()
                .any(|row| row.text.contains("graph TD") || row.text.contains("```"))
        );
        assert_eq!(cache.renders(), 1);
    }
    #[test]
    fn fenced_question_titles_remain_plain_text_when_diagrams_are_enabled() {
        use dal_core::{Owner, Question, Request, RequestId};
        use std::time::Duration;

        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Text {
                prompt: "```mermaid\ngraph TD; A-->B\n```".into(),
                placeholder: None,
            },
            timeout: Duration::from_secs(30),
            default: Answer::Cancel,
        });
        let cache = RenderCache::default();
        let rows = dialog.rendered_rows(
            80,
            20,
            crate::WidthMode::Narrow,
            DiagramSettings { enabled: true },
            &cache,
            true,
        );

        assert!(rows.iter().any(|row| row.text.contains("graph TD; A-->B")));
        assert_eq!(cache.renders(), 0);
    }

    #[test]
    fn modified_letters_cannot_approve_an_approval() {
        use crossterm::event::{KeyCode, KeyModifiers};
        use dal_core::{Owner, Preview, Question, Request, RequestId};

        let mut dialog = super::DialogUi::default();
        let id = RequestId::new_v7();
        dialog.opened(Request {
            id,
            turn: None,
            owner: Owner::Core,
            question: Question::Approval {
                tool: "exec".into(),
                preview: Preview {
                    title: "command".into(),
                    body: "echo hi".into(),
                    digest: None,
                },
                grant: None,
                call: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Decline,
        });
        arm(&mut dialog);
        assert!(
            dialog
                .key(crate::keys::Key::new(
                    KeyCode::Char('a'),
                    KeyModifiers::CONTROL
                ))
                .is_none()
        );
        assert!(
            dialog
                .key(crate::keys::Key::new(KeyCode::Char('y'), KeyModifiers::ALT))
                .is_none()
        );
        assert!(matches!(
            dialog.key(crate::keys::Key::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            Some((answered, Answer::Approve)) if answered == id
        ));
        assert!(
            dialog
                .key(crate::keys::Key::new(
                    KeyCode::Char('y'),
                    KeyModifiers::NONE
                ))
                .is_none()
        );
    }

    #[test]
    fn multiline_titles_stay_inside_the_height_budget() {
        use dal_core::{Owner, Question, Request, RequestId};

        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Text {
                prompt: "one\ntwo\nthree\nfour\nfive\nsix".into(),
                placeholder: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        let render = |height| {
            dialog.rendered_rows(
                32,
                height,
                crate::WidthMode::Narrow,
                DiagramSettings::default(),
                &RenderCache::default(),
                true,
            )
        };
        let actions = render(40).pop().expect("actions row").text;
        let title_lines = ["one", "two", "three", "four", "five", "six"];
        for height in 3usize..=10 {
            let title_len = height.saturating_sub(2).min(title_lines.len()).max(1);
            let rows = render(height);
            assert_eq!(rows.len(), title_len + 2, "height {height}");
            for (row, expected) in rows.iter().take(title_len).zip(title_lines) {
                assert_eq!(row.text, expected, "height {height}");
                assert!(
                    !row.text.contains('\n'),
                    "height {height}: embedded newline"
                );
            }
            assert!(rows[title_len].text.starts_with("> "), "height {height}");
            assert_eq!(rows.last().expect("actions row").text, actions);
        }
    }

    #[test]
    fn multiline_question_titles_keep_each_logical_line() {
        use dal_core::{Owner, Question, Request, RequestId};

        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Text {
                prompt: "first line is long enough\nsecond line stays separate".into(),
                placeholder: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        let rows = dialog.rendered_rows(
            32,
            20,
            crate::WidthMode::Narrow,
            DiagramSettings::default(),
            &RenderCache::default(),
            true,
        );
        assert_eq!(rows[0].text, "first line is long enough");
        assert_eq!(rows[1].text, "second line stays separate");
    }

    fn select_with_preview() -> super::DialogUi {
        use dal_core::{Choice, Owner, Preview, Question, Request, RequestId};

        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: "Database: Which database?".into(),
                options: vec![
                    Choice {
                        label: "SQLite".into(),
                        description: Some("Embedded, zero config".into()),
                    },
                    Choice {
                        label: "Postgres".into(),
                        description: None,
                    },
                ],
                multi: false,
                preview: Some(Preview {
                    title: "schema".into(),
                    body: "line one\nline two\nline three\nline four\nline five".into(),
                    digest: None,
                }),
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        dialog
    }

    fn select_rows(dialog: &super::DialogUi, height: usize) -> Vec<String> {
        dialog
            .rendered_rows(
                60,
                height,
                crate::WidthMode::Narrow,
                DiagramSettings::default(),
                &RenderCache::default(),
                true,
            )
            .into_iter()
            .map(|row| row.text)
            .collect()
    }

    #[test]
    fn a_long_preview_never_hides_the_choices_or_their_descriptions() {
        let dialog = select_with_preview();
        let rows = select_rows(&dialog, 7);
        assert!(rows.len() <= 7, "{rows:?}");
        assert!(
            rows.iter()
                .any(|row| row.contains("SQLite - Embedded, zero config")),
            "{rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("Postgres")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("more lines")),
            "the clipped preview names how to read the rest: {rows:?}"
        );
    }

    #[test]
    fn page_down_reads_further_into_a_clipped_preview() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut dialog = select_with_preview();
        arm(&mut dialog);
        let before = select_rows(&dialog, 7);
        assert!(
            before.iter().any(|row| row.contains("line one")),
            "{before:?}"
        );
        assert!(
            !before.iter().any(|row| row.contains("line five")),
            "{before:?}"
        );
        for _ in 0..5 {
            assert!(
                dialog
                    .key(crate::keys::Key::new(KeyCode::PageDown, KeyModifiers::NONE))
                    .is_none()
            );
        }
        let after = select_rows(&dialog, 7);
        assert!(
            after.iter().any(|row| row.contains("line five")),
            "{after:?}"
        );
        assert!(
            after.iter().any(|row| row.contains("Postgres")),
            "{after:?}"
        );
    }

    #[test]
    fn a_fresh_dialog_drops_every_key_but_esc_until_armed() {
        use crossterm::event::KeyCode;

        let id = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        let start = Instant::now();
        dialog.tick(start);
        for code in [KeyCode::Char('y'), KeyCode::Char('a'), KeyCode::Char('n')] {
            assert!(press(&mut dialog, code).is_none());
        }
        dialog.tick(start + super::ARM_DELAY.saturating_sub(std::time::Duration::from_millis(1)));
        assert!(!dialog.armed());
        assert!(press(&mut dialog, KeyCode::Char('y')).is_none());
        dialog.tick(start + super::ARM_DELAY);
        assert!(matches!(
            press(&mut dialog, KeyCode::Char('y')),
            Some((answered, Answer::Approve)) if answered == id
        ));
    }

    #[test]
    fn keys_dropped_before_arming_are_not_replayed() {
        use crossterm::event::KeyCode;

        let id = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        let start = Instant::now();
        dialog.tick(start);
        assert!(press(&mut dialog, KeyCode::Char('a')).is_none());
        dialog.tick(start + super::ARM_DELAY);
        assert!(dialog.queue.shown().is_some());
        assert!(!dialog.awaiting_resolution());
        assert!(matches!(
            press(&mut dialog, KeyCode::Char('n')),
            Some((answered, Answer::Decline)) if answered == id
        ));
    }

    #[test]
    fn esc_answers_a_dialog_that_is_not_armed() {
        use crossterm::event::KeyCode;

        let id = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        assert!(matches!(
            press(&mut dialog, KeyCode::Esc),
            Some((answered, Answer::Decline)) if answered == id
        ));
    }

    #[test]
    fn the_hint_lists_answer_keys_only_once_armed() {
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(dal_core::RequestId::new_v7()));
        let start = Instant::now();
        dialog.tick(start);
        assert_eq!(hint(&dialog), "esc denies · answer keys ready in a moment");
        dialog.tick(start + super::ARM_DELAY);
        assert_eq!(
            hint(&dialog),
            "y allow · a session · n deny · v view · esc denies"
        );
    }

    #[test]
    fn each_queued_request_arms_on_its_own() {
        use crossterm::event::KeyCode;

        let first = dal_core::RequestId::new_v7();
        let second = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(first));
        dialog.opened(approval(second));
        arm(&mut dialog);
        assert!(press(&mut dialog, KeyCode::Char('y')).is_some());
        dialog.resolved(&first.to_string());
        assert!(!dialog.armed());
        assert!(press(&mut dialog, KeyCode::Char('y')).is_none());
        arm(&mut dialog);
        assert!(matches!(
            press(&mut dialog, KeyCode::Char('y')),
            Some((answered, Answer::Approve)) if answered == second
        ));
    }

    #[test]
    fn a_text_question_ignores_paste_and_typing_until_armed() {
        use crossterm::event::KeyCode;
        use dal_core::{Owner, Question, Request, RequestId};

        let id = RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id,
            turn: None,
            owner: Owner::Core,
            question: Question::Text {
                prompt: "Name?".into(),
                placeholder: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        dialog.paste(b"early");
        assert!(press(&mut dialog, KeyCode::Char('x')).is_none());
        arm(&mut dialog);
        // A blank submission is an empty submission: it dismisses.
        assert!(matches!(
            press(&mut dialog, KeyCode::Enter),
            Some((answered, Answer::Cancel)) if answered == id
        ));
    }

    #[test]
    fn a_text_question_collects_and_submits_its_row() {
        use crossterm::event::KeyCode;
        use dal_core::{Owner, Question, Request, RequestId};

        let id = RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id,
            turn: None,
            owner: Owner::Core,
            question: Question::Text {
                prompt: "Name?".into(),
                placeholder: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        press(&mut dialog, KeyCode::Char('z'));
        assert!(matches!(
            press(&mut dialog, KeyCode::Enter),
            Some((answered, Answer::Value(_))) if answered == id
        ));
    }

    #[test]
    fn tab_reaches_other_and_enter_opens_the_text_row() {
        use crossterm::event::KeyCode;
        use dal_core::{Choice, Owner, Question, Request, RequestId};

        let id = RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id,
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: "Store: pick one".into(),
                options: vec![
                    Choice {
                        label: "Alpha".into(),
                        description: None,
                    },
                    Choice {
                        label: "Beta".into(),
                        description: None,
                    },
                ],
                multi: false,
                preview: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        // Tab walks Alpha -> Beta -> Other.
        press(&mut dialog, KeyCode::Tab);
        press(&mut dialog, KeyCode::Tab);
        // Enter on Other opens the in-dialog text row instead of answering.
        assert!(press(&mut dialog, KeyCode::Enter).is_none());
        let rows = dialog.rows(80, 20, crate::WidthMode::Narrow);
        assert!(rows.iter().any(|row| row.contains("> ")), "{rows:?}");
        press(&mut dialog, KeyCode::Char('t'));
        press(&mut dialog, KeyCode::Char('e'));
        press(&mut dialog, KeyCode::Char('x'));
        // The typed Other text rides one off-list selected label.
        let Some((answered, Answer::Value(payload))) = press(&mut dialog, KeyCode::Enter) else {
            panic!("Other submits an answer");
        };
        assert_eq!(answered, id);
        assert_eq!(payload.as_str(), r#"{"selected":["tex"]}"#);
    }

    #[test]
    fn two_open_asks_show_one_dialog_with_a_queue_count() {
        use dal_core::{Choice, Owner, Question, Request, RequestId};

        let mut dialog = super::DialogUi::default();
        let select = |prompt: &str| Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: prompt.into(),
                options: vec![
                    Choice {
                        label: "Alpha".into(),
                        description: None,
                    },
                    Choice {
                        label: "Beta".into(),
                        description: None,
                    },
                ],
                multi: false,
                preview: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        };
        dialog.opened(select("first"));
        dialog.opened(select("second"));
        arm(&mut dialog);
        let Some((_request, waiting)) = dialog.queue.shown() else {
            panic!("the head question is shown");
        };
        assert_eq!(waiting, 1, "one queued question behind the head");
        let rows = dialog.rows(80, 20, crate::WidthMode::Narrow);
        assert!(
            rows.iter().any(|row| row.contains("1 more waiting")),
            "{rows:?}"
        );
        // Resolving the shown one reveals the next; Esc dismisses only the
        // shown question's questionnaire.
        let head = dialog
            .queue
            .shown()
            .map(|(request, _)| request.id.to_string())
            .unwrap();
        dialog.resolved(&head);
        assert!(dialog.is_open());
    }

    #[test]
    fn the_countdown_slot_counts_down_from_the_request_timeout() {
        use dal_core::{Owner, Question, Request, RequestId};
        use std::time::Duration;

        let mut dialog = super::DialogUi::default();
        let start = Instant::now();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Text {
                prompt: "soon".into(),
                placeholder: None,
            },
            timeout: Duration::from_secs(150),
            default: Answer::Cancel,
        });
        dialog.tick(start);
        dialog.tick(start + Duration::from_secs(90));
        let rows = dialog.rows(80, 20, crate::WidthMode::Narrow);
        assert!(rows.iter().any(|row| row.contains("1m left")), "{rows:?}");
    }

    /// The narrow approval fold keeps both deny cues longest: `v view`,
    /// then `y allow`, then `a session` drop in that order, `esc denies`
    /// outlives the unfocused verbs, and the focused `n deny` — the
    /// conservative default — is the last segment standing so Enter never
    /// activates a verb the row no longer shows.
    #[test]
    fn a_narrow_approval_keeps_the_deny_cues_longest() {
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(dal_core::RequestId::new_v7()));
        arm(&mut dialog);
        let action_row = |width: usize| {
            dialog
                .rendered_rows(
                    width,
                    12,
                    crate::WidthMode::Narrow,
                    DiagramSettings::default(),
                    &RenderCache::default(),
                    true,
                )
                .into_iter()
                .last()
                .map(|row| row.text)
                .unwrap_or_default()
        };
        assert_eq!(
            action_row(60),
            "y allow · a session · n deny · v view · esc denies"
        );
        assert_eq!(action_row(41), "y allow · a session · n deny · esc denies");
        assert_eq!(action_row(31), "a session · n deny · esc denies");
        assert_eq!(action_row(19), "n deny · esc denies");
        assert_eq!(action_row(12), "n deny");
    }

    /// While the keys are locked the arming notice folds before the deny
    /// cue does: a narrow dialog still shows that esc ends it.
    #[test]
    fn an_unarmed_narrow_dialog_keeps_its_deny_cue() {
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(dal_core::RequestId::new_v7()));
        dialog.tick(Instant::now());
        let action_row = |width: usize| {
            dialog
                .rendered_rows(
                    width,
                    12,
                    crate::WidthMode::Narrow,
                    DiagramSettings::default(),
                    &RenderCache::default(),
                    true,
                )
                .into_iter()
                .last()
                .map(|row| row.text)
                .unwrap_or_default()
        };
        assert_eq!(action_row(60), "esc denies · answer keys ready in a moment");
        assert_eq!(action_row(12), "esc denies");
    }

    /// The focused choice paints the theme's selection range over the whole
    /// row on top of its `>` marker; moving focus moves the range.
    #[test]
    fn the_focused_option_paints_the_selection_range() {
        use crossterm::event::KeyCode;
        use dal_core::{Choice, Owner, Question, Request, RequestId};

        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: "pick".into(),
                options: vec![
                    Choice {
                        label: "Alpha".into(),
                        description: None,
                    },
                    Choice {
                        label: "Beta".into(),
                        description: None,
                    },
                ],
                multi: false,
                preview: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        let focused = |dialog: &super::DialogUi| {
            dialog
                .rendered_rows(
                    60,
                    10,
                    crate::WidthMode::Narrow,
                    DiagramSettings::default(),
                    &RenderCache::default(),
                    true,
                )
                .into_iter()
                .filter(|row| !row.selected.is_empty())
                .map(|row| row.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(focused(&dialog), ["> (*) Alpha"]);
        press(&mut dialog, KeyCode::Down);
        assert_eq!(focused(&dialog), ["> (*) Beta"]);
        press(&mut dialog, KeyCode::Down);
        assert_eq!(focused(&dialog), ["> ( ) Other"]);
    }

    /// A select whose choices outnumber the rows keeps a window around the
    /// focus. At every height down to the title, one row and the actions,
    /// the block stays inside the height, spends all of it while choices
    /// are hidden and paints exactly one focused row: the choice or Other
    /// that Enter answers. Other stays on screen whenever a row is left
    /// beside the focused choice, and once a row is left for it, the
    /// window's end counts name every hidden choice.
    #[test]
    fn more_choices_than_rows_window_the_pinned_block() {
        use crossterm::event::KeyCode;
        use dal_core::{Choice, Owner, Question, Request, RequestId};

        const CHOICES: usize = 7;
        let options = (1..=CHOICES)
            .map(|index| Choice {
                label: format!("Option {index}").into(),
                description: None,
            })
            .collect();
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: "pick".into(),
                options,
                multi: false,
                preview: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        // Focus walks every choice, then Other, one Down at a time.
        for focus in 0..=CHOICES {
            let label = if focus == CHOICES {
                "( ) Other".to_owned()
            } else {
                format!("(*) Option {}", focus + 1)
            };
            for height in 3..=12 {
                let rows = dialog.rows(60, height, crate::WidthMode::Narrow);
                let at = format!("focus {focus}, height {height}: {rows:?}");
                assert!(rows.len() <= height, "{at}");
                assert_eq!(rows.first().map(String::as_str), Some("pick"), "{at}");
                assert!(
                    rows.last().is_some_and(|row| row.contains("esc dismiss")),
                    "{at}"
                );
                let focused: Vec<&str> = rows
                    .iter()
                    .map(String::as_str)
                    .filter(|row| row.starts_with("> "))
                    .collect();
                assert_eq!(focused, [format!("> {label}").as_str()], "{at}");
                // Three rows hold the title, the focused row and the actions:
                // beside a focused choice there is no row left for Other.
                if height > 3 || focus == CHOICES {
                    assert!(rows.iter().any(|row| row.ends_with("( ) Other")), "{at}");
                }
                let shown = rows.iter().filter(|row| row.contains("Option ")).count();
                if shown < CHOICES {
                    assert_eq!(rows.len(), height, "no row sits unused: {at}");
                }
                // Four rows leave one choice row and five leave two, with no
                // room to count the rest; from six the ends carry counts.
                if height >= 6 {
                    let counted: usize = rows
                        .iter()
                        .filter_map(|row| {
                            row.strip_prefix("... ")?
                                .strip_suffix(" more")?
                                .parse::<usize>()
                                .ok()
                        })
                        .sum();
                    assert_eq!(shown + counted, CHOICES, "{at}");
                }
            }
            press(&mut dialog, KeyCode::Down);
        }
    }

    /// A clipped preview never costs the focused row its place nor pushes
    /// the block past its height: with only a few rows left, what Enter
    /// answers outranks the preview's rows, which paint none at three.
    #[test]
    fn a_tight_preview_select_keeps_the_focused_row_on_screen() {
        use crossterm::event::KeyCode;

        let mut dialog = select_with_preview();
        arm(&mut dialog);
        for label in [
            "> (*) SQLite - Embedded, zero config",
            "> (*) Postgres",
            "> ( ) Other",
        ] {
            for height in 3..=7 {
                let rows = select_rows(&dialog, height);
                let focused: Vec<&str> = rows
                    .iter()
                    .map(String::as_str)
                    .filter(|row| row.starts_with("> "))
                    .collect();
                assert_eq!(focused, [label], "height {height}: {rows:?}");
                assert!(rows.len() <= height, "height {height}: {rows:?}");
            }
            press(&mut dialog, KeyCode::Down);
        }
    }

    /// While the Other text row is open, the typed text stays on screen and
    /// inside the height: at three rows it keeps the one row left, ahead of
    /// the Other label.
    #[test]
    fn the_open_other_row_keeps_the_typed_text_on_screen() {
        use crossterm::event::KeyCode;
        use dal_core::{Choice, Owner, Question, Request, RequestId};

        let options = (1..=7)
            .map(|index| Choice {
                label: format!("Option {index}").into(),
                description: None,
            })
            .collect();
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: "pick".into(),
                options,
                multi: false,
                preview: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        for _ in 0..7 {
            press(&mut dialog, KeyCode::Down);
        }
        assert!(press(&mut dialog, KeyCode::Enter).is_none());
        press(&mut dialog, KeyCode::Char('a'));
        press(&mut dialog, KeyCode::Char('b'));
        for height in 3..=8 {
            let rows = dialog.rows(60, height, crate::WidthMode::Narrow);
            assert!(rows.len() <= height, "height {height}: {rows:?}");
            assert_eq!(
                rows.first().map(String::as_str),
                Some("pick"),
                "height {height}: {rows:?}"
            );
            assert!(
                rows.iter().any(|row| row == "> ab"),
                "height {height}: {rows:?}"
            );
        }
    }

    /// Pasted bytes carry the answer but can never write terminal commands:
    /// the rendered row shows controls escaped while the submitted payload
    /// keeps them raw.
    #[test]
    fn pasted_control_bytes_render_escaped_and_submit_raw() {
        use crossterm::event::KeyCode;
        use dal_core::{Owner, Question, Request, RequestId};

        let id = RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id,
            turn: None,
            owner: Owner::Core,
            question: Question::Text {
                prompt: "say".into(),
                placeholder: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        dialog.paste(b"a\x1b[2Kb");
        let rows = dialog.rows(60, 10, crate::WidthMode::Narrow);
        assert!(
            rows.iter().any(|row| row.contains("a\\u{1b}[2Kb")),
            "{rows:?}"
        );
        let Some((answered, Answer::Value(payload))) = press(&mut dialog, KeyCode::Enter) else {
            panic!("the paste answers");
        };
        assert_eq!(answered, id);
        assert_eq!(payload.as_str(), r#"{"text":"a\u001b[2Kb"}"#);
    }

    /// Ctrl+J is the legacy newline chord: it reaches the open Other text
    /// row of a select the same as a text question.
    #[test]
    fn ctrl_j_adds_a_newline_row_inside_the_other_escape() {
        use crossterm::event::{KeyCode, KeyModifiers};
        use dal_core::{Choice, Owner, Question, Request, RequestId};

        let id = RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id,
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: "pick".into(),
                options: vec![Choice {
                    label: "Alpha".into(),
                    description: None,
                }],
                multi: false,
                preview: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        press(&mut dialog, KeyCode::Tab);
        assert!(press(&mut dialog, KeyCode::Enter).is_none());
        press(&mut dialog, KeyCode::Char('a'));
        let newline = crate::keys::Key::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert!(dialog.key(newline).is_none());
        press(&mut dialog, KeyCode::Char('b'));
        let rows = dialog.rows(60, 10, crate::WidthMode::Narrow);
        assert!(rows.iter().any(|row| row.contains("> a")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("  b")), "{rows:?}");
        let Some((answered, Answer::Value(payload))) = press(&mut dialog, KeyCode::Enter) else {
            panic!("the two-line Other text answers");
        };
        assert_eq!(answered, id);
        assert_eq!(payload.as_str(), r#"{"selected":["a\nb"]}"#);
    }

    /// Host-supplied choice labels render escaped: control bytes can never
    /// reach the terminal through the label.
    #[test]
    fn a_host_label_renders_escaped() {
        use dal_core::{Choice, Owner, Question, Request, RequestId};

        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Select {
                prompt: "pick".into(),
                options: vec![Choice {
                    label: "bad\u{1b}[2Klabel".into(),
                    description: None,
                }],
                multi: false,
                preview: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        let rows = dialog.rows(60, 10, crate::WidthMode::Narrow);
        assert!(
            rows.iter().any(|row| row.contains("bad\\u{1b}[2Klabel")),
            "{rows:?}"
        );
    }

    /// The legacy newline chord renders the legacy hint; kitty terminals
    /// name shift+enter.
    #[test]
    fn the_text_hint_names_the_newline_chord_the_terminal_delivers() {
        use dal_core::{Owner, Question, Request, RequestId};

        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Text {
                prompt: "say".into(),
                placeholder: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        let last = |kitty: bool| {
            dialog
                .rendered_rows(
                    60,
                    10,
                    crate::WidthMode::Narrow,
                    DiagramSettings::default(),
                    &RenderCache::default(),
                    kitty,
                )
                .into_iter()
                .last()
                .map(|row| row.text)
                .unwrap_or_default()
        };
        assert_eq!(
            last(true),
            "enter submit · shift+enter newline · esc dismiss"
        );
        assert_eq!(last(false), "enter submit · ctrl+j newline · esc dismiss");
    }

    /// The action table owns the dialog's armed keys: Enter activates the
    /// focused verb, which defaults to the conservative deny.
    #[test]
    fn enter_activates_the_focused_deny_verb() {
        use crossterm::event::KeyCode;

        let id = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        arm(&mut dialog);
        assert!(matches!(
            press(&mut dialog, KeyCode::Enter),
            Some((answered, Answer::Decline)) if answered == id
        ));
    }

    /// Tab and Shift+Tab move the focused verb around the three answer
    /// verbs and wrap at both ends: the view cue is not focusable, so
    /// Right from deny wraps to allow-once and Left from deny lands on
    /// allow-session.
    #[test]
    fn tab_and_arrows_move_the_focused_verb_around_the_approval() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let id = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        arm(&mut dialog);
        // Tab is focus-forward: deny wraps to allow-once.
        press(&mut dialog, KeyCode::Tab);
        assert!(matches!(
            press(&mut dialog, KeyCode::Enter),
            Some((answered, Answer::Approve)) if answered == id
        ));

        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        arm(&mut dialog);
        // Left steps backwards: deny → session, then deny again.
        press(&mut dialog, KeyCode::Left);
        assert!(matches!(
            press(&mut dialog, KeyCode::Enter),
            Some((answered, Answer::ApproveForSession)) if answered == id
        ));

        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        arm(&mut dialog);
        // Shift+Tab arrives as BackTab (legacy CSI Z carries no modifiers;
        // kitty marks it SHIFT — resolve normalizes both).
        dialog.key(crate::keys::Key::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert!(matches!(
            press(&mut dialog, KeyCode::Enter),
            Some((answered, Answer::ApproveForSession)) if answered == id
        ));
    }

    /// A direct letter still answers without moving focus first, and a
    /// modifier over a plain key is ignored — a shifted `y` resolves to
    /// the allow verb.
    #[test]
    fn shifted_letters_and_direct_keys_still_answer() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let id = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        arm(&mut dialog);
        let Some((answered, answer)) = dialog.key(crate::keys::Key::new(
            KeyCode::Char('Y'),
            KeyModifiers::SHIFT,
        )) else {
            panic!("shifted y reaches the allow binding");
        };
        assert_eq!(answered, id);
        assert_eq!(answer, Answer::Approve);

        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(id));
        arm(&mut dialog);
        assert!(
            dialog
                .key(crate::keys::Key::new(
                    KeyCode::Char('y'),
                    KeyModifiers::CONTROL,
                ))
                .is_none()
        );
    }

    /// Esc answers before the table sees it: an armed approval declines,
    /// a non-approval kind cancels.
    #[test]
    fn esc_answers_before_the_table_sees_it() {
        use crossterm::event::KeyCode;
        use dal_core::{Owner, Question, Request};

        let id = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id,
            turn: None,
            owner: Owner::Core,
            question: Question::Confirm {
                text: "really?".into(),
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        assert!(matches!(
            press(&mut dialog, KeyCode::Esc),
            Some((answered, Answer::Cancel)) if answered == id
        ));
    }

    /// The confirm dialog's focused verb defaults to `n`: Enter says no
    /// and a Left step wraps to `y`.
    #[test]
    fn a_confirm_focuses_no_by_default() {
        use crossterm::event::KeyCode;

        let id = dal_core::RequestId::new_v7();
        let mut dialog = super::DialogUi::default();
        dialog.opened(dal_core::Request {
            id,
            turn: None,
            owner: dal_core::Owner::Core,
            question: dal_core::Question::Confirm {
                text: "really?".into(),
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        let Some((answered, Answer::Value(first))) = press(&mut dialog, KeyCode::Enter) else {
            panic!("enter activates the focused no");
        };
        assert_eq!(answered, id);
        assert_eq!(first.as_str(), "false");

        let mut dialog = super::DialogUi::default();
        dialog.opened(dal_core::Request {
            id,
            turn: None,
            owner: dal_core::Owner::Core,
            question: dal_core::Question::Confirm {
                text: "really?".into(),
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Cancel,
        });
        arm(&mut dialog);
        press(&mut dialog, KeyCode::Left);
        let Some((answered, Answer::Value(second))) = press(&mut dialog, KeyCode::Enter) else {
            panic!("left wraps focus onto yes");
        };
        assert_eq!(answered, id);
        assert_eq!(second.as_str(), "true");
    }

    /// While the arming delay is open the focused verb cannot be moved
    /// and Enter stays inert.
    #[test]
    fn the_arming_delay_covers_focus_and_activate() {
        use crossterm::event::KeyCode;

        let mut dialog = super::DialogUi::default();
        dialog.opened(approval(dal_core::RequestId::new_v7()));
        dialog.tick(Instant::now());
        assert!(press(&mut dialog, KeyCode::Tab).is_none());
        assert!(press(&mut dialog, KeyCode::Enter).is_none());
        assert!(press(&mut dialog, KeyCode::Left).is_none());
    }

    /// The draft-confirm surface defaults to Keep — the choice that loses
    /// nothing — and left/right move it before activation.
    #[test]
    fn the_draft_confirm_defaults_to_keeping_the_draft() {
        use super::{DraftChoice, DraftConfirm};
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut confirm = DraftConfirm::default();
        assert_eq!(confirm.focused(), DraftChoice::Keep);
        assert_eq!(
            confirm.key(crate::keys::Key::new(KeyCode::Left, KeyModifiers::NONE)),
            None
        );
        assert_eq!(confirm.focused(), DraftChoice::Discard);
        assert_eq!(
            confirm.key(crate::keys::Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(DraftChoice::Discard)
        );

        let mut confirm = DraftConfirm::default();
        // Shift+Tab from Keep wraps to Discard — the legacy BackTab form.
        assert_eq!(
            confirm.key(crate::keys::Key::new(KeyCode::BackTab, KeyModifiers::NONE)),
            None
        );
        assert_eq!(confirm.focused(), DraftChoice::Discard);
        // The kitty form carries SHIFT and must move the same way.
        let mut confirm = DraftConfirm::default();
        assert_eq!(
            confirm.key(crate::keys::Key::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            None
        );
        assert_eq!(confirm.focused(), DraftChoice::Discard);
    }

    /// The letters of the committed quit confirmation still answer, in both
    /// cases and with or without a reported Shift, and Esc answers Keep; the
    /// default focus stays on the choice that loses nothing.
    #[test]
    fn the_draft_confirm_keeps_the_committed_letter_answers() {
        use super::{DraftChoice, DraftConfirm};
        use crossterm::event::{KeyCode, KeyModifiers};

        let key = |code, modifiers| crate::keys::Key::new(code, modifiers);
        for (letter, modifiers, expected) in [
            ('y', KeyModifiers::NONE, DraftChoice::Discard),
            ('Y', KeyModifiers::NONE, DraftChoice::Discard),
            ('Y', KeyModifiers::SHIFT, DraftChoice::Discard),
            ('n', KeyModifiers::NONE, DraftChoice::Keep),
            ('N', KeyModifiers::NONE, DraftChoice::Keep),
            ('N', KeyModifiers::SHIFT, DraftChoice::Keep),
        ] {
            let mut confirm = DraftConfirm::default();
            assert_eq!(
                confirm.key(key(KeyCode::Char(letter), modifiers)),
                Some(expected),
                "{letter:?} {modifiers:?}"
            );
        }
        let mut confirm = DraftConfirm::default();
        assert_eq!(
            confirm.key(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(DraftChoice::Keep)
        );
        for other in ['x', 'q', ' '] {
            let mut confirm = DraftConfirm::default();
            assert_eq!(
                confirm.key(key(KeyCode::Char(other), KeyModifiers::NONE)),
                None,
                "{other:?} is ignored"
            );
            assert_eq!(confirm.focused(), DraftChoice::Keep);
        }
    }

    /// An uppercase answer letter answers an approval like its lowercase
    /// form whether the terminal reports Shift or only the uppercase
    /// character: the legacy decoder and Caps Lock send no modifier, and the
    /// committed dialog matched `Y`, `A`, `N`, and `V` either way.
    #[test]
    fn uppercase_answer_letters_answer_an_approval_with_or_without_shift() {
        use crossterm::event::{KeyCode, KeyModifiers};

        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            for (letter, expected) in [
                ('Y', Answer::Approve),
                ('A', Answer::ApproveForSession),
                ('N', Answer::Decline),
            ] {
                let id = dal_core::RequestId::new_v7();
                let mut dialog = super::DialogUi::default();
                dialog.opened(approval(id));
                arm(&mut dialog);
                let answered = dialog.key(crate::keys::Key::new(KeyCode::Char(letter), modifiers));
                assert!(
                    matches!(&answered, Some((who, answer)) if *who == id && *answer == expected),
                    "{letter:?} {modifiers:?} answers {expected:?}, got {answered:?}"
                );
            }
        }
    }
}
