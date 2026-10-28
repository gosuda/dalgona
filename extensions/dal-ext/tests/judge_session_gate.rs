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
//! - `[rules].judge = "off"` with judged rules in the set surfaces the counted
//!   inert note once per session; with no judged rules it surfaces none.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::ext::{Name, RuleRecord};
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
///
/// Each request is logged with its one-based request index before a reply is
/// served, so a failure message can name every request the session made. An
/// exhausted reply queue fails the provider stream instead of panicking: a
/// panicking handler dies inside a host task, where the test only sees a
/// timeout, while a failed stream reaches the test as a `turn.failed`
/// notice carrying the queued run log.
struct CapturingReplies {
    replies: Arc<Mutex<VecDeque<String>>>,
    runs: RunLog,
}

impl ModelHandler for CapturingReplies {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let index = {
            let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
            let index = runs.len() + 1;
            runs.push(format!(
                "request={} purpose={:?} context_items={}",
                index,
                request.purpose,
                request.context.len()
            ));
            index
        };
        let text = self
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        let Some(text) = text else {
            return Box::pin(async move {
                Ok(EventStream::new(
                    futures::stream::iter(vec![Err(ProviderError::InvalidRequest {
                        message: format!(
                            "fixture reply queue exhausted at request {index}; the session \
                             made more model requests than the test queued replies"
                        ),
                    })]),
                    || {},
                ))
            });
        };
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

/// One entry per model request in a test, in the order the fixture served them.
type RunLog = Arc<Mutex<Vec<String>>>;

struct Scratch(PathBuf, RunLog);

/// A copy of the run log so far, for attaching to a failure.
fn run_log(scratch: &Scratch) -> Vec<String> {
    scratch
        .1
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

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
        Self(dir, RunLog::default())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A judged rule registered by an extension record, the only route a judged
/// rule takes: a `judge:` line in a rule file is an unknown key and parses to
/// a plain rule. The pattern matches neither fixture reply, so this rule proves
/// only the once-per-session notice. Matched judged-off behavior is not covered
/// here.
fn judged_record(name: &str) -> RuleRecord {
    RuleRecord {
        name: Name::parse(name).expect("valid judged rule name"),
        patterns: vec!["qjudge-never".into()],
        text: "Stop that.".into(),
        judge: Some("Is this relevant?".into()),
        scope: None,
        globs: None,
        agents: None,
        mode: None,
        repeat_mode: None,
        repeat_gap: None,
        always_apply: false,
        report: false,
        enabled: true,
    }
}

fn extensions(
    replies: Arc<Mutex<VecDeque<String>>>,
    runs: RunLog,
    judged: &[&str],
) -> Outcome<Vec<dal_agent::ext::Extension>> {
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
            handler: Arc::new(CapturingReplies { replies, runs }),
            export: None,
        })
        .build()?;
    let mut extensions = vec![ttsr, model];
    if !judged.is_empty() {
        let mut rules = ExtensionBuilder::new("judged-rules", "0.1.0", ServiceSet::default())?;
        for name in judged {
            rules = rules.rule(judged_record(name));
        }
        extensions.push(rules.build()?);
    }
    Ok(extensions)
}

async fn start_host(
    scratch: &Scratch,
    config_toml: &str,
    replies: Vec<String>,
) -> Outcome<(Host, Workspace, PathBuf)> {
    start_host_judged(scratch, config_toml, replies, &[]).await
}

/// Starts a host whose extensions also register one judged rule record per
/// name.
async fn start_host_judged(
    scratch: &Scratch,
    config_toml: &str,
    replies: Vec<String>,
    judged: &[&str],
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
        extensions: extensions(
            Arc::new(Mutex::new(replies.into())),
            Arc::clone(&scratch.1),
            judged,
        )?,
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
            UpdateKind::Notice(notice)
                if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
            {
                return Err(format!("turn failed: {}", notice.text).into());
            }
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

/// Writes one user rule file into the scratch data root's `rules` directory.
fn write_user_rule(data: &std::path::Path, name: &str, front: &str) -> Outcome<()> {
    let directory = data.join("rules");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join(format!("{name}.md")),
        format!("---\n{front}---\nBody.\n"),
    )?;
    Ok(())
}

/// `[rules].judge = "off"` with two judged extension records in the set
/// surfaces the counted inert note once per session.
///
/// The note is a session-open notice, so the first turn's batch must already
/// carry it; that assertion runs before the second prompt is submitted, so
/// the missing-notice failure can never be masked by later fixture trouble.
/// The second turn then proves the note does not repeat per turn. The judged
/// patterns match neither reply: this rule proves only the notice. Matched
/// judged-off behavior is not covered here.
///
/// Failure messages attach the journal text and the per-request run log so a
/// stray extra model request (a rules interrupt reminder would journal a
/// `rule_fired`/reminder record) is diagnosable from the failure alone.
#[tokio::test]
async fn explicit_off_with_judged_rules_surfaces_inert_count_once_per_session() -> Outcome<()> {
    let scratch = Scratch::new("off-inert");
    let data = scratch.0.join("data");
    write_user_rule(&data, "plain", "condition: z\n")?;
    let config = "model = \"fixture/end\"\n[rules]\njudge = \"off\"\n[judge]\ngate = \"off\"\n";
    let replies = vec!["x marks the spot".to_owned(), "second turn".to_owned()];
    let (host, workspace, data) =
        start_host_judged(&scratch, config, replies, &["judged-a", "judged-b"]).await?;
    let evidence = |error: Box<dyn std::error::Error + Send + Sync>| {
        format!(
            "{error}; model runs so far: {:?}; journal: {}",
            run_log(&scratch),
            journal_text(&data).unwrap_or_else(|error| format!("journal unreadable: {error}"))
        )
    };
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("judge-gate-off-inert"),
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
    let first = collect_all_notices(&mut subscription)
        .await
        .map_err(evidence)?;
    let inert: Vec<&(String, String)> = first
        .iter()
        .filter(|(_, text)| text.contains("inert"))
        .collect();
    assert_eq!(
        inert.len(),
        1,
        "explicit off surfaces the judged-rule notice at session open: {first:?}"
    );
    assert!(
        inert[0].1.contains('2'),
        "the single notice carries the judged-rule count: {first:?}"
    );
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "again".into(),
            }],
        })
        .await?;
    let second = collect_all_notices(&mut subscription)
        .await
        .map_err(evidence)?;
    assert!(
        !second.iter().any(|(_, text)| text.contains("inert")),
        "the inert note is once per session, not per turn: {second:?}"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

#[tokio::test]
async fn explicit_off_without_judged_rules_surfaces_no_inert_notice() -> Outcome<()> {
    let scratch = Scratch::new("off-no-judged");
    write_user_rule(&scratch.0.join("data"), "plain", "condition: z\n")?;
    let config = "model = \"fixture/end\"\n[rules]\njudge = \"off\"\n[judge]\ngate = \"off\"\n";
    let (host, workspace, _data) = start_host(&scratch, config, vec!["all set".to_owned()]).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("judge-gate-off-no-judged"),
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
    assert!(
        notices.iter().all(|(_, text)| !text.contains("inert")),
        "no judged rules means no inert notice: {notices:?}"
    );
    assert!(
        notices
            .iter()
            .all(|(kind, _)| !matches!(kind.as_str(), "rules" | "judge")),
        "no judged rules means no rules or judge notice: {notices:?}"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
