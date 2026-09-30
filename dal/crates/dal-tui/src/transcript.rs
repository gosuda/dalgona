//! Write-once settled transcript; committed rows are never repainted.

use std::collections::HashSet;

use dal_core::{Update, UpdateKind};

use crate::render::{PixelImage, RenderRow, RenderSpan};
use crate::theme::Role;

/// Settled transcript rows with exactly-once commit per entry id.
#[derive(Debug, Default)]
pub struct Transcript {
    rows: Vec<String>,
    styles: Vec<Vec<RenderSpan>>,
    images: Vec<Option<PixelImage>>,
    image_tails: Vec<bool>,
    committed: HashSet<String>,
    pending: Vec<(String, Vec<RenderRow>)>,
}

impl Transcript {
    /// Appends settled rows for `entry_id` once; repeats return an empty vec.
    pub fn commit(&mut self, entry_id: &str, rows: &[String]) -> Vec<String> {
        if !self.committed.insert(entry_id.to_owned()) {
            return Vec::new();
        }
        self.rows.extend(rows.iter().cloned());
        self.styles.extend((0..rows.len()).map(|_| Vec::new()));
        self.images.extend((0..rows.len()).map(|_| None));
        self.image_tails.extend((0..rows.len()).map(|_| false));
        rows.to_vec()
    }

    /// Appends rendered rows with their theme roles exactly once.
    pub(crate) fn commit_rendered(&mut self, entry_id: &str, rows: &[RenderRow]) -> Vec<String> {
        self.clear_pending(entry_id);
        if !self.committed.insert(entry_id.to_owned()) {
            return Vec::new();
        }
        for row in rows {
            self.rows.push(row.text.clone());
            self.styles.push(row.spans.clone());
            self.images.push(row.image.clone());
            self.image_tails.push(row.image_tail);
        }
        rows.iter().map(|row| row.text.clone()).collect()
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
            pending_diagram: false,
            image: self.images.get(index).cloned().flatten(),
            image_tail: self.image_tails.get(index).copied().unwrap_or(false),
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
        assert!(transcript.rows().is_empty());
    }

    #[test]
    fn committed_entries_freeze_and_commit_once() {
        let mut transcript = Transcript::default();
        let rows = vec!["hello".to_owned()];
        assert_eq!(transcript.commit("e1", &rows), rows);
        assert!(transcript.commit("e1", &rows).is_empty());
        assert_eq!(transcript.rows(), &rows);
    }
    #[test]
    fn pending_diagram_rows_do_not_enter_committed_transcript() {
        let mut transcript = Transcript::default();
        let mut pending = RenderRow::new("diagram d2 rendering…", Role::Dim);
        pending.pending_diagram = true;
        transcript.set_pending("e1", vec![pending]);

        assert!(transcript.rows().is_empty());
        assert!(
            transcript
                .pending_rows()
                .any(|row| row.pending_diagram && row.text == "diagram d2 rendering…")
        );

        transcript.clear_pending("e1");
        assert!(transcript.pending_rows().next().is_none());
    }
}
