//! Write-once settled transcript; committed rows are never repainted.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use dal_core::{Update, UpdateKind};

use crate::render::{PixelImage, RenderLink, RenderRow, RenderSpan};
use crate::theme::Role;

/// Settled transcript rows with exactly-once commit per entry id.
#[derive(Debug, Default)]
pub struct Transcript {
    rows: Vec<String>,
    styles: Vec<Vec<RenderSpan>>,
    links: Vec<Vec<RenderLink>>,
    images: Vec<Option<PixelImage>>,
    image_tails: Vec<bool>,
    /// Marks the last row of each committed block; the inline screen sets
    /// one blank row after every such row.
    block_ends: Vec<bool>,
    committed: HashSet<String>,
    pending: Vec<(String, Vec<RenderRow>)>,
    tool_durations: HashMap<String, Duration>,
    search: RefCell<SearchCount>,
}

/// Matches of one query over the first `scanned` rows; `needle` is its lowercase form.
#[derive(Debug, Default)]
struct SearchCount {
    query: String,
    needle: String,
    scanned: usize,
    count: usize,
}

impl Transcript {
    /// Remembers how long the tool call `call` ran, for its settled card.
    pub(crate) fn note_tool_duration(&mut self, call: &str, duration: Duration) {
        self.tool_durations.insert(call.to_owned(), duration);
    }

    /// Returns how long the tool call `call` ran, when this client watched it.
    pub(crate) fn tool_duration(&self, call: &str) -> Option<Duration> {
        self.tool_durations.get(call).copied()
    }

    /// Appends settled rows for `entry_id` once; repeats return an empty vec.
    pub fn commit(&mut self, entry_id: &str, rows: &[String]) -> Vec<String> {
        if !self.committed.insert(entry_id.to_owned()) {
            return Vec::new();
        }
        let rendered = rows
            .iter()
            .map(|row| RenderRow::new(row.clone(), Role::Text))
            .collect::<Vec<_>>();
        self.rows
            .extend(rendered.iter().map(|row| row.text.clone()));
        self.styles
            .extend(rendered.iter().map(|row| row.spans.clone()));
        self.links
            .extend(rendered.iter().map(|row| row.links.clone()));
        self.images.extend((0..rows.len()).map(|_| None));
        self.image_tails.extend((0..rows.len()).map(|_| false));
        self.block_ends
            .extend((0..rows.len()).map(|index| index + 1 == rows.len()));
        rendered.into_iter().map(|row| row.text).collect()
    }

    /// Appends rendered rows with their theme roles exactly once.
    pub(crate) fn commit_rendered(&mut self, entry_id: &str, rows: &[RenderRow]) -> Vec<String> {
        self.clear_pending(entry_id);
        if !self.committed.insert(entry_id.to_owned()) {
            return Vec::new();
        }
        for (index, row) in rows.iter().enumerate() {
            self.rows.push(row.text.clone());
            self.styles.push(row.spans.clone());
            self.links.push(row.links.clone());
            self.images.push(row.image.clone());
            self.image_tails.push(row.image_tail);
            self.block_ends.push(index + 1 == rows.len());
        }
        rows.iter().map(|row| row.text.clone()).collect()
    }

    /// Reports whether row `index` is the last row of its committed block.
    pub(crate) fn closes_block(&self, index: usize) -> bool {
        self.block_ends.get(index).copied().unwrap_or(false)
    }

    pub(crate) fn set_pending(&mut self, entry_id: &str, rows: Vec<RenderRow>) {
        if rows.is_empty() {
            self.clear_pending(entry_id);
            return;
        }
        if let Some((_, current)) = self.pending.iter_mut().find(|(id, _)| id == entry_id) {
            *current = rows;
        } else {
            self.pending.push((entry_id.to_owned(), rows));
        }
    }

    pub(crate) fn clear_pending(&mut self, entry_id: &str) {
        self.pending.retain(|(id, _)| id != entry_id);
    }

    pub(crate) fn is_committed(&self, entry_id: &str) -> bool {
        self.committed.contains(entry_id)
    }

    pub(crate) fn pending_rows(&self) -> impl Iterator<Item = &RenderRow> {
        self.pending.iter().flat_map(|(_, rows)| rows.iter())
    }

    /// Borrows one row and its semantic style spans.
    pub(crate) fn row_data(&self, index: usize) -> Option<(&str, &[RenderSpan])> {
        Some((self.rows.get(index)?, self.styles.get(index)?))
    }

    /// Returns one owned row for current-frame projection.
    pub(crate) fn render_row(&self, index: usize) -> Option<RenderRow> {
        let (text, spans) = self.row_data(index)?;
        Some(RenderRow {
            text: text.to_owned(),
            role: Role::Text,
            color: ratatui::style::Color::Reset,
            spans: spans.to_vec(),
            links: self.links.get(index).cloned().unwrap_or_default(),
            pending_diagram: false,
            image: self.images.get(index).cloned().flatten(),
            image_tail: self.image_tails.get(index).copied().unwrap_or(false),
            cursor: None,
        })
    }

    /// Applies an update header; unknown kinds and fields change nothing.
    pub fn apply_update(&mut self, update: &Update) -> bool {
        !matches!(update.kind, UpdateKind::Unknown)
    }

    /// Borrows committed rows in order.
    #[must_use]
    pub fn rows(&self) -> &[String] {
        &self.rows
    }

    /// Counts committed rows containing `query`, ignoring case.
    ///
    /// Rows only append, so the count is carried across calls: a repeat query
    /// scans just the rows committed since the last call, and a changed query
    /// rescans once.
    pub(crate) fn search_matches(&self, query: &str) -> usize {
        let mut state = self.search.borrow_mut();
        if state.query != query {
            query.clone_into(&mut state.query);
            state.needle = query.to_lowercase();
            state.scanned = 0;
            state.count = 0;
        }
        let fresh = self.rows[state.scanned..]
            .iter()
            .filter(|row| row.to_lowercase().contains(&state.needle))
            .count();
        state.count += fresh;
        state.scanned = self.rows.len();
        state.count
    }
}

#[cfg(test)]
mod tests {
    use super::Transcript;
    use crate::render::RenderRow;
    use crate::theme::Role;
    use dal_core::{Gen, Seq, Update, UpdateKind};

    fn update(kind: UpdateKind) -> Update {
        Update {
            r#gen: Gen::new(core::num::NonZeroU64::MIN),
            seq: Seq::new(core::num::NonZeroU64::MIN),
            kind,
        }
    }

    #[test]
    fn unknown_updates_change_no_cell_and_raise_no_error() {
        let mut transcript = Transcript::default();
        assert!(!transcript.apply_update(&update(UpdateKind::Unknown)));
        assert_eq!(transcript.rows(), [] as [String; 0]);
    }

    #[test]
    fn committed_entries_freeze_and_commit_once() {
        let mut transcript = Transcript::default();
        let rows = vec!["hello".to_owned()];
        assert_eq!(transcript.commit("e1", &rows), rows);
        assert_eq!(transcript.commit("e1", &rows), [] as [String; 0]);
        assert_eq!(transcript.rows(), &rows);
    }

    #[test]
    fn search_matches_carry_across_commits_and_reset_on_a_new_query() {
        let mut transcript = Transcript::default();
        transcript.commit("e1", &["Alpha one".to_owned(), "beta".to_owned()]);
        assert_eq!(transcript.search_matches("alpha"), 1);
        assert_eq!(transcript.search_matches("alpha"), 1);
        transcript.commit("e2", &["ALPHA two".to_owned(), "gamma".to_owned()]);
        assert_eq!(transcript.search_matches("alpha"), 2);
        assert_eq!(transcript.search_matches("BETA"), 1);
        assert_eq!(transcript.search_matches("alpha"), 2);
        assert_eq!(transcript.search_matches("absent"), 0);
    }
    #[test]
    fn pending_diagram_rows_do_not_enter_committed_transcript() {
        let mut transcript = Transcript::default();
        let mut pending = RenderRow::new("diagram d2 rendering…", Role::Dim);
        pending.pending_diagram = true;
        transcript.set_pending("e1", vec![pending]);

        assert_eq!(transcript.rows(), [] as [String; 0]);
        assert!(
            transcript
                .pending_rows()
                .any(|row| row.pending_diagram && row.text == "diagram d2 rendering…")
        );

        transcript.clear_pending("e1");
        assert!(transcript.pending_rows().next().is_none());
    }
}
