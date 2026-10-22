//! Narrow-screen and Unicode proofs (DES-02): help action list, cursor and
//! commit layout, sub-floor reflow.
//!
//! What this file proves, and what it deliberately does not re-prove:
//! - `resize_probe.rs` owns the 80 -> 40 -> 100 ASCII width storm through the
//!   byte driver and the real `run_backend` loop. This probe mirrors its
//!   byte-driver half for Unicode content at floor-adjacent geometry, and
//!   adds the layout-level proofs the storm never touches: help action reach
//!   at the documented 20-column minimum, grapheme-boundary cursor columns,
//!   narrow-cap wrap integrity, and the sub-floor warning frame.
//! - `cjk_width.rs` owns terminal-truth cell widths and basic wrap/take
//!   behavior. This probe owns the cursor/commit consequences of those
//!   widths: boundary round-trips, join-back at narrow caps, and committed
//!   bytes through commit and draw.
//! - `render_snapshots.rs` owns the floor warnings for an empty view. This
//!   probe owns the floor with Unicode content and the exact
//!   `MIN_COLS` / `MIN_ROWS` usability boundary.
//! - `keyboard_decode.rs` owns wire bytes -> actions. This probe owns the
//!   action list's survival under a 20-cell clip.
//!
//! No `dal-tui/src` edits.

use std::num::NonZeroU64;

use dal_core::{
    ApprovalMode, AutoCompaction, EntryId, EntryKind, Gen, JournalPart, Mode, Page, Seq, SessionId,
    SessionInfo, SettingsView, Stats, ThinkingLevel, Timestamp, TreeOutline, TurnState, Usage,
    UsageView, View, Workspace,
};
use dal_tui::copy::ids::{MIN_COLS, MIN_ROWS, NARROW_COLS, NARROW_ROWS};
use dal_tui::frame::{NarrowWarning, RegionBudget, RegionRequest};
use dal_tui::keys::help_labels;
use dal_tui::screen::inline::{Region, commit_bytes, shrink_bytes};
use dal_tui::transcript::Transcript;
use dal_tui::width::{WidthMode, take_cells, width, wrap};
use unicode_segmentation::UnicodeSegmentation;

/// The documented minimum width: every state renders down to 20 columns
/// (DESIGN "Space" / "Components").
const MINIMUM_WIDTH: usize = 20;

/// Unicode committed rows: wide CJK, combining marks, and an emoji-adjacent
/// ZWJ sequence. Short enough to survive prose measure caps unwrapped.
const COMMITTED_UNICODE: &[&str] = &[
    "nu alpha 01",
    "nu \u{6f22}\u{5b57} beta 02",
    "nu cafe\u{301} gamma 03",
    "nu \u{1f468}\u{200d}\u{1f33e} delta 04",
];

fn committed_unicode() -> Vec<String> {
    COMMITTED_UNICODE
        .iter()
        .map(|row| (*row).to_owned())
        .collect()
}

fn has(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn count_token(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Parses one `ESC[<row>;<col>H` / `ESC[<row>H` target; `None` when the
/// bytes at `index` open no cursor-positioning sequence.
fn parse_cup_at(bytes: &[u8], index: usize) -> Option<(u16, usize)> {
    if bytes.get(index) != Some(&0x1b) || bytes.get(index + 1) != Some(&b'[') {
        return None;
    }
    let mut end = index + 2;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == index + 2 {
        return None;
    }
    let row = std::str::from_utf8(bytes.get(index + 2..end)?)
        .ok()?
        .parse::<u16>()
        .ok()?;
    if bytes.get(end) == Some(&b'H') {
        return Some((row, end + 1));
    }
    if bytes.get(end) != Some(&b';') {
        return None;
    }
    let mut col_end = end + 1;
    while bytes.get(col_end).is_some_and(u8::is_ascii_digit) {
        col_end += 1;
    }
    if bytes.get(col_end) != Some(&b'H') {
        return None;
    }
    Some((row, col_end + 1))
}

/// Row numbers referenced by `ESC[<row>;<col>H` / `ESC[<row>H` sequences.
fn targeted_rows(bytes: &[u8]) -> Vec<u16> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index + 2 <= bytes.len() {
        let Some((row, next)) = parse_cup_at(bytes, index) else {
            index += 1;
            continue;
        };
        rows.push(row);
        index = next;
    }
    rows
}

fn assert_no_scrollback_clear(bytes: &[u8], label: &str) {
    assert!(
        !has(bytes, b"\x1b[J"),
        "{label}: erase-display ED0 present: {bytes:?}"
    );
    assert!(
        !has(bytes, b"\x1b[2J"),
        "{label}: erase-display ED2 present: {bytes:?}"
    );
    assert!(
        !has(bytes, b"\x1b[3J"),
        "{label}: scrollback clear present: {bytes:?}"
    );
}

fn assert_one_bracket_pair(bytes: &[u8], label: &str) {
    let open = count_token(bytes, b"\x1b[?2026h");
    let close = count_token(bytes, b"\x1b[?2026l");
    assert_eq!(
        open, 1,
        "{label}: expected exactly one sync open, got {open}"
    );
    assert_eq!(
        close, 1,
        "{label}: expected exactly one sync close, got {close}"
    );
}

fn user_entry(id: u64, text: &str) -> Result<dal_core::EntryView, dal_tui::TuiError> {
    let nonzero = NonZeroU64::new(id).ok_or_else(|| {
        dal_tui::TuiError::Terminal(format!("entry id must be nonzero, got {id}"))
    })?;
    Ok(dal_core::EntryView {
        id: EntryId::new(nonzero),
        parent: None,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn unicode_view() -> Result<View, dal_tui::TuiError> {
    let terminal = |error: &dyn std::fmt::Display| {
        dal_tui::TuiError::Terminal(format!("narrow_unicode fixture: {error}"))
    };
    let workspace = Workspace::new(std::env::temp_dir()).map_err(|error| terminal(&error))?;
    Ok(View {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::MIN),
        session: SessionInfo {
            id: SessionId::parse("018f0f62-3b00-7000-8000-000000000001")
                .map_err(|error| terminal(&error))?,
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
            items: COMMITTED_UNICODE
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let id = u64::try_from(index + 1).map_err(|error| terminal(&error))?;
                    user_entry(id, text)
                })
                .collect::<Result<Vec<_>, _>>()?,
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

fn test_options(
    rt: &tokio::runtime::Handle,
    mode: WidthMode,
) -> Result<dal_tui::TuiOptions, dal_tui::TuiError> {
    let workspace = Workspace::new(std::env::temp_dir())
        .map_err(|error| dal_tui::TuiError::Terminal(format!("test workspace: {error}")))?;
    Ok(dal_tui::TuiOptions {
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
            term: Some("xterm-256color".to_owned()),
            width_mode: mode,
            ..dal_tui::EnvFacts::default()
        },
        rt: rt.clone(),
    })
}

fn drawn_text(
    width_cells: u16,
    height_cells: u16,
    mode: WidthMode,
) -> Result<String, dal_tui::TuiError> {
    let failed = |error: &dyn std::fmt::Display| {
        dal_tui::TuiError::Terminal(format!("narrow_unicode draw: {error}"))
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|error| failed(&error))?;
    let opts = test_options(runtime.handle(), mode)?;
    let backend = ratatui::backend::TestBackend::new(width_cells, height_cells);
    let mut terminal = ratatui::Terminal::new(backend).map_err(|error| failed(&error))?;
    let view = unicode_view()?;
    dal_tui::draw_frame(&mut terminal, dal_tui::Screen::Inline, &view, &opts)?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect())
}

fn drawn_fullscreen_text(width_cells: u16, height_cells: u16) -> Result<String, dal_tui::TuiError> {
    let failed = |error: &dyn std::fmt::Display| {
        dal_tui::TuiError::Terminal(format!("narrow_unicode draw: {error}"))
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|error| failed(&error))?;
    let opts = test_options(runtime.handle(), WidthMode::Narrow)?;
    let backend = ratatui::backend::TestBackend::new(width_cells, height_cells);
    let mut terminal = ratatui::Terminal::new(backend).map_err(|error| failed(&error))?;
    let view = unicode_view()?;
    dal_tui::draw_frame(&mut terminal, dal_tui::Screen::Fullscreen, &view, &opts)?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect())
}

/// DES-02 (a): the F1 action list across every usable narrow width.
///
/// Contract: DESIGN "Components" (every state renders down to 20 columns
/// without clipping that lacks a named expand path) + "Keys" (the app action
/// table) + `copy::ids::MIN_COLS` (12 columns stay usable; only below that
/// does the floor own the frame). The actionable part of a help row is its
/// key chord: a clip at ANY usable width — 12 through 20, not just the
/// 20-column DESIGN minimum — must keep the chord intact and stay
/// grapheme-safe, so no critical action is clipped away while the scroll
/// window + footer keep the rest reachable.
#[test]
fn help_action_list_keeps_critical_keys_intact_at_narrow_widths() {
    let labels = help_labels();
    assert!(!labels.is_empty(), "the help overlay must list actions");
    // The documented close/quit/interrupt/expand promises must be present:
    // Esc closes the overlay, Ctrl+D quits, Ctrl+C interrupts, Ctrl+O
    // expands, Ctrl+T opens the transcript, F1 re-opens help.
    for critical in ["f1", "esc", "ctrl+d", "ctrl+c", "ctrl+o", "ctrl+t"] {
        assert!(
            labels.iter().any(|(key, _)| key == critical),
            "critical action {critical:?} missing from the help list: {labels:?}"
        );
    }
    let floor = usize::from(MIN_COLS);
    for cells in floor..=MINIMUM_WIDTH {
        for mode in [WidthMode::Narrow, WidthMode::Cjk] {
            check_help_clip(cells, mode, &labels);
        }
    }
}

/// One width's share of the help-clip proof: every action row clips to a
/// non-empty grapheme-safe prefix that fits the width and keeps its key
/// chord intact.
fn check_help_clip(cells: usize, mode: WidthMode, labels: &[(String, &'static str)]) {
    for (key, label) in labels {
        let row = format!("{key} {label}");
        let clipped = take_cells(&row, cells, mode);
        assert!(
            !clipped.is_empty(),
            "help row {row:?} clips to nothing at {cells} cells"
        );
        assert!(
            width(&clipped, mode) <= cells,
            "help row {row:?} exceeds {cells} cells after clipping: {clipped:?}"
        );
        assert!(
            row.starts_with(clipped.as_str()),
            "help clip must keep whole grapheme clusters from the row start: {row:?} -> {clipped:?}"
        );
        assert!(
            clipped.starts_with(key.as_str()),
            "help clip at {cells} cells cut the key chord {key:?} of {row:?}: {clipped:?}"
        );
    }
}

/// DES-02 (a)+(c): the region budget keeps help/composer rows at narrow but
/// usable widths and fails closed exactly at the documented floor.
///
/// Contract: DESIGN "Space" region budgets + `copy::ids::MIN_COLS/MIN_ROWS`
/// (12 columns / 8 rows). In-tree `frame.rs` tests pin the floor points
/// (11x24, 80x7) and the 80x8 overlay floor; the unproved edges are the
/// usable side of the boundary (12x24, 80x8 without overlay) and a
/// help-height overlay at the 20-column minimum.
#[test]
fn narrow_budget_keeps_overlay_rows_and_fails_closed_at_the_floor() {
    let help_height = help_labels().len().max(1);
    // At the documented minimum width a help-height overlay keeps its rows
    // and the status row: nothing collapses to the floor early.
    let budget = RegionBudget::allocate(
        20,
        24,
        RegionRequest {
            overlay: Some(help_height),
            ..RegionRequest::default()
        },
    );
    assert_eq!(budget.warning, None, "20x24 is usable, not floored");
    assert!(
        budget.overlay >= 3,
        "help overlay keeps at least its three-row action floor at 20x24: {budget:?}"
    );
    assert_eq!(budget.status, 1, "the status row keeps the last row");
    assert!(
        budget.overlay + budget.status <= budget.cap,
        "overlay + status fit the live-block cap: {budget:?}"
    );
    // The exact documented boundary stays usable on both axes.
    assert_eq!(
        RegionBudget::allocate(12, 24, RegionRequest::default()).warning,
        None,
        "exactly MIN_COLS columns stays usable"
    );
    assert_eq!(
        RegionBudget::allocate(80, 8, RegionRequest::default()).warning,
        None,
        "exactly MIN_ROWS rows stays usable"
    );
    assert_eq!(
        RegionBudget::allocate(12, 8, RegionRequest::default()).warning,
        None,
        "the fully floored corner stays usable at exactly MIN_COLS x MIN_ROWS"
    );
    // One cell below either floor fails closed to warning + status only.
    assert_eq!(
        RegionBudget::allocate(11, 24, RegionRequest::default()).warning,
        Some(NarrowWarning::Columns),
        "below MIN_COLS the columns floor owns the frame"
    );
    assert_eq!(
        RegionBudget::allocate(80, 7, RegionRequest::default()).warning,
        Some(NarrowWarning::Rows),
        "below MIN_ROWS the rows floor owns the frame"
    );
    assert_eq!(MIN_COLS, 12, "documented minimum usable width");
    assert_eq!(MIN_ROWS, 8, "documented minimum usable height");
}

/// DES-02 (b): Unicode cursor columns follow measured cells at every
/// grapheme boundary.
///
/// Contract: DESIGN "Typography" (all width decisions go through
/// `width::width`; combining marks are zero-width inside their cluster, wide
/// CJK / ZWJ / flag / jamo clusters are two cells). The composer caret
/// column is the measured width of the text before the caret, so the proof
/// is a round-trip: at every grapheme boundary, clipping the text to the
/// measured column returns exactly the prefix before the caret, and no two
/// boundaries share a column (the caret column always identifies one
/// boundary, never a cluster interior).
#[test]
fn unicode_caret_columns_round_trip_at_every_grapheme_boundary() {
    const DRAFTS: &[&str] = &[
        "\u{6f22}\u{5b57}\u{30c6}\u{30b9}\u{30c8}",
        "a\u{301}e\u{301}o\u{308}",
        "\u{1f468}\u{200d}\u{1f33e}\u{1f1eb}\u{1f1f7}",
        "\u{1100}\u{1161}\u{11a8}\u{2764}\u{fe0f}",
        "mix \u{6f22}a\u{301}\u{1f468}\u{200d}\u{1f33e} end",
    ];
    for mode in [WidthMode::Narrow, WidthMode::Cjk] {
        for draft in DRAFTS {
            let clusters: Vec<&str> = draft.graphemes(true).collect();
            assert!(
                clusters.len() > 1,
                "draft {draft:?} must span several clusters"
            );
            let mut prefix = String::new();
            let mut columns = Vec::with_capacity(clusters.len() + 1);
            columns.push(0);
            for cluster in &clusters {
                prefix.push_str(cluster);
                let column = width(&prefix, mode);
                // The caret column of this boundary maps back to the same
                // boundary: the column never lands inside a cluster.
                assert_eq!(
                    take_cells(draft, column, mode),
                    prefix,
                    "column {column} does not round-trip to its boundary in {draft:?} ({mode:?})"
                );
                columns.push(column);
            }
            assert_eq!(
                width(draft, mode),
                *columns.last().expect("a column per boundary"),
                "total width must equal the last caret column in {draft:?}"
            );
            let mut sorted = columns.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                columns.len(),
                "two caret boundaries share column in {draft:?} ({mode:?}): {columns:?}"
            );
        }
    }
    // Emoji-adjacent widths that select the caret math: a bare heart is one
    // cell, VS16 makes it two.
    assert_eq!(width("\u{2764}", WidthMode::Narrow), 1);
    assert_eq!(width("\u{2764}\u{fe0f}", WidthMode::Narrow), 2);
}

/// DES-02 (b): narrow-cap wrapping never splits a cluster and joins back
/// byte-for-byte.
///
/// Contract: DESIGN "Typography" (cluster width is the maximum member width;
/// no cluster splits) + "Space" (measure caps shrink toward 20 columns; the
/// prompt gutter takes 2 cells, so a 20-column composer wraps at 18).
/// `cjk_width.rs` owns basic wrap behavior; the unproved edges are mixed
/// CJK/combining/emoji text at the narrow caps 19/10/4/2/1 (cap 1 forces
/// every wide cluster onto its own row).
#[test]
fn unicode_wrap_joins_back_byte_for_byte_at_narrow_caps() {
    const TEXT: &str = concat!(
        "\u{6f22}\u{5b57}\u{30c6}\u{30b9}\u{30c8} beta ",
        "\u{1f468}\u{200d}\u{1f33e} cafe\u{301} ",
        "\u{1f1eb}\u{1f1f7} done",
    );
    for mode in [WidthMode::Narrow, WidthMode::Cjk] {
        for cap in [19_usize, 10, 4, 2, 1] {
            let rows = wrap(TEXT, cap, mode);
            assert!(!rows.is_empty(), "wrap at cap {cap} emitted no rows");
            assert_eq!(
                rows.concat(),
                TEXT,
                "wrap at cap {cap} ({mode:?}) must join back byte-for-byte"
            );
            for row in &rows {
                if width(row, mode) > cap {
                    // A terminal cannot represent an over-wide cluster in
                    // fewer cells without changing content: the row holds
                    // exactly that one cluster.
                    assert_eq!(
                        row.graphemes(true).count(),
                        1,
                        "over-wide row at cap {cap} splits a cluster: {row:?}"
                    );
                }
            }
        }
    }
}

/// DES-02 (b)+(c): Unicode transcript rows commit exactly once and reach a
/// real frame byte-identically.
///
/// Contract: CONCEPTS F4 + `transcript.rs` ("committed rows are never
/// repainted"): `Transcript::commit` is write-once per entry id, and the
/// commit bytes carry each row exactly once inside one sync bracket pair
/// with no erase-display traffic.
#[test]
fn unicode_transcript_commits_once_and_reaches_the_frame() {
    let texts = committed_unicode();
    let mut transcript = Transcript::default();
    assert_eq!(transcript.commit("narrow-unicode-0", &texts), texts);
    assert_eq!(
        transcript.rows(),
        texts.as_slice(),
        "commit must preserve Unicode rows byte-for-byte"
    );
    assert!(
        transcript.commit("narrow-unicode-0", &texts).is_empty(),
        "re-commit of the same entry must emit nothing"
    );
    assert_eq!(
        transcript.rows(),
        texts.as_slice(),
        "re-commit must not duplicate rows"
    );

    let commit = commit_bytes(&texts, 6, true);
    assert_one_bracket_pair(&commit, "unicode commit");
    assert_no_scrollback_clear(&commit, "unicode commit");
    for text in &texts {
        assert_eq!(
            count_token(&commit, text.as_bytes()),
            1,
            "committed row {text:?} must occur exactly once in the commit bytes"
        );
    }

    // The same rows reach a real fullscreen frame with their grapheme
    // sequence intact: the transcript window carries committed text, not a
    // re-layout of it. The `TestBackend` buffer exposes each wide cluster's
    // second cell as filler, so the frame is normalized by dropping exactly
    // the one filler space after each two-cell cluster before comparing
    // (byte-identity itself is proven at the `Transcript` level above).
    let frame = without_cell_padding(
        &drawn_fullscreen_text(40, 24).expect("frame draws"),
        WidthMode::Narrow,
    );
    for text in COMMITTED_UNICODE {
        assert!(
            frame.contains(text),
            "fullscreen frame drops committed row {text:?}: {frame:?}"
        );
    }
}

/// Drops the `TestBackend` wide-cell filler: exactly one space after each
/// two-cell grapheme cluster. A real space following a wide cluster survives
/// (only the first space is filler), so normalized text stays comparable
/// with byte-level `contains`.
fn without_cell_padding(frame: &str, mode: WidthMode) -> String {
    let mut out = String::new();
    let mut filler_pending = false;
    for cluster in frame.graphemes(true) {
        if filler_pending {
            filler_pending = false;
            if cluster == " " {
                continue;
            }
        }
        out.push_str(cluster);
        filler_pending = width(cluster, mode) == 2;
    }
    out
}

/// DES-02 (c): a narrow shrink cycle preserves committed Unicode bytes with
/// zero repaints above the live top.
///
/// Contract: CONCEPTS F4 + `inline.rs` ("committed rows are never erased").
/// Mirrors the `resize_probe.rs` Part A approach — commit frame, shrink
/// frame, whole-cycle once-only — but for Unicode content at floor-adjacent
/// geometry (a 3-row live block, one surplus row), which the ASCII 80->40
/// storm never covers.
#[test]
fn narrow_shrink_cycle_preserves_unicode_bytes_with_zero_repaints() {
    let texts = committed_unicode();
    let mut transcript = Transcript::default();
    assert_eq!(transcript.commit("narrow-unicode-0", &texts), texts);

    // Floor-adjacent geometry: a 3-row live block under the commits.
    let live = Region { top: 9, height: 3 };
    let commit = commit_bytes(&texts, 6, true);
    assert_one_bracket_pair(&commit, "narrow commit");
    assert_no_scrollback_clear(&commit, "narrow commit");
    for row in targeted_rows(&commit) {
        assert!(
            row >= 6,
            "commit write to row {row} above commit top 6: {:?}",
            targeted_rows(&commit)
        );
    }

    // Narrow shrink: erase the single surplus live row in place.
    let surplus = 1_u16;
    let first_surplus = live.top + live.height - surplus;
    let shrink = shrink_bytes(first_surplus, surplus, true);
    assert_one_bracket_pair(&shrink, "narrow shrink");
    assert_no_scrollback_clear(&shrink, "narrow shrink");
    assert!(
        !has(&shrink, b"\x1b[?1049h") && !has(&shrink, b"\x1b[?1049l"),
        "byte driver must not enter the alternate screen"
    );
    // A fold, not filter().count(): the buffer is tiny and bytecount is not
    // a dependency, so the naive-bytecount lint's suggested crate is out of
    // reach here.
    assert_eq!(
        shrink
            .iter()
            .fold(0_usize, |count, byte| count + usize::from(*byte == b'K')),
        usize::from(surplus),
        "shrink erases exactly the surplus rows"
    );
    for row in targeted_rows(&shrink) {
        assert!(
            row >= live.top,
            "shrink write to row {row} above live top {}: {:?}",
            live.top,
            targeted_rows(&shrink)
        );
    }
    for text in &texts {
        assert!(
            !has(&shrink, text.as_bytes()),
            "shrink repainted committed row {text:?}"
        );
    }

    // Width-independence: the settled transcript is identical after the
    // shrink, and across the whole cycle each Unicode row occurs exactly
    // once — byte-for-byte preservation, not re-layout.
    assert_eq!(
        transcript.rows(),
        texts.as_slice(),
        "shrink must preserve committed Unicode rows"
    );
    let cycle: Vec<u8> = commit.iter().chain(shrink.iter()).copied().collect();
    for text in &texts {
        assert_eq!(
            count_token(&cycle, text.as_bytes()),
            1,
            "committed row {text:?} duplicated across the narrow cycle"
        );
    }
}

/// DES-02 (c): below the floor, a frame with Unicode content still warns
/// without panicking; at the 20-column minimum it renders, not floors.
///
/// Contract: `copy::ids::NARROW_COLS/NARROW_ROWS` + DESIGN "Space" floor.
/// `render_snapshots.rs` owns the floor for an empty view; the unproved edge
/// is wide/cluster content (CJK, ZWJ) flowing through the floor clip, plus
/// the positive proof that 20 columns stays usable with Unicode content.
#[test]
fn sub_floor_frame_with_unicode_content_warns_without_panic() {
    // Below MIN_COLS the columns warning owns the frame even with Unicode
    // entries: the clipped warning prefix, not wide content, is visible.
    let narrow = drawn_text(10, 24, WidthMode::Narrow).expect("frame draws");
    assert!(
        narrow.contains(&take_cells(NARROW_COLS, 10, WidthMode::Narrow)),
        "10-column frame must show the clipped columns warning: {narrow:?}"
    );
    // Below MIN_ROWS the rows warning owns the frame.
    let short = drawn_text(80, 7, WidthMode::Narrow).expect("frame draws");
    assert!(
        short.contains(NARROW_ROWS),
        "7-row frame must show the rows warning: {short:?}"
    );
    // At the documented minimum width the frame renders content: neither
    // floor warning appears with the same Unicode view.
    let minimum = drawn_text(20, 24, WidthMode::Narrow).expect("frame draws");
    assert!(
        !minimum.contains(NARROW_ROWS) && !minimum.contains(NARROW_COLS),
        "20-column frame must not floor with Unicode content: {minimum:?}"
    );
}
