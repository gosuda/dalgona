#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! The repeat guard and the warning router through a real host.
//!
//! The units under test are the real `dal-tools` guard extension and the real
//! `dal-agent` host: its hook dispatch, its turn service, its `fs.read`
//! confinement and its update stream. Nothing is stubbed on either side. The
//! model is a scripted in-process handler and the tools are synthetic
//! fixtures, so no network, credential or child process is involved. The guard
//! is built from the same `[guard]` section type the product decodes.
//!
//! What a real host lets a test observe:
//!
//! - A stop must end the turn through the host's own cancellation. The tests
//!   read the terminal outcome (`Stop::Cancelled`), the number of model
//!   requests and the number of tool runs. They never read a clock, so a
//!   cancellation that waits out the host's hook budget, or that never
//!   arrives, fails on its outcome instead of on a timing threshold.
//! - The guard's `fs.read` request goes through the host's session `fs.read`
//!   service, which only resolves workspace-relative paths. A footer naming an
//!   absolute path is refused by the real service, which is the refusal these
//!   tests use for the withheld-read behavior.
//!
//! Not covered here: a refused `turn` service. The guard is a built-in
//! extension, so the host never gates its services and a real host cannot
//! refuse it the cancel; see the proof report for the exact seam.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dal_agent::ext::{
    ArgError, BoxFuture, EventStream, Extension, ExtensionBuilder, ModelCx, ModelError,
    ModelHandler, ModelRecord, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription, ToolError};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, EntryKind, Expect, GuardSection, ModelId,
    ModelInfo, ModelRequest, Name, PageReq, Part, RawJson, ServiceSet, SessionId, Stop,
    ThinkingLevel, ToolClass, ToolSpec, TurnId, UpdateKind, Visibility, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent, ToolArgs};
use dal_tools::{Calibration, FindingsHandle, GuardConfig, GuardFindings, guard_extension};

type Outcome<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Bounds each wait on the host so a stuck turn fails with a message instead
/// of hanging the suite. No assertion compares an elapsed time with it.
const WAIT: Duration = Duration::from_secs(20);

const FILE_A: &str = r#"{"path":"a.txt"}"#;
const FILE_B: &str = r#"{"path":"b.txt"}"#;

/// A build whose only warning sits in the full-output log, not in the preview.
const LOG_ONLY_WARNING: &str = r#"{"command":"cargo build","preview":"   Compiling widget v0.1.0\nFinished dev profile","log":"   Compiling widget v0.1.0\nsrc/in_log.rs:7: unused variable: only_in_the_log\nFinished dev profile\n"}"#;

/// A build with one warning in the preview and a different one in the log.
const FIRST_BUILD: &str = r#"{"command":"cargo build","preview":"src/first_preview.rs:3: unused variable: only_in_the_first_preview","log":"src/first_log.rs:7: unused variable: only_in_the_first_log\n"}"#;

/// A later build, with the same split between preview and log.
const SECOND_BUILD: &str = r#"{"command":"cargo test","preview":"src/second_preview.rs:4: unused variable: only_in_the_second_preview","log":"src/second_log.rs:9: unused variable: only_in_the_second_log\n"}"#;

/// One scripted model reply.
#[derive(Clone, Copy)]
enum Reply {
    /// A response that calls one tool and stops for its result.
    Call {
        id: &'static str,
        tool: &'static str,
        args: &'static str,
    },
    /// A final text response that ends the turn.
    Text(&'static str),
}

/// The scripted model: pops one reply per request and counts requests, so a
/// test can tell whether the loop asked the model again after a stop.
struct Script {
    replies: Mutex<VecDeque<Reply>>,
    requests: AtomicUsize,
}

impl Script {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            requests: AtomicUsize::new(0),
        })
    }

    /// Drops whatever the last turn left unused and queues `replies`.
    fn replace(&self, replies: Vec<Reply>) {
        *self.replies.lock().unwrap_or_else(PoisonError::into_inner) = replies.into();
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl ModelHandler for Script {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let reply = self
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or(Reply::Text("the script ran out of replies"));
        Box::pin(std::future::ready(Ok(stream(reply_events(reply)))))
    }
}

fn usage() -> dal_core::Usage {
    dal_core::Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

fn stream(events: Vec<StreamEvent>) -> EventStream {
    let items: Vec<Result<StreamEvent, ProviderError>> = events.into_iter().map(Ok).collect();
    EventStream::new(futures::stream::iter(items), || {})
}

fn reply_events(reply: Reply) -> Vec<StreamEvent> {
    match reply {
        Reply::Text(text) => vec![
            StreamEvent::TextDelta { text: text.into() },
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage { usage: usage() },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ],
        Reply::Call { id, tool, args } => {
            let args = RawJson::parse(args).expect("scripted tool arguments are valid JSON");
            vec![
                StreamEvent::ToolCallStarted {
                    id: id.into(),
                    name: tool.into(),
                },
                StreamEvent::ToolCallsDone {
                    calls: vec![dal_provider::ToolCall {
                        id: id.into(),
                        name: tool.into(),
                        args: ToolArgs::Parsed(args),
                    }],
                },
                StreamEvent::Usage { usage: usage() },
                StreamEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        }
    }
}

/// Where the fixture `exec` tool says its full output lives.
enum Footer {
    /// `jobs/<call>.log` inside the workspace: a path the host's session
    /// `fs.read` resolves.
    Workspace,
    /// An absolute `<base>/jobs/<call>.log` outside the workspace, the shape
    /// the real exec tool prints for its job log: the host refuses it.
    Outside(PathBuf),
}

enum Shape {
    /// Returns `ran`.
    Plain,
    /// Writes the call's `log` argument to its job log and returns the
    /// `preview` argument followed by the `Full output:` footer line.
    Spill(Footer),
}

#[derive(serde::Deserialize)]
struct SpillArgs {
    preview: String,
    log: String,
}

/// Writes the job log of one fixture `exec` call and builds its result text.
fn spill_output(footer: &Footer, root: &Path, id: &str, args: &str) -> Outcome<String> {
    let parsed: SpillArgs =
        sonic_rs::from_str(args).map_err(|error| format!("spill arguments: {error}"))?;
    let (directory, shown) = match footer {
        Footer::Workspace => (root.join("jobs"), format!("jobs/{id}.log")),
        Footer::Outside(base) => {
            let directory = base.join("jobs");
            let shown = directory.join(format!("{id}.log"));
            (directory, shown.to_string_lossy().into_owned())
        }
    };
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join(format!("{id}.log")), parsed.log)?;
    Ok(format!("{}\nFull output: {shown}", parsed.preview))
}

/// A model-visible fixture tool. It is named for the class the guard keys on:
/// the warning router only reads results of the tool named `exec`.
struct Fixture {
    name: Name,
    spec: Arc<ToolSpec>,
    class: ToolClass,
    shape: Arc<Shape>,
    runs: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new(name: &str, class: ToolClass, shape: Shape) -> (Arc<Self>, Arc<Mutex<Vec<String>>>) {
        let name = Name::parse(name).expect("fixture tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "A fixture tool.".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("fixture schema"),
            grammar: None,
        });
        let runs = Arc::new(Mutex::new(Vec::new()));
        let tool = Arc::new(Self {
            name,
            spec,
            class,
            shape: Arc::new(shape),
            runs: Arc::clone(&runs),
        });
        (tool, runs)
    }
}

impl Tool for Fixture {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(self.class.clone())
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(call.args.as_str().to_owned());
        let shape = Arc::clone(&self.shape);
        let root = cx.workspace().as_path().to_path_buf();
        Box::pin(async move {
            let id = call.id.as_str().to_owned();
            let args = call.args.as_str().to_owned();
            let written = tokio::task::spawn_blocking(move || match &*shape {
                Shape::Plain => Ok("ran".to_owned()),
                Shape::Spill(footer) => spill_output(footer, &root, &id, &args),
            })
            .await;
            match written {
                Ok(Ok(text)) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(text))),
                Ok(Err(error)) => ToolOutcome::Err(ToolError::message(error.to_string())),
                Err(join) => {
                    ToolOutcome::Err(ToolError::message(format!("fixture task failed: {join}")))
                }
            }
        })
    }
}

fn tools_extension(tools: Vec<Arc<Fixture>>) -> Outcome<Extension> {
    let mut builder = ExtensionBuilder::new("fixture-tools", "0.1.0", ServiceSet::EMPTY)?;
    for tool in tools {
        builder = builder.tool(tool, Visibility::Model);
    }
    Ok(builder.build()?)
}

fn model_extension(script: &Arc<Script>) -> Outcome<Extension> {
    Ok(
        ExtensionBuilder::new("fixture-model", "0.1.0", ServiceSet::default())?
            .model(ModelRecord {
                id: ModelId::parse("fixture/end")?,
                caps: Caps {
                    context_window: Some(100_000),
                    thinking: Box::new([ThinkingLevel::Off]),
                    tool_use: true,
                    image_input: false,
                    custom_grammar: false,
                },
                handler: script.clone(),
                export: None,
            })
            .build()?,
    )
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "dal-guard-lifecycle-{tag}-{}-{}",
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

/// Which `Full output:` footer the fixture `exec` tool prints.
#[derive(Clone, Copy)]
enum Log {
    Workspace,
    Outside,
}

/// Whether the guard under test is on.
#[derive(Clone, Copy)]
enum Guard {
    On,
    Off,
}

/// One real host with one open session, the real guard and the fixtures.
struct Rig {
    host: Host,
    agent: Agent,
    subscription: Subscription,
    session: SessionId,
    findings: FindingsHandle,
    script: Arc<Script>,
    mutations: Arc<Mutex<Vec<String>>>,
    looks: Arc<Mutex<Vec<String>>>,
    _scratch: Scratch,
}

async fn rig(tag: &str, guard: Guard, log: Log, replies: Vec<Reply>) -> Outcome<Rig> {
    let scratch = Scratch::new(tag);
    let data = scratch.0.join("data");
    let workspace_dir = scratch.0.join("workspace");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let footer = match log {
        Log::Workspace => Footer::Workspace,
        Log::Outside => Footer::Outside(scratch.0.join("outside")),
    };
    let (mutate, mutations) = Fixture::new("mutate", ToolClass::Patch, Shape::Plain);
    let (look, looks) = Fixture::new("look", ToolClass::Read, Shape::Plain);
    let exec_class = ToolClass::Exec {
        read_only: false,
        grant: None,
    };
    let (exec, _) = Fixture::new("exec", exec_class, Shape::Spill(footer));
    let script = Script::new(replies);

    // The unit under test: the real guard, enabled through the core section.
    let section = GuardSection {
        enabled: matches!(guard, Guard::On),
        ..GuardSection::default()
    };
    let parts = guard_extension(GuardConfig::from_section(&section, &Calibration::none())?)?;
    let findings = parts.findings;
    let extensions = vec![
        parts.extension,
        tools_extension(vec![mutate, look, exec])?,
        model_extension(&script)?,
    ];

    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("approval = \"all\"\nmodel = \"fixture/end\"\n"),
    )?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
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
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("guard-lifecycle"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    let session = agent.view(PageReq::default())?.session.id;
    Ok(Rig {
        host,
        agent,
        subscription,
        session,
        findings,
        script,
        mutations,
        looks,
        _scratch: scratch,
    })
}

async fn finish(rig: Rig) -> Outcome<()> {
    rig.host.close(rig.session).await?;
    Ok(())
}

/// One settled tool result as the update stream published it.
#[derive(Debug)]
struct SettledCall {
    is_error: bool,
    text: Box<str>,
}

/// What one prompt published, up to and including its terminal update.
#[derive(Debug)]
struct Turn {
    id: TurnId,
    stop: Stop,
    settled: Vec<SettledCall>,
    /// `(kind, text)` of every notice published during the turn.
    notices: Vec<(Box<str>, Box<str>)>,
}

impl Turn {
    fn notices_of(&self, kind: &str) -> usize {
        self.notices
            .iter()
            .filter(|(found, _)| found.as_ref() == kind)
            .count()
    }
}

/// Submits one prompt and reads the update stream until the turn ends. The
/// guard's observer hooks run inline in the turn driver, so every notice they
/// publish precedes the turn's end on an uncancelled turn.
async fn prompt(rig: &mut Rig, text: &str) -> Outcome<Turn> {
    rig.agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    let mut settled = Vec::new();
    let mut notices = Vec::new();
    while let Some(delivery) = tokio::time::timeout(WAIT, rig.subscription.next())
        .await
        .map_err(|_| "the turn neither ended nor failed within the wait: it is stuck")?
    {
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::ToolSettled { outcome, .. } => settled.push(SettledCall {
                is_error: outcome.is_error,
                text: outcome.text.clone(),
            }),
            UpdateKind::Notice(notice) => {
                notices.push((notice.kind.clone(), notice.text.clone()));
            }
            UpdateKind::TurnEnded { turn, stop, .. } => {
                return Ok(Turn {
                    id: *turn,
                    stop: *stop,
                    settled,
                    notices,
                });
            }
            _ => {}
        }
    }
    Err("the update stream closed before the turn ended".into())
}

fn logged(runs: &Mutex<Vec<String>>) -> Vec<String> {
    runs.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Counts the journaled tool results that report success.
fn successful_results(rig: &Rig) -> Outcome<usize> {
    let view = rig.agent.view(PageReq::default())?;
    Ok(view
        .entries
        .items
        .iter()
        .filter(|entry| matches!(&entry.kind, EntryKind::ToolResult { error: false, .. }))
        .count())
}

/// Waits for the guard's turn-end observer to publish the findings of `turn`.
/// The observer runs after the client sees `TurnEnded`, so the findings are
/// polled for; a turn without findings fails after the wait.
async fn findings_of(rig: &Rig, turn: TurnId) -> Outcome<Arc<GuardFindings>> {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = rig.findings.last(&rig.session)
            && found.turn == turn
        {
            return Ok(found);
        }
        if Instant::now() > deadline {
            return Err("the guard recorded no findings for the ended turn".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn warning_texts(findings: &GuardFindings) -> Vec<&str> {
    findings
        .warnings
        .iter()
        .map(|(_, _, text)| &**text)
        .collect()
}

/// Three identical mutating calls, then a text reply that is only reached when
/// the turn was not stopped.
fn identical_calls() -> Vec<Reply> {
    vec![
        Reply::Call {
            id: "m1",
            tool: "mutate",
            args: FILE_A,
        },
        Reply::Call {
            id: "m2",
            tool: "mutate",
            args: FILE_A,
        },
        Reply::Call {
            id: "m3",
            tool: "mutate",
            args: FILE_A,
        },
        Reply::Text("the turn was not stopped"),
    ]
}

#[tokio::test]
async fn the_third_identical_mutation_never_runs_and_the_turn_ends_cancelled() -> Outcome<()> {
    let mut rig = rig("stop", Guard::On, Log::Workspace, identical_calls()).await?;
    let turn = prompt(&mut rig, "write a.txt").await?;

    // The cancel request is served by the host while the guard hook awaits
    // it. Riding out the hook budget instead leaves the budget text in the
    // call's result, with the turn ended normally.
    assert!(
        !turn
            .settled
            .iter()
            .any(|call| call.text.contains("over budget")),
        "the cancel request waited out the hook budget instead of completing: {:?}",
        turn.settled
    );
    assert_eq!(
        turn.stop,
        Stop::Cancelled,
        "the stop must cancel the turn through the host (notices {:?}, results {:?})",
        turn.notices,
        turn.settled
    );
    assert_eq!(
        logged(&rig.mutations).len(),
        2,
        "two identical calls run; a third run means the stop never blocked the call"
    );
    assert_eq!(
        rig.script.requests(),
        3,
        "no model request follows the stop; a fourth means the loop kept going"
    );
    assert_eq!(
        successful_results(&rig)?,
        2,
        "only the two calls before the stop produce a successful result"
    );
    finish(rig).await
}

#[tokio::test]
async fn a_stopped_turn_does_not_poison_the_next_turn() -> Outcome<()> {
    let mut rig = rig("fresh", Guard::On, Log::Workspace, identical_calls()).await?;
    let first = prompt(&mut rig, "write a.txt").await?;
    assert_eq!(
        first.stop,
        Stop::Cancelled,
        "the first turn stops: {first:?}"
    );
    assert_eq!(logged(&rig.mutations).len(), 2);

    // The stop leaves one scripted reply unused; the second turn must see only
    // its own script. Two identical calls are below the stop, so both must run
    // when the count starts fresh with the turn.
    rig.script.replace(vec![
        Reply::Call {
            id: "n1",
            tool: "mutate",
            args: FILE_A,
        },
        Reply::Call {
            id: "n2",
            tool: "mutate",
            args: FILE_A,
        },
        Reply::Text("second turn done"),
    ]);
    let second = prompt(&mut rig, "write a.txt again").await?;
    assert_eq!(
        second.stop,
        Stop::EndTurn,
        "the next turn runs to its end: {second:?}"
    );
    assert_eq!(
        logged(&rig.mutations).len(),
        4,
        "both calls of the second turn run: the stop of the first turn must not carry over"
    );
    assert!(
        second.settled.iter().all(|call| !call.is_error),
        "no call of the second turn is blocked: {:?}",
        second.settled
    );
    finish(rig).await
}

#[tokio::test]
async fn varied_mutations_and_repeated_reads_are_never_stopped() -> Outcome<()> {
    let call = |id, tool, args| Reply::Call { id, tool, args };
    let replies = vec![
        call("v1", "mutate", FILE_A),
        call("v2", "mutate", FILE_B),
        call("v3", "mutate", FILE_A),
        call("v4", "mutate", FILE_B),
        call("v5", "mutate", FILE_A),
        call("r1", "look", FILE_A),
        call("r2", "look", FILE_A),
        call("r3", "look", FILE_A),
        call("r4", "look", FILE_A),
        Reply::Text("all done"),
    ];
    let mut rig = rig("varied", Guard::On, Log::Workspace, replies).await?;
    let turn = prompt(&mut rig, "change files").await?;

    assert_eq!(
        turn.stop,
        Stop::EndTurn,
        "alternating mutations and repeated reads are not a repeat: {turn:?}"
    );
    assert_eq!(logged(&rig.mutations).len(), 5, "every mutation runs");
    assert_eq!(logged(&rig.looks).len(), 4, "every read runs");
    assert_eq!(
        rig.script.requests(),
        10,
        "the model is asked once per round"
    );
    assert!(
        turn.settled.iter().all(|call| !call.is_error),
        "no call is blocked: {:?}",
        turn.settled
    );
    finish(rig).await
}

#[tokio::test]
async fn a_disabled_guard_lets_identical_mutations_run() -> Outcome<()> {
    let mut rig = rig("off", Guard::Off, Log::Workspace, identical_calls()).await?;
    let turn = prompt(&mut rig, "write a.txt").await?;

    assert_eq!(
        turn.stop,
        Stop::EndTurn,
        "a disabled guard never stops a turn: {turn:?}"
    );
    assert_eq!(
        logged(&rig.mutations).len(),
        3,
        "the host runs the identical calls when the guard is off, so the stop above is the guard's"
    );
    assert_eq!(rig.script.requests(), 4);
    finish(rig).await
}

#[tokio::test]
async fn an_in_workspace_spill_log_is_read_and_its_warning_reported() -> Outcome<()> {
    let replies = vec![
        Reply::Call {
            id: "e1",
            tool: "exec",
            args: LOG_ONLY_WARNING,
        },
        Reply::Text("built"),
    ];
    let mut rig = rig("granted", Guard::On, Log::Workspace, replies).await?;
    let turn = prompt(&mut rig, "build").await?;
    assert_eq!(turn.stop, Stop::EndTurn, "{turn:?}");

    let findings = findings_of(&rig, turn.id).await?;
    assert!(
        findings.warnings.iter().any(|(path, line, text)| {
            &**path == "src/in_log.rs" && *line == 7 && text.contains("only_in_the_log")
        }),
        "the warning exists only in the job log, so the guard must have read the file through \
         the host; found {:?}, notices {:?}",
        warning_texts(&findings),
        turn.notices
    );
    assert_eq!(
        turn.notices_of("guard_fs_read_denied"),
        0,
        "the host served the read, so nothing is withheld: {:?}",
        turn.notices
    );
    finish(rig).await
}

#[tokio::test]
async fn a_refused_spill_path_falls_back_to_previews_and_reports_once() -> Outcome<()> {
    let first_replies = vec![
        Reply::Call {
            id: "e1",
            tool: "exec",
            args: FIRST_BUILD,
        },
        Reply::Text("first built"),
    ];
    let mut rig = rig("refused", Guard::On, Log::Outside, first_replies).await?;

    let first = prompt(&mut rig, "build").await?;
    assert_eq!(first.stop, Stop::EndTurn, "{first:?}");
    assert_eq!(
        first.notices_of("guard_fs_read_denied"),
        1,
        "the host refuses the absolute job log path and the guard says so once: {:?}",
        first.notices
    );
    assert!(
        first
            .notices
            .iter()
            .any(|(kind, text)| kind.as_ref() == "guard_fs_read_denied"
                && text.contains("fs.read denied")),
        "the notice names the withheld service: {:?}",
        first.notices
    );
    let findings = findings_of(&rig, first.id).await?;
    let texts = warning_texts(&findings);
    assert!(
        texts
            .iter()
            .any(|text| text.contains("only_in_the_first_preview")),
        "the preview is still scanned after a refused read: {texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|text| text.contains("only_in_the_first_log")),
        "a log the host refused to serve must not be scanned: {texts:?}"
    );

    rig.script.replace(vec![
        Reply::Call {
            id: "e2",
            tool: "exec",
            args: SECOND_BUILD,
        },
        Reply::Text("second built"),
    ]);
    let second = prompt(&mut rig, "test").await?;
    assert_eq!(second.stop, Stop::EndTurn, "{second:?}");
    assert_eq!(
        second.notices_of("guard_fs_read_denied"),
        0,
        "the refusal is reported once per session, not once per exec call: {:?}",
        second.notices
    );
    let findings = findings_of(&rig, second.id).await?;
    let texts = warning_texts(&findings);
    assert!(
        texts
            .iter()
            .any(|text| text.contains("only_in_the_second_preview")),
        "previews keep being scanned for the rest of the session: {texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|text| text.contains("only_in_the_second_log")),
        "the second log is never scanned either: {texts:?}"
    );
    finish(rig).await
}
