//! Composer edit buffer: grapheme cursor, line and word motion, kill and yank, undo,
//! history, and the wrapped layout that places the caret.

use dal_core::command::{Classify, classify, tokens};
use unicode_segmentation::UnicodeSegmentation;

use crate::width::{WidthMode, escape, width};

const UNDO_DEPTH: usize = 64;

/// Grapheme-indexed composer draft with boundary history.
///
/// `cursor` is a byte index that always sits on a grapheme boundary of `text`.
#[derive(Debug, Default, Clone)]
pub(crate) struct Composer {
    text: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    /// The draft that history recall replaced, restored when recall runs past the end.
    draft: Option<String>,
    killed: String,
    undo: Vec<(String, usize)>,
    typing: bool,
}

impl Composer {
    /// Whether the draft holds no text.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the draft and puts the caret at its end.
    pub(crate) fn set(&mut self, text: &str) {
        self.checkpoint();
        text.clone_into(&mut self.text);
        self.cursor = self.text.len();
    }

    /// Drops the draft without recording it in history.
    pub(crate) fn clear(&mut self) {
        self.checkpoint();
        self.text.clear();
        self.cursor = 0;
    }

    /// Inserts text at the grapheme cursor, snapping forward to boundaries.
    pub(crate) fn insert(&mut self, text: &str) {
        if !self.typing || text.chars().any(char::is_whitespace) {
            self.checkpoint();
        }
        self.typing = true;
        self.history_index = None;
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.snap_forward();
    }

    /// Inserts a raw paste as content.
    pub(crate) fn insert_paste(&mut self, bytes: &[u8]) {
        self.insert(&String::from_utf8_lossy(bytes));
    }

    /// Deletes the grapheme before the caret.
    pub(crate) fn backspace(&mut self) {
        let start = self.previous_boundary(self.cursor);
        self.remove(start..self.cursor);
    }

    /// Deletes the grapheme under the caret.
    pub(crate) fn delete(&mut self) {
        let end = self.next_boundary(self.cursor);
        self.remove(self.cursor..end);
    }

    /// Moves the cursor left by one grapheme.
    pub(crate) fn move_left(&mut self) {
        self.typing = false;
        self.cursor = self.previous_boundary(self.cursor);
    }

    /// Moves the cursor right by one grapheme.
    pub(crate) fn move_right(&mut self) {
        self.typing = false;
        self.cursor = self.next_boundary(self.cursor);
    }

    /// Moves the cursor to the start of the previous word.
    pub(crate) fn word_left(&mut self) {
        self.typing = false;
        self.cursor = self.word_start(self.cursor);
    }

    /// Moves the cursor past the end of the next word.
    pub(crate) fn word_right(&mut self) {
        self.typing = false;
        self.cursor = self.word_end(self.cursor);
    }

    /// Moves the cursor to the start of its line.
    pub(crate) fn line_start(&mut self) {
        self.typing = false;
        self.cursor = self.line_bounds(self.cursor).0;
    }

    /// Moves the cursor to the end of its line.
    pub(crate) fn line_end(&mut self) {
        self.typing = false;
        self.cursor = self.line_bounds(self.cursor).1;
    }

    /// Whether the cursor sits on the first line of the draft.
    #[must_use]
    pub(crate) fn on_first_line(&self) -> bool {
        !self.text[..self.cursor].contains('\n')
    }

    /// Whether the cursor sits on the last line of the draft.
    #[must_use]
    pub(crate) fn on_last_line(&self) -> bool {
        !self.text[self.cursor..].contains('\n')
    }

    /// Moves the cursor up one line, keeping its grapheme column.
    pub(crate) fn move_up(&mut self) {
        self.typing = false;
        let (start, _) = self.line_bounds(self.cursor);
        if start == 0 {
            return;
        }
        let column = self.text[start..self.cursor].graphemes(true).count();
        let (previous, end) = self.line_bounds(start - 1);
        self.cursor = self.column_offset(previous, end, column);
    }

    /// Moves the cursor down one line, keeping its grapheme column.
    pub(crate) fn move_down(&mut self) {
        self.typing = false;
        let (start, end) = self.line_bounds(self.cursor);
        if end == self.text.len() {
            return;
        }
        let column = self.text[start..self.cursor].graphemes(true).count();
        let (next, next_end) = self.line_bounds(end + 1);
        self.cursor = self.column_offset(next, next_end, column);
    }

    /// Deletes from the start of the line to the caret into the kill buffer.
    pub(crate) fn kill_line_start(&mut self) {
        let start = self.line_bounds(self.cursor).0;
        self.kill(start..self.cursor);
    }

    /// Deletes from the caret to the end of the line into the kill buffer.
    pub(crate) fn kill_line_end(&mut self) {
        let (_, end) = self.line_bounds(self.cursor);
        let end = if end == self.cursor && end < self.text.len() {
            end + 1
        } else {
            end
        };
        self.kill(self.cursor..end);
    }

    /// Deletes the word before the caret into the kill buffer.
    pub(crate) fn delete_word_back(&mut self) {
        let start = self.word_start(self.cursor);
        self.kill(start..self.cursor);
    }

    /// Deletes the word after the caret into the kill buffer.
    pub(crate) fn delete_word_forward(&mut self) {
        let end = self.word_end(self.cursor);
        self.kill(self.cursor..end);
    }

    /// Inserts the last killed text at the caret.
    pub(crate) fn yank(&mut self) {
        let killed = self.killed.clone();
        if !killed.is_empty() {
            self.checkpoint();
            self.typing = false;
            self.insert(&killed);
        }
    }

    /// Restores the draft before the latest edit.
    pub(crate) fn undo(&mut self) {
        self.typing = false;
        if let Some((text, cursor)) = self.undo.pop() {
            self.text = text;
            self.cursor = cursor;
        }
    }

    /// Recalls the previous history entry at the upper boundary.
    pub(crate) fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = if let Some(index) = self.history_index {
            index.saturating_sub(1)
        } else {
            self.draft = Some(self.text.clone());
            self.history.len() - 1
        };
        self.history_index = Some(next);
        self.text = self.history[next].clone();
        self.cursor = self.text.len();
        self.typing = false;
    }

    /// Recalls the next history entry; past the newest, the draft returns.
    pub(crate) fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        self.typing = false;
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.text = self.draft.take().unwrap_or_default();
        } else {
            self.history_index = Some(index + 1);
            self.text = self.history[index + 1].clone();
        }
        self.cursor = self.text.len();
    }

    /// Submits the draft, recording history and clearing the buffer.
    pub(crate) fn take(&mut self) -> String {
        let line = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_index = None;
        self.draft = None;
        self.undo.clear();
        self.typing = false;
        if !line.trim().is_empty() {
            self.history.push(line.clone());
        }
        line
    }

    /// Borrows the draft text.
    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns the grapheme cursor as a byte index on a boundary.
    #[must_use]
    pub(crate) const fn cursor(&self) -> usize {
        self.cursor
    }

    fn checkpoint(&mut self) {
        if self
            .undo
            .last()
            .is_some_and(|(text, cursor)| *text == self.text && *cursor == self.cursor)
        {
            return;
        }
        if self.undo.len() == UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.undo.push((self.text.clone(), self.cursor));
    }

    fn remove(&mut self, range: std::ops::Range<usize>) {
        if range.is_empty() {
            return;
        }
        self.checkpoint();
        self.typing = false;
        self.history_index = None;
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.snap_forward();
    }

    fn kill(&mut self, range: std::ops::Range<usize>) {
        if range.is_empty() {
            return;
        }
        self.killed = self.text[range.clone()].to_owned();
        self.remove(range);
    }

    fn previous_boundary(&self, from: usize) -> usize {
        self.text[..from]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(byte, _)| byte)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.text[from..]
            .graphemes(true)
            .next()
            .map_or(self.text.len(), |cluster| from + cluster.len())
    }

    /// Start of the word the caret is in or after: skips spaces, then the word.
    fn word_start(&self, from: usize) -> usize {
        let mut at = from;
        while at > 0 && self.cluster_before(at).is_some_and(is_blank) {
            at = self.previous_boundary(at);
        }
        while at > 0
            && self
                .cluster_before(at)
                .is_some_and(|cluster| !is_blank(cluster))
        {
            at = self.previous_boundary(at);
        }
        at
    }

    /// End of the next word: skips spaces, then the word.
    fn word_end(&self, from: usize) -> usize {
        let mut at = from;
        while at < self.text.len() && self.cluster_at(at).is_some_and(is_blank) {
            at = self.next_boundary(at);
        }
        while at < self.text.len()
            && self
                .cluster_at(at)
                .is_some_and(|cluster| !is_blank(cluster))
        {
            at = self.next_boundary(at);
        }
        at
    }

    fn cluster_before(&self, at: usize) -> Option<&str> {
        self.text[..at].graphemes(true).next_back()
    }

    fn cluster_at(&self, at: usize) -> Option<&str> {
        self.text[at..].graphemes(true).next()
    }

    /// Byte bounds of the line holding `at`, excluding its newline.
    fn line_bounds(&self, at: usize) -> (usize, usize) {
        let start = self.text[..at].rfind('\n').map_or(0, |index| index + 1);
        let end = self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |index| at + index);
        (start, end)
    }

    /// The byte offset `column` graphemes into the line `start..end`, clamped to its end.
    fn column_offset(&self, start: usize, end: usize, column: usize) -> usize {
        self.text[start..end]
            .grapheme_indices(true)
            .nth(column)
            .map_or(end, |(byte, _)| start + byte)
    }

    /// Moves the cursor forward onto the next grapheme boundary of the draft.
    fn snap_forward(&mut self) {
        self.cursor = self.cursor.min(self.text.len());
        while !self.text.is_char_boundary(self.cursor) {
            self.cursor += 1;
        }
        if self.cursor == 0 || self.cursor == self.text.len() {
            return;
        }
        let at = self.cursor;
        let boundary = self
            .text
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .find(|byte| *byte >= at);
        self.cursor = boundary.unwrap_or(self.text.len());
    }
}

fn is_blank(cluster: &str) -> bool {
    cluster.chars().all(char::is_whitespace)
}

/// The wrapped rows of a draft and where its caret falls among them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Laid {
    /// Display text of each visual row, control characters escaped.
    pub(crate) rows: Vec<String>,
    /// The caret's visual row and cell column within `rows`.
    pub(crate) caret: (usize, usize),
}

/// Wraps `text` into rows of at most `cap` cells and locates byte `cursor` among them.
///
/// A cluster wider than the room left starts the next row, so no wide cluster
/// splits. A caret after a full row's last cell opens an empty row for it.
pub(crate) fn layout(text: &str, cursor: usize, cap: usize, mode: WidthMode) -> Laid {
    let cap = cap.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut caret = (0, 0);
    let mut offset = 0;
    for line in text.split('\n') {
        let mut row = String::new();
        let mut used = 0;
        for cluster in line.graphemes(true) {
            let shown = escape(cluster);
            let cells = width(&shown, mode);
            if used > 0 && used + cells > cap {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            if offset == cursor {
                caret = (rows.len(), used);
            }
            row.push_str(&shown);
            used += cells;
            offset += cluster.len();
        }
        if offset == cursor {
            caret = if used >= cap {
                rows.push(std::mem::take(&mut row));
                (rows.len(), 0)
            } else {
                (rows.len(), used)
            };
        }
        rows.push(row);
        offset += 1;
    }
    Laid { rows, caret }
}
/// Removes the trailing grapheme cluster from `text`, if any.
///
/// `String::pop` removes one `char`, which splits flags, ZWJ emoji, and
/// combining sequences and leaves a dangling joiner or a lone regional
/// indicator behind. Backspace in a terminal composer must delete the
/// whole cluster the user sees as one glyph.
pub(crate) fn pop_grapheme(text: &mut String) {
    if let Some((byte, _)) = text.grapheme_indices(true).next_back() {
        text.truncate(byte);
    }
}

/// The slash command and partial argument under the cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SlashCompletion {
    /// The command name as written, without the leading slash.
    pub(crate) name: String,
    /// The partial argument being typed.
    pub(crate) prefix: String,
}

/// Splits a `/name tail` draft into its command name and partial argument.
///
/// The tail lexes through the fixed command lexer, so quoted spans complete
/// as one value. A draft ending in a separator starts a fresh argument. A
/// mid-typing lexer failure clips the prefix at the failing token, so an
/// unclosed quote still completes its intended value. Plain text drafts
/// complete nothing.
#[must_use]
pub(crate) fn slash_completion(draft: &str) -> Option<SlashCompletion> {
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
    use super::{Composer, SlashCompletion, layout, pop_grapheme, slash_completion};
    use crate::width::WidthMode;

    #[test]
    fn pop_grapheme_removes_whole_clusters() {
        let mut text = String::from("ab🇯🇵");
        pop_grapheme(&mut text);
        assert_eq!(text, "ab");
        let mut text = String::from("ab👨‍👩‍👧");
        pop_grapheme(&mut text);
        assert_eq!(text, "ab");
        let mut text = String::from("a\u{301}漢");
        pop_grapheme(&mut text);
        assert_eq!(text, "a\u{301}");
        // The decomposed é is one cluster: base and mark leave together.
        pop_grapheme(&mut text);
        assert_eq!(text, "");
        // A decomposed 한 is three jamo in one cluster: one pop clears it.
        let mut text = String::from("x\u{1112}\u{1161}\u{11ab}");
        pop_grapheme(&mut text);
        assert_eq!(text, "x");
        let mut text = String::new();
        pop_grapheme(&mut text);
        assert_eq!(text, "");
    }

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

    #[test]
    fn typing_and_deleting_happen_at_the_caret() {
        let mut composer = Composer::default();
        composer.set("abcdef");
        composer.move_left();
        composer.move_left();
        composer.insert("X");
        assert_eq!(composer.text(), "abcdXef");
        composer.backspace();
        composer.delete();
        assert_eq!(composer.text(), "abcdf");
        composer.line_start();
        composer.insert("> ");
        composer.line_end();
        composer.insert("!");
        assert_eq!(composer.text(), "> abcdf!");
    }

    #[test]
    fn deleting_never_splits_a_cluster() {
        let mut composer = Composer::default();
        composer.set("a🇯🇵b");
        composer.move_left();
        composer.backspace();
        assert_eq!(composer.text(), "ab");
        let mut composer = Composer::default();
        composer.set("e\u{301}x");
        composer.line_start();
        composer.delete();
        assert_eq!(composer.text(), "x");
    }

    #[test]
    fn words_kills_yank_and_undo() {
        let mut composer = Composer::default();
        composer.set("one two three");
        composer.word_left();
        composer.delete_word_back();
        assert_eq!(composer.text(), "one three");
        composer.line_end();
        composer.yank();
        assert_eq!(composer.text(), "one threetwo ");
        composer.undo();
        assert_eq!(composer.text(), "one three");
        composer.line_start();
        composer.delete_word_forward();
        assert_eq!(composer.text(), " three");
        composer.kill_line_end();
        assert_eq!(composer.text(), "");
        composer.yank();
        assert_eq!(composer.text(), " three");
    }

    #[test]
    fn up_and_down_keep_the_column_across_lines() {
        let mut composer = Composer::default();
        composer.set("abcd\nef\ngh");
        composer.move_left();
        composer.move_up();
        assert_eq!(&composer.text()[composer.cursor()..], "f\ngh");
        composer.move_up();
        assert_eq!(&composer.text()[composer.cursor()..], "bcd\nef\ngh");
        assert!(composer.on_first_line());
        composer.move_down();
        composer.move_down();
        assert!(composer.on_last_line());
    }

    #[test]
    fn history_recall_returns_the_unsent_draft() {
        let mut composer = Composer::default();
        composer.insert("sent");
        composer.take();
        composer.insert("draft");
        composer.history_prev();
        assert_eq!(composer.text(), "sent");
        composer.history_next();
        assert_eq!(composer.text(), "draft");
    }

    #[test]
    fn layout_wraps_whole_clusters_and_places_the_caret() {
        let laid = layout("ab漢cd", 5, 3, WidthMode::Narrow);
        assert_eq!(laid.rows, ["ab", "漢c", "d"]);
        assert_eq!(laid.caret, (1, 2));
        let laid = layout("abc", 3, 3, WidthMode::Narrow);
        assert_eq!(laid.rows, ["abc", ""]);
        assert_eq!(laid.caret, (1, 0));
        let laid = layout("one\ntwo", 5, 20, WidthMode::Narrow);
        assert_eq!(laid.rows, ["one", "two"]);
        assert_eq!(laid.caret, (1, 1));
        let laid = layout("", 0, 20, WidthMode::Narrow);
        assert_eq!(laid.rows, [""]);
        assert_eq!(laid.caret, (0, 0));
    }
}
