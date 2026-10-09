//! Sandbox launcher boundary: when the sandbox is on, every child runs
//! through the helper or is refused with the exact setup text; it never
//! falls back to an unsandboxed spawn. When the sandbox is off, the same
//! call runs directly.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{Env, Host, Product, SessionRef, SpawnOpts, ToolError};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part, Preview, RawJson,
    ToolClass, ToolSpec, Visibility, Workspace,
};

const REFUSAL: &str = "sandbox: no sandbox helper. SDK embedders must pass a helper path; the dalgon binary provides dalgon __sandbox.";

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"probe-call\",\"name\":\"fixture__probe\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"probe-call\",\"name\":\"fixture__probe\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// A model-visible tool that spawns one workspace write through the same
/// checked door the exec tool uses: authorize, spawn, wait.
struct SpawnProbe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for SpawnProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let preview = Preview {
                title: "spawn-probe".into(),
                body: "printf spawned > probe.txt".into(),
                digest: None,
            };
            let approved = match cx.authorize(preview).await {
                Ok(approved) => approved,
                Err(reason) => return ToolOutcome::Err(ToolError::Denied(reason)),
            };
            let argv = [
                OsString::from("sh"),
                OsString::from("-c"),
                OsString::from("printf spawned > probe.txt"),
            ];
            let opts = SpawnOpts {
                cwd: cx.workspace().as_path().to_path_buf(),
                timeout: None,
                env: Vec::new(),
                stdout_prefix_limit: 0,
            };
            let mut proc = match cx.spawn(&argv, opts, approved) {
                Ok(proc) => proc,
                Err(error) => return ToolOutcome::Err(error),
            };
            match proc.wait(cx.cancel()).await {
                Ok(result) => ToolOutcome::Ok(ToolOutput::from_text(format!(
                    "probe exit {status:?}",
                    status = result.status
                ))),
                Err(error) => ToolOutcome::Err(error),
            }
        })
    }
}

/// Starts one host with the probe tool registered and runs one turn whose
/// provider step calls it; returns the final view, the workspace path, and
/// the retained temp guard so assertions observe a live filesystem.
#[expect(
    clippy::too_many_lines,
    reason = "one linear fixture build keeps the boundary test readable"
)]
async fn run_probe_turn(
    sandbox: bool,
) -> Result<(dal_core::View, std::path::PathBuf, tempfile::TempDir), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}"))?;
    let mut user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    if sandbox {
        user = format!("sandbox = true\n{user}");
    }
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let probe = Arc::new(SpawnProbe {
        name: Name::parse("fixture__probe")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__probe")?,
            description: "spawn probe tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
    });
    let extension = ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .script_tool(
            probe,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture")?,
                    kind: ExportKind::Tool,
                    local: Name::parse("probe")?,
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#)?,
                description: "spawn probe export".into(),
            },
        )
        .build()?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(tmp.path().join("home")),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.path().join("cache")),
            ),
        ]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let workspace = Workspace::new(workspace_dir.clone())?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    // The fold starts at the ask policy; the probe must run without a
    // front end so the spawn door itself is what the test observes.
    let _ = agent
        .submit(Command::SetApproval {
            mode: dal_core::ApprovalMode::All,
            save: dal_core::Save::SessionOnly,
        })
        .await?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "spawn the probe".into(),
            }],
        })
        .await?;
    assert!(
        matches!(reply, dal_core::Reply::Accepted { .. }),
        "prompt not accepted: {reply:?}"
    );
    let mut ended = false;
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..20 {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await;
        let Ok(Some(delivery)) = delivery else {
            break;
        };
        if let dal_agent::Delivery::Update(update) = &delivery {
            seen.push(format!("{:?}", update.kind));
            if matches!(update.kind, dal_core::UpdateKind::TurnEnded { .. }) {
                ended = true;
                break;
            }
        }
    }
    let view = agent.view(dal_core::PageReq::default())?;
    assert!(
        ended,
        "turn runs to TurnEnded on the scripted fixture (seen={seen:?}, turn={:?})",
        view.turn,
    );
    Ok((view, workspace_dir, tmp))
}

/// One probe turn with the sandbox configured on and no helper.
#[cfg(all(not(windows), not(target_os = "macos")))]
async fn sandbox_on_turn()
-> Result<(dal_core::View, std::path::PathBuf, tempfile::TempDir), Box<dyn std::error::Error>> {
    run_probe_turn(true).await
}

/// One probe turn with the sandbox configured off.
async fn sandbox_off_turn()
-> Result<(dal_core::View, std::path::PathBuf, tempfile::TempDir), Box<dyn std::error::Error>> {
    run_probe_turn(false).await
}

/// The helper-less refusal is the Linux contract: macOS enforces through
/// Seatbelt (always present on a macOS host) and Windows refuses every
/// sandboxed spawn with its own setup text.
#[cfg(all(not(windows), not(target_os = "macos")))]
#[tokio::test]
async fn sandbox_on_without_helper_refuses_and_never_spawns() {
    let (view, workspace_dir, _guard) = sandbox_on_turn().await.expect("sandbox-on turn");
    let dump = format!("{view:?}");
    assert!(
        dump.contains(REFUSAL),
        "the refusal text reaches the journal: {dump}"
    );
    assert!(
        !workspace_dir.join("probe.txt").exists(),
        "a refused sandbox must never spawn the child unsandboxed"
    );
}

#[tokio::test]
async fn sandbox_off_spawns_directly() {
    let (view, workspace_dir, _guard) = sandbox_off_turn().await.expect("sandbox-off turn");
    let dump = format!("{view:?}");
    assert!(
        !dump.contains(REFUSAL),
        "the sandbox is off, so no refusal may appear: {dump}"
    );
    assert!(
        workspace_dir.join("probe.txt").exists(),
        "the probe child ran directly and wrote its file"
    );
}
