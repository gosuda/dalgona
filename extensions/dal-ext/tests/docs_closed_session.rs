#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! The `dal://` documentation door against a closed session: reading the
//! built-in manual while a session's actor is gone serves the same pages it
//! served live, opens no journal for write, and leaves every byte and mtime
//! under the data root exactly as the close left them.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ApprovalMode, ClientId, Command, Config, ConfigProduct, Expect, Part, Reply, Save, SessionId,
    UpdateKind, Workspace,
};

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "dal-docs-closed-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
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

/// One path under a tree: its bytes (files only) and modification time.
type Tree = Vec<(PathBuf, Option<Vec<u8>>, SystemTime)>;

/// Every path under `root` with its bytes and mtime in stable order, so a
/// created, removed, truncated, appended, or touched file all show as a diff.
fn tree(root: &Path) -> Tree {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let metadata = std::fs::symlink_metadata(&path).expect("stat path");
        let modified = metadata.modified().expect("read mtime");
        if !metadata.is_dir() {
            found.push((
                path.clone(),
                Some(std::fs::read(&path).expect("read file")),
                modified,
            ));
            continue;
        }
        for child in std::fs::read_dir(&path).expect("list directory") {
            stack.push(child.expect("read directory entry").path());
        }
        found.push((path, None, modified));
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    found
}

/// Finds one session's journal under
/// `<data_root>/sessions/<workspace key>/<session>/journal.jsonl`.
fn journal_path(data_root: &Path, session: SessionId) -> PathBuf {
    let name = session.to_string();
    let mut found = None;
    for workspace in std::fs::read_dir(data_root.join("sessions")).expect("sessions directory") {
        let candidate = workspace
            .expect("workspace entry")
            .path()
            .join(&name)
            .join("journal.jsonl");
        if candidate.is_file() {
            found = Some(candidate);
            break;
        }
    }
    assert!(
        found.is_some(),
        "the closed session's journal stays on disk: {session}"
    );
    found.expect("journal present")
}

async fn start_host(scratch: &Scratch) -> (Host, Workspace, PathBuf) {
    let data = scratch.0.join("data");
    let workspace_dir = scratch.0.join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, STEP_END).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let manual = dal_ext::docs::extension().expect("manual extension builds");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![manual],
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
    (
        host,
        Workspace::new(workspace_dir).expect("workspace"),
        data,
    )
}

/// Submits one prompt and waits until its turn ends.
async fn prompt_and_wait(agent: &Agent, text: &str) {
    let mut subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        })
        .await
        .expect("approval set");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("prompt submitted");
    assert!(
        matches!(reply, Reply::Accepted { .. }),
        "prompt not accepted: {reply:?}"
    );
    let mut ended = false;
    for _ in 0..40 {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await;
        let Ok(Some(delivery)) = delivery else {
            break;
        };
        if let Delivery::Update(update) = &delivery
            && matches!(update.kind, UpdateKind::TurnEnded { .. })
        {
            ended = true;
            break;
        }
    }
    assert!(ended, "the scripted turn never reached TurnEnded");
}

#[tokio::test]
async fn dal_manual_reads_against_a_closed_session_leave_the_data_root_untouched() {
    let scratch = Scratch::new();
    let (host, workspace, data) = start_host(&scratch).await;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("docs-closed"),
        )
        .await
        .expect("session opens");
    prompt_and_wait(&agent, "closed docs probe entry").await;
    let session = agent.session();

    let rows = host.docs();
    assert!(
        rows.iter().any(|row| row.uri.as_ref() == "dal://changelog"),
        "the manual lists its changelog page"
    );
    let live: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            let uri = row.uri.to_string();
            let text = host.doc(&uri).expect("page reads while live").text;
            (uri, text.into_string())
        })
        .collect();
    let entry_uri = {
        let view = agent.view(dal_core::PageReq::default()).expect("live view");
        let first = view
            .entries
            .items
            .first()
            .expect("the turn journals entries");
        format!("session://{session}/{}", first.id)
    };
    let entry_live = host
        .doc(&entry_uri)
        .expect("entry reads while live")
        .text
        .into_string();

    host.close(session).await.expect("session closes");
    let journal = journal_path(&data, session);
    let journal_bytes = std::fs::read(&journal).expect("read closed journal");
    let before = tree(&data);

    for (uri, live_text) in &live {
        let doc = host
            .doc(uri)
            .expect("a manual page reads with its session closed");
        assert_eq!(doc.uri.as_ref(), uri.as_str(), "the uri echoes the request");
        assert_eq!(
            doc.text.as_ref(),
            live_text.as_str(),
            "{uri}: the closed read serves the page it served live"
        );
    }
    assert_eq!(
        host.docs().len(),
        live.len(),
        "the listing is the same with the session closed"
    );
    assert_eq!(
        host.doc(&entry_uri)
            .expect("a closed entry reads beside manual reads")
            .text
            .as_ref(),
        entry_live.as_str(),
        "the closed entry JSON is the live entry JSON"
    );
    assert!(
        agent.poll_status().await.is_err(),
        "reading documents never revives the closed session's actor"
    );

    assert_eq!(
        std::fs::read(&journal).expect("reread closed journal"),
        journal_bytes,
        "document reads leave the journal bytes untouched"
    );
    assert_eq!(
        tree(&data),
        before,
        "document reads create, remove, append to, or touch nothing under the data root"
    );

    // Negative control: the tree comparison above detects a one-byte append,
    // so an unchanged tree means the reads wrote nothing.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"\n"))
        .expect("append one byte to the journal");
    assert_ne!(
        tree(&data),
        before,
        "the snapshot comparison detects a journal append"
    );
}
