//! Composer edit buffer: grapheme cursor, history, paste-as-content.

use dal_core::command::{Classify, classify, tokens};
use unicode_segmentation::UnicodeSegmentation;

/// Grapheme-indexed composer draft with boundary history.
#[derive(Debug, Default, Clone)]
pub struct Composer {
    text: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
}

impl Composer {
    /// Inserts text at the grapheme cursor, snapping forward to boundaries.
    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.snap_cursor();
    }

    /// Inserts a raw paste as content.
    pub fn insert_paste(&mut self, bytes: &[u8]) {
        self.insert(&String::from_utf8_lossy(bytes));
    }

    /// Moves the cursor left by one grapheme.
    pub fn move_left(&mut self) {
        let mut index = self.cursor;
        for (byte, _) in self.text.grapheme_indices(true) {
            if byte < self.cursor {
                index = byte;
            }
        }
        self.cursor = index;
    }

    /// Moves the cursor right by one grapheme.
    pub fn move_right(&mut self) {
        let mut next = self.text.len();
        for (byte, _) in self.text.grapheme_indices(true) {
            if byte > self.cursor {
                next = byte;
                break;
            }
        }
        self.cursor = next;
    }

    /// Recalls the previous history entry at the upper boundary.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = self
            .history_index
            .map_or(self.history.len() - 1, |index| index.saturating_sub(1));
        self.history_index = Some(next);
        self.text = self.history[next].clone();
        self.cursor = self.text.len();
    }

    /// Recalls the next history entry.
    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.text.clear();
            self.cursor = 0;
        } else {
            self.history_index = Some(index + 1);
            self.text = self.history[index + 1].clone();
            self.cursor = self.text.len();
        }
    }

    /// Submits the draft, recording history and clearing the buffer.
    pub fn take(&mut self) -> String {
        let line = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_index = None;
        if !line.trim().is_empty() {
            self.history.push(line.clone());
        }
        line
    }

    /// Borrows the draft text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the grapheme cursor as a byte index on a boundary.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    fn snap_cursor(&mut self) {
        if self.text.is_empty() {
            self.cursor = 0;
            return;
        }
        if self.text.is_char_boundary(self.cursor) {
            let mut boundary = 0;
            for (byte, _) in self.text.grapheme_indices(true) {
                if byte <= self.cursor {
                    boundary = byte;
                }
            }
            self.cursor = boundary;
            if self.cursor > 0 {
                let previous = &self.text[..self.cursor];
                if let Some((byte, _)) = previous.grapheme_indices(true).next_back() {
                    let _ = byte;
                }
            }
        } else {
            while self.cursor < self.text.len() && !self.text.is_char_boundary(self.cursor) {
                self.cursor += 1;
            }
        }
    }
}
/// The slash command and partial argument under the cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlashCompletion {
    /// The command name as written, without the leading slash.
    pub name: String,
    /// The partial argument being typed.
    pub prefix: String,
}

/// Splits a `/name tail` draft into its command name and partial argument.
///
/// The tail lexes through the fixed command lexer, so quoted spans complete
/// as one value. A draft ending in a separator starts a fresh argument. A
/// mid-typing lexer failure clips the prefix at the failing token, so an
/// unclosed quote still completes its intended value. Plain text drafts
/// complete nothing.
#[must_use]
pub fn slash_completion(draft: &str) -> Option<SlashCompletion> {
    let Classify::Command { name, args } = classify(draft) else {
        return None;
    };
    if draft.ends_with([' ', '\t']) {
        return Some(SlashCompletion {
            name: name.to_string(),
            prefix: String::new(),
        });
    }
    let tail = args.as_ref();
    let prefix = match tokens(tail) {
        Ok(items) => items
            .last()
            .map_or_else(String::new, std::string::ToString::to_string),
        Err(error) => fallback_prefix(tail, error),
    };
    Some(SlashCompletion {
        name: name.to_string(),
        prefix,
    })
}

/// Reads the completion prefix from a tail the fixed lexer rejects.
///
/// An unclosed quote clips at the opening quote and drops it, so the filter
/// sees the intended value. A trailing backslash is dropped and the prefix
/// ends at the previous separator.
fn fallback_prefix(tail: &str, error: dal_core::command::LexError) -> String {
    if error.quote().is_some() {
        let clipped = tail.get(error.at()..).unwrap_or(tail);
        let unquoted = clipped
            .strip_prefix('\'')
            .or_else(|| clipped.strip_prefix('"'))
            .unwrap_or(clipped);
        return unquoted.to_owned();
    }
    let unescaped = tail.strip_suffix('\\').unwrap_or(tail);
    unescaped
        .rsplit([' ', '\t'])
        .next()
        .unwrap_or(unescaped)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Composer, SlashCompletion, slash_completion};

    #[test]
    fn cursor_never_sits_inside_a_grapheme() {
        let mut composer = Composer::default();
        composer.insert("a\u{0301}b");
        assert_eq!(composer.text(), "a\u{0301}b");
        composer.move_left();
        composer.move_left();
        assert!(composer.text().is_char_boundary(composer.cursor()));
    }

    #[test]
    fn history_recalls_at_the_boundary() {
        let mut composer = Composer::default();
        composer.insert("first");
        composer.take();
        composer.insert("second");
        composer.take();
        composer.history_prev();
        assert_eq!(composer.text(), "second");
        composer.history_prev();
        assert_eq!(composer.text(), "first");
        composer.history_next();
        assert_eq!(composer.text(), "second");
    }

    #[test]
    fn slash_completion_splits_name_and_partial_argument() {
        assert_eq!(
            slash_completion("/model open"),
            Some(SlashCompletion {
                name: "model".into(),
                prefix: "open".into()
            })
        );
        assert_eq!(
            slash_completion("/model"),
            Some(SlashCompletion {
                name: "model".into(),
                prefix: String::new()
            })
        );
        assert_eq!(
            slash_completion("/quality:todos src --lim"),
            Some(SlashCompletion {
                name: "quality:todos".into(),
                prefix: "--lim".into()
            })
        );
    }

    #[test]
    fn slash_completion_groups_quoted_spans() {
        assert_eq!(
            slash_completion("/model 'openai gpt'"),
            Some(SlashCompletion {
                name: "model".into(),
                prefix: "openai gpt".into()
            })
        );
        assert_eq!(
            slash_completion("/model \"open"),
            Some(SlashCompletion {
                name: "model".into(),
                prefix: "open".into()
            })
        );
        assert_eq!(
            slash_completion("/quality:todos src \"my fi"),
            Some(SlashCompletion {
                name: "quality:todos".into(),
                prefix: "my fi".into()
            })
        );
        assert_eq!(
            slash_completion("/model ab\\"),
            Some(SlashCompletion {
                name: "model".into(),
                prefix: "ab".into()
            })
        );
    }

    #[test]
    fn slash_completion_starts_fresh_after_a_separator() {
        assert_eq!(
            slash_completion("/model openai/ "),
            Some(SlashCompletion {
                name: "model".into(),
                prefix: String::new()
            })
        );
    }

    #[test]
    fn slash_completion_ignores_plain_text() {
        assert_eq!(slash_completion("hello"), None);
        assert_eq!(slash_completion("/"), None);
        assert_eq!(slash_completion("/skill:"), None);
    }
}
