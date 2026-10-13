#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]

//! Headless and TUI probes against the built product binaries.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{collections::BTreeMap, error::Error, fs, io, path::PathBuf, time::Duration};

use dal_agent::{Agent, Delivery, Env, SessionRef};
use dal_core::{
    Command, Config, ConfigProduct, Expect, PageReq, Part, Reply, StreamChannel, UpdateKind,
    Workspace,
};
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiOptions, WidthMode};
use dal_wire::{MemoryTransport, serve_rpc};
use ratatui::{Terminal, backend::TestBackend};
use sonic_rs::{JsonValueTrait, Value};
use support::{TestDir, scripted_session};

const REMOVALS: [&str; 3] = ["guard", "subagent", "sandbox"];
const HEADLESS_RESPONSE: &str = concat!(
    "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"focus-probe\",\"name\":\"focus__focus\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"focus-probe\",\"name\":\"focus__focus\",\"args\":{\"kind\":\"parsed\",\"value\":{\"value\":\"extension removal probe\"}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n",
    "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"surface probe assistant\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
);

async fn headless_probe(agent: &Agent) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Return the scripted surface response.".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));
    let (text, focus_result) = tokio::time::timeout(Duration::from_secs(10), async {
        let mut text = String::new();
        let mut focus_result = false;
        loop {
            let Some(delivery) = subscription.next().await else {
                return Err(io::Error::other("headless probe subscription closed"));
            };
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::Delta {
                    channel: StreamChannel::Text,
                    text: chunk,
                    ..
                } => text.push_str(chunk),
                UpdateKind::ToolSettled { call, outcome } if call.as_str() == "focus-probe" => {
                    assert!(!outcome.is_error, "{outcome:?}");
                    assert!(
                        outcome.text.contains("extension removal probe"),
                        "{outcome:?}"
                    );
                    focus_result = true;
                }
                UpdateKind::TurnEnded { .. } => break,
                _ => {}
            }
        }
        Ok::<_, io::Error>((text, focus_result))
    })
    .await??;
    assert_eq!(text, "surface probe assistant");
    assert!(focus_result, "the loaded focus tool must run successfully");
    Ok(())
}

fn tui_probe(agent: &Agent, session: SessionRef) -> Result<(), Box<dyn Error + Send + Sync>> {
    let view = agent.view(PageReq::default())?;
    assert!(matches!(view.turn, dal_core::TurnState::Idle));
    let options = TuiOptions {
        session,
        screen: Screen::Fullscreen,
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
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    dal_tui::draw_frame(&mut terminal, Screen::Fullscreen, &view, &options)?;
    let rendered = render_tui_buffer(&terminal);
    assert!(rendered.contains("surface probe assistant"), "{rendered}");
    Ok(())
}

fn render_tui_buffer(terminal: &Terminal<TestBackend>) -> String {
    let mut text = String::new();
    for row in terminal.backend().buffer().content().chunks(80) {
        for cell in row {
            text.push_str(cell.symbol());
        }
        text.push('\n');
    }
    text
}

async fn rpc_probe(host: dal_agent::Host) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (transport, mut peer) = MemoryTransport::pair(8);
    let server = serve_rpc(host, transport);
    let client = async move {
        let request = sonic_rs::to_string(&sonic_rs::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientInfo": {"name": "full-product-gate", "version": "1"},
                "capabilities": ["sessions"]
            }
        }))?;
        peer.send_frame(request).await?;
        let frame = tokio::time::timeout(Duration::from_secs(5), peer.read_frame())
            .await?
            .expect("RPC server returned an initialize response");
        let response: Value = sonic_rs::from_str(&frame)?;
        assert_eq!(
            response
                .get("result")
                .and_then(|result| result.get("protocolVersion"))
                .and_then(sonic_rs::JsonValueTrait::as_i64),
            Some(1),
            "{response}"
        );
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    };
    let (server_result, client_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, client)
    })
    .await?;
    server_result?;
    client_result?;
    Ok(())
}

async fn probe_without(extension_name: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/dalgon/tests/fixtures");
    let plugin_dir = data.path().join("plugins/focus");
    fs::create_dir_all(&plugin_dir)?;
    fs::copy(
        fixtures.join("plugins/focus.star"),
        plugin_dir.join("plugin.star"),
    )?;
    let replay = data.path().join("surface-scripted.jsonl");
    fs::write(&replay, HEADLESS_RESPONSE)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\nplugins = [\"focus\"]\n[providers.scripted]\nfixture = {:?}\n",
        replay.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let focus = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "focus")
        .unwrap();
    assert_eq!(focus.version(), "0.1.0");
    let present = product
        .extensions
        .iter()
        .filter(|extension| extension.name() == extension_name)
        .count();
    assert_eq!(
        present, 1,
        "expected one {extension_name} extension to remove"
    );
    let original_len = product.extensions.len();
    product
        .extensions
        .retain(|extension| extension.name() != extension_name);
    assert_eq!(product.extensions.len() + 1, original_len);
    let env = Env {
        vars: BTreeMap::default(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session.clone()).await?;
    headless_probe(&harness.agent).await?;
    tui_probe(&harness.agent, session)?;
    rpc_probe(harness.host.clone()).await?;
    let report = tokio::time::timeout(
        Duration::from_secs(5),
        harness.host.shutdown(Duration::from_secs(1)),
    )
    .await?;
    assert_eq!(report.sessions_closed, 1);
    Ok(())
}

#[tokio::test]
async fn removing_non_load_bearing_extensions_keeps_first_four_gates_green()
-> Result<(), Box<dyn Error + Send + Sync>> {
    for extension in REMOVALS {
        probe_without(extension).await?;
    }
    Ok(())
}
