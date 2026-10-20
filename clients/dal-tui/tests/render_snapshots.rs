//! State catalog snapshots: status bands, floors, hint fold, no-color grid.

use dal_core::{
    ApprovalMode, AutoCompaction, Gen, Mode, Page, Seq, SessionId, SessionInfo, SettingsView,
    Stats, ThinkingLevel, Timestamp, TreeOutline, TurnState, Usage, UsageView, View, Workspace,
};
use dal_tui::frame::{RegionBudget, RegionRequest};
use dal_tui::status::{StatusData, render};
use dal_tui::width::WidthMode;
use std::num::NonZeroU64;

fn idle() -> StatusData<'static> {
    StatusData {
        state: None,
        spinner: None,
        model: Some("acme/opus"),
        path: Some("~/work/shop"),
        branch: Some("main"),
        tokens: Some("in 14k out 4k"),
        context: Some("ctx 47%"),
        context_role: None,
        agents: Some("3 agents"),
        cost: Some("$0.42"),
    }
}

fn empty_view() -> Result<View, Box<dyn std::error::Error>> {
    let workspace = Workspace::new(std::env::temp_dir())?;
    Ok(View {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::MIN),
        session: SessionInfo {
            id: SessionId::new_v7(),
            name: None,
            preview: "".into(),
            workspace,
            updated_at: Timestamp::UNIX_EPOCH,
            created_at: Some(Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: Some(Seq::new(NonZeroU64::MIN)),
        },
        turn: TurnState::Idle,
        entries: Page {
            items: Vec::new(),
            next_before: None,
        },
        tree: TreeOutline {
            branches: Vec::new(),
        },
        settings: SettingsView {
            model: None,
            thinking: ThinkingLevel::Medium,
            approval: ApprovalMode::Ask,
            mode: Mode::Normal,
            name: None,
            scoped_models: None,
        },
        open: Vec::new(),
        changes: Vec::new(),
        usage: UsageView {
            usage: Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                premium_requests: None,
                cost_usd: None,
            },
            context_tokens: 0,
            context_window: 128_000,
        },
        stats: Stats {
            steers_queued: 0,
            follow_ups_queued: 0,
            retries: 0,
            dropped_observations: 0,
            auto_compaction: AutoCompaction::Off,
        },
    })
}

fn drawn_frame(width: u16, height: u16) -> Result<String, Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let workspace = Workspace::new(std::env::temp_dir())?;
    let opts = dal_tui::TuiOptions {
        session: dal_agent::SessionRef::Ephemeral { workspace },
        screen: dal_tui::Screen::Inline,
        theme_request: dal_tui::ThemeRequest::Palette,
        default_model: None,
        images: false,
        diagrams: false,
        motion: false,
        editor: "true".into(),
        color: dal_tui::ColorMode::Never,
        binary: "dal",
        env: dal_tui::EnvFacts {
            stdin_tty: true,
            ..dal_tui::EnvFacts::default()
        },
        rt: runtime.handle().clone(),
    };
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend)?;
    let view = empty_view()?;
    dal_tui::draw_frame(&mut terminal, dal_tui::Screen::Inline, &view, &opts)?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect())
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
fn short_terminal_renders_the_rows_warning() {
    let text = drawn_frame(80, 7).expect("frame draws");
    assert!(
        text.contains("dalgon needs at least 8 rows"),
        "rows floor shows the rows copy: {text:?}"
    );
    assert!(
        !text.contains("12 columns"),
        "rows floor never shows the columns copy"
    );
}

#[test]
fn narrow_terminal_renders_the_columns_warning() {
    let text = drawn_frame(11, 24).expect("frame draws");
    assert!(
        text.contains("dalgon need"),
        "columns floor clips the columns copy to 11 cells: {text:?}"
    );
    assert!(
        !text.contains("8 rows"),
        "columns floor never shows the rows copy"
    );
}

#[test]
fn healthy_terminal_shows_no_narrow_warning() {
    let text = drawn_frame(80, 24).expect("frame draws");
    assert!(
        !text.contains("needs at least"),
        "no floor warning at healthy dimensions"
    );
    let _ = RegionBudget::allocate(80, 24, RegionRequest::default());
}

#[test]
fn rendered_status_has_no_ansi_sequences() {
    let plain = render(idle(), 80, WidthMode::Narrow);
    assert!(!plain.contains('\x1b'));
}
