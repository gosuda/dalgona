//! Pure terminal-safe encoding for untrusted display text.

use std::borrow::Cow;
use std::fmt::Write as _;

/// Appends a visible representation of one character, escaping all terminal
/// controls and Unicode bidirectional formatting controls.
pub fn escape_into(output: &mut String, character: char) {
    match character {
        '\t' => output.push_str("\\t"),
        '\n' => output.push_str("\\n"),
        '\r' => output.push_str("\\r"),
        '\u{7}' => output.push_str("\\a"),
        character if character.is_control() || is_bidi_control(character) => {
            let _ = write!(output, "\\u{{{:x}}}", u32::from(character));
        }
        character => output.push(character),
    }
}

/// Makes all controls visible, including line feeds, for inline terminal rendering.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        escape_into(&mut escaped, character);
    }
    escaped
}

/// Escapes terminal controls while preserving line feeds for streamed text.
///
/// Returns the original borrowed text when no escaping is needed. Keep this
/// rendering separate from the raw assistant message used by files and pipes.
#[must_use]
pub fn escape_multiline(text: &str) -> Cow<'_, str> {
    if text.chars().all(|character| {
        character == '\n' || (!character.is_control() && !is_bidi_control(character))
    }) {
        return Cow::Borrowed(text);
    }

    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character == '\n' {
            escaped.push('\n');
        } else {
            escape_into(&mut escaped, character);
        }
    }
    Cow::Owned(escaped)
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{61c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{escape, escape_multiline};

    #[test]
    fn terminal_controls_are_escaped_as_visible_text() {
        let payload =
            "x\u{1b}]52;c;YWJj\u{7}\u{1b}[2J\r\t\u{0}\u{7f}\u{9b}[0m\u{9c}\u{202e}\u{2066}y";
        let visible =
            r"x\u{1b}]52;c;YWJj\a\u{1b}[2J\r\t\u{0}\u{7f}\u{9b}[0m\u{9c}\u{202e}\u{2066}y";
        assert_eq!(escape(payload), visible);
        assert_eq!(escape_multiline(payload).as_ref(), visible);
    }

    #[test]
    fn line_feeds_are_preserved_only_for_streamed_text() {
        let text = "one\nשלום\n世界";
        assert_eq!(escape(text), r"one\nשלום\n世界");
        assert_eq!(escape_multiline(text).as_ref(), text);
        assert!(matches!(escape_multiline(text), Cow::Borrowed(_)));
    }

    #[test]
    fn all_bidi_formatting_controls_are_escaped() {
        let payload = "\u{61c}\u{200e}\u{200f}\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}";
        let visible = r"\u{61c}\u{200e}\u{200f}\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}";
        assert_eq!(escape_multiline(payload).as_ref(), visible);
    }
}
