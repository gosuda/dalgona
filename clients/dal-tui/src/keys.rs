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
    /// Transcript scrolling and jumping while a viewport owns them.
    Transcript,
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
    /// Extend the fullscreen transcript selection one row or grapheme left.
    SelectionLeft,
    /// Extend the fullscreen transcript selection one row or grapheme right.
    SelectionRight,
    /// Extend the fullscreen transcript selection one row up.
    SelectionUp,
    /// Extend the fullscreen transcript selection one row down.
    SelectionDown,
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
    /// Jump the transcript to the latest entry.
    TranscriptJumpLatest,
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
            Owner::Transcript => "transcript",
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
pub fn resolve_in(key: Key, _kitty: bool, owners: &[Owner]) -> Option<Action> {
    owners.iter().find_map(|owner| {
        BINDINGS.iter().find_map(|binding| {
            if binding.owner != *owner {
                return None;
            }
            let matches = binding.default == key || binding.legacy == Some(key);
            matches.then_some(binding.action)
        })
    })
}

/// Returns the compact action labels used by F1 help.
#[must_use]
pub fn help_labels() -> Vec<(String, &'static str)> {
    BINDINGS
        .iter()
        .filter(|binding| matches!(binding.owner, Owner::App | Owner::Transcript))
        .map(|binding| (key_label(binding.default), binding.label))
        .collect()
}

/// Names a key chord in lowercase, modifiers first: `ctrl+up`, `pgdn`, `f1`.
fn key_label(key: Key) -> String {
    let mut label = String::new();
    for (modifier, name) in [
        (KeyModifiers::CONTROL, "ctrl+"),
        (KeyModifiers::ALT, "alt+"),
        (KeyModifiers::SHIFT, "shift+"),
    ] {
        if key.modifiers.contains(modifier) {
            label.push_str(name);
        }
    }
    match key.code {
        KeyCode::Char(character) => label.extend(character.to_lowercase()),
        KeyCode::F(number) => {
            label.push('f');
            label.push_str(&number.to_string());
        }
        KeyCode::Esc => label.push_str("esc"),
        KeyCode::Enter => label.push_str("enter"),
        KeyCode::Tab => label.push_str("tab"),
        KeyCode::Backspace => label.push_str("backspace"),
        KeyCode::Up => label.push_str("up"),
        KeyCode::Down => label.push_str("down"),
        KeyCode::Left => label.push_str("left"),
        KeyCode::Right => label.push_str("right"),
        KeyCode::Home => label.push_str("home"),
        KeyCode::End => label.push_str("end"),
        KeyCode::PageUp => label.push_str("pgup"),
        KeyCode::PageDown => label.push_str("pgdn"),
        other => {
            let name = format!("{other:?}");
            label.push_str(&name.to_lowercase());
        }
    }
    label
}

#[cfg(test)]
mod tests {
    use super::{
        Action, BINDINGS, Binding, InputEvent, Key, KeyDecoder, Owner, help_labels, resolve,
        resolve_in, validate,
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
                .any(|(key, label)| key == "f1" && *label == "Help")
        );
    }

    #[test]
    fn help_names_every_app_key_without_a_placeholder() {
        let labels = help_labels();
        assert_ne!(labels.len(), 0);
        for (key, label) in &labels {
            assert!(
                key != "key" && !key.is_empty(),
                "{label} must show its real key, got {key:?}"
            );
        }
        let named = |wanted: &str| labels.iter().any(|(key, _)| key == wanted);
        for wanted in [
            "ctrl+up",
            "ctrl+down",
            "ctrl+g",
            "ctrl+r",
            "pgup",
            "pgdn",
            "ctrl+shift+f",
        ] {
            assert!(named(wanted), "{wanted} is missing from {labels:?}");
        }
    }

    #[test]
    fn end_is_a_composer_key_and_the_viewport_jump_key() {
        let end = Key::new(KeyCode::End, KeyModifiers::NONE);
        let composer_context = [Owner::App, Owner::Composer, Owner::Editor];
        assert_eq!(
            resolve_in(end, false, &composer_context),
            Some(Action::LineEnd),
            "End belongs to the composer line end outside a viewport"
        );
        assert_eq!(
            resolve_in(end, true, &composer_context),
            Some(Action::LineEnd)
        );
        assert_eq!(
            resolve_in(end, false, &[Owner::Dialog, Owner::Picker]),
            Some(Action::PickerLast),
            "a picker keeps its own End binding"
        );
        assert_eq!(
            resolve_in(end, false, &[Owner::App, Owner::Transcript]),
            Some(Action::TranscriptJumpLatest),
            "a viewport owns End for jump to latest"
        );
        let jump = help_labels()
            .into_iter()
            .find(|(key, _)| key == "end")
            .map(|(_, label)| label);
        assert_eq!(jump, Some("Jump to latest"));
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
        assert_eq!(decoder.feed(&[0xe6, 0x97], start), []);
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
    fn the_delete_key_decodes_from_the_legacy_tilde_form() {
        let mut decoder = KeyDecoder::default();
        assert_eq!(
            decoder.feed(b"\x1b[3~", Instant::now()),
            [InputEvent::Key(Key::new(
                KeyCode::Delete,
                KeyModifiers::NONE
            ))]
        );
    }

    #[test]
    fn escape_aborts_partial_csi_without_leaking_literal_text() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(
            decoder.feed(b"\x1b[<0;10;\x1b[C", start),
            [InputEvent::Key(Key::new(
                KeyCode::Right,
                KeyModifiers::NONE
            ))]
        );
    }

    #[test]
    fn unmapped_control_aborts_partial_csi_without_leaking_literal_text() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(
            decoder.feed(b"\x1b[<0;10;\x1ca", start),
            [
                InputEvent::Key(Key::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)),
                InputEvent::Key(Key::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            ]
        );
    }

    #[test]
    fn control_byte_aborts_partial_csi_sequence() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(decoder.feed(b"\x1b[<0;10;", start).len(), 0);
        assert_eq!(
            decoder.feed(&[0x03], start + Duration::from_millis(1)),
            [InputEvent::Key(Key::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            ))]
        );
        assert_eq!(
            decoder.feed(b"x", start + Duration::from_millis(2)),
            [InputEvent::Key(Key::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE
            ))]
        );

        let mut decoder = KeyDecoder::default();
        assert_eq!(
            decoder.feed(b"\x1b[<0;10;\x03", start),
            [InputEvent::Key(Key::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            ))]
        );
    }

    #[test]
    fn split_paste_start_and_bare_end_are_handled() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(decoder.feed(b"\x1b[20", start), []);
        assert_eq!(decoder.feed(b"0~hi", start + Duration::from_millis(5)), []);
        assert_eq!(
            decoder.feed(b"\x1b[201~", start + Duration::from_millis(6)),
            [InputEvent::Paste(b"hi".to_vec())]
        );
        assert_eq!(
            decoder.feed(b"\x1b[201~", start + Duration::from_millis(7)),
            []
        );
    }

    #[test]
    fn lone_escape_waits_fifty_milliseconds() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(decoder.feed(b"\x1b", start), []);
        assert_eq!(
            decoder.tick(start + Duration::from_millis(50)),
            [InputEvent::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE))]
        );
    }

    #[test]
    fn missing_paste_end_closes_at_two_seconds() {
        let start = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(decoder.feed(b"\x1b[200~hi", start), []);
        assert_eq!(
            decoder.tick(start + Duration::from_secs(2)),
            [InputEvent::Paste(b"hi".to_vec())]
        );
    }

    #[test]
    fn legacy_unit_separator_decodes_to_ctrl_underscore_and_resolves_undo() {
        let now = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(
            decoder.feed(&[0x1c], now),
            [InputEvent::Key(Key::new(
                KeyCode::Char('\\'),
                KeyModifiers::CONTROL
            ))]
        );
        assert_eq!(
            decoder.feed(&[0x1d], now),
            [InputEvent::Key(Key::new(
                KeyCode::Char(']'),
                KeyModifiers::CONTROL
            ))]
        );
        assert_eq!(
            decoder.feed(&[0x1e], now),
            [InputEvent::Key(Key::new(
                KeyCode::Char('^'),
                KeyModifiers::CONTROL
            ))]
        );

        let events = decoder.feed(&[0x1f], now);
        assert_eq!(
            events,
            [InputEvent::Key(Key::new(
                KeyCode::Char('_'),
                KeyModifiers::CONTROL
            ))]
        );
        let InputEvent::Key(key) = events[0] else {
            panic!("expected key event");
        };
        assert_eq!(
            resolve_in(key, false, &[Owner::Composer]),
            Some(Action::Undo)
        );
        assert_eq!(
            resolve_in(key, true, &[Owner::Composer]),
            Some(Action::Undo)
        );

        let mut csi_decoder = KeyDecoder::default();
        let csi_events = csi_decoder.feed(b"\x1b[95;5u", now);
        assert_eq!(
            csi_events,
            [InputEvent::Key(Key::new(
                KeyCode::Char('_'),
                KeyModifiers::CONTROL
            ))]
        );
        let InputEvent::Key(csi_key) = csi_events[0] else {
            panic!("expected csi key event");
        };
        assert_eq!(
            resolve_in(csi_key, true, &[Owner::Composer]),
            Some(Action::Undo)
        );
        assert_eq!(
            resolve_in(csi_key, false, &[Owner::Composer]),
            Some(Action::Undo)
        );
    }

    #[test]
    fn byte_eight_and_delete_decode_to_backspace_and_alt_backspace_deletes_word() {
        let now = Instant::now();
        let mut decoder = KeyDecoder::default();
        assert_eq!(
            decoder.feed(&[0x08], now),
            [InputEvent::Key(Key::new(
                KeyCode::Backspace,
                KeyModifiers::NONE
            ))]
        );
        assert_eq!(
            decoder.feed(&[0x7f], now),
            [InputEvent::Key(Key::new(
                KeyCode::Backspace,
                KeyModifiers::NONE
            ))]
        );

        let mut alt_bs_del = KeyDecoder::default();
        let events_del = alt_bs_del.feed(b"\x1b\x7f", now);
        assert_eq!(
            events_del,
            [InputEvent::Key(Key::new(
                KeyCode::Backspace,
                KeyModifiers::ALT
            ))]
        );
        let InputEvent::Key(alt_bs_key) = events_del[0] else {
            panic!("expected key event");
        };
        assert_eq!(
            resolve_in(alt_bs_key, false, &[Owner::Composer]),
            Some(Action::DeleteWordBack)
        );
        assert_eq!(
            resolve_in(alt_bs_key, true, &[Owner::Composer]),
            Some(Action::DeleteWordBack)
        );

        let mut alt_bs_eight = KeyDecoder::default();
        let events_eight = alt_bs_eight.feed(b"\x1b\x08", now);
        assert_eq!(
            events_eight,
            [InputEvent::Key(Key::new(
                KeyCode::Backspace,
                KeyModifiers::ALT
            ))]
        );
    }

    #[test]
    fn documented_legacy_alternative_chords_resolve_under_kitty_and_legacy_modes() {
        let now = Instant::now();
        let composer = [Owner::Composer];

        let mut ctrl_b_dec = KeyDecoder::default();
        let legacy_ctrl_b = ctrl_b_dec.feed(&[0x02], now);
        assert_eq!(
            legacy_ctrl_b,
            [InputEvent::Key(Key::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL
            ))]
        );
        let InputEvent::Key(b_key) = legacy_ctrl_b[0] else {
            panic!("expected key");
        };
        assert_eq!(
            resolve_in(b_key, true, &composer),
            Some(Action::CharLeft),
            "Ctrl+B must move cursor left even when kitty negotiation is active"
        );
        assert_eq!(resolve_in(b_key, false, &composer), Some(Action::CharLeft));

        let mut csi_b_dec = KeyDecoder::default();
        let csi_b = csi_b_dec.feed(b"\x1b[98;5u", now);
        assert_eq!(csi_b, legacy_ctrl_b);

        let mut left_dec = KeyDecoder::default();
        let left_events = left_dec.feed(b"\x1b[D", now);
        let InputEvent::Key(left_key) = left_events[0] else {
            panic!("expected key");
        };
        assert_eq!(
            resolve_in(left_key, true, &composer),
            Some(Action::CharLeft)
        );
        assert_eq!(
            resolve_in(left_key, false, &composer),
            Some(Action::CharLeft)
        );

        let mut newline_dec = KeyDecoder::default();
        let j_events = newline_dec.feed(b"\n", now);
        assert_eq!(
            j_events,
            [InputEvent::Key(Key::new(
                KeyCode::Char('j'),
                KeyModifiers::CONTROL
            ))]
        );
        let InputEvent::Key(j_key) = j_events[0] else {
            panic!("expected key");
        };
        assert_eq!(
            resolve_in(j_key, true, &composer),
            Some(Action::Newline),
            "Ctrl+J must insert newline even when kitty negotiation is active"
        );
        assert_eq!(resolve_in(j_key, false, &composer), Some(Action::Newline));

        let mut shift_enter_dec = KeyDecoder::default();
        let se_events = shift_enter_dec.feed(b"\x1b[13;2u", now);
        let InputEvent::Key(se_key) = se_events[0] else {
            panic!("expected key");
        };
        assert_eq!(resolve_in(se_key, true, &composer), Some(Action::Newline));
        assert_eq!(resolve_in(se_key, false, &composer), Some(Action::Newline));
    }

    #[test]
    fn shared_keys_respect_explicit_owner_precedence_in_both_modes() {
        let ctrl_b = Key::new(KeyCode::Char('b'), KeyModifiers::CONTROL);

        assert_eq!(
            resolve_in(ctrl_b, false, &[Owner::Composer, Owner::Transcript]),
            Some(Action::CharLeft)
        );
        assert_eq!(
            resolve_in(ctrl_b, true, &[Owner::Composer, Owner::Transcript]),
            Some(Action::CharLeft)
        );
        assert_eq!(
            resolve_in(ctrl_b, false, &[Owner::Transcript, Owner::Composer]),
            Some(Action::TranscriptPageUp)
        );
        assert_eq!(
            resolve_in(ctrl_b, true, &[Owner::Transcript, Owner::Composer]),
            Some(Action::TranscriptPageUp)
        );

        let esc = Key::new(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(
            resolve_in(esc, false, &[Owner::Dialog, Owner::App]),
            Some(Action::ApprovalDeny)
        );
        assert_eq!(
            resolve_in(esc, true, &[Owner::Dialog, Owner::App]),
            Some(Action::ApprovalDeny)
        );
        assert_eq!(
            resolve_in(esc, false, &[Owner::App, Owner::Dialog]),
            Some(Action::CloseOverlayOrInterrupt)
        );
        assert_eq!(
            resolve_in(esc, true, &[Owner::App, Owner::Dialog]),
            Some(Action::CloseOverlayOrInterrupt)
        );
        assert_eq!(
            resolve_in(esc, false, &[Owner::Picker, Owner::App]),
            Some(Action::PickerCancel)
        );
        assert_eq!(
            resolve_in(esc, true, &[Owner::Picker, Owner::App]),
            Some(Action::PickerCancel)
        );
    }
}
