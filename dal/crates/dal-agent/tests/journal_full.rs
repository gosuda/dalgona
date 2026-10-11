#![cfg_attr(
    unix,
    expect(clippy::expect_used, reason = "integration tests fail loudly")
)]
#![cfg_attr(
    unix,
    expect(
        clippy::disallowed_methods,
        reason = "capped children re-exec the test binary"
    )
)]
#![cfg_attr(unix, expect(clippy::panic, reason = "integration tests fail loudly"))]

//! A resolution the journal cannot take must fail closed: the answerer
//! gets the journal error, no resolution is published, and the session
//! reports the failure instead of acknowledging the answer.
//!
//! The fault is a real filesystem limit: the child process runs under
//! `ulimit -f`, so appends past the cap fail with EFBIG as root or not.
//! `SIGXFSZ` stays ignored across the exec, which turns the oversize write
//! into a recoverable error instead of a dead process. The limit is set
//! high enough for session setup and the approval opening, but far below
//! the 64 KiB answer the test resolves with, so alignment cannot matter.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, Extension, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef, Subscription, ToolError};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part, Question,
    RawJson, ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};

const WAIT: Duration = Duration::from_secs(20);
/// Journal headroom in 512-byte blocks: setup needs a few KiB, while both
/// capped appends carry 64 KiB, so the cap fails them under either shell
/// block-size convention.
const FSIZE_BLOCKS: &str = "48";

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"ask-call\",\"name\":\"fixture__ask_big\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"ask-call\",\"name\":\"fixture__ask_big\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct AskBig {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for AskBig {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let question = Question::Text {
                prompt: "say it back".into(),
                placeholder: None,
            };
            match cx.services().ask(cx.caller(), question).await {
                Ok(Some(Answer::Value(value))) => {
                    ToolOutcome::Ok(Box::new(ToolOutput::from_text(format!("{value:?}"))))
                }
                Ok(other) => ToolOutcome::Err(ToolError::message(format!(
                    "the question did not resolve to a value: {other:?}"
                ))),
                Err(error) => ToolOutcome::Err(ToolError::message(error.to_string())),
            }
        })
    }
}

/// Builds the capped child's host: temp tree, scripted provider fixture,
/// config, product, and environment.
async fn child_setup(
    extensions: Vec<Extension>,
) -> Result<(tempfile::TempDir, Host, Workspace), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}"))?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
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
    let workspace = Workspace::new(workspace_dir)?;
    let host = Host::start(product, config, env).await?;
    Ok((tmp, host, workspace))
}

/// The big-answer extension export the resolution case asks through.
fn ask_big_extension() -> Result<Extension, Box<dyn std::error::Error>> {
    Ok(ExtensionBuilder::new(
        "fixture",
        "0.1.0",
        dal_core::ServiceSet::from_names(["ask"])?,
    )?
    .script_tool(
        Arc::new(AskBig {
            name: Name::parse("fixture__ask_big")?,
            spec: Arc::new(ToolSpec {
                name: Name::parse("fixture__ask_big")?,
                description: "big answer probe".into(),
                parameters: RawJson::parse(r#"{"type":"object"}"#)?,
                grammar: None,
            }),
        }),
        Visibility::Model,
        ExportSpec {
            id: ExportId {
                plugin: Name::parse("fixture")?,
                kind: ExportKind::Tool,
                local: Name::parse("ask_big")?,
            },
            uses: OpSet::EMPTY,
            input: RawJson::parse(r#"{"type":"object"}"#)?,
            description: "big ask export".into(),
        },
    )
    .build()?)
}

async fn child_main() -> TestResult {
    let (_tmp, host, workspace) = child_setup(vec![ask_big_extension()?]).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let mut subscription: Subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "ask big".into(),
            }],
        })
        .await?;
    let request = tokio::time::timeout(WAIT, async {
        loop {
            let delivery = subscription
                .next()
                .await
                .ok_or("the session closed before the question opened")?;
            if let Delivery::Update(update) = &delivery
                && let UpdateKind::RequestOpened(request) = &update.kind
            {
                return Ok::<_, Box<dyn std::error::Error>>(request.id);
            }
        }
    })
    .await
    .map_err(|_| {
        "the journal failed before the question opened; raise the ulimit -f cap".to_string()
    })??;
    // A 64 KiB answer cannot fit under the file-size cap: its resolution
    // append must fail while every earlier append succeeded.
    let big = "x".repeat(65536);
    let answer = RawJson::parse(&format!("\"{big}\""))?;
    let failed = agent.answer(request, Answer::Value(answer)).await;
    let error = failed
        .err()
        .ok_or("the unjournaled resolution was acknowledged")?;
    assert!(
        error.to_string().contains("journal"),
        "unexpected failure: {error}"
    );
    let resolved = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let delivery = subscription.next().await.ok_or("the session closed")?;
            if let Delivery::Update(update) = &delivery
                && let UpdateKind::RequestResolved { id, .. } = &update.kind
                && *id == request
            {
                return Ok::<_, Box<dyn std::error::Error>>(());
            }
        }
    })
    .await;
    assert!(resolved.is_err(), "an unjournaled resolution was published");
    assert!(
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "again".into()
                }],
            })
            .await
            .is_err(),
        "the session continued after its journal failed"
    );
    Ok(())
}

#[test]
fn a_resolution_the_journal_cannot_take_is_not_acknowledged() {
    if std::env::var("DAL_FSIZE_CHILD").is_ok() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime");
        return runtime.block_on(async {
            if let Err(error) = child_main().await {
                panic!("child failed: {error}");
            }
        });
    }
    run_capped_child(
        "DAL_FSIZE_CHILD",
        "a_resolution_the_journal_cannot_take_is_not_acknowledged",
    );
}
/// The child for the first-append case: a 64 KiB prompt cannot fit under
/// the file-size cap, so the turn-start append fails while setup fit.
async fn first_append_child_main() -> TestResult {
    let (_tmp, host, workspace) = child_setup(Vec::new()).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let mut subscription: Subscription = agent.subscribe(None)?;
    let big = "y".repeat(65536);
    // The prompt is accepted before the turn-start write lands; the 64 KiB
    // entry cannot fit under the cap, so the turn must then stay silent.
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: big.into() }],
        })
        .await?;
    let silent = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let delivery = subscription.next().await.ok_or("the session closed")?;
            if let Delivery::Update(update) = &delivery {
                match &update.kind {
                    UpdateKind::Delta { .. } | UpdateKind::RequestOpened(_) => {
                        return Err::<(), Box<dyn std::error::Error>>(
                            format!(
                                "a provider effect ran after the failed append: {:?}",
                                update.kind
                            )
                            .into(),
                        );
                    }
                    _ => {}
                }
            }
        }
    })
    .await;
    match silent {
        Err(_) => {}
        Ok(inner) => panic!("the failed append still drove provider effects: {inner:?}"),
    }
    assert!(
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "again".into()
                }],
            })
            .await
            .is_err(),
        "the session continued after its journal failed"
    );
    Ok(())
}

/// Runs one child case under the file-size cap and asserts it really ran:
/// the child's stdout must report its single passing test.
fn run_capped_child(case_env: &str, test_name: &str) {
    let exe = std::env::current_exe().expect("test binary path");
    let script = format!(
        "trap '' 25; ulimit -f {FSIZE_BLOCKS}; exec \"{}\" --exact \"{test_name}\" --nocapture",
        exe.display()
    );
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env(case_env, "1")
        .output()
        .expect("spawn the capped child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "capped child failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_failed_first_append_runs_no_provider_effect() {
    if std::env::var("DAL_FSIZE_FIRST").is_ok() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime");
        return runtime.block_on(async {
            if let Err(error) = first_append_child_main().await {
                panic!("child failed: {error}");
            }
        });
    }
    run_capped_child(
        "DAL_FSIZE_FIRST",
        "a_failed_first_append_runs_no_provider_effect",
    );
}
