//! Alternate-screen renderer: header, notices, viewport, bottom stack, search.

use std::cell::{Cell, RefCell};
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use crate::transcript::Transcript;

/// One end of the fullscreen keyboard selection: an absolute row of the
/// selectable space (a committed transcript row or a visible live assistant
/// row) and a byte offset on a grapheme boundary within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionPoint {
    pub(crate) row: usize,
    pub(crate) offset: usize,
}

/// The fullscreen transcript selection: an anchor that stays where the
/// selection started and a focus that Shift+arrows move. Either end may sit
/// before the other; copy and render always use the ordered range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) anchor: SelectionPoint,
    pub(crate) focus: SelectionPoint,
}

impl Selection {
    /// The selection ends in document order.
    fn ordered(&self) -> (SelectionPoint, SelectionPoint) {
        let backwards = self.focus.row < self.anchor.row
            || (self.focus.row == self.anchor.row && self.focus.offset < self.anchor.offset);
        if backwards {
            (self.focus, self.anchor)
        } else {
            (self.anchor, self.focus)
        }
    }

    /// The selected byte range inside one transcript row, if the selection
    /// covers any of it. An empty range means the selection only passes
    /// through the row edge and marks nothing here.
    pub(crate) fn row_span(&self, row: usize, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if row < start.row || row > end.row || start == end {
            return None;
        }
        let begin = (if row == start.row { start.offset } else { 0 }).min(len);
        let stop = (if row == end.row { end.offset } else { len }).min(len);
        (begin < stop).then_some(begin..stop)
    }
}

/// One Shift+arrow step of the selection focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionStep {
    Left,
    Right,
    Up,
    Down,
}

/// The fullscreen selectable row space: committed transcript rows in order,
/// then the visible live assistant rows of one rendered frame. Committed row
/// text is never copied here; it resolves against the transcript, which is
/// write-once and append-only, so a committed index is a stable identity.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SelectableSpace {
    committed_len: usize,
    live: Vec<Box<str>>,
}

impl SelectableSpace {
    /// A space of `committed_len` committed rows and no live rows.
    pub(crate) fn committed_only(committed_len: usize) -> Self {
        Self {
            committed_len,
            live: Vec::new(),
        }
    }

    /// The frame's space: every committed row plus the rendered live rows.
    pub(crate) fn new(committed_len: usize, live: Vec<Box<str>>) -> Self {
        Self {
            committed_len,
            live,
        }
    }

    /// Rows the space names: committed rows then live rows.
    pub(crate) fn row_count(&self) -> usize {
        self.committed_len + self.live.len()
    }
}

/// Fullscreen scroll, follow, search, and keyboard-selection state.
///
/// The follow machine is explicit and total: `follow` holds exactly when the
/// window sticks to the live edge. Scrolling up detaches and freezes the
/// window at an absolute row, so new content never moves the view; End
/// re-attaches; a page never passes the oldest row.
///
/// The keyboard selection exists only on the fullscreen screen: `apply_event`
/// creates it from Shift+arrows there and never on the inline screen, where
/// the terminal's native selection applies.
#[derive(Debug, Clone)]
pub(crate) struct Viewport {
    follow: bool,
    /// Absolute top row of the frozen window while detached.
    anchor: usize,
    /// Search filter text, while the filter row is open.
    search: Option<String>,
    /// Visible window height observed at the last render; one page of scrolling.
    page: Cell<usize>,
    /// The keyboard selection, while one is active. Interior mutability lets
    /// the renderer reconcile selection points with the row space it is
    /// about to cache, through the shared `&Viewport` in `FrameInput`.
    selection: RefCell<Option<Selection>>,
    /// The selectable row space of the last rendered frame: committed rows
    /// plus the visible live assistant rows.
    space: RefCell<SelectableSpace>,
    /// Whether a frame has rendered yet. Before the first render the
    /// committed transcript rows are the whole selectable space.
    rendered: Cell<bool>,
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
            selection: RefCell::new(None),
            space: RefCell::new(SelectableSpace::default()),
            rendered: Cell::new(false),
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

    /// Highest top row a window of the observed height can take over `rows`.
    fn limit(&self, rows: usize) -> usize {
        rows.saturating_sub(self.page.get().max(1))
    }

    /// Page up: detaches and moves the frozen window one page toward the
    /// oldest row. A page is the visible window height, never one row.
    pub(crate) fn scroll_up(&mut self, rows: usize) {
        let page = self.page.get().max(1);
        if self.follow {
            self.anchor = self.limit(rows);
            self.follow = false;
        }
        self.anchor = self.anchor.min(self.limit(rows)).saturating_sub(page);
    }

    /// Page down: moves the frozen window one page toward the live edge,
    /// never past a full window of newest rows. A following view is already
    /// at the edge.
    pub(crate) fn scroll_down(&mut self, rows: usize) {
        if self.follow {
            return;
        }
        let page = self.page.get().max(1);
        self.anchor = self.anchor.saturating_add(page).min(self.limit(rows));
    }

    /// Pins the window top to `row`, as a search match does, clamped so the
    /// frozen window stays a full window inside the transcript.
    pub(crate) fn jump_to(&mut self, row: usize, rows: usize) {
        self.follow = false;
        self.anchor = row.min(self.limit(rows));
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

    /// The active keyboard selection, when one exists.
    #[must_use]
    pub(crate) fn selection(&self) -> Option<Selection> {
        *self.selection.borrow()
    }

    /// Drops the keyboard selection; the next Shift+arrow starts a new one.
    pub(crate) fn clear_selection(&mut self) {
        *self.selection.borrow_mut() = None;
    }

    /// Caches the frame's selectable row space: every committed transcript
    /// row, then the visible live assistant rows. Selection points that named
    /// live rows of the previous frame are re-identified by exact content
    /// against the rows that committed or re-rendered since; a point that no
    /// longer identifies any row drops the selection instead of letting it
    /// name different text.
    pub(crate) fn update_space(&self, space: SelectableSpace, transcript: &Transcript) {
        if self.rendered.get() {
            let prior = self.space.borrow();
            self.reconcile(&prior, &space, transcript);
        }
        *self.space.borrow_mut() = space;
        self.rendered.set(true);
    }

    /// Re-identifies live-range selection points after the row space changes
    /// between renders. `from` is the space the selection was made in.
    /// Committed rows are write-once and append-only, so a point naming a
    /// committed row never moves. An unchanged live prefix with only its last
    /// row extended keeps its points in place. Other live changes require
    /// exact row matches in newly committed rows or the next live range.
    /// An unmatched row clears the selection.
    fn reconcile(&self, from: &SelectableSpace, to: &SelectableSpace, transcript: &Transcript) {
        let mut selection = self.selection.borrow_mut();
        let Some(current) = *selection else {
            return;
        };
        if current.anchor.row < from.committed_len && current.focus.row < from.committed_len {
            return;
        }
        if from.committed_len == to.committed_len
            && let Some((tail, prefix)) = from.live.split_last()
            && to.live.len() >= from.live.len()
            && to.live[..prefix.len()] == *prefix
            && to.live[prefix.len()].starts_with(tail.as_ref())
        {
            return;
        }
        let grown = to.committed_len.min(transcript.rows().len());
        if grown < from.committed_len {
            *selection = None;
            return;
        }
        let fresh = &transcript.rows()[from.committed_len..grown];
        let mut row_map: Vec<usize> = Vec::with_capacity(from.live.len());
        let mut committed_at = 0usize;
        let mut live_at = 0usize;
        let mut matched = true;
        'rows: for live in &from.live {
            while committed_at < fresh.len() {
                if fresh[committed_at].as_str() == live.as_ref() {
                    row_map.push(from.committed_len + committed_at);
                    committed_at += 1;
                    continue 'rows;
                }
                committed_at += 1;
            }
            while live_at < to.live.len() {
                if to.live[live_at].as_ref() == live.as_ref() {
                    row_map.push(to.committed_len + live_at);
                    live_at += 1;
                    continue 'rows;
                }
                live_at += 1;
            }
            matched = false;
            break;
        }
        if !matched {
            *selection = None;
            return;
        }
        let remap = |point: SelectionPoint| {
            if point.row < from.committed_len {
                return point;
            }
            SelectionPoint {
                row: row_map[point.row - from.committed_len],
                ..point
            }
        };
        *selection = Some(Selection {
            anchor: remap(current.anchor),
            focus: remap(current.focus),
        });
    }

    /// Extends the keyboard selection one step, starting it at the bottom of
    /// the visible window when none is active. A step past the first or last
    /// space row extends nothing.
    pub(crate) fn extend_selection(&mut self, step: SelectionStep, transcript: &Transcript) {
        let space = self.selection_space(transcript);
        let (anchor, focus) = if let Some(selection) = *self.selection.borrow() {
            (selection.anchor, selection.focus)
        } else {
            let Some(origin) = self.selection_origin(&space, transcript) else {
                return;
            };
            (origin, origin)
        };
        let Some(next) = step_point(focus, step, &space, transcript) else {
            return;
        };
        *self.selection.borrow_mut() = Some(Selection {
            anchor,
            focus: next,
        });
    }

    /// The selected transcript text in document order, one `\n` between
    /// rows. Returns `None` when no selection is active or when it is empty
    /// (anchor equal to focus), so Ctrl+C falls through to interrupting.
    #[must_use]
    pub(crate) fn selected_text(&self, transcript: &Transcript) -> Option<String> {
        let space = self.selection_space(transcript);
        let selection = (*self.selection.borrow())?;
        let (start, end) = selection.ordered();
        if start == end {
            return None;
        }
        let count = space.row_count();
        if start.row >= count {
            return None;
        }
        let last = end.row.min(count - 1);
        let mut text = String::new();
        for row in start.row..=last {
            let line = row_text(&space, transcript, row)?;
            let begin = if row == start.row { start.offset } else { 0 };
            let stop = if row == end.row {
                end.offset
            } else {
                line.len()
            };
            let (begin, stop) = (begin.min(line.len()), stop.min(line.len()));
            if row > start.row {
                text.push('\n');
            }
            if begin < stop {
                text.push_str(&line[begin..stop]);
            }
        }
        Some(text)
    }

    /// The space selection math runs over: the frame projection, seeded from
    /// the committed transcript on first access (before the first render, in
    /// unit tests). Every index below `committed_len` names a committed row;
    /// every index at or above it names a live row.
    fn selection_space(&self, transcript: &Transcript) -> std::cell::Ref<'_, SelectableSpace> {
        if !self.rendered.get() {
            *self.space.borrow_mut() = SelectableSpace::committed_only(transcript.rows().len());
            self.rendered.set(true);
        }
        self.space.borrow()
    }

    /// Where a fresh selection starts: the end of the last visible row, so
    /// Shift+Up or Shift+Left selects what is on screen without a prior
    /// scroll. The last row is the last visible live row while a stream
    /// runs, so a fresh selection covers streamed text too.
    fn selection_origin(
        &self,
        space: &SelectableSpace,
        transcript: &Transcript,
    ) -> Option<SelectionPoint> {
        let count = space.row_count();
        let last = count.checked_sub(1)?;
        let height = self.page.get().max(1);
        let top = self.window_top(count, height);
        let row = top.saturating_add(height).saturating_sub(1).min(last);
        Some(SelectionPoint {
            row,
            offset: row_text(space, transcript, row)?.len(),
        })
    }
}

/// The text one space index names: committed rows resolve against the
/// transcript, live rows against the frame-cached live suffix.
fn row_text<'a>(
    space: &'a SelectableSpace,
    transcript: &'a Transcript,
    index: usize,
) -> Option<&'a str> {
    if index < space.committed_len {
        return transcript.rows().get(index).map(String::as_str);
    }
    space.live.get(index - space.committed_len).map(Box::as_ref)
}

/// The point one `step` from `point` over the space rows: horizontal steps
/// walk graphemes and wrap across row edges; vertical steps keep the
/// grapheme column of the target row.
fn step_point(
    point: SelectionPoint,
    step: SelectionStep,
    space: &SelectableSpace,
    transcript: &Transcript,
) -> Option<SelectionPoint> {
    match step {
        SelectionStep::Left => step_horizontal(point, false, space, transcript),
        SelectionStep::Right => step_horizontal(point, true, space, transcript),
        SelectionStep::Up => step_vertical(point, false, space, transcript),
        SelectionStep::Down => step_vertical(point, true, space, transcript),
    }
}

/// One grapheme left or right; at a row edge the step continues on the
/// neighboring row. The first and last rows bound the step.
fn step_horizontal(
    point: SelectionPoint,
    forward: bool,
    space: &SelectableSpace,
    transcript: &Transcript,
) -> Option<SelectionPoint> {
    let text = row_text(space, transcript, point.row)?;
    if forward {
        if let Some(cluster) = text[point.offset..].graphemes(true).next() {
            return Some(SelectionPoint {
                row: point.row,
                offset: point.offset + cluster.len(),
            });
        }
        row_text(space, transcript, point.row + 1)?;
        return Some(SelectionPoint {
            row: point.row + 1,
            offset: 0,
        });
    }
    if point.offset > 0 {
        let offset = text[..point.offset]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(byte, _)| byte);
        return Some(SelectionPoint {
            row: point.row,
            offset,
        });
    }
    let row = point.row.checked_sub(1)?;
    row_text(space, transcript, row).map(|text| SelectionPoint {
        row,
        offset: text.len(),
    })
}

/// One row up or down. A focus at its row's end stays at the target row's
/// end, like a caret on the last cell; any other offset keeps its grapheme
/// column, clamped to the target row's length. The step never leaves the
/// space.
fn step_vertical(
    point: SelectionPoint,
    down: bool,
    space: &SelectableSpace,
    transcript: &Transcript,
) -> Option<SelectionPoint> {
    let text = row_text(space, transcript, point.row)?;
    let row = if down {
        point.row.checked_add(1)?
    } else {
        point.row.checked_sub(1)?
    };
    let target = row_text(space, transcript, row)?;
    let offset = if point.offset == text.len() {
        target.len()
    } else {
        let column = text[..point.offset].graphemes(true).count();
        target
            .grapheme_indices(true)
            .nth(column)
            .map_or(target.len(), |(byte, _)| byte)
    };
    Some(SelectionPoint { row, offset })
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
        viewport.jump_to(7, 60);
        assert_eq!(viewport.window_top(60, 10), 7);
        viewport.jump_latest();
        assert!(viewport.follows());
        assert_eq!(viewport.window_top(60, 10), 50);
    }

    mod window_props {
        use super::Viewport;
        use proptest::prelude::*;

        #[derive(Debug, Clone, Copy)]
        enum Step {
            Up,
            Down,
            Latest,
            Jump(usize),
            Rows(usize),
        }

        fn step() -> impl Strategy<Value = Step> {
            prop_oneof![
                Just(Step::Up),
                Just(Step::Down),
                Just(Step::Latest),
                (0usize..400).prop_map(Step::Jump),
                (0usize..400).prop_map(Step::Rows),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// Whatever the key sequence, the window stays inside the
            /// transcript, a detached view never moves under arriving rows,
            /// and End always re-attaches at the live edge.
            #[test]
            fn offset_clamps_and_end_always_follows(
                height in 1usize..40,
                steps in prop::collection::vec(step(), 0..60),
            ) {
                let mut viewport = Viewport::following();
                viewport.observe(height);
                let mut rows = 100usize;
                for step in steps {
                    match step {
                        Step::Up => viewport.scroll_up(rows),
                        Step::Down => viewport.scroll_down(rows),
                        Step::Latest => viewport.jump_latest(),
                        Step::Jump(row) => viewport.jump_to(row, rows),
                        Step::Rows(count) => {
                            let before = viewport.window_top(rows, height);
                            let detached = !viewport.follows();
                            let grown = rows.max(count);
                            if detached && before <= grown.saturating_sub(height) {
                                prop_assert_eq!(viewport.window_top(grown, height), before);
                            }
                            rows = grown;
                        }
                    }
                    let top = viewport.window_top(rows, height);
                    prop_assert!(top <= rows.saturating_sub(height));
                }
                viewport.jump_latest();
                prop_assert!(viewport.follows());
                prop_assert_eq!(viewport.window_top(rows, height), rows.saturating_sub(height));
            }

            /// Paging up from anywhere never passes the oldest row.
            #[test]
            fn paging_up_stops_at_the_oldest_row(
                rows in 0usize..300,
                height in 1usize..40,
                presses in 0usize..80,
            ) {
                let mut viewport = Viewport::following();
                viewport.observe(height);
                for _ in 0..presses {
                    viewport.scroll_up(rows);
                }
                let floor = if presses == 0 { rows.saturating_sub(height) } else {
                    rows.saturating_sub(height).saturating_sub(presses * height)
                };
                prop_assert_eq!(viewport.window_top(rows, height), floor);
            }
        }
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

    mod selection {
        use super::Viewport;
        use crate::screen::fullscreen::{
            SelectableSpace, Selection, SelectionPoint, SelectionStep,
        };
        use crate::transcript::Transcript;

        /// Three committed rows; the observed window is the whole transcript.
        fn fixture() -> (Viewport, Transcript) {
            let mut transcript = Transcript::default();
            transcript.commit(
                "entry-1",
                &["first row".to_owned(), "second row".to_owned()],
            );
            transcript.commit("entry-2", &["third row".to_owned()]);
            let viewport = Viewport::following();
            viewport.observe(3);
            (viewport, transcript)
        }

        #[test]
        fn shift_up_starts_at_the_visible_bottom_and_selects_up() {
            let (mut viewport, transcript) = fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            let selection = viewport.selection().expect("a selection starts");
            assert_eq!(
                selection.anchor,
                SelectionPoint {
                    row: 2,
                    offset: "third row".len()
                }
            );
            assert_eq!(
                selection.focus,
                SelectionPoint {
                    row: 1,
                    offset: "second row".len()
                }
            );
            assert_eq!(
                viewport.selected_text(&transcript).as_deref(),
                Some("\nthird row")
            );
        }

        #[test]
        fn horizontal_steps_walk_graphemes_and_wrap_rows() {
            let (mut viewport, transcript) = fixture();
            viewport.extend_selection(SelectionStep::Left, &transcript);
            let selection = viewport.selection().expect("a selection starts");
            assert_eq!(selection.focus.row, 2);
            assert_eq!(selection.focus.offset, "third ro".len());
            viewport.extend_selection(SelectionStep::Left, &transcript);
            viewport.extend_selection(SelectionStep::Left, &transcript);
            assert_eq!(
                viewport.selection().map(|selection| selection.focus),
                Some(SelectionPoint {
                    row: 2,
                    offset: "third ".len()
                })
            );
            // Six more steps reach the row start; the seventh wraps onto the
            // end of the row above.
            for _ in 0..="third ".len() {
                viewport.extend_selection(SelectionStep::Left, &transcript);
            }
            assert_eq!(
                viewport.selection().map(|selection| selection.focus),
                Some(SelectionPoint {
                    row: 1,
                    offset: "second row".len()
                })
            );
        }

        #[test]
        fn vertical_steps_stay_at_ends_and_keep_the_column() {
            let (mut viewport, transcript) = fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            viewport.extend_selection(SelectionStep::Up, &transcript);
            // Both steps started at a row end, so both land on row ends.
            assert_eq!(
                viewport.selection().map(|selection| selection.focus),
                Some(SelectionPoint {
                    row: 0,
                    offset: "first row".len()
                })
            );
            assert_eq!(
                viewport.selected_text(&transcript).as_deref(),
                Some("\nsecond row\nthird row")
            );
            // A mid-row offset keeps its grapheme column across rows: from
            // the last cell, one step left lands inside the row, and the
            // down step carries that column into the longer row below.
            viewport.extend_selection(SelectionStep::Left, &transcript);
            assert_eq!(
                viewport.selection().map(|selection| selection.focus),
                Some(SelectionPoint {
                    row: 0,
                    offset: "first ro".len()
                })
            );
            viewport.extend_selection(SelectionStep::Down, &transcript);
            assert_eq!(
                viewport.selection().map(|selection| selection.focus),
                Some(SelectionPoint {
                    row: 1,
                    offset: "second r".len()
                })
            );
        }

        #[test]
        fn steps_never_leave_the_transcript_and_clear_drops_it() {
            let (mut viewport, transcript) = fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            viewport.extend_selection(SelectionStep::Up, &transcript);
            let at_top = viewport.selection();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            assert_eq!(viewport.selection(), at_top, "row zero bounds the step");
            assert!(
                viewport.selected_text(&transcript).is_some(),
                "the selection is still active"
            );
            viewport.clear_selection();
            assert_eq!(viewport.selected_text(&transcript), None);
            // An empty transcript starts nothing.
            let mut empty = Viewport::following();
            empty.extend_selection(SelectionStep::Up, &Transcript::default());
            assert!(empty.selection().is_none());
        }

        #[test]
        fn row_span_marks_only_the_covered_bytes() {
            let selection = Selection {
                anchor: SelectionPoint { row: 0, offset: 2 },
                focus: SelectionPoint { row: 1, offset: 3 },
            };
            assert_eq!(selection.row_span(0, 9), Some(2..9));
            assert_eq!(selection.row_span(1, 10), Some(0..3));
            assert_eq!(selection.row_span(2, 9), None);
            // An empty selection marks nothing anywhere.
            let empty = Selection {
                anchor: SelectionPoint { row: 1, offset: 3 },
                focus: SelectionPoint { row: 1, offset: 3 },
            };
            assert_eq!(empty.row_span(1, 10), None);
        }

        /// Three committed rows plus two live rows, as a busy fullscreen
        /// frame with a held stream renders them.
        fn live_fixture() -> (Viewport, Transcript) {
            let (viewport, transcript) = fixture();
            viewport.update_space(
                SelectableSpace::new(
                    transcript.rows().len(),
                    ["fourth row", "fifth row"]
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                ),
                &transcript,
            );
            (viewport, transcript)
        }

        #[test]
        fn busy_live_selection_copies_the_two_live_rows() {
            let (mut viewport, transcript) = live_fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            for _ in "fourth row".chars() {
                viewport.extend_selection(SelectionStep::Left, &transcript);
            }
            assert_eq!(
                viewport.selected_text(&transcript).as_deref(),
                Some("fourth row\nfifth row")
            );
        }

        #[test]
        fn collapse_by_inverse_step_empties_the_live_selection() {
            let (mut viewport, transcript) = live_fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            viewport.extend_selection(SelectionStep::Down, &transcript);
            let selection = viewport
                .selection()
                .expect("a collapsed selection stays empty");
            assert_eq!(selection.anchor, selection.focus);
            assert_eq!(viewport.selected_text(&transcript), None);
        }

        #[test]
        fn selection_survives_live_to_committed_settle() {
            let (mut viewport, mut transcript) = live_fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            for _ in "fourth row".chars() {
                viewport.extend_selection(SelectionStep::Left, &transcript);
            }
            transcript.commit(
                "entry-3",
                &["fourth row".to_owned(), "fifth row".to_owned()],
            );
            viewport.update_space(
                SelectableSpace::committed_only(transcript.rows().len()),
                &transcript,
            );
            assert_eq!(
                viewport.selected_text(&transcript).as_deref(),
                Some("fourth row\nfifth row")
            );
            let selection = viewport
                .selection()
                .expect("the settle keeps the selection");
            assert!(selection.anchor.row < transcript.rows().len());
            assert!(selection.focus.row < transcript.rows().len());
        }

        #[test]
        fn settle_tail_growth_remaps_live_rows_by_content() {
            let mut viewport = Viewport::following();
            viewport.observe(3);
            let mut transcript = Transcript::default();
            transcript.commit("entry-1", &["first row".to_owned()]);
            viewport.update_space(
                SelectableSpace::new(1, vec!["fifth row".into()]),
                &transcript,
            );
            viewport.extend_selection(SelectionStep::Left, &transcript);
            let before = viewport.selected_text(&transcript);
            assert_eq!(before.as_deref(), Some("w"));
            transcript.commit(
                "entry-2",
                &[
                    "second row".to_owned(),
                    "third row".to_owned(),
                    "fourth row".to_owned(),
                    "fifth row".to_owned(),
                ],
            );
            viewport.update_space(
                SelectableSpace::committed_only(transcript.rows().len()),
                &transcript,
            );
            assert_eq!(viewport.selected_text(&transcript), before);
            let selection = viewport
                .selection()
                .expect("the tail row keeps its identity");
            assert_eq!(selection.focus.row, 4);
            assert_eq!(selection.focus.offset, "fifth ro".len());
        }

        #[test]
        fn settle_with_changed_content_drops_the_live_selection() {
            let (mut viewport, mut transcript) = live_fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            transcript.commit(
                "entry-3",
                &["rendered four".to_owned(), "rendered five".to_owned()],
            );
            viewport.update_space(
                SelectableSpace::committed_only(transcript.rows().len()),
                &transcript,
            );
            assert!(viewport.selection().is_none());
            assert_eq!(viewport.selected_text(&transcript), None);
        }

        #[test]
        fn no_selection_copies_nothing() {
            let (viewport, transcript) = live_fixture();
            assert_eq!(viewport.selected_text(&transcript), None);
        }

        #[test]
        fn live_append_keeps_the_selection_on_the_grown_row() {
            let (mut viewport, transcript) = live_fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            for _ in "fourth row".chars() {
                viewport.extend_selection(SelectionStep::Left, &transcript);
            }
            let before = viewport.selected_text(&transcript);
            assert!(before.is_some());
            viewport.update_space(
                SelectableSpace::new(
                    transcript.rows().len(),
                    ["fourth row", "fifth row, and then some more"]
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                ),
                &transcript,
            );
            assert_eq!(viewport.selected_text(&transcript), before);
            let selection = viewport
                .selection()
                .expect("an appended tail keeps the selection");
            assert_eq!(selection.focus.row, 3);
            assert_eq!(selection.anchor.row, 4);
            viewport.clear_selection();
            viewport.extend_selection(SelectionStep::Left, &transcript);
            let partial = viewport.selected_text(&transcript);
            assert!(partial.is_some());
            viewport.update_space(
                SelectableSpace::new(
                    transcript.rows().len(),
                    ["fourth row", "fifth row, and then some more, again"]
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                ),
                &transcript,
            );
            assert_eq!(viewport.selected_text(&transcript), partial);
            assert!(viewport.selection().is_some());
        }

        #[test]
        fn live_append_keeps_the_selection_before_a_later_duplicate() {
            let (mut viewport, transcript) = live_fixture();
            viewport.extend_selection(SelectionStep::Up, &transcript);
            for _ in "fourth row".chars() {
                viewport.extend_selection(SelectionStep::Left, &transcript);
            }
            viewport.update_space(
                SelectableSpace::new(
                    transcript.rows().len(),
                    ["fourth row", "fifth row appended", "fifth row"]
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                ),
                &transcript,
            );
            assert_eq!(
                viewport.selected_text(&transcript).as_deref(),
                Some("fourth row\nfifth row")
            );
        }
    }
}
