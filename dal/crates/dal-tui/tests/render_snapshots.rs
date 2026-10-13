//! State catalog snapshots: status bands, floors, hint fold, no-color grid.

use dal_tui::frame::{RegionBudget, RegionRequest};
use dal_tui::status::{StatusData, render};
use dal_tui::width::WidthMode;

fn idle() -> StatusData<'static> {
    StatusData {
        state: None,
        spinner: None,
        model: Some("acme/opus"),
        path: Some("~/work/shop (main)"),
        tokens: Some("in 14k out 4k"),
        context: Some("ctx 47%"),
        agents: Some("3 agents"),
        cost: Some("$0.42"),
    }
}

#[test]
fn idle_state_inline_widths() {
    let full = render(idle(), 120, WidthMode::Narrow);
    assert!(full.contains("acme/opus") && full.contains("$0.42"));
    let narrow = render(idle(), 40, WidthMode::Narrow);
    assert!(!narrow.contains("$0.42"));
    assert!(narrow.contains("acme/opus"));
}

#[test]
fn narrow_floors_report_exact_copy() {
    let rows = RegionBudget::allocate(80, 7, RegionRequest::default());
    assert_eq!(rows.warning, Some(dal_tui::frame::NarrowWarning::Rows));
    let cols = RegionBudget::allocate(11, 24, RegionRequest::default());
    assert_eq!(cols.warning, Some(dal_tui::frame::NarrowWarning::Columns));
    assert_eq!(
        dal_tui::copy::ids::NARROW_ROWS,
        "dalgon needs at least 8 rows"
    );
    assert_eq!(
        dal_tui::copy::ids::NARROW_COLS,
        "dalgon needs at least 12 columns"
    );
}

#[test]
fn no_color_text_grid_matches_colored_grid() {
    let colored = render(idle(), 80, WidthMode::Narrow);
    let plain = render(idle(), 80, WidthMode::Narrow);
    assert_eq!(colored, plain);
    assert!(!plain.contains('\x1b'));
}
