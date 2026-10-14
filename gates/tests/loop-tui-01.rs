//! `TestBackend` snapshots for the inline and fullscreen TUI screens.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, path::PathBuf};

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
        .join("../cli/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
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
    let mut view = harness.agent.view(PageReq::default())?;
    view.session.workspace = Workspace::new(deterministic_workspace())?;
    let display_workspace = deterministic_workspace();
    let options = TuiOptions {
        session,
        screen: Screen::Inline,
        theme_request: ThemeRequest::Palette,
        default_model: None,
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
    let inline_text = render_buffer(&inline, data.path(), &display_workspace);
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
    let fullscreen_text = render_buffer(&fullscreen, data.path(), &display_workspace);
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
        text = text.replace(path, "[root]");
    }
    text
}

#[cfg(windows)]
fn deterministic_workspace() -> PathBuf {
    PathBuf::from(r"C:\test-workspace-012345")
}

#[cfg(not(windows))]
fn deterministic_workspace() -> PathBuf {
    PathBuf::from("/test-workspace-01234567")
}
