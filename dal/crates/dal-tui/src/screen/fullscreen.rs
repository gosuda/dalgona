//! Alternate-screen renderer: header, notices, viewport, bottom stack, search.

use std::cell::Cell;

/// Fullscreen scroll, follow, and search state.
///
/// The follow machine is explicit and total: `follow` holds exactly when the
/// window sticks to the live edge. Scrolling up detaches and freezes the
/// window at an absolute row, so new content never moves the view; End
/// re-attaches; a page never passes the oldest row.
#[derive(Debug, Clone)]
pub(crate) struct Viewport {
    follow: bool,
    /// Absolute top row of the frozen window while detached.
    anchor: usize,
    /// Search filter text, while the filter row is open.
    search: Option<String>,
    /// Visible window height observed at the last render; one page of scrolling.
    page: Cell<usize>,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::following()
    }
}

impl Viewport {
    /// Creates a following viewport.
    #[must_use]
    pub(crate) fn following() -> Self {
        Self {
            follow: true,
            anchor: 0,
            search: None,
            page: Cell::new(0),
        }
    }

    /// Whether the window sticks to the live edge.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn follows(&self) -> bool {
        self.follow
    }

    /// Remembers the visible window height: one page of scrolling.
    pub(crate) fn observe(&self, height: usize) {
        self.page.set(height);
    }

    /// Top row of the visible window for `rows` transcript rows and a window
    /// `height` rows tall. A detached view stays frozen at its anchor even as
    /// rows arrive; a resize that grows the window pulls the top down so the
    /// window never passes the live edge.
    #[must_use]
    pub(crate) fn window_top(&self, rows: usize, height: usize) -> usize {
        if self.follow {
            return rows.saturating_sub(height);
        }
        self.anchor.min(rows.saturating_sub(height))
    }

    /// Page up: detaches and moves the frozen window one page toward the
    /// oldest row. A page is the visible window height, never one row.
    pub(crate) fn scroll_up(&mut self, rows: usize) {
        let page = self.page.get().max(1);
        if self.follow {
            self.anchor = rows.saturating_sub(page);
            self.follow = false;
        }
        self.anchor = self.anchor.saturating_sub(page);
    }

    /// Page down: moves the frozen window one page toward the live edge,
    /// never past a full window of newest rows. A following view is already
    /// at the edge.
    pub(crate) fn scroll_down(&mut self, rows: usize) {
        if self.follow {
            return;
        }
        let page = self.page.get().max(1);
        self.anchor = self
            .anchor
            .saturating_add(page)
            .min(rows.saturating_sub(self.page.get().max(1)));
    }

    /// Jumps the frozen window to `row`, as a search match does.
    pub(crate) fn jump_to(&mut self, row: usize) {
        self.follow = false;
        self.anchor = row;
    }

    /// Re-attaches follow at the live edge.
    pub(crate) fn jump_latest(&mut self) {
        self.follow = true;
        self.anchor = 0;
    }

    /// Opens the filter row over the viewport top.
    pub(crate) fn open_search(&mut self) {
        self.search = Some(String::new());
    }

    /// Closes search and returns focus to the composer.
    pub(crate) fn close_search(&mut self) {
        self.search = None;
    }

    /// The open filter text, while the row shows.
    #[must_use]
    pub(crate) fn search_text(&self) -> Option<&str> {
        self.search.as_deref()
    }

    /// Types one character into the open filter row; closed stays closed.
    pub(crate) fn type_search(&mut self, character: char) {
        if let Some(query) = &mut self.search {
            query.push(character);
        }
    }

    /// Removes the last character from the open filter row.
    pub(crate) fn backspace_search(&mut self) {
        if let Some(query) = &mut self.search {
            query.pop();
        }
    }

    /// The follow-stopped cue while the view is detached.
    #[must_use]
    pub(crate) fn cue(&self) -> Option<&'static str> {
        (!self.follow).then_some(crate::copy::ids::FOLLOW_STOPPED)
    }
}

/// Returns the header row count: one row when wide and tall enough.
#[must_use]
pub(crate) const fn header_rows(width: u16, height: u16) -> usize {
    if width >= 60 && height >= 14 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::{Viewport, header_rows};

    #[test]
    fn header_hides_below_width_or_height_floors() {
        assert_eq!(header_rows(120, 40), 1);
        assert_eq!(header_rows(59, 40), 0);
        assert_eq!(header_rows(80, 13), 0);
    }

    #[test]
    fn scroll_detaches_and_end_reattaches() {
        let mut viewport = Viewport::following();
        viewport.scroll_up(60);
        assert_eq!(
            viewport.cue(),
            Some("following stopped · press end to jump to the latest")
        );
        assert!(!viewport.follows());
        viewport.jump_latest();
        assert_eq!(viewport.cue(), None);
        assert!(viewport.follows());
    }

    #[test]
    fn default_viewport_follows_the_live_edge() {
        let viewport = Viewport::default();
        assert!(viewport.follows());
        assert_eq!(viewport.window_top(40, 10), 30);
    }

    #[test]
    fn scrolling_moves_a_page_not_one_row() {
        let mut viewport = Viewport::following();
        viewport.observe(10);
        viewport.scroll_up(60);
        // The window top was 50; one page of 10 moves it to 40.
        assert_eq!(viewport.window_top(60, 10), 40);
        viewport.scroll_up(60);
        assert_eq!(viewport.window_top(60, 10), 30);
    }

    #[test]
    fn scrolling_past_the_top_stays_at_the_oldest_row() {
        let mut viewport = Viewport::following();
        viewport.observe(10);
        for _ in 0..20 {
            viewport.scroll_up(60);
        }
        assert_eq!(viewport.window_top(60, 10), 0);
        assert!(!viewport.follows());
        viewport.jump_latest();
        assert!(viewport.follows());
        assert_eq!(viewport.window_top(60, 10), 50);
    }

    #[test]
    fn detaching_freezes_the_window_while_rows_arrive() {
        let mut viewport = Viewport::following();
        viewport.observe(10);
        viewport.scroll_up(60);
        assert_eq!(viewport.window_top(60, 10), 40);
        // Twenty new rows arrive; the frozen window does not move.
        assert_eq!(viewport.window_top(80, 10), 40);
    }

    #[test]
    fn page_down_clamps_at_the_newest_window() {
        let mut viewport = Viewport::following();
        viewport.observe(10);
        viewport.scroll_up(60);
        for _ in 0..20 {
            viewport.scroll_down(60);
        }
        assert_eq!(viewport.window_top(60, 10), 50);
        assert!(!viewport.follows(), "only End re-attaches follow");
        viewport.jump_latest();
        assert_eq!(viewport.window_top(60, 10), 50);
    }

    #[test]
    fn page_down_while_following_changes_nothing() {
        let mut viewport = Viewport::following();
        viewport.observe(10);
        viewport.scroll_down(60);
        assert!(viewport.follows());
        assert_eq!(viewport.window_top(60, 10), 50);
    }

    #[test]
    fn jump_to_pins_a_row_and_end_returns_to_follow() {
        let mut viewport = Viewport::following();
        viewport.observe(10);
        viewport.jump_to(7);
        assert_eq!(viewport.window_top(60, 10), 7);
        viewport.jump_latest();
        assert!(viewport.follows());
        assert_eq!(viewport.window_top(60, 10), 50);
    }

    #[test]
    fn search_opens_types_and_closes() {
        let mut viewport = Viewport::following();
        assert_eq!(viewport.search_text(), None);
        viewport.open_search();
        assert_eq!(viewport.search_text(), Some(""));
        viewport.type_search('a');
        viewport.type_search('b');
        viewport.backspace_search();
        assert_eq!(viewport.search_text(), Some("a"));
        viewport.close_search();
        assert_eq!(viewport.search_text(), None);
    }
}
