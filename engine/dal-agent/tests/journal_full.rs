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
//! gets the journal error, the refused answer is neither published nor
//! journaled, and the session reports the failure instead of acknowledging
//! the answer. The request is open again, so it still owes the one terminal
//! record every request gets: the turn's core withdrawal, published only
//! after it is durable. A refused session approval grants nothing either:
//! its allowance leaves with the failed batch, so a retry that declines
//! still lands and the next call to the same tool meets a question, not a
//! live allowance the journal never recorded.
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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, Extension, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef, Subscription, ToolError};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part, Question,
    RawJson, Record, RequestId, ResolutionCause, ToolClass, ToolSpec, UpdateKind, Visibility,
    Workspace,
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
    child_setup_with(extensions, &format!("{STEP_CALL}{STEP_END}")).await
}

/// [`child_setup`] with the provider script the case needs.
async fn child_setup_with(
    extensions: Vec<Extension>,
    script: &str,
) -> Result<(tempfile::TempDir, Host, Workspace), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, script)?;
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

/// One request's terminal outcome: its answer, answerer, and cause.
type Terminal = (Answer, String, Option<ResolutionCause>);

/// The only terminal outcome the refused request may ever have: the turn's
/// core withdrawal, never the answer the journal could not take.
fn core_withdrawal() -> Terminal {
    (
        Answer::Cancel,
        "core".to_owned(),
        Some(ResolutionCause::Withdrawn),
    )
}

/// Every resolution published for `request`, up to the end of its turn.
async fn published_until_turn_end(
    subscription: &mut Subscription,
    request: RequestId,
) -> Result<Vec<Terminal>, Box<dyn std::error::Error>> {
    let mut published = Vec::new();
    loop {
        let delivery = subscription
            .next()
            .await
            .ok_or("the session closed before the turn ended")?;
        let Delivery::Update(update) = &delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::RequestResolved {
                id,
                answer,
                by,
                cause,
            } if *id == request => {
                published.push((answer.clone(), by.as_str().to_owned(), *cause));
            }
            UpdateKind::TurnEnded { .. } => return Ok(published),
            _ => {}
        }
    }
}

/// Every journaled terminal record for `request`, with its default flag.
fn journaled_terminals(records: &[Record], request: RequestId) -> Vec<(Terminal, bool)> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Resolved {
                request: id,
                answer,
                by,
                was_default,
                cause,
                ..
            } if *id == request => Some((
                (answer.clone(), by.as_str().to_owned(), *cause),
                *was_default,
            )),
            _ => None,
        })
        .collect()
}

async fn child_main() -> TestResult {
    let (tmp, host, workspace) = child_setup(vec![ask_big_extension()?]).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
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
    // The failed frame reopened the request, so the turn that asked it ends
    // with the one terminal record every request owes. Everything published
    // about the request until then must be that durable core withdrawal,
    // never the refused answer.
    let published =
        tokio::time::timeout(WAIT, published_until_turn_end(&mut subscription, request))
            .await
            .map_err(|_| "the turn never ended after the failed resolution".to_string())??;
    assert_eq!(
        published,
        vec![core_withdrawal()],
        "an unjournaled resolution was published, or the request got no single terminal outcome"
    );
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
    let session = agent.view(dal_core::PageReq::default())?.session.id;
    host.close(session).await?;
    drop(agent);
    drop(host);
    let store = dal_store::Store::new(tmp.path().join("data"), workspace, dal_core::Product::Dal);
    let (mut journal, _) = store.open_session(session).await?;
    assert_eq!(
        journaled_terminals(journal.records(), request),
        vec![(core_withdrawal(), false)],
        "the journal must hold the withdrawal as the request's one terminal record, not the refused answer"
    );
    journal.close().await?;
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

struct OversizedReminder {
    committed: Arc<AtomicUsize>,
    pending: bool,
}

impl dal_agent::ext::WatchFactory for OversizedReminder {
    fn start(
        &self,
        _turn: &dal_agent::ext::TurnInfo<'_>,
    ) -> Option<Box<dyn dal_agent::ext::StreamWatch>> {
        Some(Box::new(Self {
            committed: Arc::clone(&self.committed),
            pending: false,
        }))
    }
}

impl dal_agent::ext::StreamWatch for OversizedReminder {
    fn feed(&mut self, _channel: dal_core::Channel, _delta: &str) -> dal_core::StreamVerdict {
        dal_core::StreamVerdict::Continue
    }

    fn finish(&mut self) -> dal_core::StreamVerdict {
        self.pending = true;
        dal_core::StreamVerdict::Continue
    }

    fn take_fires(&mut self) -> Vec<dal_core::ext::StreamFire> {
        if !std::mem::take(&mut self.pending) {
            return Vec::new();
        }
        vec![dal_core::ext::StreamFire {
            index: 0,
            rule: "oversized".into(),
            action: dal_core::ext::StreamFireAction::Reminder,
            text: Some("r".repeat(65536).into()),
            judged: false,
        }]
    }

    fn record_body(
        &self,
        _fire: usize,
        _at: dal_core::Timestamp,
        entry: Option<dal_core::EntryId>,
    ) -> Option<dal_agent::ext::StreamWatchRecord> {
        let entry = entry.expect("a reminder fire receives its allocated entry");
        Some(dal_agent::ext::StreamWatchRecord {
            kind: "fire".into(),
            body: RawJson::parse(&format!(r#"{{"entry":{}}}"#, entry.get())).expect("entry record"),
        })
    }

    fn gate_record(&mut self, _fire: usize, _entry: Option<dal_core::EntryId>) {
        self.committed.fetch_add(1, Ordering::SeqCst);
    }
}

async fn await_watch_append_failure(subscription: &mut Subscription) -> TestResult {
    loop {
        let delivery = subscription
            .next()
            .await
            .ok_or("the session closed without reporting the failed append")?;
        let Delivery::Update(update) = delivery else {
            continue;
        };
        if matches!(
            update.kind,
            UpdateKind::RuleFired { .. }
                | UpdateKind::ToolSettled { .. }
                | UpdateKind::TurnEnded { .. }
        ) {
            return Err(format!("published an undurable rule batch: {update:?}").into());
        }
        if let UpdateKind::Notice(notice) = &update.kind
            && notice.kind.as_ref() == "driver.failed"
        {
            assert!(
                notice.text.contains("journal"),
                "the rule must reach the journal boundary: {notice:?}"
            );
            return Ok(());
        }
    }
}

async fn watch_append_child_main() -> TestResult {
    let committed = Arc::new(AtomicUsize::new(0));
    let extension = ExtensionBuilder::new("oversized", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .output_stream(Arc::new(OversizedReminder {
            committed: Arc::clone(&committed),
            pending: false,
        }))
        .build()?;
    let (tmp, host, workspace) = child_setup(vec![extension]).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Exercise a refused rule delivery.".into(),
            }],
        })
        .await?;
    tokio::time::timeout(WAIT, await_watch_append_failure(&mut subscription)).await??;
    let view = agent.view(dal_core::PageReq::default())?;
    assert!(
        view.entries.items.iter().all(|entry| !matches!(
            entry.kind,
            dal_core::EntryKind::Assistant { .. } | dal_core::EntryKind::Reminder { .. }
        )),
        "a rejected batch must not reach the client projection"
    );
    let session = view.session.id;
    host.close(session).await?;
    assert_eq!(
        committed.load(Ordering::SeqCst),
        0,
        "a rejected append must not consume the rule's repeat gate"
    );
    drop(agent);
    drop(host);
    let store = dal_store::Store::new(tmp.path().join("data"), workspace, dal_core::Product::Dal);
    let (mut journal, _) = store.open_session(session).await?;
    assert!(
        journal.records().iter().all(|record| !matches!(
            record,
            dal_core::Record::Assistant(_)
                | dal_core::Record::Reminder(_)
                | dal_core::Record::Ext { .. }
        )),
        "the refused response, reminder, and extension record must stay out of replay"
    );
    let (replayed, _) =
        dal_core::Session::replay(journal.records().to_vec(), dal_core::Timestamp::now())?;
    assert!(
        replayed.rule_seeds().is_empty(),
        "reopening must not restore a fire from the refused batch"
    );
    journal.close().await?;
    Ok(())
}

#[test]
fn a_failed_watch_append_neither_publishes_nor_latches_the_fire() {
    if std::env::var("DAL_FSIZE_WATCH").is_ok() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime");
        return runtime.block_on(async {
            if let Err(error) = watch_append_child_main().await {
                panic!("child failed: {error}");
            }
        });
    }
    run_capped_child(
        "DAL_FSIZE_WATCH",
        "a_failed_watch_append_neither_publishes_nor_latches_the_fire",
    );
}

/// Asks for approval of its own name and reports that it ran once approved.
struct ApprovalProbe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for ApprovalProbe {
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
            let preview = dal_core::Preview {
                title: "approval-probe".into(),
                body: "printf approved".into(),
                digest: None,
            };
            match cx.authorize(preview).await {
                Ok(_) => ToolOutcome::Ok(Box::new(ToolOutput::from_text("the probe ran"))),
                Err(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
            }
        })
    }
}

/// The approval-probe extension export the allowance case calls twice.
fn approval_probe_extension() -> Result<Extension, Box<dyn std::error::Error>> {
    Ok(
        ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)?
            .script_tool(
                Arc::new(ApprovalProbe {
                    name: Name::parse("fixture__probe")?,
                    spec: Arc::new(ToolSpec {
                        name: Name::parse("fixture__probe")?,
                        description: "approval probe tool".into(),
                        parameters: RawJson::parse(r#"{"type":"object"}"#)?,
                        grammar: None,
                    }),
                }),
                Visibility::Model,
                ExportSpec {
                    id: ExportId {
                        plugin: Name::parse("fixture")?,
                        kind: ExportKind::Tool,
                        local: Name::parse("probe")?,
                    },
                    uses: OpSet::EMPTY,
                    input: RawJson::parse(r#"{"type":"object"}"#)?,
                    description: "approval probe export".into(),
                },
            )
            .build()?,
    )
}

/// One provider step that calls the probe under call id `id`.
fn probe_call(id: &str) -> String {
    STEP_CALL
        .replace("ask-call", id)
        .replace("fixture__ask_big", "fixture__probe")
}

/// The id of the next approval the probe opens. A turn that ends instead
/// never asked: the tool ran on an allowance.
async fn next_probe_approval(
    subscription: &mut Subscription,
) -> Result<RequestId, Box<dyn std::error::Error>> {
    tokio::time::timeout(WAIT, async {
        loop {
            let delivery = subscription
                .next()
                .await
                .ok_or("the session closed before the approval opened")?;
            let Delivery::Update(update) = &delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request)
                    if matches!(
                        &request.question,
                        Question::Approval { tool, .. } if &**tool == "fixture__probe"
                    ) =>
                {
                    return Ok::<_, Box<dyn std::error::Error>>(request.id);
                }
                UpdateKind::TurnEnded { .. } => {
                    return Err(
                        "the turn ended without an approval: the refused session approval is still live"
                            .into(),
                    );
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| "no approval opened".to_string())?
}

/// Waits for the end of the running turn.
async fn until_turn_end(subscription: &mut Subscription) -> TestResult {
    tokio::time::timeout(WAIT, async {
        loop {
            let delivery = subscription
                .next()
                .await
                .ok_or("the session closed before the turn ended")?;
            if let Delivery::Update(update) = &delivery
                && matches!(update.kind, UpdateKind::TurnEnded { .. })
            {
                return Ok::<(), Box<dyn std::error::Error>>(());
            }
        }
    })
    .await
    .map_err(|_| "the turn never ended".to_string())?
}

/// The child for the allowance case: a session approval whose record the
/// journal refuses must not leave the tool allowed. The refusing client's
/// identity is 64 KiB, so only the records it authors cannot fit under the
/// cap; a plain client's records still land.
async fn allowance_child_main() -> TestResult {
    let script = format!(
        "{}{}{STEP_END}",
        probe_call("probe-one"),
        probe_call("probe-two")
    );
    let (tmp, host, workspace) =
        child_setup_with(vec![approval_probe_extension()?], &script).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let session = agent.session();
    let heavy = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: workspace.clone(),
            },
            ClientId::new("h".repeat(65536)),
        )
        .await?;
    let mut subscription: Subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "probe twice".into(),
            }],
        })
        .await?;
    let first = next_probe_approval(&mut subscription).await?;
    let error = heavy
        .answer(first, Answer::ApproveForSession)
        .await
        .err()
        .ok_or("the unjournaled session approval was acknowledged")?;
    assert!(
        error.to_string().contains("journal"),
        "unexpected failure: {error}"
    );
    // The failed frame reopened the question, so a plain decline still lands.
    agent.answer(first, Answer::Decline).await?;
    // The refused approval must not outlive its failure: the second call to
    // the same tool asks again instead of running on a live allowance.
    let second = next_probe_approval(&mut subscription).await?;
    assert_ne!(second, first, "the second call asked a new question");
    agent.answer(second, Answer::Decline).await?;
    until_turn_end(&mut subscription).await?;
    host.close(session).await?;
    drop(heavy);
    drop(agent);
    drop(host);
    let store = dal_store::Store::new(tmp.path().join("data"), workspace, dal_core::Product::Dal);
    let (mut journal, _) = store.open_session(session).await?;
    let records = journal.records().to_vec();
    journal.close().await?;
    assert!(
        records
            .iter()
            .all(|record| !matches!(record, Record::AllowAlways { .. })),
        "the refused session approval must leave no allowance record"
    );
    assert_eq!(
        journaled_terminals(&records, first),
        vec![(
            (
                Answer::Decline,
                "probe".to_owned(),
                Some(ResolutionCause::Answered)
            ),
            false
        )],
        "the retried decline is the request's one terminal record"
    );
    let (replayed, _) = dal_core::Session::replay(records, dal_core::Timestamp::now())?;
    assert!(
        replayed.allow_always().is_empty(),
        "reopening must not restore the refused session approval"
    );
    Ok(())
}

#[test]
fn a_failed_session_approval_leaves_no_live_allowance() {
    if std::env::var("DAL_FSIZE_ALLOW").is_ok() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime");
        return runtime.block_on(async {
            if let Err(error) = allowance_child_main().await {
                panic!("child failed: {error}");
            }
        });
    }
    run_capped_child(
        "DAL_FSIZE_ALLOW",
        "a_failed_session_approval_leaves_no_live_allowance",
    );
}
