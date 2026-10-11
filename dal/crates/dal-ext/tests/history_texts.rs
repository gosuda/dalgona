#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! `Services::history_texts` returns the prior assistant turn texts from the
//! journal of a resumed session, and is empty for a fresh one.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{BoxFuture, Extension, ExtensionBuilder, HookCx, HookError, ObserveHook};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::ext::{SessionStart, Settled};
use dal_core::{ClientId, Command, Config, ConfigProduct, Expect, Part, ServiceSet, Workspace};

fn step(text: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{text}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

#[derive(Debug, Eq, PartialEq)]
enum Seen {
    Start { resumed: bool, texts: Vec<String> },
    Settled,
}

struct Log(Arc<Mutex<Vec<Seen>>>);

impl Log {
    fn push(log: &Arc<Mutex<Vec<Seen>>>, seen: Seen) {
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(seen);
    }
}

impl ObserveHook<SessionStart> for Log {
    fn call(&self, input: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let log = Arc::clone(&self.0);
        Box::pin(async move {
            let texts = cx
                .services
                .history_texts(&cx.caller)
                .await
                .map_err(|error| HookError::Failed {
                    message: error.to_string().into(),
                })?;
            Log::push(
                &log,
                Seen::Start {
                    resumed: input.resumed,
                    texts,
                },
            );
            Ok(())
        })
    }
}

impl ObserveHook<Settled> for Log {
    fn call(&self, _input: Settled, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let log = Arc::clone(&self.0);
        Box::pin(async move {
            Log::push(&log, Seen::Settled);
            Ok(())
        })
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "dal-history-texts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn extension(log: &Arc<Mutex<Vec<Seen>>>) -> Extension {
    ExtensionBuilder::new("history-probe", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .on_session_start(Log(Arc::clone(log)))
        .on_settled(Log(Arc::clone(log)))
        .build()
        .expect("extension")
}

async fn wait_for(log: &Arc<Mutex<Vec<Seen>>>, want: impl Fn(&[Seen]) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if want(
                &log.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("hook event arrived");
}

async fn start_host(scratch: &Scratch, log: &Arc<Mutex<Vec<Seen>>>) -> (Host, Workspace) {
    let data = scratch.0.join("data");
    let workspace_dir = scratch.0.join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(
        &fixture,
        format!("{}{}", step("first answer"), step("second answer")),
    )
    .expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension(log)],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(scratch.0.join("home")),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(scratch.0.join("cache")),
            ),
        ]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    (host, Workspace::new(workspace_dir).expect("workspace"))
}

async fn prompt(agent: &dal_agent::Agent, text: &str) {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("prompt accepted");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario drives setup, resume, and assertions in place"
)]
async fn resumed_session_sees_prior_assistant_texts_and_fresh_sees_none() {
    let scratch = Scratch::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let (host, workspace) = start_host(&scratch, &log).await;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("history-new"),
        )
        .await
        .expect("session");
    let session = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id;
    wait_for(&log, |seen| !seen.is_empty()).await;
    prompt(&agent, "one").await;
    wait_for(&log, |seen| seen.contains(&Seen::Settled)).await;
    prompt(&agent, "two").await;
    wait_for(&log, |seen| {
        seen.iter().filter(|event| **event == Seen::Settled).count() == 2
    })
    .await;
    host.close(session).await.expect("close");

    let resumed = host
        .open(
            SessionRef::Continue {
                workspace: workspace.clone(),
            },
            ClientId::new("history-continue"),
        )
        .await
        .expect("continued session");
    assert_eq!(
        resumed
            .view(dal_core::PageReq::default())
            .expect("view")
            .session
            .id,
        session
    );
    wait_for(&log, |seen| {
        seen.iter()
            .filter(|event| matches!(event, Seen::Start { .. }))
            .count()
            == 2
    })
    .await;
    let starts: Vec<_> = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(|event| match event {
            Seen::Start { resumed, texts } => Some((*resumed, texts.clone())),
            Seen::Settled => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![
            (false, Vec::new()),
            (
                true,
                vec!["first answer".to_owned(), "second answer".to_owned()]
            ),
        ]
    );

    let fresh = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("history-fresh"),
        )
        .await
        .expect("fresh session");
    wait_for(&log, |seen| {
        seen.iter()
            .filter(|event| matches!(event, Seen::Start { .. }))
            .count()
            == 3
    })
    .await;
    let last = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .rev()
        .find_map(|event| match event {
            Seen::Start { resumed, texts } => Some((*resumed, texts.clone())),
            Seen::Settled => None,
        });
    assert_eq!(
        last,
        Some((false, Vec::new())),
        "a new session beside a resumed one sees none of its texts"
    );
    let fresh_id = fresh
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id;
    host.close(fresh_id).await.expect("fresh close");
    host.close(session).await.expect("resumed close");
}
