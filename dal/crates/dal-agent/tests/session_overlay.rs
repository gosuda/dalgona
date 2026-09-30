#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! A session tool a battery registers at run time runs through the hooks and
//! the ladder, and its first successful call is journaled as `ToolPromoted`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, Extension, ExtensionBuilder, Hook, HookCx, HookError, ObserveHook,
    RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::ext::{SessionStart, ToolCallEvent, ToolCallVerdict};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part, RawJson, ServiceSet,
    ToolClass, ToolSpec, Visibility, Workspace,
};

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"c1\",\"name\":\"mcp_echo\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"c1\",\"name\":\"mcp_echo\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

struct Echo {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for Echo {
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

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Ok(ToolOutput::from_text("echoed")) })
    }
}

fn echo() -> Arc<dyn Tool> {
    let name = Name::parse("mcp_echo").expect("tool name");
    Arc::new(Echo {
        spec: Arc::new(ToolSpec {
            name: name.clone(),
            description: "overlay echo".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
            grammar: None,
        }),
        name,
    })
}

struct Register {
    registered: Arc<Mutex<bool>>,
}

impl ObserveHook<SessionStart> for Register {
    fn call(&self, _input: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let registered = Arc::clone(&self.registered);
        Box::pin(async move {
            cx.services
                .add_session_tools(&cx.caller, vec![(echo(), Visibility::Deferred)])
                .await
                .map_err(|error| HookError::Failed {
                    message: error.to_string().into(),
                })?;
            *registered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            Ok(())
        })
    }
}

struct Watch {
    calls: Arc<Mutex<Vec<String>>>,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for Watch {
    fn call(
        &self,
        input: ToolCallEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(input.tool.to_string());
        Box::pin(async { Ok(ToolCallVerdict::Allow) })
    }
}

fn battery(registered: &Arc<Mutex<bool>>, calls: &Arc<Mutex<Vec<String>>>) -> Extension {
    ExtensionBuilder::new("battery", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .on_session_start(Register {
            registered: Arc::clone(registered),
        })
        .on_tool_call(Watch {
            calls: Arc::clone(calls),
        })
        .build()
        .expect("extension")
}

fn journals_naming(root: &Path, needle: &str) -> usize {
    let mut found = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(text) = std::fs::read_to_string(&path)
                && text
                    .lines()
                    .any(|line| line.contains("\"tool_promoted\"") && line.contains(needle))
            {
                found += 1;
            }
        }
    }
    found
}

async fn wait_until(what: &str, check: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test]
async fn session_tool_runs_through_hooks_and_first_success_is_journaled_as_promoted() {
    let guard = tempfile::tempdir().expect("tempdir");
    let tmp = guard.path().to_path_buf();
    let data = tmp.join("data");
    let workspace_dir = tmp.join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}")).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let registered = Arc::new(Mutex::new(false));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![battery(&registered, &calls)],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (OsString::from("HOME"), OsString::from(tmp.join("home"))),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.join("cache")),
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
            ClientId::new("overlay-test"),
        )
        .await
        .expect("session");
    let session = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id;
    wait_until("the battery to register its session tool", || {
        *registered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    })
    .await;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "call the overlay tool".into(),
            }],
        })
        .await
        .expect("prompt accepted");
    wait_until("the overlay call to finish", || {
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|tool| tool == "mcp_echo")
    })
    .await;
    host.close(session).await.expect("close");

    assert_eq!(
        journals_naming(&data, "mcp_echo"),
        1,
        "the first successful call of a deferred session tool journals ToolPromoted once"
    );
}
