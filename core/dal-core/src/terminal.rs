//! Terminal-safe rendering for untrusted client text.

use std::borrow::Cow;
use std::fmt::Write as _;

/// Escapes C0/C1 controls, DEL, and Unicode bidirectional formatting marks.
///
/// The input is borrowed when no escaping is necessary.
#[must_use]
pub fn escape(text: &str) -> Cow<'_, str> {
    escape_impl(text, false)
}

/// Escapes terminal controls while retaining line feeds for streamed output.
///
/// Only LF is preserved: carriage returns, tabs, and every other terminal
/// control are made visible. Redirected output should bypass this transform.
#[must_use]
pub fn escape_preserving_newlines(text: &str) -> Cow<'_, str> {
    escape_impl(text, true)
}

/// Appends one character in its terminal-safe form.
///
/// Rendering clients that track byte offsets for links use this to keep the
/// escaped text and the offset mapping in sync.
pub fn push_escaped(output: &mut String, character: char) {
    match character {
        '\t' => output.push_str("\\t"),
        '\n' => output.push_str("\\n"),
        '\r' => output.push_str("\\r"),
        '\u{7}' => output.push_str("\\a"),
        character if needs_escape(character) => {
            let _ = write!(output, "\\u{{{:x}}}", u32::from(character));
        }
        character => output.push(character),
    }
}

fn escape_impl(text: &str, preserve_newlines: bool) -> Cow<'_, str> {
    let Some((first, _)) = text.char_indices().find(|(_, character)| {
        needs_escape(*character) && (!preserve_newlines || *character != '\n')
    }) else {
        return Cow::Borrowed(text);
    };
    let mut output = String::with_capacity(text.len());
    output.push_str(&text[..first]);
    for character in text[first..].chars() {
        if preserve_newlines && character == '\n' {
            output.push('\n');
        } else {
            push_escaped(&mut output, character);
        }
    }
    Cow::Owned(output)
}

fn needs_escape(character: char) -> bool {
    character.is_control() || is_bidi_control(character)
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

    use super::{escape, escape_preserving_newlines, push_escaped};

    #[test]
    fn makes_terminal_controls_and_bidi_marks_visible() {
        let input = "text\t\n\r\x07\x1b\u{9b}\x7f\u{202e}end";
        assert_eq!(
            escape(input),
            "text\\t\\n\\r\\a\\u{1b}\\u{9b}\\u{7f}\\u{202e}end"
        );
        assert_eq!(
            escape_preserving_newlines(input),
            "text\\t\n\\r\\a\\u{1b}\\u{9b}\\u{7f}\\u{202e}end"
        );
    }

    #[test]
    fn preserves_safe_unicode_without_allocation() {
        let input = "Hello\nمرحبا";
        assert!(matches!(
            escape_preserving_newlines(input),
            Cow::Borrowed(_)
        ));
        assert!(matches!(escape("Hello مرحبا"), Cow::Borrowed(_)));
    }

    #[test]
    fn appends_bidi_marks_as_visible_code_points() {
        let mut output = String::new();
        push_escaped(&mut output, '\u{200f}');
        assert_eq!(output, "\\u{200f}");
    }
}
