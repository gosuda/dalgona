#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! A `tool_call` hook rewrite is classified again before the tool runs: an
//! invalid rewrite fails the call closed and a valid rewrite still runs. A
//! hook block reaches the model with the blocking extension named.
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

/// Observed `(call, ok, preview)` triples for settled tool results.
type SettledCalls = Arc<Mutex<Vec<(String, bool, Box<str>)>>>;

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
        let escapes = path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) || (path.is_absolute() && !path.starts_with(ws.as_path()));
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
        Box::pin(async { ToolOutcome::Ok(Box::new(ToolOutput::from_text("ran"))) })
    }
}

/// What the fixture hook answers for every call.
#[derive(Clone, Copy)]
enum Step {
    Rewrite(&'static str),
    Block(&'static str),
}

struct Guard {
    step: Step,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for Guard {
    fn call(
        &self,
        _input: ToolCallEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let verdict = match self.step {
            Step::Rewrite(args) => ToolCallVerdict::Rewrite {
                args: RawJson::parse(args).expect("rewrite arguments"),
            },
            Step::Block(reason) => ToolCallVerdict::Block {
                reason: reason.into(),
            },
        };
        Box::pin(async { Ok(verdict) })
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

/// Answers `Allow`, or blocks with `block`, and records the arguments every
/// call hands it.
struct Spy {
    seen: Arc<Mutex<Vec<String>>>,
    block: Option<&'static str>,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for Spy {
    fn call(
        &self,
        input: ToolCallEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(input.args.as_str().to_owned());
        let verdict = self
            .block
            .map_or(ToolCallVerdict::Allow, |reason| ToolCallVerdict::Block {
                reason: reason.into(),
            });
        Box::pin(async { Ok(verdict) })
    }
}

/// An extension whose only part is a `tool_call` hook that records its input.
fn spy_extension(
    name: &str,
    seen: Arc<Mutex<Vec<String>>>,
    block: Option<&'static str>,
) -> Extension {
    ExtensionBuilder::new(name, "0.1.0", ServiceSet::EMPTY)
        .expect("spy builder")
        .on_tool_call(Spy { seen, block })
        .build()
        .expect("spy extension")
}

fn extension(step: Step, ran: Arc<Mutex<Vec<String>>>, seen: Settled) -> Extension {
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
        .on_tool_call(Guard { step })
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

/// Runs one scripted call whose `tool_call` hook answers `step`, then returns
/// the settled result and the arguments the tool actually ran with.
async fn run_with_hook(step: Step) -> ((bool, Box<str>), Vec<String>) {
    run_with_extras(step, Vec::new(), Vec::new()).await
}

/// [`run_with_hook`] with `before` registered ahead of the `fixture`
/// extension and `after` registered behind it.
async fn run_with_extras(
    step: Step,
    before: Vec<Extension>,
    after: Vec<Extension>,
) -> ((bool, Box<str>), Vec<String>) {
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
    let mut extensions = before;
    extensions.push(extension(step, Arc::clone(&ran), Arc::clone(&seen)));
    extensions.extend(after);
    let product = Product {
        name: "dal",
        data_root: data.clone(),
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
    let ((ok, preview), ran) = run_with_hook(Step::Rewrite(r#"{"path":"/etc/passwd"}"#)).await;
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
    let ((ok, preview), ran) = run_with_hook(Step::Rewrite(r#"{"path":"other.txt"}"#)).await;
    assert!(ok, "the valid rewrite must run: {preview}");
    assert_eq!(ran, vec![r#"{"path":"other.txt"}"#.to_owned()]);
}

/// Rewrites thread through the extensions in registration order: one ahead
/// of the rewriting extension is handed the model's arguments, one behind it
/// is handed the rewritten ones, and its block names that later extension
/// and keeps the tool from running.
#[tokio::test]
async fn a_later_extension_is_handed_the_rewrite_and_can_block_it() {
    let early = Arc::new(Mutex::new(Vec::new()));
    let late = Arc::new(Mutex::new(Vec::new()));
    let ((ok, preview), ran) = run_with_extras(
        Step::Rewrite(r#"{"path":"other.txt"}"#),
        vec![spy_extension("early", Arc::clone(&early), None)],
        vec![spy_extension(
            "late",
            Arc::clone(&late),
            Some("rewritten path refused"),
        )],
    )
    .await;
    assert!(!ok, "the later block must fail the call: {preview}");
    assert_eq!(
        preview.as_ref(),
        "blocked by late: rewritten path refused",
        "the block names the later extension, not the rewriting one"
    );
    assert!(ran.is_empty(), "the tool must not run: {ran:?}");
    let handed = |log: &Arc<Mutex<Vec<String>>>| {
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    };
    assert_eq!(
        handed(&early),
        vec![r#"{"path":"inside.txt"}"#.to_owned()],
        "an extension ahead of the rewrite sees the model's arguments"
    );
    assert_eq!(
        handed(&late),
        vec![r#"{"path":"other.txt"}"#.to_owned()],
        "an extension after the rewrite sees the rewritten arguments"
    );
}

#[tokio::test]
async fn a_hook_block_names_the_blocking_extension_and_never_runs_the_tool() {
    let ((ok, preview), ran) = run_with_hook(Step::Block("placeholder text in arguments")).await;
    assert!(!ok, "a blocked call must fail: {preview}");
    assert_eq!(
        preview.as_ref(),
        "blocked by fixture: placeholder text in arguments"
    );
    assert!(ran.is_empty(), "the tool must not run: {ran:?}");
}
#[derive(serde::Deserialize)]
struct OpArgs {
    op: String,
}

struct ClassTool {
    name: Name,
    spec: Arc<ToolSpec>,
    ran: Arc<Mutex<Vec<String>>>,
}

impl Tool for ClassTool {
    fn name(&self) -> &Name {
        &self.name
    }
    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }
    fn classify(&self, args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        let parsed: OpArgs = sonic_rs::from_str(args.as_str())
            .map_err(|error| ArgError::message(format!("op is required: {error}.")))?;
        match parsed.op.as_str() {
            "read" => Ok(ToolClass::Read),
            "write" => Ok(ToolClass::Other),
            _ => Err(ArgError::message(format!("unknown op {}.", parsed.op))),
        }
    }
    fn run<'a>(&'a self, call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        self.ran
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call.args.as_str().to_owned());
        Box::pin(async { ToolOutcome::Ok(Box::new(ToolOutput::from_text("ran"))) })
    }
}

struct SelectiveRewrite {
    target: &'static str,
    to: &'static str,
}

impl Hook<ToolCallEvent, ToolCallVerdict> for SelectiveRewrite {
    fn call(
        &self,
        input: ToolCallEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let verdict = if input.call.as_str() == self.target {
            ToolCallVerdict::Rewrite {
                args: RawJson::parse(self.to).expect("rewrite arguments"),
            }
        } else {
            ToolCallVerdict::Allow
        };
        Box::pin(async { Ok(verdict) })
    }
}

struct CallResults {
    seen: SettledCalls,
}

impl ObserveHook<ToolResultEvent> for CallResults {
    fn call(
        &self,
        input: ToolResultEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let seen = Arc::clone(&self.seen);
        Box::pin(async move {
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((input.call.as_str().to_owned(), input.ok, input.preview));
            Ok(())
        })
    }
}

/// Builds the classification fixture extension that rewrites the `second`
/// call's arguments to `to` while recording settled `(call, ok, preview)`
/// triples.
fn class_rewrite_extension(
    ran: Arc<Mutex<Vec<String>>>,
    seen: SettledCalls,
    to: &'static str,
) -> Extension {
    let name = Name::parse("fixture__class").expect("tool name");
    let tool = Arc::new(ClassTool {
        name: name.clone(),
        spec: Arc::new(ToolSpec {
            name,
            description: "read or write by op".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("tool schema"),
            grammar: None,
        }),
        ran,
    });
    ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .on_tool_call(SelectiveRewrite {
            target: "second",
            to,
        })
        .on_tool_result(CallResults { seen })
        .script_tool(
            tool,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture").expect("plugin name"),
                    kind: ExportKind::Tool,
                    local: Name::parse("class").expect("export name"),
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#).expect("export schema"),
                description: "class export".into(),
            },
        )
        .build()
        .expect("extension")
}

/// Runs a `first` call with `op` read and a `second` call with `second_op`;
/// the hook rewrites the `second` call's arguments to `rewrite_to`. Returns
/// the settled triples and the arguments the tool actually ran with.
async fn run_two_calls(
    second_op: &str,
    rewrite_to: &'static str,
) -> (Vec<(String, bool, Box<str>)>, Vec<String>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let step_call = format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"first\",\"name\":\"fixture__class\"}},{{\"type\":\"tool_call_started\",\"id\":\"second\",\"name\":\"fixture__class\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"first\",\"name\":\"fixture__class\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"op\":\"read\"}}}}}},{{\"id\":\"second\",\"name\":\"fixture__class\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"op\":\"{second_op}\"}}}}}}]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    );
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{step_call}{STEP_END}")).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let ran = Arc::new(Mutex::new(Vec::new()));
    let seen: SettledCalls = Arc::new(Mutex::new(Vec::new()));
    let extension = class_rewrite_extension(Arc::clone(&ran), Arc::clone(&seen), rewrite_to);
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
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("rewrite-class-test"),
        )
        .await
        .expect("session");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run two reads".into(),
            }],
        })
        .await
        .expect("prompt accepted");
    let settled = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let done = seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len();
            if done >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    settled.expect("both tool results arrived");
    let seen = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let ran = ran
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    (seen, ran)
}

#[tokio::test]
async fn hook_rewrite_from_read_to_mutating_class_is_refused_without_running() {
    let (seen, ran) = run_two_calls("read", r#"{"op":"write"}"#).await;
    let by_call = |id: &str| {
        seen.iter()
            .find(|(call, _, _)| call == id)
            .unwrap_or_else(|| panic!("missing result for {id}: {seen:?}"))
            .clone()
    };
    let (_, a_ok, _) = by_call("first");
    let (_, b_ok, b_preview) = by_call("second");
    assert!(a_ok, "the untouched read must still run: {seen:?}");
    assert!(!b_ok, "the class-changing rewrite must fail: {seen:?}");
    assert!(
        b_preview.contains("changing its classification")
            && b_preview.contains("read")
            && (b_preview.contains("keep the rewrite read-only")
                || b_preview.contains("separately")),
        "the refusal names the classification change and the fix: {b_preview}"
    );
    assert_eq!(
        ran,
        vec![r#"{"op":"read"}"#.to_owned()],
        "the mutating rewrite must never run beside the read: {ran:?}"
    );
}

/// The re-classification guards only a read-only call: a mutating call that a
/// hook rewrites into a read keeps running, with the rewritten arguments,
/// beside the untouched read.
#[tokio::test]
async fn hook_rewrite_from_a_mutating_class_to_read_still_runs() {
    let (seen, mut ran) = run_two_calls("write", r#"{"op":"read"}"#).await;
    assert_eq!(seen.len(), 2, "both calls settle: {seen:?}");
    assert!(
        seen.iter().all(|(_, ok, _)| *ok),
        "neither call may be refused: {seen:?}"
    );
    ran.sort();
    assert_eq!(
        ran,
        vec![r#"{"op":"read"}"#.to_owned(), r#"{"op":"read"}"#.to_owned()],
        "the rewritten call ran with its rewritten arguments: {ran:?}"
    );
}
