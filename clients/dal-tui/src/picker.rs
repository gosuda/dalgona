//! Pickers and settings rows: model, resume, tree, settings, help.

use dal_core::{Command, EntryKind, EntryView, Family, JournalPart, ModelInfo, ModelRoute, View};

use crate::copy::ids;

/// Filters model ids by a typed substring, case-insensitive.
#[must_use]
pub fn filter_models<'a>(models: &'a [String], filter: &str) -> Vec<&'a str> {
    let needle = filter.to_lowercase();
    let mut matches: Vec<&str> = models
        .iter()
        .map(String::as_str)
        .filter(|model| model.to_lowercase().contains(&needle))
        .collect();
    matches.sort_unstable();
    matches
}

/// Renders the model picker title for a provider and count.
#[must_use]
pub fn model_title(count: u64, provider: &str) -> String {
    crate::copy::render(
        ids::MODEL_PICKER_TITLE,
        &[("n", &count.to_string()), ("provider", provider)],
        count,
    )
}

/// One model shown by the terminal model picker.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelOption {
    /// Human-readable name rendered in the list.
    pub label: String,
    /// Typed host command submitted when the row is selected.
    pub command: Command,
    /// Provider-qualified scope id (`{provider}/{route.id()}`) for `Api`
    /// rows with a known provider; `None` for synthetic, harness, or
    /// legacy rows the scope gate cannot match. Never derived from display
    /// labels or family tables.
    pub scoped_id: Option<Box<str>>,
}

/// Converts host model rows into typed route selections.
#[must_use]
pub fn model_options(models: Vec<ModelInfo>) -> Vec<ModelOption> {
    models
        .into_iter()
        .map(|model| {
            let label = format!("{} · {}", model.name, route_label(&model.route));
            let scoped_id = match &model.route {
                ModelRoute::Api { .. } if !model.provider.is_empty() => {
                    Some(format!("{}/{}", model.provider, model.route.id()).into_boxed_str())
                }
                _ => None,
            };
            let command = Command::SetModel {
                model: model.route,
                save: dal_core::command::Save::SessionAndDefault,
            };
            ModelOption {
                label,
                command,
                scoped_id,
            }
        })
        .collect()
}

pub(crate) fn route_label(route: &ModelRoute) -> String {
    match route {
        ModelRoute::Api { family, model, .. } => {
            let provider = match family {
                Family::Chat => "openai-chat",
                Family::Responses => "openai-responses",
                Family::Codex => "openai-codex",
                Family::Anthropic => "anthropic",
                Family::Bedrock => "bedrock",
                Family::Cursor => "cursor",
                Family::Devin => "devin",
                Family::Droid => "droid",
                Family::GitLab => "gitlab",
                Family::GoogleGenerativeAi => "google-generative-ai",
                Family::GoogleVertex => "google-vertex",
                Family::GoogleGeminiCli => "google-gemini-cli",
                Family::GoogleAntigravity => "google-antigravity",
                Family::Mistral => "mistral",
                Family::Ollama => "ollama",
                Family::PiMessages => "radius",
            };
            format!("{provider}/{model}")
        }
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PickerAction {
    Command(Command),
    ToggleDiagrams,
    SaveDiagrams,
    SetDiagrams {
        enabled: bool,
        save: dal_core::command::Save,
    },
    /// Open the remove-all confirmation.
    ConfirmLogoutAll,
    /// One scoped-model row: carries the `{provider}/{id}` the scope gate
    /// matches. Entering the row alone scopes that one model; in a
    /// multi-select the checked rows win instead.
    ScopeModel(Box<str>),
    /// Load the row's stored text into the composer as the draft.
    SetDraft(Box<str>),
    /// Resume the stored session. Handled by the runtime's resume-target path
    /// through public host ownership, never by guessing.
    ResumeSession(ResumeTarget),
    /// Activate nothing: the failed model picker keeps its instruction row
    /// selectable without running a command or dismissing the failure state.
    Noop,
}

/// A stored session the terminal can reopen: its identity and the workspace
/// the host's listing reported for it, so the reopen resolves the session
/// where it lives, not in the workspace of the session being left.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResumeTarget {
    /// The session to reopen.
    pub(crate) id: dal_core::SessionId,
    /// The workspace the listing named for it.
    pub(crate) workspace: dal_core::Workspace,
}

#[derive(Clone, Debug)]
pub(crate) struct PickerOption {
    pub(crate) label: String,
    pub(crate) action: PickerAction,
}

#[derive(Clone, Debug)]
pub(crate) struct PickerUi {
    pub(crate) title: String,
    options: Vec<PickerOption>,
    visible: Vec<usize>,
    selected: usize,
    filter: String,
    diagram_enabled: Option<bool>,
    model_fallback: bool,
    /// The model list failed to load: the failure copy is the title, `r`
    /// retries through the real model source, and a typed id still runs
    /// `/model <id>`. Every other picker leaves this false.
    load_failed: bool,
    /// Space toggles rows instead of activating them; Enter submits the
    /// checked set. Option indices survive refiltering so narrowing the
    /// query never loses a check.
    multi: bool,
    checked: std::collections::BTreeSet<usize>,
    /// The resume row whose delete the user armed with a first `d`. It lives
    /// and dies with this picker: closing the picker, opening a new one, or
    /// any move or filter edit drops it, so a delete is only ever confirmed
    /// by a second `d` on the same row in the same, unchanged opening.
    armed_delete: Option<dal_core::SessionId>,
}

impl PickerUi {
    pub(crate) fn new(title: impl Into<String>, filter: &str, options: Vec<PickerOption>) -> Self {
        let mut picker = Self {
            title: title.into(),
            options,
            visible: Vec::new(),
            selected: 0,
            filter: filter.to_owned(),
            diagram_enabled: None,
            model_fallback: false,
            load_failed: false,
            multi: false,
            checked: std::collections::BTreeSet::new(),
            armed_delete: None,
        };
        picker.refilter();
        picker
    }

    /// The resume row whose delete is armed, if any.
    pub(crate) fn armed_delete(&self) -> Option<dal_core::SessionId> {
        self.armed_delete
    }

    /// Arms the delete of one resume row; the next `d` on that same row
    /// confirms it.
    pub(crate) fn arm_delete(&mut self, id: dal_core::SessionId) {
        self.armed_delete = Some(id);
    }

    /// Drops an armed delete: the confirmation is spent or abandoned.
    pub(crate) fn disarm_delete(&mut self) {
        self.armed_delete = None;
    }

    /// Removes the resume row of one session and drops any armed delete. Used
    /// after a delete whose fresh listing could not be read, so the removed
    /// session is never offered again while the rest of the picker stays.
    pub(crate) fn remove_resume_row(&mut self, id: dal_core::SessionId) {
        self.options.retain(|option| {
            !matches!(&option.action, PickerAction::ResumeSession(target) if target.id == id)
        });
        self.armed_delete = None;
        self.refilter();
    }

    /// Marks the picker multi-select: space toggles the focused row, Enter
    /// submits every checked row, and `checked` seeds the initial set.
    pub(crate) fn with_multi(&mut self, checked: impl IntoIterator<Item = usize>) {
        self.multi = true;
        self.checked = checked.into_iter().collect();
    }

    /// Whether space toggles and Enter submits the checked set.
    pub(crate) fn is_multi(&self) -> bool {
        self.multi
    }

    /// Whether the row at `index` in the visible list is checked. Only a
    /// multi picker reports checks.
    pub(crate) fn is_checked(&self, index: usize) -> bool {
        self.multi
            && self
                .visible
                .get(index)
                .is_some_and(|option| self.checked.contains(option))
    }

    /// Toggles the focused row's check; ignored outside a multi picker.
    /// Returns the row's new state.
    pub(crate) fn toggle_selected(&mut self) -> Option<bool> {
        if !self.multi {
            return None;
        }
        let index = *self.visible.get(self.selected)?;
        Some(if self.checked.insert(index) {
            true
        } else {
            self.checked.remove(&index);
            false
        })
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
        self.armed_delete = None;
        let count = self.visible_len();
        if count == 0 {
            return;
        }
        self.selected = if down {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
    }

    pub(crate) fn add_filter_char(&mut self, character: char) {
        self.armed_delete = None;
        self.filter.push(character);
        self.refilter();
    }

    pub(crate) fn remove_filter_char(&mut self) {
        self.armed_delete = None;
        crate::composer::pop_grapheme(&mut self.filter);
        self.refilter();
    }

    /// Pasted text joins the query like typed characters: the one-line
    /// filter drops every control character (same normalization as the
    /// sign-in input), refiltering once, and never activates a row or
    /// submits.
    pub(crate) fn paste(&mut self, text: &str) {
        self.armed_delete = None;
        self.filter.extend(text.chars().filter(|c| !c.is_control()));
        self.refilter();
    }

    pub(crate) fn activate_selected(&mut self) -> Option<PickerAction> {
        if let Some(id) = self.model_filter_fallback() {
            return Some(PickerAction::Command(Command::Run {
                name: "model".into(),
                args: id.into(),
                expected: None,
            }));
        }
        if self.multi {
            // The checked rows are the whole answer. A committed id the model
            // list no longer names is a row of its own (see
            // `scoped_models_picker`), so unchecking it removes it and an
            // untouched one is submitted unchanged. Nothing checked clears.
            let mut seen = std::collections::BTreeSet::new();
            let mut ids: Vec<Box<str>> = Vec::new();
            for id in self
                .checked
                .iter()
                .filter_map(|index| self.options.get(*index))
                .filter_map(|option| match &option.action {
                    PickerAction::ScopeModel(id) => Some(id.clone()),
                    _ => None,
                })
            {
                if seen.insert(id.to_string()) {
                    ids.push(id);
                }
            }
            ids.sort();
            return Some(PickerAction::Command(Command::SetScopedModels(ids)));
        }
        let index = *self.visible.get(self.selected)?;
        let action = self.options.get(index)?.action.clone();
        match action {
            PickerAction::ScopeModel(id) => {
                Some(PickerAction::Command(Command::SetScopedModels(vec![id])))
            }
            PickerAction::ToggleDiagrams => {
                let enabled = self.diagram_enabled.as_mut()?;
                *enabled = !*enabled;
                let enabled = *enabled;
                self.options[index].label = settings_diagram_label(enabled);
                Some(PickerAction::SetDiagrams {
                    enabled,
                    save: dal_core::command::Save::SessionOnly,
                })
            }
            PickerAction::SaveDiagrams => Some(PickerAction::SetDiagrams {
                enabled: self.diagram_enabled?,
                save: dal_core::command::Save::SessionAndDefault,
            }),
            action => Some(action),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.visible_len() == 0
    }

    pub(crate) fn selected_fallback_index(&self) -> usize {
        self.visible_len().saturating_sub(1)
    }

    pub(crate) fn model_filter_fallback(&self) -> Option<&str> {
        (self.model_fallback && self.visible.is_empty() && !self.filter.trim().is_empty())
            .then_some(self.filter.trim())
    }
    pub(crate) fn is_settings(&self) -> bool {
        self.diagram_enabled.is_some()
    }

    /// Whether the model list failed to load and this picker is the
    /// failure surface (`r` retries, a typed id still runs `/model <id>`).
    pub(crate) fn is_load_failed(&self) -> bool {
        self.load_failed
    }

    /// The picker query/filter, kept intact across a retry.
    pub(crate) fn filter(&self) -> &str {
        &self.filter
    }

    /// Shows the fetching state while a retry runs through the model source.
    pub(crate) fn begin_retry(&mut self) {
        ids::MODEL_PICKER_FETCHING.clone_into(&mut self.title);
    }

    /// Restores the failure copy when a retry fails again.
    pub(crate) fn fail_retry(&mut self) {
        ids::MODEL_PICKER_FAIL.clone_into(&mut self.title);
    }

    pub(crate) fn is_selected(&self, index: usize) -> bool {
        self.selected == index
    }

    /// Focused visible position, for resume-picker delete needs.
    pub(crate) fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether this picker holds resume rows (delete armed only here).
    pub(crate) fn is_resume_picker(&self) -> bool {
        self.options
            .iter()
            .any(|option| matches!(option.action, PickerAction::ResumeSession(_)))
    }

    pub(crate) fn visible_len(&self) -> usize {
        self.visible.len() + usize::from(self.model_filter_fallback().is_some())
    }

    pub(crate) fn visible_option(&self, index: usize) -> Option<&PickerOption> {
        self.visible
            .get(index)
            .and_then(|option| self.options.get(*option))
    }

    pub(crate) fn visible_range(&self) -> std::ops::Range<usize> {
        let count = self.visible_len().min(6);
        let start = self
            .selected
            .saturating_sub(count / 2)
            .min(self.visible_len().saturating_sub(count));
        start..start + count
    }

    fn refilter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible.clear();
        self.visible.extend(
            self.options
                .iter()
                .enumerate()
                .filter(|(_, option)| {
                    needle.is_empty()
                        || (!matches!(&option.action, PickerAction::Noop)
                            && option.label.to_lowercase().contains(&needle))
                })
                .map(|(index, _)| index),
        );
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }
}

pub(crate) fn tree_picker(view: &View, filter: &str) -> PickerUi {
    let current_leaf = view.entries.items.last().map(|entry| entry.id);
    let options = view
        .tree
        .branches
        .iter()
        .filter(|branch| Some(branch.leaf) != current_leaf)
        .map(|branch| {
            let label = match branch.label.as_deref().filter(|label| !label.is_empty()) {
                Some(label) => format!("{label} · {}", branch.preview),
                None => branch.preview.to_string(),
            };
            PickerOption {
                label,
                action: PickerAction::Command(Command::MoveLeaf(branch.leaf)),
            }
        })
        .collect();
    PickerUi::new(ids::TREE_TITLE, filter, options)
}

pub(crate) fn fork_picker(entries: &[EntryView], filter: &str) -> PickerUi {
    let options = entries
        .iter()
        .rev()
        .filter_map(|entry| {
            let EntryKind::User { parts } = &entry.kind else {
                return None;
            };
            let preview = parts
                .iter()
                .map(|part| match part {
                    JournalPart::Text { text } => text.as_ref(),
                    JournalPart::TextBlob { .. } => "stored text",
                    JournalPart::Image { .. } | JournalPart::ImageBlob { .. } => "image",
                    JournalPart::Blob { .. } => "attachment",
                })
                .next()
                .unwrap_or("user message");
            let preview = preview.lines().next().unwrap_or("");
            Some(PickerOption {
                label: format!(
                    "{} · {}",
                    entry.id,
                    crate::width::take_cells(preview, 56, crate::width::WidthMode::Narrow)
                ),
                action: PickerAction::Command(Command::Fork(entry.id)),
            })
        })
        .collect();
    PickerUi::new(ids::FORK_PICKER_TITLE, filter, options)
}

fn settings_diagram_label(enabled: bool) -> String {
    crate::copy::render(
        ids::SETTINGS_DIAGRAMS,
        &[("value", if enabled { "on" } else { "off" })],
        1,
    )
}

pub(crate) fn settings_picker(diagrams: bool, filter: &str) -> PickerUi {
    let mut picker = PickerUi::new(
        ids::SETTINGS_TITLE,
        filter,
        vec![
            PickerOption {
                label: settings_diagram_label(diagrams),
                action: PickerAction::ToggleDiagrams,
            },
            PickerOption {
                label: ids::SETTINGS_DIAGRAMS_SAVE.to_owned(),
                action: PickerAction::SaveDiagrams,
            },
        ],
    );
    picker.diagram_enabled = Some(diagrams);
    picker
}

pub(crate) fn model_picker(models: &[ModelOption], filter: &str) -> PickerUi {
    let mut options = models
        .iter()
        .map(|model| PickerOption {
            label: model.label.clone(),
            action: PickerAction::Command(model.command.clone()),
        })
        .collect::<Vec<_>>();
    options.sort_by(|left, right| left.label.as_bytes().cmp(right.label.as_bytes()));
    let count = u64::try_from(models.len()).unwrap_or_default();
    let mut picker = PickerUi::new(model_title(count, "all providers"), filter, options);
    picker.model_fallback = true;
    picker
}

/// One scoped-model row: `id` is the `{provider}/{id}` the session's scope
/// gate matches against resolved routes.
pub(crate) struct ScopedModelRow {
    /// Scoped id in `{provider}/{id}` form.
    pub(crate) id: Box<str>,
    /// The rendered row label.
    pub(crate) label: String,
}

/// The `/scoped-models` picker: a multi-select over the model catalog
/// whose checked rows submit as `SetScopedModels`. Every committed id in
/// `active` arrives checked. One the catalog has no row for stays on screen
/// as a selectable row of its own, after the catalog rows, labelled as not in
/// the current model list: it keeps its scope until the user unchecks it, and
/// unchecking every row submits the empty list that clears the scope.
pub(crate) fn scoped_models_picker(
    rows: Vec<ScopedModelRow>,
    active: &[Box<str>],
    filter: &str,
) -> PickerUi {
    let mut checked = Vec::new();
    let mut options = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            if active.iter().any(|id| **id == *row.id) {
                checked.push(index);
            }
            PickerOption {
                label: row.label,
                action: PickerAction::ScopeModel(row.id),
            }
        })
        .collect::<Vec<_>>();
    let mut unavailable: Vec<&str> = Vec::new();
    for id in active {
        let id: &str = id;
        let listed = options.iter().any(|option| match &option.action {
            PickerAction::ScopeModel(row) => &**row == id,
            _ => false,
        });
        if !listed && !unavailable.contains(&id) {
            unavailable.push(id);
        }
    }
    for id in unavailable {
        checked.push(options.len());
        options.push(PickerOption {
            label: crate::copy::render(ids::SCOPED_UNAVAILABLE_ROW, &[("id", id)], 1),
            action: PickerAction::ScopeModel(id.into()),
        });
    }
    let mut picker = PickerUi::new(ids::SCOPED_MODELS_TITLE, filter, options);
    picker.with_multi(checked);
    picker
}

/// One resume row: `target` resumes through public host ownership,
/// `label` is the rendered `RESUME_ROW` text.
pub(crate) struct SessionRow {
    /// The session to resume and the workspace it lives in.
    pub(crate) target: ResumeTarget,
    /// Rendered row label.
    pub(crate) label: String,
}

/// The `/resume` picker: every session is a row whose activation resumes
/// that session through the runtime resume-target path. Filter narrows by
/// label; `d` on this picker deletes with explicit confirmation.
pub(crate) fn resume_picker(rows: Vec<SessionRow>, filter: &str) -> PickerUi {
    let options = rows
        .into_iter()
        .map(|row| PickerOption {
            label: row.label,
            action: PickerAction::ResumeSession(row.target),
        })
        .collect();
    PickerUi::new(ids::RESUME_TITLE, filter, options)
}

/// Returns the focused resume-picker session, when the picker holds a
/// resume row under the cursor.
pub(crate) fn focused_resume_session(picker: &PickerUi) -> Option<dal_core::SessionId> {
    let option = picker.visible_option(picker.selected_index())?;
    match &option.action {
        PickerAction::ResumeSession(target) => Some(target.id),
        _ => None,
    }
}

/// The prompt-history picker: every past user message is a row whose
/// activation loads the message text into the composer as the draft.
pub(crate) fn history_picker(entries: &[EntryView], filter: &str) -> PickerUi {
    let options = entries
        .iter()
        .rev()
        .filter_map(|entry| {
            let EntryKind::User { parts } = &entry.kind else {
                return None;
            };
            let text = parts
                .iter()
                .filter_map(|part| match part {
                    JournalPart::Text { text } => Some(text.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            let preview = text.lines().next().unwrap_or("");
            Some(PickerOption {
                label: crate::width::take_cells(preview, 56, crate::width::WidthMode::Narrow)
                    .to_string(),
                action: PickerAction::SetDraft(text.into()),
            })
        })
        .collect();
    PickerUi::new(ids::HISTORY_PICKER_TITLE, filter, options)
}

/// The model picker's failure surface: the failure copy is the title, the
/// typed-id fallback is the one selectable row, and `r` retries the real
/// model source while the query/filter stays intact.
pub(crate) fn model_picker_failed(filter: &str) -> PickerUi {
    let mut picker = PickerUi::new(
        ids::MODEL_PICKER_FAIL,
        filter,
        vec![PickerOption {
            label: ids::MODEL_PICKER_FALLBACK.to_owned(),
            action: PickerAction::Noop,
        }],
    );
    picker.model_fallback = true;
    picker.load_failed = true;
    picker
}

/// Renders one resume row. `messages` is the count the host's listing
/// reported; `None` means the host could not read the journal, which the row
/// says outright instead of showing a count nobody measured.
#[must_use]
pub fn resume_row(name: &str, when: &str, messages: Option<u64>, dir: &str) -> String {
    let Some(messages) = messages else {
        return crate::copy::render(
            ids::RESUME_ROW_UNREADABLE,
            &[("name", name), ("when", when), ("dir", dir)],
            1,
        );
    };
    crate::copy::render(
        ids::RESUME_ROW,
        &[
            ("name", name),
            ("when", when),
            ("n", &messages.to_string()),
            ("dir", dir),
        ],
        messages,
    )
}

/// Tracks the once-per-session Luna Reserve offer notice.
#[derive(Debug, Default)]
pub struct LunaOffer {
    shown: bool,
}

impl LunaOffer {
    /// Returns the offer notice once when the reserve banner is present.
    pub fn notice(&mut self, banner: Option<&str>) -> Option<&'static str> {
        if banner == Some("luna_reserve") && !self.shown {
            self.shown = true;
            Some(ids::LUNA_OFFER)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LunaOffer, filter_models, model_title, resume_row};

    #[test]
    fn model_filter_matches_case_insensitively_in_order() {
        let models = vec!["gpt-reserve".to_owned(), "opus-4".to_owned()];
        assert_eq!(filter_models(&models, "GPT"), ["gpt-reserve"]);
        assert!(model_title(2, "acme").contains("acme"));
    }

    #[test]
    fn luna_offer_shows_once_and_sends_no_command() {
        let mut offer = LunaOffer::default();
        assert_eq!(
            offer.notice(Some("luna_reserve")),
            Some(
                "note: usage is blocked. Luna Reserve is available: type /model gpt-reserve to switch."
            )
        );
        assert_eq!(offer.notice(Some("luna_reserve")), None);
        assert_eq!(offer.notice(None), None);
        assert!(resume_row("fix", "today", Some(1), "/w").contains("/w"));
    }

    /// A resume row prints the count the host's listing measured, and a row
    /// whose journal could not be read says so instead of showing a zero.
    #[test]
    fn resume_rows_show_the_listed_count_or_say_it_is_unreadable() {
        assert_eq!(
            resume_row("fix", "today", Some(3), "/w"),
            "fix · today · 3 messages · /w"
        );
        assert_eq!(
            resume_row("fix", "today", Some(1), "/w"),
            "fix · today · 1 message · /w"
        );
        assert_eq!(
            resume_row("fix", "today", Some(0), "/w"),
            "fix · today · 0 messages · /w",
            "a measured zero is a real zero"
        );
        let unreadable = resume_row("fix", "today", None, "/w");
        assert_eq!(unreadable, "fix · today · messages unreadable · /w");
        assert!(!unreadable.contains("0 message"));
    }

    /// A committed scope id the model list does not name stays selected as a
    /// visible row once, survives an untouched submit, and can be unchecked:
    /// unchecking every row submits the empty list that clears the scope.
    #[test]
    fn a_committed_scope_id_missing_from_the_catalog_is_a_removable_row() {
        use dal_core::Command;

        use super::PickerAction;

        let rows = vec![super::ScopedModelRow {
            id: "openai/gpt-6".into(),
            label: "GPT 6".into(),
        }];
        let active: Vec<Box<str>> = vec![
            "openai/gpt-6".into(),
            "anthropic/vanished".into(),
            "anthropic/vanished".into(),
        ];
        let mut picker = super::scoped_models_picker(rows, &active, "");
        assert_eq!(picker.visible_len(), 2, "the missing id is one row, once");
        assert!(picker.is_checked(0) && picker.is_checked(1));
        assert!(picker.visible_option(1).is_some_and(|option| {
            option.label.contains("not in the current model list")
                && option.label.contains("anthropic/vanished")
        }));
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::SetScopedModels(ids)))
                if ids.iter().map(|id| id.as_ref()).eq(["anthropic/vanished", "openai/gpt-6"])
        ));
        // Uncheck the listed row only: the missing id is still committed.
        assert_eq!(picker.toggle_selected(), Some(false));
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::SetScopedModels(ids)))
                if ids.iter().map(|id| id.as_ref()).eq(["anthropic/vanished"])
        ));
        // Uncheck the missing row too: the scope clears.
        picker.move_selection(true);
        assert_eq!(picker.toggle_selected(), Some(false));
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::SetScopedModels(ids))) if ids.is_empty()
        ));
    }

    #[test]
    fn failed_model_picker_accepts_an_id_matching_its_hint() {
        use dal_core::Command;

        use super::PickerAction;

        let mut picker = super::model_picker_failed("");
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Noop)
        ));
        picker.add_filter_char('i');
        picker.add_filter_char('d');
        let activated = picker.activate_selected();
        assert!(
            matches!(&activated, Some(PickerAction::Command(Command::Run { name, args, .. })) if name.as_ref() == "model" && args.as_ref() == "id"),
            "typed id must run /model, got {activated:?}"
        );
    }

    #[test]
    fn failed_model_picker_retry_titles_cycle_and_keep_the_filter() {
        let mut picker = super::model_picker_failed("gpt");
        picker.begin_retry();
        assert_eq!(picker.title, super::ids::MODEL_PICKER_FETCHING);
        picker.fail_retry();
        assert_eq!(picker.title, super::ids::MODEL_PICKER_FAIL);
        assert_eq!(picker.filter(), "gpt");
        assert!(picker.is_load_failed());
    }
}

#[cfg(test)]
mod picker_tests {
    use std::num::NonZeroU64;

    use super::{PickerAction, PickerOption, PickerUi};
    use dal_core::Command;

    #[test]
    fn typed_filter_updates_visible_choices_and_selection() {
        let mut picker = PickerUi::new(
            "Pick",
            "",
            vec![
                PickerOption {
                    label: "alpha".to_owned(),
                    action: PickerAction::Command(Command::Run {
                        name: "model".into(),
                        args: "openai/alpha".into(),
                        expected: None,
                    }),
                },
                PickerOption {
                    label: "beta".to_owned(),
                    action: PickerAction::Command(Command::Run {
                        name: "model".into(),
                        args: "openai/beta".into(),
                        expected: None,
                    }),
                },
            ],
        );
        picker.add_filter_char('b');
        assert_eq!(picker.visible_len(), 1);
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::Run { args, .. })) if args.as_ref() == "openai/beta"
        ));
        picker.remove_filter_char();
        assert_eq!(picker.visible_len(), 2);
    }

    #[test]
    fn fork_picker_uses_user_messages_and_preserves_entry_identity() {
        let target = dal_core::EntryId::new(NonZeroU64::MIN);
        let entries = vec![
            dal_core::EntryView {
                id: target,
                parent: None,
                kind: dal_core::EntryKind::User {
                    parts: vec![dal_core::JournalPart::Text {
                        text: "fix the parser".into(),
                    }],
                },
            },
            dal_core::EntryView {
                id: dal_core::EntryId::new(NonZeroU64::new(2).unwrap_or(NonZeroU64::MIN)),
                parent: None,
                kind: dal_core::EntryKind::Model {
                    route: dal_core::ModelRoute::Harness {
                        id: "dalgon/normal".into(),
                    },
                },
            },
        ];
        let mut picker = super::fork_picker(&entries, "parser");
        assert_eq!(picker.visible_len(), 1);
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::Fork(id))) if id == target
        ));
    }

    #[test]
    fn host_model_choices_preserve_the_exact_api_family_and_provider() {
        let row = |provider: &str| dal_core::ModelInfo {
            route: dal_core::ModelRoute::Api {
                family: dal_core::Family::Chat,
                model: "gpt-4.1".into(),
                provider: Some(provider.into()),
            },
            name: "GPT 4.1".into(),
            caps: dal_core::Caps {
                context_window: Some(128_000),
                thinking: Vec::new().into_boxed_slice(),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            provider: provider.into(),
        };
        let options = super::model_options(vec![row("openai"), row("acme-proxy")]);
        assert!(options[0].label.contains("openai-chat/gpt-4.1"));
        assert_eq!(options[0].scoped_id.as_deref(), Some("openai/gpt-4.1"));
        assert_eq!(options[1].scoped_id.as_deref(), Some("acme-proxy/gpt-4.1"));
        // Two same-family providers list one native id: each row submits the
        // route that names its own provider, never one shared route.
        for (option, expected) in options.iter().zip(["openai", "acme-proxy"]) {
            assert!(matches!(
                &option.command,
                Command::SetModel {
                    model: dal_core::ModelRoute::Api {
                        family: dal_core::Family::Chat,
                        provider: Some(provider),
                        ..
                    },
                    save: dal_core::command::Save::SessionAndDefault,
                } if provider.as_ref() == expected
            ));
        }
    }

    #[test]
    fn pasted_text_filters_like_typed_text_and_clamps_selection() {
        let mut picker = PickerUi::new(
            "Pick",
            "",
            vec![
                PickerOption {
                    label: "alpha".to_owned(),
                    action: PickerAction::Command(Command::Run {
                        name: "model".into(),
                        args: "openai/alpha".into(),
                        expected: None,
                    }),
                },
                PickerOption {
                    label: "beta".to_owned(),
                    action: PickerAction::Command(Command::Run {
                        name: "model".into(),
                        args: "openai/beta".into(),
                        expected: None,
                    }),
                },
            ],
        );
        picker.move_selection(true);
        picker.paste("beta");
        assert_eq!(picker.filter(), "beta");
        assert_eq!(picker.visible_len(), 1);
        assert_eq!(picker.selected_fallback_index(), 0);
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::Run { args, .. })) if args.as_ref() == "openai/beta"
        ));
    }

    #[test]
    fn pasted_id_reaches_the_failed_model_picker_fallback_without_newlines() {
        let mut picker = super::model_picker_failed("");
        picker.paste("acme/gpt-6\r\nignored-second-line");
        assert_eq!(picker.filter(), "acme/gpt-6ignored-second-line");
        assert_eq!(picker.visible_len(), 1);
        assert!(picker.is_selected(picker.selected_fallback_index()));
        let activated = picker.activate_selected();
        assert!(
            matches!(&activated, Some(PickerAction::Command(Command::Run { name, args, .. })) if name.as_ref() == "model" && args.as_ref() == "acme/gpt-6ignored-second-line"),
            "pasted id must run /model, got {activated:?}"
        );
    }

    #[test]
    fn settings_toggle_is_session_only_and_save_uses_default_semantics() {
        let mut picker = super::settings_picker(false, "");
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::SetDiagrams {
                enabled: true,
                save: dal_core::command::Save::SessionOnly,
            })
        ));
        assert!(
            picker
                .visible_option(0)
                .is_some_and(|option| option.label.contains("on"))
        );
        picker.move_selection(true);
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::SetDiagrams {
                enabled: true,
                save: dal_core::command::Save::SessionAndDefault,
            })
        ));
    }

    #[test]
    fn navigation_wraps_and_selection_returns_the_typed_command() {
        let mut picker = PickerUi::new(
            "Pick",
            "",
            vec![
                PickerOption {
                    label: "first".to_owned(),
                    action: PickerAction::Command(Command::MoveLeaf(dal_core::EntryId::new(
                        NonZeroU64::MIN,
                    ))),
                },
                PickerOption {
                    label: "second".to_owned(),
                    action: PickerAction::Command(Command::Run {
                        name: "model".into(),
                        args: "openai/gpt-6".into(),
                        expected: None,
                    }),
                },
            ],
        );
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::MoveLeaf(_)))
        ));
        picker.move_selection(false);
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::Run { .. }))
        ));
        picker.move_selection(true);
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::MoveLeaf(_)))
        ));
    }

    /// A multi picker toggles the focused row on space and submits every
    /// checked row's scope id on enter; an empty set clears the scope.
    #[test]
    fn a_multi_picker_submits_the_checked_scope() {
        let rows = vec![
            super::ScopedModelRow {
                id: "openai/gpt-6".into(),
                label: "GPT 6".into(),
            },
            super::ScopedModelRow {
                id: "anthropic/opus-4".into(),
                label: "Opus 4".into(),
            },
        ];
        let mut picker = super::scoped_models_picker(rows, &["anthropic/opus-4".into()], "");
        assert!(picker.is_multi());
        assert!(!picker.is_checked(0));
        assert!(picker.is_checked(1));
        // Uncheck the second row and check the first: enter submits only
        // the checked ids, whatever the focus is.
        picker.move_selection(true);
        assert_eq!(picker.toggle_selected(), Some(false));
        picker.move_selection(false);
        assert_eq!(picker.toggle_selected(), Some(true));
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::SetScopedModels(ids)))
                if ids.iter().map(|id| id.as_ref()).eq(["openai/gpt-6"])
        ));
        // Checking nothing submits the empty scope, which clears it.
        picker.toggle_selected();
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::SetScopedModels(ids))) if ids.is_empty()
        ));
    }

    /// Filter narrowing cannot lose a check: checked option indices
    /// survive refiltering and still submit.
    #[test]
    fn checks_survive_a_narrowing_filter() {
        let rows = vec![
            super::ScopedModelRow {
                id: "openai/gpt-6".into(),
                label: "GPT 6".into(),
            },
            super::ScopedModelRow {
                id: "anthropic/opus-4".into(),
                label: "Opus 4".into(),
            },
        ];
        let mut picker = super::scoped_models_picker(rows, &[], "");
        picker.toggle_selected();
        picker.add_filter_char('o');
        assert_eq!(picker.visible_len(), 1);
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::Command(Command::SetScopedModels(ids)))
                if ids.iter().map(|id| id.as_ref()).eq(["openai/gpt-6"])
        ));
    }

    /// Space on a single picker is a filter character, not a toggle: the
    /// checked contract stays off until `with_multi` marks the picker.
    #[test]
    fn a_single_picker_has_no_toggle_state() {
        let mut picker = PickerUi::new(
            "Pick",
            "",
            vec![PickerOption {
                label: "alpha".to_owned(),
                action: PickerAction::Noop,
            }],
        );
        assert!(!picker.is_multi());
        assert_eq!(picker.toggle_selected(), None);
        assert!(!picker.is_checked(0));
    }

    /// The history picker loads a past user message as the draft text and
    /// ignores non-user and textless entries.
    #[test]
    fn history_picker_loads_the_message_as_a_draft() {
        let entries = vec![
            dal_core::EntryView {
                id: dal_core::EntryId::new(NonZeroU64::MIN),
                parent: None,
                kind: dal_core::EntryKind::User {
                    parts: vec![
                        dal_core::JournalPart::Text {
                            text: "first\nline".into(),
                        },
                        dal_core::JournalPart::Text {
                            text: "second".into(),
                        },
                    ],
                },
            },
            dal_core::EntryView {
                id: dal_core::EntryId::new(NonZeroU64::new(2).unwrap_or(NonZeroU64::MIN)),
                parent: None,
                kind: dal_core::EntryKind::Model {
                    route: dal_core::ModelRoute::Harness {
                        id: "dalgon/normal".into(),
                    },
                },
            },
        ];
        let mut picker = super::history_picker(&entries, "");
        assert_eq!(picker.visible_len(), 1);
        assert!(matches!(
            picker.activate_selected(),
            Some(PickerAction::SetDraft(text)) if text.as_ref() == "first\nline\nsecond"
        ));
    }

    /// Paste drops control characters beyond newlines — an embedded escape
    /// or NUL can never smuggle a filter state.
    #[test]
    fn paste_drops_every_control_character() {
        let mut picker = PickerUi::new(
            "Pick",
            "",
            vec![PickerOption {
                label: "abc".to_owned(),
                action: PickerAction::Noop,
            }],
        );
        picker.paste("a\u{1b}\u{0}bc\t");
        assert_eq!(picker.filter(), "abc");
    }
}
