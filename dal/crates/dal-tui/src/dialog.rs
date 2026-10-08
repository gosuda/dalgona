//! One-dialog-at-a-time request queue with exactly-once answers.

use std::collections::HashSet;

use dal_core::{Answer, CallGrant, Question, RawJson, Request};

use crate::diagram::{DiagramSettings, RenderCache};
use crate::render::{RenderRow, text_rows};
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
    match choice {
        'y' | 'Y' => value_bool(true),
        _ => value_bool(false),
    }
}

/// Builds a single-select value answer from the focused option label.
#[must_use]
pub fn select_single(label: &str) -> Answer {
    value_string(label)
}

/// Builds a multi-select value answer in option order (possibly empty).
#[must_use]
pub fn select_multi(labels: &[&str]) -> Answer {
    let mut body = String::from("[");
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        append_json_string(&mut body, label);
    }
    body.push(']');
    RawJson::parse(&body).map_or(Answer::Cancel, Answer::Value)
}

/// Builds a free-text value answer; empty text sends no answer.
#[must_use]
pub fn text_answer(text: &str) -> Option<Answer> {
    if text.is_empty() {
        return None;
    }
    Some(value_string(text))
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

fn value_string(text: &str) -> Answer {
    let mut body = String::new();
    append_json_string(&mut body, text);
    RawJson::parse(&body).map_or(Answer::Cancel, Answer::Value)
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
            ch if ch.is_control() => {
                let _ = write!(output, "\\u{:04x}", u32::from(ch));
            }
            ch => output.push(ch),
        }
    }
    output.push('"');
}

fn value_bool(value: bool) -> Answer {
    RawJson::parse(if value { "true" } else { "false" }).map_or(Answer::Cancel, Answer::Value)
}

/// Keyboard and rendering state for the request currently at the queue head.
#[derive(Debug, Default)]
pub struct DialogUi {
    /// Open requests, in arrival order.
    pub queue: RequestQueue,
    focused: usize,
    checked: std::collections::BTreeSet<usize>,
    input: String,
    expanded: bool,
    scroll: usize,
    empty_hint: bool,
    active_id: Option<String>,
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

    /// Inserts pasted content only into a live free-text question.
    pub fn paste(&mut self, bytes: &[u8]) {
        let Some((request, _)) = self.queue.shown() else {
            return;
        };
        if matches!(request.question, Question::Text { .. }) && !self.queue.keys_disabled() {
            self.input.push_str(&String::from_utf8_lossy(bytes));
            self.empty_hint = false;
        }
    }

    /// Maps a key into an answer; an answered request disables its keys until resolution.
    pub fn key(&mut self, key: crate::keys::Key) -> Option<(dal_core::RequestId, Answer)> {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (request, _) = self.queue.shown()?;
        if self.queue.keys_disabled() {
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
            Question::Select { options, multi, .. } => match key.code {
                KeyCode::Esc => Some(Answer::Cancel),
                KeyCode::Up => {
                    self.focused = self.focused.saturating_sub(1);
                    None
                }
                KeyCode::Down => {
                    self.focused = (self.focused + 1).min(options.len().saturating_sub(1));
                    None
                }
                KeyCode::Char(' ') if *multi => {
                    if !self.checked.insert(self.focused) {
                        self.checked.remove(&self.focused);
                    }
                    None
                }
                KeyCode::Char(' ') => options
                    .get(self.focused)
                    .map(|option| select_single(&option.label)),
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
                _ => None,
            },
            Question::Text { .. } => match key.code {
                KeyCode::Esc => Some(Answer::Cancel),
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.input.push('\n');
                    None
                }
                KeyCode::Enter => {
                    let answer = text_answer(&self.input);
                    self.empty_hint = answer.is_none();
                    answer
                }
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input.push('\n');
                    None
                }
                KeyCode::Backspace => {
                    crate::composer::pop_grapheme(&mut self.input);
                    None
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input.push(ch);
                    self.empty_hint = false;
                    None
                }
                _ => None,
            },
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
        let mut rows = text_rows(&title, width, mode, settings, cache)
            .into_iter()
            .map(|mut row| {
                row.role = Role::Accent;
                row
            })
            .collect::<Vec<_>>();
        rows.truncate(height.saturating_sub(2).max(1));
        let mut body = self.body(&request.question, width, mode, settings, cache);
        let actions = self.actions(&request.question);
        let visible = height.saturating_sub(rows.len() + 1).max(1);
        let hidden = body.len().saturating_sub(visible);
        if hidden > 0 && !self.expanded {
            let shown = visible.saturating_sub(1);
            body.truncate(shown);
            body.push(RenderRow::new(
                format!("... {} more lines · pgdn", hidden + 1),
                Role::Dim,
            ));
        }
        rows.extend(body.into_iter().skip(self.scroll).take(visible));
        rows.push(RenderRow::new(actions, Role::Text));
        rows.into_iter()
            .map(|row| row.clipped(width, mode))
            .collect()
    }

    fn body(
        &self,
        question: &Question,
        width: usize,
        mode: crate::WidthMode,
        settings: DiagramSettings,
        cache: &RenderCache,
    ) -> Vec<RenderRow> {
        let mut rows = match question {
            Question::Approval { preview, grant, .. } => {
                let mut rows = Vec::new();
                if let Some(grant) = grant {
                    rows.push(RenderRow::new(grant_clause(grant), Role::Text));
                }
                rows.extend(text_rows(&preview.body, width, mode, settings, cache));
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
                let mut rows = preview
                    .iter()
                    .flat_map(|preview| text_rows(&preview.body, width, mode, settings, cache))
                    .collect::<Vec<_>>();
                rows.extend(options.iter().enumerate().map(|(index, option)| {
                    let checked =
                        self.checked.contains(&index) || (!*multi && index == self.focused);
                    let marker = match (*multi, checked) {
                        (true, true) => "[x]",
                        (true, false) => "[ ]",
                        (false, true) => "(*)",
                        (false, false) => "( )",
                    };
                    RenderRow::new(
                        format!(
                            "{} {marker} {}",
                            if index == self.focused { ">" } else { " " },
                            option.label
                        ),
                        Role::Text,
                    )
                }));
                rows
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
        if rows.is_empty() {
            rows.push(RenderRow::new(String::new(), Role::Text));
        }
        rows
    }

    fn actions(&self, question: &Question) -> String {
        match question {
            Question::Approval { .. } | Question::Grant { .. } => {
                if self.queue.keys_disabled() {
                    "Waiting for the request to settle...".to_owned()
                } else {
                    "y allow · a session · n deny · v view · esc denies".to_owned()
                }
            }
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
        self.expanded = false;
        self.scroll = 0;
        self.empty_hint = false;
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
        assert!(matches!(select_multi(&[]), Answer::Value(_)));
        assert!(text_answer("").is_none());
        assert!(matches!(text_answer("x"), Some(Answer::Value(_))));
        assert!(matches!(confirm_answer('n'), Answer::Value(_)));
    }

    #[test]
    fn text_and_select_values_preserve_json_controls() {
        let text = "C:\\\\tmp\\n\\\"x\\\"\\t\\u{2028}";
        let Some(Answer::Value(value)) = text_answer(text) else {
            panic!("text answer must retain the value");
        };
        assert_eq!(value.decode_as::<String>().unwrap(), text);

        let Answer::Value(value) = select_multi(&["a\\\\b", "c\\n\\\"d"]) else {
            panic!("select answer must retain the values");
        };
        assert_eq!(
            value.decode_as::<Vec<String>>().unwrap(),
            ["a\\\\b", "c\\n\\\"d"]
        );
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
        for height in 3..=10 {
            let rows = render(height);
            assert!(rows.len() <= height, "height {height}: {} rows", rows.len());
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
}
