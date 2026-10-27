//! `SetScopedModels` through a real host: the control command every client
//! (the terminal picker, a remote connection) submits to choose the models
//! `/model` lists.
//!
//! A save writes the `scoped_models` key of the user's `dal.toml`, so the
//! next process starts with the same scope; an empty list clears it; a save
//! that cannot be written still changes the session and says it did not
//! persist. Every case boots a host the way the process edge does: read the
//! file, load the configuration, start the host, tell it where saves land.
#![expect(
    clippy::expect_used,
    reason = "test fixtures abort at the failed boundary"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dal_agent::{Agent, AgentError, Env, Host, Product, SessionRef};
use dal_core::{ClientId, Command, Config, ConfigProduct, Output, PageReq, Reply, Workspace};

const CLOSE: Duration = Duration::from_secs(5);

/// A user file with a comment, a default, and an extension table that a
/// scope save must leave alone. It sets no `model`: a configured non-synthetic
/// model makes every session open resolve it through the provider catalog,
/// which would tie these cases to the network instead of the save.
const SEED: &str =
    "# kept note\nthinking = \"high\"\n\n[plugin.user_tool]\nanything = 1 # kept inline\n";

struct Rig {
    _temp: tempfile::TempDir,
    data: PathBuf,
    config_path: PathBuf,
    workspace: Workspace,
    host: Host,
    agent: Agent,
}

/// Starts a host over the current file (when there is one) and opens a new
/// session on it. `save_to` is where default saves land; `None` leaves the
/// host with nowhere to write.
async fn boot(
    data: &Path,
    config_path: &Path,
    save_to: Option<&Path>,
    workspace: &Workspace,
) -> (Host, Agent) {
    let text = std::fs::read_to_string(config_path).ok();
    let config =
        Config::load(ConfigProduct::Dalgon, data, "", text.as_deref()).expect("config loads");
    let product = Product {
        name: "dal",
        data_root: data.to_path_buf(),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.as_path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    if let Some(path) = save_to {
        host.set_config_path(path.to_path_buf());
    }
    let agent = open(&host, workspace).await;
    (host, agent)
}

async fn open(host: &Host, workspace: &Workspace) -> Agent {
    host.open(
        SessionRef::New {
            workspace: workspace.clone(),
            name: None,
        },
        ClientId::new("scoped-models-persist"),
    )
    .await
    .expect("session opens")
}

impl Rig {
    /// A host over the user file `seed`, which it also saves into.
    async fn with_file(seed: &str) -> Self {
        Self::start(Some(seed), true).await
    }

    /// A host with no user file and nowhere to write one.
    async fn without_save_target() -> Self {
        Self::start(None, false).await
    }

    async fn start(seed: Option<&str>, saves: bool) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let data = temp.path().join("data");
        let workspace_dir = temp.path().join("workspace");
        let config_dir = temp.path().join("config");
        for dir in [&data, &workspace_dir, &config_dir] {
            std::fs::create_dir_all(dir).expect("fixture directory");
        }
        let config_path = config_dir.join("dal.toml");
        if let Some(seed) = seed {
            std::fs::write(&config_path, seed).expect("seed config");
        }
        let workspace = Workspace::new(workspace_dir).expect("workspace");
        let save_to = saves.then_some(config_path.as_path());
        let (host, agent) = boot(&data, &config_path, save_to, &workspace).await;
        Self {
            _temp: temp,
            data,
            config_path,
            workspace,
            host,
            agent,
        }
    }

    /// Closes the host and boots a fresh one over whatever the file now says.
    async fn restart(self) -> Self {
        let Self {
            _temp: temp,
            data,
            config_path,
            workspace,
            host,
            agent,
        } = self;
        drop(agent);
        let _ = host.shutdown(CLOSE).await;
        let (host, agent) = boot(&data, &config_path, Some(&config_path), &workspace).await;
        Self {
            _temp: temp,
            data,
            config_path,
            workspace,
            host,
            agent,
        }
    }

    fn saved(&self) -> String {
        std::fs::read_to_string(&self.config_path).expect("config file reads")
    }

    /// Whether the file as it now stands loads the way the next start would.
    fn loads(&self) -> bool {
        Config::load(
            ConfigProduct::Dalgon,
            &self.data,
            "",
            Some(self.saved().as_str()),
        )
        .is_ok()
    }
}

fn ids(list: &[&str]) -> Vec<Box<str>> {
    list.iter().map(|id| Box::<str>::from(*id)).collect()
}

async fn submit(agent: &Agent, list: &[&str]) -> Result<Reply, AgentError> {
    agent.submit(Command::SetScopedModels(ids(list))).await
}

/// The reply's text, or a description of the reply when it carries none, so
/// the failing assertion shows what came back.
fn said(reply: Reply) -> String {
    let Reply::Done(Output::Text(text)) = reply else {
        return format!("not a text reply: {reply:?}");
    };
    text.to_string()
}

fn view_scope(agent: &Agent) -> Vec<Box<str>> {
    agent
        .view(PageReq::default())
        .expect("view snapshots")
        .settings
        .scoped_models
}

#[tokio::test]
async fn a_saved_scope_is_written_to_dal_toml_and_keeps_the_rest_of_the_file() {
    let rig = Rig::with_file(SEED).await;
    let list = ["beta/gpt-4o", "alpha/mini"];
    let reply = submit(&rig.agent, &list).await.expect("the scope applies");
    assert!(matches!(reply, Reply::Done(_)), "{reply:?}");
    assert_eq!(rig.agent.scoped_models(), ids(&list), "the session changes");
    let saved = rig.saved();
    assert!(
        saved.contains("scoped_models"),
        "nothing was saved: {saved}"
    );
    for kept in [
        "# kept note",
        "thinking = \"high\"",
        "[plugin.user_tool]",
        "anything = 1 # kept inline",
    ] {
        assert!(saved.contains(kept), "the save dropped {kept:?}: {saved}");
    }
    assert!(rig.loads(), "the saved file must load at the next start");
    let _ = rig.host.shutdown(CLOSE).await;
}

/// The saved list keeps the order it was submitted in and every segment of a
/// nested id (`openrouter/vendor/model`).
#[tokio::test]
async fn a_saved_scope_survives_a_restart_in_order() {
    let rig = Rig::with_file(SEED).await;
    let list = ["beta/gpt-4o", "openrouter/vendor/model", "alpha/mini"];
    submit(&rig.agent, &list).await.expect("the scope applies");
    let rig = rig.restart().await;
    assert_eq!(
        rig.agent.scoped_models(),
        ids(&list),
        "a new process must start with the saved scope, in saved order"
    );
    assert_eq!(
        view_scope(&rig.agent),
        ids(&list),
        "the picker reads the saved scope from the view"
    );
    let _ = rig.host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn an_empty_scope_clears_the_saved_list_for_the_next_start() {
    let rig = Rig::with_file(SEED).await;
    submit(&rig.agent, &["beta/gpt-4o"])
        .await
        .expect("the scope applies");
    let rig = rig.restart().await;
    assert_eq!(
        rig.agent.scoped_models(),
        ids(&["beta/gpt-4o"]),
        "the first save must have persisted"
    );
    submit(&rig.agent, &[]).await.expect("the scope clears");
    assert!(rig.agent.scoped_models().is_empty(), "the session clears");
    assert!(rig.loads(), "the cleared file must load at the next start");
    let rig = rig.restart().await;
    assert!(
        rig.agent.scoped_models().is_empty(),
        "a cleared scope must stay cleared after a restart"
    );
    assert_eq!(view_scope(&rig.agent), Vec::<Box<str>>::new());
    for kept in ["# kept note", "anything = 1 # kept inline"] {
        assert!(rig.saved().contains(kept), "clearing dropped {kept:?}");
    }
    let _ = rig.host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn save_and_clear_print_the_count_and_cleared_lines() {
    let rig = Rig::with_file(SEED).await;
    let two = submit(&rig.agent, &["beta/gpt-4o", "alpha/mini"])
        .await
        .expect("two apply");
    assert_eq!(
        said(two),
        "2 scoped models saved. /model now lists only these."
    );
    let one = submit(&rig.agent, &["beta/gpt-4o"])
        .await
        .expect("one applies");
    assert_eq!(
        said(one),
        "1 scoped model saved. /model now lists only these."
    );
    let none = submit(&rig.agent, &[]).await.expect("none clears");
    assert_eq!(
        said(none),
        "Scoped models cleared. /model lists every model."
    );
    let _ = rig.host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_failed_save_changes_the_session_and_says_it_did_not_persist() {
    let rig = Rig::without_save_target().await;
    let reply = submit(&rig.agent, &["beta/gpt-4o"])
        .await
        .expect("the session scope still applies");
    let text = said(reply);
    assert!(
        text.contains("It did not persist: cannot save dal.toml: no config path"),
        "a failed save must say so and why: {text}"
    );
    assert!(
        !text.contains(" saved."),
        "a failed save must not claim success: {text}"
    );
    assert_eq!(rig.agent.scoped_models(), ids(&["beta/gpt-4o"]));
    assert_eq!(view_scope(&rig.agent), ids(&["beta/gpt-4o"]));
    let _ = rig.host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_document_that_cannot_be_edited_is_left_byte_identical_and_reported() {
    let rig = Rig::with_file(SEED).await;
    let broken = "[plugin\n";
    std::fs::write(&rig.config_path, broken).expect("break the file after startup");
    let reply = submit(&rig.agent, &["beta/gpt-4o"])
        .await
        .expect("the session scope still applies");
    let text = said(reply);
    assert!(
        text.contains("It did not persist: cannot update dal.toml"),
        "{text}"
    );
    assert_eq!(
        rig.saved(),
        broken,
        "a refused edit must not touch the file"
    );
    assert_eq!(rig.agent.scoped_models(), ids(&["beta/gpt-4o"]));
    let _ = rig.host.shutdown(CLOSE).await;
}

/// An empty id is a list the next start could not load (`scoped_models`
/// entries must be non-empty), so the writer refuses it: the reply says it
/// did not persist and the file keeps its bytes and still loads.
#[tokio::test]
async fn an_empty_id_is_never_written_to_dal_toml() {
    let rig = Rig::with_file(SEED).await;
    let before = rig.saved();
    let reply = submit(&rig.agent, &["beta/gpt-4o", ""])
        .await
        .expect("the session scope still applies");
    let text = said(reply);
    assert!(
        text.contains("It did not persist: cannot update dal.toml"),
        "{text}"
    );
    assert_eq!(
        rig.saved(),
        before,
        "a refused list must not reach the file"
    );
    assert!(rig.loads(), "the file must still load at the next start");
    let _ = rig.host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_save_keeps_edits_made_to_the_file_after_startup() {
    let rig = Rig::with_file(SEED).await;
    let mut edited = rig.saved();
    edited.push_str("\n[plugin.late_edit]\nkept = true\n");
    std::fs::write(&rig.config_path, &edited).expect("edit the file after startup");
    submit(&rig.agent, &["beta/gpt-4o"])
        .await
        .expect("the scope applies");
    let saved = rig.saved();
    assert!(
        saved.contains("scoped_models"),
        "nothing was saved: {saved}"
    );
    for kept in ["# kept note", "[plugin.late_edit]\nkept = true"] {
        assert!(saved.contains(kept), "the save dropped {kept:?}: {saved}");
    }
    assert!(rig.loads(), "the saved file must load at the next start");
    let _ = rig.host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_saved_scope_seeds_later_sessions_of_the_same_host_only() {
    let rig = Rig::with_file(SEED).await;
    let earlier = open(&rig.host, &rig.workspace).await;
    submit(&rig.agent, &["beta/gpt-4o", "alpha/mini"])
        .await
        .expect("the scope applies");
    let later = open(&rig.host, &rig.workspace).await;
    assert_eq!(
        later.scoped_models(),
        ids(&["beta/gpt-4o", "alpha/mini"]),
        "a session opened after the save starts with the saved scope"
    );
    assert!(
        earlier.scoped_models().is_empty(),
        "a session already open keeps its own scope"
    );
    let _ = rig.host.shutdown(CLOSE).await;
}
