//! Alternate-screen renderer: header, viewport, bottom stack, search.

/// Fullscreen scroll and follow state.
#[derive(Debug, Default, Clone)]
pub struct Viewport {
    /// Whether the viewport follows the live edge.
    pub follow: bool,
    /// Detached scroll offset from the live edge.
    pub offset: usize,
    /// Search filter row text, when open.
    pub search: Option<String>,
}

impl Viewport {
    /// Creates a following viewport.
    #[must_use]
    pub fn following() -> Self {
        Self {
            follow: true,
            offset: 0,
            search: None,
        }
    }

    /// Detaches follow on scroll; shows the stopped-follow cue.
    pub fn scroll_up(&mut self) {
        self.follow = false;
        self.offset += 1;
    }

    /// Re-attaches follow at the latest entry.
    pub fn jump_latest(&mut self) {
        self.follow = true;
        self.offset = 0;
    }

    /// Opens the filter row over the viewport top.
    pub fn open_search(&mut self) {
        self.search = Some(String::new());
    }

    /// Closes search and returns focus to the composer.
    pub fn close_search(&mut self) {
        self.search = None;
    }

    /// Returns the follow-stopped cue when detached.
    #[must_use]
    pub fn cue(&self) -> Option<&'static str> {
        (!self.follow).then_some(crate::copy::ids::FOLLOW_STOPPED)
    }
}

/// Returns the header row count: one row when wide and tall enough.
#[must_use]
pub const fn header_rows(width: u16, height: u16) -> usize {
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
        viewport.scroll_up();
        assert_eq!(
            viewport.cue(),
            Some("following stopped · press end to jump to the latest")
        );
        viewport.jump_latest();
        assert_eq!(viewport.cue(), None);
    }
}
