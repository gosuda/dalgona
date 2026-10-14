//! One-dialog-at-a-time request queue with exactly-once answers.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use dal_core::{Answer, CallGrant, ExecPreview, Question, RawJson, Request};

use crate::diagram::{DiagramSettings, RenderCache};
use crate::render::{Prose, RenderRow, prose_rows, text_rows};
use crate::theme::Role;

/// Pending requests in open order with sent-answer tracking.
#[derive(Debug, Default)]
pub struct RequestQueue {
    open: Vec<Request>,
    answered: HashSet<dal_core::RequestId>,
    keys_disabled: bool,
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
    pub fn resolved(&mut self, id: &str) -> Option<String> {
        let position = self
            .open
            .iter()
            .position(|request| request.id.to_string() == id)?;
        let request = self.open.remove(position);
        self.sync_disabled();
        Some(dialog_title(&request.question))
    }

    /// Rebuilds the queue from a `Resync` view; silently drops vanished ids.
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
    pub fn mark_answered(&mut self, id: dal_core::RequestId) -> bool {
        if !self.answered.insert(id) {
            return false;
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
    empty_hint: bool,
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

    /// Advances the arming clock; call once per frame just before painting.
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
            self.empty_hint = false;
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
            && matches!(
                (&request.question, key.code),
                (Question::Select { .. }, KeyCode::Char('o'))
                    | (Question::Text { .. }, KeyCode::Char('j'))
            );
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
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(ch);
                self.empty_hint = false;
                FreeText::Editing
            }
            _ => FreeText::Ignored,
        }
    }

    /// Maps one key to an answer for the shown question kind, mutating only
    /// dialog-local focus, checked set, scroll, and input buffer.
    fn question_answer(&mut self, question: &Question, key: crate::keys::Key) -> Option<Answer> {
        use crossterm::event::{KeyCode, KeyModifiers};
        match question {
            Question::Approval { .. } | Question::Grant { .. } => match key.code {
                KeyCode::Char('y' | 'Y') => Some(Answer::Approve),
                KeyCode::Char('a' | 'A') => Some(Answer::ApproveForSession),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(Answer::Decline),
                KeyCode::Char('v' | 'V') => {
                    self.expanded = !self.expanded;
                    None
                }
                KeyCode::PageDown => {
                    self.scroll = self.scroll.saturating_add(1);
                    None
                }
                KeyCode::PageUp => {
                    self.scroll = self.scroll.saturating_sub(1);
                    None
                }
                _ => None,
            },
            Question::Select { options, multi, .. } => {
                if self.text_mode {
                    return self.other_row_key(key);
                }
                // Focus reaches every offered option plus the always-present
                // Other row; arrows and tab move with wrap.
                let rows = options.len() + 1;
                match key.code {
                    KeyCode::Esc => Some(Answer::Cancel),
                    KeyCode::Up => {
                        self.focused = (self.focused + rows - 1) % rows;
                        None
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        self.focused = (self.focused + 1) % rows;
                        None
                    }
                    KeyCode::Char(' ') if *multi && self.focused < options.len() => {
                        if !self.checked.insert(self.focused) {
                            self.checked.remove(&self.focused);
                        }
                        None
                    }
                    KeyCode::Char(' ') if self.focused < options.len() => options
                        .get(self.focused)
                        .map(|option| select_single(&option.label)),
                    KeyCode::Enter if self.focused == options.len() => {
                        // The Other escape opens the in-dialog text row.
                        self.text_mode = true;
                        None
                    }
                    KeyCode::Enter if *multi => {
                        let labels: Vec<&str> = self
                            .checked
                            .iter()
                            .filter_map(|index| options.get(*index))
                            .map(|option| option.label.as_ref())
                            .collect();
                        Some(select_multi(&labels))
                    }
                    KeyCode::Enter => options
                        .get(self.focused)
                        .map(|option| select_single(&option.label)),
                    KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.expanded = !self.expanded;
                        None
                    }
                    KeyCode::PageDown => {
                        self.expanded = true;
                        self.scroll = self.scroll.saturating_add(1);
                        None
                    }
                    KeyCode::PageUp => {
                        self.scroll = self.scroll.saturating_sub(1);
                        None
                    }
                    _ => None,
                }
            }
            Question::Text { .. } => self.text_row_key(key),
            Question::Confirm { .. } => match key.code {
                KeyCode::Char('y' | 'Y') => Some(confirm_answer('y')),
                KeyCode::Char('n' | 'N') => Some(confirm_answer('n')),
                KeyCode::Esc => Some(Answer::Cancel),
                _ => None,
            },
            _ => (key.code == KeyCode::Esc).then_some(Answer::Cancel),
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
        )
        .into_iter()
        .map(|row| row.text)
        .collect()
    }

    pub(crate) fn rendered_rows(
        &self,
        width: usize,
        height: usize,
        mode: crate::WidthMode,
        settings: DiagramSettings,
        cache: &RenderCache,
    ) -> Vec<RenderRow> {
        let Some((request, waiting)) = self.queue.shown() else {
            return Vec::new();
        };
        let mut title = dialog_title(&request.question);
        if waiting > 0 {
            use std::fmt::Write as _;
            let _ = write!(title, " · {waiting} more waiting");
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
        let (mut body, pinned) = self.body_parts(&request.question, width, mode, settings, cache);
        let actions = self.actions(&request.question);
        let visible = height.saturating_sub(rows.len() + 1).max(1);
        // Pinned rows (the choices of a selection) stay on screen; only the
        // rows above them scroll.
        let room = visible.saturating_sub(pinned.len()).max(1);
        let hidden = body.len().saturating_sub(room);
        let scroll = if self.expanded {
            self.scroll.min(hidden)
        } else {
            self.scroll
        };
        if hidden > 0 && !self.expanded {
            let shown = room.saturating_sub(1);
            body.truncate(shown);
            body.push(RenderRow::new(
                format!("... {} more lines · pgdn", hidden + 1),
                Role::Dim,
            ));
        }
        rows.extend(body.into_iter().skip(scroll).take(room));
        rows.extend(pinned);
        rows.push(RenderRow::new(actions, Role::Text));
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
            return vec![RenderRow::new(format!("> {}", self.input), Role::Text)];
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
    /// always-present Other action.
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
                RenderRow::new(
                    format!(
                        "{} {marker} {}{description}",
                        if index == self.focused { ">" } else { " " },
                        option.label
                    ),
                    Role::Text,
                )
            })
            .collect();
        let marker = if multi { "[ ]" } else { "( )" };
        pinned.push(RenderRow::new(
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
        ));
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
                if let Some(exec) = ExecPreview::from_preview(preview) {
                    rows.push(RenderRow::new(
                        format!("cwd: {}", exec.cwd.display()),
                        Role::Text,
                    ));
                    rows.push(RenderRow::new(
                        format!("shell: {}", exec.shell.display()),
                        Role::Text,
                    ));
                    rows.extend(rows_of(&exec.command, Prose::Verbatim));
                } else {
                    rows.extend(rows_of(&preview.body, Prose::Verbatim));
                }
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
            Question::Text { placeholder, .. } => {
                vec![RenderRow::new(
                    format!(
                        "> {}",
                        if self.input.is_empty() {
                            placeholder.as_deref().unwrap_or("")
                        } else {
                            &self.input
                        }
                    ),
                    Role::Text,
                )]
            }
            Question::Confirm { .. } => vec![RenderRow::new("[y] Yes   [n] No", Role::Text)],
            _ => vec![RenderRow::new(
                "This question type is not supported here. Answer it from another client.",
                Role::Text,
            )],
        };
        if self.empty_hint {
            rows.push(RenderRow::new(
                crate::copy::ids::ASK_EMPTY_TEXT,
                Role::Warning,
            ));
        }
        if rows.is_empty() && pinned.is_empty() {
            rows.push(RenderRow::new(String::new(), Role::Text));
        }
        (rows, pinned)
    }

    fn actions(&self, question: &Question) -> String {
        let approving = matches!(question, Question::Approval { .. } | Question::Grant { .. });
        if approving && self.queue.keys_disabled() {
            return "Waiting for the request to settle...".to_owned();
        }
        if !self.armed() {
            let esc = if approving {
                crate::copy::ids::APPROVAL_ESC_DENIES
            } else {
                "esc dismiss"
            };
            return format!("{esc} · {}", crate::copy::ids::DIALOG_ARMING);
        }
        match question {
            Question::Approval { .. } | Question::Grant { .. } => format!(
                "{} · v view · {}",
                crate::copy::ids::DIALOG_ACTIONS_SHORT,
                crate::copy::ids::APPROVAL_ESC_DENIES
            ),
            Question::Select { .. } if self.text_mode => crate::copy::ids::ASK_HINT_TEXT.to_owned(),
            Question::Select { multi: true, .. } => crate::copy::ids::ASK_HINT_MULTI.to_owned(),
            Question::Select { .. } => crate::copy::ids::ASK_HINT_SINGLE.to_owned(),
            Question::Text { .. } => crate::copy::ids::ASK_HINT_TEXT.to_owned(),
            Question::Confirm { .. } => crate::copy::ids::ASK_HINT_CONFIRM.to_owned(),
            _ => "esc dismiss".to_owned(),
        }
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
        self.empty_hint = false;
        self.arming = Arming::Fresh;
        self.opened_at = self.now.or_else(|| Some(Instant::now()));
    }
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
    fn exec_dialog_shows_the_effective_directory_shell_and_command() {
        use dal_core::{Answer, ExecPreview, Owner, Question, Request, RequestId};

        let workspace = std::env::current_dir().expect("read workspace");
        let cwd = workspace.join("project");
        let shell = workspace.join("bin/bash");
        let mut dialog = super::DialogUi::default();
        dialog.opened(Request {
            id: RequestId::new_v7(),
            turn: None,
            owner: Owner::Core,
            question: Question::Approval {
                tool: "exec".into(),
                preview: ExecPreview {
                    command: "echo approved".into(),
                    cwd: cwd.clone(),
                    shell: shell.clone(),
                }
                .to_preview()
                .expect("serialize execution context"),
                grant: None,
                call: None,
            },
            timeout: std::time::Duration::from_secs(30),
            default: Answer::Decline,
        });
        let rows = dialog.rows(400, 30, crate::WidthMode::Narrow);
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!("cwd: {}", cwd.display()))),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!("shell: {}", shell.display()))),
            "{rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("echo approved")), "{rows:?}");
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
        );
        assert!(off.iter().any(|row| row.text.contains("graph TD")));
        assert_eq!(cache.renders(), 0);

        let cache = RenderCache::default();
        let on = dialog.rendered_rows(
            80,
            20,
            crate::WidthMode::Narrow,
            DiagramSettings { enabled: true },
            &cache,
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
}
