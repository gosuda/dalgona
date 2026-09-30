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
}

/// Converts host model rows into typed route selections.
#[must_use]
pub fn model_options(models: Vec<ModelInfo>) -> Vec<ModelOption> {
    models
        .into_iter()
        .map(|model| {
            let label = format!("{} · {}", model.name, route_label(&model.route));
            ModelOption {
                label,
                command: Command::SetModel {
                    model: model.route,
                    save: dal_core::command::Save::SessionAndDefault,
                },
            }
        })
        .collect()
}

fn route_label(route: &ModelRoute) -> String {
    match route {
        ModelRoute::Api { family, model } => {
            let provider = match family {
                Family::Chat => "openai-chat",
                Family::Responses => "openai-responses",
                Family::Codex => "openai-codex",
                Family::Anthropic => "anthropic",
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
        };
        picker.refilter();
        picker
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
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
        self.filter.push(character);
        self.refilter();
    }

    pub(crate) fn remove_filter_char(&mut self) {
        self.filter.pop();
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
        let index = *self.visible.get(self.selected)?;
        let action = self.options.get(index)?.action.clone();
        match action {
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

    pub(crate) fn is_selected(&self, index: usize) -> bool {
        self.selected == index
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
                    needle.is_empty() || option.label.to_lowercase().contains(&needle)
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
                action: PickerAction::Command(Command::MoveLeaf(branch.leaf.clone())),
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
                .find_map(|part| match part {
                    JournalPart::Text { text } => Some(text.as_ref()),
                    JournalPart::TextBlob { .. } => Some("stored text"),
                    JournalPart::Image { .. } | JournalPart::ImageBlob { .. } => Some("image"),
                    JournalPart::Blob { .. } => Some("attachment"),
                })
                .unwrap_or("user message");
            let preview = preview.lines().next().unwrap_or("");
            Some(PickerOption {
                label: format!(
                    "{} · {}",
                    entry.id,
                    crate::width::take_cells(preview, 56, crate::width::WidthMode::Narrow)
                ),
                action: PickerAction::Command(Command::Fork(entry.id.clone())),
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

/// Renders one resume row.
#[must_use]
pub fn resume_row(name: &str, when: &str, messages: u64, dir: &str) -> String {
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
        assert_eq!(model_title(2, "acme").contains("acme"), true);
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
        assert_eq!(resume_row("fix", "today", 1, "/w").contains("/w"), true);
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
                id: target.clone(),
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
    fn host_model_choices_preserve_the_exact_api_family() {
        let options = super::model_options(vec![dal_core::ModelInfo {
            route: dal_core::ModelRoute::Api {
                family: dal_core::Family::Chat,
                model: "gpt-4.1".into(),
            },
            name: "GPT 4.1".into(),
            caps: dal_core::Caps {
                context_window: Some(128_000),
                thinking: Vec::new().into_boxed_slice(),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
        }]);
        assert!(options[0].label.contains("openai-chat/gpt-4.1"));
        assert!(matches!(
            &options[0].command,
            Command::SetModel {
                model: dal_core::ModelRoute::Api {
                    family: dal_core::Family::Chat,
                    ..
                },
                save: dal_core::command::Save::SessionAndDefault,
            }
        ));
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
}
