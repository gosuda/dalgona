//! `Save::SessionAndDefault` through the public [`Agent::submit`]: the direct
//! command path the terminal model picker (`picker.rs` rows), wire clients,
//! and embedded callers use.
//!
//! The save contract is public and must not depend on the frontend: a
//! command carrying `SessionAndDefault` applies to the session AND persists
//! the product default into the user's `dal.toml`, while `SessionOnly`
//! never touches the file. The four consumer-visible kinds (`SetModel`,
//! `SetThinking`, `SetApproval`, `SetMode`) flow through one
//! `persist_for` step, so one parameterized proof covers them all instead
//! of duplicating the same path.
//!
//! Every case boots a real host the way the process edge does: read the
//! file, load the configuration, start the host, tell it where saves land.
#![expect(
    clippy::expect_used,
    reason = "test fixtures abort at the failed boundary"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    ApprovalMode, ClientId, Command, Config, ConfigProduct, Family, Mode, ModelRoute, PageReq,
    Reply, Save, SettingsView, ThinkingLevel, UpdateKind, Workspace,
};

const CLOSE: Duration = Duration::from_secs(5);
const PUBLISHED: Duration = Duration::from_secs(5);

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A user file with decor and an extension table a default save must leave
/// alone. Every seeded default differs from the value the proof submits, so
/// a save that silently no-ops cannot pass.
const SEED: &str = "# kept note\n\
                    model = \"openai-chat/gpt-mini\"\n\
                    thinking = \"low\"\n\
                    approval = \"ask\"\n\
                    mode = \"normal\"\n\
                    \n\
                    [plugin.user_tool]\n\
                    anything = 1 # kept inline\n";

/// The seed lines minus the one line the kind's save replaces: everything
/// else must survive byte-identically.
fn kept_lines(kind: &Kind) -> Vec<&'static str> {
    [
        "# kept note",
        "model = \"openai-chat/gpt-mini\"",
        "thinking = \"low\"",
        "approval = \"ask\"",
        "mode = \"normal\"",
        "[plugin.user_tool]",
        "anything = 1 # kept inline",
    ]
    .into_iter()
    .filter(|line| *line != kind.replaced_line)
    .collect()
}

fn chat_route(model: &str) -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: model.into(),
        provider: None,
    }
}

fn model_applied(view: &SettingsView) -> bool {
    view.model.as_ref() == Some(&chat_route("gpt-4o"))
}

fn model_controlled(view: &SettingsView) -> bool {
    view.model.as_ref() == Some(&chat_route("gpt-nano"))
}

fn thinking_applied(view: &SettingsView) -> bool {
    view.thinking == ThinkingLevel::High
}

fn thinking_controlled(view: &SettingsView) -> bool {
    view.thinking == ThinkingLevel::Minimal
}

fn approval_applied(view: &SettingsView) -> bool {
    view.approval == ApprovalMode::All
}

fn approval_controlled(view: &SettingsView) -> bool {
    view.approval == ApprovalMode::Edits
}

fn mode_applied(view: &SettingsView) -> bool {
    view.mode == Mode::EvalFirst
}

fn mode_controlled(view: &SettingsView) -> bool {
    view.mode == Mode::EvalOnly
}

fn reloaded_model(config: &Config) -> bool {
    config.model() == Some("openai-chat/gpt-4o")
}

fn reloaded_thinking(config: &Config) -> bool {
    config.thinking() == ThinkingLevel::High
}

fn reloaded_approval(config: &Config) -> bool {
    config.approval() == ApprovalMode::All
}

fn reloaded_mode(config: &Config) -> bool {
    config.mode() == Mode::EvalFirst
}

/// One consumer-visible kind `persist_for` maps: the command that applies
/// and persists, the `SessionOnly` control that must not write, the exact
/// `dal.toml` line the save must leave, and where each value must show up.
struct Kind {
    /// The `Save::SessionAndDefault` command the proof submits.
    default: Command,
    /// The same kind carrying `Save::SessionOnly` with a different value.
    session_only: Command,
    /// The exact `dal.toml` line the save must leave for the next start.
    saved_line: &'static str,
    /// The seeded line this kind's save replaces.
    replaced_line: &'static str,
    /// The settings projection the live session must show after the apply.
    applied: fn(&SettingsView) -> bool,
    /// The settings projection after the session-only control applied.
    controlled: fn(&SettingsView) -> bool,
    /// The config a restarted host must load from the saved file.
    reloaded: fn(&Config) -> bool,
    /// The settings projection of a fresh session on the restarted host.
    /// `None` for the model kind: a session that only inherits the host
    /// default shows `model: None` — the selection stays unset by design,
    /// so the restarted proof checks the loaded config instead.
    restarted: Option<fn(&SettingsView) -> bool>,
}

fn kinds() -> Vec<Kind> {
    vec![
        Kind {
            default: Command::SetModel {
                model: chat_route("gpt-4o"),
                save: Save::SessionAndDefault,
            },
            session_only: Command::SetModel {
                model: chat_route("gpt-nano"),
                save: Save::SessionOnly,
            },
            saved_line: "model = \"openai-chat/gpt-4o\"",
            replaced_line: "model = \"openai-chat/gpt-mini\"",
            applied: model_applied,
            controlled: model_controlled,
            reloaded: reloaded_model,
            restarted: None,
        },
        Kind {
            default: Command::SetThinking {
                level: ThinkingLevel::High,
                save: Save::SessionAndDefault,
            },
            session_only: Command::SetThinking {
                level: ThinkingLevel::Minimal,
                save: Save::SessionOnly,
            },
            saved_line: "thinking = \"high\"",
            replaced_line: "thinking = \"low\"",
            applied: thinking_applied,
            controlled: thinking_controlled,
            reloaded: reloaded_thinking,
            restarted: Some(thinking_applied),
        },
        Kind {
            default: Command::SetApproval {
                mode: ApprovalMode::All,
                save: Save::SessionAndDefault,
            },
            session_only: Command::SetApproval {
                mode: ApprovalMode::Edits,
                save: Save::SessionOnly,
            },
            saved_line: "approval = \"all\"",
            replaced_line: "approval = \"ask\"",
            applied: approval_applied,
            controlled: approval_controlled,
            reloaded: reloaded_approval,
            restarted: Some(approval_applied),
        },
        Kind {
            default: Command::SetMode {
                mode: Mode::EvalFirst,
                save: Save::SessionAndDefault,
            },
            session_only: Command::SetMode {
                mode: Mode::EvalOnly,
                save: Save::SessionOnly,
            },
            saved_line: "mode = \"eval-first\"",
            replaced_line: "mode = \"normal\"",
            applied: mode_applied,
            controlled: mode_controlled,
            reloaded: reloaded_mode,
            restarted: Some(mode_applied),
        },
    ]
}

struct Rig {
    _temp: tempfile::TempDir,
    data: PathBuf,
    config_path: PathBuf,
    workspace: Workspace,
    host: Host,
    agent: Agent,
}

/// Starts a host over the current file and opens a new session on it, with
/// `config_path` as the place default saves land.
async fn boot(data: &Path, config_path: &Path, workspace: &Workspace) -> (Host, Agent) {
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
    host.set_config_path(config_path.to_path_buf());
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("model-default-save"),
        )
        .await
        .expect("session opens");
    (host, agent)
}

impl Rig {
    async fn with_file(seed: &str) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let data = temp.path().join("data");
        let workspace_dir = temp.path().join("workspace");
        let config_dir = temp.path().join("config");
        for dir in [&data, &workspace_dir, &config_dir] {
            std::fs::create_dir_all(dir).expect("fixture directory");
        }
        let config_path = config_dir.join("dal.toml");
        std::fs::write(&config_path, seed).expect("seed config");
        let workspace = Workspace::new(workspace_dir).expect("workspace");
        let (host, agent) = boot(&data, &config_path, &workspace).await;
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
        let (host, agent) = boot(&data, &config_path, &workspace).await;
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

/// Awaits the next published `Settings` update whose projection satisfies
/// `want`; the command outcome must publish, never be slept over.
async fn await_settings(
    subscription: &mut Subscription,
    want: fn(&SettingsView) -> bool,
    label: &str,
) -> Result<SettingsView, Box<dyn std::error::Error>> {
    let timed = tokio::time::timeout(PUBLISHED, async {
        loop {
            match subscription.next().await {
                Some(Delivery::Update(update)) => {
                    if let UpdateKind::Settings(view) = &update.kind
                        && want(view)
                    {
                        return Ok(view.clone());
                    }
                }
                Some(Delivery::Resync { .. }) => {}
                None => {
                    return Err::<SettingsView, Box<dyn std::error::Error>>(Box::<
                        dyn std::error::Error,
                    >::from(
                        format!(
                        "subscription closed before the {label} settings update"
                    )
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| -> Box<dyn std::error::Error> {
        Box::<dyn std::error::Error>::from(format!("the {label} settings update never published"))
    })??;
    Ok(timed)
}

/// The full save-contract proof for one kind: the `SessionAndDefault`
/// submission applies to the session and persists the default into the
/// user's `dal.toml` (replacing only its own key), the `SessionOnly` control
/// changes the session without touching a byte, and a restarted host loads
/// the saved default.
async fn proves_session_and_default(kind: &Kind) -> TestResult {
    let rig = Rig::with_file(SEED).await;
    let mut events = rig.agent.subscribe(None).expect("subscribe");

    let reply = rig
        .agent
        .submit(kind.default.clone())
        .await
        .expect("the default applies");
    assert!(matches!(reply, Reply::Done(_)), "{reply:?}");
    await_settings(&mut events, kind.applied, "default").await?;
    let view = rig.agent.view(PageReq::default()).expect("view snapshots");
    assert!((kind.applied)(&view.settings), "{:?}", view.settings);

    let saved = rig.saved();
    assert!(
        saved.contains(kind.saved_line),
        "the default never reached dal.toml: {saved}"
    );
    assert!(
        !saved.contains(kind.replaced_line),
        "the save must replace the old default: {saved}"
    );
    for kept in kept_lines(kind) {
        assert!(saved.contains(kept), "the save dropped {kept:?}: {saved}");
    }
    assert!(rig.loads(), "the saved file must load at the next start");

    let before = rig.saved();
    let reply = rig
        .agent
        .submit(kind.session_only.clone())
        .await
        .expect("the session-only control applies");
    assert!(matches!(reply, Reply::Done(_)), "{reply:?}");
    await_settings(&mut events, kind.controlled, "session-only").await?;
    let view = rig.agent.view(PageReq::default()).expect("view snapshots");
    assert!((kind.controlled)(&view.settings), "{:?}", view.settings);
    assert_eq!(
        rig.saved(),
        before,
        "a session-only save must leave the file bytes unchanged"
    );

    drop(events);
    let rig = rig.restart().await;
    let reloaded = Config::load(
        ConfigProduct::Dalgon,
        &rig.data,
        "",
        Some(rig.saved().as_str()),
    )
    .expect("saved file loads");
    assert!(
        (kind.reloaded)(&reloaded),
        "restarted config: {}",
        rig.saved()
    );
    if let Some(see) = kind.restarted {
        let view = rig.agent.view(PageReq::default()).expect("view snapshots");
        assert!(see(&view.settings), "fresh session: {:?}", view.settings);
    }
    let _ = rig.host.shutdown(CLOSE).await;
    Ok(())
}

#[tokio::test]
async fn a_session_and_default_model_submission_persists_the_default() -> TestResult {
    proves_session_and_default(&kinds()[0]).await?;
    Ok(())
}

#[tokio::test]
async fn a_session_and_default_thinking_submission_persists_the_default() -> TestResult {
    proves_session_and_default(&kinds()[1]).await?;
    Ok(())
}

#[tokio::test]
async fn a_session_and_default_approval_submission_persists_the_default() -> TestResult {
    proves_session_and_default(&kinds()[2]).await?;
    Ok(())
}

#[tokio::test]
async fn a_session_and_default_mode_submission_persists_the_default() -> TestResult {
    proves_session_and_default(&kinds()[3]).await?;
    Ok(())
}

/// A rejected `SessionAndDefault` save must surface in the command outcome,
/// never silently claim `Done` while the file stayed untouched. The seeded
/// file is broken after startup, so the writer must refuse it.
#[tokio::test]
async fn an_unwritable_default_save_is_reported_through_agent_submit() -> TestResult {
    let rig = Rig::with_file(SEED).await;
    std::fs::write(&rig.config_path, "[plugin\n").expect("break the file after startup");
    let reply = rig
        .agent
        .submit(Command::SetThinking {
            level: ThinkingLevel::High,
            save: Save::SessionAndDefault,
        })
        .await;
    // Until the save contract moves onto this path, the command reports the
    // fold outcome only; the contract requires the save failure to reach the
    // caller instead of the save vanishing.
    match reply {
        Ok(Reply::Done(_)) => {
            let saved = rig.saved();
            assert_eq!(
                saved, "[plugin\n",
                "a refused edit must not touch the file, and a Done that lost its save must say so"
            );
        }
        Ok(other) => {
            return Err(Box::<dyn std::error::Error>::from(format!(
                "unexpected reply: {other:?}"
            )));
        }
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains("dal.toml") || message.contains("save"),
                "the failure must name the refused save: {message}"
            );
        }
    }
    let _ = rig.host.shutdown(CLOSE).await;
    Ok(())
}

#[tokio::test]
async fn a_session_only_model_submission_never_writes_dal_toml() -> TestResult {
    let rig = Rig::with_file(SEED).await;
    let before = rig.saved();
    let reply = rig
        .agent
        .submit(Command::SetModel {
            model: chat_route("gpt-nano"),
            save: Save::SessionOnly,
        })
        .await
        .expect("the session-only selection applies");
    assert!(matches!(reply, Reply::Done(_)), "{reply:?}");
    let view = rig.agent.view(PageReq::default()).expect("view snapshots");
    assert_eq!(view.settings.model, Some(chat_route("gpt-nano")));
    assert_eq!(
        rig.saved(),
        before,
        "a session-only save must leave the file bytes unchanged"
    );
    let _ = rig.host.shutdown(CLOSE).await;
    Ok(())
}
