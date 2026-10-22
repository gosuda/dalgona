//! End-to-end keyboard evidence: wire bytes -> `KeyDecoder` -> `resolve_in`.
//!
//! Covers the CSI-u control-byte and alternate-binding repairs (Ctrl+B,
//! Ctrl+O, Ctrl+_) plus the promised actions (help, expand, picker, quit,
//! cancel) in BOTH terminal modes (`kitty = true/false`). DESIGN.md is the
//! contract; `keys::BINDINGS` is the table under test.
//!
//! Also covers the repaired gaps from `.outline/sdd/report-keyboard-verify.md`:
//! picker Ctrl+P/N/B/F chords, Undo on Ctrl+-, `AcceptCycleCompletion` on
//! Shift+Tab/BackTab, and `DialogFocusRight` on Tab.

#![cfg(test)]

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use dal_tui::keys::{Action, BINDINGS, InputEvent, Key, KeyDecoder, Owner, resolve_in, validate};

fn decode_all(bytes: &[u8]) -> Vec<InputEvent> {
    let start = Instant::now();
    let mut decoder = KeyDecoder::default();
    let mut events = decoder.feed(bytes, start);
    // Settle lone-ESC style partials without disturbing complete sequences.
    events.extend(decoder.tick(start + Duration::from_millis(100)));
    events
}

fn decode_key(bytes: &[u8]) -> Key {
    let events = decode_all(bytes);
    assert_eq!(events.len(), 1, "one key from {bytes:?}, got {events:?}");
    let InputEvent::Key(key) = events[0] else {
        unreachable!("key from {bytes:?}, got {events:?}");
    };
    key
}

fn expect_action(bytes: &[u8], owners: &[Owner], action: Action) {
    let key = decode_key(bytes);
    for kitty in [false, true] {
        assert_eq!(
            resolve_in(key, kitty, owners),
            Some(action),
            "{bytes:?} kitty={kitty} owners={owners:?}"
        );
    }
}

#[test]
fn control_bytes_decode_to_their_documented_ctrl_keys() {
    let cases: &[(u8, char)] = &[
        (0x02, 'b'),
        (0x03, 'c'),
        (0x04, 'd'),
        (0x06, 'f'),
        (0x07, 'g'),
        (0x0b, 'k'),
        (0x0c, 'l'),
        (0x0e, 'n'),
        (0x0f, 'o'),
        (0x10, 'p'),
        (0x12, 'r'),
        (0x14, 't'),
        (0x15, 'u'),
        (0x19, 'y'),
        (0x1a, 'z'),
    ];
    for (byte, letter) in cases {
        assert_eq!(
            decode_key(&[*byte]),
            Key::new(KeyCode::Char(*letter), KeyModifiers::CONTROL),
            "control byte 0x{byte:02x}",
        );
    }
    assert_eq!(
        decode_key(&[0x1c]),
        Key::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)
    );
    assert_eq!(
        decode_key(&[0x1d]),
        Key::new(KeyCode::Char(']'), KeyModifiers::CONTROL)
    );
    assert_eq!(
        decode_key(&[0x1e]),
        Key::new(KeyCode::Char('^'), KeyModifiers::CONTROL)
    );
    assert_eq!(
        decode_key(&[0x1f]),
        Key::new(KeyCode::Char('_'), KeyModifiers::CONTROL)
    );
    assert_eq!(
        decode_key(b"\r"),
        Key::new(KeyCode::Enter, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\t"),
        Key::new(KeyCode::Tab, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(&[0x08]),
        Key::new(KeyCode::Backspace, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(&[0x7f]),
        Key::new(KeyCode::Backspace, KeyModifiers::NONE)
    );
    // LF arrives as Ctrl+J: the legacy newline key.
    assert_eq!(
        decode_key(b"\n"),
        Key::new(KeyCode::Char('j'), KeyModifiers::CONTROL)
    );
}

#[test]
fn legacy_escape_and_csi_sequences_decode() {
    assert_eq!(
        decode_key(b"\x1bOP"),
        Key::new(KeyCode::F(1), KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1bOR"),
        Key::new(KeyCode::F(3), KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[A"),
        Key::new(KeyCode::Up, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[B"),
        Key::new(KeyCode::Down, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[1;5A"),
        Key::new(KeyCode::Up, KeyModifiers::CONTROL)
    );
    assert_eq!(
        decode_key(b"\x1b[1;5B"),
        Key::new(KeyCode::Down, KeyModifiers::CONTROL)
    );
    assert_eq!(
        decode_key(b"\x1b[D"),
        Key::new(KeyCode::Left, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[5~"),
        Key::new(KeyCode::PageUp, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[6~"),
        Key::new(KeyCode::PageDown, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[H"),
        Key::new(KeyCode::Home, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[F"),
        Key::new(KeyCode::End, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[Z"),
        Key::new(KeyCode::BackTab, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1bb"),
        Key::new(KeyCode::Char('b'), KeyModifiers::ALT)
    );
    assert_eq!(
        decode_key(b"\x1bf"),
        Key::new(KeyCode::Char('f'), KeyModifiers::ALT)
    );
    assert_eq!(
        decode_key(b"\x1bd"),
        Key::new(KeyCode::Char('d'), KeyModifiers::ALT)
    );
    assert_eq!(
        decode_key(b"\x1b\x7f"),
        Key::new(KeyCode::Backspace, KeyModifiers::ALT)
    );
    assert_eq!(
        decode_key(b"\x1b"),
        Key::new(KeyCode::Esc, KeyModifiers::NONE)
    );
}

#[test]
fn csi_u_control_codes_decode_to_ctrl_keys() {
    let cases: &[(&[u8], char)] = &[
        (b"\x1b[98;5u", 'b'),
        (b"\x1b[99;5u", 'c'),
        (b"\x1b[100;5u", 'd'),
        (b"\x1b[102;5u", 'f'),
        (b"\x1b[111;5u", 'o'),
        (b"\x1b[116;5u", 't'),
    ];
    for (bytes, letter) in cases {
        assert_eq!(
            decode_key(bytes),
            Key::new(KeyCode::Char(*letter), KeyModifiers::CONTROL),
            "CSI-u {bytes:?}",
        );
    }
    assert_eq!(
        decode_key(b"\x1b[95;5u"),
        Key::new(KeyCode::Char('_'), KeyModifiers::CONTROL)
    );
    assert_eq!(
        decode_key(b"\x1b[13;2u"),
        Key::new(KeyCode::Enter, KeyModifiers::SHIFT)
    );
    assert_eq!(
        decode_key(b"\x1b[102;6u"),
        Key::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        )
    );
}

#[test]
fn repaired_control_bytes_resolve_in_both_modes() {
    let composer = [Owner::Composer];
    let app = [Owner::App];
    // Ctrl+B: cursor left, legacy byte and CSI-u, kitty on and off.
    for bytes in [&[0x02][..], b"\x1b[98;5u".as_slice()] {
        let key = decode_key(bytes);
        for kitty in [false, true] {
            assert_eq!(resolve_in(key, kitty, &composer), Some(Action::CharLeft));
        }
    }
    // Ctrl+O: expand, legacy byte and CSI-u.
    for bytes in [&[0x0f][..], b"\x1b[111;5u".as_slice()] {
        let key = decode_key(bytes);
        for kitty in [false, true] {
            assert_eq!(resolve_in(key, kitty, &app), Some(Action::ExpandCollapse));
        }
    }
    // Ctrl+_: undo, legacy unit separator and CSI-u.
    for bytes in [&[0x1f][..], b"\x1b[95;5u".as_slice()] {
        let key = decode_key(bytes);
        for kitty in [false, true] {
            assert_eq!(resolve_in(key, kitty, &composer), Some(Action::Undo));
        }
    }
    // Newline: LF and kitty Shift+Enter agree.
    expect_action(b"\n", &composer, Action::Newline);
    expect_action(b"\x1b[13;2u", &composer, Action::Newline);
}

#[test]
fn promised_help_expand_quit_cancel_resolve_in_both_modes() {
    let app = [Owner::App];
    let picker = [Owner::Picker];
    let dialog = [Owner::Dialog];
    // Help overlay: F1 via legacy SS3.
    expect_action(b"\x1bOP", &app, Action::Help);
    // Expand / quit via legacy control bytes and CSI-u.
    expect_action(&[0x0f], &app, Action::ExpandCollapse);
    expect_action(b"\x1b[111;5u", &app, Action::ExpandCollapse);
    expect_action(&[0x04], &app, Action::QuitEmptyComposer);
    expect_action(b"\x1b[100;5u", &app, Action::QuitEmptyComposer);
    // Cancel paths: picker, dialog (Esc is Deny's legacy key), app.
    expect_action(b"\x1b", &picker, Action::PickerCancel);
    expect_action(b"\x1b", &dialog, Action::ApprovalDeny);
    expect_action(b"\x1b", &app, Action::CloseOverlayOrInterrupt);
    expect_action(b"\r", &picker, Action::PickerAccept);
}

#[test]
fn picker_documented_keys_resolve_in_both_modes() {
    let picker = [Owner::Picker];
    expect_action(b"\x1b[A", &picker, Action::PickerMoveUp);
    expect_action(b"\x1b[B", &picker, Action::PickerMoveDown);
    expect_action(b"k", &picker, Action::PickerMoveUp);
    expect_action(b"j", &picker, Action::PickerMoveDown);
    expect_action(b"\x1b[5~", &picker, Action::PickerPageUp);
    expect_action(b"\x1b[6~", &picker, Action::PickerPageDown);
    expect_action(b"\x1b[H", &picker, Action::PickerFirst);
    expect_action(b"\x1b[F", &picker, Action::PickerLast);
    expect_action(b"d", &picker, Action::PickerDelete);
    // Transcript paging shares the Ctrl+B / Ctrl+F chords by context.
    let transcript = [Owner::Transcript];
    expect_action(&[0x02], &transcript, Action::TranscriptPageUp);
    expect_action(&[0x06], &transcript, Action::TranscriptPageDown);
    expect_action(b"\x1b[98;5u", &transcript, Action::TranscriptPageUp);
    expect_action(b"\x1b[102;5u", &transcript, Action::TranscriptPageDown);
}

#[test]
fn every_table_binding_resolves_in_both_modes() {
    assert!(validate(BINDINGS).is_ok());
    for binding in BINDINGS {
        // Two pairs share one key by design; the first row owns `resolve`
        // while the runtime disambiguates by state (selection present for
        // CopySelection/Interrupt, ask form open for AskSubmit/DialogActivate).
        let shadowed_by = match binding.action {
            Action::Interrupt => Some(Action::CopySelection),
            Action::DialogActivate => Some(Action::AskSubmit),
            // Esc resolves to the earlier ApprovalDeny row (its legacy key),
            // so the AskDismiss default never wins `resolve_in` outright.
            Action::AskDismiss => Some(Action::ApprovalDeny),
            _ => None,
        };
        let owners = [binding.owner];
        for kitty in [false, true] {
            let want = shadowed_by.unwrap_or(binding.action);
            assert_eq!(
                resolve_in(binding.default, kitty, &owners),
                Some(want),
                "default {:?} kitty={kitty}",
                binding.default,
            );
            if let Some(legacy) = binding.legacy {
                assert_eq!(
                    resolve_in(legacy, kitty, &owners),
                    Some(want),
                    "legacy {legacy:?} kitty={kitty}",
                );
            }
        }
    }
}

#[test]
fn repaired_picker_ctrl_chords_resolve_in_both_modes() {
    let picker = [Owner::Picker];
    // Ctrl+P / Ctrl+N move the selection; legacy control bytes and CSI-u
    // agree (decoder: 0x10 -> 'p'+CTRL, 0x0e -> 'n'+CTRL).
    assert_eq!(
        decode_key(&[0x10]),
        Key::new(KeyCode::Char('p'), KeyModifiers::CONTROL)
    );
    assert_eq!(
        decode_key(&[0x0e]),
        Key::new(KeyCode::Char('n'), KeyModifiers::CONTROL)
    );
    expect_action(&[0x10], &picker, Action::PickerMoveUp);
    expect_action(b"\x1b[112;5u", &picker, Action::PickerMoveUp);
    expect_action(&[0x0e], &picker, Action::PickerMoveDown);
    expect_action(b"\x1b[110;5u", &picker, Action::PickerMoveDown);
    // Ctrl+B / Ctrl+F page; same wire pattern as the transcript paging
    // chords, scoped to the picker owner here.
    expect_action(&[0x02], &picker, Action::PickerPageUp);
    expect_action(b"\x1b[98;5u", &picker, Action::PickerPageUp);
    expect_action(&[0x06], &picker, Action::PickerPageDown);
    expect_action(b"\x1b[102;5u", &picker, Action::PickerPageDown);
}

#[test]
fn undo_ctrl_minus_resolves_in_both_modes() {
    // CSI-u 45;5u decodes to '-'+CTRL; there is no legacy single byte for
    // Ctrl+-, so only the CSI-u form is asserted on the wire.
    assert_eq!(
        decode_key(b"\x1b[45;5u"),
        Key::new(KeyCode::Char('-'), KeyModifiers::CONTROL)
    );
    expect_action(b"\x1b[45;5u", &[Owner::Composer], Action::Undo);
    expect_action(b"\x1b[45;5u", &[Owner::Editor], Action::Undo);
    // The existing Ctrl+_ chord keeps working alongside.
    expect_action(&[0x1f], &[Owner::Composer], Action::Undo);
    expect_action(&[0x1f], &[Owner::Editor], Action::Undo);
}

#[test]
fn accept_cycle_completion_shift_tab_resolves_in_both_modes() {
    // Legacy Shift+Tab arrives as BackTab; kitty CSI-u 9;2u arrives as
    // Tab+SHIFT. Both must accept or cycle the composer completion.
    assert_eq!(
        decode_key(b"\x1b[Z"),
        Key::new(KeyCode::BackTab, KeyModifiers::NONE)
    );
    assert_eq!(
        decode_key(b"\x1b[9;2u"),
        Key::new(KeyCode::Tab, KeyModifiers::SHIFT)
    );
    let composer = [Owner::Composer];
    expect_action(b"\x1b[Z", &composer, Action::AcceptCycleCompletion);
    expect_action(b"\x1b[9;2u", &composer, Action::AcceptCycleCompletion);
    // Plain Tab still completes.
    expect_action(b"\t", &composer, Action::AcceptCycleCompletion);
}

#[test]
fn dialog_focus_right_tab_resolves_in_both_modes() {
    // 0x09 decodes to Tab; the dialog table must move focus right on it
    // instead of relying only on the Select-question direct match.
    assert_eq!(
        decode_key(b"\t"),
        Key::new(KeyCode::Tab, KeyModifiers::NONE)
    );
    expect_action(b"\t", &[Owner::Dialog], Action::DialogFocusRight);
}

fn expect_no_action(bytes: &[u8], owners: &[Owner]) {
    let key = decode_key(bytes);
    for kitty in [false, true] {
        assert_eq!(
            resolve_in(key, kitty, owners),
            None,
            "{bytes:?} kitty={kitty} owners={owners:?} must not resolve"
        );
    }
}

fn expect_all(owners: &[Owner], cases: &[(&[u8], Action)]) {
    for (bytes, action) in cases {
        expect_action(bytes, owners, *action);
    }
}

/// The composer chords are pinned to their documented actions by literal wire
/// bytes, not by reading the table back. A table-derived check stays green when
/// two rows trade keys (Ctrl+K for Ctrl+U, Up for Down), so every user's editing
/// keys would silently trade places.
#[test]
fn composer_chords_resolve_to_their_documented_actions() {
    expect_all(
        &[Owner::Composer],
        &[
            (b"\x01", Action::LineStart),
            (b"\x05", Action::LineEnd),
            (b"\x0b", Action::KillLineEnd),
            (b"\x15", Action::KillLineStart),
            (b"\x17", Action::DeleteWordBack),
            (b"\x19", Action::Yank),
            (b"\x1b\x7f", Action::DeleteWordBack),
            (b"\x1bd", Action::DeleteWordForward),
            (b"\x1bb", Action::WordLeft),
            (b"\x1bf", Action::WordRight),
            (b"\x02", Action::CharLeft),
            (b"\x06", Action::CharRight),
            (b"\x1b[D", Action::CharLeft),
            (b"\x1b[C", Action::CharRight),
            (b"\x1b[H", Action::LineStart),
            (b"\x1b[F", Action::LineEnd),
            (b"\x1b[A", Action::HistoryPrev),
            (b"\x1b[B", Action::HistoryNext),
            (b"\x10", Action::HistoryPrev),
            (b"\x0e", Action::HistoryNext),
            (b"\r", Action::Submit),
        ],
    );
}

/// Dialog answers and transcript selection are pinned the same way: a swapped
/// pair here approves what the user meant to deny, or selects the wrong way.
#[test]
fn dialog_and_transcript_chords_resolve_to_their_documented_actions() {
    expect_all(
        &[Owner::Dialog],
        &[
            (b"y", Action::ApprovalAllowOnce),
            (b"a", Action::ApprovalAllowSession),
            (b"n", Action::ApprovalDeny),
            (b"v", Action::ApprovalView),
            (b"r", Action::RetryNow),
            (b"c", Action::RetryCancel),
            (b" ", Action::AskToggle),
            (b"\x1b[A", Action::AskMoveUp),
            (b"\x1b[B", Action::AskMoveDown),
            (b"\x1b[D", Action::DialogFocusLeft),
            (b"\x1b[C", Action::DialogFocusRight),
            (b"\x1b[5~", Action::DialogScrollUp),
            (b"\x1b[6~", Action::DialogScrollDown),
        ],
    );
    expect_all(
        &[Owner::Transcript],
        &[
            (b"\x1b[1;2A", Action::SelectionUp),
            (b"\x1b[1;2B", Action::SelectionDown),
            (b"\x1b[1;2D", Action::SelectionLeft),
            (b"\x1b[1;2C", Action::SelectionRight),
            (b"\x1b[F", Action::TranscriptJumpLatest),
        ],
    );
}

/// One chord means different things to different owners, and the first listed
/// owner wins. Resolving by table order instead would hand Ctrl+P to the
/// picker while the composer has focus.
#[test]
fn a_chord_belongs_to_its_owner_and_the_first_listed_owner_wins() {
    let letter_y = decode_key(b"y");
    let ctrl_y = decode_key(b"\x19");
    let ctrl_p = decode_key(b"\x10");
    for kitty in [false, true] {
        assert_eq!(
            resolve_in(letter_y, kitty, &[Owner::Dialog]),
            Some(Action::ApprovalAllowOnce)
        );
        // A text owner must not claim the letter the dialog answers with.
        assert_eq!(
            resolve_in(
                letter_y,
                kitty,
                &[
                    Owner::Composer,
                    Owner::Editor,
                    Owner::App,
                    Owner::Transcript
                ]
            ),
            None
        );
        assert_eq!(
            resolve_in(ctrl_y, kitty, &[Owner::Dialog, Owner::Picker]),
            None
        );
        assert_eq!(
            resolve_in(ctrl_y, kitty, &[Owner::Composer]),
            Some(Action::Yank)
        );
        assert_eq!(
            resolve_in(ctrl_p, kitty, &[Owner::Picker, Owner::Composer]),
            Some(Action::PickerMoveUp)
        );
        assert_eq!(
            resolve_in(ctrl_p, kitty, &[Owner::Composer, Owner::Picker]),
            Some(Action::HistoryPrev)
        );
        // An owner that does not bind the key defers to the next one listed.
        assert_eq!(
            resolve_in(letter_y, kitty, &[Owner::Composer, Owner::Dialog]),
            Some(Action::ApprovalAllowOnce)
        );
        assert_eq!(resolve_in(letter_y, kitty, &[]), None);
    }
}

/// No printable key may resolve in an owner that shares the keyboard with the
/// text field, legacy spellings included: `validate` checks each row's default
/// key only, so a printable legacy key would otherwise swallow typed text. A
/// chord is also exactly its modifiers: Ctrl+Alt+K is not Ctrl+K.
#[test]
fn text_owners_never_claim_a_printable_key_and_modifiers_match_exactly() {
    let text_owners = [
        Owner::Composer,
        Owner::Editor,
        Owner::App,
        Owner::Transcript,
    ];
    for scalar in 0x20_u8..=0x7e {
        let character = char::from(scalar);
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            let key = Key::new(KeyCode::Char(character), modifiers);
            for kitty in [false, true] {
                assert_eq!(
                    resolve_in(key, kitty, &text_owners),
                    None,
                    "printable {character:?} with {modifiers:?} must reach the text field"
                );
            }
        }
    }
    let composer = [Owner::Composer];
    expect_action(b"\x0b", &composer, Action::KillLineEnd);
    expect_no_action(b"\x1b[107;7u", &composer);
    expect_no_action(b"\x1b[107;6u", &composer);
    expect_no_action(b"\x1b[107;3u", &composer);
}
