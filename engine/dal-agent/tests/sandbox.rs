//! Live Linux sandbox proofs: the dal-agent launcher admits and refuses
//! through the real `dalgon __sandbox` Landlock helper.
//!
//! The helper binary is the sibling `dalgon` binary built from this
//! workspace (`cargo build -p dalgon --bin dalgon`); the test locates it
//! next to the test binary's target directory. Sync filesystem and process
//! setup stays outside the async runtime; the runtime only drives the
//! Host/agent calls.

#![expect(
    clippy::expect_used,
    reason = "fixture setup must fail at its boundary"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
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

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"probe-call\",\"name\":\"fixture__probe\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"probe-call\",\"name\":\"fixture__probe\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// Locates the `dalgon` binary that serves as the sandbox helper.
///
/// The helper is the workspace `dalgon` binary. When the caller supplies
/// `DAL_SANDBOX_HELPER`, that explicit path identifies the helper alone: an
/// invalid path fails the run instead of silently falling back, so one
/// explicit input names exactly one binary. Without the variable the test
/// uses the sibling of this test binary `<target>/debug/dalgon`; Cargo keeps
/// intermediate dep binaries in `deps/` and the exported bin at
/// `<target>/debug/dalgon`, so the sibling is the normal build-layout
/// answer. Panics with the exact build command otherwise.
fn helper_binary() -> PathBuf {
    let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    if let Some(exe) = environment.remove(std::ffi::OsStr::new("DAL_SANDBOX_HELPER")) {
        let path = PathBuf::from(exe);
        assert!(
            path.is_file(),
            "DAL_SANDBOX_HELPER names a missing file: {}",
            path.display(),
        );
        return path;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let debug = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("target debug dir");
    let candidate = debug.join("dalgon");
    assert!(
        candidate.is_file(),
        "build the sandbox helper first: cargo build -p dalgon --bin dalgon (looked in {}; \
         set DAL_SANDBOX_HELPER to an explicit dalgon path to override)",
        candidate.display(),
    );
    candidate
}

/// A model-visible tool that runs one shell script through the same checked
/// door the exec tool uses: authorize, spawn, wait. The settled status and
/// the capture's denial flag are reported as text so the test observes the
/// real enforcement outcome.
struct ShellProbe {
    name: Name,
    spec: Arc<ToolSpec>,
    script: String,
}

impl Tool for ShellProbe {
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
                title: "shell-probe".into(),
                body: self.script.clone().into(),
                digest: None,
            };
            let approved = match cx.authorize(preview).await {
                Ok(approved) => approved,
                Err(reason) => return ToolOutcome::Err(ToolError::Denied(reason)),
            };
            let argv = [
                OsString::from("sh"),
                OsString::from("-c"),
                OsString::from(&self.script),
            ];
            let opts = SpawnOpts {
                cwd: cx.workspace().as_path().to_path_buf(),
                timeout: None,
                env: Vec::new(),
                stdout_prefix_limit: 4096,
            };
            let mut proc = match cx.spawn(&argv, opts, approved) {
                Ok(proc) => proc,
                Err(error) => return ToolOutcome::Err(error),
            };
            match proc.wait(cx.cancel()).await {
                Ok(result) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(format!(
                    "probe status={status:?} denial_seen={denial} prefix={prefix} tail={tail}",
                    status = result.status,
                    denial = result.denial_seen,
                    prefix = String::from_utf8_lossy(&result.stdout_prefix),
                    tail = String::from_utf8_lossy(&result.completion_tail),
                )))),
                Err(error) => ToolOutcome::Err(error),
            }
        })
    }
}

/// Synchronous fixture: data, workspace, home, cache, and a captured temp
/// root that is a sibling of (never a parent of) the protected data root,
/// so no overlap refusal can fire and the helper door is what the test
/// observes.
struct Fixture {
    _guard: tempfile::TempDir,
    data: PathBuf,
    workspace_dir: PathBuf,
    vars: BTreeMap<OsString, OsString>,
}

fn setup_fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("fixture root");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    let stemp = tmp.path().join("stemp");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    std::fs::create_dir_all(&home).expect("home dir");
    std::fs::create_dir_all(&cache).expect("cache dir");
    std::fs::create_dir_all(&stemp).expect("captured temp dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}")).expect("script fixture");
    let vars = BTreeMap::from([
        (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
        (OsString::from("HOME"), OsString::from(&home)),
        (OsString::from("XDG_CACHE_HOME"), OsString::from(&cache)),
        (OsString::from("TMPDIR"), OsString::from(&stemp)),
    ]);
    Fixture {
        _guard: tmp,
        data,
        workspace_dir,
        vars,
    }
}

/// Drives one scripted turn whose probe tool runs `script` under the
/// sandboxed launcher. Only Host/agent calls run on the runtime; all
/// filesystem assertions happen outside, on the returned view.
async fn run_sandboxed_turn(fixture: &Fixture, helper: &Path, script: &str) -> dal_core::View {
    let user = format!(
        "sandbox = true\napproval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture
            .data
            .join("script.jsonl")
            .to_string_lossy()
            .replace('\\', "\\\\")
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        &fixture.data,
        "",
        Some(user.as_str()),
    )
    .expect("config");
    let extension = probe_extension(script);
    let env = Env {
        vars: fixture.vars.clone(),
        cwd: fixture.workspace_dir.clone(),
        sandbox_helper: Some(helper.to_path_buf()),
    };
    let product = Product {
        name: "dal",
        data_root: fixture.data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(fixture.workspace_dir.clone()).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let _ = agent
        .submit(Command::SetApproval {
            mode: dal_core::ApprovalMode::All,
            save: dal_core::Save::SessionOnly,
        })
        .await
        .expect("approval");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "spawn the probe".into(),
            }],
        })
        .await
        .expect("prompt");
    assert!(
        matches!(reply, dal_core::Reply::Accepted { .. }),
        "prompt not accepted: {reply:?}"
    );
    let mut ended = false;
    for _ in 0..20 {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await;
        let Ok(Some(delivery)) = delivery else {
            break;
        };
        if let dal_agent::Delivery::Update(update) = &delivery
            && matches!(update.kind, dal_core::UpdateKind::TurnEnded { .. })
        {
            ended = true;
            break;
        }
    }
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    assert!(ended, "turn runs to TurnEnded on the scripted fixture");
    view
}

/// The probe tool and its extension: one shell script through the same
/// checked door the exec tool uses.
fn probe_extension(script: &str) -> dal_agent::ext::Extension {
    let probe = Arc::new(ShellProbe {
        name: Name::parse("fixture__probe").expect("probe name"),
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__probe").expect("probe name"),
            description: "sandbox spawn probe".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("empty params"),
            grammar: None,
        }),
        script: script.to_string(),
    });
    ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)
        .expect("extension builder")
        .script_tool(
            probe,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture").expect("plugin name"),
                    kind: ExportKind::Tool,
                    local: Name::parse("probe").expect("tool name"),
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#).expect("empty input"),
                description: "sandbox spawn probe export".into(),
            },
        )
        .build()
        .expect("extension")
}

fn block_on_turn(fixture: &Fixture, helper: &Path, script: &str) -> dal_core::View {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(run_sandboxed_turn(fixture, helper, script))
}

/// The probe gate itself executes on this platform: the helper prints one
/// decimal Landlock ABI followed by `\n`, and this kernel reports ABI 3 or
/// newer, which is what the launcher admission requires.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test harness: the fixture probes the built sandbox helper binary"
)]
fn helper_probe_reports_supported_abi_before_any_session_child_starts() {
    let helper = helper_binary();
    let output = std::process::Command::new(&helper)
        .arg("__sandbox")
        .arg("--probe")
        .output()
        .expect("probe spawns");
    assert!(output.status.success(), "probe exits 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let token = stdout
        .strip_suffix('\n')
        .expect("probe output ends with newline");
    assert!(
        !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_digit()),
        "probe prints one decimal ABI, got: {stdout:?}"
    );
    let abi: u32 = token.parse().expect("decimal ABI parses");
    assert!(
        abi >= 3,
        "launcher admission needs Landlock ABI 3, helper reports {abi}"
    );
}

/// A sandboxed write inside the allowed workspace succeeds: the child runs
/// through the helper and its file lands.
#[test]
fn sandboxed_write_inside_allowed_root_succeeds() {
    let fixture = setup_fixture();
    let helper = helper_binary();
    let view = block_on_turn(&fixture, &helper, "printf spawned > probe.txt");
    let dump = format!("{view:?}");
    assert!(
        dump.contains("denial_seen=false"),
        "no denial flag on an allowed write: {dump}"
    );
    assert!(
        fixture.workspace_dir.join("probe.txt").is_file(),
        "the sandboxed child wrote its workspace file"
    );
}

/// A sandboxed write outside every allowed root is refused: the kernel
/// denies the write, the denial phrase surfaces, and the file never lands.
/// A direct-spawn fallback would have created the file, so absence proves
/// the child ran under enforcement.
#[test]
fn sandboxed_write_outside_allowed_roots_is_refused() {
    // The deny target lives outside the workspace, the captured temp root,
    // and the cache root, so no grant covers it.
    let outside = PathBuf::from("/dev/shm").join(format!("sandbox-deny-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).expect("outside dir");
    let target = outside.join("evil.txt");
    assert!(!target.exists(), "deny target starts absent");

    let fixture = setup_fixture();
    let helper = helper_binary();
    let script = format!("printf evil > {}", target.display());
    let view = block_on_turn(&fixture, &helper, &script);
    let dump = format!("{view:?}");
    assert!(
        !target.exists(),
        "the sandboxed child must not write outside its roots"
    );
    assert!(
        dump.contains("Permission denied") || dump.contains("denial_seen=true"),
        "the kernel refusal surfaces in the turn: {dump}"
    );
    let _ = std::fs::remove_dir_all(&outside);
}
