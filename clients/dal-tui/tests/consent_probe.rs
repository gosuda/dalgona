//! Consent/approval prompt lifecycle probe: render, approve, deny,
//! no-repaint-after-resolve, and caller-visible resolution.
//!
//! Each probe drives the real dialog path (`DialogUi`, plus one settled
//! `draw_frame`) and captures the rendered rows into a local frame that the
//! assertions then pin. No prints: the asserted frame content is the evidence.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use dal_core::{Answer, Owner, Preview, Question, Request, RequestId};
use dal_tui::dialog::{ARM_DELAY, DialogUi};
use dal_tui::keys::Key;

fn approval(id: RequestId) -> Request {
    Request {
        id,
        turn: None,
        owner: Owner::Core,
        question: Question::Approval {
            tool: "exec".into(),
            preview: Preview {
                title: "command".into(),
                body: "echo hi".into(),
                digest: None,
            },
            grant: None,
            call: None,
        },
        timeout: Duration::from_secs(30),
        default: Answer::Decline,
    }
}

fn press(dialog: &mut DialogUi, code: KeyCode) -> Option<(RequestId, Answer)> {
    dialog.key(Key::new(code, KeyModifiers::NONE))
}

/// Opens one approval and arms it, so the frame under test lists answer keys.
fn armed(request: Request) -> DialogUi {
    let mut dialog = DialogUi::default();
    dialog.opened(request);
    let start = Instant::now();
    dialog.tick(start);
    dialog.tick(start + ARM_DELAY);
    assert!(dialog.armed(), "the probe frame must list answer keys");
    dialog
}

fn frame(dialog: &DialogUi) -> Vec<String> {
    dialog.rows(80, 12, dal_tui::width::WidthMode::Narrow)
}

#[test]
fn consent_request_renders_the_consent_surface() {
    let dialog = armed(approval(RequestId::new_v7()));
    let frame = frame(&dialog);
    let text = frame.join("\n");
    assert!(
        text.contains("Allow this command?"),
        "consent title renders: {frame:?}"
    );
    assert!(
        text.contains("y allow · a session · n deny"),
        "typed consent actions render: {frame:?}"
    );
    assert!(
        text.contains("esc denies"),
        "fail-closed escape hint renders: {frame:?}"
    );
}

#[test]
fn approve_key_resolves_to_typed_approve_exactly_once() {
    let id = RequestId::new_v7();
    let mut dialog = armed(approval(id));
    assert!(
        matches!(press(&mut dialog, KeyCode::Char('y')), Some((answered, Answer::Approve)) if answered == id),
        "y resolves to the typed approve outcome"
    );
    assert!(
        press(&mut dialog, KeyCode::Char('y')).is_none(),
        "a second y never double-resolves"
    );
    assert!(
        press(&mut dialog, KeyCode::Char('n')).is_none(),
        "no key answers after the first answer"
    );
    let frame = frame(&dialog);
    assert!(
        frame
            .join("\n")
            .contains("Waiting for the request to settle..."),
        "answered prompt parks keys until resolution: {frame:?}"
    );
}

#[test]
fn deny_keys_resolve_to_typed_decline() {
    let id = RequestId::new_v7();
    let mut dialog = armed(approval(id));
    assert!(
        matches!(press(&mut dialog, KeyCode::Char('n')), Some((answered, Answer::Decline)) if answered == id),
        "n resolves to the typed decline outcome"
    );

    let id = RequestId::new_v7();
    let mut dialog = armed(approval(id));
    assert!(
        matches!(press(&mut dialog, KeyCode::Esc), Some((answered, Answer::Decline)) if answered == id),
        "Esc resolves to the typed decline outcome"
    );
}

#[test]
fn resolved_prompt_never_repaints() {
    let id = RequestId::new_v7();
    let mut dialog = armed(approval(id));
    let title = dialog.resolved(&id.to_string());
    assert_eq!(
        title.as_deref(),
        Some("Allow this command?"),
        "resolution carries the title for the outcome notice"
    );
    assert!(
        !dialog.is_open(),
        "no dialog owns the keyboard after resolve"
    );
    assert!(
        frame(&dialog).is_empty(),
        "a resolved prompt paints zero rows"
    );
    assert!(
        dialog.resolved(&id.to_string()).is_none(),
        "a second resolution announces nothing"
    );
}

#[test]
fn dropped_answered_prompt_still_announces_its_resolution() {
    let id = RequestId::new_v7();
    let mut dialog = armed(approval(id));
    assert!(
        matches!(press(&mut dialog, KeyCode::Char('y')), Some((answered, Answer::Approve)) if answered == id),
        "the caller answer lands first"
    );
    // Host snapshot sweep drops the answered request before its resolution
    // update arrives (e.g. turn cancel); the outcome must stay visible.
    dialog.resync(Vec::new());
    let title = dialog.resolved(&id.to_string());
    assert_eq!(
        title.as_deref(),
        Some("Allow this command?"),
        "cancel/withdrawal still names the prompt instead of swallowing it"
    );
    let notice = dal_tui::copy::render(
        dal_tui::copy::ids::REQUEST_CANCELLED,
        &[("title", title.as_deref().unwrap_or_default())],
        1,
    );
    assert!(
        notice.contains("was cancelled with the turn"),
        "caller-visible cancellation text: {notice:?}"
    );
}

#[test]
fn settled_frame_carries_an_open_consent_request() {
    use dal_core::{
        ApprovalMode, AutoCompaction, Gen, Mode, Page, Seq, SessionId, SessionInfo, SettingsView,
        Stats, ThinkingLevel, Timestamp, TreeOutline, TurnState, Usage, UsageView, View, Workspace,
    };
    use std::num::NonZeroU64;

    let workspace = Workspace::new(std::env::temp_dir()).expect("temp workspace");
    let request = approval(RequestId::new_v7());
    let view = View {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::MIN),
        session: SessionInfo {
            id: SessionId::new_v7(),
            name: None,
            preview: "".into(),
            workspace: workspace.clone(),
            updated_at: Timestamp::UNIX_EPOCH,
            created_at: Some(Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: Some(Seq::new(NonZeroU64::MIN)),
            message_count: None,
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
            scoped_models: Vec::new(),
        },
        open: vec![request],
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
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime");
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
        env: dal_tui::EnvFacts::default(),
        rt: runtime.handle().clone(),
    };
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    dal_tui::draw_frame(&mut terminal, dal_tui::Screen::Inline, &view, &opts)
        .expect("settled frame draws");
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        text.contains("Allow this command?"),
        "settled frame carries the open consent surface"
    );
}
