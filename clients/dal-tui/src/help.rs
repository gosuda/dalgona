//! A scrollable F1 overlay over the shared key table.

use std::cell::Cell;

use crate::keys::help_labels;
use crate::render::RenderRow;
use crate::theme::Role;

/// The F1 help overlay, open while it owns the composer and hint rows.
#[derive(Debug)]
pub(crate) struct HelpUi {
    labels: Vec<(String, &'static str)>,
    /// First key-table row at the top of the window.
    scroll: usize,
    /// Visible window height observed at the last render; one page of scrolling.
    page: Cell<usize>,
}

impl Default for HelpUi {
    fn default() -> Self {
        Self {
            labels: help_labels(),
            scroll: 0,
            page: Cell::new(0),
        }
    }
}

impl HelpUi {
    pub(crate) fn height(&self) -> usize {
        self.labels.len()
    }

    /// The overlay rows, at most `max_rows` when the table has more rows to
    /// give. The last row counts the keys still below the window, so every
    /// binding stays reachable on short terminals.
    pub(crate) fn rows(&self, max_rows: usize) -> Vec<RenderRow> {
        let total = self.labels.len();
        let footer = usize::from(total > max_rows);
        let window = max_rows.saturating_sub(footer).max(1).min(total);
        self.page.set(window);
        let scroll = self.scroll.min(total - window);
        let mut rows: Vec<RenderRow> = self.labels[scroll..scroll + window]
            .iter()
            .map(|(key, label)| RenderRow::new(format!("{key} {label}"), Role::Text))
            .collect();
        let remaining = total - scroll - window;
        if remaining > 0 {
            let count = u64::try_from(remaining).unwrap_or(u64::MAX);
            // The footer names the key that reads the rows it counts, the
            // same affordance the dialog body's `... {n} more lines · pgdn`
            // carries.
            rows.push(RenderRow::new(
                format!(
                    "{} · pgdn",
                    crate::copy::render(
                        crate::copy::ids::HELP_MORE,
                        &[("n", &count.to_string())],
                        count,
                    )
                ),
                Role::Dim,
            ));
        }
        rows
    }

    /// Highest first-row offset a window of the observed height can take.
    fn limit(&self) -> usize {
        self.labels.len().saturating_sub(self.page.get().max(1))
    }

    /// Page up: moves the window one page toward the first key.
    pub(crate) fn page_up(&mut self) {
        let page = self.page.get().max(1);
        self.scroll = self.scroll.min(self.limit()).saturating_sub(page);
    }

    /// Page down: moves the window one page toward the last key.
    pub(crate) fn page_down(&mut self) {
        let page = self.page.get().max(1);
        self.scroll = self.scroll.saturating_add(page).min(self.limit());
    }

    /// Scrolls one row toward the first key.
    pub(crate) fn line_up(&mut self) {
        self.scroll = self.scroll.min(self.limit()).saturating_sub(1);
    }

    /// Scrolls one row toward the last key.
    pub(crate) fn line_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1).min(self.limit());
    }
}

#[cfg(test)]
mod tests {
    use super::{HelpUi, help_labels};

    fn row_text(index: usize) -> String {
        let (key, label) = &help_labels()[index];
        format!("{key} {label}")
    }

    #[test]
    fn rows_show_every_key_when_the_table_fits() {
        let help = HelpUi::default();
        let total = help_labels().len();
        let rows = help.rows(total);
        assert_eq!(rows.len(), total);
        assert!(rows.iter().all(|row| !row.text.starts_with("... ")));
        assert!(rows.iter().any(|row| row.text == row_text(1)));
    }

    #[test]
    fn clipped_rows_reserve_a_footer_counting_the_keys_below() {
        let help = HelpUi::default();
        let total = help_labels().len();
        let rows = help.rows(total - 2);
        assert_eq!(rows.len(), total - 2);
        assert_eq!(
            rows.last().expect("the overflow footer").text,
            "... 3 more keys · pgdn"
        );
        assert!(
            rows[..rows.len() - 1]
                .iter()
                .all(|row| !row.text.starts_with("... "))
        );
    }

    #[test]
    fn scrolling_clamps_at_both_ends_and_after_a_shrink() {
        let mut help = HelpUi::default();
        let total = help_labels().len();
        help.page.set(4);
        help.page_down();
        assert_eq!(help.scroll, 4);
        help.line_down();
        assert_eq!(help.scroll, 5);
        help.line_up();
        assert_eq!(help.scroll, 4);
        help.page_up();
        assert_eq!(help.scroll, 0);
        help.page_up();
        assert_eq!(help.scroll, 0, "scrolling never leaves the first key");

        // A resize that shrinks the window clamps the stored offset at the
        // next render: the last table rows show with no footer below them.
        help.scroll = total;
        let rows = help.rows(4);
        assert_eq!(rows.len(), 3, "three rows and no footer at the bottom");
        assert_eq!(rows[0].text, row_text(total - 3));
        assert_eq!(rows[2].text, row_text(total - 1));
        help.page_down();
        assert_eq!(
            help.scroll,
            total - 3,
            "the next page starts from the clamp"
        );
    }

    #[test]
    fn page_scroll_preserves_visible_boundaries_as_the_key_table_grows() {
        let mut help = HelpUi::default();
        let total = help_labels().len();
        let first_page = help.rows(6);
        let visible_keys = first_page.len() - 1;
        help.page_down();
        assert_eq!(help.rows(6)[0].text, row_text(visible_keys));
        for _ in 0..total {
            help.page_down();
        }
        assert_eq!(
            help.rows(6).last().expect("the last visible key").text,
            row_text(total - 1)
        );
        help.page_up();
        assert_eq!(help.rows(6)[0].text, row_text(total - visible_keys * 2));
    }
}
