#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! Session-open judge gate (J-10) through the real TTSR setup slot.
//!
//! The `[judge].gate` contract at session open, before the first turn:
//! - `gate = "on"` with an unresolvable judge role fails session open with
//!   exactly `judge unavailable: <reason>` and generates nothing.
//! - `gate = "auto"` with an unresolvable judge role opens the session and
//!   emits exactly one `judge`-kind notice with the exact auto-off text.
//! - The reason is derived without catalog work of its own: an explicit
//!   `[judge].model` that fails readiness names `unknown judge model
//!   "<model>"`; a failing session route means `no credentials for the
//!   judge role`.
//! - `gate = "on"` with a ready explicit model still opens (control).

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Expect, ModelId, ModelRequest, Part,
    ServiceSet, Stop, ThinkingLevel, UpdateKind, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent};

type Outcome<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(20);
const UNKNOWN_MODEL: &str = "missing/model";

fn on_unknown_message() -> String {
    format!(r#"judge unavailable: unknown judge model "{UNKNOWN_MODEL}""#)
}

fn auto_unknown_notice() -> String {
    format!(
        r#"judge: auto resolved off (unknown judge model "{UNKNOWN_MODEL}"); judge-fed features are off"#
    )
}

const ON_WITHOUT_CREDENTIALS: &str = "judge unavailable: no credentials for the judge role";

/// Deterministic single-reply model; also serves session-model readiness.
struct CapturingReplies {
    replies: Arc<Mutex<VecDeque<String>>>,
}

impl ModelHandler for CapturingReplies {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let text = self
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .expect("the test queues one reply per turn");
        Box::pin(async move {
            let events: Vec<Result<StreamEvent, ProviderError>> = vec![
                Ok(StreamEvent::TextDelta { text }),
                Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
                Ok(StreamEvent::Usage {
                    usage: dal_core::Usage {
                        input_tokens: 10,
                        cached_input_tokens: 0,
                        output_tokens: 5,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        premium_requests: None,
                        cost_usd: None,
                    },
                }),
                Ok(StreamEvent::Stop {
                    reason: StopReason::EndTurn,
                }),
            ];
            Ok(EventStream::new(futures::stream::iter(events), || {}))
        })
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "dal-judge-gate-{tag}-{}-{}",
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

fn extensions(replies: Arc<Mutex<VecDeque<String>>>) -> Outcome<Vec<dal_agent::ext::Extension>> {
    let pick_edit_style =
        Arc::new(|_route: &dal_core::ModelRoute| dal_ext::ttsr::readers::EditStyle::Replace);
    let ttsr = dal_ext::ttsr::extension(dal_ext::ttsr::TtsrDeps {
        pick_edit_style,
        lane: false,
    })?;
    let model = ExtensionBuilder::new("fixture-model", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse("fixture/end")?,
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(CapturingReplies { replies }),
            export: None,
        })
        .build()?;
    Ok(vec![ttsr, model])
}

async fn start_host(
    scratch: &Scratch,
    config_toml: &str,
    replies: Vec<String>,
) -> Outcome<(Host, Workspace, PathBuf)> {
    let data = scratch.0.join("data");
    let workspace_dir = scratch.0.join("workspace");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(config_toml))?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: extensions(Arc::new(Mutex::new(replies.into())))?,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
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
    let host = Host::start(product, config, env).await?;
    Ok((host, Workspace::new(workspace_dir)?, data))
}

/// Raw journal text across the scratch data root.
fn journal_text(data: &std::path::Path) -> Outcome<String> {
    let mut text = String::new();
    let mut pending = vec![data.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for found in std::fs::read_dir(&directory)? {
            let path = found?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
                text.push_str(&std::fs::read_to_string(&path)?);
            }
        }
    }
    Ok(text)
}

async fn collect_judge_notices(subscription: &mut Subscription) -> Outcome<Vec<String>> {
    let mut notices = Vec::new();
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .map_err(|_| "turn timed out")?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err("subscription closed mid-turn".into());
        };
        match &update.kind {
            UpdateKind::Notice(notice) if notice.kind.as_ref() == "judge" => {
                notices.push(notice.text.to_string());
            }
            UpdateKind::Notice(notice)
                if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
            {
                return Err(format!("turn failed: {}", notice.text).into());
            }
            UpdateKind::TurnEnded { stop, .. } => {
                assert_eq!(*stop, Stop::EndTurn, "the turn ends cleanly");
                return Ok(notices);
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn on_gate_with_unknown_explicit_model_fails_open_with_reason() -> Outcome<()> {
    let scratch = Scratch::new("on-unknown");
    let config =
        format!("model = \"fixture/end\"\n[judge]\ngate = \"on\"\nmodel = \"{UNKNOWN_MODEL}\"\n");
    let (host, workspace, data) = start_host(&scratch, &config, Vec::new()).await?;
    let opened = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("judge-gate-on"),
        )
        .await;
    let Err(error) = opened else {
        let _ = host.shutdown(Duration::from_secs(1)).await;
        return Err("gate=on with an unknown judge model opened".into());
    };
    let message = error.to_string();
    assert!(
        message.contains(&on_unknown_message()),
        "session open must fail with the exact reasoned string, got: {message}"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    let journal = journal_text(&data)?;
    assert!(
        !journal.contains("\"type\":\"assistant\""),
        "a failed session open must not generate: {journal}"
    );
    assert!(
        !journal.contains("\"type\":\"rule_fired\""),
        "a failed session open must not journal a rule fire: {journal}"
    );
    Ok(())
}

#[tokio::test]
async fn auto_gate_with_unknown_explicit_model_opens_with_exactly_one_notice() -> Outcome<()> {
    let scratch = Scratch::new("auto-unknown");
    let config =
        format!("model = \"fixture/end\"\n[judge]\ngate = \"auto\"\nmodel = \"{UNKNOWN_MODEL}\"\n");
    let (host, workspace, _data) =
        start_host(&scratch, &config, vec!["all set".to_owned()]).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("judge-gate-auto"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "hello".into(),
            }],
        })
        .await?;
    let notices = collect_judge_notices(&mut subscription).await?;
    assert_eq!(
        notices,
        [auto_unknown_notice()],
        "auto resolution failure must emit exactly one judge notice"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

#[tokio::test]
async fn on_gate_without_any_route_fails_open_with_credentials_reason() -> Outcome<()> {
    let scratch = Scratch::new("on-noroute");
    let config = "[judge]\ngate = \"on\"\n".to_owned();
    let (host, workspace, data) = start_host(&scratch, &config, Vec::new()).await?;
    let opened = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("judge-gate-credentials"),
        )
        .await;
    let Err(error) = opened else {
        let _ = host.shutdown(Duration::from_secs(1)).await;
        return Err("gate=on without any judge route opened".into());
    };
    let message = error.to_string();
    assert!(
        message.contains(ON_WITHOUT_CREDENTIALS),
        "session open must fail with the credentials reason, got: {message}"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    let journal = journal_text(&data)?;
    assert!(
        !journal.contains("\"type\":\"assistant\""),
        "a failed session open must not generate: {journal}"
    );
    Ok(())
}

#[tokio::test]
async fn on_gate_with_ready_explicit_model_opens() -> Outcome<()> {
    let scratch = Scratch::new("on-ready");
    let config =
        "model = \"fixture/end\"\n[judge]\ngate = \"on\"\nmodel = \"fixture/end\"\n".to_owned();
    let (host, workspace, _data) = start_host(&scratch, &config, vec!["ready".to_owned()]).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("judge-gate-ready"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "hello".into(),
            }],
        })
        .await?;
    let notices = collect_judge_notices(&mut subscription).await?;
    assert!(
        notices.is_empty(),
        "a ready judge emits no auto-off notice: {notices:?}"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

async fn collect_all_notices(subscription: &mut Subscription) -> Outcome<Vec<(String, String)>> {
    let mut notices = Vec::new();
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .map_err(|_| "turn timed out")?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err("subscription closed mid-turn".into());
        };
        match &update.kind {
            UpdateKind::Notice(notice) => {
                notices.push((notice.kind.to_string(), notice.text.to_string()));
            }
            UpdateKind::TurnEnded { stop, .. } => {
                assert_eq!(*stop, Stop::EndTurn, "the turn ends cleanly");
                return Ok(notices);
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn auto_off_without_judged_rules_emits_no_rules_notice() -> Outcome<()> {
    let scratch = Scratch::new("auto-no-judged");
    let config =
        format!("model = \"fixture/end\"\n[judge]\ngate = \"auto\"\nmodel = \"{UNKNOWN_MODEL}\"\n");
    let (host, workspace, _data) =
        start_host(&scratch, &config, vec!["all set".to_owned()]).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("judge-gate-auto-no-judged"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "hello".into(),
            }],
        })
        .await?;
    let notices = collect_all_notices(&mut subscription).await?;
    let judge: Vec<_> = notices
        .iter()
        .filter(|(kind, _)| kind == "judge")
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        judge,
        [auto_unknown_notice()].iter().collect::<Vec<_>>(),
        "auto resolution failure must emit exactly one judge notice naming the cause"
    );
    assert!(
        notices
            .iter()
            .filter(|(kind, _)| *kind == "rules")
            .collect::<Vec<_>>()
            .is_empty(),
        "no rules auto-off notice without judged rules: {notices:?}"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
