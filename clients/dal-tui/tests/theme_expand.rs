//! DES-03 theme, expand-path, and image-placement proofs.
//!
//! Contract source: repo-root `DESIGN.md` and `CONCEPTS.md` (there is no
//! `docs/DESIGN.md`; the TUI contract lives at the root). Line map:
//!
//! - Color gate (DESIGN.md Color): six roles (`text`, `dim`, `accent`,
//!   `success`, `warning`, `error`) at >= 4.5 against `canvas`, `surface`,
//!   `selected`; `faint` at >= 3.0 against `canvas`. Nine shipped themes.
//! - Named expand paths: `Ctrl+O` "Expand or collapse newest card" (Keys
//!   table); tool card "`ctrl+o` expands" (Components table + Tool cards);
//!   diagram header `diagram {kind} · {n} rows · ctrl+o expands` with expand
//!   to source text (Diagrams); thinking collapsed
//!   `Thinking {dur} · ctrl+o to show reasoning` and expanded
//!   `Thinking · {n} steps · {dur} · ctrl+o to hide` (Thinking); approval
//!   `V` "View full command / preview" (Keys table) with the
//!   `y allow · a session · n deny · v view · esc denies` armed hint
//!   (Consent lock).
//! - Scrollback (CONCEPTS.md Letterpress + F4): committed transcript is
//!   write-once, never repainted; expand/collapse must not move it.
//! - Images (DESIGN.md Images): placeholder card
//!   `image {name} · {w}x{h} · {size}`, expanded note, frozen-bytes
//!   invariant (scrollback retains the placeholder text), graphics opt-in.
//!
//! Every assertion here goes through public API only (`draw_frame` cell
//! attributes, `DialogUi`, `Transcript`, `copy`, `image`, `keys`,
//! `diagram::raster`, `theme`). Whole-stream diagram copying is owned by the
//! `DiagramCopy` lane; this file never touches it.

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use dal_core::{
    Answer, ApprovalMode, AssistantStop, AutoCompaction, Block, CallId, Choice, EntryId, EntryKind,
    EntryView, Family, Gen, JournalPart, Mode, Owner, Page, Preview, Question, RawJson, Request,
    RequestId, Seq, SessionId, SessionInfo, SettingsView, Stats, ThinkingLevel, Timestamp,
    TreeOutline, TurnState, Usage, UsageView, View, Workspace,
};
use dal_tui::copy::ids;
use dal_tui::dialog::{ARM_DELAY, DialogUi};
use dal_tui::image::{ImageProbe, Rung};
use dal_tui::keys::{Action, Key, Owner as KeyOwner};
use dal_tui::theme::{Fill, Role, SHIPPED_THEMES};
use dal_tui::transcript::Transcript;
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiOptions, WidthMode};
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

// --- Shared harness -------------------------------------------------------

const ALL_ROLES: [Role; 7] = [
    Role::Text,
    Role::Dim,
    Role::Faint,
    Role::Accent,
    Role::Success,
    Role::Warning,
    Role::Error,
];
const ALL_FILLS: [Fill; 3] = [Fill::Canvas, Fill::Surface, Fill::Selected];

fn idle_view() -> Result<View, dal_tui::TuiError> {
    let workspace = Workspace::new(std::env::temp_dir())
        .map_err(|error| dal_tui::TuiError::Terminal(format!("temp workspace: {error}")))?;
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

fn base_opts(
    rt: &tokio::runtime::Handle,
    theme: ThemeRequest,
    color: ColorMode,
) -> Result<TuiOptions, dal_tui::TuiError> {
    let workspace = Workspace::new(std::env::temp_dir())
        .map_err(|error| dal_tui::TuiError::Terminal(format!("temp workspace: {error}")))?;
    Ok(TuiOptions {
        session: dal_agent::SessionRef::Ephemeral { workspace },
        screen: Screen::Fullscreen,
        theme_request: theme,
        default_model: None,
        images: false,
        diagrams: false,
        motion: false,
        editor: "true".into(),
        color,
        binary: "dal",
        env: EnvFacts::default(),
        rt: rt.clone(),
    })
}

fn named_screen_opts(
    rt: &tokio::runtime::Handle,
    theme: &str,
    color: ColorMode,
) -> Result<TuiOptions, dal_tui::TuiError> {
    base_opts(rt, ThemeRequest::Named(theme.into()), color)
}

fn named_diagram_opts(
    rt: &tokio::runtime::Handle,
    theme: &str,
    color: ColorMode,
) -> Result<TuiOptions, dal_tui::TuiError> {
    let mut opts = base_opts(rt, ThemeRequest::Named(theme.into()), color)?;
    opts.diagrams = true;
    Ok(opts)
}

fn draw(
    view: &View,
    opts: &TuiOptions,
    screen: Screen,
    width: u16,
    height: u16,
) -> Result<ratatui::Terminal<TestBackend>, dal_tui::TuiError> {
    let backend = TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend)
        .map_err(|error| dal_tui::TuiError::Terminal(format!("test terminal: {error}")))?;
    dal_tui::draw_frame(&mut terminal, screen, view, opts)?;
    Ok(terminal)
}

/// Rendered text rows, right-trimmed, in frame order.
fn texts(terminal: &ratatui::Terminal<TestBackend>, width: u16, height: u16) -> Vec<String> {
    let mut rows = Vec::with_capacity(usize::from(height));
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            let index = usize::from(y) * usize::from(width) + usize::from(x);
            let cell = &terminal.backend().buffer().content[index];
            line.push_str(cell.symbol());
        }
        rows.push(line.trim_end().to_owned());
    }
    rows
}

fn find_row(rows: &[String], marker: &str) -> usize {
    let position = rows.iter().position(|row| row.contains(marker));
    assert!(
        position.is_some(),
        "marker {marker:?} missing from frame: {rows:?}"
    );
    position.unwrap_or(0)
}

/// Foreground colors of the non-space cells of one frame row.
fn row_fgs(terminal: &ratatui::Terminal<TestBackend>, width: u16, y: usize) -> Vec<Color> {
    let mut out = Vec::new();
    for x in 0..width {
        let index = y * usize::from(width) + usize::from(x);
        let cell = &terminal.backend().buffer().content[index];
        if cell.symbol() != " " {
            out.push(cell.fg);
        }
    }
    out
}

fn row_modifiers(terminal: &ratatui::Terminal<TestBackend>, width: u16, y: usize) -> Vec<Modifier> {
    let mut out = Vec::new();
    for x in 0..width {
        let index = y * usize::from(width) + usize::from(x);
        let cell = &terminal.backend().buffer().content[index];
        if cell.symbol() != " " {
            out.push(cell.modifier);
        }
    }
    out
}

fn theme_color(theme: &str, role: Role) -> Result<Color, dal_tui::TuiError> {
    Ok(dal_tui::theme::load(
        &ThemeRequest::Named(theme.into()),
        ColorMode::Truecolor,
        None,
        None,
    )?
    .color(role))
}

fn user_entry(id: u64, text: &str) -> Result<EntryView, dal_tui::TuiError> {
    Ok(EntryView {
        id: EntryId::new(NonZeroU64::new(id).ok_or_else(|| {
            dal_tui::TuiError::Terminal(format!("entry id must be nonzero, got {id}"))
        })?),
        parent: None,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn reasoning_entry(id: u64, text: &str) -> Result<EntryView, dal_tui::TuiError> {
    Ok(EntryView {
        id: EntryId::new(NonZeroU64::new(id).ok_or_else(|| {
            dal_tui::TuiError::Terminal(format!("entry id must be nonzero, got {id}"))
        })?),
        parent: None,
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "probe-model".into(),
            content: vec![Block::Reasoning {
                text: text.into(),
                replay: RawJson::null(),
            }],
            usage: Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                premium_requests: None,
                cost_usd: None,
            },
            stop: AssistantStop::Done,
            route: None,
            replay: None,
        },
    })
}

fn tool_entry(
    id: u64,
    name: &str,
    error: bool,
    text: &str,
    elapsed_ms: Option<u64>,
) -> Result<EntryView, dal_tui::TuiError> {
    Ok(EntryView {
        id: EntryId::new(NonZeroU64::new(id).ok_or_else(|| {
            dal_tui::TuiError::Terminal(format!("entry id must be nonzero, got {id}"))
        })?),
        parent: None,
        kind: EntryKind::ToolResult {
            call: CallId::new("call-1"),
            name: name.into(),
            error,
            parts: vec![JournalPart::Text { text: text.into() }],
            changes: Vec::new(),
            elapsed_ms,
        },
    })
}

fn diagram_entry(id: u64) -> Result<EntryView, dal_tui::TuiError> {
    Ok(EntryView {
        id: EntryId::new(NonZeroU64::new(id).ok_or_else(|| {
            dal_tui::TuiError::Terminal(format!("entry id must be nonzero, got {id}"))
        })?),
        parent: None,
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "probe-model".into(),
            content: vec![Block::Text {
                text: "intro\n```mermaid\ngraph TD; A-->B\n```\noutro".into(),
                replay: None,
            }],
            usage: Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                premium_requests: None,
                cost_usd: None,
            },
            stop: AssistantStop::Done,
            route: None,
            replay: None,
        },
    })
}

fn approval_request(id: RequestId, body: &str) -> Request {
    Request {
        id,
        turn: None,
        owner: Owner::Core,
        question: Question::Approval {
            tool: "exec".into(),
            preview: Preview {
                title: "command".into(),
                body: body.into(),
                digest: None,
            },
            grant: None,
            call: None,
        },
        timeout: Duration::from_secs(30),
        default: Answer::Decline,
    }
}

fn armed_approval(body: &str) -> DialogUi {
    let mut dialog = DialogUi::default();
    dialog.opened(approval_request(RequestId::new_v7(), body));
    let start = Instant::now();
    dialog.tick(start);
    dialog.tick(start + ARM_DELAY);
    assert!(dialog.armed(), "the probe frame must list answer keys");
    dialog
}

// --- (a) Theme contrast and visibility ------------------------------------

/// DESIGN.md Color: six roles at >= 4.5 on three fills, faint at >= 3.0 on
/// canvas, for exactly nine shipped themes: 9 * (6*3 + 1) = 171 pairs.
#[test]
fn contrast_gate_checks_every_design_pair() {
    assert_eq!(
        dal_tui::theme::verify_contrast_gate().expect("contrast gate passes"),
        9 * (6 * 3 + 1),
        "the gate must witness every DESIGN pair, no fewer"
    );
}

/// The WCAG math under the gate is honest: black on white is 21:1.
#[test]
fn contrast_math_pins_the_wcag_endpoints() {
    use opaline::OpalineColor;
    let ratio = dal_tui::theme::contrast_ratio(
        OpalineColor::new(255, 255, 255),
        OpalineColor::new(0, 0, 0),
    );
    assert!(
        (ratio - 21.0).abs() < 0.05,
        "white on black must read 21:1, got {ratio}"
    );
    let same = dal_tui::theme::contrast_ratio(
        OpalineColor::new(0x12, 0x34, 0x56),
        OpalineColor::new(0x12, 0x34, 0x56),
    );
    assert!(
        (same - 1.0).abs() < 1e-9,
        "identical colors must read 1:1, got {same}"
    );
}

/// No shipped theme leaves any role invisible on any fill: every surface the
/// TUI paints resolves to a real color, except 16-color `text`, which is
/// `Reset` by design (the terminal default foreground). `NO_COLOR` keeps the
/// dim/reverse meaning carriers DESIGN.md requires (bold, dim, reverse,
/// underline).
fn assert_mode_visible(theme: &dal_tui::theme::ResolvedTheme, name: &str, mode: ColorMode) {
    for role in ALL_ROLES {
        for fill in ALL_FILLS {
            if mode == ColorMode::Sixteen && role == Role::Text {
                continue;
            }
            assert_ne!(
                theme.color_on(role, fill),
                Color::Reset,
                "{name} {role:?} on {fill:?} at {mode:?} must stay visible"
            );
        }
    }
}

#[test]
fn every_shipped_theme_keeps_all_roles_visible() {
    assert_eq!(SHIPPED_THEMES.len(), 9, "DESIGN ships exactly nine themes");
    for name in SHIPPED_THEMES {
        for mode in [ColorMode::Truecolor, ColorMode::Ansi256, ColorMode::Sixteen] {
            let theme = dal_tui::theme::load(&ThemeRequest::Named(name.into()), mode, None, None)
                .unwrap_or_else(|error| panic!("{name} loads at {mode:?}: {error}"));
            assert_mode_visible(&theme, name, mode);
        }
        assert!(
            dal_tui::theme::ResolvedTheme::modifiers(Role::Dim).contains(Modifier::DIM)
                && dal_tui::theme::ResolvedTheme::modifiers(Role::Faint).contains(Modifier::DIM),
            "{name}: dim/faint must keep the DIM carrier under NO_COLOR"
        );
        assert!(
            dal_tui::theme::ResolvedTheme::selection_style()
                .add_modifier
                .contains(Modifier::REVERSED),
            "{name}: selection must keep reverse video without color"
        );
    }
}

/// Committed text cells carry the theme `text` color on every shipped theme,
/// and color never changes the text itself.
#[test]
fn committed_text_cells_carry_theme_text_color() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    let mut first_texts: Option<Vec<String>> = None;
    for name in SHIPPED_THEMES {
        let mut view = idle_view().expect("temp workspace");
        view.entries.items =
            vec![user_entry(1, "theme-probe committed alpha 01").expect("entry id")];
        let opts = named_screen_opts(runtime.handle(), name, ColorMode::Truecolor)
            .expect("temp workspace");
        let terminal = draw(&view, &opts, Screen::Fullscreen, 80, 24).expect("frame draws");
        let rows = texts(&terminal, 80, 24);
        let y = find_row(&rows, "theme-probe committed alpha 01");
        let expected = theme_color(name, Role::Text).expect("shipped theme loads");
        let fgs = row_fgs(&terminal, 80, y);
        assert!(!fgs.is_empty(), "{name}: committed row paints no cells");
        assert!(
            fgs.iter().all(|fg| *fg == expected),
            "{name}: committed text must carry the text color {expected:?}, got {fgs:?}"
        );
        if let Some(first) = &first_texts {
            assert_eq!(&rows, first, "{name}: theme must never change text");
        } else {
            first_texts = Some(rows);
        }
    }
}

/// Chrome roles on every shipped theme: the committed thinking row paints in
/// `text` (the transcript seam keeps text plus spans only, and DESIGN.md
/// Typography holds the transcript to normal plus bold), while the approval
/// dialog keeps its own rows, so its title carries `accent` and its scroll
/// footer carries `dim` with the DIM modifier.
#[test]
fn transcript_and_approval_chrome_carry_text_dim_and_accent() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    let long_body = (1..=30)
        .map(|n| format!("chrome probe preview line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    for name in SHIPPED_THEMES {
        let mut view = idle_view().expect("temp workspace");
        view.entries.items =
            vec![reasoning_entry(1, "probe step one\n\nprobe step two").expect("entry id")];
        view.open = vec![approval_request(RequestId::new_v7(), &long_body)];
        let opts = named_screen_opts(runtime.handle(), name, ColorMode::Truecolor)
            .expect("temp workspace");
        let terminal = draw(&view, &opts, Screen::Fullscreen, 80, 24).expect("frame draws");
        let rows = texts(&terminal, 80, 24);

        let thinking = find_row(&rows, "ctrl+o to show reasoning");
        let text = theme_color(name, Role::Text).expect("shipped theme loads");
        assert!(
            row_fgs(&terminal, 80, thinking)
                .iter()
                .all(|fg| *fg == text),
            "{name}: committed thinking row paints in text"
        );

        let title = find_row(&rows, "Allow this command?");
        let accent = theme_color(name, Role::Accent).expect("shipped theme loads");
        assert!(
            row_fgs(&terminal, 80, title).iter().all(|fg| *fg == accent),
            "{name}: approval title must carry the accent color"
        );

        let footer = find_row(&rows, "more lines · pgdn");
        let dim = theme_color(name, Role::Dim).expect("shipped theme loads");
        assert!(
            row_fgs(&terminal, 80, footer).iter().all(|fg| *fg == dim),
            "{name}: dialog scroll footer must carry the dim color"
        );
        assert!(
            row_modifiers(&terminal, 80, footer)
                .iter()
                .all(|modifier| modifier.contains(Modifier::DIM)),
            "{name}: dialog scroll footer must keep the DIM carrier"
        );
    }
}

/// Below the 12-column floor the frame is the warning plus status, and the
/// warning row carries the `warning` role on every shipped theme.
#[test]
fn narrow_floor_carries_the_warning_role() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    for name in SHIPPED_THEMES {
        let view = idle_view().expect("temp workspace");
        let opts = named_screen_opts(runtime.handle(), name, ColorMode::Truecolor)
            .expect("temp workspace");
        let terminal = draw(&view, &opts, Screen::Fullscreen, 11, 24).expect("frame draws");
        let rows = texts(&terminal, 11, 24);
        let y = find_row(&rows, "dalgon need");
        let warning = theme_color(name, Role::Warning).expect("shipped theme loads");
        assert!(
            row_fgs(&terminal, 11, y).iter().all(|fg| *fg == warning),
            "{name}: the narrow floor must carry the warning color"
        );
    }
}

fn context_frame(
    runtime: &tokio::runtime::Handle,
    theme: &str,
    tokens: u64,
) -> Result<(ratatui::Terminal<TestBackend>, Vec<String>), dal_tui::TuiError> {
    let mut view = idle_view()?;
    view.usage.context_tokens = tokens;
    view.usage.context_window = 128_000;
    let opts = named_screen_opts(runtime, theme, ColorMode::Truecolor)?;
    let terminal = draw(&view, &opts, Screen::Fullscreen, 80, 24)?;
    let rows = texts(&terminal, 80, 24);
    Ok((terminal, rows))
}

fn assert_context_quiet(
    runtime: &tokio::runtime::Handle,
    theme: &str,
    tokens: u64,
) -> Result<(), dal_tui::TuiError> {
    let (_, rows) = context_frame(runtime, theme, tokens)?;
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("rising") || row.contains("high")),
        "{theme}: sub-threshold context must name no threshold word: {rows:?}"
    );
    Ok(())
}

fn assert_context_word(
    runtime: &tokio::runtime::Handle,
    theme: &str,
    tokens: u64,
    word: &str,
    role: Role,
) -> Result<(), dal_tui::TuiError> {
    let (terminal, rows) = context_frame(runtime, theme, tokens)?;
    let y = find_row(&rows, word);
    let expected = theme_color(theme, role)?;
    assert!(
        row_fgs(&terminal, 80, y).contains(&expected),
        "{theme}: {word} must carry the {role:?} color"
    );
    Ok(())
}

/// The status context slot carries `warning` above 70% and `error` above 90%
/// (DESIGN.md Status line and composer), with exact threshold boundaries on
/// every shipped theme.
#[test]
fn context_slot_thresholds_name_warning_and_error() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    for name in SHIPPED_THEMES {
        assert_context_quiet(runtime.handle(), name, 88_320).expect("context frame");
        assert_context_word(runtime.handle(), name, 89_600, "rising", Role::Warning)
            .expect("context frame");
        assert_context_word(runtime.handle(), name, 115_200, "high", Role::Error)
            .expect("context frame");
        assert_context_word(runtime.handle(), name, 122_880, "high", Role::Error)
            .expect("context frame");
    }
}

/// `NO_COLOR` strips color SGR but keeps words, dim, and text identity.
#[test]
fn no_color_keeps_meaning_without_color() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    let long_body = (1..=30)
        .map(|n| format!("nocolor probe preview line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut view = idle_view().expect("temp workspace");
    view.entries.items = vec![
        reasoning_entry(1, "probe step one").expect("entry id"),
        tool_entry(
            2,
            "exec",
            true,
            "Permission denied: exec was declined.",
            Some(1500),
        )
        .expect("entry id"),
    ];
    view.open = vec![approval_request(RequestId::new_v7(), &long_body)];
    let color_opts = named_screen_opts(runtime.handle(), "flexoki-dark", ColorMode::Truecolor)
        .expect("temp workspace");
    let plain_opts = named_screen_opts(runtime.handle(), "flexoki-dark", ColorMode::Never)
        .expect("temp workspace");
    let colored = draw(&view, &color_opts, Screen::Fullscreen, 80, 24).expect("frame draws");
    let plain = draw(&view, &plain_opts, Screen::Fullscreen, 80, 24).expect("frame draws");
    assert_eq!(
        texts(&plain, 80, 24),
        texts(&colored, 80, 24),
        "NO_COLOR must change no text"
    );
    for y in 0..24 {
        assert!(
            row_fgs(&plain, 80, y).iter().all(|fg| *fg == Color::Reset),
            "NO_COLOR row {y} must strip color SGR"
        );
    }
    let rows = texts(&plain, 80, 24);
    let footer = find_row(&rows, "more lines · pgdn");
    assert!(
        row_modifiers(&plain, 80, footer)
            .iter()
            .all(|modifier| modifier.contains(Modifier::DIM)),
        "NO_COLOR must keep the dim carrier on dim rows"
    );
    assert!(
        rows.iter().any(|row| row.contains("failed")),
        "the failed word, not its color, carries the error state"
    );
}

// --- (b) Named expand paths ------------------------------------------------

/// The Keys table contract: Ctrl+O (legacy 0x0f) expands or collapses the
/// newest card, in both the kitty and legacy decoders.
#[test]
fn ctrl_o_resolves_to_expand_collapse_newest_card() {
    let key = Key::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    for kitty in [false, true] {
        assert_eq!(
            dal_tui::keys::resolve(key, kitty),
            Some(Action::ExpandCollapse),
            "ctrl+o must resolve with kitty={kitty}"
        );
    }
    let binding = dal_tui::keys::BINDINGS
        .iter()
        .find(|binding| binding.owner == KeyOwner::App && binding.action == Action::ExpandCollapse)
        .expect("the ExpandCollapse binding exists");
    assert_eq!(binding.default, key, "default ctrl+o binding");
    assert_eq!(
        binding.legacy,
        Some(key),
        "the legacy column is the contract for non-kitty terminals"
    );
}

/// Thinking collapsed row: one row promising `ctrl+o`, in `dim`.
#[test]
fn thinking_collapsed_row_promises_ctrl_o() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    let mut view = idle_view().expect("temp workspace");
    view.entries.items =
        vec![reasoning_entry(1, "probe step one\nprobe step two").expect("entry id")];
    let opts = named_screen_opts(runtime.handle(), "flexoki-dark", ColorMode::Truecolor)
        .expect("temp workspace");
    let terminal = draw(&view, &opts, Screen::Fullscreen, 80, 24).expect("frame draws");
    let rows = texts(&terminal, 80, 24);
    let hits: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("ctrl+o to show reasoning"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(hits.len(), 1, "collapsed thinking is one row: {rows:?}");
    assert!(
        !rows.iter().any(|row| row.contains("probe step one")),
        "collapsed thinking hides the reasoning text: {rows:?}"
    );
}

/// Thinking expanded header template: `Thinking · {n} steps · {dur} ·
/// ctrl+o to hide`, with honest singular/plural.
#[test]
fn thinking_expanded_header_names_steps_and_hide() {
    let three = dal_tui::copy::render(ids::THINKING_OPEN, &[("n", "3"), ("dur", "")], 3)
        .replace(" ·  · ", " · ");
    assert!(
        three.contains("3 steps") && three.contains("ctrl+o to hide"),
        "expanded header names the step count and the hide path: {three:?}"
    );
    let one = dal_tui::copy::render(ids::THINKING_OPEN, &[("n", "1"), ("dur", "9 ms")], 1);
    assert!(
        one.contains("1 step ") && !one.contains("1 steps") && one.contains("9 ms"),
        "singular step count with duration: {one:?}"
    );
}

/// Tool cards settle to exactly the specified headers (`tool.ok` /
/// `tool.failed`), one row each: the collapsed invariant the `ctrl+o`
/// path opens from. `draw_frame` commits through the untimed path, so the
/// duration slot is omitted exactly as the spec allows ("omitted when
/// un-timed"); timed rendering is witnessed in-tree.
#[test]
fn tool_cards_settle_to_the_exact_collapsed_spec() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    let mut view = idle_view().expect("temp workspace");
    view.entries.items = vec![
        tool_entry(1, "read", false, "1\talpha\n2\tbeta", Some(52)).expect("entry id"),
        tool_entry(
            2,
            "exec",
            true,
            "Permission denied: exec was declined.",
            Some(1500),
        )
        .expect("entry id"),
    ];
    let opts = named_screen_opts(runtime.handle(), "flexoki-dark", ColorMode::Truecolor)
        .expect("temp workspace");
    let terminal = draw(&view, &opts, Screen::Fullscreen, 80, 24).expect("frame draws");
    let rows = texts(&terminal, 80, 24);
    assert!(
        rows.iter().any(|row| row.trim() == "ok  read 1 alpha"),
        "settled ok header is exact: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.trim() == "failed  exec · Permission denied: exec was declined."),
        "settled failed header is exact: {rows:?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("beta")),
        "the one-line summary keeps the rest for the expand view: {rows:?}"
    );
}

/// Diagram settled header advertises `ctrl+o expands` at the fence position;
/// the fenced source never leaks into transcript rows.
#[test]
fn diagram_settled_header_advertises_ctrl_o_expands() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    let mut view = idle_view().expect("temp workspace");
    view.entries.items = vec![diagram_entry(1).expect("entry id")];
    let opts = named_diagram_opts(runtime.handle(), "flexoki-dark", ColorMode::Truecolor)
        .expect("temp workspace");
    let terminal = draw(&view, &opts, Screen::Fullscreen, 80, 24).expect("frame draws");
    let rows = texts(&terminal, 80, 24);
    let header = find_row(&rows, "ctrl+o expands");
    assert!(
        rows[header].contains("diagram mermaid"),
        "the header names the diagram kind: {:?}",
        rows[header]
    );
    let intro = find_row(&rows, "intro");
    assert!(
        intro < header,
        "prose precedes the card at its fence position: {rows:?}"
    );
    let outro = find_row(&rows, "outro");
    assert!(outro > header, "prose follows the card: {rows:?}");
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("graph TD") || row.contains("```")),
        "the fenced source never leaks into transcript rows: {rows:?}"
    );
}

/// Approval `V` ("View full command / preview"): collapse names the scroll
/// path, expand shows the whole preview with choices pinned, collapse
/// restores byte-identically, and scrollback never moves (CONCEPTS F4).
#[test]
fn approval_v_round_trip_restores_collapse_and_preserves_scrollback() {
    let body = (1..=30)
        .map(|n| format!("approval preview line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut dialog = armed_approval(&body);
    let collapsed = dialog.rows(80, 12, WidthMode::Narrow);
    let collapsed_text = collapsed.join("\n");
    assert!(
        collapsed_text.contains("more lines · pgdn"),
        "collapse names the scroll path: {collapsed:?}"
    );
    assert!(
        collapsed_text.contains("approval preview line 01")
            && !collapsed_text.contains("approval preview line 30"),
        "collapse clips the preview tail: {collapsed:?}"
    );
    assert!(
        collapsed_text.contains("v view"),
        "the armed hint lists the view key (Consent lock): {collapsed:?}"
    );

    // Scrollback freezes the collapsed promise before the view opens.
    let mut transcript = Transcript::default();
    assert_eq!(
        transcript.commit("des03-approval-scrollback", &collapsed),
        collapsed,
        "first commit lands every row"
    );

    // Expand: the full preview, choices still listed, no truncation footer.
    assert!(
        dialog
            .key(Key::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .is_none(),
        "v toggles the view, it answers nothing"
    );
    let expanded = dialog.rows(80, 40, WidthMode::Narrow);
    let expanded_text = expanded.join("\n");
    assert!(
        expanded_text.contains("approval preview line 30"),
        "expand shows the whole preview: {expanded:?}"
    );
    assert!(
        !expanded_text.contains("more lines"),
        "expand drops the truncation footer: {expanded:?}"
    );
    assert_eq!(
        transcript.rows(),
        collapsed.as_slice(),
        "scrollback never moves while the view is open"
    );

    // Collapse: byte-identical restore, and the frozen rows stay frozen.
    assert!(
        dialog
            .key(Key::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .is_none()
    );
    assert_eq!(
        dialog.rows(80, 12, WidthMode::Narrow),
        collapsed,
        "collapse restores the exact collapsed rows"
    );
    assert_eq!(transcript.rows(), collapsed.as_slice());
    assert!(
        transcript
            .commit("des03-approval-scrollback", &collapsed)
            .is_empty(),
        "re-commit after the cycle emits nothing"
    );
}

/// Select `ctrl+o` toggles the preview without losing the pinned choices,
/// and restores on the second press.
#[test]
fn select_ctrl_o_toggles_preview_without_losing_choices() {
    let body = (1..=20)
        .map(|n| format!("select preview line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut dialog = DialogUi::default();
    dialog.opened(Request {
        id: RequestId::new_v7(),
        turn: None,
        owner: Owner::Core,
        question: Question::Select {
            prompt: "Pick a store".into(),
            options: vec![
                Choice {
                    label: "choice-alpha".into(),
                    description: Some("Embedded, zero config".into()),
                },
                Choice {
                    label: "choice-beta".into(),
                    description: None,
                },
            ],
            multi: false,
            preview: Some(Preview {
                title: "stores".into(),
                body: body.into(),
                digest: None,
            }),
        },
        timeout: Duration::from_secs(30),
        default: Answer::Cancel,
    });
    let start = Instant::now();
    dialog.tick(start);
    dialog.tick(start + ARM_DELAY);
    assert!(dialog.armed());

    let collapsed = dialog.rows(80, 10, WidthMode::Narrow);
    let collapsed_text = collapsed.join("\n");
    assert!(
        collapsed_text.contains("more lines · pgdn"),
        "a clipped preview names its scroll path: {collapsed:?}"
    );
    for choice in ["choice-alpha", "choice-beta"] {
        assert!(
            collapsed_text.contains(choice),
            "collapse keeps the choices pinned: {collapsed:?}"
        );
    }

    assert!(
        dialog
            .key(Key::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
            .is_none(),
        "ctrl+o toggles the preview, it answers nothing"
    );
    let expanded = dialog.rows(80, 30, WidthMode::Narrow);
    let expanded_text = expanded.join("\n");
    assert!(
        expanded_text.contains("select preview line 20"),
        "expand shows the whole preview: {expanded:?}"
    );
    for choice in ["choice-alpha", "choice-beta"] {
        assert!(
            expanded_text.contains(choice),
            "expand keeps the choices pinned: {expanded:?}"
        );
    }

    assert!(
        dialog
            .key(Key::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
            .is_none()
    );
    assert_eq!(
        dialog.rows(80, 10, WidthMode::Narrow),
        collapsed,
        "the second press restores the collapsed rows"
    );
}

// --- (c) Image placement ---------------------------------------------------

/// `scale_to_cells` never exceeds its budget at the documented measure caps
/// (DESIGN.md Space: 20->18, 40->36, 60->54, 80->72, 100->92, 120->100;
/// pixel rows cap at 16): image blocks cannot outgrow the rows reserved for
/// them, so they cannot overlap the composer or transcript rows.
#[test]
fn image_block_geometry_never_exceeds_its_budget() {
    use dal_tui::diagram::raster::scale_to_cells;
    for (width, cap) in [
        (20, 18),
        (40, 36),
        (60, 54),
        (80, 72),
        (100, 92),
        (120, 100),
    ] {
        let (columns, rows) = scale_to_cells(640, 480, 10, 20, cap, 16);
        assert!(
            (1..=cap).contains(&columns) && (1..=16).contains(&rows),
            "640x480 at width {width} (cap {cap}) must fit its budget, got {columns}x{rows}"
        );
    }
    // A panoramic source binds the width exactly instead of cropping rows.
    let (columns, rows) = scale_to_cells(2000, 100, 10, 20, 72, 16);
    assert_eq!(
        (columns, rows),
        (72, 2),
        "a wide source lands exactly on the cap with rows rounded up"
    );
    // Degenerate inputs reserve no block, so there is nothing to overlap.
    assert_eq!(scale_to_cells(0, 480, 10, 20, 72, 16), (0, 0));
    assert_eq!(scale_to_cells(640, 0, 10, 20, 72, 16), (0, 0));
    // An 8K source stays inside the same budget.
    let (columns, rows) = scale_to_cells(7680, 4320, 10, 20, 72, 16);
    assert!(
        columns <= 72 && rows <= 16 && columns >= 1 && rows >= 1,
        "8K source must stay budgeted, got {columns}x{rows}"
    );
}

/// The placeholder card is exactly one row and scrollback keeps it frozen
/// (the frozen-bytes invariant at the text seam).
#[test]
fn placeholder_card_is_one_row_and_scrollback_frozen() {
    let card = dal_tui::image::card("diagram", Some((640, 480)), 2048);
    assert!(
        card.starts_with("image diagram · 640x480 · "),
        "placeholder names kind, dimensions, size: {card:?}"
    );
    assert!(!card.contains('\n'), "the card is one row: {card:?}");
    assert_eq!(
        ids::IMAGE_OPEN,
        "The image was sent to the model. It is not shown in the terminal.",
        "the expanded note is the documented sentence"
    );
    assert!(!ids::IMAGE_OPEN.contains('\n'), "the note is one row");

    let rows = vec![card, ids::IMAGE_OPEN.to_owned()];
    let mut transcript = Transcript::default();
    assert_eq!(transcript.commit("des03-image-card", &rows), rows);
    assert_eq!(transcript.rows(), rows.as_slice());
    assert!(
        transcript.commit("des03-image-card", &rows).is_empty(),
        "the frozen card never duplicates"
    );
}

/// With `images = false` no rung is admitted even when every probe succeeds,
/// so no pixel block can exist to overlap anything; with kitty confirmed the
/// opt-in admits exactly the direct rung.
#[test]
fn images_off_admits_no_rung_while_kitty_opt_in_is_exact() {
    let probes_ok = ImageProbe {
        kitty_ok: true,
        da1_sixel: true,
    };
    assert_eq!(
        dal_tui::image::resolve_rung(&EnvFacts::default(), probes_ok, false),
        None,
        "images=false must admit no rung whatever the probes say"
    );
    let kitty = EnvFacts {
        term_program: Some("kitty".into()),
        ..EnvFacts::default()
    };
    assert_eq!(
        dal_tui::image::resolve_rung(&kitty, ImageProbe::default(), true),
        Some(Rung::KittyDirect),
        "confirmed kitty with images=true admits the direct rung"
    );
}

/// Committed art, composer, and status share no rows at documented sizes:
/// transcript rows stay above the composer row, the composer prompt row never
/// drops, and the frame never exceeds its height.
#[test]
fn art_composer_and_status_share_no_rows_at_documented_sizes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("probe runtime");
    for (width, height) in [(80_u16, 24_u16), (100, 30), (40, 12)] {
        let mut view = idle_view().expect("temp workspace");
        view.entries.items = vec![
            diagram_entry(1).expect("entry id"),
            tool_entry(2, "read", false, "hello world", Some(52)).expect("entry id"),
            reasoning_entry(3, "probe step one").expect("entry id"),
        ];
        let opts = named_diagram_opts(runtime.handle(), "flexoki-dark", ColorMode::Truecolor)
            .expect("temp workspace");
        let terminal = draw(&view, &opts, Screen::Fullscreen, width, height).expect("frame draws");
        let rows = texts(&terminal, width, height);
        assert!(
            rows.len() <= usize::from(height),
            "{width}x{height}: frame exceeds its height: {rows:?}"
        );
        let composer = find_row(&rows, "> Ask dal");
        let status = rows
            .iter()
            .rposition(|row| !row.is_empty())
            .expect("status paints");
        assert!(
            composer < status,
            "{width}x{height}: composer must sit above status"
        );
        for marker in ["ctrl+o expands", "ctrl+o to show reasoning", "ok  read"] {
            if let Some(position) = rows.iter().position(|row| row.contains(marker)) {
                assert!(
                    position < composer,
                    "{width}x{height}: transcript row {marker:?} must stay above the composer"
                );
            }
        }
        assert!(
            rows.iter()
                .any(|row| row.contains("ctrl+o to show reasoning")),
            "{width}x{height}: the collapsed thinking row stays visible"
        );
    }
    // At 20x10 the transcript window may show no committed rows, but the
    // frame stays valid: the prompt row never drops and status paints last.
    let narrow = draw(
        &idle_view().expect("temp workspace"),
        &named_diagram_opts(runtime.handle(), "flexoki-dark", ColorMode::Truecolor)
            .expect("temp workspace"),
        Screen::Fullscreen,
        20,
        10,
    )
    .expect("frame draws");
    let narrow_rows = texts(&narrow, 20, 10);
    assert!(narrow_rows.len() <= 10, "20x10 frame exceeds its height");
    let composer = find_row(&narrow_rows, "> Ask dal");
    let status = narrow_rows
        .iter()
        .rposition(|row| !row.is_empty())
        .expect("status paints");
    assert!(composer < status, "20x10 keeps composer above status");
}
