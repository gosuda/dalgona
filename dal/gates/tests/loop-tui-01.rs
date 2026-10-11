//! `TestBackend` snapshots for the inline and fullscreen TUI screens.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    path::{Path, PathBuf},
};

use dal_agent::{Env, SessionRef};
use dal_core::{Config, ConfigProduct, PageReq, Workspace};
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiOptions, WidthMode};
use ratatui::{Terminal, backend::TestBackend};
use support::{TestDir, scripted_session};

#[tokio::test]
async fn tui_backend_snapshots_inline_and_fullscreen() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut snapshot_settings = insta::Settings::clone_current();
    snapshot_settings.set_prepend_module_to_snapshot(false);
    let _snapshot_settings = snapshot_settings.bind_to_scope();
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session.clone()).await?;
    let view = harness.agent.view(PageReq::default())?;
    let options = TuiOptions {
        session,
        screen: Screen::Inline,
        theme_request: ThemeRequest::Palette,
        images: false,
        diagrams: false,
        motion: false,
        editor: "vi".into(),
        color: ColorMode::Never,
        binary: "dalgon",
        env: EnvFacts {
            stdin_tty: true,
            term: Some("xterm-256color".to_owned()),
            width_mode: WidthMode::Narrow,
            ..EnvFacts::default()
        },
        rt: tokio::runtime::Handle::current(),
    };

    let mut inline = Terminal::new(TestBackend::new(80, 24))
        .expect("TestBackend accepts the approved 80x24 geometry");
    dal_tui::draw_frame(&mut inline, Screen::Inline, &view, &options)?;
    let inline_text = render_buffer(&inline, data.path(), workspace.path());
    assert_eq!(inline.backend().buffer().area.width, 80);
    assert!(inline_text.contains(dal_tui::copy::ids::COMPOSER_PLACEHOLDER));
    insta::assert_snapshot!("loop-tui-01__inline", inline_text);

    let mut fullscreen = Terminal::new(TestBackend::new(80, 24))
        .expect("TestBackend accepts the approved 80x24 geometry");
    let mut fullscreen_options = options.clone();
    fullscreen_options.screen = Screen::Fullscreen;
    dal_tui::draw_frame(
        &mut fullscreen,
        Screen::Fullscreen,
        &view,
        &fullscreen_options,
    )?;
    let fullscreen_text = render_buffer(&fullscreen, data.path(), workspace.path());
    assert_eq!(fullscreen.backend().buffer().area.height, 24);
    assert!(fullscreen_text.contains(dal_tui::copy::ids::COMPOSER_PLACEHOLDER));
    insta::assert_snapshot!("loop-tui-01__fullscreen", fullscreen_text);

    let _ = harness
        .host
        .shutdown(std::time::Duration::from_secs(1))
        .await;
    Ok(())
}

fn render_buffer(
    terminal: &Terminal<TestBackend>,
    data: &std::path::Path,
    workspace: &std::path::Path,
) -> String {
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for row in buffer.content().chunks(80) {
        for cell in row {
            text.push_str(cell.symbol());
        }
        text.push('\n');
        for cell in row {
            use std::fmt::Write as _;
            let _ = write!(text, "{:?}/{:?}/{:?} ", cell.fg, cell.bg, cell.modifier);
        }
        text.push('\n');
    }
    if let Some(path) = data.to_str() {
        text = text.replace(path, "[data]");
    }
    if let Some(path) = workspace.to_str() {
        text = text.replace(path, "[workspace]");
    }
    for path in [data, workspace] {
        if let Some(name) = Path::new(path).file_name().and_then(|name| name.to_str()) {
            text = text.replace(name, "[root]");
        }
    }
    // The status row truncates the workspace path to its leading cells, so a
    // temp-dir spelling deeper than that never reaches the path masks above.
    // Fold every head the truncation can leave back to the same root token.
    if let Some(temp) = std::env::temp_dir().to_str().map(str::to_owned) {
        for head in (8..=temp.len()).rev() {
            if temp.is_char_boundary(head) {
                text = text.replace(&temp[..head], "[root]");
            }
        }
    }
    for prefix in ["/tmp/dalgon-gates-", "dalgon-gates-"] {
        let mut search = 0;
        while let Some(offset) = text[search..].find(prefix) {
            let start = search + offset;
            let rest = &text[start + prefix.len()..];
            let mut matched = rest
                .find(|ch: char| !ch.is_ascii_digit())
                .unwrap_or(rest.len());
            if let Some(suffix) = rest[matched..].strip_prefix('-') {
                let id_digits = suffix
                    .find(|ch: char| !ch.is_ascii_digit())
                    .unwrap_or(suffix.len());
                matched += 1 + id_digits;
            }
            text.replace_range(start..start + prefix.len() + matched, "[root]");
            search = start + "[root]".len();
        }
    }
    text
}
