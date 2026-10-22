//! Host document schemes: `job://` URIs resolve through the owning live
//! session's job table, live `session://` entries read back as JSON, and
//! closed sessions read the same JSON from their journal read-only, with
//! typed missing and typed failure outcomes and no actor revival.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

#[cfg(unix)]
use dal_agent::SpawnOpts;
use dal_agent::error::SchemeError;
use dal_agent::ext::{
    ArgError, BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome,
    ToolOutput,
};
use dal_agent::{Agent, Env, Host, Product, SessionRef};
use dal_core::{
    ApprovalMode, ClientId, Command, Config, ConfigProduct, Expect, ListQuery, ModelInfo, Name,
    Part, RawJson, Save, ToolClass, ToolSpec, Visibility, Workspace,
};
#[cfg(unix)]
use dal_core::{JobId, Preview};

#[cfg(unix)]
const DETACH_TOOL: &str = "doctest__detach";
const NOOP_TOOL: &str = "doctest__noop";

const STEP_CALL_TEMPLATE: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"doc-call\",\"name\":\"TOOLNAME\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"doc-call\",\"name\":\"TOOLNAME\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// A model-visible tool that detaches a `sleep 60` child and publishes its
/// job identity, leaving a real row in the owning session's job table.
#[cfg(unix)]
struct DetachProbe {
    name: Name,
    spec: Arc<ToolSpec>,
    job: Arc<Mutex<Option<JobId>>>,
}

#[cfg(unix)]
impl Tool for DetachProbe {
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
                title: "detach-probe".into(),
                body: "sleep 60".into(),
                digest: None,
            };
            let approved = match cx.authorize(preview).await {
                Ok(approved) => approved,
                Err(reason) => {
                    return ToolOutcome::Err(dal_agent::ToolError::Denied(reason));
                }
            };
            let argv = [
                OsString::from("sh"),
                OsString::from("-c"),
                OsString::from("sleep 60"),
            ];
            let opts = SpawnOpts {
                cwd: cx.workspace().as_path().to_path_buf(),
                timeout: None,
                env: Vec::new(),
                stdout_prefix_limit: 0,
            };
            let proc = match cx.spawn(&argv, opts, approved) {
                Ok(proc) => proc,
                Err(error) => return ToolOutcome::Err(error),
            };
            let job = cx.detach(proc);
            *self.job.lock().unwrap_or_else(PoisonError::into_inner) = Some(job);
            ToolOutcome::Detached(job)
        })
    }
}

/// A model-visible tool that settles at once with no side effects.
struct NoopProbe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for NoopProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move { ToolOutcome::Ok(Box::new(ToolOutput::from_text("noop ok"))) })
    }
}

fn tool_spec(name: &str) -> Result<Arc<ToolSpec>, Box<dyn std::error::Error>> {
    Ok(Arc::new(ToolSpec {
        name: Name::parse(name)?,
        description: "document scheme probe tool".into(),
        parameters: RawJson::parse(r#"{"type":"object"}"#)?,
        grammar: None,
    }))
}

/// Starts one host with a single probe tool and a scripted fixture that
/// calls it, returning the host and its retained temp guard.
async fn start_host(
    tool: Arc<dyn Tool>,
    tool_name: &str,
) -> Result<(Host, tempfile::TempDir), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    let step = STEP_CALL_TEMPLATE.replace("TOOLNAME", tool_name);
    std::fs::write(&fixture, format!("{step}{STEP_END}"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let extension = ExtensionBuilder::new("doctest", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .tool(tool, Visibility::Model)
        .build()?;
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
        cwd: workspace_dir,
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    Ok((host, tmp))
}

async fn open_session(
    host: &Host,
    workspace_dir: &std::path::Path,
) -> Result<Agent, Box<dyn std::error::Error>> {
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir.to_path_buf())?,
                name: None,
            },
            ClientId::new("doc-probe"),
        )
        .await?;
    Ok(agent)
}

/// Finds one session's journal under the store layout
/// `<data_root>/sessions/<workspace key>/<session>/journal.jsonl`.
fn journal_path(
    data_root: &std::path::Path,
    session: dal_core::SessionId,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let name = session.to_string();
    for workspace in std::fs::read_dir(data_root.join("sessions"))? {
        let candidate = workspace?.path().join(&name).join("journal.jsonl");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("the closed session's journal stays on disk: {session}").into())
}

/// Reads every live entry back through `session://` URIs, keeping the
/// request URI alongside the JSON text it served while the actor is live.
fn live_entry_texts(
    host: &Host,
    session: dal_core::SessionId,
    view: &dal_core::View,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let mut live = Vec::with_capacity(view.entries.items.len());
    for item in &view.entries.items {
        let uri = format!("session://{session}/{}", item.id);
        let text = host.doc(&uri)?.text.into_string();
        live.push((uri, text));
    }
    Ok(live)
}

/// Lists one session directory by file name, so journal reads can prove
/// they add and remove no file.
fn directory_files(
    journal: &std::path::Path,
) -> Result<std::collections::BTreeSet<OsString>, Box<dyn std::error::Error>> {
    let parent = journal.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "session directory missing")
    })?;
    let mut files = std::collections::BTreeSet::new();
    for child in std::fs::read_dir(parent)? {
        files.insert(child?.file_name());
    }
    Ok(files)
}

/// Checks every closed entry still serves the JSON it served while live.
fn check_closed_entries(
    host: &Host,
    live: &[(String, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    for (uri, live_text) in live {
        let doc = host.doc(uri)?;
        assert_eq!(doc.uri.as_ref(), uri.as_str(), "the uri echoes the request");
        assert_eq!(
            doc.text.as_ref(),
            live_text.as_str(),
            "the closed entry JSON is the live entry JSON"
        );
    }
    Ok(())
}

/// Submits one prompt and waits for its turn to end, returning the view.
async fn prompt_and_wait(
    agent: &Agent,
    text: &str,
) -> Result<dal_core::View, Box<dyn std::error::Error>> {
    let mut subscription = agent.subscribe(None)?;
    let _ = agent
        .submit(Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        })
        .await?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
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
    let view = agent.view(dal_core::PageReq::default())?;
    assert!(ended, "scripted turn reaches TurnEnded");
    Ok(view)
}

#[cfg(unix)]
#[tokio::test]
async fn live_job_uris_resolve_through_the_owning_session_table() {
    let job_cell: Arc<Mutex<Option<JobId>>> = Arc::new(Mutex::new(None));
    let probe = Arc::new(DetachProbe {
        name: Name::parse(DETACH_TOOL).expect("tool name"),
        spec: tool_spec(DETACH_TOOL).expect("tool spec"),
        job: Arc::clone(&job_cell),
    });
    let (host, tmp) = start_host(probe, DETACH_TOOL).await.expect("host starts");
    let workspace_dir = tmp.path().join("w");
    let agent = open_session(&host, &workspace_dir)
        .await
        .expect("session opens");
    prompt_and_wait(&agent, "detach a sleeper")
        .await
        .expect("detach turn ends");
    let job = job_cell
        .lock()
        .expect("job cell")
        .expect("detach publishes its job identity");

    let uri = format!("job://{job}");
    let doc = host.doc(&uri).expect("live job resolves");
    assert_eq!(
        doc.uri.as_ref(),
        uri.as_str(),
        "resolved uri echoes the request"
    );
    let text = doc.text.into_string();
    assert!(
        text.contains("Full output:") && text.contains(&job.to_string()),
        "job page carries the table row state and log pointer: {text}"
    );

    let log_uri = format!("job://{job}/log");
    let log = host.doc(&log_uri).expect("live job log resolves");
    assert_eq!(log.uri.as_ref(), log_uri.as_str());
    assert_eq!(
        log.text.as_ref(),
        "",
        "a quiet job's log page is its empty output tail, never the status page"
    );
    for route in ["bogus", "log/", "LOG", "log/extra"] {
        let bad = format!("job://{job}/{route}");
        match host.doc(&bad) {
            Err(SchemeError::NotFound { uri }) => assert_eq!(uri.as_ref(), bad.as_str()),
            other => panic!("route {route:?} is outside the job grammar, got {other:?}"),
        }
    }

    let missing = JobId::new_v7();
    match host.doc(&format!("job://{missing}")) {
        Err(SchemeError::Failed { message }) => assert_eq!(
            message.into_string(),
            format!("job {missing} is not running."),
            "unknown ids keep the dispatch table text"
        ),
        other => panic!("unknown job id must report not running, got {other:?}"),
    }
    match host.doc("job://not-a-job-id") {
        Err(SchemeError::NotFound { uri }) => {
            assert_eq!(uri.as_ref(), "job://not-a-job-id");
        }
        other => panic!("malformed job id must be not found, got {other:?}"),
    }

    host.close(agent.session())
        .await
        .expect("close reaps the sleeper");
}

#[tokio::test]
async fn live_session_entries_read_back_as_json() {
    let probe = Arc::new(NoopProbe {
        name: Name::parse(NOOP_TOOL).expect("tool name"),
        spec: tool_spec(NOOP_TOOL).expect("tool spec"),
    });
    let (host, tmp) = start_host(probe, NOOP_TOOL).await.expect("host starts");
    let agent = open_session(&host, &tmp.path().join("w"))
        .await
        .expect("session opens");
    let marker = "session-doc-probe-marker-entries-read-back";
    let view = prompt_and_wait(&agent, marker).await.expect("turn ends");
    assert!(!view.entries.items.is_empty(), "the turn journals entries");

    let session = agent.session();
    let mut hits = 0u32;
    for item in &view.entries.items {
        let uri = format!("session://{session}/{}", item.id);
        let doc = host.doc(&uri).expect("live entry resolves");
        assert_eq!(doc.uri.as_ref(), uri.as_str());
        let text = doc.text.into_string();
        let trimmed = text.trim();
        assert!(
            trimmed.starts_with('{') && trimmed.ends_with('}'),
            "session entries read back as JSON objects: {text}"
        );
        hits += u32::from(trimmed.contains(marker));
    }
    assert_eq!(hits, 1, "exactly the prompt entry carries the marker text");

    // Paths outside the `<session>/<entry>` grammar are typed misses: a
    // malformed entry never defaults to the first entry or panics.
    let first = view.entries.items[0].id;
    for path in [
        format!("{session}"),
        format!("{session}/"),
        format!("{session}/0"),
        format!("{session}/-{first}"),
        format!("{session}/abc"),
        format!("{session}/{first}/{first}"),
        format!("{session}/{first}x"),
        format!("not-a-session/{first}"),
        format!("/{first}"),
    ] {
        let bad = format!("session://{path}");
        match host.doc(&bad) {
            Err(SchemeError::NotFound { uri }) => assert_eq!(uri.as_ref(), bad.as_str()),
            other => panic!("{bad} is outside the session grammar, got {other:?}"),
        }
    }

    host.close(agent.session()).await.expect("close");
}

#[tokio::test]
async fn closed_sessions_report_typed_missing_without_revival() {
    let probe = Arc::new(NoopProbe {
        name: Name::parse(NOOP_TOOL).expect("tool name"),
        spec: tool_spec(NOOP_TOOL).expect("tool spec"),
    });
    let (host, tmp) = start_host(probe, NOOP_TOOL).await.expect("host starts");
    let workspace_dir = tmp.path().join("w");
    let keeper = open_session(&host, &workspace_dir)
        .await
        .expect("keeper session opens");
    let agent = open_session(&host, &workspace_dir)
        .await
        .expect("session opens");
    let view = prompt_and_wait(&agent, "closed session probe entry")
        .await
        .expect("turn ends");
    assert!(!view.entries.items.is_empty(), "the turn journals entries");
    let session = agent.session();
    let live = live_entry_texts(&host, session, &view).expect("entries read while live");

    host.close(session).await.expect("session closes");
    let page = host
        .sessions(ListQuery {
            limit: None,
            cursor: None,
            search: None,
        })
        .expect("sessions list");
    assert!(
        page.items.iter().any(|info| info.id == session),
        "the closed journal stays on disk; only its actor is gone"
    );
    // With every session closed, only the store's root scan can serve the
    // reads below: no live workspace or actor is left to answer for them.
    host.close(keeper.session()).await.expect("keeper closes");
    let journal = journal_path(host.data_root(), session).expect("closed journal path");
    let bytes = std::fs::read(&journal).expect("read closed journal");
    let modified = std::fs::metadata(&journal)
        .and_then(|metadata| metadata.modified())
        .expect("journal mtime");
    let files = directory_files(&journal).expect("list session directory");

    check_closed_entries(&host, &live).expect("closed entries read from their journal");
    assert!(
        agent.poll_status().await.is_err(),
        "reading a closed session never revives its actor"
    );
    assert_eq!(
        std::fs::read(&journal).expect("reread closed journal"),
        bytes,
        "document reads leave the journal bytes untouched"
    );
    assert_eq!(
        std::fs::metadata(&journal)
            .and_then(|metadata| metadata.modified())
            .expect("journal mtime after reads"),
        modified,
        "document reads leave the journal mtime untouched"
    );
    let after = directory_files(&journal).expect("relist session directory");
    assert_eq!(
        after, files,
        "document reads add and remove no session file"
    );

    let missing_entry = format!("session://{session}/999999");
    match host.doc(&missing_entry) {
        Err(SchemeError::NotFound { uri: missing }) => {
            assert_eq!(missing.as_ref(), missing_entry.as_str());
        }
        other => panic!("an entry the journal never held must be typed missing, got {other:?}"),
    }
    let missing_session = format!("session://{}/1", dal_core::SessionId::new_v7());
    match host.doc(&missing_session) {
        Err(SchemeError::NotFound { uri: missing }) => {
            assert_eq!(missing.as_ref(), missing_session.as_str());
        }
        other => panic!("a session the store never held must be typed missing, got {other:?}"),
    }

    let mut damaged = bytes.clone();
    damaged.extend_from_slice(b"this is not a record\n");
    std::fs::write(&journal, &damaged).expect("stage a corrupt complete record");
    match host.doc(&live[0].0) {
        Err(SchemeError::Failed { message }) => assert!(
            message.contains("damaged"),
            "corruption is a typed failure naming the damage: {message}"
        ),
        other => panic!("a corrupt journal must fail typed, never read as missing, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(&journal).expect("reread corrupt journal"),
        damaged,
        "a failed read never repairs the journal"
    );
}
