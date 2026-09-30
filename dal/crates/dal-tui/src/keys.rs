//! One key-binding table for both terminal modes and their legacy fallbacks.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::TuiError;

/// Context that owns a binding when multiple contexts match the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    /// An approval or question dialog.
    Dialog,
    /// A picker or navigator.
    Picker,
    /// Global application actions.
    App,
    /// Composer editing and submission.
    Composer,
    /// Text cursor movement and editing.
    Editor,
}

/// Semantic action resolved from the key table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Close the active overlay, or interrupt a running turn.
    CloseOverlayOrInterrupt,
    /// Copy the active selection.
    CopySelection,
    /// Interrupt the active turn.
    Interrupt,
    /// Quit when the composer is empty.
    QuitEmptyComposer,
    /// Expand or collapse the newest expandable card.
    ExpandCollapse,
    /// Open the read-only transcript overlay.
    TranscriptOverlay,
    /// Search the transcript.
    SearchTranscript,
    /// Recall the previous prompt.
    PrevPrompt,
    /// Recall the next prompt.
    NextPrompt,
    /// Open the external editor.
    ExternalEditor,
    /// Open keyboard help.
    Help,
    /// Clear the live region.
    ClearRegion,
    /// Suspend the process on POSIX.
    Suspend,
    /// Submit the composer.
    Submit,
    /// Insert a newline into the composer.
    Newline,
    /// Accept or cycle completion.
    AcceptCycleCompletion,
    /// Recall the previous history entry.
    HistoryPrev,
    /// Recall the next history entry.
    HistoryNext,
    /// Search prompt history.
    HistorySearch,
    /// Move the editor cursor left.
    CharLeft,
    /// Move the editor cursor right.
    CharRight,
    /// Move the cursor one word left.
    WordLeft,
    /// Move the cursor one word right.
    WordRight,
    /// Move to the start of the line.
    LineStart,
    /// Move to the end of the line.
    LineEnd,
    /// Delete the previous word.
    DeleteWordBack,
    /// Delete the next word.
    DeleteWordForward,
    /// Delete to the start of the line.
    KillLineStart,
    /// Delete to the end of the line.
    KillLineEnd,
    /// Yank the deleted text.
    Yank,
    /// Undo the latest edit.
    Undo,
    /// Move the picker selection upward.
    PickerMoveUp,
    /// Move the picker selection downward.
    PickerMoveDown,
    /// Move one page upward in a picker.
    PickerPageUp,
    /// Move one page downward in a picker.
    PickerPageDown,
    /// Select the first picker row.
    PickerFirst,
    /// Select the last picker row.
    PickerLast,
    /// Toggle a multi-select picker row.
    PickerToggle,
    /// Accept the picker selection.
    PickerAccept,
    /// Cancel the picker.
    PickerCancel,
    /// Delete the selected resume row.
    PickerDelete,
    /// Move focus left in a dialog.
    DialogFocusLeft,
    /// Move focus right in a dialog.
    DialogFocusRight,
    /// Activate the focused dialog action.
    DialogActivate,
    /// Scroll the dialog body upward.
    DialogScrollUp,
    /// Scroll the dialog body downward.
    DialogScrollDown,
    /// Approve a call once.
    ApprovalAllowOnce,
    /// Approve a call for this session.
    ApprovalAllowSession,
    /// Deny an approval.
    ApprovalDeny,
    /// Expand the approval preview.
    ApprovalView,
    /// Move within ask choices.
    AskMoveUp,
    /// Move within ask choices.
    AskMoveDown,
    /// Toggle an ask choice.
    AskToggle,
    /// Submit an ask answer.
    AskSubmit,
    /// Dismiss an ask question.
    AskDismiss,
    /// Answer yes.
    AskYes,
    /// Answer no.
    AskNo,
    /// Retry the provider request now.
    RetryNow,
    /// Cancel the retrying turn.
    RetryCancel,
    /// Scroll the transcript upward.
    TranscriptPageUp,
    /// Scroll the transcript downward.
    TranscriptPageDown,
    /// Jump to the latest transcript entry.
    JumpLatest,
}

/// A normalized key code and modifier set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// Terminal key code.
    pub code: KeyCode,
    /// Active modifiers.
    pub modifiers: KeyModifiers,
}

impl Key {
    /// Constructs a normalized key value.
    #[must_use]
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

/// One binding and its fallback key for non-kitty terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Owning input context.
    pub owner: Owner,
    /// Semantic action.
    pub action: Action,
    /// User-facing action description.
    pub label: &'static str,
    /// Preferred key.
    pub default: Key,
    /// Legacy key, when one exists.
    pub legacy: Option<Key>,
}

mod bindings;
mod decoder;

pub use bindings::BINDINGS;
pub use decoder::{InputEvent, KeyDecoder};

/// Validates that application bindings reserve printable keys for text input.
///
/// # Errors
/// Returns the exact two-line startup error when a non-dialog, non-picker action
/// binds an unmodified printable character.
pub fn validate(table: &[Binding]) -> Result<(), TuiError> {
    for binding in table {
        let KeyCode::Char(character) = binding.default.code else {
            continue;
        };
        if binding.default.modifiers != KeyModifiers::NONE
            || character.is_control()
            || matches!(binding.owner, Owner::Dialog | Owner::Picker)
        {
            continue;
        }
        let context = match binding.owner {
            Owner::Dialog => "dialog",
            Owner::Picker => "picker",
            Owner::App => "app",
            Owner::Composer => "composer",
            Owner::Editor => "editor",
        };
        return Err(crate::term::te_printable_key(
            context,
            binding.action_name(),
        ));
    }
    Ok(())
}

impl Binding {
    fn action_name(self) -> &'static str {
        match self.action {
            Action::QuitEmptyComposer => "quit",
            Action::Submit => "submit",
            Action::Help => "help",
            Action::ApprovalAllowOnce => "approval_allow_once",
            Action::ApprovalAllowSession => "approval_allow_session",
            Action::ApprovalDeny => "approval_deny",
            _ => self.label,
        }
    }
}

/// Resolves a key to the first matching action in owner order.
#[must_use]
pub fn resolve(key: Key, kitty: bool) -> Option<Action> {
    resolve_in(
        key,
        kitty,
        &[
            Owner::Dialog,
            Owner::Picker,
            Owner::App,
            Owner::Composer,
            Owner::Editor,
        ],
    )
}

/// Resolves a key only in the currently active input contexts, in owner order.
#[must_use]
pub fn resolve_in(key: Key, kitty: bool, owners: &[Owner]) -> Option<Action> {
    owners.iter().find_map(|owner| {
        BINDINGS.iter().find_map(|binding| {
            if binding.owner != *owner {
                return None;
            }
            let matches = binding.default == key || (!kitty && binding.legacy == Some(key));
            matches.then_some(binding.action)
        })
    })
}

/// Returns the compact action labels used by F1 help.
#[must_use]
pub fn help_labels() -> Vec<(&'static str, &'static str)> {
    BINDINGS
        .iter()
        .filter(|binding| binding.owner == Owner::App)
        .map(|binding| (key_label(binding.default), binding.label))
        .collect()
}

fn key_label(key: Key) -> &'static str {
    match (key.code, key.modifiers) {
        (KeyCode::F(1), _) => "f1",
        (KeyCode::F(3), _) => "f3",
        (KeyCode::Esc, _) => "esc",
        (KeyCode::Enter, _) => "enter",
        (KeyCode::Tab, _) => "tab",
        (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => "ctrl+c",
        (KeyCode::Char('d'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => "ctrl+d",
        (KeyCode::Char('o'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => "ctrl+o",
        (KeyCode::Char('t'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => "ctrl+t",
        (KeyCode::Char('l'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => "ctrl+l",
        (KeyCode::Char('z'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => "ctrl+z",
        (KeyCode::Char('f'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            "ctrl+shift+f"
        }
        _ => "key",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Action, BINDINGS, Binding, InputEvent, Key, KeyDecoder, Owner, help_labels, resolve,
        validate,
    };
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::time::{Duration, Instant};

    #[test]
    fn f1_help_is_a_legacy_key_and_has_visible_labels() {
        let help = Key::new(KeyCode::F(1), KeyModifiers::NONE);
        assert_eq!(resolve(help, false), Some(Action::Help));
        assert!(
            help_labels()
                .iter()
                .any(|(key, label)| *key == "f1" && *label == "Help")
        );
    }

    #[test]
    fn printable_quit_binding_fails_before_terminal_setup() {
        let invalid = Binding {
            owner: Owner::App,
            action: Action::QuitEmptyComposer,
            default: Key::new(KeyCode::Char('q'), KeyModifiers::NONE),
            legacy: None,
            label: "Quit",
        };
        let error = validate(&[invalid]).err().map(|error| error.to_string());
        assert!(error.is_some_and(|text| text.contains("binding app.quit uses a printable key")));
    }

    #[test]
    fn default_table_resolves_dialog_before_app() {
        assert_eq!(
            resolve(Key::new(KeyCode::Esc, KeyModifiers::NONE), false),
            Some(Action::ApprovalDeny)
        );
        assert!(validate(BINDINGS).is_ok());
    }
    #[test]
    fn utf8_invalid_prefix_yields_one_replacement_and_replays_ascii() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert!(decoder.feed(&[0xe6, 0x97], start).is_empty());
        assert_eq!(
            decoder.feed(b"a", start + Duration::from_millis(1)),
            [
                InputEvent::Key(Key::new(KeyCode::Char('\u{fffd}'), KeyModifiers::NONE)),
                InputEvent::Key(Key::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            ]
        );
    }

    #[test]
    fn csi_keys_support_legacy_and_kitty_modifiers() {
        let mut decoder = KeyDecoder::default();
        let start = Instant::now();
        assert_eq!(
            decoder.feed(b"\x1b[A", start),
            [InputEvent::Key(Key::new(KeyCode::Up, KeyModifiers::NONE))]
        );
        assert_eq!(
            decoder.feed(b"\x1b[1;5A\x1b[13;2u", start),
            [
                InputEvent::Key(Key::new(KeyCode::Up, KeyModifiers::CONTROL)),
                InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            ]
        );
    }

    #[test]
    fn split_paste_start_and_bare_end_are_handled() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert!(decoder.feed(b"\x1b[20", start).is_empty());
        assert!(
            decoder
                .feed(b"0~hi", start + Duration::from_millis(5))
                .is_empty()
        );
        assert_eq!(
            decoder.feed(b"\x1b[201~", start + Duration::from_millis(6)),
            [InputEvent::Paste(b"hi".to_vec())]
        );
        assert!(
            decoder
                .feed(b"\x1b[201~", start + Duration::from_millis(7))
                .is_empty()
        );
    }

    #[test]
    fn lone_escape_waits_fifty_milliseconds() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert!(decoder.feed(b"\x1b", start).is_empty());
        assert_eq!(
            decoder.tick(start + Duration::from_millis(50)),
            [InputEvent::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE))]
        );
    }

    #[test]
    fn missing_paste_end_closes_at_two_seconds() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert!(decoder.feed(b"\x1b[200~hi", start).is_empty());
        assert_eq!(
            decoder.tick(start + Duration::from_secs(2)),
            [InputEvent::Paste(b"hi".to_vec())]
        );
    }
}
