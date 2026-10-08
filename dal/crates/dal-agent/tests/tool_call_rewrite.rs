#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! A `tool_call` hook rewrite is classified again before the tool runs: an
//! invalid rewrite fails the call closed and a valid rewrite still runs.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, Extension, ExtensionBuilder, Hook, HookCx, HookError,
    ObserveHook, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::ext::{ExportId, ExportKind, OpSet, ToolCallEvent, ToolCallVerdict};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part, RawJson, ServiceSet,
    ToolClass, ToolResultEvent, ToolSpec, Visibility, Workspace,
};

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"rewrite-call\",\"name\":\"fixture__stat\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"rewrite-call\",\"name\":\"fixture__stat\",\"args\":{\"kind\":\"parsed\",\"value\":{\"path\":\"inside.txt\"}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// Observed `(ok, preview)` pairs for settled tool results.
type Settled = Arc<Mutex<Vec<(bool, Box<str>)>>>;

#[derive(serde::Deserialize)]
struct PathArgs {
    path: String,
}

/// A tool whose classification rejects paths outside the workspace.
struct Stat {
    name: Name,
    spec: Arc<ToolSpec>,
    ran: Arc<Mutex<Vec<String>>>,
}

impl Tool for Stat {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, args: &RawValue, ws: &Workspace) -> Result<ToolClass, ArgError> {
        let parsed: PathArgs = sonic_rs::from_str(args.as_str())
            .map_err(|error| ArgError::message(format!("path is required: {error}.")))?;
        let path = Path::new(&parsed.path);
        let escapes = path.components().any(|part| part == Component::ParentDir)
            || (path.is_absolute() && !path.starts_with(ws.as_path()));
        if escapes {
            return Err(ArgError::message(format!(
                "{} is outside the workspace.",
                parsed.path
            )));
        }
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        self.ran
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call.args.as_str().to_owned());
        Box::pin(async { ToolOutcome::Ok(ToolOutput::from_text("ran")) })
    }
}

struct Rewrite {
    args: &'static str,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for Rewrite {
    fn call(
        &self,
        _input: ToolCallEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let args = RawJson::parse(self.args).expect("rewrite arguments");
        Box::pin(async { Ok(ToolCallVerdict::Rewrite { args }) })
    }
}

struct Results {
    seen: Settled,
}

impl ObserveHook<ToolResultEvent> for Results {
    fn call(
        &self,
        input: ToolResultEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let seen = Arc::clone(&self.seen);
        Box::pin(async move {
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((input.ok, input.preview));
            Ok(())
        })
    }
}

fn extension(rewrite: &'static str, ran: Arc<Mutex<Vec<String>>>, seen: Settled) -> Extension {
    let name = Name::parse("fixture__stat").expect("tool name");
    let stat = Arc::new(Stat {
        name: name.clone(),
        spec: Arc::new(ToolSpec {
            name,
            description: "stat a workspace path".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("tool schema"),
            grammar: None,
        }),
        ran,
    });
    ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .on_tool_call(Rewrite { args: rewrite })
        .on_tool_result(Results { seen })
        .script_tool(
            stat,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture").expect("plugin name"),
                    kind: ExportKind::Tool,
                    local: Name::parse("stat").expect("export name"),
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#).expect("export schema"),
                description: "stat export".into(),
            },
        )
        .build()
        .expect("extension")
}

/// Runs one scripted call whose arguments the hook rewrites, then returns
/// the settled result and the arguments the tool actually ran with.
async fn run_with_rewrite(rewrite: &'static str) -> ((bool, Box<str>), Vec<String>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}")).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let ran = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension(rewrite, Arc::clone(&ran), Arc::clone(&seen))],
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
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("rewrite-test"),
        )
        .await
        .expect("session");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "stat the file".into(),
            }],
        })
        .await
        .expect("prompt accepted");
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let first = seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .first()
                .cloned();
            if let Some(result) = first {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("tool result arrived");
    let ran = ran
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    (result, ran)
}

#[tokio::test]
async fn rewrite_to_an_out_of_workspace_path_fails_closed_without_running_the_tool() {
    let ((ok, preview), ran) = run_with_rewrite(r#"{"path":"/etc/passwd"}"#).await;
    assert!(!ok, "the rewritten call must fail: {preview}");
    assert!(
        preview.contains("invalid arguments for fixture__stat")
            && preview.contains("/etc/passwd is outside the workspace."),
        "the denial names the resolve error: {preview}"
    );
    assert!(ran.is_empty(), "the tool must not run: {ran:?}");
}

#[tokio::test]
async fn rewrite_to_a_valid_path_still_runs_with_the_rewritten_arguments() {
    let ((ok, preview), ran) = run_with_rewrite(r#"{"path":"other.txt"}"#).await;
    assert!(ok, "the valid rewrite must run: {preview}");
    assert_eq!(ran, vec![r#"{"path":"other.txt"}"#.to_owned()]);
}
